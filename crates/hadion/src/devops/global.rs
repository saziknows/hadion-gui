//! App-wide areas: state that is not about one repository (the home inbox,
//! the command palette, the Settings page, terminals, notifications,
//! updates). Their state lives in [`Globals`] (one per app); their prefs in
//! `Settings::ext` ([`super::ExtPrefs`]).
//!
//! Contract of an app-wide area module (`palette`, `identity`, `terminal`,
//! `automation`, `packaging`):
//! - `State` (Default), `Act` (Clone + Debug), `Msg` with `summary()`,
//!   `Prefs` (serde, in `ExtPrefs`).
//! - `apply(cx, &mut State, &mut GlobalCx, act)` and
//!   `on_msg(cx, &mut State, &mut GlobalCx, msg)`.
//! - `tick(cx, &mut State, &mut GlobalCx)`: called every frame; must be
//!   cheap (compare a timestamp, then return).
//! - `script(cmd, arg)`.
//!
//! The `cx` is built for the active repository (its id, working directory
//! and GitHub remote), so app-wide areas can still read about it.

use crate::devops::{
    Ctx, DevOpsAction, DevOpsMsg, automation, home, identity, packaging, palette, settings_page,
    terminal,
};
use crate::state::{RepoState, Settings};

/// Every app-wide area's state.
#[derive(Default)]
pub struct Globals {
    pub home: home::State,
    pub palette: palette::State,
    pub identity: identity::State,
    pub terminal: terminal::State,
    pub automation: automation::State,
    pub packaging: packaging::State,
    /// The section the Settings page shows.
    pub settings_section: settings_page::Section,
}

/// What an app-wide controller can touch besides its own state.
pub struct GlobalCx<'a> {
    /// Its prefs live in `settings.ext`; changes are saved with the session.
    pub settings: &'a mut Settings,
    pub repos: &'a [RepoState],
    pub active: usize,
    /// For threads that must wake the UI (terminal output).
    pub egui: &'a egui::Context,
}

impl GlobalCx<'_> {
    pub fn active_repo(&self) -> Option<&RepoState> {
        self.repos.get(self.active)
    }
}

/// What app-wide views can read.
pub struct GlobalView<'a> {
    pub settings: &'a Settings,
    pub repos: &'a [RepoState],
    pub active: usize,
}

impl GlobalView<'_> {
    pub fn active_repo(&self) -> Option<&RepoState> {
        self.repos.get(self.active)
    }
}

/// Route an app-wide action.
pub fn apply(cx: &mut Ctx, g: &mut Globals, gcx: &mut GlobalCx, action: DevOpsAction) {
    match action {
        DevOpsAction::Home(a) => home::apply(cx, &mut g.home, gcx.repos, a),
        DevOpsAction::Palette(a) => palette::apply(cx, &mut g.palette, gcx, a),
        DevOpsAction::Identity(a) => identity::apply(cx, &mut g.identity, gcx, a),
        DevOpsAction::Terminal(a) => terminal::apply(cx, &mut g.terminal, gcx, a),
        DevOpsAction::Automation(a) => automation::apply(cx, &mut g.automation, gcx, a),
        DevOpsAction::Packaging(a) => packaging::apply(cx, &mut g.packaging, gcx, a),
        DevOpsAction::SetSettingsSection(s) => {
            g.settings_section = s;
            settings_page::section_shown(cx, g, gcx);
        }
        _ => {}
    }
}

/// Route an app-wide reply.
pub fn on_msg(cx: &mut Ctx, g: &mut Globals, gcx: &mut GlobalCx, msg: DevOpsMsg) {
    match msg {
        DevOpsMsg::Home(m) => home::on_msg(cx, &mut g.home, gcx.repos, m),
        DevOpsMsg::Palette(m) => palette::on_msg(cx, &mut g.palette, gcx, m),
        DevOpsMsg::Identity(m) => identity::on_msg(cx, &mut g.identity, gcx, m),
        DevOpsMsg::Terminal(m) => terminal::on_msg(cx, &mut g.terminal, gcx, m),
        DevOpsMsg::Automation(m) => automation::on_msg(cx, &mut g.automation, gcx, m),
        DevOpsMsg::Packaging(m) => packaging::on_msg(cx, &mut g.packaging, gcx, m),
        _ => {}
    }
}

/// Every frame: timers of the app-wide areas (notification polling, the
/// update check, terminal housekeeping).
pub fn tick(cx: &mut Ctx, g: &mut Globals, gcx: &mut GlobalCx) {
    palette::tick(cx, &mut g.palette, gcx);
    identity::tick(cx, &mut g.identity, gcx);
    terminal::tick(cx, &mut g.terminal, gcx);
    automation::tick(cx, &mut g.automation, gcx);
    packaging::tick(cx, &mut g.packaging, gcx);
}
