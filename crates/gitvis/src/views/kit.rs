//! The gitvis component kit.
//!
//! Pages are built from these pieces only, so type, spacing and colour stay
//! the same everywhere. Rules the kit enforces:
//!
//! * Text is never smaller than `size::CAPTION`, and explanations are never
//!   faint: secondary text uses `text_2`, only captions and timestamps use
//!   `text_3`. Paragraphs are capped at [`PROSE`] points per line.
//! * Colour carries meaning: neutral by default, a [`Tone`] only for status.
//! * Tooltips only where they add information; [`tip`] ignores empty text.
//! * Lists are virtualised rows of fixed height ([`list`] + [`paint_row`]).

use std::time::Duration;

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Label, Layout, Margin, Painter, Rect,
    Response, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};

use crate::format;
use crate::state::{Action, ScanRun};
use crate::theme::{Palette, medium, pal, radius, semibold, size, space};
use crate::views::icons::{self, Icon, IconButton};
use crate::views::truncated;

pub use crate::theme::Tone;

/// Longest line of running text, in points.
pub const PROSE: f32 = 760.0;
/// Widest a detail pane's content gets on a wide window.
pub const DETAIL_MAX: f32 = 1040.0;
/// Two-line list row.
pub const ROW_H: f32 = 54.0;
/// One-line list row.
pub const ROW_H_ONE: f32 = 32.0;

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

/// Run `add` with the line length capped at [`PROSE`].
fn capped<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let w = ui.available_width().min(PROSE);
    ui.scope(|ui| {
        ui.set_max_width(w);
        add(ui)
    })
    .inner
}

/// Title of the selected item in a detail pane, or of a card.
pub fn title(ui: &mut Ui, text: &str) -> Response {
    let c = pal(ui).text_strong;
    ui.add(
        Label::new(
            RichText::new(text)
                .size(size::TITLE)
                .family(semibold())
                .color(c),
        )
        .wrap(),
    )
}

/// A smaller title for groups inside a section.
pub fn subtitle(ui: &mut Ui, text: &str) -> Response {
    let c = pal(ui).text_strong;
    ui.add(
        Label::new(
            RichText::new(text)
                .size(size::BODY)
                .family(semibold())
                .color(c),
        )
        .wrap(),
    )
}

/// Running text in the normal colour.
pub fn body(ui: &mut Ui, text: &str) -> Response {
    let c = pal(ui).text;
    capped(ui, |ui| {
        ui.add(Label::new(RichText::new(text).size(size::BODY).color(c)).wrap())
    })
}

/// Explanations, subtitles and labels: smaller, secondary colour.
pub fn secondary(ui: &mut Ui, text: &str) -> Response {
    let c = pal(ui).text_2;
    capped(ui, |ui| {
        ui.add(Label::new(RichText::new(text).size(size::SMALL).color(c)).wrap())
    })
}

/// Timestamps, counts and "nothing here" notes.
pub fn muted(ui: &mut Ui, text: &str) -> Response {
    let c = pal(ui).text_3;
    capped(ui, |ui| {
        ui.add(Label::new(RichText::new(text).size(size::SMALL).color(c)).wrap())
    })
}

/// `RichText` for an uppercase caption.
pub fn caption_text(p: &Palette, text: &str) -> RichText {
    RichText::new(text.to_uppercase())
        .size(size::CAPTION)
        .family(medium())
        .color(p.text_3)
        .extra_letter_spacing(0.7)
}

/// Monospace `RichText` (hashes, paths in tables, commands).
pub fn mono(text: impl Into<String>) -> RichText {
    RichText::new(text).monospace().size(size::MONO)
}

/// Accent-coloured clickable text with a pointing-hand cursor.
pub fn link(ui: &mut Ui, text: &str) -> Response {
    let c = pal(ui).accent;
    ui.add(
        Label::new(RichText::new(text).color(c))
            .sense(Sense::click())
            .wrap(),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Place `widget` centred in `rect` on top of whatever is there, without
/// touching the parent's layout. Use this for hover buttons and badges drawn
/// over rows, tabs and blocks: `Ui::put` advances the parent's cursor to the
/// end of the placed rect, so content after it shifts while it is shown.
pub fn overlay(ui: &mut Ui, rect: Rect, widget: impl egui::Widget) -> Response {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::centered_and_justified(egui::Direction::TopDown)),
    );
    child.add(widget)
}

/// A tooltip, or nothing when `text` is empty.
pub fn tip(resp: Response, text: &str) -> Response {
    if text.trim().is_empty() {
        resp
    } else {
        resp.on_hover_text(text)
    }
}

// ---------------------------------------------------------------------------
// Structure
// ---------------------------------------------------------------------------

/// Section header with space above it.
pub fn section(ui: &mut Ui, title: &str, count: Option<usize>) {
    section_with(ui, title, count, |_| {});
}

/// Section header with widgets (buttons, toggles) on the right.
pub fn section_with(
    ui: &mut Ui,
    title: &str,
    count: Option<usize>,
    trailing: impl FnOnce(&mut Ui),
) {
    let p = *pal(ui);
    ui.add_space(space::XL);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(caption_text(&p, title));
        if let Some(n) = count {
            ui.label(
                RichText::new(format::count(n))
                    .size(size::CAPTION)
                    .color(p.text_3),
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            trailing(ui)
        });
    });
    ui.add_space(space::S - 2.0);
}

/// Hairline across the available width.
pub fn hairline(ui: &mut Ui) {
    let c = pal(ui).border;
    let r = ui.available_rect_before_wrap();
    let y = ui.cursor().top();
    ui.painter().hline(r.x_range(), y, Stroke::new(1.0, c));
    ui.add_space(1.0);
}

/// A tinted note with a coloured edge: "why it matters", warnings, tips.
pub fn callout(ui: &mut Ui, tone: Tone, title: &str, body_text: &str) {
    callout_with(ui, tone, title, body_text, |_| {});
}

/// [`callout`] with extra content (buttons, code) under the text.
pub fn callout_with(
    ui: &mut Ui,
    tone: Tone,
    title: &str,
    body_text: &str,
    extra: impl FnOnce(&mut Ui),
) {
    let p = *pal(ui);
    let c = match tone {
        Tone::Neutral => p.text_3,
        t => t.color(&p),
    };
    let width = ui.available_width().min(PROSE + 40.0);
    let resp = Frame::NONE
        .fill(c.gamma_multiply(if ui.visuals().dark_mode { 0.10 } else { 0.08 }))
        .corner_radius(CornerRadius::same(radius::M))
        .inner_margin(Margin {
            left: 16,
            right: 14,
            top: 10,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.set_width(width - 30.0);
            ui.spacing_mut().item_spacing.y = 4.0;
            if !title.is_empty() {
                ui.label(
                    RichText::new(title)
                        .size(size::SMALL)
                        .family(semibold())
                        .color(p.text_strong),
                );
            }
            if !body_text.is_empty() {
                ui.add(Label::new(RichText::new(body_text).size(size::SMALL).color(p.text)).wrap());
            }
            extra(ui);
        })
        .response;
    let r = resp.rect;
    ui.painter().rect_filled(
        Rect::from_min_max(r.min, pos2(r.left() + 3.0, r.bottom())),
        CornerRadius {
            nw: radius::M,
            sw: radius::M,
            ne: 0,
            se: 0,
        },
        c,
    );
}

/// Monospace block (commands, snippets) with a copy button on hover.
pub fn code_block(ui: &mut Ui, text: &str, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let resp = Frame::NONE
        .fill(if ui.visuals().dark_mode {
            p.chrome
        } else {
            p.raised
        })
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(radius::M))
        .inner_margin(Margin {
            left: 12,
            right: 40,
            top: 9,
            bottom: 9,
        })
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.add(Label::new(mono(text).color(p.text)).wrap());
        })
        .response;
    if ui.rect_contains_pointer(resp.rect) {
        let r = Rect::from_min_size(
            pos2(resp.rect.right() - 32.0, resp.rect.top() + 5.0),
            vec2(26.0, 24.0),
        );
        let b = overlay(ui, r, IconButton::icon(Icon::Copy).compact());
        if b.on_hover_text("Copy").clicked() {
            actions.push(Action::Copy(text.to_owned()));
        }
    }
}

/// A centred placeholder for empty or not-yet-loaded content. Returns true
/// when the action button was clicked.
pub fn empty_state(
    ui: &mut Ui,
    icon: Icon,
    title_text: &str,
    body_text: &str,
    action: Option<(Icon, &str)>,
) -> bool {
    let p = *pal(ui);
    let mut clicked = false;
    ui.add_space((ui.available_height() * 0.22).min(180.0));
    ui.vertical_centered(|ui| {
        ui.set_max_width(440.0);
        icons::show(ui, icon, 30.0, p.text_3);
        ui.add_space(space::M);
        ui.label(
            RichText::new(title_text)
                .size(size::TITLE)
                .family(semibold())
                .color(p.text_strong),
        );
        if !body_text.is_empty() {
            ui.add_space(space::XS);
            ui.add(Label::new(RichText::new(body_text).size(size::SMALL).color(p.text_2)).wrap());
        }
        if let Some((icon, label)) = action {
            ui.add_space(space::L);
            clicked = IconButton::new(icon, label)
                .primary(true)
                .show(ui)
                .clicked();
        }
    });
    clicked
}

/// A spinner with a line of text.
pub fn loading(ui: &mut Ui, text: &str) {
    let c = pal(ui).text_2;
    ui.horizontal(|ui| {
        ui.add(egui::Spinner::new().size(14.0).color(c));
        ui.label(RichText::new(text).size(size::SMALL).color(c));
    });
}

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

/// Label/value rows with a fixed label column.
pub struct Kv<'a> {
    ui: &'a mut Ui,
    label_w: f32,
    palette: Palette,
}

/// Key-value table; `label_w` is the label column width (110 is typical).
pub fn kv(ui: &mut Ui, label_w: f32, rows: impl FnOnce(&mut Kv)) {
    let palette = *pal(ui);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 7.0;
        let mut k = Kv {
            ui,
            label_w,
            palette,
        };
        rows(&mut k);
    });
}

impl Kv<'_> {
    /// A row whose value is built by `value`.
    pub fn row(&mut self, label: &str, value: impl FnOnce(&mut Ui)) {
        let (w, c) = (self.label_w, self.palette.text_2);
        self.ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.allocate_ui_with_layout(vec2(w, 20.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(w);
                // SMALL labels next to BODY values: drop the label so the
                // baselines line up.
                ui.add_space(1.5);
                ui.add(Label::new(RichText::new(label).size(size::SMALL).color(c)).truncate());
            });
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.set_width(ui.available_width());
                value(ui)
            });
        });
    }

    /// A row with a text value (wraps).
    pub fn text(&mut self, label: &str, value: impl Into<RichText>) {
        let c = self.palette.text;
        let v: RichText = value.into();
        self.row(label, |ui| {
            ui.add(Label::new(v.color(c)).wrap());
        });
    }

    /// A row with a muted value ("not signed", "none").
    pub fn muted(&mut self, label: &str, value: &str) {
        let c = self.palette.text_3;
        self.row(label, |ui| {
            ui.add(Label::new(RichText::new(value).color(c)).wrap());
        });
    }
}

/// A big-number tile.
pub struct Stat<'a> {
    pub label: &'a str,
    pub value: String,
    pub tone: Tone,
    pub tip: &'a str,
}

impl<'a> Stat<'a> {
    pub fn new(label: &'a str, value: impl Into<String>) -> Self {
        Self {
            label,
            value: value.into(),
            tone: Tone::Neutral,
            tip: "",
        }
    }

    /// Colour the value; use only for a problem (`Danger`/`Warning`) or a
    /// clearly good state (`Success`).
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    /// Colour the value when `cond` holds.
    pub fn tone_if(self, cond: bool, tone: Tone) -> Self {
        if cond { self.tone(tone) } else { self }
    }

    pub fn tip(mut self, tip: &'a str) -> Self {
        self.tip = tip;
        self
    }
}

/// A wrapped row of stat tiles.
pub fn stats(ui: &mut Ui, items: &[Stat]) {
    // Tiles are measured and allocated at a fixed size, so a tile that does
    // not fit moves to the next row instead of squeezing its label.
    let p = *pal(ui);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
        for s in items {
            let color = match s.tone {
                Tone::Neutral => p.text_strong,
                t => t.color(&p),
            };
            let painter = ui.painter().clone();
            let value = painter.layout_no_wrap(
                s.value.clone(),
                FontId::new(size::DISPLAY, semibold()),
                color,
            );
            let label = painter.layout_no_wrap(
                s.label.to_owned(),
                FontId::proportional(size::SMALL),
                p.text_2,
            );
            let (pad_x, pad_y) = (14.0, 10.0);
            let w = (value.size().x.max(label.size().x) + 2.0 * pad_x).max(108.0);
            let h = value.size().y + 1.0 + label.size().y + 2.0 * pad_y;
            let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
            if ui.is_rect_visible(rect) {
                painter.rect(
                    rect,
                    CornerRadius::same(radius::L),
                    p.raised,
                    Stroke::new(1.0, p.border),
                    StrokeKind::Inside,
                );
                let vy = rect.top() + pad_y;
                let vh = value.size().y;
                painter.galley(pos2(rect.left() + pad_x, vy), value, color);
                painter.galley(pos2(rect.left() + pad_x, vy + vh + 1.0), label, p.text_2);
            }
            tip(resp, s.tip);
        }
    });
    ui.add_space(space::XS);
}

fn badge_colors(p: &Palette, tone: Tone, dark: bool) -> (Color32, Color32) {
    match tone {
        Tone::Neutral => (p.hover, p.text_2),
        t => {
            let c = t.color(p);
            (c.gamma_multiply(if dark { 0.16 } else { 0.12 }), c)
        }
    }
}

const BADGE_H: f32 = 18.0;

fn badge_font() -> FontId {
    FontId::new(size::CAPTION, medium())
}

/// Width of a badge with `text`.
pub fn badge_width(painter: &Painter, text: &str) -> f32 {
    painter
        .layout_no_wrap(text.to_owned(), badge_font(), Color32::WHITE)
        .size()
        .x
        + 12.0
}

/// Paint a badge whose right edge is at `right`, vertically centred on
/// `center_y`; returns its left edge.
pub fn paint_badge(
    painter: &Painter,
    p: &Palette,
    dark: bool,
    right: f32,
    center_y: f32,
    text: &str,
    tone: Tone,
) -> f32 {
    let (fill, fg) = badge_colors(p, tone, dark);
    let g = painter.layout_no_wrap(text.to_owned(), badge_font(), fg);
    let w = g.size().x + 12.0;
    let rect = Rect::from_min_size(pos2(right - w, center_y - BADGE_H / 2.0), vec2(w, BADGE_H));
    painter.rect_filled(rect, CornerRadius::same(radius::S), fill);
    painter.galley(
        pos2(rect.left() + 6.0, rect.center().y - g.size().y / 2.0),
        g,
        fg,
    );
    rect.left()
}

/// An inline badge.
pub fn badge(ui: &mut Ui, text: &str, tone: Tone) -> Response {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let w = badge_width(ui.painter(), text);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, BADGE_H), Sense::hover());
    paint_badge(
        ui.painter(),
        &p,
        dark,
        rect.right(),
        rect.center().y,
        text,
        tone,
    );
    resp
}

/// One labelled bar: `fraction` in 0..1, `value` at the right end. The
/// label gets a tooltip only when it had to be cut.
pub fn bar_row(ui: &mut Ui, label: &str, fraction: f32, value: &str, color: Color32) -> Response {
    let p = *pal(ui);
    let h = 26.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let painter = ui.painter();
    let label_w = (rect.width() * 0.36).clamp(110.0, 340.0);
    let value_w = 110.0;
    let g = truncated(
        painter,
        label,
        FontId::proportional(size::SMALL + 0.5),
        p.text,
        label_w - 12.0,
    );
    let elided = g.elided;
    painter.galley(
        pos2(rect.left(), rect.center().y - g.size().y / 2.0),
        g,
        p.text,
    );
    let track = Rect::from_min_max(
        pos2(rect.left() + label_w, rect.center().y - 3.0),
        pos2(rect.right() - value_w, rect.center().y + 3.0),
    );
    painter.rect_filled(track, CornerRadius::same(3), p.raised);
    let w = (track.width() * fraction.clamp(0.0, 1.0)).max(if fraction > 0.0 { 4.0 } else { 0.0 });
    painter.rect_filled(
        Rect::from_min_size(track.min, vec2(w, track.height())),
        CornerRadius::same(3),
        color,
    );
    painter.text(
        pos2(rect.right(), rect.center().y),
        Align2::RIGHT_CENTER,
        value,
        FontId::proportional(size::SMALL),
        p.text_2,
    );
    if elided {
        resp.on_hover_text_at_pointer(label)
    } else {
        resp
    }
}

/// Bars scaled to the largest value; `fmt` renders the value text.
pub fn bars(
    ui: &mut Ui,
    items: &[(String, f64)],
    color: Color32,
    max_rows: usize,
    fmt: impl Fn(f64) -> String,
) {
    let max = items.iter().map(|(_, v)| *v).fold(0.0, f64::max).max(1e-9);
    for (label, v) in items.iter().take(max_rows) {
        bar_row(ui, label, (*v / max) as f32, &fmt(*v), color);
    }
    if items.len() > max_rows {
        muted(
            ui,
            &format!("and {} more", format::count(items.len() - max_rows)),
        );
    }
}

/// Bars of counts.
pub fn count_bars(ui: &mut Ui, items: &[(String, u32)], color: Color32, max_rows: usize) {
    let v: Vec<(String, f64)> = items.iter().map(|(l, n)| (l.clone(), *n as f64)).collect();
    bars(ui, &v, color, max_rows, |x| format::count(x as usize));
}

/// A table column.
#[derive(Clone, Copy)]
pub struct Col<'a> {
    pub title: &'a str,
    /// Right-aligned, tabular.
    pub numeric: bool,
}

impl<'a> Col<'a> {
    pub fn text(title: &'a str) -> Self {
        Self {
            title,
            numeric: false,
        }
    }

    pub fn num(title: &'a str) -> Self {
        Self {
            title,
            numeric: true,
        }
    }
}

/// A painted table: caption header, hairlines between rows, numeric
/// columns right-aligned. Text columns share the remaining width and are
/// cut with a tooltip when too long.
pub fn table(ui: &mut Ui, cols: &[Col], rows: &[Vec<String>]) {
    if cols.is_empty() {
        return;
    }
    let p = *pal(ui);
    let row_h = 28.0;
    let head_h = 24.0;
    let gap = 24.0;
    let font = FontId::proportional(size::SMALL + 0.5);
    let head_font = FontId::new(size::CAPTION, medium());
    let painter = ui.painter().clone();
    let measure = |s: &str, f: &FontId| {
        painter
            .layout_no_wrap(s.to_owned(), f.clone(), Color32::WHITE)
            .size()
            .x
    };
    // Natural widths.
    let mut widths: Vec<f32> = cols
        .iter()
        .map(|c| measure(&c.title.to_uppercase(), &head_font) + 6.0)
        .collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(cols.len()) {
            widths[i] = widths[i].max(measure(cell, &font));
        }
    }
    let avail = ui.available_width();
    let total = widths.iter().sum::<f32>() + gap * (cols.len() - 1) as f32;
    if total > avail {
        // Shrink text columns proportionally; numeric ones keep their width.
        let numeric: f32 = cols
            .iter()
            .zip(&widths)
            .filter(|(c, _)| c.numeric)
            .map(|(_, w)| *w)
            .sum();
        let text_total: f32 = cols
            .iter()
            .zip(&widths)
            .filter(|(c, _)| !c.numeric)
            .map(|(_, w)| *w)
            .sum();
        let room = (avail - numeric - gap * (cols.len() - 1) as f32).max(60.0);
        if text_total > 0.0 {
            for (c, w) in cols.iter().zip(widths.iter_mut()) {
                if !c.numeric {
                    *w = (*w * room / text_total).max(40.0);
                }
            }
        }
    }
    let table_w = (widths.iter().sum::<f32>() + gap * (cols.len() - 1) as f32).min(avail);
    let (rect, _) = ui.allocate_exact_size(
        vec2(table_w, head_h + row_h * rows.len() as f32),
        Sense::hover(),
    );
    let pointer = ui.ctx().pointer_hover_pos();
    let mut hover_text: Option<String> = None;
    let mut x = rect.left();
    for (i, c) in cols.iter().enumerate() {
        let w = widths[i];
        let (pos, align) = if c.numeric {
            (pos2(x + w, rect.top() + head_h / 2.0), Align2::RIGHT_CENTER)
        } else {
            (pos2(x, rect.top() + head_h / 2.0), Align2::LEFT_CENTER)
        };
        painter.text(
            pos,
            align,
            c.title.to_uppercase(),
            head_font.clone(),
            p.text_3,
        );
        for (r, row) in rows.iter().enumerate() {
            let y = rect.top() + head_h + r as f32 * row_h;
            let cell = row.get(i).map(String::as_str).unwrap_or("");
            let color = if i == 0 { p.text } else { p.text_2 };
            let g = truncated(&painter, cell, font.clone(), color, w);
            let gx = if c.numeric { x + w - g.size().x } else { x };
            let cell_rect = Rect::from_min_size(pos2(x, y), vec2(w, row_h));
            if g.elided && pointer.is_some_and(|p| cell_rect.contains(p)) {
                hover_text = Some(cell.to_owned());
            }
            painter.galley(pos2(gx, y + row_h / 2.0 - g.size().y / 2.0), g, color);
        }
        x += w + gap;
    }
    for r in 0..=rows.len() {
        let y = rect.top() + head_h + r as f32 * row_h;
        if r < rows.len() {
            painter.hline(rect.x_range(), y, Stroke::new(1.0, p.border));
        }
    }
    if let Some(t) = hover_text {
        let id = ui.make_persistent_id(("table_tip", rect.min.x as i32, rect.min.y as i32));
        let resp = ui.interact(rect, id, Sense::hover());
        resp.on_hover_text_at_pointer(t);
    }
}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

/// What sits left of a row's title.
#[derive(Clone, Copy)]
pub enum Leading<'a> {
    None,
    Dot(Color32),
    Icon(Icon, Color32),
    Avatar(&'a str),
}

/// Content of a standard two-line row.
pub struct Row<'a> {
    pub leading: Leading<'a>,
    pub title: &'a str,
    pub subtitle: &'a str,
    /// Right-aligned on the title line: a time, a size, a count.
    pub meta: &'a str,
    /// Right-aligned on the subtitle line.
    pub badges: &'a [(String, Tone)],
    pub mono_title: bool,
    /// Greyed out (dismissed, merged, gone).
    pub dimmed: bool,
}

impl<'a> Row<'a> {
    pub fn new(title: &'a str) -> Self {
        Self {
            leading: Leading::None,
            title,
            subtitle: "",
            meta: "",
            badges: &[],
            mono_title: false,
            dimmed: false,
        }
    }

    pub fn leading(mut self, l: Leading<'a>) -> Self {
        self.leading = l;
        self
    }

    pub fn subtitle(mut self, s: &'a str) -> Self {
        self.subtitle = s;
        self
    }

    pub fn meta(mut self, s: &'a str) -> Self {
        self.meta = s;
        self
    }

    pub fn badges(mut self, b: &'a [(String, Tone)]) -> Self {
        self.badges = b;
        self
    }

    pub fn mono(mut self) -> Self {
        self.mono_title = true;
        self
    }

    pub fn dimmed(mut self, d: bool) -> Self {
        self.dimmed = d;
        self
    }
}

/// Paint a row into `rect` (its background is drawn by [`list`]).
pub fn paint_row(ui: &Ui, rect: Rect, row: &Row, selected: bool) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let painter = ui.painter();
    let two_lines = !row.subtitle.is_empty() || !row.badges.is_empty();
    let (title_y, sub_y) = if two_lines {
        (rect.top() + 18.0, rect.top() + 37.0)
    } else {
        (rect.center().y, rect.center().y)
    };
    let mut x = rect.left() + 12.0;
    let lead_x = x + 7.0;
    match row.leading {
        Leading::None => {}
        Leading::Dot(c) => {
            painter.circle_filled(pos2(lead_x, title_y), 4.0, c);
            x += 22.0;
        }
        Leading::Icon(icon, c) => {
            icons::paint(painter, icon, pos2(lead_x, title_y), 14.0, c);
            x += 24.0;
        }
        Leading::Avatar(name) => {
            crate::views::avatar(painter, pos2(lead_x + 2.0, title_y + 1.0), 10.0, name, dark);
            x += 26.0;
        }
    }
    let right = rect.right() - 12.0;
    let (title_c, sub_c) = if row.dimmed {
        (p.text_3, p.text_3)
    } else if selected {
        (p.text_strong, p.text_2)
    } else {
        (p.text, p.text_2)
    };
    // Title line: meta on the right.
    let mut title_right = right;
    if !row.meta.is_empty() {
        let g = painter.layout_no_wrap(
            row.meta.to_owned(),
            FontId::proportional(size::SMALL),
            p.text_3,
        );
        let w = g.size().x;
        painter.galley(pos2(right - w, title_y - g.size().y / 2.0), g, p.text_3);
        title_right -= w + 10.0;
    }
    let title_font = if row.mono_title {
        FontId::monospace(size::MONO + 0.5)
    } else {
        FontId::new(size::BODY, medium())
    };
    let g = truncated(
        painter,
        row.title,
        title_font,
        title_c,
        (title_right - x).max(20.0),
    );
    painter.galley(pos2(x, title_y - g.size().y / 2.0), g, title_c);
    if !two_lines {
        return;
    }
    // Subtitle line: badges on the right.
    let mut sub_right = right;
    for (text, tone) in row.badges.iter().rev() {
        let tone = if row.dimmed { Tone::Neutral } else { *tone };
        sub_right = paint_badge(painter, &p, dark, sub_right, sub_y, text, tone) - 4.0;
    }
    if !row.subtitle.is_empty() {
        let g = truncated(
            painter,
            row.subtitle,
            FontId::proportional(size::SMALL),
            sub_c,
            (sub_right - x - 6.0).max(20.0),
        );
        painter.galley(pos2(x, sub_y - g.size().y / 2.0), g, sub_c);
    }
}

/// Background of a list row: selection or hover, inset and rounded.
pub fn row_background(ui: &Ui, rect: Rect, hovered: bool, selected: bool) {
    let p = pal(ui);
    let r = rect.shrink2(vec2(4.0, 1.0));
    if selected {
        ui.painter()
            .rect_filled(r, CornerRadius::same(radius::M), p.selection);
    } else if hovered {
        ui.painter()
            .rect_filled(r, CornerRadius::same(radius::M), p.hover);
    }
}

/// A virtualised list of `n` rows of height `row_h`. The background is
/// painted first; `row` paints the content and handles the response.
pub fn list(
    ui: &mut Ui,
    id: &str,
    n: usize,
    row_h: f32,
    is_selected: impl Fn(usize) -> bool,
    mut row: impl FnMut(&mut Ui, Rect, &Response, usize),
) {
    egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false])
        .show_rows(ui, row_h, n, |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for i in range {
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let sel = is_selected(i);
                row_background(ui, rect, resp.hovered(), sel);
                row(ui, rect, &resp, i);
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
            }
        });
}

/// Header above a list: title, count, and widgets on the right.
pub fn list_header(ui: &mut Ui, title: &str, count: Option<usize>, trailing: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            RichText::new(title)
                .size(size::BODY)
                .family(semibold())
                .color(p.text_strong),
        );
        if let Some(n) = count {
            ui.label(
                RichText::new(format::count(n))
                    .size(size::SMALL)
                    .color(p.text_3),
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            trailing(ui)
        });
    });
}

// ---------------------------------------------------------------------------
// Controls
// ---------------------------------------------------------------------------

/// A filter / search input with an icon.
pub fn search_box(ui: &mut Ui, id: &str, text: &mut String, hint: &str) -> Response {
    let p = *pal(ui);
    let id = ui.make_persistent_id(id);
    let focused = ui.memory(|m| m.has_focus(id));
    let stroke = if focused { p.accent } else { p.border };
    Frame::NONE
        .fill(if ui.visuals().dark_mode {
            p.chrome
        } else {
            p.canvas
        })
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(CornerRadius::same(radius::M))
        .inner_margin(Margin::symmetric(8, 3))
        .show(ui, |ui| {
            let row_w = ui.available_width();
            ui.allocate_ui_with_layout(
                vec2(row_w, 24.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_width(row_w);
                    ui.spacing_mut().item_spacing.x = 6.0;
                    icons::show(ui, Icon::Search, 13.0, p.text_3);
                    let w = ui.available_width() - (if text.is_empty() { 0.0 } else { 34.0 });
                    let resp = ui.add(
                        egui::TextEdit::singleline(text)
                            .id(id)
                            .frame(Frame::NONE)
                            // egui puts single-line text at the top of a
                            // taller field; centre it on the row instead.
                            .margin(Margin::ZERO)
                            .align(Align2::LEFT_CENTER)
                            .min_size(vec2(0.0, 24.0))
                            .hint_text(RichText::new(hint).color(p.text_3))
                            .desired_width(w.max(40.0)),
                    );
                    if !text.is_empty()
                        && IconButton::icon(Icon::Close)
                            .compact()
                            .show(ui)
                            .on_hover_text("Clear")
                            .clicked()
                    {
                        text.clear();
                    }
                    resp
                },
            )
            .inner
        })
        .inner
}

/// A filter chip; selected chips are tinted.
pub fn chip(ui: &mut Ui, label: &str, selected: bool) -> Response {
    let p = *pal(ui);
    let font = FontId::new(size::SMALL, medium());
    let g = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, Color32::WHITE);
    let size = vec2(g.size().x + 20.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let (fill, stroke, fg) = if selected {
        (p.selection, p.accent.gamma_multiply(0.6), p.text_strong)
    } else if resp.hovered() {
        (p.hover, p.border_strong, p.text)
    } else {
        (Color32::TRANSPARENT, p.border, p.text_2)
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(12),
        fill,
        Stroke::new(1.0, stroke),
        StrokeKind::Inside,
    );
    ui.painter().galley_with_override_text_color(
        pos2(rect.left() + 10.0, rect.center().y - g.size().y / 2.0),
        g,
        fg,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Segmented control; returns the clicked option.
pub fn segmented<T: Copy + PartialEq>(ui: &mut Ui, options: &[(T, &str)], value: T) -> Option<T> {
    let p = *pal(ui);
    let mut clicked = None;
    let font = FontId::new(size::SMALL, medium());
    let widths: Vec<f32> = options
        .iter()
        .map(|(_, label)| {
            ui.painter()
                .layout_no_wrap((*label).to_owned(), font.clone(), Color32::WHITE)
                .size()
                .x
                + 20.0
        })
        .collect();
    let total = widths.iter().sum::<f32>() + 2.0 * (options.len().saturating_sub(1)) as f32 + 4.0;
    ui.allocate_ui_with_layout(
        vec2(total, 28.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            Frame::NONE
                .fill(if ui.visuals().dark_mode {
                    p.chrome
                } else {
                    p.raised
                })
                .corner_radius(CornerRadius::same(radius::M))
                .inner_margin(Margin::same(2))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        for (v, label) in options {
                            let g = ui.painter().layout_no_wrap(
                                (*label).to_owned(),
                                font.clone(),
                                Color32::WHITE,
                            );
                            let (rect, resp) = ui
                                .allocate_exact_size(vec2(g.size().x + 20.0, 24.0), Sense::click());
                            let on = *v == value;
                            if on {
                                ui.painter().rect_filled(
                                    rect,
                                    CornerRadius::same(radius::S + 1),
                                    if ui.visuals().dark_mode {
                                        p.hover
                                    } else {
                                        p.canvas
                                    },
                                );
                            }
                            let fg = if on {
                                p.text_strong
                            } else if resp.hovered() {
                                p.text
                            } else {
                                p.text_2
                            };
                            ui.painter().galley_with_override_text_color(
                                pos2(rect.left() + 10.0, rect.center().y - g.size().y / 2.0),
                                g,
                                fg,
                            );
                            if resp.clicked() && !on {
                                clicked = Some(*v);
                            }
                        }
                    });
                });
        },
    );
    clicked
}

// ---------------------------------------------------------------------------
// Navigation and page frames
// ---------------------------------------------------------------------------

/// One entry of a [`section_nav`].
pub struct NavItem<'a> {
    pub label: &'a str,
    /// A count shown on the right; tinted when the tone is not neutral.
    pub count: Option<(String, Tone)>,
    pub hint: &'a str,
}

impl<'a> NavItem<'a> {
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            count: None,
            hint: "",
        }
    }

    pub fn count(mut self, n: usize, tone: Tone) -> Self {
        if n > 0 {
            self.count = Some((format::count(n), tone));
        }
        self
    }

    pub fn hint(mut self, hint: &'a str) -> Self {
        self.hint = hint;
        self
    }
}

/// A group of [`NavItem`]s under a caption.
pub struct NavGroup<'a> {
    pub title: &'a str,
    pub items: Vec<NavItem<'a>>,
}

/// Vertical section navigation; items are numbered across groups in order.
/// Returns the index of the clicked item.
pub fn section_nav(ui: &mut Ui, groups: &[NavGroup], selected: usize) -> Option<usize> {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let mut clicked = None;
    let mut index = 0usize;
    for (gi, g) in groups.iter().enumerate() {
        if !g.title.is_empty() {
            ui.add_space(if gi == 0 { 2.0 } else { space::L });
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(caption_text(&p, g.title));
            });
            ui.add_space(2.0);
        }
        for item in &g.items {
            let (rect, resp) =
                ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
            let on = index == selected;
            if on {
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(radius::M), p.selection);
            } else if resp.hovered() {
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(radius::M), p.hover);
            }
            let fg = if on { p.text_strong } else { p.text };
            let font = if on {
                FontId::new(size::BODY, medium())
            } else {
                FontId::proportional(size::BODY)
            };
            let mut right = rect.right() - 10.0;
            if let Some((text, tone)) = &item.count {
                if *tone == Tone::Neutral {
                    let g = ui.painter().layout_no_wrap(
                        text.clone(),
                        FontId::proportional(size::SMALL),
                        p.text_3,
                    );
                    right -= g.size().x;
                    ui.painter().galley(
                        pos2(right, rect.center().y - g.size().y / 2.0),
                        g,
                        p.text_3,
                    );
                } else {
                    right =
                        paint_badge(ui.painter(), &p, dark, right, rect.center().y, text, *tone);
                }
                right -= 8.0;
            }
            let g = truncated(
                ui.painter(),
                item.label,
                font,
                fg,
                (right - rect.left() - 12.0).max(20.0),
            );
            ui.painter().galley(
                pos2(rect.left() + 10.0, rect.center().y - g.size().y / 2.0),
                g,
                fg,
            );
            let resp = tip(resp, item.hint).on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.clicked() && !on {
                clicked = Some(index);
            }
            index += 1;
        }
    }
    clicked
}

/// A page with a section navigation column on the left and `content` on
/// the right (which may split further with [`split`]).
pub fn page_with_nav(
    ui: &mut Ui,
    id: &str,
    nav: impl FnOnce(&mut Ui),
    content: impl FnOnce(&mut Ui),
) {
    let p = *pal(ui);
    egui::Panel::left(id)
        .resizable(false)
        .exact_size(228.0)
        .frame(Frame::NONE.fill(p.panel).inner_margin(Margin {
            left: 8,
            right: 8,
            top: 12,
            bottom: 8,
        }))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            egui::ScrollArea::vertical()
                .id_salt((id, "nav"))
                .auto_shrink([false, false])
                .show(ui, nav);
        });
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas))
        .show(ui, content);
}

/// Page header: title, a secondary line, and actions on the right.
pub fn page_header(
    ui: &mut Ui,
    title_text: &str,
    subtitle_text: &str,
    actions_ui: impl FnOnce(&mut Ui),
) {
    let p = *pal(ui);
    Frame::NONE
        .inner_margin(Margin {
            left: 24,
            right: 16,
            top: 14,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(
                        RichText::new(title_text)
                            .size(size::HEADING)
                            .family(semibold())
                            .color(p.text_strong),
                    );
                    if !subtitle_text.is_empty() {
                        ui.label(
                            RichText::new(subtitle_text)
                                .size(size::SMALL)
                                .color(p.text_2),
                        );
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    actions_ui(ui)
                });
            });
        });
    hairline(ui);
}

/// List pane on the left, scrollable detail on the right. The detail's
/// scroll position is kept per `key`: pass the selected item's key so
/// switching items starts at the top.
pub fn split_keyed(
    ui: &mut Ui,
    id: &str,
    key: &str,
    actions: &mut Vec<Action>,
    list_ui: impl FnOnce(&mut Ui, &mut Vec<Action>),
    detail_ui: impl FnOnce(&mut Ui, &mut Vec<Action>),
) {
    let p = *pal(ui);
    egui::Panel::left(id)
        .resizable(true)
        .default_size(420.0)
        .size_range(300.0..=760.0)
        .frame(Frame::NONE.fill(p.panel).inner_margin(Margin {
            left: 8,
            right: 8,
            top: 12,
            bottom: 6,
        }))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            list_ui(ui, actions);
        });
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt((id, "detail", key))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    Frame::NONE
                        .inner_margin(Margin {
                            left: 28,
                            right: 28,
                            top: 20,
                            bottom: 28,
                        })
                        .show(ui, |ui| {
                            ui.set_max_width(DETAIL_MAX);
                            ui.spacing_mut().item_spacing.y = 8.0;
                            detail_ui(ui, actions)
                        });
                });
        });
}

/// A full-width scrollable page of sections.
pub fn scroll_page(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(id)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    Frame::NONE
                        .inner_margin(Margin {
                            left: 28,
                            right: 28,
                            top: 20,
                            bottom: 32,
                        })
                        .show(ui, |ui| {
                            // Never wider than the viewport (narrow windows).
                            let w = ui.available_width().min(DETAIL_MAX + 120.0);
                            ui.set_max_width(w);
                            ui.spacing_mut().item_spacing.y = 8.0;
                            add(ui)
                        });
                });
        });
}

// ---------------------------------------------------------------------------
// Progress
// ---------------------------------------------------------------------------

/// A running job: spinner, phase text, counts, elapsed time, an optional
/// cancel button, and a thin bar under it (indeterminate when the total is
/// unknown). Returns true when cancel was clicked.
pub fn progress(ui: &mut Ui, run: &ScanRun, fallback: &str, cancellable: bool) -> bool {
    let p = *pal(ui);
    let pr = &run.progress;
    let mut text = if pr.phase.is_empty() {
        fallback.to_owned()
    } else {
        pr.phase.clone()
    };
    if let Some(t) = pr.total.filter(|t| *t > 0) {
        text.push_str(&format!(
            "  ·  {} of {}",
            format::count(pr.done),
            format::count(t)
        ));
    } else if pr.done > 0 {
        text.push_str(&format!("  ·  {}", format::count(pr.done)));
    }
    if pr.findings > 0 {
        text.push_str(&format!(
            "  ·  {}",
            format::plural(pr.findings, "finding", "findings")
        ));
    }
    let elapsed = run.started.elapsed();
    let mut cancel = false;
    Frame::NONE
        .inner_margin(Margin::symmetric(16, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.add(egui::Spinner::new().size(13.0).color(p.accent));
                ui.add(Label::new(RichText::new(&text).size(size::SMALL).color(p.text)).truncate());
                ui.label(
                    RichText::new(format::millis(elapsed.as_millis() as u64))
                        .size(size::SMALL)
                        .color(p.text_3),
                );
                if cancellable {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        cancel = IconButton::new(Icon::Stop, "Cancel")
                            .compact()
                            .show(ui)
                            .clicked();
                    });
                }
            });
            ui.add_space(4.0);
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 3.0), Sense::hover());
            ui.painter()
                .rect_filled(rect, CornerRadius::same(2), p.raised);
            match pr.total.filter(|t| *t > 0) {
                Some(t) => {
                    let f = (pr.done as f32 / t as f32).clamp(0.0, 1.0);
                    ui.painter().rect_filled(
                        Rect::from_min_size(rect.min, vec2(rect.width() * f, rect.height())),
                        CornerRadius::same(2),
                        p.accent,
                    );
                }
                None => {
                    let t = ui.input(|i| i.time) as f32;
                    let seg = rect.width() * 0.25;
                    let phase = (t * 0.6).fract();
                    let x0 = rect.left() - seg + (rect.width() + seg) * phase;
                    let r = Rect::from_min_max(
                        pos2(x0.max(rect.left()), rect.top()),
                        pos2((x0 + seg).min(rect.right()), rect.bottom()),
                    );
                    if r.width() > 0.0 {
                        ui.painter().rect_filled(r, CornerRadius::same(2), p.accent);
                    }
                }
            }
        });
    ui.ctx().request_repaint_after(Duration::from_millis(60));
    cancel
}
