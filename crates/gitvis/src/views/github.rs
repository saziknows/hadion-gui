//! Actions page: workflow runs with their jobs, steps, annotations and full
//! logs; pull requests with their numbers, reviews and checks. Everything
//! comes through `gh api`. Also the commit checks section shared with the
//! details panel ([`checks_section`]).

use std::collections::HashMap;
use std::hash::Hash;
use std::time::Duration;

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Label, Layout, Margin, Rect, Response,
    RichText, Sense, Stroke, Ui, pos2, vec2,
};

use gitvis_core::github::{
    Annotation, CheckRun, GitHubData, Job, Outcome, PullRequest, RateLimit, WorkflowRun,
};
use gitvis_core::joblog::{LogKind, LogLine};
use gitvis_core::ops::GitOp;
use gitvis_core::{Oid, Snapshot};

use crate::format;
use crate::state::{
    Action, ChecksState, Dialog, GitHubPage, GitHubTab, LogState, RepoState, RunDetail, RunFilter,
    Settings,
};
use crate::theme::{Palette, Tone, medium, pal, semibold, size, space};
use crate::views::icons::{self, Icon, IconButton, outcome_dot};
use crate::views::kit;
use crate::views::truncated;

const JOB_ROW_H: f32 = 36.0;
const STEP_ROW_H: f32 = 26.0;
/// One-line rows: checks, reviews, runs on a PR's head.
const LINE_ROW_H: f32 = 28.0;
const LOG_ROW_H: f32 = 19.0;
/// Log lines wider than this are clipped rather than scrolled forever.
const LOG_MAX_CHARS: usize = 4_000;
/// Where a job's steps and annotations start, under the job name.
const STEP_INDENT: f32 = 46.0;

/// Runs that pass the page's status chip and text filter, newest first.
pub fn visible_runs<'a>(data: &'a GitHubData, page: &GitHubPage) -> Vec<&'a WorkflowRun> {
    let needle = page.run_filter.trim().to_lowercase();
    data.runs
        .iter()
        .filter(|r| page.run_status.accepts(r.outcome()))
        .filter(|r| {
            needle.is_empty()
                || [
                    r.name.as_str(),
                    r.title.as_str(),
                    r.branch.as_str(),
                    r.actor.as_str(),
                    r.event.as_str(),
                    r.sha.as_str(),
                    r.path.as_str(),
                ]
                .iter()
                .any(|f| f.to_lowercase().contains(&needle))
                || r.run_number.to_string() == needle
        })
        .collect()
}

struct Cx<'a> {
    data: &'a GitHubData,
    snap: Option<&'a Snapshot>,
    checks: &'a HashMap<Oid, ChecksState>,
    busy: bool,
    now: i64,
    /// `ui.input().time`, for the running-arc animation.
    t: f64,
    dark: bool,
}

// ---------------------------------------------------------------------------
// Outcome vocabulary
// ---------------------------------------------------------------------------

fn outcome_tone(o: Outcome) -> Tone {
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

/// Sentence-case name of an outcome.
fn outcome_text(o: Outcome) -> &'static str {
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

fn outcome_fg(p: &Palette, o: Outcome) -> Color32 {
    match outcome_tone(o) {
        Tone::Neutral => p.text_2,
        t => t.color(p),
    }
}

fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

fn run_title(run: &WorkflowRun) -> &str {
    if run.title.is_empty() {
        &run.name
    } else {
        &run.title
    }
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

pub fn show(ui: &mut Ui, repo: &mut RepoState, settings: &Settings, actions: &mut Vec<Action>) {
    let RepoState {
        github,
        github_remote,
        github_error,
        github_loading,
        github_at,
        gh_page,
        snapshot,
        rate,
        checks,
        busy,
        ..
    } = repo;
    let Some((owner, name)) = github_remote.as_ref() else {
        return;
    };
    let now = format::now_secs();
    let t = ui.input(|i| i.time);

    page_head(
        ui,
        owner,
        name,
        *github_loading,
        *github_at,
        rate.as_ref(),
        gh_page.tab,
        now,
        actions,
    );

    if let Some(err) = github_error.as_deref()
        && github.is_none()
    {
        empty_pane(ui, |ui| {
            let body = format!(
                "{err}\n\ngitvis talks to GitHub through the gh CLI: run `gh auth login` in a terminal, then retry."
            );
            if kit::empty_state(
                ui,
                Icon::Warning,
                "Could not reach GitHub",
                &body,
                Some((Icon::Fetch, "Retry")),
            ) {
                actions.push(Action::RefreshGitHub);
            }
        });
        return;
    }
    let Some(data) = github.as_ref() else {
        empty_pane(ui, |ui| {
            ui.add_space((ui.available_height() * 0.3).min(200.0));
            ui.vertical_centered(|ui| {
                ui.set_max_width(320.0);
                kit::loading(ui, "Loading pull requests and workflow runs…");
            });
        });
        return;
    };
    let cx = Cx {
        data,
        snap: snapshot.as_ref(),
        checks,
        busy: busy.is_some(),
        now,
        t,
        dark: settings.dark,
    };
    match gh_page.tab {
        GitHubTab::Actions => actions_tab(ui, &cx, gh_page, actions),
        GitHubTab::PullRequests => prs_tab(ui, &cx, gh_page, actions),
    }
}

#[allow(clippy::too_many_arguments)]
fn page_head(
    ui: &mut Ui,
    owner: &str,
    name: &str,
    loading: bool,
    at: Option<std::time::Instant>,
    rate: Option<&RateLimit>,
    tab: GitHubTab,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let title = match tab {
        GitHubTab::Actions => "Actions",
        GitHubTab::PullRequests => "Pull requests",
    };
    let mut sub = vec![format!("{owner}/{name}")];
    if loading {
        sub.push("refreshing…".to_owned());
    } else if let Some(at) = at {
        sub.push(format!(
            "refreshed {}",
            format::relative(now - at.elapsed().as_secs() as i64, now)
        ));
    }
    if let Some(rate) = rate {
        let mut s = format!(
            "{} of {} API requests left",
            format::count(rate.remaining as usize),
            format::count(rate.limit as usize)
        );
        if rate.remaining < rate.limit / 5 {
            s.push_str(&format!(
                ", resets in {}",
                format::duration((rate.reset - now).max(0))
            ));
        }
        sub.push(s);
    }
    kit::page_header(ui, title, &sub.join("  ·  "), |ui| {
        if loading {
            ui.add_space(6.0);
            ui.add(egui::Spinner::new().size(14.0));
            ui.add_space(6.0);
        } else if IconButton::icon(Icon::Fetch)
            .show(ui)
            .on_hover_text("Refresh pull requests, runs and the selected run's jobs")
            .clicked()
        {
            actions.push(Action::RefreshGitHub);
        }
        if IconButton::icon(Icon::External)
            .show(ui)
            .on_hover_text(format!("Open {owner}/{name} on GitHub"))
            .clicked()
        {
            actions.push(Action::OpenUrl(format!(
                "https://github.com/{owner}/{name}"
            )));
        }
        ui.add_space(6.0);
        let options = [
            (GitHubTab::Actions, "Runs"),
            (GitHubTab::PullRequests, "Pull requests"),
        ];
        let w = segmented_width(ui, &options);
        ui.allocate_ui_with_layout(vec2(w, 28.0), Layout::left_to_right(Align::Center), |ui| {
            if let Some(t) = kit::segmented(ui, &options, tab) {
                actions.push(Action::SetGitHubTab(t));
            }
        });
    });
}

/// Width `kit::segmented` takes, so it can sit in a right-to-left toolbar.
fn segmented_width<T>(ui: &Ui, options: &[(T, &str)]) -> f32 {
    let font = FontId::new(size::SMALL, medium());
    let labels: f32 = options
        .iter()
        .map(|(_, l)| {
            ui.painter()
                .layout_no_wrap((*l).to_owned(), font.clone(), Color32::WHITE)
                .size()
                .x
                + 20.0
        })
        .sum();
    labels + 2.0 * options.len().saturating_sub(1) as f32 + 4.0
}

/// The list pane on the left (same frame as `kit::split`).
fn list_pane(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
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
            add(ui)
        });
}

/// The scrollable, padded, width-capped detail pane (as in `kit::split`),
/// with its own scroll position per `salt`.
fn detail_pane(ui: &mut Ui, salt: impl Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui)) {
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

/// The remaining area for an empty state or a spinner.
fn empty_pane(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas).inner_margin(Margin::same(24)))
        .show(ui, add);
}

/// `kit::search_box` on one row. Works around two kit issues: it centres
/// itself in all the available height (so it gets a row of its own), and
/// with text in it the clear button overflows the width by 8 points (which
/// makes a resizable panel grow every frame).
fn search_field(ui: &mut Ui, id: &str, text: &mut String, hint: &str) -> Response {
    ui.horizontal(|ui| {
        if !text.is_empty() {
            ui.set_max_width(ui.available_width() - 8.0);
        }
        kit::search_box(ui, id, text, hint)
    })
    .inner
}

/// Accent-coloured clickable text that never wraps (hashes, paths, ids).
fn link_text(ui: &mut Ui, text: &str, mono: bool) -> Response {
    let c = pal(ui).accent;
    let rt = if mono {
        kit::mono(text).color(c)
    } else {
        RichText::new(text).color(c)
    };
    ui.add(
        Label::new(rt)
            .sense(Sense::click())
            .wrap_mode(egui::TextWrapMode::Extend),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// `2026-10-26 16:46 · 1h ago`
fn when(secs: i64, now: i64) -> String {
    format!(
        "{}  ·  {}",
        format::absolute(secs),
        format::relative(secs, now)
    )
}

fn small(p: &Palette, text: impl Into<String>) -> RichText {
    RichText::new(text).size(size::SMALL).color(p.text_2)
}

fn caption(p: &Palette, text: impl Into<String>) -> RichText {
    RichText::new(text).size(size::SMALL).color(p.text_3)
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

fn actions_tab(ui: &mut Ui, cx: &Cx, page: &mut GitHubPage, actions: &mut Vec<Action>) {
    list_pane(ui, "gh_runs", |ui| runs_list(ui, cx, page, actions));

    let Some(run_id) = page.selected_run else {
        empty_pane(ui, |ui| {
            kit::empty_state(
                ui,
                Icon::Play,
                "Select a workflow run",
                "Its jobs, steps, annotations and full logs appear here.",
                None,
            );
        });
        return;
    };
    let Some(run) = cx.data.run(run_id) else {
        let pending = page.pending_run == Some(run_id);
        detail_pane(ui, ("gh_run", run_id), |ui| {
            if pending {
                kit::loading(ui, &format!("Loading run {run_id}…"));
            } else {
                kit::callout(
                    ui,
                    Tone::Warning,
                    "Run not found",
                    &format!("Run {run_id} is not among the loaded workflow runs."),
                );
            }
        });
        return;
    };
    if run.is_running() {
        ui.ctx().request_repaint_after(Duration::from_millis(90));
    }
    let show_timestamps = page.show_timestamps;
    let detail = page.runs.entry(run_id).or_default();

    // Log viewer at the bottom, when a job is selected.
    if let Some(job_id) = detail.selected_job {
        let p = *pal(ui);
        let fill = if ui.visuals().dark_mode {
            p.chrome
        } else {
            p.canvas
        };
        let avail = ui.available_height();
        egui::Panel::bottom("gh_log_view")
            .resizable(true)
            .default_size((avail * 0.5).max(200.0))
            .size_range(140.0..=(avail - 120.0).max(160.0))
            .frame(Frame::NONE.fill(fill))
            .show(ui, |ui| {
                // A panel shrinks to its content (and stores that size) unless
                // the content claims the whole area.
                ui.set_min_width(ui.available_width());
                ui.set_min_height(ui.available_height());
                let mut filter = page.log_filter.clone();
                let mut timestamps = show_timestamps;
                log_viewer(
                    ui,
                    cx,
                    run,
                    detail,
                    job_id,
                    &mut filter,
                    &mut timestamps,
                    actions,
                );
                page.log_filter = filter;
                page.show_timestamps = timestamps;
            });
    }

    detail_pane(ui, ("gh_run", run_id), |ui| {
        run_header(ui, cx, run, detail, actions);
        jobs_section(ui, cx, run, detail, actions);
        run_facts(ui, cx, run, detail, actions);
    });
}

fn runs_list(ui: &mut Ui, cx: &Cx, page: &mut GitHubPage, actions: &mut Vec<Action>) {
    let hint = format!("Filter {} runs", format::count(cx.data.runs.len()));
    search_field(ui, "gh_run_filter", &mut page.run_filter, &hint);
    ui.add_space(space::S);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        ui.add_space(2.0);
        for f in [
            RunFilter::All,
            RunFilter::Failed,
            RunFilter::Running,
            RunFilter::Success,
        ] {
            let n = cx
                .data
                .runs
                .iter()
                .filter(|r| f.accepts(r.outcome()))
                .count();
            let label = format!("{}  {}", f.label(), format::count(n));
            if kit::chip(ui, &label, page.run_status == f).clicked() {
                page.run_status = f;
            }
        }
    });
    ui.add_space(space::S);

    let runs = visible_runs(cx.data, page);
    if runs.is_empty() {
        ui.add_space(space::S);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            kit::muted(
                ui,
                if cx.data.runs.is_empty() {
                    "This repository has no workflow runs."
                } else {
                    "No runs match the filter."
                },
            );
        });
        return;
    }
    if runs.iter().any(|r| r.is_running()) {
        ui.ctx().request_repaint_after(Duration::from_millis(90));
    }
    let selected = page.selected_run;
    kit::list(
        ui,
        "gh_runs_list",
        runs.len(),
        kit::ROW_H,
        |i| selected == Some(runs[i].id),
        |ui, rect, resp, i| {
            let run = runs[i];
            let mut subtitle = format!("{} #{}", run.name, run.run_number);
            for part in [&run.branch, &run.event, &run.actor] {
                if !part.is_empty() {
                    subtitle.push_str("  ·  ");
                    subtitle.push_str(part);
                }
            }
            let meta = run
                .started
                .or(run.created)
                .map(|s| format::relative(s, cx.now))
                .unwrap_or_default();
            let badges = run_badges(run);
            kit::paint_row(
                ui,
                rect,
                &kit::Row::new(run_title(run))
                    .leading(kit::Leading::Dot(Color32::TRANSPARENT))
                    .subtitle(&subtitle)
                    .meta(&meta)
                    .badges(&badges),
                selected == Some(run.id),
            );
            outcome_dot(
                ui.painter(),
                run.outcome(),
                pos2(rect.left() + 19.0, rect.top() + 18.0),
                6.0,
                cx.t,
            );
            if resp.clicked() {
                actions.push(Action::OpenRun {
                    run: run.id,
                    job: None,
                });
            }
            resp.context_menu(|ui| run_menu(ui, cx, run, actions));
        },
    );
}

/// Only the state that matters: failure, running, a re-run attempt.
fn run_badges(run: &WorkflowRun) -> Vec<(String, Tone)> {
    let mut b = Vec::new();
    let o = run.outcome();
    if o.is_bad() || o.is_running() {
        b.push((outcome_text(o).to_owned(), outcome_tone(o)));
    }
    if run.attempt > 1 {
        b.push((format!("Attempt {}", run.attempt), Tone::Neutral));
    }
    b
}

fn run_menu(ui: &mut Ui, cx: &Cx, run: &WorkflowRun, actions: &mut Vec<Action>) {
    ui.set_min_width(240.0);
    if ui.button("Show run").clicked() {
        actions.push(Action::OpenRun {
            run: run.id,
            job: None,
        });
        ui.close();
    }
    if ui.button("Open on GitHub").clicked() {
        actions.push(Action::OpenUrl(run.url.clone()));
        ui.close();
    }
    if let Ok(oid) = Oid::from_str(&run.sha)
        && cx.snap.is_some_and(|s| s.index_of.contains_key(&oid))
        && ui.button("Show commit in graph").clicked()
    {
        actions.push(Action::JumpTo(oid));
        ui.close();
    }
    ui.separator();
    rerun_items(ui, cx, run, actions);
    ui.separator();
    if ui.button("Copy run URL").clicked() {
        actions.push(Action::Copy(run.url.clone()));
        ui.close();
    }
    if ui.button("Copy commit SHA").clicked() {
        actions.push(Action::Copy(run.sha.clone()));
        ui.close();
    }
}

fn rerun_items(ui: &mut Ui, cx: &Cx, run: &WorkflowRun, actions: &mut Vec<Action>) {
    if !run.is_running() {
        if ui
            .add_enabled(!cx.busy, egui::Button::new("Re-run all jobs"))
            .clicked()
        {
            actions.push(Action::Run(GitOp::GhRunRerun {
                id: run.id,
                failed_only: false,
            }));
            ui.close();
        }
        if run.is_failure()
            && ui
                .add_enabled(!cx.busy, egui::Button::new("Re-run failed jobs"))
                .clicked()
        {
            actions.push(Action::Run(GitOp::GhRunRerun {
                id: run.id,
                failed_only: true,
            }));
            ui.close();
        }
    } else if ui
        .add_enabled(
            !cx.busy,
            egui::Button::new(RichText::new("Cancel run…").color(pal(ui).danger)),
        )
        .clicked()
    {
        actions.push(Action::Dialog(cancel_dialog(run)));
        ui.close();
    }
}

fn cancel_dialog(run: &WorkflowRun) -> Dialog {
    Dialog::Confirm {
        title: format!("Cancel {} #{}?", run.name, run.run_number),
        body: format!(
            "{}\n\nRunning jobs are stopped; queued jobs are skipped.",
            run.title
        ),
        confirm: "Cancel run".into(),
        danger: true,
        ops: vec![GitOp::GhRunCancel { id: run.id }],
    }
}

/// Title with the outcome icon on the left and `buttons` on the right.
fn title_row(
    ui: &mut Ui,
    title: &str,
    icon: impl FnOnce(&mut Ui, Rect),
    buttons: impl FnOnce(&mut Ui),
) {
    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        buttons(ui);
        ui.add_space(space::M);
        ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let (r, _) = ui.allocate_exact_size(vec2(20.0, 22.0), Sense::hover());
            icon(ui, r);
            kit::title(ui, title);
        });
    });
}

fn run_header(
    ui: &mut Ui,
    cx: &Cx,
    run: &WorkflowRun,
    detail: &RunDetail,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let outcome = run.outcome();
    title_row(
        ui,
        run_title(run),
        |ui, r| outcome_dot(ui.painter(), outcome, r.center(), 8.0, cx.t),
        |ui| {
            if IconButton::icon(Icon::External)
                .compact()
                .show(ui)
                .on_hover_text("Open this run on GitHub")
                .clicked()
            {
                actions.push(Action::OpenUrl(run.url.clone()));
            }
            if IconButton::icon(Icon::Copy)
                .compact()
                .show(ui)
                .on_hover_text("Copy the run's URL")
                .clicked()
            {
                actions.push(Action::Copy(run.url.clone()));
            }
            if detail.jobs_loading {
                ui.add_space(6.0);
                ui.add(egui::Spinner::new().size(13.0));
                ui.add_space(6.0);
            } else if IconButton::icon(Icon::Fetch)
                .compact()
                .show(ui)
                .on_hover_text("Reload the jobs of this run")
                .clicked()
            {
                actions.push(Action::RefreshRun(run.id));
            }
            if run.is_running() {
                if IconButton::new(Icon::Stop, "Cancel")
                    .compact()
                    .danger()
                    .enabled(!cx.busy)
                    .show(ui)
                    .on_hover_text("gh run cancel")
                    .clicked()
                {
                    actions.push(Action::Dialog(cancel_dialog(run)));
                }
            } else {
                if IconButton::new(Icon::Rerun, "Re-run all")
                    .compact()
                    .enabled(!cx.busy)
                    .show(ui)
                    .on_hover_text("gh run rerun")
                    .clicked()
                {
                    actions.push(Action::Run(GitOp::GhRunRerun {
                        id: run.id,
                        failed_only: false,
                    }));
                }
                if run.is_failure()
                    && IconButton::new(Icon::Rerun, "Re-run failed")
                        .compact()
                        .enabled(!cx.busy)
                        .show(ui)
                        .on_hover_text("gh run rerun --failed")
                        .clicked()
                {
                    actions.push(Action::Run(GitOp::GhRunRerun {
                        id: run.id,
                        failed_only: true,
                    }));
                }
            }
        },
    );

    // Workflow and number, with the state that matters.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
        ui.add_space(28.0);
        // The workflow name, unless the title already is the workflow name.
        let label = if run.name == run_title(run) {
            format!("#{}", run.run_number)
        } else {
            format!("{} #{}", run.name, run.run_number)
        };
        ui.label(RichText::new(label).family(medium()).color(p.text_2));
        kit::badge(ui, outcome_text(outcome), outcome_tone(outcome));
        if run.attempt > 1 {
            kit::badge(ui, &format!("Attempt {}", run.attempt), Tone::Neutral);
        }
    });
}

/// Trigger, branch, commit, actor, timing and ids of a run.
fn run_facts(
    ui: &mut Ui,
    cx: &Cx,
    run: &WorkflowRun,
    detail: &RunDetail,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let outcome = run.outcome();
    kit::section(ui, "Details", None);
    let in_graph = Oid::from_str(&run.sha)
        .ok()
        .filter(|o| cx.snap.is_some_and(|s| s.index_of.contains_key(o)));
    kit::kv(ui, 92.0, |kv| {
        kv.row("Status", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(outcome_text(outcome)).color(outcome_fg(&p, outcome)));
                if let Some(c) = run
                    .conclusion
                    .as_deref()
                    .filter(|c| !outcome.is_running() && *c != outcome.label())
                {
                    ui.label(small(&p, c));
                }
            });
        });
        if run.path.is_empty() {
            kv.muted("Workflow", "no workflow file");
        } else {
            kv.row("Workflow", |ui| {
                if link_text(ui, &run.path, true)
                    .on_hover_text("Open the workflow file on GitHub at this commit")
                    .clicked()
                {
                    actions.push(Action::OpenUrl(cx.data.file_url(&run.sha, &run.path)));
                }
            });
        }
        kv.text("Trigger", &run.event);
        if !run.actor.is_empty() {
            kv.row("Actor", |ui| {
                ui.horizontal_wrapped(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                    crate::views::avatar(ui.painter(), r.center(), 9.0, &run.actor, cx.dark);
                    ui.label(RichText::new(&run.actor).color(p.text));
                    if !run.triggering_actor.is_empty() && run.triggering_actor != run.actor {
                        ui.label(small(&p, format!("re-run by {}", run.triggering_actor)));
                    }
                });
            });
        }
        if !run.branch.is_empty() {
            kv.row("Branch", |ui| {
                ui.horizontal(|ui| {
                    kit::badge(ui, &run.branch, Tone::Neutral);
                });
            });
        }
        if !run.sha.is_empty() {
            kv.row("Commit", |ui| {
                ui.horizontal_wrapped(|ui| {
                    let r = link_text(ui, short_sha(&run.sha), true);
                    let r = r.on_hover_text(if in_graph.is_some() {
                        "Show this commit in the graph"
                    } else {
                        "Copy the full SHA (the commit is not in the graph)"
                    });
                    if r.clicked() {
                        match in_graph {
                            Some(oid) => actions.push(Action::JumpTo(oid)),
                            None => actions.push(Action::Copy(run.sha.clone())),
                        }
                    }
                    let msg = run.head_message.lines().next().unwrap_or("");
                    if !msg.is_empty() {
                        ui.label(RichText::new(msg).color(p.text));
                    }
                    if !run.head_author.is_empty() {
                        ui.label(small(&p, format!("by {}", run.head_author)));
                    }
                });
            });
        }
        match run.started.or(run.created) {
            Some(s) => kv.text("Started", when(s, cx.now)),
            None => kv.muted("Started", "not started"),
        }
        kv.row("Duration", |ui| {
            ui.horizontal_wrapped(|ui| {
                match run.duration_secs(cx.now) {
                    Some(d) if run.is_running() => {
                        ui.label(
                            RichText::new(format!("{} so far", format::duration(d))).color(p.text),
                        );
                    }
                    Some(d) => {
                        ui.label(RichText::new(format::duration(d)).color(p.text));
                    }
                    None => {
                        ui.label(RichText::new("—").color(p.text_3));
                    }
                }
                if let Some(q) = run.queue_secs() {
                    ui.label(small(&p, format!("queued {}", format::duration(q))));
                }
            });
        });
        if let Some(u) = run.updated {
            kv.text(
                if run.is_running() {
                    "Updated"
                } else {
                    "Finished"
                },
                when(u, cx.now),
            );
        }
        kv.text("Attempt", run.attempt.to_string());
        if let Some(Ok(jobs)) = &detail.jobs {
            let runners: std::collections::BTreeSet<&str> = jobs
                .iter()
                .flat_map(|j| j.labels.iter().map(String::as_str))
                .collect();
            if !runners.is_empty() {
                kv.text(
                    "Runners",
                    runners.into_iter().collect::<Vec<_>>().join(", "),
                );
            }
        }
        kv.row("Run id", |ui| {
            if link_text(ui, &run.id.to_string(), true)
                .on_hover_text("Copy")
                .clicked()
            {
                actions.push(Action::Copy(run.id.to_string()));
            }
        });
    });
}

fn jobs_section(
    ui: &mut Ui,
    cx: &Cx,
    run: &WorkflowRun,
    detail: &mut RunDetail,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let n = detail.jobs().len();
    let mut expand_all: Option<bool> = None;
    kit::section_with(ui, "Jobs", (n > 0).then_some(n), |ui| {
        if n > 1 {
            if IconButton::text("Collapse all")
                .compact()
                .show(ui)
                .clicked()
            {
                expand_all = Some(false);
            }
            if IconButton::text("Expand all").compact().show(ui).clicked() {
                expand_all = Some(true);
            }
        }
        if let Some(at) = detail.jobs_at {
            ui.label(caption(
                &p,
                format!(
                    "fetched {}",
                    format::relative(cx.now - at.elapsed().as_secs() as i64, cx.now)
                ),
            ));
        }
    });
    if let Some(expand) = expand_all {
        let ids: Vec<u64> = detail.jobs().iter().map(|j| j.id).collect();
        if expand {
            detail.expanded.extend(ids);
        } else {
            detail.expanded.clear();
        }
    }
    match &detail.jobs {
        None => {
            if detail.jobs_loading {
                kit::loading(ui, "Loading jobs…");
            } else {
                kit::muted(ui, "Jobs not loaded.");
            }
            return;
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
                actions.push(Action::RefreshRun(run.id));
            }
            return;
        }
        Some(Ok(jobs)) if jobs.is_empty() => {
            kit::muted(ui, "This run has no jobs yet.");
            return;
        }
        _ => {}
    }

    let jobs: Vec<Job> = detail.jobs().to_vec();
    // A summary only when there are many jobs and some need attention.
    let bad = jobs.iter().filter(|j| j.outcome().is_bad()).count();
    let running = jobs.iter().filter(|j| j.outcome().is_running()).count();
    if jobs.len() > 3 && bad + running > 0 {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            let groups = [
                (bad, "failed", Tone::Danger),
                (running, "running", Tone::Warning),
                (
                    jobs.iter().filter(|j| j.outcome().is_good()).count(),
                    "passed",
                    Tone::Neutral,
                ),
                (
                    jobs.iter()
                        .filter(|j| j.outcome() == Outcome::Skipped)
                        .count(),
                    "skipped",
                    Tone::Neutral,
                ),
            ];
            for (n, label, tone) in groups {
                if n > 0 {
                    kit::badge(ui, &format!("{n} {label}"), tone);
                }
            }
        });
        ui.add_space(2.0);
    }
    ui.scope(|ui| jobs_rows(ui, cx, run, &jobs, detail, actions));
}

fn jobs_rows(
    ui: &mut Ui,
    cx: &Cx,
    run: &WorkflowRun,
    jobs: &[Job],
    detail: &mut RunDetail,
    actions: &mut Vec<Action>,
) {
    let mut toggles: Vec<u64> = Vec::new();
    ui.spacing_mut().item_spacing.y = 0.0;
    for job in jobs {
        let expanded = detail.expanded.contains(&job.id);
        let selected = detail.selected_job == Some(job.id);
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), JOB_ROW_H), Sense::click());
        if ui.is_rect_visible(rect) {
            kit::row_background(ui, rect, resp.hovered(), selected);
            let log_label = match detail.logs.get(&job.id) {
                Some(LogState { loading: true, .. }) => "Loading…",
                _ => "Log",
            };
            // The log button sits in a detached child so the row keeps its height.
            let btn_rect = Rect::from_min_max(
                pos2(rect.right() - 120.0, rect.top()),
                pos2(rect.right() - 6.0, rect.bottom()),
            );
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(btn_rect)
                    .layout(Layout::right_to_left(Align::Center)),
            );
            let btn = IconButton::new(Icon::List, log_label)
                .compact()
                .selected(selected)
                .show(&mut child)
                .on_hover_text("Show this job's log below");
            if btn.clicked() {
                actions.push(Action::ShowLog {
                    run: run.id,
                    job: job.id,
                    step: None,
                });
            }
            let right = btn.rect.left() - space::M;
            job_row(ui, cx, job, rect, right, expanded, selected);
        }
        if resp.clicked() {
            toggles.push(job.id);
        }
        resp.context_menu(|ui| {
            ui.set_min_width(220.0);
            if ui.button("Show log").clicked() {
                actions.push(Action::ShowLog {
                    run: run.id,
                    job: job.id,
                    step: None,
                });
                ui.close();
            }
            if ui.button("Load annotations").clicked() {
                actions.push(Action::LoadAnnotations {
                    run: run.id,
                    job: job.id,
                });
                ui.close();
            }
            if ui.button("Open job on GitHub").clicked() {
                actions.push(Action::OpenUrl(job.url.clone()));
                ui.close();
            }
            ui.separator();
            if ui.button("Copy job URL").clicked() {
                actions.push(Action::Copy(job.url.clone()));
                ui.close();
            }
            if ui.button("Copy job id").clicked() {
                actions.push(Action::Copy(job.id.to_string()));
                ui.close();
            }
        });
        if expanded {
            steps_and_annotations(ui, cx, run, job, detail, actions);
        }
    }
    for id in toggles {
        if !detail.expanded.remove(&id) {
            detail.expanded.insert(id);
        }
    }
}

/// Chevron, outcome, name, runner and steps; duration right-aligned at `right`.
fn job_row(ui: &Ui, cx: &Cx, job: &Job, rect: Rect, right: f32, expanded: bool, selected: bool) {
    let p = *pal(ui);
    let painter = ui.painter();
    let mid = rect.center().y;
    chevron(painter, pos2(rect.left() + 14.0, mid), expanded, p.text_3);
    outcome_dot(
        painter,
        job.outcome(),
        pos2(rect.left() + 32.0, mid),
        6.0,
        cx.t,
    );
    let dur = job
        .duration_secs(cx.now)
        .map(format::duration)
        .unwrap_or_else(|| outcome_text(job.outcome()).to_owned());
    let dur_g = painter.layout_no_wrap(dur, FontId::proportional(size::SMALL), p.text_2);
    let dur_x = right - dur_g.size().x;
    painter.galley(pos2(dur_x, mid - dur_g.size().y / 2.0), dur_g, p.text_2);
    let x = rect.left() + STEP_INDENT;
    let max_x = dur_x - space::M;
    let name_c = if selected { p.text_strong } else { p.text };
    let g = truncated(
        painter,
        &job.name,
        FontId::new(size::BODY, medium()),
        name_c,
        (max_x - x).max(40.0),
    );
    let name_w = g.size().x;
    painter.galley(pos2(x, mid - g.size().y / 2.0), g, name_c);
    let mut meta: Vec<String> = Vec::new();
    if !job.labels.is_empty() {
        meta.push(job.labels.join(", "));
    }
    if let Some(r) = &job.runner {
        meta.push(r.clone());
    }
    if job.attempt > 1 {
        meta.push(format!("attempt {}", job.attempt));
    }
    meta.push(format::plural(job.steps.len(), "step", "steps"));
    let mx = x + name_w + 10.0;
    if max_x - mx > 40.0 {
        let g = truncated(
            painter,
            &meta.join("  ·  "),
            FontId::proportional(size::SMALL),
            p.text_2,
            max_x - mx,
        );
        painter.galley(pos2(mx, mid - g.size().y / 2.0), g, p.text_2);
    }
}

fn chevron(p: &egui::Painter, c: egui::Pos2, open: bool, color: Color32) {
    let s = 3.5;
    let pts = if open {
        vec![
            pos2(c.x - s, c.y - s * 0.5),
            pos2(c.x, c.y + s * 0.5),
            pos2(c.x + s, c.y - s * 0.5),
        ]
    } else {
        vec![
            pos2(c.x - s * 0.5, c.y - s),
            pos2(c.x + s * 0.5, c.y),
            pos2(c.x - s * 0.5, c.y + s),
        ]
    };
    p.line(pts, Stroke::new(1.5, color));
}

fn steps_and_annotations(
    ui: &mut Ui,
    cx: &Cx,
    run: &WorkflowRun,
    job: &Job,
    detail: &RunDetail,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let job_dur = job.duration_secs(cx.now).unwrap_or(0).max(1) as f32;
    let num_font = FontId::monospace(size::SMALL);
    for step in &job.steps {
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), STEP_ROW_H), Sense::click());
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let row = Rect::from_min_max(pos2(rect.left() + STEP_INDENT - 14.0, rect.top()), rect.max);
        kit::row_background(ui, row, resp.hovered(), false);
        let painter = ui.painter();
        let mid = rect.center().y;
        let outcome = step.outcome();
        let x0 = rect.left() + STEP_INDENT;
        outcome_dot(painter, outcome, pos2(x0 + 5.0, mid), 5.0, cx.t);
        painter.text(
            pos2(x0 + 34.0, mid),
            Align2::RIGHT_CENTER,
            step.number.to_string(),
            num_font.clone(),
            p.text_3,
        );
        // Fixed duration column so the time bars line up.
        let right = rect.right() - 12.0;
        let dur_col = 64.0;
        let bar_w = 90.0;
        let dur = step.duration_secs(cx.now);
        let dur_text = dur.map(format::duration).unwrap_or_else(|| {
            if outcome == Outcome::Skipped {
                "skipped".into()
            } else {
                String::new()
            }
        });
        painter.text(
            pos2(right, mid),
            Align2::RIGHT_CENTER,
            dur_text,
            FontId::proportional(size::SMALL),
            if dur.is_some() { p.text_2 } else { p.text_3 },
        );
        let bar_left = right - dur_col - bar_w;
        if let Some(d) = dur
            && d > 0
        {
            let frac = (d as f32 / job_dur).clamp(0.02, 1.0);
            let color = if outcome.is_bad() {
                p.danger.gamma_multiply(0.7)
            } else {
                p.text_3.gamma_multiply(0.6)
            };
            painter.rect_filled(
                Rect::from_min_size(pos2(bar_left, mid - 2.0), vec2(bar_w * frac, 4.0)),
                CornerRadius::same(2),
                color,
            );
        }
        let mut name_right = bar_left - space::M;
        // Start and finish times appear in place while hovered.
        if resp.hovered()
            && let Some(s) = step.started
        {
            let range = match step.completed {
                Some(c) => format!("{} – {}", format::clock(s), format::clock(c)),
                None => format!("since {}", format::clock(s)),
            };
            let r = painter.text(
                pos2(name_right, mid),
                Align2::RIGHT_CENTER,
                range,
                FontId::proportional(size::SMALL),
                p.text_3,
            );
            name_right = r.left() - space::M;
        }
        let name_x = x0 + 44.0;
        let g = truncated(
            painter,
            &step.name,
            FontId::proportional(size::BODY - 0.5),
            p.text,
            (name_right - name_x).max(40.0),
        );
        painter.galley(pos2(name_x, mid - g.size().y / 2.0), g, p.text);
        if resp.clicked() {
            actions.push(Action::ShowLog {
                run: run.id,
                job: job.id,
                step: Some(step.number),
            });
        }
    }

    // Annotations, indented under the job like its steps.
    ui.spacing_mut().item_spacing.y = 8.0;
    Frame::NONE
        .inner_margin(Margin {
            left: STEP_INDENT as i8,
            right: 0,
            top: 6,
            bottom: 10,
        })
        .show(ui, |ui| match detail.annotations.get(&job.id) {
            Some(Ok(list)) if list.is_empty() => {
                kit::muted(ui, "No annotations.");
            }
            Some(Ok(list)) => {
                for a in list {
                    annotation(ui, cx, a, actions);
                }
            }
            Some(Err(e)) => {
                kit::callout(ui, Tone::Danger, "Could not load the annotations", e);
            }
            None => {
                if detail.annotations_loading.contains(&job.id) {
                    kit::loading(ui, "Loading annotations…");
                } else if IconButton::new(Icon::Warning, "Load annotations")
                    .compact()
                    .show(ui)
                    .on_hover_text("Errors, warnings and notices GitHub attached to this job")
                    .clicked()
                {
                    actions.push(Action::LoadAnnotations {
                        run: run.id,
                        job: job.id,
                    });
                }
            }
        });
    ui.spacing_mut().item_spacing.y = 0.0;
}

fn annotation(ui: &mut Ui, cx: &Cx, a: &Annotation, actions: &mut Vec<Action>) {
    let (tone, level) = match a.level.as_str() {
        "failure" => (Tone::Danger, "Failure"),
        "warning" => (Tone::Warning, "Warning"),
        "notice" => (Tone::Neutral, "Notice"),
        other => (Tone::Neutral, other),
    };
    let title = if a.title.is_empty() {
        level.to_owned()
    } else {
        format!("{level}: {}", a.title)
    };
    let location = if a.path.is_empty() {
        String::new()
    } else if a.start_line == a.end_line || a.end_line == 0 {
        format!("{}:{}", a.path, a.start_line)
    } else {
        format!("{}:{}-{}", a.path, a.start_line, a.end_line)
    };
    let local = cx.snap.and_then(|s| {
        let p = s.workdir.join(&a.path);
        (!a.path.is_empty() && p.is_file()).then_some(p)
    });
    kit::callout_with(ui, tone, &title, a.message.trim(), |ui| {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if !location.is_empty() {
                let r = link_text(ui, &location, true).on_hover_text(if local.is_some() {
                    "Open this file"
                } else {
                    "Copy the path"
                });
                if r.clicked() {
                    match &local {
                        Some(p) => actions.push(Action::OpenPath(p.clone())),
                        None => actions.push(Action::Copy(a.path.clone())),
                    }
                }
            }
            if IconButton::new(Icon::Copy, "Copy message")
                .compact()
                .show(ui)
                .clicked()
            {
                actions.push(Action::Copy(a.message.clone()));
            }
        });
    });
}

// ---------------------------------------------------------------------------
// Log viewer
// ---------------------------------------------------------------------------

enum LogRow {
    /// Index into the step-starts list.
    Step(usize),
    Line(usize),
}

#[allow(clippy::too_many_arguments)]
fn log_viewer(
    ui: &mut Ui,
    cx: &Cx,
    run: &WorkflowRun,
    detail: &mut RunDetail,
    job_id: u64,
    filter: &mut String,
    show_timestamps: &mut bool,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let job = detail.jobs().iter().find(|j| j.id == job_id).cloned();
    let job_name = job
        .as_ref()
        .map(|j| j.name.clone())
        .unwrap_or_else(|| format!("Job {job_id}"));
    let mut close = false;
    let mut jump: Option<i32> = None;
    let mut fold: Option<bool> = None;
    let mut copy_all = false;

    let doc_stats = match detail.logs.get(&job_id) {
        Some(LogState {
            doc: Some(Ok(doc)), ..
        }) => Some((
            doc.log.lines.len(),
            doc.log.errors.len(),
            doc.log.warnings.len(),
            doc.log.bytes,
            doc.matches.len(),
        )),
        _ => None,
    };

    // Toolbar.
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
                    RichText::new(&job_name)
                        .size(size::BODY)
                        .family(semibold())
                        .color(p.text_strong),
                );
                if let Some((lines, errors, warnings, bytes, matched)) = doc_stats {
                    ui.label(caption(
                        &p,
                        format!(
                            "{} lines  ·  {}",
                            format::count(lines),
                            format::bytes(bytes)
                        ),
                    ));
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
                        ui.label(small(&p, format!("{} matching", format::count(matched))));
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
                        close = true;
                    }
                    if let Some(j) = &job
                        && IconButton::icon(Icon::External)
                            .compact()
                            .show(ui)
                            .on_hover_text("Open this job on GitHub")
                            .clicked()
                    {
                        actions.push(Action::OpenUrl(j.url.clone()));
                    }
                    if IconButton::icon(Icon::Copy)
                        .compact()
                        .show(ui)
                        .on_hover_text("Copy the shown lines")
                        .clicked()
                    {
                        copy_all = true;
                    }
                    ui.add_space(4.0);
                    icons::divider(ui, 16.0);
                    ui.add_space(4.0);
                    if IconButton::text("Unfold")
                        .compact()
                        .show(ui)
                        .on_hover_text("Expand all groups")
                        .clicked()
                    {
                        fold = Some(false);
                    }
                    if IconButton::text("Fold")
                        .compact()
                        .show(ui)
                        .on_hover_text("Collapse all groups")
                        .clicked()
                    {
                        fold = Some(true);
                    }
                    if IconButton::new(Icon::Clock, "Time")
                        .compact()
                        .selected(*show_timestamps)
                        .show(ui)
                        .on_hover_text("Show the runner's timestamps (UTC)")
                        .clicked()
                    {
                        *show_timestamps = !*show_timestamps;
                    }
                    if doc_stats.is_some_and(|s| s.1 > 0) {
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
                            let resp = search_field(ui, "gh_log_filter", filter, "Filter lines");
                            if (resp.has_focus() || resp.lost_focus())
                                && ui.input(|i| i.key_pressed(egui::Key::Escape))
                            {
                                filter.clear();
                                resp.surrender_focus();
                            }
                        },
                    );
                });
            });
        });
    kit::hairline(ui);
    if close {
        detail.selected_job = None;
        return;
    }

    let Some(state) = detail.logs.get_mut(&job_id) else {
        log_note(ui, |ui| {
            kit::muted(ui, "Log not requested.");
        });
        return;
    };
    let doc = match &mut state.doc {
        None => {
            log_note(ui, |ui| kit::loading(ui, "Downloading the log…"));
            return;
        }
        Some(Err(e)) => {
            let e = e.clone();
            log_note(ui, |ui| {
                let mut retry = false;
                kit::callout_with(
                    ui,
                    Tone::Warning,
                    "The log is not available",
                    &format!(
                        "{e}\n\nLogs of running jobs and of jobs that never started are not available through the API."
                    ),
                    |ui| {
                        retry = IconButton::new(Icon::Fetch, "Retry")
                            .compact()
                            .show(ui)
                            .clicked();
                    },
                );
                if retry {
                    actions.push(Action::ShowLog {
                        run: run.id,
                        job: job_id,
                        step: None,
                    });
                }
            });
            return;
        }
        Some(Ok(doc)) => doc,
    };
    doc.set_filter(filter);
    if let Some(f) = fold {
        if f {
            doc.collapsed = (0..doc.log.groups.len() as u32).collect();
        } else {
            doc.collapsed.clear();
        }
    }
    if let Some(dir) = jump
        && !doc.log.errors.is_empty()
    {
        let errors = &doc.log.errors;
        let cur = doc.selected;
        let next = match (dir, cur) {
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
        // Make sure the target is not hidden in a folded group.
        if let Some(g) = doc.log.lines[next].group {
            doc.collapsed.remove(&g);
        }
    }
    if doc.steps.is_none()
        && let Some(j) = &job
    {
        let steps: Vec<(u32, Option<i64>)> =
            j.steps.iter().map(|s| (s.number, s.started)).collect();
        doc.steps = Some(doc.log.step_starts(&steps));
    }

    // Display rows: step headers interleaved with (filtered, unfolded) lines.
    let filtering = !doc.filter.trim().is_empty();
    let steps = doc.steps.clone().unwrap_or_default();
    let mut rows: Vec<LogRow> = Vec::with_capacity(doc.matches.len() + steps.len());
    let mut next_step = 0usize;
    let mut scroll_row: Option<usize> = None;
    for &li in &doc.matches {
        if !filtering {
            while next_step < steps.len() && steps[next_step].1 <= li {
                rows.push(LogRow::Step(next_step));
                next_step += 1;
            }
        }
        let line = &doc.log.lines[li];
        if !filtering
            && line.kind != LogKind::Group
            && line.group.is_some_and(|g| doc.collapsed.contains(&g))
        {
            continue;
        }
        if doc.scroll_to == Some(li) {
            scroll_row = Some(rows.len());
        }
        rows.push(LogRow::Line(li));
    }
    if !filtering {
        while next_step < steps.len() {
            rows.push(LogRow::Step(next_step));
            next_step += 1;
        }
    }
    if copy_all {
        let text: String = rows
            .iter()
            .filter_map(|r| match r {
                LogRow::Line(i) => Some(doc.log.lines[*i].text.as_str()),
                LogRow::Step(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        actions.push(Action::Copy(text));
    }
    doc.scroll_to = None;
    if filtering && rows.is_empty() {
        log_note(ui, |ui| {
            kit::muted(ui, "No lines match the filter.");
        });
        return;
    }

    let font = FontId::monospace(size::MONO);
    let char_w = ui
        .painter()
        .layout_no_wrap("0".repeat(20), font.clone(), Color32::WHITE)
        .size()
        .x
        / 20.0;
    let digits = doc.log.lines.len().max(1).to_string().len() as f32;
    let num_w = 12.0 + digits * char_w + 14.0;
    let clock_w = if *show_timestamps {
        12.0 * char_w + 14.0
    } else {
        0.0
    };
    let gutter = num_w + clock_w;
    let avail = ui.available_width();
    let content_w =
        (gutter + 18.0 + doc.log.max_chars.min(LOG_MAX_CHARS) as f32 * char_w + 24.0).max(avail);
    let avail_h = ui.available_height();
    let steps_by_number: HashMap<u32, &gitvis_core::github::Step> = job
        .as_ref()
        .map(|j| j.steps.iter().map(|s| (s.number, s)).collect())
        .unwrap_or_default();

    ui.spacing_mut().item_spacing.y = 0.0;
    let mut area = egui::ScrollArea::both()
        .id_salt(("gh_log_area", job_id))
        .auto_shrink([false, false]);
    if let Some(row) = scroll_row {
        area = area.vertical_scroll_offset((row as f32 * LOG_ROW_H - avail_h * 0.3).max(0.0));
    }
    let dark = cx.dark;
    let mut select: Option<usize> = None;
    let mut toggle_group: Option<u32> = None;
    let mut copy: Option<String> = None;
    area.show_rows(ui, LOG_ROW_H, rows.len(), |ui, range| {
        let clip_right = ui.clip_rect().right();
        for i in range {
            let (rect, resp) = ui.allocate_exact_size(vec2(content_w, LOG_ROW_H), Sense::click());
            let painter = ui.painter().clone();
            let mid = rect.center().y;
            let full =
                Rect::from_min_max(rect.min, pos2(clip_right.max(rect.right()), rect.bottom()));
            match rows[i] {
                LogRow::Step(si) => {
                    let (number, _) = steps[si];
                    let step = steps_by_number.get(&number);
                    painter.rect_filled(full, 0, p.raised);
                    let outcome = step.map(|s| s.outcome()).unwrap_or(Outcome::Unknown);
                    outcome_dot(
                        &painter,
                        outcome,
                        pos2(rect.left() + num_w - 8.0, mid),
                        5.0,
                        cx.t,
                    );
                    let x = rect.left() + gutter;
                    let r = painter.text(
                        pos2(x, mid),
                        Align2::LEFT_CENTER,
                        format!("Step {number}"),
                        FontId::proportional(size::SMALL),
                        p.text_2,
                    );
                    let name = step.map(|s| s.name.as_str()).unwrap_or("");
                    let r = painter.text(
                        pos2(r.right() + 8.0, mid),
                        Align2::LEFT_CENTER,
                        name,
                        FontId::new(size::SMALL, medium()),
                        p.text_strong,
                    );
                    if let Some(d) = step.and_then(|s| s.duration_secs(cx.now)) {
                        painter.text(
                            pos2(r.right() + 10.0, mid),
                            Align2::LEFT_CENTER,
                            format::duration(d),
                            FontId::proportional(size::SMALL),
                            p.text_3,
                        );
                    }
                }
                LogRow::Line(li) => {
                    let line = &doc.log.lines[li];
                    let selected = doc.selected == Some(li);
                    let edge = match line.kind {
                        LogKind::Error => Some(p.danger),
                        LogKind::Warning => Some(p.warning),
                        _ => None,
                    };
                    if selected {
                        painter.rect_filled(full, 0, p.selection);
                    } else if let Some(c) = edge {
                        painter.rect_filled(
                            full,
                            0,
                            c.gamma_multiply(if dark { 0.13 } else { 0.10 }),
                        );
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
                    if *show_timestamps && !line.clock.is_empty() {
                        painter.text(
                            pos2(rect.left() + num_w, mid),
                            Align2::LEFT_CENTER,
                            &line.clock,
                            font.clone(),
                            p.text_3,
                        );
                    }
                    let mut x = rect.left() + gutter;
                    if line.kind == LogKind::Group {
                        let gi = doc
                            .log
                            .groups
                            .iter()
                            .position(|g| g.header == li)
                            .map(|g| g as u32);
                        let open = gi.is_some_and(|g| !doc.collapsed.contains(&g));
                        chevron(&painter, pos2(x + 5.0, mid), open, p.text_2);
                        x += 18.0;
                        let g = line_galley(&painter, line, "", p.text_strong, &font, dark);
                        let w = g.size().x;
                        painter.galley(pos2(x, mid - g.size().y / 2.0), g, p.text_strong);
                        if let Some(g) = gi.and_then(|g| doc.log.groups.get(g as usize)) {
                            painter.text(
                                pos2(x + w + 10.0, mid),
                                Align2::LEFT_CENTER,
                                format::plural(g.len, "line", "lines"),
                                FontId::proportional(size::SMALL),
                                p.text_3,
                            );
                        }
                        if resp.clicked()
                            && let Some(g) = gi
                        {
                            toggle_group = Some(g);
                        }
                    } else {
                        if line.group.is_some() {
                            x += 18.0;
                        }
                        let base = match line.kind {
                            LogKind::Error => p.danger,
                            LogKind::Warning => p.warning,
                            LogKind::Notice => p.accent,
                            LogKind::Debug => p.text_3,
                            LogKind::Command => p.purple,
                            LogKind::Section => p.text_strong,
                            _ => p.text,
                        };
                        let prefix = match line.kind {
                            LogKind::Error => "error: ",
                            LogKind::Warning => "warning: ",
                            LogKind::Notice => "notice: ",
                            LogKind::Debug => "debug: ",
                            LogKind::Command => "$ ",
                            _ => "",
                        };
                        let galley = line_galley(&painter, line, prefix, base, &font, dark);
                        painter.galley(pos2(x, mid - galley.size().y / 2.0), galley, base);
                        if resp.clicked() {
                            select = Some(li);
                        }
                    }
                    let text = line.text.clone();
                    let group_text = line.group.and_then(|g| {
                        let g = doc.log.groups.get(g as usize)?;
                        Some(
                            doc.log.lines[g.header..=g.header + g.len]
                                .iter()
                                .map(|l| l.text.as_str())
                                .collect::<Vec<_>>()
                                .join("\n"),
                        )
                    });
                    resp.context_menu(|ui| {
                        ui.set_min_width(180.0);
                        if ui.button("Copy line").clicked() {
                            copy = Some(text.clone());
                            ui.close();
                        }
                        if let Some(g) = &group_text
                            && ui.button("Copy group").clicked()
                        {
                            copy = Some(g.clone());
                            ui.close();
                        }
                        if ui.button("Copy line with timestamp").clicked() {
                            copy = Some(format!("{} {}", line.clock, text));
                            ui.close();
                        }
                    });
                }
            }
        }
    });
    if let Some(li) = select {
        doc.selected = Some(li);
    }
    if let Some(g) = toggle_group
        && !doc.collapsed.remove(&g)
    {
        doc.collapsed.insert(g);
    }
    if let Some(text) = copy {
        actions.push(Action::Copy(text));
    }
}

/// A padded note inside the log panel.
fn log_note(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    Frame::NONE
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, add);
}

fn line_galley(
    p: &egui::Painter,
    line: &LogLine,
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
    let text: String = line.text.chars().take(LOG_MAX_CHARS).collect();
    if line.spans.is_empty() {
        job.append(&text, 0.0, fmt(base));
        return p.layout_job(job);
    }
    let chars: Vec<char> = text.chars().collect();
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

// ---------------------------------------------------------------------------
// Checks (shared with the commit details panel)
// ---------------------------------------------------------------------------

/// A one-line row: outcome icon, title, a caption after it, and a value on
/// the right. `external` shows a link icon while hovered (the click opens a
/// browser).
fn line_row(
    ui: &mut Ui,
    outcome: Outcome,
    title: &str,
    secondary: &str,
    right_text: &str,
    external: bool,
    t: f64,
) -> Response {
    let p = *pal(ui);
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), LINE_ROW_H), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    kit::row_background(ui, rect, resp.hovered(), false);
    let painter = ui.painter();
    let mid = rect.center().y;
    outcome_dot(painter, outcome, pos2(rect.left() + 15.0, mid), 5.5, t);
    let mut right = rect.right() - 10.0;
    if !right_text.is_empty() {
        let r = painter.text(
            pos2(right, mid),
            Align2::RIGHT_CENTER,
            right_text,
            FontId::proportional(size::SMALL),
            p.text_2,
        );
        right = r.left() - 8.0;
    }
    if external && resp.hovered() {
        icons::paint(
            painter,
            Icon::External,
            pos2(right - 6.0, mid),
            12.0,
            p.text_2,
        );
        right -= 20.0;
    }
    let x = rect.left() + 30.0;
    let g = truncated(
        painter,
        title,
        FontId::proportional(size::BODY - 0.5),
        p.text,
        (right - x).max(30.0),
    );
    let w = g.size().x;
    painter.galley(pos2(x, mid - g.size().y / 2.0), g, p.text);
    let sx = x + w + 8.0;
    if !secondary.is_empty() && right - sx > 30.0 {
        let g = truncated(
            painter,
            secondary,
            FontId::proportional(size::SMALL),
            p.text_3,
            right - sx,
        );
        painter.galley(pos2(sx, mid - g.size().y / 2.0), g, p.text_3);
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// The check runs of a commit: a section header with refresh, a summary
/// when something failed or is running, then one row per check. Actions
/// jobs open in the app; other apps open their URL.
pub fn checks_section(
    ui: &mut Ui,
    sha: Oid,
    state: Option<&ChecksState>,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let t = ui.input(|i| i.time);
    let loading = state.is_some_and(|s| s.loading);
    let result = state.and_then(|s| s.result.as_ref());
    let count = match result {
        Some(Ok(c)) if !c.is_empty() => Some(c.len()),
        _ => None,
    };
    kit::section_with(ui, "Checks", count, |ui| {
        if loading {
            ui.add_space(6.0);
            ui.add(egui::Spinner::new().size(12.0).color(p.text_3));
            ui.add_space(6.0);
        } else if IconButton::icon(Icon::Fetch)
            .compact()
            .show(ui)
            .on_hover_text("Refresh checks")
            .clicked()
        {
            actions.push(Action::RefreshChecks(sha));
        }
        if let Some(at) = state.and_then(|s| s.at) {
            ui.label(caption(
                &p,
                format!(
                    "refreshed {}",
                    format::relative(now - at.elapsed().as_secs() as i64, now)
                ),
            ));
        }
    });
    let list = match result {
        None => {
            if loading {
                kit::loading(ui, "Loading checks…");
            } else {
                kit::muted(ui, "Not loaded.");
            }
            return;
        }
        Some(Err(e)) => {
            kit::callout(ui, Tone::Danger, "Could not load the checks", e);
            return;
        }
        Some(Ok(list)) if list.is_empty() => {
            kit::muted(ui, "No checks reported for this commit.");
            return;
        }
        Some(Ok(list)) => list,
    };
    let bad = list.iter().filter(|c| c.outcome().is_bad()).count();
    let running = list.iter().filter(|c| c.outcome().is_running()).count();
    let good = list.iter().filter(|c| c.outcome().is_good()).count();
    if running > 0 {
        ui.ctx().request_repaint_after(Duration::from_millis(90));
    }
    if bad > 0 || running > 0 {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            if bad > 0 {
                kit::badge(ui, &format!("{bad} failed"), Tone::Danger);
            }
            if running > 0 {
                kit::badge(ui, &format!("{running} running"), Tone::Warning);
            }
            if good > 0 {
                kit::badge(ui, &format!("{good} passed"), Tone::Neutral);
            }
        });
        ui.add_space(2.0);
    }
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for c in list {
            check_row(ui, c, now, t, actions);
        }
    });
}

fn check_row(ui: &mut Ui, c: &CheckRun, now: i64, t: f64, actions: &mut Vec<Action>) {
    let actions_ids = c.actions_ids();
    let dur = c
        .duration_secs(now)
        .map(format::duration)
        .unwrap_or_else(|| outcome_text(c.outcome()).to_owned());
    let resp = line_row(
        ui,
        c.outcome(),
        &c.name,
        &c.app,
        &dur,
        actions_ids.is_none(),
        t,
    );
    if resp.clicked() {
        match actions_ids {
            Some((run, job)) => actions.push(Action::OpenRun {
                run,
                job: Some(job),
            }),
            None => {
                let url = if c.details_url.is_empty() {
                    c.html_url.clone()
                } else {
                    c.details_url.clone()
                };
                if !url.is_empty() {
                    actions.push(Action::OpenUrl(url));
                }
            }
        }
    }
    resp.context_menu(|ui| {
        ui.set_min_width(200.0);
        if actions_ids.is_some() && ui.button("Show job and log").clicked() {
            if let Some((run, job)) = actions_ids {
                actions.push(Action::OpenRun {
                    run,
                    job: Some(job),
                });
            }
            ui.close();
        }
        if !c.html_url.is_empty() && ui.button("Open on GitHub").clicked() {
            actions.push(Action::OpenUrl(c.html_url.clone()));
            ui.close();
        }
        if !c.details_url.is_empty() && ui.button("Open details URL").clicked() {
            actions.push(Action::OpenUrl(c.details_url.clone()));
            ui.close();
        }
        ui.separator();
        if ui.button("Copy name").clicked() {
            actions.push(Action::Copy(c.name.clone()));
            ui.close();
        }
        let summary = [c.title.as_deref(), c.summary.as_deref()]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty() && *s != "null")
            .collect::<Vec<_>>()
            .join("\n\n");
        if !summary.is_empty() && ui.button("Copy summary").clicked() {
            actions.push(Action::Copy(summary));
            ui.close();
        }
    });
}

// ---------------------------------------------------------------------------
// Pull requests
// ---------------------------------------------------------------------------

fn prs_tab(ui: &mut Ui, cx: &Cx, page: &mut GitHubPage, actions: &mut Vec<Action>) {
    list_pane(ui, "gh_prs", |ui| prs_list(ui, cx, page, actions));
    let Some(number) = page.selected_pr else {
        empty_pane(ui, |ui| {
            kit::empty_state(
                ui,
                Icon::PullRequest,
                "Select a pull request",
                "Its reviews, checks, workflow runs and description appear here.",
                None,
            );
        });
        return;
    };
    detail_pane(ui, ("gh_pr", number), |ui| {
        pr_detail(ui, cx, page, number, actions)
    });
}

fn prs_list(ui: &mut Ui, cx: &Cx, page: &mut GitHubPage, actions: &mut Vec<Action>) {
    let hint = format!(
        "Filter {}",
        format::plural(cx.data.prs.len(), "pull request", "pull requests")
    );
    search_field(ui, "gh_pr_filter", &mut page.pr_filter, &hint);
    ui.add_space(space::S);
    let needle = page.pr_filter.trim().to_lowercase();
    let prs: Vec<&PullRequest> = cx
        .data
        .prs
        .iter()
        .filter(|p| {
            needle.is_empty()
                || p.title.to_lowercase().contains(&needle)
                || p.author.to_lowercase().contains(&needle)
                || p.head.to_lowercase().contains(&needle)
                || p.base.to_lowercase().contains(&needle)
                || p.labels.iter().any(|l| l.to_lowercase().contains(&needle))
                || p.number.to_string() == needle.trim_start_matches('#')
        })
        .collect();
    if prs.is_empty() {
        ui.add_space(space::S);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            kit::muted(
                ui,
                if cx.data.prs.is_empty() {
                    "No open pull requests."
                } else {
                    "No pull requests match the filter."
                },
            );
        });
        return;
    }
    let selected = page.selected_pr;
    let p = *pal(ui);
    kit::list(
        ui,
        "gh_prs_list",
        prs.len(),
        kit::ROW_H,
        |i| selected == Some(prs[i].number),
        |ui, rect, resp, i| {
            let pr = prs[i];
            let subtitle = format!(
                "#{}  ·  {}  ·  {} → {}",
                pr.number, pr.author, pr.head, pr.base
            );
            let meta = pr
                .updated
                .map(|s| format::relative(s, cx.now))
                .unwrap_or_default();
            let badges = pr_badges(cx, page, pr, false);
            let color = if pr.draft { p.text_3 } else { p.text_2 };
            kit::paint_row(
                ui,
                rect,
                &kit::Row::new(&pr.title)
                    .leading(kit::Leading::Icon(Icon::PullRequest, color))
                    .subtitle(&subtitle)
                    .meta(&meta)
                    .badges(&badges),
                selected == Some(pr.number),
            );
            if resp.clicked() {
                actions.push(Action::OpenPr(pr.number));
            }
            resp.context_menu(|ui| pr_menu(ui, cx, pr, actions));
        },
    );
}

/// The newest review per reviewer.
fn latest_reviews(reviews: &[gitvis_core::github::Review]) -> Vec<&gitvis_core::github::Review> {
    let mut latest: Vec<&gitvis_core::github::Review> = Vec::new();
    for r in reviews.iter().rev() {
        if !latest.iter().any(|l| l.user == r.user) {
            latest.push(r);
        }
    }
    latest
}

/// Checks outcome of a PR's head: the loaded check runs, else the workflow
/// runs on that commit.
fn head_outcome(cx: &Cx, pr: &PullRequest) -> Option<Outcome> {
    let from_checks = Oid::from_str(&pr.head_sha)
        .ok()
        .and_then(|o| cx.checks.get(&o))
        .and_then(|s| match &s.result {
            Some(Ok(list)) if !list.is_empty() => Some(gitvis_core::github::aggregate(
                list.iter().map(|c| c.outcome()),
            )),
            _ => None,
        });
    from_checks.or_else(|| {
        let mut runs = cx.data.runs_for_sha(&pr.head_sha).peekable();
        runs.peek()?;
        Some(gitvis_core::github::aggregate(runs.map(|r| r.outcome())))
    })
}

/// Review state, failing or running CI, draft. `pending` adds "Review
/// requested", which the list leaves out to stay calm.
fn pr_badges(cx: &Cx, page: &GitHubPage, pr: &PullRequest, pending: bool) -> Vec<(String, Tone)> {
    let mut b = Vec::new();
    let reviews = page
        .prs
        .get(&pr.number)
        .and_then(|s| s.detail.as_ref())
        .and_then(|d| d.as_ref().ok())
        .map(|d| latest_reviews(&d.reviews));
    match reviews {
        Some(r) if r.iter().any(|r| r.state == "CHANGES_REQUESTED") => {
            b.push(("Changes requested".to_owned(), Tone::Danger));
        }
        Some(r) if r.iter().any(|r| r.state == "APPROVED") => {
            b.push(("Approved".to_owned(), Tone::Success));
        }
        _ if pending && !pr.reviewers.is_empty() => {
            b.push(("Review requested".to_owned(), Tone::Neutral))
        }
        _ => {}
    }
    match head_outcome(cx, pr) {
        Some(o) if o.is_bad() => b.push(("CI failing".to_owned(), Tone::Danger)),
        Some(o) if o.is_running() => b.push(("CI running".to_owned(), Tone::Warning)),
        _ => {}
    }
    if pr.draft {
        b.push(("Draft".to_owned(), Tone::Neutral));
    }
    b
}

fn pr_menu(ui: &mut Ui, cx: &Cx, pr: &PullRequest, actions: &mut Vec<Action>) {
    ui.set_min_width(220.0);
    if ui.button("Show pull request").clicked() {
        actions.push(Action::OpenPr(pr.number));
        ui.close();
    }
    if ui
        .add_enabled(!cx.busy, egui::Button::new("Check out PR branch"))
        .on_hover_text("gh pr checkout")
        .clicked()
    {
        actions.push(Action::Run(GitOp::GhPrCheckout { number: pr.number }));
        ui.close();
    }
    if ui.button("Open on GitHub").clicked() {
        actions.push(Action::OpenUrl(pr.url.clone()));
        ui.close();
    }
    ui.separator();
    if ui.button("Copy URL").clicked() {
        actions.push(Action::Copy(pr.url.clone()));
        ui.close();
    }
    if ui.button("Copy head SHA").clicked() {
        actions.push(Action::Copy(pr.head_sha.clone()));
        ui.close();
    }
}

fn pr_detail(ui: &mut Ui, cx: &Cx, page: &GitHubPage, number: u64, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let Some(pr) = cx.data.pr(number) else {
        kit::callout(
            ui,
            Tone::Neutral,
            "Pull request closed",
            &format!("#{number} is no longer open."),
        );
        return;
    };
    let state = page.prs.get(&number);
    let head_oid = Oid::from_str(&pr.head_sha).ok();
    let in_graph = head_oid.filter(|o| cx.snap.is_some_and(|s| s.index_of.contains_key(o)));

    title_row(
        ui,
        &pr.title,
        |ui, r| {
            icons::paint(
                ui.painter(),
                Icon::PullRequest,
                r.center(),
                16.0,
                if pr.draft { p.text_3 } else { p.text_2 },
            )
        },
        |ui| {
            if IconButton::icon(Icon::External)
                .compact()
                .show(ui)
                .on_hover_text("Open on GitHub")
                .clicked()
            {
                actions.push(Action::OpenUrl(pr.url.clone()));
            }
            if IconButton::icon(Icon::Copy)
                .compact()
                .show(ui)
                .on_hover_text("Copy the URL")
                .clicked()
            {
                actions.push(Action::Copy(pr.url.clone()));
            }
            if IconButton::new(Icon::Branch, "Check out")
                .compact()
                .primary(true)
                .enabled(!cx.busy)
                .show(ui)
                .on_hover_text("gh pr checkout: fetch the PR branch and switch to it")
                .clicked()
            {
                actions.push(Action::Run(GitOp::GhPrCheckout { number: pr.number }));
            }
            if let Some(oid) = in_graph
                && IconButton::new(Icon::Graph, "Show in graph")
                    .compact()
                    .show(ui)
                    .on_hover_text("Select the head commit on the History page")
                    .clicked()
            {
                actions.push(Action::JumpTo(oid));
            }
        },
    );
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
        ui.add_space(28.0);
        ui.label(kit::mono(format!("#{}", pr.number)).color(p.text_2));
        if pr.draft {
            kit::badge(ui, "Draft", Tone::Neutral);
        } else {
            kit::badge(ui, "Open", Tone::Neutral);
        }
        for (text, tone) in pr_badges(cx, page, pr, true)
            .into_iter()
            .filter(|(t, _)| t != "Draft")
        {
            kit::badge(ui, &text, tone);
        }
    });

    kit::section(ui, "Details", None);
    let detail = state.and_then(|s| s.detail.as_ref());
    kit::kv(ui, 92.0, |kv| {
        kv.row("Author", |ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                crate::views::avatar(ui.painter(), r.center(), 9.0, &pr.author, cx.dark);
                ui.label(RichText::new(&pr.author).color(p.text));
            });
        });
        kv.row("Branches", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
                kit::badge(ui, &pr.head, Tone::Neutral);
                ui.label(small(&p, "into"));
                kit::badge(ui, &pr.base, Tone::Neutral);
                if let Some(repo) = &pr.head_repo
                    && *repo != format!("{}/{}", cx.data.owner, cx.data.repo)
                {
                    ui.label(small(&p, format!("from the fork {repo}")));
                }
            });
        });
        if !pr.head_sha.is_empty() {
            kv.row("Head", |ui| {
                let r = link_text(ui, short_sha(&pr.head_sha), true).on_hover_text(
                    if in_graph.is_some() {
                        "Show in the graph"
                    } else {
                        "Copy the SHA (not in the graph; fetch the PR to see it)"
                    },
                );
                if r.clicked() {
                    match in_graph {
                        Some(oid) => actions.push(Action::JumpTo(oid)),
                        None => actions.push(Action::Copy(pr.head_sha.clone())),
                    }
                }
            });
        }
        if let Some(c) = pr.created {
            kv.text("Opened", when(c, cx.now));
        }
        if let Some(u) = pr.updated {
            kv.text("Updated", when(u, cx.now));
        }
        if !pr.labels.is_empty() {
            kv.row("Labels", |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                    for l in &pr.labels {
                        kit::badge(ui, l, Tone::Neutral);
                    }
                });
            });
        }
        if let Some(m) = &pr.milestone {
            kv.text("Milestone", m.as_str());
        }
        if !pr.assignees.is_empty() {
            kv.text("Assignees", pr.assignees.join(", "));
        }
        if !pr.reviewers.is_empty() {
            kv.text("Reviewers", pr.reviewers.join(", "));
        }
        if let Some(Ok(d)) = detail {
            kv.row("Changes", |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(
                        RichText::new(format!("+{}", format::count(d.additions as usize)))
                            .color(p.success),
                    );
                    ui.label(
                        RichText::new(format!("\u{2212}{}", format::count(d.deletions as usize)))
                            .color(p.danger),
                    );
                    ui.label(small(
                        &p,
                        format!(
                            "in {}",
                            format::plural(d.changed_files as usize, "file", "files")
                        ),
                    ));
                });
            });
            kv.text("Commits", d.commits.to_string());
            kv.text(
                "Comments",
                format!(
                    "{} in the conversation, {} on the code",
                    d.comments, d.review_comments
                ),
            );
        }
    });

    match detail {
        None => {
            ui.add_space(space::S);
            kit::loading(ui, "Loading size, merge state and reviews…");
        }
        Some(Err(e)) => {
            ui.add_space(space::M);
            kit::callout(ui, Tone::Danger, "Could not load the pull request", e);
        }
        Some(Ok(d)) => {
            ui.add_space(space::M);
            let (tone, title, body) = match (d.mergeable, d.mergeable_state.as_str()) {
                (Some(true), "clean") => (
                    Tone::Success,
                    "Ready to merge",
                    "No conflicts with the base branch, and the required checks pass.",
                ),
                (Some(true), "unstable") => (
                    Tone::Warning,
                    "Checks are failing",
                    "It merges cleanly, but some checks did not pass.",
                ),
                (Some(true), "blocked") => (
                    Tone::Warning,
                    "Blocked",
                    "Required reviews or checks are missing.",
                ),
                (Some(true), "behind") => (
                    Tone::Warning,
                    "Behind the base branch",
                    "The base branch has commits this branch does not have yet.",
                ),
                (Some(false), _) => (
                    Tone::Danger,
                    "Conflicts with the base branch",
                    "The conflicts must be resolved before it can be merged.",
                ),
                (_, "draft") => (
                    Tone::Neutral,
                    "Draft",
                    "Marked as a draft: not ready to be merged.",
                ),
                (_, s) if !s.is_empty() => (Tone::Neutral, "Merge state", s),
                _ => (
                    Tone::Neutral,
                    "Mergeability unknown",
                    "GitHub is still computing it; refresh in a moment.",
                ),
            };
            kit::callout(ui, tone, title, body);

            let latest = latest_reviews(&d.reviews);
            kit::section(ui, "Reviews", (!latest.is_empty()).then_some(latest.len()));
            if latest.is_empty() {
                kit::muted(ui, "No reviews yet.");
            }
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for r in latest {
                    review_row(ui, cx, r);
                }
            });
        }
    }

    // Checks and runs on the head commit.
    if let Some(oid) = head_oid {
        checks_section(ui, oid, cx.checks.get(&oid), cx.now, actions);
    }
    let runs: Vec<&WorkflowRun> = cx.data.runs_for_sha(&pr.head_sha).collect();
    if runs.iter().any(|r| r.is_running()) {
        ui.ctx().request_repaint_after(Duration::from_millis(90));
    }
    if !runs.is_empty() {
        kit::section(ui, "Workflow runs on the head commit", Some(runs.len()));
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for run in runs {
                let title = format!("{} #{}", run.name, run.run_number);
                let secondary = run
                    .started
                    .or(run.created)
                    .map(|s| format::relative(s, cx.now))
                    .unwrap_or_default();
                let dur = run
                    .duration_secs(cx.now)
                    .map(format::duration)
                    .unwrap_or_else(|| outcome_text(run.outcome()).to_owned());
                let resp = line_row(ui, run.outcome(), &title, &secondary, &dur, false, cx.t);
                if resp.clicked() {
                    actions.push(Action::OpenRun {
                        run: run.id,
                        job: None,
                    });
                }
                resp.context_menu(|ui| run_menu(ui, cx, run, actions));
            }
        });
    }

    kit::section(ui, "Description", None);
    let body = pr.body.trim();
    if body.is_empty() {
        kit::muted(ui, "No description.");
    } else {
        let shown: String = body.chars().take(20_000).collect();
        kit::body(ui, &shown);
        if body.chars().count() > 20_000 {
            kit::muted(
                ui,
                "Truncated; open the pull request on GitHub for the rest.",
            );
        }
    }
}

fn review_row(ui: &mut Ui, cx: &Cx, r: &gitvis_core::github::Review) {
    let p = *pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), LINE_ROW_H), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    let mid = rect.center().y;
    crate::views::avatar(
        painter,
        pos2(rect.left() + 15.0, mid),
        9.0,
        &r.user,
        cx.dark,
    );
    let (label, tone) = match r.state.as_str() {
        "APPROVED" => ("Approved", Tone::Success),
        "CHANGES_REQUESTED" => ("Changes requested", Tone::Danger),
        "COMMENTED" => ("Commented", Tone::Neutral),
        "DISMISSED" => ("Dismissed", Tone::Neutral),
        "PENDING" => ("Pending", Tone::Warning),
        other => (other, Tone::Neutral),
    };
    let mut right = rect.right() - 10.0;
    if let Some(s) = r.submitted {
        let t = painter.text(
            pos2(right, mid),
            Align2::RIGHT_CENTER,
            format::relative(s, cx.now),
            FontId::proportional(size::SMALL),
            p.text_3,
        );
        right = t.left() - space::M;
    }
    let g = truncated(
        painter,
        &r.user,
        FontId::proportional(size::BODY - 0.5),
        p.text,
        (right - rect.left() - 140.0).max(40.0),
    );
    let name_w = g.size().x;
    painter.galley(pos2(rect.left() + 32.0, mid - g.size().y / 2.0), g, p.text);
    let w = kit::badge_width(painter, label);
    kit::paint_badge(
        painter,
        &p,
        ui.visuals().dark_mode,
        rect.left() + 32.0 + name_w + 10.0 + w,
        mid,
        label,
        tone,
    );
}
