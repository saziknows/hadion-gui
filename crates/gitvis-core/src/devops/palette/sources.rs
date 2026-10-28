//! Builders that turn plain data (ref lists, file lists, pull requests,
//! commits, runs, tasks, repositories, commands) into palette items. They
//! are pure, so they are tested without the app.

use std::path::PathBuf;

use super::item::{Item, Target};
use super::scope::PREFIXES;
use crate::RefKind;

/// A branch or tag.
#[derive(Clone, Debug, Default)]
pub struct RefEntry {
    pub name: String,
    pub full: String,
    pub kind: Option<RefKind>,
    /// The branch HEAD is on.
    pub is_head: bool,
    pub upstream: Option<String>,
    pub upstream_gone: bool,
    pub ahead: usize,
    pub behind: usize,
    /// Short SHA of the commit it points at.
    pub short: String,
}

/// Branches first (the current one on top), then remote branches, then
/// tags, each in the order given.
pub fn ref_items(refs: &[RefEntry]) -> Vec<Item> {
    let rank = |r: &RefEntry| match (r.kind, r.is_head) {
        (Some(RefKind::Local), true) => 0,
        (Some(RefKind::Local), false) => 1,
        (Some(RefKind::Remote), _) => 2,
        _ => 3,
    };
    let mut sorted: Vec<&RefEntry> = refs.iter().filter(|r| r.kind.is_some()).collect();
    sorted.sort_by_key(|r| rank(r));
    sorted
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            let kind = r.kind.unwrap_or(RefKind::Local);
            let mut sub = Vec::new();
            match kind {
                RefKind::Local => {
                    if r.is_head {
                        sub.push("current branch".to_owned());
                    }
                    match (&r.upstream, r.upstream_gone) {
                        (Some(u), true) => sub.push(format!("upstream {u} is gone")),
                        (Some(u), false) => sub.push(format!("tracks {u}")),
                        (None, _) => sub.push("no upstream".to_owned()),
                    }
                    match (r.ahead, r.behind) {
                        (0, 0) => {}
                        (a, 0) => sub.push(format!("{a} ahead")),
                        (0, b) => sub.push(format!("{b} behind")),
                        (a, b) => sub.push(format!("{a} ahead, {b} behind")),
                    }
                }
                RefKind::Remote => {
                    let remote = r.name.split('/').next().unwrap_or("");
                    sub.push(format!("remote branch on {remote}"));
                }
                RefKind::Tag => sub.push("tag".to_owned()),
            }
            if !r.short.is_empty() {
                sub.push(r.short.clone());
            }
            let badge = match (kind, r.is_head) {
                (RefKind::Local, true) => "Current branch",
                (RefKind::Local, false) => "Branch",
                (RefKind::Remote, _) => "Remote branch",
                (RefKind::Tag, _) => "Tag",
            };
            let mut item = Item::new(
                Target::Ref {
                    name: r.name.clone(),
                    full: r.full.clone(),
                    kind,
                },
                r.name.clone(),
                sub.join(" · "),
            )
            .badge(badge);
            item.order = i as u32;
            item
        })
        .collect()
}

/// One item per path; the file name is matched on its own too (with a
/// small bonus), so `main` finds `src/main.rs` before `maintenance/x.rs`.
pub fn file_items(paths: &[String]) -> Vec<Item> {
    paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (dir, name) = split_path(p);
            let mut item = Item::new(Target::File(p.clone()), p.clone(), dir.to_owned());
            if !dir.is_empty() {
                item.extra = Some(super::fuzzy::Haystack::new(name));
                item.extra_adjust = BASENAME_BONUS;
            }
            item.order = i as u32;
            item
        })
        .collect()
}

/// Added to a match inside the file name alone.
pub const BASENAME_BONUS: i32 = 6;

/// `src/view/main.rs` → (`src/view`, `main.rs`).
pub fn split_path(p: &str) -> (&str, &str) {
    match p.rfind('/') {
        Some(i) => (&p[..i], &p[i + 1..]),
        None => ("", p),
    }
}

/// A commit of the loaded history.
#[derive(Clone, Debug, Default)]
pub struct CommitEntry {
    pub sha: String,
    pub short: String,
    pub summary: String,
    pub author: String,
    /// Author time, unix seconds (0 when unknown).
    pub time: i64,
    /// Branch and tag names on it.
    pub refs: Vec<String>,
}

/// Commits in the order given (newest first); matched on the summary, the
/// author and ref names (as keywords) and the SHA (by prefix, 4 or more
/// characters).
pub fn commit_items(commits: &[CommitEntry]) -> Vec<Item> {
    commits
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut sub = vec![c.short.clone()];
            if !c.author.is_empty() {
                sub.push(c.author.clone());
            }
            if !c.refs.is_empty() {
                sub.push(c.refs.join(", "));
            }
            let summary = if c.summary.trim().is_empty() {
                "(no message)".to_owned()
            } else {
                c.summary.clone()
            };
            let mut kw = c.author.clone();
            for r in &c.refs {
                kw.push(' ');
                kw.push_str(r);
            }
            let mut item = Item::new(Target::Commit(c.sha.clone()), summary, sub.join(" · "))
                .keywords(&kw)
                .exact(&c.sha, 4);
            item.order = i as u32;
            item.time = (c.time > 0).then_some(c.time);
            item
        })
        .collect()
}

/// A pull request.
#[derive(Clone, Debug, Default)]
pub struct PrEntry {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub head: String,
    pub base: String,
    /// `Open`, `Draft`, `Merged`, `Closed`.
    pub state: String,
}

/// Pull requests, matched on the title, the author and branches
/// (keywords) and the number (by prefix).
pub fn pr_items(prs: &[PrEntry]) -> Vec<Item> {
    prs.iter()
        .enumerate()
        .map(|(i, p)| {
            let mut sub = format!("#{}", p.number);
            if !p.author.is_empty() {
                sub.push_str(&format!(" · {}", p.author));
            }
            if !p.head.is_empty() {
                sub.push_str(&format!(" · {} → {}", p.head, p.base));
            }
            let mut item = Item::new(Target::Pr(p.number), p.title.clone(), sub)
                .badge(if p.state.is_empty() {
                    "Pull request"
                } else {
                    &p.state
                })
                .keywords(&format!("{} {} {}", p.author, p.head, p.base))
                .exact(&p.number.to_string(), 1);
            item.order = i as u32;
            item
        })
        .collect()
}

/// An issue.
#[derive(Clone, Debug, Default)]
pub struct IssueEntry {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub open: bool,
    pub labels: Vec<String>,
}

pub fn issue_items(issues: &[IssueEntry]) -> Vec<Item> {
    issues
        .iter()
        .enumerate()
        .map(|(i, x)| {
            let mut sub = format!("#{} · issue", x.number);
            if !x.author.is_empty() {
                sub.push_str(&format!(" · {}", x.author));
            }
            if !x.labels.is_empty() {
                sub.push_str(&format!(" · {}", x.labels.join(", ")));
            }
            let mut item = Item::new(Target::Issue(x.number), x.title.clone(), sub)
                .badge(if x.open {
                    "Open issue"
                } else {
                    "Closed issue"
                })
                .keywords(&format!("{} {}", x.author, x.labels.join(" ")))
                .exact(&x.number.to_string(), 1);
            item.order = i as u32;
            item
        })
        .collect()
}

/// A workflow run.
#[derive(Clone, Debug, Default)]
pub struct RunEntry {
    pub id: u64,
    pub workflow: String,
    /// Commit or pull request title it ran for.
    pub title: String,
    pub branch: String,
    pub number: u64,
    pub event: String,
    /// `Failed`, `Passed`, `Running`, …
    pub outcome: String,
    /// Created, unix seconds (0 when unknown).
    pub time: i64,
}

/// Runs, titled `workflow: commit title`, matched on both, the branch and
/// event (keywords) and the run number (by prefix).
pub fn run_items(runs: &[RunEntry]) -> Vec<Item> {
    runs.iter()
        .enumerate()
        .map(|(i, r)| {
            let mut sub = vec![format!("#{}", r.number)];
            for s in [&r.branch, &r.event] {
                if !s.is_empty() {
                    sub.push(s.clone());
                }
            }
            let title = if r.title.is_empty() {
                r.workflow.clone()
            } else {
                format!("{}: {}", r.workflow, r.title)
            };
            let mut item = Item::new(Target::Run(r.id), title, sub.join(" · "))
                .badge(if r.outcome.is_empty() {
                    "Run"
                } else {
                    &r.outcome
                })
                .keywords(&format!("{} {}", r.branch, r.event))
                .exact(&r.number.to_string(), 1);
            item.order = i as u32;
            item.time = (r.time > 0).then_some(r.time);
            item
        })
        .collect()
}

/// A task found by the workbench.
#[derive(Clone, Debug, Default)]
pub struct TaskEntry {
    pub id: String,
    pub name: String,
    /// `npm`, `make`, `cargo`, …
    pub source: String,
    /// Directory relative to the root (empty for the root).
    pub dir: String,
    pub description: String,
}

pub fn task_items(tasks: &[TaskEntry]) -> Vec<Item> {
    tasks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut sub = format!("{} task", t.source);
            if !t.dir.is_empty() {
                sub.push_str(&format!(" in {}", t.dir));
            }
            if !t.description.is_empty() {
                sub.push_str(&format!(" · {}", t.description));
            }
            let mut item = Item::new(Target::Task(t.id.clone()), t.name.clone(), sub)
                .badge("Task")
                .keywords(&format!("run task {} {}", t.source, t.dir));
            item.order = i as u32;
            item
        })
        .collect()
}

/// An open or recent repository.
#[derive(Clone, Debug, Default)]
pub struct RepoEntry {
    pub path: PathBuf,
    pub name: String,
    /// Index of its tab when open.
    pub open: Option<usize>,
    pub active: bool,
    /// Checked-out branch (open repositories).
    pub branch: Option<String>,
}

/// Open repositories (the active one first), then recent ones that are not
/// open, without duplicates.
pub fn repo_items(repos: &[RepoEntry]) -> Vec<Item> {
    let mut seen = std::collections::HashSet::new();
    let mut sorted: Vec<&RepoEntry> = repos.iter().collect();
    sorted.sort_by_key(|r| (!r.active, r.open.is_none()));
    sorted
        .into_iter()
        .filter(|r| seen.insert(r.path.to_string_lossy().to_lowercase()))
        .enumerate()
        .map(|(i, r)| {
            let mut sub = r.path.display().to_string();
            if let Some(b) = &r.branch {
                sub.push_str(&format!(" · {b}"));
            }
            let badge = match (r.active, r.open) {
                (true, _) => "Current tab",
                (false, Some(_)) => "Open tab",
                (false, None) => "Recent",
            };
            let mut item = Item::new(Target::Repo(r.path.clone()), r.name.clone(), sub)
                .badge(badge)
                .keywords(&r.path.display().to_string());
            item.order = i as u32;
            item
        })
        .collect()
}

/// A command of the app's catalogue.
#[derive(Clone, Debug, Default)]
pub struct CommandEntry {
    pub id: String,
    pub title: String,
    /// `Go to`, `Git`, `App`, `Review`, …
    pub category: String,
    /// `Ctrl+K`, or empty.
    pub shortcut: String,
    /// Other words that find it.
    pub keywords: String,
    /// What it does, shown after the title.
    pub hint: String,
}

pub fn command_items(cmds: &[CommandEntry]) -> Vec<Item> {
    cmds.iter()
        .enumerate()
        .map(|(i, c)| {
            let sub = if c.hint.is_empty() {
                c.category.clone()
            } else {
                c.hint.clone()
            };
            let mut item = Item::new(Target::Command(c.id.clone()), c.title.clone(), sub)
                .badge(c.category.clone())
                .shortcut(&c.shortcut)
                .keywords(&format!("{} {}", c.category, c.keywords));
            item.order = i as u32;
            item
        })
        .collect()
}

/// The prefix help rows.
pub fn prefix_items() -> Vec<Item> {
    PREFIXES
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut item = Item::new(Target::Prefix(p.scope), p.label, p.help)
                .shortcut(&p.ch.to_string())
                .keywords(super::scope::short_label(p.scope));
            item.order = i as u32;
            item
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::palette::item::Kind;

    fn r(name: &str, kind: RefKind, head: bool) -> RefEntry {
        let full = match kind {
            RefKind::Local => format!("refs/heads/{name}"),
            RefKind::Remote => format!("refs/remotes/{name}"),
            RefKind::Tag => format!("refs/tags/{name}"),
        };
        RefEntry {
            name: name.into(),
            full,
            kind: Some(kind),
            is_head: head,
            short: "abc1234".into(),
            ..Default::default()
        }
    }

    #[test]
    fn refs_order_and_subtitles() {
        let mut main = r("main", RefKind::Local, true);
        main.upstream = Some("origin/main".into());
        main.ahead = 2;
        main.behind = 1;
        let mut gone = r("old", RefKind::Local, false);
        gone.upstream = Some("origin/old".into());
        gone.upstream_gone = true;
        let items = ref_items(&[
            r("v1.0", RefKind::Tag, false),
            r("origin/main", RefKind::Remote, false),
            r("feature", RefKind::Local, false),
            main,
            gone,
            RefEntry::default(),
        ]);
        let names: Vec<&str> = items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(names, ["main", "feature", "old", "origin/main", "v1.0"]);
        assert_eq!(
            items[0].subtitle,
            "current branch · tracks origin/main · 2 ahead, 1 behind · abc1234"
        );
        assert_eq!(items[0].badge_text(), "Current branch");
        assert_eq!(items[1].subtitle, "no upstream · abc1234");
        assert_eq!(items[2].subtitle, "upstream origin/old is gone · abc1234");
        assert_eq!(items[3].subtitle, "remote branch on origin · abc1234");
        assert_eq!(items[3].kind(), Kind::Remote);
        assert_eq!(items[4].kind(), Kind::Tag);
        assert_eq!(items[4].key, "ref:refs/tags/v1.0");
        assert_eq!(
            items.iter().map(|i| i.order).collect::<Vec<_>>(),
            [0, 1, 2, 3, 4]
        );
    }

    #[test]
    fn files_split_and_match_by_name() {
        let items = file_items(&["src/main.rs".into(), "README.md".into()]);
        assert_eq!(items[0].title, "src/main.rs");
        assert_eq!(items[0].subtitle, "src");
        assert!(items[0].extra.is_some());
        assert_eq!(items[0].extra_adjust, BASENAME_BONUS);
        assert!(items[1].extra.is_none());
        assert_eq!(items[1].key, "file:README.md");
        assert_eq!(split_path("a/b/c.rs"), ("a/b", "c.rs"));
        assert_eq!(split_path("c.rs"), ("", "c.rs"));
    }

    #[test]
    fn commits_match_sha_by_prefix() {
        let items = commit_items(&[CommitEntry {
            sha: "A1B2C3D4E5".into(),
            short: "a1b2c3d".into(),
            summary: "Fix the login page".into(),
            author: "Ada".into(),
            time: 1_700_000_000,
            refs: vec!["main".into()],
        }]);
        let c = &items[0];
        assert_eq!(c.subtitle, "a1b2c3d · Ada · main");
        assert_eq!(c.time, Some(1_700_000_000));
        assert_eq!(c.exact.as_deref(), Some("a1b2c3d4e5"));
        assert_eq!(c.exact_min, 4);
        let empty = commit_items(&[CommitEntry::default()]);
        assert_eq!(empty[0].title, "(no message)");
    }

    #[test]
    fn prs_issues_runs() {
        let prs = pr_items(&[PrEntry {
            number: 42,
            title: "Add the palette".into(),
            author: "sam".into(),
            head: "palette".into(),
            base: "main".into(),
            state: "Draft".into(),
        }]);
        assert_eq!(prs[0].subtitle, "#42 · sam · palette → main");
        assert_eq!(prs[0].badge_text(), "Draft");
        assert_eq!(prs[0].exact.as_deref(), Some("42"));
        let issues = issue_items(&[IssueEntry {
            number: 7,
            title: "Crash on start".into(),
            author: "kim".into(),
            open: true,
            labels: vec!["bug".into()],
        }]);
        assert_eq!(issues[0].subtitle, "#7 · issue · kim · bug");
        assert_eq!(issues[0].badge_text(), "Open issue");
        let runs = run_items(&[RunEntry {
            id: 9,
            workflow: "CI".into(),
            title: "Fix tests".into(),
            branch: "main".into(),
            number: 311,
            event: "push".into(),
            outcome: "Failed".into(),
            time: 0,
        }]);
        assert_eq!(runs[0].title, "CI: Fix tests");
        assert_eq!(runs[0].subtitle, "#311 · main · push");
        assert_eq!(runs[0].time, None);
        assert_eq!(runs[0].badge_text(), "Failed");
        assert_eq!(runs[0].key, "run:9");
    }

    #[test]
    fn tasks_repos_commands_prefixes() {
        let t = task_items(&[TaskEntry {
            id: "npm:web:build".into(),
            name: "build".into(),
            source: "npm".into(),
            dir: "web".into(),
            description: "Bundle the app".into(),
        }]);
        assert_eq!(t[0].subtitle, "npm task in web · Bundle the app");
        assert_eq!(t[0].kind(), Kind::Task);
        let repos = repo_items(&[
            RepoEntry {
                path: "C:/r/b".into(),
                name: "b".into(),
                open: None,
                ..Default::default()
            },
            RepoEntry {
                path: "C:/r/a".into(),
                name: "a".into(),
                open: Some(1),
                ..Default::default()
            },
            RepoEntry {
                path: "C:/r/c".into(),
                name: "c".into(),
                open: Some(0),
                active: true,
                branch: Some("main".into()),
            },
            RepoEntry {
                path: "C:/R/A".into(),
                name: "a".into(),
                open: None,
                ..Default::default()
            },
        ]);
        let names: Vec<&str> = repos.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(names, ["c", "a", "b"]);
        assert_eq!(repos[0].badge_text(), "Current tab");
        assert_eq!(repos[0].subtitle, "C:/r/c · main");
        assert_eq!(repos[1].badge_text(), "Open tab");
        assert_eq!(repos[2].badge_text(), "Recent");
        let cmds = command_items(&[CommandEntry {
            id: "fetch".into(),
            title: "Fetch".into(),
            category: "Git".into(),
            shortcut: String::new(),
            keywords: "download".into(),
            hint: "Fetch all remotes and prune".into(),
        }]);
        assert_eq!(cmds[0].subtitle, "Fetch all remotes and prune");
        assert_eq!(cmds[0].badge_text(), "Git");
        assert!(!cmds[0].shortcut);
        let p = prefix_items();
        assert_eq!(p.len(), PREFIXES.len());
        assert_eq!(p[0].badge_text(), ">");
    }
}
