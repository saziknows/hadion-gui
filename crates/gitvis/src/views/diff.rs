//! Diff viewer: unified or side-by-side, syntax coloured, with word-level
//! emphasis, line selection and per-hunk / per-line staging.

use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Galley, Id, Layout, Margin, Painter, Rect,
    Response, RichText, Sense, Ui, pos2, vec2,
};

use gitvis_core::diff::{DiffLine, FileDiff, Hunk, LineKind};
use gitvis_core::ops::GitOp;
use gitvis_core::{ChangeKind, DiffTarget, Snapshot};

use crate::format;
use crate::state::{Action, Dialog, DiffMode, DiffSelection, RepoState, Settings, Span};
use crate::theme::{self, Palette, Tone, pal, radius, semibold, size};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit;
use crate::views::menus;
use crate::views::truncated;

const ROW_H: f32 = 20.0;
const NUM_W: f32 = 44.0;
const MARK_W: f32 = 18.0;
/// Panel margin set by the app around this view; the code area paints
/// its background out to the panel edges.
const PANEL_MARGIN: f32 = 6.0;

/// One visual row of a hunk in the current mode.
#[derive(Clone, Copy)]
enum HunkRow {
    One(usize),
    /// Left (old) and right (new) line indices of a side-by-side row.
    Pair(Option<usize>, Option<usize>),
}

impl HunkRow {
    fn lines(self) -> impl Iterator<Item = usize> {
        let (a, b) = match self {
            HunkRow::One(i) => (Some(i), None),
            HunkRow::Pair(l, r) => (l, if l == r { None } else { r }),
        };
        a.into_iter().chain(b)
    }
}

enum Row {
    Header(usize),
    Line(usize, usize),
}

fn hunk_rows(hunk: &Hunk, mode: DiffMode) -> Vec<HunkRow> {
    match mode {
        DiffMode::Unified => (0..hunk.lines.len()).map(HunkRow::One).collect(),
        DiffMode::Split => {
            let n = hunk.lines.len();
            let mut rows = Vec::with_capacity(n);
            let mut i = 0;
            while i < n {
                match hunk.lines[i].kind {
                    LineKind::Context => {
                        rows.push(HunkRow::Pair(Some(i), Some(i)));
                        i += 1;
                    }
                    LineKind::NoNewline => i += 1,
                    LineKind::Added => {
                        rows.push(HunkRow::Pair(None, Some(i)));
                        i += 1;
                    }
                    LineKind::Removed => {
                        let mut removed = Vec::new();
                        let mut j = i;
                        while j < n
                            && matches!(hunk.lines[j].kind, LineKind::Removed | LineKind::NoNewline)
                        {
                            if hunk.lines[j].kind == LineKind::Removed {
                                removed.push(j);
                            }
                            j += 1;
                        }
                        let mut added = Vec::new();
                        while j < n
                            && matches!(hunk.lines[j].kind, LineKind::Added | LineKind::NoNewline)
                        {
                            if hunk.lines[j].kind == LineKind::Added {
                                added.push(j);
                            }
                            j += 1;
                        }
                        for k in 0..removed.len().max(added.len()) {
                            rows.push(HunkRow::Pair(
                                removed.get(k).copied(),
                                added.get(k).copied(),
                            ));
                        }
                        i = j;
                    }
                }
            }
            rows
        }
    }
}

fn non_empty(v: Vec<usize>) -> Option<Vec<usize>> {
    if v.is_empty() { None } else { Some(v) }
}

/// Added/removed line indices covered by a selection.
fn selected_lines(sel: &DiffSelection, rows: &[HunkRow], hunk: &Hunk) -> Vec<usize> {
    let range = sel.range();
    let end = (*range.end()).min(rows.len().saturating_sub(1));
    if rows.is_empty() || *range.start() > end {
        return Vec::new();
    }
    rows[*range.start()..=end]
        .iter()
        .flat_map(|r| r.lines())
        .filter(|&i| matches!(hunk.lines[i].kind, LineKind::Added | LineKind::Removed))
        .collect()
}

/// The change kind of the open file, from the commit's file list or the
/// working-tree status.
fn change_kind(
    target: &DiffTarget,
    snap: &Snapshot,
    details: &std::collections::HashMap<gitvis_core::Oid, gitvis_core::CommitDetails>,
) -> Option<ChangeKind> {
    let find = |list: &[gitvis_core::FileEntry], path: &str| {
        list.iter().find(|e| e.path == path).map(|e| e.kind)
    };
    match target {
        DiffTarget::Commit { id, path, .. } => details
            .get(id)
            .and_then(|d| d.files.iter().find(|f| f.path == *path))
            .map(|f| f.kind),
        DiffTarget::Unstaged { path } => find(&snap.status.unstaged, path),
        DiffTarget::Staged { path, .. } => find(&snap.status.staged, path),
        DiffTarget::Untracked { .. } => Some(ChangeKind::Untracked),
        DiffTarget::Conflicted { .. } => Some(ChangeKind::Conflicted),
    }
}

fn kind_badge(kind: ChangeKind) -> (&'static str, Tone) {
    match kind {
        ChangeKind::Added => ("Added", Tone::Success),
        ChangeKind::Untracked => ("Untracked", Tone::Success),
        ChangeKind::Modified => ("Modified", Tone::Warning),
        ChangeKind::TypeChange => ("Type changed", Tone::Warning),
        ChangeKind::Deleted => ("Deleted", Tone::Danger),
        ChangeKind::Conflicted => ("Conflicted", Tone::Danger),
        ChangeKind::Renamed => ("Renamed", Tone::Purple),
        ChangeKind::Copied => ("Copied", Tone::Purple),
    }
}

/// Where the change sits, for working-tree diffs.
fn place_badge(target: &DiffTarget) -> Option<&'static str> {
    match target {
        DiffTarget::Unstaged { .. } => Some("Unstaged"),
        DiffTarget::Staged { .. } => Some("Staged"),
        _ => None,
    }
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, settings: &Settings, actions: &mut Vec<Action>) {
    let RepoState {
        file,
        snapshot,
        diff,
        diff_selection,
        busy,
        workdir,
        details,
        ..
    } = repo;
    let Some(target) = file.as_ref() else { return };
    let Some(snap) = snapshot.as_ref() else {
        return;
    };
    let p = *pal(ui);
    let busy = busy.is_some();
    let mode = settings.diff_mode;
    if diff_selection.is_some_and(|s| s.mode != mode) {
        *diff_selection = None;
    }
    let doc = match diff {
        Some(Ok(d)) => Some(d),
        _ => None,
    };

    // Header: path and file actions, then what changed and the view options.
    Frame::NONE
        .inner_margin(Margin {
            left: 8,
            right: 2,
            top: 6,
            bottom: 8,
        })
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let narrow = ui.available_width() < 620.0;
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if IconButton::icon(Icon::Close)
                        .compact()
                        .show(ui)
                        .on_hover_text("Close (Esc)")
                        .clicked()
                    {
                        actions.push(Action::CloseFile);
                    }
                    file_actions(ui, snap, target, busy, actions);
                    ui.add_space(6.0);
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        let (rect, _) = ui
                            .allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
                        paint_path(ui.painter(), &p, rect, target.path());
                    });
                });
            });
            if let Some(old) = target.old_path() {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("Renamed from {old}"))
                            .size(size::SMALL)
                            .color(p.text_2),
                    )
                    .truncate(),
                );
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if let Some(kind) = change_kind(target, snap, details) {
                    let (label, tone) = kind_badge(kind);
                    kit::badge(ui, label, tone);
                }
                if let Some(place) = place_badge(target) {
                    kit::badge(ui, place, Tone::Neutral);
                }
                if let Some(doc) = doc {
                    let fd = &doc.diff;
                    if fd.combined {
                        kit::badge(ui, "Combined diff", Tone::Neutral);
                    }
                    let (a, d) = fd.stats();
                    if a > 0 || d > 0 {
                        ui.add_space(2.0);
                    }
                    if a > 0 {
                        ui.label(
                            RichText::new(format!("+{}", format::count(a)))
                                .size(size::SMALL)
                                .family(theme::medium())
                                .color(p.success),
                        );
                    }
                    if d > 0 {
                        ui.label(
                            RichText::new(format!("\u{2212}{}", format::count(d)))
                                .size(size::SMALL)
                                .family(theme::medium())
                                .color(p.danger),
                        );
                    }
                    if let Some(lang) = &doc.language {
                        ui.add_space(2.0);
                        ui.label(RichText::new(lang).size(size::SMALL).color(p.text_3));
                    }
                }
                if !narrow {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        view_options(ui, settings, mode, actions);
                    });
                }
            });
            if narrow {
                // A narrow pane gets the view options on their own row.
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        view_options(ui, settings, mode, actions);
                    });
                });
            }
        });
    kit::hairline(ui);

    let doc = match diff {
        None => {
            code_background(ui, &p);
            Frame::NONE
                .inner_margin(Margin::symmetric(12, 14))
                .show(ui, |ui| kit::loading(ui, "Loading diff…"));
            return;
        }
        Some(Err(e)) => {
            code_background(ui, &p);
            ui.add_space(12.0);
            kit::callout(ui, Tone::Danger, "Could not load the diff", e.as_str());
            return;
        }
        Some(Ok(d)) => d,
    };
    let fd = &doc.diff;

    if fd.binary {
        code_background(ui, &p);
        kit::empty_state(
            ui,
            Icon::Info,
            "Binary file",
            "There is no text diff to show.",
            None,
        );
        return;
    }
    if fd.too_large {
        code_background(ui, &p);
        kit::empty_state(
            ui,
            Icon::Warning,
            "Diff too large",
            "This diff is too large to display.",
            None,
        );
        return;
    }
    if fd.hunks.is_empty() {
        code_background(ui, &p);
        let (title, body) = if matches!(target, DiffTarget::Untracked { .. }) {
            ("Empty file", "The new file has no content yet.")
        } else if settings.diff_ignore_ws {
            (
                "Only whitespace changed",
                "Turn off \u{201c}Ignore whitespace\u{201d} to see the changes.",
            )
        } else {
            ("No changes", "")
        };
        kit::empty_state(ui, Icon::Check, title, body, None);
        return;
    }

    let per_hunk: Vec<Vec<HunkRow>> = fd.hunks.iter().map(|h| hunk_rows(h, mode)).collect();
    let line_ops = line_ops_for(target, fd);

    // Selection bar.
    if let Some(sel) = *diff_selection
        && let Some(hunk) = fd.hunks.get(sel.hunk)
        && let Some(lines) = non_empty(selected_lines(&sel, &per_hunk[sel.hunk], hunk))
    {
        Frame::NONE
            .fill(p.selection)
            .inner_margin(Margin {
                left: 12,
                right: 6,
                top: 3,
                bottom: 3,
            })
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    // Buttons first (right to left), the count in what is left.
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        if !busy {
                            selection_buttons(ui, &line_ops, fd, sel.hunk, &lines, actions);
                        }
                        if IconButton::text("Clear")
                            .compact()
                            .show(ui)
                            .on_hover_text("Esc")
                            .clicked()
                        {
                            actions.push(Action::DiffEscape);
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "{} selected",
                                        format::plural(lines.len(), "line", "lines")
                                    ))
                                    .size(size::SMALL)
                                    .family(theme::medium())
                                    .color(p.text_strong),
                                )
                                .truncate(),
                            );
                        });
                    });
                });
            });
    }
    code_background(ui, &p);

    // Rows and geometry.
    let mut rows: Vec<Row> = Vec::new();
    let mut max_chars = 40usize;
    for (h, hunk) in fd.hunks.iter().enumerate() {
        rows.push(Row::Header(h));
        max_chars = max_chars.max(hunk.header.chars().count());
        for (r, _) in per_hunk[h].iter().enumerate() {
            rows.push(Row::Line(h, r));
        }
        for l in &hunk.lines {
            max_chars = max_chars.max(l.text.chars().count());
        }
    }
    let font = FontId::monospace(size::MONO);
    let char_w = ui
        .painter()
        .layout_no_wrap("0".repeat(20), font.clone(), Color32::WHITE)
        .size()
        .x
        / 20.0;
    let avail = ui.available_width();
    let geo = match mode {
        DiffMode::Unified => Geo {
            char_w,
            font: font.clone(),
            pane_w: 0.0,
            content_w: (NUM_W * 2.0 + MARK_W + 8.0 + max_chars as f32 * char_w + 24.0).max(avail),
        },
        DiffMode::Split => {
            let pane = ((avail - 1.0) / 2.0).max(120.0);
            Geo {
                char_w,
                font: font.clone(),
                pane_w: pane,
                // Panes clip long lines; scrolling both sides at once helps nobody.
                content_w: avail,
            }
        }
    };
    let dark = ui.visuals().dark_mode;
    let can_select_ops = line_ops.is_some() && !busy;
    let has_hunk_ops = matches!(
        target,
        DiffTarget::Unstaged { .. } | DiffTarget::Staged { .. }
    ) && fd.can_apply_hunks()
        && !busy;

    ui.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::both()
        .id_salt(("diff", target.path(), mode == DiffMode::Split))
        .auto_shrink([false, false])
        .show_rows(ui, ROW_H, rows.len(), |ui, range| {
            let clip_right = ui.clip_rect().right();
            let shift = ui.input(|i| i.modifiers.shift);
            for i in range {
                let (rect, resp) = ui.allocate_exact_size(
                    vec2(geo.content_w, ROW_H),
                    match rows[i] {
                        Row::Header(_) => Sense::hover(),
                        Row::Line(..) => Sense::click_and_drag(),
                    },
                );
                let painter = ui.painter().clone();
                match rows[i] {
                    Row::Header(h) => {
                        let hunk = &fd.hunks[h];
                        hunk_header(&painter, &p, rect, hunk, clip_right, has_hunk_ops);
                        if has_hunk_ops {
                            hunk_buttons(ui, &p, rect, clip_right, target, fd, h, actions);
                        }
                        let hunk_text = hunk_text(hunk);
                        resp.context_menu(|ui| {
                            menus::begin(ui, 180.0);
                            menus::action(
                                ui,
                                "Copy hunk",
                                actions,
                                Action::Copy(hunk_text.clone()),
                            );
                        });
                    }
                    Row::Line(h, r) => {
                        let hunk = &fd.hunks[h];
                        let row = per_hunk[h][r];
                        let selected =
                            diff_selection.is_some_and(|s| s.hunk == h && s.range().contains(&r));
                        match row {
                            HunkRow::One(l) => draw_unified(
                                &painter,
                                &p,
                                rect,
                                &hunk.lines[l],
                                &doc.spans[h][l],
                                &geo,
                                dark,
                            ),
                            HunkRow::Pair(l, rr) => draw_split(
                                &painter,
                                &p,
                                rect,
                                l.map(|i| (&hunk.lines[i], &doc.spans[h][i][..])),
                                rr.map(|i| (&hunk.lines[i], &doc.spans[h][i][..])),
                                &geo,
                                dark,
                            ),
                        }
                        if selected {
                            painter.rect_filled(rect, 0, p.accent.gamma_multiply(0.16));
                            painter.rect_filled(
                                Rect::from_min_max(
                                    rect.min,
                                    pos2(rect.left() + 3.0, rect.bottom()),
                                ),
                                0,
                                p.accent,
                            );
                        }

                        // Selection by click, shift-click and drag.
                        if resp.drag_started() || resp.clicked() {
                            match diff_selection.as_mut() {
                                Some(s) if shift && s.hunk == h => s.end = r,
                                _ => {
                                    *diff_selection = Some(DiffSelection {
                                        hunk: h,
                                        anchor: r,
                                        end: r,
                                        mode,
                                        dragging: resp.drag_started(),
                                    })
                                }
                            }
                        }
                        if let Some(s) = diff_selection.as_mut()
                            && s.dragging
                            && s.hunk == h
                            && resp.contains_pointer()
                        {
                            s.end = r;
                        }

                        let line_texts: Vec<String> =
                            row.lines().map(|i| hunk.lines[i].text.clone()).collect();
                        let sel_now = *diff_selection;
                        resp.context_menu(|ui| {
                            menus::begin(ui, 220.0);
                            menus::action(
                                ui,
                                "Copy line",
                                actions,
                                Action::Copy(line_texts.join("\n")),
                            );
                            if let Some(s) = sel_now
                                && s.hunk == h
                            {
                                let range = s.range();
                                let end = (*range.end()).min(per_hunk[h].len() - 1);
                                let text: Vec<String> = per_hunk[h][*range.start()..=end]
                                    .iter()
                                    .flat_map(|r| r.lines())
                                    .map(|i| hunk.lines[i].text.clone())
                                    .collect();
                                menus::action(
                                    ui,
                                    "Copy selected lines",
                                    actions,
                                    Action::Copy(text.join("\n")),
                                );
                                if can_select_ops {
                                    let lines = selected_lines(&s, &per_hunk[h], hunk);
                                    if !lines.is_empty() {
                                        menus::separator(ui);
                                        selection_menu_items(ui, &line_ops, fd, h, &lines, actions);
                                    }
                                }
                            }
                            menus::separator(ui);
                            file_menu_items(ui, workdir, target, actions);
                        });
                    }
                }
            }
        });
    if let Some(s) = diff_selection.as_mut()
        && s.dragging
        && ui.input(|i| i.pointer.any_released())
    {
        s.dragging = false;
    }
}

/// Fill the rest of the pane, out to the panel edges, with the code
/// surface.
fn code_background(ui: &Ui, p: &Palette) {
    let r = ui.available_rect_before_wrap();
    let bg = Rect::from_min_max(
        pos2(r.left() - PANEL_MARGIN, r.top()),
        pos2(r.right() + PANEL_MARGIN, r.bottom() + PANEL_MARGIN),
    );
    ui.painter().rect_filled(bg, 0, p.canvas);
}

/// Directory in the tertiary colour (cut from the front when long), file
/// name in semibold.
fn paint_path(painter: &Painter, p: &Palette, rect: Rect, path: &str) {
    let (dir, name) = format::split_path(path);
    let name_g = truncated(
        painter,
        name,
        FontId::new(size::TITLE, semibold()),
        p.text_strong,
        rect.width(),
    );
    let room = rect.width() - name_g.size().x;
    let y = rect.center().y;
    let mut x = rect.left();
    if !dir.is_empty() && room > 40.0 {
        let g = elide_front(
            painter,
            dir,
            FontId::proportional(size::TITLE),
            p.text_3,
            room,
        );
        painter.galley(pos2(x, y - g.size().y / 2.0), g.clone(), p.text_3);
        x += g.size().x;
    }
    painter.galley(pos2(x, y - name_g.size().y / 2.0), name_g, p.text_strong);
}

/// `text` on one line, its start replaced by `…` when wider than `max_w`.
fn elide_front(
    painter: &Painter,
    text: &str,
    font: FontId,
    color: Color32,
    max_w: f32,
) -> Arc<Galley> {
    let full = painter.layout_no_wrap(text.to_owned(), font.clone(), color);
    if full.size().x <= max_w {
        return full;
    }
    let starts: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    // Smallest cut whose remainder fits: binary search over char starts.
    let (mut lo, mut hi) = (1usize, starts.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        let w = painter
            .layout_no_wrap(
                format!("\u{2026}{}", &text[starts[mid]..]),
                font.clone(),
                color,
            )
            .size()
            .x;
        if w <= max_w { hi = mid } else { lo = mid + 1 }
    }
    let rest = starts.get(lo).map(|&i| &text[i..]).unwrap_or("");
    painter.layout_no_wrap(format!("\u{2026}{rest}"), font, color)
}

/// Unified / Split, whitespace and context size, right to left.
fn view_options(ui: &mut Ui, settings: &Settings, mode: DiffMode, actions: &mut Vec<Action>) {
    let ctx_btn = dropdown_button(ui, &format!("\u{b1}{} lines", settings.diff_context))
        .on_hover_text("Context lines around changes");
    egui::Popup::menu(&ctx_btn)
        .id(Id::unique("diff_context_menu"))
        .show(|ui| {
            menus::begin(ui, 140.0);
            for n in [1u32, 3, 6, 10, 25] {
                let on = settings.diff_context == n;
                if menus::entry_with_hint(
                    ui,
                    &format!("{n} lines"),
                    if on { "\u{2713}" } else { "" },
                    None,
                    false,
                )
                .clicked()
                {
                    actions.push(Action::SetDiffContext(n));
                    ui.close();
                }
            }
        });
    let ws = kit::chip(ui, "Ignore whitespace", settings.diff_ignore_ws);
    let ws = if settings.diff_ignore_ws {
        ws.on_hover_text("Whitespace-only changes are hidden and hunks are read-only")
    } else {
        ws
    };
    if ws.clicked() {
        actions.push(Action::ToggleDiffWhitespace);
    }
    if let Some(m) = kit::segmented(
        ui,
        &[(DiffMode::Unified, "Unified"), (DiffMode::Split, "Split")],
        mode,
    ) {
        actions.push(Action::SetDiffMode(m));
    }
}

/// A compact ghost button with a trailing chevron that opens a menu.
fn dropdown_button(ui: &mut Ui, label: &str) -> Response {
    let p = *pal(ui);
    let g = ui.painter().layout_no_wrap(
        label.to_owned(),
        FontId::new(size::SMALL, theme::medium()),
        Color32::PLACEHOLDER,
    );
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 36.0, 26.0), Sense::click());
    let hovered = resp.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(radius::M), p.hover);
    }
    let fg = if hovered { p.text_strong } else { p.text };
    let y = rect.center().y;
    ui.painter()
        .galley(pos2(rect.left() + 9.0, y - g.size().y / 2.0), g, fg);
    crate::views::icons::paint(
        ui.painter(),
        Icon::ChevronDown,
        pos2(rect.right() - 13.0, y),
        10.0,
        p.text_2,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

struct Geo {
    char_w: f32,
    font: FontId,
    pane_w: f32,
    content_w: f32,
}

/// Line and gutter backgrounds: soft tints of the success / danger colours.
fn kind_fill(kind: LineKind, dark: bool, p: &Palette) -> (Color32, Color32) {
    let (line, gutter) = if dark { (0.12, 0.20) } else { (0.10, 0.18) };
    match kind {
        LineKind::Added => (
            p.success.gamma_multiply(line),
            p.success.gamma_multiply(gutter),
        ),
        LineKind::Removed => (
            p.danger.gamma_multiply(line),
            p.danger.gamma_multiply(gutter),
        ),
        _ => (Color32::TRANSPARENT, p.panel),
    }
}

fn marker_for(kind: LineKind, p: &Palette) -> (&'static str, Color32) {
    match kind {
        LineKind::Added => ("+", p.success),
        LineKind::Removed => ("\u{2212}", p.danger),
        _ => ("", Color32::TRANSPARENT),
    }
}

/// Lay out one line with its syntax spans.
fn line_galley(
    p: &Painter,
    line: &DiffLine,
    spans: &[Span],
    font: &FontId,
    default: Color32,
) -> Arc<Galley> {
    if spans.is_empty() {
        return p.layout_no_wrap(line.text.clone(), font.clone(), default);
    }
    let text = &line.text;
    let byte_at: Vec<usize> = text
        .char_indices()
        .map(|(b, _)| b)
        .chain(std::iter::once(text.len()))
        .collect();
    let mut job = LayoutJob::default();
    let mut pos = 0u32;
    let fmt = |color: Color32| TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    let nchars = byte_at.len() as u32 - 1;
    for s in spans {
        let (start, end) = (s.start.min(nchars), s.end.min(nchars));
        if start > pos {
            job.append(
                &text[byte_at[pos as usize]..byte_at[start as usize]],
                0.0,
                fmt(default),
            );
        }
        if end > start {
            job.append(
                &text[byte_at[start as usize]..byte_at[end as usize]],
                0.0,
                fmt(s.color),
            );
        }
        pos = pos.max(end);
    }
    if pos < nchars {
        job.append(&text[byte_at[pos as usize]..], 0.0, fmt(default));
    }
    p.layout_job(job)
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    painter: &Painter,
    p: &Palette,
    x: f32,
    rect: Rect,
    line: &DiffLine,
    spans: &[Span],
    geo: &Geo,
    dark: bool,
) {
    let mid = rect.center().y;
    // Word-level emphasis behind the changed tokens.
    if !line.emph.is_empty() {
        let color = match line.kind {
            LineKind::Added => p.success,
            _ => p.danger,
        }
        .gamma_multiply(if dark { 0.34 } else { 0.30 });
        for &(s, e) in &line.emph {
            let r = Rect::from_min_max(
                pos2(x + s as f32 * geo.char_w, rect.top() + 1.0),
                pos2(x + e as f32 * geo.char_w, rect.bottom() - 1.0),
            );
            painter.rect_filled(r, CornerRadius::same(2), color);
        }
    }
    let text_color = if line.kind == LineKind::NoNewline {
        p.text_3
    } else {
        p.text
    };
    let galley = line_galley(painter, line, spans, &geo.font, text_color);
    painter.galley(pos2(x, mid - galley.size().y / 2.0), galley, text_color);
}

fn draw_number(painter: &Painter, p: &Palette, x_right: f32, mid: f32, n: Option<u32>) {
    if let Some(n) = n {
        painter.text(
            pos2(x_right, mid),
            Align2::RIGHT_CENTER,
            n.to_string(),
            FontId::monospace(size::CAPTION + 0.5),
            p.text_3,
        );
    }
}

fn draw_unified(
    painter: &Painter,
    p: &Palette,
    rect: Rect,
    line: &DiffLine,
    spans: &[Span],
    geo: &Geo,
    dark: bool,
) {
    let (bg, gutter_bg) = kind_fill(line.kind, dark, p);
    if bg != Color32::TRANSPARENT {
        painter.rect_filled(rect, 0, bg);
    }
    let gutter_w = NUM_W * 2.0 + MARK_W;
    painter.rect_filled(
        Rect::from_min_max(rect.min, pos2(rect.left() + gutter_w, rect.bottom())),
        0,
        gutter_bg,
    );
    let mid = rect.center().y;
    draw_number(painter, p, rect.left() + NUM_W - 6.0, mid, line.old);
    draw_number(painter, p, rect.left() + NUM_W * 2.0 - 6.0, mid, line.new);
    let (marker, color) = marker_for(line.kind, p);
    if !marker.is_empty() {
        painter.text(
            pos2(rect.left() + NUM_W * 2.0 + 5.0, mid),
            Align2::LEFT_CENTER,
            marker,
            geo.font.clone(),
            color,
        );
    }
    draw_text(
        painter,
        p,
        rect.left() + gutter_w + 8.0,
        rect,
        line,
        spans,
        geo,
        dark,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_split(
    painter: &Painter,
    p: &Palette,
    rect: Rect,
    left: Option<(&DiffLine, &[Span])>,
    right: Option<(&DiffLine, &[Span])>,
    geo: &Geo,
    dark: bool,
) {
    let mid = rect.center().y;
    let panes = [
        (
            Rect::from_min_max(rect.min, pos2(rect.left() + geo.pane_w, rect.bottom())),
            left,
            true,
        ),
        (
            Rect::from_min_max(pos2(rect.left() + geo.pane_w + 1.0, rect.top()), rect.max),
            right,
            false,
        ),
    ];
    for (pane, side, is_old) in panes {
        let gutter_w = NUM_W + MARK_W;
        match side {
            Some((line, spans)) => {
                let (bg, gutter_bg) = kind_fill(line.kind, dark, p);
                if bg != Color32::TRANSPARENT {
                    painter.rect_filled(pane, 0, bg);
                }
                painter.rect_filled(
                    Rect::from_min_max(pane.min, pos2(pane.left() + gutter_w, pane.bottom())),
                    0,
                    gutter_bg,
                );
                draw_number(
                    painter,
                    p,
                    pane.left() + NUM_W - 6.0,
                    mid,
                    if is_old { line.old } else { line.new },
                );
                let (marker, color) = marker_for(line.kind, p);
                if !marker.is_empty() {
                    painter.text(
                        pos2(pane.left() + NUM_W + 5.0, mid),
                        Align2::LEFT_CENTER,
                        marker,
                        geo.font.clone(),
                        color,
                    );
                }
                let clip = painter.with_clip_rect(pane.intersect(painter.clip_rect()));
                draw_text(
                    &clip,
                    p,
                    pane.left() + gutter_w + 6.0,
                    pane,
                    line,
                    spans,
                    geo,
                    dark,
                );
            }
            None => {
                painter.rect_filled(pane, 0, p.panel);
            }
        }
    }
    painter.rect_filled(
        Rect::from_min_max(
            pos2(rect.left() + geo.pane_w, rect.top()),
            pos2(rect.left() + geo.pane_w + 1.0, rect.bottom()),
        ),
        0,
        p.border,
    );
}

/// Width the hunk buttons take at the right of a header row.
const HUNK_BUTTONS_W: f32 = 210.0;

/// A raised band with the `@@` range and the enclosing function.
fn hunk_header(
    painter: &Painter,
    p: &Palette,
    rect: Rect,
    hunk: &Hunk,
    clip_right: f32,
    has_buttons: bool,
) {
    painter.rect_filled(rect, 0, p.raised);
    let y = rect.center().y;
    let r = painter.text(
        pos2(rect.left() + 12.0, y),
        Align2::LEFT_CENTER,
        hunk.range_text(),
        FontId::monospace(size::MONO - 1.0),
        p.text_3,
    );
    let ctx = hunk.context();
    if !ctx.is_empty() {
        let reserve = if has_buttons { HUNK_BUTTONS_W } else { 12.0 };
        let g = truncated(
            painter,
            ctx,
            FontId::monospace(size::MONO - 0.5),
            p.text_2,
            (clip_right - reserve - r.right() - 12.0).max(40.0),
        );
        painter.galley(pos2(r.right() + 12.0, y - g.size().y / 2.0), g, p.text_2);
    }
}

/// A text button that fits a diff row (the kit's buttons are taller than
/// a code line); painted on top of the row, outside the layout.
fn row_button(
    ui: &Ui,
    p: &Palette,
    right: f32,
    y: f32,
    id: Id,
    label: &str,
    danger: bool,
) -> (Response, f32) {
    let font = FontId::new(size::SMALL, theme::medium());
    let g = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, Color32::PLACEHOLDER);
    let rect = Rect::from_min_max(
        pos2(right - g.size().x - 16.0, y - 9.0),
        pos2(right, y + 9.0),
    );
    let resp = ui.interact(rect, id, Sense::click());
    let hovered = resp.hovered();
    if hovered {
        let fill = if danger {
            p.danger.gamma_multiply(0.16)
        } else {
            p.hover
        };
        ui.painter()
            .rect_filled(rect, CornerRadius::same(radius::S), fill);
    }
    let color = if danger {
        p.danger
    } else if hovered {
        p.text_strong
    } else {
        p.text
    };
    ui.painter()
        .galley(pos2(rect.left() + 8.0, y - g.size().y / 2.0), g, color);
    (
        resp.on_hover_cursor(egui::CursorIcon::PointingHand),
        rect.left(),
    )
}

fn hunk_text(hunk: &Hunk) -> String {
    let mut out = hunk.header.clone();
    out.push('\n');
    for l in &hunk.lines {
        let m = match l.kind {
            LineKind::Added => "+",
            LineKind::Removed => "-",
            LineKind::Context => " ",
            LineKind::NoNewline => "",
        };
        out.push_str(m);
        out.push_str(&l.text);
        out.push('\n');
    }
    out
}

/// Which line-level operations apply to this target.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LineOps {
    /// Stage / discard (unstaged changes).
    Worktree,
    /// Unstage (index changes).
    Index,
}

fn line_ops_for(target: &DiffTarget, fd: &FileDiff) -> Option<LineOps> {
    if !fd.can_apply_hunks() {
        return None;
    }
    match target {
        DiffTarget::Unstaged { .. } => Some(LineOps::Worktree),
        DiffTarget::Staged { .. } => Some(LineOps::Index),
        _ => None,
    }
}

#[allow(clippy::collapsible_match)]
fn selection_buttons(
    ui: &mut Ui,
    ops: &Option<LineOps>,
    fd: &FileDiff,
    hunk: usize,
    lines: &[usize],
    actions: &mut Vec<Action>,
) {
    match ops {
        Some(LineOps::Worktree) => {
            // Right to left: the main action ends up rightmost.
            if let Some(patch) = fd.selected_lines_patch(hunk, lines, false)
                && IconButton::new(Icon::Plus, "Stage lines")
                    .compact()
                    .primary(true)
                    .show(ui)
                    .clicked()
            {
                actions.push(Action::Run(GitOp::ApplyPatch {
                    patch,
                    cached: true,
                    reverse: false,
                }));
            }
            if let Some(patch) = fd.selected_lines_patch(hunk, lines, true)
                && IconButton::text("Discard lines")
                    .compact()
                    .danger()
                    .show(ui)
                    .clicked()
            {
                actions.push(Action::Dialog(discard_lines_dialog(lines.len(), patch)));
            }
        }
        Some(LineOps::Index) => {
            if let Some(patch) = fd.selected_lines_patch(hunk, lines, true)
                && IconButton::new(Icon::Minus, "Unstage lines")
                    .compact()
                    .primary(true)
                    .show(ui)
                    .clicked()
            {
                actions.push(Action::Run(GitOp::ApplyPatch {
                    patch,
                    cached: true,
                    reverse: true,
                }));
            }
        }
        None => {}
    }
}

#[allow(clippy::collapsible_match)]
fn selection_menu_items(
    ui: &mut Ui,
    ops: &Option<LineOps>,
    fd: &FileDiff,
    hunk: usize,
    lines: &[usize],
    actions: &mut Vec<Action>,
) {
    let n = lines.len();
    match ops {
        Some(LineOps::Worktree) => {
            if let Some(patch) = fd.selected_lines_patch(hunk, lines, false) {
                menus::action(
                    ui,
                    &format!("Stage {} selected", format::plural(n, "line", "lines")),
                    actions,
                    Action::Run(GitOp::ApplyPatch {
                        patch,
                        cached: true,
                        reverse: false,
                    }),
                );
            }
            if let Some(patch) = fd.selected_lines_patch(hunk, lines, true) {
                menus::danger_action(
                    ui,
                    &format!("Discard {} selected…", format::plural(n, "line", "lines")),
                    actions,
                    Action::Dialog(discard_lines_dialog(n, patch)),
                );
            }
        }
        Some(LineOps::Index) => {
            if let Some(patch) = fd.selected_lines_patch(hunk, lines, true) {
                menus::action(
                    ui,
                    &format!("Unstage {} selected", format::plural(n, "line", "lines")),
                    actions,
                    Action::Run(GitOp::ApplyPatch {
                        patch,
                        cached: true,
                        reverse: true,
                    }),
                );
            }
        }
        None => {}
    }
}

fn discard_lines_dialog(n: usize, patch: Vec<u8>) -> Dialog {
    Dialog::Confirm {
        title: format!("Discard {}?", format::plural(n, "line", "lines")),
        body: "The selected changes revert to the staged version. This cannot be undone.".into(),
        confirm: "Discard".into(),
        danger: true,
        ops: vec![GitOp::ApplyPatch {
            patch,
            cached: false,
            reverse: true,
        }],
    }
}

fn file_menu_items(
    ui: &mut Ui,
    workdir: &std::path::Path,
    target: &DiffTarget,
    actions: &mut Vec<Action>,
) {
    let path = workdir.join(target.path());
    if path.is_file() {
        menus::action(ui, "Open file", actions, Action::OpenPath(path.clone()));
    }
    if let Some(dir) = path.parent().filter(|d| d.is_dir()) {
        menus::action(
            ui,
            "Reveal in Explorer",
            actions,
            Action::OpenPath(dir.to_path_buf()),
        );
    }
    menus::action(
        ui,
        "Copy path",
        actions,
        Action::Copy(target.path().to_owned()),
    );
}

/// File-level actions, right to left (the rightmost is added first).
fn file_actions(
    ui: &mut Ui,
    snap: &Snapshot,
    target: &DiffTarget,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    let unborn = snap.head.target().is_none();
    match target {
        DiffTarget::Unstaged { path } => {
            if IconButton::new(Icon::Plus, "Stage file")
                .compact()
                .enabled(!busy)
                .show(ui)
                .clicked()
            {
                actions.push(Action::Run(GitOp::Stage {
                    paths: vec![path.clone()],
                }));
            }
            if IconButton::text("Discard")
                .compact()
                .danger()
                .enabled(!busy)
                .show(ui)
                .clicked()
            {
                actions.push(Action::Dialog(Dialog::Confirm {
                    title: format!("Discard changes in {}?", short(path)),
                    body: format!("Unstaged changes in {path} are lost. This cannot be undone."),
                    confirm: "Discard".into(),
                    danger: true,
                    ops: vec![GitOp::Discard {
                        paths: vec![path.clone()],
                    }],
                }));
            }
        }
        DiffTarget::Untracked { path } => {
            if IconButton::new(Icon::Plus, "Stage file")
                .compact()
                .enabled(!busy)
                .show(ui)
                .clicked()
            {
                actions.push(Action::Run(GitOp::Stage {
                    paths: vec![path.clone()],
                }));
            }
            if IconButton::text("Delete")
                .compact()
                .danger()
                .enabled(!busy)
                .show(ui)
                .clicked()
            {
                actions.push(Action::Dialog(Dialog::Confirm {
                    title: format!("Delete {}?", short(path)),
                    body: format!("{path} is untracked. Deleting it cannot be undone."),
                    confirm: "Delete file".into(),
                    danger: true,
                    ops: vec![GitOp::DeleteUntracked {
                        paths: vec![path.clone()],
                    }],
                }));
            }
        }
        DiffTarget::Staged { path, .. } => {
            if IconButton::new(Icon::Minus, "Unstage file")
                .compact()
                .enabled(!busy)
                .show(ui)
                .clicked()
            {
                actions.push(Action::Run(GitOp::Unstage {
                    paths: vec![path.clone()],
                    unborn,
                }));
            }
        }
        DiffTarget::Conflicted { path } => {
            if IconButton::new(Icon::Check, "Mark resolved")
                .compact()
                .enabled(!busy)
                .show(ui)
                .on_hover_text("Stage the file as it is on disk")
                .clicked()
            {
                actions.push(Action::Run(GitOp::MarkResolved {
                    paths: vec![path.clone()],
                }));
            }
            if IconButton::text("Take theirs")
                .compact()
                .enabled(!busy)
                .show(ui)
                .clicked()
            {
                actions.push(Action::Run(GitOp::TakeTheirs { path: path.clone() }));
            }
            if IconButton::text("Keep ours")
                .compact()
                .enabled(!busy)
                .show(ui)
                .clicked()
            {
                actions.push(Action::Run(GitOp::TakeOurs { path: path.clone() }));
            }
        }
        DiffTarget::Commit { path, .. } => {
            if IconButton::icon(Icon::Copy)
                .compact()
                .show(ui)
                .on_hover_text("Copy path")
                .clicked()
            {
                actions.push(Action::Copy(path.clone()));
            }
        }
    }
}

/// Stage / unstage / discard buttons on a hunk header, right-aligned in
/// the visible part of the row.
#[allow(clippy::too_many_arguments)]
fn hunk_buttons(
    ui: &Ui,
    p: &Palette,
    rect: Rect,
    clip_right: f32,
    target: &DiffTarget,
    diff: &FileDiff,
    h: usize,
    actions: &mut Vec<Action>,
) {
    let Some(patch) = diff.hunk_patch(h) else {
        return;
    };
    let y = rect.center().y;
    let right = clip_right.min(rect.right()) - 6.0;
    let id = |k: &str| Id::unique(("hunk_btn", h, k.to_owned()));
    match target {
        DiffTarget::Unstaged { .. } => {
            let (stage, left) = row_button(ui, p, right, y, id("stage"), "Stage hunk", false);
            if stage.clicked() {
                actions.push(Action::Run(GitOp::ApplyPatch {
                    patch: patch.clone(),
                    cached: true,
                    reverse: false,
                }));
            }
            let (discard, _) =
                row_button(ui, p, left - 4.0, y, id("discard"), "Discard hunk", true);
            if discard.clicked() {
                actions.push(Action::Dialog(Dialog::Confirm {
                    title: "Discard this hunk?".into(),
                    body: "The lines in this hunk revert to the staged version. This cannot be undone.".into(),
                    confirm: "Discard hunk".into(),
                    danger: true,
                    ops: vec![GitOp::ApplyPatch {
                        patch,
                        cached: false,
                        reverse: true,
                    }],
                }));
            }
        }
        DiffTarget::Staged { .. } => {
            let (unstage, _) = row_button(ui, p, right, y, id("unstage"), "Unstage hunk", false);
            if unstage.clicked() {
                actions.push(Action::Run(GitOp::ApplyPatch {
                    patch,
                    cached: true,
                    reverse: true,
                }));
            }
        }
        _ => {}
    }
}

fn short(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}
