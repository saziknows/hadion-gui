//! The command palette (Ctrl+K): one fuzzy search over pages, actions,
//! branches, tags, files, commits, pull requests, runs, tasks and settings
//! of the active repository (and the other open ones).

use serde::{Deserialize, Serialize};

use crate::devops::ctx::Ctx;
use crate::devops::global::{GlobalCx, GlobalView};
use crate::state::{Action, RepoState, Settings};

#[derive(Default)]
pub struct State {
    pub open: bool,
}

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

pub fn is_open(st: &State) -> bool {
    st.open
}

pub fn toggle(st: &mut State) {
    st.open = !st.open;
}

/// The palette overlay (drawn above everything while open).
pub fn show(_ctx: &egui::Context, _st: &mut State, _v: &GlobalView, _actions: &mut Vec<Action>) {}

#[allow(dead_code)]
fn _unused(_: &RepoState, _: &Settings) {}
