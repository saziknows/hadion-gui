//! Pieces of the Runs tab (`views/github.rs`) that belong to CI control:
//! the bulk bar for picked runs, the run and job menu entries (re-runs
//! with debug logging, force cancel, delete logs, delete run), and the
//! run detail's artifacts section.

use std::collections::BTreeSet;

use egui::{RichText, Ui, vec2};

use gitvis_core::devops::ci::model::Artifact;
use gitvis_core::github::{Job, Outcome, WorkflowRun};

use super::widgets::{self, bytes};
use super::{Act, EXPIRES_SOON, Load, PickMode, State};
use crate::format;
use crate::state::Action;
use crate::theme::{pal, space};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit;

/// Pick a run from a click in the list: ctrl toggles, shift extends.
/// Returns true when the click was a pick (not an open).
pub fn click_pick(ui: &Ui, run: u64, actions: &mut Vec<Action>) -> bool {
    let m = ui.input(|i| i.modifiers);
    if m.shift {
        actions.push(
            Act::Pick {
                run,
                mode: PickMode::Range,
            }
            .into(),
        );
        true
    } else if m.command || m.ctrl {
        actions.push(
            Act::Pick {
                run,
                mode: PickMode::Toggle,
            }
            .into(),
        );
        true
    } else {
        false
    }
}

/// The bar above the list while runs are picked: counts and the bulk
/// actions that apply.
pub fn bulk_bar(
    ui: &mut Ui,
    picked: &BTreeSet<u64>,
    runs: &[WorkflowRun],
    actions: &mut Vec<Action>,
) {
    if picked.is_empty() {
        return;
    }
    let p = *pal(ui);
    let chosen: Vec<&WorkflowRun> = runs.iter().filter(|r| picked.contains(&r.id)).collect();
    let running: Vec<u64> = chosen
        .iter()
        .filter(|r| r.is_running())
        .map(|r| r.id)
        .collect();
    let failed: Vec<u64> = chosen
        .iter()
        .filter(|r| !r.is_running() && (r.is_failure() || r.outcome() == Outcome::Cancelled))
        .map(|r| r.id)
        .collect();
    egui::Frame::NONE
        .fill(p.selection)
        .corner_radius(egui::CornerRadius::same(crate::theme::radius::M))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                ui.label(
                    RichText::new(format!("{} picked", format::count(picked.len())))
                        .family(crate::theme::semibold())
                        .color(p.text_strong),
                );
                if !running.is_empty()
                    && IconButton::new(Icon::Stop, &format!("Cancel {} running", running.len()))
                        .compact()
                        .danger()
                        .show(ui)
                        .clicked()
                {
                    actions.push(Act::AskCancelRuns(running.clone()).into());
                }
                if !failed.is_empty()
                    && IconButton::new(Icon::Rerun, &format!("Re-run {} failed", failed.len()))
                        .compact()
                        .show(ui)
                        .clicked()
                {
                    actions.push(Act::AskRerunFailedRuns(failed.clone()).into());
                }
                if IconButton::text("Clear selection")
                    .compact()
                    .show(ui)
                    .clicked()
                {
                    actions.push(Act::ClearPicks.into());
                }
            });
            ui.label(
                RichText::new("Ctrl-click to pick runs, Shift-click to pick a range")
                    .size(crate::theme::size::SMALL)
                    .color(p.text_2),
            );
        });
    ui.add_space(space::S);
}

/// Extra entries of a run's context menu.
pub fn run_menu(
    ui: &mut Ui,
    run: &WorkflowRun,
    runs: &[WorkflowRun],
    busy: bool,
    picked: bool,
    actions: &mut Vec<Action>,
) {
    let danger = pal(ui).danger;
    let id = run.id;
    let mut item = |ui: &mut Ui, label: &str, enabled: bool, act: Act| {
        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
            actions.push(act.into());
            ui.close();
        }
    };
    if !run.is_running() {
        item(
            ui,
            "Re-run all jobs…",
            !busy,
            Act::AskRerun {
                run: id,
                failed_only: false,
                debug: false,
            },
        );
        item(
            ui,
            "Re-run all jobs with debug logging…",
            !busy,
            Act::AskRerun {
                run: id,
                failed_only: false,
                debug: true,
            },
        );
        if run.is_failure() || run.outcome() == Outcome::Cancelled {
            item(
                ui,
                "Re-run failed jobs…",
                !busy,
                Act::AskRerun {
                    run: id,
                    failed_only: true,
                    debug: false,
                },
            );
            item(
                ui,
                "Re-run failed jobs with debug logging…",
                !busy,
                Act::AskRerun {
                    run: id,
                    failed_only: true,
                    debug: true,
                },
            );
        }
        if run.is_failure() {
            ui.separator();
            item(
                ui,
                "Compare with the last passing run",
                true,
                Act::CompareOpen(id),
            );
        }
    } else {
        if ui
            .add_enabled(
                !busy,
                egui::Button::new(RichText::new("Cancel run…").color(danger)),
            )
            .clicked()
        {
            actions.push(
                Act::AskCancel {
                    run: id,
                    force: false,
                }
                .into(),
            );
            ui.close();
        }
        if ui
            .add_enabled(
                !busy,
                egui::Button::new(RichText::new("Force cancel…").color(danger)),
            )
            .clicked()
        {
            actions.push(
                Act::AskCancel {
                    run: id,
                    force: true,
                }
                .into(),
            );
            ui.close();
        }
    }
    let on_branch = runs
        .iter()
        .filter(|r| r.branch == run.branch && r.is_running())
        .count();
    if !run.branch.is_empty() && on_branch > 0 {
        let label = format!("Cancel everything running on {} ({on_branch})…", run.branch);
        if ui
            .add(egui::Button::new(RichText::new(label).color(danger)))
            .clicked()
        {
            actions.push(Act::AskCancelBranch(run.branch.clone()).into());
            ui.close();
        }
    }
    ui.separator();
    if ui
        .button(if picked {
            "Unpick"
        } else {
            "Pick for a bulk action"
        })
        .clicked()
    {
        actions.push(
            Act::Pick {
                run: id,
                mode: PickMode::Toggle,
            }
            .into(),
        );
        ui.close();
    }
    if !run.is_running() {
        ui.separator();
        if ui
            .add_enabled(
                !busy,
                egui::Button::new(RichText::new("Delete logs…").color(danger)),
            )
            .clicked()
        {
            actions.push(Act::AskDeleteLogs(id).into());
            ui.close();
        }
        if ui
            .add_enabled(
                !busy,
                egui::Button::new(RichText::new("Delete run…").color(danger)),
            )
            .clicked()
        {
            actions.push(Act::AskDeleteRun(id).into());
            ui.close();
        }
    }
}

/// The "More" menu of the run header (what the header buttons do not
/// show).
pub fn more_menu(ui: &mut Ui, run: &WorkflowRun, busy: bool, actions: &mut Vec<Action>) {
    ui.set_min_width(260.0);
    let danger = pal(ui).danger;
    let id = run.id;
    if !run.is_running() {
        for (label, failed_only, debug) in [
            ("Re-run all jobs with debug logging…", false, true),
            ("Re-run failed jobs with debug logging…", true, true),
        ] {
            if failed_only && !(run.is_failure() || run.outcome() == Outcome::Cancelled) {
                continue;
            }
            if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
                actions.push(
                    Act::AskRerun {
                        run: id,
                        failed_only,
                        debug,
                    }
                    .into(),
                );
                ui.close();
            }
        }
        ui.separator();
        if ui
            .add_enabled(
                !busy,
                egui::Button::new(RichText::new("Delete logs…").color(danger)),
            )
            .clicked()
        {
            actions.push(Act::AskDeleteLogs(id).into());
            ui.close();
        }
        if ui
            .add_enabled(
                !busy,
                egui::Button::new(RichText::new("Delete run…").color(danger)),
            )
            .clicked()
        {
            actions.push(Act::AskDeleteRun(id).into());
            ui.close();
        }
    } else if ui
        .add_enabled(
            !busy,
            egui::Button::new(RichText::new("Force cancel…").color(danger)),
        )
        .clicked()
    {
        actions.push(
            Act::AskCancel {
                run: id,
                force: true,
            }
            .into(),
        );
        ui.close();
    }
}

/// Extra entries of a job's context menu.
pub fn job_menu(ui: &mut Ui, run: &WorkflowRun, job: &Job, busy: bool, actions: &mut Vec<Action>) {
    if run.is_running() {
        return;
    }
    for (label, debug) in [
        ("Re-run this job…", false),
        ("Re-run this job with debug logging…", true),
    ] {
        if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
            actions.push(
                Act::AskRerunJob {
                    run: run.id,
                    job: job.id,
                    name: job.name.clone(),
                    debug,
                }
                .into(),
            );
            ui.close();
        }
    }
    ui.separator();
}

/// A job's re-run button text while a re-run of it is in flight.
pub fn job_busy(ci: &State, job: u64) -> bool {
    ci.busy_jobs.contains(&job)
}

/// The run detail's Artifacts section.
pub fn artifacts_section(
    ui: &mut Ui,
    run: &WorkflowRun,
    load: Option<&Load<Vec<Artifact>>>,
    downloads: &std::collections::HashMap<u64, u64>,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let n = load.and_then(|l| l.ok()).map(Vec::len);
    kit::section_with(ui, "Artifacts", n.filter(|n| *n > 0), |ui| {
        if load.is_some_and(|l| l.loading) {
            ui.add(egui::Spinner::new().size(12.0));
        } else if IconButton::icon(Icon::Fetch)
            .compact()
            .show(ui)
            .on_hover_text("Reload this run's artifacts")
            .clicked()
        {
            actions.push(
                Act::LoadRunArtifacts {
                    run: run.id,
                    force: true,
                }
                .into(),
            );
        }
    });
    let Some(load) = load else {
        kit::muted(ui, "Not loaded.");
        return;
    };
    let retry = Act::LoadRunArtifacts {
        run: run.id,
        force: true,
    }
    .into();
    let Some(list) = widgets::loaded(ui, load, "Loading artifacts…", retry, actions) else {
        return;
    };
    if list.is_empty() {
        kit::muted(
            ui,
            if run.is_running() {
                "None yet: artifacts appear once a job uploads them."
            } else {
                "This run uploaded no artifacts."
            },
        );
        return;
    }
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for a in list {
            let (rect, resp) =
                ui.allocate_exact_size(vec2(ui.available_width(), 30.0), egui::Sense::hover());
            kit::row_background(ui, rect, ui.rect_contains_pointer(rect), false);
            let painter = ui.painter();
            let mid = rect.center().y;
            crate::views::icons::paint(
                painter,
                Icon::Package,
                egui::pos2(rect.left() + 15.0, mid),
                13.0,
                p.text_3,
            );
            let mut right = vec![bytes(a.size_in_bytes)];
            if a.expired {
                right.push("expired".to_owned());
            } else if let Some(e) = a.expires() {
                right.push(if a.expires_within(now, EXPIRES_SOON) {
                    format!("expires in {}", format::duration(e - now))
                } else {
                    format!("expires {}", format::absolute(e))
                });
            }
            let soon = a.expires_within(now, EXPIRES_SOON);
            let btn_w = if a.expired { 0.0 } else { 116.0 };
            let rr = painter.text(
                egui::pos2(rect.right() - 10.0 - btn_w, mid),
                egui::Align2::RIGHT_CENTER,
                right.join("  ·  "),
                egui::FontId::proportional(crate::theme::size::SMALL),
                if soon { p.warning } else { p.text_2 },
            );
            let g = crate::views::truncated(
                painter,
                &a.name,
                egui::FontId::proportional(crate::theme::size::BODY - 0.5),
                p.text,
                (rr.left() - rect.left() - 46.0).max(40.0),
            );
            painter.galley(
                egui::pos2(rect.left() + 32.0, mid - g.size().y / 2.0),
                g,
                p.text,
            );
            if !a.expired {
                let br = egui::Rect::from_min_max(
                    egui::pos2(rect.right() - 6.0 - btn_w, rect.top()),
                    egui::pos2(rect.right() - 6.0, rect.bottom()),
                );
                let downloading = downloads.contains_key(&a.id);
                let b = kit::overlay(
                    ui,
                    br,
                    IconButton::new(
                        Icon::Download,
                        if downloading {
                            "Again…"
                        } else {
                            "Download…"
                        },
                    )
                    .compact(),
                );
                if b.on_hover_text("Pick a folder, then gh run download")
                    .clicked()
                {
                    actions.push(
                        Act::DownloadArtifact {
                            id: a.id,
                            dir: None,
                        }
                        .into(),
                    );
                }
            }
            let _ = resp;
        }
    });
    let total: u64 = list.iter().map(|a| a.size_in_bytes).sum();
    widgets::note(
        ui,
        &format!(
            "{}, {} in total. The Artifacts tab lists every artifact of the repository.",
            format::plural(list.len(), "artifact", "artifacts"),
            bytes(total)
        ),
    );
}
