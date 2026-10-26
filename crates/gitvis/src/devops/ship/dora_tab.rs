//! The DORA metrics tab: source and period, the four metrics with their
//! bands, weekly charts, the deployments of the period, how it is
//! computed and how much of it local history covers.

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Layout, Rect, Sense, Stroke, Ui, pos2, vec2,
};

use gitvis_core::devops::ship::deploy;
use gitvis_core::devops::ship::dora::{self, Band, FIX_WINDOW_DAYS, Outcome, Report, THRESHOLDS};

use super::{Act, DoraSource, State, count, ui as sui};
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
    let mut offset = repo.devops.ship.scroll_to.take();
    sui::scroll_page(ui, "ship_dora_page", &mut offset, |ui| {
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
            kit::empty_state(
                ui,
                Icon::Gauge,
                "Nothing deployed in this period",
                &format!(
                    "{} {} read in total, none in the last {} days. Pick a longer period or another source (a production environment, published releases, version tags or a deploy workflow).",
                    format::count(all),
                    if all == 1 { "event was" } else { "events were" },
                    rep.days
                ),
                None,
            );
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
    form::card(ui, Tone::Neutral, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            kit::secondary(ui, "Count as a deployment");
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
            ui.add_space(12.0);
            kit::secondary(ui, "Period");
            let periods = [(30u32, "30 days"), (90, "90 days"), (180, "180 days")];
            if let Some(p) = kit::segmented(ui, &periods, st.dora.period) {
                actions.push(Act::DoraPeriod(p).into());
            }
        });
        ui.add_space(6.0);
        match st.dora.source.clone() {
            Some(DoraSource::Environment(cur)) => {
                ui.horizontal(|ui| {
                    kit::secondary(ui, "Environment");
                    let mut v = cur.clone();
                    let opts: Vec<(String, String)> = if envs.is_empty() {
                        vec![(cur.clone(), cur.clone())]
                    } else {
                        envs.iter().map(|e| (e.clone(), e.clone())).collect()
                    };
                    if form::select(ui, "ship_dora_env", &mut v, &opts, 220.0) {
                        actions.push(Act::DoraSource(DoraSource::Environment(v)).into());
                    }
                    kit::muted(ui, "Successful and failed deployments to it.");
                });
            }
            Some(DoraSource::Tags(cur)) => {
                ui.horizontal(|ui| {
                    kit::secondary(ui, "Tag pattern");
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
                    kit::muted(ui, "A git for-each-ref glob, dated by each tag's creation; every tag counts as a successful deployment.");
                });
            }
            Some(DoraSource::Workflow(id, _)) => {
                ui.horizontal(|ui| {
                    kit::secondary(ui, "Workflow");
                    let mut v = id;
                    let opts: Vec<(u64, String)> = workflows.clone();
                    if form::select(ui, "ship_dora_workflow", &mut v, &opts, 260.0)
                        && let Some((wid, n)) = workflows.iter().find(|(w, _)| *w == v)
                    {
                        actions.push(Act::DoraSource(DoraSource::Workflow(*wid, n.clone())).into());
                    }
                    kit::muted(
                        ui,
                        "Completed runs on the default branch: success deploys, failure fails.",
                    );
                });
            }
            Some(DoraSource::Releases) => {
                kit::muted(
                    ui,
                    "Published releases (not drafts or pre-releases), dated by publication. A release has no failed state, so only the revert / hotfix heuristic estimates failures.",
                );
            }
            None => {}
        }
        if !has_gh {
            kit::muted(
                ui,
                "Without a GitHub remote (or gh), only local tags can be counted.",
            );
        }
    });
}

fn coverage(ui: &mut Ui, st: &State, rep: &Report) {
    let c = &rep.coverage;
    let mut parts = vec![format!(
        "{} in the last {} days",
        count(c.events, "event", "events"),
        rep.days
    )];
    if c.folded > 0 {
        parts.push(format!(
            "{} repeated deployments of one commit within an hour folded into one",
            format::count(c.folded)
        ));
    }
    parts.push(format!(
        "{} with a lead time",
        count(c.lead_commits, "commit", "commits")
    ));
    kit::secondary(ui, &parts.join(" · "));
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
    } else {
        format!("{:.1}/week", rep.per_week)
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
                        "Heuristic: {:.0}% ({} of {}) followed by a revert or hotfix within {FIX_WINDOW_DAYS} days",
                        h * 100.0,
                        rep.fix_followed,
                        rep.successes
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
                &format::absolute(v.0)[..10],
                v.1 as u64,
                v.2 as u64
            )
        },
        |m| format!("{m:.0}"),
    );
    kit::muted(
        ui,
        "Blue: successful deployments; red on top: failed ones. Weeks start on Monday (UTC).",
    );
    let lead: Vec<(i64, f64, f64)> = rep
        .weeks
        .iter()
        .map(|w| (w.start, w.lead_median.unwrap_or(0) as f64 / 3_600.0, 0.0))
        .collect();
    if lead.iter().any(|v| v.1 > 0.0) {
        kit::section(ui, "Lead time per week (median, hours)", None);
        week_chart(
            ui,
            "ship_dora_lead",
            &lead,
            p.purple,
            p.purple,
            |v| {
                format!(
                    "Week of {}: median lead time {}",
                    &format::absolute(v.0)[..10],
                    if v.1 > 0.0 {
                        sui::span((v.1 * 3_600.0) as i64)
                    } else {
                        "none".to_owned()
                    }
                )
            },
            |m| format!("{m:.0} h"),
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
                ins::muted(format!(
                    "{} · {}",
                    format::absolute(r.time),
                    format::relative(r.time, now)
                )),
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
    ins::grid(ui, "ship_dora_rows", &cols, &cells, true, |i, resp| {
        if resp.clicked()
            && let Some(sha) = &shown[i].sha
        {
            actions.push(Act::JumpTo(sha.clone()).into());
        }
    });
    ins::more(ui, rep.rows.len(), shown.len());
}

fn how(ui: &mut Ui, st: &State) {
    kit::section(ui, "How this is computed", None);
    let source = st
        .dora
        .source
        .as_ref()
        .map(|s| s.describe())
        .unwrap_or_else(|| "the chosen source".to_owned());
    let points = [
        format!(
            "Deployments are {source}. Repeated deployments of the same commit within an hour count once (parallel jobs of one run)."
        ),
        "Deployment frequency: successful deployments in the period divided by its days (and times seven for a week).".to_owned(),
        format!(
            "Lead time for changes: for each successful deployment, the commits it brought that the previous {} deployed commits did not have (git log NEW ^PREV …, read locally); lead time = deployment time − commit (committer) time. Median and 90th percentile over all those commits.",
            dora::EXCLUDE_PREVIOUS
        ),
        format!(
            "Change failure rate: failed or errored deployments over all completed ones. Separately, a heuristic counts successful deployments followed within {FIX_WINDOW_DAYS} days by a deployment that brought a revert (\"Revert …\") or a hotfix commit."
        ),
        "Time to restore: from a failed deployment to the next successful one in the same environment; consecutive failures are one incident. Median over the incidents of the period.".to_owned(),
    ];
    for t in points {
        kit::body(ui, &format!("•  {t}"));
    }
    ui.add_space(6.0);
    kit::subtitle(ui, "Bands (DORA State of DevOps 2023 clusters)");
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
}
