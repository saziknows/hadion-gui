//! The console view: output of a streamed process (task runs, `act`,
//! `terraform plan`, `kubectl logs`, `git bisect run`, image scans) with
//! its command, status, duration and controls, plus a compact run list.

#![allow(dead_code)] // Used by the DevOps pages; removed at integration.
use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align, Align2, Color32, FontId, Frame, Layout, Margin, RichText, Sense, Ui, pos2, vec2,
};

use crate::devops::console::{Console, ConsoleAction, ConsoleLine, ConsoleRun};
use crate::format;
use crate::state::{Action, RepoId};
use crate::theme::{Tone, pal, size};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit;

const LINE_H: f32 = 18.0;
const MAX_CHARS: usize = 3_000;

/// Tone of a run's status.
pub fn status_tone(run: &ConsoleRun) -> Tone {
    if run.running() {
        Tone::Accent
    } else if run.succeeded() {
        Tone::Success
    } else if run.cancelled() {
        Tone::Neutral
    } else {
        Tone::Danger
    }
}

/// A run's header (title, status, duration, command, directory, buttons)
/// and its output filling the rest of `ui`. `id_salt` keeps the scroll
/// position per place it is shown.
pub fn output(ui: &mut Ui, run: &ConsoleRun, id_salt: &str, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    if run.running() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(200));
    }
    Frame::NONE
        .inner_margin(Margin {
            left: 16,
            right: 12,
            top: 10,
            bottom: 8,
        })
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(&run.title)
                                .size(size::TITLE)
                                .family(crate::theme::semibold())
                                .color(p.text_strong),
                        );
                        ui.add_space(4.0);
                        kit::badge(ui, &run.status_text(), status_tone(run));
                        if run.running() {
                            ui.add(egui::Spinner::new().size(12.0).color(p.accent));
                        }
                    });
                    let mut facts = vec![
                        format!("started {}", run.started_clock),
                        format::millis(run.elapsed_ms()),
                        format::plural(run.lines.len() + run.dropped, "line", "lines"),
                        format::bytes(run.bytes),
                    ];
                    if let Some(pid) = run.pid {
                        facts.push(format!("pid {pid}"));
                    }
                    if run.attempt > 1 {
                        facts.push(format!("attempt {}", run.attempt));
                    }
                    if run.dropped > 0 {
                        facts.push(format!(
                            "first {} dropped",
                            format::plural(run.dropped, "line", "lines")
                        ));
                    }
                    ui.label(
                        RichText::new(facts.join("  ·  "))
                            .size(size::SMALL)
                            .color(p.text_2),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if run.running() {
                        if IconButton::new(Icon::Stop, "Stop")
                            .show(ui)
                            .on_hover_text("Kill the process and everything it started")
                            .clicked()
                        {
                            actions.push(Action::Console(ConsoleAction::Stop(run.id)));
                        }
                    } else {
                        if IconButton::icon(Icon::Close)
                            .show(ui)
                            .on_hover_text("Remove this run")
                            .clicked()
                        {
                            actions.push(Action::Console(ConsoleAction::Remove(run.id)));
                        }
                        if IconButton::new(Icon::Rerun, "Run again")
                            .show(ui)
                            .on_hover_text(&run.command)
                            .clicked()
                        {
                            actions.push(Action::Console(ConsoleAction::Rerun(run.id)));
                        }
                    }
                    if IconButton::icon(Icon::Copy)
                        .show(ui)
                        .on_hover_text("Copy the output")
                        .clicked()
                    {
                        actions.push(Action::Console(ConsoleAction::Copy(run.id)));
                    }
                });
            });
            ui.add_space(6.0);
            // The command and where it ran, copyable.
            let cmd = format!("{}\n  in {}", run.command, run.spec.cwd.display());
            let resp = ui
                .add(
                    egui::Label::new(
                        RichText::new(&cmd)
                            .family(egui::FontFamily::Monospace)
                            .size(size::MONO - 0.5)
                            .color(p.text_2),
                    )
                    .sense(Sense::click())
                    .wrap(),
                )
                .on_hover_text("Click to copy the command")
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.clicked() {
                actions.push(Action::Copy(run.command.clone()));
            }
        });
    kit::hairline(ui);
    lines(ui, run, id_salt);
}

/// The output lines alone, virtualised, sticking to the bottom while new
/// lines arrive (scroll up to stop following).
pub fn lines(ui: &mut Ui, run: &ConsoleRun, id_salt: &str) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let fill = if dark { p.chrome } else { p.canvas };
    Frame::NONE.fill(fill).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.set_min_height(ui.available_height());
        if run.lines.is_empty() {
            Frame::NONE
                .inner_margin(Margin::symmetric(16, 12))
                .show(ui, |ui| {
                    kit::muted(
                        ui,
                        if run.running() {
                            "Waiting for output…"
                        } else {
                            "No output."
                        },
                    );
                });
            return;
        }
        let font = FontId::monospace(size::MONO - 0.5);
        egui::ScrollArea::both()
            .id_salt(("console_lines", id_salt, run.id))
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show_rows(ui, LINE_H, run.lines.len(), |ui, range| {
                for i in range {
                    let line = &run.lines[i];
                    let (rect, _) = ui.allocate_exact_size(
                        vec2(
                            (line.text.len().min(MAX_CHARS) as f32 * 7.6 + 80.0)
                                .max(ui.available_width()),
                            LINE_H,
                        ),
                        Sense::hover(),
                    );
                    let painter = ui.painter();
                    painter.text(
                        pos2(rect.left() + 44.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        (i + 1 + run.dropped).to_string(),
                        FontId::monospace(size::CAPTION),
                        p.text_3,
                    );
                    let base = if line.stderr && line.spans.is_empty() {
                        p.text_2
                    } else {
                        p.text
                    };
                    let g = galley(painter, line, base, &font, dark);
                    painter.galley(
                        pos2(rect.left() + 56.0, rect.center().y - g.size().y / 2.0),
                        g,
                        base,
                    );
                }
            });
    });
}

fn galley(
    p: &egui::Painter,
    line: &ConsoleLine,
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

/// A compact list of runs (newest first) for one repository, optionally
/// filtered. Returns the id of a clicked run.
pub fn run_list(
    ui: &mut Ui,
    console: &Console,
    repo: RepoId,
    selected: Option<u64>,
    keep: impl Fn(&ConsoleRun) -> bool,
) -> Option<u64> {
    let p = *pal(ui);
    let mut clicked = None;
    let runs: Vec<&ConsoleRun> = console.for_repo(repo).filter(|r| keep(r)).collect();
    if runs.is_empty() {
        kit::muted(ui, "Nothing has run yet.");
        return None;
    }
    for run in runs {
        let (rect, resp) = ui.allocate_exact_size(
            vec2(ui.available_width(), kit::ROW_H_ONE + 6.0),
            Sense::click(),
        );
        let on = selected == Some(run.id);
        kit::row_background(ui, rect, resp.hovered(), on);
        let painter = ui.painter();
        let mid = rect.center().y;
        let tone = status_tone(run);
        painter.circle_filled(pos2(rect.left() + 14.0, mid), 4.5, tone.color(&p));
        let right_text = if run.running() {
            format!("running {}", format::millis(run.elapsed_ms()))
        } else {
            format!(
                "{} · {}",
                run.status_text(),
                format::millis(run.elapsed_ms())
            )
        };
        let r = painter.text(
            pos2(rect.right() - 10.0, mid),
            Align2::RIGHT_CENTER,
            right_text,
            FontId::proportional(size::SMALL),
            p.text_2,
        );
        let g = crate::views::truncated(
            painter,
            &run.title,
            FontId::proportional(size::BODY - 0.5),
            p.text,
            (r.left() - rect.left() - 40.0).max(30.0),
        );
        painter.galley(pos2(rect.left() + 28.0, mid - g.size().y / 2.0), g, p.text);
        if resp
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            clicked = Some(run.id);
        }
    }
    clicked
}
