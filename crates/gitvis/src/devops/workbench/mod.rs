//! The Tools page: the local daily workflow. Tasks (discovered and run in
//! the console), the console of every streamed process, interactive
//! rebase, bisect, file history and worktrees.
//!
//! One file per section: each holds the section's state, its controller
//! (`apply_*` / `on_*`) and its view. This file owns the contract the
//! dispatcher calls and the page frame.

mod bisect;
mod console_tab;
mod history;
mod rebase;
mod tasks;
mod ui;
mod worktrees;

use std::path::PathBuf;

use gitvis_core::devops::tools::{Installed, Tool};
use gitvis_core::devops::workbench::bisect::{BisectInfo, Verdict};
use gitvis_core::devops::workbench::history::FileHistory;
use gitvis_core::devops::workbench::rebase::{BaseChoice, Loaded, Outcome, Step};
use gitvis_core::devops::workbench::tasks::Discovery;
use gitvis_core::devops::workbench::worktrees::{AddMode, Entry};
use serde::{Deserialize, Serialize};

use crate::devops::console::{Console, ConsoleRun};
use crate::devops::ctx::Ctx;
use crate::state::{Action, DiffDoc, Page, RepoState, Settings};
use crate::theme::Tone;
use crate::views::kit::{self, NavGroup, NavItem};

/// Sections of the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum Tab {
    #[default]
    Tasks,
    Console,
    Rebase,
    Bisect,
    History,
    Worktrees,
    /// Drawn by `devops::gitplus`.
    Submodules,
    /// Drawn by `devops::gitplus`.
    Lfs,
    /// Drawn by `devops::gitplus`.
    Patches,
    /// Drawn by `devops::gitplus`.
    Hooks,
    /// Drawn by `devops::gitplus`.
    Stashes,
}

const NAV: [(&str, &[Tab]); 3] = [
    ("Run", &[Tab::Tasks, Tab::Console]),
    (
        "History",
        &[Tab::Rebase, Tab::Bisect, Tab::History, Tab::Patches],
    ),
    (
        "Repository",
        &[
            Tab::Worktrees,
            Tab::Stashes,
            Tab::Submodules,
            Tab::Lfs,
            Tab::Hooks,
        ],
    ),
];

impl Tab {
    /// The git-extras tab this is, when `devops::gitplus` draws it.
    pub fn gitplus(self) -> Option<crate::devops::gitplus::Tab> {
        use crate::devops::gitplus::Tab as G;
        Some(match self {
            Tab::Submodules => G::Submodules,
            Tab::Lfs => G::Lfs,
            Tab::Patches => G::Patches,
            Tab::Hooks => G::Hooks,
            Tab::Stashes => G::Stashes,
            _ => return None,
        })
    }
}

impl Tab {
    pub fn label(self) -> &'static str {
        match self {
            Tab::Tasks => "Tasks",
            Tab::Console => "Console",
            Tab::Rebase => "Interactive rebase",
            Tab::Bisect => "Bisect",
            Tab::History => "File history",
            Tab::Worktrees => "Worktrees",
            Tab::Submodules => "Submodules",
            Tab::Lfs => "Git LFS",
            Tab::Patches => "Patches",
            Tab::Hooks => "Hooks",
            Tab::Stashes => "Stashes",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Tab::Tasks => {
                "Scripts, targets and commands found in this repository: package.json, Makefile, Cargo, justfile, Taskfile and more"
            }
            Tab::Console => {
                "Every process started from gitvis for this repository, with its output"
            }
            Tab::Rebase => {
                "Reorder, reword, squash, fix up, edit or drop commits, with a preview and the exact todo file"
            }
            Tab::Bisect => {
                "Find the commit that broke something: mark commits good or bad, or let a test command decide"
            }
            Tab::History => "Every commit that changed one file, across renames, with its diff",
            Tab::Worktrees => {
                "Checkouts of this repository in other folders: state, add, lock, move, remove, prune"
            }
            Tab::Submodules => "Submodules: status, init, update, sync, open as a tab",
            Tab::Lfs => "Large files tracked by Git LFS: patterns, files, locks",
            Tab::Patches => "Create patch files from commits, apply patches and mailboxes",
            Tab::Hooks => {
                "Git hooks: list, enable, edit; the gitvis secret check as a pre-commit hook; pre-commit and semgrep"
            }
            Tab::Stashes => {
                "Stashes with their changes: apply, pop, drop, branch; stash selected files"
            }
        }
    }

    pub fn from_name(s: &str) -> Option<Tab> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "tasks" | "task" => Tab::Tasks,
            "console" => Tab::Console,
            "rebase" | "interactive-rebase" => Tab::Rebase,
            "bisect" => Tab::Bisect,
            "history" | "file-history" => Tab::History,
            "worktrees" | "worktree" => Tab::Worktrees,
            "submodules" | "submodule" => Tab::Submodules,
            "lfs" => Tab::Lfs,
            "patches" | "patch" => Tab::Patches,
            "hooks" | "hook" => Tab::Hooks,
            "stashes" | "stash" => Tab::Stashes,
            _ => return None,
        })
    }
}

/// One finished task run, kept across sessions.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunRecord {
    pub task: String,
    /// Unix seconds when it ended.
    pub at: i64,
    pub status: String,
    pub ok: bool,
    pub ms: u64,
    pub code: Option<i32>,
    pub args: String,
}

/// Runs kept in the preferences, newest last.
const MAX_RECORDS: usize = 200;
/// Recently run tasks remembered.
const MAX_RECENT: usize = 8;

#[derive(Default)]
pub struct State {
    pub tab: Tab,
    pub tasks: tasks::Tasks,
    /// The run shown on the Console tab.
    pub console_selected: Option<u64>,
    pub rebase: rebase::Rebase,
    pub bisect: bisect::Bisect,
    pub worktrees: worktrees::Worktrees,
    pub history: history::History,
    pub favourites: Vec<String>,
    /// Task ids, most recent first.
    pub recent: Vec<String>,
    pub records: Vec<RunRecord>,
}

/// What the views ask for.
#[derive(Clone, Debug)]
pub enum Act {
    SetTab(Tab),
    // Tasks
    DiscoverTasks,
    SelectTask(Option<String>),
    SelectTaskIndex(usize),
    RunTask(String),
    RunTaskIndex(usize),
    /// A run that needed a confirmation: the task and its extra arguments.
    RunTaskConfirmed(String, Vec<String>),
    ToggleFavourite(String),
    // Console
    SelectRun(u64),
    // Interactive rebase
    /// From the graph: edit the commits from this one up to HEAD.
    RebaseFrom(String),
    LoadRebase(String),
    RebaseStep(usize, Step),
    RebaseMove(usize, usize),
    RebaseFocus(Option<usize>),
    RebaseReset,
    StartRebase,
    RunRebase,
    // Bisect
    RefreshBisect,
    StartBisect,
    StartBisectConfirmed {
        bad: String,
        goods: Vec<String>,
        run: Option<String>,
        cmd_shell: bool,
    },
    MarkBisect(Verdict),
    RunBisect,
    RunBisectConfirmed {
        cmd: String,
        cmd_shell: bool,
    },
    ResetBisect,
    ResetBisectConfirmed,
    // Worktrees
    LoadWorktrees,
    SelectWorktree(Option<String>),
    SelectWorktreeIndex(usize),
    /// Fill the add form with a new branch name (scripts).
    SetWorktreeBranch(String),
    AddWorktree,
    AddWorktreeConfirmed(String, AddMode),
    RemoveWorktree(String),
    RemoveWorktreeConfirmed(String, bool, bool),
    LockWorktree(String),
    LockWorktreeConfirmed(String, String),
    UnlockWorktree(String),
    UnlockWorktreeConfirmed(String),
    MoveWorktree(String),
    MoveWorktreeConfirmed(String, String),
    PrunePreview,
    Prune,
    PruneConfirmed,
    // File history
    FileHistory(String),
    SelectHistory(Option<usize>),
    OpenVersion {
        sha: String,
        path: String,
    },
}

/// Replies of background work.
pub enum Msg {
    Discovered(Result<Discovery, String>, Vec<(String, bool)>),
    Tools(Vec<(Tool, Option<Installed>)>),
    RebaseSuggestions(Vec<BaseChoice>, bool),
    RebaseLoaded(Result<Loaded, String>),
    RebaseDone(Result<Outcome, String>),
    Bisect(Box<Result<BisectInfo, String>>),
    BisectChanged(Result<String, String>),
    Worktrees(Result<Vec<Entry>, String>),
    PrunePreview(Result<String, String>),
    WorktreeChanged,
    TrackedFiles(Result<Vec<String>, String>),
    History(Result<FileHistory, String>),
    HistoryDiff(String, Result<DiffDoc, String>),
    VersionSaved(Result<PathBuf, String>),
}

impl Msg {
    /// One line for the activity log (`Err` when the work failed).
    pub fn summary(&self) -> Result<String, String> {
        use gitvis_core::devops::workbench::history::FileHistory as Fh;
        match self {
            Msg::Discovered(Ok(d), _) => Ok(format!(
                "{} tasks in {} projects · {} folders · {} files read · {} ms",
                d.tasks.len(),
                d.projects.len(),
                d.dirs_scanned,
                d.files_read,
                d.elapsed_ms
            )),
            Msg::Discovered(Err(e), _) => Err(e.clone()),
            Msg::Tools(t) => Ok(format!(
                "{} tools checked, {} installed",
                t.len(),
                t.iter().filter(|(_, i)| i.is_some()).count()
            )),
            Msg::RebaseSuggestions(s, _) => Ok(format!("{} base suggestions", s.len())),
            Msg::RebaseLoaded(Ok(l)) => Ok(format!(
                "{} commits between {} and HEAD",
                l.commits.len(),
                l.base
            )),
            Msg::RebaseLoaded(Err(e)) => Err(e.clone()),
            Msg::RebaseDone(Ok(o)) => Ok(match o {
                Outcome::Done(s) | Outcome::Stopped(s) | Outcome::Conflict(s) => s.clone(),
            }),
            Msg::RebaseDone(Err(e)) => Err(e.clone()),
            Msg::Bisect(r) => match r.as_ref() {
                Ok(b) if b.active => {
                    Ok(format!("Bisecting: {} commits tested", b.log.entries.len()))
                }
                Ok(_) => Ok("No bisection running".to_owned()),
                Err(e) => Err(e.clone()),
            },
            Msg::BisectChanged(r) => r.clone(),
            Msg::Worktrees(Ok(w)) => Ok(format!("{} worktrees", w.len())),
            Msg::Worktrees(Err(e)) => Err(e.clone()),
            Msg::PrunePreview(r) => r.clone().map(|t| {
                let n = gitvis_core::devops::workbench::worktrees::parse_prune(&t).len();
                format!("{n} worktree entries to prune")
            }),
            Msg::WorktreeChanged => Ok(String::new()),
            Msg::TrackedFiles(Ok(f)) => Ok(format!("{} tracked files", f.len())),
            Msg::TrackedFiles(Err(e)) => Err(e.clone()),
            Msg::History(Ok(h)) => {
                let h: &Fh = h;
                Ok(format!(
                    "{} commits touch {} · {} renames · {} ms{}",
                    h.commits.len(),
                    h.path,
                    h.renames(),
                    h.elapsed_ms,
                    if h.truncated { " · capped" } else { "" }
                ))
            }
            Msg::History(Err(e)) => Err(e.clone()),
            Msg::HistoryDiff(k, Ok(d)) => Ok(format!("Diff of {k}: {} hunks", d.diff.hunks.len())),
            Msg::HistoryDiff(_, Err(e)) => Err(e.clone()),
            Msg::VersionSaved(Ok(p)) => Ok(format!("Saved {}", p.display())),
            Msg::VersionSaved(Err(e)) => Err(e.clone()),
        }
    }
}

/// Persisted per repository.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub favourites: Vec<String>,
    pub recent: Vec<String>,
    pub runs: Vec<RunRecord>,
    /// Bisect test command and shell (`cmd` when true).
    pub bisect_cmd: String,
    pub bisect_cmd_shell: bool,
}

impl State {
    pub fn prefs(&self) -> Prefs {
        Prefs {
            favourites: self.favourites.clone(),
            recent: self.recent.clone(),
            runs: self.records.clone(),
            bisect_cmd: self.bisect.test_cmd.clone(),
            bisect_cmd_shell: self.bisect.cmd_shell,
        }
    }

    pub fn apply_prefs(&mut self, p: &Prefs) {
        self.favourites = p.favourites.clone();
        self.recent = p.recent.clone();
        self.records = p.runs.clone();
        self.bisect.test_cmd = p.bisect_cmd.clone();
        self.bisect.cmd_shell = p.bisect_cmd_shell;
    }

    /// Remember a finished task run.
    fn record(&mut self, r: RunRecord) {
        self.records.push(r);
        let extra = self.records.len().saturating_sub(MAX_RECORDS);
        self.records.drain(..extra);
    }

    fn touch_recent(&mut self, id: &str) {
        self.recent.retain(|r| r != id);
        self.recent.insert(0, id.to_owned());
        self.recent.truncate(MAX_RECENT);
    }
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::SetTab(t) => {
            repo.devops.workbench.tab = t;
            tab_shown(cx, repo);
        }
        Act::DiscoverTasks
        | Act::SelectTask(_)
        | Act::SelectTaskIndex(_)
        | Act::RunTask(_)
        | Act::RunTaskIndex(_)
        | Act::RunTaskConfirmed(..)
        | Act::ToggleFavourite(_) => tasks::apply(cx, repo, act),
        Act::SelectRun(id) => repo.devops.workbench.console_selected = Some(id),
        Act::RebaseFrom(_)
        | Act::LoadRebase(_)
        | Act::RebaseStep(..)
        | Act::RebaseMove(..)
        | Act::RebaseFocus(_)
        | Act::RebaseReset
        | Act::StartRebase
        | Act::RunRebase => rebase::apply(cx, repo, act),
        Act::RefreshBisect
        | Act::StartBisect
        | Act::StartBisectConfirmed { .. }
        | Act::MarkBisect(_)
        | Act::RunBisect
        | Act::RunBisectConfirmed { .. }
        | Act::ResetBisect
        | Act::ResetBisectConfirmed => bisect::apply(cx, repo, act),
        Act::LoadWorktrees
        | Act::SelectWorktree(_)
        | Act::SelectWorktreeIndex(_)
        | Act::SetWorktreeBranch(_)
        | Act::AddWorktree
        | Act::AddWorktreeConfirmed(..)
        | Act::RemoveWorktree(_)
        | Act::RemoveWorktreeConfirmed(..)
        | Act::LockWorktree(_)
        | Act::LockWorktreeConfirmed(..)
        | Act::UnlockWorktree(_)
        | Act::UnlockWorktreeConfirmed(_)
        | Act::MoveWorktree(_)
        | Act::MoveWorktreeConfirmed(..)
        | Act::PrunePreview
        | Act::Prune
        | Act::PruneConfirmed => worktrees::apply(cx, repo, act),
        Act::FileHistory(_) | Act::SelectHistory(_) | Act::OpenVersion { .. } => {
            history::apply(cx, repo, act)
        }
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    match msg {
        Msg::Discovered(..) | Msg::Tools(_) => tasks::on_msg(cx, repo, msg),
        Msg::RebaseSuggestions(..) | Msg::RebaseLoaded(_) | Msg::RebaseDone(_) => {
            rebase::on_msg(cx, repo, msg)
        }
        Msg::Bisect(_) | Msg::BisectChanged(_) => bisect::on_msg(cx, repo, msg),
        Msg::Worktrees(_) | Msg::PrunePreview(_) | Msg::WorktreeChanged => {
            worktrees::on_msg(cx, repo, msg)
        }
        Msg::TrackedFiles(_) | Msg::History(_) | Msg::HistoryDiff(..) | Msg::VersionSaved(_) => {
            history::on_msg(cx, repo, msg)
        }
    }
}

/// A console run started by this area (or any other) ended.
pub fn on_console_done(cx: &mut Ctx, repo: &mut RepoState, run: &ConsoleRun) {
    tasks::on_console_done(cx, repo, run);
    bisect::on_console_done(cx, repo, run);
}

/// The page was opened (fetch what it needs the first time).
pub fn on_page_shown(cx: &mut Ctx, repo: &mut RepoState) {
    tab_shown(cx, repo);
}

/// The current tab became visible: load what it shows.
fn tab_shown(cx: &mut Ctx, repo: &mut RepoState) {
    match repo.devops.workbench.tab {
        Tab::Tasks => tasks::shown(cx, repo),
        Tab::Console => {}
        Tab::Rebase => rebase::shown(cx, repo),
        Tab::Bisect => bisect::shown(cx, repo),
        Tab::History => history::shown(cx, repo),
        Tab::Worktrees => worktrees::shown(cx, repo),
        t => {
            if let Some(g) = t.gitplus() {
                crate::devops::gitplus::on_tab_shown(cx, repo, g);
            }
        }
    }
}

fn show_tab(tab: Tab) -> Vec<Action> {
    vec![Action::ShowPage(Page::Workbench), Act::SetTab(tab).into()]
}

/// `GITVIS_SCRIPT` commands this area understands.
pub fn script(cmd: &str, arg: &str) -> Option<Vec<Action>> {
    let num = arg.trim().parse::<usize>().ok();
    // `tools-history`, `tools-rebase`, …: the same commands, for when
    // another area answers to the short name first.
    let cmd = match cmd.strip_prefix("tools-") {
        Some(rest)
            if matches!(
                rest,
                "task" | "rebase" | "bisect" | "history" | "worktrees" | "worktree"
            ) =>
        {
            rest
        }
        _ => cmd,
    };
    Some(match cmd {
        "tools" | "workbench" => vec![Action::ShowPage(Page::Workbench)],
        "tools-tab" => show_tab(Tab::from_name(arg)?),
        "select-task" => {
            let mut v = show_tab(Tab::Tasks);
            v.push(Act::SelectTaskIndex(num?).into());
            v
        }
        "task" => {
            let mut v = show_tab(Tab::Tasks);
            v.push(Act::RunTaskIndex(num?).into());
            v
        }
        "rebase" => {
            let mut v = show_tab(Tab::Rebase);
            if !arg.trim().is_empty() {
                v.push(Act::LoadRebase(arg.trim().to_owned()).into());
            }
            v
        }
        "rebase-action" => {
            let mut it = arg.split_whitespace();
            let row = it.next()?.parse().ok()?;
            let step = Step::from_name(it.next()?)?;
            vec![Act::RebaseStep(row, step).into()]
        }
        "rebase-move" => {
            let mut it = arg.split_whitespace();
            let from = it.next()?.parse().ok()?;
            let to = it.next()?.parse().ok()?;
            vec![Act::RebaseMove(from, to).into()]
        }
        "rebase-focus" => vec![Act::RebaseFocus(num).into()],
        "rebase-start" => vec![Act::StartRebase.into()],
        // Opens the confirmation and runs the plan without waiting for
        // it: scratch repositories only.
        "rebase-run-now" => vec![Act::StartRebase.into(), Act::RunRebase.into()],
        "bisect" => show_tab(Tab::Bisect),
        "bisect-start" => vec![Act::StartBisect.into()],
        "bisect-mark" => {
            let v = match arg.trim() {
                "good" => Verdict::Good,
                "bad" => Verdict::Bad,
                "skip" => Verdict::Skip,
                _ => return None,
            };
            vec![Act::MarkBisect(v).into()]
        }
        "bisect-run" => vec![Act::RunBisect.into()],
        // Skips the confirmation: scratch repositories only.
        "bisect-run-now" if !arg.trim().is_empty() => vec![
            Act::RunBisectConfirmed {
                cmd: arg.trim().to_owned(),
                cmd_shell: false,
            }
            .into(),
        ],
        "bisect-reset" => vec![Act::ResetBisect.into()],
        "history" => {
            let mut v = show_tab(Tab::History);
            if !arg.trim().is_empty() {
                v.push(Act::FileHistory(arg.trim().to_owned()).into());
            }
            v
        }
        "history-item" => vec![Act::SelectHistory(num).into()],
        "worktrees" => show_tab(Tab::Worktrees),
        "worktree" => {
            let mut v = show_tab(Tab::Worktrees);
            v.push(Act::SelectWorktreeIndex(num?).into());
            v
        }
        "worktree-add" => {
            let mut v = Vec::new();
            if !arg.trim().is_empty() {
                v.push(Act::SetWorktreeBranch(arg.trim().to_owned()).into());
            }
            v.push(Act::AddWorktree.into());
            v
        }
        "worktree-remove" => vec![Act::RemoveWorktree(arg.trim().to_owned()).into()],
        "prune-preview" => vec![Act::PrunePreview.into()],
        _ => return None,
    })
}

pub fn show(
    ui: &mut egui::Ui,
    repo: &mut RepoState,
    console: &Console,
    settings: &Settings,
    actions: &mut Vec<Action>,
) {
    let tab = repo.devops.workbench.tab;
    let order: Vec<Tab> = NAV.iter().flat_map(|(_, t)| t.iter().copied()).collect();
    let counts = nav_counts(repo, console);
    let mut clicked: Option<Tab> = None;
    kit::page_with_nav(
        ui,
        "wb_nav",
        |ui| {
            let groups: Vec<NavGroup> = NAV
                .iter()
                .map(|(title, tabs)| NavGroup {
                    title,
                    items: tabs
                        .iter()
                        .map(|t| {
                            let item = NavItem::new(t.label()).hint(t.hint());
                            match counts.iter().find(|(k, _, _)| k == t) {
                                Some((_, n, tone)) => item.count(*n, *tone),
                                None => item,
                            }
                        })
                        .collect(),
                })
                .collect();
            let selected = order.iter().position(|t| *t == tab).unwrap_or(0);
            if let Some(i) = kit::section_nav(ui, &groups, selected) {
                clicked = order.get(i).copied();
            }
        },
        |ui| match tab {
            Tab::Tasks => tasks::show(ui, repo, console, settings, actions),
            Tab::Console => console_tab::show(ui, repo, console, actions),
            Tab::Rebase => rebase::show(ui, repo, settings, actions),
            Tab::Bisect => bisect::show(ui, repo, console, actions),
            Tab::History => history::show(ui, repo, actions),
            Tab::Worktrees => worktrees::show(ui, repo, actions),
            t => {
                if let Some(g) = t.gitplus() {
                    crate::devops::gitplus::show_tab(ui, repo, console, settings, g, actions);
                }
            }
        },
    );
    if let Some(t) = clicked {
        actions.push(Act::SetTab(t).into());
    }
}

/// Counts on the navigation: tasks found, running processes, worktrees,
/// and a warning on a rebase or bisect in progress.
fn nav_counts(repo: &RepoState, console: &Console) -> Vec<(Tab, usize, Tone)> {
    let wb = &repo.devops.workbench;
    let mut out = Vec::new();
    if let Some(Ok(d)) = &wb.tasks.discovery {
        out.push((Tab::Tasks, d.tasks.len(), Tone::Neutral));
    }
    let running = console.for_repo(repo.id).filter(|r| r.running()).count();
    let total = console.for_repo(repo.id).count();
    if running > 0 {
        out.push((Tab::Console, running, Tone::Accent));
    } else if total > 0 {
        out.push((Tab::Console, total, Tone::Neutral));
    }
    let pending = repo.snapshot.as_ref().map(|s| s.pending);
    if pending == Some(gitvis_core::Pending::Rebase) {
        out.push((Tab::Rebase, 1, Tone::Warning));
    }
    if pending == Some(gitvis_core::Pending::Bisect) {
        let tested = match &wb.bisect.info {
            Some(Ok(b)) => b.log.entries.len().max(1),
            _ => 1,
        };
        out.push((Tab::Bisect, tested, Tone::Warning));
    }
    if let Some(Ok(w)) = &wb.worktrees.list {
        out.push((Tab::Worktrees, w.len(), Tone::Neutral));
    }
    if let Some(Ok(h)) = &wb.history.result {
        out.push((Tab::History, h.commits.len(), Tone::Neutral));
    }
    out
}

/// A dot on the rail entry and a line for its tooltip, when this area
/// needs attention (e.g. pending approvals, critical alerts, a running
/// task). `None` when nothing does.
pub fn rail_badge(repo: &RepoState, console: &Console) -> Option<(crate::theme::Tone, String)> {
    let running = console.for_repo(repo.id).filter(|r| r.running()).count();
    let pending = repo.snapshot.as_ref().map(|s| s.pending);
    let state = match pending {
        Some(gitvis_core::Pending::Rebase) => Some("Rebase in progress"),
        Some(gitvis_core::Pending::Bisect) => Some("Bisect in progress"),
        _ => None,
    };
    let tasks = (running > 0).then(|| {
        format!(
            "{running} task{} running",
            if running == 1 { "" } else { "s" }
        )
    });
    match (state, tasks) {
        (Some(s), Some(t)) => Some((Tone::Warning, format!("{s} · {t}"))),
        (Some(s), None) => Some((Tone::Warning, s.to_owned())),
        (None, Some(t)) => Some((Tone::Accent, t)),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::DevOpsAction;

    fn acts(cmd: &str, arg: &str) -> Vec<Act> {
        script(cmd, arg)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|a| match a {
                Action::DevOps(DevOpsAction::Workbench(a)) => Some(a),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn script_commands() {
        assert!(matches!(
            acts("rebase-action", "2 squash").as_slice(),
            [Act::RebaseStep(2, Step::Squash)]
        ));
        assert!(matches!(
            acts("tools-tab", "worktrees").as_slice(),
            [Act::SetTab(Tab::Worktrees)]
        ));
        assert!(matches!(
            acts("history", "src/main.rs").as_slice(),
            [Act::SetTab(Tab::History), Act::FileHistory(p)] if p == "src/main.rs"
        ));
        assert!(matches!(
            acts("tools-history", "a.rs").as_slice(),
            [Act::SetTab(Tab::History), Act::FileHistory(_)]
        ));
        assert!(matches!(
            acts("task", "3").as_slice(),
            [Act::SetTab(Tab::Tasks), Act::RunTaskIndex(3)]
        ));
        assert!(matches!(
            acts("bisect-mark", "skip").as_slice(),
            [Act::MarkBisect(Verdict::Skip)]
        ));
        assert!(script("bisect-mark", "maybe").is_none());
        assert!(script("select-task", "x").is_none());
        assert!(script("not-a-command", "").is_none());
        assert_eq!(
            script("tools", "").map(|v| v.len()),
            Some(1),
            "just the page"
        );
    }

    #[test]
    fn tab_names() {
        for t in [
            Tab::Tasks,
            Tab::Console,
            Tab::Rebase,
            Tab::Bisect,
            Tab::History,
            Tab::Worktrees,
            Tab::Submodules,
            Tab::Lfs,
            Tab::Patches,
            Tab::Hooks,
            Tab::Stashes,
        ] {
            let name = match t {
                Tab::Tasks => "tasks",
                Tab::Console => "console",
                Tab::Rebase => "rebase",
                Tab::Bisect => "bisect",
                Tab::History => "history",
                Tab::Worktrees => "worktrees",
                Tab::Submodules => "submodules",
                Tab::Lfs => "lfs",
                Tab::Patches => "patches",
                Tab::Hooks => "hooks",
                Tab::Stashes => "stashes",
            };
            assert_eq!(Tab::from_name(name), Some(t));
            assert!(!t.hint().is_empty());
        }
        assert_eq!(Tab::from_name("nope"), None);
    }

    #[test]
    fn rail_badge_counts_running_runs() {
        let repo = RepoState::new(7, std::path::PathBuf::from("."));
        let mut console = Console::default();
        assert!(rail_badge(&repo, &console).is_none());
        console.add(
            1,
            Some(7),
            "npm run dev".into(),
            gitvis_core::devops::proc::ProcSpec::new("npm", "."),
        );
        console.add(
            2,
            Some(8),
            "other repo".into(),
            gitvis_core::devops::proc::ProcSpec::new("npm", "."),
        );
        let (tone, text) = rail_badge(&repo, &console).unwrap();
        assert_eq!(tone, Tone::Accent);
        assert_eq!(text, "1 task running");
    }

    #[test]
    fn prefs_round_trip() {
        let mut s = State::default();
        s.favourites.push("npm::build".into());
        s.touch_recent("npm::test");
        s.touch_recent("npm::build");
        s.touch_recent("npm::test");
        assert_eq!(s.recent, vec!["npm::test", "npm::build"]);
        for i in 0..(MAX_RECORDS + 5) {
            s.record(RunRecord {
                task: format!("t{i}"),
                ..Default::default()
            });
        }
        assert_eq!(s.records.len(), MAX_RECORDS);
        assert_eq!(s.records[0].task, "t5", "the oldest go first");
        let json = serde_json::to_string(&s.prefs()).unwrap();
        let back: Prefs = serde_json::from_str(&json).unwrap();
        let mut t = State::default();
        t.apply_prefs(&back);
        assert_eq!(t.favourites, s.favourites);
        assert_eq!(t.records.len(), MAX_RECORDS);
        let empty: Prefs = serde_json::from_str("{}").unwrap();
        assert!(empty.favourites.is_empty());
    }
}
