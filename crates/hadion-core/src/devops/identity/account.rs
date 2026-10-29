//! The GitHub CLI account: `gh auth status` (the `--json hosts` form of
//! gh 2.70+, and the two text forms of older versions), the signed-in user,
//! the rate limits, which Hadion features the token's scopes allow, and the
//! `gh` commands that fix what is missing.
//!
//! Tokens are never read: gh masks them in `auth status` (`gho_****`), and
//! only the prefix (`gho_`, which says what kind of token it is) is kept.

use serde::Deserialize;
use serde_json::Value;

use super::config::{Entry, split_key};
use crate::devops::gh::Gh;
use crate::github::ApiCall;

/// One account gh knows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Account {
    pub host: String,
    pub login: String,
    pub active: bool,
    /// `success`, `error`, `timeout`.
    pub state: String,
    pub error: Option<String>,
    /// `keyring`, `GITHUB_TOKEN`, the hosts.yml path.
    pub token_source: String,
    /// `None` when gh does not report scopes (fine-grained and app tokens).
    pub scopes: Option<Vec<String>>,
    /// `https` or `ssh`.
    pub protocol: String,
    /// The token's prefix only (`gho_`).
    pub token_kind: Option<String>,
}

impl Account {
    pub fn ok(&self) -> bool {
        self.state == "success"
    }

    /// What kind of token the prefix says this is.
    pub fn token_label(&self) -> Option<&'static str> {
        Some(match self.token_kind.as_deref()? {
            "gho_" => "OAuth token (gh auth login)",
            "ghp_" => "Personal access token (classic)",
            "github_pat_" => "Fine-grained personal access token",
            "ghu_" => "GitHub App user token",
            "ghs_" => "GitHub App installation token",
            _ => "Token",
        })
    }
}

fn split_scopes(s: &str) -> Option<Vec<String>> {
    let v: Vec<String> = s
        .split(',')
        .map(|x| x.trim().trim_matches('\'').trim().to_owned())
        .filter(|x| !x.is_empty() && x != "none")
        .collect();
    if s.trim().is_empty() { None } else { Some(v) }
}

/// Parse `gh auth status --json hosts`.
pub fn parse_status_json(text: &str) -> Result<Vec<Account>, String> {
    let v: Value =
        serde_json::from_str(text.trim()).map_err(|e| format!("gh auth status --json: {e}"))?;
    let hosts = v
        .get("hosts")
        .and_then(Value::as_object)
        .ok_or("gh auth status --json: no hosts")?;
    let mut out = Vec::new();
    for (host, list) in hosts {
        for a in list.as_array().into_iter().flatten() {
            let s = |k: &str| a.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
            let error = ["error", "message"]
                .iter()
                .find_map(|k| a.get(*k).and_then(Value::as_str))
                .map(str::to_owned)
                .filter(|e| !e.is_empty());
            out.push(Account {
                host: if s("host").is_empty() {
                    host.clone()
                } else {
                    s("host")
                },
                login: s("login"),
                active: a.get("active").and_then(Value::as_bool).unwrap_or(false),
                state: if s("state").is_empty() {
                    "success".to_owned()
                } else {
                    s("state")
                },
                error,
                token_source: s("tokenSource"),
                scopes: a
                    .get("scopes")
                    .and_then(Value::as_str)
                    .and_then(split_scopes),
                protocol: s("gitProtocol"),
                token_kind: None,
            });
        }
    }
    Ok(out)
}

fn token_prefix(masked: &str) -> Option<String> {
    let t = masked.trim();
    if t.starts_with("github_pat_") {
        return Some("github_pat_".to_owned());
    }
    let i = t.find('_')?;
    (i <= 4).then(|| t[..=i].to_owned())
}

/// Parse the text of `gh auth status` (stdout or stderr), in the format of
/// gh 2.40+ (`Logged in to HOST account LOGIN (SOURCE)` with `- key: value`
/// lines) or older (`Logged in to HOST as LOGIN (SOURCE)`, `✓` lines).
pub fn parse_status_text(text: &str) -> Vec<Account> {
    let mut out: Vec<Account> = Vec::new();
    let mut host = String::new();
    for raw in text.lines() {
        if raw.trim().is_empty() {
            continue;
        }
        if !raw.starts_with(char::is_whitespace) && !raw.contains(' ') {
            host = raw.trim().trim_end_matches(':').to_owned();
            continue;
        }
        let line = raw
            .trim()
            .trim_start_matches(['✓', 'X', '✗', '!', '-', '*', '×'])
            .trim();
        let start = line
            .find("Logged in to ")
            .map(|i| (i + "Logged in to ".len(), true))
            .or_else(|| {
                line.find("Failed to log in to ")
                    .map(|i| (i + "Failed to log in to ".len(), false))
            });
        if let Some((i, ok)) = start {
            let rest = &line[i..];
            let (h, rest) = rest.split_once(' ').unwrap_or((rest, ""));
            let (login_part, is_new) = if let Some(r) = rest.strip_prefix("account ") {
                (r, true)
            } else if let Some(r) = rest.strip_prefix("as ") {
                (r, false)
            } else {
                (rest, false)
            };
            let (login, source) = match login_part.find(" (") {
                Some(p) => (
                    login_part[..p].trim(),
                    login_part[p + 2..].trim_end_matches(')').trim(),
                ),
                None => (login_part.trim(), ""),
            };
            out.push(Account {
                host: if host.is_empty() {
                    h.to_owned()
                } else {
                    host.clone()
                },
                login: login.to_owned(),
                // The old format lists one account per host: the active one.
                active: !is_new,
                state: if ok { "success" } else { "error" }.to_owned(),
                error: None,
                token_source: source.to_owned(),
                scopes: None,
                protocol: String::new(),
                token_kind: None,
            });
            continue;
        }
        let Some(a) = out.last_mut() else {
            continue;
        };
        if let Some(v) = line.strip_prefix("Active account:") {
            a.active = v.trim() == "true";
        } else if let Some(v) = line.strip_prefix("Git operations protocol:") {
            a.protocol = v.trim().to_owned();
        } else if line.starts_with("Git operations for ") {
            // Old: "Git operations for github.com configured to use https protocol."
            if let Some(p) = line.split(" to use ").nth(1) {
                a.protocol = p.split_whitespace().next().unwrap_or("").to_owned();
            }
        } else if let Some(v) = line.strip_prefix("Token scopes:") {
            a.scopes = split_scopes(v);
        } else if let Some(v) = line.strip_prefix("Token:") {
            a.token_kind = token_prefix(v);
        } else if !a.ok() || line.contains("invalid") || line.contains("no longer valid") {
            let msg = line.to_owned();
            a.error = Some(match a.error.take() {
                Some(e) => format!("{e}\n{msg}"),
                None => msg,
            });
            if line.contains("invalid") || line.contains("no longer valid") {
                a.state = "error".to_owned();
            }
        }
    }
    out
}

/// Did gh say nobody is signed in?
pub fn not_logged_in(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("not logged into any") || t.contains("not logged in to any")
}

/// One rate limit bucket of `gh api rate_limit`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bucket {
    pub name: String,
    pub limit: u64,
    pub used: u64,
    pub remaining: u64,
    /// Epoch seconds.
    pub reset: i64,
}

/// The buckets of `gh api rate_limit`: core, graphql, search first.
pub fn parse_rate(v: &Value) -> Vec<Bucket> {
    let Some(res) = v.get("resources").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out: Vec<Bucket> = res
        .iter()
        .map(|(name, b)| {
            let n = |k: &str| b.get(k).and_then(Value::as_u64).unwrap_or(0);
            Bucket {
                name: name.clone(),
                limit: n("limit"),
                used: n("used"),
                remaining: n("remaining"),
                reset: n("reset") as i64,
            }
        })
        .collect();
    let rank = |n: &str| match n {
        "core" => 0,
        "graphql" => 1,
        "search" => 2,
        "code_search" => 3,
        _ => 4,
    };
    out.sort_by(|a, b| rank(&a.name).cmp(&rank(&b.name)).then(a.name.cmp(&b.name)));
    out
}

/// The signed-in user (`gh api user`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct User {
    pub login: String,
    pub name: Option<String>,
    pub html_url: Option<String>,
    pub email: Option<String>,
    pub created_at: Option<String>,
    pub public_repos: Option<u64>,
    pub two_factor_authentication: Option<bool>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

/// A Hadion feature that needs a token scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Feature {
    pub name: &'static str,
    /// Where it is in Hadion.
    pub place: &'static str,
    /// Any of these scopes is enough; the first is what to ask for.
    pub any_of: &'static [&'static str],
    pub why: &'static str,
}

/// Features and the scopes they need (classic tokens and `gh auth login`).
pub const FEATURES: [Feature; 10] = [
    Feature {
        name: "Private repositories",
        place: "Review, Actions, Ship, Govern",
        any_of: &["repo"],
        why: "Read and change pull requests, runs, releases and settings of private repositories (public ones need no scope to read).",
    },
    Feature {
        name: "Workflow files",
        place: "Actions, pushing .github/workflows",
        any_of: &["workflow"],
        why: "GitHub refuses pushes and API writes that change workflow files without it.",
    },
    Feature {
        name: "Teams and organisations",
        place: "Review requests, Govern → CODEOWNERS, Home",
        any_of: &["read:org"],
        why: "Team members for review requests and CODEOWNERS checks, organisation repositories.",
    },
    Feature {
        name: "Notifications inbox",
        place: "Home → Inbox",
        any_of: &["notifications", "repo"],
        why: "The notifications API answers 403 without one of these.",
    },
    Feature {
        name: "Security alerts",
        place: "Govern → Dependabot, code scanning",
        any_of: &["security_events", "repo"],
        why: "Alert lists of private repositories.",
    },
    Feature {
        name: "Packages",
        place: "Ship → Packages",
        any_of: &["read:packages"],
        why: "Container images and packages on GHCR: versions, tags, sizes.",
    },
    Feature {
        name: "Delete package versions",
        place: "Ship → Packages",
        any_of: &["delete:packages"],
        why: "Removing old image versions.",
    },
    Feature {
        name: "Upload SSH keys",
        place: "Settings → Keys and signing",
        any_of: &["admin:public_key", "write:public_key"],
        why: "gh ssh-key add --type authentication.",
    },
    Feature {
        name: "Upload SSH signing keys",
        place: "Settings → Keys and signing",
        any_of: &["admin:ssh_signing_key", "write:ssh_signing_key"],
        why: "gh ssh-key add --type signing, for \"Verified\" on SSH-signed commits.",
    },
    Feature {
        name: "Upload GPG keys",
        place: "Settings → Keys and signing",
        any_of: &["admin:gpg_key", "write:gpg_key"],
        why: "gh gpg-key add, for \"Verified\" on GPG-signed commits.",
    },
];

/// Scopes that include others.
const IMPLIES: [(&str, &[&str]); 10] = [
    (
        "repo",
        &[
            "repo:status",
            "repo_deployment",
            "public_repo",
            "repo:invite",
            "security_events",
        ],
    ),
    ("admin:org", &["write:org", "read:org"]),
    ("write:org", &["read:org"]),
    ("admin:public_key", &["write:public_key", "read:public_key"]),
    ("admin:gpg_key", &["write:gpg_key", "read:gpg_key"]),
    (
        "admin:ssh_signing_key",
        &["write:ssh_signing_key", "read:ssh_signing_key"],
    ),
    ("write:packages", &["read:packages"]),
    ("user", &["read:user", "user:email", "user:follow"]),
    ("admin:repo_hook", &["write:repo_hook", "read:repo_hook"]),
    ("project", &["read:project"]),
];

/// The scope among `have` that grants `need`, if any.
pub fn grants<'a>(have: &'a [String], need: &str) -> Option<&'a str> {
    have.iter().map(String::as_str).find(|h| {
        *h == need
            || IMPLIES
                .iter()
                .any(|(parent, kids)| parent == h && kids.contains(&need))
    })
}

/// Whether a feature works with a token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Need {
    /// Granted, by this scope.
    Ok(String),
    /// Missing; this scope would add it.
    Missing(&'static str),
    /// Scopes are not reported for this token.
    Unknown,
}

/// Every feature against the token's scopes.
pub fn analyse(scopes: Option<&[String]>) -> Vec<(Feature, Need)> {
    FEATURES
        .iter()
        .map(|f| {
            let need = match scopes {
                None => Need::Unknown,
                Some(have) => match f.any_of.iter().find_map(|n| grants(have, n)) {
                    Some(by) => Need::Ok(by.to_owned()),
                    None => Need::Missing(f.any_of[0]),
                },
            };
            (*f, need)
        })
        .collect()
}

/// The scopes to request for everything missing, without duplicates.
pub fn missing_scopes(analysis: &[(Feature, Need)]) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = Vec::new();
    for (_, n) in analysis {
        if let Need::Missing(s) = n
            && !v.contains(s)
        {
            v.push(s);
        }
    }
    v
}

/// `gh auth refresh -h HOST -s a,b`: adds scopes to the active account's
/// token (gh opens the browser for a one-time code).
pub fn refresh_command(host: &str, scopes: &[&str]) -> String {
    format!("gh auth refresh -h {host} -s {}", scopes.join(","))
}

/// `gh auth switch -h HOST -u LOGIN`.
pub fn switch_args(host: &str, login: &str) -> Vec<String> {
    vec![
        "auth".into(),
        "switch".into(),
        "-h".into(),
        host.into(),
        "-u".into(),
        login.into(),
    ]
}

/// `gh auth setup-git`: gh answers git's credential requests for GitHub.
pub fn setup_git_args() -> Vec<String> {
    vec!["auth".into(), "setup-git".into()]
}

/// Which credential helpers git has, and whether gh answers for GitHub.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Credentials {
    /// `credential.helper` and `credential.<url>.helper` entries.
    pub helpers: Vec<Entry>,
    /// `credential.https://github.com.helper` runs `gh auth git-credential`.
    pub gh_for_github: bool,
}

/// Read the credential helpers from a config listing.
pub fn credentials(entries: &[Entry]) -> Credentials {
    let helpers: Vec<Entry> = entries
        .iter()
        .filter(|e| {
            let (s, _, n) = split_key(&e.key);
            s == "credential" && n == "helper"
        })
        .cloned()
        .collect();
    let gh_for_github = helpers.iter().any(|e| {
        e.subsection()
            .is_some_and(|u| u.trim_end_matches('/') == "https://github.com")
            && e.value_text().contains("auth git-credential")
    });
    Credentials {
        helpers,
        gh_for_github,
    }
}

/// What the GitHub account section shows.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub accounts: Vec<Account>,
    /// `--json` or `text`: which `gh auth status` form was read.
    pub status_form: &'static str,
    /// gh said nobody is signed in.
    pub signed_out: bool,
    /// The status command failed for another reason.
    pub status_error: Option<String>,
    pub user: Option<Result<User, String>>,
    pub rate: Option<Result<Vec<Bucket>, String>>,
}

impl Report {
    /// The active account of github.com (or of any host).
    pub fn active(&self) -> Option<&Account> {
        self.accounts
            .iter()
            .find(|a| a.active && a.host == "github.com")
            .or_else(|| self.accounts.iter().find(|a| a.active))
    }
}

/// `gh auth status` (JSON first, the text form for older gh), then the
/// user and the rate limits when someone is signed in.
pub fn load(gh: &Gh, calls: &mut Vec<ApiCall>) -> Report {
    let mut r = Report::default();
    match gh.cli(&["auth", "status", "--json", "hosts"], None, calls) {
        Ok(t) => match parse_status_json(&t) {
            Ok(a) => {
                r.accounts = a;
                r.status_form = "--json";
            }
            Err(e) => r.status_error = Some(e),
        },
        Err(e) => {
            let msg = e.to_string();
            if not_logged_in(&msg) {
                r.signed_out = true;
            } else if msg.contains("unknown flag") || msg.contains("unknown shorthand") {
                // gh older than 2.70: read the text form (it exits 1 when an
                // account has a problem, so both outcomes are parsed).
                r.status_form = "text";
                let text = match gh.cli(&["auth", "status"], None, calls) {
                    Ok(t) => t,
                    Err(e) => e.to_string(),
                };
                if not_logged_in(&text) {
                    r.signed_out = true;
                } else {
                    r.accounts = parse_status_text(&text);
                    if r.accounts.is_empty() {
                        r.status_error = Some(text);
                    }
                }
            } else {
                r.status_error = Some(msg);
            }
        }
    }
    if r.accounts.is_empty() && r.status_error.is_none() {
        r.signed_out = true;
    }
    if r.accounts.iter().any(|a| a.active && a.ok()) {
        r.user = Some(
            gh.get_json::<User>("user", "user", calls)
                .map_err(|e| e.to_string()),
        );
        r.rate = Some(
            gh.get_json::<Value>("rate_limit", "rate limit", calls)
                .map(|v| parse_rate(&v))
                .map_err(|e| e.to_string()),
        );
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::identity::Scope;
    use crate::devops::identity::config::Origin;

    #[test]
    fn json_status() {
        let a = parse_status_json(
            r#"{"hosts":{"github.com":[{"state":"success","active":true,"host":"github.com","login":"octocat","tokenSource":"keyring","scopes":"gist, read:org, repo, workflow","gitProtocol":"https"},{"state":"error","active":false,"host":"github.com","login":"devrdd","tokenSource":"keyring","error":"The token in keyring is invalid.","gitProtocol":"https"}]}}"#,
        )
        .unwrap();
        assert_eq!(a.len(), 2);
        assert!(a[0].active && a[0].ok());
        assert_eq!(
            a[0].scopes.as_deref().unwrap(),
            ["gist", "read:org", "repo", "workflow"]
        );
        assert_eq!(a[1].state, "error");
        assert_eq!(
            a[1].error.as_deref(),
            Some("The token in keyring is invalid.")
        );
        assert_eq!(a[1].scopes, None);
        assert!(parse_status_json("not json").is_err());
    }

    #[test]
    fn text_status_new_format() {
        let t = "github.com\n  ✓ Logged in to github.com account octocat (keyring)\n  - Active account: true\n  - Git operations protocol: https\n  - Token: gho_************************************\n  - Token scopes: 'gist', 'read:org', 'repo', 'workflow'\n\n  X Failed to log in to github.com account devrdd (keyring)\n  - Active account: false\n  - The token in keyring is invalid.\n  - To re-authenticate, run: gh auth login -h github.com\n\nghe.corp.example\n  ✓ Logged in to ghe.corp.example account me (GH_ENTERPRISE_TOKEN)\n  - Active account: true\n  - Git operations protocol: ssh\n  - Token: ghp_************************************\n";
        let a = parse_status_text(t);
        assert_eq!(a.len(), 3);
        assert_eq!(a[0].login, "octocat");
        assert_eq!(a[0].token_source, "keyring");
        assert!(a[0].active);
        assert_eq!(a[0].protocol, "https");
        assert_eq!(a[0].token_kind.as_deref(), Some("gho_"));
        assert_eq!(a[0].token_label(), Some("OAuth token (gh auth login)"));
        assert_eq!(a[0].scopes.as_ref().unwrap().len(), 4);
        assert_eq!(a[1].state, "error");
        assert!(!a[1].active);
        assert!(a[1].error.as_deref().unwrap().contains("invalid"));
        assert_eq!(a[2].host, "ghe.corp.example");
        assert_eq!(a[2].protocol, "ssh");
        assert_eq!(a[2].scopes, None, "no scopes line");
        // Nothing beyond the prefix of the masked token is kept.
        assert!(!format!("{a:?}").contains("****"));
    }

    #[test]
    fn text_status_old_format() {
        let t = "github.com\n  ✓ Logged in to github.com as octocat (C:\\Users\\me\\AppData\\Roaming\\GitHub CLI\\hosts.yml)\n  ✓ Git operations for github.com configured to use ssh protocol.\n  ✓ Token: ghp_************************************\n  ✓ Token scopes: gist, read:org, repo\n";
        let a = parse_status_text(t);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].login, "octocat");
        assert!(a[0].active);
        assert_eq!(a[0].protocol, "ssh");
        assert!(a[0].token_source.ends_with("hosts.yml"));
        assert_eq!(
            a[0].scopes.as_deref().unwrap(),
            ["gist", "read:org", "repo"]
        );
        assert!(not_logged_in(
            "You are not logged into any GitHub hosts. To log in, run: gh auth login"
        ));
        assert!(parse_status_text("You are not logged into any GitHub hosts.").is_empty());
    }

    #[test]
    fn scope_analysis() {
        let have: Vec<String> = ["gist", "read:org", "repo", "workflow"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let a = analyse(Some(&have));
        let by_name = |n: &str| a.iter().find(|(f, _)| f.name == n).unwrap().1.clone();
        assert_eq!(by_name("Private repositories"), Need::Ok("repo".into()));
        assert_eq!(by_name("Notifications inbox"), Need::Ok("repo".into()));
        assert_eq!(by_name("Security alerts"), Need::Ok("repo".into()));
        assert_eq!(by_name("Packages"), Need::Missing("read:packages"));
        assert_eq!(
            by_name("Upload SSH keys"),
            Need::Missing("admin:public_key")
        );
        let missing = missing_scopes(&a);
        assert_eq!(
            missing,
            [
                "read:packages",
                "delete:packages",
                "admin:public_key",
                "admin:ssh_signing_key",
                "admin:gpg_key"
            ]
        );
        assert_eq!(
            refresh_command("github.com", &missing[..2]),
            "gh auth refresh -h github.com -s read:packages,delete:packages"
        );
        // Parents grant their children.
        let admin = vec!["admin:org".to_owned(), "write:packages".to_owned()];
        assert_eq!(grants(&admin, "read:org"), Some("admin:org"));
        assert_eq!(grants(&admin, "read:packages"), Some("write:packages"));
        assert!(analyse(None).iter().all(|(_, n)| *n == Need::Unknown));
        assert_eq!(
            switch_args("github.com", "devrdd").join(" "),
            "auth switch -h github.com -u devrdd"
        );
    }

    #[test]
    fn rate_limits() {
        let v: Value = serde_json::from_str(
            r#"{"resources":{"search":{"limit":30,"used":0,"remaining":30,"reset":1700000060},"core":{"limit":5000,"used":12,"remaining":4988,"reset":1700003600},"graphql":{"limit":5000,"used":0,"remaining":5000,"reset":1700003600},"code_scanning_upload":{"limit":1000,"used":0,"remaining":1000,"reset":1}},"rate":{}}"#,
        )
        .unwrap();
        let b = parse_rate(&v);
        assert_eq!(
            b.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(),
            ["core", "graphql", "search", "code_scanning_upload"]
        );
        assert_eq!(b[0].remaining, 4988);
        assert_eq!(b[0].reset, 1700003600);
        assert!(parse_rate(&Value::Null).is_empty());
    }

    #[test]
    fn credential_helpers() {
        let e = |key: &str, value: &str| Entry {
            scope: Scope::Global,
            origin: Origin::File("x".into()),
            file: None,
            key: key.to_owned(),
            value: Some(value.to_owned()),
            line: None,
        };
        let c = credentials(&[
            e("credential.helper", "manager"),
            e("credential.https://github.com.helper", ""),
            e(
                "credential.https://github.com.helper",
                "!'C:\\Program Files\\GitHub CLI\\gh.exe' auth git-credential",
            ),
            e("credential.https://dev.azure.com.usehttppath", "true"),
        ]);
        assert_eq!(c.helpers.len(), 3);
        assert!(c.gh_for_github);
        assert!(!credentials(&[e("credential.helper", "manager")]).gh_for_github);
    }
}
