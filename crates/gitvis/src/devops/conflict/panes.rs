//! The aligned code panes: (base |) ours | result | theirs, one shared
//! vertical scroll (the panes always line up: every conflict block is as
//! tall as its longest side, shorter sides are hatched) and one shared
//! horizontal offset (shift + wheel). Conflict blocks are tinted in their
//! side's colour; the result shows an unresolved conflict as the file has
//! it, markers included.

use std::sync::Arc;

use egui::text::LayoutJob;
use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Galley, Layout, Margin, Painter, Rect,
    RichText, Sense, Stroke, TextFormat, Ui, UiBuilder, pos2, vec2,
};
use gitvis_core::devops::conflict::load;
use gitvis_core::devops::conflict::markers;
use gitvis_core::devops::conflict::resolve::Resolution;

use super::session::{Col, HEADER_H, LINE_H, PartKey, RawLine, RowKind, Session, raw_layout};
use super::{Act, State};
use crate::state::{Action, Span};
use crate::theme::{Palette, Tone, medium, pal, semibold, size};
use crate::views::icons::{Icon, IconButton};
use crate::views::{kit, menus, truncated};

const COL_HEAD_H: f32 = 48.0;

/// The colour a pane stands for.
fn side_color(p: &Palette, col: Col) -> Color32 {
    match col {
        Col::Ours => p.accent,
        Col::Theirs => p.purple,
        Col::Base => p.text_3,
        Col::Result => p.warning,
    }
}

/// Where a result row's text comes from.
enum Src {
    Marker(String),
    Part(PartKey, usize),
}

/// The text of result row `r` of hunk `k`.
fn result_src(s: &Session, k: usize, r: usize) -> Option<Src> {
    let h = s.hunk(k)?;
    let on = markers::line_count(&h.ours);
    let tn = markers::line_count(&h.theirs);
    let raw = |r: usize| -> Option<Src> {
        let rows = raw_layout(h);
        let line = *rows.get(r)?;
        Some(match line {
            RawLine::Marker => {
                let idx = rows[..r].iter().filter(|x| **x == RawLine::Marker).count();
                Src::Marker(
                    super::session::marker_texts(h)
                        .get(idx)
                        .cloned()
                        .unwrap_or_default(),
                )
            }
            RawLine::Ours(i) => Src::Part(PartKey::Ours(k), i),
            RawLine::Base(i) => Src::Part(PartKey::Base(k), i),
            RawLine::Theirs(i) => Src::Part(PartKey::Theirs(k), i),
        })
    };
    match s.res.get(k)? {
        Resolution::Unresolved => raw(r),
        Resolution::Base if h.base.is_none() => raw(r),
        Resolution::Ours => (r < on).then_some(Src::Part(PartKey::Ours(k), r)),
        Resolution::Theirs => (r < tn).then_some(Src::Part(PartKey::Theirs(k), r)),
        Resolution::Base => {
            let bn = h.base.as_deref().map(markers::line_count).unwrap_or(0);
            (r < bn).then_some(Src::Part(PartKey::Base(k), r))
        }
        Resolution::OursThenTheirs => {
            if r < on {
                Some(Src::Part(PartKey::Ours(k), r))
            } else {
                (r < on + tn).then_some(Src::Part(PartKey::Theirs(k), r - on))
            }
        }
        Resolution::TheirsThenOurs => {
            if r < tn {
                Some(Src::Part(PartKey::Theirs(k), r))
            } else {
                (r < on + tn).then_some(Src::Part(PartKey::Ours(k), r - tn))
            }
        }
        Resolution::Custom(b) => {
            (r < markers::line_count(b)).then_some(Src::Part(PartKey::Custom(load::hash(b)), r))
        }
    }
}

fn line_of(s: &mut Session, key: PartKey, i: usize, dark: bool) -> Option<(String, Vec<Span>)> {
    let part = s.part(key, dark);
    Some((
        part.lines.get(i)?.clone(),
        part.spans.get(i).cloned().unwrap_or_default(),
    ))
}

/// One line with its syntax spans (char offsets).
fn galley(
    painter: &Painter,
    text: &str,
    spans: &[Span],
    font: &FontId,
    default: Color32,
) -> Arc<Galley> {
    if spans.is_empty() {
        return painter.layout_no_wrap(text.to_owned(), font.clone(), default);
    }
    let byte_at: Vec<usize> = text
        .char_indices()
        .map(|(b, _)| b)
        .chain(std::iter::once(text.len()))
        .collect();
    let nchars = byte_at.len() as u32 - 1;
    let fmt = |color: Color32| TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    let mut pos = 0u32;
    for sp in spans {
        let (start, end) = (sp.start.min(nchars), sp.end.min(nchars));
        if start > pos {
            job.append(
                &text[byte_at[pos as usize]..byte_at[start as usize]],
                0.0,
                fmt(default),
            );
        }
        if end > start.max(pos) {
            let from = start.max(pos);
            job.append(
                &text[byte_at[from as usize]..byte_at[end as usize]],
                0.0,
                fmt(sp.color),
            );
        }
        pos = pos.max(end);
    }
    if pos < nchars {
        job.append(&text[byte_at[pos as usize]..], 0.0, fmt(default));
    }
    painter.layout_job(job)
}

/// Diagonal hatching for "no line here" (continuous across rows).
fn hatch(painter: &Painter, rect: Rect, color: Color32) {
    let p = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    let step = 9.0;
    let (t, b) = (rect.top(), rect.bottom());
    let first = ((rect.left() + t) / step).floor() * step;
    let mut c = first;
    while c < rect.right() + b + step {
        p.line_segment([pos2(c - t, t), pos2(c - b, b)], Stroke::new(1.0, color));
        c += step;
    }
}

struct Geo {
    cols: Vec<(Col, Rect)>,
    gutter: f32,
    font: FontId,
}

impl Geo {
    fn rect(&self, col: Col) -> Option<Rect> {
        self.cols.iter().find(|(c, _)| *c == col).map(|(_, r)| *r)
    }
}

pub fn show(ui: &mut Ui, st: &mut State, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let show_base = st.show_base;
    let (ours_l, theirs_l) = st.sides(st.current());
    let Some(s) = st.current_mut() else { return };
    let path = s.path().to_owned();
    let base_from_stages = s.hunks().iter().any(|h| h.base_from_stages);
    let added = s.load.entry.kind == gitvis_core::devops::conflict::ConflictKind::BothAdded;
    let any_base = s.hunks().iter().any(|h| h.base.is_some());
    let layout = s.layout(show_base, dark);
    let unresolved = s.unresolved();

    // Columns.
    let order: Vec<Col> = if show_base {
        vec![Col::Base, Col::Ours, Col::Result, Col::Theirs]
    } else {
        vec![Col::Ours, Col::Result, Col::Theirs]
    };
    let full = ui.available_rect_before_wrap();
    let font = FontId::monospace(size::MONO);
    let char_w = ui
        .painter()
        .layout_no_wrap("0".repeat(20), font.clone(), Color32::WHITE)
        .size()
        .x
        / 20.0;
    let digits = (layout.rows.len().max(10) as f32).log10().floor() + 1.0;
    let gutter = (digits.max(3.0) * char_w + 18.0).round();
    let n = order.len() as f32;
    let col_w = ((full.width() - (n - 1.0)) / n).floor();
    let mk_cols = |x0: f32, top: f32, bottom: f32| -> Vec<(Col, Rect)> {
        order
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let x = x0 + i as f32 * (col_w + 1.0);
                (
                    *c,
                    Rect::from_min_max(pos2(x, top), pos2(x + col_w, bottom)),
                )
            })
            .collect()
    };

    // Column headers.
    let (head, _) = ui.allocate_exact_size(vec2(full.width(), COL_HEAD_H), Sense::hover());
    ui.painter().rect_filled(head, 0, p.panel);
    for (col, r) in mk_cols(full.left(), head.top(), head.bottom()) {
        let color = side_color(&p, col);
        let bar_color = if col == Col::Result && unresolved == 0 {
            p.success
        } else {
            color
        };
        ui.painter().rect_filled(
            Rect::from_min_size(r.min, vec2(r.width(), 3.0)),
            0,
            bar_color,
        );
        let (cap, name) = match col {
            Col::Ours => (
                if ours_l.tag.is_empty() {
                    "Ours".to_owned()
                } else {
                    format!("Ours · {}", ours_l.tag)
                },
                ours_l.name.clone(),
            ),
            Col::Theirs => (
                if theirs_l.tag.is_empty() {
                    "Theirs".to_owned()
                } else {
                    format!("Theirs · {}", theirs_l.tag)
                },
                theirs_l.name.clone(),
            ),
            Col::Base => (
                "Base".to_owned(),
                if added {
                    "None: both sides added the file".to_owned()
                } else if !any_base {
                    "Common ancestor: not known for these conflicts".to_owned()
                } else if base_from_stages {
                    "Common ancestor, from the index stages".to_owned()
                } else {
                    "Common ancestor".to_owned()
                },
            ),
            Col::Result => (
                "Result".to_owned(),
                if unresolved == 0 {
                    "Every conflict resolved".to_owned()
                } else {
                    format!(
                        "{} to resolve",
                        crate::format::plural(unresolved, "conflict", "conflicts")
                    )
                },
            ),
        };
        let x = r.left() + 12.0;
        let cg = ui.painter().layout_no_wrap(
            cap.to_uppercase(),
            FontId::new(size::CAPTION, medium()),
            p.text_3,
        );
        ui.painter().galley(pos2(x, r.top() + 10.0), cg, p.text_3);
        let ng = truncated(
            ui.painter(),
            &name,
            FontId::new(size::SMALL, medium()),
            p.text_strong,
            r.width() - 24.0,
        );
        ui.painter()
            .galley(pos2(x, r.top() + 26.0), ng, p.text_strong);
        if col != *order.last().unwrap_or(&Col::Theirs) {
            ui.painter()
                .vline(r.right() + 0.5, r.y_range(), Stroke::new(1.0, p.border));
        }
    }
    ui.painter()
        .hline(full.x_range(), head.bottom(), Stroke::new(1.0, p.border));

    // Shared horizontal scroll (shift + wheel, or a horizontal wheel).
    let body = Rect::from_min_max(pos2(full.left(), head.bottom() + 1.0), full.max);
    let text_w = col_w - gutter - 16.0;
    let max_h = (layout.max_chars as f32 * char_w - text_w + 24.0).max(0.0);
    if ui.rect_contains_pointer(body) {
        let dx = ui.input_mut(|i| {
            let d = i.smooth_scroll_delta.x;
            i.smooth_scroll_delta.x = 0.0;
            d
        });
        if dx != 0.0 {
            s.hscroll = (s.hscroll - dx).clamp(0.0, max_h);
        }
    }
    s.hscroll = s.hscroll.clamp(0.0, max_h);
    let hscroll = s.hscroll;

    let mut area = egui::ScrollArea::vertical()
        .id_salt(("conflict_panes", path.as_str()))
        .auto_shrink([false, false]);
    if let Some(k) = s.scroll_to.take()
        && let Some(&row) = layout.headers.get(k)
    {
        area = area.vertical_scroll_offset((layout.rows[row].y - 60.0).max(0.0));
    }
    let focus = s.focus;
    let editing = s.editing.as_ref().map(|(k, _)| *k);
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    area.show_viewport(ui, |ui, viewport| {
        ui.set_height(layout.total_h);
        let origin = ui.max_rect().min;
        let width = ui.max_rect().width();
        let geo = Geo {
            cols: mk_cols(origin.x, 0.0, 0.0),
            gutter,
            font: font.clone(),
        };
        let mut edit_rect: Option<Rect> = None;
        let painter = ui.painter().clone();
        // Pane backgrounds and separators.
        let top = origin.y + viewport.top();
        let bottom = origin.y + viewport.bottom();
        for (i, (_, r)) in geo.cols.iter().enumerate() {
            if i + 1 < geo.cols.len() {
                painter.vline(r.right() + 0.5, top..=bottom, Stroke::new(1.0, p.border));
            }
            painter.rect_filled(
                Rect::from_min_max(pos2(r.left(), top), pos2(r.left() + gutter, bottom)),
                0,
                if dark { p.chrome } else { p.panel },
            );
        }
        let first = layout.rows.partition_point(|r| r.y + r.h < viewport.top());
        for i in first..layout.rows.len() {
            let row = layout.rows[i];
            if row.y > viewport.bottom() {
                break;
            }
            let y = origin.y + row.y;
            let row_rect = Rect::from_min_size(pos2(origin.x, y), vec2(width, row.h));
            let resp = ui.interact(row_rect, ui.make_persistent_id(("crow", i)), Sense::click());
            match row.kind {
                RowKind::Text { seg, line } => {
                    let item = line_of(s, PartKey::Text(seg), line, dark);
                    for (col, r) in &geo.cols {
                        let cell =
                            Rect::from_min_max(pos2(r.left(), y), pos2(r.right(), y + row.h));
                        let num = row.nums[col_index(*col)];
                        draw_line(
                            &painter,
                            &p,
                            &geo,
                            cell,
                            num,
                            item.as_ref(),
                            p.text,
                            hscroll,
                        );
                    }
                    resp.context_menu(|ui| {
                        menus::begin(ui, 180.0);
                        if let Some((t, _)) = &item {
                            menus::action(ui, "Copy line", actions, Action::Copy(t.clone()));
                        }
                    });
                }
                RowKind::Header { hunk } => {
                    header_row(
                        ui,
                        s,
                        &p,
                        &geo,
                        y,
                        hunk,
                        focus == hunk,
                        editing == Some(hunk),
                        actions,
                    );
                    if resp.clicked() {
                        actions.push(Act::Focus(hunk).into());
                    }
                    hunk_menu(&resp, s, hunk, actions);
                }
                RowKind::Block { hunk, row: r } => {
                    let Some(h) = s.hunk(hunk) else { continue };
                    let on = markers::line_count(&h.ours);
                    let tn = markers::line_count(&h.theirs);
                    let bn = h.base.as_deref().map(markers::line_count).unwrap_or(0);
                    let resolved = s.resolved(hunk);
                    for (col, rr) in &geo.cols {
                        let cell =
                            Rect::from_min_max(pos2(rr.left(), y), pos2(rr.right(), y + row.h));
                        let num = row.nums[col_index(*col)];
                        let (src, has) = match col {
                            Col::Ours => (Some(Src::Part(PartKey::Ours(hunk), r)), r < on),
                            Col::Theirs => (Some(Src::Part(PartKey::Theirs(hunk), r)), r < tn),
                            Col::Base => (Some(Src::Part(PartKey::Base(hunk), r)), r < bn),
                            // The edit box covers the block (drawn last).
                            Col::Result if editing == Some(hunk) => continue,
                            Col::Result => {
                                let src = result_src(s, hunk, r);
                                let has = src.is_some();
                                (src, has)
                            }
                        };
                        let tint = match col {
                            Col::Result if resolved => p.success,
                            c => side_color(&p, *c),
                        };
                        let text_rect = Rect::from_min_max(
                            pos2(cell.left() + geo.gutter, cell.top()),
                            cell.max,
                        );
                        if !has {
                            hatch(&painter, text_rect, p.border);
                            continue;
                        }
                        painter.rect_filled(
                            text_rect,
                            0,
                            tint.gamma_multiply(if dark { 0.11 } else { 0.08 }),
                        );
                        painter.rect_filled(
                            Rect::from_min_size(text_rect.min, vec2(3.0, cell.height())),
                            0,
                            tint.gamma_multiply(0.8),
                        );
                        match src {
                            Some(Src::Marker(m)) => {
                                painter.rect_filled(text_rect, 0, p.warning.gamma_multiply(0.10));
                                let item = (m, Vec::new());
                                draw_line(
                                    &painter,
                                    &p,
                                    &geo,
                                    cell,
                                    num,
                                    Some(&item),
                                    p.warning,
                                    hscroll,
                                );
                            }
                            Some(Src::Part(key, li)) => {
                                let item = line_of(s, key, li, dark);
                                draw_line(
                                    &painter,
                                    &p,
                                    &geo,
                                    cell,
                                    num,
                                    item.as_ref(),
                                    p.text,
                                    hscroll,
                                );
                            }
                            None => {}
                        }
                    }
                    if resp.clicked() {
                        actions.push(Act::Focus(hunk).into());
                    }
                    hunk_menu(&resp, s, hunk, actions);
                    if editing == Some(hunk)
                        && edit_rect.is_none()
                        && let Some(rr) = geo.rect(Col::Result)
                    {
                        let header = layout.headers[hunk];
                        let y0 = origin.y + layout.rows[header].y + HEADER_H;
                        let rows = layout.block_rows[hunk];
                        edit_rect = Some(Rect::from_min_size(
                            pos2(rr.left() + 4.0, y0 + 2.0),
                            vec2(rr.width() - 8.0, rows as f32 * LINE_H - 4.0),
                        ));
                    }
                }
            }
        }
        if let (Some(rect), Some(k)) = (edit_rect, editing) {
            edit_box(ui, s, &p, rect, k, &path);
        }
    });
}

fn col_index(c: Col) -> usize {
    match c {
        Col::Ours => 0,
        Col::Result => 1,
        Col::Theirs => 2,
        Col::Base => 3,
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_line(
    painter: &Painter,
    p: &Palette,
    geo: &Geo,
    cell: Rect,
    num: Option<u32>,
    item: Option<&(String, Vec<Span>)>,
    color: Color32,
    hscroll: f32,
) {
    let mid = cell.center().y;
    if let Some(n) = num {
        painter.text(
            pos2(cell.left() + geo.gutter - 10.0, mid),
            Align2::RIGHT_CENTER,
            n.to_string(),
            FontId::monospace(size::CAPTION + 0.5),
            p.text_3,
        );
    }
    let Some((text, spans)) = item else { return };
    if text.is_empty() {
        return;
    }
    let clip = Rect::from_min_max(
        pos2(cell.left() + geo.gutter + 4.0, cell.top()),
        pos2(cell.right() - 4.0, cell.bottom()),
    );
    let pc = painter.with_clip_rect(clip.intersect(painter.clip_rect()));
    let g = galley(&pc, text, spans, &geo.font, color);
    let x = cell.left() + geo.gutter + 10.0 - hscroll;
    pc.galley(pos2(x, mid - g.size().y / 2.0), g, color);
}

/// The bar above a conflict: per side "Take", the status and the result's
/// actions.
#[allow(clippy::too_many_arguments)]
fn header_row(
    ui: &mut Ui,
    s: &Session,
    p: &Palette,
    geo: &Geo,
    y: f32,
    k: usize,
    focused: bool,
    editing: bool,
    actions: &mut Vec<Action>,
) {
    let Some(h) = s.hunk(k) else { return };
    let res = s.res[k].clone();
    let resolved = s.resolved(k);
    let painter = ui.painter().clone();
    let total = s.hunk_count();
    let left = geo.cols.first().map(|(_, r)| r.left()).unwrap_or(0.0);
    let right = geo.cols.last().map(|(_, r)| r.right()).unwrap_or(0.0);
    let band = Rect::from_min_max(pos2(left, y), pos2(right, y + HEADER_H));
    painter.rect_filled(band, 0, if focused { p.selection } else { p.panel });
    painter.hline(band.x_range(), band.top() + 0.5, Stroke::new(1.0, p.border));
    // Each cell fits its pane: labels give way first, then the buttons
    // shorten ("Take ours" to "Ours", Edit and Both to icons), so nothing
    // overlaps in a narrow window.
    let text_w =
        |s: &str, font: FontId| painter.layout_no_wrap(s.to_owned(), font, p.text).size().x;
    // A compact IconButton: 9 points of padding a side, a 14 point icon
    // and a 7 point gap; 26 square without a label.
    let button_w = |label: Option<&str>, icon: bool| -> f32 {
        match label {
            Some(l) => {
                18.0 + if icon { 21.0 } else { 0.0 } + text_w(l, FontId::new(size::SMALL, medium()))
            }
            None => 26.0,
        }
    };
    for (i, (col, r)) in geo.cols.iter().enumerate() {
        let cell = Rect::from_min_max(pos2(r.left(), y), pos2(r.right(), y + HEADER_H));
        let inner = cell.shrink2(vec2(8.0, 4.0));
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(inner)
                .layout(Layout::right_to_left(Align::Center)),
        );
        child.set_clip_rect(inner.intersect(ui.clip_rect()));
        child.spacing_mut().item_spacing.x = 4.0;
        // The label at the left of a cell, in what its buttons leave: whole
        // or not at all ("Confl…" says nothing the toolbar does not).
        let label = |text: &str, font: FontId, color: Color32, buttons: f32| {
            let room = inner.width() - buttons - 12.0;
            let g = painter.layout_no_wrap(text.to_owned(), font, color);
            if g.size().x <= room {
                painter.galley(
                    pos2(inner.left() + 4.0, inner.center().y - g.size().y / 2.0),
                    g,
                    color,
                );
            }
        };
        let take = |full: &'static str, short: &'static str, w: f32| -> &'static str {
            if w >= button_w(Some(full), false) + 4.0 {
                full
            } else {
                short
            }
        };
        match col {
            Col::Ours => {
                let t = take("Take ours", "Ours", inner.width());
                if IconButton::text(t)
                    .compact()
                    .selected(res == Resolution::Ours)
                    .enabled(!editing)
                    .show(&mut child)
                    .on_hover_text("Take our side for this conflict")
                    .clicked()
                {
                    actions.push(Act::Resolve(k, Resolution::Ours).into());
                }
                if i == 0 {
                    // "Conflict 2 of 5" at the start of the bar.
                    label(
                        &format!("Conflict {} of {total}", k + 1),
                        FontId::new(size::SMALL, semibold()),
                        p.text_strong,
                        button_w(Some(t), false),
                    );
                }
            }
            Col::Theirs => {
                let t = take("Take theirs", "Theirs", inner.width());
                if IconButton::text(t)
                    .compact()
                    .selected(res == Resolution::Theirs)
                    .enabled(!editing)
                    .show(&mut child)
                    .on_hover_text("Take their side for this conflict")
                    .clicked()
                {
                    actions.push(Act::Resolve(k, Resolution::Theirs).into());
                }
                label(
                    &format!("line {} in the file", h.start_line + 1),
                    FontId::proportional(size::SMALL),
                    p.text_3,
                    button_w(Some(t), false),
                );
            }
            Col::Base => {
                let known = h.base.is_some();
                let t = take("Take base", "Base", inner.width());
                let b = IconButton::text(t)
                    .compact()
                    .selected(res == Resolution::Base)
                    .enabled(known && !editing)
                    .show(&mut child);
                if i == 0 {
                    label(
                        &format!("Conflict {} of {total}", k + 1),
                        FontId::new(size::SMALL, semibold()),
                        p.text_strong,
                        button_w(Some(t), false),
                    );
                }
                let b = if known {
                    b
                } else {
                    b.on_hover_text(
                        "The common ancestor of this conflict is not known: the file uses the merge conflict style. Recreate the markers in zdiff3 style to see it.",
                    )
                };
                if b.clicked() {
                    actions.push(Act::Resolve(k, Resolution::Base).into());
                }
            }
            Col::Result => {
                let (text, tone) = if editing {
                    ("Editing".to_owned(), Tone::Accent)
                } else if resolved {
                    (res.label().to_owned(), Tone::Success)
                } else if matches!(res, Resolution::Custom(_)) {
                    ("Edited, markers left".to_owned(), Tone::Warning)
                } else {
                    ("Unresolved".to_owned(), Tone::Warning)
                };
                let badge_w = kit::badge_width(&painter, &text) + 8.0;
                // Buttons with labels when they fit beside the status,
                // icons when not; the status goes when even icons need
                // the room (the result's tint still tells it).
                let undo = !editing && res != Resolution::Unresolved;
                let labelled_w = if editing {
                    button_w(Some("Apply"), false) + button_w(Some("Cancel"), false) + 4.0
                } else {
                    button_w(Some("Edit"), true)
                        + button_w(Some("Both"), true)
                        + 4.0
                        + if undo { 30.0 } else { 0.0 }
                };
                let icons_w = if editing {
                    56.0
                } else {
                    56.0 + if undo { 30.0 } else { 0.0 }
                };
                let labelled = inner.width() >= labelled_w + badge_w;
                let show_badge =
                    inner.width() >= badge_w + if labelled { labelled_w } else { icons_w };
                if editing {
                    let apply = if labelled {
                        IconButton::text("Apply")
                    } else {
                        IconButton::icon(Icon::Check)
                    };
                    if apply
                        .primary(true)
                        .compact()
                        .show(&mut child)
                        .on_hover_text("Apply the edit")
                        .clicked()
                    {
                        actions.push(Act::EditApply.into());
                    }
                    let cancel = if labelled {
                        IconButton::text("Cancel")
                    } else {
                        IconButton::icon(Icon::Close)
                    };
                    if cancel
                        .compact()
                        .show(&mut child)
                        .on_hover_text("Cancel the edit")
                        .clicked()
                    {
                        actions.push(Act::EditCancel.into());
                    }
                } else {
                    if undo
                        && IconButton::icon(Icon::Undo)
                            .compact()
                            .show(&mut child)
                            .on_hover_text("Back to unresolved")
                            .clicked()
                    {
                        actions.push(Act::Resolve(k, Resolution::Unresolved).into());
                    }
                    let edit = if labelled {
                        IconButton::new(Icon::Edit, "Edit")
                    } else {
                        IconButton::icon(Icon::Edit)
                    }
                    .compact()
                    .enabled(s.load.utf8)
                    .show(&mut child)
                    .on_hover_text("Edit this conflict's result");
                    if edit.clicked() {
                        actions.push(Act::EditHunk(k).into());
                    }
                    let both = if labelled {
                        IconButton::new(Icon::ChevronDown, "Both")
                    } else {
                        IconButton::icon(Icon::Merge)
                    }
                    .compact()
                    .selected(matches!(
                        res,
                        Resolution::OursThenTheirs | Resolution::TheirsThenOurs
                    ))
                    .show(&mut child)
                    .on_hover_text("Both sides: ours then theirs, or theirs then ours");
                    egui::Popup::menu(&both)
                        .id(egui::Id::unique(("conflict_both", k)))
                        .show(|ui| {
                            menus::begin(ui, 200.0);
                            menus::action(
                                ui,
                                "Ours, then theirs",
                                actions,
                                Act::Resolve(k, Resolution::OursThenTheirs).into(),
                            );
                            menus::action(
                                ui,
                                "Theirs, then ours",
                                actions,
                                Act::Resolve(k, Resolution::TheirsThenOurs).into(),
                            );
                        });
                }
                if show_badge {
                    let dark = ui.visuals().dark_mode;
                    kit::paint_badge(
                        &painter,
                        p,
                        dark,
                        inner.left() + 4.0 + kit::badge_width(&painter, &text),
                        inner.center().y,
                        &text,
                        tone,
                    );
                }
                if i == 0 {
                    label(
                        &format!("Conflict {} of {total}", k + 1),
                        FontId::new(size::SMALL, semibold()),
                        p.text_strong,
                        inner.width(),
                    );
                }
            }
        }
    }
}

/// Right-click on a conflict: copy any side.
fn hunk_menu(resp: &egui::Response, s: &Session, k: usize, actions: &mut Vec<Action>) {
    let Some(h) = s.hunk(k) else { return };
    let text = |b: &[u8]| String::from_utf8_lossy(&markers::to_lf(b)).into_owned();
    // Built only while the menu is open.
    resp.context_menu(|ui| {
        menus::begin(ui, 200.0);
        menus::action(ui, "Copy ours", actions, Action::Copy(text(&h.ours)));
        menus::action(ui, "Copy theirs", actions, Action::Copy(text(&h.theirs)));
        if let Some(b) = &h.base {
            menus::action(ui, "Copy base", actions, Action::Copy(text(b)));
        }
        let result = gitvis_core::devops::conflict::resolve::hunk_bytes(h, &s.res[k]);
        menus::action(ui, "Copy result", actions, Action::Copy(text(&result)));
    });
}

/// The text box over a conflict's result while it is edited.
fn edit_box(ui: &mut Ui, s: &mut Session, p: &Palette, rect: Rect, k: usize, path: &str) {
    let focus = std::mem::take(&mut s.edit_focus);
    let Some((_, buf)) = s.editing.as_mut() else {
        return;
    };
    let before = buf.lines().count();
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
    let resp = Frame::NONE
        .fill(if ui.visuals().dark_mode {
            p.chrome
        } else {
            p.canvas
        })
        .stroke(Stroke::new(1.0, p.accent))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(8, 2))
        .show(&mut child, |ui| {
            ui.set_min_size(rect.size() - vec2(16.0, 4.0));
            ui.add(
                egui::TextEdit::multiline(buf)
                    .id_salt(("conflict_edit", path, k))
                    .font(FontId::monospace(size::MONO))
                    .frame(Frame::NONE)
                    .desired_width(rect.width() - 20.0)
                    .desired_rows(1)
                    .lock_focus(true)
                    .hint_text(
                        RichText::new("Empty: the conflict's lines are removed").color(p.text_3),
                    ),
            )
        })
        .inner;
    if focus {
        resp.request_focus();
    }
    if resp.changed() {
        let after = s
            .editing
            .as_ref()
            .map(|(_, b)| b.lines().count())
            .unwrap_or(0);
        if after != before {
            s.rev += 1;
        }
    }
}
