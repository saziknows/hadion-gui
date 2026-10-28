//! Graph filters: which commits match an author, a path, a date range and
//! a message text.
//!
//! Author and dates are matched in memory against the loaded commits
//! (instant, even for 50,000 commits). Path and message need git: one
//! `git log --all --format=%H [-i -F --grep=TEXT] [-- PATH]`, streamed,
//! capped at [`MAX_IDS`] and cancellable, whose commit set the in-memory
//! match is then intersected with.

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use super::{Streamed, git_line};
use crate::{Commit, Oid};

/// Most commit ids a path / message query keeps.
pub const MAX_IDS: usize = 250_000;

/// What the user filters by (persisted per repository).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Spec {
    /// Part of the author's name or e-mail (case-insensitive).
    pub author: String,
    /// A file or folder; commits that touched it.
    pub path: String,
    /// `YYYY-MM-DD`, inclusive.
    pub since: String,
    /// `YYYY-MM-DD`, inclusive (the whole day).
    pub until: String,
    /// Text in the commit message (case-insensitive, literal).
    pub message: String,
}

impl Spec {
    pub fn is_active(&self) -> bool {
        !self.author.trim().is_empty()
            || !self.path.trim().is_empty()
            || !self.since.trim().is_empty()
            || !self.until.trim().is_empty()
            || !self.message.trim().is_empty()
    }

    /// Path or message: needs a git query.
    pub fn needs_git(&self) -> bool {
        !self.path.trim().is_empty() || !self.message.trim().is_empty()
    }

    /// The part of the spec the git query depends on.
    pub fn git_key(&self) -> (String, String) {
        (
            self.path.trim().replace('\\', "/"),
            self.message.trim().to_owned(),
        )
    }

    /// How many filters are set.
    pub fn count(&self) -> usize {
        [
            &self.author,
            &self.path,
            &self.since,
            &self.until,
            &self.message,
        ]
        .iter()
        .filter(|s| !s.trim().is_empty())
        .count()
    }

    /// `author "ann" · path src/ · since 2026-01-01`.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        let mut add = |k: &str, v: &str| {
            if !v.trim().is_empty() {
                parts.push(format!("{k} {}", v.trim()));
            }
        };
        add("author", &self.author);
        add("path", &self.path);
        add("since", &self.since);
        add("until", &self.until);
        add("message", &self.message);
        parts.join(" · ")
    }
}

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

/// `YYYY-MM-DD` (also `YYYY/MM/DD`, `YYYY-MM`, `YYYY`) → the epoch second
/// of local midnight of the first day it covers, and of the day after the
/// last one. `offset` is the local UTC offset in seconds. Empty → `None`.
pub fn parse_date(text: &str, offset: i64) -> Result<Option<(i64, i64)>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    let bad = || format!("`{t}` is not a date: use YYYY-MM-DD, e.g. 2026-03-31.");
    let parts: Vec<&str> = t.split(['-', '/', '.']).collect();
    let num = |s: &str| s.trim().parse::<i64>().map_err(|_| bad());
    let y = num(parts[0])?;
    if !(1970..=2200).contains(&y) || parts.len() > 3 {
        return Err(bad());
    }
    let (m1, m2) = match parts.get(1) {
        Some(m) => {
            let m = num(m)?;
            if !(1..=12).contains(&m) {
                return Err(bad());
            }
            (m as u32, m as u32)
        }
        None => (1, 12),
    };
    let (d1, d2) = match parts.get(2) {
        Some(d) => {
            let d = num(d)?;
            if d < 1 || d > days_in_month(y, m1) as i64 {
                return Err(bad());
            }
            (d as u32, d as u32)
        }
        None => (1, days_in_month(y, m2)),
    };
    let start = days_from_civil(y, m1, d1) * 86_400 - offset;
    let end = (days_from_civil(y, m2, d2) + 1) * 86_400 - offset;
    Ok(Some((start, end)))
}

/// The in-memory part of a spec, ready to test commits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Local {
    author: String,
    /// Inclusive lower bound.
    since: Option<i64>,
    /// Exclusive upper bound.
    until: Option<i64>,
}

impl Local {
    pub fn matches(&self, c: &Commit) -> bool {
        if let Some(s) = self.since
            && c.time < s
        {
            return false;
        }
        if let Some(u) = self.until
            && c.time >= u
        {
            return false;
        }
        if !self.author.is_empty()
            && !c.author.to_lowercase().contains(&self.author)
            && !c.email.to_lowercase().contains(&self.author)
        {
            return false;
        }
        true
    }

    pub fn is_empty(&self) -> bool {
        self.author.is_empty() && self.since.is_none() && self.until.is_none()
    }
}

/// Errors per field: (since, until).
pub fn compile(spec: &Spec, offset: i64) -> (Local, Option<String>, Option<String>) {
    let (since, since_err) = match parse_date(&spec.since, offset) {
        Ok(r) => (r.map(|(s, _)| s), None),
        Err(e) => (None, Some(e)),
    };
    let (until, until_err) = match parse_date(&spec.until, offset) {
        Ok(r) => (r.map(|(_, e)| e), None),
        Err(e) => (None, Some(e)),
    };
    (
        Local {
            author: spec.author.trim().to_lowercase(),
            since,
            until,
        },
        since_err,
        until_err,
    )
}

/// Per commit index: does it match? `set` is the git query's result (path,
/// message), `None` when the spec does not need one.
pub fn mask(commits: &[Commit], local: &Local, set: Option<&HashSet<Oid>>) -> Vec<bool> {
    commits
        .iter()
        .map(|c| local.matches(c) && set.is_none_or(|s| s.contains(&c.id)))
        .collect()
}

/// Distinct authors of the loaded commits, most commits first:
/// `(name, e-mail, commits)`.
pub fn authors(commits: &[Commit]) -> Vec<(String, String, usize)> {
    let mut by: std::collections::HashMap<(String, String), usize> = Default::default();
    for c in commits {
        *by.entry((c.author.clone(), c.email.to_lowercase()))
            .or_insert(0) += 1;
    }
    let mut v: Vec<(String, String, usize)> = by.into_iter().map(|((n, e), k)| (n, e, k)).collect();
    v.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    v
}

/// The `git log` arguments of the path / message query.
pub fn log_args(spec: &Spec) -> Vec<String> {
    let (path, message) = spec.git_key();
    let mut v: Vec<String> = vec!["log".into(), "--all".into(), "--format=%H".into()];
    if !message.is_empty() {
        v.push("-i".into());
        v.push("-F".into());
        v.push(format!("--grep={message}"));
    }
    if !path.is_empty() {
        v.push("--".into());
        v.push(path);
    }
    v
}

/// The commits a git query found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuerySet {
    pub ids: HashSet<Oid>,
    /// [`MAX_IDS`] was reached.
    pub truncated: bool,
    pub elapsed_ms: u64,
    pub command: String,
}

/// Parse `--format=%H` lines.
pub fn parse_ids(text: &str) -> HashSet<Oid> {
    text.lines()
        .filter_map(|l| Oid::from_str(l.trim()).ok())
        .collect()
}

/// Stream the query, stopping at `cap` ids or when `cancel` is set
/// (`Err("cancelled")`).
pub fn query(
    dir: &Path,
    spec: &Spec,
    cap: usize,
    cancel: &AtomicBool,
    cmds: &mut Vec<String>,
) -> Result<QuerySet, String> {
    let started = Instant::now();
    let args = log_args(spec);
    let command = git_line(&args);
    cmds.push(command.clone());
    let mut proc = Streamed::start(dir, &args)?;
    let stdout = proc.child.stdout.take().ok_or("no output")?;
    let mut ids = HashSet::new();
    let mut truncated = false;
    let mut killed = false;
    for line in BufReader::new(stdout).lines() {
        if cancel.load(Ordering::Relaxed) {
            proc.kill();
            let _ = proc.finish(true);
            return Err("cancelled".into());
        }
        let Ok(line) = line else { break };
        if let Ok(id) = Oid::from_str(line.trim()) {
            ids.insert(id);
        }
        if ids.len() >= cap {
            truncated = true;
            killed = true;
            proc.kill();
            break;
        }
    }
    proc.finish(killed)?;
    Ok(QuerySet {
        ids,
        truncated,
        elapsed_ms: started.elapsed().as_millis() as u64,
        command,
    })
}

/// Rows (in order) of the matching commits; `wip` shifts by one.
pub fn rows(mask: &[bool], wip: bool) -> Vec<usize> {
    mask.iter()
        .enumerate()
        .filter(|(_, m)| **m)
        .map(|(i, _)| i + wip as usize)
        .collect()
}

/// The next (or previous) row in `rows` after `from`, wrapping around.
pub fn step(rows: &[usize], from: Option<usize>, forward: bool) -> Option<usize> {
    if rows.is_empty() {
        return None;
    }
    let Some(from) = from else {
        return Some(if forward {
            rows[0]
        } else {
            rows[rows.len() - 1]
        });
    };
    if forward {
        rows.iter().copied().find(|r| *r > from).or(Some(rows[0]))
    } else {
        rows.iter()
            .rev()
            .copied()
            .find(|r| *r < from)
            .or(Some(rows[rows.len() - 1]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(n: u8, author: &str, email: &str, time: i64) -> Commit {
        Commit {
            id: Oid::from_bytes(&[n; 20]).unwrap(),
            short: String::new(),
            parent_ids: Vec::new(),
            summary: String::new(),
            author: author.into(),
            email: email.into(),
            time,
        }
    }

    #[test]
    fn dates() {
        // 2026-03-31 00:00 UTC = 1774915200
        assert_eq!(
            parse_date("2026-03-31", 0).unwrap(),
            Some((1_774_915_200, 1_774_915_200 + 86_400))
        );
        // Local midnight two hours east of UTC is two hours earlier.
        assert_eq!(
            parse_date("2026-03-31", 7200).unwrap().unwrap().0,
            1_774_915_200 - 7200
        );
        let (s, e) = parse_date("2024-02", 0).unwrap().unwrap();
        assert_eq!((e - s) / 86_400, 29, "leap February");
        let (s, e) = parse_date("2025", 0).unwrap().unwrap();
        assert_eq!((e - s) / 86_400, 365);
        assert_eq!(parse_date("  ", 0).unwrap(), None);
        assert!(parse_date("2026-02-30", 0).is_err());
        assert!(parse_date("yesterday", 0).is_err());
        assert!(parse_date("2026-13-01", 0).is_err());
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn local_matching_and_sets() {
        let day = 86_400;
        let commits = vec![
            commit(1, "Ann Lee", "ann@example.com", 10 * day),
            commit(2, "Bob", "bob@corp.example", 11 * day + 5),
            commit(3, "ann", "Ann@Other.example", 12 * day),
        ];
        let spec = Spec {
            author: "ANN".into(),
            ..Default::default()
        };
        let (l, _, _) = compile(&spec, 0);
        assert_eq!(mask(&commits, &l, None), vec![true, false, true]);

        let spec = Spec {
            since: "1970-01-12".into(),
            until: "1970-01-12".into(),
            ..Default::default()
        };
        let (l, a, b) = compile(&spec, 0);
        assert!(a.is_none() && b.is_none());
        assert_eq!(mask(&commits, &l, None), vec![false, true, false]);

        let spec = Spec {
            author: "corp".into(),
            ..Default::default()
        };
        let (l, _, _) = compile(&spec, 0);
        assert_eq!(mask(&commits, &l, None), vec![false, true, false]);

        let set: HashSet<Oid> = [commits[0].id, commits[1].id].into_iter().collect();
        let (l, _, _) = compile(&Spec::default(), 0);
        assert!(l.is_empty());
        let m = mask(&commits, &l, Some(&set));
        assert_eq!(m, vec![true, true, false]);
        assert_eq!(rows(&m, true), vec![1, 2]);
        assert_eq!(rows(&m, false), vec![0, 1]);

        let (_, se, _) = compile(
            &Spec {
                since: "soon".into(),
                ..Default::default()
            },
            0,
        );
        assert!(se.is_some());
    }

    #[test]
    fn query_args_and_ids() {
        let spec = Spec {
            path: "src\\views ".into(),
            message: " fix #12 ".into(),
            ..Default::default()
        };
        assert!(spec.needs_git());
        assert_eq!(spec.count(), 2);
        assert_eq!(
            log_args(&spec),
            vec![
                "log",
                "--all",
                "--format=%H",
                "-i",
                "-F",
                "--grep=fix #12",
                "--",
                "src/views"
            ]
        );
        assert_eq!(spec.describe(), "path src\\views · message fix #12");
        let ids = parse_ids(
            "0123456789012345678901234567890123456789\nnot a sha\n\n89abcdef0123456789abcdef0123456789abcdef\n",
        );
        assert_eq!(ids.len(), 2);
        assert!(!Spec::default().is_active());
    }

    #[test]
    fn authors_by_count() {
        let commits = vec![
            commit(1, "Ann", "ann@x", 0),
            commit(2, "Bob", "bob@x", 0),
            commit(3, "Ann", "ANN@x", 0),
        ];
        let a = authors(&commits);
        assert_eq!(a[0], ("Ann".into(), "ann@x".into(), 2));
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn stepping() {
        let rows = vec![2, 5, 9];
        assert_eq!(step(&rows, Some(5), true), Some(9));
        assert_eq!(step(&rows, Some(9), true), Some(2), "wraps");
        assert_eq!(step(&rows, Some(5), false), Some(2));
        assert_eq!(step(&rows, Some(2), false), Some(9), "wraps back");
        assert_eq!(step(&rows, Some(3), true), Some(5), "from a non-match");
        assert_eq!(step(&rows, None, true), Some(2));
        assert_eq!(step(&rows, None, false), Some(9));
        assert_eq!(step(&[], Some(1), true), None);
    }
}
