//! The home page: every open repository at a glance and the GitHub inbox
//! (notifications, review requests, mentions, assignments, failed runs on
//! the user's branches). Global: its state lives in the app, not in a
//! repository.

use serde::{Deserialize, Serialize};

use crate::devops::ctx::Ctx;
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

/// Persisted in the global settings.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {}

impl State {
    pub fn prefs(&self) -> Prefs {
        Prefs::default()
    }

    pub fn apply_prefs(&mut self, _p: &Prefs) {}
}

pub fn apply(_cx: &mut Ctx, _state: &mut State, _repos: &[RepoState], act: Act) {
    match act {}
}

pub fn on_msg(_cx: &mut Ctx, _state: &mut State, _repos: &[RepoState], msg: Msg) {
    match msg {}
}

/// The home page was opened.
pub fn on_page_shown(_cx: &mut Ctx, _state: &mut State, _repos: &[RepoState]) {}

/// `GITVIS_SCRIPT` commands this area understands.
pub fn script(_cmd: &str, _arg: &str) -> Option<Vec<Action>> {
    None
}

pub fn show(
    ui: &mut egui::Ui,
    _state: &mut State,
    _repos: &[RepoState],
    _active: usize,
    _settings: &Settings,
    _actions: &mut Vec<Action>,
) {
    crate::views::kit::scroll_page(ui, "Home_page", |ui| {
        crate::views::kit::empty_state(
            ui,
            crate::views::icons::Icon::Info,
            "Home",
            "Coming in this branch.",
            None,
        );
    });
}

/// A dot on the Home rail entry and a tooltip line (e.g. unread inbox
/// items).
pub fn rail_badge(_state: &State) -> Option<(crate::theme::Tone, String)> {
    None
}
