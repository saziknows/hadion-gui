//! Stashes: the list with branch, age and changed files (`git stash list`
//! plus `git stash show --numstat --include-untracked` per entry), one
//! stash's patch, and the commands that apply, pop, drop, branch from and
//! create a stash of selected files.

use std::collections::HashSet;
use std::path::Path;

use super::{PatchFile, git, git_ok, git_probe, parse_numstat, split_patch, unquote};

/// `stash list` record: ref, sha, time, reflog subject, parents.
pub const LIST_FORMAT: &str = "--format=%gd%x1f%H%x1f%ct%x1f%gs%x1f%P%x1e";

/// Stashes whose files are listed (each costs one or two git calls).
pub const MAX_DETAILED: usize = 60;

/// One changed file of a stash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StashFile {
    pub path: String,
    /// `None` for binary files.
    pub added: Option<u64>,
    pub deleted: Option<u64>,
    /// Stashed with `--include-untracked`.
    pub untracked: bool,
}

/// One stash entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StashEntry {
    /// `N` of `stash@{N}`.
    pub index: usize,
    pub sha: String,
    /// Unix seconds when it was made.
    pub time: i64,
    /// The branch it was made on (`None` on a detached HEAD).
    pub branch: Option<String>,
    /// The message given (`-m`), or git's `WIP` text with the commit.
    pub message: String,
    /// Made without a message: git wrote `WIP on BRANCH: SHA SUBJECT`.
    pub wip: bool,
    /// The commit HEAD was on (first parent).
    pub base: Option<String>,
    /// Has an untracked-files commit (third parent).
    pub has_untracked: bool,
    /// `None` until listed (or past [`MAX_DETAILED`]).
    pub files: Option<Vec<StashFile>>,
    pub files_error: Option<String>,
}

impl StashEntry {
    pub fn reference(&self) -> String {
        format!("stash@{{{}}}", self.index)
    }

    pub fn added(&self) -> u64 {
        self.files.iter().flatten().filter_map(|f| f.added).sum()
    }

    pub fn deleted(&self) -> u64 {
        self.files.iter().flatten().filter_map(|f| f.deleted).sum()
    }

    pub fn untracked_count(&self) -> usize {
        self.files.iter().flatten().filter(|f| f.untracked).count()
    }
}

pub fn list_args() -> Vec<String> {
    vec!["stash".into(), "list".into(), LIST_FORMAT.into()]
}

/// `On main: message` → (`main`, `message`, false); `WIP on main: 1a2b3c
/// Subject` → (`main`, `1a2b3c Subject`, true); `(no branch)` → `None`.
pub fn parse_reflog_subject(s: &str) -> (Option<String>, String, bool) {
    let s = s.trim();
    let (wip, rest) = if let Some(r) = s.strip_prefix("WIP on ") {
        (true, r)
    } else if let Some(r) = s.strip_prefix("On ") {
        (false, r)
    } else {
        return (None, s.to_owned(), false);
    };
    match rest.split_once(": ") {
        Some((branch, msg)) => {
            let branch = (branch != "(no branch)").then(|| branch.to_owned());
            (branch, msg.to_owned(), wip)
        }
        None => (None, rest.trim_end_matches(':').to_owned(), wip),
    }
}

/// Parse [`LIST_FORMAT`] output.
pub fn parse_list(text: &str) -> Vec<StashEntry> {
    let mut out = Vec::new();
    for rec in text.split('\u{1e}') {
        let rec = rec.trim_matches(['\n', '\r']);
        if rec.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = rec.split('\u{1f}').collect();
        if f.len() < 5 {
            continue;
        }
        let Some(index) = f[0]
            .trim()
            .strip_prefix("stash@{")
            .and_then(|r| r.strip_suffix('}'))
            .and_then(|n| n.parse().ok())
        else {
            continue;
        };
        let parents: Vec<&str> = f[4].split_whitespace().collect();
        let (branch, message, wip) = parse_reflog_subject(f[3]);
        out.push(StashEntry {
            index,
            sha: f[1].trim().to_owned(),
            time: f[2].trim().parse().unwrap_or(0),
            branch,
            message,
            wip,
            base: parents.first().map(|p| (*p).to_owned()),
            has_untracked: parents.len() >= 3,
            files: None,
            files_error: None,
        });
    }
    out
}

pub fn numstat_args(index: usize) -> Vec<String> {
    vec![
        "-c".into(),
        "core.quotepath=false".into(),
        "stash".into(),
        "show".into(),
        "--numstat".into(),
        "--include-untracked".into(),
        format!("stash@{{{index}}}"),
    ]
}

pub fn untracked_args(index: usize) -> Vec<String> {
    vec![
        "-c".into(),
        "core.quotepath=false".into(),
        "stash".into(),
        "show".into(),
        "--only-untracked".into(),
        "--name-only".into(),
        format!("stash@{{{index}}}"),
    ]
}

/// The full patch, untracked files included, with the default `a/` `b/`
/// prefixes whatever the user's diff settings (`git stash show` mangles
/// `--src-prefix=…`, so the settings are overridden instead).
pub fn patch_args(index: usize) -> Vec<String> {
    vec![
        "-c".into(),
        "core.quotepath=false".into(),
        "-c".into(),
        "diff.noprefix=false".into(),
        "-c".into(),
        "diff.mnemonicPrefix=false".into(),
        "stash".into(),
        "show".into(),
        "-p".into(),
        "--include-untracked".into(),
        "--no-color".into(),
        "--no-ext-diff".into(),
        format!("stash@{{{index}}}"),
    ]
}

/// Files of one stash from its numstat and the untracked listing.
pub fn files_of(numstat: &str, untracked: &str) -> Vec<StashFile> {
    let untracked: HashSet<String> = untracked
        .lines()
        .map(|l| unquote(l.trim()))
        .filter(|l| !l.is_empty())
        .collect();
    parse_numstat(numstat)
        .into_iter()
        .map(|(path, added, deleted)| StashFile {
            untracked: untracked.contains(&path),
            path,
            added,
            deleted,
        })
        .collect()
}

/// The stash list with each entry's files (up to [`MAX_DETAILED`]).
pub fn load(dir: &Path, cmds: &mut Vec<String>) -> Result<Vec<StashEntry>, String> {
    let text = git(dir, &list_args(), cmds)?;
    let mut list = parse_list(&text);
    for e in list.iter_mut().take(MAX_DETAILED) {
        match git(dir, &numstat_args(e.index), cmds) {
            Ok(numstat) => {
                let untracked = if e.has_untracked {
                    git(dir, &untracked_args(e.index), cmds).unwrap_or_default()
                } else {
                    String::new()
                };
                e.files = Some(files_of(&numstat, &untracked));
            }
            Err(err) => e.files_error = Some(err),
        }
    }
    Ok(list)
}

/// The patch of one stash, file by file.
pub fn patch(dir: &Path, index: usize, cmds: &mut Vec<String>) -> Result<Vec<PatchFile>, String> {
    let args = patch_args(index);
    cmds.push(super::git_line(&args));
    crate::cli::run(dir, &args, None)
        .map_err(|e| e.to_string())
        .map(|o| split_patch(&o.stdout))
}

/// `git stash push -m MSG [--keep-index] [--include-untracked] -- paths…`.
/// No paths stashes every change.
pub fn push_args(
    message: &str,
    keep_index: bool,
    include_untracked: bool,
    paths: &[String],
) -> Vec<String> {
    let mut v: Vec<String> = vec!["stash".into(), "push".into()];
    let message = message.trim();
    if !message.is_empty() {
        v.push("-m".into());
        v.push(message.to_owned());
    }
    if keep_index {
        v.push("--keep-index".into());
    }
    if include_untracked {
        v.push("--include-untracked".into());
    }
    if !paths.is_empty() {
        v.push("--".into());
        v.extend(paths.iter().cloned());
    }
    v
}

/// `git stash apply [--index] stash@{N}`.
pub fn apply_args(index: usize, restore_index: bool) -> Vec<String> {
    with_index("apply", index, restore_index)
}

/// `git stash pop [--index] stash@{N}`.
pub fn pop_args(index: usize, restore_index: bool) -> Vec<String> {
    with_index("pop", index, restore_index)
}

fn with_index(verb: &str, index: usize, restore_index: bool) -> Vec<String> {
    let mut v: Vec<String> = vec!["stash".into(), verb.into()];
    if restore_index {
        v.push("--index".into());
    }
    v.push(format!("stash@{{{index}}}"));
    v
}

/// `git stash drop stash@{N}`.
pub fn drop_args(index: usize) -> Vec<String> {
    vec!["stash".into(), "drop".into(), format!("stash@{{{index}}}")]
}

/// `git stash branch NAME stash@{N}`: a new branch at the stash's base
/// commit with the stash applied, then dropped.
pub fn branch_args(name: &str, index: usize) -> Vec<String> {
    vec![
        "stash".into(),
        "branch".into(),
        name.trim().to_owned(),
        format!("stash@{{{index}}}"),
    ]
}

/// What a stash action does, for confirmations and the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StashAction {
    Apply { restore_index: bool },
    Pop { restore_index: bool },
    Drop,
}

impl StashAction {
    pub fn args(self, index: usize) -> Vec<String> {
        match self {
            StashAction::Apply { restore_index } => apply_args(index, restore_index),
            StashAction::Pop { restore_index } => pop_args(index, restore_index),
            StashAction::Drop => drop_args(index),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            StashAction::Apply { .. } => "Apply stash",
            StashAction::Pop { .. } => "Pop stash",
            StashAction::Drop => "Drop stash",
        }
    }
}

/// Run `args` on `stash@{index}` after checking it is still the stash
/// `sha` (indices shift when another stash is made or dropped).
pub fn run_checked(
    dir: &Path,
    index: usize,
    sha: &str,
    args: &[String],
    cmds: &mut Vec<String>,
) -> Result<String, String> {
    let now = git_probe(
        dir,
        &[
            "rev-parse".to_owned(),
            "--verify".into(),
            "--quiet".into(),
            format!("stash@{{{index}}}"),
        ],
        cmds,
    );
    if now.as_deref() != Some(sha) {
        return Err(format!(
            "stash@{{{index}}} is no longer the stash you picked (the list changed). Refresh and pick it again."
        ));
    }
    git_ok(dir, args, cmds).map(|t| {
        if t.trim().is_empty() {
            "Done".to_owned()
        } else {
            t
        }
    })
}

/// A branch name git accepts (`git check-ref-format --branch`).
pub fn valid_branch_name(dir: &Path, name: &str, cmds: &mut Vec<String>) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Enter a branch name.".to_owned());
    }
    git_ok(dir, &["check-ref-format", "--branch", name], cmds)
        .map(|_| ())
        .map_err(|_| format!("\"{name}\" is not a valid branch name."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflog_subjects() {
        assert_eq!(
            parse_reflog_subject("On main: my message"),
            (Some("main".into()), "my message".into(), false)
        );
        assert_eq!(
            parse_reflog_subject("WIP on feat/x: 1894563 init: first"),
            (Some("feat/x".into()), "1894563 init: first".into(), true)
        );
        assert_eq!(
            parse_reflog_subject("WIP on (no branch): abc Subject"),
            (None, "abc Subject".into(), true)
        );
        assert_eq!(
            parse_reflog_subject("autostash"),
            (None, "autostash".into(), false)
        );
    }

    #[test]
    fn parses_the_list() {
        let text = "stash@{0}\u{1f}6d06\u{1f}1793137499\u{1f}On feat: on feat\u{1f}1894 fa8a\u{1e}\n\
stash@{1}\u{1f}73bd\u{1f}1793137400\u{1f}WIP on main: 1894563 init\u{1f}1894 a000\u{1e}\n\
stash@{2}\u{1f}bdca\u{1f}1793137300\u{1f}On main: my message\u{1f}1894 a000 ea5f\u{1e}\n";
        let v = parse_list(text);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].index, 0);
        assert_eq!(v[0].sha, "6d06");
        assert_eq!(v[0].branch.as_deref(), Some("feat"));
        assert_eq!(v[0].message, "on feat");
        assert!(!v[0].has_untracked);
        assert_eq!(v[1].time, 1793137400);
        assert!(v[1].wip);
        assert_eq!(v[1].base.as_deref(), Some("1894"));
        assert!(v[2].has_untracked);
        assert_eq!(v[2].reference(), "stash@{2}");
    }

    #[test]
    fn files_and_totals() {
        let mut e = parse_list("stash@{0}\u{1f}a\u{1f}1\u{1f}On m: x\u{1f}p q r\u{1e}")
            .pop()
            .unwrap();
        e.files = Some(files_of(
            "2\t1\ta.txt\n1\t0\tu.txt\n-\t-\tlogo.png\n",
            "u.txt\n",
        ));
        let files = e.files.as_ref().unwrap();
        assert!(files[1].untracked);
        assert!(!files[0].untracked);
        assert_eq!(files[2].added, None);
        assert_eq!((e.added(), e.deleted(), e.untracked_count()), (3, 1, 1));
    }

    #[test]
    fn command_lines() {
        assert_eq!(
            push_args(
                " wip ",
                true,
                true,
                &["a b.txt".to_owned(), "c.txt".to_owned()]
            ),
            vec![
                "stash",
                "push",
                "-m",
                "wip",
                "--keep-index",
                "--include-untracked",
                "--",
                "a b.txt",
                "c.txt"
            ]
        );
        assert_eq!(push_args("", false, false, &[]), vec!["stash", "push"]);
        assert_eq!(
            apply_args(2, true),
            vec!["stash", "apply", "--index", "stash@{2}"]
        );
        assert_eq!(pop_args(0, false), vec!["stash", "pop", "stash@{0}"]);
        assert_eq!(drop_args(3), vec!["stash", "drop", "stash@{3}"]);
        assert_eq!(
            branch_args(" fix ", 1),
            vec!["stash", "branch", "fix", "stash@{1}"]
        );
        assert!(patch_args(4).contains(&"--include-untracked".to_owned()));
        assert_eq!(
            StashAction::Pop {
                restore_index: false
            }
            .args(1),
            pop_args(1, false)
        );
    }
}
