//! The DORA metrics tab: source and period, the four metrics with their
//! bands, weekly charts, the deployments of the period, how it is
//! computed and how much of it local history covers.

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Layout, Rect, Sense, Stroke, Ui, pos2, vec2,
};

use hadion_core::devops::ship::deploy;
use hadion_core::devops::ship::dora::{self, Band, FIX_WINDOW_DAYS, Outcome, Report, THRESHOLDS};

use super::{Act, DoraSource, State, count, ui as sui};
use crate::devops::pages::explain;
use crate::format;
use crate::state::{Action, RepoState, Settings};
use crate::theme::{self, Tone, pal, radius, size};
use crate::views::form;
use crate::views::icons::{Icon, IconButton};
use crate::views::insights::{self as ins, Cell};
use crate::views::kit::{self, Col};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Env,
    Releases,
    Tags,
    Workflow,
}

fn kind_of(s: Option<&DoraSource>) -> Option<Kind> {
    Some(match s? {
        DoraSource::Environment(_) => Kind::Env,
        DoraSource::Releases => Kind::Releases,
        DoraSource::Tags(_) => Kind::Tags,
        DoraSource::Workflow(..) => Kind::Workflow,
    })
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, _settings: &Settings, actions: &mut Vec<Action>) {
    let st = &repo.devops.ship;
    let d = &st.dora;
    let subtitle = match &d.source {
        Some(s) => format!("From {} · last {} days", s.describe(), d.period),
        None => {
            "Deployment frequency, lead time, change failure rate and time to restore".to_owned()
        }
    };
    kit::page_header(ui, "DORA metrics", &subtitle, |ui| {
        if IconButton::new(Icon::Rerun, "Refresh")
            .compact()
            .enabled(!d.events.loading && !d.local.loading)
            .show(ui)
            .on_hover_text(
                "Read the deployments (or releases, tags, runs) and the local history again",
            )
            .clicked()
        {
            actions.push(Act::DoraRefresh.into());
        }
        if d.events.loading || d.local.loading {
            ui.add(egui::Spinner::new().size(14.0));
        }
    });
    sui::scroll_page(ui, "ship_dora_page", |ui| {
        controls(ui, repo, actions);
        let st = &repo.devops.ship;
        let d = &st.dora;
        ui.add_space(6.0);
        if d.source.is_none() {
            kit::loading(ui, "Looking for a production environment and releases…");
            return;
        }
        if let Some(e) = d.events.err() {
            sui::inline_error(ui, "the deployments", e, Act::DoraRefresh, actions);
            return;
        }
        if d.events.loading {
            kit::loading(
                ui,
                &format!(
                    "Reading {}…",
                    d.source.as_ref().map(|s| s.describe()).unwrap_or_default()
                ),
            );
            return;
        }
        if d.local.loading {
            kit::loading(
                ui,
                "Reading the commits each deployment brought from local history…",
            );
            return;
        }
        let Some(rep) = &d.report else {
            return;
        };
        if rep.coverage.events == 0 {
            let all = d.events.ok().map(Vec::len).unwrap_or(0);
            let other = "Count something else above: a production environment, published releases, version tags or a deploy workflow.";
            let (title, body) = if all == 0 {
                let what = match &d.source {
                    Some(DoraSource::Tags(p)) => format!("No tag matches {p} in this repository."),
                    Some(DoraSource::Releases) => {
                        "This repository has no published release.".to_owned()
                    }
                    Some(DoraSource::Environment(e)) => {
                        format!("Nothing was deployed to {e} in the last 240 days.")
                    }
                    Some(DoraSource::Workflow(_, n)) => {
                        format!(
                            "{n} has no completed run on the default branch in the last 240 days."
                        )
                    }
                    None => String::new(),
                };
                ("Nothing to count".to_owned(), format!("{what}\n{other}"))
            } else {
                (
                    "Nothing deployed in this period".to_owned(),
                    format!(
                        "{} {} read, none in the last {} days.\nPick a longer period, or another source.",
                        format::count(all),
                        if all == 1 { "event was" } else { "events were" },
                        rep.days
                    ),
                )
            };
            kit::empty_state(ui, Icon::Gauge, &title, &body, None);
            how(ui, st);
            return;
        }
        coverage(ui, st, rep);
        metrics(ui, rep);
        charts(ui, rep);
        rows(ui, repo, rep, actions);
        how(ui, &repo.devops.ship);
    });
}

fn controls(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) {
    let workflows: Vec<(u64, String)> = repo
        .github
        .as_ref()
        .map(|g| g.workflows.iter().map(|w| (w.id, w.name.clone())).collect())
        .unwrap_or_default();
    let has_gh = repo.github_remote.is_some() && repo.devops.ship.gh_ok != Some(false);
    let st: &mut State = &mut repo.devops.ship;
    let envs = st.env_order();
    let kind = kind_of(st.dora.source.as_ref());
    // Plain rows on the canvas: a segmented control on a raised card is
    // invisible in the light theme (both are the raised colour).
    let label_w = 168.0;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        form::row(ui, label_w, "Count as a deployment", |ui| {
            let opts = [
                (Some(Kind::Env), "Environment"),
                (Some(Kind::Releases), "Releases"),
                (Some(Kind::Tags), "Tags"),
                (Some(Kind::Workflow), "Workflow runs"),
            ];
            if let Some(k) = kit::segmented(ui, &opts, kind).flatten() {
                let s = match k {
                    Kind::Env => envs
                        .iter()
                        .rev()
                        .find(|e| deploy::is_production(e))
                        .or_else(|| envs.last())
                        .map(|e| DoraSource::Environment(e.clone())),
                    Kind::Releases => Some(DoraSource::Releases),
                    Kind::Tags => {
                        Some(DoraSource::Tags(if st.dora.tag_pattern.trim().is_empty() {
                            "v*".to_owned()
                        } else {
                            st.dora.tag_pattern.trim().to_owned()
                        }))
                    }
                    Kind::Workflow => workflows
                        .iter()
                        .find(|(_, n)| {
                            let n = n.to_lowercase();
                            n.contains("deploy") || n.contains("release") || n.contains("publish")
                        })
                        .or_else(|| workflows.first())
                        .map(|(id, n)| DoraSource::Workflow(*id, n.clone())),
                };
                match s {
                    Some(s) => actions.push(Act::DoraSource(s).into()),
                    None => actions.push(Action::Toast(
                        crate::state::ToastKind::Info,
                        match k {
                            Kind::Env => "This repository has no environment".to_owned(),
                            _ => "No workflow found (open the Actions page once to load them)"
                                .to_owned(),
                        },
                    )),
                }
            }
        });
        form::row(ui, label_w, "Period", |ui| {
            let periods = [(30u32, "30 days"), (90, "90 days"), (180, "180 days")];
            if let Some(p) = kit::segmented(ui, &periods, st.dora.period) {
                actions.push(Act::DoraPeriod(p).into());
            }
        });
        let mut help: Vec<&str> = Vec::new();
        match st.dora.source.clone() {
            Some(DoraSource::Environment(cur)) => {
                form::row(ui, label_w, "Environment", |ui| {
                    let mut v = cur.clone();
                    let opts: Vec<(String, String)> = if envs.is_empty() {
                        vec![(cur.clone(), cur.clone())]
                    } else {
                        envs.iter().map(|e| (e.clone(), e.clone())).collect()
                    };
                    if form::select(ui, "ship_dora_env", &mut v, &opts, 220.0) {
                        actions.push(Act::DoraSource(DoraSource::Environment(v)).into());
                    }
                });
                help.push("Its successful and failed deployments count.");
            }
            Some(DoraSource::Tags(cur)) => {
                form::row(ui, label_w, "Tag pattern", |ui| {
                    let resp = form::input(
                        ui,
                        "ship_dora_tags",
                        &mut st.dora.tag_pattern,
                        form::InputOpts::new("v*").width(160.0).mono(),
                    );
                    let changed = st.dora.tag_pattern.trim() != cur;
                    if (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && changed)
                        || form::button(ui, "Apply", changed).clicked()
                    {
                        actions.push(
                            Act::DoraSource(DoraSource::Tags(st.dora.tag_pattern.trim().to_owned()))
                                .into(),
                        );
                    }
                });
                help.push("A git for-each-ref glob. Each tag is a successful deployment, dated by its creation.");
            }
            Some(DoraSource::Workflow(id, _)) => {
                form::row(ui, label_w, "Workflow", |ui| {
                    let mut v = id;
                    let opts: Vec<(u64, String)> = workflows.clone();
                    if form::select(ui, "ship_dora_workflow", &mut v, &opts, 260.0)
                        && let Some((wid, n)) = workflows.iter().find(|(w, _)| *w == v)
                    {
                        actions.push(Act::DoraSource(DoraSource::Workflow(*wid, n.clone())).into());
                    }
                });
                help.push(
                    "Its completed runs on the default branch: a success deploys, a failure fails.",
                );
            }
            Some(DoraSource::Releases) => {
                help.push("Published releases (not drafts or pre-releases), dated by publication.");
                help.push("A release cannot fail, so only the revert / hotfix heuristic estimates failures.");
            }
            None => {}
        }
        if !has_gh {
            help.push("Without a GitHub remote (or gh), only local tags can be counted.");
        }
        if !help.is_empty() {
            // Under the controls, which start one item spacing after the
            // label column.
            ui.horizontal_top(|ui| {
                ui.add_space(label_w + ui.spacing().item_spacing.x);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for h in help {
                        kit::secondary(ui, h);
                    }
                });
            });
        }
    });
    ui.add_space(4.0);
    kit::hairline(ui);
}

fn coverage(ui: &mut Ui, st: &State, rep: &Report) {
    let c = &rep.coverage;
    kit::secondary(
        ui,
        &format!(
            "{} in the last {} days, {} with a lead time.",
            count(c.events, "event", "events"),
            rep.days,
            count(c.lead_commits, "commit", "commits")
        ),
    );
    if c.folded > 0 {
        kit::secondary(
            ui,
            &format!(
                "{} of one commit within an hour folded into one.",
                count(c.folded, "repeated deployment", "repeated deployments")
            ),
        );
    }
    if c.missing_local > 0 {
        kit::callout(
            ui,
            Tone::Warning,
            &format!(
                "{} of {} deployed commits are not in the local history",
                format::count(c.missing_local),
                format::count(c.with_rev)
            ),
            "Lead time needs the commits each deployment brought, which only local history has. Fetch (git fetch --all --tags) to include them.",
        );
    }
    if c.first_without_predecessor {
        kit::callout(
            ui,
            Tone::Neutral,
            "The first deployment of the period has no earlier one",
            "Its commits cannot be told apart from all of history, so they are left out of the lead time.",
        );
    }
    if c.truncated > 0 {
        kit::muted(
            ui,
            &format!(
                "{} brought more than {} commits; only the newest were read.",
                count(c.truncated, "deployment", "deployments"),
                format::count(dora::MAX_COMMITS_PER_DEPLOY)
            ),
        );
    }
    let _ = st;
}

/// One metric card: caption, value, band, explanation.
fn metric_card(
    ui: &mut Ui,
    w: f32,
    label: &str,
    value: &str,
    band: Option<Band>,
    lines: &[String],
) {
    let p = *pal(ui);
    ui.allocate_ui_with_layout(vec2(w, 150.0), Layout::top_down(Align::Min), |ui| {
        ui.set_width(w);
        form::card(ui, Tone::Neutral, |ui| {
            ui.set_min_height(118.0);
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.label(kit::caption_text(&p, label));
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(value)
                        .size(size::DISPLAY)
                        .family(theme::semibold())
                        .color(p.text_strong),
                );
                if let Some(b) = band {
                    kit::badge(ui, b.label(), sui::band_tone(b));
                }
            });
            for l in lines {
                ui.add(
                    egui::Label::new(egui::RichText::new(l).size(size::SMALL).color(p.text_2))
                        .wrap(),
                );
            }
        });
    });
}

fn metrics(ui: &mut Ui, rep: &Report) {
    kit::section(ui, "The four keys", None);
    let avail = ui.available_width();
    let cols = if avail > 1000.0 { 4 } else { 2 };
    let w = (avail - 12.0 * (cols as f32 - 1.0)) / cols as f32;
    let freq_value = if rep.per_day >= 1.0 {
        format!("{:.1}/day", rep.per_day)
    } else if rep.per_week >= 1.0 {
        format!("{:.1}/week", rep.per_week)
    } else {
        format!("{:.1}/month", rep.per_day * 30.0)
    };
    let lead_value = rep
        .lead
        .map(|l| sui::span(l.median))
        .unwrap_or_else(|| "–".to_owned());
    let cfr_value = rep
        .failure_rate
        .map(|r| format!("{:.0}%", r * 100.0))
        .unwrap_or_else(|| "–".to_owned());
    let restore_value = rep
        .restore
        .map(|r| sui::span(r.median))
        .unwrap_or_else(|| "–".to_owned());
    let cards: Vec<(&str, String, Option<Band>, Vec<String>)> = vec![
        (
            "Deployment frequency",
            freq_value,
            rep.band_frequency,
            vec![
                format!(
                    "{} successful in {} days",
                    count(rep.successes, "deployment", "deployments"),
                    rep.days
                ),
                format!("{:.2} per day · {:.1} per week", rep.per_day, rep.per_week),
            ],
        ),
        (
            "Lead time for changes",
            lead_value,
            rep.band_lead,
            match rep.lead {
                Some(l) => vec![
                    format!("Median of {}", count(l.n, "commit", "commits")),
                    format!("p90 {} · mean {}", sui::span(l.p90), sui::span(l.mean)),
                ],
                None => vec!["No commit with a known deployment in the period.".to_owned()],
            },
        ),
        ("Change failure rate", cfr_value, rep.band_failure, {
            let mut v = vec![format!(
                "{} of {} completed failed",
                format::count(rep.failures),
                format::count(rep.successes + rep.failures)
            )];
            if let Some(h) = rep.fix_rate {
                v.push(format!(
                    "{} of {} followed by a revert or hotfix within {FIX_WINDOW_DAYS} days ({:.0}%)",
                    rep.fix_followed,
                    rep.successes,
                    h * 100.0
                ));
            }
            v
        }),
        (
            "Time to restore",
            restore_value,
            rep.band_restore,
            match rep.restore {
                Some(r) => vec![
                    format!("Median of {}", count(r.n, "incident", "incidents")),
                    if rep.unresolved > 0 {
                        "One failure is not restored yet".to_owned()
                    } else {
                        format!("Longest {}", sui::span(r.p90.max(r.median)))
                    },
                ],
                None if rep.unresolved > 0 => {
                    vec!["The only failure is not restored yet.".to_owned()]
                }
                None => vec!["No failed deployment in the period.".to_owned()],
            },
        ),
    ];
    for chunk in cards.chunks(cols) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            for (label, value, band, lines) in chunk {
                metric_card(ui, w, label, value, *band, lines);
            }
        });
        ui.add_space(4.0);
    }
    if !rep.fix_examples.is_empty() {
        // One line per fix commit, with the deployments it followed.
        let mut by_fix: Vec<(&str, Vec<&str>)> = Vec::new();
        for (label, subject) in &rep.fix_examples {
            match by_fix.iter_mut().find(|(s, _)| s == subject) {
                Some((_, labels)) => labels.push(label),
                None => by_fix.push((subject, vec![label])),
            }
        }
        let lines: Vec<String> = by_fix
            .iter()
            .map(|(s, labels)| format!("\"{s}\" after {}", labels.join(", ")))
            .collect();
        kit::callout(
            ui,
            Tone::Neutral,
            "Deployments followed by a revert or hotfix",
            &lines.join("\n"),
        );
    }
}

/// Weekly bars: `values[i] = (week start, main, stacked)`.
fn week_chart(
    ui: &mut Ui,
    id: &str,
    values: &[(i64, f64, f64)],
    main: Color32,
    stacked: Color32,
    tip: impl Fn(&(i64, f64, f64)) -> String,
    axis: impl Fn(f64) -> String,
) {
    let p = *pal(ui);
    let h = 150.0;
    let w = ui.available_width().min(kit::DETAIL_MAX);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h + 22.0), Sense::hover());
    let painter = ui.painter_at(rect.expand(2.0));
    let max = values
        .iter()
        .map(|v| v.1 + v.2)
        .fold(0.0, f64::max)
        .max(1e-9);
    let plot = Rect::from_min_max(
        pos2(rect.left() + 44.0, rect.top() + 6.0),
        pos2(rect.right(), rect.top() + h),
    );
    // Axis: zero and max.
    painter.hline(plot.x_range(), plot.bottom(), Stroke::new(1.0, p.border));
    painter.hline(
        plot.x_range(),
        plot.top(),
        Stroke::new(1.0, p.border.gamma_multiply(0.5)),
    );
    painter.text(
        pos2(plot.left() - 8.0, plot.top()),
        Align2::RIGHT_CENTER,
        axis(max),
        FontId::proportional(size::CAPTION),
        p.text_3,
    );
    painter.text(
        pos2(plot.left() - 8.0, plot.bottom()),
        Align2::RIGHT_CENTER,
        "0",
        FontId::proportional(size::CAPTION),
        p.text_3,
    );
    let n = values.len().max(1);
    let slot = plot.width() / n as f32;
    let bw = (slot * 0.7).clamp(3.0, 36.0);
    let mut hovered: Option<usize> = None;
    let pointer = resp.hover_pos();
    for (i, v) in values.iter().enumerate() {
        let x = plot.left() + slot * (i as f32 + 0.5);
        let total = v.1 + v.2;
        let hm = (v.1 / max) as f32 * plot.height();
        let hs = (v.2 / max) as f32 * plot.height();
        if total > 0.0 {
            let r = Rect::from_min_max(
                pos2(x - bw / 2.0, plot.bottom() - hm),
                pos2(x + bw / 2.0, plot.bottom()),
            );
            painter.rect_filled(r, CornerRadius::same(radius::S), main);
            if hs > 0.0 {
                let r2 = Rect::from_min_max(
                    pos2(x - bw / 2.0, plot.bottom() - hm - hs),
                    pos2(x + bw / 2.0, plot.bottom() - hm),
                );
                painter.rect_filled(r2, CornerRadius::same(radius::S), stacked);
            }
        }
        if pointer.is_some_and(|q| (q.x - x).abs() <= slot / 2.0) {
            hovered = Some(i);
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(x - slot / 2.0, plot.top()),
                    pos2(x + slot / 2.0, plot.bottom()),
                ),
                CornerRadius::same(radius::S),
                p.hover.gamma_multiply(0.5),
            );
        }
        // A date under every few bars.
        let every = (n / 6).max(1);
        if i % every == 0 {
            painter.text(
                pos2(x, plot.bottom() + 12.0),
                Align2::CENTER_CENTER,
                short_date(v.0),
                FontId::proportional(size::CAPTION),
                p.text_3,
            );
        }
    }
    let _ = id;
    if let Some(i) = hovered {
        resp.on_hover_text_at_pointer(tip(&values[i]));
    }
}

/// Colour swatches with their labels, then a note.
fn legend(ui: &mut Ui, items: &[(Color32, &str)], note: &str) {
    let p = *pal(ui);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (c, label) in items {
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().rect_filled(r, CornerRadius::same(2), *c);
            ui.label(
                egui::RichText::new(*label)
                    .size(size::SMALL)
                    .color(p.text_2),
            );
            ui.add_space(8.0);
        }
        ui.label(egui::RichText::new(note).size(size::SMALL).color(p.text_2));
    });
}

/// `2026-10-26` (local).
fn day(t: i64) -> String {
    let a = format::absolute(t);
    a.get(..10).unwrap_or(&a).to_owned()
}

fn short_date(t: i64) -> String {
    // `2026-10-26 12:00` → `10-26`
    let a = format::absolute(t);
    a.get(5..10).unwrap_or(&a).to_owned()
}

fn charts(ui: &mut Ui, rep: &Report) {
    let p = *pal(ui);
    kit::section(
        ui,
        "Deployments per week",
        Some(rep.successes + rep.failures),
    );
    let values: Vec<(i64, f64, f64)> = rep
        .weeks
        .iter()
        .map(|w| (w.start, w.deploys as f64, w.failures as f64))
        .collect();
    week_chart(
        ui,
        "ship_dora_weeks",
        &values,
        p.accent,
        p.danger,
        |v| {
            format!(
                "Week of {}: {} successful, {} failed",
                day(v.0),
                v.1 as u64,
                v.2 as u64
            )
        },
        |m| format!("{m:.0}"),
    );
    legend(
        ui,
        &[(p.accent, "Successful"), (p.danger, "Failed (on top)")],
        "Weeks start on Monday (UTC).",
    );
    let lead: Vec<(i64, f64, f64)> = rep
        .weeks
        .iter()
        .map(|w| (w.start, w.lead_median.unwrap_or(0) as f64 / 3_600.0, 0.0))
        .collect();
    if lead.iter().any(|v| v.1 > 0.0) {
        kit::section(ui, "Lead time per week (median)", None);
        week_chart(
            ui,
            "ship_dora_lead",
            &lead,
            p.purple,
            p.purple,
            |v| {
                format!(
                    "Week of {}: median lead time {}",
                    day(v.0),
                    if v.1 > 0.0 {
                        sui::span((v.1 * 3_600.0) as i64)
                    } else {
                        "none".to_owned()
                    }
                )
            },
            |m| {
                if m > 96.0 {
                    format!("{:.0} d", m / 24.0)
                } else {
                    format!("{m:.0} h")
                }
            },
        );
    }
}

fn rows(ui: &mut Ui, repo: &RepoState, rep: &Report, actions: &mut Vec<Action>) {
    let now = format::now_secs();
    kit::section(ui, "Deployments in the period", Some(rep.rows.len()));
    let cols = [
        Col::text("When"),
        Col::text("Deployment"),
        Col::text("Outcome"),
        Col::text("By"),
        Col::num("New commits"),
        Col::num("Median lead"),
        Col::text("Notes"),
    ];
    let shown: Vec<&dora::Row> = rep.rows.iter().take(80).collect();
    let cells: Vec<Vec<Cell>> = shown
        .iter()
        .map(|r| {
            let (o, t) = match r.outcome {
                Outcome::Success => ("Success", Tone::Success),
                Outcome::Failure => ("Failure", Tone::Danger),
                Outcome::Other => ("Other", Tone::Neutral),
            };
            let mut notes = Vec::new();
            if r.fix_followed {
                notes.push(format!("revert or hotfix within {FIX_WINDOW_DAYS} d"));
            }
            if let Some(s) = r.restored_after {
                notes.push(format!("restored after {}", sui::span(s)));
            }
            if r.sha.is_none() {
                notes.push("not in local history".to_owned());
            }
            vec![
                ins::muted(sui::when(r.time, now)),
                if r.sha.as_deref().is_some_and(|s| sui::in_graph(repo, s)) {
                    Cell::MonoLink(r.label.clone())
                } else {
                    Cell::Mono(r.label.clone())
                },
                Cell::Badges(vec![(o.to_owned(), t)]),
                ins::sec(r.creator.clone()),
                r.new_commits
                    .map(ins::num)
                    .unwrap_or_else(|| ins::muted("–")),
                r.lead_median
                    .map(|l| ins::sec(sui::span(l)))
                    .unwrap_or_else(|| ins::muted("–")),
                ins::muted(notes.join(" · ")),
            ]
        })
        .collect();
    crate::devops::pages::layout::wide(ui, "ship_dora_rows", 860.0, |ui| {
        ins::grid(ui, "ship_dora_rows", &cols, &cells, true, |i, resp| {
            if resp.clicked()
                && let Some(sha) = &shown[i].sha
            {
                actions.push(Act::JumpTo(sha.clone()).into());
            }
        });
    });
    ins::more(ui, rep.rows.len(), shown.len());
}

/// What the four metrics mean (one plain sentence each), with the method
/// and the performance bands folded away.
fn how(ui: &mut Ui, st: &State) {
    kit::section(ui, "How this is computed", None);
    let source = st
        .dora
        .source
        .as_ref()
        .map(|s| s.describe())
        .unwrap_or_else(|| "the chosen source".to_owned());
    kit::body(ui, &format!("Deployments here are the {source}."));
    ui.add_space(2.0);
    explain::terms(
        ui,
        TERM_W,
        &[
            ("Deployment frequency", "How often a deployment succeeds."),
            (
                "Lead time for changes",
                "How long a commit takes to reach a successful deployment.",
            ),
            ("Change failure rate", "The share of deployments that fail."),
            (
                "Time to restore",
                "How long it takes from a failed deployment to the next successful one.",
            ),
        ],
    );
    ui.add_space(4.0);
    explain::details(
        ui,
        "ship_dora_method",
        "Method, step by step",
        false,
        |ui| {
            let lead = format!(
                "For each successful deployment: the commits it brought that the previous {} deployed commits did not have.\nRead from local history: git log NEW ^PREV …\nLead time = deployment time − commit (committer) time.\nMedian and 90th percentile over all those commits.",
                dora::EXCLUDE_PREVIOUS
            );
            let failure = format!(
                "Failed or errored deployments ÷ all completed ones.\nSeparately, a heuristic counts the successful deployments followed within {FIX_WINDOW_DAYS} days by a deployment that brought a revert (\"Revert …\") or a hotfix commit."
            );
            explain::terms(
                ui,
                TERM_W - 26.0,
                &[
                    (
                        "Deployments",
                        "Repeated deployments of the same commit within an hour count once (parallel jobs of one run).",
                    ),
                    (
                        "Deployment frequency",
                        "Successful deployments in the period ÷ its days.\n× 7 for a week.",
                    ),
                    ("Lead time for changes", &lead),
                    ("Change failure rate", &failure),
                    (
                        "Time to restore",
                        "From a failed deployment to the next successful one in the same environment.\nConsecutive failures are one incident.\nMedian over the incidents of the period.",
                    ),
                ],
            );
        },
    );
    explain::details(
        ui,
        "ship_dora_bands",
        "Performance bands (DORA State of DevOps 2023 clusters)",
        false,
        bands,
    );
}

/// Width of the term column in the explanations.
const TERM_W: f32 = 200.0;

/// The band thresholds: a table where it fits, a group per metric where
/// it does not.
fn bands(ui: &mut Ui) {
    // The table needs about 950 points; below that its cells are cut.
    if ui.available_width() >= 960.0 {
        let rows: Vec<Vec<String>> = THRESHOLDS
            .iter()
            .map(|(m, b)| {
                let mut r = vec![(*m).to_owned()];
                r.extend(b.iter().map(|s| (*s).to_owned()));
                r
            })
            .collect();
        kit::table(
            ui,
            &[
                Col::text("Metric"),
                Col::text("Elite"),
                Col::text("High"),
                Col::text("Medium"),
                Col::text("Low"),
            ],
            &rows,
        );
        return;
    }
    for (m, b) in THRESHOLDS {
        ui.add_space(8.0);
        kit::subtitle(ui, m);
        let rows: Vec<(&str, &str)> = [Band::Elite, Band::High, Band::Medium, Band::Low]
            .iter()
            .zip(b.iter())
            .map(|(band, t)| (band.label(), *t))
            .collect();
        explain::terms(ui, 90.0, &rows);
    }
}
