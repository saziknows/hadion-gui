//! Packaging: update checks and installs (GitHub releases, SHA-256
//! verified, swapped in on restart), crash reports (a panic hook writing a
//! report; offered on the next start), and the Updates / Crash reports /
//! About sections of the Settings page.

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

/// Updates, Crash reports and About sections.
pub fn settings_section(
    ui: &mut egui::Ui,
    _st: &mut State,
    _settings: &mut Settings,
    _section: Section,
    _actions: &mut Vec<Action>,
) {
    crate::views::kit::scroll_page(ui, "Updates_stub", |ui| {
        crate::views::kit::empty_state(
            ui,
            crate::views::icons::Icon::Info,
            "Updates",
            "Coming in this branch.",
            None,
        );
    });
}

/// Right side of the status bar (right-to-left): "Update available", …
pub fn hook_status_items(
    _ui: &mut egui::Ui,
    _st: &State,
    _v: &GlobalView,
    _actions: &mut Vec<Action>,
) {
}

/// Installed first thing in `main` (before the window opens).
pub fn install_crash_handler() {}

#[allow(dead_code)]
fn _unused(_: &RepoState) {}
