//! More of git: Tools page tabs (Submodules, Git LFS, Patches, Hooks),
//! "Stash selected files" in the WIP panel, "Split this commit" in the
//! commit menu, and `gitvis hook pre-commit` (the secret check as a real
//! git hook).

use serde::{Deserialize, Serialize};

use crate::devops::console::{Console, ConsoleRun};
use crate::devops::ctx::Ctx;
use crate::state::{Action, RepoState, Settings};
use gitvis_core::Commit;

use crate::devops::hooks::{HookCx, WipList};

/// Tools page tabs drawn here (see `workbench::Tab::gitplus`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Tab {
    #[default]
    Submodules,
    Lfs,
    Patches,
    Hooks,
}

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

/// One of this area's Tools tabs became visible.
pub fn on_tab_shown(_cx: &mut Ctx, _repo: &mut RepoState, _tab: Tab) {}

pub fn show_tab(
    ui: &mut egui::Ui,
    _repo: &mut RepoState,
    _console: &Console,
    _settings: &Settings,
    _tab: Tab,
    _actions: &mut Vec<Action>,
) {
    crate::views::kit::scroll_page(ui, "Git extras_stub", |ui| {
        crate::views::kit::empty_state(
            ui,
            crate::views::icons::Icon::Info,
            "Git extras",
            "Coming in this branch.",
            None,
        );
    });
}

pub fn hook_commit_menu(_ui: &mut egui::Ui, _h: &HookCx, _c: &Commit, _actions: &mut Vec<Action>) {}

pub fn hook_wip_file_menu(
    _ui: &mut egui::Ui,
    _h: &HookCx,
    _path: &str,
    _list: WipList,
    _actions: &mut Vec<Action>,
) {
}

/// `gitvis hook <name> [args]` run by git as a hook (before the UI starts).
/// Returns the exit code, or `None` when the arguments are not a hook call.
pub fn cli_main(_args: &[std::ffi::OsString]) -> Option<i32> {
    None
}
