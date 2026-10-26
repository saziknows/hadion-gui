//! Review loop: pull requests (list, detail, create, merge, auto-merge,
//! review in the diff with threads and suggestions) and issues.

use serde::{Deserialize, Serialize};

use crate::devops::console::{Console, ConsoleRun};
use crate::devops::ctx::Ctx;
use crate::state::{Action, RepoState, Settings};

#[derive(Default)]
pub struct State {}

#[derive(Clone, Debug)]
pub enum Act {
    /// Show a pull request (from the graph, the checks section, a script).
    OpenPr(u64),
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

pub fn apply(_cx: &mut Ctx, _repo: &mut RepoState, act: Act) {
    match act {
        Act::OpenPr(_) => {}
    }
}

pub fn on_msg(_cx: &mut Ctx, _repo: &mut RepoState, msg: Msg) {
    match msg {}
}

/// A console run started by this area (or any other) ended.
pub fn on_console_done(_cx: &mut Ctx, _repo: &mut RepoState, _run: &ConsoleRun) {}

/// The page was opened (fetch what it needs the first time).
pub fn on_page_shown(_cx: &mut Ctx, _repo: &mut RepoState) {}

/// `GITVIS_SCRIPT` commands this area understands.
pub fn script(_cmd: &str, _arg: &str) -> Option<Vec<Action>> {
    None
}

pub fn show(
    ui: &mut egui::Ui,
    _repo: &mut RepoState,
    _console: &Console,
    _settings: &Settings,
    _actions: &mut Vec<Action>,
) {
    crate::views::kit::scroll_page(ui, "Pull requests and issues_page", |ui| {
        crate::views::kit::empty_state(
            ui,
            crate::views::icons::Icon::Info,
            "Pull requests and issues",
            "Coming in this branch.",
            None,
        );
    });
}

/// A dot on the rail entry and a line for its tooltip, when this area
/// needs attention (e.g. pending approvals, critical alerts, a running
/// task). `None` when nothing does.
pub fn rail_badge(_repo: &RepoState, _console: &Console) -> Option<(crate::theme::Tone, String)> {
    None
}
