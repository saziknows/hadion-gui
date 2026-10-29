//! Actions page: workflow runs with their jobs, steps, annotations and full
//! logs (the `Runs` tab); the other tabs (workflows, artifacts, caches,
//! validation, local runs) are drawn by `devops::ci::show_tab`. Everything
//! comes through `gh api`. Also the commit checks section shared with the
//! details panel ([`checks_section`]). Pull requests live on the Review page
//! (`devops::review`).

use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Label, Layout, Margin, Rect, Response,
    RichText, Sense, Stroke, Ui, pos2, vec2,
};

use hadion_core::github::{
    Annotation, CheckRun, GitHubData, Job, NotOnGitHub, Outcome, RateLimit, WorkflowRun,
};
use hadion_core::joblog::{LogKind, LogLine};
use hadion_core::{Oid, Snapshot};

use crate::devops::{Console, ci};
use crate::format;
use crate::state::{
    Action, ChecksState, GitHubPage, LogState, RepoState, RunDetail, RunFilter, Settings,
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
    /// For the log viewer's extension hook (`devops::hooks`).
    log_hook: &'a crate::devops::hooks::LogHookCx<'a>,
    data: &'a GitHubData,
    snap: Option<&'a Snapshot>,
    now: i64,
    /// `ui.input().time`, for the running-arc animation.
    t: f64,
    dark: bool,
    /// The API budget, so watching a running job can slow down or pause.
    rate: Option<RateLimit>,
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

pub fn show(
    ui: &mut Ui,
    repo: &mut RepoState,
    console: &Console,
    settings: &Settings,
    actions: &mut Vec<Action>,
) {
    let tab = repo.devops.ci.tab;
    let Some((owner, name)) = repo.github_remote.clone() else {
        return;
    };
    let now = format::now_secs();
    page_head(
        ui,
        &owner,
        &name,
        repo.github_loading,
        repo.github_at,
        repo.rate.as_ref(),
        tab,
        now,
        actions,
    );
    if tab != ci::Tab::Runs {
        ci::show_tab(ui, repo, tab, console, settings, actions);
        return;
    }
    let compare_open = repo
        .devops
        .ci
        .compare
        .as_ref()
        .is_some_and(|c| Some(c.bad) == repo.gh_page.selected_run);
    {
        let RepoState {
            github,
            github_remote,
            github_error,
            gh_page,
            snapshot,
            devops,
            rate,
            ..
        } = &mut *repo;
        if github_remote.is_none() {
            return;
        }
        let t = ui.input(|i| i.time);

        if let Some(err) = github_error.as_deref()
            && github.is_none()
        {
            empty_pane(ui, |ui| {
                let body = format!(
                    "{err}\n\nhadion talks to GitHub through the gh CLI: run `gh auth login` in a terminal, then retry."
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
                    ui.set_max_width(ui.available_width().min(320.0));
                    kit::loading(ui, "Loading pull requests and workflow runs…");
                });
            });
            return;
        };
        // The AI state and the CI state are disjoint fields.
        let (ai_state, ci) = (&devops.ai, &mut devops.ci);
        let log_hook = crate::devops::hooks::LogHookCx {
            settings,
            ai: ai_state,
        };
        let cx = Cx {
            log_hook: &log_hook,
            data,
            snap: snapshot.as_ref(),
            now,
            t,
            dark: settings.dark,
            rate: *rate,
        };
        list_pane(ui, "gh_runs", |ui| runs_list(ui, &cx, gh_page, ci, actions));
        if !compare_open {
            actions_tab(ui, &cx, gh_page, ci, actions);
            return;
        }
    }
    ci::show_compare(ui, repo, actions);
}

#[allow(clippy::too_many_arguments)]
fn page_head(
    ui: &mut Ui,
    owner: &str,
    name: &str,
    loading: bool,
    at: Option<std::time::Instant>,
    rate: Option<&RateLimit>,
    tab: ci::Tab,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let title = "Actions";
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
    let options: Vec<(ci::Tab, &str)> = ci::Tab::ALL.iter().map(|t| (*t, t.label())).collect();
    // Tabs, two icon buttons and their gaps.
    let controls_w = segmented_width(ui, &options) + 2.0 * 30.0 + 16.0;
    let (subtitle, dropped) = fit_subtitle(ui, title, &sub, controls_w);
    kit::page_header(ui, title, &subtitle, |ui| {
        let what = match tab {
            ci::Tab::Runs => "Refresh pull requests, runs and the selected run's jobs",
            ci::Tab::Workflows => {
                "Re-read the workflow files, refresh the runs and the selected workflow's history"
            }
            ci::Tab::Artifacts => "Reload the artifact list",
            ci::Tab::Caches => "Reload the cache usage and entries",
            ci::Tab::Validate => "Check the workflow files again",
            ci::Tab::Local => "Look for act and Docker again and re-read the workflow files",
        };
        // What the subtitle had no room for.
        let tip = if dropped.is_empty() {
            what.to_owned()
        } else {
            format!("{what}\n\n{}", dropped.join("\n"))
        };
        if loading && tab == ci::Tab::Runs {
            ui.add_space(6.0);
            ui.add(egui::Spinner::new().size(14.0));
            ui.add_space(6.0);
        } else if IconButton::icon(Icon::Fetch)
            .show(ui)
            .on_hover_text(tip)
            .clicked()
        {
            actions.push(if tab == ci::Tab::Runs {
                Action::RefreshGitHub
            } else {
                ci::Act::Refresh(tab).into()
            });
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
        let w = segmented_width(ui, &options);
        ui.allocate_ui_with_layout(vec2(w, 28.0), Layout::left_to_right(Align::Center), |ui| {
            if let Some(t) = kit::segmented(ui, &options, tab) {
                actions.push(ci::Act::SetTab(t).into());
            }
        });
    });
}

/// A section whose body is folded away until its header is clicked: the
/// header looks like `kit::section`'s caption, with a chevron. For the
/// depth behind a page ("Where this comes from", "What is checked"), so
/// the page leads with what matters. `open` is the state before the first
/// click (a script can ask for it open).
pub(crate) fn details_section(
    ui: &mut Ui,
    id_salt: impl Hash + std::fmt::Debug,
    title: &str,
    open: bool,
    add: impl FnOnce(&mut Ui),
) {
    let p = *pal(ui);
    let id = ui.make_persistent_id(("details_section", id_salt));
    let mut is_open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(open);
    ui.add_space(space::XL);
    let font = FontId::new(size::CAPTION, medium());
    let g = ui
        .painter()
        .layout_no_wrap(title.to_uppercase(), font, p.text_3);
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 26.0, 20.0), Sense::click());
    let c = if resp.hovered() { p.text } else { p.text_3 };
    chevron(
        ui.painter(),
        pos2(rect.left() + 6.0, rect.center().y),
        is_open,
        c,
    );
    ui.painter().galley(
        pos2(rect.left() + 18.0, rect.center().y - g.size().y / 2.0),
        g,
        c,
    );
    if resp
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        is_open = !is_open;
    }
    ui.data_mut(|d| d.insert_temp(id, is_open));
    ui.add_space(space::S - 2.0);
    if is_open {
        add(ui);
    }
}

/// A page header's subtitle that fits beside `controls_w` points of
/// controls on the right (`kit::page_header` puts them side by side, and
/// they overlapped on a narrow window). Parts are dropped from the end;
/// the first (usually the repository) always stays. Returns the subtitle
/// and the dropped parts, for a tooltip.
pub(crate) fn fit_subtitle(
    ui: &Ui,
    title: &str,
    parts: &[String],
    controls_w: f32,
) -> (String, Vec<String>) {
    // The header's frame has 24 + 16 points of margin; keep a gap of 24.
    let room = ui.available_width() - 40.0 - 24.0 - controls_w;
    let painter = ui.painter();
    let width = |s: &str, size: f32| {
        painter
            .layout_no_wrap(s.to_owned(), FontId::proportional(size), Color32::WHITE)
            .size()
            .x
    };
    let title_w = width(title, size::HEADING);
    let mut keep = parts.len();
    while keep > 1 {
        let s = parts[..keep].join("  ·  ");
        if width(&s, size::SMALL).max(title_w) <= room {
            break;
        }
        keep -= 1;
    }
    (parts[..keep].join("  ·  "), parts[keep..].to_vec())
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

/// The list pane on the left (same frame as `kit::split`). It never takes
/// more than 45% of the page, so the detail keeps room on a narrow window.
fn list_pane(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    let w = ui.available_width();
    egui::Panel::left(id)
        .resizable(true)
        .default_size((w * 0.36).clamp(300.0, 420.0))
        .size_range(300.0..=(w * 0.45).clamp(300.0, 760.0))
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
                            ui.set_max_width(ui.available_width().min(kit::DETAIL_MAX));
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

fn actions_tab(
    ui: &mut Ui,
    cx: &Cx,
    page: &mut GitHubPage,
    ci: &mut ci::State,
    actions: &mut Vec<Action>,
) {
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

    // The job in the viewer: while it runs, keep its progress fresh; once
    // a watched job has finished, download its log.
    if let Some(job) = detail
        .selected_job
        .and_then(|id| detail.jobs().iter().find(|j| j.id == id))
    {
        watch_tick(ui, cx, run_id, job, detail, ci, actions);
    }

    // Log viewer at the bottom, when a job is selected.
    if let Some(job_id) = detail.selected_job {
        let watch = ci
            .watch
            .as_ref()
            .filter(|w| w.run == run_id && w.job == job_id);
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
                    watch,
                    &mut filter,
                    &mut timestamps,
                    actions,
                );
                page.log_filter = filter;
                page.show_timestamps = timestamps;
            });
    }

    // A run's artifacts are fetched once, when it is first shown.
    if ci.run_artifacts.get(&run_id).is_none_or(|l| l.unloaded()) {
        actions.push(
            ci::Act::LoadRunArtifacts {
                run: run_id,
                force: false,
            }
            .into(),
        );
    }
    let ci = &*ci;
    detail_pane(ui, ("gh_run", run_id), |ui| {
        run_header(ui, cx, run, detail, ci, actions);
        jobs_section(ui, cx, run, detail, ci, actions);
        ci::runs::artifacts_section(
            ui,
            run,
            ci.run_artifacts.get(&run_id),
            &ci.downloads,
            cx.now,
            actions,
        );
        run_facts(ui, cx, run, detail, actions);
    });
}

/// What a job's Log button (or a click on one of its steps) asks for. A
/// finished job's log is downloaded. A job GitHub has no log for yet
/// (running, queued) or at all (never ran) is only selected: requesting
/// its log would fail with HTTP 404, and the viewer shows its progress.
pub fn show_log_action(run: u64, job: &Job, step: Option<u32>) -> Action {
    if ci::no_log_yet(job) {
        ci::Act::OpenJob { run, job: job.id }.into()
    } else {
        Action::ShowLog {
            run,
            job: job.id,
            step,
        }
    }
}

/// Fewer than this many API requests left: no more progress reads.
fn watch_paused(rate: Option<RateLimit>) -> bool {
    rate.is_some_and(|r| r.remaining < ci::POLL_FLOOR)
}

/// The job shown in the log viewer, once per frame. A running or queued
/// job's progress is read again when its interval is up (only while this
/// view is on screen, so a hidden page costs nothing); a watched job that
/// has finished gets its log downloaded.
fn watch_tick(
    ui: &Ui,
    cx: &Cx,
    run: u64,
    job: &Job,
    detail: &RunDetail,
    ci: &ci::State,
    actions: &mut Vec<Action>,
) {
    if !job.outcome().is_running() {
        // Not asked for yet; or asked for while it ran (another way in
        // than this view requested it), which GitHub answered with 404,
        // and this view watched it finish.
        let watched = ci
            .watch
            .as_ref()
            .is_some_and(|w| w.run == run && w.job == job.id);
        let waiting = match detail.logs.get(&job.id) {
            Some(LogState {
                loading: false,
                doc: None,
            }) => true,
            Some(LogState {
                loading: false,
                doc: Some(Err(e)),
            }) => watched && hadion_core::github::is_not_found(e),
            _ => false,
        };
        if waiting && !job.never_ran() {
            actions.push(
                ci::Act::FetchLog {
                    run,
                    job: job.id,
                    attempt: 0,
                }
                .into(),
            );
        }
        return;
    }
    let w = ci
        .watch
        .as_ref()
        .filter(|w| w.run == run && w.job == job.id);
    if detail.jobs_loading || w.is_some_and(|w| w.in_flight) || watch_paused(cx.rate) {
        return;
    }
    // A tight budget: the slowest pace.
    let low = cx.rate.is_some_and(|r| r.remaining < r.limit / 10);
    let interval = match w {
        _ if low => ci::POLL_MAX,
        Some(w) => w.interval,
        None => ci::POLL_MIN,
    };
    let last = w.map(|w| w.polled).or(detail.jobs_at);
    let now = std::time::Instant::now();
    match last.map(|t| t + interval) {
        Some(due) if due > now => ui.ctx().request_repaint_after(due - now),
        _ => actions.push(ci::Act::PollJob { run, job: job.id }.into()),
    }
}

/// The pick box at the left of a run row.
fn pick_box(ui: &Ui, rect: Rect, picked: bool, hovered: bool) {
    let p = *pal(ui);
    let bx = Rect::from_center_size(
        pos2(rect.left() + 17.0, rect.top() + 18.0),
        vec2(14.0, 14.0),
    );
    if picked {
        ui.painter()
            .rect_filled(bx, CornerRadius::same(crate::theme::radius::S), p.accent);
        icons::paint(ui.painter(), Icon::Check, bx.center(), 10.0, Color32::WHITE);
    } else {
        ui.painter().rect(
            bx,
            CornerRadius::same(crate::theme::radius::S),
            Color32::TRANSPARENT,
            Stroke::new(1.0, if hovered { p.text_3 } else { p.border_strong }),
            egui::StrokeKind::Inside,
        );
    }
}

fn runs_list(
    ui: &mut Ui,
    cx: &Cx,
    page: &mut GitHubPage,
    ci: &mut ci::State,
    actions: &mut Vec<Action>,
) {
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
    ci::runs::bulk_bar(ui, &ci.picked, &cx.data.runs, actions);

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
    let picking = !ci.picked.is_empty();
    let picked = &ci.picked;
    let busy_runs = &ci.busy_runs;
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
            let mut badges = run_badges(run);
            if let Some(b) = busy_runs.get(&run.id) {
                badges.insert(0, (short_busy(b), Tone::Accent));
            }
            let is_picked = picked.contains(&run.id);
            let hovered = ui.rect_contains_pointer(rect);
            // A pick column on the left; the row content moves right of it.
            let content = Rect::from_min_max(pos2(rect.left() + 24.0, rect.top()), rect.max);
            kit::paint_row(
                ui,
                content,
                &kit::Row::new(run_title(run))
                    .leading(kit::Leading::Dot(Color32::TRANSPARENT))
                    .subtitle(&subtitle)
                    .meta(&meta)
                    .badges(&badges),
                selected == Some(run.id),
            );
            if is_picked || picking || hovered {
                pick_box(ui, rect, is_picked, hovered);
            }
            outcome_dot(
                ui.painter(),
                run.outcome(),
                pos2(content.left() + 19.0, rect.top() + 18.0),
                6.0,
                cx.t,
            );
            if resp.clicked() {
                let on_box = resp
                    .interact_pointer_pos()
                    .is_some_and(|p| p.x < rect.left() + 30.0);
                if on_box {
                    actions.push(
                        ci::Act::Pick {
                            run: run.id,
                            mode: ci::PickMode::Toggle,
                        }
                        .into(),
                    );
                } else if !ci::runs::click_pick(ui, run.id, actions) {
                    actions.push(Action::OpenRun {
                        run: run.id,
                        job: None,
                    });
                }
            }
            resp.context_menu(|ui| {
                run_menu(
                    ui,
                    cx,
                    run,
                    is_picked,
                    busy_runs.contains_key(&run.id),
                    actions,
                )
            });
        },
    );
}

/// `Cancel 3 runs` → `Cancelling…` style badge text for a change in flight.
fn short_busy(label: &str) -> String {
    let l = label.to_ascii_lowercase();
    if l.starts_with("cancel") || l.starts_with("force") {
        "Cancelling…".to_owned()
    } else if l.starts_with("re-run") {
        "Re-running…".to_owned()
    } else if l.starts_with("delete") {
        "Deleting…".to_owned()
    } else {
        "Working…".to_owned()
    }
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

fn run_menu(
    ui: &mut Ui,
    cx: &Cx,
    run: &WorkflowRun,
    picked: bool,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    ui.set_min_width(280.0);
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
    ci::runs::run_menu(ui, run, &cx.data.runs, busy, picked, actions);
    ui.separator();
    if ui.button("Copy run URL").clicked() {
        actions.push(Action::Copy(run.url.clone()));
        ui.close();
    }
    if ui.button("Copy commit SHA").clicked() {
        actions.push(Action::Copy(run.sha.clone()));
        ui.close();
    }
    if ui.button("Copy run id").clicked() {
        actions.push(Action::Copy(run.id.to_string()));
        ui.close();
    }
}

/// Narrower than this, a title moves above its buttons.
const TITLE_MIN_W: f32 = 260.0;

/// Title with the outcome icon on the left and `buttons` on the right.
/// When the buttons would leave the title less than [`TITLE_MIN_W`] (a
/// failed run's five buttons on a narrow window left it one letter per
/// line), the buttons go under the title instead. Their width is taken
/// from the previous frame.
fn title_row(
    ui: &mut Ui,
    id_salt: impl Hash + std::fmt::Debug,
    title: &str,
    icon: impl FnOnce(&mut Ui, Rect),
    buttons: impl FnOnce(&mut Ui),
) {
    let id = egui::Id::unique(("title_row_buttons", id_salt));
    let buttons_w: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(0.0);
    if ui.available_width() - buttons_w - space::M < TITLE_MIN_W {
        title_heading(ui, title, icon);
        // Under the title, right-aligned, in the same order as beside it.
        let w = ui
            .allocate_ui_with_layout(
                vec2(ui.available_width(), 28.0),
                Layout::right_to_left(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    ui.scope(|ui| buttons(ui)).response.rect.width()
                },
            )
            .inner;
        ui.data_mut(|d| d.insert_temp(id, w));
        return;
    }
    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let w = ui.scope(|ui| buttons(ui)).response.rect.width();
        ui.data_mut(|d| d.insert_temp(id, w));
        ui.add_space(space::M);
        title_heading(ui, title, icon);
    });
}

/// The icon and the wrapping title of a [`title_row`].
fn title_heading(ui: &mut Ui, title: &str, icon: impl FnOnce(&mut Ui, Rect)) {
    ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (r, _) = ui.allocate_exact_size(vec2(20.0, 22.0), Sense::hover());
        icon(ui, r);
        kit::title(ui, title);
    });
}

fn run_header(
    ui: &mut Ui,
    cx: &Cx,
    run: &WorkflowRun,
    detail: &RunDetail,
    ci: &ci::State,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let outcome = run.outcome();
    let busy = ci.busy_runs.get(&run.id);
    title_row(
        ui,
        ("gh_run_title", run.id),
        run_title(run),
        |ui, r| outcome_dot(ui.painter(), outcome, r.center(), 8.0, cx.t),
        |ui| {
            let more = IconButton::new(Icon::ChevronDown, "More")
                .compact()
                .show(ui)
                .on_hover_text("Debug re-runs, force cancel, delete logs, delete run");
            egui::Popup::menu(&more)
                .id(egui::Id::unique(("gh_run_more", run.id)))
                .show(|ui| ci::runs::more_menu(ui, run, busy.is_some(), actions));
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
            if let Some(label) = busy {
                ui.add_space(4.0);
                ui.label(small(&p, short_busy(label)));
                ui.add(egui::Spinner::new().size(13.0));
                ui.add_space(4.0);
            } else if run.is_running() {
                if IconButton::new(Icon::Stop, "Cancel")
                    .compact()
                    .danger()
                    .show(ui)
                    .on_hover_text("POST actions/runs/{id}/cancel")
                    .clicked()
                {
                    actions.push(
                        ci::Act::AskCancel {
                            run: run.id,
                            force: false,
                        }
                        .into(),
                    );
                }
            } else {
                if IconButton::new(Icon::Rerun, "Re-run all")
                    .compact()
                    .show(ui)
                    .on_hover_text("POST actions/runs/{id}/rerun")
                    .clicked()
                {
                    actions.push(
                        ci::Act::AskRerun {
                            run: run.id,
                            failed_only: false,
                            debug: false,
                        }
                        .into(),
                    );
                }
                if (run.is_failure() || outcome == Outcome::Cancelled)
                    && IconButton::new(Icon::Rerun, "Re-run failed")
                        .compact()
                        .show(ui)
                        .on_hover_text("POST actions/runs/{id}/rerun-failed-jobs")
                        .clicked()
                {
                    actions.push(
                        ci::Act::AskRerun {
                            run: run.id,
                            failed_only: true,
                            debug: false,
                        }
                        .into(),
                    );
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
    if run.is_failure() {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add_space(28.0);
            if IconButton::new(Icon::Chart, "Compare with the last green run")
                .compact()
                .show(ui)
                .on_hover_text(
                    "Commits, files and a normalised log diff against the newest earlier passing run of this workflow on this branch",
                )
                .clicked()
            {
                actions.push(ci::Act::CompareOpen(run.id).into());
            }
        });
    }
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
    ci: &ci::State,
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
    ui.scope(|ui| jobs_rows(ui, cx, run, &jobs, detail, ci, actions));
}

fn jobs_rows(
    ui: &mut Ui,
    cx: &Cx,
    run: &WorkflowRun,
    jobs: &[Job],
    detail: &mut RunDetail,
    ci: &ci::State,
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
                .on_hover_text(if job.outcome().is_running() {
                    "Show this job's progress below (its log comes when it finishes)"
                } else {
                    "Show this job's log below"
                });
            if btn.clicked() {
                actions.push(show_log_action(run.id, job, None));
            }
            let right = btn.rect.left() - space::M;
            job_row(ui, cx, job, rect, right, expanded, selected);
        }
        if resp.clicked() {
            toggles.push(job.id);
        }
        resp.context_menu(|ui| {
            ui.set_min_width(260.0);
            ci::runs::job_menu(
                ui,
                run,
                job,
                ci::runs::job_busy(ci, job.id) || ci.busy_runs.contains_key(&run.id),
                actions,
            );
            if ui.button("Show log").clicked() {
                actions.push(show_log_action(run.id, job, None));
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
    // GitHub names its hosted runners "GitHub Actions 1002946877": noise.
    // A self-hosted runner's name says where the job ran.
    if let Some(r) = job.runner.as_ref().filter(|r| !is_hosted_runner_name(r)) {
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

/// `GitHub Actions 1002946877`: the name GitHub gives each hosted runner.
fn is_hosted_runner_name(name: &str) -> bool {
    name.strip_prefix("GitHub Actions ")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// A step's outcome dot. A step that has not started yet gets a quiet grey
/// ring: the amber "queued" ring made a running job's later steps look
/// like warnings.
fn step_dot(painter: &egui::Painter, p: &Palette, o: Outcome, center: egui::Pos2, t: f64) {
    if o == Outcome::Queued {
        painter.circle_stroke(center, 4.5, Stroke::new(1.2, p.text_3));
    } else {
        outcome_dot(painter, o, center, 5.0, t);
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
        step_dot(painter, &p, outcome, pos2(x0 + 5.0, mid), cx.t);
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
            actions.push(show_log_action(run.id, job, Some(step.number)));
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

/// What the log panel shows for the selected job.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    /// The downloaded log.
    Log,
    /// Running or queued: GitHub publishes the log when the job finishes,
    /// so the panel shows the job's progress.
    Live,
    /// The job never ran, so it has no log.
    NeverRan,
    /// Downloading, not asked for, or the download failed.
    Pending,
}

fn panel_for(job: Option<&Job>, state: Option<&LogState>) -> Panel {
    if matches!(
        state,
        Some(LogState {
            doc: Some(Ok(_)),
            ..
        })
    ) {
        return Panel::Log;
    }
    match job {
        Some(j) if j.outcome().is_running() => Panel::Live,
        Some(j) if j.never_ran() => Panel::NeverRan,
        _ => Panel::Pending,
    }
}

#[allow(clippy::too_many_arguments)]
fn log_viewer(
    ui: &mut Ui,
    cx: &Cx,
    run: &WorkflowRun,
    detail: &mut RunDetail,
    job_id: u64,
    watch: Option<&ci::JobWatch>,
    filter: &mut String,
    show_timestamps: &mut bool,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let job = detail.jobs().iter().find(|j| j.id == job_id).cloned();
    let panel = panel_for(job.as_ref(), detail.logs.get(&job_id));
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

    // Toolbar. The controls on the right are laid out first; the job's
    // name and numbers get what is left and are cut, never overlapped. On a
    // narrow panel (the log tools leave the name a few letters) the name
    // gets a row of its own above the controls.
    let log_tools = panel == Panel::Log;
    let own_row = log_tools && ui.available_width() < 680.0;
    Frame::NONE
        .inner_margin(Margin {
            left: 16,
            right: 10,
            top: 6,
            bottom: 6,
        })
        .show(ui, |ui| {
            if own_row {
                ui.allocate_ui_with_layout(
                    vec2(ui.available_width(), 24.0),
                    Layout::left_to_right(Align::Center),
                    |ui| log_title(ui, &job_name, doc_stats, !filter.trim().is_empty()),
                );
            }
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), 28.0),
                Layout::right_to_left(Align::Center),
                |ui| {
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
                    if log_tools
                        && IconButton::icon(Icon::Copy)
                            .compact()
                            .show(ui)
                            .on_hover_text("Copy the shown lines")
                            .clicked()
                    {
                        copy_all = true;
                    }
                    // "Explain this failure" and other areas' tools: they
                    // need a log (or a failed download to retry).
                    if matches!(panel, Panel::Log | Panel::Pending) {
                        crate::devops::hooks::log_viewer_tools(
                            ui,
                            cx.log_hook,
                            run.id,
                            job_id,
                            actions,
                        );
                    }
                    if log_tools {
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
                        let reserve = if own_row { 0.0 } else { 160.0 };
                        let w = (ui.available_width() - reserve).clamp(110.0, 240.0);
                        ui.allocate_ui_with_layout(
                            vec2(w, 26.0),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                let resp =
                                    search_field(ui, "gh_log_filter", filter, "Filter lines");
                                if (resp.has_focus() || resp.lost_focus())
                                    && ui.input(|i| i.key_pressed(egui::Key::Escape))
                                {
                                    filter.clear();
                                    resp.surrender_focus();
                                }
                            },
                        );
                    }
                    if !own_row {
                        ui.add_space(space::M);
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            log_title(ui, &job_name, doc_stats, !filter.trim().is_empty());
                        });
                    }
                },
            );
        });
    kit::hairline(ui);
    if close {
        detail.selected_job = None;
        return;
    }

    match (panel, &job) {
        (Panel::Live, Some(j)) => {
            let jobs_at = detail.jobs_at;
            live_panel(ui, cx, run.id, j, watch, jobs_at, actions);
            return;
        }
        (Panel::NeverRan, Some(j)) => {
            log_note(ui, |ui| never_ran_note(ui, j));
            return;
        }
        _ => {}
    }
    let jobs_loading = detail.jobs_loading;
    let Some(state) = detail.logs.get_mut(&job_id) else {
        log_note(ui, |ui| {
            kit::muted(ui, "The log has not been requested.");
        });
        return;
    };
    let doc = match &mut state.doc {
        None => {
            let text = if job.is_none() && jobs_loading {
                "Loading the job…"
            } else {
                "Downloading the log…"
            };
            log_note(ui, |ui| kit::loading(ui, text));
            return;
        }
        Some(Err(e)) => {
            let e = e.clone();
            log_note(ui, |ui| log_unavailable(ui, run.id, job_id, &e, actions));
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
    let steps_by_number: HashMap<u32, &hadion_core::github::Step> = job
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

/// The left of the log toolbar: the job's name, then the log's size and
/// error / warning counts.
fn log_title(
    ui: &mut Ui,
    job_name: &str,
    stats: Option<(usize, usize, usize, usize, usize)>,
    filtering: bool,
) {
    let mut badges: Vec<(String, Tone)> = Vec::new();
    let mut facts = String::new();
    if let Some((lines, errors, warnings, bytes, matched)) = stats {
        facts = if filtering {
            format!(
                "{} of {} lines",
                format::count(matched),
                format::count(lines)
            )
        } else {
            format!("{} lines, {}", format::count(lines), format::bytes(bytes))
        };
        if errors > 0 {
            badges.push((format::plural(errors, "error", "errors"), Tone::Danger));
        }
        if warnings > 0 {
            badges.push((
                format::plural(warnings, "warning", "warnings"),
                Tone::Warning,
            ));
        }
    }
    toolbar_title(ui, job_name, &facts, &badges);
}

/// The left of a log toolbar whose controls were laid out first (right to
/// left): a list icon, `name`, then `facts` and `badges` when there is room
/// for them. The name is cut first; the facts are dropped only when even a
/// short name would not fit, so nothing overlaps the controls.
pub(crate) fn toolbar_title(ui: &mut Ui, name: &str, facts: &str, badges: &[(String, Tone)]) {
    let p = *pal(ui);
    ui.spacing_mut().item_spacing.x = 8.0;
    icons::show(ui, Icon::List, 14.0, p.text_2);
    let painter = ui.painter().clone();
    let facts_w = if facts.is_empty() {
        0.0
    } else {
        painter
            .layout_no_wrap(
                facts.to_owned(),
                FontId::proportional(size::SMALL),
                p.text_3,
            )
            .size()
            .x
            + 8.0
    };
    let badges_w: f32 = badges
        .iter()
        .map(|(t, _)| kit::badge_width(&painter, t) + 8.0)
        .sum();
    let avail = ui.available_width();
    let show_facts = avail - facts_w - badges_w >= 120.0;
    let name_w = if show_facts {
        avail - facts_w - badges_w
    } else {
        avail
    };
    ui.scope(|ui| {
        ui.set_max_width(name_w.max(40.0));
        ui.add(
            Label::new(
                RichText::new(name)
                    .size(size::BODY)
                    .family(semibold())
                    .color(p.text_strong),
            )
            .truncate(),
        );
    });
    if show_facts {
        if !facts.is_empty() {
            ui.label(caption(&p, facts));
        }
        for (t, tone) in badges {
            kit::badge(ui, t, *tone);
        }
    }
}

/// "Checked 12 s ago, next check in 18 s" under a watched job, or why it
/// is not being checked.
fn watch_status(cx: &Cx, watch: Option<&ci::JobWatch>, jobs_at: Option<Instant>) -> String {
    if let Some(r) = cx.rate.filter(|_| watch_paused(cx.rate)) {
        return format!(
            "Paused to save the API budget: {} requests left until {}.",
            r.remaining,
            format::clock(r.reset)
        );
    }
    let ago = |t: Instant| format::relative(cx.now - t.elapsed().as_secs() as i64, cx.now);
    match watch {
        Some(w) if w.in_flight => "Checking for progress…".to_owned(),
        Some(w) => {
            let next = w
                .interval
                .saturating_sub(w.polled.elapsed())
                .as_secs()
                .max(1);
            match &w.error {
                Some(e) => format!("The last check failed ({e}). Trying again in {next} s."),
                None => format!("Checked {}, next check in {next} s.", ago(w.polled)),
            }
        }
        None => jobs_at
            .map(|t| format!("Checked {}.", ago(t)))
            .unwrap_or_default(),
    }
}

/// The log panel of a running or queued job: what it is doing and for how
/// long, that its log comes when it finishes, and its steps as they go.
fn live_panel(
    ui: &mut Ui,
    cx: &Cx,
    run: u64,
    job: &Job,
    watch: Option<&ci::JobWatch>,
    jobs_at: Option<Instant>,
    actions: &mut Vec<Action>,
) {
    let queued = job.outcome() == Outcome::Queued;
    let current = job.current_step().filter(|_| !queued).map(|s| s.number);
    // Wide: the status on the left and the steps beside it, each scrolling
    // on its own, so following the running step never hides the status.
    // Narrow: one scrolling column, status first (no following then).
    let wide = ui.available_width() >= 760.0;
    if !wide {
        egui::ScrollArea::vertical()
            .id_salt(("gh_live", job.id))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                Frame::NONE
                    .inner_margin(Margin {
                        left: 16,
                        right: 12,
                        top: 10,
                        bottom: 10,
                    })
                    .show(ui, |ui| {
                        live_info(ui, cx, run, job, watch, jobs_at, false, actions);
                        if !job.steps.is_empty() {
                            ui.add_space(space::S);
                            steps_caption(ui, job);
                            live_steps(ui, cx, job, current, false);
                        }
                    });
            });
        return;
    }
    Frame::NONE
        .inner_margin(Margin {
            left: 16,
            right: 12,
            top: 10,
            bottom: 6,
        })
        .show(ui, |ui| {
            let h = ui.available_height();
            // As wide as the run detail above, at most.
            let steps_w = (ui.available_width() - 400.0 - space::XL).min(kit::DETAIL_MAX - 400.0);
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(vec2(400.0, h), Layout::top_down(Align::Min), |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt(("gh_live_info", job.id))
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            live_info(ui, cx, run, job, watch, jobs_at, true, actions);
                        });
                });
                ui.add_space(space::XL);
                ui.allocate_ui_with_layout(vec2(steps_w, h), Layout::top_down(Align::Min), |ui| {
                    if job.steps.is_empty() {
                        kit::muted(ui, "No steps yet: a runner has not picked the job up.");
                        return;
                    }
                    steps_caption(ui, job);
                    egui::ScrollArea::vertical()
                        .id_salt(("gh_live_steps", job.id))
                        .auto_shrink([false, false])
                        .show(ui, |ui| live_steps(ui, cx, job, current, true));
                });
            });
        });
}

/// `STEPS  28 of 36 done` above a running job's steps.
fn steps_caption(ui: &mut Ui, job: &Job) {
    let p = *pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(kit::caption_text(&p, "Steps"));
        ui.label(caption(
            &p,
            format!("{} of {} done", job.steps_done(), job.steps.len()),
        ));
    });
    ui.add_space(2.0);
}

/// What a running job is doing: the step and for how long, why there is
/// no log yet, a link to GitHub's live view, and when it was last checked.
/// `full` adds the step's name and the runner as rows (the wide layout).
#[allow(clippy::too_many_arguments)]
fn live_info(
    ui: &mut Ui,
    cx: &Cx,
    run: u64,
    job: &Job,
    watch: Option<&ci::JobWatch>,
    jobs_at: Option<Instant>,
    full: bool,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let queued = job.outcome() == Outcome::Queued;
    let current = job.current_step().filter(|_| !queued);
    ui.spacing_mut().item_spacing.y = 6.0;
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(16.0, 20.0), Sense::hover());
        outcome_dot(ui.painter(), job.outcome(), r.center(), 6.0, cx.t);
        let head = match current {
            _ if queued => "Waiting for a runner".to_owned(),
            Some(s) if full => format!("Running step {} of {}", s.number, job.steps.len()),
            Some(s) => format!("Step {} of {}: {}", s.number, job.steps.len(), s.name),
            None => "Running".to_owned(),
        };
        ui.add(
            Label::new(
                RichText::new(head)
                    .size(size::TITLE)
                    .family(semibold())
                    .color(p.text_strong),
            )
            .truncate(),
        );
    });
    kit::secondary(
        ui,
        "GitHub publishes a job's log when the job finishes. It will appear here by itself.",
    );
    kit::kv(ui, 88.0, |kv| {
        if let Some(s) = current {
            if full {
                kv.text("Step", s.name.as_str());
            }
            if let Some(st) = s.started {
                kv.text(
                    "Step time",
                    format!("{} so far", format::duration((cx.now - st).max(0))),
                );
            }
        }
        if let Some(d) = job.duration_secs(cx.now) {
            kv.text(
                if queued { "Waiting" } else { "Job time" },
                format!("{} so far", format::duration(d)),
            );
        }
        if full && !job.labels.is_empty() {
            kv.text("Runs on", job.labels.join(", "));
        }
    });
    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        if IconButton::new(Icon::External, "Watch live on GitHub")
            .compact()
            .show(ui)
            .on_hover_text("GitHub's page streams the log while the job runs")
            .clicked()
        {
            actions.push(Action::OpenUrl(job.url.clone()));
        }
        let busy = watch.is_some_and(|w| w.in_flight);
        if IconButton::new(Icon::Fetch, "Check now")
            .compact()
            .enabled(!busy && !watch_paused(cx.rate))
            .show(ui)
            .clicked()
        {
            actions.push(ci::Act::PollJob { run, job: job.id }.into());
        }
    });
    let status = watch_status(cx, watch, jobs_at);
    if !status.is_empty() {
        kit::muted(ui, &status);
    }
}

/// A running job's steps: outcome, number, name and time, the running one
/// highlighted. With `follow`, the running step is scrolled into view
/// whenever it changes.
fn live_steps(ui: &mut Ui, cx: &Cx, job: &Job, current: Option<u32>, follow: bool) {
    let p = *pal(ui);
    let scrolled_id = egui::Id::unique(("gh_live_scrolled", job.id));
    let scrolled: Option<u32> = ui.data(|d| d.get_temp(scrolled_id));
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for step in &job.steps {
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), STEP_ROW_H), Sense::hover());
            let o = step.outcome();
            let is_current = current == Some(step.number);
            if follow && is_current && scrolled != Some(step.number) {
                ui.scroll_to_rect(rect, Some(Align::Center));
                ui.data_mut(|d| d.insert_temp(scrolled_id, step.number));
            }
            if !ui.is_rect_visible(rect) {
                continue;
            }
            if is_current {
                ui.painter().rect_filled(
                    rect.shrink2(vec2(0.0, 1.0)),
                    CornerRadius::same(crate::theme::radius::M),
                    p.selection,
                );
            }
            let painter = ui.painter();
            let mid = rect.center().y;
            step_dot(painter, &p, o, pos2(rect.left() + 14.0, mid), cx.t);
            painter.text(
                pos2(rect.left() + 46.0, mid),
                Align2::RIGHT_CENTER,
                step.number.to_string(),
                FontId::monospace(size::SMALL),
                p.text_3,
            );
            let dur = match step.duration_secs(cx.now) {
                Some(d) if o.is_running() => format!("{} so far", format::duration(d)),
                Some(d) => format::duration(d),
                None if o == Outcome::Skipped => "skipped".to_owned(),
                None => String::new(),
            };
            let r = painter.text(
                pos2(rect.right() - 10.0, mid),
                Align2::RIGHT_CENTER,
                dur,
                FontId::proportional(size::SMALL),
                p.text_2,
            );
            let name_c = if o == Outcome::Queued {
                p.text_3
            } else if is_current {
                p.text_strong
            } else {
                p.text
            };
            let x = rect.left() + 58.0;
            let g = truncated(
                painter,
                &step.name,
                FontId::proportional(size::BODY - 0.5),
                name_c,
                (r.left() - x - 12.0).max(40.0),
            );
            painter.galley(pos2(x, mid - g.size().y / 2.0), g, name_c);
        }
    });
}

/// A finished job that never reached a runner has no log.
fn never_ran_note(ui: &mut Ui, job: &Job) {
    let why = match job.outcome() {
        Outcome::Skipped => {
            "GitHub skipped it: its `if:` condition was false, or a job it needs did not succeed."
        }
        Outcome::Cancelled => "It was cancelled while it waited for a runner.",
        Outcome::Failure => {
            "It failed before a runner picked it up (an invalid workflow or runner label, for example)."
        }
        _ => "It ended before a runner picked it up.",
    };
    kit::callout(
        ui,
        Tone::Neutral,
        "This job never ran, so it has no log",
        why,
    );
}

/// A finished job's log could not be downloaded. GitHub's 404 means the
/// log is gone (expired or deleted), which is information, not a failure.
fn log_unavailable(ui: &mut Ui, run: u64, job: u64, error: &str, actions: &mut Vec<Action>) {
    let gone = hadion_core::github::is_not_found(error);
    let (tone, title, body) = if gone {
        (
            Tone::Neutral,
            "GitHub has no log for this job",
            format!(
                "GitHub keeps job logs for a limited time (90 days unless the repository sets less), and people with write access can delete them.\nGitHub answered: {error}"
            ),
        )
    } else {
        (
            Tone::Warning,
            "Could not download the log",
            error.to_owned(),
        )
    };
    let mut retry = false;
    kit::callout_with(ui, tone, title, &body, |ui| {
        ui.add_space(2.0);
        retry = IconButton::new(Icon::Fetch, "Try again")
            .compact()
            .show(ui)
            .clicked();
    });
    if retry {
        actions.push(Action::ShowLog {
            run,
            job,
            step: None,
        });
    }
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
    // A commit GitHub does not have: no checks, and nothing went wrong.
    let away = match result {
        Some(Ok(c)) if c.is_empty() => hadion_core::github::not_on_github(&sha.to_string()),
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
            .on_hover_text(if away.is_some() {
                "Check again"
            } else {
                "Refresh checks"
            })
            .clicked()
        {
            actions.push(Action::RefreshChecks(sha));
        }
        if let Some(at) = state.and_then(|s| s.at) {
            ui.label(caption(
                &p,
                format!(
                    "{} {}",
                    if away.is_some() {
                        "checked"
                    } else {
                        "refreshed"
                    },
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
            match away {
                Some(NotOnGitHub::Unpushed) => kit::callout(
                    ui,
                    Tone::Neutral,
                    "Not on GitHub yet",
                    "This commit has not been pushed (no remote branch contains it), so it has no checks.",
                ),
                Some(NotOnGitHub::Unknown) => kit::callout(
                    ui,
                    Tone::Neutral,
                    "Not on GitHub",
                    "GitHub has no commit with this SHA, so it has no checks. It may be on another remote, or GitHub has not seen the push yet.",
                ),
                None => {
                    kit::muted(ui, "No checks reported for this commit.");
                }
            }
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

/// Show an Actions check's job on the Actions page. A running, queued or
/// skipped job has no log on GitHub, so it is selected without asking for
/// one (the log viewer shows its progress).
fn open_check_job(c: &CheckRun, run: u64, job: u64, actions: &mut Vec<Action>) {
    let o = c.outcome();
    if o.is_running() || o == Outcome::Skipped {
        actions.push(Action::OpenRun { run, job: None });
        actions.push(ci::Act::OpenJob { run, job }.into());
    } else {
        actions.push(Action::OpenRun {
            run,
            job: Some(job),
        });
    }
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
            Some((run, job)) => open_check_job(c, run, job, actions),
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
                open_check_job(c, run, job, actions);
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
