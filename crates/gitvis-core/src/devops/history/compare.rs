//! Compare two revisions: each side is a branch, tag, SHA or expression
//! (`HEAD~3`, `main@{1}`); the second side may be the working tree.
//!
//! - Two-dot (`A..B`): the files differ between A and B themselves
//!   (`git diff A B`).
//! - Three-dot (`A...B`): what B changed since it forked from A, i.e. the
//!   diff from their merge base to B (`git diff A...B`).
//!
//! Either way the commits are listed per side (`git log A ^B` and `git log
//! B ^A`, capped) and counted by `rev-list --left-right --count A...B`, so
//! a capped list still shows the real numbers.

use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use super::numstat::{self, FileStat};
use super::{git_bytes, git_text};
use crate::diff::FileDiff;

/// Most commits listed per side.
pub const MAX_COMMITS: usize = 1_000;
/// Diffs larger than this are not shown (the app offers to open the files).
pub const MAX_DIFF_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Mode {
    /// `A..B`: A against B.
    #[default]
    TwoDot,
    /// `A...B`: B against the merge base.
    ThreeDot,
}

impl Mode {
    pub fn separator(self) -> &'static str {
        match self {
            Mode::TwoDot => "..",
            Mode::ThreeDot => "...",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::TwoDot => "Two dots  A..B",
            Mode::ThreeDot => "Three dots  A...B",
        }
    }

    pub fn explain(self) -> &'static str {
        match self {
            Mode::TwoDot => {
                "Files: A against B as they are now. A change made on A after the two forked shows as its reverse."
            }
            Mode::ThreeDot => {
                "Files: only what B changed since it forked from A (merge base against B), like a pull request."
            }
        }
    }
}

/// What to compare. `b == None` is the working tree.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Request {
    pub a: String,
    pub b: Option<String>,
    pub mode: Mode,
}

impl Request {
    /// `A..B`, `A...B`, `A..working tree`.
    pub fn title(&self) -> String {
        format!(
            "{}{}{}",
            self.a,
            self.mode.separator(),
            self.b.as_deref().unwrap_or("working tree")
        )
    }
}

/// A revision as typed and what it points at.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Resolved {
    pub input: String,
    pub sha: String,
    pub subject: String,
    pub author: String,
    pub time: i64,
}

impl Resolved {
    pub fn short(&self) -> &str {
        super::short(&self.sha)
    }
}

/// A commit on one side.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RangeCommit {
    pub sha: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub subject: String,
}

impl RangeCommit {
    pub fn short(&self) -> &str {
        super::short(&self.sha)
    }
}

/// The result of a comparison.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Comparison {
    pub request: Request,
    pub a: Resolved,
    /// `None`: the working tree (commits are counted against HEAD).
    pub b: Option<Resolved>,
    pub head: Option<Resolved>,
    pub merge_base: Option<Resolved>,
    /// Commits reachable from A but not B, newest first (capped).
    pub only_a: Vec<RangeCommit>,
    pub only_b: Vec<RangeCommit>,
    /// The real counts (`rev-list --left-right --count`).
    pub count_a: usize,
    pub count_b: usize,
    pub files: Vec<FileStat>,
    pub elapsed_ms: u64,
}

impl Comparison {
    /// The SHA of side B, or HEAD for the working tree.
    pub fn b_sha(&self) -> &str {
        match (&self.b, &self.head) {
            (Some(b), _) => &b.sha,
            (None, Some(h)) => &h.sha,
            _ => "",
        }
    }

    pub fn truncated(&self) -> bool {
        self.only_a.len() < self.count_a || self.only_b.len() < self.count_b
    }

    /// The `git diff` revision arguments for this comparison.
    pub fn diff_revs(&self) -> Vec<String> {
        diff_revs(
            &self.a.sha,
            self.b.as_ref().map(|b| b.sha.as_str()),
            self.request.mode,
        )
    }
}

/// `git rev-parse` arguments that turn a revision into a commit SHA.
pub fn resolve_args(rev: &str) -> Vec<String> {
    vec![
        "rev-parse".into(),
        "--verify".into(),
        "--quiet".into(),
        "--end-of-options".into(),
        format!("{rev}^{{commit}}"),
    ]
}

/// The revision arguments of `git diff`.
pub fn diff_revs(a: &str, b: Option<&str>, mode: Mode) -> Vec<String> {
    match (b, mode) {
        (Some(b), Mode::TwoDot) => vec![a.to_owned(), b.to_owned()],
        (Some(b), Mode::ThreeDot) => vec![format!("{a}...{b}")],
        (None, Mode::TwoDot) => vec![a.to_owned()],
        (None, Mode::ThreeDot) => vec!["--merge-base".into(), a.to_owned()],
    }
}

/// `git diff --raw --numstat -M -z` of the comparison.
pub fn files_args(a: &str, b: Option<&str>, mode: Mode) -> Vec<String> {
    let mut v: Vec<String> = ["diff", "--raw", "--numstat", "-M", "-z", "--no-color"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    v.extend(diff_revs(a, b, mode));
    v
}

/// `git diff -M` of one file (both names for a rename).
pub fn file_diff_args(
    a: &str,
    b: Option<&str>,
    mode: Mode,
    path: &str,
    old_path: Option<&str>,
) -> Vec<String> {
    let mut v: Vec<String> = ["diff", "-M", "--no-color", "--no-ext-diff"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    v.extend(diff_revs(a, b, mode));
    v.push("--".into());
    if let Some(old) = old_path.filter(|o| *o != path) {
        v.push(old.to_owned());
    }
    v.push(path.to_owned());
    v
}

/// `git log` of the commits reachable from `include` but not `exclude`.
pub fn side_log_args(include: &str, exclude: &str, cap: usize) -> Vec<String> {
    vec![
        "log".into(),
        "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%s".into(),
        format!("--max-count={}", cap.max(1)),
        include.to_owned(),
        format!("^{exclude}"),
    ]
}

pub fn count_args(a: &str, b: &str) -> Vec<String> {
    vec![
        "rev-list".into(),
        "--left-right".into(),
        "--count".into(),
        format!("{a}...{b}"),
    ]
}

/// `3\t5` → (3, 5).
pub fn parse_count(text: &str) -> Option<(usize, usize)> {
    let mut it = text.split_whitespace();
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

/// Parse [`side_log_args`] output (newest first).
pub fn parse_side(text: &str) -> Vec<RangeCommit> {
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.splitn(6, '\u{1f}').collect();
        if f.len() < 6 {
            continue;
        }
        out.push(RangeCommit {
            sha: f[0].trim().to_owned(),
            parents: f[1].split_whitespace().map(str::to_owned).collect(),
            author: f[2].to_owned(),
            email: f[3].to_owned(),
            time: f[4].trim().parse().unwrap_or(0),
            subject: f[5].to_owned(),
        });
    }
    out
}

/// Resolve a revision to a commit with its subject.
pub fn resolve(dir: &Path, rev: &str, cmds: &mut Vec<String>) -> Result<Resolved, String> {
    let rev = rev.trim();
    if rev.is_empty() {
        return Err("Enter a branch, tag or commit.".into());
    }
    if rev.starts_with('-') {
        return Err(format!("`{rev}` is not a revision."));
    }
    let sha = git_text(dir, &resolve_args(rev), cmds)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("`{rev}` is not a branch, tag or commit in this repository."))?;
    let info = git_text(
        dir,
        &["log", "-1", "--format=%s%x1f%an%x1f%at", sha.as_str()],
        cmds,
    )
    .unwrap_or_default();
    let mut f = info.trim_end().splitn(3, '\u{1f}');
    Ok(Resolved {
        input: rev.to_owned(),
        sha,
        subject: f.next().unwrap_or("").to_owned(),
        author: f.next().unwrap_or("").to_owned(),
        time: f.next().and_then(|t| t.trim().parse().ok()).unwrap_or(0),
    })
}

/// Run the comparison.
pub fn run(dir: &Path, req: &Request, cmds: &mut Vec<String>) -> Result<Comparison, String> {
    let started = Instant::now();
    let a = resolve(dir, &req.a, cmds)?;
    let b = match &req.b {
        Some(b) => Some(resolve(dir, b, cmds)?),
        None => None,
    };
    let head = if b.is_none() {
        Some(resolve(dir, "HEAD", cmds).map_err(|_| {
            "The repository has no commits yet, so there is nothing to compare the working tree with."
                .to_owned()
        })?)
    } else {
        None
    };
    let b_sha = b
        .as_ref()
        .or(head.as_ref())
        .map(|r| r.sha.clone())
        .unwrap_or_default();
    let merge_base = git_text(dir, &["merge-base", a.sha.as_str(), b_sha.as_str()], cmds)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .and_then(|sha| resolve(dir, &sha, cmds).ok());
    let (count_a, count_b) = git_text(dir, &count_args(&a.sha, &b_sha), cmds)
        .ok()
        .and_then(|t| parse_count(&t))
        .unwrap_or((0, 0));
    let only_a = if count_a == 0 {
        Vec::new()
    } else {
        parse_side(&git_text(
            dir,
            &side_log_args(&a.sha, &b_sha, MAX_COMMITS),
            cmds,
        )?)
    };
    let only_b = if count_b == 0 {
        Vec::new()
    } else {
        parse_side(&git_text(
            dir,
            &side_log_args(&b_sha, &a.sha, MAX_COMMITS),
            cmds,
        )?)
    };
    if req.mode == Mode::ThreeDot && merge_base.is_none() {
        return Err(format!(
            "{} and {} have no common history, so there is no merge base for a three-dot comparison. Use two dots.",
            a.input,
            b.as_ref()
                .map(|b| b.input.as_str())
                .unwrap_or("the working tree")
        ));
    }
    let raw = git_bytes(
        dir,
        &files_args(&a.sha, b.as_ref().map(|b| b.sha.as_str()), req.mode),
        cmds,
    )?;
    let files = numstat::parse(&raw);
    Ok(Comparison {
        request: req.clone(),
        a,
        b,
        head,
        merge_base,
        only_a,
        only_b,
        count_a,
        count_b,
        files,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// The diff of one file of a comparison (`FileDiff::too_large` past
/// [`MAX_DIFF_BYTES`]).
pub fn file_diff(
    dir: &Path,
    c: &Comparison,
    file: &FileStat,
    cmds: &mut Vec<String>,
) -> Result<FileDiff, String> {
    let args = file_diff_args(
        &c.a.sha,
        c.b.as_ref().map(|b| b.sha.as_str()),
        c.request.mode,
        &file.path,
        file.old_path.as_deref(),
    );
    let out = git_bytes(dir, &args, cmds)?;
    if out.len() > MAX_DIFF_BYTES {
        return Ok(FileDiff::too_large());
    }
    let mut diffs = crate::diff::parse(&out);
    Ok(if diffs.is_empty() {
        FileDiff::default()
    } else {
        diffs.remove(0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rev_ranges() {
        assert_eq!(diff_revs("a", Some("b"), Mode::TwoDot), vec!["a", "b"]);
        assert_eq!(diff_revs("a", Some("b"), Mode::ThreeDot), vec!["a...b"]);
        assert_eq!(diff_revs("a", None, Mode::TwoDot), vec!["a"]);
        assert_eq!(
            diff_revs("a", None, Mode::ThreeDot),
            vec!["--merge-base", "a"]
        );
        assert_eq!(
            file_diff_args("a", Some("b"), Mode::TwoDot, "new.rs", Some("old.rs")),
            vec![
                "diff",
                "-M",
                "--no-color",
                "--no-ext-diff",
                "a",
                "b",
                "--",
                "old.rs",
                "new.rs"
            ]
        );
        assert_eq!(
            files_args("a", Some("b"), Mode::ThreeDot).last().unwrap(),
            "a...b"
        );
        assert_eq!(resolve_args("HEAD~3").last().unwrap(), "HEAD~3^{commit}");
        let r = Request {
            a: "main".into(),
            b: None,
            mode: Mode::ThreeDot,
        };
        assert_eq!(r.title(), "main...working tree");
    }

    #[test]
    fn counts_and_sides() {
        assert_eq!(parse_count("3\t5\n"), Some((3, 5)));
        assert_eq!(parse_count("x"), None);
        let text = "aaa\u{1f}p1\u{1f}Ann\u{1f}a@x\u{1f}100\u{1f}One\nbbb\u{1f}p1 p2\u{1f}Bob\u{1f}b@x\u{1f}200\u{1f}Merge: with \u{1f} inside\nccc\u{1f}\u{1f}Bob\u{1f}b@x\u{1f}50\u{1f}Root\nbroken line\n";
        let r = parse_side(text);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].subject, "One");
        assert_eq!(r[1].parents, vec!["p1", "p2"]);
        assert_eq!(r[1].subject, "Merge: with \u{1f} inside");
        assert!(r[2].parents.is_empty());
        assert_eq!(r[2].time, 50);
        assert_eq!(side_log_args("b", "a", 10)[3..], ["b", "^a"]);
    }
}
