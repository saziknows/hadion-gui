//! The palette's items for the active repository, built from the app state
//! with the core builders and kept until their data changes: every source
//! has a stamp (a hash of what it is built from) checked each frame, so a
//! source is rebuilt only after a reload, a fetch or a new list.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use gitvis_core::devops::palette::sources::{
    self as core, CommitEntry, IssueEntry, PrEntry, RefEntry, RepoEntry, RunEntry, TaskEntry,
};
use gitvis_core::devops::palette::{Item, Scope};
use gitvis_core::RefKind;
use gitvis_core::github::Outcome;

use super::commands::{self, Catalog};
use super::{Cx, FileList};
use crate::state::RepoId;

/// Where items come from, in the order ties are broken.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Src {
    Commands,
    Tasks,
    Repos,
    Refs,
    Prs,
    Runs,
    Files,
    Commits,
    Help,
}

impl Src {
    pub const ALL: [Src; 9] = [
        Src::Commands,
        Src::Tasks,
        Src::Repos,
        Src::Refs,
        Src::Prs,
        Src::Runs,
        Src::Files,
        Src::Commits,
        Src::Help,
    ];

    /// The sources a scope searches.
    pub fn for_scope(scope: Scope) -> &'static [Src] {
        match scope {
            Scope::All => &Src::ALL[..8],
            Scope::Commands => &[Src::Commands, Src::Tasks],
            Scope::Refs => &[Src::Refs],
            Scope::Prs => &[Src::Prs],
            Scope::Files => &[Src::Files],
            Scope::Commits => &[Src::Commits],
            Scope::Runs => &[Src::Runs],
            Scope::Repos => &[Src::Repos],
            Scope::Help => &[Src::Help],
        }
    }

    fn index(self) -> usize {
        Src::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }
}

#[derive(Default)]
struct Built {
    stamp: Option<u64>,
    items: Vec<Item>,
}

/// Built sources plus the command catalogue they came with.
#[derive(Default)]
pub struct Cache {
    built: [Built; 9],
    pub catalog: Catalog,
    catalog_stamp: Option<u64>,
}

impl Cache {
    /// Make sure `srcs` are built for the current data.
    pub fn ensure(&mut self, cx: &Cx, files: &HashMap<RepoId, FileList>, srcs: &[Src]) {
        let cs = commands::stamp(cx);
        if self.catalog_stamp != Some(cs) {
            self.catalog = commands::catalog(cx);
            self.catalog_stamp = Some(cs);
        }
        for &s in srcs {
            let stamp = stamp(s, cx, files, cs);
            let b = &mut self.built[s.index()];
            if b.stamp != Some(stamp) {
                b.items = build(s, cx, files, &self.catalog);
                b.stamp = Some(stamp);
            }
        }
    }

    pub fn items(&self, s: Src) -> &[Item] {
        &self.built[s.index()].items
    }

    /// The stamp the items were built with (0 before the first build).
    pub fn stamp(&self, s: Src) -> u64 {
        self.built[s.index()].stamp.unwrap_or(0)
    }

    /// The item with this key in `s`, if it is built.
    pub fn find(&self, s: Src, key: &str) -> Option<&Item> {
        self.items(s).iter().find(|i| i.key == key)
    }
}

fn stamp(s: Src, cx: &Cx, files: &HashMap<RepoId, FileList>, catalog: u64) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let repo = cx.repo();
    (s as u8, repo.map(|r| r.id)).hash(&mut h);
    match s {
        Src::Commands => catalog.hash(&mut h),
        Src::Tasks => repo
            .map(|r| r.devops.workbench.tasks.at)
            .hash(&mut h),
        Src::Repos => {
            for r in cx.repos {
                (&r.workdir, r.snapshot.as_ref().and_then(|s| s.head.branch())).hash(&mut h);
            }
            cx.active.hash(&mut h);
            cx.settings.recent.hash(&mut h);
        }
        Src::Refs | Src::Commits => repo.map(|r| r.loaded_at).hash(&mut h),
        Src::Prs => {
            if let Some(r) = repo {
                let rv = &r.devops.review;
                (r.github_at, rv.open.at, rv.closed.at).hash(&mut h);
                let mut ats: Vec<_> = rv.issues.lists.values().map(|f| f.at).collect();
                ats.sort();
                ats.hash(&mut h);
            }
        }
        Src::Runs => repo.map(|r| r.github_at).hash(&mut h),
        Src::Files => repo
            .and_then(|r| files.get(&r.id))
            .map(|f| f.at)
            .hash(&mut h),
        Src::Help => {}
    }
    h.finish()
}

fn build(s: Src, cx: &Cx, files: &HashMap<RepoId, FileList>, catalog: &Catalog) -> Vec<Item> {
    match s {
        Src::Commands => core::command_items(&catalog.entries()),
        Src::Tasks => core::task_items(&task_entries(cx)),
        Src::Repos => core::repo_items(&repo_entries(cx)),
        Src::Refs => core::ref_items(&ref_entries(cx)),
        Src::Prs => {
            let (prs, issues) = pr_entries(cx);
            let mut v = core::pr_items(&prs);
            let mut i = core::issue_items(&issues);
            let n = v.len() as u32;
            for x in &mut i {
                x.order += n;
            }
            v.append(&mut i);
            v
        }
        Src::Runs => core::run_items(&run_entries(cx)),
        Src::Files => match cx.repo().and_then(|r| files.get(&r.id)) {
            Some(FileList {
                paths: Some(Ok(p)), ..
            }) => core::file_items(p),
            _ => Vec::new(),
        },
        Src::Commits => core::commit_items(&commit_entries(cx)),
        Src::Help => core::prefix_items(),
    }
}

pub fn ref_entries(cx: &Cx) -> Vec<RefEntry> {
    let Some(snap) = cx.snap() else {
        return Vec::new();
    };
    let short = |o: gitvis_core::Oid| o.to_string()[..7].to_owned();
    let mut v = Vec::new();
    for b in &snap.branches {
        v.push(RefEntry {
            name: b.name.clone(),
            full: b.full.clone(),
            kind: Some(RefKind::Local),
            is_head: b.is_head,
            upstream: b.upstream.clone(),
            upstream_gone: b.upstream_gone,
            ahead: b.ahead,
            behind: b.behind,
            short: short(b.target),
        });
    }
    for r in &snap.remotes {
        for b in &r.branches {
            // `origin/HEAD` is a pointer, not a branch.
            if b.branch == "HEAD" {
                continue;
            }
            v.push(RefEntry {
                name: b.name.clone(),
                full: b.full.clone(),
                kind: Some(RefKind::Remote),
                short: short(b.target),
                ..Default::default()
            });
        }
    }
    // Newest tags first (they are usually sorted by name, oldest first).
    for t in snap.tags.iter().rev() {
        v.push(RefEntry {
            name: t.name.clone(),
            full: t.full.clone(),
            kind: Some(RefKind::Tag),
            short: short(t.target),
            ..Default::default()
        });
    }
    v
}

pub fn commit_entries(cx: &Cx) -> Vec<CommitEntry> {
    let Some(snap) = cx.snap() else {
        return Vec::new();
    };
    snap.commits
        .iter()
        .map(|c| CommitEntry {
            sha: c.id.to_string(),
            short: c.short.clone(),
            summary: c.summary.clone(),
            author: c.author.clone(),
            time: c.time,
            refs: snap
                .refs
                .get(&c.id)
                .map(|l| l.iter().map(|r| r.name.clone()).collect())
                .unwrap_or_default(),
        })
        .collect()
}

/// Pull requests from the Review page's lists when loaded, else from the
/// GitHub data loaded with the repository; issues from the Review page.
pub fn pr_entries(cx: &Cx) -> (Vec<PrEntry>, Vec<IssueEntry>) {
    let Some(r) = cx.repo() else {
        return (Vec::new(), Vec::new());
    };
    let mut seen = std::collections::HashSet::new();
    let mut prs = Vec::new();
    let rv = &r.devops.review;
    for list in [&rv.open, &rv.closed] {
        if let Some(l) = &list.data {
            for p in l.items.iter().chain(&l.requested).chain(&l.mine) {
                if seen.insert(p.number) {
                    let state = match p.state {
                        _ if p.draft => "Draft",
                        gitvis_core::devops::review::model::PrState::Open => "Open",
                        gitvis_core::devops::review::model::PrState::Merged => "Merged",
                        gitvis_core::devops::review::model::PrState::Closed => "Closed",
                    };
                    prs.push(PrEntry {
                        number: p.number,
                        title: p.title.clone(),
                        author: p.author.clone(),
                        head: p.head.clone(),
                        base: p.base.clone(),
                        state: state.to_owned(),
                    });
                }
            }
        }
    }
    if let Some(gh) = &r.github {
        for p in &gh.prs {
            if seen.insert(p.number) {
                prs.push(PrEntry {
                    number: p.number,
                    title: p.title.clone(),
                    author: p.author.clone(),
                    head: p.head.clone(),
                    base: p.base.clone(),
                    state: if p.draft { "Draft" } else { "Open" }.to_owned(),
                });
            }
        }
    }
    // Newest first.
    prs.sort_by(|a, b| b.number.cmp(&a.number));
    let mut issues = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for f in rv.issues.lists.values() {
        if let Some(list) = &f.data {
            for i in list.iter().filter(|i| !i.is_pr) {
                if seen.insert(i.number) {
                    issues.push(IssueEntry {
                        number: i.number,
                        title: i.title.clone(),
                        author: i.author.clone(),
                        open: i.open,
                        labels: i.labels.iter().map(|l| l.name.clone()).collect(),
                    });
                }
            }
        }
    }
    issues.sort_by(|a, b| b.number.cmp(&a.number));
    (prs, issues)
}

pub fn run_entries(cx: &Cx) -> Vec<RunEntry> {
    let Some(gh) = cx.repo().and_then(|r| r.github.as_ref()) else {
        return Vec::new();
    };
    gh.runs
        .iter()
        .map(|r| RunEntry {
            id: r.id,
            workflow: r.name.clone(),
            title: r.title.clone(),
            branch: r.branch.clone(),
            number: r.run_number,
            event: r.event.clone(),
            outcome: outcome_label(r.outcome()).to_owned(),
            time: r.created.unwrap_or(0),
        })
        .collect()
}

/// Sentence-case badge text of a run outcome.
pub fn outcome_label(o: Outcome) -> &'static str {
    match o {
        Outcome::Queued => "Queued",
        Outcome::InProgress => "Running",
        Outcome::Success => "Passed",
        Outcome::Failure => "Failed",
        Outcome::Cancelled => "Cancelled",
        Outcome::Skipped => "Skipped",
        Outcome::Neutral => "Neutral",
        Outcome::TimedOut => "Timed out",
        Outcome::ActionRequired => "Action required",
        Outcome::Stale => "Stale",
        Outcome::Unknown => "Unknown",
    }
}

pub fn task_entries(cx: &Cx) -> Vec<TaskEntry> {
    let Some(Some(Ok(d))) = cx.repo().map(|r| &r.devops.workbench.tasks.discovery) else {
        return Vec::new();
    };
    d.tasks
        .iter()
        .map(|t| TaskEntry {
            id: t.id.clone(),
            name: t.name.clone(),
            source: t.program.clone(),
            dir: t.dir.clone(),
            description: t.description.clone(),
        })
        .collect()
}

pub fn repo_entries(cx: &Cx) -> Vec<RepoEntry> {
    let mut v: Vec<RepoEntry> = cx
        .repos
        .iter()
        .enumerate()
        .map(|(i, r)| RepoEntry {
            path: r.workdir.clone(),
            name: r.name.clone(),
            open: Some(i),
            active: i == cx.active,
            branch: r
                .snapshot
                .as_ref()
                .and_then(|s| s.head.branch().map(str::to_owned)),
        })
        .collect();
    for p in &cx.settings.recent {
        v.push(RepoEntry {
            name: p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string()),
            path: p.clone(),
            open: None,
            active: false,
            branch: None,
        });
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{RepoState, Settings};

    #[test]
    fn scopes_pick_sources() {
        assert_eq!(Src::for_scope(Scope::All).len(), 8);
        assert!(!Src::for_scope(Scope::All).contains(&Src::Help));
        assert_eq!(Src::for_scope(Scope::Commands), [Src::Commands, Src::Tasks]);
        assert_eq!(Src::for_scope(Scope::Help), [Src::Help]);
        for s in Src::ALL {
            assert_eq!(Src::ALL[s.index()], s);
        }
    }

    #[test]
    fn rebuilds_only_when_the_data_changes() {
        let settings = Settings::default();
        let mut repos = vec![RepoState::new(1, "C:/a".into())];
        let files = HashMap::new();
        let mut cache = Cache::default();
        {
            let cx = Cx {
                settings: &settings,
                repos: &repos,
                active: 0,
            };
            cache.ensure(&cx, &files, &[Src::Repos, Src::Commands, Src::Refs]);
        }
        let s1 = cache.stamp(Src::Repos);
        let r1 = cache.stamp(Src::Refs);
        assert_eq!(cache.items(Src::Repos).len(), 1);
        assert!(!cache.items(Src::Commands).is_empty());
        repos.push(RepoState::new(2, "C:/b".into()));
        let cx = Cx {
            settings: &settings,
            repos: &repos,
            active: 0,
        };
        cache.ensure(&cx, &files, &[Src::Repos, Src::Refs]);
        assert_ne!(cache.stamp(Src::Repos), s1);
        assert_eq!(cache.items(Src::Repos).len(), 2);
        // Refs did not change.
        assert_eq!(cache.stamp(Src::Refs), r1);
    }

    #[test]
    fn recent_repositories_are_listed_once() {
        let mut settings = Settings::default();
        settings.recent = vec!["C:/a".into(), "C:/c".into()];
        let repos = vec![RepoState::new(1, "C:/a".into())];
        let cx = Cx {
            settings: &settings,
            repos: &repos,
            active: 0,
        };
        let items = core::repo_items(&repo_entries(&cx));
        let names: Vec<&str> = items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(names, ["a", "c"]);
    }
}
