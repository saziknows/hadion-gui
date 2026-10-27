//! The gitvis secret check as a git `pre-commit` hook: scan the added
//! lines of the staged diff (and the names of newly staged files) with the
//! Exposure page's rules, print the findings redacted (never the secret) to
//! standard error with file:line and rule, and fail when a high or critical
//! one is staged. Findings dismissed in gitvis are listed in
//! `<git dir>/gitvis-dismissed` (fingerprints only) and do not block.

use std::collections::HashSet;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use crate::secrets::{
    self, Collector, LineSink, Place, Severity, check_path, fingerprint, walk_patch,
};

/// The staged diff, with fixed prefixes and unquoted paths whatever the
/// user's settings.
pub const DIFF_ARGS: [&str; 12] = [
    "-c",
    "core.quotepath=false",
    "-c",
    "diff.noprefix=false",
    "-c",
    "diff.mnemonicPrefix=false",
    "diff",
    "--cached",
    "-U0",
    "--no-color",
    "--no-renames",
    "--no-ext-diff",
];

/// File (in the common git dir) with the fingerprints of dismissed findings.
pub const DISMISSED_FILE: &str = "gitvis-dismissed";

/// One staged finding, already redacted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedFinding {
    pub severity: Severity,
    pub rule: &'static str,
    pub name: &'static str,
    pub path: String,
    /// 0 for a sensitive file name.
    pub line: u32,
    /// The secret with its middle hidden; empty for a file name rule.
    pub redacted: String,
    pub fingerprint: String,
    /// A sensitive file name (`id_rsa`, `.env`) rather than content.
    pub file_rule: bool,
}

impl StagedFinding {
    pub fn blocks(&self) -> bool {
        self.severity >= Severity::High
    }

    pub fn location(&self) -> String {
        if self.line > 0 {
            format!("{}:{}", self.path, self.line)
        } else {
            self.path.clone()
        }
    }
}

/// What the scan saw.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StagedScan {
    pub findings: Vec<StagedFinding>,
    pub files: usize,
    pub lines: u64,
    pub elapsed_ms: u64,
}

/// Scan what is staged in `dir`.
pub fn scan_staged(dir: &Path) -> Result<StagedScan, String> {
    let started = Instant::now();
    let rules = secrets::rules();
    let out = crate::cli::run(dir, &DIFF_ARGS, None).map_err(|e| e.to_string())?;
    let files = out
        .stdout
        .split(|&b| b == b'\n')
        .filter(|l| l.starts_with(b"diff --git "))
        .count();
    let mut collector = Collector::new(rules);
    let cancel = AtomicBool::new(false);
    let (lines, new_files) = {
        let mut sink = LineSink {
            collector: &mut collector,
            place: Place::Staged,
            current: None,
            commits: 0,
            lines: 0,
            new_files: Vec::new(),
            report: None,
        };
        walk_patch(BufReader::new(&out.stdout[..]), &cancel, &mut sink)
            .map_err(|e| e.to_string())?;
        (sink.lines, sink.new_files)
    };
    let mut findings = Vec::new();
    for f in collector.finish(None, &HashSet::new(), false) {
        for o in f.occurrences.iter().filter(|o| o.place == Place::Staged) {
            findings.push(StagedFinding {
                severity: f.severity,
                rule: f.rule,
                name: f.name,
                // Names with spaces carry the diff header's trailing tab.
                path: o.path.trim_end_matches('\t').to_owned(),
                line: o.line,
                redacted: f.redacted(),
                fingerprint: f.fingerprint.clone(),
                file_rule: false,
            });
        }
    }
    for (path, _) in new_files {
        let path = path.trim_end_matches('\t').to_owned();
        if let Some(rule) = check_path(&path) {
            findings.push(StagedFinding {
                severity: rule.severity,
                rule: rule.id,
                name: rule.name,
                fingerprint: fingerprint(rule.id, &path),
                path,
                line: 0,
                redacted: String::new(),
                file_rule: true,
            });
        }
    }
    findings.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
    Ok(StagedScan {
        findings,
        files,
        lines,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// The common git dir of `dir` (shared by worktrees, like the hooks).
pub fn common_dir(dir: &Path) -> Option<PathBuf> {
    let out = crate::cli::run(dir, &["rev-parse", "--git-common-dir"], None).ok()?;
    let printed = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!printed.is_empty()).then(|| super::resolve_in(dir, &printed))
}

/// Fingerprints in a dismissed-findings file (`#` comments allowed).
pub fn parse_dismissed(text: &str) -> HashSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split_whitespace().next().unwrap_or(l).to_owned())
        .collect()
}

/// The dismissed-findings file for the hook.
pub fn dismissed_text(fingerprints: &[String]) -> String {
    let mut v: Vec<&String> = fingerprints.iter().collect();
    v.sort();
    v.dedup();
    let mut s = String::from(
        "# Findings dismissed in gitvis (Exposure page): the pre-commit secret check\n# does not block on these. Fingerprints only; no secret is stored here.\n",
    );
    for f in v {
        s.push_str(f);
        s.push('\n');
    }
    s
}

/// Write the dismissed fingerprints next to the hooks.
pub fn write_dismissed(common: &Path, fingerprints: &[String]) -> Result<PathBuf, String> {
    let path = common.join(DISMISSED_FILE);
    std::fs::write(&path, dismissed_text(fingerprints))
        .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    Ok(path)
}

/// Read the dismissed fingerprints (empty when there is no file).
pub fn read_dismissed(common: &Path) -> HashSet<String> {
    std::fs::read_to_string(common.join(DISMISSED_FILE))
        .map(|t| parse_dismissed(&t))
        .unwrap_or_default()
}

fn pad(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n >= w {
        s.to_owned()
    } else {
        format!("{s}{}", " ".repeat(w - n))
    }
}

/// The text printed to standard error and the hook's exit code (1 when a
/// high or critical finding that is not dismissed is staged).
pub fn report(scan: &StagedScan, dismissed: &HashSet<String>) -> (String, i32) {
    let (skipped, shown): (Vec<&StagedFinding>, Vec<&StagedFinding>) = scan
        .findings
        .iter()
        .partition(|f| dismissed.contains(&f.fingerprint));
    let files = if scan.files == 1 {
        "1 staged file".to_owned()
    } else {
        format!("{} staged files", scan.files)
    };
    let skipped_note = if skipped.is_empty() {
        String::new()
    } else {
        format!(
            ", {} dismissed in gitvis",
            if skipped.len() == 1 {
                "1 finding".to_owned()
            } else {
                format!("{} findings", skipped.len())
            }
        )
    };
    if shown.is_empty() {
        return (
            format!(
                "gitvis secret check: no secrets in {files} ({} ms{skipped_note}).\n",
                scan.elapsed_ms
            ),
            0,
        );
    }
    let blocking = shown.iter().any(|f| f.blocks());
    let mut s = format!(
        "gitvis secret check: {} in {files}{skipped_note}\n\n",
        if shown.len() == 1 {
            "1 finding".to_owned()
        } else {
            format!("{} findings", shown.len())
        }
    );
    let name_w = shown
        .iter()
        .map(|f| f.name.chars().count())
        .max()
        .unwrap_or(0)
        .min(34);
    let loc_w = shown
        .iter()
        .map(|f| f.location().chars().count())
        .max()
        .unwrap_or(0)
        .min(48);
    for f in &shown {
        let what = if f.file_rule {
            "(sensitive file name)".to_owned()
        } else {
            f.redacted.clone()
        };
        s.push_str(&format!(
            "  {}  {}  {}  {}\n",
            pad(f.severity.label(), 8),
            pad(f.name, name_w),
            pad(&f.location(), loc_w),
            what
        ));
    }
    s.push('\n');
    if blocking {
        s.push_str(
            "The commit was stopped: a high or critical secret is staged.\n\
  - Remove it from the file and stage the file again (git add FILE),\n\
    or unstage the file: git restore --staged FILE\n\
  - If the secret was real, rotate it: it may be in other places already.\n\
  - A false positive? Dismiss it on gitvis's Exposure page, then update the\n\
    hook on Tools > Hooks, or skip the check once: git commit --no-verify\n",
        );
        (s, 1)
    } else {
        s.push_str("Nothing high or critical: the commit goes ahead.\n");
        (s, 0)
    }
}

/// The whole hook: scan `dir`'s staged changes, honour the dismissed
/// findings, return (stderr text, exit code). A scan error does not block
/// the commit (exit 0 with the error printed).
pub fn run(dir: &Path) -> (String, i32) {
    let dismissed = common_dir(dir)
        .map(|c| read_dismissed(&c))
        .unwrap_or_default();
    match scan_staged(dir) {
        Ok(scan) => report(&scan, &dismissed),
        Err(e) => (
            format!(
                "gitvis secret check: could not scan the staged changes ({e}); the commit goes ahead.\n"
            ),
            0,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(sev: Severity, line: u32, fp: &str) -> StagedFinding {
        StagedFinding {
            severity: sev,
            rule: "github-pat",
            name: "GitHub token",
            path: "config/.env".into(),
            line,
            redacted: "ghp_••••Ba4".into(),
            fingerprint: fp.into(),
            file_rule: false,
        }
    }

    #[test]
    fn report_blocks_on_high() {
        let scan = StagedScan {
            findings: vec![
                finding(Severity::Critical, 3, "a"),
                finding(Severity::Low, 9, "b"),
            ],
            files: 2,
            lines: 10,
            elapsed_ms: 12,
        };
        let (text, code) = report(&scan, &HashSet::new());
        assert_eq!(code, 1);
        assert!(text.contains("2 findings in 2 staged files"));
        assert!(text.contains("config/.env:3"));
        assert!(text.contains("ghp_••••Ba4"));
        assert!(text.contains("git commit --no-verify"));
        // Dismissing the critical one lets the commit through.
        let dismissed: HashSet<String> = ["a".to_owned()].into();
        let (text, code) = report(&scan, &dismissed);
        assert_eq!(code, 0);
        assert!(text.contains("1 finding dismissed in gitvis"));
        assert!(text.contains("goes ahead"));
    }

    #[test]
    fn clean_report() {
        let scan = StagedScan {
            files: 1,
            elapsed_ms: 5,
            ..Default::default()
        };
        let (text, code) = report(&scan, &HashSet::new());
        assert_eq!(code, 0);
        assert_eq!(
            text,
            "gitvis secret check: no secrets in 1 staged file (5 ms).\n"
        );
    }

    #[test]
    fn dismissed_file_round_trip() {
        let t = dismissed_text(&["b".into(), "a".into(), "a".into()]);
        assert!(t.starts_with('#'));
        let set = parse_dismissed(&t);
        assert_eq!(set.len(), 2);
        assert!(set.contains("a") && set.contains("b"));
    }
}
