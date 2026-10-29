//! Several commits picked in the graph: their order (oldest first, the
//! order git applies them in), line totals, and the commands of the
//! actions on them: one `git cherry-pick A B C`, a numbered series of
//! `git format-patch`, and the marks of an interactive rebase (squash them
//! together, reword them, drop them).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use super::git_stdin;
use crate::devops::workbench::history::split_rename;
use crate::devops::workbench::rebase::{Plan, Step};
use crate::{Commit, Oid};

/// Commits whose line counts are read for the totals.
pub const MAX_TOTALS: usize = 500;

/// The picked commits in graph order, oldest (highest index) first; ids
/// not in the graph go last.
pub fn oldest_first(picked: &[Oid], index_of: &HashMap<Oid, usize>) -> Vec<Oid> {
    let mut v: Vec<Oid> = picked.to_vec();
    v.sort_by_key(|id| std::cmp::Reverse(index_of.get(id).copied().unwrap_or(0)));
    let mut seen = HashSet::new();
    v.retain(|id| seen.insert(*id));
    v
}

/// `git cherry-pick A B C` (oldest first): git applies them in order and,
/// on a conflict, stops with the rest queued (`--continue` / `--abort`).
pub fn cherry_pick_args(oldest_first: &[String]) -> Vec<String> {
    let mut v = vec!["cherry-pick".to_owned()];
    v.extend(oldest_first.iter().cloned());
    v
}

/// One `git format-patch` per commit, numbered in order so the files sort
/// as the series: `0001-…`, `0002-…`.
pub fn format_patch_commands(out_dir: &Path, oldest_first: &[String]) -> Vec<Vec<String>> {
    oldest_first
        .iter()
        .enumerate()
        .map(|(i, sha)| {
            vec![
                "format-patch".into(),
                "-o".into(),
                out_dir.display().to_string(),
                "--start-number".into(),
                (i + 1).to_string(),
                "-1".into(),
                sha.clone(),
            ]
        })
        .collect()
}

/// Where patch files go when no folder was chosen (scripts, tests).
pub fn default_patch_dir(first_short: &str) -> PathBuf {
    std::env::temp_dir()
        .join("hadion-patches")
        .join(first_short)
}

/// Line counts of the picked commits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub commits: usize,
    /// Distinct paths touched.
    pub files: usize,
    pub added: u64,
    pub deleted: u64,
    /// Per commit: (files, added, deleted).
    pub per_commit: HashMap<String, (usize, u64, u64)>,
    /// Only the first [`MAX_TOTALS`] commits were read.
    pub capped: bool,
}

pub fn totals_args() -> Vec<String> {
    vec![
        "-c".into(),
        "core.quotepath=false".into(),
        "log".into(),
        "--no-walk=unsorted".into(),
        "--stdin".into(),
        "--numstat".into(),
        "-M".into(),
        "--format=%x1e%H".into(),
    ]
}

/// Parse `--format=%x1e%H --numstat` output.
pub fn parse_totals(text: &str) -> Totals {
    let mut t = Totals::default();
    let mut paths: HashSet<String> = HashSet::new();
    for rec in text.split('\u{1e}') {
        let mut lines = rec.lines();
        let Some(sha) = lines.next().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let mut c = (0usize, 0u64, 0u64);
        for l in lines {
            let mut p = l.splitn(3, '\t');
            let (Some(a), Some(d), Some(path)) = (p.next(), p.next(), p.next()) else {
                continue;
            };
            let (_, new) = split_rename(path);
            paths.insert(new);
            c.0 += 1;
            c.1 += a.parse::<u64>().unwrap_or(0);
            c.2 += d.parse::<u64>().unwrap_or(0);
        }
        t.added += c.1;
        t.deleted += c.2;
        t.commits += 1;
        t.per_commit.insert(sha.to_owned(), c);
    }
    t.files = paths.len();
    t
}

/// Read the totals of up to [`MAX_TOTALS`] commits.
pub fn totals(dir: &Path, shas: &[String], cmds: &mut Vec<String>) -> Result<Totals, String> {
    let capped = shas.len() > MAX_TOTALS;
    let take = &shas[..shas.len().min(MAX_TOTALS)];
    let mut input = take.join("\n");
    input.push('\n');
    let out = git_stdin(dir, &totals_args(), input.as_bytes(), cmds)?;
    let mut t = parse_totals(&String::from_utf8_lossy(&out));
    t.capped = capped;
    Ok(t)
}

/// Are all `targets` ancestors of (or equal to) `head`, walking the loaded
/// commits? Commits outside the graph count as not reachable.
pub fn reachable_from(
    head: Oid,
    targets: &[Oid],
    commits: &[Commit],
    index_of: &HashMap<Oid, usize>,
) -> bool {
    reachable(head, targets, commits, index_of).len()
        == targets.iter().collect::<HashSet<_>>().len()
}

/// Which of `targets` are ancestors of (or equal to) `head`.
pub fn reachable(
    head: Oid,
    targets: &[Oid],
    commits: &[Commit],
    index_of: &HashMap<Oid, usize>,
) -> HashSet<Oid> {
    let mut want: HashSet<Oid> = targets.iter().copied().collect();
    let mut found = HashSet::new();
    if want.is_empty() {
        return found;
    }
    // Nothing older than the oldest target can lead to one.
    let floor = targets
        .iter()
        .filter_map(|t| index_of.get(t))
        .max()
        .copied()
        .unwrap_or(usize::MAX);
    let mut seen: HashSet<Oid> = HashSet::new();
    let mut queue = VecDeque::from([head]);
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        if want.remove(&id) {
            found.insert(id);
        }
        if want.is_empty() {
            break;
        }
        let Some(&i) = index_of.get(&id) else {
            continue;
        };
        if i > floor {
            continue;
        }
        for p in &commits[i].parent_ids {
            queue.push_back(*p);
        }
    }
    found
}

/// What to do with the picked commits in an interactive rebase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RebaseAction {
    /// Fold the newer ones into the oldest (moved next to it).
    Squash,
    /// Edit each message.
    Reword,
    /// Remove them.
    Drop,
}

impl RebaseAction {
    pub fn label(self) -> &'static str {
        match self {
            RebaseAction::Squash => "Squash into one",
            RebaseAction::Reword => "Reword",
            RebaseAction::Drop => "Drop",
        }
    }

    pub fn from_name(s: &str) -> Option<RebaseAction> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "squash" => RebaseAction::Squash,
            "reword" => RebaseAction::Reword,
            "drop" => RebaseAction::Drop,
            _ => return None,
        })
    }
}

/// The marks for `action` on commits given oldest first.
pub fn rebase_marks(oldest_first: &[String], action: RebaseAction) -> Vec<(String, Step)> {
    oldest_first
        .iter()
        .enumerate()
        .map(|(i, sha)| {
            let step = match action {
                RebaseAction::Squash if i == 0 => Step::Pick,
                RebaseAction::Squash => Step::Squash,
                RebaseAction::Reword => Step::Reword,
                RebaseAction::Drop => Step::Drop,
            };
            (sha.clone(), step)
        })
        .collect()
}

/// Apply marks to a freshly loaded plan: set each commit's step; a folding
/// step (squash, fixup) also moves the commit right after the previous
/// marked one, so non-adjacent commits fold together. Returns the marks
/// whose commit is not in the plan (merges, or outside the range).
pub fn apply_marks(plan: &mut Plan, marks: &[(String, Step)]) -> Vec<String> {
    let mut missing = Vec::new();
    let mut anchor: Option<usize> = None;
    for (sha, step) in marks {
        let Some(i) = plan.rows.iter().position(|r| r.commit.sha == *sha) else {
            missing.push(sha.clone());
            continue;
        };
        let at = match anchor {
            Some(a) if step.folds() => {
                let to = if i > a { a + 1 } else { a };
                plan.move_row(i, to);
                to
            }
            _ => i,
        };
        plan.set_step(at, *step);
        anchor = Some(at);
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::workbench::rebase::RebaseCommit;

    fn oid(n: u8) -> Oid {
        Oid::from_bytes(&[n; 20]).unwrap()
    }

    fn commit(n: u8, parents: &[u8]) -> Commit {
        Commit {
            id: oid(n),
            short: String::new(),
            parent_ids: parents.iter().map(|p| oid(*p)).collect(),
            summary: String::new(),
            author: String::new(),
            email: String::new(),
            time: 0,
        }
    }

    fn rc(sha: &str) -> RebaseCommit {
        RebaseCommit {
            sha: sha.into(),
            short: sha.into(),
            subject: format!("subject {sha}"),
            message: format!("subject {sha}"),
            author: String::new(),
            email: String::new(),
            time: 0,
            files: 0,
            added: 0,
            deleted: 0,
            pushed: false,
        }
    }

    #[test]
    fn order_and_commands() {
        let index: HashMap<Oid, usize> = [(oid(1), 0), (oid(2), 5), (oid(3), 2)].into();
        assert_eq!(
            oldest_first(&[oid(1), oid(2), oid(3), oid(2)], &index),
            vec![oid(2), oid(3), oid(1)]
        );
        let shas = vec!["aaa".to_owned(), "bbb".to_owned()];
        assert_eq!(cherry_pick_args(&shas), vec!["cherry-pick", "aaa", "bbb"]);
        let p = format_patch_commands(Path::new("C:/out"), &shas);
        assert_eq!(p.len(), 2);
        assert_eq!(
            p[1],
            vec![
                "format-patch",
                "-o",
                "C:/out",
                "--start-number",
                "2",
                "-1",
                "bbb"
            ]
        );
    }

    #[test]
    fn totals_parse() {
        let text = "\u{1e}aaa\n\n3\t1\tsrc/a.rs\n-\t-\tlogo.png\n\u{1e}bbb\n\n2\t2\tsrc/{old => new}/b.rs\n1\t0\tsrc/a.rs\n\u{1e}ccc\n";
        let t = parse_totals(text);
        assert_eq!(t.commits, 3);
        assert_eq!(t.files, 3);
        assert_eq!((t.added, t.deleted), (6, 3));
        assert_eq!(t.per_commit["aaa"], (2, 3, 1));
        assert_eq!(t.per_commit["ccc"], (0, 0, 0));
    }

    #[test]
    fn ancestry() {
        // 5 (head) -> 4 -> 2 -> 1 ; 3 -> 1 (side branch)
        let commits = vec![
            commit(5, &[4]),
            commit(4, &[2]),
            commit(3, &[1]),
            commit(2, &[1]),
            commit(1, &[]),
        ];
        let index: HashMap<Oid, usize> =
            commits.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
        assert!(reachable_from(oid(5), &[oid(4), oid(1)], &commits, &index));
        assert!(!reachable_from(oid(5), &[oid(3)], &commits, &index));
        assert!(reachable_from(oid(5), &[], &commits, &index));
        assert!(reachable_from(oid(3), &[oid(3)], &commits, &index));
        let r = reachable(oid(5), &[oid(3), oid(2), oid(9)], &commits, &index);
        assert_eq!(r, [oid(2)].into_iter().collect());
    }

    #[test]
    fn marks_and_plan() {
        let shas = vec!["c1".to_owned(), "c3".to_owned(), "c5".to_owned()];
        assert_eq!(
            rebase_marks(&shas, RebaseAction::Squash),
            vec![
                ("c1".to_owned(), Step::Pick),
                ("c3".to_owned(), Step::Squash),
                ("c5".to_owned(), Step::Squash)
            ]
        );
        assert!(
            rebase_marks(&shas, RebaseAction::Drop)
                .iter()
                .all(|(_, s)| *s == Step::Drop)
        );
        let mut plan = Plan::new(
            ["c1", "c2", "c3", "c4", "c5"]
                .iter()
                .map(|s| rc(s))
                .collect(),
        );
        let missing = apply_marks(&mut plan, &rebase_marks(&shas, RebaseAction::Squash));
        assert!(missing.is_empty());
        let order: Vec<&str> = plan.rows.iter().map(|r| r.commit.sha.as_str()).collect();
        assert_eq!(order, vec!["c1", "c3", "c5", "c2", "c4"]);
        let steps: Vec<Step> = plan.rows.iter().map(|r| r.step).collect();
        assert_eq!(
            steps,
            vec![
                Step::Pick,
                Step::Squash,
                Step::Squash,
                Step::Pick,
                Step::Pick
            ]
        );
        assert_eq!(plan.groups()[0].members, vec![1, 2]);

        let mut plan = Plan::new(["c1", "c2", "c3"].iter().map(|s| rc(s)).collect());
        let missing = apply_marks(
            &mut plan,
            &rebase_marks(&["c2".into(), "zz".into()], RebaseAction::Reword),
        );
        assert_eq!(missing, vec!["zz".to_owned()]);
        assert_eq!(plan.rows[1].step, Step::Reword);
        assert_eq!(
            RebaseAction::from_name("SQUASH"),
            Some(RebaseAction::Squash)
        );
    }
}
