//! The job log viewer of the forge Actions page: virtualised rows, nested
//! foldable sections (GitLab `section_start` / Azure `##[group]`) with
//! their durations, colours from ANSI spans, error navigation, a filter,
//! copy line / section.

use std::collections::HashSet;

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align, Align2, Color32, FontId, Frame, Layout, Margin, Rect, RichText, Sense, Ui, pos2, vec2,
};

use gitvis_core::devops::forge::trace::{Line, LineKind};

use super::{Act, LogState};
use crate::format;
use crate::state::Action;
use crate::theme::{Tone, medium, pal, semibold, size};
use crate::views::icons::{self, Icon, IconButton};
use crate::views::kit;

const ROW_H: f32 = 19.0;
const MAX_CHARS: usize = 4_000;
const INDENT: f32 = 16.0;

fn chevron(p: &egui::Painter, c: egui::Pos2, open: bool, color: Color32) {
    let s = 3.6;
    let pts = if open {
        vec![
            pos2(c.x - s, c.y - s * 0.5),
            pos2(c.x, c.y + s * 0.6),
            pos2(c.x + s, c.y - s * 0.5),
        ]
    } else {
        vec![
            pos2(c.x - s * 0.5, c.y - s),
            pos2(c.x + s * 0.6, c.y),
            pos2(c.x - s * 0.5, c.y + s),
        ]
    };
    p.add(egui::Shape::line(pts, egui::Stroke::new(1.5, color)));
}

fn galley(
    p: &egui::Painter,
    line: &Line,
    prefix: &str,
    base: Color32,
    font: &FontId,
    dark: bool,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let fmt = |color: Color32| TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    if !prefix.is_empty() {
        job.append(prefix, 0.0, fmt(base));
    }
    let chars: Vec<char> = line.text.chars().take(MAX_CHARS).collect();
    if line.spans.is_empty() {
        job.append(&chars.iter().collect::<String>(), 0.0, fmt(base));
        return p.layout_job(job);
    }
    let mut pos = 0usize;
    for span in &line.spans {
        let (s, e) = (
            (span.start as usize).min(chars.len()),
            (span.end as usize).min(chars.len()),
        );
        if s > pos {
            job.append(&chars[pos..s].iter().collect::<String>(), 0.0, fmt(base));
        }
        if e > s {
            let mut c = Color32::from_rgb(span.rgb[0], span.rgb[1], span.rgb[2]);
            if !dark {
                c = c.lerp_to_gamma(Color32::BLACK, 0.35);
            }
            job.append(&chars[s..e].iter().collect::<String>(), 0.0, fmt(c));
        }
        pos = pos.max(e);
    }
    if pos < chars.len() {
        job.append(&chars[pos..].iter().collect::<String>(), 0.0, fmt(base));
    }
    p.layout_job(job)
}

/// Depth of a line for indentation: sections it is nested in.
fn depth(log: &gitvis_core::devops::forge::trace::ForgeLog, line: &Line) -> usize {
    let mut d = 0;
    let mut s = line.section;
    while let Some(id) = s {
        d += 1;
        s = log.sections.get(id as usize).and_then(|x| x.parent);
    }
    d
}

fn note(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    Frame::NONE
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, add);
}

/// The viewer for one job's log. `title` names the job, `web` is its page.
#[allow(clippy::too_many_arguments)]
pub fn viewer(
    ui: &mut Ui,
    state: Option<&mut LogState>,
    pipeline: u64,
    key: u64,
    title: &str,
    web: &str,
    filter: &mut String,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let mut jump: Option<i32> = None;
    let mut fold: Option<bool> = None;
    let mut copy_all = false;
    let stats = match &state {
        Some(LogState {
            doc: Some(Ok(d)), ..
        }) => Some((
            d.log.lines.len(),
            d.log.errors.len(),
            d.log.warnings.len(),
            d.log.bytes,
            d.log.sections.len(),
            d.matches.len(),
        )),
        _ => None,
    };
    Frame::NONE
        .inner_margin(Margin {
            left: 16,
            right: 10,
            top: 6,
            bottom: 6,
        })
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                icons::show(ui, Icon::List, 14.0, p.text_2);
                ui.label(
                    RichText::new(title)
                        .size(size::BODY)
                        .family(semibold())
                        .color(p.text_strong),
                );
                if let Some((lines, errors, warnings, bytes, sections, matched)) = stats {
                    ui.label(
                        RichText::new(format!(
                            "{} lines  ·  {}  ·  {}",
                            format::count(lines),
                            format::bytes(bytes),
                            format::plural(sections, "section", "sections")
                        ))
                        .size(size::SMALL)
                        .color(p.text_3),
                    );
                    if errors > 0 {
                        kit::badge(ui, &format::plural(errors, "error", "errors"), Tone::Danger);
                    }
                    if warnings > 0 {
                        kit::badge(
                            ui,
                            &format::plural(warnings, "warning", "warnings"),
                            Tone::Warning,
                        );
                    }
                    if !filter.trim().is_empty() {
                        ui.label(
                            RichText::new(format!("{} matching", format::count(matched)))
                                .size(size::SMALL)
                                .color(p.text_2),
                        );
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if IconButton::icon(Icon::Close)
                        .compact()
                        .show(ui)
                        .on_hover_text("Hide the log")
                        .clicked()
                    {
                        actions.push(Act::CloseLog.into());
                    }
                    if !web.is_empty()
                        && IconButton::icon(Icon::External)
                            .compact()
                            .show(ui)
                            .on_hover_text("Open this job on the web")
                            .clicked()
                    {
                        actions.push(Action::OpenUrl(web.to_owned()));
                    }
                    if IconButton::icon(Icon::Copy)
                        .compact()
                        .show(ui)
                        .on_hover_text("Copy the shown lines")
                        .clicked()
                    {
                        copy_all = true;
                    }
                    if IconButton::icon(Icon::Fetch)
                        .compact()
                        .show(ui)
                        .on_hover_text("Download the log again")
                        .clicked()
                    {
                        actions.push(Act::ShowLog { pipeline, key }.into());
                    }
                    ui.add_space(4.0);
                    icons::divider(ui, 16.0);
                    ui.add_space(4.0);
                    if IconButton::text("Unfold")
                        .compact()
                        .show(ui)
                        .on_hover_text("Expand every section")
                        .clicked()
                    {
                        fold = Some(false);
                    }
                    if IconButton::text("Fold")
                        .compact()
                        .show(ui)
                        .on_hover_text("Collapse every section")
                        .clicked()
                    {
                        fold = Some(true);
                    }
                    if stats.is_some_and(|s| s.1 > 0) {
                        if IconButton::icon(Icon::ChevronDown)
                            .compact()
                            .show(ui)
                            .on_hover_text("Next error")
                            .clicked()
                        {
                            jump = Some(1);
                        }
                        if IconButton::icon(Icon::ChevronUp)
                            .compact()
                            .show(ui)
                            .on_hover_text("Previous error")
                            .clicked()
                        {
                            jump = Some(-1);
                        }
                    }
                    ui.add_space(4.0);
                    let w = ui.available_width().clamp(120.0, 240.0);
                    ui.allocate_ui_with_layout(
                        vec2(w, 26.0),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.set_max_width(w - 8.0);
                            kit::search_box(ui, "forge_log_filter", filter, "Filter lines");
                        },
                    );
                });
            });
        });
    kit::hairline(ui);

    let Some(state) = state else {
        note(ui, |ui| {
            kit::muted(ui, "Log not requested.");
        });
        return;
    };
    let doc = match &mut state.doc {
        None => {
            note(ui, |ui| kit::loading(ui, "Downloading the log…"));
            return;
        }
        Some(Err(e)) => {
            let e = e.clone();
            let hint = if e.contains("HTTP 401")
                || e.contains("HTTP 403")
                || e.contains("401 Unauthorized")
            {
                "This job's log is not public. Sign in (glab auth login) to read it, or open the job on the web."
            } else if e.contains("HTTP 404") {
                "The log is gone (expired or erased), or the job never started."
            } else {
                "Jobs that never started have no log; a running job's log grows until it ends."
            };
            note(ui, |ui| {
                let mut retry = false;
                kit::callout_with(
                    ui,
                    Tone::Warning,
                    "The log is not available",
                    &format!("{e}\n\n{hint}"),
                    |ui| {
                        retry = IconButton::new(Icon::Fetch, "Retry")
                            .compact()
                            .show(ui)
                            .clicked()
                    },
                );
                if retry {
                    actions.push(Act::ShowLog { pipeline, key }.into());
                }
            });
            return;
        }
        Some(Ok(d)) => d,
    };
    doc.set_filter(filter);
    if let Some(f) = fold {
        doc.folded = if f {
            (0..doc.log.sections.len() as u32).collect()
        } else {
            HashSet::new()
        };
    }
    if let Some(dir) = jump
        && !doc.log.errors.is_empty()
    {
        let errors = &doc.log.errors;
        let next = match (dir, doc.selected) {
            (1, Some(c)) => errors.iter().copied().find(|&e| e > c).unwrap_or(errors[0]),
            (-1, Some(c)) => errors
                .iter()
                .rev()
                .copied()
                .find(|&e| e < c)
                .unwrap_or(*errors.last().unwrap()),
            _ => errors[0],
        };
        doc.selected = Some(next);
        doc.scroll_to = Some(next);
    }
    // Unfold the sections around the line to scroll to.
    if let Some(t) = doc.scroll_to {
        let mut s = doc.log.lines.get(t).and_then(|l| l.section);
        while let Some(id) = s {
            doc.folded.remove(&id);
            s = doc.log.sections.get(id as usize).and_then(|x| x.parent);
        }
    }
    let filtering = !doc.filter.trim().is_empty();
    let rows: Vec<usize> = doc
        .matches
        .iter()
        .copied()
        .filter(|&i| filtering || !doc.log.hidden(i, &doc.folded))
        .collect();
    if copy_all {
        let text: String = rows
            .iter()
            .map(|&i| doc.log.lines[i].text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        actions.push(Action::Copy(text));
    }
    if rows.is_empty() {
        note(ui, |ui| {
            kit::muted(
                ui,
                if filtering {
                    "No lines match the filter."
                } else {
                    "The log is empty."
                },
            );
        });
        return;
    }
    let scroll_row = doc
        .scroll_to
        .and_then(|t| rows.iter().position(|&r| r == t));
    doc.scroll_to = None;

    let font = FontId::monospace(size::MONO);
    let char_w = ui
        .painter()
        .layout_no_wrap("0".repeat(20), font.clone(), Color32::WHITE)
        .size()
        .x
        / 20.0;
    let digits = doc.log.lines.len().max(1).to_string().len() as f32;
    let num_w = 12.0 + digits * char_w + 14.0;
    let has_clock = doc.log.lines.iter().take(50).any(|l| !l.clock.is_empty());
    let clock_w = if has_clock { 12.0 * char_w + 14.0 } else { 0.0 };
    let gutter = num_w + clock_w;
    let max_depth = doc
        .log
        .sections
        .iter()
        .map(|s| s.depth as usize + 1)
        .max()
        .unwrap_or(0);
    let content_w = (gutter
        + 18.0
        + max_depth as f32 * INDENT
        + doc.log.max_chars.min(MAX_CHARS) as f32 * char_w
        + 24.0)
        .max(ui.available_width());
    let avail_h = ui.available_height();
    ui.spacing_mut().item_spacing.y = 0.0;
    let mut area = egui::ScrollArea::both()
        .id_salt(("forge_log_area", pipeline, key))
        .auto_shrink([false, false]);
    if let Some(r) = scroll_row {
        area = area.vertical_scroll_offset((r as f32 * ROW_H - avail_h * 0.3).max(0.0));
    }
    let mut select: Option<usize> = None;
    let mut toggle: Option<u32> = None;
    let mut copy: Option<String> = None;
    area.show_rows(ui, ROW_H, rows.len(), |ui, range| {
        let clip_right = ui.clip_rect().right();
        for r in range {
            let li = rows[r];
            let line = &doc.log.lines[li];
            let (rect, resp) = ui.allocate_exact_size(vec2(content_w, ROW_H), Sense::click());
            let painter = ui.painter().clone();
            let mid = rect.center().y;
            let full =
                Rect::from_min_max(rect.min, pos2(clip_right.max(rect.right()), rect.bottom()));
            let selected = doc.selected == Some(li);
            let edge = match line.kind {
                LineKind::Error => Some(p.danger),
                LineKind::Warning => Some(p.warning),
                _ => None,
            };
            if selected {
                painter.rect_filled(full, 0, p.selection);
            } else if line.kind == LineKind::Header || line.kind == LineKind::Banner {
                painter.rect_filled(full, 0, p.raised);
            } else if let Some(c) = edge {
                painter.rect_filled(full, 0, c.gamma_multiply(if dark { 0.13 } else { 0.10 }));
            } else if resp.hovered() {
                painter.rect_filled(full, 0, p.hover.gamma_multiply(0.6));
            }
            if let Some(c) = edge {
                painter.rect_filled(
                    Rect::from_min_size(rect.min, vec2(2.0, rect.height())),
                    0,
                    c,
                );
            }
            painter.text(
                pos2(rect.left() + num_w - 14.0, mid),
                Align2::RIGHT_CENTER,
                (li + 1).to_string(),
                font.clone(),
                p.text_3,
            );
            if has_clock && !line.clock.is_empty() {
                painter.text(
                    pos2(rect.left() + num_w, mid),
                    Align2::LEFT_CENTER,
                    &line.clock,
                    font.clone(),
                    p.text_3,
                );
            }
            let mut x = rect.left() + gutter + depth(&doc.log, line) as f32 * INDENT;
            if let Some(sid) = line.opens {
                let open = !doc.folded.contains(&sid);
                chevron(&painter, pos2(x + 5.0, mid), open, p.text_2);
                x += 18.0;
                let g = galley(
                    &painter,
                    line,
                    "",
                    p.text_strong,
                    &FontId::new(size::MONO, medium()),
                    dark,
                );
                let w = g.size().x;
                painter.galley(pos2(x, mid - g.size().y / 2.0), g, p.text_strong);
                if let Some(s) = doc.log.sections.get(sid as usize) {
                    let mut facts = vec![format::plural(s.len, "line", "lines")];
                    if let Some(d) = s.duration {
                        facts.push(format::duration(d));
                    }
                    if s.name != line.text {
                        facts.push(s.name.clone());
                    }
                    painter.text(
                        pos2(x + w + 10.0, mid),
                        Align2::LEFT_CENTER,
                        facts.join("  ·  "),
                        FontId::proportional(size::SMALL),
                        p.text_3,
                    );
                }
                if resp.clicked() {
                    toggle = Some(sid);
                }
            } else {
                if line.section.is_some() {
                    x += 18.0;
                }
                let base = match line.kind {
                    LineKind::Error => p.danger,
                    LineKind::Warning => p.warning,
                    LineKind::Command => p.purple,
                    LineKind::Banner => p.text_strong,
                    _ => p.text,
                };
                let g = galley(&painter, line, "", base, &font, dark);
                painter.galley(pos2(x, mid - g.size().y / 2.0), g, base);
                if resp.clicked() {
                    select = Some(li);
                }
            }
            let text = line.text.clone();
            let section_text = line.opens.or(line.section).map(|s| doc.log.section_text(s));
            resp.context_menu(|ui| {
                ui.set_min_width(180.0);
                if ui.button("Copy line").clicked() {
                    copy = Some(text.clone());
                    ui.close();
                }
                if let Some(t) = &section_text
                    && ui.button("Copy section").clicked()
                {
                    copy = Some(t.clone());
                    ui.close();
                }
            });
        }
    });
    if let Some(li) = select {
        doc.selected = Some(li);
    }
    if let Some(s) = toggle
        && !doc.folded.remove(&s)
    {
        doc.folded.insert(s);
    }
    if let Some(t) = copy {
        actions.push(Action::Copy(t));
    }
}
