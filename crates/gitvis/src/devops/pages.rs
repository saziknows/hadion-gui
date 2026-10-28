//! The Ship page: one section navigation over two areas, releases and
//! deployments ([`super::ship`]) and what is running ([`super::runtime`]).
//!
//! Its submodules are shared by the Ship, Govern and Tools pages: pane
//! frames that stay inside a narrow window ([`layout`]) and the pieces
//! explanations are built from ([`explain`]).

pub mod explain;
pub mod layout;

use crate::devops::console::Console;
use crate::devops::ctx::Ctx;
use crate::devops::{DevOpsAction, runtime, ship};
use crate::state::{Action, RepoState, Settings};
use crate::views::kit::{self, NavGroup, NavItem};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ShipTab {
    #[default]
    Releases,
    Environments,
    WhereIsMyCommit,
    Dora,
    Packages,
    Kubernetes,
    Terraform,
}

/// Which area draws a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Ship,
    Runtime,
}

const NAV: [(&str, &[ShipTab]); 3] = [
    ("Release", &[ShipTab::Releases]),
    (
        "Deploy",
        &[
            ShipTab::Environments,
            ShipTab::WhereIsMyCommit,
            ShipTab::Dora,
        ],
    ),
    (
        "Running",
        &[ShipTab::Packages, ShipTab::Kubernetes, ShipTab::Terraform],
    ),
];

impl ShipTab {
    pub fn label(self) -> &'static str {
        match self {
            ShipTab::Releases => "Releases",
            ShipTab::Environments => "Environments",
            ShipTab::WhereIsMyCommit => "Where is my commit",
            ShipTab::Dora => "DORA metrics",
            ShipTab::Packages => "Packages",
            ShipTab::Kubernetes => "Kubernetes",
            ShipTab::Terraform => "Terraform",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            ShipTab::Releases => "Published releases and their assets; publish a new one",
            ShipTab::Environments => {
                "What is deployed where, pending approvals, promote and roll back"
            }
            ShipTab::WhereIsMyCommit => {
                "Which branches, pull request, releases and environments contain a commit"
            }
            ShipTab::Dora => {
                "Deployment frequency, lead time, change failure rate and time to restore"
            }
            ShipTab::Packages => "Container images on GitHub Packages, matched to commits",
            ShipTab::Kubernetes => "Deployments, images, rollouts and pod logs through kubectl",
            ShipTab::Terraform => "Terraform roots, validate, plan and drift",
        }
    }

    pub fn owner(self) -> Owner {
        match self {
            ShipTab::Packages | ShipTab::Kubernetes | ShipTab::Terraform => Owner::Runtime,
            _ => Owner::Ship,
        }
    }

    /// Names accepted by the `ship-tab` script command.
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "releases" | "release" => ShipTab::Releases,
            "environments" | "envs" | "deployments" => ShipTab::Environments,
            "where" | "commit" | "where-is-my-commit" => ShipTab::WhereIsMyCommit,
            "dora" | "metrics" => ShipTab::Dora,
            "packages" | "ghcr" => ShipTab::Packages,
            "kubernetes" | "k8s" => ShipTab::Kubernetes,
            "terraform" | "tf" => ShipTab::Terraform,
            _ => return None,
        })
    }
}

/// Switch tab and let its owner fetch what it needs.
pub fn set_tab(cx: &mut Ctx, repo: &mut RepoState, tab: ShipTab) {
    repo.devops.ship_tab = tab;
    tab_shown(cx, repo);
}

/// The Ship page (or its tab) became visible.
pub fn tab_shown(cx: &mut Ctx, repo: &mut RepoState) {
    let tab = repo.devops.ship_tab;
    match tab.owner() {
        Owner::Ship => ship::on_tab_shown(cx, repo, tab),
        Owner::Runtime => runtime::on_tab_shown(cx, repo, tab),
    }
}

pub fn show_ship(
    ui: &mut egui::Ui,
    repo: &mut RepoState,
    console: &Console,
    settings: &Settings,
    actions: &mut Vec<Action>,
) {
    let tab = repo.devops.ship_tab;
    let order: Vec<ShipTab> = NAV.iter().flat_map(|(_, t)| t.iter().copied()).collect();
    let mut clicked: Option<ShipTab> = None;
    kit::page_with_nav(
        ui,
        "ship_nav",
        |ui| {
            let groups: Vec<NavGroup> = NAV
                .iter()
                .map(|(title, tabs)| NavGroup {
                    title,
                    items: tabs
                        .iter()
                        .map(|t| NavItem::new(t.label()).hint(t.hint()))
                        .collect(),
                })
                .collect();
            let selected = order.iter().position(|t| *t == tab).unwrap_or(0);
            if let Some(i) = kit::section_nav(ui, &groups, selected) {
                clicked = order.get(i).copied();
            }
        },
        |ui| match tab.owner() {
            Owner::Ship => ship::show_tab(ui, repo, tab, console, settings, actions),
            Owner::Runtime => runtime::show_tab(ui, repo, tab, console, settings, actions),
        },
    );
    if let Some(t) = clicked {
        actions.push(Action::DevOps(DevOpsAction::SetShipTab(t)));
    }
}
