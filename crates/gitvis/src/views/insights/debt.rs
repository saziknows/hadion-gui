//! Debt: TODO/FIXME markers.

use egui::Ui;

use gitvis_core::insights::InsightsReport;
use gitvis_core::insights::days_between;
use gitvis_core::insights::debt::{KINDS, Marker};

use super::{
    Cx, age_tone, button_row, commit_ref, days_text, detail_title, dot_color, list_none, list_top,
    path_label, when, why,
};
use crate::format;
use crate::state::{Action, ScanRun};
use crate::theme::pal;
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Leading, Row, Stat, Tone};

pub fn marker_key(m: &Marker) -> String {
    format!("{}:{}", m.path, m.line)
}

/// Markers that pass the filter (path, text, kind or assignee), in list
/// order.
pub fn visible<'a>(rep: &'a InsightsReport, filter: &str) -> Vec<&'a Marker> {
    let needle = filter.trim().to_lowercase();
    rep.debt
        .markers
        .iter()
        .filter(|m| {
            needle.is_empty()
                || m.path.to_lowercase().contains(&needle)
                || m.text.to_lowercase().contains(&needle)
                || m.kind.to_lowercase() == needle
                || m.assignee
                    .as_deref()
                    .is_some_and(|a| a.to_lowercase().contains(&needle))
        })
        .collect()
}

/// Known-broken markers are a problem; hacks are a warning; the rest is
/// information.
fn kind_tone(kind: &str) -> Tone {
    match kind {
        "FIXME" | "BUG" | "NOCOMMIT" => Tone::Danger,
        "HACK" | "XXX" | "KLUDGE" | "TEMP" => Tone::Warning,
        _ => Tone::Neutral,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    cx: &Cx,
    rep: &InsightsReport,
    selected: &mut Option<String>,
    filter: &mut String,
    dating_run: &Option<ScanRun>,
    actions: &mut Vec<Action>,
) {
    let d = &rep.debt;
    let items = visible(rep, filter);
    let sel = selected.clone();
    let mut f = filter.clone();
    kit::split_keyed(
        ui,
        "ins_debt",
        sel.as_deref().unwrap_or(""),
        actions,
        |ui, actions| {
            list_top(
                ui,
                "Markers",
                items.len(),
                sel.is_some(),
                Some((
                    "ins_debt_filter",
                    &mut f,
                    "Filter: path, text, kind, assignee",
                )),
                actions,
            );
            if items.is_empty() {
                list_none(
                    ui,
                    if d.markers.is_empty() {
                        "No TODO/FIXME style markers in the tracked text files."
                    } else {
                        "No markers match."
                    },
                );
                return;
            }
            let p = *pal(ui);
            kit::list(
                ui,
                "ins_debt_list",
                items.len(),
                kit::ROW_H,
                |i| sel.as_deref() == Some(marker_key(items[i]).as_str()),
                |ui, rect, resp, i| {
                    let m = items[i];
                    let mut badges: Vec<(String, Tone)> =
                        vec![(m.kind.to_owned(), kind_tone(m.kind))];
                    if let Some(a) = &m.assignee {
                        badges.push((format!("@{a}"), Tone::Neutral));
                    }
                    if let Some(t) = &m.ticket {
                        badges.push((t.clone(), Tone::Neutral));
                    }
                    let meta = match m.time {
                        Some(t) => format::relative(t, cx.now),
                        None => String::new(),
                    };
                    let title = if m.text.is_empty() {
                        m.kind
                    } else {
                        m.text.as_str()
                    };
                    let sub = format!("{}:{}", m.path, m.line);
                    let dot = match m.time {
                        Some(t) => dot_color(&p, age_tone(days_between(t, cx.now))),
                        None => p.text_3,
                    };
                    kit::paint_row(
                        ui,
                        rect,
                        &Row::new(title)
                            .leading(Leading::Dot(dot))
                            .subtitle(&sub)
                            .meta(&meta)
                            .badges(&badges),
                        sel.as_deref() == Some(marker_key(m).as_str()),
                    );
                    if resp.clicked() {
                        actions.push(Action::SelectInsight(Some(marker_key(m))));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Copy path:line").clicked() {
                            actions.push(Action::Copy(marker_key(m)));
                            ui.close();
                        }
                        if ui.button("Open file").clicked() {
                            actions.push(Action::OpenPath(cx.workdir.join(&m.path)));
                            ui.close();
                        }
                    });
                },
            );
        },
        |ui, actions| match sel
            .as_deref()
            .and_then(|k| d.markers.iter().find(|m| marker_key(m) == k))
        {
            Some(m) => marker_detail(ui, cx, m, actions),
            None => overview(ui, cx, rep, dating_run, actions),
        },
    );
    *filter = f;
}

fn marker_detail(ui: &mut Ui, cx: &Cx, m: &Marker, actions: &mut Vec<Action>) {
    let mut badges: Vec<(String, Tone)> = vec![(m.kind.to_owned(), kind_tone(m.kind))];
    if let Some(a) = &m.assignee {
        badges.push((format!("@{a}"), Tone::Neutral));
    }
    if let Some(t) = &m.ticket {
        badges.push((t.clone(), Tone::Neutral));
    }
    let title = if m.text.is_empty() {
        m.kind
    } else {
        m.text.as_str()
    };
    detail_title(ui, title, &badges);
    ui.add_space(4.0);
    kit::kv(ui, 110.0, |kv| {
        kv.row("Location", |ui| {
            path_label(ui, cx, &marker_key(m), actions);
        });
        match (m.time, &m.author) {
            (Some(t), Some(a)) => {
                kv.text("Written", format!("{}  ·  by {a}", when(t, cx.now)));
                kv.text("Waiting", days_text(days_between(t, cx.now)));
                if let Some(c) = m.commit {
                    kv.row("Commit", |ui| commit_ref(ui, cx, c, "", actions));
                }
            }
            _ => kv.muted("Written", "not dated yet"),
        }
    });
    match m.time {
        Some(t) if days_between(t, cx.now) > 730 => why(
            ui,
            "This marker is more than two years old. Either the work is no longer wanted (delete the note) or it never will be done this way (open a ticket instead and remove the comment).",
        ),
        None => {
            kit::secondary(
                ui,
                "Date the markers on the overview (git blame on the files with markers) to see who wrote this one and when.",
            );
        }
        _ => {}
    }
    button_row(ui, |ui| {
        if IconButton::new(Icon::External, "Open file")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::OpenPath(cx.workdir.join(&m.path)));
        }
        if IconButton::new(Icon::Copy, "Copy path:line")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::Copy(marker_key(m)));
        }
        if IconButton::new(Icon::Copy, "Copy blame command")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::Copy(format!(
                "git blame -L {},{} -- \"{}\"",
                m.line, m.line, m.path
            )));
        }
    });
}

fn overview(
    ui: &mut Ui,
    cx: &Cx,
    rep: &InsightsReport,
    dating_run: &Option<ScanRun>,
    actions: &mut Vec<Action>,
) {
    let d = &rep.debt;
    let p = *pal(ui);
    let broken = d.count("FIXME") + d.count("BUG");
    let scanned_tip = format!(
        "Text files read ({}, {} lines)",
        format::bytes(d.bytes_scanned as usize),
        format::count(d.lines_scanned as usize)
    );
    super::stats(
        ui,
        &[
            Stat::new("Markers", format::count(d.markers.len()))
                .tip("TODO, FIXME, HACK, XXX, BUG, OPTIMIZE, DEPRECATED, NOCOMMIT, KLUDGE, TEMP"),
            Stat::new("Files", format::count(d.files_with_markers))
                .tip("Files containing at least one marker"),
            Stat::new("Files scanned", format::count(d.files_scanned)).tip(&scanned_tip),
            Stat::new("Per 1,000 lines", format!("{:.2}", d.density))
                .tip("Markers per 1,000 lines of text"),
            Stat::new("FIXME and BUG", format::count(broken as usize))
                .tone_if(broken > 0, Tone::Danger)
                .tip("Known-broken markers"),
            Stat::new("NOCOMMIT", format::count(d.count("NOCOMMIT") as usize))
                .tone_if(d.count("NOCOMMIT") > 0, Tone::Danger)
                .tip("Markers that were never meant to be committed"),
        ],
    );
    kit::secondary(
        ui,
        "Comments that say the code is not finished. Select one on the left; date them to see who wrote each and how long it has been waiting.",
    );
    if d.truncated {
        kit::callout(
            ui,
            Tone::Warning,
            "Capped",
            "The scan stopped at 20,000 markers; the counts are lower bounds.",
        );
    }

    kit::section(ui, "Age", None);
    match dating_run {
        Some(run) => {
            if kit::progress(ui, run, "Dating markers", true) {
                actions.push(Action::CancelDating);
            }
        }
        None if d.dated => {
            let dated = d.markers.iter().filter(|m| m.time.is_some()).count();
            kit::kv(ui, 110.0, |kv| {
                kv.text(
                    "Dated",
                    format!(
                        "{} of {} markers, {} blamed",
                        format::count(dated),
                        format::count(d.markers.len()),
                        super::count_of(d.dated_files, "file", "files")
                    ),
                );
                if let Some(o) = d.oldest() {
                    let t = o.time.unwrap_or(cx.now);
                    kv.text(
                        "Oldest",
                        format!(
                            "{} at {}:{} by {}, {}",
                            o.kind,
                            o.path,
                            o.line,
                            o.author.as_deref().unwrap_or("?"),
                            format::relative(t, cx.now)
                        ),
                    );
                }
            });
            let mut buckets = [
                ("Under 3 months", 0u32),
                ("3 to 12 months", 0),
                ("1 to 2 years", 0),
                ("Over 2 years", 0),
            ];
            for t in d.markers.iter().filter_map(|m| m.time) {
                let days = days_between(t, cx.now);
                let i = if days < 90 {
                    0
                } else if days < 365 {
                    1
                } else if days < 730 {
                    2
                } else {
                    3
                };
                buckets[i].1 += 1;
            }
            let items: Vec<(String, u32)> =
                buckets.iter().map(|(l, n)| (l.to_string(), *n)).collect();
            kit::count_bars(ui, &items, p.accent, 4);
            let mut authors: Vec<(String, u32)> = Vec::new();
            for m in &d.markers {
                if let Some(a) = &m.author {
                    match authors.iter_mut().find(|(n, _)| n == a) {
                        Some((_, n)) => *n += 1,
                        None => authors.push((a.clone(), 1)),
                    }
                }
            }
            authors.sort_by_key(|a| std::cmp::Reverse(a.1));
            if !authors.is_empty() {
                super::group(ui, "Who wrote them", Some(authors.len()));
                kit::count_bars(ui, &authors, p.accent, 12);
            }
            button_row(ui, |ui| {
                if IconButton::new(Icon::Rerun, "Date again")
                    .compact()
                    .show(ui)
                    .clicked()
                {
                    actions.push(Action::DateDebt);
                }
            });
        }
        None => {
            kit::secondary(
                ui,
                "Runs git blame on the files with markers (the 400 files with most markers first) to find who wrote each one and when.",
            );
            if !d.markers.is_empty() {
                button_row(ui, |ui| {
                    if IconButton::new(Icon::Clock, "Date markers")
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Action::DateDebt);
                    }
                });
            }
        }
    }
    if !d.by_kind.is_empty() {
        kit::section(ui, "By kind", None);
        let items: Vec<(String, u32)> = KINDS
            .iter()
            .filter_map(|k| {
                d.by_kind
                    .iter()
                    .find(|(kk, _)| kk == k)
                    .map(|(_, n)| (k.to_string(), *n))
            })
            .collect();
        kit::count_bars(ui, &items, p.accent, 12);
    }
    if !d.by_dir.is_empty() {
        kit::section(ui, "By directory", Some(d.by_dir.len()));
        kit::count_bars(ui, &d.by_dir, p.accent, 25);
    }
    if !d.by_assignee.is_empty() {
        kit::section(ui, "By assignee", Some(d.by_assignee.len()));
        kit::count_bars(ui, &d.by_assignee, p.accent, 15);
    }
    ui.add_space(12.0);
}
