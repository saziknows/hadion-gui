//! Times a full snapshot load: `cargo run -p hadion-core --example bench -- <repo>`

use std::path::PathBuf;
use std::time::Instant;

use hadion_core::{LoadOptions, repo};

fn main() {
    let path: PathBuf = std::env::args()
        .nth(1)
        .map(Into::into)
        .unwrap_or_else(|| ".".into());
    let opts = LoadOptions::default();
    let t = Instant::now();
    let snap = repo::load(&path, &opts).expect("load");
    let load = t.elapsed();
    println!(
        "{}: {} commits ({}), {} lanes, {} branches, {} remote branches, {} tags, {} stashes, {} changes",
        snap.name,
        snap.commits.len(),
        if snap.truncated {
            "truncated"
        } else {
            "complete"
        },
        snap.graph.width,
        snap.branches.len(),
        snap.remotes.iter().map(|r| r.branches.len()).sum::<usize>(),
        snap.tags.len(),
        snap.stashes.len(),
        snap.status.change_count(),
    );
    println!("load: {load:?}");

    let t = Instant::now();
    let (status, _) = repo::load_status(&path).expect("status");
    println!(
        "status only: {:?} ({} changes)",
        t.elapsed(),
        status.change_count()
    );

    if let Some(c) = snap.commits.first() {
        let t = Instant::now();
        let d = repo::commit_details(&path, c.id).expect("details");
        println!(
            "details of {}: {:?} ({} files)",
            c.short,
            t.elapsed(),
            d.files.len()
        );
    }
}
