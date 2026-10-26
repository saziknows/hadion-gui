//! Local git reads shared by the ship features: resolving revisions,
//! ancestry, counts and short logs. Every command is recorded (repeated
//! calls of one kind are summarised so the activity log stays readable).

use std::path::Path;

use crate::{Error, Result, cli};

/// Unit separator / record separator for `--format` output.
pub(crate) const US: char = '\u{1f}';
pub(crate) const RS: char = '\u{1e}';

/// Run `git args…`, record the command, return stdout (lossy UTF-8).
pub(crate) fn git(dir: &Path, args: &[&str], commands: &mut Vec<String>) -> Result<String> {
    commands.push(format!("git {}", render(args)));
    let out = cli::run(dir, args, None)?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `git args…` with `stdin`, recorded with a note about the input.
pub(crate) fn git_stdin(
    dir: &Path,
    args: &[&str],
    stdin: &[u8],
    note: &str,
    commands: &mut Vec<String>,
) -> Result<String> {
    commands.push(format!("git {}   # {note}", render(args)));
    let out = cli::run(dir, args, Some(stdin))?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn render(args: &[&str]) -> String {
    crate::devops::proc::render_args(args)
}

/// Counts repeated commands of one kind and records only the first few,
/// then one summary line (`… and 37 more git merge-base --is-ancestor`).
pub(crate) struct Tally {
    what: &'static str,
    shown: usize,
    extra: usize,
}

impl Tally {
    pub(crate) const SHOWN: usize = 6;

    pub(crate) fn new(what: &'static str) -> Self {
        Self {
            what,
            shown: 0,
            extra: 0,
        }
    }

    pub(crate) fn record(&mut self, command: String, commands: &mut Vec<String>) {
        if self.shown < Self::SHOWN {
            commands.push(command);
            self.shown += 1;
        } else {
            self.extra += 1;
        }
    }

    pub(crate) fn finish(self, commands: &mut Vec<String>) {
        if self.extra > 0 {
            commands.push(format!("… and {} more {}", self.extra, self.what));
        }
    }
}

/// `git merge-base --is-ancestor a b`: `Some(true)` when `a` is an ancestor
/// of (or equal to) `b`, `Some(false)` when not, `None` when git could not
/// tell (an object is missing).
pub(crate) fn is_ancestor(
    dir: &Path,
    a: &str,
    b: &str,
    tally: &mut Tally,
    commands: &mut Vec<String>,
) -> Option<bool> {
    let args = ["merge-base", "--is-ancestor", a, b];
    tally.record(format!("git {}", render(&args)), commands);
    let out = cli::run_raw("git", dir, &args, None, &[]).ok()?;
    match out.code {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// Resolve revisions to full commit ids with one `git cat-file
/// --batch-check`; `None` for revisions that are not (local) commits.
pub fn resolve_many(
    dir: &Path,
    revs: &[String],
    commands: &mut Vec<String>,
) -> Vec<Option<String>> {
    if revs.is_empty() {
        return Vec::new();
    }
    let mut input = String::new();
    for r in revs {
        // A newline inside a name would shift every answer: never ask for it.
        input.push_str(&r.replace(['\n', '\r'], " "));
        input.push_str("^{commit}\n");
    }
    let note = format!("{} revisions on standard input", revs.len());
    let Ok(out) = git_stdin(
        dir,
        &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
        input.as_bytes(),
        &note,
        commands,
    ) else {
        return vec![None; revs.len()];
    };
    let mut answers: Vec<Option<String>> = out
        .lines()
        .map(|l| {
            let mut parts = l.split_whitespace();
            match (parts.next(), parts.next()) {
                (Some(id), Some("commit")) if id.len() >= 40 => Some(id.to_owned()),
                _ => None,
            }
        })
        .collect();
    answers.resize(revs.len(), None);
    answers
}

/// One revision to a full commit id, `None` when it does not resolve.
pub fn resolve(dir: &Path, rev: &str, commands: &mut Vec<String>) -> Option<String> {
    resolve_many(dir, &[rev.to_owned()], commands)
        .into_iter()
        .next()
        .flatten()
}

/// `git rev-list --count range`.
pub(crate) fn count(dir: &Path, range: &str, commands: &mut Vec<String>) -> Option<u32> {
    git(dir, &["rev-list", "--count", range], commands)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// The remote default branch as `origin/main`: `origin/HEAD` when set,
/// else the first of `origin/main`, `origin/master`, `origin/trunk`,
/// `origin/develop` that exists.
pub fn default_branch(
    dir: &Path,
    hint: Option<&str>,
    commands: &mut Vec<String>,
) -> Option<String> {
    if let Some(h) = hint.filter(|h| !h.is_empty()) {
        let name = format!("origin/{h}");
        if resolve(dir, &name, commands).is_some() {
            return Some(name);
        }
    }
    if let Ok(s) = git(
        dir,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
        commands,
    ) {
        let s = s.trim();
        if !s.is_empty() {
            return Some(s.to_owned());
        }
    }
    let candidates: Vec<String> = ["main", "master", "trunk", "develop"]
        .iter()
        .map(|b| format!("origin/{b}"))
        .collect();
    let found = resolve_many(dir, &candidates, commands);
    candidates
        .into_iter()
        .zip(found)
        .find(|(_, f)| f.is_some())
        .map(|(c, _)| c)
}

/// One commit of a short log.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogLine {
    pub sha: String,
    pub author: String,
    /// Author time, seconds since the epoch.
    pub author_time: i64,
    /// Committer time.
    pub commit_time: i64,
    pub subject: String,
    pub body: String,
    pub parents: usize,
}

/// `git log` over `range` (e.g. `v1.0..main`), newest first, at most
/// `max` commits. `first_parent` follows only the mainline (merge commits
/// stand for the pull requests they merged).
pub fn log(
    dir: &Path,
    range: &str,
    max: usize,
    first_parent: bool,
    with_body: bool,
    commands: &mut Vec<String>,
) -> Result<Vec<LogLine>> {
    let format = if with_body {
        "--format=%x1e%H%x1f%an%x1f%at%x1f%ct%x1f%P%x1f%s%x1f%b"
    } else {
        "--format=%x1e%H%x1f%an%x1f%at%x1f%ct%x1f%P%x1f%s"
    };
    let max_arg = format!("--max-count={max}");
    let mut args = vec!["log", format, max_arg.as_str()];
    if first_parent {
        args.push("--first-parent");
    }
    args.push(range);
    args.push("--");
    let out = git(dir, &args, commands)?;
    Ok(parse_log(&out))
}

pub(crate) fn parse_log(out: &str) -> Vec<LogLine> {
    out.split(RS)
        .filter(|r| !r.trim().is_empty())
        .filter_map(|r| {
            let mut f = r.splitn(7, US);
            let sha = f.next()?.trim().to_owned();
            if sha.len() < 7 {
                return None;
            }
            let author = f.next()?.to_owned();
            let author_time = f.next()?.trim().parse().ok()?;
            let commit_time = f.next()?.trim().parse().ok()?;
            let parents = f.next()?.split_whitespace().count();
            let subject = f.next().unwrap_or("").trim().to_owned();
            let body = f.next().unwrap_or("").trim().to_owned();
            Some(LogLine {
                sha,
                author,
                author_time,
                commit_time,
                subject,
                body,
                parents,
            })
        })
        .collect()
}

/// An error for a revision that does not resolve.
pub(crate) fn not_found(rev: &str) -> Error {
    Error::new(format!(
        "{rev} is not a commit in this repository (fetch if it exists on the remote)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_log_records() {
        let out = format!(
            "{RS}aaaaaaaa{US}Ada{US}100{US}200{US}p1 p2{US}Merge pull request #3 from x/y{US}Add things\n\nmore{RS}bbbbbbbb{US}Bob{US}50{US}60{US}p0{US}fix: crash (#7){US}\n"
        );
        let l = parse_log(&out);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].sha, "aaaaaaaa");
        assert_eq!(l[0].parents, 2);
        assert_eq!(l[0].body, "Add things\n\nmore");
        assert_eq!(l[1].author_time, 50);
        assert_eq!(l[1].commit_time, 60);
        assert_eq!(l[1].subject, "fix: crash (#7)");
        assert_eq!(l[1].body, "");
    }

    #[test]
    fn tally_summarises() {
        let mut commands = Vec::new();
        let mut t = Tally::new("git merge-base --is-ancestor");
        for i in 0..10 {
            t.record(format!("c{i}"), &mut commands);
        }
        t.finish(&mut commands);
        assert_eq!(commands.len(), Tally::SHOWN + 1);
        assert_eq!(
            commands.last().unwrap(),
            "… and 4 more git merge-base --is-ancestor"
        );
    }
}
