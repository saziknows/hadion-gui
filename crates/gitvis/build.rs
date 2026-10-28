//! Embeds what the build knows about itself, for the About section, crash
//! reports and update checks:
//!
//! | Variable | Value |
//! |---|---|
//! | `GITVIS_BUILD_COMMIT` | full commit hash (`git rev-parse HEAD`) |
//! | `GITVIS_BUILD_DIRTY` | `1` when tracked files had uncommitted changes |
//! | `GITVIS_BUILD_DATE` | `2026-10-27T21:14:03Z` (UTC; `SOURCE_DATE_EPOCH` wins) |
//! | `GITVIS_BUILD_TARGET` | target triple |
//! | `GITVIS_BUILD_PROFILE` | `debug` / `release` |
//! | `GITVIS_BUILD_RUSTC` | `rustc -V` |
//! | `GITVIS_BUILD_REMOTE` | `owner/repo` of a GitHub `origin` (never the URL itself) |
//!
//! It never fails: outside a git checkout, or without git, the values are
//! empty and the app shows "unknown". CI can pass `GITVIS_BUILD_COMMIT` /
//! `GITVIS_BUILD_DATE` explicitly. It reruns when HEAD, the checked-out
//! branch or packed refs move, and when the sources change (for the dirty
//! flag).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let manifest =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into()));
    for var in [
        "GITVIS_BUILD_COMMIT",
        "GITVIS_BUILD_DATE",
        "GITVIS_BUILD_DIRTY",
        "SOURCE_DATE_EPOCH",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src");
    let core = manifest.join("..").join("gitvis-core").join("src");
    if core.is_dir() {
        println!("cargo:rerun-if-changed={}", core.display());
    }

    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git")
            .args(args)
            .current_dir(&manifest)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };

    // Rerun when HEAD moves: HEAD itself, the branch it points to, packed refs.
    if let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        let git_dir = PathBuf::from(git_dir);
        let common = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .map(PathBuf::from)
            .unwrap_or_else(|| git_dir.clone());
        watch(&git_dir.join("HEAD"));
        if let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD"))
            && let Some(r) = head.trim().strip_prefix("ref: ")
        {
            watch(&common.join(r));
        }
        watch(&common.join("packed-refs"));
    }

    let commit = env_or("GITVIS_BUILD_COMMIT", || git(&["rev-parse", "HEAD"]));
    let dirty = env_or("GITVIS_BUILD_DIRTY", || {
        git(&["status", "--porcelain", "--untracked-files=no"])
            .map(|s| if s.is_empty() { "0".into() } else { "1".into() })
    });
    let date = env_or("GITVIS_BUILD_DATE", || {
        let secs = std::env::var("SOURCE_DATE_EPOCH")
            .ok()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or_else(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0)
            });
        Some(iso(secs))
    });
    let remote = git(&["config", "--get", "remote.origin.url"])
        .and_then(|u| github_repo(&u))
        .unwrap_or_default();
    let rustc = std::env::var("RUSTC")
        .ok()
        .and_then(|r| Command::new(r).arg("-V").output().ok())
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();

    emit("GITVIS_BUILD_COMMIT", &commit);
    emit("GITVIS_BUILD_DIRTY", &dirty);
    emit("GITVIS_BUILD_DATE", &date);
    emit(
        "GITVIS_BUILD_TARGET",
        &std::env::var("TARGET").unwrap_or_default(),
    );
    emit(
        "GITVIS_BUILD_PROFILE",
        &std::env::var("PROFILE").unwrap_or_default(),
    );
    emit("GITVIS_BUILD_RUSTC", &rustc);
    emit("GITVIS_BUILD_REMOTE", &remote);
}

fn watch(p: &Path) {
    // A path that does not exist makes cargo rerun every time: only real ones.
    if p.exists() {
        println!("cargo:rerun-if-changed={}", p.display());
    }
}

fn env_or(name: &str, f: impl FnOnce() -> Option<String>) -> String {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(f)
        .unwrap_or_default()
}

fn emit(name: &str, value: &str) {
    let v: String = value.chars().filter(|c| !c.is_control()).collect();
    println!("cargo:rustc-env={name}={v}");
}

/// `owner/repo` of a github.com remote URL; anything else (other hosts,
/// local paths) gives `None`. Credentials in the URL are dropped.
fn github_repo(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = match url.strip_prefix("git@github.com:") {
        Some(r) => r,
        None => {
            let after = url.split_once("://")?.1;
            let host_path = after.rsplit_once('@').map_or(after, |(_, r)| r);
            let (host, path) = host_path.split_once('/')?;
            if !host.split(':').next()?.eq_ignore_ascii_case("github.com") {
                return None;
            }
            path
        }
    };
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, repo) = rest.split_once('/')?;
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (ok(owner) && ok(repo)).then(|| format!("{owner}/{repo}"))
}

/// `2026-10-27T21:14:03Z` from Unix seconds (civil_from_days).
fn iso(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}
