//! Runs the exposure scanner from the command line and prints the report:
//!
//! ```text
//! cargo run --release -p gitvis-core --example scan -- <repo> [--history N] [--all-refs] [--show N] [--reveal]
//! ```
//!
//! `--history N` also reads the newest N commits (0 = all). Secrets are
//! redacted unless `--reveal` is given.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use gitvis_core::secrets::{self, HistoryScope, ScanOptions, Severity};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut path: Option<PathBuf> = None;
    let mut history: Option<usize> = None;
    let mut all_refs = false;
    let mut show = 40usize;
    let mut reveal = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--history" => history = Some(args.next().and_then(|n| n.parse().ok()).unwrap_or(2000)),
            "--all-refs" => all_refs = true,
            "--show" => show = args.next().and_then(|n| n.parse().ok()).unwrap_or(40),
            "--reveal" => reveal = true,
            other => path = Some(other.into()),
        }
    }
    let path = path.unwrap_or_else(|| ".".into());
    let opts = ScanOptions {
        worktree: true,
        history: history.map(|max_commits| HistoryScope {
            max_commits,
            all_refs,
        }),
    };
    let cancel = AtomicBool::new(false);
    let t = Instant::now();
    let mut last = String::new();
    let report = secrets::scan(&path, &opts, &cancel, &mut |p| {
        let line = format!(
            "{} {}{}  {} findings",
            p.phase,
            p.done,
            p.total.map(|t| format!("/{t}")).unwrap_or_default(),
            p.findings
        );
        if line != last {
            eprintln!("[{:6.1}s] {line}", t.elapsed().as_secs_f32());
            last = line;
        }
    })
    .expect("scan");
    let s = &report.stats;
    println!(
        "{} findings in {:.1}s: {} critical, {} high, {} medium, {} low ({} secrets, {} files, {} config)",
        report.total(),
        t.elapsed().as_secs_f32(),
        report.count(Severity::Critical),
        report.count(Severity::High),
        report.count(Severity::Medium),
        report.count(Severity::Low),
        report.findings.len(),
        report.files.len(),
        report.config.len()
    );
    println!(
        "files: {} listed, {} read, {} binary, {} large, {} skipped kinds, {:.1} MB, {} lines; history: {} commits{}; remotes: {}, pushed commits: {}, head pushed: {}",
        s.files_listed,
        s.files_scanned,
        s.skipped_binary,
        s.skipped_large,
        s.skipped_kind,
        s.bytes as f64 / 1e6,
        s.lines,
        s.commits,
        s.commits_total
            .map(|t| format!(" of {t}"))
            .unwrap_or_default(),
        s.remotes,
        s.pushed_commits,
        s.head_pushed
    );
    let mut by_rule: Vec<(&str, usize)> = Vec::new();
    for f in &report.findings {
        match by_rule.iter_mut().find(|(n, _)| *n == f.rule) {
            Some(e) => e.1 += 1,
            None => by_rule.push((f.rule, 1)),
        }
    }
    by_rule.sort_by_key(|e| std::cmp::Reverse(e.1));
    println!("by rule:");
    for (r, n) in &by_rule {
        println!("  {n:5}  {r}");
    }
    println!("findings (most severe first):");
    for f in report.findings.iter().take(show) {
        let o = &f.occurrences[0];
        println!(
            "  [{}] {} · {} · {}:{} · {}{}{}{}{} · {} occ · {} commits",
            f.severity.label(),
            f.name,
            if reveal {
                f.secret.clone()
            } else {
                f.redacted()
            },
            o.path,
            o.line,
            if f.at_head { "HEAD " } else { "" },
            if f.in_worktree { "worktree " } else { "" },
            if f.staged { "staged " } else { "" },
            if f.history_only() {
                "history-only "
            } else {
                ""
            },
            if f.pushed { "pushed" } else { "local" },
            f.occurrence_count,
            f.commit_count
        );
        println!("        {}", o.text.trim());
    }
    if report.findings.len() > show {
        println!("  … {} more", report.findings.len() - show);
    }
    for f in &report.files {
        println!(
            "  [{}] {} · {}{}{}{}",
            f.severity.label(),
            f.rule.name,
            f.path,
            if f.tracked { " · tracked" } else { "" },
            if f.untracked { " · untracked" } else { "" },
            if f.pushed { " · pushed" } else { "" }
        );
    }
    for c in &report.config {
        println!("  [{}] {}", c.severity.label(), c.title);
    }
    println!("commands:");
    for c in &report.commands {
        println!("  {c}");
    }
}
