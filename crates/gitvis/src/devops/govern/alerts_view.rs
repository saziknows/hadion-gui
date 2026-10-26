//! Dependabot and code-scanning alerts: list, detail, fix guidance and the
//! dismiss / reopen forms.

use std::collections::BTreeMap;

use egui::{RichText, Ui};

use gitvis_core::devops::govern::Feature;
use gitvis_core::devops::govern::alerts::{
    self, CodeAlert, CodeReason, DependabotAlert, DependabotReason, Severity, StateFilter,
};

use super::widgets::{self, severity_badge, severity_dot};
use super::{Act, AlertKind, DismissForm, State};
use crate::format;
use crate::state::{Action, Page, RepoState};
use crate::theme::{pal, size};
use crate::views::form;
use crate::views::icons::{Icon, IconButton};
use crate::views::insights::{Cell, grid, mono, num};
use crate::views::kit::{self, Col, Leading, Row, Tone};

const FILTERS: [(StateFilter, &str); 4] = [
    (StateFilter::Open, "Open"),
    (StateFilter::Dismissed, "Dismissed"),
    (StateFilter::Fixed, "Fixed"),
    (StateFilter::All, "All"),
];

fn matches(search: &str, fields: &[&str]) -> bool {
    let q = search.trim().to_lowercase();
    q.is_empty() || fields.iter().any(|f| f.to_lowercase().contains(&q))
}

fn filter_dep<'a>(all: &'a [DependabotAlert], q: &str) -> Vec<&'a DependabotAlert> {
    all.iter()
        .filter(|a| {
            matches(
                q,
                &[
                    &a.package,
                    &a.summary,
                    &a.manifest,
                    &a.ghsa,
                    a.cve.as_deref().unwrap_or(""),
                    &a.ecosystem,
                ],
            )
        })
        .collect()
}

fn filter_code<'a>(all: &'a [CodeAlert], q: &str) -> Vec<&'a CodeAlert> {
    all.iter()
        .filter(|a| {
            matches(
                q,
                &[&a.rule_id, &a.description, &a.path, &a.tool, &a.message],
            )
        })
        .collect()
}

/// Dependabot alerts after the filter box, in display order.
pub fn visible_dependabot(st: &State) -> Vec<&DependabotAlert> {
    st.dependabot
        .ok()
        .map(|v| filter_dep(v, &st.dep_search))
        .unwrap_or_default()
}

/// Code-scanning alerts after the filter box.
pub fn visible_code(st: &State) -> Vec<&CodeAlert> {
    st.code
        .ok()
        .map(|v| filter_code(v, &st.code_search))
        .unwrap_or_default()
}

fn state_badge(state: &str) -> (String, Tone) {
    match state {
        "open" => ("Open".into(), Tone::Accent),
        "fixed" => ("Fixed".into(), Tone::Success),
        "dismissed" => ("Dismissed".into(), Tone::Neutral),
        "auto_dismissed" => ("Auto-dismissed".into(), Tone::Neutral),
        other => (other.to_owned(), Tone::Neutral),
    }
}

#[allow(clippy::too_many_arguments)]
fn header(
    ui: &mut Ui,
    title: &str,
    subtitle: &str,
    filter: StateFilter,
    kind: AlertKind,
    fetch: &super::Fetch<impl Sized>,
    shown: usize,
    actions: &mut Vec<Action>,
) {
    kit::page_header(ui, title, subtitle, |ui| {
        widgets::refresh(
            ui,
            fetch.loading,
            "Read the alerts again (one request)",
            actions,
        );
        if let Some(f) = kit::segmented(ui, &FILTERS, filter) {
            actions.push(Act::SetAlertFilter(kind, f).into());
        }
        ui.add_space(6.0);
        let extra = if shown >= 100 {
            "first 100".to_owned()
        } else {
            String::new()
        };
        widgets::fetched(ui, fetch, &extra);
    });
}

fn exposure_link(ui: &mut Ui, actions: &mut Vec<Action>) {
    ui.add_space(4.0);
    kit::callout_with(
        ui,
        Tone::Neutral,
        "Secret scanning alerts",
        "Leaked credentials that GitHub found are listed with the local secret scan on the Exposure page.",
        |ui| {
            if IconButton::new(Icon::Shield, "Open the Exposure page")
                .compact()
                .show(ui)
                .clicked()
            {
                actions.push(Action::ShowPage(Page::Security));
            }
        },
    );
}

// ---------------------------------------------------------------------------
// Dependabot
// ---------------------------------------------------------------------------

pub fn dependabot(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) {
    let gh = widgets::gh(repo);
    let st = &repo.devops.govern;
    let shown = st.dependabot.ok().map(|v| v.len()).unwrap_or(0);
    header(
        ui,
        "Dependabot alerts",
        "Vulnerable dependencies in the manifests GitHub knows, from GitHub's advisory database",
        st.dep_filter,
        AlertKind::Dependabot,
        &st.dependabot,
        shown,
        actions,
    );
    if let Some(e) = st.dependabot.error.clone() {
        kit::scroll_page(ui, "gov_dep_err", |ui| {
            widgets::denied(
                ui,
                gh.as_ref(),
                repo.devops.govern.access.ok(),
                Feature::Dependabot,
                &e,
                actions,
            );
            exposure_link(ui, actions);
        });
        return;
    }
    if st.dependabot.ok().is_none() {
        kit::scroll_page(ui, "gov_dep_wait", |ui| {
            widgets::loading(ui, "Reading Dependabot alerts from GitHub…");
        });
        return;
    }
    let sel = st.dep_selected;
    let key = sel.map(|n| n.to_string()).unwrap_or_default();
    let RepoState { devops, github, .. } = repo;
    let prs: &[gitvis_core::github::PullRequest] =
        github.as_ref().map(|g| g.prs.as_slice()).unwrap_or(&[]);
    let State {
        dependabot,
        dep_search,
        dismiss,
        dep_packages,
        dep_filter,
        ..
    } = &mut devops.govern;
    let filter = *dep_filter;
    let all: &[DependabotAlert] = dependabot.ok().map(|v| v.as_slice()).unwrap_or(&[]);
    let visible = filter_dep(all, dep_search);
    let packages: &[gitvis_core::osint::Package] = dep_packages.as_deref().unwrap_or(&[]);
    kit::split_keyed(
        ui,
        "gov_dep",
        &key,
        actions,
        |ui, actions| {
            widgets::list_head(
                ui,
                "Alerts",
                visible.len(),
                sel.is_some(),
                Act::SelectAlert(AlertKind::Dependabot, None),
                actions,
            );
            widgets::list_filter(
                ui,
                "gov_dep_search",
                dep_search,
                "Package, advisory, manifest",
            );
            if visible.is_empty() {
                widgets::list_none(
                    ui,
                    if all.is_empty() {
                        match filter {
                            StateFilter::Open => {
                                "No open Dependabot alerts: every dependency GitHub knows is clear of known advisories."
                            }
                            _ => "No alerts in this state.",
                        }
                    } else {
                        "Nothing matches the filter."
                    },
                );
                return;
            }
            kit::list(
                ui,
                "gov_dep_list",
                visible.len(),
                kit::ROW_H,
                |i| sel == Some(visible[i].number),
                |ui, rect, resp, i| {
                    let a = &visible[i];
                    let title = format!("{}  {}", a.package, a.summary);
                    let sub = format!("{} · {} · {}", a.ecosystem, a.manifest, a.vulnerable_range);
                    let mut badges = vec![severity_badge(a.severity)];
                    if !a.is_open() {
                        badges.push(state_badge(&a.state));
                    }
                    kit::paint_row(
                        ui,
                        rect,
                        &Row::new(&title)
                            .leading(Leading::Dot(severity_dot(ui, a.severity)))
                            .subtitle(&sub)
                            .meta(&format!("#{}", a.number))
                            .badges(&badges)
                            .dimmed(!a.is_open()),
                        sel == Some(a.number),
                    );
                    if resp.clicked() {
                        actions
                            .push(Act::SelectAlert(AlertKind::Dependabot, Some(a.number)).into());
                    }
                },
            );
        },
        |ui, actions| match sel.and_then(|n| all.iter().find(|a| a.number == n)) {
            Some(a) => dependabot_detail(ui, a, packages, prs, gh.as_ref(), dismiss, actions),
            None => dependabot_overview(ui, all, prs, filter, actions),
        },
    );
}

fn dependabot_overview(
    ui: &mut Ui,
    all: &[DependabotAlert],
    prs: &[gitvis_core::github::PullRequest],
    filter: StateFilter,
    actions: &mut Vec<Action>,
) {
    let counts = alerts::severity_counts(all.iter().map(|a| a.severity));
    let patched = all.iter().filter(|a| a.first_patched.is_some()).count();
    let direct = all.iter().filter(|a| a.relationship == "direct").count();
    let runtime = all.iter().filter(|a| a.scope == "runtime").count();
    let with_pr = all
        .iter()
        .filter(|a| alerts::dependabot_pr(a, prs).is_some())
        .count();
    let mut tiles = widgets::severity_tiles(&counts);
    tiles.extend([
        kit::Stat::new("Alerts", all.len().to_string()),
        kit::Stat::new("Have a patched version", patched.to_string()),
        kit::Stat::new("Direct dependencies", direct.to_string())
            .tip("Alerts on dependencies the manifests name directly (the rest are transitive)"),
        kit::Stat::new("Runtime scope", runtime.to_string())
            .tip("Alerts on runtime dependencies (the rest are development-only)"),
        kit::Stat::new("Open Dependabot PRs", with_pr.to_string())
            .tip("Alerts with an open pull request from Dependabot that updates the package"),
    ]);
    crate::views::insights::stats(ui, &tiles);
    kit::secondary(
        ui,
        &format!(
            "{} alerts ({}). Select one for the advisory, the fix and the Dependabot pull request.",
            filter.label(),
            format::plural(all.len(), "alert", "alerts")
        ),
    );
    if !all.is_empty() {
        let mut eco: BTreeMap<String, u32> = BTreeMap::new();
        let mut manifests: BTreeMap<String, (usize, Severity)> = BTreeMap::new();
        for a in all {
            *eco.entry(a.ecosystem.clone()).or_insert(0) += 1;
            let m = manifests
                .entry(a.manifest.clone())
                .or_insert((0, Severity::Unknown));
            m.0 += 1;
            m.1 = m.1.max(a.severity);
        }
        kit::section(ui, "By ecosystem", Some(eco.len()));
        let mut items: Vec<(String, u32)> = eco.into_iter().collect();
        items.sort_by_key(|a| std::cmp::Reverse(a.1));
        kit::count_bars(ui, &items, pal(ui).accent, 12);
        kit::section(ui, "By manifest", Some(manifests.len()));
        let mut rows: Vec<(String, usize, Severity)> =
            manifests.into_iter().map(|(k, (n, s))| (k, n, s)).collect();
        rows.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)));
        let cells: Vec<Vec<Cell>> = rows
            .iter()
            .take(40)
            .map(|(m, n, s)| {
                vec![
                    mono(m.as_str()),
                    num(*n),
                    Cell::Badges(vec![severity_badge(*s)]),
                ]
            })
            .collect();
        grid(
            ui,
            "gov_dep_manifests",
            &[
                Col::text("Manifest"),
                Col::num("Alerts"),
                Col::text("Worst"),
            ],
            &cells,
            false,
            |_, _| {},
        );
    }
    exposure_link(ui, actions);
}

fn dependabot_detail(
    ui: &mut Ui,
    a: &DependabotAlert,
    packages: &[gitvis_core::osint::Package],
    prs: &[gitvis_core::github::PullRequest],
    gh: Option<&gitvis_core::devops::gh::Gh>,
    dismiss: &mut Option<DismissForm>,
    actions: &mut Vec<Action>,
) {
    let mut badges = vec![severity_badge(a.severity), state_badge(&a.state)];
    if !a.ecosystem.is_empty() {
        badges.push((a.ecosystem.clone(), Tone::Neutral));
    }
    if a.scope == "development" {
        badges.push(("Development only".into(), Tone::Neutral));
    }
    kit::title(ui, &a.summary);
    badge_row(ui, &badges);
    ui.add_space(4.0);
    let installed = alerts::installed_version(a, packages);
    kit::kv(ui, 150.0, |kv| {
        kv.text("Package", kit::mono(&a.package));
        kv.text("Manifest", kit::mono(&a.manifest));
        if !a.relationship.is_empty() || !a.scope.is_empty() {
            kv.text(
                "Dependency",
                format!(
                    "{}{}",
                    a.relationship,
                    if a.scope.is_empty() {
                        String::new()
                    } else {
                        format!(", {} scope", a.scope)
                    }
                ),
            );
        }
        kv.text("Vulnerable versions", kit::mono(&a.vulnerable_range));
        match &a.first_patched {
            Some(v) => kv.text("First patched", kit::mono(v)),
            None => kv.muted("First patched", "no patched version yet"),
        }
        match &installed {
            Some(v) => kv.text("Locked here", kit::mono(v)),
            None => kv.muted("Locked here", "not found in the local lockfiles"),
        }
        kv.text("Advisory", kit::mono(a.ids()));
        if let Some(score) = a.cvss_score {
            kv.text(
                "CVSS",
                format!(
                    "{score:.1} ({}){}",
                    Severity::from_score(score).label(),
                    a.cvss_vector
                        .as_ref()
                        .map(|v| format!("  ·  {v}"))
                        .unwrap_or_default()
                ),
            );
        }
        if !a.cwes.is_empty() {
            kv.text(
                "Weakness",
                a.cwes
                    .iter()
                    .map(|(id, n)| format!("{id} {n}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        if let Some((pct, pctl)) = a.epss {
            kv.text(
                "EPSS",
                format!(
                    "{:.2}% chance of exploitation in 30 days (higher than {:.0}% of advisories)",
                    pct * 100.0,
                    pctl * 100.0
                ),
            );
        }
        kv.text("Published", widgets::when(a.published));
        kv.text("Alert opened", widgets::when(a.created));
        if a.updated.is_some() {
            kv.text("Updated", widgets::when(a.updated));
        }
        if a.dismissed_at.is_some() {
            kv.text(
                "Dismissed",
                format!(
                    "{} by {}",
                    widgets::when(a.dismissed_at),
                    a.dismissed_by.as_deref().unwrap_or("?")
                ),
            );
            if let Some(r) = &a.dismissed_reason {
                kv.text("Reason", r.replace('_', " "));
            }
            if let Some(c) = &a.dismissed_comment {
                kv.text("Comment", c.as_str());
            }
        }
        if a.fixed_at.is_some() {
            kv.text("Fixed", widgets::when(a.fixed_at));
        }
    });
    alert_buttons(
        ui,
        AlertKind::Dependabot,
        a.number,
        a.is_open(),
        &a.html_url,
        dismiss,
        actions,
    );
    if let Some(d) = dismiss
        && d.kind == AlertKind::Dependabot
        && d.number == a.number
    {
        dismiss_form(ui, d, actions);
    }

    kit::section(ui, "Fix", None);
    let fix = alerts::fix_guidance(a, installed.as_deref());
    kit::body(ui, &fix.text);
    if let Some(cmd) = &fix.command {
        let full = if fix.dir.is_empty() {
            cmd.clone()
        } else {
            format!("cd {} && {cmd}", fix.dir)
        };
        kit::code_block(ui, &full, actions);
    }
    match alerts::dependabot_pr(a, prs) {
        Some(pr) => {
            ui.add_space(2.0);
            kit::callout_with(
                ui,
                Tone::Success,
                &format!("Dependabot opened pull request #{}", pr.number),
                &pr.title,
                |ui| {
                    ui.horizontal(|ui| {
                        if IconButton::new(Icon::PullRequest, "Show the pull request")
                            .compact()
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Action::OpenPr(pr.number));
                        }
                        if IconButton::new(Icon::External, "Open on GitHub")
                            .compact()
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Action::OpenUrl(pr.url.clone()));
                        }
                    });
                },
            );
        }
        None => {
            kit::muted(
                ui,
                &format!(
                    "No open Dependabot pull request for {} (head dependabot/{}/…). With Dependabot security updates on, GitHub opens one by itself.",
                    a.package,
                    alerts::dependabot_dir(&a.ecosystem)
                ),
            );
        }
    }

    if !a.description.trim().is_empty() {
        kit::section(ui, "Advisory", None);
        kit::body(
            ui,
            &gitvis_core::devops::govern::clip(a.description.trim(), 4000),
        );
    }
    if !a.references.is_empty() {
        kit::section(ui, "References", Some(a.references.len()));
        for r in a.references.iter().take(20) {
            if kit::link(ui, r).clicked() {
                actions.push(Action::OpenUrl(r.clone()));
            }
        }
    }
    if let Some(gh) = gh {
        ui.add_space(8.0);
        kit::muted(
            ui,
            &format!(
                "gh api {}  ·  advisory: https://github.com/advisories/{}",
                alerts::dependabot_endpoint(gh, StateFilter::Open),
                a.ghsa
            ),
        );
    }
}

fn badge_row(ui: &mut Ui, badges: &[(String, Tone)]) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        for (t, tone) in badges {
            kit::badge(ui, t, *tone);
        }
    });
}

fn alert_buttons(
    ui: &mut Ui,
    kind: AlertKind,
    number: u64,
    open: bool,
    url: &str,
    dismiss: &Option<DismissForm>,
    actions: &mut Vec<Action>,
) {
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if !url.is_empty()
            && IconButton::new(Icon::External, "Open on GitHub")
                .compact()
                .show(ui)
                .clicked()
        {
            actions.push(Action::OpenUrl(url.to_owned()));
        }
        if open {
            let showing = dismiss
                .as_ref()
                .is_some_and(|d| d.kind == kind && d.number == number);
            if !showing
                && IconButton::new(Icon::Close, "Dismiss…")
                    .compact()
                    .show(ui)
                    .clicked()
            {
                actions.push(Act::OpenDismiss(kind, number).into());
            }
        } else if IconButton::new(Icon::Undo, "Reopen")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Act::ReviewReopen(kind, number).into());
        }
        if IconButton::new(Icon::Copy, "Copy link")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::Copy(url.to_owned()));
        }
    });
}

fn dismiss_form(ui: &mut Ui, d: &mut DismissForm, actions: &mut Vec<Action>) {
    ui.add_space(6.0);
    form::card(ui, Tone::Neutral, |ui| {
        kit::subtitle(ui, &format!("Dismiss alert #{}", d.number));
        ui.add_space(4.0);
        form::field(ui, "Reason", None, |ui| match d.kind {
            AlertKind::Dependabot => {
                for r in DependabotReason::ALL {
                    if form::radio(ui, d.dep_reason == r, r.label()) {
                        d.dep_reason = r;
                    }
                }
            }
            AlertKind::Code => {
                for r in CodeReason::ALL {
                    if form::radio(ui, d.code_reason == r, r.label()) {
                        d.code_reason = r;
                    }
                }
            }
        });
        let left = alerts::MAX_COMMENT as i64 - d.comment.chars().count() as i64;
        let help = format!(
            "Optional. GitHub keeps it with the alert; {} characters left.",
            left.max(0)
        );
        form::field(ui, "Comment", Some(&help), |ui| {
            form::area(
                ui,
                ("gov_dismiss_comment", d.number),
                &mut d.comment,
                "Why this is safe to ignore",
                3,
            );
        });
        if left < 0 {
            form::error(ui, "The comment is cut to 280 characters.");
        }
        form::actions_row(ui, |ui| {
            if form::primary(ui, "Review dismissal", true).clicked() {
                actions.push(Act::ReviewDismiss.into());
            }
            if form::button(ui, "Cancel", true).clicked() {
                actions.push(Act::CloseDismiss.into());
            }
        });
    });
}

// ---------------------------------------------------------------------------
// Code scanning
// ---------------------------------------------------------------------------

pub fn code(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) {
    let gh = widgets::gh(repo);
    let st = &repo.devops.govern;
    let shown = st.code.ok().map(|v| v.len()).unwrap_or(0);
    header(
        ui,
        "Code scanning",
        "Results of CodeQL and other SARIF analyses uploaded to GitHub",
        st.code_filter,
        AlertKind::Code,
        &st.code,
        shown,
        actions,
    );
    if let Some(e) = st.code.error.clone() {
        kit::scroll_page(ui, "gov_code_err", |ui| {
            widgets::denied(
                ui,
                gh.as_ref(),
                repo.devops.govern.access.ok(),
                Feature::CodeScanning,
                &e,
                actions,
            );
            exposure_link(ui, actions);
        });
        return;
    }
    if st.code.ok().is_none() {
        kit::scroll_page(ui, "gov_code_wait", |ui| {
            widgets::loading(ui, "Reading code scanning alerts from GitHub…");
        });
        return;
    }
    let sel = st.code_selected;
    let key = sel.map(|n| n.to_string()).unwrap_or_default();
    let State {
        code,
        code_search,
        dismiss,
        snippets,
        code_filter,
        ..
    } = &mut repo.devops.govern;
    let filter = *code_filter;
    let all: &[CodeAlert] = code.ok().map(|v| v.as_slice()).unwrap_or(&[]);
    let visible = filter_code(all, code_search);
    let snippets = &*snippets;
    kit::split_keyed(
        ui,
        "gov_code",
        &key,
        actions,
        |ui, actions| {
            widgets::list_head(
                ui,
                "Alerts",
                visible.len(),
                sel.is_some(),
                Act::SelectAlert(AlertKind::Code, None),
                actions,
            );
            widgets::list_filter(ui, "gov_code_search", code_search, "Rule, path, tool");
            if visible.is_empty() {
                widgets::list_none(
                    ui,
                    if all.is_empty() {
                        match filter {
                            StateFilter::Open => {
                                "No open code scanning alerts in the latest analysis."
                            }
                            _ => "No alerts in this state.",
                        }
                    } else {
                        "Nothing matches the filter."
                    },
                );
                return;
            }
            kit::list(
                ui,
                "gov_code_list",
                visible.len(),
                kit::ROW_H,
                |i| sel == Some(visible[i].number),
                |ui, rect, resp, i| {
                    let a = &visible[i];
                    let title = if a.description.is_empty() {
                        a.rule_id.clone()
                    } else {
                        a.description.clone()
                    };
                    let sub = format!("{} · {}", a.location(), a.tool);
                    let mut badges = vec![severity_badge(a.severity())];
                    if !a.is_open() {
                        badges.push(state_badge(&a.state));
                    }
                    kit::paint_row(
                        ui,
                        rect,
                        &Row::new(&title)
                            .leading(Leading::Dot(severity_dot(ui, a.severity())))
                            .subtitle(&sub)
                            .meta(&format!("#{}", a.number))
                            .badges(&badges)
                            .dimmed(!a.is_open()),
                        sel == Some(a.number),
                    );
                    if resp.clicked() {
                        actions.push(Act::SelectAlert(AlertKind::Code, Some(a.number)).into());
                    }
                },
            );
        },
        |ui, actions| match sel.and_then(|n| all.iter().find(|a| a.number == n)) {
            Some(a) => code_detail(ui, a, snippets.get(&a.number), dismiss, actions),
            None => code_overview(ui, all, filter, actions),
        },
    );
}

fn code_overview(ui: &mut Ui, all: &[CodeAlert], filter: StateFilter, actions: &mut Vec<Action>) {
    let counts = alerts::severity_counts(all.iter().map(|a| a.severity()));
    let mut tiles = widgets::severity_tiles(&counts);
    let tools: BTreeMap<&str, u32> = all.iter().fold(BTreeMap::new(), |mut m, a| {
        *m.entry(a.tool.as_str()).or_insert(0) += 1;
        m
    });
    let files: std::collections::BTreeSet<&str> = all.iter().map(|a| a.path.as_str()).collect();
    tiles.extend([
        kit::Stat::new("Alerts", all.len().to_string()),
        kit::Stat::new("Files", files.len().to_string()),
        kit::Stat::new("Tools", tools.len().to_string()),
    ]);
    crate::views::insights::stats(ui, &tiles);
    kit::secondary(
        ui,
        &format!(
            "{} alerts. Select one for the rule, the code it points at in your working tree, and the dismiss options.",
            filter.label()
        ),
    );
    if !all.is_empty() {
        kit::section(ui, "By rule", None);
        let mut rules: BTreeMap<String, (u32, Severity)> = BTreeMap::new();
        for a in all {
            let e = rules
                .entry(a.rule_id.clone())
                .or_insert((0, Severity::Unknown));
            e.0 += 1;
            e.1 = e.1.max(a.severity());
        }
        let mut rows: Vec<(String, u32, Severity)> =
            rules.into_iter().map(|(k, (n, s))| (k, n, s)).collect();
        rows.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)));
        let cells: Vec<Vec<Cell>> = rows
            .iter()
            .take(40)
            .map(|(r, n, s)| {
                vec![
                    mono(r.as_str()),
                    num(*n as usize),
                    Cell::Badges(vec![severity_badge(*s)]),
                ]
            })
            .collect();
        grid(
            ui,
            "gov_code_rules",
            &[Col::text("Rule"), Col::num("Alerts"), Col::text("Worst")],
            &cells,
            false,
            |_, _| {},
        );
        kit::section(ui, "By tool", Some(tools.len()));
        let items: Vec<(String, u32)> = tools.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
        kit::count_bars(ui, &items, pal(ui).accent, 10);
    }
    exposure_link(ui, actions);
}

fn code_detail(
    ui: &mut Ui,
    a: &CodeAlert,
    snippet: Option<&Option<alerts::Snippet>>,
    dismiss: &mut Option<DismissForm>,
    actions: &mut Vec<Action>,
) {
    let title = if a.description.is_empty() {
        a.rule_id.clone()
    } else {
        a.description.clone()
    };
    kit::title(ui, &title);
    let mut badges = vec![severity_badge(a.severity()), state_badge(&a.state)];
    if a.security_severity.is_some() {
        badges.push((format!("Rule severity: {}", a.rule_severity), Tone::Neutral));
    }
    badge_row(ui, &badges);
    ui.add_space(4.0);
    kit::kv(ui, 130.0, |kv| {
        kv.text("Rule", kit::mono(&a.rule_id));
        kv.text(
            "Tool",
            if a.tool_version.is_empty() {
                a.tool.clone()
            } else {
                format!("{} {}", a.tool, a.tool_version)
            },
        );
        kv.text("Location", kit::mono(a.location()));
        if !a.git_ref.is_empty() {
            kv.text(
                "Analysed",
                format!(
                    "{} at {}",
                    a.git_ref.trim_start_matches("refs/heads/"),
                    a.commit_sha.get(..10).unwrap_or(&a.commit_sha)
                ),
            );
        }
        if !a.tags.is_empty() {
            kv.text("Tags", a.tags.join(", "));
        }
        kv.text("Opened", widgets::when(a.created));
        if a.dismissed_at.is_some() {
            kv.text(
                "Dismissed",
                format!(
                    "{} by {}",
                    widgets::when(a.dismissed_at),
                    a.dismissed_by.as_deref().unwrap_or("?")
                ),
            );
            if let Some(r) = &a.dismissed_reason {
                kv.text("Reason", r.as_str());
            }
            if let Some(c) = &a.dismissed_comment {
                kv.text("Comment", c.as_str());
            }
        }
        if a.fixed_at.is_some() {
            kv.text("Fixed", widgets::when(a.fixed_at));
        }
    });
    alert_buttons(
        ui,
        AlertKind::Code,
        a.number,
        a.is_open(),
        &a.html_url,
        dismiss,
        actions,
    );
    if let Some(d) = dismiss
        && d.kind == AlertKind::Code
        && d.number == a.number
    {
        dismiss_form(ui, d, actions);
    }
    if !a.message.is_empty() {
        kit::section(ui, "Message", None);
        kit::body(ui, &a.message);
    }
    kit::section(ui, "Code in the working tree", None);
    match snippet {
        None => kit::loading(ui, &format!("Reading {}…", a.path)),
        Some(None) => {
            kit::muted(
                ui,
                &format!(
                    "{} is not in the working tree (deleted, renamed, or binary), so there is no code to show.",
                    a.path
                ),
            );
        }
        Some(Some(s)) => {
            if s.short {
                kit::callout(
                    ui,
                    Tone::Warning,
                    "The file changed since the analysis",
                    &format!(
                        "The alert points at line {}, but the file now has fewer lines. Showing what is there.",
                        a.start_line
                    ),
                );
            }
            if s.lines.is_empty() {
                kit::muted(ui, "No lines at that position.");
            } else {
                widgets::code_lines(ui, s.first, &s.lines, s.highlight);
                ui.label(
                    RichText::new(format!(
                        "{}  ·  {} highlighted  ·  from the working tree, which may differ from the analysed commit",
                        a.path,
                        if s.highlight.0 == s.highlight.1 {
                            format!("line {}", s.highlight.0)
                        } else {
                            format!("lines {}–{}", s.highlight.0, s.highlight.1)
                        }
                    ))
                    .size(size::SMALL)
                    .color(pal(ui).text_2),
                );
            }
        }
    }
    let about = if !a.full_description.is_empty() {
        a.full_description.as_str()
    } else {
        ""
    };
    if !about.is_empty() || !a.help.is_empty() {
        kit::section(ui, "About the rule", None);
        if !about.is_empty() {
            kit::body(ui, about);
        }
        if !a.help.is_empty() {
            egui::CollapsingHeader::new(RichText::new("Rule help (Markdown)").size(size::SMALL))
                .id_salt(("gov_code_help", a.number))
                .show(ui, |ui| {
                    kit::code_block(
                        ui,
                        &gitvis_core::devops::govern::clip(&a.help, 6000),
                        actions,
                    );
                });
        }
    }
}
