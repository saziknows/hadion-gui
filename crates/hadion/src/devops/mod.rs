//! Feature areas and their controllers (the DevOps pages, plus the
//! app-wide features built on the same framework).
//!
//! Each area owns its module: state (`State`), what its views ask for
//! (`Act`), the replies of its background work (`Msg`), what it persists
//! (`Prefs`), its controller, its script commands (`script`) and its views.
//! The app routes [`Action::DevOps`] and `Event::DevOps` here and never needs
//! to know an area's internals.
//!
//! Repository areas (state in `RepoState::devops`, prefs per repository):
//!
//! | Area | Where it shows | Core module |
//! |---|---|---|
//! | [`review`] | Review page | `devops::review` |
//! | [`ci`] | Actions tabs beside Runs | `devops::ci` |
//! | [`ship`], [`runtime`] | Ship page ([`pages::show_ship`]) | `devops::ship`, `devops::runtime` |
//! | [`govern`] | Govern page | `devops::govern` |
//! | [`workbench`] | Tools page | `devops::workbench` |
//! | [`forge`] | Review and Actions on GitLab / Azure DevOps | `devops::forge` |
//! | [`conflict`] | Conflict editor (History page, replaces the graph) | `devops::conflict` |
//! | [`history`] | Compare page, graph multi-select, filters, blame, file tree, signatures | `devops::history` |
//! | [`gitplus`] | Tools page tabs (submodules, LFS, patches, hooks), stash / split helpers | `devops::gitplus` |
//! | [`ai`] | Buttons in the WIP panel, Review, Actions log; AI settings | `devops::ai` |
//!
//! App-wide areas (state in [`global::Globals`], prefs in [`ExtPrefs`] /
//! `Settings::ext`), see [`global`]: [`home`] (Home page), [`palette`]
//! (Ctrl+K), [`identity`] (git config, identities, keys, signing, accounts on
//! the Settings page), [`terminal`] (terminal panel), [`automation`]
//! (notifications, repository groups, issue-tracker links, release helpers),
//! [`packaging`] (updates, crash reports).
//!
//! Extension hooks that other views call (commit menu, commit panel, WIP
//! panel, log viewer, pull requests, status bar) live in [`hooks`].

pub mod ai;
pub mod automation;
pub mod ci;
pub mod conflict;
pub mod console;
pub mod ctx;
pub mod forge;
pub mod gitplus;
pub mod global;
pub mod govern;
pub mod history;
pub mod home;
pub mod hooks;
pub mod identity;
pub mod packaging;
pub mod pages;
pub mod palette;
pub mod review;
pub mod runtime;
pub mod settings_page;
pub mod ship;
pub mod terminal;
pub mod workbench;

use serde::{Deserialize, Serialize};

pub use console::{Console, ConsoleAction, ConsoleRun};
pub use ctx::{Ctx, Effect, Task, TaskDone, TaskKind, TaskLog};
pub use pages::ShipTab;

use crate::state::{Action, Page, RepoState};

/// Per-repository state of every repository area.
#[derive(Default)]
pub struct DevOpsState {
    pub review: review::State,
    pub ci: ci::State,
    pub ship: ship::State,
    pub runtime: runtime::State,
    pub govern: govern::State,
    pub workbench: workbench::State,
    pub forge: forge::State,
    pub conflict: conflict::State,
    pub history: history::State,
    pub gitplus: gitplus::State,
    pub ai: ai::State,
    pub ship_tab: ShipTab,
}

/// Per-repository preferences of every repository area.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DevOpsPrefs {
    pub review: review::Prefs,
    pub ci: ci::Prefs,
    pub ship: ship::Prefs,
    pub runtime: runtime::Prefs,
    pub govern: govern::Prefs,
    pub workbench: workbench::Prefs,
    pub forge: forge::Prefs,
    pub conflict: conflict::Prefs,
    pub history: history::Prefs,
    pub gitplus: gitplus::Prefs,
}

/// App-wide preferences (`Settings::ext`): AI configuration, terminal,
/// palette, identities, notifications and integrations, updates.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtPrefs {
    pub ai: ai::Prefs,
    pub palette: palette::Prefs,
    pub identity: identity::Prefs,
    pub terminal: terminal::Prefs,
    pub automation: automation::Prefs,
    pub packaging: packaging::Prefs,
}

impl DevOpsState {
    pub fn prefs(&self) -> DevOpsPrefs {
        DevOpsPrefs {
            review: self.review.prefs(),
            ci: self.ci.prefs(),
            ship: self.ship.prefs(),
            runtime: self.runtime.prefs(),
            govern: self.govern.prefs(),
            workbench: self.workbench.prefs(),
            forge: self.forge.prefs(),
            conflict: self.conflict.prefs(),
            history: self.history.prefs(),
            gitplus: self.gitplus.prefs(),
        }
    }

    pub fn apply_prefs(&mut self, p: &DevOpsPrefs) {
        self.review.apply_prefs(&p.review);
        self.ci.apply_prefs(&p.ci);
        self.ship.apply_prefs(&p.ship);
        self.runtime.apply_prefs(&p.runtime);
        self.govern.apply_prefs(&p.govern);
        self.workbench.apply_prefs(&p.workbench);
        self.forge.apply_prefs(&p.forge);
        self.conflict.apply_prefs(&p.conflict);
        self.history.apply_prefs(&p.history);
        self.gitplus.apply_prefs(&p.gitplus);
    }
}

/// What a view can ask an area for.
#[derive(Clone, Debug)]
pub enum DevOpsAction {
    Review(review::Act),
    Ci(ci::Act),
    Ship(ship::Act),
    Runtime(runtime::Act),
    Govern(govern::Act),
    Workbench(workbench::Act),
    Forge(forge::Act),
    Conflict(conflict::Act),
    History(history::Act),
    Gitplus(gitplus::Act),
    Ai(ai::Act),
    Home(home::Act),
    Palette(palette::Act),
    Identity(identity::Act),
    Terminal(terminal::Act),
    Automation(automation::Act),
    Packaging(packaging::Act),
    SetShipTab(ShipTab),
    SetSettingsSection(settings_page::Section),
}

impl DevOpsAction {
    /// Handled by an app-wide area (with [`global::GlobalCx`]) rather than
    /// a repository area.
    pub fn is_global(&self) -> bool {
        matches!(
            self,
            DevOpsAction::Home(_)
                | DevOpsAction::Palette(_)
                | DevOpsAction::Identity(_)
                | DevOpsAction::Terminal(_)
                | DevOpsAction::Automation(_)
                | DevOpsAction::Packaging(_)
                | DevOpsAction::SetSettingsSection(_)
        )
    }
}

/// Replies of background work, routed back to the area that asked.
pub enum DevOpsMsg {
    Review(review::Msg),
    Ci(ci::Msg),
    Ship(ship::Msg),
    Runtime(runtime::Msg),
    Govern(govern::Msg),
    Workbench(workbench::Msg),
    Forge(forge::Msg),
    Conflict(conflict::Msg),
    History(history::Msg),
    Gitplus(gitplus::Msg),
    Ai(ai::Msg),
    Home(home::Msg),
    Palette(palette::Msg),
    Identity(identity::Msg),
    Terminal(terminal::Msg),
    Automation(automation::Msg),
    Packaging(packaging::Msg),
}

impl DevOpsMsg {
    /// One line for the activity log.
    pub fn summary(&self) -> Result<String, String> {
        match self {
            DevOpsMsg::Review(m) => m.summary(),
            DevOpsMsg::Ci(m) => m.summary(),
            DevOpsMsg::Ship(m) => m.summary(),
            DevOpsMsg::Runtime(m) => m.summary(),
            DevOpsMsg::Govern(m) => m.summary(),
            DevOpsMsg::Workbench(m) => m.summary(),
            DevOpsMsg::Forge(m) => m.summary(),
            DevOpsMsg::Conflict(m) => m.summary(),
            DevOpsMsg::History(m) => m.summary(),
            DevOpsMsg::Gitplus(m) => m.summary(),
            DevOpsMsg::Ai(m) => m.summary(),
            DevOpsMsg::Home(m) => m.summary(),
            DevOpsMsg::Palette(m) => m.summary(),
            DevOpsMsg::Identity(m) => m.summary(),
            DevOpsMsg::Terminal(m) => m.summary(),
            DevOpsMsg::Automation(m) => m.summary(),
            DevOpsMsg::Packaging(m) => m.summary(),
        }
    }

    /// Handled by an app-wide area.
    pub fn is_global(&self) -> bool {
        matches!(
            self,
            DevOpsMsg::Home(_)
                | DevOpsMsg::Palette(_)
                | DevOpsMsg::Identity(_)
                | DevOpsMsg::Terminal(_)
                | DevOpsMsg::Automation(_)
                | DevOpsMsg::Packaging(_)
        )
    }
}

macro_rules! wire {
    ($area:ident, $variant:ident) => {
        impl From<$area::Msg> for DevOpsMsg {
            fn from(m: $area::Msg) -> Self {
                DevOpsMsg::$variant(m)
            }
        }
        impl From<$area::Act> for DevOpsAction {
            fn from(a: $area::Act) -> Self {
                DevOpsAction::$variant(a)
            }
        }
        impl From<$area::Act> for Action {
            fn from(a: $area::Act) -> Self {
                Action::DevOps(DevOpsAction::$variant(a))
            }
        }
    };
}

wire!(review, Review);
wire!(ci, Ci);
wire!(ship, Ship);
wire!(runtime, Runtime);
wire!(govern, Govern);
wire!(workbench, Workbench);
wire!(forge, Forge);
wire!(conflict, Conflict);
wire!(history, History);
wire!(gitplus, Gitplus);
wire!(ai, Ai);
wire!(home, Home);
wire!(palette, Palette);
wire!(identity, Identity);
wire!(terminal, Terminal);
wire!(automation, Automation);
wire!(packaging, Packaging);

impl From<DevOpsAction> for Action {
    fn from(a: DevOpsAction) -> Self {
        Action::DevOps(a)
    }
}

/// Route a repository-level action to its area (app-wide ones go to
/// [`global::apply`]).
pub fn apply(cx: &mut Ctx, repo: &mut RepoState, action: DevOpsAction) {
    match action {
        DevOpsAction::Review(a) => review::apply(cx, repo, a),
        DevOpsAction::Ci(a) => ci::apply(cx, repo, a),
        DevOpsAction::Ship(a) => ship::apply(cx, repo, a),
        DevOpsAction::Runtime(a) => runtime::apply(cx, repo, a),
        DevOpsAction::Govern(a) => govern::apply(cx, repo, a),
        DevOpsAction::Workbench(a) => workbench::apply(cx, repo, a),
        DevOpsAction::Forge(a) => forge::apply(cx, repo, a),
        DevOpsAction::Conflict(a) => conflict::apply(cx, repo, a),
        DevOpsAction::History(a) => history::apply(cx, repo, a),
        DevOpsAction::Gitplus(a) => gitplus::apply(cx, repo, a),
        DevOpsAction::Ai(a) => ai::apply(cx, repo, a),
        DevOpsAction::SetShipTab(t) => pages::set_tab(cx, repo, t),
        DevOpsAction::Home(_)
        | DevOpsAction::Palette(_)
        | DevOpsAction::Identity(_)
        | DevOpsAction::Terminal(_)
        | DevOpsAction::Automation(_)
        | DevOpsAction::Packaging(_)
        | DevOpsAction::SetSettingsSection(_) => {}
    }
}

/// Route a reply to its repository area (app-wide replies go to
/// [`global::on_msg`]).
pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: DevOpsMsg) {
    match msg {
        DevOpsMsg::Review(m) => review::on_msg(cx, repo, m),
        DevOpsMsg::Ci(m) => ci::on_msg(cx, repo, m),
        DevOpsMsg::Ship(m) => ship::on_msg(cx, repo, m),
        DevOpsMsg::Runtime(m) => runtime::on_msg(cx, repo, m),
        DevOpsMsg::Govern(m) => govern::on_msg(cx, repo, m),
        DevOpsMsg::Workbench(m) => workbench::on_msg(cx, repo, m),
        DevOpsMsg::Forge(m) => forge::on_msg(cx, repo, m),
        DevOpsMsg::Conflict(m) => conflict::on_msg(cx, repo, m),
        DevOpsMsg::History(m) => history::on_msg(cx, repo, m),
        DevOpsMsg::Gitplus(m) => gitplus::on_msg(cx, repo, m),
        DevOpsMsg::Ai(m) => ai::on_msg(cx, repo, m),
        DevOpsMsg::Home(_)
        | DevOpsMsg::Palette(_)
        | DevOpsMsg::Identity(_)
        | DevOpsMsg::Terminal(_)
        | DevOpsMsg::Automation(_)
        | DevOpsMsg::Packaging(_) => {}
    }
}

/// Tell every repository area that a console run of this repository ended.
pub fn on_console_done(cx: &mut Ctx, repo: &mut RepoState, run: &ConsoleRun) {
    review::on_console_done(cx, repo, run);
    ci::on_console_done(cx, repo, run);
    ship::on_console_done(cx, repo, run);
    runtime::on_console_done(cx, repo, run);
    govern::on_console_done(cx, repo, run);
    workbench::on_console_done(cx, repo, run);
    forge::on_console_done(cx, repo, run);
    conflict::on_console_done(cx, repo, run);
    history::on_console_done(cx, repo, run);
    gitplus::on_console_done(cx, repo, run);
    ai::on_console_done(cx, repo, run);
}

/// A page became visible: let its area fetch what it needs.
pub fn page_shown(cx: &mut Ctx, repo: &mut RepoState, page: Page) {
    let forge = repo.github_remote.is_none() && repo.forge_remote.is_some();
    match page {
        Page::Review if forge => forge::on_page_shown(cx, repo, page),
        Page::GitHub if forge => forge::on_page_shown(cx, repo, page),
        Page::Review => review::on_page_shown(cx, repo),
        Page::GitHub => ci::on_page_shown(cx, repo),
        Page::Ship => pages::tab_shown(cx, repo),
        Page::Govern => govern::on_page_shown(cx, repo),
        Page::Workbench => workbench::on_page_shown(cx, repo),
        Page::Compare => history::on_page_shown(cx, repo),
        _ => {}
    }
}

/// After every repository load (a new snapshot): areas that track the
/// working tree (conflicts, submodules, bisect, …) can refresh cheaply.
pub fn on_snapshot(cx: &mut Ctx, repo: &mut RepoState) {
    conflict::on_snapshot(cx, repo);
    history::on_snapshot(cx, repo);
    gitplus::on_snapshot(cx, repo);
}

/// Script commands of every area, tried in turn.
pub fn script(cmd: &str, arg: &str) -> Option<Vec<Action>> {
    if cmd == "page" {
        let page = match arg.trim().to_ascii_lowercase().as_str() {
            "home" => Page::Home,
            "graph" | "history" => Page::Graph,
            "review" | "prs" => Page::Review,
            "actions" | "github" | "ci" | "pipelines" => Page::GitHub,
            "ship" => Page::Ship,
            "exposure" | "security" => Page::Security,
            "insights" => Page::Insights,
            "govern" | "admin" => Page::Govern,
            "workbench" | "tools" => Page::Workbench,
            "compare" => Page::Compare,
            "settings" => Page::Settings,
            _ => return None,
        };
        return Some(vec![Action::ShowPage(page)]);
    }
    if cmd == "ship" {
        // Tab first, so opening the page fetches for the right tab.
        let mut out = Vec::new();
        if let Some(t) = ShipTab::from_name(arg) {
            out.push(Action::DevOps(DevOpsAction::SetShipTab(t)));
        }
        out.push(Action::ShowPage(Page::Ship));
        return Some(out);
    }
    if cmd == "ship-tab" {
        let t = ShipTab::from_name(arg)?;
        return Some(vec![
            Action::ShowPage(Page::Ship),
            Action::DevOps(DevOpsAction::SetShipTab(t)),
        ]);
    }
    if cmd == "settings" {
        let mut out = Vec::new();
        if let Some(s) = settings_page::Section::from_name(arg) {
            out.push(Action::DevOps(DevOpsAction::SetSettingsSection(s)));
        }
        out.push(Action::ShowPage(Page::Settings));
        return Some(out);
    }
    if cmd == "palette" {
        return Some(vec![Action::TogglePalette]);
    }
    if cmd == "terminal" {
        return Some(vec![Action::ToggleTerminal]);
    }
    if cmd == "safe-mode" {
        return Some(vec![Action::SetSafeMode(arg.trim() != "off")]);
    }
    review::script(cmd, arg)
        .or_else(|| ci::script(cmd, arg))
        .or_else(|| ship::script(cmd, arg))
        .or_else(|| runtime::script(cmd, arg))
        .or_else(|| govern::script(cmd, arg))
        .or_else(|| workbench::script(cmd, arg))
        .or_else(|| forge::script(cmd, arg))
        .or_else(|| conflict::script(cmd, arg))
        .or_else(|| history::script(cmd, arg))
        .or_else(|| gitplus::script(cmd, arg))
        .or_else(|| ai::script(cmd, arg))
        .or_else(|| home::script(cmd, arg))
        .or_else(|| palette::script(cmd, arg))
        .or_else(|| identity::script(cmd, arg))
        .or_else(|| terminal::script(cmd, arg))
        .or_else(|| automation::script(cmd, arg))
        .or_else(|| packaging::script(cmd, arg))
}
