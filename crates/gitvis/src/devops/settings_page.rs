//! The Settings page (`Page::Settings`, the gear on the rail, Ctrl+,):
//! section navigation, and each section drawn by the area that owns it.
//!
//! | Section | Owner |
//! |---|---|
//! | Appearance, Behaviour, Safety | this module |
//! | Git config, Identities, Keys and signing, GitHub account | [`super::identity`] |
//! | AI assist | [`super::ai`] |
//! | Terminal | [`super::terminal`] |
//! | Notifications, Integrations | [`super::automation`] |
//! | Updates, Crash reports, About | [`super::packaging`] |

use egui::Ui;

use crate::devops::global::{GlobalCx, Globals};
use crate::devops::{Ctx, DevOpsAction, ai, automation, identity, packaging, terminal};
use crate::state::{Action, PullPref, RepoState, Settings};
use crate::theme::{Tone, space};
use crate::views::form;
use crate::views::kit::{self, NavGroup, NavItem};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Section {
    #[default]
    Appearance,
    Behaviour,
    Safety,
    Terminal,
    GitConfig,
    Identities,
    Keys,
    GitHubAccount,
    Ai,
    Notifications,
    Integrations,
    Updates,
    CrashReports,
    About,
}

const NAV: [(&str, &[Section]); 4] = [
    (
        "App",
        &[
            Section::Appearance,
            Section::Behaviour,
            Section::Safety,
            Section::Terminal,
        ],
    ),
    (
        "Git",
        &[Section::GitConfig, Section::Identities, Section::Keys],
    ),
    (
        "Accounts and services",
        &[
            Section::GitHubAccount,
            Section::Ai,
            Section::Notifications,
            Section::Integrations,
        ],
    ),
    (
        "gitvis",
        &[Section::Updates, Section::CrashReports, Section::About],
    ),
];

impl Section {
    pub fn label(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Behaviour => "Behaviour",
            Section::Safety => "Safety",
            Section::Terminal => "Terminal",
            Section::GitConfig => "Git config",
            Section::Identities => "Identities",
            Section::Keys => "Keys and signing",
            Section::GitHubAccount => "GitHub account",
            Section::Ai => "AI assist",
            Section::Notifications => "Notifications",
            Section::Integrations => "Integrations",
            Section::Updates => "Updates",
            Section::CrashReports => "Crash reports",
            Section::About => "About",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Section::Appearance => "Theme, dates, columns",
            Section::Behaviour => "Pull strategy, scanning, the activity log",
            Section::Safety => "Safe mode: block every change outside this machine",
            Section::Terminal => "Shell, font size, scrollback",
            Section::GitConfig => "Global, system and repository git configuration",
            Section::Identities => "Name and e-mail per folder (includeIf)",
            Section::Keys => "SSH and GPG keys, commit signing",
            Section::GitHubAccount => "gh sign-in, token scopes",
            Section::Ai => "Claude API key, model, what is sent",
            Section::Notifications => "Desktop notifications for runs and reviews",
            Section::Integrations => "Jira and Linear links",
            Section::Updates => "Check for and install new versions",
            Section::CrashReports => "Crashes saved on this machine",
            Section::About => "Version, paths, licences",
        }
    }

    /// Names accepted by the `settings` script command.
    pub fn from_name(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        if s.is_empty() {
            return None;
        }
        NAV.iter()
            .flat_map(|(_, items)| items.iter().copied())
            .find(|sec| {
                let label = sec.label().to_ascii_lowercase();
                label == s || label.replace(' ', "-") == s || label.split(' ').next() == Some(&s)
            })
            .or(match s.as_str() {
                "keys" | "signing" => Some(Section::Keys),
                "github" | "account" => Some(Section::GitHubAccount),
                "ai" | "claude" => Some(Section::Ai),
                "safe" | "safe-mode" => Some(Section::Safety),
                "crash" | "crashes" => Some(Section::CrashReports),
                _ => None,
            })
    }
}

/// A section became visible: let its owner load what it shows.
pub fn section_shown(cx: &mut Ctx, g: &mut Globals, gcx: &mut GlobalCx) {
    let section = g.settings_section;
    identity::on_section_shown(cx, &mut g.identity, gcx, section);
    terminal::on_section_shown(cx, &mut g.terminal, gcx, section);
    automation::on_section_shown(cx, &mut g.automation, gcx, section);
    packaging::on_section_shown(cx, &mut g.packaging, gcx, section);
}

pub fn show(
    ui: &mut Ui,
    g: &mut Globals,
    settings: &mut Settings,
    repo: Option<&RepoState>,
    actions: &mut Vec<Action>,
) {
    let current = g.settings_section;
    let safe_mode = settings.safe_mode;
    let order: Vec<Section> = NAV.iter().flat_map(|(_, s)| s.iter().copied()).collect();
    let mut clicked: Option<Section> = None;
    kit::page_with_nav(
        ui,
        "settings_nav",
        |ui| {
            let groups: Vec<NavGroup> = NAV
                .iter()
                .map(|(title, items)| NavGroup {
                    title,
                    items: items
                        .iter()
                        .map(|s| {
                            let item = NavItem::new(s.label()).hint(s.hint());
                            if *s == Section::Safety && safe_mode {
                                item.count(1, Tone::Warning)
                            } else {
                                item
                            }
                        })
                        .collect(),
                })
                .collect();
            let selected = order.iter().position(|s| *s == current).unwrap_or(0);
            if let Some(i) = kit::section_nav(ui, &groups, selected) {
                clicked = order.get(i).copied();
            }
        },
        |ui| match current {
            Section::Appearance => own_section(ui, "Appearance", current.hint(), |ui| {
                appearance(ui, settings, actions)
            }),
            Section::Behaviour => own_section(ui, "Behaviour", current.hint(), |ui| {
                behaviour(ui, settings, actions)
            }),
            Section::Safety => own_section(ui, "Safety", current.hint(), |ui| {
                safety(ui, settings, actions)
            }),
            Section::Terminal => terminal::settings_section(ui, &mut g.terminal, settings, actions),
            Section::GitConfig | Section::Identities | Section::Keys | Section::GitHubAccount => {
                identity::settings_section(ui, &mut g.identity, settings, repo, current, actions)
            }
            Section::Ai => ai::settings_section(ui, settings, repo, actions),
            Section::Notifications | Section::Integrations => automation::settings_section(
                ui,
                &mut g.automation,
                settings,
                repo,
                current,
                actions,
            ),
            Section::Updates | Section::CrashReports | Section::About => {
                packaging::settings_section(ui, &mut g.packaging, settings, current, actions)
            }
        },
    );
    if let Some(s) = clicked {
        actions.push(Action::DevOps(DevOpsAction::SetSettingsSection(s)));
    }
}

/// A section drawn here: header, then a scrolling body.
fn own_section(ui: &mut Ui, title: &str, subtitle: &str, add: impl FnOnce(&mut Ui)) {
    kit::page_header(ui, title, subtitle, |_| {});
    kit::scroll_page(ui, &format!("settings_{title}"), |ui| {
        ui.set_max_width(kit::PROSE);
        add(ui)
    });
}

fn appearance(ui: &mut Ui, settings: &Settings, actions: &mut Vec<Action>) {
    form::card(ui, Tone::Neutral, |ui| {
        let mut dark = settings.dark;
        if form::checkbox(ui, &mut dark, "Dark theme").clicked() {
            actions.push(Action::ToggleTheme);
        }
        let mut rel = settings.relative_dates;
        if form::checkbox(ui, &mut rel, "Relative dates (\"3 hours ago\")").clicked() {
            actions.push(Action::ToggleRelativeDates);
        }
        let mut author = settings.show_author;
        if form::checkbox(ui, &mut author, "Author column in the graph").clicked() {
            actions.push(Action::ToggleAuthor);
        }
        let mut status_bar = settings.show_status_bar;
        if form::checkbox(ui, &mut status_bar, "Status bar").clicked() {
            actions.push(Action::ToggleStatusBar);
        }
    });
}

fn behaviour(ui: &mut Ui, settings: &Settings, actions: &mut Vec<Action>) {
    kit::section(ui, "Default pull", None);
    form::card(ui, Tone::Neutral, |ui| {
        for pp in [PullPref::Merge, PullPref::FastForwardOnly, PullPref::Rebase] {
            if form::radio(ui, settings.pull == pp, pp.label()) {
                actions.push(Action::SetPull(pp));
            }
        }
    });
    ui.add_space(space::L);
    kit::section(ui, "Activity and scanning", None);
    form::card(ui, Tone::Neutral, |ui| {
        let mut verbose = settings.verbose;
        if form::checkbox(ui, &mut verbose, "Verbose activity log").clicked() {
            actions.push(Action::ToggleVerbose);
        }
        kit::muted(
            ui,
            "Record repository reads and API calls too, with their full output, not only operations and failures.",
        );
        ui.add_space(space::S);
        let mut scan = settings.scan_on_open;
        if form::checkbox(ui, &mut scan, "Scan for secrets when a repository opens").clicked() {
            actions.push(Action::ToggleScanOnOpen);
        }
        kit::muted(
            ui,
            "Reads every file of the working tree for credentials. Staged changes are always checked before a commit.",
        );
    });
}

fn safety(ui: &mut Ui, settings: &Settings, actions: &mut Vec<Action>) {
    let on = settings.safe_mode;
    form::card(ui, if on { Tone::Warning } else { Tone::Neutral }, |ui| {
        let mut v = on;
        if form::checkbox(ui, &mut v, "Safe mode").clicked() {
            actions.push(Action::SetSafeMode(v));
        }
        ui.add_space(space::XS);
        kit::body(
            ui,
            if on {
                "Safe mode is on. Every change outside this machine is refused before it starts, with a message saying what was blocked."
            } else {
                "When on, gitvis refuses every change outside this machine before it starts. Reading stays possible everywhere."
            },
        );
    });
    ui.add_space(space::L);
    kit::section(ui, "What safe mode blocks", None);
    let rows: [(&str, &str, &str); 8] = [
        (
            "GitHub API",
            "POST, PUT, PATCH and DELETE requests, GraphQL mutations",
            "GET requests and GraphQL queries",
        ),
        (
            "gh",
            "secret set, release create and every other subcommand that writes",
            "auth status, run download, release download, pr checkout",
        ),
        (
            "git",
            "push, including deleting remote branches and tags",
            "fetch, pull and every local change",
        ),
        (
            "glab, az",
            "API writes, pull request and pipeline changes",
            "lists, details, logs",
        ),
        (
            "kubectl",
            "apply, delete, scale, rollout restart and undo, context changes",
            "get, describe, logs, rollout status and history",
        ),
        (
            "helm",
            "install, upgrade, uninstall, rollback, push",
            "list, status, template",
        ),
        (
            "terraform, tofu",
            "apply, destroy, import, state changes",
            "fmt, init, validate, plan",
        ),
        ("docker", "push", "scans, inspect, pull"),
    ];
    let p = *crate::theme::pal(ui);
    for (tool, blocked, allowed) in rows {
        ui.add_space(space::S);
        ui.label(
            egui::RichText::new(tool)
                .family(crate::theme::semibold())
                .color(p.text_strong),
        );
        kit::kv(ui, 110.0, |kv| {
            kv.text("Blocked", blocked);
            kv.muted("Still allowed", allowed);
        });
    }
    ui.add_space(space::M);
    kit::callout(
        ui,
        Tone::Neutral,
        "Not covered",
        "Task scripts (npm, make, cargo …) run whatever they contain: safe mode cannot tell a deploy script from a build. Local git changes (commit, rebase, reset) are not remote and stay allowed; undo them from the reflog.",
    );
}
