//! Git OSINT: what a repository tells an outsider beyond credentials.
//!
//! * **People**: every author and committer identity, personal vs corporate
//!   vs noreply addresses, aliases, UTC offsets and a weekday × hour heat
//!   map per person, and how many of their commits were pushed.
//! * **Messages**: trailers (`Co-authored-by`, `Signed-off-by`, …), ticket
//!   keys, URLs, and commit messages that describe security incidents.
//! * **Footprint**: internal hostnames, private and public IPs, cloud
//!   account identifiers, buckets, registries, Slack workspaces, local
//!   paths with user names, e-mail addresses, phone numbers, IBANs,
//!   payment cards and social security numbers in the working tree.
//! * **Metadata**: EXIF (GPS, camera, owner) in images, author fields in
//!   PDFs and Office documents, local paths inside committed binaries, and
//!   junk files that leak the local environment.
//! * **History leftovers**: commits only the reflog still reaches, stashes
//!   with secrets, backup-looking branches, deleted sensitive files, and on
//!   demand the unreachable objects `git fsck` finds and the pull-request
//!   refs the remote still advertises.
//! * **Supply chain**: dependencies pinned to git URLs, packages from
//!   lockfiles (checked against osv.dev on demand), committed hooks and
//!   filter drivers.

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use regex::Regex;

use crate::secrets::{
    self, Collector, CommitRef, FileRule, Finding, GENERIC_MAX_LINE, LineSink, Place, Progress,
    Rules, Severity, Spec, check_path, is_test_path, spec,
};
use crate::{Error, Oid, Result, cli};

/// Binary or document files larger than this are not parsed for metadata.
const MAX_META_BYTES: u64 = 20 * 1024 * 1024;
/// Unreachable blobs: per-blob and total content limits for the secret scan.
const MAX_UNREACHABLE_BLOB: usize = 1024 * 1024;
const MAX_UNREACHABLE_TOTAL: u64 = 200 * 1024 * 1024;
/// Newest commits searched for deleted sensitive files (`git log
/// --diff-filter=D` diffs every commit it visits).
const DELETED_SCAN_COMMITS: usize = 10_000;

// ---------------------------------------------------------------------------
// Options, report
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OsintOptions {
    /// Newest commits to read for identities and messages (0 = all).
    pub max_commits: usize,
    /// All branches, tags and remotes instead of HEAD only.
    pub all_refs: bool,
}

impl Default for OsintOptions {
    fn default() -> Self {
        Self {
            max_commits: 50_000,
            all_refs: true,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct OsintStats {
    pub commits: usize,
    pub files: usize,
    pub bytes: u64,
    pub elapsed_ms: u64,
    pub remotes: usize,
    pub pushed_commits: usize,
    pub head_pushed: bool,
}

#[derive(Clone, Debug, Default)]
pub struct OsintReport {
    pub people: People,
    pub messages: Messages,
    pub footprint: Vec<Finding>,
    pub metadata: Vec<MetaFinding>,
    pub junk: Vec<JunkFile>,
    pub archaeology: Archaeology,
    pub supply: Supply,
    pub codeowners: Option<CodeOwners>,
    pub stats: OsintStats,
    pub commands: Vec<String>,
}

// ---------------------------------------------------------------------------
// People
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EmailKind {
    /// A personal mailbox (gmail, outlook, …): names the human directly.
    Personal,
    /// A company or project domain: names the employer.
    Corporate,
    /// A GitHub noreply address: reveals only the GitHub login.
    NoReply,
    /// Not a deliverable address (localhost, missing domain).
    Invalid,
}

impl EmailKind {
    pub fn label(self) -> &'static str {
        match self {
            EmailKind::Personal => "personal",
            EmailKind::Corporate => "corporate",
            EmailKind::NoReply => "noreply",
            EmailKind::Invalid => "invalid",
        }
    }
}

const PERSONAL_PROVIDERS: &[&str] = &[
    "gmail.com",
    "googlemail.com",
    "yahoo.com",
    "yahoo.co.uk",
    "ymail.com",
    "hotmail.com",
    "hotmail.co.uk",
    "outlook.com",
    "live.com",
    "msn.com",
    "icloud.com",
    "me.com",
    "mac.com",
    "aol.com",
    "protonmail.com",
    "proton.me",
    "pm.me",
    "gmx.de",
    "gmx.net",
    "gmx.com",
    "web.de",
    "mail.com",
    "mail.ru",
    "yandex.ru",
    "yandex.com",
    "qq.com",
    "163.com",
    "126.com",
    "foxmail.com",
    "naver.com",
    "fastmail.com",
    "hey.com",
    "tutanota.com",
    "tuta.io",
    "zoho.com",
    "posteo.de",
    "mailbox.org",
    "seznam.cz",
    "wp.pl",
    "o2.pl",
    "libero.it",
    "orange.fr",
    "free.fr",
    "laposte.net",
    "t-online.de",
    "bluewin.ch",
];

pub fn email_kind(email: &str) -> EmailKind {
    let lower = email.trim().to_lowercase();
    let Some((local, domain)) = lower.split_once('@') else {
        return EmailKind::Invalid;
    };
    if domain.ends_with("users.noreply.github.com")
        || domain == "noreply.github.com"
        || local.contains("noreply")
        || local.contains("no-reply")
    {
        return EmailKind::NoReply;
    }
    if !domain.contains('.')
        || domain == "localhost"
        || domain.ends_with(".local")
        || domain.ends_with(".localdomain")
        || domain.ends_with(".lan")
        || domain == "example.com"
        || domain.starts_with("(none)")
    {
        return EmailKind::Invalid;
    }
    if PERSONAL_PROVIDERS.contains(&domain) {
        return EmailKind::Personal;
    }
    EmailKind::Corporate
}

/// One e-mail address as it appears in the history.
#[derive(Clone, Debug, Default)]
pub struct Identity {
    pub email: String,
    pub domain: String,
    pub kind: Option<EmailKind>,
    /// Names used with this address, most frequent first.
    pub names: Vec<(String, usize)>,
    /// Commits authored.
    pub commits: usize,
    /// Commits committed (merges, rebases, cherry-picks by this person).
    pub committed: usize,
    /// Authored commits reachable from a remote-tracking branch.
    pub pushed: usize,
    pub first: i64,
    pub last: i64,
    /// UTC offsets (minutes) seen on authored commits, most frequent first.
    pub offsets: Vec<(i32, usize)>,
    /// Authored commits per local weekday (Monday = 0) and hour.
    pub heat: [[u32; 24]; 7],
    pub weekend: usize,
    /// Commits between 22:00 and 06:00 local time.
    pub night: usize,
    /// Other addresses used with the same name(s).
    pub aliases: Vec<String>,
}

impl Identity {
    pub fn kind(&self) -> EmailKind {
        self.kind.unwrap_or_else(|| email_kind(&self.email))
    }

    pub fn name(&self) -> &str {
        self.names.first().map(|(n, _)| n.as_str()).unwrap_or("")
    }

    /// Busiest local hour, if any commits.
    pub fn peak_hour(&self) -> Option<usize> {
        let mut by_hour = [0u32; 24];
        for day in &self.heat {
            for (h, n) in day.iter().enumerate() {
                by_hour[h] += n;
            }
        }
        let (h, n) = by_hour
            .iter()
            .enumerate()
            .max_by_key(|(_, n)| **n)
            .unwrap_or((0, &0));
        (*n > 0).then_some(h)
    }
}

#[derive(Clone, Debug, Default)]
pub struct People {
    /// Most commits first.
    pub identities: Vec<Identity>,
    pub total_commits: usize,
    pub local_name: String,
    pub local_email: String,
    /// Where `user.email` comes from: `repository`, `global`, or empty.
    pub local_scope: String,
}

impl People {
    pub fn count(&self, kind: EmailKind) -> usize {
        self.identities.iter().filter(|i| i.kind() == kind).count()
    }
}

/// `+0200` → 120.
fn parse_offset(s: &str) -> Option<i32> {
    let s = s.trim();
    let (sign, rest) = match s.chars().next()? {
        '+' => (1, &s[1..]),
        '-' => (-1, &s[1..]),
        _ => return None,
    };
    if rest.len() != 4 {
        return None;
    }
    let h: i32 = rest[..2].parse().ok()?;
    let m: i32 = rest[2..].parse().ok()?;
    Some(sign * (h * 60 + m))
}

/// `(weekday Monday=0, hour)` of an epoch time at a UTC offset in minutes.
fn local_slot(at: i64, offset_min: i32) -> (usize, usize) {
    let local = at + offset_min as i64 * 60;
    let days = local.div_euclid(86_400);
    // 1970-01-01 was a Thursday: Monday-based index 3.
    let weekday = (days + 3).rem_euclid(7) as usize;
    let hour = (local.rem_euclid(86_400) / 3600) as usize;
    (weekday, hour)
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct Trailer {
    pub kind: String,
    pub total: usize,
    /// Values with counts, most frequent first.
    pub values: Vec<(String, usize)>,
}

#[derive(Clone, Debug)]
pub struct Incident {
    pub commit: CommitRef,
    pub reason: &'static str,
    pub pushed: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Messages {
    pub scanned: usize,
    pub trailers: Vec<Trailer>,
    /// Ticket project keys with counts and a few sample ids.
    pub tickets: Vec<(String, usize, Vec<String>)>,
    /// Domains of URLs in messages, with counts.
    pub domains: Vec<(String, usize)>,
    /// URLs on internal-looking hosts, with the commit.
    pub internal_urls: Vec<(String, CommitRef)>,
    pub incidents: Vec<Incident>,
}

struct IncidentRule {
    reason: &'static str,
    regex: Regex,
}

fn incident_rules() -> &'static [IncidentRule] {
    static RULES: OnceLock<Vec<IncidentRule>> = OnceLock::new();
    RULES.get_or_init(|| {
        [
            (
                "a secret was removed or rotated",
                r"(?i)\b(remov\w*|delet\w*|strip\w*|purg\w*|scrub\w*|redact\w*|rotat\w*|revok\w*|invalidat\w*)\b.{0,60}\b(password|passwd|secret|credential|(?:aws|api|private|ssh|access|secret|signing|deploy|encryption|gpg|pgp|service|cloud|auth) ?(?:token|key))s?\b",
            ),
            (
                "a secret was removed or rotated",
                r"(?i)\b(password|passwd|secret|credential|(?:aws|api|private|ssh|access|secret|signing|deploy|encryption|gpg|pgp|service|cloud|auth) ?(?:token|key))s?\b.{0,40}\b(remov\w*|delet\w*|rotat\w*|revok\w*|leak\w*|expos\w*)\b",
            ),
            (
                "something leaked or was exposed",
                r"(?i)\b(leak(ed|s|ing)?|exposed|exposure|compromised)\b.{0,40}\b(secret|password|passwd|credential|(?:aws|api|private|ssh|access|secret|signing|deploy|encryption|gpg|pgp|service|cloud|auth) ?(?:token|key)|pii|personal data|user data|customer data)s?\b|\b(secret|password|passwd|credential|(?:aws|api|private|ssh|access|secret|signing|deploy|encryption|gpg|pgp|service|cloud|auth) ?(?:token|key)|pii|personal data)s?\b.{0,40}\b(leak(ed|s|ing)?|exposed|exposure|compromised)\b",
            ),
            (
                "a security check was disabled or bypassed",
                r"(?i)\b(disabl\w*|skip\w*|bypass\w*|turn(ed)? off|remov\w*)\b.{0,40}\b(auth(entication|orization|z|n)?( check)?|login check|ssl|tls|cert(ificate)?s?( (check|validation|verification|pinning))?|(signature|integrity|hash|checksum|origin|host|cup|token) (check|verification|validation)|csrf|xsrf|cors( check)?|sandbox|2fa|mfa|security check|permission check|rate limit\w*)\b",
            ),
            (
                "a credential was hard-coded",
                r"(?i)\bhard-?cod\w*\b.{0,40}\b(password|passwd|secret|api ?key|private key|token|credential)s?\b",
            ),
            (
                "a temporary hack",
                r"(?i)\b(temporary|temp|quick|dirty|ugly) (hack|workaround)\b|\bhack\b.{0,24}\b(auth|login|security|around)\b",
            ),
            (
                "a backdoor or debug access",
                r"(?i)\bbackdoor\b|\b(debug|test|admin|hidden) (user|account|login|password|credentials?)\b|\bmaster password\b|\bgod mode\b",
            ),
            (
                "something was committed by accident",
                r"(?i)\boops\b|\baccidentall?y\b|\bby mistake\b|\bshould(n'?t| not) (have|be) (been )?(commit|push|add)\w*\b|\bwrong (file|branch|repo)\b",
            ),
            (
                "security vulnerability mentioned",
                r"(?i)\b(cve-\d{4}-\d+|vulnerabilit\w*|exploit\w*|xss|sql injection|rce|remote code execution|path traversal|privilege escalation)\b",
            ),
        ]
        .iter()
        .map(|(reason, p)| IncidentRule {
            reason,
            regex: Regex::new(p).expect("incident rule"),
        })
        .collect()
    })
}

const TICKET_EXCLUDE: &[&str] = &[
    "SHA", "MD", "UTF", "ISO", "RFC", "CVE", "IPV", "HTTP", "TLS", "SSL", "GPT", "ES", "ECMA", "X",
    "UTC", "GMT", "PR", "MR", "WIP", "TODO", "OSX", "WIN", "API", "CPU", "GPU", "ARM", "X86",
    "AES", "RSA", "DSA", "EC", "PBKDF", "HMAC", "NIST", "FIPS", "PCI", "EU", "US", "UK", "COVID",
    "GCC", "LLVM", "MSVC", "VS", "SDK", "NDK", "JDK", "JRE", "GA", "RC", "BETA", "ALPHA", "V", "R",
    "C", "GL", "GLES", "DX", "OPENGL", "IE", "WCAG", "UE", "ST", "LTS", "CSS", "HTML", "XML",
    "JSON", "YAML", "PNG", "JPEG", "GIF", "MP", "H", "AV", "VP", "AMD", "INTEL", "NVIDIA", "QA",
    "OS", "IOS", "MACOS", "UI", "UX", "ID", "KB", "MB", "GB", "TB", "GHSA", "CWE", "OWASP", "A",
    "I", "E", "P", "S", "T", "N", "M", "L", "SE", "TS", "JS",
];

// ---------------------------------------------------------------------------
// Footprint rules
// ---------------------------------------------------------------------------

fn valid_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u16>().is_ok_and(|n| n <= 255))
}

fn valid_private_ip(s: &str) -> bool {
    valid_ipv4(s)
}

fn valid_public_ip(s: &str) -> bool {
    if !valid_ipv4(s) {
        return false;
    }
    let o: Vec<u8> = s.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    let (a, b) = (o[0], o[1]);
    // Private, loopback, link-local, multicast, reserved, documentation.
    if a == 10
        || a == 127
        || a == 0
        || (a == 192 && b == 168)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 169 && b == 254)
        || (a == 100 && (64..=127).contains(&b))
        || a >= 224
        || (a == 192 && b == 0 && o[2] == 2)
        || (a == 198 && b == 51 && o[2] == 100)
        || (a == 203 && b == 0 && o[2] == 113)
        || (a == 198 && (18..=19).contains(&b))
    {
        return false;
    }
    // Public resolvers and obvious samples.
    !matches!(
        s,
        "8.8.8.8"
            | "8.8.4.4"
            | "1.1.1.1"
            | "1.0.0.1"
            | "9.9.9.9"
            | "149.112.112.112"
            | "208.67.222.222"
            | "208.67.220.220"
            | "255.255.255.255"
            | "1.2.3.4"
            | "4.3.2.1"
            | "123.123.123.123"
            | "111.111.111.111"
    ) && o[3] != 0
        && !(o[0] == o[1] && o[1] == o[2] && o[2] == o[3])
}

fn valid_account_id(s: &str) -> bool {
    s.len() == 12
        && s != "123456789012"
        && s != "000000000000"
        && !s.chars().all(|c| c == s.chars().next().unwrap_or('0'))
}

fn valid_gcp_project(s: &str) -> bool {
    let l = s.to_lowercase();
    ![
        "my-project",
        "project-id",
        "your-project",
        "example",
        "my-gcp-project",
        "sample-project",
        "test-project",
        "your-project-id",
        "my-project-id",
        "project",
        "projects",
        "_",
    ]
    .contains(&l.as_str())
        && !l.contains("example")
        && !l.contains("your-")
        && !l.contains("my-")
        && !l.contains("placeholder")
        && !l.starts_with('$')
        && !l.starts_with('{')
        && !l.starts_with('<')
}

fn valid_azure_sub(s: &str) -> bool {
    !s.chars().all(|c| c == '0' || c == '-')
        && s.to_lowercase() != "00000000-0000-0000-0000-000000000000"
        && s.to_lowercase() != "12345678-1234-1234-1234-123456789012"
}

fn valid_slack(s: &str) -> bool {
    let l = s.to_lowercase();
    ![
        "app",
        "api",
        "hooks",
        "files",
        "slack",
        "example",
        "yourteam",
        "your-team",
        "myteam",
        "my-team",
        "team",
        "workspace",
        "your-workspace",
        "my-workspace",
        "acme",
        "www",
        "status",
    ]
    .iter()
    .any(|p| l.starts_with(&format!("{p}.slack.com")))
}

const GENERIC_USERS: &[&str] = &[
    "runner",
    "user",
    "username",
    "jenkins",
    "travis",
    "circleci",
    "root",
    "vsts",
    "build",
    "builder",
    "ubuntu",
    "ec2-user",
    "administrator",
    "public",
    "default",
    "admin",
    "me",
    "you",
    "name",
    "foo",
    "bar",
    "baz",
    "example",
    "docker",
    "node",
    "app",
    "vagrant",
    "dev",
    "developer",
    "test",
    "tester",
    "ci",
    "git",
    "github",
    "gitlab",
    "buildkite",
    "teamcity",
    "bamboo",
    "appveyor",
    "azureagent",
    "worker",
    "agent",
    "home",
    "local",
    "share",
    "lib",
    "bin",
    "all",
    "your",
    "user1",
    "someone",
    "somebody",
    "sysadmin",
    "web",
    "www",
    "www-data",
    "nobody",
    "deploy",
    "service",
    "svc",
    "bot",
    "pi",
    "guest",
    "student",
    "alice",
    "bob",
    "john",
    "jane",
    "johndoe",
    "janedoe",
    "jdoe",
    "yourname",
    "myuser",
    "my-user",
    "username1",
    "unknown",
    "runneradmin",
    "containeradministrator",
    "containeruser",
    "vssadministrator",
    "buildbot",
    "distiller",
    "gitpod",
    "codespace",
    "coder",
    "jovyan",
    "shared",
    "common",
    "all users",
    "tmp",
    "temp",
    "src",
    "projects",
    "project",
    "opt",
    "data",
    "x",
    "xxx",
    "whoever",
    "whatever",
];

fn valid_username(s: &str) -> bool {
    let l = s.to_lowercase();
    !GENERIC_USERS.contains(&l.as_str())
        && !l.starts_with('$')
        && !l.starts_with('{')
        && !l.starts_with('%')
        && !l.starts_with('<')
        && !l.starts_with('[')
        && !l.starts_with('*')
        && !l.starts_with('~')
        && !l.contains("user_name")
        && !l.contains("username")
        && !l.contains("your_")
        && l.len() >= 2
}

fn valid_email(s: &str) -> bool {
    let l = s.to_lowercase();
    let Some((local, domain)) = l.split_once('@') else {
        return false;
    };
    let bad_domain = domain.starts_with("example.")
        || domain.contains("example.com")
        || domain.contains("example.org")
        || domain.contains("example.net")
        || domain.ends_with(".example")
        || domain.ends_with(".test")
        || domain.ends_with(".invalid")
        || domain.ends_with(".local")
        || domain.ends_with(".localhost")
        || domain == "localhost"
        || domain.contains("noreply")
        || domain.contains("no-reply")
        || domain.ends_with("users.noreply.github.com")
        || domain.ends_with(".png")
        || domain.ends_with(".jpg")
        || domain.ends_with(".js")
        || domain.ends_with(".ts")
        || domain.ends_with(".css")
        || domain.ends_with(".html")
        || domain.ends_with(".json")
        || domain.ends_with(".md")
        || domain == "domain.com"
        || domain == "email.com"
        || domain == "mail.com"
        || domain == "company.com"
        || domain == "yourdomain.com"
        || domain == "yourcompany.com"
        || domain == "mydomain.com"
        || domain == "test.com"
        || domain == "foo.com"
        || domain == "bar.com"
        || domain == "acme.com"
        || domain == "acme.org";
    let bad_local = [
        "noreply",
        "no-reply",
        "donotreply",
        "do-not-reply",
        "example",
        "user",
        "test",
        "someone",
        "somebody",
        "john.doe",
        "jane.doe",
        "johndoe",
        "janedoe",
        "jdoe",
        "your",
        "your.name",
        "yourname",
        "email",
        "name",
        "username",
        "foo",
        "bar",
        "baz",
        "me",
        "you",
        "admin",
        "root",
        "info",
        "contact",
        "support",
        "hello",
        "sales",
        "security",
        "abuse",
        "postmaster",
        "webmaster",
        "hostmaster",
        "team",
        "dev",
        "devs",
        "developer",
        "developers",
        "a",
        "b",
        "x",
        "y",
        "z",
        "mail",
        "m",
    ];
    !bad_domain
        && !bad_local.contains(&local)
        && !local.starts_with("test")
        && !local.starts_with("example")
        && !local.starts_with("user")
        && !local.contains("@")
        && !local.chars().all(|c| c.is_ascii_digit())
}

fn valid_phone(s: &str) -> bool {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if !(8..=15).contains(&digits.len()) {
        return false;
    }
    let distinct: HashSet<char> = digits.chars().collect();
    if distinct.len() < 4 {
        return false;
    }
    // Sequences and the fictional 555 exchange.
    !digits.contains("1234567") && !digits.contains("555") && !digits.ends_with("0000000")
}

/// IBAN check: country-specific length when known, then mod-97 == 1.
pub fn valid_iban(s: &str) -> bool {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if s.len() < 15 || s.len() > 34 {
        return false;
    }
    let country = &s[..2];
    let expected = match country {
        "AL" => 28,
        "AD" => 24,
        "AT" => 20,
        "AZ" => 28,
        "BH" => 22,
        "BE" => 16,
        "BA" => 20,
        "BR" => 29,
        "BG" => 22,
        "CR" => 22,
        "HR" => 21,
        "CY" => 28,
        "CZ" => 24,
        "DK" => 18,
        "DO" => 28,
        "EE" => 20,
        "FO" => 18,
        "FI" => 18,
        "FR" => 27,
        "GE" => 22,
        "DE" => 22,
        "GI" => 23,
        "GR" => 27,
        "GL" => 18,
        "GT" => 28,
        "HU" => 28,
        "IS" => 26,
        "IE" => 22,
        "IL" => 23,
        "IT" => 27,
        "JO" => 30,
        "KZ" => 20,
        "KW" => 30,
        "LV" => 21,
        "LB" => 28,
        "LI" => 21,
        "LT" => 20,
        "LU" => 20,
        "MK" => 19,
        "MT" => 31,
        "MR" => 27,
        "MU" => 30,
        "MC" => 27,
        "MD" => 24,
        "ME" => 22,
        "NL" => 18,
        "NO" => 15,
        "PK" => 24,
        "PS" => 29,
        "PL" => 28,
        "PT" => 25,
        "QA" => 29,
        "RO" => 24,
        "SM" => 27,
        "SA" => 24,
        "RS" => 22,
        "SK" => 24,
        "SI" => 19,
        "ES" => 24,
        "SE" => 24,
        "CH" => 21,
        "TN" => 24,
        "TR" => 26,
        "AE" => 23,
        "GB" => 22,
        "VG" => 24,
        "XK" => 20,
        "UA" => 29,
        "BY" => 28,
        "EG" => 29,
        "IQ" => 23,
        "LY" => 25,
        "SC" => 31,
        "ST" => 25,
        "SV" => 28,
        "TL" => 23,
        "VA" => 22,
        _ => 0,
    };
    if expected == 0 || s.len() != expected {
        return false;
    }
    if !s.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    // Move the first four characters to the end, letters → numbers, mod 97.
    let rearranged = format!("{}{}", &s[4..], &s[..4]);
    let mut rem: u32 = 0;
    for c in rearranged.chars() {
        let v = if c.is_ascii_digit() {
            c as u32 - '0' as u32
        } else {
            c.to_ascii_uppercase() as u32 - 'A' as u32 + 10
        };
        if v >= 10 {
            rem = (rem * 100 + v) % 97;
        } else {
            rem = (rem * 10 + v) % 97;
        }
    }
    rem == 1
}

/// Luhn check plus issuer prefixes; well-known test numbers rejected.
pub fn valid_card(s: &str) -> bool {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if !(13..=19).contains(&digits.len()) {
        return false;
    }
    // 17–19 digit cards exist but are rare; bare integers of that length
    // (message ids, snowflakes) are everywhere. Require grouping for them.
    if digits.len() > 16 && !s.contains([' ', '-']) {
        return false;
    }
    let first: Vec<u8> = digits.bytes().take(4).map(|b| b - b'0').collect();
    let p2 = first[0] as u32 * 10 + first[1] as u32;
    let p4 = p2 * 100 + first[2] as u32 * 10 + first[3] as u32;
    let issuer_ok = first[0] == 4
        || (51..=55).contains(&p2)
        || (2221..=2720).contains(&p4)
        || p2 == 34
        || p2 == 37
        || p4 == 6011
        || p2 == 65
        || (644..=649).contains(&(p4 / 10))
        || p2 == 35
        || (30..=36).contains(&p2);
    if !issuer_ok {
        return false;
    }
    let distinct: HashSet<char> = digits.chars().collect();
    if distinct.len() < 4 {
        return false;
    }
    if [
        "4111111111111111",
        "4242424242424242",
        "4012888888881881",
        "4000056655665556",
        "5555555555554444",
        "5105105105105100",
        "5200828282828210",
        "378282246310005",
        "371449635398431",
        "6011111111111117",
        "6011000990139424",
        "3530111333300000",
        "3566002020360505",
        "30569309025904",
        "38520000023237",
        "4222222222222",
        "4917610000000000",
        "4000000000000002",
        "4000000000000069",
        "4000000000000119",
        "4000000000000341",
        "2223003122003222",
        "6304000000000000",
        "4444333322221111",
        "5500000000000004",
        "340000000000009",
    ]
    .contains(&digits.as_str())
    {
        return false;
    }
    // Luhn.
    let mut sum = 0u32;
    for (i, b) in digits.bytes().rev().enumerate() {
        let mut d = (b - b'0') as u32;
        if i % 2 == 1 {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
    }
    sum.is_multiple_of(10)
}

fn valid_ssn(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return false;
    }
    let area: u32 = parts[0].parse().unwrap_or(0);
    let group: u32 = parts[1].parse().unwrap_or(0);
    let serial: u32 = parts[2].parse().unwrap_or(0);
    area != 0
        && area != 666
        && area < 900
        && group != 0
        && serial != 0
        && !matches!(
            s,
            "078-05-1120" | "219-09-9999" | "123-45-6789" | "111-11-1111"
        )
}

/// Cheap line tests that keep the expensive regexes off ordinary code.
fn pf_ip(l: &str) -> bool {
    let (mut dots, mut digits) = (0, 0);
    for b in l.bytes() {
        if b == b'.' {
            dots += 1;
        } else if b.is_ascii_digit() {
            digits += 1;
        }
    }
    dots >= 3 && digits >= 4
}

/// Card numbers: enough digits, and either the 4-4-4-4 shape or a payment
/// word on the line. Bare 16-digit integers (ids, hashes, timestamps) pass
/// Luhn one time in ten, which is far too often.
fn pf_card(l: &str) -> bool {
    static SHAPE: OnceLock<Regex> = OnceLock::new();
    if l.bytes().filter(|b| b.is_ascii_digit()).count() < 13 {
        return false;
    }
    let shape =
        SHAPE.get_or_init(|| Regex::new(r"\b\d{4}[ \-]\d{4}[ \-]\d{4}[ \-]\d{1,7}\b").unwrap());
    if shape.is_match(l) {
        return true;
    }
    let lower = l.to_lowercase();
    [
        "card",
        "visa",
        "mastercard",
        "amex",
        "discover",
        "credit",
        "debit",
        "payment",
        "pan=",
        "pan:",
        "\"pan\"",
        "cardholder",
    ]
    .iter()
    .any(|w| lower.contains(w))
}

fn pf_ssn(l: &str) -> bool {
    l.contains('-') && l.bytes().filter(|b| b.is_ascii_digit()).count() >= 9
}

fn pf_phone(l: &str) -> bool {
    let b = l.as_bytes();
    (0..b.len().saturating_sub(1)).any(|i| b[i] == b'+' && b[i + 1].is_ascii_digit())
}

fn pf_iban(l: &str) -> bool {
    let b = l.as_bytes();
    (0..b.len().saturating_sub(3)).any(|i| {
        b[i].is_ascii_uppercase()
            && b[i + 1].is_ascii_uppercase()
            && b[i + 2].is_ascii_digit()
            && b[i + 3].is_ascii_digit()
    })
}

fn pf_email(l: &str) -> bool {
    l.find('@').is_some_and(|i| l[i + 1..].contains('.'))
}

const WHY_INFRA: &str = "Infrastructure identifiers in a repository are a map for an attacker: which cloud accounts, hosts and buckets exist, how they are named, and where to aim first. Keep them in deployment configuration outside the repository, or at least out of public ones.";
const WHY_PII: &str = "Personal data in a repository is a privacy breach the moment the repository is shared, and it is copied into every clone and fork. Remove it, rewrite history if it was pushed, and check whether the people affected must be told.";

static FOOTPRINT_SPECS: &[Spec] = &[
    Spec {
        strict: false,
        validate: Some(valid_private_ip),
        ..spec(
            "private-ip",
            "Private IP address",
            Severity::Medium,
            r"\b(10\.\d{1,3}\.\d{1,3}\.\d{1,3}|192\.168\.\d{1,3}\.\d{1,3}|172\.(?:1[6-9]|2\d|3[01])\.\d{1,3}\.\d{1,3})\b",
            1,
            0.0,
            "A private address reveals the layout of an internal network: ranges in use, hosts, and what talks to what.",
            None,
        )
    },
    Spec {
        strict: false,
        validate: Some(valid_public_ip),
        prefilter: Some(pf_ip),
        max_line: GENERIC_MAX_LINE,
        ..spec(
            "public-ip",
            "Public IP address",
            Severity::Low,
            r"\b((?:\d{1,3}\.){3}\d{1,3})\b",
            1,
            0.0,
            "A hard-coded public address usually points at a server you operate: a direct target that bypasses DNS-based protections.",
            None,
        )
    },
    Spec {
        strict: false,
        ..spec(
            "internal-host",
            "Internal hostname",
            Severity::Medium,
            r"(?i)\b([a-z0-9][a-z0-9\-]*(?:\.[a-z0-9\-]+)*\.(?:corp|internal|intranet|lan|localdomain|private|intra))\b",
            1,
            0.0,
            WHY_INFRA,
            None,
        )
    },
    Spec {
        strict: false,
        validate: Some(valid_account_id),
        ..spec(
            "aws-account",
            "AWS account ID",
            Severity::Medium,
            r"\barn:aws[a-z\-]*:[a-z0-9\-]*:[a-z0-9\-]*:(\d{12}):",
            1,
            0.0,
            "An AWS account ID is not a secret by itself, but it is the first thing needed to enumerate roles, buckets and misconfigured trust policies.",
            None,
        )
    },
    Spec {
        strict: false,
        ..spec(
            "s3-bucket",
            "S3 bucket",
            Severity::Medium,
            r"\bs3://([a-z0-9][a-z0-9.\-]{2,62})\b",
            1,
            0.0,
            "Bucket names are global and guessable; a named bucket gets probed for public listing and write access within hours.",
            None,
        )
    },
    Spec {
        strict: false,
        ..spec(
            "s3-bucket-url",
            "S3 bucket",
            Severity::Medium,
            r"(?i)//([a-z0-9][a-z0-9.\-]{2,62})\.s3[.\-](?:[a-z0-9\-]+\.)?amazonaws\.com",
            1,
            0.0,
            "Bucket names are global and guessable; a named bucket gets probed for public listing and write access within hours.",
            None,
        )
    },
    Spec {
        strict: false,
        validate: Some(valid_gcp_project),
        ..spec(
            "gcp-project",
            "Google Cloud project",
            Severity::Medium,
            r"\b(?:projects/|gcr\.io/|-docker\.pkg\.dev/)([a-z][a-z0-9\-]{4,28}[a-z0-9])\b",
            1,
            0.0,
            WHY_INFRA,
            None,
        )
    },
    Spec {
        strict: false,
        validate: Some(valid_azure_sub),
        ..spec(
            "azure-subscription",
            "Azure subscription",
            Severity::Medium,
            r"(?i)/subscriptions/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\b",
            1,
            0.0,
            WHY_INFRA,
            None,
        )
    },
    Spec {
        strict: false,
        ..spec(
            "container-registry",
            "Private container registry",
            Severity::Medium,
            r"(?i)\b([a-z0-9.\-]+\.(?:azurecr\.io|dkr\.ecr\.[a-z0-9\-]+\.amazonaws\.com|jfrog\.io)|[a-z0-9.\-]*artifactory\.[a-z0-9.\-]+)\b",
            1,
            0.0,
            "A registry name tells an attacker where your images live and which organisation and region they belong to.",
            None,
        )
    },
    Spec {
        strict: false,
        validate: Some(valid_slack),
        ..spec(
            "slack-workspace",
            "Slack workspace",
            Severity::Low,
            r"(?i)\b([a-z0-9\-]+\.slack\.com(?:/archives/[A-Z0-9]+)?)",
            1,
            0.0,
            "The workspace name confirms where the team talks; channel IDs help target phishing and social engineering.",
            None,
        )
    },
    Spec {
        strict: false,
        validate: Some(valid_username),
        ..spec(
            "local-user-path",
            "Local path with a user name",
            Severity::Low,
            r"(?i)(?:[a-z]:\\users\\|/users/|/home/|/export/home/)([a-z0-9._\-]{2,32})[\\/]",
            1,
            0.0,
            "An absolute path leaks a developer's login name and operating system, and sometimes the project layout on their machine. Build output and generated files are the usual carriers.",
            None,
        )
    },
    Spec {
        strict: false,
        validate: Some(valid_email),
        prefilter: Some(pf_email),
        max_line: GENERIC_MAX_LINE,
        ..spec(
            "email-address",
            "E-mail address",
            Severity::Low,
            r"\b([A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,24})\b",
            1,
            0.0,
            "Addresses in code and data make phishing targets and, in data files, are personal data.",
            None,
        )
    },
    Spec {
        strict: false,
        validate: Some(valid_phone),
        prefilter: Some(pf_phone),
        max_line: GENERIC_MAX_LINE,
        ..spec(
            "phone-number",
            "Phone number",
            Severity::Low,
            r"(?:^|[^\w+.])(\+\d{1,3}(?:[ \-.]?\(?\d{2,4}\)?){2,5})\b",
            1,
            0.0,
            "Phone numbers are personal data and a social-engineering channel.",
            None,
        )
    },
    Spec {
        strict: false,
        validate: Some(valid_iban),
        prefilter: Some(pf_iban),
        ..spec(
            "iban",
            "IBAN",
            Severity::High,
            r"\b([A-Z]{2}\d{2}[A-Z0-9]{11,30})\b",
            1,
            0.0,
            WHY_PII,
            None,
        )
    },
    Spec {
        strict: false,
        validate: Some(valid_card),
        prefilter: Some(pf_card),
        max_line: GENERIC_MAX_LINE,
        ..spec(
            "payment-card",
            "Payment card number",
            Severity::High,
            r"\b((?:\d[ \-]?){12,18}\d)\b",
            1,
            0.0,
            WHY_PII,
            None,
        )
    },
    Spec {
        strict: false,
        validate: Some(valid_ssn),
        prefilter: Some(pf_ssn),
        max_line: GENERIC_MAX_LINE,
        ..spec(
            "us-ssn",
            "US Social Security number",
            Severity::Medium,
            r"\b(\d{3}-\d{2}-\d{4})\b",
            1,
            0.0,
            WHY_PII,
            None,
        )
    },
];

fn footprint_keywords(id: &str) -> &'static [&'static str] {
    match id {
        "private-ip" => &["10.", "192.168.", "172."],
        "public-ip" => &["1.", "2.", "3.", "4.", "5.", "6.", "7.", "8.", "9."],
        "internal-host" => &[
            ".corp",
            ".internal",
            ".intranet",
            ".lan",
            ".localdomain",
            ".private",
            ".intra",
        ],
        "aws-account" => &["arn:aws"],
        "s3-bucket" => &["s3://"],
        "s3-bucket-url" => &["amazonaws.com"],
        "gcp-project" => &["projects/", "gcr.io/", "pkg.dev/"],
        "azure-subscription" => &["/subscriptions/"],
        "container-registry" => &["azurecr.io", "dkr.ecr.", "jfrog.io", "artifactory."],
        "slack-workspace" => &["slack.com"],
        "local-user-path" => &["users\\", "/users/", "/home/", "/export/home/"],
        "email-address" => &["@"],
        "phone-number" => &["+"],
        "iban" => &[
            "AL", "AD", "AT", "AZ", "BH", "BE", "BA", "BR", "BG", "CR", "HR", "CY", "CZ", "DK",
            "DO", "EE", "FO", "FI", "FR", "GE", "DE", "GI", "GR", "GL", "GT", "HU", "IS", "IE",
            "IL", "IT", "JO", "KZ", "KW", "LV", "LB", "LI", "LT", "LU", "MK", "MT", "MR", "MU",
            "MC", "MD", "ME", "NL", "NO", "PK", "PS", "PL", "PT", "QA", "RO", "SM", "SA", "RS",
            "SK", "SI", "ES", "SE", "CH", "TN", "TR", "AE", "GB", "VG", "XK", "UA", "BY", "EG",
        ],
        "payment-card" => &["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"],
        "us-ssn" => &["-"],
        _ => &[],
    }
}

/// The footprint rule set, compiled once.
pub fn footprint_rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| Rules::from_specs(FOOTPRINT_SPECS, footprint_keywords))
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct MetaFinding {
    pub path: String,
    /// `JPEG`, `PNG`, `TIFF`, `PDF`, `Office document`, `OpenDocument`, `binary`.
    pub kind: &'static str,
    pub severity: Severity,
    pub size: u64,
    pub fields: Vec<(String, String)>,
    pub why: &'static str,
}

#[derive(Clone, Debug)]
pub struct JunkFile {
    pub path: String,
    pub reason: &'static str,
    pub size: u64,
}

fn rd16(d: &[u8], pos: usize, be: bool) -> Option<u16> {
    let b = d.get(pos..pos + 2)?;
    Some(if be {
        u16::from_be_bytes([b[0], b[1]])
    } else {
        u16::from_le_bytes([b[0], b[1]])
    })
}

fn rd32(d: &[u8], pos: usize, be: bool) -> Option<u32> {
    let b = d.get(pos..pos + 4)?;
    Some(if be {
        u32::from_be_bytes([b[0], b[1], b[2], b[3]])
    } else {
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    })
}

/// Fields of a TIFF/EXIF block (the bytes starting at `II*\0` / `MM\0*`).
pub fn exif_fields(t: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let be = match t.get(0..2) {
        Some(b"MM") => true,
        Some(b"II") => false,
        _ => return out,
    };
    if rd16(t, 2, be) != Some(42) {
        return out;
    }
    let Some(ifd0) = rd32(t, 4, be) else {
        return out;
    };
    let main = read_ifd(t, ifd0 as usize, be);
    let mut gps: Option<usize> = None;
    let mut exif: Option<usize> = None;
    for e in &main {
        match e.tag {
            0x010F => out.push(("Camera make".into(), e.ascii(t, be))),
            0x0110 => out.push(("Camera model".into(), e.ascii(t, be))),
            0x0131 => out.push(("Software".into(), e.ascii(t, be))),
            0x0132 => out.push(("Date".into(), e.ascii(t, be))),
            0x013B => out.push(("Artist".into(), e.ascii(t, be))),
            0x8298 => out.push(("Copyright".into(), e.ascii(t, be))),
            0x010E => out.push(("Description".into(), e.ascii(t, be))),
            0x8769 => exif = Some(e.value as usize),
            0x8825 => gps = Some(e.value as usize),
            _ => {}
        }
    }
    if let Some(off) = exif {
        for e in read_ifd(t, off, be) {
            match e.tag {
                0x9003 => out.push(("Taken".into(), e.ascii(t, be))),
                0xA430 => out.push(("Camera owner".into(), e.ascii(t, be))),
                0xA431 => out.push(("Body serial number".into(), e.ascii(t, be))),
                0xA433 => out.push(("Lens make".into(), e.ascii(t, be))),
                0xA434 => out.push(("Lens model".into(), e.ascii(t, be))),
                0xA435 => out.push(("Lens serial number".into(), e.ascii(t, be))),
                0x927C if e.count > 0 => {
                    out.push(("Maker note".into(), format!("{} bytes", e.count)))
                }
                _ => {}
            }
        }
    }
    if let Some(off) = gps {
        let entries = read_ifd(t, off, be);
        let get = |tag: u16| entries.iter().find(|e| e.tag == tag);
        let coord = |val: u16, refer: u16| -> Option<f64> {
            let v = get(val)?;
            let r = get(refer).map(|e| e.ascii(t, be)).unwrap_or_default();
            let parts = v.rationals(t, be);
            if parts.len() < 3 {
                return None;
            }
            let deg = parts[0] + parts[1] / 60.0 + parts[2] / 3600.0;
            Some(if r.starts_with('S') || r.starts_with('W') {
                -deg
            } else {
                deg
            })
        };
        if let (Some(lat), Some(lon)) = (coord(0x0002, 0x0001), coord(0x0004, 0x0003)) {
            out.push(("GPS position".into(), format!("{lat:.5}, {lon:.5}")));
            out.push((
                "Map".into(),
                format!("https://www.openstreetmap.org/?mlat={lat:.5}&mlon={lon:.5}#map=16/{lat:.5}/{lon:.5}"),
            ));
        }
        if let Some(alt) = get(0x0006) {
            let a = alt.rationals(t, be);
            if let Some(a) = a.first() {
                out.push(("GPS altitude".into(), format!("{a:.0} m")));
            }
        }
        if let Some(d) = get(0x001D) {
            out.push(("GPS date".into(), d.ascii(t, be)));
        }
    }
    out.retain(|(_, v)| !v.trim().is_empty());
    out
}

struct IfdEntry {
    tag: u16,
    typ: u16,
    count: u32,
    /// Inline value or offset.
    value: u32,
    /// Position of the 4 value bytes (for inline values).
    value_pos: usize,
}

impl IfdEntry {
    fn data_pos(&self, size: usize) -> usize {
        if self.count as usize * size <= 4 {
            self.value_pos
        } else {
            self.value as usize
        }
    }

    fn ascii(&self, t: &[u8], _be: bool) -> String {
        if self.typ != 2 && self.typ != 7 {
            return String::new();
        }
        let pos = self.data_pos(1);
        let end = (pos + self.count as usize).min(t.len());
        if pos >= end {
            return String::new();
        }
        String::from_utf8_lossy(&t[pos..end])
            .trim_end_matches('\0')
            .trim()
            .to_owned()
    }

    fn rationals(&self, t: &[u8], be: bool) -> Vec<f64> {
        if self.typ != 5 && self.typ != 10 {
            return Vec::new();
        }
        let pos = self.data_pos(8);
        (0..self.count as usize)
            .filter_map(|i| {
                let n = rd32(t, pos + i * 8, be)? as f64;
                let d = rd32(t, pos + i * 8 + 4, be)? as f64;
                Some(if d == 0.0 { 0.0 } else { n / d })
            })
            .collect()
    }
}

fn read_ifd(t: &[u8], off: usize, be: bool) -> Vec<IfdEntry> {
    let mut out = Vec::new();
    let Some(n) = rd16(t, off, be) else {
        return out;
    };
    for i in 0..(n as usize).min(200) {
        let p = off + 2 + i * 12;
        let (Some(tag), Some(typ), Some(count), Some(value)) = (
            rd16(t, p, be),
            rd16(t, p + 2, be),
            rd32(t, p + 4, be),
            rd32(t, p + 8, be),
        ) else {
            break;
        };
        out.push(IfdEntry {
            tag,
            typ,
            count,
            value,
            value_pos: p + 8,
        });
    }
    out
}

fn xmp_fields(xml: &str) -> Vec<(String, String)> {
    static RE: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    let res = RE.get_or_init(|| {
        [
            ("Creator", r"(?s)<dc:creator>.*?<rdf:li[^>]*>(.*?)</rdf:li>"),
            (
                "Creator tool",
                r#"xmp:CreatorTool="([^"]*)"|<xmp:CreatorTool>(.*?)</xmp:CreatorTool>"#,
            ),
            ("Rights", r"(?s)<dc:rights>.*?<rdf:li[^>]*>(.*?)</rdf:li>"),
            (
                "Modified",
                r#"xmp:ModifyDate="([^"]*)"|<xmp:ModifyDate>(.*?)</xmp:ModifyDate>"#,
            ),
            ("GPS latitude (XMP)", r#"exif:GPSLatitude="([^"]*)""#),
            ("GPS longitude (XMP)", r#"exif:GPSLongitude="([^"]*)""#),
            (
                "Author position",
                r"<photoshop:AuthorsPosition>(.*?)</photoshop:AuthorsPosition>",
            ),
            (
                "Owner",
                r#"aux:OwnerName="([^"]*)"|<aux:OwnerName>(.*?)</aux:OwnerName>"#,
            ),
            (
                "Serial number",
                r#"aux:SerialNumber="([^"]*)"|<aux:SerialNumber>(.*?)</aux:SerialNumber>"#,
            ),
        ]
        .iter()
        .map(|(k, p)| (*k, Regex::new(p).expect("xmp regex")))
        .collect()
    });
    let mut out = Vec::new();
    for (k, re) in res {
        if let Some(c) = re.captures(xml) {
            let v = c
                .get(1)
                .or_else(|| c.get(2))
                .map(|m| m.as_str().trim().to_owned())
                .unwrap_or_default();
            if !v.is_empty() {
                out.push(((*k).to_owned(), v));
            }
        }
    }
    out
}

/// EXIF and XMP fields of a JPEG.
pub fn jpeg_fields(data: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if data.get(0..2) != Some(&[0xFF, 0xD8]) {
        return out;
    }
    let mut pos = 2usize;
    while pos + 4 <= data.len() {
        if data[pos] != 0xFF {
            break;
        }
        let marker = data[pos + 1];
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            pos += 2;
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        let len = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        if len < 2 {
            break;
        }
        let end = (pos + 2 + len).min(data.len());
        let seg = &data[pos + 4..end];
        if marker == 0xE1 {
            if let Some(tiff) = seg.strip_prefix(b"Exif\0\0") {
                out.extend(exif_fields(tiff));
            } else if let Some(rest) = seg.strip_prefix(b"http://ns.adobe.com/xap/1.0/\0") {
                out.extend(xmp_fields(&String::from_utf8_lossy(rest)));
            }
        }
        pos = end;
    }
    out
}

/// Text chunks and embedded EXIF of a PNG.
pub fn png_fields(data: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if data.get(0..8) != Some(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return out;
    }
    let mut pos = 8usize;
    while pos + 12 <= data.len() {
        let len =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        let typ = &data[pos + 4..pos + 8];
        let body_end = (pos + 8 + len).min(data.len());
        let body = &data[pos + 8..body_end];
        match typ {
            b"tEXt" => {
                if let Some(z) = body.iter().position(|&b| b == 0) {
                    let key = String::from_utf8_lossy(&body[..z]).to_string();
                    let val = String::from_utf8_lossy(&body[z + 1..]).trim().to_string();
                    if key == "XML:com.adobe.xmp" {
                        out.extend(xmp_fields(&val));
                    } else if matches!(
                        key.as_str(),
                        "Author"
                            | "Copyright"
                            | "Software"
                            | "Comment"
                            | "Description"
                            | "Title"
                            | "Source"
                            | "Creation Time"
                    ) && !val.is_empty()
                    {
                        out.push((key, val));
                    }
                }
            }
            b"iTXt" => {
                // key\0 comp_flag comp_method lang\0 translated\0 text
                let mut parts = body.splitn(2, |&b| b == 0);
                let key = parts.next().map(|k| String::from_utf8_lossy(k).to_string());
                if let (Some(key), Some(rest)) = (key, parts.next())
                    && rest.len() > 2
                    && rest[0] == 0
                {
                    let after = &rest[2..];
                    let mut it = after.splitn(3, |&b| b == 0);
                    let _lang = it.next();
                    let _trans = it.next();
                    if let Some(text) = it.next() {
                        let val = String::from_utf8_lossy(text).to_string();
                        if key == "XML:com.adobe.xmp" {
                            out.extend(xmp_fields(&val));
                        } else if !val.trim().is_empty() {
                            out.push((key, val.trim().to_owned()));
                        }
                    }
                }
            }
            b"eXIf" => out.extend(exif_fields(body)),
            b"IEND" => break,
            _ => {}
        }
        pos = body_end + 4;
    }
    out
}

fn decode_pdf_string(raw: &str) -> String {
    raw.replace("\\(", "(")
        .replace("\\)", ")")
        .replace("\\\\", "\\")
        .trim()
        .to_owned()
}

fn decode_pdf_hex(hex: &str) -> String {
    let clean: String = hex.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let bytes: Vec<u8> = (0..clean.len() / 2)
        .filter_map(|i| u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16).ok())
        .collect();
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(&bytes).to_string()
    }
}

/// Info dictionary and XMP fields of a PDF (uncompressed ones only).
pub fn pdf_fields(data: &[u8]) -> Vec<(String, String)> {
    static RE: OnceLock<(Regex, Regex)> = OnceLock::new();
    let (lit, hex) = RE.get_or_init(|| {
        (
            Regex::new(r"/(Author|Creator|Producer|Title|Subject|Keywords|CreationDate|ModDate|Company)\s*\(((?:[^()\\]|\\.){0,300})\)").unwrap(),
            Regex::new(r"/(Author|Creator|Producer|Title|Subject|Keywords|CreationDate|ModDate|Company)\s*<([0-9A-Fa-f\s]{2,600})>").unwrap(),
        )
    });
    let text = String::from_utf8_lossy(data);
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push = |k: &str, v: String| {
        let v = v.trim().to_owned();
        if !v.is_empty() && !out.iter().any(|(ek, ev)| ek == k && *ev == v) {
            out.push((k.to_owned(), v));
        }
    };
    for c in lit.captures_iter(&text) {
        push(&c[1], decode_pdf_string(&c[2]));
    }
    for c in hex.captures_iter(&text) {
        push(&c[1], decode_pdf_hex(&c[2]));
    }
    if text.contains("<x:xmpmeta") {
        for (k, v) in xmp_fields(&text) {
            push(&k, v);
        }
    }
    out
}

/// One entry of a ZIP archive (stored or deflated), by exact name.
pub fn zip_entry(data: &[u8], name: &str) -> Option<Vec<u8>> {
    let start = data.len().saturating_sub(66 * 1024);
    let eocd = (start..data.len().saturating_sub(21))
        .rev()
        .find(|&i| data[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])?;
    let entries = rd16(data, eocd + 10, false)? as usize;
    let cd_offset = rd32(data, eocd + 16, false)? as usize;
    let mut pos = cd_offset;
    for _ in 0..entries.min(10_000) {
        if data.get(pos..pos + 4)? != [0x50, 0x4b, 0x01, 0x02] {
            return None;
        }
        let method = rd16(data, pos + 10, false)?;
        let csize = rd32(data, pos + 20, false)? as usize;
        let name_len = rd16(data, pos + 28, false)? as usize;
        let extra_len = rd16(data, pos + 30, false)? as usize;
        let comment_len = rd16(data, pos + 32, false)? as usize;
        let local = rd32(data, pos + 42, false)? as usize;
        let entry_name = String::from_utf8_lossy(data.get(pos + 46..pos + 46 + name_len)?);
        if entry_name == name {
            if data.get(local..local + 4)? != [0x50, 0x4b, 0x03, 0x04] {
                return None;
            }
            let lname = rd16(data, local + 26, false)? as usize;
            let lextra = rd16(data, local + 28, false)? as usize;
            let begin = local + 30 + lname + lextra;
            let raw = data.get(begin..begin + csize)?;
            return match method {
                0 => Some(raw.to_vec()),
                8 => {
                    let mut out = Vec::new();
                    flate2::read::DeflateDecoder::new(raw)
                        .take(16 * 1024 * 1024)
                        .read_to_end(&mut out)
                        .ok()?;
                    Some(out)
                }
                _ => None,
            };
        }
        pos += 46 + name_len + extra_len + comment_len;
    }
    None
}

fn xml_tags(xml: &str, tags: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (tag, label) in tags {
        let open = format!("<{tag}>");
        let open_attr = format!("<{tag} ");
        let close = format!("</{tag}>");
        let start = xml.find(&open).map(|i| i + open.len()).or_else(|| {
            let i = xml.find(&open_attr)?;
            let j = xml[i..].find('>')? + i + 1;
            Some(j)
        });
        if let Some(s) = start
            && let Some(e) = xml[s..].find(&close)
        {
            let v = xml[s..s + e].trim();
            if !v.is_empty() {
                out.push(((*label).to_owned(), html_unescape(v)));
            }
        }
    }
    out
}

fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

/// Core and app properties of an Office Open XML document.
pub fn office_fields(data: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(core) = zip_entry(data, "docProps/core.xml") {
        let xml = String::from_utf8_lossy(&core);
        out.extend(xml_tags(
            &xml,
            &[
                ("dc:creator", "Creator"),
                ("cp:lastModifiedBy", "Last modified by"),
                ("dc:title", "Title"),
                ("dc:subject", "Subject"),
                ("dc:description", "Description"),
                ("cp:keywords", "Keywords"),
                ("cp:category", "Category"),
                ("cp:revision", "Revision"),
                ("dcterms:created", "Created"),
                ("dcterms:modified", "Modified"),
            ],
        ));
    }
    if let Some(app) = zip_entry(data, "docProps/app.xml") {
        let xml = String::from_utf8_lossy(&app);
        out.extend(xml_tags(
            &xml,
            &[
                ("Company", "Company"),
                ("Manager", "Manager"),
                ("Application", "Application"),
                ("AppVersion", "Application version"),
                ("Template", "Template"),
                ("TotalTime", "Editing time (minutes)"),
                ("HyperlinkBase", "Hyperlink base"),
            ],
        ));
    }
    if let Some(meta) = zip_entry(data, "meta.xml") {
        let xml = String::from_utf8_lossy(&meta);
        out.extend(xml_tags(
            &xml,
            &[
                ("meta:initial-creator", "Initial creator"),
                ("dc:creator", "Creator"),
                ("meta:generator", "Generator"),
                ("meta:editing-cycles", "Editing cycles"),
                ("meta:creation-date", "Created"),
                ("dc:date", "Modified"),
                ("meta:printed-by", "Printed by"),
            ],
        ));
    }
    out
}

/// Printable ASCII and UTF-16LE runs of at least `min` characters.
pub fn binary_strings(data: &[u8], min: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for &b in data {
        if (0x20..0x7f).contains(&b) {
            cur.push(b as char);
        } else {
            if cur.len() >= min {
                out.push(std::mem::take(&mut cur));
            }
            cur.clear();
        }
    }
    if cur.len() >= min {
        out.push(cur);
    }
    // UTF-16LE: printable ASCII low byte, zero high byte.
    let mut cur = String::new();
    let mut i = 0;
    while i + 1 < data.len() {
        let (lo, hi) = (data[i], data[i + 1]);
        if hi == 0 && (0x20..0x7f).contains(&lo) {
            cur.push(lo as char);
            i += 2;
        } else {
            if cur.len() >= min {
                out.push(std::mem::take(&mut cur));
            }
            cur.clear();
            i += 1;
        }
    }
    if cur.len() >= min {
        out.push(cur);
    }
    out
}

fn junk_reason(path: &str) -> Option<&'static str> {
    let base = secrets::basename(path);
    let lower = base.to_lowercase();
    let lpath = path.to_lowercase();
    if lower == ".ds_store" || lower == ".apple.double" || base.starts_with("._") {
        return Some(
            "macOS Finder metadata: lists file names of the directory, including ones not in the repository",
        );
    }
    if lower == "thumbs.db" || lower == "ehthumbs.db" || lower == "desktop.ini" {
        return Some(
            "Windows Explorer metadata: thumbnails and folder settings from the author's machine",
        );
    }
    if lpath.contains(".idea/workspace.xml") || lpath.ends_with(".idea/usage.statistics.xml") {
        return Some(
            "JetBrains workspace file: absolute paths, open files and run configurations of one developer",
        );
    }
    if lower.ends_with(".pyc") || lpath.contains("__pycache__/") {
        return Some(
            "Compiled Python: contains the absolute source path of the machine it was built on",
        );
    }
    if lower.ends_with(".swp") || lower.ends_with(".swo") || lower.ends_with('~') {
        return Some(
            "Editor swap or backup file: a copy of a file being edited, often with the user name and host inside",
        );
    }
    if lower.ends_with(".orig") || lower.ends_with(".rej") {
        return Some("Merge leftover: an older version of a file that was meant to disappear");
    }
    if lower.ends_with(".log")
        || lower == "npm-debug.log"
        || lower == "yarn-error.log"
        || lower == "nohup.out"
    {
        return Some("Log file: time stamps, paths, host names and sometimes tokens of a real run");
    }
    if lower.ends_with(".dmp")
        || lower.ends_with(".hprof")
        || lower.ends_with(".stackdump")
        || lower.ends_with(".mdmp")
        || (lower.starts_with("core.") && lower[5..].chars().all(|c| c.is_ascii_digit()))
    {
        return Some(
            "Crash dump or heap dump: raw process memory, which can hold credentials and user data",
        );
    }
    if lpath.contains(".history/") || lpath.starts_with(".history/") {
        return Some("Editor local-history folder: every saved version of edited files");
    }
    if lower.ends_with(".bak") || lower.ends_with(".backup") || lower.ends_with(".old") {
        return Some(
            "Backup copy of a file: usually an older version with whatever it contained at the time",
        );
    }
    None
}

const WHY_EXIF: &str = "Camera metadata survives in committed images: GPS coordinates place the photographer, serial numbers and owner names identify the device and the person.";
const WHY_DOC: &str = "Document properties name the author, the last editor, the company and the tool version, and they stay in the file through every copy and export.";
const WHY_BIN: &str = "Compiled artifacts embed the absolute paths of the machine that built them: user name, project layout, sometimes the toolchain version.";

fn metadata_for(path: &str, data: &[u8]) -> Option<MetaFinding> {
    let lower = path.to_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    let (kind, fields, why): (&'static str, Vec<(String, String)>, &'static str) = match ext {
        "jpg" | "jpeg" | "jpe" => ("JPEG", jpeg_fields(data), WHY_EXIF),
        "png" => ("PNG", png_fields(data), WHY_EXIF),
        "tif" | "tiff" => ("TIFF", exif_fields(data), WHY_EXIF),
        "pdf" => ("PDF", pdf_fields(data), WHY_DOC),
        "docx" | "xlsx" | "pptx" | "docm" | "xlsm" | "pptm" | "dotx" | "xltx" | "potx" => {
            ("Office document", office_fields(data), WHY_DOC)
        }
        "odt" | "ods" | "odp" | "odg" => ("OpenDocument", office_fields(data), WHY_DOC),
        _ => return None,
    };
    if fields.is_empty() {
        return None;
    }
    let severity = if fields.iter().any(|(k, _)| k.starts_with("GPS")) {
        Severity::High
    } else if fields.iter().any(|(k, _)| {
        matches!(
            k.as_str(),
            "Artist"
                | "Camera owner"
                | "Body serial number"
                | "Lens serial number"
                | "Author"
                | "Creator"
                | "Last modified by"
                | "Company"
                | "Manager"
                | "Initial creator"
                | "Owner"
                | "Serial number"
        )
    }) {
        Severity::Medium
    } else {
        Severity::Low
    };
    Some(MetaFinding {
        path: path.to_owned(),
        kind,
        severity,
        size: data.len() as u64,
        fields,
        why,
    })
}

fn local_path_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)((?:[a-z]:\\users\\|/users/|/home/|/export/home/)[a-z0-9._\-]{2,32}[\\/][^\s\x00]{0,120})")
            .unwrap()
    })
}

fn is_binary_ext(path: &str) -> bool {
    let lower = path.to_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    matches!(
        ext,
        "exe"
            | "dll"
            | "so"
            | "dylib"
            | "pdb"
            | "o"
            | "a"
            | "lib"
            | "obj"
            | "bin"
            | "wasm"
            | "pyc"
            | "class"
            | "jar"
            | "apk"
            | "aab"
            | "ipa"
            | "dex"
            | "elf"
            | "node"
            | "pyd"
    )
}

// ---------------------------------------------------------------------------
// Archaeology
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct StashSummary {
    pub index: usize,
    pub id: Oid,
    pub message: String,
    pub time: i64,
    pub findings: usize,
}

#[derive(Clone, Debug)]
pub struct BranchNote {
    pub name: String,
    pub tip: Oid,
    pub time: i64,
    pub remote: bool,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct DeletedFile {
    pub path: String,
    pub rule: &'static FileRule,
    pub commit: CommitRef,
    pub pushed: bool,
}

impl DeletedFile {
    /// Command that brings the file back from the commit before the deletion.
    pub fn resurrect(&self) -> String {
        format!("git show {}^:\"{}\"", self.commit.id, self.path)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Archaeology {
    /// Commits only the reflog still reaches (amended, rebased, reset away).
    pub rewritten: Vec<CommitRef>,
    pub rewritten_total: usize,
    pub stashes: Vec<StashSummary>,
    pub stash_findings: Vec<Finding>,
    pub backup_branches: Vec<BranchNote>,
    pub deleted_files: Vec<DeletedFile>,
    /// Non-sample hooks in .git/hooks (local only).
    pub local_hooks: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct PrRefs {
    pub remote: String,
    pub total: usize,
    /// Heads not reachable from any branch this clone knows.
    pub unknown_heads: usize,
    /// `(number, sha)` of some of those.
    pub sample: Vec<(u64, String)>,
    pub elapsed_ms: u64,
    pub command: String,
}

#[derive(Clone, Debug, Default)]
pub struct Unreachable {
    pub commits: Vec<CommitRef>,
    pub commit_count: usize,
    pub blobs: usize,
    pub blob_bytes: u64,
    pub trees: usize,
    pub scanned_blobs: usize,
    pub scanned_bytes: u64,
    pub findings: Vec<Finding>,
    pub elapsed_ms: u64,
    pub cancelled: bool,
    pub commands: Vec<String>,
}

fn backup_branch_reason(name: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)(?:^|[/_\-.])(backup|bak|old|wip|tmp|temp|save|saved|archive|archived|junk|scratch|trash|experiment|experimental|try|dump|copy|orig|original|legacy|unused|deprecated|private|secret|hack|hacks|local|mine|personal|stash|snapshot|before|pre|debug)(?:[/_\-.]|$|\d)")
            .unwrap()
    });
    let short = name.split('/').next_back().unwrap_or(name);
    if short == "master" || short == "main" || short == "HEAD" || short == "develop" {
        return None;
    }
    re.captures(name)
        .map(|c| format!("name contains “{}”", c[1].to_lowercase()))
}

/// Commit details for a list of SHAs, through one `git log --no-walk`.
fn commit_refs(workdir: &Path, shas: &[String], commands: &mut Vec<String>) -> Vec<CommitRef> {
    if shas.is_empty() {
        return Vec::new();
    }
    let stdin = shas.join("\n") + "\n";
    let args = [
        "log",
        "--no-walk=unsorted",
        "--stdin",
        "--format=%H%x00%ct%x00%an%x00%s",
    ];
    commands.push(format!(
        "git {}   # {} sha(s) on stdin",
        args.join(" "),
        shas.len()
    ));
    let Ok(out) = cli::run(workdir, &args, Some(stdin.as_bytes())) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut p = l.split('\0');
            let id = Oid::from_str(p.next()?).ok()?;
            Some(CommitRef {
                id,
                time: p.next()?.parse().ok()?,
                author: p.next().unwrap_or("").to_owned(),
                summary: p.next().unwrap_or("").to_owned(),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Supply chain
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitDep {
    pub file: String,
    pub line: u32,
    pub name: String,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Package {
    /// osv.dev ecosystem name: `npm`, `crates.io`, `PyPI`, `Go`, `RubyGems`, `Packagist`.
    pub ecosystem: &'static str,
    pub name: String,
    pub version: String,
    pub file: String,
}

#[derive(Clone, Debug)]
pub struct HookFinding {
    pub path: String,
    pub kind: &'static str,
    pub detail: String,
    pub severity: Severity,
}

#[derive(Clone, Debug, Default)]
pub struct Supply {
    pub git_deps: Vec<GitDep>,
    pub packages: Vec<Package>,
    /// `(file, packages)` per lockfile or manifest read.
    pub lockfiles: Vec<(String, usize)>,
    pub hooks: Vec<HookFinding>,
}

#[derive(Clone, Debug)]
pub struct Vuln {
    pub package: Package,
    pub id: String,
    pub aliases: Vec<String>,
    pub summary: String,
    /// `CRITICAL` … `LOW`, or a CVSS vector/score, when known.
    pub severity: String,
    pub url: String,
}

fn git_source(value: &str) -> bool {
    let v = value.trim().trim_matches(|c| c == '"' || c == '\'');
    v.starts_with("git+")
        || v.starts_with("git://")
        || v.starts_with("github:")
        || v.starts_with("gitlab:")
        || v.starts_with("bitbucket:")
        || v.starts_with("gist:")
        || v.starts_with("git@")
        || (v.starts_with("https://github.com/") && !v.contains("/releases/download/"))
        || v.starts_with("https://gitlab.com/")
        || v.starts_with("https://bitbucket.org/")
        || v.ends_with(".git")
        || v.contains(".git#")
}

fn parse_package_json(path: &str, text: &str, out: &mut Supply) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    for section in [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ] {
        if let Some(obj) = v.get(section).and_then(|d| d.as_object()) {
            for (name, spec) in obj {
                if let Some(s) = spec.as_str()
                    && git_source(s)
                {
                    let line = text
                        .lines()
                        .position(|l| l.contains(&format!("\"{name}\"")) && l.contains(s))
                        .map(|i| i as u32 + 1)
                        .unwrap_or(0);
                    out.git_deps.push(GitDep {
                        file: path.to_owned(),
                        line,
                        name: name.clone(),
                        source: s.to_owned(),
                    });
                }
            }
        }
    }
}

fn parse_package_lock(path: &str, text: &str, out: &mut Supply) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    let mut n = 0;
    if let Some(pk) = v.get("packages").and_then(|p| p.as_object()) {
        for (key, info) in pk {
            let Some(idx) = key.rfind("node_modules/") else {
                continue;
            };
            let name = &key[idx + "node_modules/".len()..];
            let Some(version) = info.get("version").and_then(|x| x.as_str()) else {
                continue;
            };
            if let Some(res) = info.get("resolved").and_then(|x| x.as_str())
                && git_source(res)
            {
                out.git_deps.push(GitDep {
                    file: path.to_owned(),
                    line: 0,
                    name: name.to_owned(),
                    source: res.to_owned(),
                });
            }
            out.packages.push(Package {
                ecosystem: "npm",
                name: name.to_owned(),
                version: version.to_owned(),
                file: path.to_owned(),
            });
            n += 1;
        }
    } else if let Some(deps) = v.get("dependencies").and_then(|p| p.as_object()) {
        fn walk(
            path: &str,
            deps: &serde_json::Map<String, serde_json::Value>,
            out: &mut Supply,
            n: &mut usize,
        ) {
            for (name, info) in deps {
                if let Some(version) = info.get("version").and_then(|x| x.as_str()) {
                    if git_source(version) {
                        out.git_deps.push(GitDep {
                            file: path.to_owned(),
                            line: 0,
                            name: name.clone(),
                            source: version.to_owned(),
                        });
                    } else {
                        out.packages.push(Package {
                            ecosystem: "npm",
                            name: name.clone(),
                            version: version.to_owned(),
                            file: path.to_owned(),
                        });
                        *n += 1;
                    }
                }
                if let Some(sub) = info.get("dependencies").and_then(|d| d.as_object()) {
                    walk(path, sub, out, n);
                }
            }
        }
        walk(path, deps, out, &mut n);
    }
    out.lockfiles.push((path.to_owned(), n));
}

fn parse_yarn_lock(path: &str, text: &str, out: &mut Supply) {
    let mut n = 0;
    let mut current: Option<String> = None;
    for line in text.lines() {
        if !line.starts_with(' ') && !line.starts_with('#') && line.ends_with(':') {
            // "@scope/name@^1.0.0", name@^1:
            let first = line.trim_end_matches(':').split(',').next().unwrap_or("");
            let first = first.trim().trim_matches('"');
            let name = match first.rfind('@') {
                Some(i) if i > 0 => &first[..i],
                _ => first,
            };
            current = Some(name.to_owned());
        } else if let Some(name) = &current {
            let t = line.trim();
            if let Some(v) = t.strip_prefix("version ") {
                let v = v.trim().trim_matches('"');
                out.packages.push(Package {
                    ecosystem: "npm",
                    name: name.clone(),
                    version: v.to_owned(),
                    file: path.to_owned(),
                });
                n += 1;
            } else if let Some(r) = t.strip_prefix("resolved ")
                && git_source(r.trim().trim_matches('"'))
            {
                out.git_deps.push(GitDep {
                    file: path.to_owned(),
                    line: 0,
                    name: name.clone(),
                    source: r.trim().trim_matches('"').to_owned(),
                });
            }
        }
    }
    out.lockfiles.push((path.to_owned(), n));
}

fn parse_pnpm_lock(path: &str, text: &str, out: &mut Supply) {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE
        .get_or_init(|| Regex::new(r"^\s{2}'?/?(@?[^@'\s/]+(?:/[^@'\s]+)?)@([^(:'\s]+)").unwrap());
    let mut n = 0;
    let mut in_packages = false;
    for line in text.lines() {
        if line.starts_with("packages:") {
            in_packages = true;
            continue;
        }
        if in_packages && !line.starts_with(' ') && !line.is_empty() {
            in_packages = false;
        }
        if in_packages && let Some(c) = re.captures(line) {
            out.packages.push(Package {
                ecosystem: "npm",
                name: c[1].to_owned(),
                version: c[2].to_owned(),
                file: path.to_owned(),
            });
            n += 1;
        }
    }
    out.lockfiles.push((path.to_owned(), n));
}

fn parse_cargo_lock(path: &str, text: &str, out: &mut Supply) {
    let mut n = 0;
    let (mut name, mut version, mut source) = (String::new(), String::new(), String::new());
    let flush = |name: &mut String,
                 version: &mut String,
                 source: &mut String,
                 out: &mut Supply,
                 n: &mut usize| {
        if !name.is_empty() && !version.is_empty() {
            if source.starts_with("git+") {
                out.git_deps.push(GitDep {
                    file: path.to_owned(),
                    line: 0,
                    name: name.clone(),
                    source: source.clone(),
                });
            } else if source.is_empty() || source.contains("crates.io") {
                out.packages.push(Package {
                    ecosystem: "crates.io",
                    name: name.clone(),
                    version: version.clone(),
                    file: path.to_owned(),
                });
                *n += 1;
            }
        }
        name.clear();
        version.clear();
        source.clear();
    };
    for line in text.lines() {
        let t = line.trim();
        if t == "[[package]]" {
            flush(&mut name, &mut version, &mut source, out, &mut n);
        } else if let Some(v) = t.strip_prefix("name = ") {
            name = v.trim_matches('"').to_owned();
        } else if let Some(v) = t.strip_prefix("version = ") {
            version = v.trim_matches('"').to_owned();
        } else if let Some(v) = t.strip_prefix("source = ") {
            source = v.trim_matches('"').to_owned();
        }
    }
    flush(&mut name, &mut version, &mut source, out, &mut n);
    out.lockfiles.push((path.to_owned(), n));
}

fn parse_cargo_toml(path: &str, text: &str, out: &mut Supply) {
    let mut table_name: Option<String> = None;
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.starts_with('[') {
            table_name = t
                .trim_matches(['[', ']'])
                .rsplit('.')
                .next()
                .map(|s| s.to_owned());
        }
        if let Some(pos) = t.find("git = ") {
            let source = t[pos + 6..]
                .split(',')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches(['"', '}', ' '])
                .to_owned();
            let name = if t.starts_with("git = ") {
                table_name.clone().unwrap_or_default()
            } else {
                t.split('=').next().unwrap_or("").trim().to_owned()
            };
            out.git_deps.push(GitDep {
                file: path.to_owned(),
                line: i as u32 + 1,
                name,
                source,
            });
        }
    }
}

fn parse_requirements(path: &str, text: &str, out: &mut Supply) {
    let mut n = 0;
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with('-') {
            continue;
        }
        if t.contains("git+") || t.contains("@ git") {
            out.git_deps.push(GitDep {
                file: path.to_owned(),
                line: i as u32 + 1,
                name: t.split(['@', ' ', '#']).next().unwrap_or(t).to_owned(),
                source: t.to_owned(),
            });
            continue;
        }
        if let Some((name, ver)) = t.split_once("==") {
            let ver = ver.split([';', ' ', '#', '\\']).next().unwrap_or("").trim();
            let name = name.split('[').next().unwrap_or(name).trim();
            if !ver.is_empty() && !name.is_empty() {
                out.packages.push(Package {
                    ecosystem: "PyPI",
                    name: name.to_lowercase(),
                    version: ver.to_owned(),
                    file: path.to_owned(),
                });
                n += 1;
            }
        }
    }
    if n > 0 {
        out.lockfiles.push((path.to_owned(), n));
    }
}

fn parse_pipfile_lock(path: &str, text: &str, out: &mut Supply) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    let mut n = 0;
    for section in ["default", "develop"] {
        if let Some(obj) = v.get(section).and_then(|d| d.as_object()) {
            for (name, info) in obj {
                if let Some(ver) = info.get("version").and_then(|x| x.as_str()) {
                    out.packages.push(Package {
                        ecosystem: "PyPI",
                        name: name.to_lowercase(),
                        version: ver.trim_start_matches("==").to_owned(),
                        file: path.to_owned(),
                    });
                    n += 1;
                } else if let Some(g) = info.get("git").and_then(|x| x.as_str()) {
                    out.git_deps.push(GitDep {
                        file: path.to_owned(),
                        line: 0,
                        name: name.clone(),
                        source: g.to_owned(),
                    });
                }
            }
        }
    }
    out.lockfiles.push((path.to_owned(), n));
}

fn parse_go_mod(path: &str, text: &str, out: &mut Supply) {
    let mut n = 0;
    let mut in_require = false;
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.starts_with("require (") {
            in_require = true;
            continue;
        }
        if t == ")" {
            in_require = false;
            continue;
        }
        if let Some(rest) = t.strip_prefix("replace ")
            && let Some((_, to)) = rest.split_once("=>")
        {
            let to = to.trim();
            if !to.starts_with('.') && !to.starts_with('/') {
                out.git_deps.push(GitDep {
                    file: path.to_owned(),
                    line: i as u32 + 1,
                    name: rest
                        .split_once("=>")
                        .map(|(a, _)| a.trim().to_owned())
                        .unwrap_or_default(),
                    source: to.to_owned(),
                });
            }
            continue;
        }
        let req = t.strip_prefix("require ").unwrap_or(t);
        if in_require || t.starts_with("require ") {
            let mut parts = req.split_whitespace();
            if let (Some(name), Some(ver)) = (parts.next(), parts.next())
                && ver.starts_with('v')
            {
                out.packages.push(Package {
                    ecosystem: "Go",
                    name: name.to_owned(),
                    version: ver.trim_start_matches('v').to_owned(),
                    file: path.to_owned(),
                });
                n += 1;
            }
        }
    }
    if n > 0 {
        out.lockfiles.push((path.to_owned(), n));
    }
}

fn parse_gemfile(path: &str, text: &str, out: &mut Supply) {
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.starts_with("gem ") && (t.contains("git:") || t.contains("github:")) {
            out.git_deps.push(GitDep {
                file: path.to_owned(),
                line: i as u32 + 1,
                name: t.split(['\'', '"']).nth(1).unwrap_or("").to_owned(),
                source: t.to_owned(),
            });
        }
    }
}

fn parse_gemfile_lock(path: &str, text: &str, out: &mut Supply) {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^    ([A-Za-z0-9_\-.]+) \(([^)]+)\)$").unwrap());
    let mut n = 0;
    for line in text.lines() {
        if let Some(c) = re.captures(line) {
            out.packages.push(Package {
                ecosystem: "RubyGems",
                name: c[1].to_owned(),
                version: c[2].split('-').next().unwrap_or(&c[2]).to_owned(),
                file: path.to_owned(),
            });
            n += 1;
        }
    }
    out.lockfiles.push((path.to_owned(), n));
}

fn parse_composer_lock(path: &str, text: &str, out: &mut Supply) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    let mut n = 0;
    for section in ["packages", "packages-dev"] {
        if let Some(arr) = v.get(section).and_then(|d| d.as_array()) {
            for p in arr {
                if let (Some(name), Some(ver)) = (
                    p.get("name").and_then(|x| x.as_str()),
                    p.get("version").and_then(|x| x.as_str()),
                ) {
                    out.packages.push(Package {
                        ecosystem: "Packagist",
                        name: name.to_owned(),
                        version: ver.trim_start_matches('v').to_owned(),
                        file: path.to_owned(),
                    });
                    n += 1;
                }
            }
        }
    }
    out.lockfiles.push((path.to_owned(), n));
}

fn parse_pyproject(path: &str, text: &str, out: &mut Supply) {
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.contains("git = \"") || t.contains("git+") {
            out.git_deps.push(GitDep {
                file: path.to_owned(),
                line: i as u32 + 1,
                name: t.split('=').next().unwrap_or("").trim().to_owned(),
                source: t.to_owned(),
            });
        }
    }
}

/// Parse one manifest or lockfile by name.
pub fn parse_manifest(path: &str, text: &str, out: &mut Supply) {
    let base = secrets::basename(path);
    match base {
        "package.json" => parse_package_json(path, text, out),
        "package-lock.json" | "npm-shrinkwrap.json" => parse_package_lock(path, text, out),
        "yarn.lock" => parse_yarn_lock(path, text, out),
        "pnpm-lock.yaml" => parse_pnpm_lock(path, text, out),
        "Cargo.lock" => parse_cargo_lock(path, text, out),
        "Cargo.toml" => parse_cargo_toml(path, text, out),
        "Pipfile.lock" => parse_pipfile_lock(path, text, out),
        "go.mod" => parse_go_mod(path, text, out),
        "Gemfile" => parse_gemfile(path, text, out),
        "Gemfile.lock" => parse_gemfile_lock(path, text, out),
        "composer.lock" => parse_composer_lock(path, text, out),
        "pyproject.toml" => parse_pyproject(path, text, out),
        _ if base.starts_with("requirements") && base.ends_with(".txt") => {
            parse_requirements(path, text, out)
        }
        _ if base == "constraints.txt" => parse_requirements(path, text, out),
        _ => {}
    }
}

fn is_manifest(path: &str) -> bool {
    let base = secrets::basename(path);
    matches!(
        base,
        "package.json"
            | "package-lock.json"
            | "npm-shrinkwrap.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "Cargo.lock"
            | "Cargo.toml"
            | "Pipfile.lock"
            | "go.mod"
            | "Gemfile"
            | "Gemfile.lock"
            | "composer.lock"
            | "pyproject.toml"
            | "constraints.txt"
    ) || (base.starts_with("requirements") && base.ends_with(".txt"))
}

fn is_vendored(path: &str) -> bool {
    let l = path.to_lowercase();
    l.contains("node_modules/")
        || l.contains("/vendor/")
        || l.starts_with("vendor/")
        || l.contains("third_party/")
        || l.contains("/.cargo/")
}

/// Query osv.dev for known vulnerabilities of `packages` through `curl`.
/// Network access happens only here, and only when asked.
pub fn osv_check(
    packages: &[Package],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<Vec<Vuln>> {
    let mut vulns: Vec<Vuln> = Vec::new();
    let mut ids: Vec<(String, Package)> = Vec::new();
    let total = packages.len();
    for (ci, chunk) in packages.chunks(500).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        progress(&Progress {
            phase: "Querying osv.dev".into(),
            done: ci * 500,
            total: Some(total),
            bytes: 0,
            findings: ids.len(),
        });
        let queries: Vec<serde_json::Value> = chunk
            .iter()
            .map(|p| {
                serde_json::json!({
                    "package": {"name": p.name, "ecosystem": p.ecosystem},
                    "version": p.version,
                })
            })
            .collect();
        let body = serde_json::json!({ "queries": queries }).to_string();
        let out = cli::run_program(
            "curl",
            Path::new("."),
            &[
                "-sS",
                "--max-time",
                "90",
                "-H",
                "Content-Type: application/json",
                "-X",
                "POST",
                "--data-binary",
                "@-",
                "https://api.osv.dev/v1/querybatch",
            ],
            Some(body.as_bytes()),
        )
        .map_err(|e| Error::new(format!("curl to api.osv.dev failed: {e}")))?;
        let v: serde_json::Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| Error::new(format!("osv.dev answer: {e}")))?;
        if let Some(results) = v.get("results").and_then(|r| r.as_array()) {
            for (i, r) in results.iter().enumerate() {
                if let Some(vs) = r.get("vulns").and_then(|x| x.as_array()) {
                    for vu in vs {
                        if let Some(id) = vu.get("id").and_then(|x| x.as_str()) {
                            ids.push((id.to_owned(), chunk[i].clone()));
                        }
                    }
                }
            }
        }
    }
    // Details for the first few dozen distinct ids.
    let mut details: HashMap<String, (String, Vec<String>, String)> = HashMap::new();
    let mut distinct: Vec<String> = ids.iter().map(|(id, _)| id.clone()).collect();
    distinct.sort();
    distinct.dedup();
    for (i, id) in distinct.iter().take(80).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        progress(&Progress {
            phase: "Reading advisories".into(),
            done: i,
            total: Some(distinct.len().min(80)),
            bytes: 0,
            findings: ids.len(),
        });
        let Ok(out) = cli::run_program(
            "curl",
            Path::new("."),
            &[
                "-sS",
                "--max-time",
                "30",
                &format!("https://api.osv.dev/v1/vulns/{id}"),
            ],
            None,
        ) else {
            continue;
        };
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
            let summary = v
                .get("summary")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_owned();
            let aliases: Vec<String> = v
                .get("aliases")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let severity = v
                .get("database_specific")
                .and_then(|d| d.get("severity"))
                .and_then(|s| s.as_str())
                .map(str::to_owned)
                .or_else(|| {
                    v.get("severity")
                        .and_then(|s| s.as_array())
                        .and_then(|a| a.first())
                        .and_then(|s| s.get("score"))
                        .and_then(|s| s.as_str())
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            details.insert(id.clone(), (summary, aliases, severity));
        }
    }
    for (id, package) in ids {
        let (summary, aliases, severity) = details.get(&id).cloned().unwrap_or_default();
        vulns.push(Vuln {
            package,
            url: format!("https://osv.dev/vulnerability/{id}"),
            id,
            aliases,
            summary,
            severity,
        });
    }
    vulns.sort_by(|a, b| {
        severity_rank(&b.severity)
            .cmp(&severity_rank(&a.severity))
            .then_with(|| a.package.name.cmp(&b.package.name))
    });
    Ok(vulns)
}

fn severity_rank(s: &str) -> u8 {
    match s.to_uppercase().as_str() {
        "CRITICAL" => 4,
        "HIGH" => 3,
        "MODERATE" | "MEDIUM" => 2,
        "LOW" => 1,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// CODEOWNERS
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct CodeOwners {
    pub path: String,
    pub rules: usize,
    /// Owners with the number of rules naming them, most named first.
    pub owners: Vec<(String, usize)>,
}

pub fn parse_codeowners(path: &str, text: &str) -> CodeOwners {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut rules = 0;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let mut parts = t.split_whitespace();
        let _pattern = parts.next();
        let mut any = false;
        for owner in parts {
            if owner.starts_with('@') || owner.contains('@') {
                *counts.entry(owner.to_owned()).or_insert(0) += 1;
                any = true;
            }
        }
        if any {
            rules += 1;
        }
    }
    let mut owners: Vec<(String, usize)> = counts.into_iter().collect();
    owners.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    CodeOwners {
        path: path.to_owned(),
        rules,
        owners,
    }
}

// ---------------------------------------------------------------------------
// The analysis
// ---------------------------------------------------------------------------

fn config_value(workdir: &Path, key: &str) -> String {
    cli::run(workdir, &["config", "--get", key], None)
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default()
}

fn git_log_identities(
    workdir: &Path,
    opts: &OsintOptions,
    pushed: &HashSet<Oid>,
    commands: &mut Vec<String>,
    cancel: &AtomicBool,
) -> Result<(People, Messages, usize)> {
    let mut args: Vec<String> = vec![
        "log".into(),
        "--format=%x1e%H%x1f%an%x1f%ae%x1f%cn%x1f%ce%x1f%at%x1f%ai%x1f%ct%x1f%B".into(),
    ];
    if opts.max_commits > 0 {
        args.push("-n".into());
        args.push(opts.max_commits.to_string());
    }
    if opts.all_refs {
        args.extend(["--branches".into(), "--remotes".into(), "--tags".into()]);
    } else {
        args.push("HEAD".into());
    }
    commands.push(format!("git {}", args.join(" ")));
    let out = cli::run(workdir, &args, None)?;
    let text = String::from_utf8_lossy(&out.stdout);

    struct Acc {
        names: HashMap<String, usize>,
        commits: usize,
        committed: usize,
        pushed: usize,
        first: i64,
        last: i64,
        offsets: HashMap<i32, usize>,
        heat: [[u32; 24]; 7],
        weekend: usize,
        night: usize,
    }
    let mut accs: HashMap<String, Acc> = HashMap::new();
    let mut by_name: HashMap<String, HashSet<String>> = HashMap::new();
    let mut trailers: HashMap<String, HashMap<String, usize>> = HashMap::new();
    let mut tickets: HashMap<String, (usize, Vec<String>)> = HashMap::new();
    let mut domains: HashMap<String, usize> = HashMap::new();
    let mut internal_urls: Vec<(String, CommitRef)> = Vec::new();
    let mut incidents: Vec<Incident> = Vec::new();
    let trailer_re = Regex::new(
        r"(?im)^(Co-authored-by|Signed-off-by|Reviewed-by|Acked-by|Tested-by|Reported-by|Suggested-by|Helped-by|Mentored-by|Cc|Reviewed-on|Change-Id|Bug|Fixes|Closes|Resolves|See-also|Ticket|Issue|Jira)\s*:\s*(.+?)\s*$",
    )
    .unwrap();
    let ticket_re = Regex::new(r"\b([A-Z][A-Z0-9]{1,9})-(\d{1,6})\b").unwrap();
    let url_re = Regex::new(r"https?://([A-Za-z0-9.\-]+)(?::\d+)?(/[^\s)>\]]*)?").unwrap();
    let internal_host = |h: &str| {
        let l = h.to_lowercase();
        l.ends_with(".corp")
            || l.ends_with(".internal")
            || l.ends_with(".local")
            || l.ends_with(".lan")
            || l.ends_with(".intranet")
            || l.ends_with(".private")
            || l.ends_with(".intra")
            || l == "localhost"
            || l.starts_with("10.")
            || l.starts_with("192.168.")
            || (l.starts_with("172.")
                && l.split('.')
                    .nth(1)
                    .and_then(|p| p.parse::<u8>().ok())
                    .is_some_and(|p| (16..=31).contains(&p)))
            || (!l.contains('.') && l != "localhost")
    };
    let mut commits = 0usize;
    for (i, rec) in text.split('\x1e').enumerate() {
        if rec.is_empty() {
            continue;
        }
        if i % 2000 == 0 && cancel.load(Ordering::Relaxed) {
            break;
        }
        let mut p = rec.splitn(9, '\x1f');
        let (
            Some(sha),
            Some(an),
            Some(ae),
            Some(cn),
            Some(ce),
            Some(at),
            Some(ai),
            Some(ct),
            Some(body),
        ) = (
            p.next(),
            p.next(),
            p.next(),
            p.next(),
            p.next(),
            p.next(),
            p.next(),
            p.next(),
            p.next(),
        )
        else {
            continue;
        };
        let Ok(id) = Oid::from_str(sha.trim()) else {
            continue;
        };
        let at: i64 = at.parse().unwrap_or(0);
        let _ct: i64 = ct.parse().unwrap_or(0);
        let offset = ai.rsplit(' ').next().and_then(parse_offset).unwrap_or(0);
        commits += 1;
        let is_pushed = pushed.contains(&id);
        let email = ae.trim().to_lowercase();
        let acc = accs.entry(email.clone()).or_insert_with(|| Acc {
            names: HashMap::new(),
            commits: 0,
            committed: 0,
            pushed: 0,
            first: at,
            last: at,
            offsets: HashMap::new(),
            heat: [[0; 24]; 7],
            weekend: 0,
            night: 0,
        });
        acc.commits += 1;
        if is_pushed {
            acc.pushed += 1;
        }
        *acc.names.entry(an.trim().to_owned()).or_insert(0) += 1;
        acc.first = acc.first.min(at);
        acc.last = acc.last.max(at);
        *acc.offsets.entry(offset).or_insert(0) += 1;
        let (wd, hour) = local_slot(at, offset);
        acc.heat[wd][hour] += 1;
        if wd >= 5 {
            acc.weekend += 1;
        }
        if !(6..22).contains(&hour) {
            acc.night += 1;
        }
        by_name
            .entry(an.trim().to_lowercase())
            .or_default()
            .insert(email.clone());
        let cemail = ce.trim().to_lowercase();
        if cemail != email || cn.trim() != an.trim() {
            let cacc = accs.entry(cemail).or_insert_with(|| Acc {
                names: HashMap::new(),
                commits: 0,
                committed: 0,
                pushed: 0,
                first: at,
                last: at,
                offsets: HashMap::new(),
                heat: [[0; 24]; 7],
                weekend: 0,
                night: 0,
            });
            cacc.committed += 1;
            *cacc.names.entry(cn.trim().to_owned()).or_insert(0) += 0;
        } else {
            acc.committed += 1;
        }

        // Message intelligence.
        let summary = body.lines().next().unwrap_or("").trim().to_owned();
        let commit = CommitRef {
            id,
            time: at,
            author: an.trim().to_owned(),
            summary: summary.clone(),
        };
        for c in trailer_re.captures_iter(body) {
            let kind = c[1].to_string();
            let kind = kind[..1].to_uppercase() + &kind[1..].to_lowercase();
            *trailers
                .entry(kind)
                .or_default()
                .entry(c[2].trim().to_owned())
                .or_insert(0) += 1;
        }
        for c in ticket_re.captures_iter(body) {
            let key = &c[1];
            if TICKET_EXCLUDE.contains(&key) || key.chars().all(|ch| ch.is_ascii_digit()) {
                continue;
            }
            let e = tickets.entry(key.to_owned()).or_insert((0, Vec::new()));
            e.0 += 1;
            if e.1.len() < 5 && !e.1.iter().any(|s| s == &c[0]) {
                e.1.push(c[0].to_owned());
            }
        }
        for c in url_re.captures_iter(body) {
            let host = c[1].to_lowercase();
            *domains.entry(host.clone()).or_insert(0) += 1;
            if internal_host(&host) && internal_urls.len() < 200 {
                internal_urls.push((c[0].to_owned(), commit.clone()));
            }
        }
        if incidents.len() < 500 {
            for rule in incident_rules() {
                if rule.regex.is_match(body) {
                    incidents.push(Incident {
                        commit: commit.clone(),
                        reason: rule.reason,
                        pushed: is_pushed,
                    });
                    break;
                }
            }
        }
    }

    let mut identities: Vec<Identity> = accs
        .into_iter()
        .map(|(email, a)| {
            let domain = email.split('@').nth(1).unwrap_or("").to_owned();
            let mut names: Vec<(String, usize)> = a.names.into_iter().collect();
            names.sort_by(|x, y| y.1.cmp(&x.1).then_with(|| x.0.cmp(&y.0)));
            let mut offsets: Vec<(i32, usize)> = a.offsets.into_iter().collect();
            offsets.sort_by_key(|x| std::cmp::Reverse(x.1));
            let mut aliases: Vec<String> = names
                .iter()
                .flat_map(|(n, _)| by_name.get(&n.to_lowercase()).cloned().unwrap_or_default())
                .filter(|e| *e != email)
                .collect();
            aliases.sort();
            aliases.dedup();
            Identity {
                kind: Some(email_kind(&email)),
                email,
                domain,
                names,
                commits: a.commits,
                committed: a.committed,
                pushed: a.pushed,
                first: a.first,
                last: a.last,
                offsets,
                heat: a.heat,
                weekend: a.weekend,
                night: a.night,
                aliases,
            }
        })
        .collect();
    identities.sort_by(|a, b| {
        (b.commits + b.committed)
            .cmp(&(a.commits + a.committed))
            .then_with(|| a.email.cmp(&b.email))
    });

    let mut trailer_list: Vec<Trailer> = trailers
        .into_iter()
        .map(|(kind, values)| {
            let total = values.values().sum();
            let mut values: Vec<(String, usize)> = values.into_iter().collect();
            values.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            values.truncate(50);
            Trailer {
                kind,
                total,
                values,
            }
        })
        .collect();
    trailer_list.sort_by_key(|t| std::cmp::Reverse(t.total));
    let mut ticket_list: Vec<(String, usize, Vec<String>)> = tickets
        .into_iter()
        .filter(|(_, (n, _))| *n >= 2)
        .map(|(k, (n, s))| (k, n, s))
        .collect();
    ticket_list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ticket_list.truncate(40);
    let mut domain_list: Vec<(String, usize)> = domains.into_iter().collect();
    domain_list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    domain_list.truncate(60);
    incidents.sort_by_key(|i| std::cmp::Reverse(i.commit.time));

    commands.push("git config --get user.email".into());
    let local_email = config_value(workdir, "user.email");
    let local_name = config_value(workdir, "user.name");
    let local_scope = if local_email.is_empty() {
        String::new()
    } else {
        let local_only = cli::run(workdir, &["config", "--local", "--get", "user.email"], None)
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false);
        if local_only { "repository" } else { "global" }.to_owned()
    };
    Ok((
        People {
            identities,
            total_commits: commits,
            local_name,
            local_email,
            local_scope,
        },
        Messages {
            scanned: commits,
            trailers: trailer_list,
            tickets: ticket_list,
            domains: domain_list,
            internal_urls,
            incidents,
        },
        commits,
    ))
}

fn archaeology(
    workdir: &Path,
    opts: &OsintOptions,
    pushed: &HashSet<Oid>,
    commands: &mut Vec<String>,
    cancel: &AtomicBool,
) -> Archaeology {
    let mut out = Archaeology::default();

    // Commits only the reflog reaches.
    commands.push("git rev-list --reflog --not --all".into());
    if let Ok(o) = cli::run(workdir, &["rev-list", "--reflog", "--not", "--all"], None) {
        let shas: Vec<String> = String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|l| l.trim().to_owned())
            .filter(|l| l.len() == 40)
            .collect();
        out.rewritten_total = shas.len();
        let head: Vec<String> = shas.into_iter().take(200).collect();
        out.rewritten = commit_refs(workdir, &head, commands);
    }

    // Stashes, with their content scanned for secrets.
    commands.push("git stash list --format=%gd%x00%H%x00%ct%x00%gs".into());
    if let Ok(o) = cli::run(
        workdir,
        &["stash", "list", "--format=%gd%x00%H%x00%ct%x00%gs"],
        None,
    ) {
        let rules = secrets::rules();
        let mut collector = Collector::new(rules);
        for (index, line) in String::from_utf8_lossy(&o.stdout)
            .lines()
            .enumerate()
            .take(50)
        {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let mut p = line.split('\0');
            let (Some(refname), Some(sha), Some(time), msg) =
                (p.next(), p.next(), p.next(), p.next())
            else {
                continue;
            };
            let Ok(id) = Oid::from_str(sha) else { continue };
            let time: i64 = time.parse().unwrap_or(0);
            let message = msg.unwrap_or("").to_owned();
            let args = [
                "stash",
                "show",
                "-p",
                "-U0",
                "--no-color",
                "--include-untracked",
                refname,
            ];
            commands.push(format!("git {}", args.join(" ")));
            let before = collector.len();
            let occurrences_before = count_occurrences(&collector);
            if let Ok(diff) = cli::run(workdir, &args, None) {
                let commit = CommitRef {
                    id,
                    time,
                    author: String::new(),
                    summary: message.clone(),
                };
                let mut sink = LineSink {
                    collector: &mut collector,
                    place: Place::Stash,
                    current: Some(commit),
                    commits: 0,
                    lines: 0,
                    new_files: Vec::new(),
                    report: None,
                };
                let cancel_flag = AtomicBool::new(false);
                let _ =
                    secrets::walk_patch(BufReader::new(&diff.stdout[..]), &cancel_flag, &mut sink);
            }
            let found =
                (collector.len() - before).max(count_occurrences(&collector) - occurrences_before);
            out.stashes.push(StashSummary {
                index,
                id,
                message,
                time,
                findings: found,
            });
        }
        out.stash_findings = collector.finish(None, pushed, false);
    }

    // Backup-looking branches.
    commands.push(
        "git for-each-ref --format=%(refname)%00%(objectname)%00%(committerdate:unix) refs/heads refs/remotes"
            .into(),
    );
    if let Ok(o) = cli::run(
        workdir,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)%00%(committerdate:unix)",
            "refs/heads",
            "refs/remotes",
        ],
        None,
    ) {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let mut p = line.split('\0');
            let (Some(refname), Some(sha), Some(time)) = (p.next(), p.next(), p.next()) else {
                continue;
            };
            let remote = refname.starts_with("refs/remotes/");
            let short = refname
                .strip_prefix("refs/heads/")
                .or_else(|| refname.strip_prefix("refs/remotes/"))
                .unwrap_or(refname);
            if short.ends_with("/HEAD") {
                continue;
            }
            if let Some(reason) = backup_branch_reason(short)
                && let Ok(tip) = Oid::from_str(sha)
            {
                out.backup_branches.push(BranchNote {
                    name: short.to_owned(),
                    tip,
                    time: time.parse().unwrap_or(0),
                    remote,
                    reason,
                });
            }
        }
        out.backup_branches
            .sort_by_key(|b| std::cmp::Reverse(b.time));
    }

    // Deleted sensitive files (resurrectable from the parent commit). Git
    // diffs every commit for this, so the newest commits only.
    let mut args: Vec<String> = vec![
        "log".into(),
        "--diff-filter=D".into(),
        "--name-only".into(),
        "--no-renames".into(),
        "--format=%x01%H%x00%ct%x00%an%x00%s".into(),
    ];
    let deleted_cap = if opts.max_commits == 0 {
        DELETED_SCAN_COMMITS
    } else {
        opts.max_commits.min(DELETED_SCAN_COMMITS)
    };
    args.push("-n".into());
    args.push(deleted_cap.to_string());
    if opts.all_refs {
        args.extend(["--branches".into(), "--remotes".into(), "--tags".into()]);
    } else {
        args.push("HEAD".into());
    }
    commands.push(format!("git {}", args.join(" ")));
    if let Ok(o) = cli::run(workdir, &args, None) {
        let text = String::from_utf8_lossy(&o.stdout);
        let mut current: Option<CommitRef> = None;
        let mut seen: HashSet<String> = HashSet::new();
        for line in text.lines() {
            if let Some(h) = line.strip_prefix('\u{1}') {
                let mut p = h.split('\0');
                current = (|| {
                    Some(CommitRef {
                        id: Oid::from_str(p.next()?).ok()?,
                        time: p.next()?.parse().ok()?,
                        author: p.next().unwrap_or("").to_owned(),
                        summary: p.next().unwrap_or("").to_owned(),
                    })
                })();
                continue;
            }
            let path = line.trim();
            if path.is_empty() {
                continue;
            }
            if let Some(rule) = check_path(path)
                && let Some(c) = &current
                && seen.insert(path.to_owned())
                && out.deleted_files.len() < 500
            {
                out.deleted_files.push(DeletedFile {
                    path: path.to_owned(),
                    rule,
                    pushed: pushed.contains(&c.id),
                    commit: c.clone(),
                });
            }
        }
    }

    // Local hooks.
    if let Ok(git_dir) = cli::run(workdir, &["rev-parse", "--git-path", "hooks"], None) {
        let hooks_dir = String::from_utf8_lossy(&git_dir.stdout).trim().to_owned();
        let hooks_path = if Path::new(&hooks_dir).is_absolute() {
            Path::new(&hooks_dir).to_path_buf()
        } else {
            workdir.join(&hooks_dir)
        };
        if let Ok(rd) = std::fs::read_dir(hooks_path) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if !name.ends_with(".sample") && e.path().is_file() {
                    out.local_hooks.push(name);
                }
            }
            out.local_hooks.sort();
        }
    }
    out
}

fn count_occurrences(c: &Collector) -> usize {
    c.occurrence_total()
}

fn supply_chain(workdir: &Path, tracked: &[String], commands: &mut Vec<String>) -> Supply {
    let mut out = Supply::default();
    for p in tracked.iter().filter(|p| is_manifest(p) && !is_vendored(p)) {
        if let Ok(meta) = std::fs::metadata(workdir.join(p))
            && meta.len() <= 20 * 1024 * 1024
            && let Ok(text) = std::fs::read_to_string(workdir.join(p))
        {
            parse_manifest(p, &text, &mut out);
        }
    }
    let mut seen = HashSet::new();
    out.packages
        .retain(|p| seen.insert((p.ecosystem, p.name.clone(), p.version.clone())));
    out.git_deps
        .sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
    out.git_deps.dedup();

    // Hooks and drivers.
    commands.push("git config --get core.hooksPath".into());
    let hooks_path = config_value(workdir, "core.hooksPath");
    if !hooks_path.is_empty() {
        out.hooks.push(HookFinding {
            path: hooks_path.clone(),
            kind: "hooks path",
            detail: format!("core.hooksPath = {hooks_path}: scripts in this directory run on commit, push and checkout."),
            severity: Severity::Low,
        });
    }
    let hook_dirs = [
        ".husky/",
        ".githooks/",
        "githooks/",
        ".git-hooks/",
        "git-hooks/",
        ".hooks/",
    ];
    let mut committed: HashMap<&str, Vec<String>> = HashMap::new();
    for p in tracked {
        for d in hook_dirs {
            if p.starts_with(d) && !p.ends_with(".gitignore") && !p.ends_with("README.md") {
                committed.entry(d).or_default().push(p.clone());
            }
        }
        if p == ".pre-commit-config.yaml" {
            out.hooks.push(HookFinding {
                path: p.clone(),
                kind: "pre-commit framework",
                detail: "pre-commit hooks are fetched from the repositories named in this file and run on every commit once installed.".into(),
                severity: Severity::Low,
            });
        }
        if p == ".lfsconfig" {
            out.hooks.push(HookFinding {
                path: p.clone(),
                kind: "LFS configuration",
                detail: "A committed .lfsconfig can point LFS at another server for everyone who clones.".into(),
                severity: Severity::Low,
            });
        }
    }
    for (d, files) in committed {
        out.hooks.push(HookFinding {
            path: d.to_owned(),
            kind: "committed hooks",
            detail: format!(
                "{} file(s): {}. They run on git actions for everyone who installs them (husky does so on `npm install`).",
                files.len(),
                files.iter().take(6).cloned().collect::<Vec<_>>().join(", ")
            ),
            severity: Severity::Low,
        });
    }
    if tracked.iter().any(|p| p == ".gitattributes")
        && let Ok(text) = std::fs::read_to_string(workdir.join(".gitattributes"))
    {
        let drivers: Vec<String> = text
            .lines()
            .filter(|l| l.contains("filter=") || l.contains("diff=") || l.contains("merge="))
            .map(|l| l.trim().to_owned())
            .take(10)
            .collect();
        if !drivers.is_empty() {
            commands.push("git config --get-regexp ^(filter|diff|merge)\\..*\\.(clean|smudge|command|driver|textconv)$".into());
            let configured = cli::run(
                workdir,
                &[
                    "config",
                    "--get-regexp",
                    r"^(filter|diff|merge)\..*\.(clean|smudge|command|driver|textconv)$",
                ],
                None,
            )
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default();
            out.hooks.push(HookFinding {
                path: ".gitattributes".into(),
                kind: "filter or diff drivers",
                detail: format!(
                    "{}\n{}",
                    drivers.join("\n"),
                    if configured.is_empty() {
                        "No driver commands are configured locally, so nothing runs here; a clone that configures them runs those commands on checkout.".to_owned()
                    } else {
                        format!("Configured commands:\n{configured}")
                    }
                ),
                severity: if configured.is_empty() {
                    Severity::Low
                } else {
                    Severity::Medium
                },
            });
        }
    }
    out
}

/// Run the analysis. Local git and file reads only; nothing touches the
/// network (osv.dev, GitHub and `ls-remote` are separate, on demand).
pub fn analyze(
    workdir: &Path,
    opts: &OsintOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<OsintReport> {
    let started = Instant::now();
    let mut commands = Vec::new();
    let mut report = OsintReport::default();
    let mut prog = Progress {
        phase: "Reading commits".into(),
        ..Default::default()
    };
    progress(&prog);

    let pushed = secrets::remote_commits(workdir, &mut commands);
    report.stats.pushed_commits = pushed.len();
    report.stats.head_pushed = secrets::head_oid(workdir).is_some_and(|h| pushed.contains(&h));
    let (people, messages, commits) =
        git_log_identities(workdir, opts, &pushed, &mut commands, cancel)?;
    report.people = people;
    report.messages = messages;
    report.stats.commits = commits;

    // Files.
    prog.phase = "Listing files".into();
    progress(&prog);
    let tracked = secrets::git_lines(workdir, &["ls-files", "-z"], &mut commands)?;
    let untracked = secrets::git_lines(
        workdir,
        &["ls-files", "-z", "--others", "--exclude-standard"],
        &mut commands,
    )?;
    report.stats.files = tracked.len() + untracked.len();
    if cancel.load(Ordering::Relaxed) {
        return finish_report(report, commands, started, true);
    }

    // Footprint: same parallel file scan as the secrets, other rules.
    let rules = footprint_rules();
    let mut collector = Collector::new(rules);
    let paths: Vec<(String, Place)> = tracked
        .iter()
        .map(|p| (p.clone(), Place::Worktree))
        .chain(untracked.iter().map(|p| (p.clone(), Place::Untracked)))
        .collect();
    let (bytes, _lines, _scanned, _binary, _large, _kind) = secrets::scan_files(
        workdir,
        &paths,
        rules,
        &mut collector,
        cancel,
        progress,
        &Progress {
            phase: "Scanning files for footprint".into(),
            ..Default::default()
        },
    );
    report.stats.bytes = bytes;
    // Unmodified tracked files equal HEAD, so their findings are "in HEAD".
    let changed = secrets::changed_vs_head(workdir, &mut commands);
    report.footprint = collector.finish(changed.as_ref(), &pushed, report.stats.head_pushed);
    report.footprint.retain(|f| !f.occurrences.is_empty());
    report.footprint.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then(b.occurrence_count.cmp(&a.occurrence_count))
            .then_with(|| a.name.cmp(b.name))
    });

    // Metadata and junk.
    prog.phase = "Reading file metadata".into();
    prog.done = 0;
    prog.total = Some(paths.len());
    progress(&prog);
    let path_re = local_path_regex();
    let mut binary_bytes = 0u64;
    for (i, (p, _)) in paths.iter().enumerate() {
        if i % 500 == 0 {
            prog.done = i;
            progress(&prog);
            if cancel.load(Ordering::Relaxed) {
                return finish_report(report, commands, started, true);
            }
        }
        if let Some(reason) = junk_reason(p) {
            report.junk.push(JunkFile {
                path: p.clone(),
                reason,
                size: std::fs::metadata(workdir.join(p))
                    .map(|m| m.len())
                    .unwrap_or(0),
            });
        }
        let lower = p.to_lowercase();
        let ext = lower.rsplit('.').next().unwrap_or("");
        let is_doc = matches!(
            ext,
            "jpg"
                | "jpeg"
                | "jpe"
                | "png"
                | "tif"
                | "tiff"
                | "pdf"
                | "docx"
                | "xlsx"
                | "pptx"
                | "docm"
                | "xlsm"
                | "pptm"
                | "dotx"
                | "xltx"
                | "potx"
                | "odt"
                | "ods"
                | "odp"
                | "odg"
        );
        let is_bin = is_binary_ext(p);
        if !is_doc && !is_bin {
            continue;
        }
        let Ok(meta) = std::fs::metadata(workdir.join(p)) else {
            continue;
        };
        if !meta.is_file() || meta.len() > MAX_META_BYTES {
            continue;
        }
        if is_bin && binary_bytes > 400 * 1024 * 1024 {
            continue;
        }
        let Ok(data) = std::fs::read(workdir.join(p)) else {
            continue;
        };
        if is_doc {
            if let Some(m) = metadata_for(p, &data) {
                report.metadata.push(m);
            }
        } else {
            binary_bytes += data.len() as u64;
            let mut fields: Vec<(String, String)> = Vec::new();
            for s in binary_strings(&data, 12) {
                for c in path_re.captures_iter(&s) {
                    let v = c[1].to_owned();
                    let user = v
                        .split(['/', '\\'])
                        .filter(|s| !s.is_empty())
                        .nth(1)
                        .unwrap_or("");
                    if !valid_username(user) {
                        continue;
                    }
                    if !fields.iter().any(|(_, f)| *f == v) {
                        fields.push(("Local path".into(), v));
                    }
                    if fields.len() >= 12 {
                        break;
                    }
                }
                if fields.len() >= 12 {
                    break;
                }
            }
            if !fields.is_empty() {
                report.metadata.push(MetaFinding {
                    path: p.clone(),
                    kind: "binary",
                    severity: Severity::Low,
                    size: data.len() as u64,
                    fields,
                    why: WHY_BIN,
                });
            }
        }
    }
    for m in &mut report.metadata {
        if is_test_path(&m.path) {
            m.severity = m.severity.lower();
        }
    }
    report.metadata.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.path.cmp(&b.path))
    });

    // History leftovers.
    prog.phase = "Checking history leftovers".into();
    prog.done = 0;
    prog.total = None;
    progress(&prog);
    report.archaeology = archaeology(workdir, opts, &pushed, &mut commands, cancel);

    // Supply chain, CODEOWNERS.
    prog.phase = "Reading dependencies".into();
    progress(&prog);
    report.supply = supply_chain(workdir, &tracked, &mut commands);
    for candidate in ["CODEOWNERS", ".github/CODEOWNERS", "docs/CODEOWNERS"] {
        if tracked.iter().any(|p| p == candidate)
            && let Ok(text) = std::fs::read_to_string(workdir.join(candidate))
        {
            report.codeowners = Some(parse_codeowners(candidate, &text));
            break;
        }
    }
    commands.push("git remote".into());
    report.stats.remotes = cli::run(workdir, &["remote"], None)
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().count())
        .unwrap_or(0);
    finish_report(report, commands, started, false)
}

fn finish_report(
    mut report: OsintReport,
    commands: Vec<String>,
    started: Instant,
    _cancelled: bool,
) -> Result<OsintReport> {
    report.commands = commands;
    report.stats.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

/// Objects no ref reaches: `git fsck --unreachable`, commit details, blob
/// sizes, and a secret scan of the blobs. Seconds to a minute on a large
/// repository, so it runs only when asked.
pub fn unreachable(
    workdir: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<Unreachable> {
    let started = Instant::now();
    let mut commands = Vec::new();
    let mut out = Unreachable::default();
    progress(&Progress {
        phase: "Running git fsck".into(),
        ..Default::default()
    });
    let args = [
        "fsck",
        "--unreachable",
        "--no-reflogs",
        "--connectivity-only",
        "--no-progress",
    ];
    commands.push(format!("git {}", args.join(" ")));
    let res = cli::run(workdir, &args, None)?;
    let mut commits: Vec<String> = Vec::new();
    let mut blobs: Vec<String> = Vec::new();
    for line in String::from_utf8_lossy(&res.stdout).lines() {
        let mut p = line.split_whitespace();
        if p.next() != Some("unreachable") {
            continue;
        }
        match (p.next(), p.next()) {
            (Some("commit"), Some(sha)) => commits.push(sha.to_owned()),
            (Some("blob"), Some(sha)) => blobs.push(sha.to_owned()),
            (Some("tree"), Some(_)) => out.trees += 1,
            _ => {}
        }
    }
    out.commit_count = commits.len();
    out.blobs = blobs.len();
    progress(&Progress {
        phase: "Reading unreachable commits".into(),
        done: 0,
        total: Some(commits.len().min(500)),
        bytes: 0,
        findings: 0,
    });
    let head: Vec<String> = commits.iter().take(500).cloned().collect();
    out.commits = commit_refs(workdir, &head, &mut commands);
    out.commits.sort_by_key(|c| std::cmp::Reverse(c.time));

    // Blob sizes, then contents through one cat-file --batch.
    if !blobs.is_empty() && !cancel.load(Ordering::Relaxed) {
        let stdin = blobs.join("\n") + "\n";
        commands.push(format!(
            "git cat-file --batch-check   # {} blob(s) on stdin",
            blobs.len()
        ));
        let mut sizes: Vec<(String, usize)> = Vec::new();
        if let Ok(o) = cli::run(
            workdir,
            &["cat-file", "--batch-check"],
            Some(stdin.as_bytes()),
        ) {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                let mut p = line.split_whitespace();
                if let (Some(sha), Some("blob"), Some(size)) = (p.next(), p.next(), p.next()) {
                    let size: usize = size.parse().unwrap_or(0);
                    out.blob_bytes += size as u64;
                    sizes.push((sha.to_owned(), size));
                }
            }
        }
        let rules = secrets::rules();
        let mut collector = Collector::new(rules);
        let mut budget = MAX_UNREACHABLE_TOTAL;
        let wanted: Vec<String> = sizes
            .iter()
            .filter(|(_, s)| *s <= MAX_UNREACHABLE_BLOB)
            .map(|(sha, _)| sha.clone())
            .collect();
        progress(&Progress {
            phase: "Scanning unreachable blobs".into(),
            done: 0,
            total: Some(wanted.len()),
            bytes: 0,
            findings: 0,
        });
        for (ci, chunk) in wanted.chunks(2000).enumerate() {
            if cancel.load(Ordering::Relaxed) || budget == 0 {
                out.cancelled = cancel.load(Ordering::Relaxed);
                break;
            }
            let stdin = chunk.join("\n") + "\n";
            commands.push(format!(
                "git cat-file --batch   # {} blob(s) on stdin",
                chunk.len()
            ));
            let Ok(o) = cli::run(workdir, &["cat-file", "--batch"], Some(stdin.as_bytes())) else {
                break;
            };
            let data = &o.stdout[..];
            let mut pos = 0usize;
            while pos < data.len() {
                let Some(nl) = data[pos..].iter().position(|&b| b == b'\n') else {
                    break;
                };
                let header = String::from_utf8_lossy(&data[pos..pos + nl]).into_owned();
                pos += nl + 1;
                let f: Vec<&str> = header.split(' ').collect();
                if f.len() < 3 {
                    continue;
                }
                let size: usize = f[2].parse().unwrap_or(0);
                let end = (pos + size).min(data.len());
                let body = &data[pos..end];
                pos = end + 1;
                if secrets::is_binary(body) {
                    continue;
                }
                out.scanned_blobs += 1;
                out.scanned_bytes += body.len() as u64;
                budget = budget.saturating_sub(body.len() as u64);
                let text = String::from_utf8_lossy(body);
                collector.scan_text(
                    &format!("blob {}", &f[0][..12.min(f[0].len())]),
                    &text,
                    Place::Unreachable,
                    None,
                );
            }
            progress(&Progress {
                phase: "Scanning unreachable blobs".into(),
                done: ((ci + 1) * 2000).min(wanted.len()),
                total: Some(wanted.len()),
                bytes: out.scanned_bytes,
                findings: collector.len(),
            });
        }
        out.findings = collector.finish(None, &HashSet::new(), false);
    }
    out.elapsed_ms = started.elapsed().as_millis() as u64;
    out.commands = commands;
    Ok(out)
}

/// Pull-request refs the remote advertises (`refs/pull/N/head` on
/// GitHub): heads not in any branch are commits the server still serves
/// even when the PR was closed or the branch force-pushed.
pub fn pr_refs(workdir: &Path, remote: &str) -> Result<PrRefs> {
    let started = Instant::now();
    let args = ["ls-remote", "--refs", remote, "refs/pull/*/head"];
    let out = cli::run(workdir, &args, None)?;
    let mut commands = Vec::new();
    let known = secrets::remote_commits(workdir, &mut commands);
    let local: HashSet<Oid> = cli::run(workdir, &["rev-list", "--all"], None)
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| Oid::from_str(l.trim()).ok())
                .collect()
        })
        .unwrap_or_default();
    let mut res = PrRefs {
        remote: remote.to_owned(),
        command: format!("git {}", args.join(" ")),
        ..Default::default()
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut p = line.split_whitespace();
        let (Some(sha), Some(refname)) = (p.next(), p.next()) else {
            continue;
        };
        res.total += 1;
        let Ok(id) = Oid::from_str(sha) else { continue };
        if !known.contains(&id) && !local.contains(&id) {
            res.unknown_heads += 1;
            if res.sample.len() < 20
                && let Some(n) = refname
                    .strip_prefix("refs/pull/")
                    .and_then(|r| r.strip_suffix("/head"))
                    .and_then(|n| n.parse::<u64>().ok())
            {
                res.sample.push((n, sha.to_owned()));
            }
        }
    }
    res.sample.sort_by_key(|s| std::cmp::Reverse(s.0));
    res.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_kinds() {
        assert_eq!(email_kind("ann@gmail.com"), EmailKind::Personal);
        assert_eq!(email_kind("Ann@Example.ORG"), EmailKind::Corporate);
        assert_eq!(
            email_kind("12345+ann@users.noreply.github.com"),
            EmailKind::NoReply
        );
        assert_eq!(email_kind("ann@localhost"), EmailKind::Invalid);
        assert_eq!(email_kind("ann"), EmailKind::Invalid);
        assert_eq!(email_kind("ann@brave.com"), EmailKind::Corporate);
    }

    #[test]
    fn offsets_and_slots() {
        assert_eq!(parse_offset("+0200"), Some(120));
        assert_eq!(parse_offset("-0530"), Some(-330));
        assert_eq!(parse_offset("0200"), None);
        // 1970-01-01 00:00 UTC is a Thursday (index 3).
        assert_eq!(local_slot(0, 0), (3, 0));
        // Four days later: Monday; +2h offset at 23:30 UTC rolls into Tuesday 01:30.
        assert_eq!(local_slot(4 * 86_400 + 23 * 3600 + 1800, 120), (1, 1));
        // Three days later is Sunday 00:00 UTC; an hour west it is still Saturday.
        assert_eq!(local_slot(3 * 86_400, -60), (5, 23));
    }

    #[test]
    fn validators() {
        assert!(valid_private_ip("10.1.2.3"));
        assert!(!valid_private_ip("10.1.2.300"));
        assert!(valid_public_ip("203.0.114.7"));
        assert!(!valid_public_ip("203.0.113.7"), "documentation range");
        assert!(!valid_public_ip("8.8.8.8"));
        assert!(!valid_public_ip("192.168.1.1"));
        assert!(!valid_public_ip("1.2.3.4"));
        assert!(valid_iban("GB82WEST12345698765432"));
        assert!(valid_iban("DE89 3704 0044 0532 0130 00"));
        assert!(!valid_iban("GB82WEST12345698765433"));
        assert!(!valid_iban("DE8937040044053201300"), "wrong length");
        assert!(valid_card("4539 1488 0343 6467"));
        assert!(valid_card("5425233430109903"));
        assert!(!valid_card("4111111111111111"), "well-known test number");
        assert!(!valid_card("1234567890123456"), "no issuer prefix");
        assert!(!valid_card("4539148803436468"), "Luhn fails");
        assert!(valid_ssn("521-74-3920"));
        assert!(!valid_ssn("666-12-3456"));
        assert!(!valid_ssn("123-45-6789"));
        assert!(valid_phone("+41 44 268 73 41"));
        assert!(!valid_phone("+1 555 123 4567"));
        assert!(!valid_phone("+1 234 567 8901"), "a sequence");
        assert!(valid_email("maria.lopez@acmecorp.io"));
        assert!(!valid_email("user@example.com"));
        assert!(!valid_email("noreply@github.com"));
        assert!(valid_username("mlopez"));
        assert!(!valid_username("runner"));
        assert!(!valid_username("${USER}"));
    }

    #[test]
    fn footprint_rules_find_identifiers() {
        let r = footprint_rules();
        let ids = |p: &str, l: &str| -> Vec<&'static str> {
            secrets::scan_line(r, p, l)
                .into_iter()
                .map(|h| r.rules[h.rule].id)
                .collect()
        };
        assert_eq!(ids("a.yml", "db_host: 10.20.30.40"), ["private-ip"]);
        assert_eq!(ids("a.yml", "host: build.acme.corp"), ["internal-host"]);
        assert_eq!(
            ids(
                "a.tf",
                "role_arn = \"arn:aws:iam::734019283745:role/deploy\""
            ),
            ["aws-account"]
        );
        assert_eq!(
            ids("a.sh", "aws s3 cp x s3://acme-prod-backups/x"),
            ["s3-bucket"]
        );
        assert_eq!(
            ids("a.sh", "gcloud --project projects/acme-prod-317204"),
            ["gcp-project"]
        );
        assert_eq!(
            ids(
                "a.json",
                "\"id\": \"/subscriptions/8c3e0b2a-4f1d-4e9a-9c1b-2d7f6a1e5b90/resourceGroups/x\""
            ),
            ["azure-subscription"]
        );
        assert_eq!(
            ids("Dockerfile", "FROM acme.azurecr.io/api:1.2"),
            ["container-registry"]
        );
        assert_eq!(
            ids("a.md", "see https://acme-team.slack.com/archives/C04ABCD12"),
            ["slack-workspace"]
        );
        assert_eq!(
            ids("build.log", "error in C:\\Users\\mlopez\\src\\app\\main.cs"),
            ["local-user-path"]
        );
        assert!(ids("build.log", "/home/runner/work/app").is_empty());
        assert_eq!(
            ids("users.csv", "maria.lopez@acmecorp.io,+41 44 268 73 41"),
            ["email-address", "phone-number"]
        );
        assert_eq!(
            ids("pay.csv", "GB82WEST12345698765432;card 4539148803436467"),
            ["iban", "payment-card"]
        );
        assert!(
            ids("pay.csv", "4539148803436467").is_empty(),
            "no shape, no payment word"
        );
        assert!(
            ids("x.pbxproj", "CA986FB3298C1254000C6DD8 /* x */").is_empty(),
            "not an IBAN country"
        );
        assert!(
            ids(
                "s.xtb",
                r#"<translation id="4131157740532436004">Save card</translation>"#
            )
            .is_empty()
        );
        assert!(ids("x.rs", "let v = \"1.2.3.4\";").is_empty());
        assert_eq!(ids("x.txt", "ssn 521-74-3920"), ["us-ssn"]);
        assert!(ids("x.txt", "version 2.3.4-5").is_empty());
    }

    #[test]
    fn exif_gps_from_synthetic_tiff() {
        // Little-endian TIFF: IFD0 with Make (ASCII inline "Cam\0") and a
        // GPS pointer; GPS IFD with refs and lat/long rationals.
        let mut t: Vec<u8> = Vec::new();
        t.extend(b"II");
        t.extend(42u16.to_le_bytes());
        t.extend(8u32.to_le_bytes()); // IFD0 at 8
        // IFD0: 2 entries
        t.extend(2u16.to_le_bytes());
        // Make: tag 0x010F type 2 count 4 value "Cam\0"
        t.extend(0x010Fu16.to_le_bytes());
        t.extend(2u16.to_le_bytes());
        t.extend(4u32.to_le_bytes());
        t.extend(b"Cam\0");
        // GPS pointer: tag 0x8825 type 4 count 1 → offset 38
        t.extend(0x8825u16.to_le_bytes());
        t.extend(4u16.to_le_bytes());
        t.extend(1u32.to_le_bytes());
        t.extend(38u32.to_le_bytes());
        t.extend(0u32.to_le_bytes()); // next IFD
        assert_eq!(t.len(), 38);
        // GPS IFD: 4 entries
        t.extend(4u16.to_le_bytes());
        let rat_lat = 38 + 2 + 4 * 12 + 4; // after entries + next pointer
        let rat_lon = rat_lat + 24;
        t.extend(0x0001u16.to_le_bytes());
        t.extend(2u16.to_le_bytes());
        t.extend(2u32.to_le_bytes());
        t.extend(b"N\0\0\0");
        t.extend(0x0002u16.to_le_bytes());
        t.extend(5u16.to_le_bytes());
        t.extend(3u32.to_le_bytes());
        t.extend((rat_lat as u32).to_le_bytes());
        t.extend(0x0003u16.to_le_bytes());
        t.extend(2u16.to_le_bytes());
        t.extend(2u32.to_le_bytes());
        t.extend(b"E\0\0\0");
        t.extend(0x0004u16.to_le_bytes());
        t.extend(5u16.to_le_bytes());
        t.extend(3u32.to_le_bytes());
        t.extend((rat_lon as u32).to_le_bytes());
        t.extend(0u32.to_le_bytes());
        assert_eq!(t.len(), rat_lat);
        for (n, d) in [(47u32, 1u32), (22, 1), (3300, 100)] {
            t.extend(n.to_le_bytes());
            t.extend(d.to_le_bytes());
        }
        for (n, d) in [(8u32, 1u32), (32, 1), (1200, 100)] {
            t.extend(n.to_le_bytes());
            t.extend(d.to_le_bytes());
        }
        let fields = exif_fields(&t);
        assert!(
            fields.iter().any(|(k, v)| k == "Camera make" && v == "Cam"),
            "{fields:?}"
        );
        let gps = fields
            .iter()
            .find(|(k, _)| k == "GPS position")
            .map(|(_, v)| v.clone());
        assert_eq!(gps.as_deref(), Some("47.37583, 8.53667"));
        // Wrapped in a JPEG APP1 segment.
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1];
        let seg_len = (t.len() + 6 + 2) as u16;
        jpeg.extend(seg_len.to_be_bytes());
        jpeg.extend(b"Exif\0\0");
        jpeg.extend(&t);
        jpeg.extend([0xFF, 0xD9]);
        assert!(jpeg_fields(&jpeg).iter().any(|(k, _)| k == "GPS position"));
        assert!(jpeg_fields(b"not a jpeg").is_empty());
    }

    #[test]
    fn pdf_and_office_metadata() {
        let pdf = b"%PDF-1.4\n1 0 obj\n<< /Author (Maria L\\(opez\\)) /Producer (LibreOffice 7.6) /Title <FEFF00480069> >>\nendobj\n";
        let f = pdf_fields(pdf);
        assert!(
            f.iter().any(|(k, v)| k == "Author" && v == "Maria L(opez)"),
            "{f:?}"
        );
        assert!(
            f.iter()
                .any(|(k, v)| k == "Producer" && v == "LibreOffice 7.6")
        );
        assert!(f.iter().any(|(k, v)| k == "Title" && v == "Hi"));

        // A stored ZIP with docProps/core.xml.
        let name = b"docProps/core.xml";
        let body = br#"<cp:coreProperties><dc:creator>Maria Lopez</dc:creator><cp:lastModifiedBy>Jo</cp:lastModifiedBy></cp:coreProperties>"#;
        let mut z: Vec<u8> = Vec::new();
        // local header
        z.extend([0x50, 0x4b, 0x03, 0x04]);
        z.extend([0u8; 4]); // version, flags
        z.extend(0u16.to_le_bytes()); // method stored
        z.extend([0u8; 4]); // time/date
        z.extend([0u8; 4]); // crc
        z.extend((body.len() as u32).to_le_bytes());
        z.extend((body.len() as u32).to_le_bytes());
        z.extend((name.len() as u16).to_le_bytes());
        z.extend(0u16.to_le_bytes());
        z.extend(name);
        z.extend(body);
        let cd = z.len();
        z.extend([0x50, 0x4b, 0x01, 0x02]);
        z.extend([0u8; 6]); // versions, flags
        z.extend(0u16.to_le_bytes()); // method
        z.extend([0u8; 4]); // time/date
        z.extend([0u8; 4]); // crc
        z.extend((body.len() as u32).to_le_bytes());
        z.extend((body.len() as u32).to_le_bytes());
        z.extend((name.len() as u16).to_le_bytes());
        z.extend(0u16.to_le_bytes()); // extra
        z.extend(0u16.to_le_bytes()); // comment
        z.extend([0u8; 8]); // disk, attrs
        z.extend(0u32.to_le_bytes()); // local header offset
        z.extend(name);
        let cd_size = z.len() - cd;
        z.extend([0x50, 0x4b, 0x05, 0x06]);
        z.extend([0u8; 4]);
        z.extend(1u16.to_le_bytes());
        z.extend(1u16.to_le_bytes());
        z.extend((cd_size as u32).to_le_bytes());
        z.extend((cd as u32).to_le_bytes());
        z.extend(0u16.to_le_bytes());
        let f = office_fields(&z);
        assert!(
            f.iter().any(|(k, v)| k == "Creator" && v == "Maria Lopez"),
            "{f:?}"
        );
        assert!(f.iter().any(|(k, v)| k == "Last modified by" && v == "Jo"));
        assert_eq!(
            binary_strings(b"\x00\x01C:\\Users\\mlopez\\build\\app.exe\x00\x00h\x00i\x00 \x00t\x00h\x00e\x00r\x00e\x00!\x00\x00", 8),
            ["C:\\Users\\mlopez\\build\\app.exe", "hi there!"]
        );
    }

    #[test]
    fn manifests_and_codeowners() {
        let mut s = Supply::default();
        parse_manifest(
            "package.json",
            r#"{"dependencies":{"left-pad":"^1.0.0","thing":"git+https://github.com/x/thing.git#main"}}"#,
            &mut s,
        );
        assert_eq!(s.git_deps.len(), 1);
        assert_eq!(s.git_deps[0].name, "thing");
        parse_manifest(
            "package-lock.json",
            r#"{"lockfileVersion":3,"packages":{"":{},"node_modules/lodash":{"version":"4.17.20"},"node_modules/a/node_modules/b":{"version":"2.0.0"}}}"#,
            &mut s,
        );
        assert!(
            s.packages
                .iter()
                .any(|p| p.name == "lodash" && p.version == "4.17.20" && p.ecosystem == "npm")
        );
        assert!(s.packages.iter().any(|p| p.name == "b"));
        parse_manifest(
            "Cargo.lock",
            "[[package]]\nname = \"regex\"\nversion = \"1.11.1\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"mine\"\nversion = \"0.1.0\"\nsource = \"git+https://github.com/me/mine#abc\"\n",
            &mut s,
        );
        assert!(
            s.packages
                .iter()
                .any(|p| p.name == "regex" && p.ecosystem == "crates.io")
        );
        assert!(s.git_deps.iter().any(|d| d.name == "mine"));
        parse_manifest(
            "Cargo.toml",
            "[dependencies]\nfoo = { git = \"https://github.com/x/foo\", branch = \"main\" }\n",
            &mut s,
        );
        assert!(
            s.git_deps
                .iter()
                .any(|d| d.name == "foo" && d.source == "https://github.com/x/foo" && d.line == 2)
        );
        parse_manifest(
            "requirements.txt",
            "requests==2.31.0\nflask==3.0.0 ; python_version > '3'\n-e git+https://github.com/x/y.git#egg=y\n",
            &mut s,
        );
        assert!(
            s.packages
                .iter()
                .any(|p| p.name == "requests" && p.version == "2.31.0" && p.ecosystem == "PyPI")
        );
        assert!(
            s.packages
                .iter()
                .any(|p| p.name == "flask" && p.version == "3.0.0")
        );
        parse_manifest(
            "go.mod",
            "module x\n\nrequire (\n\tgithub.com/a/b v1.2.3\n)\nreplace github.com/c/d => github.com/e/d v0.0.1\n",
            &mut s,
        );
        assert!(
            s.packages
                .iter()
                .any(|p| p.name == "github.com/a/b" && p.version == "1.2.3" && p.ecosystem == "Go")
        );
        assert!(s.git_deps.iter().any(|d| d.name == "github.com/c/d"));
        parse_manifest(
            "Gemfile.lock",
            "GEM\n  specs:\n    rails (7.1.2)\n    nokogiri (1.15.4-x86_64-linux)\n",
            &mut s,
        );
        assert!(
            s.packages
                .iter()
                .any(|p| p.name == "nokogiri" && p.version == "1.15.4")
        );
        parse_manifest(
            "yarn.lock",
            "\"@babel/core@^7.0.0\", \"@babel/core@^7.1.0\":\n  version \"7.23.0\"\n  resolved \"https://registry.yarnpkg.com/@babel/core/-/core-7.23.0.tgz\"\n",
            &mut s,
        );
        assert!(
            s.packages
                .iter()
                .any(|p| p.name == "@babel/core" && p.version == "7.23.0")
        );

        let co = parse_codeowners(
            "CODEOWNERS",
            "# owners\n*.rs @acme/rust-team @ann\ndocs/ docs@acme.com\n\n",
        );
        assert_eq!(co.rules, 2);
        assert_eq!(co.owners[0].1, 1);
        assert_eq!(co.owners.len(), 3);
    }

    #[test]
    fn incidents_tickets_and_backup_names() {
        let hit = |m: &str| {
            incident_rules()
                .iter()
                .find(|r| r.regex.is_match(m))
                .map(|r| r.reason)
        };
        assert_eq!(
            hit("Remove leaked AWS key from config"),
            Some("a secret was removed or rotated")
        );
        assert_eq!(
            hit("temporarily disable ssl verification for staging"),
            Some("a security check was disabled or bypassed")
        );
        assert_eq!(
            hit("oops, committed the wrong file"),
            Some("something was committed by accident")
        );
        assert_eq!(
            hit("Fix CVE-2024-1234 in parser"),
            Some("security vulnerability mentioned")
        );
        assert_eq!(hit("Add button styles"), None);
        assert_eq!(
            hit("Reduce the override to the include it exposed"),
            None,
            "exposed alone is not an incident"
        );
        assert_eq!(hit("Disable DAT caching when debug mode is on"), None);
        assert_eq!(
            hit("Use the dialog controller in the HTTPS upgrade test"),
            None
        );
        assert_eq!(
            hit("rotate the signing key after the token leak"),
            Some("a secret was removed or rotated")
        );
        assert!(pf_card("card: 4539 1488 0343 6467"));
        assert!(pf_card("pan=4539148803436467"));
        assert!(!pf_card("id = 4539148803436467"), "a bare integer");
        assert_eq!(backup_branch_reason("feature/login"), None);
        assert!(backup_branch_reason("backup/old-main").is_some());
        assert!(backup_branch_reason("wip-login").is_some());
        assert!(backup_branch_reason("main").is_none());
        assert!(backup_branch_reason("ann/tmp2").is_some());
    }
}
