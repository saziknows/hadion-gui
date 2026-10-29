//! Extension hooks: places in shared views where areas add their own items,
//! so no area has to edit another area's (or the app's) view code.
//!
//! | Hook | Called from | Areas |
//! |---|---|---|
//! | [`commit_menu`] | graph commit context menu (`views/menus.rs`) | history, gitplus, ai |
//! | [`commit_panel`] | commit details panel, after the details (`views/details.rs`) | history, automation, ai |
//! | [`commit_file_menu`] | file row menu in the commit panel | history |
//! | [`wip_file_menu`] | file row menu in the WIP panel | gitplus, conflict |
//! | [`wip_message_tools`] | above the commit message box | ai, automation |
//! | [`multi_selection`] | details panel when several commits are picked | history |
//! | [`log_viewer_tools`] | Actions job log header (`views/github.rs`; [`LogHookCx`]) | ai |
//! | [`pr_tools`] | pull request conversation (Review page) | ai, automation |
//! | [`new_pr_tools`] | new pull request form, next to the body | ai |
//! | [`status_items`] | status bar, right side | packaging, automation |
//!
//! Hooks only read ([`HookCx`]) and push actions; an area's own `apply`
//! does the work. Keep each contribution small (a menu item, a button, a
//! short line); open the area's own page or panel for anything bigger.

use std::collections::HashMap;

use egui::Ui;
use hadion_core::github::GitHubData;
use hadion_core::{Commit, Oid, Snapshot};

use crate::devops::global::{GlobalView, Globals};
use crate::devops::{DevOpsState, ai, automation, conflict, gitplus, history, packaging};
use crate::state::{Action, Settings};

/// What a hook can read about the repository it is drawn for.
pub struct HookCx<'a> {
    pub snap: &'a Snapshot,
    pub devops: &'a DevOpsState,
    pub settings: &'a Settings,
    pub github_data: Option<&'a GitHubData>,
    /// An operation is running in this repository.
    pub busy: bool,
    /// The commit message being written in the WIP panel.
    pub commit_message: &'a str,
    /// Commit details loaded so far (the selected commit's, at least).
    pub details: &'a HashMap<hadion_core::Oid, hadion_core::CommitDetails>,
}

impl<'a> HookCx<'a> {
    /// The hook context of a loaded repository (`None` before its first
    /// snapshot).
    pub fn of(repo: &'a crate::state::RepoState, settings: &'a Settings) -> Option<Self> {
        Some(Self {
            snap: repo.snapshot.as_ref()?,
            devops: &repo.devops,
            settings,
            github_data: repo.github.as_ref(),
            busy: repo.busy.is_some(),
            commit_message: &repo.commit_message,
            details: &repo.details,
        })
    }
}

/// Which list of the WIP panel a file row is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WipList {
    Unstaged,
    Staged,
    Conflicted,
}

/// Items appended to a commit's context menu in the graph.
pub fn commit_menu(ui: &mut Ui, h: &HookCx, c: &Commit, actions: &mut Vec<Action>) {
    history::hook_commit_menu(ui, h, c, actions);
    gitplus::hook_commit_menu(ui, h, c, actions);
    ai::hook_commit_menu(ui, h, c, actions);
}

/// Lines or buttons shown in the commit details panel, after the details
/// block (signature verification, issue links, "explain this commit").
pub fn commit_panel(ui: &mut Ui, h: &HookCx, c: &Commit, actions: &mut Vec<Action>) {
    history::hook_commit_panel(ui, h, c, actions);
    automation::hook_commit_panel(ui, h, c, actions);
    ai::hook_commit_panel(ui, h, c, actions);
}

/// Items appended to a changed file's menu in the commit panel.
pub fn commit_file_menu(
    ui: &mut Ui,
    h: &HookCx,
    c: &Commit,
    path: &str,
    actions: &mut Vec<Action>,
) {
    history::hook_commit_file_menu(ui, h, c, path, actions);
}

/// Items appended to a file's menu in the WIP panel.
pub fn wip_file_menu(
    ui: &mut Ui,
    h: &HookCx,
    path: &str,
    list: WipList,
    actions: &mut Vec<Action>,
) {
    conflict::hook_wip_file_menu(ui, h, path, list, actions);
    gitplus::hook_wip_file_menu(ui, h, path, list, actions);
}

/// Small buttons shown above the commit message box.
pub fn wip_message_tools(ui: &mut Ui, h: &HookCx, actions: &mut Vec<Action>) {
    ai::hook_wip_message_tools(ui, h, actions);
    automation::hook_wip_message_tools(ui, h, actions);
}

/// The details panel when several commits are picked (`RepoState::multi`).
/// Returns true when it drew something (the single-commit panel is then
/// skipped).
pub fn multi_selection(ui: &mut Ui, h: &HookCx, picked: &[Oid], actions: &mut Vec<Action>) -> bool {
    history::hook_multi_selection(ui, h, picked, actions)
}

/// What the job log viewer can offer a hook: it draws while the CI area's
/// state is borrowed, so it carries only the AI state (a disjoint field).
pub struct LogHookCx<'a> {
    pub settings: &'a Settings,
    pub ai: &'a ai::State,
}

/// Buttons in the header of the Actions job log viewer.
pub fn log_viewer_tools(ui: &mut Ui, h: &LogHookCx, run: u64, job: u64, actions: &mut Vec<Action>) {
    ai::hook_log_viewer_tools(ui, h, run, job, actions);
}

/// A block in a pull request's conversation tab (summary, issue links).
pub fn pr_tools(ui: &mut Ui, h: &HookCx, number: u64, actions: &mut Vec<Action>) {
    ai::hook_pr_tools(ui, h, number, actions);
    automation::hook_pr_tools(ui, h, number, actions);
}

/// Buttons next to the body of the new pull request form.
pub fn new_pr_tools(ui: &mut Ui, h: &HookCx, actions: &mut Vec<Action>) {
    ai::hook_new_pr_tools(ui, h, actions);
}

/// Items on the right of the status bar (update available, notifications
/// paused, …). Drawn right-to-left.
pub fn status_items(ui: &mut Ui, g: &Globals, v: &GlobalView, actions: &mut Vec<Action>) {
    packaging::hook_status_items(ui, &g.packaging, v, actions);
    automation::hook_status_items(ui, &g.automation, v, actions);
}
