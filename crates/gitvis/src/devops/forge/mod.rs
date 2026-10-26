//! GitLab and Azure DevOps: merge / pull requests and pipelines for
//! repositories whose remote is not on GitHub. Drawn on the Review and
//! Actions pages when `RepoState::forge_remote` is set.

use serde::{Deserialize, Serialize};

use crate::devops::console::{Console, ConsoleRun};
use crate::devops::ctx::Ctx;
use crate::state::Page;
use crate::state::{Action, RepoState, Settings};

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

/// A console run started by this area (or any other) ended.
pub fn on_console_done(_cx: &mut Ctx, _repo: &mut RepoState, _run: &ConsoleRun) {}

/// The Review or Actions page was opened on a GitLab / Azure DevOps remote.
pub fn on_page_shown(_cx: &mut Ctx, _repo: &mut RepoState, _page: Page) {}

/// `GITVIS_SCRIPT` commands this area understands.
pub fn script(_cmd: &str, _arg: &str) -> Option<Vec<Action>> {
    None
}

/// The Review page for a GitLab / Azure DevOps remote.
pub fn show_review(
    ui: &mut egui::Ui,
    _repo: &mut RepoState,
    _console: &Console,
    _settings: &Settings,
    _actions: &mut Vec<Action>,
) {
    crate::views::kit::scroll_page(ui, "Merge requests_page", |ui| {
        crate::views::kit::empty_state(
            ui,
            crate::views::icons::Icon::Info,
            "Merge requests",
            "Coming in this branch.",
            None,
        );
    });
}

/// The Actions page for a GitLab / Azure DevOps remote (pipelines).
pub fn show_pipelines(
    ui: &mut egui::Ui,
    _repo: &mut RepoState,
    _console: &Console,
    _settings: &Settings,
    _actions: &mut Vec<Action>,
) {
    crate::views::kit::scroll_page(ui, "Pipelines_page", |ui| {
        crate::views::kit::empty_state(
            ui,
            crate::views::icons::Icon::Info,
            "Pipelines",
            "Coming in this branch.",
            None,
        );
    });
}

/// A dot on the Review (`page == Review`) or Actions (`page == GitHub`)
/// rail entry and a tooltip line, when it needs attention.
pub fn rail_badge(
    _repo: &RepoState,
    _console: &Console,
    _page: Page,
) -> Option<(crate::theme::Tone, String)> {
    None
}
