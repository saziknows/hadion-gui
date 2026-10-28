//! Pieces shared by the git-extras tabs: pane frames, the missing-tool
//! card, an error card with retry, small value widgets and a read-only diff
//! renderer with syntax colours (the same look as the Tools page's other
//! tabs; the workbench's helpers are private to it).

use std::hash::Hash;

use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Label, Margin, Rect, Response, RichText, Sense,
    Stroke, Ui, pos2, vec2,
};
use gitvis_core::diff::LineKind;

use crate::state::{Action, DiffDoc};
use crate::theme::{Palette, pal, size};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Tone};

/// The list pane on the left (same frame as `kit::split_keyed`).
pub fn list_pane(ui: &mut Ui, id: &str, default: f32, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    egui::Panel::left(id)
        .resizable(true)
        .default_size(default)
        .size_range(280.0..=760.0)
        .frame(Frame::NONE.fill(p.panel).inner_margin(Margin {
            left: 8,
            right: 8,
            top: 12,
            bottom: 6,
        }))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui)
        });
}

/// The scrollable, padded, width-capped detail pane.
pub fn detail_pane(ui: &mut Ui, salt: impl Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(salt)
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
                            ui.set_max_width(kit::DETAIL_MAX);
                            ui.spacing_mut().item_spacing.y = 8.0;
                            add(ui)
                        });
                });
        });
}

/// A full-width scrolling page below the header.
pub fn page(ui: &mut Ui, salt: impl Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(salt)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    Frame::NONE
                        .inner_margin(Margin {
                            left: 24,
                            right: 24,
                            top: 16,
                            bottom: 28,
                        })
                        .show(ui, |ui| {
                            ui.set_max_width(kit::DETAIL_MAX + 120.0);
                            ui.spacing_mut().item_spacing.y = 8.0;
                            add(ui)
                        });
                });
        });
}

/// The remaining area for an empty state or a spinner.
pub fn empty_pane(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas).inner_margin(Margin::same(24)))
        .show(ui, add);
}

/// A tool that is not installed: its name, why it is needed, how to get
/// it, and "Check again". Returns true when that was clicked.
pub fn missing_tool(ui: &mut Ui, name: &str, program: &str, why: &str, hint: &str) -> bool {
    let mut clicked = false;
    kit::callout_with(
        ui,
        Tone::Warning,
        &format!("{name} is not installed"),
        &format!("{why} gitvis looked for `{program}` and did not find it."),
        |ui| {
            ui.add_space(2.0);
            ui.add(
                Label::new(
                    RichText::new(format!("Install: {hint}"))
                        .size(size::SMALL)
                        .color(pal(ui).text),
                )
                .wrap(),
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

/// A path with the platform's separators (git prints forward slashes).
pub fn native_path(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().into_owned();
    if cfg!(windows) {
        s.replace('/', "\\")
    } else {
        s
    }
}

/// A path relative to `base` when it is inside it, else the native path.
pub fn short_path(p: &std::path::Path, base: &std::path::Path) -> String {
    let norm = |x: &std::path::Path| x.to_string_lossy().replace('\\', "/").to_lowercase();
    let (ps, bs) = (norm(p), norm(base));
    let bs = bs.trim_end_matches('/');
    if ps.starts_with(&format!("{bs}/")) {
        let full = p.to_string_lossy().replace('\\', "/");
        return full.get(bs.len() + 1..).unwrap_or(&full).to_owned();
    }
    native_path(p)
}

/// `when` as "2026-10-27 14:03 · 3 hours ago".
pub fn when(secs: i64, now: i64) -> String {
    format!(
        "{}  ·  {}",
        crate::format::absolute(secs),
        crate::format::relative(secs, now)
    )
}

/// "loaded 2 min ago" for a page header.
pub fn loaded_ago(at: std::time::Instant) -> String {
    let now = crate::format::now_secs();
    format!(
        "loaded {}",
        crate::format::relative(now - at.elapsed().as_secs() as i64, now)
    )
}

/// One fact on the right of a page header.
pub fn header_note(ui: &mut Ui, text: &str) {
    let c = pal(ui).text_2;
    ui.add_space(6.0);
    ui.label(RichText::new(text).size(size::SMALL).color(c));
}

/// A simple selectable one-line row (inside a scroll pane, not
/// virtualised): `leading` letter, title, right-aligned meta. Returns the
/// response.
pub fn simple_row(
    ui: &mut Ui,
    leading: Option<(&str, Color32)>,
    title: &str,
    meta: &str,
    selected: bool,
    mono: bool,
) -> Response {
    let p = *pal(ui);
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), kit::ROW_H_ONE), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    kit::row_background(ui, rect, resp.hovered(), selected);
    let painter = ui.painter();
    let mid = rect.center().y;
    let mut x = rect.left() + 12.0;
    if let Some((letter, color)) = leading {
        painter.text(
            pos2(x + 5.0, mid),
            Align2::CENTER_CENTER,
            letter,
            FontId::monospace(size::MONO),
            color,
        );
        x += 22.0;
    }
    let mut right = rect.right() - 10.0;
    if !meta.is_empty() {
        let r = painter.text(
            pos2(right, mid),
            Align2::RIGHT_CENTER,
            meta,
            FontId::proportional(size::SMALL),
            p.text_2,
        );
        right = r.left() - 10.0;
    }
    let font = if mono {
        FontId::monospace(size::MONO)
    } else {
        FontId::proportional(size::BODY)
    };
    let color = if selected { p.text_strong } else { p.text };
    let g = crate::views::truncated(painter, title, font, color, (right - x).max(20.0));
    painter.galley(pos2(x, mid - g.size().y / 2.0), g, color);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
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
        kit::muted(
            ui,
            "No text changes (a rename, a mode change or an empty file).",
        );
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
                .id_salt(("gp_diff", id))
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

/// A diff document of one patch file, highlighted (call off the UI thread).
pub fn diff_doc(raw: &[u8], path: &str, dark: bool) -> DiffDoc {
    let mut diffs = gitvis_core::diff::parse(raw);
    if diffs.is_empty() {
        return DiffDoc {
            diff: Default::default(),
            spans: Vec::new(),
            language: None,
        };
    }
    let diff = diffs.remove(0);
    let spans = crate::highlight::highlight(&diff, path, dark);
    DiffDoc {
        language: crate::highlight::language_for(path),
        diff,
        spans,
    }
}
