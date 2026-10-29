//! Bottom panel: the activity log (every operation, read and API call Hadion
//! made, with the exact commands and their full output) and the HEAD reflog
//! with undo.

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Label, Layout, Margin, Painter, Rect,
    RichText, Sense, Stroke, Ui, pos2, vec2,
};

use hadion_core::ops::{GitOp, ResetMode};

use crate::format;
use crate::state::{Action, ActivityEntry, ActivityKind, ActivityTab, Dialog, RepoState, Settings};
use crate::theme::{self, Palette, Tone, pal, size};
use crate::views::icons::{self, Icon, IconButton};
use crate::views::kit;
use crate::views::menus;
use crate::views::truncated;

/// Lines of output shown per entry when not verbose.
const COMPACT_LINES: usize = 200;
/// Hard cap even in verbose mode, to keep frames cheap.
const MAX_LINES: usize = 5_000;
/// Height of a log or reflog row.
const ROW_H: f32 = 28.0;
/// Where an entry's label starts; expanded details line up with it.
const LABEL_X: f32 = 144.0;

pub fn show(
    ui: &mut Ui,
    entries: &mut [ActivityEntry],
    repo: Option<&RepoState>,
    tab: &mut ActivityTab,
    settings: &Settings,
    actions: &mut Vec<Action>,
) {
    // A bottom panel shrinks to its content on the first frame and egui
    // remembers that height; claim the whole panel.
    ui.set_min_height(ui.available_height());
    // Filter state lives in egui memory: it is UI-only.
    let kind_id = ui.make_persistent_id("activity_kind_filter");
    let text_id = ui.make_persistent_id("activity_text_filter");
    let mut kind: Option<ActivityKind> = ui.data(|d| d.get_temp(kind_id)).unwrap_or(None);
    let mut text: String = ui.data(|d| d.get_temp(text_id)).unwrap_or_default();

    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if let Some(t) = kit::segmented(
            ui,
            &[(ActivityTab::Log, "Log"), (ActivityTab::Reflog, "Reflog")],
            *tab,
        ) {
            *tab = t;
        }
        if *tab == ActivityTab::Log {
            ui.add_space(6.0);
            for k in std::iter::once(None).chain(ActivityKind::ALL.into_iter().map(Some)) {
                let label = k.map(ActivityKind::label).unwrap_or("All");
                if kit::chip(ui, label, kind == k).clicked() {
                    kind = k;
                }
            }
            let failed = entries.iter().filter(|e| !e.ok).count();
            if failed > 0 {
                ui.add_space(4.0);
                kit::badge(ui, &format!("{} failed", format::count(failed)), Tone::Danger);
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if IconButton::icon(Icon::Close)
                .compact()
                .show(ui)
                .on_hover_text("Hide panel (Ctrl+L)")
                .clicked()
            {
                actions.push(Action::ToggleActivity);
            }
            match tab {
                ActivityTab::Log => {
                    if !entries.is_empty() && IconButton::text("Clear").compact().show(ui).clicked()
                    {
                        actions.push(Action::ClearActivity);
                    }
                    if kit::chip(ui, "Verbose", settings.verbose)
                        .on_hover_text(
                            "Also record background reads (loads, status, diffs) and GitHub API calls, and expand operations",
                        )
                        .clicked()
                    {
                        actions.push(Action::ToggleVerbose);
                    }
                    // What the chips on the left leave, up to 200 points:
                    // a narrow window shrinks the filter instead of
                    // drawing it over the chips.
                    let w = (ui.available_width() - 10.0).clamp(96.0, 200.0);
                    let resp = ui
                        .allocate_ui_with_layout(
                            vec2(w, 28.0),
                            Layout::left_to_right(Align::Center),
                            |ui| kit::search_box(ui, "activity_filter", &mut text, "Filter"),
                        )
                        .inner;
                    if resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        text.clear();
                        resp.surrender_focus();
                    }
                }
                ActivityTab::Reflog => {
                    if IconButton::icon(Icon::Fetch)
                        .compact()
                        .show(ui)
                        .on_hover_text("Reload")
                        .clicked()
                    {
                        actions.push(Action::Reload(true));
                    }
                    if let Some(undo) = repo.and_then(|r| r.undo.as_ref()) {
                        let btn = IconButton::new(Icon::Undo, &undo.label)
                            .compact()
                            .enabled(repo.is_some_and(|r| !r.is_busy()))
                            .show(ui);
                        if kit::tip(btn, &undo.detail).clicked() {
                            actions.push(Action::Undo);
                        }
                    }
                }
            }
        });
    });
    ui.data_mut(|d| {
        d.insert_temp(kind_id, kind);
        d.insert_temp(text_id, text.clone());
    });
    ui.add_space(4.0);
    match tab {
        ActivityTab::Log => log(ui, entries, kind, &text, settings, actions),
        ActivityTab::Reflog => reflog(ui, repo, actions),
    }
}

fn kind_icon(kind: ActivityKind) -> Icon {
    match kind {
        ActivityKind::Op => Icon::Branch,
        ActivityKind::Read => Icon::List,
        ActivityKind::Api => Icon::External,
        ActivityKind::Scan => Icon::Shield,
    }
}

/// A placeholder line under the header.
fn note(ui: &mut Ui, title: &str, body: &str) {
    Frame::NONE
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            let p = *pal(ui);
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.label(RichText::new(title).size(size::BODY).color(p.text));
            if !body.is_empty() {
                kit::secondary(ui, body);
            }
        });
}

fn log(
    ui: &mut Ui,
    entries: &mut [ActivityEntry],
    kind: Option<ActivityKind>,
    text: &str,
    settings: &Settings,
    actions: &mut Vec<Action>,
) {
    if entries.is_empty() {
        note(
            ui,
            "Nothing has run yet",
            "Operations show up here with their git commands and output; in verbose mode so do repository reads and GitHub API calls.",
        );
        return;
    }
    let needle = text.trim().to_lowercase();
    let matches = |e: &ActivityEntry| {
        kind.is_none_or(|k| e.kind == k)
            && (needle.is_empty()
                || e.label.to_lowercase().contains(&needle)
                || e.repo.to_lowercase().contains(&needle)
                || e.commands
                    .iter()
                    .any(|c| c.to_lowercase().contains(&needle))
                || e.output.to_lowercase().contains(&needle))
    };
    egui::ScrollArea::vertical()
        .id_salt("activity_log")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut shown = 0;
            for e in entries.iter_mut().rev() {
                if !matches(e) {
                    continue;
                }
                shown += 1;
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                if ui.is_rect_visible(rect) {
                    if e.expanded && !resp.hovered() {
                        // Open entries sit on a raised band so their
                        // details read as one group.
                        ui.painter().rect_filled(
                            rect.shrink2(vec2(4.0, 1.0)),
                            CornerRadius::same(theme::radius::M),
                            pal(ui).raised,
                        );
                    }
                    kit::row_background(ui, rect, resp.hovered(), false);
                    paint_entry(ui.painter(), pal(ui), ui.visuals().dark_mode, rect, e);
                }
                if resp.clicked() {
                    e.expanded = !e.expanded;
                }
                resp.context_menu(|ui| {
                    menus::begin(ui, 200.0);
                    if !e.commands.is_empty() {
                        menus::action(
                            ui,
                            "Copy commands",
                            actions,
                            Action::Copy(e.commands.join("\n")),
                        );
                    }
                    if !e.output.is_empty() {
                        menus::action(ui, "Copy output", actions, Action::Copy(e.output.clone()));
                    }
                    menus::action(ui, "Copy entry", actions, Action::Copy(entry_text(e)));
                });
                if e.expanded {
                    expanded(ui, e, settings.verbose, actions);
                }
            }
            if shown == 0 {
                note(ui, "No entries match the filter", "");
            }
            ui.add_space(6.0);
        });
}

/// One log row: chevron, time, kind icon, label, repository; output size,
/// duration and failure on the right.
fn paint_entry(painter: &Painter, p: &Palette, dark: bool, rect: Rect, e: &ActivityEntry) {
    let mid = rect.center().y;
    if e.expanded {
        icons::paint(
            painter,
            Icon::ChevronDown,
            pos2(rect.left() + 16.0, mid),
            10.0,
            p.text_2,
        );
    } else {
        // Pointing right (the icon set only has up and down).
        let c = pos2(rect.left() + 16.0, mid);
        painter.line(
            vec![
                pos2(c.x - 1.5, c.y - 3.0),
                pos2(c.x + 1.5, c.y),
                pos2(c.x - 1.5, c.y + 3.0),
            ],
            Stroke::new(1.5, p.text_3),
        );
    }
    painter.text(
        pos2(rect.left() + 28.0, mid),
        Align2::LEFT_CENTER,
        &e.at,
        FontId::monospace(size::MONO - 0.5),
        p.text_3,
    );
    let (icon, icon_c) = if e.ok {
        (kind_icon(e.kind), p.text_2)
    } else {
        (Icon::Error, p.danger)
    };
    icons::paint(
        painter,
        icon,
        pos2(rect.left() + LABEL_X - 16.0, mid),
        13.0,
        icon_c,
    );

    // Right side, right to left: duration, output size, failure.
    let mut right = rect.right() - 12.0;
    let meta_font = FontId::proportional(size::SMALL);
    let r = painter.text(
        pos2(right, mid),
        Align2::RIGHT_CENTER,
        format::millis(e.elapsed_ms),
        meta_font.clone(),
        p.text_3,
    );
    right = r.left() - 12.0;
    if let Some(b) = e.bytes.filter(|b| *b > 0) {
        let r = painter.text(
            pos2(right, mid),
            Align2::RIGHT_CENTER,
            format::bytes(b),
            meta_font.clone(),
            p.text_3,
        );
        right = r.left() - 12.0;
    }
    if !e.ok {
        right = kit::paint_badge(painter, p, dark, right, mid, "Failed", Tone::Danger) - 10.0;
    }

    // Label, then the repository in the secondary colour.
    let x = rect.left() + LABEL_X;
    let label_c = if e.ok { p.text } else { p.danger };
    let g = truncated(
        painter,
        &e.label,
        FontId::new(size::BODY, theme::medium()),
        label_c,
        (right - x).max(40.0),
    );
    let w = g.size().x;
    painter.galley(pos2(x, mid - g.size().y / 2.0), g, label_c);
    let rx = x + w + 10.0;
    if !e.repo.is_empty() && right - rx > 40.0 {
        let g = truncated(painter, &e.repo, meta_font, p.text_2, right - rx);
        painter.galley(pos2(rx, mid - g.size().y / 2.0), g, p.text_2);
    }
}

fn entry_text(e: &ActivityEntry) -> String {
    let mut s = format!(
        "{} {} [{}] {} ({}, {})\ncwd: {}\n",
        e.at,
        e.repo,
        e.kind.label(),
        e.label,
        if e.ok { "ok" } else { "failed" },
        format::millis(e.elapsed_ms),
        e.cwd
    );
    for c in &e.commands {
        s.push_str(&format!("$ {c}\n"));
    }
    s.push_str(&e.output);
    s
}

/// A monospace block in the kit's code-block style whose copy button
/// copies `copy` (the full text, when `text` is cut) and whose text can be
/// tinted (failed output).
fn block(ui: &mut Ui, text: &str, color: Color32, copy: &str, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let resp = Frame::NONE
        .fill(if ui.visuals().dark_mode {
            p.chrome
        } else {
            p.raised
        })
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(theme::radius::M))
        .inner_margin(Margin {
            left: 12,
            right: 40,
            top: 9,
            bottom: 9,
        })
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.add(Label::new(kit::mono(text).color(color)).wrap());
        })
        .response;
    if ui.rect_contains_pointer(resp.rect) {
        let r = Rect::from_min_size(
            pos2(resp.rect.right() - 32.0, resp.rect.top() + 5.0),
            vec2(26.0, 24.0),
        );
        let b = kit::overlay(ui, r, IconButton::icon(Icon::Copy).compact());
        if b.on_hover_text("Copy").clicked() {
            actions.push(Action::Copy(copy.to_owned()));
        }
    }
}

fn expanded(ui: &mut Ui, e: &ActivityEntry, verbose: bool, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    ui.add_space(6.0);
    // Lined up with the entry's label.
    ui.horizontal_top(|ui| {
        ui.add_space(LABEL_X);
        ui.vertical(|ui| {
            ui.set_width((ui.available_width() - 12.0).max(120.0));
            ui.spacing_mut().item_spacing.y = 8.0;
            kit::kv(ui, 84.0, |kv| {
                kv.text("Kind", e.kind.label());
                kv.row("Result", |ui| {
                    if e.ok {
                        ui.label(RichText::new("Succeeded").color(p.text));
                    } else {
                        ui.label(RichText::new("Failed").color(p.danger));
                    }
                });
                kv.text("Took", format::millis(e.elapsed_ms));
                if let Some(b) = e.bytes {
                    kv.text("Output", format::bytes(b));
                }
                if !e.cwd.is_empty() {
                    kv.text("Directory", kit::mono(&e.cwd));
                }
            });
            if !e.commands.is_empty() {
                let shown: Vec<String> = e.commands.iter().map(|c| format!("$ {c}")).collect();
                block(
                    ui,
                    &shown.join("\n"),
                    p.text,
                    &e.commands.join("\n"),
                    actions,
                );
            }
            if !e.output.trim().is_empty() {
                let total = e.output.lines().count();
                let limit = if verbose { MAX_LINES } else { COMPACT_LINES };
                let shown: String = e.output.lines().take(limit).collect::<Vec<_>>().join("\n");
                let color = if e.ok { p.text } else { p.danger };
                block(ui, &shown, color, &e.output, actions);
                if total > limit {
                    ui.label(
                        RichText::new(format!(
                            "{} more lines not shown; copy the output to see everything",
                            format::count(total - limit)
                        ))
                        .size(size::SMALL)
                        .color(p.text_3),
                    );
                }
            }
        });
    });
    ui.add_space(14.0);
}

fn reflog(ui: &mut Ui, repo: Option<&RepoState>, actions: &mut Vec<Action>) {
    let Some(repo) = repo else {
        note(
            ui,
            "No repository open",
            "Open a repository to see its reflog.",
        );
        return;
    };
    let Some(snap) = repo.snapshot.as_ref() else {
        return;
    };
    if repo.reflog.is_empty() {
        note(ui, "The reflog is empty", "");
        return;
    }
    let p = *pal(ui);
    let now = format::now_secs();
    // Rows edge to edge: `show_rows` adds the caller's spacing to the row
    // pitch, so it must be zero before the call.
    ui.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::vertical()
        .id_salt("reflog")
        .auto_shrink([false, false])
        .show_rows(ui, ROW_H, repo.reflog.len(), |ui, range| {
            for e in &repo.reflog[range] {
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let in_graph = snap.index_of.contains_key(&e.new);
                kit::row_background(ui, rect, resp.hovered(), false);
                let painter = ui.painter();
                let mid = rect.center().y;
                let mono = FontId::monospace(size::MONO - 0.5);
                painter.text(
                    pos2(rect.left() + 14.0, mid),
                    Align2::LEFT_CENTER,
                    format!("HEAD@{{{}}}", e.index),
                    mono.clone(),
                    p.text_3,
                );
                painter.text(
                    pos2(rect.left() + 104.0, mid),
                    Align2::LEFT_CENTER,
                    hadion_core::repo::short_id(e.new),
                    mono,
                    if in_graph { p.accent } else { p.text_3 },
                );
                let when = painter.text(
                    pos2(rect.right() - 12.0, mid),
                    Align2::RIGHT_CENTER,
                    format::relative(e.time, now),
                    FontId::proportional(size::SMALL),
                    p.text_3,
                );
                // Unreachable entries say so on the row (they used to say
                // it only in a tooltip on every row).
                let mut right = when.left() - 16.0;
                if !in_graph {
                    right = kit::paint_badge(
                        painter,
                        &p,
                        ui.visuals().dark_mode,
                        right,
                        mid,
                        "not in the graph",
                        Tone::Neutral,
                    ) - 12.0;
                }
                let x = rect.left() + 186.0;
                let g = truncated(
                    painter,
                    &e.message,
                    FontId::proportional(size::BODY),
                    p.text,
                    (right - x).max(40.0),
                );
                painter.galley(pos2(x, mid - g.size().y / 2.0), g, p.text);
                if resp.clicked() && in_graph {
                    actions.push(Action::JumpTo(e.new));
                }
                let sha = e.new.to_string();
                let short = hadion_core::repo::short_id(e.new);
                resp.context_menu(|ui| {
                    menus::begin(ui, 260.0);
                    // What the row does not show: both full ids and the
                    // exact time.
                    ui.add(
                        Label::new(
                            RichText::new(format!(
                                "{} \u{2192} {}\n{}",
                                hadion_core::repo::short_id(e.old),
                                short,
                                format::absolute_long(e.time)
                            ))
                            .size(size::SMALL)
                            .color(pal(ui).text_2),
                        )
                        .wrap(),
                    );
                    ui.add_space(4.0);
                    menus::separator(ui);
                    if in_graph {
                        menus::action(ui, "Show in graph", actions, Action::JumpTo(e.new));
                    }
                    menus::action(
                        ui,
                        "Create branch here…",
                        actions,
                        Action::Dialog(Dialog::CreateBranch {
                            name: String::new(),
                            start: sha.clone(),
                            start_label: short.clone(),
                            checkout: true,
                        }),
                    );
                    menus::danger_action(
                        ui,
                        &format!("Reset HEAD to {short} (hard)…"),
                        actions,
                        Action::Dialog(Dialog::Confirm {
                            title: format!("Reset to {short}?"),
                            body: format!(
                                "HEAD@{{{}}}: {}\n\nThe current branch moves to {short} and uncommitted changes are lost.",
                                e.index, e.message
                            ),
                            confirm: "Reset".into(),
                            danger: true,
                            ops: vec![GitOp::Reset {
                                target: sha.clone(),
                                mode: ResetMode::Hard,
                            }],
                        }),
                    );
                    menus::separator(ui);
                    menus::action(ui, "Copy SHA", actions, Action::Copy(sha.clone()));
                    menus::action(
                        ui,
                        "Copy previous SHA",
                        actions,
                        Action::Copy(e.old.to_string()),
                    );
                });
            }
        });
}
