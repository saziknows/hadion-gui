//! Security and administration on GitHub: acting on Dependabot and
//! code-scanning alerts (dismiss with a reason, reopen), SBOM (local
//! CycloneDX / SPDX from the lockfiles, GitHub's dependency-graph SBOM),
//! container images (Dockerfile lint, `trivy` / `grype` / `docker scout`
//! scans), branch protection (read, edit, remove; rulesets read-only),
//! CODEOWNERS (parse, validate, match a path, GitHub's error list),
//! environments (create, edit reviewers / wait timer / branch policy,
//! delete), Actions secrets (names only; set through `gh secret set` with
//! the value on standard input; delete) and variables (create, edit,
//! delete), at repository and environment level.
//!
//! Every write is a pure builder returning a [`Request`] (or, for secrets,
//! the `gh` arguments plus the standard input that carries the value), so
//! the tests check method, endpoint and body without touching GitHub.
//!
//! - [`alerts`]: Dependabot and code-scanning alerts, fix guidance.
//! - [`sbom`]: lockfile packages, purls, CycloneDX 1.5 and SPDX 2.3,
//!   GitHub's SBOM and the comparison.
//! - [`docker`]: Dockerfile discovery, parser, lint, compose images, image
//!   scanners and their JSON / SARIF summaries.
//! - [`protection`]: branch protection form and PUT body, rulesets.
//! - [`codeowners`]: parser, gitignore-style matching, validation, coverage.
//! - [`envs`]: deployment environments and branch policies.
//! - [`config`]: Actions secrets and variables.

pub mod alerts;
pub mod codeowners;
pub mod config;
pub mod docker;
pub mod envs;
pub mod protection;
pub mod sbom;

use serde::Deserialize;

use crate::Result;
use crate::devops::gh::Gh;
use crate::github::ApiCall;

pub use crate::devops::gh::{Method, Request};

/// What the token may do in the repository (`GET repos/{o}/{r}`), so a
/// refusal can say which permission is missing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Access {
    pub full_name: String,
    pub default_branch: String,
    /// `public`, `private` or `internal`.
    pub visibility: String,
    /// `User` or `Organization`.
    pub owner_type: String,
    pub owner: String,
    pub archived: bool,
    pub admin: bool,
    pub maintain: bool,
    pub push: bool,
    pub triage: bool,
    pub pull: bool,
    /// `security_and_analysis` statuses (only visible to administrators):
    /// `(feature, enabled|disabled)`.
    pub security: Vec<(String, String)>,
    pub html_url: String,
}

impl Access {
    /// The highest role: `admin`, `maintain`, `write`, `triage`, `read`.
    pub fn role(&self) -> &'static str {
        if self.admin {
            "admin"
        } else if self.maintain {
            "maintain"
        } else if self.push {
            "write"
        } else if self.triage {
            "triage"
        } else if self.pull {
            "read"
        } else {
            "none"
        }
    }

    pub fn is_org(&self) -> bool {
        self.owner_type.eq_ignore_ascii_case("organization")
    }

    pub fn is_public(&self) -> bool {
        self.visibility == "public"
    }

    /// Status of one `security_and_analysis` feature, when visible.
    pub fn security_status(&self, feature: &str) -> Option<&str> {
        self.security
            .iter()
            .find(|(f, _)| f == feature)
            .map(|(_, s)| s.as_str())
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawPermissions {
    admin: bool,
    maintain: bool,
    push: bool,
    triage: bool,
    pull: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawOwner {
    login: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawRepo {
    full_name: String,
    default_branch: String,
    visibility: String,
    private: bool,
    archived: bool,
    html_url: String,
    owner: Option<RawOwner>,
    permissions: Option<RawPermissions>,
    security_and_analysis: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Decode `GET repos/{o}/{r}`.
pub fn parse_access(body: &[u8]) -> Result<Access> {
    let raw: RawRepo = crate::devops::gh::decode(body, "repository")?;
    let perms = raw.permissions.unwrap_or_default();
    let owner = raw.owner.unwrap_or_default();
    let mut security: Vec<(String, String)> = raw
        .security_and_analysis
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| {
            v.get("status")
                .and_then(|s| s.as_str())
                .map(|s| (k, s.to_owned()))
        })
        .collect();
    security.sort();
    Ok(Access {
        full_name: raw.full_name,
        default_branch: raw.default_branch,
        visibility: if raw.visibility.is_empty() {
            if raw.private { "private" } else { "public" }.to_owned()
        } else {
            raw.visibility
        },
        owner_type: owner.kind,
        owner: owner.login,
        archived: raw.archived,
        admin: perms.admin,
        maintain: perms.maintain,
        push: perms.push,
        triage: perms.triage,
        pull: perms.pull,
        security,
        html_url: raw.html_url,
    })
}

/// `GET repos/{o}/{r}`: role, default branch, visibility.
pub fn fetch_access(gh: &Gh, calls: &mut Vec<ApiCall>) -> Result<Access> {
    parse_access(&gh.get(&gh.path(""), calls)?)
}

/// The HTTP status at the end of an error message (`… (HTTP 403)`).
pub fn http_status(error: &str) -> Option<u16> {
    let i = error.find("(HTTP ")?;
    error[i + 6..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

/// A GitHub feature a refusal can be about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    Dependabot,
    CodeScanning,
    DependencyGraph,
    Protection,
    Rulesets,
    CodeownersErrors,
    Environments,
    Secrets,
    Variables,
    OrgSecrets,
}

/// Why GitHub refused, and what to do about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Denied {
    pub title: String,
    pub body: String,
    /// Concrete steps (settings paths, commands).
    pub steps: Vec<String>,
    /// The settings page to open, when there is one.
    pub url: Option<String>,
    /// True when the feature is simply off (not a permission problem).
    pub disabled: bool,
}

/// Explain a failed read. `access` is the token's role when known.
pub fn explain(feature: Feature, error: &str, access: Option<&Access>, gh: &Gh) -> Denied {
    let status = http_status(error);
    let lower = error.to_ascii_lowercase();
    let role = access.map(|a| a.role()).unwrap_or("unknown");
    let admin = access.is_some_and(|a| a.admin);
    let settings = gh.web_url("/settings/security_analysis");
    let role_line = match access {
        Some(a) => format!(
            "Your role on {}: {role}.",
            if a.full_name.is_empty() {
                gh.full_name()
            } else {
                a.full_name.clone()
            }
        ),
        None => String::new(),
    };
    let scope_hint = "With a classic token, `gh auth refresh -h github.com -s security_events` adds the scope that security alerts need (the `repo` scope covers it for private repositories).";
    let mut d = Denied {
        title: String::new(),
        body: String::new(),
        steps: Vec::new(),
        url: None,
        disabled: false,
    };
    match feature {
        Feature::Dependabot => {
            if lower.contains("disabled for this repository") {
                d.disabled = true;
                d.title = "Dependabot alerts are turned off".into();
                d.body = format!(
                    "GitHub only raises Dependabot alerts once they are enabled for the repository. {}",
                    if admin {
                        "You are an administrator, so you can turn them on."
                    } else {
                        "Turning them on needs the admin role; ask an administrator."
                    }
                );
                d.steps = vec![
                    "Settings → Code security → Dependency graph → Enable".into(),
                    "Settings → Code security → Dependabot alerts → Enable".into(),
                    "Optional: Dependabot security updates → Enable, so GitHub opens fix pull requests".into(),
                ];
                d.url = Some(settings);
            } else if status == Some(403) || lower.contains("not authorized") {
                d.title = "Not allowed to read Dependabot alerts".into();
                d.body = format!(
                    "Dependabot alerts are visible to people with write, maintain or admin access, and to the security manager role. {role_line}"
                );
                d.steps = vec![
                    "Ask an administrator for write access or the security manager role".into(),
                    scope_hint.into(),
                ];
            } else if status == Some(404) {
                d.title = "Dependabot alerts are not available".into();
                d.body = format!(
                    "GitHub answered Not Found: the repository is not visible to this token, or alerts are not available for it. {role_line}"
                );
            }
        }
        Feature::CodeScanning => {
            if lower.contains("advanced security") {
                d.disabled = true;
                d.title = "Code scanning needs GitHub Code Security".into();
                d.body = "Private and internal repositories need GitHub Code Security (Advanced Security) for code scanning.".into();
                d.steps = vec!["Settings → Code security → GitHub Code Security → Enable".into()];
                d.url = Some(settings);
            } else if lower.contains("not enabled") || lower.contains("no analysis found") {
                d.disabled = true;
                d.title = if lower.contains("no analysis") {
                    "No code scanning analysis yet".into()
                } else {
                    "Code scanning is not set up".into()
                };
                d.body = format!(
                    "Code scanning alerts come from analyses uploaded by CodeQL or another SARIF tool. {}",
                    if access.is_some_and(|a| !a.is_public()) {
                        "On a private repository it also needs GitHub Code Security (Advanced Security)."
                    } else {
                        "It is free on public repositories."
                    }
                );
                d.steps = vec![
                    "Settings → Code security → Code scanning → CodeQL analysis → Set up → Default"
                        .into(),
                    "Or add a workflow that uploads SARIF with github/codeql-action/upload-sarif"
                        .into(),
                ];
                d.url = Some(settings);
            } else if status == Some(403) || lower.contains("not authorized") {
                d.title = "Not allowed to read code scanning alerts".into();
                d.body = format!(
                    "Code scanning alerts are visible to people with write access and to the security manager role. {role_line}"
                );
                d.steps = vec![
                    "Ask an administrator for write access or the security manager role".into(),
                    scope_hint.into(),
                ];
            } else if status == Some(404) {
                d.title = "Code scanning is not available".into();
                d.body = format!("GitHub answered Not Found. {role_line}");
            }
        }
        Feature::DependencyGraph => {
            if status == Some(404) || lower.contains("not enabled") {
                d.disabled = true;
                d.title = "GitHub has no dependency graph for this repository".into();
                d.body = "The SBOM export reads GitHub's dependency graph, which is off by default on private repositories (and always on for public ones).".into();
                d.steps = vec!["Settings → Code security → Dependency graph → Enable".into()];
                d.url = Some(settings);
            } else if status == Some(403) {
                d.title = "Not allowed to export the SBOM".into();
                d.body = format!(
                    "Exporting needs read access to the repository's contents. {role_line}"
                );
            }
        }
        Feature::Protection => {
            if lower.contains("upgrade to github pro")
                || lower.contains("make this repository public")
            {
                d.disabled = true;
                d.title = "Branch protection needs GitHub Pro, Team or a public repository".into();
                d.body = "Private repositories on a free plan cannot use branch protection rules or rulesets.".into();
            } else if status == Some(404) && admin {
                d.disabled = true;
                d.title = "This branch is not protected".into();
                d.body = "No classic branch protection rule applies. Rulesets may still apply: see the effective rules below.".into();
            } else if status == Some(404) || status == Some(403) {
                d.title = "Branch protection needs the admin role".into();
                d.body = format!(
                    "GitHub shows classic branch protection only to administrators and answers Not Found to everyone else, protected or not. {role_line} The effective rules below (rulesets) are readable with read access."
                );
                d.steps = vec!["Ask an administrator of the repository for the admin role".into()];
            }
        }
        Feature::Rulesets => {
            if status == Some(403) || status == Some(404) {
                d.title = "Rulesets are not readable".into();
                d.body = role_line.clone();
            }
        }
        Feature::CodeownersErrors => {
            if status == Some(404) {
                d.disabled = true;
                d.title = "GitHub found no CODEOWNERS file".into();
                d.body = "GitHub checks the default branch (or the branch asked for): .github/CODEOWNERS, then CODEOWNERS, then docs/CODEOWNERS. A file that exists only locally or on another branch has no GitHub errors yet.".into();
            }
        }
        Feature::Environments | Feature::Secrets | Feature::Variables | Feature::OrgSecrets => {
            if status == Some(403) || status == Some(404) {
                let what = match feature {
                    Feature::Environments => "environments",
                    Feature::Secrets => "secrets",
                    Feature::Variables => "variables",
                    _ => "organisation secrets",
                };
                d.title = format!("Not allowed to read {what}");
                d.body = format!(
                    "{} {role_line}",
                    match feature {
                        Feature::Environments => {
                            "Environments are listed for anyone who can read the repository; changing them needs the admin role."
                        }
                        Feature::OrgSecrets => {
                            "Organisation secrets shared with the repository are listed for collaborators; personal repositories have none."
                        }
                        _ => {
                            "Listing and changing Actions secrets and variables needs the admin role (or a custom role with the secrets / variables permission)."
                        }
                    }
                );
            }
        }
    }
    if d.title.is_empty() {
        d.title = "GitHub refused the request".into();
        d.body = role_line;
    }
    d
}

/// UTC `YYYY-MM-DDTHH:MM:SSZ` of an epoch time.
pub fn iso_utc(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// Seconds since the epoch.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Now as [`iso_utc`].
pub fn iso_now() -> String {
    iso_utc(now_secs())
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year as i32, month, day)
}

/// A random RFC 4122 version 4 UUID (from the process's hash keys and the
/// clock; good enough for document identifiers, not for cryptography).
pub fn new_uuid() -> String {
    use std::hash::{BuildHasher, Hasher};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut bytes = [0u8; 16];
    for (i, chunk) in bytes.chunks_mut(8).enumerate() {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(nanos);
        h.write_usize(i);
        h.write_u32(std::process::id());
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    uuid_from_bytes(bytes)
}

/// Format 16 bytes as a version 4, variant 1 UUID.
pub fn uuid_from_bytes(mut b: [u8; 16]) -> String {
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Files tracked in the index (`git ls-files -z`).
pub fn tracked_files(workdir: &std::path::Path, commands: &mut Vec<String>) -> Result<Vec<String>> {
    commands.push("git ls-files -z".into());
    let out = crate::cli::run(workdir, &["ls-files", "-z"], None)?;
    Ok(out
        .stdout
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect())
}

/// Cut a string to at most `max` characters (on a char boundary).
pub fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_and_roles() {
        let body = br#"{"full_name":"o/r","default_branch":"main","visibility":"private","private":true,
            "owner":{"login":"o","type":"Organization"},
            "permissions":{"admin":false,"maintain":false,"push":true,"triage":true,"pull":true},
            "security_and_analysis":{"secret_scanning":{"status":"enabled"},"dependabot_security_updates":{"status":"disabled"}}}"#;
        let a = parse_access(body).unwrap();
        assert_eq!(a.role(), "write");
        assert!(a.is_org());
        assert!(!a.is_public());
        assert_eq!(a.default_branch, "main");
        assert_eq!(a.security_status("secret_scanning"), Some("enabled"));
        assert_eq!(
            a.security_status("dependabot_security_updates"),
            Some("disabled")
        );
    }

    #[test]
    fn explanations() {
        let gh = Gh::new(".", "o", "r");
        let d = explain(
            Feature::Dependabot,
            "Dependabot alerts are disabled for this repository. (HTTP 403)",
            None,
            &gh,
        );
        assert!(d.disabled);
        assert!(d.steps.iter().any(|s| s.contains("Dependabot alerts")));
        assert_eq!(
            d.url.as_deref(),
            Some("https://github.com/o/r/settings/security_analysis")
        );
        let read = Access {
            pull: true,
            full_name: "o/r".into(),
            ..Default::default()
        };
        let d = explain(
            Feature::CodeScanning,
            "You are not authorized to read code scanning alerts. (HTTP 403)",
            Some(&read),
            &gh,
        );
        assert!(!d.disabled);
        assert!(d.body.contains("Your role on o/r: read."), "{}", d.body);
        let d = explain(
            Feature::Protection,
            "Not Found (HTTP 404)",
            Some(&read),
            &gh,
        );
        assert!(d.title.contains("admin"));
        let admin = Access {
            admin: true,
            ..read.clone()
        };
        let d = explain(
            Feature::Protection,
            "Branch not protected (HTTP 404)",
            Some(&admin),
            &gh,
        );
        assert!(d.disabled);
        let d = explain(
            Feature::CodeScanning,
            "Code scanning is not enabled for this repository. Please enable code scanning in the repository settings. (HTTP 403)",
            Some(&admin),
            &gh,
        );
        assert!(d.disabled);
        assert!(d.body.contains("Advanced Security"));
        assert_eq!(http_status("x (HTTP 422)\nSee y"), Some(422));
        assert_eq!(http_status("no status"), None);
    }

    #[test]
    fn times_and_ids() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(1_793_016_000), "2026-10-26T12:00:00Z");
        let u = uuid_from_bytes([0xff; 16]);
        assert_eq!(u, "ffffffff-ffff-4fff-bfff-ffffffffffff");
        let a = new_uuid();
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
        assert_ne!(a, new_uuid());
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(clip("ab", 3), "ab");
    }
}
