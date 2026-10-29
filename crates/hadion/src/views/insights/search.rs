//! Search: pickaxe and friends.

use egui::{Frame, Margin, RichText, Sense, Ui, vec2};

use hadion_core::insights::search::{SearchHit, SearchMode, SearchResult};
use hadion_core::{DiffTarget, repo};

use super::{
    Cell, CommitLine, Cx, button_row, commit_table, count_of, detail_title, list_none, list_top,
    when,
};
use crate::format;
use crate::state::{Action, InsightsPage};
use crate::theme::pal;
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Row, Stat, Tone};

pub fn show(ui: &mut Ui, cx: &Cx, page: &mut InsightsPage, actions: &mut Vec<Action>) {
    query_bar(ui, page, actions);
    if let Some(run) = &page.search_run
        && kit::progress(ui, run, "Searching history", true)
    {
        actions.push(Action::CancelSearch);
    }
    let sel = page.selected.clone();
    match &page.search {
        None => {
            if page.search_run.is_none() {
                super::scroll_empty(ui, "ins_search_empty", |ui| empty(ui, actions));
            }
        }
        Some(Err(e)) => {
            Frame::NONE
                .inner_margin(Margin::symmetric(24, 16))
                .show(ui, |ui| {
                    kit::callout(ui, Tone::Danger, "The search failed", e)
                });
        }
        Some(Ok(res)) => {
            let selected_hit = sel
                .as_deref()
                .and_then(|k| res.hits.iter().find(|h| h.id.to_string() == k));
            kit::split_keyed(
                ui,
                "ins_search",
                sel.as_deref().unwrap_or(""),
                actions,
                |ui, actions| hit_list(ui, cx, res, &sel, actions),
                |ui, actions| match selected_hit {
                    Some(h) => hit_detail(ui, cx, res, h, actions),
                    None => overview(ui, cx, res, actions),
                },
            );
        }
    }
}

/// Before the first search: what each mode finds.
fn empty(ui: &mut Ui, actions: &mut Vec<Action>) {
    kit::empty_state(
        ui,
        Icon::Search,
        "Find when something entered or left the code base",
        "Pick a mode, type, press Enter. Results come from every branch unless you turn off All refs.",
        None,
    );
    ui.add_space(8.0);
    ui.vertical_centered(|ui| {
        ui.set_max_width(ui.available_width().min(480.0));
        // One mode per row, left-aligned in the centred column.
        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
            kit::kv(ui, 116.0, |kv| {
                kv.text(
                    "Text (-S)",
                    "every commit that changed how often the text occurs: the one that introduced a function, the one that removed a config key, the moves in between",
                );
                kv.text("Diff regex (-G)", "commits whose changed lines match");
                kv.text("Message", "commit messages");
                kv.text("Author", "author names and e-mails");
            });
            ui.add_space(8.0);
            kit::code_block(
                ui,
                "git log --all -S\"needle\" --name-only --format=…",
                actions,
            );
        });
    });
}

fn query_bar(ui: &mut Ui, page: &mut InsightsPage, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    Frame::NONE
        .inner_margin(Margin {
            left: 24,
            right: 16,
            top: 10,
            bottom: 10,
        })
        .show(ui, |ui| {
            // Everything on one row when it fits; otherwise the mode and the
            // switches on the first row, the inputs and Search on the second.
            let one_row = ui.available_width() >= 1040.0;
            let mut run = false;
            if one_row {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    mode_switch(ui, page);
                    let width = (ui.available_width() - 520.0).max(220.0);
                    run |= query_inputs(ui, page, width);
                    switches(ui, page);
                    run |= search_button(ui, page);
                });
            } else {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    mode_switch(ui, page);
                    switches(ui, page);
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let width = (ui.available_width() - 180.0 - 96.0 - 16.0).max(160.0);
                    run |= query_inputs(ui, page, width);
                    run |= search_button(ui, page);
                });
            }
            if run && page.search_run.is_none() {
                actions.push(Action::RunSearch);
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new(page.search_mode.hint())
                    .size(crate::theme::size::SMALL)
                    .color(p.text_2),
            );
        });
    kit::hairline(ui);
}

fn mode_switch(ui: &mut Ui, page: &mut InsightsPage) {
    let modes: Vec<(SearchMode, &str)> = SearchMode::ALL.iter().map(|m| (*m, m.label())).collect();
    if let Some(m) = kit::segmented(ui, &modes, page.search_mode) {
        page.search_mode = m;
    }
}

/// The text and path inputs; true when Enter was pressed in either.
fn query_inputs(ui: &mut Ui, page: &mut InsightsPage, width: f32) -> bool {
    let hint = match page.search_mode {
        SearchMode::Pickaxe => "Exact text, e.g. a function name or a config key",
        SearchMode::Regex => "Regular expression matched against changed lines",
        SearchMode::Message => "Regular expression matched against commit messages",
        SearchMode::Author => "Author name or e-mail (regular expression)",
    };
    let resp = ui
        .allocate_ui(vec2(width, 28.0), |ui| {
            kit::search_box(ui, "ins_search_text", &mut page.search_text, hint)
        })
        .inner;
    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    let path_enter = ui
        .allocate_ui(vec2(180.0, 28.0), |ui| {
            let r = kit::search_box(
                ui,
                "ins_search_path",
                &mut page.search_path,
                "Path filter (optional)",
            );
            r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
        })
        .inner;
    enter || path_enter
}

fn switches(ui: &mut Ui, page: &mut InsightsPage) {
    if kit::tip(
        kit::chip(ui, "All refs", page.search_all),
        "Search every branch and tag (--all) instead of HEAD only",
    )
    .clicked()
    {
        page.search_all = !page.search_all;
    }
    if kit::chip(ui, "Ignore case", page.search_ignore_case).clicked() {
        page.search_ignore_case = !page.search_ignore_case;
    }
}

/// The Search button (hidden while a search runs); true when clicked.
fn search_button(ui: &mut Ui, page: &InsightsPage) -> bool {
    page.search_run.is_none()
        && IconButton::new(Icon::Search, "Search")
            .compact()
            .primary(true)
            .show(ui)
            .clicked()
}

fn hit_files(h: &SearchHit) -> String {
    match h.paths.len() {
        0 => "no file list".to_owned(),
        1 => h.paths[0].clone(),
        n => format!("{n} files, {}", h.paths[0]),
    }
}

fn hit_list(
    ui: &mut Ui,
    cx: &Cx,
    res: &SearchResult,
    sel: &Option<String>,
    actions: &mut Vec<Action>,
) {
    list_top(ui, "Commits", res.hits.len(), sel.is_some(), None, actions);
    if res.truncated || res.cancelled {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            kit::muted(
                ui,
                if res.cancelled {
                    "Cancelled: the list is partial."
                } else {
                    "Capped: narrow the search for more."
                },
            );
        });
        ui.add_space(4.0);
    }
    if res.hits.is_empty() {
        list_none(ui, "No commit matches.");
        return;
    }
    kit::list(
        ui,
        "ins_search_list",
        res.hits.len(),
        kit::ROW_H,
        |i| sel.as_deref() == Some(res.hits[i].id.to_string().as_str()),
        |ui, rect, resp, i| {
            let h = &res.hits[i];
            let in_graph = cx.in_graph(&h.id);
            let mut badges: Vec<(String, Tone)> = Vec::new();
            if h.is_merge {
                badges.push(("Merge".into(), Tone::Neutral));
            }
            if !in_graph {
                badges.push(("Not in graph".into(), Tone::Neutral));
            }
            let sub = format!(
                "{}  ·  {}  ·  {}",
                repo::short_id(h.id),
                h.author,
                hit_files(h)
            );
            let meta = format::relative(h.time, cx.now);
            kit::paint_row(
                ui,
                rect,
                &Row::new(&h.summary)
                    .subtitle(&sub)
                    .meta(&meta)
                    .badges(&badges),
                sel.as_deref() == Some(h.id.to_string().as_str()),
            );
            if resp.clicked() {
                actions.push(Action::SelectInsight(Some(h.id.to_string())));
            }
            if resp.double_clicked() && in_graph {
                actions.push(Action::JumpTo(h.id));
            }
            resp.context_menu(|ui| {
                if ui.button("Copy commit id").clicked() {
                    actions.push(Action::Copy(h.id.to_string()));
                    ui.close();
                }
                if in_graph && ui.button("Show in the graph").clicked() {
                    actions.push(Action::JumpTo(h.id));
                    ui.close();
                }
            });
        },
    );
}

fn hit_detail(ui: &mut Ui, cx: &Cx, res: &SearchResult, h: &SearchHit, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let in_graph = cx.in_graph(&h.id);
    let mut badges = Vec::new();
    if h.is_merge {
        badges.push(("Merge".to_owned(), Tone::Neutral));
    }
    if !in_graph {
        badges.push(("Not in graph".to_owned(), Tone::Neutral));
    }
    detail_title(ui, &h.summary, &badges);
    ui.add_space(4.0);
    kit::kv(ui, 100.0, |kv| {
        kv.text("Commit", kit::mono(h.id.to_string()));
        kv.text("Author", h.author.as_str());
        kv.text("When", when(h.time, cx.now));
    });
    button_row(ui, |ui| {
        if in_graph
            && IconButton::new(Icon::Graph, "Show in graph")
                .compact()
                .show(ui)
                .clicked()
        {
            actions.push(Action::JumpTo(h.id));
        }
        if IconButton::new(Icon::Copy, "Copy id")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::Copy(h.id.to_string()));
        }
        if IconButton::new(Icon::Copy, "Copy git show command")
            .compact()
            .show(ui)
            .clicked()
        {
            let needle = &res.query.text;
            actions.push(Action::Copy(match res.query.mode {
                SearchMode::Pickaxe => format!("git show {} -S\"{}\"", h.id, needle),
                SearchMode::Regex => format!("git show {} -G\"{}\"", h.id, needle),
                _ => format!("git show {}", h.id),
            }));
        }
    });
    kit::section(ui, "Files", Some(h.paths.len()));
    if h.paths.is_empty() {
        kit::muted(ui, "No file list.");
        return;
    }
    kit::secondary(ui, "Click a file to open its diff in this commit.");
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        for path in &h.paths {
            let resp = ui
                .add(
                    egui::Label::new(kit::mono(path).color(p.accent))
                        .sense(Sense::click())
                        .wrap(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.clicked() {
                actions.push(Action::JumpTo(h.id));
                actions.push(Action::OpenFile(DiffTarget::Commit {
                    id: h.id,
                    parent: None,
                    path: path.clone(),
                    old_path: None,
                }));
            }
            resp.context_menu(|ui| {
                if ui.button("Copy path").clicked() {
                    actions.push(Action::Copy(path.clone()));
                    ui.close();
                }
            });
        }
    });
}

fn overview(ui: &mut Ui, cx: &Cx, res: &SearchResult, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    kit::title(ui, &format!("“{}”", res.query.text));
    kit::secondary(ui, res.query.mode.hint());
    ui.add_space(4.0);
    let first = res
        .span
        .map(|(lo, _)| format::absolute(lo))
        .unwrap_or_default();
    let last = res
        .span
        .map(|(_, hi)| format::absolute(hi))
        .unwrap_or_default();
    super::stats(
        ui,
        &[
            Stat::new("Commits", format::count(res.hits.len()))
                .tip("Matching commits, newest first"),
            Stat::new("Files", format::count(res.paths.len()))
                .tip("Distinct files across the hits"),
            Stat::new("Authors", format::count(res.authors.len())),
            Stat::new(
                "First seen",
                res.span
                    .map(|(lo, _)| format::relative(lo, cx.now))
                    .unwrap_or_else(|| "–".into()),
            )
            .tip(&first),
            Stat::new(
                "Last seen",
                res.span
                    .map(|(_, hi)| format::relative(hi, cx.now))
                    .unwrap_or_else(|| "–".into()),
            )
            .tip(&last),
            Stat::new("Took", format::millis(res.elapsed_ms)),
        ],
    );
    if res.truncated {
        kit::callout(
            ui,
            Tone::Warning,
            "Capped",
            &format!(
                "The search stopped at {}; narrow it (a path filter, HEAD only) for the rest.",
                count_of(res.hits.len(), "commit", "commits")
            ),
        );
    }
    if res.cancelled {
        kit::callout(ui, Tone::Warning, "Cancelled", "The results are partial.");
    }
    if let Some(oldest) = res.hits.last() {
        kit::section(ui, "Where it first appeared", None);
        commit_table(
            ui,
            cx,
            "ins_search_oldest",
            &[],
            0,
            vec![(
                CommitLine {
                    id: oldest.id,
                    time: oldest.time,
                    author: &oldest.author,
                    summary: &oldest.summary,
                },
                Vec::<Cell>::new(),
            )],
            actions,
        );
    }
    if !res.paths.is_empty() {
        kit::section(ui, "Files by hits", Some(res.paths.len()));
        kit::count_bars(ui, &res.paths, p.accent, 25);
    }
    if !res.authors.is_empty() {
        kit::section(ui, "Authors", Some(res.authors.len()));
        kit::count_bars(ui, &res.authors, p.accent, 15);
    }
    kit::section(ui, "Command", None);
    kit::code_block(ui, &res.command, actions);
}
