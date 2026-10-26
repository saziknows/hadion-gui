//! DevOps pages and their controllers.
//!
//! Each area owns its module: state (`State`), what its views ask for
//! (`Act`), the replies of its background work (`Msg`), what it persists
//! (`Prefs`), its controller (`apply`, `on_msg`, `on_console_done`,
//! `on_page_shown`), its script commands (`script`) and its views. The app
//! routes [`Action::DevOps`] and `Event::DevOps` here and never needs to
//! know an area's internals.
//!
//! | Area | Page (rail) | Core module |
//! |---|---|---|
//! | [`home`] | Home | `devops::home` |
//! | [`review`] | Review | `devops::review` |
//! | [`ci`] | Actions (tabs beside Runs) | `devops::ci` |
//! | [`ship`], [`runtime`] | Ship ([`pages::show_ship`]) | `devops::ship`, `devops::runtime` |
//! | [`govern`] | Govern | `devops::govern` |
//! | [`workbench`] | Workbench | `devops::workbench` |
//! | [`forge`] | Review and Actions on GitLab / Azure DevOps | `devops::forge` |

pub mod ci;
pub mod console;
pub mod ctx;
pub mod forge;
pub mod govern;
pub mod home;
pub mod pages;
pub mod review;
pub mod runtime;
pub mod ship;
pub mod workbench;

use serde::{Deserialize, Serialize};

pub use console::{Console, ConsoleAction, ConsoleRun};
pub use ctx::{Ctx, Effect, Task, TaskDone, TaskKind, TaskLog};
pub use pages::ShipTab;

use crate::state::{Action, Page, RepoState};

/// Per-repository state of every area.
#[derive(Default)]
pub struct DevOpsState {
    pub review: review::State,
    pub ci: ci::State,
    pub ship: ship::State,
    pub runtime: runtime::State,
    pub govern: govern::State,
    pub workbench: workbench::State,
    pub forge: forge::State,
    pub ship_tab: ShipTab,
}

/// Per-repository preferences of every area.
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
    }
}

/// What a DevOps view can ask for.
#[derive(Clone, Debug)]
pub enum DevOpsAction {
    Review(review::Act),
    Ci(ci::Act),
    Ship(ship::Act),
    Runtime(runtime::Act),
    Govern(govern::Act),
    Workbench(workbench::Act),
    Forge(forge::Act),
    Home(home::Act),
    SetShipTab(ShipTab),
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
    Home(home::Msg),
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
            DevOpsMsg::Home(m) => m.summary(),
        }
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
wire!(home, Home);

impl From<DevOpsAction> for Action {
    fn from(a: DevOpsAction) -> Self {
        Action::DevOps(a)
    }
}

/// Route a repository-level action to its area (`Home` is handled by the
/// app, which owns the home state).
pub fn apply(cx: &mut Ctx, repo: &mut RepoState, action: DevOpsAction) {
    match action {
        DevOpsAction::Review(a) => review::apply(cx, repo, a),
        DevOpsAction::Ci(a) => ci::apply(cx, repo, a),
        DevOpsAction::Ship(a) => ship::apply(cx, repo, a),
        DevOpsAction::Runtime(a) => runtime::apply(cx, repo, a),
        DevOpsAction::Govern(a) => govern::apply(cx, repo, a),
        DevOpsAction::Workbench(a) => workbench::apply(cx, repo, a),
        DevOpsAction::Forge(a) => forge::apply(cx, repo, a),
        DevOpsAction::SetShipTab(t) => pages::set_tab(cx, repo, t),
        DevOpsAction::Home(_) => {}
    }
}

/// Route a reply to its area (`Home` is handled by the app).
pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: DevOpsMsg) {
    match msg {
        DevOpsMsg::Review(m) => review::on_msg(cx, repo, m),
        DevOpsMsg::Ci(m) => ci::on_msg(cx, repo, m),
        DevOpsMsg::Ship(m) => ship::on_msg(cx, repo, m),
        DevOpsMsg::Runtime(m) => runtime::on_msg(cx, repo, m),
        DevOpsMsg::Govern(m) => govern::on_msg(cx, repo, m),
        DevOpsMsg::Workbench(m) => workbench::on_msg(cx, repo, m),
        DevOpsMsg::Forge(m) => forge::on_msg(cx, repo, m),
        DevOpsMsg::Home(_) => {}
    }
}

/// Tell every area that a console run of this repository ended.
pub fn on_console_done(cx: &mut Ctx, repo: &mut RepoState, run: &ConsoleRun) {
    review::on_console_done(cx, repo, run);
    ci::on_console_done(cx, repo, run);
    ship::on_console_done(cx, repo, run);
    runtime::on_console_done(cx, repo, run);
    govern::on_console_done(cx, repo, run);
    workbench::on_console_done(cx, repo, run);
    forge::on_console_done(cx, repo, run);
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
        _ => {}
    }
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
    review::script(cmd, arg)
        .or_else(|| ci::script(cmd, arg))
        .or_else(|| ship::script(cmd, arg))
        .or_else(|| runtime::script(cmd, arg))
        .or_else(|| govern::script(cmd, arg))
        .or_else(|| workbench::script(cmd, arg))
        .or_else(|| forge::script(cmd, arg))
        .or_else(|| home::script(cmd, arg))
}
