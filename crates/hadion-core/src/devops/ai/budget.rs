//! Fitting large inputs into the size budget: diffs file by file with a
//! per-file cap (the rest summarised as a stat line), and CI logs as the
//! error lines with their context.

use crate::joblog::{JobLog, LogKind};

/// One file's section of a unified diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub text: String,
    pub added: usize,
    pub removed: usize,
    pub binary: bool,
}

/// Split `git diff` output at its `diff --git` headers.
pub fn split_diff(diff: &str) -> Vec<FileDiff> {
    let mut out: Vec<FileDiff> = Vec::new();
    for raw in diff.split_inclusive('\n') {
        if let Some(rest) = raw.strip_prefix("diff --git ") {
            let rest = rest.trim_end();
            let path = match rest.rfind(" b/") {
                Some(i) => rest[i + 3..].trim_matches('"').to_owned(),
                None => rest.to_owned(),
            };
            out.push(FileDiff {
                path,
                text: String::new(),
                added: 0,
                removed: 0,
                binary: false,
            });
        }
        let Some(f) = out.last_mut() else {
            continue; // text before the first header
        };
        if raw.starts_with('+') && !raw.starts_with("+++ ") {
            f.added += 1;
        } else if raw.starts_with('-') && !raw.starts_with("--- ") {
            f.removed += 1;
        } else if raw.starts_with("Binary files ") || raw.starts_with("GIT binary patch") {
            f.binary = true;
        }
        f.text.push_str(raw);
    }
    out
}

/// Lock files and generated bundles: sent as a stat line only.
pub fn is_generated(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    matches!(
        name.as_str(),
        "cargo.lock"
            | "package-lock.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "bun.lockb"
            | "composer.lock"
            | "gemfile.lock"
            | "poetry.lock"
            | "uv.lock"
            | "go.sum"
            | "flake.lock"
            | "packages.lock.json"
    ) || name.ends_with(".min.js")
        || name.ends_with(".min.css")
        || name.ends_with(".map")
        || name.ends_with(".snap")
}

/// The part of a diff that fits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fitted {
    pub text: String,
    /// Files whose changes are in `text` (fully or cut).
    pub included: Vec<String>,
    /// Files cut at the per-file cap.
    pub cut: Vec<String>,
    /// Files summarised in the stat line only.
    pub omitted: Vec<String>,
    pub files_total: usize,
    pub bytes_sent: usize,
    pub bytes_total: usize,
}

impl Fitted {
    pub fn complete(&self) -> bool {
        self.cut.is_empty() && self.omitted.is_empty()
    }

    /// `12 of 48 files, 30 KB of 210 KB; the rest summarised as a stat line`.
    pub fn summary(&self) -> String {
        if self.files_total == 0 {
            return "no changes".to_owned();
        }
        if self.complete() {
            return format!(
                "all {} ({})",
                plural(self.files_total, "file", "files"),
                super::super::plural_bytes(self.bytes_sent)
            );
        }
        let mut s = format!(
            "{} of {} files, {} of {}",
            self.included.len(),
            self.files_total,
            super::super::plural_bytes(self.bytes_sent),
            super::super::plural_bytes(self.bytes_total)
        );
        if !self.cut.is_empty() {
            s.push_str(&format!(
                "; {} cut at the per-file cap",
                plural(self.cut.len(), "file", "files")
            ));
        }
        if !self.omitted.is_empty() {
            s.push_str("; the rest summarised as a stat line");
        }
        s
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Cut a file's diff text at a line boundary to at most `cap` bytes.
fn cut_file(text: &str, cap: usize) -> (String, usize) {
    let mut out = String::new();
    let mut lines_left = 0usize;
    for l in text.split_inclusive('\n') {
        if lines_left > 0 || out.len() + l.len() > cap {
            lines_left += 1;
        } else {
            out.push_str(l);
        }
    }
    (out, lines_left)
}

/// Fit `files` into `budget` bytes: files in order, each cut at
/// `per_file_cap`; lock files and files that no longer fit go into a
/// stat line. `stat` (`git diff --stat`) is always sent by the caller.
pub fn fit_diff(files: &[FileDiff], budget: usize, per_file_cap: usize) -> Fitted {
    let mut f = Fitted {
        files_total: files.len(),
        bytes_total: files.iter().map(|x| x.text.len()).sum(),
        ..Fitted::default()
    };
    let mut used = 0usize;
    for file in files {
        if is_generated(&file.path) || file.binary {
            f.omitted.push(file.path.clone());
            continue;
        }
        let room = budget.saturating_sub(used);
        // Not even the header and one hunk fit: stat line.
        if room < 400.min(file.text.len()) {
            f.omitted.push(file.path.clone());
            continue;
        }
        let cap = per_file_cap.min(room);
        if file.text.len() <= cap {
            f.text.push_str(&file.text);
            used += file.text.len();
        } else {
            let (part, left) = cut_file(&file.text, cap);
            used += part.len();
            f.text.push_str(&part);
            if !f.text.ends_with('\n') {
                f.text.push('\n');
            }
            f.text.push_str(&format!(
                "[… {left} more lines of {} not sent: size budget]\n",
                file.path
            ));
            f.cut.push(file.path.clone());
        }
        f.included.push(file.path.clone());
    }
    if !f.omitted.is_empty() {
        f.text.push_str("\n[Not included, summarised here: ");
        let items: Vec<String> = files
            .iter()
            .filter(|x| f.omitted.contains(&x.path))
            .map(|x| {
                if x.binary {
                    format!("{} (binary)", x.path)
                } else {
                    format!("{} (+{} −{})", x.path, x.added, x.removed)
                }
            })
            .collect();
        f.text.push_str(&items.join(", "));
        f.text.push_str("]\n");
    }
    f.bytes_sent = used;
    f
}

/// Per-file cap for a budget: a third of it, at least 6 KB.
pub fn per_file_cap(budget: usize) -> usize {
    (budget / 3).max(6 * 1024).min(budget)
}

/// Cut free text (a pull request body, a template) to `cap` bytes.
pub fn cut_text(text: &str, cap: usize) -> (String, bool) {
    if text.len() <= cap {
        return (text.to_owned(), false);
    }
    let mut end = cap;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (
        format!(
            "{}\n[… {} more not sent: size budget]",
            &text[..end],
            super::super::plural_bytes(text.len() - end)
        ),
        true,
    )
}

/// The part of a CI log that is sent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogExcerpt {
    pub text: String,
    /// `(first, last)` line numbers (1-based) of each window sent.
    pub windows: Vec<(usize, usize)>,
    pub error_lines: usize,
    pub lines_sent: usize,
    pub lines_total: usize,
    pub bytes_sent: usize,
    pub bytes_total: usize,
}

impl LogExcerpt {
    /// `lines 1,180–1,290 and 5,100–5,231 of 5,231 (3 error lines), 9 KB of 1.2 MB`.
    pub fn summary(&self) -> String {
        let ranges: Vec<String> = self
            .windows
            .iter()
            .map(|(a, b)| format!("{a}–{b}"))
            .collect();
        format!(
            "lines {} of {} ({}), {} of {}",
            ranges.join(", "),
            self.lines_total,
            plural(self.error_lines, "error line", "error lines"),
            super::super::plural_bytes(self.bytes_sent),
            super::super::plural_bytes(self.bytes_total)
        )
    }
}

/// Lines that look like a failure when the runner marked none.
fn looks_like_error(text: &str) -> bool {
    let l = text.to_ascii_lowercase();
    [
        "error",
        "failed",
        "failure",
        "fatal",
        "panicked",
        "exception",
        "traceback",
        "exit code",
        "assertion",
    ]
    .iter()
    .any(|w| l.contains(w))
}

/// The error lines of a log with `context` lines around them, newest
/// failures first when the budget runs out; the last 30 lines are always
/// in. Each line is prefixed with its number so answers can quote it.
pub fn log_excerpt(log: &JobLog, context: usize, budget: usize) -> LogExcerpt {
    let n = log.lines.len();
    let mut ex = LogExcerpt {
        lines_total: n,
        bytes_total: log.bytes,
        ..LogExcerpt::default()
    };
    if n == 0 {
        return ex;
    }
    let mut errors: Vec<usize> = log
        .lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.kind == LogKind::Error)
        .map(|(i, _)| i)
        .collect();
    if errors.is_empty() {
        errors = log
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| looks_like_error(&l.text))
            .map(|(i, _)| i)
            .collect();
    }
    ex.error_lines = errors.len();
    // Windows around the errors, plus the tail, merged.
    let mut windows: Vec<(usize, usize)> = errors
        .iter()
        .map(|&i| (i.saturating_sub(context), (i + context).min(n - 1)))
        .collect();
    windows.push((n.saturating_sub(30), n - 1));
    windows.sort();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for w in windows {
        match merged.last_mut() {
            Some(last) if w.0 <= last.1 + 1 => last.1 = last.1.max(w.1),
            _ => merged.push(w),
        }
    }
    // Newest windows first until the budget is used up.
    let render = |i: usize| -> String {
        let l = &log.lines[i];
        let marker = match l.kind {
            LogKind::Error => "##[error] ",
            LogKind::Warning => "##[warning] ",
            LogKind::Group => "▸ ",
            LogKind::Command => "$ ",
            _ => "",
        };
        let mut text: String = l.text.chars().take(600).collect();
        if l.text.chars().count() > 600 {
            text.push_str(" […]");
        }
        format!("{:>6} | {marker}{text}\n", i + 1)
    };
    let mut chosen: Vec<(usize, usize, String)> = Vec::new();
    let mut used = 0usize;
    for &(a, b) in merged.iter().rev() {
        let mut rows: Vec<String> = Vec::new();
        let mut size = 0usize;
        let mut start = b + 1;
        // Fill from the bottom of the window so the error end is kept.
        for i in (a..=b).rev() {
            let s = render(i);
            if used + size + s.len() > budget {
                break;
            }
            size += s.len();
            rows.push(s);
            start = i;
        }
        if start > b {
            break;
        }
        rows.reverse();
        let block = rows.concat();
        used += size;
        chosen.push((start, b, block));
        if start > a {
            break; // budget exhausted inside this window
        }
    }
    chosen.reverse();
    let mut prev_end: Option<usize> = None;
    for (a, b, block) in chosen {
        if let Some(p) = prev_end
            && a > p + 1
        {
            ex.text
                .push_str(&format!("   … | ({} lines not sent)\n", a - p - 1));
        } else if prev_end.is_none() && a > 0 {
            ex.text
                .push_str(&format!("   … | ({a} earlier lines not sent)\n"));
        }
        ex.text.push_str(&block);
        ex.lines_sent += b - a + 1;
        ex.windows.push((a + 1, b + 1));
        prev_end = Some(b);
    }
    ex.bytes_sent = used;
    ex
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, lines: usize) -> String {
        let mut s = format!(
            "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1,{lines} +1,{lines} @@\n"
        );
        for i in 0..lines {
            s.push_str(&format!("+line {i} of {path} with some text\n"));
        }
        s
    }

    #[test]
    fn splits_and_counts() {
        let d = format!(
            "{}{}diff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\n",
            file("a.rs", 3),
            file("b c.rs", 2)
        );
        let files = split_diff(&d);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, "a.rs");
        assert_eq!(files[0].added, 3);
        assert_eq!(files[1].path, "b c.rs");
        assert!(files[2].binary);
        assert_eq!(files.iter().map(|f| f.text.len()).sum::<usize>(), d.len());
    }

    #[test]
    fn everything_fits() {
        let files = split_diff(&(file("a.rs", 3) + &file("b.rs", 3)));
        let f = fit_diff(&files, 64 * 1024, 20 * 1024);
        assert!(f.complete());
        assert_eq!(f.included, vec!["a.rs", "b.rs"]);
        assert!(f.summary().starts_with("all 2 files"), "{}", f.summary());
        assert!(!f.text.contains("Not included"));
    }

    #[test]
    fn per_file_cap_and_stat_line() {
        let mut d = file("big.rs", 2000); // ~70 KB
        d.push_str(&file("small.rs", 5));
        d.push_str(&file("Cargo.lock", 50));
        for i in 0..20 {
            d.push_str(&file(&format!("f{i}.rs"), 200));
        }
        let files = split_diff(&d);
        let f = fit_diff(&files, 30 * 1024, 10 * 1024);
        assert!(f.cut.contains(&"big.rs".to_owned()));
        assert!(f.included.contains(&"small.rs".to_owned()));
        assert!(f.omitted.contains(&"Cargo.lock".to_owned()));
        assert!(!f.omitted.is_empty());
        assert!(f.bytes_sent <= 30 * 1024);
        assert!(f.text.contains("more lines of big.rs not sent"));
        assert!(f.text.contains("[Not included, summarised here: "));
        assert!(f.text.contains("Cargo.lock (+50 −0)"));
        let s = f.summary();
        assert!(s.contains(&format!(" of {} files, ", files.len())), "{s}");
        assert!(s.contains("the rest summarised as a stat line"), "{s}");
        // The cut is on a line boundary.
        assert!(f.text.lines().all(|l| !l.ends_with("with some te")));
    }

    #[test]
    fn caps() {
        assert_eq!(per_file_cap(64 * 1024), 64 * 1024 / 3);
        assert_eq!(per_file_cap(8 * 1024), 6 * 1024);
        assert_eq!(per_file_cap(4 * 1024), 4 * 1024);
        let (t, cut) = cut_text("héllo wörld", 3);
        assert!(cut);
        assert!(t.starts_with("hé"));
        assert_eq!(cut_text("ok", 10), ("ok".to_owned(), false));
        assert!(is_generated("web/dist/app.min.js"));
        assert!(is_generated("Cargo.lock"));
        assert!(!is_generated("src/lock.rs"));
    }

    fn log(lines: &[&str]) -> JobLog {
        let raw: String = lines
            .iter()
            .map(|l| format!("2026-10-27T10:00:00.0000000Z {l}\n"))
            .collect();
        crate::joblog::parse(raw.as_bytes())
    }

    #[test]
    fn log_windows_around_errors() {
        let mut lines: Vec<String> = (0..500).map(|i| format!("step output {i}")).collect();
        lines[100] = "##[error]Process completed with exit code 1.".to_owned();
        lines[300] = "##[error]Tests failed".to_owned();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let l = log(&refs);
        let ex = log_excerpt(&l, 40, 1_000_000);
        assert_eq!(ex.error_lines, 2);
        assert_eq!(ex.windows, vec![(61, 141), (261, 341), (471, 500)]);
        assert!(ex.text.contains("   101 | ##[error] Process completed"));
        assert!(ex.text.contains("(60 earlier lines not sent)"));
        assert!(ex.text.contains("(119 lines not sent)"));
        assert_eq!(ex.lines_sent, 81 + 81 + 30);
        assert!(
            ex.summary()
                .starts_with("lines 61–141, 261–341, 471–500 of 500 (2 error lines)")
        );
    }

    #[test]
    fn log_budget_keeps_the_newest_failure() {
        let mut lines: Vec<String> = (0..500)
            .map(|i| format!("output line number {i}"))
            .collect();
        lines[100] = "##[error]first".to_owned();
        lines[450] = "##[error]second".to_owned();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let ex = log_excerpt(&log(&refs), 40, 2_000);
        assert!(ex.bytes_sent <= 2_000);
        assert!(ex.text.contains("##[error] second") || ex.text.contains("   500 |"));
        assert!(!ex.text.contains("##[error] first"));
    }

    #[test]
    fn logs_without_markers_use_keywords() {
        let ex = log_excerpt(
            &log(&["compiling", "thread 'main' panicked at src/x.rs", "done"]),
            1,
            10_000,
        );
        assert_eq!(ex.error_lines, 1);
        assert_eq!(ex.windows, vec![(1, 3)]);
        assert_eq!(log_excerpt(&JobLog::default(), 40, 100).lines_total, 0);
    }
}
