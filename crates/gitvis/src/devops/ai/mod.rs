//! AI assist (Claude API): "Write the message" in the WIP panel, "Write
//! the description" in the new pull request form, "Explain this failure" in
//! the Actions log viewer, "Summarise" on a pull request, and the AI
//! section of the Settings page. Per-repository state (requests in flight,
//! results); configuration in `Settings::ext.ai`.

use serde::{Deserialize, Serialize};

use crate::devops::console::ConsoleRun;
use crate::devops::ctx::Ctx;
use crate::state::{Action, RepoState, Settings};
use gitvis_core::Commit;

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

/// App-wide configuration (`Settings::ext.ai`): model, effort, whether a key
/// is stored (the key itself is only in the OS keychain).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {}

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

/// The AI section of the Settings page.
pub fn settings_section(
    ui: &mut egui::Ui,
    _settings: &mut Settings,
    _repo: Option<&RepoState>,
    _actions: &mut Vec<Action>,
) {
    crate::views::kit::scroll_page(ui, "AI assist_stub", |ui| {
        crate::views::kit::empty_state(
            ui,
            crate::views::icons::Icon::Info,
            "AI assist",
            "Coming in this branch.",
            None,
        );
    });
}

pub fn hook_commit_menu(_ui: &mut egui::Ui, _h: &HookCx, _c: &Commit, _actions: &mut Vec<Action>) {}

pub fn hook_commit_panel(_ui: &mut egui::Ui, _h: &HookCx, _c: &Commit, _actions: &mut Vec<Action>) {
}

pub fn hook_wip_message_tools(_ui: &mut egui::Ui, _h: &HookCx, _actions: &mut Vec<Action>) {}

pub fn hook_log_viewer_tools(
    _ui: &mut egui::Ui,
    _h: &crate::devops::hooks::LogHookCx,
    _run: u64,
    _job: u64,
    _actions: &mut Vec<Action>,
) {
}

pub fn hook_pr_tools(_ui: &mut egui::Ui, _h: &HookCx, _number: u64, _actions: &mut Vec<Action>) {}

pub fn hook_new_pr_tools(_ui: &mut egui::Ui, _h: &HookCx, _actions: &mut Vec<Action>) {}
