//! Is the forge's CLI installed and signed in? `glab auth status` for
//! GitLab; `az extension show --name azure-devops` and `az account show`
//! for Azure DevOps. Each state comes with the exact commands that fix it.

use std::path::Path;
use std::time::Duration;

use super::cli::{self, Call, Program};
use super::{ForgeKind, ForgeRemote, azure, gitlab};
use crate::devops::proc::{ProcSpec, run_capture};
use crate::devops::tools::{self as devtools, Tool};
use crate::github::ApiCall;

/// The environment variable the azure-devops extension reads a personal
/// access token from.
pub const AZURE_PAT_ENV: &str = "AZURE_DEVOPS_EXT_PAT";

/// Where the CLI stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolState {
    /// Not on PATH.
    Missing,
    /// `az` without the azure-devops extension.
    NoExtension { version: String },
    /// Installed but not signed in; `detail` is what the CLI said.
    SignedOut { version: String, detail: String },
    Ready {
        version: String,
        /// Username (GitLab) or account (Azure).
        user: String,
        /// How it is signed in (`glab auth`, `az login`, the PAT variable).
        via: String,
    },
}

impl ToolState {
    pub fn ready(&self) -> bool {
        matches!(self, ToolState::Ready { .. })
    }

    pub fn user(&self) -> &str {
        match self {
            ToolState::Ready { user, .. } => user,
            _ => "",
        }
    }
}

/// The CLI a forge needs.
pub fn tool(kind: ForgeKind) -> Tool {
    match kind {
        ForgeKind::GitLab => Tool::Glab,
        ForgeKind::Azure => Tool::Az,
    }
}

/// The commands that fix a state, in order.
pub fn fix_commands(remote: &ForgeRemote, state: &ToolState) -> Vec<String> {
    let host = if remote.host.eq_ignore_ascii_case("gitlab.com") {
        String::new()
    } else {
        format!(" --hostname {}", remote.host)
    };
    let org = remote
        .org
        .as_deref()
        .map(|o| format!("https://dev.azure.com/{o}"))
        .unwrap_or_default();
    match (remote.kind, state) {
        (ForgeKind::GitLab, ToolState::Missing) => vec![
            "winget install GLab.GLab".into(),
            format!("glab auth login{host}"),
        ],
        (ForgeKind::GitLab, _) => vec![format!("glab auth login{host}")],
        (ForgeKind::Azure, ToolState::Missing) => vec![
            "winget install Microsoft.AzureCLI".into(),
            "az extension add --name azure-devops".into(),
            "az login".into(),
        ],
        (ForgeKind::Azure, ToolState::NoExtension { .. }) => vec![
            "az extension add --name azure-devops".into(),
            "az login".into(),
        ],
        (ForgeKind::Azure, _) => vec![
            "az login".into(),
            format!(
                "az devops login --organization {org}   (alternative: a personal access token)"
            ),
        ],
    }
}

fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .to_owned()
}

/// What a status command printed.
struct Captured {
    ok: bool,
    stdout: String,
    /// stdout and stderr together (status commands print to stderr).
    all: String,
}

/// Run a call and keep both pipes.
fn capture(call: &Call, dir: &Path, calls: &mut Vec<ApiCall>) -> Captured {
    let mut spec = ProcSpec::new(call.program.name(), dir).args(call.args.iter().cloned());
    if call.program == Program::Az {
        spec = spec
            .env("AZURE_CORE_ONLY_SHOW_ERRORS", "true")
            .env("AZURE_EXTENSION_USE_DYNAMIC_INSTALL", "no");
    }
    match run_capture(&spec, Some(Duration::from_secs(60))) {
        Ok(c) => {
            let all = format!("{}\n{}", c.stdout_text(), c.stderr);
            calls.push(ApiCall {
                command: call.display(),
                elapsed_ms: c.elapsed_ms,
                bytes: c.stdout.len(),
                rate: None,
                error: (!c.success()).then(|| c.error_text()),
                input: None,
            });
            Captured {
                ok: c.success(),
                stdout: c.stdout_text(),
                all: all.trim().to_owned(),
            }
        }
        Err(e) => {
            calls.push(ApiCall {
                command: call.display(),
                elapsed_ms: 0,
                bytes: 0,
                rate: None,
                error: Some(e.to_string()),
                input: None,
            });
            Captured {
                ok: false,
                stdout: String::new(),
                all: e.to_string(),
            }
        }
    }
}

/// `az version -o tsv` prints three tab-separated versions (cli, core,
/// telemetry): keep the first.
pub fn clean_version(t: Tool, v: &str) -> String {
    match t {
        Tool::Az => match v.split_whitespace().next() {
            Some(first) if !first.is_empty() => format!("azure-cli {first}"),
            _ => v.to_owned(),
        },
        _ => v.trim().to_owned(),
    }
}

/// Check the CLI for a remote (runs one to three commands; call from a
/// background thread).
pub fn check(remote: &ForgeRemote, dir: &Path, calls: &mut Vec<ApiCall>) -> ToolState {
    let t = tool(remote.kind);
    let Some(installed) = devtools::detect(t) else {
        return ToolState::Missing;
    };
    let version = clean_version(t, &installed.version);
    match remote.kind {
        ForgeKind::GitLab => {
            let status = capture(&gitlab::auth_status(remote), dir, calls);
            if !status.ok {
                return ToolState::SignedOut {
                    version,
                    detail: first_line(&status.all),
                };
            }
            let user = match cli::run(
                &gitlab::current_user(remote).call(),
                dir,
                Duration::from_secs(30),
                calls,
            )
            .ok()
            .and_then(|b| gitlab::parse_user(&b).ok())
            {
                Some(p) => p.login,
                None => gitlab::login_from_status(&status.all).unwrap_or_default(),
            };
            ToolState::Ready {
                version,
                user,
                via: "glab auth".into(),
            }
        }
        ForgeKind::Azure => {
            let ext = capture(&azure::extension_show(), dir, calls);
            if !ext.ok {
                return ToolState::NoExtension { version };
            }
            let ext = azure::parse_extension(ext.stdout.as_bytes()).unwrap_or_default();
            let version = if ext.is_empty() {
                version
            } else {
                format!("{version}, azure-devops {ext}")
            };
            let account = capture(&azure::account_show(), dir, calls);
            if account.ok {
                let user = azure::parse_account(account.stdout.as_bytes()).unwrap_or_default();
                return ToolState::Ready {
                    version,
                    user,
                    via: "az login".into(),
                };
            }
            if std::env::var_os(AZURE_PAT_ENV).is_some_and(|v| !v.is_empty()) {
                return ToolState::Ready {
                    version,
                    user: String::new(),
                    via: format!("{AZURE_PAT_ENV} (personal access token)"),
                };
            }
            ToolState::SignedOut {
                version,
                detail: first_line(&account.all),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::forge::detect;

    #[test]
    fn fixes_name_the_exact_commands() {
        let gl = detect("https://gitlab.com/a/b").unwrap();
        assert_eq!(
            fix_commands(&gl, &ToolState::Missing),
            vec!["winget install GLab.GLab", "glab auth login"]
        );
        let own = detect("https://gitlab.example.org/a/b").unwrap();
        assert_eq!(
            fix_commands(
                &own,
                &ToolState::SignedOut {
                    version: String::new(),
                    detail: String::new()
                }
            ),
            vec!["glab auth login --hostname gitlab.example.org"]
        );
        let az = detect("https://dev.azure.com/acme/Web/_git/frontend").unwrap();
        assert_eq!(fix_commands(&az, &ToolState::Missing).len(), 3);
        assert_eq!(
            fix_commands(
                &az,
                &ToolState::NoExtension {
                    version: String::new()
                }
            )[0],
            "az extension add --name azure-devops"
        );
        let signed_out = fix_commands(
            &az,
            &ToolState::SignedOut {
                version: String::new(),
                detail: String::new(),
            },
        );
        assert_eq!(signed_out[0], "az login");
        assert!(signed_out[1].contains("https://dev.azure.com/acme"));
        assert_eq!(tool(ForgeKind::Azure), Tool::Az);
        assert_eq!(
            clean_version(Tool::Az, &["2.91.0", "2.91.0", "1.1.0"].join("\t")),
            "azure-cli 2.91.0"
        );
        assert_eq!(clean_version(Tool::Glab, " glab 1.46.1 "), "glab 1.46.1");
        assert!(!ToolState::Missing.ready());
        assert_eq!(
            ToolState::Ready {
                version: "1".into(),
                user: "ada".into(),
                via: "x".into()
            }
            .user(),
            "ada"
        );
    }
}
