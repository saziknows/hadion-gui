//! Pieces the history views share: a revision input with suggestions, the
//! changed-file row, a virtualised read-only diff, a virtualised file view
//! with line numbers and syntax colours, and the background helpers that
//! colour them.

use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Galley, Painter, Rect, Response, Sense, Stroke,
    Ui, pos2, vec2,
};
use hadion_core::ChangeKind;
use hadion_core::devops::history::numstat::Change;
use hadion_core::diff::{FileDiff, LineKind};

use crate::format;
use crate::state::{DiffDoc, Span};
use crate::theme::{self, Palette, pal, size};
use crate::views::form::{self, InputOpts};
use crate::views::kit;
use crate::views::truncated;

pub const LINE_H: f32 = 19.0;
/// Characters of a line that are laid out (the rest is cut, with a note
/// saying how much).
const MAX_CHARS: usize = 1_500;

/// Width of one character of `font` (a monospace font), for the
/// horizontal extent of a code view.
pub fn char_width(ui: &Ui, font: &FontId) -> f32 {
    let g = ui
        .painter()
        .layout_no_wrap("0".repeat(20), font.clone(), Color32::WHITE);
    (g.size().x / 20.0).max(1.0)
}

/// Width of the longest line of a code view (`longest` characters, capped
/// at [`MAX_CHARS`], plus room for the "more characters" note).
pub fn code_extent(ui: &Ui, font: &FontId, longest: usize) -> f32 {
    let note = if longest >= MAX_CHARS { 220.0 } else { 0.0 };
    longest.min(MAX_CHARS) as f32 * char_width(ui, font) + note
}

/// The size range of a resizable side pane beside a content area that
/// share `total`: the pane gets `min..=max`, but never so much that the
/// content keeps less than `content_min` (a pane dragged wide on a big
/// window would otherwise squeeze the content to nothing on a small one).
pub fn split_range(total: f32, min: f32, max: f32, content_min: f32) -> (f32, f32) {
    let hi = (total - content_min).min(max).max(min);
    (min.min(hi), hi)
}

/// Run `add` with no vertical gap between widgets. `ScrollArea::show_rows`
/// takes the row pitch as the row height plus the *caller's* item spacing,
/// so a virtualised view whose rows sit edge to edge must be shown from a
/// `Ui` with zero spacing; otherwise the rows it paints fall short of the
/// rows it scrolls (an empty band at the bottom, and the end of a long
/// file out of reach).
pub fn rows_ui<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        add(ui)
    })
    .inner
}

// ---------------------------------------------------------------------------
// Revision input
// ---------------------------------------------------------------------------

/// A revision input (branch, tag, SHA, `HEAD~3`) with matching refs offered
/// under it while it has focus. Returns the input's response and whether
/// a suggestion was picked (the value is then set).
pub fn rev_input(
    ui: &mut Ui,
    id: &str,
    value: &mut String,
    hint: &str,
    suggestions: &[String],
    width: f32,
    invalid: bool,
) -> (Response, bool) {
    suggest_input(ui, id, value, hint, suggestions, width, true, invalid)
}

/// An input with matching `suggestions` under it while it has focus.
#[allow(clippy::too_many_arguments)]
pub fn suggest_input(
    ui: &mut Ui,
    id: &str,
    value: &mut String,
    hint: &str,
    suggestions: &[String],
    width: f32,
    mono: bool,
    invalid: bool,
) -> (Response, bool) {
    let p = *pal(ui);
    let mut opts = InputOpts::new(hint).width(width).invalid(invalid);
    if mono {
        opts = opts.mono();
    }
    let resp = form::input(ui, ("hist_rev", id), value, opts);
    let hover_id = egui::Id::unique(("hist_rev_popup", id));
    let popup_hover = ui.data(|d| d.get_temp::<bool>(hover_id).unwrap_or(false));
    let mut picked = false;
    if resp.has_focus() || popup_hover {
        let needle = value.trim().to_lowercase();
        let matches: Vec<&String> = suggestions
            .iter()
            .filter(|s| needle.is_empty() || s.to_lowercase().contains(&needle))
            .filter(|s| s.as_str() != value.trim())
            .take(10)
            .collect();
        if matches.is_empty() {
            ui.data_mut(|d| d.insert_temp(hover_id, false));
            return (resp, false);
        }
        let area = egui::Area::new(hover_id.with("area"))
            .order(egui::Order::Foreground)
            .fixed_pos(resp.rect.left_bottom() + vec2(0.0, 4.0))
            .show(ui.ctx(), |ui| {
                Frame::popup(ui.style()).fill(p.overlay).show(ui, |ui| {
                    ui.set_width(resp.rect.width().max(220.0));
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for m in matches {
                        let (rect, r) = ui
                            .allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
                        kit::row_background(ui, rect, r.hovered(), false);
                        let g = truncated(
                            ui.painter(),
                            m,
                            if mono {
                                FontId::monospace(size::MONO)
                            } else {
                                FontId::proportional(size::BODY)
                            },
                            p.text,
                            rect.width() - 16.0,
                        );
                        ui.painter().galley(
                            pos2(rect.left() + 8.0, rect.center().y - g.size().y / 2.0),
                            g,
                            p.text,
                        );
                        if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            *value = m.clone();
                            picked = true;
                        }
                    }
                });
            });
        let hovered = ui.rect_contains_pointer(area.response.rect);
        ui.data_mut(|d| d.insert_temp(hover_id, hovered && !picked));
    }
    (resp, picked)
}

/// A label in a row of inputs, centred on their height. (A plain label as
/// the first item of a row sits at the top: the row only grows to the
/// inputs' height after it.)
pub fn bar_label(ui: &mut Ui, text: &str) {
    let p = *pal(ui);
    let w = ui
        .painter()
        .layout_no_wrap(text.to_owned(), FontId::proportional(size::SMALL), p.text_2)
        .size()
        .x
        + 2.0;
    ui.add_sized(
        vec2(w, form::INPUT_H),
        egui::Label::new(egui::RichText::new(text).size(size::SMALL).color(p.text_2))
            .wrap_mode(egui::TextWrapMode::Extend),
    );
}

/// Enter pressed in a single-line input that just lost focus.
pub fn submitted(ui: &Ui, resp: &Response) -> bool {
    resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

// ---------------------------------------------------------------------------
// File rows
// ---------------------------------------------------------------------------

pub fn change_kind(c: Change) -> ChangeKind {
    match c {
        Change::Added => ChangeKind::Added,
        Change::Deleted => ChangeKind::Deleted,
        Change::Renamed(_) => ChangeKind::Renamed,
        Change::Copied(_) => ChangeKind::Copied,
        Change::TypeChanged => ChangeKind::TypeChange,
        Change::Unmerged => ChangeKind::Conflicted,
        Change::Modified | Change::Unknown => ChangeKind::Modified,
    }
}

/// The kind letter in a tinted square.
pub fn paint_kind(painter: &Painter, center: egui::Pos2, kind: ChangeKind) {
    let badge = Rect::from_center_size(center, vec2(18.0, 18.0));
    let color = theme::kind_color(kind);
    painter.rect_filled(
        badge,
        CornerRadius::same(theme::radius::S),
        color.gamma_multiply(0.18),
    );
    painter.text(
        badge.center(),
        Align2::CENTER_CENTER,
        kind.letter(),
        FontId::new(size::CAPTION, theme::semibold()),
        color,
    );
}

/// A changed file in `rect`: kind, folder (dim) and name, `+a −d` or
/// `binary` on the right.
#[allow(clippy::too_many_arguments)]
pub fn paint_file_row(
    ui: &Ui,
    rect: Rect,
    kind: ChangeKind,
    path: &str,
    subtitle: Option<&str>,
    added: u64,
    deleted: u64,
    binary: bool,
    selected: bool,
) {
    let p = *pal(ui);
    let painter = ui.painter();
    let mid = rect.center().y;
    let two = subtitle.is_some_and(|s| !s.is_empty()) && rect.height() >= 40.0;
    let line1 = if two { rect.top() + 15.0 } else { mid };
    paint_kind(painter, pos2(rect.left() + 18.0, line1), kind);
    let mut right = rect.right() - 12.0;
    let font = FontId::proportional(size::SMALL);
    if binary {
        let r = painter.text(
            pos2(right, line1),
            Align2::RIGHT_CENTER,
            "binary",
            font.clone(),
            p.text_3,
        );
        right = r.left() - 10.0;
    } else {
        if deleted > 0 {
            let r = painter.text(
                pos2(right, line1),
                Align2::RIGHT_CENTER,
                format!("\u{2212}{}", format::count(deleted as usize)),
                font.clone(),
                p.danger,
            );
            right = r.left() - 6.0;
        }
        if added > 0 {
            let r = painter.text(
                pos2(right, line1),
                Align2::RIGHT_CENTER,
                format!("+{}", format::count(added as usize)),
                font.clone(),
                p.success,
            );
            right = r.left() - 6.0;
        }
        right -= 4.0;
    }
    let x = rect.left() + 36.0;
    let (dir, name) = format::split_path(path);
    let text_color = if selected { p.text_strong } else { p.text };
    let tf = FontId::proportional(size::BODY - 0.5);
    let max_w = (right - x).max(10.0);
    let name_g = painter.layout_no_wrap(name.to_owned(), tf.clone(), text_color);
    let name_w = name_g.size().x.min(max_w);
    let dir_w = (max_w - name_w - 2.0).max(0.0);
    let mut cursor = x;
    if !dir.is_empty() && dir_w > 12.0 {
        let g = truncated(painter, dir, tf.clone(), p.text_3, dir_w);
        let w = g.size().x;
        painter.galley(pos2(cursor, line1 - g.size().y / 2.0), g, p.text_3);
        cursor += w;
    }
    let g = truncated(painter, name, tf, text_color, (right - cursor).max(10.0));
    painter.galley(pos2(cursor, line1 - g.size().y / 2.0), g, text_color);
    if let Some(sub) = subtitle.filter(|_| two) {
        let g = truncated(
            painter,
            sub,
            FontId::proportional(size::SMALL),
            p.text_2,
            rect.right() - 12.0 - x,
        );
        painter.galley(pos2(x, rect.top() + 32.0 - g.size().y / 2.0), g, p.text_2);
    }
}

/// Numbers as one wrapped line of "value label" pairs: the short-window
/// form of a row of stat tiles (`insights::stats`), with the same tooltips.
pub fn compact_stats(ui: &mut Ui, items: &[kit::Stat]) {
    let p = *pal(ui);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(18.0, 4.0);
        for s in items {
            let color = match s.tone {
                kit::Tone::Neutral => p.text_strong,
                t => t.color(&p),
            };
            let resp = ui
                .horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    ui.label(
                        egui::RichText::new(&s.value)
                            .size(size::BODY + 1.0)
                            .family(theme::semibold())
                            .color(color),
                    );
                    ui.label(
                        egui::RichText::new(s.label)
                            .size(size::SMALL)
                            .color(p.text_2),
                    );
                })
                .response;
            kit::tip(resp, s.tip);
        }
    });
}

/// `kit::search_box` on its own row, kept inside a resizable pane (with
/// text, its clear button would otherwise make the pane grow).
pub fn search(ui: &mut Ui, id: &str, text: &mut String, hint: &str) -> Response {
    ui.horizontal(|ui| {
        if !text.is_empty() {
            ui.set_max_width(ui.available_width() - 8.0);
        }
        kit::search_box(ui, id, text, hint)
    })
    .inner
}

// ---------------------------------------------------------------------------
// Background colouring
// ---------------------------------------------------------------------------

/// A diff ready to draw: the parsed diff, its colours and its flat rows.
#[derive(Clone, Debug)]
pub struct DiffShow {
    pub doc: DiffDoc,
    /// `(hunk, line)`; `line == u32::MAX` is the hunk header.
    pub rows: Vec<(u32, u32)>,
    pub longest: usize,
    /// The theme the colours were made for (they are dropped in the other).
    pub dark: bool,
}

impl DiffShow {
    /// Colour and flatten (run on a background thread).
    pub fn new(diff: FileDiff, path: &str, dark: bool) -> DiffShow {
        let spans = crate::highlight::highlight(&diff, path, dark);
        let mut rows = Vec::new();
        let mut longest = 0usize;
        for (hi, h) in diff.hunks.iter().enumerate() {
            rows.push((hi as u32, u32::MAX));
            for (li, l) in h.lines.iter().enumerate() {
                rows.push((hi as u32, li as u32));
                longest = longest.max(l.text.chars().count());
            }
        }
        DiffShow {
            doc: DiffDoc {
                language: crate::highlight::language_for(path),
                diff,
                spans,
            },
            rows,
            longest: longest.min(MAX_CHARS),
            dark,
        }
    }
}

/// A file's text with colours, one entry per line.
#[derive(Clone, Debug, Default)]
pub struct TextDoc {
    pub lines: Vec<String>,
    pub spans: Vec<Vec<Span>>,
    pub language: Option<String>,
    pub longest: usize,
    /// The theme the colours were made for.
    pub dark: bool,
}

impl TextDoc {
    /// Split and colour `bytes` (run on a background thread).
    pub fn new(bytes: &[u8], path: &str, dark: bool) -> TextDoc {
        let lines: Vec<String> = FileDiff::from_new_file(bytes)
            .hunks
            .into_iter()
            .flat_map(|h| h.lines.into_iter().map(|l| l.text))
            .collect();
        let spans = highlight_lines(&lines, path, dark);
        TextDoc {
            longest: lines
                .iter()
                .map(|l| l.chars().count())
                .max()
                .unwrap_or(0)
                .min(MAX_CHARS),
            lines,
            spans,
            language: crate::highlight::language_for(path),
            dark,
        }
    }
}

/// Syntax colours for whole lines of a file (as one context hunk).
pub fn highlight_lines(lines: &[String], path: &str, dark: bool) -> Vec<Vec<Span>> {
    if lines.is_empty() {
        return Vec::new();
    }
    let mut text = lines.join("\n");
    text.push('\n');
    let mut diff = FileDiff::from_new_file(text.as_bytes());
    for h in diff.hunks.iter_mut() {
        for l in h.lines.iter_mut() {
            l.kind = LineKind::Context;
        }
    }
    let mut spans = crate::highlight::highlight(&diff, path, dark);
    let mut out = if spans.is_empty() {
        Vec::new()
    } else {
        spans.swap_remove(0)
    };
    out.resize(lines.len(), Vec::new());
    out
}

// ---------------------------------------------------------------------------
// Drawing code
// ---------------------------------------------------------------------------

/// A line of code with syntax colours (cached by egui). A line longer than
/// [`MAX_CHARS`] ends with a note in `dim` saying how much was cut.
pub fn code_galley(
    painter: &Painter,
    text: &str,
    spans: &[Span],
    font: &FontId,
    base: Color32,
    dim: Color32,
    dark: bool,
) -> Arc<Galley> {
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let chars: Vec<char> = text.chars().take(MAX_CHARS).collect();
    let fmt = |c: Color32| TextFormat {
        font_id: font.clone(),
        color: c,
        ..Default::default()
    };
    let cut = if chars.len() == MAX_CHARS {
        text.chars().count() - MAX_CHARS
    } else {
        0
    };
    let note = |job: &mut LayoutJob| {
        if cut > 0 {
            job.append(
                &format!("  \u{2026} {} more characters", format::count(cut)),
                0.0,
                TextFormat {
                    font_id: FontId::proportional(size::SMALL),
                    color: dim,
                    valign: egui::Align::Center,
                    ..Default::default()
                },
            );
        }
    };
    if spans.is_empty() {
        job.append(&chars.iter().collect::<String>(), 0.0, fmt(base));
        note(&mut job);
        return painter.layout_job(job);
    }
    let mut pos = 0usize;
    for s in spans {
        let (a, b) = (
            (s.start as usize).min(chars.len()),
            (s.end as usize).min(chars.len()),
        );
        if a > pos {
            job.append(&chars[pos..a].iter().collect::<String>(), 0.0, fmt(base));
        }
        if b > a.max(pos) {
            let c = if dark {
                s.color
            } else {
                s.color.lerp_to_gamma(Color32::BLACK, 0.1)
            };
            job.append(
                &chars[a.max(pos)..b].iter().collect::<String>(),
                0.0,
                fmt(c),
            );
        }
        pos = pos.max(b);
    }
    if pos < chars.len() {
        job.append(&chars[pos..].iter().collect::<String>(), 0.0, fmt(base));
    }
    note(&mut job);
    painter.layout_job(job)
}

fn line_fill(p: &Palette, kind: LineKind, dark: bool) -> Color32 {
    let a = if dark { 0.13 } else { 0.10 };
    match kind {
        LineKind::Added => p.success.gamma_multiply(a),
        LineKind::Removed => p.danger.gamma_multiply(a),
        _ => Color32::TRANSPARENT,
    }
}

/// The frame code views sit in.
fn code_frame(ui: &Ui) -> Frame {
    let p = *pal(ui);
    Frame::NONE
        .fill(if ui.visuals().dark_mode {
            p.chrome
        } else {
            p.canvas
        })
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(theme::radius::M))
}

/// A read-only unified diff that fills the available space: hunk headers,
/// both line numbers, markers, change tints and syntax colours. Only the
/// visible rows are drawn.
pub fn diff_view(ui: &mut Ui, id: &str, d: &DiffShow) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let diff = &d.doc.diff;
    if diff.binary {
        kit::callout(
            ui,
            kit::Tone::Neutral,
            "Binary file",
            "The contents differ, but there is no text to show.",
        );
        return;
    }
    if diff.too_large {
        kit::callout(
            ui,
            kit::Tone::Neutral,
            "Too large to show",
            "This diff is over 4 MB. Open both versions in an editor to compare them.",
        );
        return;
    }
    if diff.hunks.is_empty() {
        kit::muted(
            ui,
            "No text changes: a rename, a mode change, or an empty file.",
        );
        return;
    }
    let font = FontId::monospace(size::MONO);
    let num_font = FontId::monospace(size::CAPTION);
    let width = (code_extent(ui, &font, d.longest) + 120.0).max(ui.available_width() - 2.0);
    code_frame(ui).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::ScrollArea::both()
            .id_salt(("hist_diff", id))
            .auto_shrink([false, false])
            .show_rows(ui, LINE_H, d.rows.len(), |ui, range| {
                let top = ui.cursor().top();
                let (rect, _) = ui
                    .allocate_exact_size(vec2(width, LINE_H * range.len() as f32), Sense::hover());
                let painter = ui.painter_at(rect);
                for (k, row) in range.clone().enumerate() {
                    let (hi, li) = d.rows[row];
                    let y = top + k as f32 * LINE_H;
                    let lr = Rect::from_min_size(pos2(rect.left(), y), vec2(width, LINE_H));
                    let mid = lr.center().y;
                    let h = &diff.hunks[hi as usize];
                    if li == u32::MAX {
                        painter.rect_filled(lr, CornerRadius::ZERO, p.accent.gamma_multiply(0.08));
                        painter.text(
                            pos2(rect.left() + 10.0, mid),
                            Align2::LEFT_CENTER,
                            format!("@@ {} @@  {}", h.range_text(), h.context()),
                            num_font.clone(),
                            p.text_2,
                        );
                        continue;
                    }
                    let line = &h.lines[li as usize];
                    painter.rect_filled(lr, CornerRadius::ZERO, line_fill(&p, line.kind, dark));
                    for (x, n) in [(40.0, line.old), (80.0, line.new)] {
                        if let Some(n) = n {
                            painter.text(
                                pos2(rect.left() + x, mid),
                                Align2::RIGHT_CENTER,
                                n.to_string(),
                                num_font.clone(),
                                p.text_3,
                            );
                        }
                    }
                    let (marker, mc) = match line.kind {
                        LineKind::Added => ("+", p.success),
                        LineKind::Removed => ("\u{2212}", p.danger),
                        LineKind::NoNewline => ("\\", p.text_3),
                        LineKind::Context => (" ", p.text_3),
                    };
                    painter.text(
                        pos2(rect.left() + 90.0, mid),
                        Align2::LEFT_CENTER,
                        marker,
                        font.clone(),
                        mc,
                    );
                    let spans = d
                        .doc
                        .spans
                        .get(hi as usize)
                        .and_then(|s| s.get(li as usize))
                        .filter(|_| d.dark == dark)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    let base = if line.kind == LineKind::NoNewline {
                        p.text_3
                    } else {
                        p.text
                    };
                    let g = code_galley(&painter, &line.text, spans, &font, base, p.text_3, dark);
                    painter.galley(pos2(rect.left() + 104.0, mid - g.size().y / 2.0), g, base);
                }
            });
    });
}

/// A file's text with line numbers, filling the available space; `mark`
/// highlights a line (1-based) and `scroll_to` brings one into view.
pub fn text_view(ui: &mut Ui, id: &str, doc: &TextDoc, mark: Option<u32>, scroll_to: Option<u32>) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    if doc.lines.is_empty() {
        kit::muted(ui, "The file is empty.");
        return;
    }
    let font = FontId::monospace(size::MONO);
    let num_font = FontId::monospace(size::CAPTION);
    let digits = doc.lines.len().to_string().len() as f32;
    let gutter = 18.0 + digits * 7.0;
    let width =
        (code_extent(ui, &font, doc.longest) + gutter + 30.0).max(ui.available_width() - 2.0);
    let avail_h = ui.available_height();
    code_frame(ui).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut area = egui::ScrollArea::both()
            .id_salt(("hist_text", id))
            .auto_shrink([false, false]);
        if let Some(line) = scroll_to {
            let off = ((line.saturating_sub(1)) as f32 * LINE_H - avail_h * 0.35).max(0.0);
            area = area.vertical_scroll_offset(off);
        }
        area.show_rows(ui, LINE_H, doc.lines.len(), |ui, range| {
            let top = ui.cursor().top();
            let (rect, _) =
                ui.allocate_exact_size(vec2(width, LINE_H * range.len() as f32), Sense::hover());
            let painter = ui.painter_at(rect);
            for (k, i) in range.clone().enumerate() {
                let y = top + k as f32 * LINE_H;
                let lr = Rect::from_min_size(pos2(rect.left(), y), vec2(width, LINE_H));
                let mid = lr.center().y;
                if mark == Some(i as u32 + 1) {
                    painter.rect_filled(lr, CornerRadius::ZERO, p.accent.gamma_multiply(0.16));
                }
                painter.text(
                    pos2(rect.left() + gutter - 8.0, mid),
                    Align2::RIGHT_CENTER,
                    (i + 1).to_string(),
                    num_font.clone(),
                    p.text_3,
                );
                let spans = doc
                    .spans
                    .get(i)
                    .filter(|_| doc.dark == dark)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let g = code_galley(
                    &painter,
                    &doc.lines[i],
                    spans,
                    &font,
                    p.text,
                    p.text_3,
                    dark,
                );
                painter.galley(
                    pos2(rect.left() + gutter + 4.0, mid - g.size().y / 2.0),
                    g,
                    p.text,
                );
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_panes_leave_the_content_its_room() {
        // A wide window: the pane's own range.
        assert_eq!(split_range(1600.0, 240.0, 760.0, 380.0), (240.0, 760.0));
        // A narrow one: the pane may not take more than total - content.
        let (lo, hi) = split_range(780.0, 240.0, 760.0, 380.0);
        assert_eq!((lo, hi), (240.0, 400.0));
        // Narrower than both minimums: the pane keeps its minimum.
        assert_eq!(split_range(500.0, 240.0, 760.0, 380.0), (240.0, 240.0));
    }

    #[test]
    fn text_doc_lines_and_colours() {
        let d = TextDoc::new(b"fn main() {\n\tlet x = 1;\r\n}\n", "a.rs", true);
        assert_eq!(d.lines, vec!["fn main() {", "    let x = 1;", "}"]);
        assert_eq!(d.spans.len(), 3);
        assert!(!d.spans[0].is_empty(), "Rust is coloured");
        assert_eq!(d.language.as_deref(), Some("Rust"));
        assert_eq!(d.longest, 14);
        let empty = TextDoc::new(b"", "a.rs", true);
        assert!(empty.lines.is_empty());
    }

    #[test]
    fn diff_rows_flatten() {
        let diff = hadion_core::diff::parse(
            b"diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1,2 +1,2 @@\n fn a() {}\n-fn b() {}\n+fn c() {}\n",
        )
        .remove(0);
        let d = DiffShow::new(diff, "x.rs", false);
        assert_eq!(d.rows.len(), 4);
        assert_eq!(d.rows[0], (0, u32::MAX));
        assert_eq!(d.rows[3], (0, 2));
        assert_eq!(change_kind(Change::Renamed(90)), ChangeKind::Renamed);
    }
}
