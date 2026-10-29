//! Lost work: commits (and dropped stashes) no ref reaches, with the
//! context needed to bring them back.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use super::{Progress, git_ok, git_stdin};
use crate::{Error, Oid, Result, cli};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LostKind {
    /// Ordinary commit (amended away, reset, deleted branch…).
    Commit,
    /// A stash commit (`WIP on …` / `On branch:` with 2–3 parents).
    Stash,
    /// Merge commit.
    Merge,
}

impl LostKind {
    pub fn label(self) -> &'static str {
        match self {
            LostKind::Commit => "commit",
            LostKind::Stash => "stash",
            LostKind::Merge => "merge",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LostCommit {
    pub id: Oid,
    pub time: i64,
    pub author: String,
    pub summary: String,
    pub parents: Vec<Oid>,
    pub kind: LostKind,
    /// Reflog entry that still names it (`HEAD@{3}: commit (amend): …`),
    /// if any. Entries without one are only found by `fsck`.
    pub reflog: Option<String>,
    /// Files changed against the first parent (top 30) and the total.
    pub files: Vec<String>,
    pub files_total: u32,
    /// Branch name to restore it as (`recovered/<short>-<slug>`).
    pub suggested_branch: String,
    /// Another lost commit has this one as parent (part of a chain).
    pub has_child: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Lost {
    /// Newest first.
    pub commits: Vec<LostCommit>,
    pub stashes: usize,
    pub blobs: usize,
    pub trees: usize,
    /// Commits found through the reflog only (would survive `gc` until the
    /// reflog expires) vs truly dangling.
    pub in_reflog: usize,
    pub dangling: usize,
    pub elapsed_ms: u64,
    pub cancelled: bool,
    pub commands: Vec<String>,
    /// `gc.reflogExpire` / `gc.reflogExpireUnreachable` as configured or
    /// their defaults (90 / 30 days): when lost commits disappear.
    pub reflog_expire: String,
    pub reflog_expire_unreachable: String,
}

/// Newest lost commits whose changed files are listed.
const FILES_CAP: usize = 200;
/// Files kept per commit (the total is kept separately).
const FILES_PER_COMMIT: usize = 30;

/// `git fsck --unreachable --no-reflogs --connectivity-only` → commit
/// objects, detailed through one `git log --no-walk --stdin`, matched
/// against `git reflog --all`, with the changed files of the newest 200.
pub fn lost(
    workdir: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<Lost> {
    let started = Instant::now();
    let mut commands = Vec::new();
    let mut out = Lost {
        reflog_expire: "90 days".to_owned(),
        reflog_expire_unreachable: "30 days".to_owned(),
        ..Default::default()
    };
    let finish = |mut out: Lost, commands: Vec<String>, cancel: &AtomicBool| {
        out.cancelled = cancel.load(Ordering::Relaxed);
        out.commands = commands;
        out.elapsed_ms = started.elapsed().as_millis() as u64;
        Ok(out)
    };

    // 1. Unreachable objects, streamed (fsck prints them as it goes).
    progress(&Progress {
        phase: "Finding unreachable objects".into(),
        ..Default::default()
    });
    let args = [
        "fsck",
        "--unreachable",
        "--no-reflogs",
        "--connectivity-only",
        "--no-progress",
    ];
    commands.push(format!("git {}", args.join(" ")));
    let mut child = cli::spawn(workdir, &args)?;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut shas: Vec<String> = Vec::new();
    let mut seen = 0usize;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else {
            break;
        };
        let mut p = line.split_whitespace();
        // `unreachable commit <sha>`, `dangling commit <sha>`; `notice:`
        // and progress lines are skipped.
        if !matches!(p.next(), Some("unreachable" | "dangling")) {
            continue;
        }
        match (p.next(), p.next()) {
            (Some("commit"), Some(sha)) => shas.push(sha.to_owned()),
            (Some("blob"), Some(_)) => out.blobs += 1,
            (Some("tree"), Some(_)) => out.trees += 1,
            _ => {}
        }
        seen += 1;
        if seen.is_multiple_of(256) && cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            break;
        }
    }
    let status = child
        .wait()
        .map_err(|e| Error::new(format!("git fsck failed: {e}")))?;
    if cancel.load(Ordering::Relaxed) {
        return finish(out, commands, cancel);
    }
    if !status.success() && shas.is_empty() && out.blobs == 0 && out.trees == 0 {
        return Err(Error::new(format!("git fsck exited with {status}")));
    }
    if shas.is_empty() {
        read_expiry(workdir, &mut out, &mut commands);
        return finish(out, commands, cancel);
    }

    // 2. One `git log` for the details.
    progress(&Progress {
        phase: "Reading lost commits".into(),
        total: Some(shas.len()),
        ..Default::default()
    });
    let stdin = shas.join("\n") + "\n";
    let text = git_stdin(
        workdir,
        &[
            "log",
            "--no-walk=unsorted",
            "--stdin",
            "--format=%H%x1f%P%x1f%an%x1f%at%x1f%s%x1f%b%x1e",
        ],
        stdin.as_bytes(),
        &format!("{} sha(s) on stdin", shas.len()),
        &mut commands,
    )?;
    let mut commits: Vec<LostCommit> = String::from_utf8_lossy(&text)
        .split('\x1e')
        .filter_map(parse_commit)
        .collect();
    // Stash internals: the index (`index on …`) and untracked-files
    // commits are parents of a stash, not work of their own.
    let internals: HashSet<Oid> = commits
        .iter()
        .filter(|c| c.kind == LostKind::Stash)
        .flat_map(|c| c.parents.iter().skip(1).copied())
        .collect();
    commits.retain(|c| {
        !(internals.contains(&c.id)
            && (c.summary.starts_with("index on ") || c.summary.starts_with("untracked files on ")))
    });
    commits.sort_by(|a, b| b.time.cmp(&a.time).then_with(|| a.id.cmp(&b.id)));
    let parents: HashSet<Oid> = commits
        .iter()
        .flat_map(|c| c.parents.iter().copied())
        .collect();
    for c in &mut commits {
        c.has_child = parents.contains(&c.id);
        c.suggested_branch = suggested_branch(c.id, &c.summary);
    }
    if cancel.load(Ordering::Relaxed) {
        out.commits = commits;
        return finish(out, commands, cancel);
    }

    // 3. Which of them the reflog still names.
    progress(&Progress {
        phase: "Reading the reflog".into(),
        ..Default::default()
    });
    let reflog = git_ok(
        workdir,
        &["reflog", "show", "--all", "--format=%H%x1f%gd%x1f%gs"],
        &mut commands,
    );
    let mut named: HashMap<Oid, String> = HashMap::new();
    for line in reflog.lines() {
        let mut p = line.split('\x1f');
        let (Some(sha), Some(selector), Some(subject)) = (p.next(), p.next(), p.next()) else {
            continue;
        };
        let Ok(id) = Oid::from_str(sha.trim()) else {
            continue;
        };
        named
            .entry(id)
            .or_insert_with(|| format!("{}: {}", selector.trim(), subject.trim()));
    }
    for c in &mut commits {
        c.reflog = named.get(&c.id).cloned();
    }
    if cancel.load(Ordering::Relaxed) {
        out.commits = commits;
        return finish(out, commands, cancel);
    }

    // 4. Changed files of the newest ones.
    progress(&Progress {
        phase: "Listing changed files".into(),
        total: Some(commits.len().min(FILES_CAP)),
        ..Default::default()
    });
    let newest: Vec<String> = commits
        .iter()
        .take(FILES_CAP)
        .map(|c| c.id.to_string())
        .collect();
    let files = changed_files(workdir, &newest, &mut commands).unwrap_or_else(|| {
        changed_files_one_by_one(workdir, &commits[..newest.len()], cancel, &mut commands)
    });
    for c in &mut commits {
        if let Some(list) = files.get(&c.id) {
            c.files_total = list.len() as u32;
            c.files = list.iter().take(FILES_PER_COMMIT).cloned().collect();
        }
    }

    read_expiry(workdir, &mut out, &mut commands);
    out.stashes = commits.iter().filter(|c| c.kind == LostKind::Stash).count();
    out.in_reflog = commits.iter().filter(|c| c.reflog.is_some()).count();
    out.dangling = commits.len() - out.in_reflog;
    out.commits = commits;
    finish(out, commands, cancel)
}

/// One `%x1e`-terminated record of the detail log.
fn parse_commit(record: &str) -> Option<LostCommit> {
    let record = record.trim_start_matches(['\n', '\r']);
    let mut f = record.splitn(6, '\x1f');
    let id = Oid::from_str(f.next()?.trim()).ok()?;
    let parents: Vec<Oid> = f
        .next()?
        .split_whitespace()
        .filter_map(|p| Oid::from_str(p).ok())
        .collect();
    let author = f.next()?.to_owned();
    let time = f.next()?.trim().parse().unwrap_or(0);
    let summary = f.next()?.trim().to_owned();
    let kind = kind_of(&summary, parents.len());
    Some(LostCommit {
        id,
        time,
        author,
        summary,
        parents,
        kind,
        reflog: None,
        files: Vec::new(),
        files_total: 0,
        suggested_branch: String::new(),
        has_child: false,
    })
}

/// `WIP on main: …`, `On main: my message`, `index on main: …`.
fn is_stash_summary(summary: &str) -> bool {
    summary.starts_with("WIP on ")
        || summary.starts_with("index on ")
        || (summary.starts_with("On ") && summary.contains(": "))
}

fn kind_of(summary: &str, parents: usize) -> LostKind {
    if parents >= 2 {
        if is_stash_summary(summary) {
            LostKind::Stash
        } else {
            LostKind::Merge
        }
    } else {
        LostKind::Commit
    }
}

/// Changed files of many commits in one `git log --no-walk --name-only`;
/// merges (stashes) diff against their first parent, root commits list
/// their whole tree. `None` when the git in use lacks `--diff-merges`.
fn changed_files(
    workdir: &Path,
    shas: &[String],
    commands: &mut Vec<String>,
) -> Option<HashMap<Oid, Vec<String>>> {
    if shas.is_empty() {
        return Some(HashMap::new());
    }
    let stdin = shas.join("\n") + "\n";
    let text = git_stdin(
        workdir,
        &[
            "-c",
            "core.quotepath=false",
            "log",
            "--no-walk=unsorted",
            "--stdin",
            "--name-only",
            "--diff-merges=first-parent",
            "--format=%x1e%H",
        ],
        stdin.as_bytes(),
        &format!("{} sha(s) on stdin", shas.len()),
        commands,
    )
    .ok()?;
    let mut files = HashMap::new();
    for record in String::from_utf8_lossy(&text).split('\x1e') {
        let mut lines = record
            .lines()
            .map(|l| l.trim_end_matches('\r'))
            .filter(|l| !l.is_empty());
        let Some(id) = lines.next().and_then(|l| Oid::from_str(l.trim()).ok()) else {
            continue;
        };
        files.insert(id, lines.map(str::to_owned).collect());
    }
    Some(files)
}

/// Fallback: one `diff-tree` per commit (against the first parent for
/// stashes), cancel checked every 20.
fn changed_files_one_by_one(
    workdir: &Path,
    commits: &[LostCommit],
    cancel: &AtomicBool,
    commands: &mut Vec<String>,
) -> HashMap<Oid, Vec<String>> {
    let mut files = HashMap::new();
    for (i, c) in commits.iter().enumerate() {
        if i % 20 == 19 && cancel.load(Ordering::Relaxed) {
            break;
        }
        let sha = c.id.to_string();
        let text = match (c.kind, c.parents.first()) {
            (LostKind::Stash, Some(parent)) => {
                let parent = parent.to_string();
                git_ok(
                    workdir,
                    &[
                        "-c",
                        "core.quotepath=false",
                        "diff-tree",
                        "-r",
                        "--name-only",
                        &parent,
                        &sha,
                    ],
                    commands,
                )
            }
            _ => git_ok(
                workdir,
                &[
                    "-c",
                    "core.quotepath=false",
                    "diff-tree",
                    "--no-commit-id",
                    "--name-only",
                    "-r",
                    "--root",
                    &sha,
                ],
                commands,
            ),
        };
        files.insert(
            c.id,
            text.lines()
                .map(|l| l.trim_end_matches('\r'))
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect(),
        );
    }
    files
}

fn read_expiry(workdir: &Path, out: &mut Lost, commands: &mut Vec<String>) {
    for (key, slot) in [
        ("gc.reflogExpire", &mut out.reflog_expire),
        (
            "gc.reflogExpireUnreachable",
            &mut out.reflog_expire_unreachable,
        ),
    ] {
        let value = git_ok(workdir, &["config", "--get", key], commands);
        let value = value.trim();
        if !value.is_empty() {
            *slot = value.to_owned();
        }
    }
}

/// `recovered/<short id>-<slug of the summary>`.
pub fn suggested_branch(id: Oid, summary: &str) -> String {
    let short: String = id.to_string().chars().take(7).collect();
    let slug: String = summary
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .take(4)
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        format!("recovered/{short}")
    } else {
        format!("recovered/{short}-{slug}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stash_detection() {
        assert!(is_stash_summary("WIP on main: 1234567 last commit"));
        assert!(is_stash_summary("On feature: my saved work"));
        assert!(is_stash_summary("index on main: 1234567 last commit"));
        assert!(!is_stash_summary("On second thought, revert"));
        assert!(!is_stash_summary("Merge branch 'x'"));
        assert_eq!(kind_of("WIP on main: 1234567 x", 2), LostKind::Stash);
        assert_eq!(kind_of("WIP on main: 1234567 x", 3), LostKind::Stash);
        assert_eq!(kind_of("Merge branch 'x'", 2), LostKind::Merge);
        assert_eq!(kind_of("WIP on main: 1234567 x", 1), LostKind::Commit);
        assert_eq!(kind_of("fix: thing", 1), LostKind::Commit);
        assert_eq!(LostKind::Stash.label(), "stash");
    }

    #[test]
    fn suggested_branch_names() {
        let id = Oid::from_str("0123456789abcdef0123456789abcdef01234567").unwrap();
        assert_eq!(
            suggested_branch(id, "Fix: the Login page (again) for v2"),
            "recovered/0123456-fix-the-login-page"
        );
        assert_eq!(suggested_branch(id, "!!!"), "recovered/0123456");
        assert_eq!(suggested_branch(id, ""), "recovered/0123456");
    }

    #[test]
    fn parses_detail_records() {
        let rec = "\n0123456789abcdef0123456789abcdef01234567\x1faaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\x1fAda\x1f1700000000\x1fWIP on main: aaaaaaa thing\x1fbody\nmore\n";
        let c = parse_commit(rec).expect("parses");
        assert_eq!(c.parents.len(), 2);
        assert_eq!(c.kind, LostKind::Stash);
        assert_eq!(c.author, "Ada");
        assert_eq!(c.time, 1_700_000_000);
        assert_eq!(c.summary, "WIP on main: aaaaaaa thing");
        assert!(parse_commit("").is_none());
        let root = "0123456789abcdef0123456789abcdef01234567\x1f\x1fAda\x1f5\x1fRoot\x1f";
        let c = parse_commit(root).expect("parses");
        assert!(c.parents.is_empty());
        assert_eq!(c.kind, LostKind::Commit);
    }
}
