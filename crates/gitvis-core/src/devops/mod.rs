//! DevOps integrations, everything around the repository rather than in it:
//!
//! - [`gh`]: a typed helper over `gh api` (reads, writes with JSON bodies on
//!   standard input, GraphQL, pagination), recording every call.
//! - [`proc`]: running other tools (kubectl, terraform, act, npm, …) with
//!   streamed output, environment variables and process-tree cancellation.
//! - [`tools`]: which of those tools are installed, and their versions.
//! - Feature areas, each with no UI: [`review`] (pull requests, reviews,
//!   issues), [`ci`] (workflow control, artifacts, caches, history, local
//!   runs), [`ship`] (releases, deployments, where a commit is, DORA),
//!   [`runtime`] (registries, Kubernetes, Terraform), [`govern`] (alerts,
//!   SBOM, image scans, protection, CODEOWNERS, environments, secrets and
//!   variables), [`workbench`] (tasks, interactive rebase, bisect, worktrees,
//!   file history), [`forge`] (GitLab and Azure DevOps) and [`home`] (the
//!   cross-repository inbox).
//!
//! Rules every area follows:
//! - GitHub access only through the user's `gh auth`; no token is stored.
//! - Secret values travel on standard input ([`gh::Gh::send_withheld`],
//!   [`proc::ProcSpec::stdin`]) and never reach a command line, a log or
//!   an error message.
//! - Network calls happen only on an explicit user action (or a page the
//!   user opened), never in the background on a timer.
//! - Writes are built by pure functions (method, endpoint, body) so they can
//!   be unit tested without touching GitHub.

pub mod ci;
pub mod forge;
pub mod gh;
pub mod govern;
pub mod home;
pub mod proc;
pub mod review;
pub mod runtime;
pub mod ship;
pub mod tools;
pub mod workbench;

/// `1 byte`, `2.4 KB` style size for log lines.
pub fn plural_bytes(n: usize) -> String {
    if n == 1 {
        "1 byte".to_owned()
    } else if n < 1024 {
        format!("{n} bytes")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    }
}

/// Lower-case, dash-separated slug for branch names: `Fix the login page!`
/// → `fix-the-login-page`, at most `max` characters, no trailing dash.
pub fn slug(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(c.to_ascii_lowercase());
        } else {
            dash = true;
        }
        if out.len() >= max {
            break;
        }
    }
    if out.len() > max {
        out.truncate(max);
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("Fix the login page!", 40), "fix-the-login-page");
        assert_eq!(slug("  Ünïcode -- and   spaces ", 40), "n-code-and-spaces");
        assert_eq!(
            slug("a very long title that keeps going", 12),
            "a-very-long"
        );
        assert_eq!(slug("!!!", 10), "");
    }

    #[test]
    fn sizes() {
        assert_eq!(plural_bytes(1), "1 byte");
        assert_eq!(plural_bytes(900), "900 bytes");
        assert_eq!(plural_bytes(2048), "2.0 KB");
    }
}
