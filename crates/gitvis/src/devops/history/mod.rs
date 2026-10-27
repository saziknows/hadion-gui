//! History browsing: the Compare page (two revisions: commits, files, diff),
//! actions on several commits picked in the graph (compare, squash, reword,
//! drop, cherry-pick, create patches), graph filters (author, path, date),
//! the file tree at a commit, inline blame with "blame the previous
//! version", and signature verification (badges in the graph, details in
//! the commit panel).

use serde::{Deserialize, Serialize};

use crate::devops::console::{Console, ConsoleRun};
use crate::devops::ctx::Ctx;
use crate::state::{Action, RepoState, Settings};
use gitvis_core::{Commit, Oid};

use crate::devops::hooks::HookCx;

#[derive(Default)]
pub struct State {}

#[derive(Clone, Debug)]
pub enum Act {}

pub enum Msg {}

impl Msg {
    /// One line for the activity log (`Err` when the work failed).
    pub fn summary(&self) -> Result<String, String> {
        match *self {}
    }
}

/// Persisted per repository.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {}

impl State {
    pub fn prefs(&self) -> Prefs {
        Prefs::default()
    }

    pub fn apply_prefs(&mut self, _p: &Prefs) {}
}

pub fn apply(_cx: &mut Ctx, _repo: &mut RepoState, act: Act) {
    match act {}
}

pub fn on_msg(_cx: &mut Ctx, _repo: &mut RepoState, msg: Msg) {
    match msg {}
}

/// A console run of this repository ended.
pub fn on_console_done(_cx: &mut Ctx, _repo: &mut RepoState, _run: &ConsoleRun) {}

/// `GITVIS_SCRIPT` commands this area understands.
pub fn script(_cmd: &str, _arg: &str) -> Option<Vec<Action>> {
    None
}

/// After every load.
pub fn on_snapshot(_cx: &mut Ctx, _repo: &mut RepoState) {}

/// The Compare page was opened.
pub fn on_page_shown(_cx: &mut Ctx, _repo: &mut RepoState) {}

/// The Compare page (`Page::Compare`).
pub fn show_compare(
    ui: &mut egui::Ui,
    _repo: &mut RepoState,
    _console: &Console,
    _settings: &Settings,
    _actions: &mut Vec<Action>,
) {
    crate::views::kit::scroll_page(ui, "Compare_stub", |ui| {
        crate::views::kit::empty_state(
            ui,
            crate::views::icons::Icon::Info,
            "Compare",
            "Coming in this branch.",
            None,
        );
    });
}

pub fn hook_commit_menu(_ui: &mut egui::Ui, _h: &HookCx, _c: &Commit, _actions: &mut Vec<Action>) {}

pub fn hook_commit_panel(_ui: &mut egui::Ui, _h: &HookCx, _c: &Commit, _actions: &mut Vec<Action>) {
}

pub fn hook_commit_file_menu(
    _ui: &mut egui::Ui,
    _h: &HookCx,
    _c: &Commit,
    _path: &str,
    _actions: &mut Vec<Action>,
) {
}

/// The details panel for several picked commits; true when drawn.
pub fn hook_multi_selection(
    _ui: &mut egui::Ui,
    _h: &HookCx,
    _picked: &[Oid],
    _actions: &mut Vec<Action>,
) -> bool {
    false
}
