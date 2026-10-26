//! Comparing a failed run with the last green run of the same workflow on
//! the same branch: which run that is, the commits in between (local git
//! when both commits are here, else the compare API), and a diff of the
//! two job logs after normalising what always differs (timestamps,
//! durations, ids, temporary paths, dates).

use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use regex::Regex;

use super::model::{Between, CommitLite, CommitsSource, FileLite};
use crate::github::WorkflowRun;
use crate::joblog::{JobLog, LogKind};

/// The newest successful run of the same workflow on the same branch that
/// started before `bad`.
pub fn last_green<'a>(runs: &'a [WorkflowRun], bad: &WorkflowRun) -> Option<&'a WorkflowRun> {
    runs.iter()
        .filter(|r| {
            r.id != bad.id
                && r.workflow_id == bad.workflow_id
                && r.branch == bad.branch
                && r.is_success()
                && match (r.created, bad.created) {
                    (Some(a), Some(b)) => a < b,
                    _ => r.run_number < bad.run_number,
                }
        })
        .max_by_key(|r| (r.created.unwrap_or(0), r.run_number))
}

// ---------------------------------------------------------------------------
// Normalisation
// ---------------------------------------------------------------------------

struct Rules {
    iso: Regex,
    date: Regex,
    time: Regex,
    duration: Regex,
    uuid: Regex,
    hex: Regex,
    temp: Regex,
    long_num: Regex,
}

fn rules() -> &'static Rules {
    static R: OnceLock<Rules> = OnceLock::new();
    R.get_or_init(|| Rules {
        iso: Regex::new(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?")
            .expect("iso"),
        date: Regex::new(r"\b\d{4}[-/]\d{2}[-/]\d{2}\b").expect("date"),
        time: Regex::new(r"\b\d{1,2}:\d{2}:\d{2}(?:[.,]\d+)?\b").expect("time"),
        duration: Regex::new(
            r"(?i)\b\d+(?:\.\d+)?\s?(?:ms|millis(?:econds?)?|µs|us|ns|s|sec(?:onds?)?|secs|m|min(?:utes?)?|mins|h|hours?)\b",
        )
        .expect("duration"),
        uuid: Regex::new(
            r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b",
        )
        .expect("uuid"),
        hex: Regex::new(r"(?i)\b(?:0x)?[0-9a-f]*[0-9][0-9a-f]*[a-f][0-9a-f]*\b|\b(?:0x)?[0-9a-f]*[a-f][0-9a-f]*[0-9][0-9a-f]*\b")
            .expect("hex"),
        temp: Regex::new(
            r#"(?i)(?:/tmp/|/var/folders/|/home/runner/work/_temp/|/runner/_work/_temp/|[a-z]:\\a\\_temp\\|[a-z]:\\users\\[^\\\s]+\\appdata\\local\\temp\\|\$runner_temp/)[^\s'"]*"#,
        )
        .expect("temp"),
        long_num: Regex::new(r"\d{6,}").expect("long number"),
    })
}

/// A log line with what differs between any two runs replaced by
/// placeholders, so a diff shows only real changes. `ids` are values known
/// to differ (run ids, job ids, SHAs) and are replaced first.
pub fn normalize(line: &str, ids: &[String]) -> String {
    let r = rules();
    let mut s = line.trim_end().to_owned();
    for id in ids.iter().filter(|i| i.len() >= 4) {
        if s.contains(id.as_str()) {
            s = s.replace(id.as_str(), "<id>");
        }
    }
    let s = r.temp.replace_all(&s, "<tmp>");
    let s = r.iso.replace_all(&s, "<datetime>");
    let s = r.date.replace_all(&s, "<date>");
    let s = r.time.replace_all(&s, "<time>");
    let s = r.uuid.replace_all(&s, "<uuid>");
    let s = r.duration.replace_all(&s, "<dur>");
    let s = hex_ids(&r.hex, &s);
    let s = r.long_num.replace_all(&s, "<n>");
    s.into_owned()
}

/// Replace hex runs of 7+ characters that mix letters and digits (commit
/// SHAs, digests, object ids); shorter ones (`cafe`, `v1a`) stay.
fn hex_ids<'a>(re: &Regex, s: &'a str) -> std::borrow::Cow<'a, str> {
    re.replace_all(s, |c: &regex::Captures| {
        let m = &c[0];
        let digits = m.trim_start_matches("0x").trim_start_matches("0X");
        if digits.len() >= 7 {
            "<hex>".to_owned()
        } else {
            m.to_owned()
        }
    })
}

// ---------------------------------------------------------------------------
// Log diff
// ---------------------------------------------------------------------------

/// How a line of the diff relates the two logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tag {
    Same,
    /// Only in the green run's log.
    OnlyGood,
    /// Only in the failing run's log.
    OnlyBad,
}

/// One line of a hunk, with the original text (not the normalised one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub tag: Tag,
    /// 1-based line numbers in each log.
    pub good_no: Option<usize>,
    pub bad_no: Option<usize>,
    pub text: String,
    pub kind: LogKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub good_start: usize,
    pub bad_start: usize,
    pub lines: Vec<DiffLine>,
}

/// Where the two logs first stop matching.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Divergence {
    pub good_no: usize,
    pub bad_no: usize,
    /// Matching lines just before (original text of the failing log).
    pub before: Vec<String>,
    pub good_text: Option<String>,
    pub bad_text: Option<String>,
}

/// A line that only the failing run printed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnlyLine {
    pub no: usize,
    pub text: String,
    pub kind: LogKind,
}

impl OnlyLine {
    /// An error line (runner marker or the word in the text).
    pub fn is_error(&self) -> bool {
        self.kind == LogKind::Error
            || looks_like(&self.text, &["error", "failed", "fatal", "panicked"])
    }

    pub fn is_warning(&self) -> bool {
        self.kind == LogKind::Warning || looks_like(&self.text, &["warning", "warn:"])
    }
}

fn looks_like(text: &str, words: &[&str]) -> bool {
    let l = text.to_ascii_lowercase();
    words.iter().any(|w| l.contains(w))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogDiff {
    pub good_lines: usize,
    pub bad_lines: usize,
    /// Lines left out at the start of each log (the "Set up job" step).
    pub good_from: usize,
    pub bad_from: usize,
    /// Lines that differ after normalisation.
    pub changed: usize,
    pub first: Option<Divergence>,
    pub hunks: Vec<Hunk>,
    pub only_bad: Vec<OnlyLine>,
    /// The diff hit its time budget; hunks may be coarser than ideal.
    pub timed_out: bool,
    /// Hunks or only-in-failing lines were cut to the caps.
    pub truncated: bool,
}

/// At most this many hunks and only-in-failing lines are kept.
pub const MAX_HUNKS: usize = 60;
pub const MAX_ONLY: usize = 400;

/// The first line after the runner's "Set up job" preamble (runner and
/// image versions, token permissions, action downloads), which ends with
/// `Complete job name: …`; `None` when the log has no such line near the
/// top.
pub fn setup_end(log: &JobLog) -> Option<usize> {
    log.lines
        .iter()
        .take(400)
        .position(|l| l.text.starts_with("Complete job name:"))
        .map(|i| i + 1)
}

/// Diff two parsed logs line by line after [`normalize`]. `ids` are
/// replaced in both (run ids, job ids, SHAs of the two runs).
pub fn diff_logs(good: &JobLog, bad: &JobLog, ids: &[String], context: usize) -> LogDiff {
    diff_logs_from(good, 0, bad, 0, ids, context)
}

/// [`diff_logs`] from line `good_from` of the green log and `bad_from` of
/// the failing one (to leave out the "Set up job" step, whose runner and
/// image versions differ in every run). Line numbers stay those of the
/// whole logs.
pub fn diff_logs_from(
    good: &JobLog,
    good_from: usize,
    bad: &JobLog,
    bad_from: usize,
    ids: &[String],
    context: usize,
) -> LogDiff {
    let good_from = good_from.min(good.lines.len());
    let bad_from = bad_from.min(bad.lines.len());
    let a: Vec<String> = good.lines[good_from..]
        .iter()
        .map(|l| normalize(&l.text, ids))
        .collect();
    let b: Vec<String> = bad.lines[bad_from..]
        .iter()
        .map(|l| normalize(&l.text, ids))
        .collect();
    // Ops index the slices; shift them back to whole-log indices.
    let (go, bo) = (good_from, bad_from);
    let deadline = Instant::now() + Duration::from_secs(3);
    let ops =
        similar::capture_diff_slices_deadline(similar::Algorithm::Myers, &a, &b, Some(deadline));
    let timed_out = Instant::now() >= deadline;
    let mut changed = 0usize;
    let mut first: Option<Divergence> = None;
    let mut only_bad = Vec::new();
    let mut truncated = false;
    for op in &ops {
        let (tag, old, new) = op.as_tag_tuple();
        if tag == similar::DiffTag::Equal {
            continue;
        }
        let (old, new) = (old.start + go..old.end + go, new.start + bo..new.end + bo);
        changed += old.len().max(new.len());
        if first.is_none() {
            let before: Vec<String> = bad.lines[new.start.saturating_sub(3).max(bo)..new.start]
                .iter()
                .map(|l| l.text.clone())
                .collect();
            first = Some(Divergence {
                good_no: old.start + 1,
                bad_no: new.start + 1,
                before,
                good_text: (!old.is_empty()).then(|| good.lines[old.start].text.clone()),
                bad_text: (!new.is_empty()).then(|| bad.lines[new.start].text.clone()),
            });
        }
        for i in new.clone() {
            if only_bad.len() >= MAX_ONLY {
                truncated = true;
                break;
            }
            let l = &bad.lines[i];
            if l.text.trim().is_empty() {
                continue;
            }
            only_bad.push(OnlyLine {
                no: i + 1,
                text: l.text.clone(),
                kind: l.kind,
            });
        }
    }
    let groups = similar::group_diff_ops(ops, context);
    if groups.len() > MAX_HUNKS {
        truncated = true;
    }
    let hunks = groups
        .into_iter()
        .take(MAX_HUNKS)
        .map(|group| {
            let mut lines = Vec::new();
            let mut good_start = usize::MAX;
            let mut bad_start = usize::MAX;
            for op in group {
                let (tag, old, new) = op.as_tag_tuple();
                let (old, new) = (old.start + go..old.end + go, new.start + bo..new.end + bo);
                good_start = good_start.min(old.start);
                bad_start = bad_start.min(new.start);
                match tag {
                    similar::DiffTag::Equal => {
                        for (o, n) in old.clone().zip(new.clone()) {
                            lines.push(DiffLine {
                                tag: Tag::Same,
                                good_no: Some(o + 1),
                                bad_no: Some(n + 1),
                                text: bad.lines[n].text.clone(),
                                kind: bad.lines[n].kind,
                            });
                        }
                    }
                    _ => {
                        for o in old.clone() {
                            lines.push(DiffLine {
                                tag: Tag::OnlyGood,
                                good_no: Some(o + 1),
                                bad_no: None,
                                text: good.lines[o].text.clone(),
                                kind: good.lines[o].kind,
                            });
                        }
                        for n in new.clone() {
                            lines.push(DiffLine {
                                tag: Tag::OnlyBad,
                                good_no: None,
                                bad_no: Some(n + 1),
                                text: bad.lines[n].text.clone(),
                                kind: bad.lines[n].kind,
                            });
                        }
                    }
                }
            }
            Hunk {
                good_start: good_start.saturating_add(1),
                bad_start: bad_start.saturating_add(1),
                lines,
            }
        })
        .collect();
    LogDiff {
        good_lines: good.lines.len(),
        bad_lines: bad.lines.len(),
        good_from,
        bad_from,
        changed,
        first,
        hunks,
        only_bad,
        timed_out,
        truncated,
    }
}

// ---------------------------------------------------------------------------
// Commits in between, from the local repository
// ---------------------------------------------------------------------------

/// Whether a commit exists in the local object database.
pub fn has_commit(dir: &Path, sha: &str) -> bool {
    !sha.is_empty()
        && crate::cli::run(dir, &["cat-file", "-e", &format!("{sha}^{{commit}}")], None).is_ok()
}

/// `git log good..bad` and `git diff --numstat` / `--name-status`, when
/// both commits are local. `commands` receives the command lines.
pub fn local_between(
    dir: &Path,
    good: &str,
    bad: &str,
    commands: &mut Vec<String>,
) -> crate::Result<Between> {
    let range = format!("{good}..{bad}");
    let fmt = "--format=%H%x1f%an%x1f%at%x1f%s";
    commands.push(format!("git log {fmt} -n 500 {range}"));
    let out = crate::cli::run(dir, &["log", fmt, "-n", "500", &range], None)?;
    let text = String::from_utf8_lossy(&out.stdout);
    let commits: Vec<CommitLite> = text
        .lines()
        .filter_map(|l| {
            let mut p = l.split('\x1f');
            Some(CommitLite {
                sha: p.next()?.to_owned(),
                author: p.next()?.to_owned(),
                time: p.next()?.parse().ok(),
                summary: p.next().unwrap_or("").to_owned(),
            })
        })
        .collect();
    commands.push(format!("git rev-list --count {range}"));
    let total = crate::cli::run(dir, &["rev-list", "--count", &range], None)
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(commits.len() as u64);
    commands.push(format!("git diff --numstat -M {good} {bad}"));
    let num = crate::cli::run(dir, &["diff", "--numstat", "-M", good, bad], None)?;
    commands.push(format!("git diff --name-status -M {good} {bad}"));
    let names = crate::cli::run(dir, &["diff", "--name-status", "-M", good, bad], None)?;
    let files = parse_name_status(
        &String::from_utf8_lossy(&names.stdout),
        &String::from_utf8_lossy(&num.stdout),
    );
    Ok(Between {
        source: CommitsSource::Local,
        commits,
        files,
        total_commits: total,
        status: String::new(),
        behind_by: 0,
    })
}

/// Combine `--name-status` and `--numstat` output into files.
pub fn parse_name_status(names: &str, numstat: &str) -> Vec<FileLite> {
    let counts: Vec<(u64, u64)> = numstat
        .lines()
        .map(|l| {
            let mut p = l.split('\t');
            let a = p.next().and_then(|x| x.parse().ok()).unwrap_or(0);
            let d = p.next().and_then(|x| x.parse().ok()).unwrap_or(0);
            (a, d)
        })
        .collect();
    names
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| {
            let parts: Vec<&str> = l.split('\t').collect();
            let code = parts.first().copied().unwrap_or("");
            let status = match code.chars().next() {
                Some('A') => "added",
                Some('D') => "removed",
                Some('R') => "renamed",
                Some('C') => "copied",
                Some('T') => "changed",
                _ => "modified",
            };
            let (old, path) = if parts.len() >= 3 {
                (Some(parts[1].to_owned()), parts[2].to_owned())
            } else {
                (None, parts.get(1).copied().unwrap_or("").to_owned())
            };
            let (additions, deletions) = counts.get(i).copied().unwrap_or((0, 0));
            FileLite {
                path,
                status: status.to_owned(),
                additions,
                deletions,
                old_path: old,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::ci::history::tests::run;
    use crate::joblog;

    #[test]
    fn picks_the_newest_earlier_green_run() {
        let mut runs = vec![
            run(1, Some("success"), 10, 0),
            run(2, Some("success"), 10, 0),
            run(3, Some("failure"), 10, 0),
            run(4, Some("failure"), 10, 0),
            run(5, Some("success"), 10, 0),
        ];
        runs[1].branch = "dev".into();
        let bad = runs[3].clone();
        assert_eq!(last_green(&runs, &bad).map(|r| r.id), Some(1));
        let mut other = run(6, Some("success"), 10, 0);
        other.workflow_id = 1;
        assert_eq!(last_green(&[other], &bad), None);
        let mut dev_bad = run(7, Some("failure"), 10, 0);
        dev_bad.branch = "dev".into();
        assert_eq!(last_green(&runs, &dev_bad).map(|r| r.id), Some(2));
    }

    #[test]
    fn normalises_what_always_differs() {
        let ids = vec!["40383546623".to_owned()];
        assert_eq!(
            normalize("Finished in 3.2s (run 40383546623)", &ids),
            "Finished in <dur> (run <id>)"
        );
        assert_eq!(
            normalize("2026-10-26T19:05:21.123Z Starting at 19:05:21", &[]),
            "<datetime> Starting at <time>"
        );
        assert_eq!(normalize("took 120 ms", &[]), "took <dur>");
        assert_eq!(
            normalize("HEAD is now at 2d9e7c4 Fix", &[]),
            "HEAD is now at <hex> Fix"
        );
        assert_eq!(
            normalize("digest sha256:9f86d081884c7d659a2feaa0c55ad015", &[]),
            "digest sha256:<hex>"
        );
        assert_eq!(normalize("cafe v1a stays", &[]), "cafe v1a stays");
        assert_eq!(
            normalize("Writing /home/runner/work/_temp/abc123.sh", &[]),
            "Writing <tmp>"
        );
        assert_eq!(
            normalize(r"C:\Users\RUNNER~1\AppData\Local\Temp\xyz.ps1 ran", &[]),
            "<tmp> ran"
        );
        assert_eq!(normalize("/tmp/tmp.XyZ12/file done", &[]), "<tmp> done");
        assert_eq!(normalize("Date: 2026-10-26", &[]), "Date: <date>");
        assert_eq!(
            normalize("id 3f2504e0-4f89-11d3-9a0c-0305e82c3301", &[]),
            "id <uuid>"
        );
        assert_eq!(normalize("Job 123456789 queued", &[]), "Job <n> queued");
        assert_eq!(normalize("exit code 1", &[]), "exit code 1");
    }

    fn log(lines: &[&str]) -> JobLog {
        let raw: String = lines
            .iter()
            .enumerate()
            .map(|(i, l)| format!("2026-10-26T10:00:{:02}.0000000Z {l}\n", i % 60))
            .collect();
        joblog::parse(raw.as_bytes())
    }

    #[test]
    fn diffs_normalised_logs() {
        let good = log(&[
            "Run cargo test",
            "Compiling x v0.1.0",
            "Finished in 3.2s",
            "test a ... ok",
            "test b ... ok",
            "Done",
        ]);
        let bad = log(&[
            "Run cargo test",
            "Compiling x v0.1.0",
            "Finished in 4.9s",
            "test a ... ok",
            "test b ... FAILED",
            "##[error]Process completed with exit code 101.",
            "Done",
        ]);
        let d = diff_logs(&good, &bad, &[], 1);
        assert_eq!((d.good_lines, d.bad_lines), (6, 7));
        let first = d.first.clone().expect("diverges");
        assert_eq!((first.good_no, first.bad_no), (5, 5));
        assert_eq!(first.bad_text.as_deref(), Some("test b ... FAILED"));
        assert_eq!(first.good_text.as_deref(), Some("test b ... ok"));
        assert_eq!(
            first.before,
            ["Compiling x v0.1.0", "Finished in 4.9s", "test a ... ok"]
        );
        assert_eq!(d.only_bad.len(), 2);
        assert!(d.only_bad[0].is_error());
        assert!(d.only_bad[1].is_error());
        assert_eq!(d.only_bad[1].kind, LogKind::Error);
        assert_eq!(d.hunks.len(), 1);
        let h = &d.hunks[0];
        assert_eq!(h.lines.first().unwrap().tag, Tag::Same);
        assert!(
            h.lines
                .iter()
                .any(|l| l.tag == Tag::OnlyGood && l.text == "test b ... ok")
        );
        assert!(
            h.lines
                .iter()
                .any(|l| l.tag == Tag::OnlyBad && l.text == "test b ... FAILED")
        );
        // Same logs (modulo durations) do not differ.
        let same = diff_logs(
            &good,
            &log(&[
                "Run cargo test",
                "Compiling x v0.1.0",
                "Finished in 9.0s",
                "test a ... ok",
                "test b ... ok",
                "Done",
            ]),
            &[],
            3,
        );
        assert_eq!(same.changed, 0);
        assert!(same.first.is_none());
        assert!(same.hunks.is_empty());
    }

    #[test]
    fn diff_can_skip_the_setup_lines() {
        let good = log(&[
            "Current runner version: '2.338.0'",
            "Version: 20261012.601",
            "Run make",
            "ok",
        ]);
        let bad = log(&[
            "Current runner version: '2.339.0'",
            "Version: 20261021.618",
            "Run make",
            "boom",
        ]);
        let all = diff_logs(&good, &bad, &[], 1);
        assert_eq!(all.first.as_ref().map(|f| f.bad_no), Some(1));
        let d = diff_logs_from(&good, 2, &bad, 2, &[], 1);
        let f = d.first.expect("diverges");
        assert_eq!((f.good_no, f.bad_no), (4, 4));
        assert_eq!(f.before, ["Run make"]);
        assert_eq!(d.only_bad.len(), 1);
        assert_eq!(d.only_bad[0].no, 4);
        assert_eq!(d.hunks[0].bad_start, 3);
        assert_eq!((d.good_from, d.bad_from), (2, 2));
        assert_eq!(
            normalize("<job_1656022918> starting", &[]),
            "<job_<n>> starting"
        );
    }

    #[test]
    fn finds_the_end_of_the_setup_step() {
        let l = log(&[
            "Current runner version: '2.338.0'",
            "Version: 20261021.618",
            "Prepare workflow directory",
            "Complete job name: Dependabot",
            "##[group]Run mkdir -p x",
            "done",
        ]);
        assert_eq!(setup_end(&l), Some(4));
        assert_eq!(l.lines[4].text, "Run mkdir -p x");
        assert_eq!(setup_end(&log(&["no preamble here"])), None);
    }

    #[test]
    fn name_status_and_numstat() {
        let names = "M\tsrc/a.rs\nA\tnew.txt\nR087\told.rs\tnewer.rs\nD\tgone\n";
        let num = "3\t1\tsrc/a.rs\n10\t0\tnew.txt\n0\t0\told.rs => newer.rs\n0\t5\tgone\n";
        let f = parse_name_status(names, num);
        assert_eq!(f.len(), 4);
        assert_eq!(
            (f[0].status.as_str(), f[0].additions, f[0].deletions),
            ("modified", 3, 1)
        );
        assert_eq!(f[1].status, "added");
        assert_eq!(f[2].status, "renamed");
        assert_eq!(f[2].old_path.as_deref(), Some("old.rs"));
        assert_eq!(f[2].path, "newer.rs");
        assert_eq!((f[3].status.as_str(), f[3].deletions), ("removed", 5));
    }
}
