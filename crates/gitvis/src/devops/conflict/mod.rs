//! The merge conflict editor: base / ours / theirs side by side with the
//! result, per-hunk accept (ours, theirs, both, base), manual editing, save
//! and mark resolved. Opens from a conflicted file in the WIP panel and
//! replaces the graph on the History page while it is open.

use serde::{Deserialize, Serialize};

use crate::devops::console::{Console, ConsoleRun};
use crate::devops::ctx::Ctx;
use crate::devops::hooks::{HookCx, WipList};
use crate::state::{Action, RepoState, Settings};

#[derive(Default)]
pub struct State {
    /// The conflicted file being edited.
    pub open: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Act {
    /// Open the editor for a conflicted file.
    Open(String),
    #[allow(dead_code)] // the editor's close button
    Close,
}

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

pub fn apply(_cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::Open(path) => repo.devops.conflict.open = Some(path),
        Act::Close => repo.devops.conflict.open = None,
    }
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

/// After every load: close the editor when its file is no longer conflicted.
pub fn on_snapshot(_cx: &mut Ctx, _repo: &mut RepoState) {}

/// Does the editor replace the graph?
pub fn is_open(repo: &RepoState) -> bool {
    repo.devops.conflict.open.is_some()
}

/// The editor, drawn in place of the graph.
pub fn show_editor(
    ui: &mut egui::Ui,
    _repo: &mut RepoState,
    _console: &Console,
    _settings: &Settings,
    _actions: &mut Vec<Action>,
) {
    crate::views::kit::scroll_page(ui, "Conflict editor_stub", |ui| {
        crate::views::kit::empty_state(
            ui,
            crate::views::icons::Icon::Info,
            "Conflict editor",
            "Coming in this branch.",
            None,
        );
    });
}

/// "Resolve in the conflict editor" on conflicted files.
pub fn hook_wip_file_menu(
    _ui: &mut egui::Ui,
    _h: &HookCx,
    _path: &str,
    _list: WipList,
    _actions: &mut Vec<Action>,
) {
}
