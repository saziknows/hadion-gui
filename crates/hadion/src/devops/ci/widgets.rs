//! View pieces shared by the CI tabs: load states, the missing-tool card,
//! headers, the duration trend chart, one-line run rows.

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Layout, Rect, Response, RichText, Sense, Stroke,
    Ui, pos2, vec2,
};

use hadion_core::devops::tools::Tool;
use hadion_core::github::{Outcome, WorkflowRun};

use super::Load;
use crate::format;
use crate::state::Action;
use crate::theme::{Palette, Tone, medium, pal, size, space};
use crate::views::icons::{Icon, IconButton, outcome_dot};
use crate::views::kit;
use crate::views::truncated;

/// `87%`.
pub fn pct(x: f64) -> String {
    format!("{:.0}%", x * 100.0)
}

/// `640 B`, `12.3 KB`, `1.5 MB`, `4.50 GB`.
pub fn bytes(n: u64) -> String {
    const GB: u64 = 1024 * 1024 * 1024;
    if n >= GB {
        format!("{:.2} GB", n as f64 / GB as f64)
    } else {
        format::bytes(n as usize)
    }
}

/// `2026-10-26 16:46 · 1h ago`.
pub fn when(secs: i64, now: i64) -> String {
    format!(
        "{}  ·  {}",
        format::absolute(secs),
        format::relative(secs, now)
    )
}

pub fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

pub fn outcome_tone(o: Outcome) -> Tone {
    if o.is_bad() {
        Tone::Danger
    } else if o.is_running() {
        Tone::Warning
    } else if o.is_good() {
        Tone::Success
    } else {
        Tone::Neutral
    }
}

pub fn outcome_text(o: Outcome) -> &'static str {
    match o {
        Outcome::Queued => "Queued",
        Outcome::InProgress => "Running",
        Outcome::Success => "Passed",
        Outcome::Failure => "Failed",
        Outcome::Cancelled => "Cancelled",
        Outcome::Skipped => "Skipped",
        Outcome::Neutral => "Neutral",
        Outcome::TimedOut => "Timed out",
        Outcome::ActionRequired => "Action required",
        Outcome::Stale => "Stale",
        Outcome::Unknown => "Unknown",
    }
}

/// Colour of a bar or dot for an outcome.
pub fn outcome_color(p: &Palette, o: Outcome) -> Color32 {
    match outcome_tone(o) {
        Tone::Neutral => p.text_3,
        t => t.color(p),
    }
}

/// The loading, error and never-loaded states of a [`Load`]. Returns the
/// data once there is some (a refresh keeps showing the old data).
/// `retry` is pushed when the retry button is clicked.
pub fn loaded<'a, T>(
    ui: &mut Ui,
    load: &'a Load<T>,
    what: &str,
    loading_text: &str,
    retry: Action,
    actions: &mut Vec<Action>,
) -> Option<&'a T> {
    match &load.data {
        Some(Ok(d)) => Some(d),
        Some(Err(e)) => {
            if load.loading {
                kit::loading(ui, loading_text);
                return None;
            }
            let mut again = false;
            let mut body = e.clone();
            if e.contains("(HTTP 5") {
                body.push_str(
                    "

GitHub answered with a server error; it does that when a listing is very large or the service is busy. Retry in a moment.",
                );
            }
            let title = format!("Could not load {what}");
            kit::callout_with(ui, Tone::Danger, &title, &body, |ui| {
                ui.add_space(2.0);
                again = IconButton::new(Icon::Fetch, "Retry")
                    .compact()
                    .show(ui)
                    .clicked();
            });
            if again {
                actions.push(retry);
            }
            None
        }
        None => {
            if load.loading {
                kit::loading(ui, loading_text);
            } else {
                let mut go = false;
                ui.horizontal(|ui| {
                    kit::muted(ui, "Not loaded yet.");
                    go = IconButton::new(Icon::Fetch, "Load")
                        .compact()
                        .show(ui)
                        .clicked();
                });
                if go {
                    actions.push(retry);
                }
            }
            None
        }
    }
}

/// The state of a list pane while its data is not there: a spinner, or a
/// pointer to the error shown in the detail pane.
pub fn list_state<T>(ui: &mut Ui, load: &Load<T>, what: &str, loading_text: &str) {
    if load.loading {
        kit::loading(ui, loading_text);
    } else if load.err().is_some() {
        kit::muted(
            ui,
            &format!("Could not load {what}; the error and a retry are on the right."),
        );
    } else {
        kit::muted(ui, "Not loaded yet.");
    }
}

/// A short caption in the tertiary colour (counts, freshness), on one line.
pub fn note(ui: &mut Ui, text: &str) -> Response {
    let c = pal(ui).text_3;
    ui.label(RichText::new(text).size(size::SMALL).color(c))
}

/// `refreshed 2m ago`, or `refreshing…`.
pub fn freshness<T>(load: &Load<T>, now: i64) -> String {
    if load.loading {
        return "refreshing…".to_owned();
    }
    match load.at {
        Some(at) => format!(
            "refreshed {}",
            format::relative(now - at.elapsed().as_secs() as i64, now)
        ),
        None => String::new(),
    }
}

/// A missing tool: what it is, why it is needed, how to install it, and a
/// "Check again" button. Returns true when that was clicked.
pub fn tool_missing(ui: &mut Ui, tool: Tool, why: &str) -> bool {
    let mut again = false;
    kit::callout_with(
        ui,
        Tone::Warning,
        &format!("{} is not installed", tool.name()),
        why,
        |ui| {
            ui.add_space(2.0);
            ui.label(
                RichText::new("Install it with")
                    .size(size::SMALL)
                    .color(pal(ui).text_2),
            );
            ui.label(
                kit::mono(tool.install_hint())
                    .color(pal(ui).text)
                    .size(size::MONO),
            );
            ui.add_space(4.0);
            again = IconButton::new(Icon::Fetch, "Check again")
                .compact()
                .show(ui)
                .clicked();
        },
    );
    again
}

/// Call first in a `kit::split_keyed` detail. The kit caps the detail at
/// `kit::DETAIL_MAX` with `set_max_width`, and egui places that width
/// without clamping it, so on a narrow window the detail was wider than
/// what is on screen: text ran off the right edge and right-aligned
/// buttons were out of view. This narrows it back to the visible part
/// (the right margin of the kit's frame is 28 points).
pub fn fit_detail(ui: &mut Ui) {
    let visible = ui.clip_rect().right() - 28.0 - ui.max_rect().left();
    ui.set_max_width(visible.clamp(160.0, kit::DETAIL_MAX));
}

/// Title on the left, buttons on the right (added right to left: the first
/// one ends up rightmost).
pub fn header(ui: &mut Ui, title: &str, buttons: impl FnOnce(&mut Ui)) {
    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        buttons(ui);
        ui.add_space(space::M);
        ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
            kit::title(ui, title);
        });
    });
}

/// A wrapped row of badges.
pub fn badges(ui: &mut Ui, items: &[(String, Tone)]) {
    if items.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        for (t, tone) in items {
            kit::badge(ui, t, *tone);
        }
    });
}

/// A one-line run row: outcome dot, `#N title`, a caption, and duration ·
/// time on the right. Returns the response (click opens the run).
pub fn run_row(ui: &mut Ui, run: &WorkflowRun, caption: &str, now: i64, t: f64) -> Response {
    let p = *pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    kit::row_background(ui, rect, resp.hovered(), false);
    let painter = ui.painter();
    let mid = rect.center().y;
    outcome_dot(
        painter,
        run.outcome(),
        pos2(rect.left() + 15.0, mid),
        5.5,
        t,
    );
    let dur = run
        .duration_secs(now)
        .map(format::duration)
        .unwrap_or_default();
    let started = run
        .started
        .or(run.created)
        .map(|s| format::relative(s, now))
        .unwrap_or_default();
    let right_text = [dur, started]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("  ·  ");
    let r = painter.text(
        pos2(rect.right() - 10.0, mid),
        Align2::RIGHT_CENTER,
        right_text,
        FontId::proportional(size::SMALL),
        p.text_2,
    );
    let x = rect.left() + 30.0;
    let title = if run.title.is_empty() {
        &run.name
    } else {
        &run.title
    };
    let g = truncated(
        painter,
        &format!("#{}  {title}", run.run_number),
        FontId::proportional(size::BODY - 0.5),
        p.text,
        (r.left() - x - 12.0).max(30.0),
    );
    let w = g.size().x;
    painter.galley(pos2(x, mid - g.size().y / 2.0), g, p.text);
    let cx = x + w + 8.0;
    if !caption.is_empty() && r.left() - cx > 40.0 {
        let g = truncated(
            painter,
            caption,
            FontId::proportional(size::SMALL),
            p.text_3,
            r.left() - cx - 12.0,
        );
        painter.galley(pos2(cx, mid - g.size().y / 2.0), g, p.text_3);
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// One bar per run (oldest on the left, newest on the right), height by
/// duration, colour by outcome, a dashed median line; hovering shows the
/// run, clicking opens it.
pub fn duration_chart(ui: &mut Ui, runs: &[&WorkflowRun], now: i64, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    if runs.is_empty() {
        kit::muted(ui, "No runs to chart.");
        return;
    }
    let axis_w = 58.0;
    let plot_h = 140.0;
    let label_h = 18.0;
    let width = ui.available_width().min(kit::DETAIL_MAX);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, plot_h + label_h + 6.0), Sense::click());
    let painter = ui.painter_at(rect.expand(2.0));
    let plot = Rect::from_min_max(
        pos2(rect.left() + axis_w, rect.top() + 4.0),
        pos2(rect.right() - 4.0, rect.top() + 4.0 + plot_h),
    );
    let durations: Vec<i64> = runs
        .iter()
        .map(|r| r.duration_secs(now).unwrap_or(0))
        .collect();
    let longest = durations.iter().copied().max().unwrap_or(1).max(1);
    // A few very long runs (a stuck job) would flatten every other bar:
    // scale to the bulk of the runs and clip the outliers.
    let p90 = hadion_core::devops::ci::history::percentile(&durations, 90.0).unwrap_or(longest);
    let scale = if p90 > 0 && longest > p90 * 4 {
        (p90 * 2).max(1)
    } else {
        longest
    };
    let clipped = durations.iter().filter(|d| **d > scale).count();
    let max = scale as f32;
    // Grid: zero, half, max.
    let mut grid = vec![
        (0.0f32, "0s".to_owned()),
        (1.0, format::duration(max as i64)),
    ];
    if max >= 4.0 {
        grid.push((0.5, format::duration((max * 0.5) as i64)));
    }
    for (frac, label) in grid {
        let y = plot.bottom() - plot.height() * frac;
        painter.hline(plot.x_range(), y, Stroke::new(1.0, p.border));
        painter.text(
            pos2(plot.left() - 8.0, y),
            Align2::RIGHT_CENTER,
            label,
            FontId::proportional(size::CAPTION),
            p.text_3,
        );
    }
    let n = runs.len();
    let slot = plot.width() / n as f32;
    let gap = if slot > 6.0 { 2.0 } else { 0.5 };
    let bar_w = (slot - gap).clamp(1.0, 16.0);
    let pointer = resp.hover_pos();
    let mut hovered: Option<usize> = None;
    for (i, (r, d)) in runs.iter().zip(&durations).enumerate() {
        let x0 = plot.left() + i as f32 * slot + (slot - bar_w) / 2.0;
        let over = *d > scale;
        let h = (*d as f32 / max * plot.height()).clamp(2.0, plot.height());
        let bar = Rect::from_min_max(pos2(x0, plot.bottom() - h), pos2(x0 + bar_w, plot.bottom()));
        let col_rect = Rect::from_min_max(
            pos2(plot.left() + i as f32 * slot, plot.top()),
            pos2(plot.left() + (i + 1) as f32 * slot, plot.bottom()),
        );
        let is_hover = pointer.is_some_and(|p| col_rect.contains(p));
        if is_hover {
            hovered = Some(i);
            painter.rect_filled(col_rect, CornerRadius::same(2), p.hover);
        }
        let c = outcome_color(&p, r.outcome());
        painter.rect_filled(
            bar,
            CornerRadius::same(if bar_w > 4.0 { 2 } else { 0 }),
            if is_hover { c } else { c.gamma_multiply(0.85) },
        );
        if over {
            // A break mark: the bar goes on beyond the scale.
            let y = plot.top() + 7.0;
            painter.line_segment(
                [pos2(x0 - 1.0, y + 2.0), pos2(x0 + bar_w + 1.0, y - 2.0)],
                Stroke::new(2.5, p.canvas),
            );
        }
    }
    // Median of finished runs.
    let finished: Vec<i64> = runs
        .iter()
        .zip(&durations)
        .filter(|(r, _)| !r.is_running())
        .map(|(_, d)| *d)
        .collect();
    if let Some(m) = hadion_core::devops::ci::history::median(&finished) {
        let y = plot.bottom() - m as f32 / max * plot.height();
        let mut x = plot.left();
        while x < plot.right() {
            painter.line_segment(
                [pos2(x, y), pos2((x + 5.0).min(plot.right()), y)],
                Stroke::new(1.0, p.text_2),
            );
            x += 9.0;
        }
        let g = painter.layout_no_wrap(
            format!("median {}", format::duration(m)),
            FontId::proportional(size::CAPTION),
            p.text_2,
        );
        let at = pos2(plot.right() - g.size().x - 6.0, y - g.size().y - 4.0);
        painter.rect_filled(
            Rect::from_min_size(at - vec2(4.0, 1.0), g.size() + vec2(8.0, 2.0)),
            CornerRadius::same(3),
            p.canvas.gamma_multiply(0.9),
        );
        painter.galley(at, g, p.text_2);
    }
    if clipped > 0 {
        painter.text(
            pos2(plot.center().x, plot.bottom() + 4.0 + label_h / 2.0),
            Align2::CENTER_CENTER,
            format!(
                "{} beyond the scale, the longest {}",
                format::plural(clipped, "run", "runs"),
                format::duration(longest)
            ),
            FontId::proportional(size::CAPTION),
            p.text_2,
        );
    }
    // Dates of the first and last run.
    let date = |r: &WorkflowRun| {
        r.started
            .or(r.created)
            .map(format::absolute)
            .unwrap_or_default()
    };
    let ly = plot.bottom() + 4.0 + label_h / 2.0;
    painter.text(
        pos2(plot.left(), ly),
        Align2::LEFT_CENTER,
        date(runs[0]),
        FontId::proportional(size::CAPTION),
        p.text_3,
    );
    painter.text(
        pos2(plot.right(), ly),
        Align2::RIGHT_CENTER,
        format!("{}  (newest)", date(runs[n - 1])),
        FontId::proportional(size::CAPTION),
        p.text_3,
    );
    if let Some(i) = hovered {
        let r = runs[i];
        let mut tip = format!(
            "#{}  {}\n{}  ·  {}",
            r.run_number,
            if r.title.is_empty() {
                &r.name
            } else {
                &r.title
            },
            outcome_text(r.outcome()),
            format::duration(durations[i])
        );
        if let Some(q) = r.queue_secs() {
            tip.push_str(&format!("  ·  queued {}", format::duration(q)));
        }
        tip.push_str(&format!(
            "\n{}  ·  {}{}",
            if r.branch.is_empty() {
                "no branch"
            } else {
                &r.branch
            },
            r.event,
            if r.attempt > 1 {
                format!("  ·  attempt {}", r.attempt)
            } else {
                String::new()
            }
        ));
        if let Some(s) = r.started.or(r.created) {
            tip.push_str(&format!("\n{}", when(s, now)));
        }
        tip.push_str("\nClick to open the run");
        let clicked = resp.clicked();
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text_at_pointer(tip);
        if clicked {
            actions.push(Action::OpenRun {
                run: r.id,
                job: None,
            });
        }
    }
}

/// A small legend for outcome colours.
pub fn outcome_legend(ui: &mut Ui) {
    let p = *pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        for (label, c) in [
            ("Passed", p.success),
            ("Failed", p.danger),
            ("Running", p.warning),
            ("Cancelled or skipped", p.text_3),
        ] {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                ui.painter().rect_filled(r, CornerRadius::same(2), c);
                ui.label(RichText::new(label).size(size::SMALL).color(p.text_2));
            });
        }
    });
}

/// A thin usage bar with a label: `fraction` of the width filled.
pub fn meter(ui: &mut Ui, fraction: f32, label: &str, tone: Tone) {
    let p = *pal(ui);
    let w = ui.available_width().min(kit::PROSE);
    let (rect, _) = ui.allocate_exact_size(vec2(w, 22.0), Sense::hover());
    let track = Rect::from_min_size(pos2(rect.left(), rect.top() + 2.0), vec2(w, 6.0));
    ui.painter()
        .rect_filled(track, CornerRadius::same(3), p.raised);
    let c = match tone {
        Tone::Neutral => p.accent,
        t => t.color(&p),
    };
    ui.painter().rect_filled(
        Rect::from_min_size(track.min, vec2(w * fraction.clamp(0.0, 1.0), 6.0)),
        CornerRadius::same(3),
        c,
    );
    ui.painter().text(
        pos2(rect.left(), track.bottom() + 8.0),
        Align2::LEFT_CENTER,
        label,
        FontId::new(size::SMALL, medium()),
        p.text_2,
    );
    ui.add_space(4.0);
}
