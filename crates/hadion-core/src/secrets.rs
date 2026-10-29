//! What a repository exposes that it should not: credentials in file
//! contents, sensitive files (keys, `.env`, credential stores) and leaky
//! configuration — in the working tree, in what is staged, and in history.
//!
//! Content rules are regular expressions with a captured secret, filtered
//! by placeholder heuristics and Shannon entropy. History is read by
//! streaming `git log -p -U0` and scanning only added lines, so a 2,000
//! commit scan of a large repository takes seconds, not minutes. Whether a
//! secret is in HEAD follows from `git diff --name-only HEAD`: unmodified
//! tracked files equal HEAD, and only the changed ones are read from HEAD
//! through `git cat-file --batch`. Secrets never appear on a command line.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use aho_corasick::AhoCorasick;
use regex::Regex;

use crate::{Error, Oid, Result, cli};

/// Files larger than this are not read (dumps, bundles, media).
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Lines longer than this are skipped (minified code, data blobs).
const MAX_LINE_BYTES: usize = 20_000;
/// Occurrences kept per finding; the total is still counted.
const MAX_OCCURRENCES: usize = 200;
/// Distinct commits kept per finding.
const MAX_COMMITS: usize = 100;
/// Characters of context kept around a match in a snippet.
const SNIPPET_CONTEXT: usize = 160;

// ---------------------------------------------------------------------------
// Severity
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub const ALL: [Severity; 4] = [
        Severity::Critical,
        Severity::High,
        Severity::Medium,
        Severity::Low,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Severity::Critical => "Critical",
            Severity::High => "High",
            Severity::Medium => "Medium",
            Severity::Low => "Low",
        }
    }

    /// One step less severe (for matches in test fixtures).
    pub fn lower(self) -> Severity {
        match self {
            Severity::Critical => Severity::High,
            Severity::High => Severity::Medium,
            _ => Severity::Low,
        }
    }
}

// ---------------------------------------------------------------------------
// Content rules
// ---------------------------------------------------------------------------

pub struct Rule {
    pub id: &'static str,
    pub name: &'static str,
    pub severity: Severity,
    /// Why a match matters, for the detail view.
    pub why: &'static str,
    /// Where the credential can be revoked.
    pub rotate: Option<&'static str>,
    regex: Regex,
    /// Capture group holding the secret (0 = whole match).
    group: usize,
    /// Minimum Shannon entropy (bits per character) of the secret.
    min_entropy: f64,
    /// Restrict the rule to some files.
    paths: Option<fn(&str) -> bool>,
    /// Lines longer than this are skipped for the rule (minified bundles
    /// and saved web pages drown the generic rules in noise).
    max_line: usize,
    /// Apply the placeholder and entropy filters (credentials); off for
    /// footprint rules whose values are identifiers, not secrets.
    strict: bool,
    /// Extra check on the captured value (checksums, ranges).
    validate: Option<fn(&str) -> bool>,
    /// Cheap test on the whole line before the regex runs, for rules whose
    /// keywords (digits, `@`, `-`) match almost every line.
    prefilter: Option<fn(&str) -> bool>,
}

pub struct Rules {
    pub rules: Vec<Rule>,
    /// Case-insensitive keywords; a rule's regex only runs on lines that
    /// contain one of its keywords (a few MB/s per thread with the regexes
    /// alone, hundreds with the gate).
    keywords: AhoCorasick,
    /// Bitset of rule indices per keyword pattern.
    kw_rules: Vec<u64>,
}

impl Rules {
    pub fn get(&self, id: &str) -> Option<&Rule> {
        self.rules.iter().find(|r| r.id == id)
    }
}

/// The built-in rule set, compiled once.
pub fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(build_rules)
}

/// A rule before compilation; the footprint rules in `osint.rs` use the
/// same shape.
pub(crate) struct Spec {
    pub(crate) id: &'static str,
    pub(crate) name: &'static str,
    pub(crate) severity: Severity,
    pub(crate) pattern: &'static str,
    pub(crate) group: usize,
    pub(crate) min_entropy: f64,
    pub(crate) why: &'static str,
    pub(crate) rotate: Option<&'static str>,
    pub(crate) paths: Option<fn(&str) -> bool>,
    pub(crate) max_line: usize,
    pub(crate) strict: bool,
    pub(crate) validate: Option<fn(&str) -> bool>,
    pub(crate) prefilter: Option<fn(&str) -> bool>,
}

/// Line length cap for the generic rules.
pub(crate) const GENERIC_MAX_LINE: usize = 2_000;

#[allow(clippy::too_many_arguments)]
pub(crate) const fn spec(
    id: &'static str,
    name: &'static str,
    severity: Severity,
    pattern: &'static str,
    group: usize,
    min_entropy: f64,
    why: &'static str,
    rotate: Option<&'static str>,
) -> Spec {
    Spec {
        id,
        name,
        severity,
        pattern,
        group,
        min_entropy,
        why,
        rotate,
        paths: None,
        max_line: MAX_LINE_BYTES,
        strict: true,
        validate: None,
        prefilter: None,
    }
}

fn is_npmrc(path: &str) -> bool {
    matches!(
        basename(path),
        ".npmrc" | ".yarnrc" | ".yarnrc.yml" | ".yarnrc.yaml"
    )
}

fn is_config_file(path: &str) -> bool {
    let base = basename(path);
    base == ".env"
        || base.starts_with(".env.")
        || base.ends_with(".env")
        || [
            ".ini",
            ".cfg",
            ".conf",
            ".config",
            ".properties",
            ".toml",
            ".yml",
            ".yaml",
            ".tfvars",
            ".envrc",
            ".sh",
            ".ps1",
            ".bat",
            ".cmd",
        ]
        .iter()
        .any(|e| base.ends_with(e))
}

const WHY_TOKEN: &str = "An access token committed to a repository is readable by everyone who can read the repository, and by anyone who ever cloned it. Revoke it and issue a new one; removing the line is not enough.";
const WHY_CLOUD: &str = "Cloud credentials in a repository are harvested by bots within minutes of a public push and used for crypto-mining, data theft or lateral movement. Revoke the key immediately, then check the provider's audit log.";
const WHY_GENERIC: &str = "A value that looks like a real credential is assigned to a password- or key-named setting. If it is real, it has been shared with everyone who has the repository; move it to an environment variable or a secret manager and rotate it.";

static SPECS: &[Spec] = &[
    spec(
        "private-key",
        "Private key",
        Severity::Critical,
        r"-----BEGIN (?:RSA |DSA |EC |OPENSSH |PGP )?PRIVATE KEY(?: BLOCK)?-----",
        0,
        0.0,
        "A private key in the repository lets anyone with the history impersonate its owner: decrypt traffic, sign releases or log in to servers. Treat the key as compromised and generate a new pair.",
        None,
    ),
    spec(
        "encrypted-private-key",
        "Encrypted private key",
        Severity::High,
        r"-----BEGIN ENCRYPTED PRIVATE KEY-----",
        0,
        0.0,
        "The key is passphrase-protected, but an offline attacker can try passphrases forever. Replace the key pair.",
        None,
    ),
    spec(
        "putty-key",
        "PuTTY private key",
        Severity::Critical,
        r"PuTTY-User-Key-File-\d",
        0,
        0.0,
        "A PuTTY .ppk file carries an SSH private key. Anyone with the repository can log in wherever its public half is authorised.",
        None,
    ),
    spec(
        "aws-access-key",
        "AWS access key ID",
        Severity::Critical,
        r"\b((?:AKIA|ASIA)[0-9A-Z]{16})\b",
        1,
        0.0,
        WHY_CLOUD,
        Some("https://console.aws.amazon.com/iam/home#/security_credentials"),
    ),
    spec(
        "aws-secret-key",
        "AWS secret access key",
        Severity::Critical,
        r#"(?i)(?:aws|amazon)[a-z0-9_\-.]{0,30}(?:secret|key)[a-z0-9_\-.]{0,20}\s*[=:]\s*["']?([A-Za-z0-9/+]{40})(?:["'\s,;]|$)"#,
        1,
        3.5,
        WHY_CLOUD,
        Some("https://console.aws.amazon.com/iam/home#/security_credentials"),
    ),
    spec(
        "github-token",
        "GitHub token",
        Severity::Critical,
        r"\b((?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36,255})\b",
        1,
        3.0,
        "A GitHub token acts as its owner: it can read private repositories, push code, and change settings. Revoke it under Settings → Developer settings.",
        Some("https://github.com/settings/tokens"),
    ),
    spec(
        "github-pat",
        "GitHub fine-grained token",
        Severity::Critical,
        r"\b(github_pat_[A-Za-z0-9]{22}_[A-Za-z0-9]{59})\b",
        1,
        3.0,
        "A fine-grained personal access token grants its configured permissions to anyone holding it.",
        Some("https://github.com/settings/tokens?type=beta"),
    ),
    spec(
        "gitlab-token",
        "GitLab token",
        Severity::High,
        r"\b(glpat-[A-Za-z0-9_\-]{20,})\b",
        1,
        3.0,
        WHY_TOKEN,
        Some("https://gitlab.com/-/user_settings/personal_access_tokens"),
    ),
    spec(
        "slack-token",
        "Slack token",
        Severity::High,
        r"\b(xox[abposr]-[0-9A-Za-z\-]{10,250})\b",
        1,
        3.0,
        "A Slack token can read and post messages as the app or user it belongs to.",
        Some("https://api.slack.com/apps"),
    ),
    spec(
        "slack-webhook",
        "Slack webhook URL",
        Severity::High,
        r"(https://hooks\.slack\.com/services/T[A-Za-z0-9_]+/B[A-Za-z0-9_]+/[A-Za-z0-9_]+)",
        1,
        0.0,
        "Anyone with the webhook URL can post messages into the channel. Regenerate it in the Slack app settings.",
        Some("https://api.slack.com/apps"),
    ),
    spec(
        "stripe-live",
        "Stripe live secret key",
        Severity::Critical,
        r"\b((?:sk|rk)_live_[0-9a-zA-Z]{20,99})\b",
        1,
        3.0,
        "A live Stripe secret key moves real money: refunds, payouts, customer data. Roll it in the Stripe dashboard right away.",
        Some("https://dashboard.stripe.com/apikeys"),
    ),
    spec(
        "stripe-test",
        "Stripe test secret key",
        Severity::Low,
        r"\b((?:sk|rk)_test_[0-9a-zA-Z]{20,99})\b",
        1,
        3.0,
        "A test-mode key cannot move real money, but it exposes test customer data and is a sign that live keys may be handled the same way.",
        Some("https://dashboard.stripe.com/test/apikeys"),
    ),
    spec(
        "google-api-key",
        "Google API key",
        Severity::Medium,
        r"\b(AIza[0-9A-Za-z\-_]{35})\b",
        1,
        3.5,
        "Google API keys are often meant for client apps, but an unrestricted key can run up a bill on any enabled API. Check its restrictions in the Cloud console.",
        Some("https://console.cloud.google.com/apis/credentials"),
    ),
    spec(
        "google-oauth-secret",
        "Google OAuth client secret",
        Severity::High,
        r"\b(GOCSPX-[A-Za-z0-9_\-]{28})\b",
        1,
        3.0,
        "An OAuth client secret lets an attacker impersonate your application to Google and to its users.",
        Some("https://console.cloud.google.com/apis/credentials"),
    ),
    spec(
        "gcp-service-account",
        "Google service account file",
        Severity::High,
        r#""private_key_id"\s*:\s*"([a-f0-9]{40})""#,
        1,
        0.0,
        "A service-account JSON file carries a private key that authenticates as that account to Google Cloud.",
        Some("https://console.cloud.google.com/iam-admin/serviceaccounts"),
    ),
    spec(
        "openai-key",
        "OpenAI API key",
        Severity::High,
        r"\b(sk-(?:proj-|svcacct-|admin-)?[A-Za-z0-9_\-]{0,80}T3BlbkFJ[A-Za-z0-9_\-]{20,})",
        1,
        3.0,
        WHY_TOKEN,
        Some("https://platform.openai.com/api-keys"),
    ),
    spec(
        "anthropic-key",
        "Anthropic API key",
        Severity::High,
        r"\b(sk-ant-[A-Za-z0-9_\-]{40,})",
        1,
        3.0,
        WHY_TOKEN,
        Some("https://console.anthropic.com/settings/keys"),
    ),
    spec(
        "huggingface-token",
        "Hugging Face token",
        Severity::High,
        r"\b(hf_[A-Za-z0-9]{34,})\b",
        1,
        3.0,
        WHY_TOKEN,
        Some("https://huggingface.co/settings/tokens"),
    ),
    spec(
        "npm-token",
        "npm access token",
        Severity::High,
        r"\b(npm_[A-Za-z0-9]{36})\b",
        1,
        3.0,
        "An npm token can publish packages under your name: a supply-chain attack waiting to happen.",
        Some("https://www.npmjs.com/settings/~/tokens"),
    ),
    Spec {
        paths: Some(is_npmrc),
        ..spec(
            "npmrc-auth",
            "npm registry credential",
            Severity::High,
            r#"(?i)(?:_auth(?:token)?\s*=|npmAuthToken\s*:)\s*["']?([^\s'"]{8,})"#,
            1,
            0.0,
            "A registry credential in .npmrc or .yarnrc authenticates installs and publishes as you.",
            Some("https://www.npmjs.com/settings/~/tokens"),
        )
    },
    spec(
        "pypi-token",
        "PyPI token",
        Severity::High,
        r"\b(pypi-AgEIcHlwaS5vcmc[A-Za-z0-9_\-]{50,})",
        1,
        3.0,
        "A PyPI token can upload releases of your packages.",
        Some("https://pypi.org/manage/account/token/"),
    ),
    spec(
        "digitalocean-token",
        "DigitalOcean token",
        Severity::High,
        r"\b(do[pors]_v1_[a-f0-9]{64})\b",
        1,
        3.0,
        WHY_CLOUD,
        Some("https://cloud.digitalocean.com/account/api/tokens"),
    ),
    spec(
        "sendgrid-key",
        "SendGrid API key",
        Severity::High,
        r"\b(SG\.[A-Za-z0-9_\-]{22}\.[A-Za-z0-9_\-]{43})\b",
        1,
        3.0,
        "A SendGrid key sends e-mail as your domain: phishing with your reputation.",
        Some("https://app.sendgrid.com/settings/api_keys"),
    ),
    spec(
        "twilio-key",
        "Twilio API key",
        Severity::Medium,
        r"\b(SK[0-9a-fA-F]{32})\b",
        1,
        3.0,
        "A Twilio API key can send messages and make calls billed to your account.",
        Some("https://console.twilio.com/us1/account/keys-credentials/api-keys"),
    ),
    spec(
        "mailgun-key",
        "Mailgun API key",
        Severity::Medium,
        r"\b(key-[0-9a-zA-Z]{32})\b",
        1,
        3.0,
        "A Mailgun key sends e-mail as your domain.",
        Some("https://app.mailgun.com/settings/api_security"),
    ),
    spec(
        "mailchimp-key",
        "Mailchimp API key",
        Severity::Medium,
        r"\b([0-9a-f]{32}-us[0-9]{1,2})\b",
        1,
        3.0,
        "A Mailchimp key exposes your audience lists and can send campaigns.",
        Some("https://admin.mailchimp.com/account/api/"),
    ),
    spec(
        "telegram-bot-token",
        "Telegram bot token",
        Severity::High,
        r"\b([0-9]{8,10}:AA[A-Za-z0-9_\-]{33})\b",
        1,
        3.0,
        "Whoever holds the token controls the bot and reads everything sent to it.",
        None,
    ),
    spec(
        "discord-token",
        "Discord bot token",
        Severity::Medium,
        r"\b([MN][A-Za-z0-9]{23,}\.[A-Za-z0-9_\-]{6}\.[A-Za-z0-9_\-]{27,})\b",
        1,
        4.0,
        "Whoever holds the token controls the bot in every server it joined.",
        Some("https://discord.com/developers/applications"),
    ),
    spec(
        "discord-webhook",
        "Discord webhook URL",
        Severity::Medium,
        r"(https://(?:ptb\.|canary\.)?discord(?:app)?\.com/api/webhooks/[0-9]+/[A-Za-z0-9_\-]+)",
        1,
        0.0,
        "Anyone with the URL can post into the channel.",
        None,
    ),
    spec(
        "shopify-token",
        "Shopify token",
        Severity::High,
        r"\b(shp(?:at|ca|pat|ss)_[a-fA-F0-9]{32})\b",
        1,
        3.0,
        WHY_TOKEN,
        None,
    ),
    spec(
        "square-token",
        "Square token",
        Severity::High,
        r"\b(sq0(?:atp|csp)-[0-9A-Za-z\-_]{22,43})\b",
        1,
        3.0,
        "A Square token reaches payments and customer data.",
        Some("https://developer.squareup.com/apps"),
    ),
    spec(
        "azure-storage-key",
        "Azure storage account key",
        Severity::Critical,
        r"(?i)AccountKey=([A-Za-z0-9+/=]{86,88})",
        1,
        3.5,
        WHY_CLOUD,
        Some(
            "https://portal.azure.com/#view/HubsExtension/BrowseResource/resourceType/Microsoft.Storage%2FStorageAccounts",
        ),
    ),
    spec(
        "azure-shared-key",
        "Azure shared access key",
        Severity::High,
        r"(?i)SharedAccessKey=([A-Za-z0-9+/=]{40,})",
        1,
        3.5,
        WHY_CLOUD,
        None,
    ),
    spec(
        "facebook-token",
        "Facebook access token",
        Severity::High,
        r"\b(EAACEdEose0cBA[0-9A-Za-z]+)\b",
        1,
        3.0,
        WHY_TOKEN,
        None,
    ),
    spec(
        "age-secret-key",
        "age secret key",
        Severity::High,
        r"\b(AGE-SECRET-KEY-1[QPZRY9X8GF2TVDW0S3JN54KHCE6MUA7L]{58})\b",
        1,
        0.0,
        "An age identity decrypts everything encrypted to it.",
        None,
    ),
    spec(
        "vault-token",
        "HashiCorp Vault token",
        Severity::High,
        r"\b(hvs\.[A-Za-z0-9_\-]{24,})\b",
        1,
        3.0,
        "A Vault token unlocks every secret its policies allow.",
        None,
    ),
    spec(
        "databricks-token",
        "Databricks token",
        Severity::High,
        r"\b(dapi[a-h0-9]{32})\b",
        1,
        3.0,
        WHY_TOKEN,
        None,
    ),
    spec(
        "linear-key",
        "Linear API key",
        Severity::High,
        r"\b(lin_api_[A-Za-z0-9]{40})\b",
        1,
        3.0,
        WHY_TOKEN,
        Some("https://linear.app/settings/api"),
    ),
    spec(
        "notion-token",
        "Notion token",
        Severity::High,
        r"\b((?:secret|ntn)_[A-Za-z0-9]{43,})\b",
        1,
        3.0,
        WHY_TOKEN,
        Some("https://www.notion.so/my-integrations"),
    ),
    spec(
        "postman-key",
        "Postman API key",
        Severity::High,
        r"\b(PMAK-[a-f0-9]{24}-[a-f0-9]{34})\b",
        1,
        3.0,
        WHY_TOKEN,
        None,
    ),
    spec(
        "grafana-token",
        "Grafana token",
        Severity::High,
        r"\b(gl(?:c|sa)_[A-Za-z0-9+/=_]{32,})\b",
        1,
        3.0,
        WHY_TOKEN,
        None,
    ),
    spec(
        "doppler-token",
        "Doppler token",
        Severity::High,
        r"\b(dp\.(?:pt|st|ct|sa|scim|audit)\.[A-Za-z0-9]{40,})\b",
        1,
        3.0,
        "A Doppler token reads the secrets of the projects it is scoped to.",
        None,
    ),
    spec(
        "onepassword-token",
        "1Password service account token",
        Severity::High,
        r"\b(ops_eyJ[A-Za-z0-9_\-]{50,})",
        1,
        3.0,
        "A 1Password service account token reads the vaults it was granted.",
        None,
    ),
    spec(
        "dropbox-token",
        "Dropbox token",
        Severity::Medium,
        r"\b(sl\.[A-Za-z0-9_\-]{130,})",
        1,
        3.0,
        WHY_TOKEN,
        None,
    ),
    spec(
        "twitter-bearer",
        "Twitter/X bearer token",
        Severity::Medium,
        r"\b(AAAAAAAAAAAAAAAAAAAAA[A-Za-z0-9%]{60,})",
        1,
        3.0,
        WHY_TOKEN,
        None,
    ),
    spec(
        "heroku-key",
        "Heroku API key",
        Severity::Medium,
        r#"(?i)heroku[a-z0-9_\-]{0,20}\s*[=:]\s*["']?([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})"#,
        1,
        0.0,
        WHY_CLOUD,
        Some("https://dashboard.heroku.com/account"),
    ),
    Spec {
        max_line: GENERIC_MAX_LINE,
        ..spec(
            "jwt",
            "JSON Web Token",
            Severity::Medium,
            r"\b(eyJ[A-Za-z0-9_\-]{8,}\.eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{16,})",
            1,
            3.0,
            "A signed JWT is a bearer credential until it expires; many never do. Check what it grants and revoke the session or key that signed it.",
            None,
        )
    },
    spec(
        "url-credentials",
        "Password in URL",
        Severity::High,
        r#"\b[a-zA-Z][a-zA-Z0-9+.\-]{1,20}://[^\s/:@'"]{1,64}:([^\s/@'"]{3,200})@[A-Za-z0-9.\-]"#,
        1,
        0.0,
        "A connection string with an embedded password gives direct access to the database or service it points at.",
        None,
    ),
    Spec {
        max_line: GENERIC_MAX_LINE,
        ..spec(
            "basic-auth-header",
            "Basic authorization header",
            Severity::Medium,
            r"(?i)authorization\W{1,3}\s*basic\s+([A-Za-z0-9+/=]{16,})",
            1,
            3.0,
            "Basic auth is just base64 of user:password; anyone can decode it.",
            None,
        )
    },
    Spec {
        max_line: GENERIC_MAX_LINE,
        ..spec(
            "bearer-token",
            "Bearer token",
            Severity::Medium,
            r"(?i)authorization\W{1,3}\s*bearer\s+([A-Za-z0-9_\-.=:+/]{20,})",
            1,
            3.0,
            WHY_TOKEN,
            None,
        )
    },
    Spec {
        max_line: GENERIC_MAX_LINE,
        ..spec(
            "query-secret",
            "Secret in URL query string",
            Severity::Medium,
            r#"(?i)[?&](?:api[_\-]?key|apikey|access[_\-]?token|auth[_\-]?token|token|secret|password|passwd|pwd|client[_\-]?secret)=([^&\s'"<>]{8,})"#,
            1,
            3.0,
            "Credentials in URLs end up in logs, browser history and proxies.",
            None,
        )
    },
    Spec {
        max_line: GENERIC_MAX_LINE,
        ..spec(
            "generic-credential",
            "Hard-coded credential",
            Severity::Medium,
            r#"(?i)(?:password|passwd|pwd|secret|api[_\-]?key|apikey|access[_\-]?token|auth[_\-]?token|client[_\-]?secret|secret[_\-]?key|private[_\-]?key|encryption[_\-]?key|signing[_\-]?key)[a-z0-9_\-]{0,20}["']?\s*(?:[:=]|=>|:=)\s*["']([^"'\s]{8,160})["']"#,
            1,
            2.5,
            WHY_GENERIC,
            None,
        )
    },
    Spec {
        paths: Some(is_config_file),
        ..spec(
            "config-credential",
            "Credential in configuration file",
            Severity::High,
            r#"(?i)^\s*(?:export\s+)?[A-Za-z0-9_.\-]*(?:PASSWORD|PASSWD|SECRET|TOKEN|API_?KEY|APIKEY|PRIVATE_?KEY|ACCESS_?KEY|AUTH|CREDENTIAL)[A-Za-z0-9_.\-]*\s*[=:]\s*["']?([^"'\s]{8,200})["']?(?:\s+#.*)?\s*$"#,
            1,
            2.5,
            "Configuration files (.env, .ini, .yaml, …) are the usual home of real credentials. If this value is real, every clone has it.",
            None,
        )
    },
    Spec {
        max_line: GENERIC_MAX_LINE,
        ..spec(
            "high-entropy",
            "High-entropy string near a secret-like name",
            Severity::Low,
            r#"(?i)(?:key|secret|token|password|passwd|pwd|auth|credential|private)[a-z0-9_\-]{0,20}["']?\s*(?:[:=]|=>)\s*["']?([A-Za-z0-9+/=_\-]{32,})["']?"#,
            1,
            4.5,
            "A long random-looking string assigned to a secret-like name. Often a real key with an unknown format; sometimes a hash or an ID.",
            None,
        )
    },
];

/// Rules whose match is only as good as its heuristics; they get the
/// stricter value checks (name-like values, dummy hosts).
fn is_generic_rule(id: &str) -> bool {
    matches!(
        id,
        "generic-credential"
            | "config-credential"
            | "high-entropy"
            | "url-credentials"
            | "query-secret"
            | "bearer-token"
            | "basic-auth-header"
    )
}

/// Hosts that only ever appear in documentation and tests.
fn is_placeholder_host(host: &str) -> bool {
    let h = host.to_lowercase();
    h == "localhost"
        || h.starts_with("127.")
        || h == "0.0.0.0"
        || h.starts_with("example.")
        || h.contains("example.com")
        || h.contains("example.org")
        || h.contains("example.net")
        || h.ends_with(".example")
        || h.ends_with(".test")
        || h.ends_with(".invalid")
        || h.ends_with(".localhost")
        || matches!(
            h.as_str(),
            "host" | "hostname" | "server" | "foo" | "bar" | "foopy"
        )
}

/// Lower-case substrings at least one of which every match of the rule
/// contains. Lines without any keyword of a rule skip its regex.
fn keywords_for(id: &str) -> &'static [&'static str] {
    match id {
        "private-key" | "encrypted-private-key" => &["private key"],
        "putty-key" => &["putty-user-key"],
        "aws-access-key" => &["akia", "asia"],
        "aws-secret-key" => &["aws", "amazon"],
        "github-token" => &["ghp_", "gho_", "ghu_", "ghs_", "ghr_"],
        "github-pat" => &["github_pat_"],
        "gitlab-token" => &["glpat-"],
        "slack-token" => &["xox"],
        "slack-webhook" => &["hooks.slack.com"],
        "stripe-live" => &["_live_"],
        "stripe-test" => &["_test_"],
        "google-api-key" => &["aiza"],
        "google-oauth-secret" => &["gocspx-"],
        "gcp-service-account" => &["private_key_id"],
        "openai-key" => &["t3blbkfj"],
        "anthropic-key" => &["sk-ant-"],
        "huggingface-token" => &["hf_"],
        "npm-token" => &["npm_"],
        "npmrc-auth" => &["_auth", "npmauthtoken"],
        "pypi-token" => &["pypi-"],
        "digitalocean-token" => &["_v1_"],
        "sendgrid-key" => &["sg."],
        "twilio-key" => &["sk"],
        "mailgun-key" => &["key-"],
        "mailchimp-key" => &["-us"],
        "telegram-bot-token" => &[":aa"],
        "discord-token" => &["discord", "token"],
        "discord-webhook" => &["discord"],
        "shopify-token" => &["shp"],
        "square-token" => &["sq0"],
        "azure-storage-key" => &["accountkey"],
        "azure-shared-key" => &["sharedaccesskey"],
        "facebook-token" => &["eaacedeose0cba"],
        "age-secret-key" => &["age-secret-key-"],
        "vault-token" => &["hvs."],
        "databricks-token" => &["dapi"],
        "linear-key" => &["lin_api_"],
        "notion-token" => &["secret_", "ntn_"],
        "postman-key" => &["pmak-"],
        "grafana-token" => &["glc_", "glsa_"],
        "doppler-token" => &["dp."],
        "onepassword-token" => &["ops_eyj"],
        "dropbox-token" => &["sl."],
        "twitter-bearer" => &["aaaaaaaaaaaaaaaaaaaaa"],
        "heroku-key" => &["heroku"],
        "jwt" => &["eyj"],
        "url-credentials" => &["://"],
        "basic-auth-header" => &["basic"],
        "bearer-token" => &["bearer"],
        "query-secret" => &["key=", "token=", "secret=", "password=", "passwd=", "pwd="],
        "generic-credential" => &["password", "passwd", "pwd", "secret", "key", "token"],
        "config-credential" => &[
            "password",
            "passwd",
            "secret",
            "token",
            "key",
            "auth",
            "credential",
        ],
        "high-entropy" => &[
            "key",
            "secret",
            "token",
            "password",
            "passwd",
            "pwd",
            "auth",
            "credential",
            "private",
        ],
        _ => &[],
    }
}

fn build_rules() -> Rules {
    Rules::from_specs(SPECS, keywords_for)
}

impl Rules {
    /// Compile a rule set; `keywords` gives the gate words of each rule id.
    pub(crate) fn from_specs(
        specs: &'static [Spec],
        keywords: fn(&str) -> &'static [&'static str],
    ) -> Rules {
        assert!(specs.len() <= 64, "rule bitset is a u64");
        let rules: Vec<Rule> = specs
            .iter()
            .map(|s| Rule {
                id: s.id,
                name: s.name,
                severity: s.severity,
                why: s.why,
                rotate: s.rotate,
                regex: Regex::new(s.pattern).unwrap_or_else(|e| panic!("rule {}: {e}", s.id)),
                group: s.group,
                min_entropy: s.min_entropy,
                paths: s.paths,
                max_line: s.max_line,
                strict: s.strict,
                validate: s.validate,
                prefilter: s.prefilter,
            })
            .collect();
        let mut kws: Vec<&'static str> = Vec::new();
        let mut kw_rules: Vec<u64> = Vec::new();
        for (i, s) in specs.iter().enumerate() {
            let words = keywords(s.id);
            assert!(!words.is_empty(), "rule {} has no keywords", s.id);
            for w in words {
                match kws.iter().position(|k| k == w) {
                    Some(p) => kw_rules[p] |= 1 << i,
                    None => {
                        kws.push(w);
                        kw_rules.push(1 << i);
                    }
                }
            }
        }
        let keywords = AhoCorasick::builder()
            .ascii_case_insensitive(true)
            .build(&kws)
            .expect("keyword automaton");
        Rules {
            rules,
            keywords,
            kw_rules,
        }
    }
}

/// One secret found in one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// Index into [`Rules::rules`].
    pub rule: usize,
    /// Byte range of the secret within the line.
    pub start: usize,
    pub end: usize,
    pub secret: String,
}

/// Shannon entropy in bits per character.
pub fn entropy(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    let mut counts: HashMap<char, usize> = HashMap::new();
    let mut n = 0usize;
    for c in s.chars() {
        *counts.entry(c).or_insert(0) += 1;
        n += 1;
    }
    let n = n as f64;
    counts
        .values()
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

const PLACEHOLDER_WORDS: &[&str] = &[
    "example",
    "sample",
    "placeholder",
    "your_",
    "your-",
    "yourkey",
    "yourtoken",
    "your ",
    "xxxx",
    "changeme",
    "change_me",
    "change-me",
    "dummy",
    "fake",
    "insert",
    "redacted",
    "replace",
    "<",
    ">",
    "${",
    "{{",
    "{}",
    "%s",
    "%(",
    "todo",
    "fixme",
    "abcdef",
    "123456",
    "password",
    "passwd",
    "_here",
    "-here",
    "******",
    "....",
    "undefined",
    "process.env",
    "os.environ",
    "getenv",
    "mock",
    "lorem",
];

/// Whole values that are stand-ins, not secrets.
const PLACEHOLDER_EXACT: &[&str] = &[
    "foo", "bar", "baz", "qux", "quux", "user", "pass", "pwd", "test", "admin", "root", "guest",
    "hunter2", "mypwd", "mypass", "none", "null", "nil", "true", "false", "empty", "default",
    "demo", "changeit", "secret", "letmein", "welcome", "qwerty",
];

/// Values that are clearly not real secrets: templates, variable
/// references, documentation examples, repeated characters.
pub fn is_placeholder(secret: &str) -> bool {
    let s = secret.trim_matches(|c: char| c == '"' || c == '\'');
    if s.len() < 3 {
        return true;
    }
    if s.starts_with('$') || s.starts_with('%') || s.starts_with('{') || s.starts_with('@') {
        return true;
    }
    let lower = s.to_lowercase();
    if PLACEHOLDER_EXACT.contains(&lower.as_str()) {
        return true;
    }
    if PLACEHOLDER_WORDS.iter().any(|w| lower.contains(w)) {
        return true;
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap_or(' ');
    if chars.all(|c| c == first) {
        return true;
    }
    // Only three or fewer distinct characters in a long string: "abababab".
    let distinct: HashSet<char> = s.chars().collect();
    distinct.len() <= 3 && s.len() >= 8
}

fn has_allow_marker(line: &str) -> bool {
    line.contains("hadion:allow")
        || line.contains("gitleaks:allow")
        || line.contains("pragma: allowlist secret")
        || line.contains("nosecret")
}

/// Scan one line of `path`. Overlapping matches keep the most severe rule.
pub fn scan_line(rules: &Rules, path: &str, line: &str) -> Vec<Hit> {
    if line.len() > MAX_LINE_BYTES || line.is_empty() || has_allow_marker(line) {
        return Vec::new();
    }
    let mut mask: u64 = 0;
    for m in rules.keywords.find_overlapping_iter(line) {
        mask |= rules.kw_rules[m.pattern().as_usize()];
    }
    if mask == 0 {
        return Vec::new();
    }
    let mut hits: Vec<Hit> = Vec::new();
    for (idx, rule) in rules.rules.iter().enumerate() {
        if mask & (1u64 << idx) == 0 || line.len() > rule.max_line {
            continue;
        }
        if let Some(allowed) = rule.paths
            && !allowed(path)
        {
            continue;
        }
        if let Some(pre) = rule.prefilter
            && !pre(line)
        {
            continue;
        }
        for caps in rule.regex.captures_iter(line) {
            let Some(m) = caps.get(rule.group).or_else(|| caps.get(0)) else {
                continue;
            };
            let secret = m.as_str();
            if let Some(valid) = rule.validate
                && !valid(secret)
            {
                continue;
            }
            if rule.group != 0 && rule.strict {
                if is_placeholder(secret) {
                    continue;
                }
                if rule.min_entropy > 0.0 && entropy(secret) < rule.min_entropy {
                    continue;
                }
                if is_generic_rule(rule.id) {
                    // Never flag URLs, calls or paths as passwords, nor a
                    // value that is itself a setting name ("api_auth_token").
                    let lower = secret.to_lowercase();
                    // Plain words and identifiers ("Contrasenya", "isPrivate")
                    // are not credentials; real ones carry digits or symbols.
                    let wordy = secret.chars().all(|c| c.is_alphabetic()) && secret.len() < 24;
                    if secret.contains("://")
                        || secret.contains('(')
                        || secret.contains('\\')
                        || secret.starts_with('/')
                        || wordy
                        || [
                            "token",
                            "key",
                            "password",
                            "passwd",
                            "secret",
                            "auth",
                            "credential",
                            "nonce",
                        ]
                        .iter()
                        .any(|w| lower.contains(w))
                    {
                        continue;
                    }
                    if rule.id == "url-credentials"
                        && let Some(whole) = caps.get(0)
                    {
                        // The whole match ends with `@` + first host character.
                        let rest = &line[whole.end().saturating_sub(1)..];
                        let host: String = rest
                            .chars()
                            .take_while(|c| {
                                !matches!(c, '/' | ':' | '?' | '#' | '"' | '\'' | ')' | ' ' | '\t')
                            })
                            .collect();
                        if is_placeholder_host(&host) {
                            continue;
                        }
                    }
                }
            }
            hits.push(Hit {
                rule: idx,
                start: m.start(),
                end: m.end(),
                secret: secret.to_owned(),
            });
        }
    }
    // Same span matched by several rules: keep the most severe (then the
    // more specific, i.e. earlier, rule).
    hits.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then(
                rules.rules[b.rule]
                    .severity
                    .cmp(&rules.rules[a.rule].severity),
            )
            .then(a.rule.cmp(&b.rule))
    });
    let mut out: Vec<Hit> = Vec::with_capacity(hits.len());
    for h in hits {
        if let Some(last) = out.last()
            && h.start < last.end
        {
            continue;
        }
        out.push(h);
    }
    out
}

/// `AKIA••••••••••••MPLE`: first and last characters kept, the middle hidden.
pub fn redact(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    let n = chars.len();
    if n <= 8 {
        return "•".repeat(n.max(4));
    }
    let keep = if n >= 20 { 4 } else { 2 };
    let head: String = chars[..keep].iter().collect();
    let tail: String = chars[n - keep..].iter().collect();
    format!("{head}{}{tail}", "•".repeat((n - 2 * keep).min(24)))
}

/// Stable identifier of a (rule, secret) pair, for dismissals. FNV-1a over
/// the pair, hex encoded; the secret cannot be recovered from it in
/// practice, and it never leaves the machine anyway.
pub fn fingerprint(rule: &str, secret: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in rule.bytes().chain([0u8]).chain(secret.bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Trim a long line to the part around the match. Returns the snippet and
/// the match range within it.
pub(crate) fn snippet(line: &str, start: usize, end: usize) -> (String, usize, usize) {
    let line = line.trim_end_matches(['\r', '\n']);
    let end = end.min(line.len());
    let start = start.min(end);
    let mut from = start.saturating_sub(SNIPPET_CONTEXT);
    while from > 0 && !line.is_char_boundary(from) {
        from -= 1;
    }
    let mut to = (end + SNIPPET_CONTEXT).min(line.len());
    while to < line.len() && !line.is_char_boundary(to) {
        to += 1;
    }
    let mut s = String::new();
    if from > 0 {
        s.push('…');
    }
    let prefix = s.len();
    s.push_str(&line[from..to]);
    if to < line.len() {
        s.push('…');
    }
    (s, prefix + (start - from), prefix + (end - from))
}

// ---------------------------------------------------------------------------
// Sensitive file names
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRule {
    pub id: &'static str,
    pub name: &'static str,
    pub severity: Severity,
    pub why: &'static str,
}

const fn frule(
    id: &'static str,
    name: &'static str,
    severity: Severity,
    why: &'static str,
) -> FileRule {
    FileRule {
        id,
        name,
        severity,
        why,
    }
}

static FILE_RULES: &[FileRule] = &[
    frule(
        "env-file",
        "Environment file",
        Severity::High,
        "Files named .env hold the configuration of a deployment, usually including database passwords and API keys. They belong in .gitignore, with a committed .env.example as documentation.",
    ),
    frule(
        "ssh-private-key",
        "SSH private key",
        Severity::Critical,
        "An SSH private key logs in wherever its public half is authorised. Generate a new pair and remove this one from every authorized_keys file.",
    ),
    frule(
        "ssh-dir",
        "File from an .ssh directory",
        Severity::High,
        "Contents of ~/.ssh (keys, config, known hosts) describe how to reach your servers.",
    ),
    frule(
        "key-file",
        "Key file",
        Severity::High,
        "Files with key or certificate-store extensions (.key, .p12, .pfx, .jks, .keystore, .ppk, .p8) usually carry private keys.",
    ),
    frule(
        "pem-file",
        "PEM file",
        Severity::Medium,
        "A .pem file may be a public certificate (harmless) or a private key (critical). The content scan tells which; treat it as a key unless proven otherwise.",
    ),
    frule(
        "pgp-key",
        "PGP keyring",
        Severity::High,
        "A PGP secret keyring decrypts and signs as its owner.",
    ),
    frule(
        "git-credentials",
        "Git credential store",
        Severity::Critical,
        ".git-credentials holds plaintext passwords and tokens for every remote you logged in to.",
    ),
    frule(
        "netrc",
        "netrc credentials",
        Severity::High,
        ".netrc holds plaintext logins and passwords for remote hosts.",
    ),
    frule(
        "npmrc",
        "npm configuration",
        Severity::Medium,
        ".npmrc often contains an _authToken for the registry. The content scan reports it when present.",
    ),
    frule(
        "pypirc",
        "PyPI credentials",
        Severity::High,
        ".pypirc holds the password or token used to upload packages.",
    ),
    frule(
        "htpasswd",
        "htpasswd file",
        Severity::Medium,
        "Password hashes for HTTP basic auth; crackable offline.",
    ),
    frule(
        "db-credentials",
        "Database client credentials",
        Severity::High,
        "Client credential files (.pgpass, .my.cnf, .boto, .s3cfg) hold plaintext passwords and keys.",
    ),
    frule(
        "cloud-credentials",
        "Cloud credentials file",
        Severity::Critical,
        "Cloud CLI credential files (AWS, GCP service accounts, Azure, Firebase admin SDK) grant full API access as that identity.",
    ),
    frule(
        "kube-config",
        "Kubernetes config",
        Severity::High,
        "A kubeconfig carries cluster certificates and tokens: full access to the cluster.",
    ),
    frule(
        "docker-config",
        "Docker registry credentials",
        Severity::High,
        "Docker config.json stores registry logins, often base64 of user:password.",
    ),
    frule(
        "terraform-state",
        "Terraform state",
        Severity::High,
        "Terraform state stores every resource attribute in plaintext, including generated passwords and keys.",
    ),
    frule(
        "terraform-vars",
        "Terraform variables",
        Severity::Medium,
        ".tfvars files frequently hold provider credentials and passwords.",
    ),
    frule(
        "secrets-file",
        "Secrets file",
        Severity::High,
        "A file named secrets.* or credentials.* is where people put credentials.",
    ),
    frule(
        "vault-token",
        "Vault token file",
        Severity::High,
        "A Vault token file unlocks every secret its policies allow.",
    ),
    frule(
        "password-database",
        "Password database",
        Severity::High,
        "KeePass databases hold all your passwords; offline brute force is cheap.",
    ),
    frule(
        "vpn-config",
        "VPN configuration",
        Severity::Medium,
        "OpenVPN and WireGuard configs usually embed private keys and certificates.",
    ),
    frule(
        "shell-history",
        "Shell history",
        Severity::Medium,
        "Shell and database client histories record passwords typed on the command line.",
    ),
    frule(
        "database-dump",
        "Database or data dump",
        Severity::Low,
        "Dumps (.sql, .sqlite, .db, .dump, .bak) often contain personal data and password hashes.",
    ),
    frule(
        "wp-config",
        "WordPress configuration",
        Severity::High,
        "wp-config.php holds the database password and authentication salts.",
    ),
    frule(
        "rails-secrets",
        "Rails credentials",
        Severity::High,
        "config/master.key decrypts credentials.yml.enc; database.yml holds passwords.",
    ),
    frule(
        "env-backup",
        "Editor backup of a secret file",
        Severity::Medium,
        "Backup and swap files (.env~, .env.bak, .swp) copy secret files that would otherwise be ignored.",
    ),
];

fn file_rule(id: &str) -> &'static FileRule {
    FILE_RULES
        .iter()
        .find(|r| r.id == id)
        .unwrap_or_else(|| panic!("file rule {id}"))
}

pub fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn extension(base: &str) -> &str {
    match base.rfind('.') {
        Some(i) if i > 0 => &base[i + 1..],
        _ => "",
    }
}

/// Sensitive-looking file name, if any. Public keys and `.env.example`
/// style templates are not flagged.
pub fn check_path(path: &str) -> Option<&'static FileRule> {
    let base = basename(path);
    let lower = base.to_lowercase();
    let ext = extension(&lower).to_owned();
    let dir_lower = path.to_lowercase();
    let in_dir = |d: &str| dir_lower.contains(&format!("{d}/")) || dir_lower.starts_with(d);

    let env_template = [
        ".env.example",
        ".env.sample",
        ".env.template",
        ".env.dist",
        ".env.schema",
        ".env.defaults",
        ".env.test.example",
        "example.env",
        "sample.env",
        "template.env",
    ]
    .contains(&lower.as_str())
        || lower.ends_with(".example")
        || lower.ends_with(".sample")
        || lower.ends_with(".template");
    if !env_template {
        if lower.starts_with(".env")
            && (lower.ends_with('~')
                || lower.ends_with(".bak")
                || lower.ends_with(".swp")
                || lower.ends_with(".orig"))
        {
            return Some(file_rule("env-backup"));
        }
        if lower == ".env" || lower.starts_with(".env.") || lower.ends_with(".env") {
            return Some(file_rule("env-file"));
        }
    }
    if [
        "id_rsa",
        "id_dsa",
        "id_ecdsa",
        "id_ed25519",
        "id_ecdsa_sk",
        "id_ed25519_sk",
    ]
    .contains(&lower.as_str())
        || (lower.starts_with("id_") && ext == "ppk")
    {
        return Some(file_rule("ssh-private-key"));
    }
    if in_dir(".ssh") && !lower.ends_with(".pub") {
        return Some(file_rule("ssh-dir"));
    }
    if lower == ".git-credentials" {
        return Some(file_rule("git-credentials"));
    }
    if lower == ".netrc" || lower == "_netrc" {
        return Some(file_rule("netrc"));
    }
    if lower == ".npmrc" || lower == ".yarnrc" || lower == ".yarnrc.yml" {
        return Some(file_rule("npmrc"));
    }
    if lower == ".pypirc" {
        return Some(file_rule("pypirc"));
    }
    if lower == ".htpasswd" {
        return Some(file_rule("htpasswd"));
    }
    if [
        ".pgpass",
        ".my.cnf",
        ".boto",
        ".s3cfg",
        ".dbeaver-data-sources.xml",
    ]
    .contains(&lower.as_str())
    {
        return Some(file_rule("db-credentials"));
    }
    if (in_dir(".aws") && (lower == "credentials" || lower == "config"))
        || lower == "credentials"
        || lower == "credentials.json"
        || lower.starts_with("service-account") && ext == "json"
        || lower.starts_with("service_account") && ext == "json"
        || lower.contains("firebase-adminsdk") && ext == "json"
        || lower.starts_with("client_secret") && ext == "json"
        || lower == "azureprofile.json"
        || lower == "accesstokens.json"
        || lower == ".azure-credentials"
    {
        return Some(file_rule("cloud-credentials"));
    }
    if lower == "kubeconfig"
        || lower.starts_with("kubeconfig.")
        || (in_dir(".kube") && lower == "config")
    {
        return Some(file_rule("kube-config"));
    }
    if in_dir(".docker") && lower == "config.json" {
        return Some(file_rule("docker-config"));
    }
    if ext == "tfstate" || lower.ends_with(".tfstate.backup") {
        return Some(file_rule("terraform-state"));
    }
    if ext == "tfvars" || lower.ends_with(".tfvars.json") {
        return Some(file_rule("terraform-vars"));
    }
    if lower == "wp-config.php" {
        return Some(file_rule("wp-config"));
    }
    if lower == "master.key" || (lower == "database.yml" && in_dir("config")) {
        return Some(file_rule("rails-secrets"));
    }
    if [".vault-token", ".vault_token"].contains(&lower.as_str()) {
        return Some(file_rule("vault-token"));
    }
    if ext == "kdbx" || ext == "kdb" || ext == "agilekeychain" {
        return Some(file_rule("password-database"));
    }
    if ext == "ovpn" || (ext == "conf" && lower.starts_with("wg")) {
        return Some(file_rule("vpn-config"));
    }
    if [
        ".bash_history",
        ".zsh_history",
        ".sh_history",
        ".psql_history",
        ".mysql_history",
        ".python_history",
        ".node_repl_history",
        ".irb_history",
    ]
    .contains(&lower.as_str())
    {
        return Some(file_rule("shell-history"));
    }
    if [
        "secring.gpg",
        "secring.pgp",
        "private.gpg",
        "private-keys-v1.d",
    ]
    .contains(&lower.as_str())
        || in_dir(".gnupg")
        || lower.ends_with("secret.asc")
        || lower.ends_with("private.asc")
        || lower.ends_with("-private.key")
    {
        return Some(file_rule("pgp-key"));
    }
    if ["key", "p12", "pfx", "jks", "keystore", "ppk", "p8", "bks"].contains(&ext.as_str()) {
        return Some(file_rule("key-file"));
    }
    if ext == "pem" {
        return Some(file_rule("pem-file"));
    }
    let stem = lower.strip_suffix(&format!(".{ext}")).unwrap_or(&lower);
    if ["secrets", "secret", ".secrets", "credentials", "creds"].contains(&stem)
        && [
            "json",
            "yml",
            "yaml",
            "toml",
            "ini",
            "txt",
            "env",
            "properties",
            "xml",
            "",
        ]
        .contains(&ext.as_str())
    {
        return Some(file_rule("secrets-file"));
    }
    if ["sql", "sqlite", "sqlite3", "db", "dump", "bak", "mdb"].contains(&ext.as_str()) {
        return Some(file_rule("database-dump"));
    }
    None
}

/// Files whose contents are not worth scanning: lockfiles, minified
/// bundles, media and archives.
pub fn skip_content(path: &str) -> bool {
    let base = basename(path).to_lowercase();
    if [
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "cargo.lock",
        "poetry.lock",
        "pipfile.lock",
        "composer.lock",
        "gemfile.lock",
        "go.sum",
        "flake.lock",
        "bun.lockb",
        ".ds_store",
        "thumbs.db",
    ]
    .contains(&base.as_str())
    {
        return true;
    }
    if base.ends_with(".min.js") || base.ends_with(".min.css") || base.ends_with(".bundle.js") {
        return true;
    }
    // Translation catalogues: UI strings, never credentials.
    if path.contains(".lproj/") || path.contains("/locales/") || path.contains("/i18n/") {
        return true;
    }
    let ext = extension(&base);
    if ["strings", "xliff", "xlf", "po", "pot", "resx", "arb"].contains(&ext) {
        return true;
    }
    [
        "map", "svg", "png", "jpg", "jpeg", "gif", "ico", "webp", "bmp", "tif", "tiff", "psd",
        "woff", "woff2", "ttf", "otf", "eot", "pdf", "zip", "gz", "tgz", "bz2", "xz", "zst", "7z",
        "rar", "jar", "war", "class", "so", "dll", "dylib", "exe", "bin", "o", "a", "lib", "obj",
        "pyc", "pyo", "wasm", "mp3", "mp4", "m4a", "mov", "avi", "mkv", "wav", "ogg", "flac", "pb",
        "onnx", "parquet", "pak", "lock", "snap", "icns", "heic", "avif",
    ]
    .contains(&ext)
}

/// Test fixtures, examples, documentation and vendored code: a match there
/// is less likely to be a live credential of this project.
pub fn is_test_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    [
        "/test/",
        "/tests/",
        "/__tests__/",
        "/testing/",
        "/spec/",
        "/fixtures/",
        "/fixture/",
        "/testdata/",
        "/test_data/",
        "/mocks/",
        "/__mocks__/",
        "/__snapshots__/",
        "/examples/",
        "/example/",
        "/samples/",
        "/docs/",
        "/third_party/",
        "/vendor/",
        "/node_modules/",
        "/external/",
    ]
    .iter()
    .any(|d| format!("/{lower}").contains(d))
        || {
            let base = basename(&lower);
            [
                "_test",
                "test_",
                ".test",
                "tests",
                "unittest",
                "browsertest",
                "spec.",
                "_spec",
                "fixture",
                "mock",
                "example",
                "sample",
            ]
            .iter()
            .any(|w| base.contains(w))
        }
}

/// Final adjustments to sensitive-file findings: fixtures are expected in
/// test data, and everything under a test path is one step less urgent.
fn settle_files(files: &mut Vec<FileFinding>) {
    files.retain(|f| !(f.rule.id == "database-dump" && is_test_path(&f.path)));
    for f in files.iter_mut() {
        if is_test_path(&f.path) {
            f.severity = f.severity.lower();
        }
    }
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

/// Where an occurrence was seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Place {
    /// In the HEAD tree.
    Head,
    /// Tracked file as it is on disk.
    Worktree,
    /// Added lines of the staged diff.
    Staged,
    /// Added lines of the unstaged diff.
    Unstaged,
    /// An untracked, not ignored file.
    Untracked,
    /// Added lines of a past commit.
    History,
    /// Inside a stash entry.
    Stash,
    /// An object no ref reaches any more (`git fsck --unreachable`).
    Unreachable,
}

impl Place {
    pub fn label(self) -> &'static str {
        match self {
            Place::Head => "HEAD",
            Place::Worktree => "working tree",
            Place::Staged => "staged",
            Place::Unstaged => "unstaged",
            Place::Untracked => "untracked",
            Place::History => "history",
            Place::Stash => "stash",
            Place::Unreachable => "unreachable",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitRef {
    pub id: Oid,
    pub time: i64,
    pub author: String,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Occurrence {
    pub place: Place,
    pub path: String,
    /// 1-based line number; 0 when unknown.
    pub line: u32,
    pub commit: Option<Oid>,
    /// The line around the match (shortened); the secret sits at `start..end`.
    pub text: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug)]
pub struct Finding {
    pub rule: &'static str,
    pub name: &'static str,
    pub severity: Severity,
    pub why: &'static str,
    pub rotate: Option<&'static str>,
    pub secret: String,
    pub fingerprint: String,
    pub occurrences: Vec<Occurrence>,
    pub occurrence_count: usize,
    /// Commits that added the secret, oldest first.
    pub commits: Vec<CommitRef>,
    pub commit_count: usize,
    pub at_head: bool,
    pub in_worktree: bool,
    pub staged: bool,
    /// Some occurrence is reachable from a remote-tracking branch.
    pub pushed: bool,
    /// Every occurrence is in a test, fixture or example path.
    pub test_only: bool,
}

impl Finding {
    pub fn first_commit(&self) -> Option<&CommitRef> {
        self.commits.first()
    }

    pub fn last_commit(&self) -> Option<&CommitRef> {
        self.commits.last()
    }

    pub fn paths(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.occurrences.iter().map(|o| o.path.as_str()).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    pub fn redacted(&self) -> String {
        redact(&self.secret)
    }

    /// Only in uncommitted changes: nothing has left the machine yet.
    pub fn uncommitted_only(&self) -> bool {
        !self.at_head && self.commits.is_empty() && self.in_worktree
    }

    /// Was committed at some point but is gone from HEAD.
    pub fn history_only(&self) -> bool {
        !self.at_head && !self.in_worktree && !self.commits.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct FileFinding {
    pub rule: &'static FileRule,
    pub severity: Severity,
    pub path: String,
    pub fingerprint: String,
    /// In the index.
    pub tracked: bool,
    /// Staged addition.
    pub staged: bool,
    /// On disk, not tracked and not ignored: `git add .` would take it.
    pub untracked: bool,
    pub size: Option<u64>,
    /// Commit that added the file, when history was scanned.
    pub added_in: Option<CommitRef>,
    pub pushed: bool,
    /// Content findings inside this file.
    pub secrets: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigFix {
    /// `git remote set-url NAME URL`.
    SetRemoteUrl { name: String, url: String },
}

#[derive(Clone, Debug)]
pub struct ConfigFinding {
    pub id: &'static str,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    pub why: &'static str,
    pub fix: Option<ConfigFix>,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub files_listed: usize,
    pub files_scanned: usize,
    pub skipped_binary: usize,
    pub skipped_large: usize,
    pub skipped_kind: usize,
    pub bytes: u64,
    pub lines: u64,
    pub commits: usize,
    pub commits_total: Option<usize>,
    pub history: bool,
    pub all_refs: bool,
    pub remotes: usize,
    pub pushed_commits: usize,
    pub head_pushed: bool,
    pub elapsed_ms: u64,
    pub cancelled: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub files: Vec<FileFinding>,
    pub config: Vec<ConfigFinding>,
    pub stats: Stats,
    /// Commands run, for the activity log (secrets never appear in them).
    pub commands: Vec<String>,
}

impl Report {
    pub fn count(&self, s: Severity) -> usize {
        self.findings.iter().filter(|f| f.severity == s).count()
            + self.files.iter().filter(|f| f.severity == s).count()
            + self.config.iter().filter(|f| f.severity == s).count()
    }

    pub fn total(&self) -> usize {
        self.findings.len() + self.files.len() + self.config.len()
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    /// Findings that would go into the next commit.
    pub fn staged(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| f.staged)
    }

    pub fn staged_files(&self) -> impl Iterator<Item = &FileFinding> {
        self.files.iter().filter(|f| f.staged)
    }

    /// Fold another report's working-tree knowledge into this one (the
    /// quick scan of staged and unstaged changes into a full report).
    pub fn merge(&mut self, other: &Report) {
        for f in &other.findings {
            match self
                .findings
                .iter_mut()
                .find(|m| m.fingerprint == f.fingerprint)
            {
                Some(m) => {
                    for o in &f.occurrences {
                        if !m
                            .occurrences
                            .iter()
                            .any(|x| x.place == o.place && x.path == o.path && x.line == o.line)
                        {
                            m.occurrences.push(o.clone());
                            m.occurrence_count += 1;
                        }
                    }
                    m.staged |= f.staged;
                    m.in_worktree |= f.in_worktree;
                }
                None => self.findings.push(f.clone()),
            }
        }
        for f in &other.files {
            match self.files.iter_mut().find(|m| m.path == f.path) {
                Some(m) => {
                    m.staged |= f.staged;
                    m.untracked |= f.untracked;
                    m.tracked |= f.tracked;
                    if m.size.is_none() {
                        m.size = f.size;
                    }
                }
                None => self.files.push(f.clone()),
            }
        }
        sort_report(self);
    }
}

fn sort_report(r: &mut Report) {
    r.findings.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then(b.staged.cmp(&a.staged))
            .then(b.at_head.cmp(&a.at_head))
            .then(b.in_worktree.cmp(&a.in_worktree))
            .then(b.commit_count.cmp(&a.commit_count))
            .then_with(|| a.name.cmp(b.name))
    });
    r.files.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then(b.staged.cmp(&a.staged))
            .then(b.tracked.cmp(&a.tracked))
            .then_with(|| a.path.cmp(&b.path))
    });
    r.config.sort_by_key(|c| std::cmp::Reverse(c.severity));
}

// ---------------------------------------------------------------------------
// Scanning
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryScope {
    /// Newest commits to read; 0 = all.
    pub max_commits: usize,
    /// All branches, tags and remotes instead of HEAD only.
    pub all_refs: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanOptions {
    /// Read every tracked and untracked file on disk.
    pub worktree: bool,
    pub history: Option<HistoryScope>,
}

/// Progress of a running scan, reported to the UI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub phase: String,
    pub done: usize,
    pub total: Option<usize>,
    pub bytes: u64,
    pub findings: usize,
}

/// Accumulates occurrences per (rule, secret).
pub(crate) struct Collector<'r> {
    pub(crate) rules: &'r Rules,
    map: HashMap<(usize, String), Acc>,
    order: Vec<(usize, String)>,
}

#[derive(Default)]
struct Acc {
    occurrences: Vec<Occurrence>,
    count: usize,
    commits: HashMap<Oid, CommitRef>,
    commit_count: usize,
}

impl<'r> Collector<'r> {
    pub(crate) fn new(rules: &'r Rules) -> Self {
        Self {
            rules,
            map: HashMap::new(),
            order: Vec::new(),
        }
    }

    pub(crate) fn add(&mut self, hit: Hit, occ: Occurrence, commit: Option<&CommitRef>) {
        let key = (hit.rule, hit.secret);
        let acc = match self.map.get_mut(&key) {
            Some(a) => a,
            None => {
                self.order.push(key.clone());
                self.map.entry(key).or_default()
            }
        };
        acc.count += 1;
        if acc.occurrences.len() < MAX_OCCURRENCES {
            acc.occurrences.push(occ);
        }
        if let Some(c) = commit
            && !acc.commits.contains_key(&c.id)
        {
            acc.commit_count += 1;
            if acc.commits.len() < MAX_COMMITS {
                acc.commits.insert(c.id, c.clone());
            }
        }
    }

    pub(crate) fn scan_text(
        &mut self,
        path: &str,
        text: &str,
        place: Place,
        commit: Option<&CommitRef>,
    ) -> u64 {
        let mut lines = 0u64;
        for (i, line) in text.split('\n').enumerate() {
            lines += 1;
            for hit in scan_line(self.rules, path, line) {
                let (text, start, end) = snippet(line, hit.start, hit.end);
                self.add(
                    hit,
                    Occurrence {
                        place,
                        path: path.to_owned(),
                        line: i as u32 + 1,
                        commit: commit.map(|c| c.id),
                        text,
                        start,
                        end,
                    },
                    commit,
                );
            }
        }
        lines
    }

    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }

    /// Occurrences recorded so far, across all findings.
    pub(crate) fn occurrence_total(&self) -> usize {
        self.map.values().map(|a| a.count).sum()
    }

    /// Build the findings. `changed` lists the tracked paths whose content
    /// differs from HEAD: a working-tree occurrence in any other tracked
    /// file is therefore also in HEAD. `None` when HEAD was not checked.
    pub(crate) fn finish(
        self,
        changed: Option<&HashSet<String>>,
        pushed: &HashSet<Oid>,
        head_pushed: bool,
    ) -> Vec<Finding> {
        let mut out = Vec::with_capacity(self.map.len());
        for key in self.order {
            let Some(acc) = self.map.get(&key) else {
                continue;
            };
            let rule = &self.rules.rules[key.0];
            let mut occurrences = acc.occurrences.clone();
            let count = acc.count;
            if let Some(changed) = changed {
                for o in &mut occurrences {
                    if o.place == Place::Worktree && !changed.contains(&o.path) {
                        o.place = Place::Head;
                    }
                }
            }
            let at_head = occurrences.iter().any(|o| o.place == Place::Head);
            let mut commits: Vec<CommitRef> = acc.commits.values().cloned().collect();
            commits.sort_by_key(|c| c.time);
            // Judged on what the scans saw, before HEAD hits replaced the
            // working-tree copies of the same lines.
            let in_worktree = acc.occurrences.iter().any(|o| {
                matches!(
                    o.place,
                    Place::Worktree | Place::Staged | Place::Unstaged | Place::Untracked
                )
            });
            let staged = acc.occurrences.iter().any(|o| o.place == Place::Staged);
            let test_only =
                !occurrences.is_empty() && occurrences.iter().all(|o| is_test_path(&o.path));
            let severity = if test_only {
                rule.severity.lower()
            } else {
                rule.severity
            };
            let pushed = commits.iter().any(|c| pushed.contains(&c.id)) || (at_head && head_pushed);
            out.push(Finding {
                rule: rule.id,
                name: rule.name,
                severity,
                why: rule.why,
                rotate: rule.rotate,
                fingerprint: fingerprint(rule.id, &key.1),
                secret: key.1,
                occurrences,
                occurrence_count: count,
                commits,
                commit_count: acc.commit_count,
                at_head,
                in_worktree,
                staged,
                pushed,
                test_only,
            });
        }
        out
    }
}

/// Receives what [`walk_patch`] finds in a patch stream.
pub(crate) trait PatchSink {
    fn commit(&mut self, _c: &CommitRef) {}
    fn new_file(&mut self, _path: &str) {}
    fn line(&mut self, path: &str, line: u32, text: &str);
    fn progress(&mut self, _bytes: u64) {}
}

/// Parse the stream of `git log -p -U0 --format=<\x01 header>` (or a plain
/// `git diff -U0`), reporting commits, new files and added lines.
pub(crate) fn walk_patch<R: BufRead, S: PatchSink>(
    mut reader: R,
    cancel: &AtomicBool,
    sink: &mut S,
) -> std::io::Result<u64> {
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut bytes = 0u64;
    let mut path: Option<String> = None;
    let mut new_line: u32 = 0;
    let mut binary = false;
    let mut expect_plus_header = false;
    let mut since_progress = 0u64;
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        since_progress += n as u64;
        if since_progress > 4 * 1024 * 1024 {
            since_progress = 0;
            sink.progress(bytes);
            if cancel.load(Ordering::Relaxed) {
                break;
            }
        }
        let raw = &buf[..];
        if raw.first() == Some(&0x01) {
            let text = String::from_utf8_lossy(&raw[1..]);
            let text = text.trim_end_matches(['\r', '\n']);
            let mut parts = text.splitn(4, '\0');
            let sha = parts.next().unwrap_or("");
            let time = parts
                .next()
                .and_then(|t| t.parse::<i64>().ok())
                .unwrap_or(0);
            let author = parts.next().unwrap_or("").to_owned();
            let summary = parts.next().unwrap_or("").to_owned();
            if let Ok(id) = Oid::from_str(sha) {
                sink.commit(&CommitRef {
                    id,
                    time,
                    author,
                    summary,
                });
            }
            path = None;
            binary = false;
            continue;
        }
        if raw.starts_with(b"diff --git ") {
            path = None;
            binary = false;
            expect_plus_header = false;
            new_line = 0;
            continue;
        }
        if raw.starts_with(b"Binary files ") || raw.starts_with(b"GIT binary patch") {
            binary = true;
            continue;
        }
        if raw.starts_with(b"--- ") {
            expect_plus_header = true;
            continue;
        }
        if expect_plus_header && raw.starts_with(b"+++ ") {
            expect_plus_header = false;
            let p = String::from_utf8_lossy(&raw[4..]);
            let p = p.trim_end_matches(['\r', '\n']);
            let p = p.trim_matches('"');
            path = if p == "/dev/null" {
                None
            } else {
                Some(p.strip_prefix("b/").unwrap_or(p).to_owned())
            };
            continue;
        }
        expect_plus_header = false;
        if raw.starts_with(b"new file mode") {
            // The path arrives with the `+++` header; report it at the first hunk.
            new_line = u32::MAX;
            continue;
        }
        if raw.starts_with(b"@@ ") {
            if new_line == u32::MAX
                && let Some(p) = &path
            {
                sink.new_file(p);
            }
            new_line = hunk_new_start(raw);
            continue;
        }
        if binary {
            continue;
        }
        let Some(p) = &path else { continue };
        match raw.first() {
            Some(b'+') if new_line != u32::MAX => {
                let content = String::from_utf8_lossy(&raw[1..]);
                let content = content.trim_end_matches(['\r', '\n']);
                sink.line(p, new_line, content);
                new_line = new_line.saturating_add(1);
            }
            Some(b' ') => new_line = new_line.saturating_add(1),
            _ => {}
        }
    }
    sink.progress(bytes);
    Ok(bytes)
}

/// Scans added lines straight into a collector; used by both scans.
pub(crate) struct LineSink<'a, 'r> {
    pub(crate) collector: &'a mut Collector<'r>,
    pub(crate) place: Place,
    pub(crate) current: Option<CommitRef>,
    pub(crate) commits: usize,
    pub(crate) lines: u64,
    pub(crate) new_files: Vec<(String, Option<CommitRef>)>,
    pub(crate) report: Option<ProgressReporter<'a>>,
}

pub(crate) struct ProgressReporter<'a> {
    pub(crate) progress: &'a mut dyn FnMut(&Progress),
    pub(crate) last: Instant,
    pub(crate) total: Option<usize>,
    pub(crate) base_bytes: u64,
}

impl PatchSink for LineSink<'_, '_> {
    fn commit(&mut self, c: &CommitRef) {
        self.commits += 1;
        self.current = Some(c.clone());
    }

    fn new_file(&mut self, path: &str) {
        self.new_files.push((path.to_owned(), self.current.clone()));
    }

    fn line(&mut self, path: &str, line: u32, text: &str) {
        self.lines += 1;
        for hit in scan_line(self.collector.rules, path, text) {
            let (snippet_text, start, end) = snippet(text, hit.start, hit.end);
            self.collector.add(
                hit,
                Occurrence {
                    place: self.place,
                    path: path.to_owned(),
                    line,
                    commit: self.current.as_ref().map(|c| c.id),
                    text: snippet_text,
                    start,
                    end,
                },
                self.current.as_ref(),
            );
        }
    }

    fn progress(&mut self, bytes: u64) {
        let findings = self.collector.len();
        let commits = self.commits;
        if let Some(r) = &mut self.report
            && r.last.elapsed().as_millis() > 150
        {
            r.last = Instant::now();
            (r.progress)(&Progress {
                phase: "Scanning history".into(),
                done: commits,
                total: r.total,
                bytes: r.base_bytes + bytes,
                findings,
            });
        }
    }
}

/// `@@ -a,b +c,d @@` → c.
fn hunk_new_start(raw: &[u8]) -> u32 {
    let s = String::from_utf8_lossy(raw);
    s.split('+')
        .nth(1)
        .and_then(|rest| rest.split([',', ' ']).next().and_then(|n| n.parse().ok()))
        .unwrap_or(1)
}

pub(crate) fn git_lines(
    workdir: &Path,
    args: &[&str],
    commands: &mut Vec<String>,
) -> Result<Vec<String>> {
    commands.push(format!("git {}", args.join(" ")));
    let out = cli::run(workdir, args, None)?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches(['\r', '\n']).to_owned())
        .collect())
}

pub(crate) fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|&b| b == 0)
}

/// Read and scan files on disk in parallel. Returns (bytes, lines,
/// scanned, binary, large, kind-skipped).
pub(crate) fn scan_files(
    workdir: &Path,
    paths: &[(String, Place)],
    rules: &Rules,
    collector: &mut Collector,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
    base: &Progress,
) -> (u64, u64, usize, usize, usize, usize) {
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8);
    type Found = (usize, Hit, Occurrence);
    /// (hits, bytes, lines, scanned, binary, large, kind-skipped) of one thread.
    type ThreadOut = (Vec<Found>, u64, u64, usize, usize, usize, usize);
    let results: Vec<ThreadOut> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let next = &next;
                s.spawn(move || {
                    let mut found: Vec<Found> = Vec::new();
                    let (mut bytes, mut lines, mut scanned, mut binary, mut large, mut kind) =
                        (0u64, 0u64, 0usize, 0usize, 0usize, 0usize);
                    loop {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some((path, place)) = paths.get(i) else {
                            break;
                        };
                        if skip_content(path) {
                            kind += 1;
                            continue;
                        }
                        let full = workdir.join(path);
                        let Ok(meta) = std::fs::metadata(&full) else {
                            continue;
                        };
                        if !meta.is_file() {
                            continue;
                        }
                        if meta.len() > MAX_FILE_BYTES {
                            large += 1;
                            continue;
                        }
                        let Ok(data) = std::fs::read(&full) else {
                            continue;
                        };
                        if is_binary(&data) {
                            binary += 1;
                            continue;
                        }
                        scanned += 1;
                        bytes += data.len() as u64;
                        let text = String::from_utf8_lossy(&data);
                        for (li, line) in text.split('\n').enumerate() {
                            lines += 1;
                            for hit in scan_line(rules, path, line) {
                                let (text, start, end) = snippet(line, hit.start, hit.end);
                                found.push((
                                    i,
                                    hit,
                                    Occurrence {
                                        place: *place,
                                        path: path.clone(),
                                        line: li as u32 + 1,
                                        commit: None,
                                        text,
                                        start,
                                        end,
                                    },
                                ));
                            }
                        }
                    }
                    (found, bytes, lines, scanned, binary, large, kind)
                })
            })
            .collect();
        // Report progress from this thread while the workers run.
        let total = paths.len();
        let mut last = Instant::now();
        loop {
            let done = next.load(Ordering::Relaxed).min(total);
            if last.elapsed().as_millis() > 120 {
                last = Instant::now();
                progress(&Progress {
                    phase: "Scanning files".into(),
                    done,
                    total: Some(total),
                    bytes: base.bytes,
                    findings: base.findings,
                });
            }
            if done >= total || cancel.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut all: Vec<Found> = Vec::new();
    let (mut bytes, mut lines, mut scanned, mut binary, mut large, mut kind) = (0, 0, 0, 0, 0, 0);
    for (found, b, l, s, bi, la, k) in results {
        all.extend(found);
        bytes += b;
        lines += l;
        scanned += s;
        binary += bi;
        large += la;
        kind += k;
    }
    // Deterministic order: by file, then line.
    all.sort_by(|a, b| a.0.cmp(&b.0).then(a.2.line.cmp(&b.2.line)));
    for (_, hit, occ) in all {
        collector.add(hit, occ, None);
    }
    (bytes, lines, scanned, binary, large, kind)
}

/// Tracked paths whose index or working-tree content differs from HEAD
/// (modified, staged, deleted; renames as delete + add). `None` without a
/// HEAD commit.
pub(crate) fn changed_vs_head(
    workdir: &Path,
    commands: &mut Vec<String>,
) -> Option<HashSet<String>> {
    head_oid(workdir)?;
    let args = ["diff", "--name-only", "-z", "--no-renames", "HEAD"];
    commands.push(format!("git {}", args.join(" ")));
    let out = cli::run(workdir, &args, None).ok()?;
    Some(
        String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_owned())
            .collect(),
    )
}

/// HEAD's version of each path, through one `git cat-file --batch`.
/// Missing paths (added since HEAD) are skipped.
fn head_contents(
    workdir: &Path,
    paths: &[String],
    commands: &mut Vec<String>,
) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    if paths.is_empty() {
        return out;
    }
    let mut stdin = String::new();
    for p in paths {
        stdin.push_str("HEAD:");
        stdin.push_str(p);
        stdin.push('\n');
    }
    commands.push(format!(
        "git cat-file --batch   # {} path(s) on stdin",
        paths.len()
    ));
    let Ok(res) = cli::run(workdir, &["cat-file", "--batch"], Some(stdin.as_bytes())) else {
        return out;
    };
    let data = &res.stdout[..];
    let mut pos = 0usize;
    let mut i = 0usize;
    while pos < data.len() && i < paths.len() {
        let Some(nl) = data[pos..].iter().position(|&b| b == b'\n') else {
            break;
        };
        let header = String::from_utf8_lossy(&data[pos..pos + nl]).into_owned();
        pos += nl + 1;
        let fields: Vec<&str> = header.split(' ').collect();
        if fields.len() >= 3 {
            let size: usize = fields[2].parse().unwrap_or(0);
            let end = (pos + size).min(data.len());
            if fields[1] == "blob" {
                out.push((paths[i].clone(), data[pos..end].to_vec()));
            }
            pos = end + 1;
        }
        i += 1;
    }
    out
}

pub(crate) fn remote_commits(workdir: &Path, commands: &mut Vec<String>) -> HashSet<Oid> {
    let mut set = HashSet::new();
    commands.push("git rev-list --remotes".into());
    if let Ok(out) = cli::run(workdir, &["rev-list", "--remotes"], None) {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Ok(id) = Oid::from_str(line.trim()) {
                set.insert(id);
            }
        }
    }
    set
}

pub(crate) fn head_oid(workdir: &Path) -> Option<Oid> {
    let out = cli::run(workdir, &["rev-parse", "--verify", "-q", "HEAD"], None).ok()?;
    Oid::from_str(String::from_utf8_lossy(&out.stdout).trim()).ok()
}

/// Credentials in remote URLs, plaintext credential helpers, disabled TLS.
fn config_findings(workdir: &Path, commands: &mut Vec<String>) -> (Vec<ConfigFinding>, usize) {
    let mut out = Vec::new();
    let mut remotes = 0;
    commands.push("git remote -v".into());
    if let Ok(res) = cli::run(workdir, &["remote", "-v"], None) {
        let cred =
            Regex::new(r"^([a-zA-Z][a-zA-Z0-9+.\-]*://)([^/@\s]+):([^/@\s]+)@(.*)$").unwrap();
        let mut seen = HashSet::new();
        for line in String::from_utf8_lossy(&res.stdout).lines() {
            let mut parts = line.split_whitespace();
            let (Some(name), Some(url)) = (parts.next(), parts.next()) else {
                continue;
            };
            if seen.insert(name.to_owned()) {
                remotes += 1;
            }
            if let Some(c) = cred.captures(url)
                && seen.insert(format!("cred:{name}"))
            {
                let clean = format!("{}{}@{}", &c[1], &c[2], &c[4]);
                let shown = format!("{}{}:{}@{}", &c[1], &c[2], redact(&c[3]), &c[4]);
                out.push(ConfigFinding {
                    id: "remote-url-credentials",
                    severity: Severity::High,
                    title: format!("Password embedded in the URL of remote {name}"),
                    detail: format!("{name} → {shown}\nThe password is stored in plain text in .git/config and shows up in every `git remote -v`, log and error message."),
                    why: "Remote URLs are copied into scripts, CI configuration and bug reports; a password inside one leaks easily. Use a credential helper or an SSH key instead.",
                    fix: Some(ConfigFix::SetRemoteUrl {
                        name: name.to_owned(),
                        url: clean,
                    }),
                    fingerprint: fingerprint("remote-url-credentials", &format!("{name}:{}", &c[3])),
                });
            }
        }
    }
    commands.push("git config --get credential.helper".into());
    if let Ok(res) = cli::run(workdir, &["config", "--get", "credential.helper"], None) {
        let v = String::from_utf8_lossy(&res.stdout).trim().to_owned();
        if v == "store" || v.starts_with("store ") {
            out.push(ConfigFinding {
                id: "credential-store",
                severity: Severity::Low,
                title: "Credentials are stored in plain text (credential.helper = store)".into(),
                detail: format!("credential.helper = {v}\nEvery password and token git has used is written unencrypted to ~/.git-credentials. This is a local risk, not something in the repository."),
                why: "A stolen laptop or a malicious program gets every git credential at once. Prefer the OS keychain (`manager` on Windows, `osxkeychain` on macOS, `libsecret` on Linux) or SSH keys.",
                fix: None,
                fingerprint: fingerprint("credential-store", &v),
            });
        }
    }
    commands.push("git config --get http.sslVerify".into());
    if let Ok(res) = cli::run(workdir, &["config", "--get", "http.sslVerify"], None) {
        let v = String::from_utf8_lossy(&res.stdout).trim().to_lowercase();
        if v == "false" {
            out.push(ConfigFinding {
                id: "ssl-verify-off",
                severity: Severity::Medium,
                title: "TLS certificate verification is disabled (http.sslVerify = false)".into(),
                detail: "Pushes and fetches over HTTPS accept any certificate, so credentials can be intercepted on the way to the server.".into(),
                why: "Without certificate checks, anyone on the network path can impersonate the remote and capture the token or password used to authenticate.",
                fix: None,
                fingerprint: fingerprint("ssl-verify-off", "false"),
            });
        }
    }
    (out, remotes)
}

/// Run the scan. `progress` is called from the scanning thread; `cancel`
/// stops the scan early with a partial report (`stats.cancelled`).
pub fn scan(
    workdir: &Path,
    opts: &ScanOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<Report> {
    let started = Instant::now();
    let rules = rules();
    let mut commands = Vec::new();
    let mut stats = Stats {
        history: opts.history.is_some(),
        all_refs: opts.history.as_ref().is_some_and(|h| h.all_refs),
        ..Default::default()
    };
    let mut collector = Collector::new(rules);
    let mut files: HashMap<String, FileFinding> = HashMap::new();
    let mut prog = Progress {
        phase: "Listing files".into(),
        ..Default::default()
    };
    progress(&prog);

    // --- Working tree -----------------------------------------------------
    if opts.worktree {
        let tracked = git_lines(workdir, &["ls-files", "-z"], &mut commands)?;
        let untracked = git_lines(
            workdir,
            &["ls-files", "-z", "--others", "--exclude-standard"],
            &mut commands,
        )?;
        stats.files_listed = tracked.len() + untracked.len();
        let mut paths: Vec<(String, Place)> = Vec::with_capacity(stats.files_listed);
        for p in &tracked {
            if let Some(rule) = check_path(p) {
                files.insert(
                    p.clone(),
                    FileFinding {
                        rule,
                        severity: rule.severity,
                        path: p.clone(),
                        fingerprint: fingerprint(rule.id, p),
                        tracked: true,
                        staged: false,
                        untracked: false,
                        size: std::fs::metadata(workdir.join(p)).ok().map(|m| m.len()),
                        added_in: None,
                        pushed: false,
                        secrets: 0,
                    },
                );
            }
            paths.push((p.clone(), Place::Worktree));
        }
        for p in &untracked {
            if let Some(rule) = check_path(p) {
                files.insert(
                    p.clone(),
                    FileFinding {
                        rule,
                        // Not committed yet: one step less urgent.
                        severity: rule.severity.lower(),
                        path: p.clone(),
                        fingerprint: fingerprint(rule.id, p),
                        tracked: false,
                        staged: false,
                        untracked: true,
                        size: std::fs::metadata(workdir.join(p)).ok().map(|m| m.len()),
                        added_in: None,
                        pushed: false,
                        secrets: 0,
                    },
                );
            }
            paths.push((p.clone(), Place::Untracked));
        }
        let (bytes, lines, scanned, binary, large, kind) = scan_files(
            workdir,
            &paths,
            rules,
            &mut collector,
            cancel,
            progress,
            &prog,
        );
        stats.bytes += bytes;
        stats.lines += lines;
        stats.files_scanned = scanned;
        stats.skipped_binary = binary;
        stats.skipped_large = large;
        stats.skipped_kind = kind;
        prog.bytes = stats.bytes;
        prog.findings = collector.len();
    }

    // --- History ------------------------------------------------------------
    if let Some(scope) = &opts.history
        && !cancel.load(Ordering::Relaxed)
    {
        let range: Vec<&str> = if scope.all_refs {
            vec!["--branches", "--remotes", "--tags"]
        } else {
            vec!["HEAD"]
        };
        let limit = scope.max_commits.to_string();
        let mut count_args = vec!["rev-list", "--count"];
        if scope.max_commits > 0 {
            count_args.extend(["-n", limit.as_str()]);
        }
        count_args.extend(range.iter());
        commands.push(format!("git {}", count_args.join(" ")));
        let total = cli::run(workdir, &count_args, None).ok().and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<usize>()
                .ok()
        });
        stats.commits_total = total;
        prog.phase = "Scanning history".into();
        prog.done = 0;
        prog.total = total;
        progress(&prog);

        let mut args = vec![
            "log",
            "-p",
            "-U0",
            "--no-color",
            "--no-renames",
            "--no-ext-diff",
            "--diff-filter=AM",
            "--format=%x01%H%x00%ct%x00%an%x00%s",
        ];
        if scope.max_commits > 0 {
            args.extend(["-n", limit.as_str()]);
        }
        args.extend(range.iter());
        commands.push(format!("git {}", args.join(" ")));
        let mut child = cli::spawn(workdir, &args)?;
        let stdout = child.stdout.take().ok_or_else(|| Error::new("no stdout"))?;
        let reader = BufReader::with_capacity(1 << 20, stdout);
        let (commits, new_files) = {
            let mut sink = LineSink {
                collector: &mut collector,
                place: Place::History,
                current: None,
                commits: 0,
                lines: 0,
                new_files: Vec::new(),
                report: Some(ProgressReporter {
                    progress,
                    last: Instant::now(),
                    total,
                    base_bytes: stats.bytes,
                }),
            };
            let res = walk_patch(reader, cancel, &mut sink);
            stats.lines += sink.lines;
            match res {
                Ok(bytes) => stats.bytes += bytes,
                Err(e) => {
                    let _ = child.kill();
                    return Err(Error::new(format!("reading git log: {e}")));
                }
            }
            (sink.commits, sink.new_files)
        };
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            stats.cancelled = true;
        }
        let status = child.wait().map_err(|e| Error::new(e.to_string()))?;
        if !status.success() && !stats.cancelled {
            return Err(Error::new(format!("git log exited with {status}")));
        }
        stats.commits = commits;
        for (path, c) in new_files {
            let Some(c) = c else { continue };
            if let Some(rule) = check_path(&path) {
                let entry = files.entry(path.clone()).or_insert_with(|| FileFinding {
                    rule,
                    severity: rule.severity,
                    path: path.clone(),
                    fingerprint: fingerprint(rule.id, &path),
                    tracked: false,
                    staged: false,
                    untracked: false,
                    size: None,
                    added_in: None,
                    pushed: false,
                    secrets: 0,
                });
                // Oldest commit wins (history streams newest first).
                if entry.added_in.as_ref().is_none_or(|a| c.time < a.time) {
                    entry.added_in = Some(c);
                }
            }
        }
    }

    // --- Cross-checks -------------------------------------------------------
    prog.phase = "Checking HEAD".into();
    prog.done = 0;
    prog.total = None;
    prog.findings = collector.len();
    progress(&prog);
    // Unmodified tracked files equal HEAD; only files that differ need their
    // HEAD version read (one `cat-file --batch`), so nothing is grepped twice.
    let changed = if cancel.load(Ordering::Relaxed) || !opts.worktree {
        None
    } else {
        changed_vs_head(workdir, &mut commands)
    };
    if let Some(changed) = &changed {
        let mut wanted: Vec<String> = changed
            .iter()
            .filter(|p| !skip_content(p))
            .cloned()
            .collect();
        wanted.sort();
        for (path, data) in head_contents(workdir, &wanted, &mut commands) {
            if data.len() as u64 > MAX_FILE_BYTES || is_binary(&data) {
                continue;
            }
            let text = String::from_utf8_lossy(&data);
            stats.bytes += data.len() as u64;
            stats.lines += collector.scan_text(&path, &text, Place::Head, None);
        }
    }

    prog.phase = "Checking remotes".into();
    progress(&prog);
    let pushed = remote_commits(workdir, &mut commands);
    stats.pushed_commits = pushed.len();
    let head_id = head_oid(workdir);
    stats.head_pushed = head_id.is_some_and(|h| pushed.contains(&h));

    prog.phase = "Checking configuration".into();
    progress(&prog);
    let (config, remotes) = config_findings(workdir, &mut commands);
    stats.remotes = remotes;

    let mut findings = collector.finish(changed.as_ref(), &pushed, stats.head_pushed);
    // Sensitive files that also contain secrets: count them; pushed flag.
    let mut files: Vec<FileFinding> = files.into_values().collect();
    for f in &mut files {
        f.secrets = findings
            .iter()
            .filter(|s| s.occurrences.iter().any(|o| o.path == f.path))
            .count();
        f.pushed = f.added_in.as_ref().is_some_and(|c| pushed.contains(&c.id))
            || (f.tracked && stats.head_pushed);
        if f.rule.id == "pem-file" && f.secrets > 0 {
            f.severity = Severity::Critical;
        }
    }
    settle_files(&mut files);
    let mut config = config;
    if opts.worktree && files.iter().any(|f| f.untracked) && !workdir.join(".gitignore").exists() {
        config.push(ConfigFinding {
            id: "no-gitignore",
            severity: Severity::Medium,
            title: "No .gitignore, and sensitive files are lying around untracked".into(),
            detail: format!(
                "{} untracked sensitive file(s) would be added by `git add .` or `git add -A`.",
                files.iter().filter(|f| f.untracked).count()
            ),
            why: "Without a .gitignore nothing stops the next broad `git add` from committing keys and .env files.",
            fix: None,
            fingerprint: fingerprint("no-gitignore", ""),
        });
    }
    findings.retain(|f| !f.occurrences.is_empty());
    stats.elapsed_ms = started.elapsed().as_millis() as u64;
    let mut report = Report {
        findings,
        files,
        config,
        stats,
        commands,
    };
    sort_report(&mut report);
    Ok(report)
}

/// Quick scan of what is about to be committed: added lines of the staged
/// and unstaged diffs, untracked files, and sensitive names among them.
pub fn scan_changes(workdir: &Path) -> Result<Report> {
    let started = Instant::now();
    let rules = rules();
    let cancel = AtomicBool::new(false);
    let mut commands = Vec::new();
    let mut collector = Collector::new(rules);
    let mut files: HashMap<String, FileFinding> = HashMap::new();
    let mut stats = Stats::default();

    for (place, extra) in [(Place::Staged, Some("--cached")), (Place::Unstaged, None)] {
        let mut args = vec!["diff", "-U0", "--no-color", "--no-renames", "--no-ext-diff"];
        if let Some(e) = extra {
            args.push(e);
        }
        commands.push(format!("git {}", args.join(" ")));
        let out = cli::run(workdir, &args, None)?;
        let (bytes, lines, new_files) = {
            let mut sink = LineSink {
                collector: &mut collector,
                place,
                current: None,
                commits: 0,
                lines: 0,
                new_files: Vec::new(),
                report: None,
            };
            let bytes = walk_patch(BufReader::new(&out.stdout[..]), &cancel, &mut sink)
                .map_err(|e| Error::new(e.to_string()))?;
            (bytes, sink.lines, sink.new_files)
        };
        stats.bytes += bytes;
        stats.lines += lines;
        if place == Place::Staged {
            for (p, _) in new_files {
                if let Some(rule) = check_path(&p) {
                    files.insert(
                        p.clone(),
                        FileFinding {
                            rule,
                            severity: rule.severity,
                            path: p.clone(),
                            fingerprint: fingerprint(rule.id, &p),
                            tracked: true,
                            staged: true,
                            untracked: false,
                            size: std::fs::metadata(workdir.join(&p)).ok().map(|m| m.len()),
                            added_in: None,
                            pushed: false,
                            secrets: 0,
                        },
                    );
                }
            }
        }
    }

    let untracked = git_lines(
        workdir,
        &["ls-files", "-z", "--others", "--exclude-standard"],
        &mut commands,
    )?;
    stats.files_listed = untracked.len();
    let paths: Vec<(String, Place)> = untracked
        .iter()
        .map(|p| (p.clone(), Place::Untracked))
        .collect();
    for p in &untracked {
        if let Some(rule) = check_path(p) {
            files.entry(p.clone()).or_insert_with(|| FileFinding {
                rule,
                severity: rule.severity.lower(),
                path: p.clone(),
                fingerprint: fingerprint(rule.id, p),
                tracked: false,
                staged: false,
                untracked: true,
                size: std::fs::metadata(workdir.join(p)).ok().map(|m| m.len()),
                added_in: None,
                pushed: false,
                secrets: 0,
            });
        }
    }
    let mut noop = |_: &Progress| {};
    let (bytes, lines, scanned, binary, large, kind) = scan_files(
        workdir,
        &paths,
        rules,
        &mut collector,
        &cancel,
        &mut noop,
        &Progress::default(),
    );
    stats.bytes += bytes;
    stats.lines += lines;
    stats.files_scanned = scanned;
    stats.skipped_binary = binary;
    stats.skipped_large = large;
    stats.skipped_kind = kind;

    let findings = collector.finish(None, &HashSet::new(), false);
    let mut files: Vec<FileFinding> = files.into_values().collect();
    settle_files(&mut files);
    for f in &mut files {
        f.secrets = findings
            .iter()
            .filter(|s| s.occurrences.iter().any(|o| o.path == f.path))
            .count();
    }
    stats.elapsed_ms = started.elapsed().as_millis() as u64;
    let mut report = Report {
        findings,
        files,
        config: Vec::new(),
        stats,
        commands,
    };
    sort_report(&mut report);
    Ok(report)
}

/// `git filter-repo` invocation that scrubs a secret from every commit.
/// The expressions file keeps the secret off the command line.
pub fn purge_instructions(secret: &str) -> String {
    format!(
        "# 1. Install git-filter-repo (pip install git-filter-repo), then in a fresh clone:\n\
         printf '%s' '{secret}==>***REMOVED***' > expressions.txt\n\
         git filter-repo --replace-text expressions.txt\n\
         # 2. Force-push every branch and tag, ask collaborators to re-clone,\n\
         #    and contact GitHub support to purge cached views of the old commits.\n\
         git push --force --all && git push --force --tags"
    )
}

/// `git filter-repo` invocation that removes a file from every commit.
pub fn purge_file_instructions(path: &str) -> String {
    format!(
        "# In a fresh clone, with git-filter-repo installed:\n\
         git filter-repo --invert-paths --path '{path}'\n\
         git push --force --all && git push --force --tags"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits(path: &str, line: &str) -> Vec<(&'static str, String)> {
        let r = rules();
        scan_line(r, path, line)
            .into_iter()
            .map(|h| (r.rules[h.rule].id, h.secret))
            .collect()
    }

    fn ids(path: &str, line: &str) -> Vec<&'static str> {
        hits(path, line).into_iter().map(|(id, _)| id).collect()
    }

    #[test]
    fn rules_compile_and_have_unique_ids() {
        let r = rules();
        let mut seen = HashSet::new();
        for rule in &r.rules {
            assert!(seen.insert(rule.id), "duplicate rule id {}", rule.id);
        }
        assert!(r.rules.len() > 40);
    }

    #[test]
    fn well_known_formats_are_found() {
        assert_eq!(
            hits("a.rs", "let k = \"AKIAJ6ZQMPTX3V7UBNRQ\";"),
            vec![("aws-access-key", "AKIAJ6ZQMPTX3V7UBNRQ".to_owned())]
        );
        assert_eq!(
            ids("x", "token: ghp_z1Yx2Wv3Ut4Sr5Qp6On7Ml8Kj9Ih0Hg1Fe2Dc3Ba4"),
            ["github-token"]
        );
        assert_eq!(
            ids(
                "x",
                "export SLACK=xoxb-917203645081-2938475610293-Qr7TmZp2Lk9WvXc4Yb8Nd3Hs"
            ),
            ["slack-token"]
        );
        assert_eq!(
            ids("x", "STRIPE_KEY = 'sk_live_51H8sJ2KzQyXbW3n9LmP7Rt0V'"),
            ["stripe-live"]
        );
        assert_eq!(
            ids("x", "key = 'sk_test_51H8sJ2KzQyXbW3n9LmP7Rt0V'"),
            ["stripe-test"]
        );
        assert_eq!(
            ids("k.pem", "-----BEGIN RSA PRIVATE KEY-----"),
            ["private-key"]
        );
        assert_eq!(
            ids("k.pem", "-----BEGIN ENCRYPTED PRIVATE KEY-----"),
            ["encrypted-private-key"]
        );
        assert_eq!(
            hits(
                "db.yml",
                "url: postgres://app:S3cr3tPassw0rd!@db.internal:5432/app"
            ),
            vec![("url-credentials", "S3cr3tPassw0rd!".to_owned())]
        );
        assert_eq!(
            ids(
                "x",
                "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.dGhpcyBpcyBhIHNpZ25hdHVyZQ"
            ),
            ["jwt"],
            "the JWT rule is more specific than the bearer rule for the same span"
        );
        assert_eq!(
            ids(
                "x",
                "const key = 'AIzaSyD-9tSrke72PouQMnMX-a7eZSW0jkFMBWY';"
            ),
            ["google-api-key"]
        );
        assert_eq!(
            ids(
                "x",
                "openai = \"sk-proj-abc123T3BlbkFJdef456ghi789jkl012mno345\""
            ),
            ["openai-key"]
        );
        assert_eq!(
            ids(
                "x",
                "https://hooks.slack.com/services/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"
            ),
            ["slack-webhook"]
        );
    }

    #[test]
    fn generic_rules_need_quotes_and_entropy() {
        assert_eq!(
            hits("settings.py", "PASSWORD = \"tr0ub4dor&3xyz\""),
            vec![("generic-credential", "tr0ub4dor&3xyz".to_owned())]
        );
        // Variable references, templates and placeholders are not secrets.
        assert!(ids("x", "password = \"${DB_PASSWORD}\"").is_empty());
        assert!(ids("x", "password = \"{{ vault_pw }}\"").is_empty());
        assert!(ids("x", "password: \"<your-password-here>\"").is_empty());
        assert!(ids("x", "api_key = \"changeme123\"").is_empty());
        assert!(ids("x", "api_key = \"xxxxxxxxxxxxxxxx\"").is_empty());
        assert!(ids("x", "password = os.environ[\"PW\"]").is_empty());
        assert!(ids("x", "secret = \"aaaaaaaaaaaa\"").is_empty());
        assert!(
            ids("x", "token = \"AKIAIOSFODNN7EXAMPLE\"").is_empty(),
            "AWS docs example"
        );
        // Config files: unquoted values count.
        assert_eq!(
            hits(".env", "DATABASE_PASSWORD=q8Zr!vP2m#Lw9xT"),
            vec![("config-credential", "q8Zr!vP2m#Lw9xT".to_owned())]
        );
        assert!(
            ids("main.rs", "DATABASE_PASSWORD=q8Zr!vP2m#Lw9xT").is_empty(),
            "not a config file"
        );
        assert!(ids(".env", "DATABASE_PASSWORD=").is_empty());
        assert_eq!(
            hits(".env", "API_TOKEN = 'q8Zr!vP2mLw9xT'  # prod"),
            vec![("config-credential", "q8Zr!vP2mLw9xT".to_owned())]
        );
        // Values that are setting names, dummy hosts, and giant lines.
        assert!(
            ids(
                "a.java",
                "String PREF_AUTH_TOKEN = \"brave_vpn_api_auth_token\";"
            )
            .is_empty()
        );
        assert!(ids("t.cc", "\"https://user:pass@example.com/path\"").is_empty());
        assert!(ids("t.cc", "{\"socks5://foo:bar@foopy\"}").is_empty());
        assert!(ids("t.cc", "\"ftp://rms:secret123@example.com\"").is_empty());
        assert!(
            !ids(
                "db.yml",
                "url: postgres://app:S3cr3tPassw0rd!@db.internal:5432/app"
            )
            .is_empty()
        );
        let long = format!(
            "x = 1; {} password = \"tr0ub4dor&3xyz\";",
            "a".repeat(2_500)
        );
        assert!(
            ids("bundle.js", &long).is_empty(),
            "generic rules skip huge lines"
        );
        let long_key = format!("{} k = \"AKIAJ6ZQMPTX3V7UBNRQ\";", "a".repeat(2_500));
        assert_eq!(ids("bundle.js", &long_key), ["aws-access-key"]);
        // Registry credentials only count in registry config files.
        let npmrc = "//registry.npmjs.org/:_authToken=Qm7Lk9WvXc4Yb8Nd3HsZp2Tr";
        assert_eq!(ids(".npmrc", npmrc), ["npmrc-auth"]);
        assert!(
            ids(
                "config.js",
                "env.RBE_service_no_auth = env.RBE_service_no_auth || true"
            )
            .is_empty()
        );
        assert!(
            ids(
                "mod.rs",
                "pub const XHR_AUTH_ALL: XHR_AUTH = XHR_AUTH(0i32);"
            )
            .is_empty()
        );
        // Allow markers.
        assert!(ids("x", "k = \"AKIAJ6ZQMPTX3V7UBNRQ\" # hadion:allow").is_empty());
        assert!(ids("x", "k = \"AKIAJ6ZQMPTX3V7UBNRQ\" // gitleaks:allow").is_empty());
    }

    #[test]
    fn entropy_and_redaction() {
        assert!(entropy("aaaa") < 0.01);
        assert!(entropy("abcd") > 1.99 && entropy("abcd") < 2.01);
        assert!(entropy("Kj8#mN2$pQ9!xR4&") > 3.5);
        assert_eq!(redact("short"), "•••••");
        assert_eq!(
            redact("AKIAJ6ZQMPTX3V7UBNRQ"),
            "AKIA••••••••••••UBNRQ".replace("UBNRQ", "BNRQ")
        );
        assert_eq!(redact("1234567890"), "12••••••90");
        assert_eq!(fingerprint("a", "b").len(), 16);
        assert_eq!(fingerprint("a", "b"), fingerprint("a", "b"));
        assert_ne!(fingerprint("a", "b"), fingerprint("a", "c"));
    }

    #[test]
    fn sensitive_file_names() {
        let id = |p: &str| check_path(p).map(|r| r.id);
        assert_eq!(id(".env"), Some("env-file"));
        assert_eq!(id("config/.env.production"), Some("env-file"));
        assert_eq!(id(".env.example"), None);
        assert_eq!(id(".env.sample"), None);
        assert_eq!(id("id_rsa"), Some("ssh-private-key"));
        assert_eq!(id("keys/id_rsa.pub"), None);
        assert_eq!(id(".ssh/config"), Some("ssh-dir"));
        assert_eq!(id("certs/server.key"), Some("key-file"));
        assert_eq!(id("certs/chain.pem"), Some("pem-file"));
        assert_eq!(id("terraform.tfstate"), Some("terraform-state"));
        assert_eq!(id("prod.tfvars"), Some("terraform-vars"));
        assert_eq!(id(".git-credentials"), Some("git-credentials"));
        assert_eq!(id("home/.aws/credentials"), Some("cloud-credentials"));
        assert_eq!(
            id("firebase-adminsdk-x1y2z.json"),
            Some("cloud-credentials")
        );
        assert_eq!(id("secrets.yml"), Some("secrets-file"));
        assert_eq!(id("backup.sql"), Some("database-dump"));
        assert_eq!(id("src/main.rs"), None);
        assert_eq!(id("README.md"), None);
        assert_eq!(id(".env.bak"), Some("env-backup"));
        assert!(skip_content("package-lock.json"));
        assert!(skip_content("dist/app.min.js"));
        assert!(skip_content("logo.png"));
        assert!(!skip_content("src/config.ts"));
    }

    #[test]
    fn patch_stream_yields_added_lines_with_numbers() {
        let patch = "\u{1}0123456789abcdef0123456789abcdef01234567\x001700000000\0Ann\0first\n\
\n\
diff --git a/.env b/.env\n\
new file mode 100644\n\
index 0000000..e69de29\n\
--- /dev/null\n\
+++ b/.env\n\
@@ -0,0 +1,2 @@\n\
+A=1\n\
+TOKEN=ghp_z1Yx2Wv3Ut4Sr5Qp6On7Ml8Kj9Ih0Hg1Fe2Dc3Ba4\n\
diff --git a/img.png b/img.png\n\
Binary files /dev/null and b/img.png differ\n\
\u{1}fedcba9876543210fedcba9876543210fedcba98\x001700000100\0Bob\0second\n\
\n\
diff --git a/src/a.rs b/src/a.rs\n\
index 1..2 100644\n\
--- a/src/a.rs\n\
+++ b/src/a.rs\n\
@@ -10,0 +11,2 @@ fn x() {\n\
+    let a = 1;\n\
+    let b = 2;\n\
@@ -40 +42 @@ fn y() {\n\
-    old\n\
+    new\n";
        #[derive(Default)]
        struct Rec {
            commits: Vec<(String, i64, String, String)>,
            new_files: Vec<String>,
            lines: Vec<(String, u32, String)>,
        }
        impl PatchSink for Rec {
            fn commit(&mut self, c: &CommitRef) {
                self.commits.push((
                    c.id.to_string(),
                    c.time,
                    c.author.clone(),
                    c.summary.clone(),
                ));
            }
            fn new_file(&mut self, p: &str) {
                self.new_files.push(p.to_owned());
            }
            fn line(&mut self, p: &str, n: u32, t: &str) {
                self.lines.push((p.to_owned(), n, t.to_owned()));
            }
        }
        let cancel = AtomicBool::new(false);
        let mut rec = Rec::default();
        let bytes = walk_patch(BufReader::new(patch.as_bytes()), &cancel, &mut rec).unwrap();
        assert_eq!(bytes as usize, patch.len());
        assert_eq!(rec.commits.len(), 2);
        assert_eq!(rec.commits[0].1, 1700000000);
        assert_eq!(rec.commits[1].2, "Bob");
        assert_eq!(rec.new_files, [".env"]);
        assert_eq!(
            rec.lines,
            vec![
                (".env".to_owned(), 1, "A=1".to_owned()),
                (
                    ".env".to_owned(),
                    2,
                    "TOKEN=ghp_z1Yx2Wv3Ut4Sr5Qp6On7Ml8Kj9Ih0Hg1Fe2Dc3Ba4".to_owned()
                ),
                ("src/a.rs".to_owned(), 11, "    let a = 1;".to_owned()),
                ("src/a.rs".to_owned(), 12, "    let b = 2;".to_owned()),
                ("src/a.rs".to_owned(), 42, "    new".to_owned()),
            ]
        );
    }

    #[test]
    fn snippets_keep_the_match_in_range() {
        let long = format!("{}SECRETVALUE{}", "a".repeat(500), "b".repeat(500));
        let (s, start, end) = snippet(&long, 500, 511);
        assert_eq!(&s[start..end], "SECRETVALUE");
        assert!(s.starts_with('…') && s.ends_with('…'));
        assert!(s.len() < 400);
        let (s, start, end) = snippet("k=v\r\n", 2, 3);
        assert_eq!((&s[start..end], s.as_str()), ("v", "k=v"));
    }

    #[test]
    fn collector_merges_and_counts() {
        let r = rules();
        let mut c = Collector::new(r);
        let commit = CommitRef {
            id: Oid::from_str("0123456789abcdef0123456789abcdef01234567").unwrap(),
            time: 5,
            author: "A".into(),
            summary: "s".into(),
        };
        let text = "k = 'ghp_z1Yx2Wv3Ut4Sr5Qp6On7Ml8Kj9Ih0Hg1Fe2Dc3Ba4'";
        c.scan_text("a.txt", text, Place::History, Some(&commit));
        c.scan_text("b.txt", text, Place::History, Some(&commit));
        c.scan_text("tests/c.txt", text, Place::Worktree, None);
        assert_eq!(c.len(), 1);
        // tests/c.txt is unmodified versus HEAD, so its line is in HEAD too.
        let findings = c.finish(Some(&HashSet::new()), &HashSet::from([commit.id]), false);
        let f = &findings[0];
        assert_eq!(f.rule, "github-token");
        assert_eq!(f.occurrence_count, 3);
        assert_eq!(f.commit_count, 1);
        assert!(f.at_head && f.in_worktree && f.pushed && !f.staged);
        assert!(f.occurrences.iter().any(|o| o.place == Place::Head));
        assert!(!f.test_only, "a.txt is not a test path");
        assert_eq!(f.severity, Severity::Critical);
        assert_eq!(f.paths(), ["a.txt", "b.txt", "tests/c.txt"]);
    }

    #[test]
    fn report_merge_adds_staged_knowledge() {
        let r = rules();
        let mut c = Collector::new(r);
        let text = "k = 'ghp_z1Yx2Wv3Ut4Sr5Qp6On7Ml8Kj9Ih0Hg1Fe2Dc3Ba4'";
        c.scan_text("a.txt", text, Place::Worktree, None);
        let mut full = Report {
            findings: c.finish(None, &HashSet::new(), false),
            ..Default::default()
        };
        let mut q = Collector::new(r);
        q.scan_text("a.txt", text, Place::Staged, None);
        let quick = Report {
            findings: q.finish(None, &HashSet::new(), false),
            ..Default::default()
        };
        full.merge(&quick);
        assert_eq!(full.findings.len(), 1);
        assert!(full.findings[0].staged);
        assert_eq!(full.findings[0].occurrences.len(), 2);
        assert_eq!(full.count(Severity::Critical), 1);
    }
}
