//! Read-only checks of the review queries against a real repository.
//! Ignored by default (they need `gh auth` and the network):
//!
//! ```text
//! cargo test -p hadion-core --test review_live -- --ignored --nocapture
//! ```
//!
//! `HADION_REVIEW_REPO=owner/name` picks the repository (default
//! `brave/brave-core`). Nothing here writes to GitHub.

use hadion_core::devops::gh::Gh;
use hadion_core::devops::review::{ListKind, fetch};

fn gh() -> Gh {
    let repo = std::env::var("HADION_REVIEW_REPO").unwrap_or_else(|_| "brave/brave-core".into());
    let (o, r) = repo.split_once('/').expect("owner/name");
    Gh::new(std::env::temp_dir(), o, r)
}

#[test]
#[ignore]
fn list_detail_files_commits() {
    let gh = gh();
    let mut calls = Vec::new();
    let meta = fetch::fetch_repo_meta(&gh, &mut calls).expect("repository");
    println!("meta: {meta:?}");
    let list = fetch::fetch_pr_list(&gh, ListKind::Open, None, &mut calls).expect("list");
    println!(
        "{} of {} open (light: {}, cost {:?}), {} requested, {} mine, viewer {}",
        list.items.len(),
        list.total,
        list.light,
        list.cost,
        list.requested.len(),
        list.mine.len(),
        list.viewer
    );
    let pr = list
        .items
        .iter()
        .max_by_key(|p| p.threads)
        .expect("a pull request");
    println!("#{} {} threads={}", pr.number, pr.title, pr.threads);
    let d = fetch::fetch_pr_detail(&gh, pr.number, &mut calls).expect("detail");
    println!(
        "timeline {} of {}, threads {} of {}, status {:?}",
        d.timeline.len(),
        d.timeline_total,
        d.threads.len(),
        d.threads_total,
        hadion_core::devops::review::merge_status(&d.pr)
    );
    let files = fetch::fetch_pr_files(&gh, pr.number, &mut calls).expect("files");
    println!("{} files", files.len());
    for f in files.iter().take(3) {
        let hunks =
            hadion_core::devops::review::patch::parse_patch(f.patch.as_deref().unwrap_or(""));
        let (a, del) = hadion_core::devops::review::patch::stats(&hunks);
        println!(
            "  {} +{}/-{} parsed +{a}/-{del}",
            f.path, f.additions, f.deletions
        );
        assert_eq!((a as u64, del as u64), (f.additions, f.deletions));
    }
    let commits = fetch::fetch_pr_commits(&gh, pr.number, &mut calls).expect("commits");
    println!("{} commits", commits.len());
    for c in &calls {
        println!("{} · {} bytes · {} ms", c.command, c.bytes, c.elapsed_ms);
    }
}
