//! Gathering what each request sends, through git, then redacting and
//! fitting it to the budget. Every function records the commands it ran.

use std::path::Path;

use super::budget::{self, Fitted};
use super::prompts::{CommitExplainInput, CommitInput};
use super::redact::{self, Redactions};
use crate::cli;

fn git(dir: &Path, args: &[&str], commands: &mut Vec<String>) -> Result<String, String> {
    commands.push(format!("git {}", crate::devops::proc::render_args(args)));
    let out = cli::run(dir, args, None).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A diff after redaction and fitting.
#[derive(Clone, Debug, Default)]
pub struct Material {
    pub text: String,
    pub fitted: Fitted,
    pub redactions: Redactions,
}

/// Redact a diff, then fit it into `budget` bytes.
pub fn prepare_diff(diff: &str, budget: usize) -> Material {
    let (clean, redactions) = redact::diff(diff);
    let files = budget::split_diff(&clean);
    let fitted = budget::fit_diff(&files, budget, budget::per_file_cap(budget));
    Material {
        text: fitted.text.clone(),
        fitted,
        redactions,
    }
}

/// The staged changes for a commit message.
pub fn staged(
    dir: &Path,
    budget: usize,
    commands: &mut Vec<String>,
) -> Result<(CommitInput, Material), String> {
    let branch = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"], commands)
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|_| "(no commits yet)".to_owned());
    let stat = git(dir, &["diff", "--cached", "--stat=120"], commands)?;
    let files = git(dir, &["diff", "--cached", "--name-status"], commands)?;
    if files.trim().is_empty() {
        return Err("Nothing is staged: stage the changes to describe first.".to_owned());
    }
    let diff = git(
        dir,
        &[
            "diff",
            "--cached",
            "-U3",
            "--no-color",
            "--no-ext-diff",
            "--find-renames",
        ],
        commands,
    )?;
    let subjects = git(dir, &["log", "-20", "--format=%s"], commands).unwrap_or_default();
    let material = prepare_diff(&diff, budget);
    let (files_clean, _) = redact::text(&files, "staged-files.txt");
    let input = CommitInput {
        branch,
        stat,
        files: files_clean,
        recent_subjects: subjects.lines().map(str::to_owned).collect(),
        diff: material.text.clone(),
        diff_note: material.fitted.summary(),
    };
    Ok((input, material))
}

/// One commit for "Explain this commit".
pub fn commit(
    dir: &Path,
    sha: &str,
    budget: usize,
    commands: &mut Vec<String>,
) -> Result<(CommitExplainInput, Material), String> {
    if !sha.chars().all(|c| c.is_ascii_hexdigit()) || sha.is_empty() {
        return Err(format!("Not a commit hash: {sha}"));
    }
    let header = git(
        dir,
        &[
            "show",
            "-s",
            "--format=commit %H%nAuthor: %an <%ae>%nDate: %aI%nParents: %P%n%n%B",
            sha,
        ],
        commands,
    )?;
    let stat = git(dir, &["show", "--format=", "--stat=120", sha], commands)?;
    let diff = git(
        dir,
        &[
            "show",
            "--format=",
            "-U3",
            "--no-color",
            "--no-ext-diff",
            "--first-parent",
            sha,
        ],
        commands,
    )?;
    let material = prepare_diff(&diff, budget);
    let (header, mut r) = redact::text(&header, "COMMIT_EDITMSG");
    let mut material = material;
    r.merge(&material.redactions);
    material.redactions = r;
    let input = CommitExplainInput {
        header,
        stat,
        diff: material.text.clone(),
        diff_note: material.fitted.summary(),
    };
    Ok((input, material))
}

/// `(subject, body)` of the commits in `base..head`, oldest first.
pub fn commits_between(
    dir: &Path,
    base: &str,
    head: &str,
    commands: &mut Vec<String>,
) -> Result<Vec<(String, String)>, String> {
    let range = format!("{base}..{head}");
    let text = git(
        dir,
        &[
            "log",
            "--no-merges",
            "--reverse",
            "--max-count=200",
            "--format=%s%x1f%b%x1e",
            &range,
            "--",
        ],
        commands,
    )?;
    Ok(parse_commits(&text))
}

/// `%s%x1f%b%x1e` records.
pub fn parse_commits(text: &str) -> Vec<(String, String)> {
    text.split('\x1e')
        .filter_map(|rec| {
            let rec = rec.trim_start_matches(['\n', '\r']);
            let (s, b) = rec.split_once('\x1f')?;
            let s = s.trim();
            (!s.is_empty()).then(|| (s.to_owned(), b.trim().to_owned()))
        })
        .collect()
}

/// `git diff --stat base...head`.
pub fn stat_between(
    dir: &Path,
    base: &str,
    head: &str,
    commands: &mut Vec<String>,
) -> Result<String, String> {
    git(
        dir,
        &["diff", "--stat=120", &format!("{base}...{head}"), "--"],
        commands,
    )
}

/// The workflow file at `sha` (or in the working tree when the commit is
/// not here).
pub fn workflow_file(
    dir: &Path,
    sha: &str,
    path: &str,
    commands: &mut Vec<String>,
) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    if !sha.is_empty()
        && let Ok(t) = git(dir, &["show", &format!("{sha}:{path}")], commands)
    {
        return Some(t);
    }
    std::fs::read_to_string(dir.join(path)).ok()
}

/// The part of a workflow file about one job: the lines before `jobs:`
/// (name, triggers, env) and the job's own block, matched by its key or
/// its `name:` (a matrix job `build (ubuntu, 3.12)` matches `build`).
pub fn workflow_excerpt(text: &str, job_name: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let Some(jobs_at) = lines.iter().position(|l| l.trim_end() == "jobs:") else {
        return lines
            .iter()
            .take(max_lines)
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
    };
    let base = job_name.split(" (").next().unwrap_or(job_name).trim();
    let base = base.split(" / ").last().unwrap_or(base).trim();
    // Job keys: lines indented once under `jobs:`.
    let indent_of = |l: &str| l.len() - l.trim_start().len();
    let mut starts: Vec<(usize, String)> = Vec::new();
    let mut job_indent: Option<usize> = None;
    for (i, l) in lines.iter().enumerate().skip(jobs_at + 1) {
        if l.trim().is_empty() || l.trim_start().starts_with('#') {
            continue;
        }
        let ind = indent_of(l);
        if ind == 0 {
            break;
        }
        let ji = *job_indent.get_or_insert(ind);
        if ind == ji
            && let Some(key) = l.trim().strip_suffix(':')
        {
            starts.push((i, key.trim_matches(['"', '\'']).to_owned()));
        }
    }
    let end_of = |k: usize| -> usize {
        starts.get(k + 1).map(|s| s.0).unwrap_or_else(|| {
            lines
                .iter()
                .enumerate()
                .skip(starts[k].0 + 1)
                .find(|(_, l)| !l.trim().is_empty() && indent_of(l) == 0)
                .map(|(i, _)| i)
                .unwrap_or(lines.len())
        })
    };
    let found = starts.iter().enumerate().find(|(k, (i, key))| {
        if key.eq_ignore_ascii_case(base) {
            return true;
        }
        let end = end_of(*k);
        lines[*i + 1..end].iter().any(|l| {
            l.trim()
                .strip_prefix("name:")
                .map(|n| {
                    let n = n.trim().trim_matches(['"', '\'']);
                    let n = n.split(" (").next().unwrap_or(n).trim();
                    n.eq_ignore_ascii_case(base)
                        || n.split("${{").next().unwrap_or(n).trim() == base
                })
                .unwrap_or(false)
        })
    });
    let mut out: Vec<&str> = lines[..=jobs_at].to_vec();
    match found {
        Some((k, (i, _))) => {
            out.push("  # … other jobs left out …");
            out.extend_from_slice(&lines[*i..end_of(k)]);
        }
        None => out.extend(lines[jobs_at + 1..].iter().copied()),
    }
    if out.len() > max_lines {
        let n = out.len() - max_lines;
        out.truncate(max_lines);
        let mut s = out.join("\n");
        s.push_str(&format!("\n# … {n} more lines not sent"));
        return s;
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const WF: &str = "name: CI\non:\n  push:\n    branches: [main]\nenv:\n  RUST_LOG: info\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - run: cargo clippy\n  test:\n    name: Tests\n    strategy:\n      matrix:\n        os: [ubuntu, windows]\n    runs-on: ${{ matrix.os }}\n    steps:\n      - uses: actions/checkout@v4\n      - run: cargo test\n";

    #[test]
    fn workflow_job_by_key_and_name() {
        let e = workflow_excerpt(WF, "lint", 200);
        assert!(e.starts_with("name: CI\non:"));
        assert!(e.contains("cargo clippy"));
        assert!(!e.contains("cargo test"));
        let e = workflow_excerpt(WF, "Tests (ubuntu)", 200);
        assert!(e.contains("cargo test"), "{e}");
        assert!(!e.contains("cargo clippy"));
        assert!(e.contains("other jobs left out"));
        // Unknown job: the whole jobs section.
        let e = workflow_excerpt(WF, "deploy", 200);
        assert!(e.contains("cargo clippy") && e.contains("cargo test"));
        // Capped.
        let e = workflow_excerpt(WF, "deploy", 5);
        assert!(e.ends_with("more lines not sent"));
        assert_eq!(workflow_excerpt("a: 1\nb: 2", "x", 1), "a: 1");
    }

    #[test]
    fn commit_records() {
        let text = "Add a\x1fBecause.\n\nMore.\n\x1e\nFix b\x1f\x1e\n";
        let c = parse_commits(text);
        assert_eq!(
            c,
            vec![
                ("Add a".to_owned(), "Because.\n\nMore.".to_owned()),
                ("Fix b".to_owned(), String::new())
            ]
        );
        assert!(parse_commits("").is_empty());
    }

    #[test]
    fn diffs_are_redacted_before_fitting() {
        let key = format!("AKIA{}", "Q7ZK4WJ2MX9PLR3T");
        let d = format!(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n+let k = \"{key}\";\n"
        );
        let m = prepare_diff(&d, 64 * 1024);
        assert!(!m.text.contains(&key));
        assert_eq!(m.redactions.values(), 1);
        assert!(m.fitted.complete());
    }
}
