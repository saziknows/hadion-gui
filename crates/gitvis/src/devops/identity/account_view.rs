//! Settings → GitHub account: who gh is signed in as (every host and
//! account), the user, the rate limits, which gitvis features the token's
//! scopes allow with the exact fix commands, switching accounts, and git's
//! credential helpers.

use egui::Ui;
use gitvis_core::devops::identity::account::{self, Account, Need};
use gitvis_core::devops::identity::config::Snapshot;
use gitvis_core::devops::identity::{git_home, tilde};
use gitvis_core::devops::tools::Tool;

use super::widgets as w;
use super::{AccountData, Act, State};
use crate::devops::settings_page::Section;
use crate::format;
use crate::state::{Action, Settings};
use crate::theme::{pal, size, space};
use crate::views::form;
use crate::views::icons::{Icon, IconButton};
use crate::views::insights::{self, Cell};
use crate::views::kit::{self, Col, Stat, Tone};

pub fn show(ui: &mut Ui, st: &mut State, settings: &mut Settings, actions: &mut Vec<Action>) {
    let safe = settings.safe_mode;
    let State {
        account,
        config,
        busy,
        ..
    } = st;
    let busy = *busy;
    if w::header(
        ui,
        "GitHub account",
        "Who gh is signed in as, what its token allows, rate limits, git credentials",
        account.loading,
        account.at,
        |_| {},
    ) {
        actions.push(Act::Refresh(Section::GitHubAccount).into());
    }
    kit::scroll_page(ui, "identity_account", |ui| {
        match &account.data {
            None => kit::loading(ui, "gh auth status --json hosts"),
            Some(Err(e)) => {
                if w::error_card(ui, "Could not read the gh account", e, "Try again") {
                    actions.push(Act::Refresh(Section::GitHubAccount).into());
                }
            }
            Some(Ok(a)) => body(ui, a, safe, busy, actions),
        }
        credentials(ui, config.ok(), account.ok(), safe, busy, actions);
    });
}

fn body(ui: &mut Ui, a: &AccountData, safe: bool, busy: bool, actions: &mut Vec<Action>) {
    if a.gh.is_none() {
        if w::missing_tool(
            ui,
            Tool::Gh.name(),
            "gh",
            "gitvis talks to GitHub through gh and the account you signed in with (gh auth login).",
            Tool::Gh.install_hint(),
        ) {
            actions.push(Act::Refresh(Section::GitHubAccount).into());
        }
        return;
    }
    let r = &a.report;
    if let Some(e) = &r.status_error {
        if w::error_card(ui, "gh auth status failed", e, "Try again") {
            actions.push(Act::Refresh(Section::GitHubAccount).into());
        }
        return;
    }
    if r.signed_out {
        kit::callout_with(
            ui,
            Tone::Warning,
            "gh is not signed in",
            "Review, Actions, Ship, Govern and Home need a GitHub account. In a terminal:",
            |ui| {
                ui.add_space(4.0);
                kit::code_block(ui, "gh auth login", actions);
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(
                        "It asks for the host and protocol, prints a one-time code and opens github.com/login/device in your browser. Press Refresh afterwards.",
                    )
                    .size(size::SMALL)
                    .color(pal(ui).text),
                );
            },
        );
        return;
    }
    let mut hosts: Vec<&str> = Vec::new();
    for acc in &r.accounts {
        if !hosts.contains(&acc.host.as_str()) {
            hosts.push(&acc.host);
        }
    }
    for host in hosts {
        let list: Vec<&Account> = r.accounts.iter().filter(|x| x.host == host).collect();
        kit::section(ui, host, Some(list.len()));
        for acc in list {
            account_card(ui, acc, safe, busy, actions);
            ui.add_space(space::S);
        }
    }
    if let Some(user) = &r.user {
        kit::section(ui, "Signed in as", None);
        match user {
            Ok(u) => {
                kit::kv(ui, 150.0, |kv| {
                    kv.text("Login", kit::mono(u.login.as_str()));
                    match &u.name {
                        Some(n) if !n.is_empty() => kv.text("Name", n.clone()),
                        _ => kv.muted("Name", "not set on GitHub"),
                    }
                    if let Some(url) = &u.html_url {
                        kv.row("Profile", |ui| {
                            if kit::link(ui, url).clicked() {
                                actions.push(Action::OpenUrl(url.clone()));
                            }
                        });
                    }
                    if let Some(c) = &u.created_at {
                        kv.text("Account created", c.get(..10).unwrap_or(c).to_owned());
                    }
                    if let Some(n) = u.public_repos {
                        kv.text("Public repositories", format::count(n as usize));
                    }
                    if let Some(t) = u.two_factor_authentication {
                        kv.text(
                            "Two-factor",
                            if t {
                                "On"
                            } else {
                                "Off: turn it on at github.com/settings/security"
                            },
                        );
                    }
                });
            }
            Err(e) => {
                if w::error_card(ui, "gh api user failed", e, "Try again") {
                    actions.push(Act::Refresh(Section::GitHubAccount).into());
                }
            }
        }
    }
    if let Some(rate) = &r.rate {
        kit::section(ui, "Rate limits", None);
        match rate {
            Ok(b) => rate_limits(ui, b),
            Err(e) => {
                if w::error_card(ui, "gh api rate_limit failed", e, "Try again") {
                    actions.push(Act::Refresh(Section::GitHubAccount).into());
                }
            }
        }
    }
    scopes(ui, r.active(), actions);
}

fn account_card(ui: &mut Ui, acc: &Account, safe: bool, busy: bool, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    form::card(
        ui,
        if acc.ok() {
            Tone::Neutral
        } else {
            Tone::Danger
        },
        |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                kit::subtitle(ui, &acc.login);
                if acc.active {
                    kit::badge(ui, "Active", Tone::Success);
                }
                if !acc.ok() {
                    kit::badge(
                        ui,
                        if acc.state.is_empty() {
                            "Problem"
                        } else if acc.state == "timeout" {
                            "Timed out"
                        } else {
                            "Not working"
                        },
                        Tone::Danger,
                    );
                }
            });
            ui.add_space(2.0);
            kit::kv(ui, 130.0, |kv| {
                match acc.token_label() {
                    Some(l) => kv.text("Token", l),
                    None => kv.muted("Token", "kept by gh; gitvis never reads it"),
                }
                if !acc.token_source.is_empty() {
                    kv.text("Stored in", acc.token_source.clone());
                }
                if !acc.protocol.is_empty() {
                    kv.text("Git protocol", acc.protocol.clone());
                }
                match &acc.scopes {
                Some(s) if !s.is_empty() => kv.row("Scopes", |ui| {
                    w::badges(ui, &s.iter().map(|x| (x.clone(), Tone::Neutral)).collect::<Vec<_>>());
                }),
                Some(_) => kv.muted("Scopes", "none"),
                None => kv.muted(
                    "Scopes",
                    "not reported for this token (fine-grained and app tokens are checked per request)",
                ),
            }
                if let Some(e) = &acc.error {
                    kv.text("Problem", egui::RichText::new(e.as_str()).color(p.danger));
                }
            });
            if !acc.ok() {
                ui.add_space(2.0);
                kit::code_block(ui, &format!("gh auth login -h {}", acc.host), actions);
            }
            if !acc.active && acc.ok() {
                w::buttons(ui, |ui| {
                    if IconButton::new(Icon::Rerun, &format!("Switch to {}…", acc.login))
                        .compact()
                        .enabled(!busy && !safe)
                        .show(ui)
                        .clicked()
                    {
                        actions.push(
                            Act::SwitchAccount {
                                host: acc.host.clone(),
                                login: acc.login.clone(),
                            }
                            .into(),
                        );
                    }
                });
                if safe {
                    kit::muted(ui, "Safe mode is on: gitvis does not run gh auth switch.");
                }
            }
        },
    );
}

fn rate_limits(ui: &mut Ui, buckets: &[account::Bucket]) {
    let now = format::now_secs();
    let reset = |t: i64| {
        let mins = ((t - now).max(0) + 59) / 60;
        if mins <= 1 {
            "resets within a minute".to_owned()
        } else {
            format!("resets in {mins} min")
        }
    };
    let tiles: Vec<(String, String, Tone)> = buckets
        .iter()
        .filter(|b| matches!(b.name.as_str(), "core" | "graphql" | "search"))
        .map(|b| {
            let label = match b.name.as_str() {
                "core" => "REST API",
                "graphql" => "GraphQL",
                _ => "Search",
            };
            (
                format!(
                    "{} of {}",
                    format::count(b.remaining as usize),
                    format::count(b.limit as usize)
                ),
                format!("{label} left · {}", reset(b.reset)),
                if b.limit > 0 && b.remaining * 10 < b.limit {
                    Tone::Warning
                } else {
                    Tone::Neutral
                },
            )
        })
        .collect();
    let stats: Vec<Stat> = tiles
        .iter()
        .map(|(v, l, t)| Stat::new(l, v.clone()).tone(*t))
        .collect();
    insights::stats(ui, &stats);
    let rows: Vec<Vec<Cell>> = buckets
        .iter()
        .map(|b| {
            vec![
                Cell::Mono(b.name.clone()),
                Cell::Sec(format::count(b.used as usize)),
                Cell::Text(format::count(b.remaining as usize)),
                Cell::Sec(format::count(b.limit as usize)),
                Cell::Muted(reset(b.reset)),
            ]
        })
        .collect();
    insights::grid(
        ui,
        "identity_rate",
        &[
            Col::text("Bucket"),
            Col::num("Used"),
            Col::num("Left"),
            Col::num("Limit"),
            Col::text("Reset"),
        ],
        &rows,
        false,
        |_, _| {},
    );
    kit::muted(ui, "gh api rate_limit does not count against the limits.");
}

fn scopes(ui: &mut Ui, active: Option<&Account>, actions: &mut Vec<Action>) {
    kit::section(ui, "What the token allows", None);
    let Some(acc) = active else {
        kit::muted(ui, "No active account.");
        return;
    };
    kit::secondary(
        ui,
        &format!(
            "gitvis features and the token scopes they need, for {} on {}. gh auth login asks for gist, read:org, repo and workflow.",
            acc.login, acc.host
        ),
    );
    let analysis = account::analyse(acc.scopes.as_deref());
    let rows: Vec<Vec<Cell>> = analysis
        .iter()
        .map(|(f, need)| {
            vec![
                Cell::Text(f.name.to_owned()),
                Cell::Sec(f.place.to_owned()),
                Cell::Mono(f.any_of.join(" or ")),
                match need {
                    Need::Ok(by) => Cell::Badges(vec![(format!("Granted by {by}"), Tone::Success)]),
                    Need::Missing(s) => Cell::Badges(vec![(format!("Missing {s}"), Tone::Danger)]),
                    Need::Unknown => Cell::Badges(vec![("Not reported".to_owned(), Tone::Neutral)]),
                },
            ]
        })
        .collect();
    insights::grid(
        ui,
        "identity_scopes",
        &[
            Col::text("Feature"),
            Col::text("Where"),
            Col::text("Needs"),
            Col::text("Status"),
        ],
        &rows,
        false,
        |_, _| {},
    );
    let missing = account::missing_scopes(&analysis);
    if missing.is_empty() {
        if acc.scopes.is_some() {
            ui.add_space(4.0);
            kit::callout(
                ui,
                Tone::Success,
                "Every feature is covered",
                "The token has every scope gitvis uses.",
            );
        }
        return;
    }
    kit::section(ui, "Add the missing scopes", Some(missing.len()));
    kit::callout(
        ui,
        Tone::Neutral,
        "How gh auth refresh works",
        &format!(
            "It adds scopes to the token of the active account ({}); the scopes it has stay. gh prints a one-time code and opens github.com/login/device in your browser, where you approve the new scopes. gitvis does not run it for you: copy it into a terminal. For another account, switch to it first.",
            acc.login
        ),
    );
    ui.add_space(4.0);
    w::label(ui, "Everything at once");
    kit::code_block(ui, &account::refresh_command(&acc.host, &missing), actions);
    for &s in &missing {
        let features: Vec<&str> = analysis
            .iter()
            .filter(|(_, n)| *n == Need::Missing(s))
            .map(|(f, _)| f.name)
            .collect();
        ui.add_space(4.0);
        w::label(ui, &format!("{s}: {}", features.join(", ")));
        kit::code_block(ui, &account::refresh_command(&acc.host, &[s]), actions);
    }
}

fn credentials(
    ui: &mut Ui,
    cfg: Option<&Snapshot>,
    account: Option<&AccountData>,
    safe: bool,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    kit::section(ui, "Git credentials", None);
    kit::secondary(
        ui,
        "How git signs in to HTTPS remotes: credential.helper, and helpers for one host (credential.<url>.helper), which win for that host. An empty value clears the helpers listed before it.",
    );
    let Some(cfg) = cfg else {
        kit::loading(ui, "Reading git config");
        return;
    };
    let home = git_home();
    let c = account::credentials(&cfg.entries);
    if c.helpers.is_empty() {
        kit::callout(
            ui,
            Tone::Warning,
            "No credential helper",
            "git asks for a user name and password (a token) on every HTTPS push; gitvis cannot answer such prompts. Git Credential Manager (credential.helper=manager) or gh auth setup-git fix that.",
        );
    } else {
        let rows: Vec<Vec<Cell>> = c
            .helpers
            .iter()
            .map(|e| {
                vec![
                    if e.value_text().is_empty() {
                        Cell::Muted("(empty: clears the helpers before it)".to_owned())
                    } else {
                        Cell::Mono(e.value_text().to_owned())
                    },
                    Cell::Sec(e.subsection().unwrap_or("every host").to_owned()),
                    Cell::Badges(vec![(e.scope.title().to_owned(), w::scope_tone(e.scope))]),
                    Cell::Muted(e.location(home.as_deref())),
                ]
            })
            .collect();
        insights::grid(
            ui,
            "identity_creds",
            &[
                Col::text("Helper"),
                Col::text("For"),
                Col::text("Scope"),
                Col::text("Where"),
            ],
            &rows,
            false,
            |_, _| {},
        );
    }
    ui.add_space(4.0);
    let gh_installed = account.is_none_or(|a| a.gh.is_some());
    if c.gh_for_github {
        kit::callout(
            ui,
            Tone::Success,
            "gh answers for github.com",
            "git over HTTPS to github.com uses the token of gh's active account (set up by gh auth setup-git).",
        );
    } else if gh_installed {
        kit::callout_with(
            ui,
            Tone::Neutral,
            "gh is not git's credential helper for github.com",
            &format!(
                "git asks {} for github.com credentials. gh auth setup-git makes gh answer instead, with the account you signed in with; it writes two credential entries to {}.",
                c.helpers
                    .iter()
                    .rev()
                    .find(|e| e.subsection().is_none() && !e.value_text().is_empty())
                    .map(|e| e.value_text().to_owned())
                    .unwrap_or_else(|| "nobody".to_owned()),
                cfg.global_file
                    .as_deref()
                    .map(|f| tilde(f, home.as_deref()))
                    .unwrap_or_else(|| "~/.gitconfig".to_owned())
            ),
            |ui| {
                ui.add_space(4.0);
                kit::code_block(ui, "gh auth setup-git", actions);
                ui.add_space(2.0);
                if IconButton::new(Icon::Gear, "Run gh auth setup-git…")
                    .compact()
                    .enabled(!busy && !safe)
                    .show(ui)
                    .clicked()
                {
                    actions.push(Act::SetupGit.into());
                }
                if safe {
                    kit::muted(
                        ui,
                        "Safe mode is on: gitvis does not run gh auth subcommands that change something.",
                    );
                }
            },
        );
    }
}
