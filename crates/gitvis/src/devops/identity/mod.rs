//! Git configuration and identities on the Settings page: git config
//! (system / global / repository) editor, identities per folder
//! (`includeIf`), SSH and GPG keys with commit signing setup, and the GitHub
//! CLI account (sign-in state, token scopes, fixes).

use serde::{Deserialize, Serialize};

use crate::devops::ctx::Ctx;
use crate::devops::global::{GlobalCx, GlobalView};
use crate::devops::settings_page::Section;
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

/// App-wide preferences (`Settings::ext`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {}

pub fn apply(_cx: &mut Ctx, _st: &mut State, _g: &mut GlobalCx, act: Act) {
    match act {}
}

pub fn on_msg(_cx: &mut Ctx, _st: &mut State, _g: &mut GlobalCx, msg: Msg) {
    match msg {}
}

/// Every frame; keep it cheap.
pub fn tick(_cx: &mut Ctx, _st: &mut State, _g: &mut GlobalCx) {}

/// `GITVIS_SCRIPT` commands this area understands.
pub fn script(_cmd: &str, _arg: &str) -> Option<Vec<Action>> {
    None
}

/// A Settings section became visible (check `section`; most are not yours).
pub fn on_section_shown(_cx: &mut Ctx, _st: &mut State, _g: &mut GlobalCx, _section: Section) {}

/// Git config, Identities, Keys and signing, GitHub account sections.
pub fn settings_section(
    ui: &mut egui::Ui,
    _st: &mut State,
    _settings: &mut Settings,
    _repo: Option<&RepoState>,
    _section: Section,
    _actions: &mut Vec<Action>,
) {
    crate::views::kit::scroll_page(ui, "Identity_stub", |ui| {
        crate::views::kit::empty_state(
            ui,
            crate::views::icons::Icon::Info,
            "Identity",
            "Coming in this branch.",
            None,
        );
    });
}

#[allow(dead_code)]
fn _unused(_: &GlobalView) {}
