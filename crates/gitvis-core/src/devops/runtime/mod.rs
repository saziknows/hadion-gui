//! What is running: container packages on GHCR mapped to commits and tags,
//! Kubernetes through `kubectl` (contexts, namespaces, deployments with
//! images matched to commits / branches / tags, rollout status, pods, logs,
//! rollout restart and undo) and Terraform / OpenTofu (roots in the
//! repository, which roots a change touches, `validate`, `fmt -check`,
//! `plan` with a parsed summary, drift through `plan -refresh-only`).
//!
//! - [`matching`]: image tags, OCI revision labels and digests → commits,
//!   git tags and branch tips, and how far a commit is behind a branch.
//! - [`packages`]: GitHub Container Registry requests and models.
//! - [`kube`]: `kubectl` argument builders (always `--context` and
//!   `--request-timeout`), JSON models, rollout state, error triage.
//! - [`terraform`]: root detection, loose HCL facts, command lines and
//!   parsers for `validate -json`, plan text, `show -json` and drift.
//!
//! Nothing here writes on its own: writes (deleting a package version,
//! `rollout restart` / `undo`, `scale`) are pure builders the app runs after
//! a confirmation.

pub mod kube;
pub mod matching;
pub mod packages;
pub mod terraform;

/// The program to run for a tool: `env` (e.g. `GITVIS_KUBECTL`) overrides
/// the default name, so tests and screenshots can use a recording double.
pub fn program_override(env: &str, default: &str) -> String {
    std::env::var(env)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

/// Whether a program can be started: a PATH lookup (cheap), or the
/// override's file existing.
pub fn program_available(program: &str) -> bool {
    super::proc::resolve(program).is_some()
}

/// Path and version of a tool, honouring its override variable: with
/// `env` set, the override is run with `version_args` (first line kept);
/// otherwise [`tools::detect`](super::tools::detect) (cached). Call from a
/// background thread.
pub fn detect(
    tool: super::tools::Tool,
    env: &str,
    version_args: &[&str],
) -> Option<super::tools::Installed> {
    let Some(over) = std::env::var(env).ok().filter(|s| !s.trim().is_empty()) else {
        return super::tools::detect(tool);
    };
    let path = super::proc::resolve(over.trim())?;
    let spec = super::proc::ProcSpec::new(path.to_string_lossy().into_owned(), ".")
        .args(version_args.iter().copied());
    let version = super::proc::run_capture(&spec, Some(std::time::Duration::from_secs(10)))
        .ok()
        .map(|c| {
            let text = c.stdout_text();
            text.lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("")
                .to_owned()
        })
        .unwrap_or_default();
    Some(super::tools::Installed { path, version })
}
