//! CI control on the Actions page: dispatch with inputs, debug re-runs,
//! bulk cancel, artifacts, caches, per-workflow history and flaky jobs,
//! comparing with the last green run, workflow validation, local runs (act).

use serde::{Deserialize, Serialize};

use crate::devops::console::{Console, ConsoleRun};
use crate::devops::ctx::Ctx;
use crate::state::{Action, RepoState, Settings};

/// Tabs of the Actions page. `Runs` is the run list in `views/github.rs`;
/// every other tab is drawn by [`show_tab`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Runs,
}

impl Tab {
    pub const ALL: [Tab; 1] = [Tab::Runs];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Runs => "Runs",
        }
    }
}

#[derive(Default)]
pub struct State {
    pub tab: Tab,
}

#[derive(Clone, Debug)]
pub enum Act {
    SetTab(Tab),
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
        Act::SetTab(t) => repo.devops.ci.tab = t,
    }
}

pub fn on_msg(_cx: &mut Ctx, _repo: &mut RepoState, msg: Msg) {
    match msg {}
}

/// A console run started by this area (or any other) ended.
pub fn on_console_done(_cx: &mut Ctx, _repo: &mut RepoState, _run: &ConsoleRun) {}

/// The Actions page was opened.
pub fn on_page_shown(_cx: &mut Ctx, _repo: &mut RepoState) {}

/// `GITVIS_SCRIPT` commands this area understands.
pub fn script(_cmd: &str, _arg: &str) -> Option<Vec<Action>> {
    None
}

/// Every tab but `Runs`.
pub fn show_tab(
    ui: &mut egui::Ui,
    _repo: &mut RepoState,
    _tab: Tab,
    _console: &Console,
    _settings: &Settings,
    _actions: &mut Vec<Action>,
) {
    crate::views::kit::scroll_page(ui, "Actions_page", |ui| {
        crate::views::kit::empty_state(
            ui,
            crate::views::icons::Icon::Info,
            "Actions",
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
