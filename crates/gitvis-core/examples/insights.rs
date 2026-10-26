//! Runs the insights analysis from the command line and prints the report:
//!
//! ```text
//! cargo run --release -p gitvis-core --example insights -- <repo> [--commits N] [--all-refs] [--weight] [--lost] [--date-debt] [--pickaxe TEXT]
//! ```

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use gitvis_core::insights::search::{SearchMode, SearchQuery};
use gitvis_core::insights::{self, InsightsOptions, Progress};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut path: Option<PathBuf> = None;
    let mut opts = InsightsOptions::default();
    let (mut weight, mut lost, mut date_debt) = (false, false, false);
    let mut pickaxe: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--commits" => {
                opts.max_commits = args.next().and_then(|n| n.parse().ok()).unwrap_or(10_000)
            }
            "--all-refs" => opts.all_refs = true,
            "--weight" => weight = true,
            "--lost" => lost = true,
            "--date-debt" => date_debt = true,
            "--pickaxe" => pickaxe = args.next(),
            other => path = Some(other.into()),
        }
    }
    let path = path.unwrap_or_else(|| ".".into());
    let cancel = AtomicBool::new(false);
    let t = Instant::now();
    let mut last = String::new();
    let mut progress = |p: &Progress| {
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
    };
    let r = insights::analyze(&path, &opts, &cancel, &mut progress).expect("analyze");
    let s = &r.stats;
    println!(
        "{} commits · {} files · {} of numstat · {:.1}s{}",
        s.commits,
        s.files,
        s.log_bytes / 1024,
        s.elapsed_ms as f64 / 1000.0,
        if s.cancelled { " (cancelled)" } else { "" }
    );
    for (p, ms) in &s.phases {
        println!("  {p:<40} {ms:>7} ms");
    }

    let h = &r.health;
    println!(
        "\nHEALTH: {} landmines · lf {} crlf {} mixed {} fighting {} bom {} non-utf8 {} · exec: {} scripts without +x, {} non-scripts with · {} submodules · {} loose objects, {} packs ({} KB) · commit-graph {}",
        h.landmines.len(),
        h.line_endings.lf,
        h.line_endings.crlf,
        h.line_endings.mixed.len(),
        h.line_endings.fighting.len(),
        h.line_endings.bom.len(),
        h.line_endings.non_utf8.len(),
        h.exec_bits.scripts_without.len(),
        h.exec_bits.non_scripts_with.len(),
        h.submodules.len(),
        h.weight.loose_objects,
        h.weight.packs,
        h.weight.pack_bytes / 1024,
        h.weight.commit_graph
    );
    for m in h.landmines.iter().take(12) {
        println!(
            "  {:<9} {:<28} {} {}",
            m.severity.label(),
            m.kind.label(),
            m.paths.first().map(String::as_str).unwrap_or(""),
            if m.paths.len() > 1 {
                format!("(+{} more)", m.paths.len() - 1)
            } else {
                String::new()
            }
        );
    }
    for a in &h.weight.advice {
        println!("  advice: {a}");
    }

    let hi = &r.history;
    println!(
        "\nHISTORY: {} hotspots · {} couplings · {} dirs aged · {} dirs owned ({} orphaned)",
        hi.hotspots.len(),
        hi.couplings.len(),
        hi.age.len(),
        hi.ownership.len(),
        hi.ownership.iter().filter(|o| o.orphaned).count()
    );
    for x in hi.hotspots.iter().take(10) {
        println!(
            "  {:>7.1} {:>5} commits {:>4} authors {:>4} fixes  {}",
            x.score, x.commits, x.authors, x.fixes, x.path
        );
    }
    for c in hi.couplings.iter().take(5) {
        println!(
            "  coupled {:>3}× {:>3.0}%  {}  ↔  {}",
            c.together,
            c.confidence * 100.0,
            c.a,
            c.b
        );
    }
    for o in hi.ownership.iter().take(5) {
        println!(
            "  bus {}{} {:<40} {}",
            o.bus_factor,
            if o.orphaned { " ORPHANED" } else { "" },
            o.dir,
            o.owners
                .iter()
                .take(3)
                .map(|w| format!("{} {:.0}%", w.name, w.share * 100.0))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let q = &hi.quality;
    println!(
        "QUALITY: {} commits · {} merges · {} wip · {} fixups · {} reverts · {} giant · {} bots · {} author≠committer · {} date anomalies · conventional {} · issue refs {} · signatures {:?}",
        q.total,
        q.merges,
        q.wip.len(),
        q.fixups.len(),
        q.reverts.len(),
        q.giant.len(),
        q.bot_commits,
        q.author_committer_differ,
        q.date_anomalies.len(),
        q.conventional,
        q.with_issue_ref,
        q.signatures
            .as_ref()
            .map(|s| (s.checked, s.good, s.bad, s.none, s.error))
    );
    let tl = &hi.timeline;
    println!(
        "TIMELINE: {} months · {} authors ({} active) · {} languages · peak {} {:02}:00",
        tl.months.len(),
        tl.authors.len(),
        tl.active_authors,
        tl.languages.len(),
        tl.peak_weekday,
        tl.peak_hour
    );

    let b = &r.refs.branches;
    let rel = &r.refs.releases;
    println!(
        "\nREFS: {} local · {} remote · {} merged · {} gone · {} stale · {} throwaway · {} tags ({} annotated, {} signed, {} orphan) · last tag {:?} · {} unreleased · cadence {:?} d · scheme {} · next {:?}",
        b.local.len(),
        b.remote.len(),
        b.merged,
        b.gone,
        b.stale,
        b.throwaway,
        rel.tags.len(),
        rel.annotated,
        rel.signed,
        rel.orphan,
        rel.last_tag,
        rel.unreleased_total,
        rel.cadence_days,
        rel.scheme,
        rel.suggested_next
    );
    for x in b.local.iter().take(8) {
        println!(
            "  {:<30} unique {:?} conflicts {} dupes {}{}{}{}",
            x.name,
            x.unique,
            x.conflicts.len(),
            x.duplicates.len(),
            if x.merged { " merged" } else { "" },
            if x.upstream_gone { " gone" } else { "" },
            if x.stale { " stale" } else { "" }
        );
    }

    let d = &r.debt;
    println!(
        "\nDEBT: {} markers in {} files ({} scanned, {} lines) · {:?}",
        d.markers.len(),
        d.files_with_markers,
        d.files_scanned,
        d.lines_scanned,
        d.by_kind
    );
    let c = &r.ci;
    println!(
        "CI: {} workflows · {} actions ({} unpinned third-party, {} pinned to SHA) · {} findings · other {:?}",
        c.workflows.len(),
        c.actions.len(),
        c.unpinned_third_party,
        c.pinned_sha,
        c.findings.len(),
        c.other_ci
            .iter()
            .map(|(_, n)| n.as_str())
            .collect::<Vec<_>>()
    );
    for f in c.findings.iter().take(10) {
        println!(
            "  {:<9} {:<30} {} · {}",
            f.severity.label(),
            f.id,
            f.workflow,
            f.title
        );
    }
    let l = &r.license;
    println!(
        "LICENCES: project {:?} · declared {:?} · {} deps ({} copyleft) · {} foreign holders · {} findings",
        l.main(),
        l.declared,
        l.dependencies.len(),
        l.copyleft.len(),
        l.foreign.len(),
        l.findings.len()
    );
    for f in &l.findings {
        println!("  {:<9} {:<28} {}", f.severity.label(), f.id, f.title);
    }

    if weight {
        let w = insights::health::weight(&path, &cancel, &mut progress).expect("weight");
        println!(
            "\nWEIGHT: {} blobs · {} MB uncompressed · {} MB on disk · {} commits · {} trees · dead weight {} MB · {:.1}s",
            w.total_blobs,
            w.total_bytes >> 20,
            w.total_disk >> 20,
            w.commits,
            w.trees,
            w.deleted_bytes >> 20,
            w.elapsed_ms as f64 / 1000.0
        );
        for b in w.blobs.iter().take(10) {
            println!(
                "  {:>8} KB {:>8} KB disk {} {}",
                b.size / 1024,
                b.disk_size / 1024,
                if b.in_head { "      " } else { "(gone)" },
                b.path
            );
        }
    }
    if date_debt {
        let dated =
            insights::debt::date(&path, &d.markers, 400, &cancel, &mut progress).expect("date");
        let n = dated.markers.iter().filter(|m| m.time.is_some()).count();
        println!(
            "\nDATED: {n} of {} markers · {} files blamed · {:.1}s",
            dated.markers.len(),
            dated.files_blamed,
            dated.elapsed_ms as f64 / 1000.0
        );
    }
    if lost {
        let l = insights::recover::lost(&path, &cancel, &mut progress).expect("lost");
        println!(
            "\nLOST: {} commits ({} stashes, {} in reflog, {} dangling) · {} blobs · {} trees · {:.1}s",
            l.commits.len(),
            l.stashes,
            l.in_reflog,
            l.dangling,
            l.blobs,
            l.trees,
            l.elapsed_ms as f64 / 1000.0
        );
        for c in l.commits.iter().take(10) {
            println!(
                "  {} {:<6} {} {} files  {}  {}",
                &c.id.to_string()[..7],
                c.kind.label(),
                c.author,
                c.files_total,
                c.summary,
                c.reflog.as_deref().unwrap_or("(dangling)")
            );
        }
    }
    if let Some(text) = pickaxe {
        let q = SearchQuery {
            text,
            mode: SearchMode::Pickaxe,
            all_refs: true,
            ..Default::default()
        };
        let res = insights::search::search(&path, &q, &cancel).expect("search");
        println!(
            "\nSEARCH: {} hits{} · {} files · {:.1}s · {}",
            res.hits.len(),
            if res.truncated { "+" } else { "" },
            res.paths.len(),
            res.elapsed_ms as f64 / 1000.0,
            res.command
        );
        for h in res.hits.iter().take(10) {
            println!(
                "  {} {} {:<20} {}  [{}]",
                &h.id.to_string()[..7],
                h.time,
                h.author,
                h.summary,
                h.paths.join(", ")
            );
        }
    }
}
