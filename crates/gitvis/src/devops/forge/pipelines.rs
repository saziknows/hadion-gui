//! The Actions page on GitLab and Azure DevOps: pipelines → jobs (grouped
//! by stage, or the Azure stage / job / task tree) → the job log; retry,
//! cancel and run with confirmations.

use egui::{Align2, FontId, Frame, Margin, Rect, RichText, Sense, Ui, pos2, vec2};

use gitvis_core::devops::forge::azure::Target;
use gitvis_core::devops::forge::model::{Job, Pipeline};
use gitvis_core::devops::forge::{ForgeKind, ForgeRemote};
use gitvis_core::github::Outcome;

use super::review::{detail_pane, list_frame};
use super::{Act, RunForm, State, VarRow, current_branch, gate, log};
use crate::format;
use crate::state::{Action, RepoState};
use crate::theme::{Tone, medium, pal, size, space};
use crate::views::form;
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Leading, Row};
use crate::views::truncated;

const JOB_H: f32 = 30.0;

pub fn tone(o: Outcome) -> Tone {
    if o.is_bad() {
        Tone::Danger
    } else if o.is_running() {
        Tone::Accent
    } else if o.is_good() {
        Tone::Success
    } else if o == Outcome::Neutral {
        Tone::Warning
    } else {
        Tone::Neutral
    }
}

fn visible(st: &State) -> Vec<&Pipeline> {
    let t = st.pipeline_filter.trim().to_lowercase();
    st.pipelines
        .data
        .as_ref()
        .map(|v| {
            v.iter()
                .filter(|p| {
                    t.is_empty()
                        || p.reference.to_lowercase().contains(&t)
                        || p.name.to_lowercase().contains(&t)
                        || p.status.to_lowercase().contains(&t)
                        || p.sha.starts_with(&t)
                        || p.id.to_string() == t
                        || p.user.to_lowercase().contains(&t)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Ids of the listed pipelines in display order (script `pipeline N`).
pub fn visible_ids(st: &State) -> Vec<u64> {
    visible(st).iter().map(|p| p.id).collect()
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) {
    let Some(remote) = repo.forge_remote.clone() else {
        return;
    };
    if !gate::show(ui, &remote, &repo.devops.forge, "Pipelines", actions) {
        return;
    }
    let branch = current_branch(repo);
    header(ui, &remote, &repo.devops.forge, actions);
    let st = &mut repo.devops.forge;
    if let Some(form) = st.run_form.as_mut() {
        run_form(ui, &remote, form, actions);
        return;
    }
    list_pane(ui, &remote, st, branch.as_deref(), actions);
    let Some(id) = st.selected_pipeline else {
        let p = *pal(ui);
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(p.canvas).inner_margin(Margin::same(24)))
            .show(ui, |ui| {
                kit::empty_state(
                    ui,
                    Icon::Play,
                    "Select a pipeline",
                    "Its jobs, durations and full logs appear here.",
                    None,
                );
            });
        return;
    };
    let listed = st
        .pipelines
        .data
        .as_ref()
        .and_then(|v| v.iter().find(|p| p.id == id).cloned());
    let can_write = st.can_write();
    // Log panel at the bottom when a job is selected.
    if let Some((pid, key)) = st.job.filter(|(p, _)| *p == id) {
        let jobs: Vec<Job> = st
            .runs
            .get(&id)
            .and_then(|d| d.jobs.as_ref())
            .and_then(|r| r.as_ref().ok())
            .cloned()
            .unwrap_or_default();
        let job = jobs.iter().find(|j| j.log_id == Some(key));
        let title = job
            .map(|j| j.name.clone())
            .unwrap_or_else(|| format!("Job {key}"));
        let web = job.map(|j| j.web_url.clone()).unwrap_or_default();
        let p = *pal(ui);
        let fill = if ui.visuals().dark_mode {
            p.chrome
        } else {
            p.canvas
        };
        let avail = ui.available_height();
        let State {
            logs, log_filter, ..
        } = st;
        egui::Panel::bottom("forge_log_view")
            .resizable(true)
            .default_size((avail * 0.55).max(220.0))
            .size_range(140.0..=(avail - 120.0).max(160.0))
            .frame(Frame::NONE.fill(fill))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.set_min_height(ui.available_height());
                log::viewer(
                    ui,
                    logs.get_mut(&(pid, key)),
                    pid,
                    key,
                    &title,
                    &web,
                    log_filter,
                    actions,
                );
            });
    }
    let st = &mut repo.devops.forge;
    let detail = st.runs.entry(id).or_default();
    let selected_job = st.job.filter(|(p, _)| *p == id).map(|(_, k)| k);
    detail_pane(ui, ("forge_pipeline", id), |ui| {
        if let Some(Err(e)) = &detail.pipeline {
            let mut retry = false;
            kit::callout_with(ui, Tone::Danger, "Could not load the pipeline", e, |ui| {
                retry = IconButton::new(Icon::Fetch, "Retry")
                    .compact()
                    .show(ui)
                    .clicked();
            });
            if retry {
                actions.push(Act::RefreshPipeline(id).into());
            }
        }
        let pipeline = detail
            .pipeline
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .cloned()
            .or(listed);
        let Some(pl) = pipeline else {
            kit::loading(ui, &format!("Loading pipeline {id}…"));
            return;
        };
        pipeline_header(
            ui,
            &remote,
            &pl,
            detail.loading,
            detail.at,
            can_write,
            actions,
        );
        kit::section(
            ui,
            match remote.kind {
                ForgeKind::GitLab => "Jobs",
                ForgeKind::Azure => "Stages, jobs and tasks",
            },
            detail
                .jobs
                .as_ref()
                .and_then(|r| r.as_ref().ok())
                .map(|j| j.len()),
        );
        match &detail.jobs {
            None if detail.loading => kit::loading(ui, "Loading jobs…"),
            None => {
                kit::muted(ui, "Not loaded.");
            }
            Some(Err(e)) => {
                let mut retry = false;
                kit::callout_with(ui, Tone::Danger, "Could not load the jobs", e, |ui| {
                    retry = IconButton::new(Icon::Fetch, "Retry")
                        .compact()
                        .show(ui)
                        .clicked();
                });
                if retry {
                    actions.push(Act::RefreshPipeline(id).into());
                }
            }
            Some(Ok(jobs)) if jobs.is_empty() => {
                kit::muted(ui, "This pipeline has no jobs.");
            }
            Some(Ok(jobs)) => {
                kit::secondary(ui, "Click a job to read its log.");
                jobs_list(ui, &remote, &pl, jobs, selected_job, can_write, actions);
            }
        }
    });
    if pipeline_running(&repo.devops.forge, id) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(500));
    }
}

fn pipeline_running(st: &State, id: u64) -> bool {
    st.runs
        .get(&id)
        .and_then(|d| d.pipeline.as_ref())
        .and_then(|r| r.as_ref().ok())
        .is_some_and(|p| p.outcome.is_running())
}

fn header(ui: &mut Ui, remote: &ForgeRemote, st: &State, actions: &mut Vec<Action>) {
    let now = format::now_secs();
    let (mode, badge) = gate::mode_line(remote, st);
    let mut sub = vec![format!("{} on {}", remote.path, remote.host)];
    if let Some(v) = &st.pipelines.data {
        sub.push(format::plural(v.len(), "pipeline", "pipelines"));
    }
    if st.pipelines.loading {
        sub.push("loading…".into());
    } else if let Some(at) = st.pipelines.at {
        sub.push(format!("fetched {}", format::relative(at, now)));
    }
    if !mode.is_empty() {
        sub.push(mode);
    }
    let can_write = st.can_write();
    kit::page_header(ui, "Pipelines", &sub.join("  ·  "), |ui| {
        if IconButton::new(Icon::Play, "Run pipeline")
            .primary(true)
            .enabled(can_write)
            .show(ui)
            .clicked()
        {
            actions.push(Act::NewRun.into());
        }
        if IconButton::icon(Icon::External)
            .show(ui)
            .on_hover_text("Open the pipelines on the web")
            .clicked()
        {
            let url = match remote.kind {
                ForgeKind::GitLab => format!("{}/-/pipelines", remote.web_url),
                ForgeKind::Azure => Target::of(remote)
                    .map(|t| t.pipelines_url())
                    .unwrap_or_else(|| remote.web_url.clone()),
            };
            actions.push(Action::OpenUrl(url));
        }
        if st.pipelines.loading {
            ui.add(egui::Spinner::new().size(14.0));
        } else if IconButton::new(Icon::Fetch, "Refresh").show(ui).clicked() {
            actions.push(Act::RefreshPipelines.into());
        }
        if let Some((text, tone)) = &badge {
            ui.add_space(6.0);
            kit::badge(ui, text, *tone);
            if IconButton::text("Leave public mode")
                .compact()
                .show(ui)
                .clicked()
            {
                actions.push(Act::UsePublic(false).into());
            }
        }
    });
}

fn list_pane(
    ui: &mut Ui,
    remote: &ForgeRemote,
    st: &mut State,
    branch: Option<&str>,
    actions: &mut Vec<Action>,
) {
    list_frame(ui, "forge_pipelines", |ui| {
        let total = st.pipelines.data.as_ref().map(|v| v.len());
        kit::list_header(ui, "Pipelines", total, |_| {});
        ui.add_space(space::XS);
        ui.horizontal(|ui| {
            ui.set_max_width(ui.available_width() - 8.0);
            kit::search_box(
                ui,
                "forge_pipeline_filter",
                &mut st.pipeline_filter,
                "Filter by branch, status, commit, user",
            );
        });
        ui.add_space(space::XS);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if kit::chip(ui, "All branches", !st.branch_only).clicked() && st.branch_only {
                actions.push(Act::SetBranchOnly(false).into());
            }
            if let Some(b) = branch
                && kit::chip(ui, &format!("Only {b}"), st.branch_only).clicked()
                && !st.branch_only
            {
                actions.push(Act::SetBranchOnly(true).into());
            }
        });
        ui.add_space(space::S);
        if let Some(e) = &st.pipelines.error {
            let mut retry = false;
            kit::callout_with(ui, Tone::Danger, "Could not load the pipelines", e, |ui| {
                retry = IconButton::new(Icon::Fetch, "Retry")
                    .compact()
                    .show(ui)
                    .clicked();
            });
            if retry {
                actions.push(Act::RefreshPipelines.into());
            }
        }
        let Some(all) = &st.pipelines.data else {
            if st.pipelines.loading {
                kit::loading(ui, "Loading pipelines…");
            } else if st.pipelines.error.is_none() {
                kit::muted(ui, "Not loaded yet.");
            }
            return;
        };
        if all.is_empty() {
            kit::muted(
                ui,
                &if st.branch_only {
                    format!(
                        "No pipelines ran on {} yet.",
                        branch.unwrap_or("this branch")
                    )
                } else {
                    format!("No pipelines ran in {} yet.", remote.path)
                },
            );
            return;
        }
        let rows = visible(st);
        if rows.is_empty() {
            kit::muted(ui, "Nothing matches the filter.");
            return;
        }
        let now = format::now_secs();
        let selected = st.selected_pipeline;
        let ids: Vec<u64> = rows.iter().map(|p| p.id).collect();
        kit::list(
            ui,
            "forge_pipeline_list",
            rows.len(),
            kit::ROW_H,
            |i| selected == Some(ids[i]),
            |ui, rect, resp, i| {
                let p = *pal(ui);
                let pl = rows[i];
                let title = if pl.name.is_empty() {
                    pl.reference.clone()
                } else {
                    format!("{}  ·  {}", pl.name, pl.reference)
                };
                let mut sub = vec![format!(
                    "#{}",
                    if pl.number.is_empty() {
                        pl.id.to_string()
                    } else {
                        pl.number.clone()
                    }
                )];
                if !pl.sha.is_empty() {
                    sub.push(pl.sha.chars().take(8).collect());
                }
                if !pl.source.is_empty() {
                    sub.push(pl.source.clone());
                }
                if let Some(d) = pl.duration {
                    sub.push(format::duration(d));
                }
                if !pl.user.is_empty() {
                    sub.push(pl.user.clone());
                }
                let sub = sub.join("  ·  ");
                let badges = vec![(pl.status.clone(), tone(pl.outcome))];
                let meta = pl
                    .created
                    .map(|t| format::relative(t, now))
                    .unwrap_or_default();
                kit::paint_row(
                    ui,
                    rect,
                    &Row::new(&title)
                        .leading(Leading::Dot(tone(pl.outcome).color(&p)))
                        .subtitle(&sub)
                        .meta(&meta)
                        .badges(&badges),
                    selected == Some(pl.id),
                );
                if resp.clicked() {
                    actions.push(Act::SelectPipeline(pl.id).into());
                }
            },
        );
    });
}

#[allow(clippy::too_many_arguments)]
fn pipeline_header(
    ui: &mut Ui,
    remote: &ForgeRemote,
    pl: &Pipeline,
    loading: bool,
    at: Option<i64>,
    can_write: bool,
    actions: &mut Vec<Action>,
) {
    let now = format::now_secs();
    let title = if pl.name.is_empty() {
        format!("Pipeline #{}", pl.id)
    } else {
        format!("{} #{}", pl.name, pl.number)
    };
    kit::title(ui, &title);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        kit::badge(ui, &pl.status, tone(pl.outcome));
        kit::badge(ui, &pl.reference, Tone::Neutral);
        if loading {
            ui.add(egui::Spinner::new().size(12.0));
        } else if let Some(at) = at {
            kit::muted(ui, &format!("loaded {}", format::relative(at, now)));
        }
    });
    kit::kv(ui, 130.0, |kv| {
        if !pl.sha.is_empty() {
            kv.text(
                "Commit",
                kit::mono(pl.sha.chars().take(12).collect::<String>()),
            );
        }
        if !pl.source.is_empty() {
            kv.text(
                match remote.kind {
                    ForgeKind::GitLab => "Source",
                    ForgeKind::Azure => "Reason",
                },
                pl.source.clone(),
            );
        }
        if !pl.user.is_empty() {
            kv.text("Started by", pl.user.clone());
        }
        if let Some(t) = pl.created {
            kv.text(
                "Created",
                format!("{} ({})", format::relative(t, now), format::absolute(t)),
            );
        }
        if let Some(t) = pl.started {
            kv.text("Started", format::absolute(t));
        }
        if let Some(t) = pl.finished {
            kv.text("Finished", format::absolute(t));
        }
        let mut timing = Vec::new();
        if let Some(d) = pl.duration {
            timing.push(format!("ran {}", format::duration(d)));
        } else if pl.outcome.is_running()
            && let Some(s) = pl.started
        {
            timing.push(format!("running for {}", format::duration(now - s)));
        }
        if let Some(q) = pl.queued {
            timing.push(format!("queued {}", format::duration(q)));
        }
        if !timing.is_empty() {
            kv.text("Duration", timing.join(", "));
        }
        kv.text("Id", kit::mono(pl.id.to_string()));
    });
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if can_write {
            match remote.kind {
                ForgeKind::GitLab => {
                    if (pl.outcome.is_bad() || pl.outcome == Outcome::Cancelled)
                        && IconButton::new(Icon::Rerun, "Retry failed jobs…")
                            .show(ui)
                            .clicked()
                    {
                        actions.push(Act::RetryPipeline(pl.id).into());
                    }
                }
                ForgeKind::Azure => {
                    if !pl.outcome.is_running()
                        && IconButton::new(Icon::Rerun, "Run again…")
                            .show(ui)
                            .clicked()
                    {
                        actions.push(
                            Act::RunAgain {
                                definition: pl.name.clone(),
                                reference: pl.reference.clone(),
                            }
                            .into(),
                        );
                    }
                }
            }
            if pl.outcome.is_running() && IconButton::new(Icon::Stop, "Cancel…").show(ui).clicked()
            {
                actions.push(Act::CancelPipeline(pl.id).into());
            }
        }
        if !pl.web_url.is_empty()
            && IconButton::new(Icon::External, "Open on the web")
                .show(ui)
                .clicked()
        {
            actions.push(Action::OpenUrl(pl.web_url.clone()));
        }
        if IconButton::new(Icon::Fetch, "Refresh")
            .enabled(!loading)
            .show(ui)
            .clicked()
        {
            actions.push(Act::RefreshPipeline(pl.id).into());
        }
    });
}

fn jobs_list(
    ui: &mut Ui,
    remote: &ForgeRemote,
    pl: &Pipeline,
    jobs: &[Job],
    selected: Option<u64>,
    can_write: bool,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let now = format::now_secs();
    let gitlab = remote.kind == ForgeKind::GitLab;
    let mut last_stage: Option<&str> = None;
    for j in jobs {
        if gitlab && last_stage != Some(j.stage.as_str()) {
            last_stage = Some(j.stage.as_str());
            let in_stage: Vec<&Job> = jobs.iter().filter(|x| x.stage == j.stage).collect();
            let agg = gitvis_core::github::aggregate(in_stage.iter().map(|x| x.outcome));
            // The forge's own word for the stage: the status of its worst job.
            let word = in_stage
                .iter()
                .find(|x| x.outcome == agg)
                .map(|x| x.status.clone())
                .unwrap_or_else(|| agg.label().to_owned());
            ui.add_space(space::S);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.label(kit::caption_text(&p, &j.stage));
                ui.label(
                    RichText::new(format::plural(in_stage.len(), "job", "jobs"))
                        .size(size::CAPTION)
                        .color(p.text_3),
                );
                kit::badge(ui, &word, tone(agg));
            });
        }
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), JOB_H), Sense::click());
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let hovered = ui.rect_contains_pointer(rect);
        let on = j.log_id.is_some() && selected == j.log_id;
        kit::row_background(ui, rect, hovered && j.log_id.is_some(), on);
        let painter = ui.painter().clone();
        let mid = rect.center().y;
        let indent = if gitlab { 0.0 } else { j.depth as f32 * 18.0 };
        let x0 = rect.left() + 12.0 + indent;
        painter.circle_filled(pos2(x0 + 4.0, mid), 4.5, tone(j.outcome).color(&p));
        // Right side: status, duration.
        let mut right = rect.right() - 10.0;
        let mut facts = vec![j.status.clone()];
        if let Some(d) = j.duration {
            facts.push(format::duration(d));
        } else if j.outcome.is_running()
            && let Some(s) = j.started
        {
            facts.push(format!("{} so far", format::duration(now - s)));
        }
        let r = painter.text(
            pos2(right, mid),
            Align2::RIGHT_CENTER,
            facts.join("  ·  "),
            FontId::proportional(size::SMALL),
            p.text_2,
        );
        right = r.left() - 10.0;
        // Badges before the facts.
        let dark = ui.visuals().dark_mode;
        if j.allow_failure && j.outcome.is_bad() {
            right = kit::paint_badge(
                &painter,
                &p,
                dark,
                right,
                mid,
                "Allowed to fail",
                Tone::Neutral,
            ) - 6.0;
        }
        if j.errors > 0 {
            right = kit::paint_badge(
                &painter,
                &p,
                dark,
                right,
                mid,
                &format::plural(j.errors as usize, "error", "errors"),
                Tone::Danger,
            ) - 6.0;
        }
        if j.warnings > 0 {
            right = kit::paint_badge(
                &painter,
                &p,
                dark,
                right,
                mid,
                &format::plural(j.warnings as usize, "warning", "warnings"),
                Tone::Warning,
            ) - 6.0;
        }
        if hovered && can_write && gitlab {
            right -= 96.0;
        }
        let name_font = if !gitlab && j.depth == 0 {
            FontId::new(size::BODY, medium())
        } else {
            FontId::proportional(size::BODY - 0.5)
        };
        let g = truncated(
            &painter,
            &j.name,
            name_font,
            p.text,
            (right - x0 - 18.0).max(40.0),
        );
        let w = g.size().x;
        painter.galley(pos2(x0 + 16.0, mid - g.size().y / 2.0), g, p.text);
        let mut extra = Vec::new();
        if !j.failure_reason.is_empty() {
            extra.push(j.failure_reason.clone());
        }
        if !j.runner.is_empty() {
            extra.push(j.runner.clone());
        }
        if !gitlab && j.depth == 0 && !j.stage.is_empty() {
            extra.push(j.stage.to_lowercase());
        }
        let ex = x0 + 16.0 + w + 10.0;
        if !extra.is_empty() && right - ex > 40.0 {
            let g = truncated(
                &painter,
                &extra.join("  ·  "),
                FontId::proportional(size::SMALL),
                p.text_3,
                right - ex,
            );
            painter.galley(pos2(ex, mid - g.size().y / 2.0), g, p.text_3);
        }
        // Hover actions (GitLab).
        let mut hit = false;
        if hovered && can_write && gitlab {
            let br =
                Rect::from_min_size(pos2(r.left() - 104.0, rect.top() + 3.0), vec2(90.0, 24.0));
            if j.outcome.is_running() {
                if kit::overlay(ui, br, IconButton::new(Icon::Stop, "Cancel").compact()).clicked() {
                    hit = true;
                    actions.push(
                        Act::CancelJob {
                            pipeline: pl.id,
                            job: j.id,
                        }
                        .into(),
                    );
                }
            } else if kit::overlay(ui, br, IconButton::new(Icon::Rerun, "Retry").compact())
                .clicked()
            {
                hit = true;
                actions.push(
                    Act::RetryJob {
                        pipeline: pl.id,
                        job: j.id,
                    }
                    .into(),
                );
            }
        }
        if hovered && j.log_id.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if resp.clicked()
            && !hit
            && let Some(key) = j.log_id
        {
            actions.push(
                Act::ShowLog {
                    pipeline: pl.id,
                    key,
                }
                .into(),
            );
        }
        // Azure issues under the row.
        for issue in j.issues.iter().take(3) {
            ui.horizontal(|ui| {
                ui.add_space(12.0 + indent + 16.0);
                ui.label(RichText::new(issue).size(size::SMALL).color(
                    if issue.starts_with("warning") {
                        p.warning
                    } else {
                        p.danger
                    },
                ));
            });
        }
    }
}

fn run_form(ui: &mut Ui, remote: &ForgeRemote, f: &mut RunForm, actions: &mut Vec<Action>) {
    let gitlab = remote.kind == ForgeKind::GitLab;
    egui::ScrollArea::vertical()
        .id_salt("forge_run_form")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            Frame::NONE.inner_margin(Margin::symmetric(28, 20)).show(ui, |ui| {
                ui.set_max_width(ui.available_width().min(kit::PROSE + 80.0));
                kit::title(ui, "Run a pipeline");
                kit::secondary(
                    ui,
                    &if gitlab {
                        format!("Starts a pipeline of {} on a branch or tag, as POST projects/:id/pipeline with the variables in a JSON body on standard input.", remote.path)
                    } else {
                        format!("Queues a run of an Azure Pipelines pipeline in {} with az pipelines run.", remote.project.as_deref().unwrap_or(""))
                    },
                );
                ui.add_space(space::S);
                if !gitlab {
                    form::field(ui, "Pipeline", Some("The pipeline's name, as Azure DevOps shows it."), |ui| {
                        form::text(ui, "forge_run_def", &mut f.definition, "frontend-ci");
                    });
                }
                form::field(ui, "Branch or tag", None, |ui| {
                    form::text(ui, "forge_run_ref", &mut f.reference, "main");
                });
                kit::section(ui, "Variables", Some(f.vars.iter().filter(|v| !v.key.trim().is_empty()).count()));
                if gitlab {
                    kit::secondary(ui, "Mark a value as secret to mask it in the confirmation and the activity log; it still goes to GitLab as a normal pipeline variable.");
                } else {
                    kit::callout(
                        ui,
                        Tone::Warning,
                        "Values go on the command line",
                        "az pipelines run has no way to read variables from standard input, so they are passed as arguments and appear in the activity log. Never put secrets here; use a secret pipeline variable in Azure DevOps instead.",
                    );
                }
                let mut remove: Option<usize> = None;
                for (i, v) in f.vars.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        form::input(ui, ("forge_var_key", i), &mut v.key, form::InputOpts::new("NAME").width(200.0).mono());
                        let mut opts = form::InputOpts::new("value").width(260.0).mono();
                        if v.secret {
                            opts = opts.password();
                        }
                        form::input(ui, ("forge_var_value", i), &mut v.value, opts);
                        if gitlab {
                            form::checkbox(ui, &mut v.secret, "Secret");
                        }
                        if IconButton::icon(Icon::Close).compact().show(ui).on_hover_text("Remove").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    f.vars.remove(i);
                }
                if IconButton::new(Icon::Plus, "Add a variable").compact().show(ui).clicked() {
                    f.vars.push(VarRow::default());
                }
                form::error(ui, &f.error);
                form::actions_row(ui, |ui| {
                    if form::primary(ui, "Run pipeline…", true).clicked() {
                        actions.push(Act::SubmitRun.into());
                    }
                    if form::button(ui, "Cancel", true).clicked() {
                        actions.push(Act::CloseRun.into());
                    }
                });
            });
        });
}
