//! "Fetch all": `git fetch --all --prune` in each open repository, one after
//! another, with what each fetch changed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::devops::proc::{ProcSpec, run_capture};

/// The arguments of one fetch.
pub const ARGS: [&str; 3] = ["fetch", "--all", "--prune"];

/// The command line as shown in the confirmation and the log.
pub fn command_line() -> String {
    format!("git {}", ARGS.join(" "))
}

/// The process for one repository.
pub fn spec(dir: &Path) -> ProcSpec {
    ProcSpec::new("git", dir).args(ARGS)
}

/// What git reported about the refs it touched (from its stderr).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Remotes fetched (`Fetching origin`).
    pub remotes: Vec<String>,
    /// New branches and tags.
    pub new: usize,
    /// Fast-forwarded or force-updated refs.
    pub updated: usize,
    /// Refs removed by `--prune`.
    pub pruned: usize,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.new == 0 && self.updated == 0 && self.pruned == 0
    }

    /// `2 new, 3 updated, 1 pruned` or `up to date`.
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "up to date".to_owned();
        }
        let mut parts = Vec::new();
        if self.new > 0 {
            parts.push(format!("{} new", self.new));
        }
        if self.updated > 0 {
            parts.push(format!("{} updated", self.updated));
        }
        if self.pruned > 0 {
            parts.push(format!("{} pruned", self.pruned));
        }
        parts.join(", ")
    }
}

/// Count ref updates in `git fetch` output:
/// ` * [new branch]  x -> origin/x`, `   a1..b2  main -> origin/main`,
/// ` + a1...b2 x -> origin/x  (forced update)`, ` - [deleted] (none) -> origin/y`.
pub fn parse_changes(stderr: &str) -> Changes {
    let mut c = Changes::default();
    for line in stderr.lines() {
        let l = line.trim();
        if let Some(r) = l.strip_prefix("Fetching ") {
            c.remotes.push(r.trim().to_owned());
            continue;
        }
        if !l.contains("->") {
            continue;
        }
        if l.starts_with("- [deleted]") || l.contains("[deleted]") {
            c.pruned += 1;
        } else if l.starts_with("* [new") {
            c.new += 1;
        } else if l.starts_with('+') || l.contains("..") || l.starts_with("t [tag update]") {
            c.updated += 1;
        }
    }
    c
}

/// The outcome of one repository's fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub name: String,
    pub dir: PathBuf,
    pub ok: bool,
    pub elapsed_ms: u64,
    pub changes: Changes,
    /// git's output (stderr, where fetch reports), trimmed.
    pub output: String,
}

/// Run one fetch with a timeout (fetches that wait for a password or a
/// dead host are killed).
pub fn run(name: &str, dir: &Path, timeout: Duration) -> Outcome {
    match run_capture(&spec(dir), Some(timeout)) {
        Ok(c) => {
            let output = if c.stderr.trim().is_empty() {
                c.stdout_text().trim().to_owned()
            } else {
                c.stderr.trim().to_owned()
            };
            Outcome {
                name: name.to_owned(),
                dir: dir.to_owned(),
                ok: c.success(),
                elapsed_ms: c.elapsed_ms,
                changes: parse_changes(&c.stderr),
                output: if c.success() { output } else { c.error_text() },
            }
        }
        Err(e) => Outcome {
            name: name.to_owned(),
            dir: dir.to_owned(),
            ok: false,
            elapsed_ms: 0,
            changes: Changes::default(),
            output: e.to_string(),
        },
    }
}

/// One line for the activity log and the toast.
pub fn summary(outcomes: &[Outcome]) -> String {
    let failed = outcomes.iter().filter(|o| !o.ok).count();
    let changed = outcomes
        .iter()
        .filter(|o| o.ok && !o.changes.is_empty())
        .count();
    let mut s = format!(
        "Fetched {} of {} {}",
        outcomes.len() - failed,
        outcomes.len(),
        if outcomes.len() == 1 {
            "repository"
        } else {
            "repositories"
        }
    );
    if changed > 0 {
        s.push_str(&format!(", {changed} with new commits or refs"));
    }
    if failed > 0 {
        s.push_str(&format!(", {failed} failed"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_ref_updates() {
        let out = "Fetching origin\n\
From github.com:o/r\n \
* [new branch]      feature    -> origin/feature\n \
* [new tag]         v1.2       -> v1.2\n   \
a1b2c3d..e4f5a6b  main       -> origin/main\n \
+ 1111111...2222222 rebased  -> origin/rebased  (forced update)\n \
- [deleted]         (none)     -> origin/old\n\
Fetching upstream\n";
        let c = parse_changes(out);
        assert_eq!(c.remotes, vec!["origin", "upstream"]);
        assert_eq!((c.new, c.updated, c.pruned), (2, 2, 1));
        assert_eq!(c.summary(), "2 new, 2 updated, 1 pruned");
        assert_eq!(parse_changes("Fetching origin\n").summary(), "up to date");
    }

    #[test]
    fn command_and_summary() {
        assert_eq!(command_line(), "git fetch --all --prune");
        let s = spec(Path::new("."));
        assert_eq!(s.program, "git");
        assert_eq!(s.args, vec!["fetch", "--all", "--prune"]);
        let ok = Outcome {
            name: "a".into(),
            dir: PathBuf::from("a"),
            ok: true,
            elapsed_ms: 10,
            changes: Changes {
                new: 1,
                ..Default::default()
            },
            output: String::new(),
        };
        let bad = Outcome {
            ok: false,
            changes: Changes::default(),
            ..ok.clone()
        };
        assert_eq!(
            summary(&[ok.clone(), bad]),
            "Fetched 1 of 2 repositories, 1 with new commits or refs, 1 failed"
        );
        assert_eq!(
            summary(&[ok]),
            "Fetched 1 of 1 repository, 1 with new commits or refs"
        );
    }

    #[test]
    fn fetch_against_a_local_bare_remote() {
        let root = std::env::temp_dir().join(format!("gitvis-home-fetch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let git = |dir: &Path, args: &[&str]| {
            let st = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.com")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.com")
                .output()
                .unwrap();
            assert!(
                st.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&st.stderr)
            );
        };
        let bare = root.join("remote.git");
        let work = root.join("work");
        let other = root.join("other");
        std::fs::create_dir_all(&bare).unwrap();
        git(&bare, &["init", "--bare", "-q", "-b", "main"]);
        git(&root, &["clone", "-q", bare.to_str().unwrap(), "work"]);
        git(&root, &["clone", "-q", bare.to_str().unwrap(), "other"]);
        git(&work, &["commit", "-q", "--allow-empty", "-m", "one"]);
        git(&work, &["push", "-q", "origin", "HEAD:main"]);
        git(&work, &["push", "-q", "origin", "HEAD:feature"]);
        let o = run("other", &other, Duration::from_secs(60));
        assert!(o.ok, "{}", o.output);
        assert_eq!(o.changes.new, 2, "{}", o.output);
        let again = run("other", &other, Duration::from_secs(60));
        assert!(again.ok && again.changes.is_empty(), "{:?}", again.changes);
        let _ = std::fs::remove_dir_all(&root);
    }
}
