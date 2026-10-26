pub mod activity;
pub mod console;
pub mod details;
pub mod dialogs;
pub mod diff;
pub mod form;
pub mod github;
pub mod graph;
pub mod icons;
pub mod insights;
pub mod kit;
pub mod menus;
pub mod osint;
pub mod rail;
pub mod security;
pub mod sidebar;
pub mod statusbar;
pub mod toasts;
pub mod toolbar;
pub mod welcome;

use egui::{
    Align2, Color32, CornerRadius, FontId, Galley, Painter, Pos2, Rect, Stroke, StrokeKind, pos2,
    vec2,
};
use std::sync::Arc;

/// Lay out `text` on one line, cut with `…` when wider than `max_width`.
pub fn truncated(
    painter: &Painter,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> Arc<Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap.max_width = max_width.max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    painter.layout_job(job)
}

/// Draw a rounded pill with text, cut with `…` when wider than `max_width`;
/// returns its rect.
pub fn pill(
    painter: &Painter,
    pos: Pos2,
    text: &str,
    fill: Color32,
    stroke: Color32,
    text_color: Color32,
    max_width: f32,
) -> Rect {
    let font = FontId::new(11.5, crate::theme::medium());
    let galley = truncated(
        painter,
        text,
        font,
        text_color,
        (max_width - 12.0).max(16.0),
    );
    let size = vec2(galley.size().x + 12.0, 18.0);
    let rect = Rect::from_min_size(pos, size);
    painter.rect(
        rect,
        CornerRadius::same(5),
        fill,
        Stroke::new(1.0, stroke),
        StrokeKind::Inside,
    );
    painter.galley(
        pos2(rect.left() + 6.0, rect.center().y - galley.size().y / 2.0),
        galley,
        text_color,
    );
    rect
}

pub fn pill_width(painter: &Painter, text: &str) -> f32 {
    painter
        .layout_no_wrap(
            text.to_owned(),
            FontId::new(11.5, crate::theme::medium()),
            Color32::WHITE,
        )
        .size()
        .x
        + 12.0
}

/// Author avatar: coloured circle with initials.
pub fn avatar(painter: &Painter, center: Pos2, radius: f32, name: &str, dark: bool) {
    let color = crate::theme::name_color(name, dark);
    painter.circle_filled(
        center,
        radius,
        color.gamma_multiply(if dark { 0.35 } else { 0.25 }),
    );
    painter.text(
        center,
        Align2::CENTER_CENTER,
        crate::format::initials(name),
        FontId::proportional((radius * 0.95).max(7.0)),
        color,
    );
}
