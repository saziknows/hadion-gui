//! Runs the OSINT analysis from the command line and prints the report:
//!
//! ```text
//! cargo run --release -p hadion-core --example osint -- <repo> [--commits N] [--head-only] [--unreachable]
//! ```

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use hadion_core::osint::{self, EmailKind, OsintOptions};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut path: Option<PathBuf> = None;
    let mut opts = OsintOptions::default();
    let mut unreachable = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--commits" => {
                opts.max_commits = args.next().and_then(|n| n.parse().ok()).unwrap_or(50_000)
            }
            "--head-only" => opts.all_refs = false,
            "--unreachable" => unreachable = true,
            other => path = Some(other.into()),
        }
    }
    let path = path.unwrap_or_else(|| ".".into());
    let cancel = AtomicBool::new(false);
    let t = Instant::now();
    let mut last = String::new();
    let r = osint::analyze(&path, &opts, &cancel, &mut |p| {
        let line = format!(
            "{} {}{}",
            p.phase,
            p.done,
            p.total.map(|t| format!("/{t}")).unwrap_or_default()
        );
        if line != last {
            eprintln!("[{:6.1}s] {line}", t.elapsed().as_secs_f32());
            last = line;
        }
    })
    .expect("analyze");
    let p = &r.people;
    println!(
        "{} commits, {} identities ({} personal, {} corporate, {} noreply, {} invalid) in {:.1}s; local user.email {} ({})",
        p.total_commits,
        p.identities.len(),
        p.count(EmailKind::Personal),
        p.count(EmailKind::Corporate),
        p.count(EmailKind::NoReply),
        p.count(EmailKind::Invalid),
        r.stats.elapsed_ms as f64 / 1000.0,
        p.local_email,
        p.local_scope
    );
    for id in p.identities.iter().take(15) {
        println!(
            "  {:<9} {:<40} {:<24} {:>6} commits {:>6} pushed  peak {:?}  offsets {:?}  aliases {}",
            id.kind().label(),
            id.email,
            id.name(),
            id.commits,
            id.pushed,
            id.peak_hour(),
            id.offsets.iter().take(2).collect::<Vec<_>>(),
            id.aliases.len()
        );
    }
    let m = &r.messages;
    println!(
        "messages: {} trailers kinds, {} ticket projects, {} domains, {} internal urls, {} incidents",
        m.trailers.len(),
        m.tickets.len(),
        m.domains.len(),
        m.internal_urls.len(),
        m.incidents.len()
    );
    for t in m.trailers.iter().take(6) {
        println!("  trailer {} ×{}", t.kind, t.total);
    }
    for (k, n, s) in m.tickets.iter().take(8) {
        println!("  ticket {k} ×{n} {s:?}");
    }
    for inc in m.incidents.iter().take(12) {
        println!(
            "  incident [{}] {} · {}{}",
            inc.reason,
            inc.commit.summary,
            &inc.commit.id.to_string()[..8],
            if inc.pushed { " pushed" } else { "" }
        );
    }
    println!("footprint: {} findings", r.footprint.len());
    let mut by_rule: Vec<(&str, usize)> = Vec::new();
    for f in &r.footprint {
        match by_rule.iter_mut().find(|(n, _)| *n == f.rule) {
            Some(e) => e.1 += 1,
            None => by_rule.push((f.rule, 1)),
        }
    }
    by_rule.sort_by_key(|e| std::cmp::Reverse(e.1));
    for (rule, n) in &by_rule {
        println!("  {n:5}  {rule}");
    }
    for f in r.footprint.iter().take(25) {
        println!(
            "  [{}] {} · {} · {}:{} · {} occ",
            f.severity.label(),
            f.name,
            f.secret,
            f.occurrences[0].path,
            f.occurrences[0].line,
            f.occurrence_count
        );
    }
    println!(
        "metadata: {} files, junk: {}",
        r.metadata.len(),
        r.junk.len()
    );
    for mf in r.metadata.iter().take(15) {
        println!(
            "  [{}] {} {} · {}",
            mf.severity.label(),
            mf.kind,
            mf.path,
            mf.fields
                .iter()
                .take(4)
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" · ")
        );
    }
    for j in r.junk.iter().take(10) {
        println!("  junk {} · {}", j.path, j.reason);
    }
    let a = &r.archaeology;
    println!(
        "archaeology: {} rewritten commits, {} stashes ({} findings), {} backup branches, {} deleted sensitive files, local hooks {:?}",
        a.rewritten_total,
        a.stashes.len(),
        a.stash_findings.len(),
        a.backup_branches.len(),
        a.deleted_files.len(),
        a.local_hooks
    );
    for b in a.backup_branches.iter().take(10) {
        println!(
            "  branch {} · {}{}",
            b.name,
            b.reason,
            if b.remote { " (remote)" } else { "" }
        );
    }
    for d in a.deleted_files.iter().take(10) {
        println!("  deleted {} · {} · {}", d.path, d.rule.name, d.resurrect());
    }
    let s = &r.supply;
    println!(
        "supply: {} git deps, {} packages from {} lockfiles, {} hook notes; codeowners {:?}",
        s.git_deps.len(),
        s.packages.len(),
        s.lockfiles.len(),
        s.hooks.len(),
        r.codeowners.as_ref().map(|c| (c.rules, c.owners.len()))
    );
    for d in s.git_deps.iter().take(10) {
        println!("  gitdep {} ← {} ({}:{})", d.name, d.source, d.file, d.line);
    }
    for h in &s.hooks {
        println!(
            "  hook [{}] {} · {}",
            h.kind,
            h.path,
            h.detail.lines().next().unwrap_or("")
        );
    }
    println!("commands:");
    for c in &r.commands {
        println!("  {c}");
    }
    if unreachable {
        let t = Instant::now();
        let u = osint::unreachable(&path, &cancel, &mut |p| {
            eprintln!("[unreachable] {} {}", p.phase, p.done)
        })
        .expect("fsck");
        println!(
            "unreachable in {:.1}s: {} commits, {} blobs ({} bytes), {} trees; scanned {} blobs, {} findings",
            t.elapsed().as_secs_f32(),
            u.commit_count,
            u.blobs,
            u.blob_bytes,
            u.trees,
            u.scanned_blobs,
            u.findings.len()
        );
        for f in u.findings.iter().take(10) {
            println!(
                "  [{}] {} · {} · {}",
                f.severity.label(),
                f.name,
                f.redacted(),
                f.paths().join(",")
            );
        }
    }
}
