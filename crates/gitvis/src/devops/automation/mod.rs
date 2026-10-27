//! Automation: desktop notifications (opt-in polling for finished runs on
//! your branches and new review requests), repository groups with batch
//! operations (Home page), Jira / Linear links (commit panel, pull
//! requests, branch names, the commit message), release helpers (version
//! bumps and CHANGELOG.md when publishing on the Ship page).

use serde::{Deserialize, Serialize};

use crate::devops::ctx::Ctx;
use crate::devops::global::{GlobalCx, GlobalView};
use crate::state::{Action, RepoState, Settings};
use gitvis_core::Commit;

use crate::devops::hooks::HookCx;
use crate::devops::settings_page::Section;

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

/// Notifications and Integrations sections.
pub fn settings_section(
    ui: &mut egui::Ui,
    _st: &mut State,
    _settings: &mut Settings,
    _repo: Option<&RepoState>,
    _section: Section,
    _actions: &mut Vec<Action>,
) {
    crate::views::kit::scroll_page(ui, "Automation_stub", |ui| {
        crate::views::kit::empty_state(
            ui,
            crate::views::icons::Icon::Info,
            "Automation",
            "Coming in this branch.",
            None,
        );
    });
}

pub fn hook_commit_panel(_ui: &mut egui::Ui, _h: &HookCx, _c: &Commit, _actions: &mut Vec<Action>) {
}

pub fn hook_wip_message_tools(_ui: &mut egui::Ui, _h: &HookCx, _actions: &mut Vec<Action>) {}

pub fn hook_pr_tools(_ui: &mut egui::Ui, _h: &HookCx, _number: u64, _actions: &mut Vec<Action>) {}

/// Right side of the status bar (right-to-left).
pub fn hook_status_items(
    _ui: &mut egui::Ui,
    _st: &State,
    _v: &GlobalView,
    _actions: &mut Vec<Action>,
) {
}
