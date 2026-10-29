//! Pieces shared by the Tools page sections: pane frames, the missing-tool
//! card, an error card with retry, and a read-only diff renderer with
//! syntax colours.

use std::hash::Hash;

use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Label, Margin, Rect, Response, RichText, Sense,
    Stroke, Ui, pos2, vec2,
};
use hadion_core::devops::tools::Tool;
use hadion_core::diff::LineKind;

use crate::devops::pages::layout;
use crate::state::{Action, DiffDoc};
use crate::theme::{Palette, pal, size};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Tone};

/// The list pane on the left (gives way in a narrow window, see
/// `pages::layout`).
pub fn list_pane(ui: &mut Ui, id: &str, default: f32, add: impl FnOnce(&mut Ui)) {
    layout::list_pane(ui, id, default, add);
}

/// The scrollable, padded, width-capped detail pane.
pub fn detail_pane(ui: &mut Ui, salt: impl Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui)) {
    layout::detail_pane(ui, salt, add);
}

/// A full-width scrolling page below the header.
pub fn page(ui: &mut Ui, salt: impl Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui)) {
    layout::page(ui, salt, add);
}

/// The remaining area for an empty state or a spinner.
pub fn empty_pane(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas).inner_margin(Margin::same(24)))
        .show(ui, add);
}

/// `kit::search_box` on its own row (it centres itself in the available
/// height, and with text its clear button overflows by 8 points, which
/// makes a resizable panel grow).
pub fn search_field(ui: &mut Ui, id: &str, text: &mut String, hint: &str) -> Response {
    ui.horizontal(|ui| {
        if !text.is_empty() {
            ui.set_max_width(ui.available_width() - 8.0);
        }
        kit::search_box(ui, id, text, hint)
    })
    .inner
}

/// A tool that is not installed: its name, why it is needed, how to get
/// it, and "Check again". Returns true when that was clicked.
pub fn missing_tool(ui: &mut Ui, tool: Option<Tool>, program: &str, why: &str) -> bool {
    let (name, hint) = match tool {
        Some(t) => (t.name().to_owned(), t.install_hint().to_owned()),
        None => (
            program.to_owned(),
            format!("Install {program} so it is on PATH, then check again."),
        ),
    };
    let mut clicked = false;
    kit::callout_with(
        ui,
        Tone::Warning,
        &format!("{name} is not installed"),
        &format!("{why} Hadion looked for `{program}` on PATH and did not find it."),
        |ui| {
            ui.add_space(2.0);
            ui.label(
                RichText::new(format!("Install: {hint}"))
                    .size(size::SMALL)
                    .color(pal(ui).text),
            );
            ui.add_space(4.0);
            clicked = IconButton::new(Icon::Rerun, "Check again")
                .compact()
                .show(ui)
                .clicked();
        },
    );
    clicked
}

/// An error with the exact message and a retry button. Returns true when
/// retry was clicked.
pub fn error_card(ui: &mut Ui, title: &str, error: &str, retry: &str) -> bool {
    let mut clicked = false;
    kit::callout_with(ui, Tone::Danger, title, error, |ui| {
        ui.add_space(4.0);
        clicked = IconButton::new(Icon::Rerun, retry)
            .compact()
            .show(ui)
            .clicked();
    });
    clicked
}

/// A row of buttons that wraps.
pub fn buttons(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        add(ui)
    });
}

/// `+12  −3` in the status colours.
pub fn churn(ui: &mut Ui, added: u64, deleted: u64) {
    let p = *pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            RichText::new(format!("+{}", crate::format::count(added as usize)))
                .size(size::SMALL)
                .color(p.success),
        );
        ui.label(
            RichText::new(format!("−{}", crate::format::count(deleted as usize)))
                .size(size::SMALL)
                .color(p.danger),
        );
    });
}

/// A monospace value that can be clicked to copy.
pub fn copyable(ui: &mut Ui, text: &str, actions: &mut Vec<Action>) {
    let c = pal(ui).text;
    let resp = ui
        .add(
            Label::new(kit::mono(text).color(c))
                .sense(Sense::click())
                .wrap(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.clicked() {
        actions.push(Action::Copy(text.to_owned()));
    }
}

const DIFF_LINE_H: f32 = 19.0;
/// Lines drawn before the rest is summarised.
const DIFF_MAX_LINES: usize = 4_000;

fn line_fill(p: &Palette, kind: LineKind, dark: bool) -> Color32 {
    let a = if dark { 0.13 } else { 0.10 };
    match kind {
        LineKind::Added => p.success.gamma_multiply(a),
        LineKind::Removed => p.danger.gamma_multiply(a),
        _ => Color32::TRANSPARENT,
    }
}

/// A read-only unified diff: hunk headers, both line numbers, markers,
/// change tints and syntax colours.
pub fn diff_view(ui: &mut Ui, id: &str, doc: &DiffDoc) {
    diff_view_or(
        ui,
        id,
        doc,
        "No text changes in this commit (a rename, a mode change or a merge).",
    );
}

/// [`diff_view`] with its own line for a diff without text changes.
pub fn diff_view_or(ui: &mut Ui, id: &str, doc: &DiffDoc, no_text: &str) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let d = &doc.diff;
    if d.binary {
        kit::muted(ui, "Binary file: no text diff.");
        return;
    }
    if d.too_large {
        kit::muted(ui, "Too large to show.");
        return;
    }
    if d.hunks.is_empty() {
        kit::muted(ui, no_text);
        return;
    }
    let font = FontId::monospace(size::MONO);
    let num_font = FontId::monospace(size::CAPTION);
    let total: usize = d.hunks.iter().map(|h| h.lines.len() + 1).sum();
    let longest = d
        .hunks
        .iter()
        .flat_map(|h| h.lines.iter())
        .map(|l| l.text.chars().count())
        .max()
        .unwrap_or(0)
        .min(400);
    let width = (longest as f32 * 7.6 + 110.0).max(ui.available_width());
    Frame::NONE
        .fill(if dark { p.chrome } else { p.canvas })
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(crate::theme::radius::M))
        .show(ui, |ui| {
            egui::ScrollArea::horizontal()
                .id_salt(("wb_diff", id))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let rows = total.min(DIFF_MAX_LINES);
                    let (rect, _) = ui.allocate_exact_size(
                        vec2(width, rows as f32 * DIFF_LINE_H + 8.0),
                        Sense::hover(),
                    );
                    let painter = ui.painter_at(rect);
                    let mut y = rect.top() + 4.0;
                    let mut drawn = 0usize;
                    'outer: for (hi, h) in d.hunks.iter().enumerate() {
                        if drawn >= rows {
                            break;
                        }
                        let hr = Rect::from_min_size(
                            pos2(rect.left(), y),
                            vec2(rect.width(), DIFF_LINE_H),
                        );
                        painter.rect_filled(hr, CornerRadius::ZERO, p.accent.gamma_multiply(0.08));
                        painter.text(
                            pos2(rect.left() + 10.0, hr.center().y),
                            Align2::LEFT_CENTER,
                            format!("@@ {} @@  {}", h.range_text(), h.context()),
                            num_font.clone(),
                            p.text_2,
                        );
                        y += DIFF_LINE_H;
                        drawn += 1;
                        for (li, line) in h.lines.iter().enumerate() {
                            if drawn >= rows {
                                break 'outer;
                            }
                            let lr = Rect::from_min_size(
                                pos2(rect.left(), y),
                                vec2(rect.width(), DIFF_LINE_H),
                            );
                            if ui.is_rect_visible(lr) {
                                painter.rect_filled(
                                    lr,
                                    CornerRadius::ZERO,
                                    line_fill(&p, line.kind, dark),
                                );
                                let mid = lr.center().y;
                                for (x, n) in [(38.0, line.old), (76.0, line.new)] {
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
                                    LineKind::Removed => ("−", p.danger),
                                    LineKind::NoNewline => ("\\", p.text_3),
                                    LineKind::Context => (" ", p.text_3),
                                };
                                painter.text(
                                    pos2(rect.left() + 86.0, mid),
                                    Align2::LEFT_CENTER,
                                    marker,
                                    font.clone(),
                                    mc,
                                );
                                let spans = doc
                                    .spans
                                    .get(hi)
                                    .and_then(|s| s.get(li))
                                    .map(Vec::as_slice)
                                    .unwrap_or(&[]);
                                let base = if line.kind == LineKind::NoNewline {
                                    p.text_3
                                } else {
                                    p.text
                                };
                                let g = line_galley(&painter, &line.text, spans, &font, base, dark);
                                painter.galley(
                                    pos2(rect.left() + 100.0, mid - g.size().y / 2.0),
                                    g,
                                    base,
                                );
                            }
                            y += DIFF_LINE_H;
                            drawn += 1;
                        }
                    }
                });
        });
    if total > DIFF_MAX_LINES {
        kit::muted(
            ui,
            &format!(
                "Showing the first {} of {} lines.",
                crate::format::count(DIFF_MAX_LINES),
                crate::format::count(total)
            ),
        );
    }
}

fn line_galley(
    painter: &egui::Painter,
    text: &str,
    spans: &[crate::state::Span],
    font: &FontId,
    base: Color32,
    dark: bool,
) -> std::sync::Arc<egui::Galley> {
    use egui::text::{LayoutJob, TextFormat};
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let chars: Vec<char> = text.chars().take(2_000).collect();
    let fmt = |c: Color32| TextFormat {
        font_id: font.clone(),
        color: c,
        ..Default::default()
    };
    if spans.is_empty() {
        job.append(&chars.iter().collect::<String>(), 0.0, fmt(base));
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
        if b > a {
            let c = if dark {
                s.color
            } else {
                s.color.lerp_to_gamma(Color32::BLACK, 0.1)
            };
            job.append(&chars[a..b].iter().collect::<String>(), 0.0, fmt(c));
        }
        pos = pos.max(b);
    }
    if pos < chars.len() {
        job.append(&chars[pos..].iter().collect::<String>(), 0.0, fmt(base));
    }
    painter.layout_job(job)
}
