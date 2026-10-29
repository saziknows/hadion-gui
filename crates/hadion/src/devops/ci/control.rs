//! The CI controller: what each [`Act`] does and what each [`Msg`]
//! changes. Every GitHub write is confirmed first (the dialog shows the
//! exact requests and bodies) and then sent through `cx.gh_change`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use hadion_core::devops::ci::{self as core, act as core_act, compare as core_compare};
use hadion_core::devops::ci::{history, model, requests, workflow};
use hadion_core::devops::gh::{Gh, Request};
use hadion_core::devops::proc::{ProcSpec, run_capture};
use hadion_core::devops::tools::{self, Tool};
use hadion_core::github::{Job, WorkflowRun, gh_program};

use super::{
    Act, ActState, Analysis, ArtifactList, CacheList, CacheSel, Change, CompareReport,
    CompareState, DispatchForm, History, JobDiff, LocalInfo, Msg, PickMode, ScriptCmd, Tab, Tools,
};
use crate::devops::console::ConsoleRun;
use crate::devops::ctx::Ctx;
use crate::format;
use crate::state::{Action, LogDoc, LogState, RepoState, ToastKind};

/// Failing jobs whose logs a comparison diffs, at most.
pub const COMPARE_MAX_JOBS: usize = 5;
/// Runs the job analysis looks at, at most.
pub const ANALYSE_MAX_RUNS: usize = 30;

fn plural(n: usize, one: &str, many: &str) -> String {
    format::plural(n, one, many)
}

/// The confirmation's detail lines: each request and its JSON body.
pub fn request_details(reqs: &[&Request]) -> Vec<String> {
    let mut out = Vec::new();
    const MAX: usize = 40;
    for r in reqs.iter().take(MAX) {
        match r.body_preview() {
            Some(b) => out.push(format!("{}\n{b}", r.display())),
            None => out.push(r.display()),
        }
    }
    if reqs.len() > MAX {
        out.push(format!(
            "… and {} more requests like these",
            reqs.len() - MAX
        ));
    }
    out
}

fn run_label(r: &WorkflowRun) -> String {
    let title = if r.title.is_empty() {
        &r.name
    } else {
        &r.title
    };
    let mut s = format!("{} #{}", r.name, r.run_number);
    if !r.branch.is_empty() {
        s.push_str(&format!(" on {}", r.branch));
    }
    if title != &r.name {
        s.push_str(&format!(": {title}"));
    }
    s
}

fn runs_of(repo: &RepoState) -> &[WorkflowRun] {
    repo.github
        .as_ref()
        .map(|d| d.runs.as_slice())
        .unwrap_or(&[])
}

fn find_run(repo: &RepoState, id: u64) -> Option<WorkflowRun> {
    if let Some(r) = repo.github.as_ref().and_then(|d| d.run(id)) {
        return Some(r.clone());
    }
    repo.devops
        .ci
        .history
        .values()
        .filter_map(|h| h.ok())
        .flat_map(|h| h.runs.iter())
        .find(|r| r.id == id)
        .cloned()
}

fn need_gh(cx: &mut Ctx) -> Option<Gh> {
    let gh = cx.gh.clone();
    if gh.is_none() {
        cx.toast(
            ToastKind::Error,
            "This repository has no GitHub remote".to_owned(),
        );
    }
    gh
}

/// The visible run ids of the Runs tab, in list order.
fn visible_ids(repo: &RepoState) -> Vec<u64> {
    match repo.github.as_ref() {
        Some(d) => crate::views::github::visible_runs(d, &repo.gh_page)
            .iter()
            .map(|r| r.id)
            .collect(),
        None => Vec::new(),
    }
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::SetTab(t) => {
            repo.devops.ci.tab = t;
            tab_shown(cx, repo, t);
        }
        Act::Refresh(t) => refresh(cx, repo, t),
        Act::DispatchSettled { workflow_id } => {
            cx.action(Action::RefreshGitHub);
            if let Some(id) = workflow_id {
                load_history(cx, repo, id, true);
            }
        }
        Act::RefreshRuns(ids) => fetch_runs_now(cx, ids),
        // ------------------------------------------------------------ Runs
        Act::Pick { run, mode } => pick(repo, run, mode),
        Act::PickRuns(ids) => {
            let ci = &mut repo.devops.ci;
            ci.anchor = ids.last().copied();
            ci.picked = ids.into_iter().collect();
        }
        Act::ClearPicks => {
            repo.devops.ci.picked.clear();
            repo.devops.ci.anchor = None;
        }
        Act::AskCancelRuns(ids) => ask_cancel_runs(cx, repo, &ids, None),
        Act::AskCancelBranch(branch) => {
            let ids: Vec<u64> = runs_of(repo)
                .iter()
                .filter(|r| r.branch == branch && r.is_running())
                .map(|r| r.id)
                .collect();
            ask_cancel_runs(cx, repo, &ids, Some(&branch));
        }
        Act::AskRerunFailedRuns(ids) => ask_rerun_failed_runs(cx, repo, &ids),
        Act::AskRerun {
            run,
            failed_only,
            debug,
        } => ask_rerun(cx, repo, run, failed_only, debug),
        Act::AskRerunJob {
            run,
            job,
            name,
            debug,
        } => ask_rerun_job(cx, repo, run, job, &name, debug),
        Act::AskCancel { run, force } => ask_cancel(cx, repo, run, force),
        Act::AskDeleteLogs(run) => ask_delete_logs(cx, repo, run),
        Act::AskDeleteRun(run) => ask_delete_run(cx, repo, run),
        Act::LoadRunArtifacts { run, force } => load_run_artifacts(cx, repo, run, force),
        Act::OpenJob { run, job } => open_job(repo, run, job),
        Act::PollJob { run, job } => poll_job(cx, repo, run, job),
        Act::FetchLog { run, job, attempt } => fetch_log(cx, repo, run, job, attempt),
        Act::Send(change) => send(cx, repo, change),
        // --------------------------------------------------------- Compare
        Act::CompareOpen(run) => compare_open(cx, repo, run, false),
        Act::CompareStart => compare_start(cx, repo),
        Act::CompareClose => repo.devops.ci.compare = None,
        Act::CompareJob(i) => {
            if let Some(c) = &mut repo.devops.ci.compare {
                c.job = i;
            }
        }
        Act::CompareExpand { commits } => {
            if let Some(c) = &mut repo.devops.ci.compare {
                if commits {
                    c.all_commits = true;
                } else {
                    c.all_files = true;
                }
            }
        }
        // ------------------------------------------------------- Workflows
        Act::SelectWorkflow(path) => select_workflow(cx, repo, path),
        Act::LoadHistory { workflow, force } => load_history(cx, repo, workflow, force),
        Act::ApplyBranch => {
            let ci = &mut repo.devops.ci;
            ci.wf_branch_applied = ci.wf_branch.trim().to_owned();
            if let Some(id) = selected_workflow_id(repo) {
                load_history(cx, repo, id, false);
            }
        }
        Act::AnalyseJobs { workflow } => analyse_jobs(cx, repo, workflow),
        Act::AskToggleWorkflow { id, enable } => ask_toggle_workflow(cx, repo, id, enable),
        Act::OpenDispatch(path) => open_dispatch(cx, repo, &path),
        Act::CloseDispatch => repo.devops.ci.dispatch = None,
        Act::AskDispatch => ask_dispatch(cx, repo),
        // ------------------------------------------------------- Artifacts
        Act::SelectArtifact(id) => repo.devops.ci.artifact_selected = id,
        Act::DownloadArtifact { id, dir } => download_artifact(cx, repo, id, dir),
        Act::AskDeleteArtifact(id) => ask_delete_artifact(cx, repo, id),
        // ---------------------------------------------------------- Caches
        Act::SetCacheSort(s) => {
            repo.devops.ci.cache_sort = s;
            load_caches(cx, repo);
        }
        Act::SetCacheView(v) => {
            repo.devops.ci.cache_view = v;
            repo.devops.ci.cache_selected = None;
        }
        Act::SelectCache(sel) => repo.devops.ci.cache_selected = sel,
        Act::AskDeleteCache(id) => ask_delete_cache(cx, repo, id),
        Act::AskDeleteCacheKey { key, git_ref } => ask_delete_cache_key(cx, repo, &key, &git_ref),
        Act::AskDeleteRef(r) => ask_delete_refs(cx, repo, &[r], None),
        Act::CheckPrStates => check_pr_states(cx, repo),
        Act::AskDeleteClosedPrCaches => {
            let closed = closed_pr_refs(repo);
            ask_delete_refs(cx, repo, &closed, Some("closed pull requests"));
        }
        // -------------------------------------------------------- Validate
        Act::Validate => validate(cx, repo),
        Act::SelectLintFile(f) => repo.devops.ci.lint_file = f,
        // ----------------------------------------------------------- Local
        Act::CheckTools => check_tools(cx, repo),
        Act::ListActJobs => list_act_jobs(cx, repo),
        Act::PickSecretFile => {
            if let Some(p) = rfd::FileDialog::new()
                .set_title("Secrets file for act (dotenv: NAME=value per line)")
                .set_directory(&repo.workdir)
                .pick_file()
            {
                repo.devops.ci.act.secret_file = Some(p);
            }
        }
        Act::ClearSecretFile => repo.devops.ci.act.secret_file = None,
        Act::RunLocally => run_act(cx, repo),
        Act::ShowActRun(id) => repo.devops.ci.act.shown = id,
        Act::Script(cmd) => run_script(cx, repo, cmd),
    }
}

/// A tab became visible: fetch what it needs, once.
pub fn tab_shown(cx: &mut Ctx, repo: &mut RepoState, tab: Tab) {
    match tab {
        Tab::Runs => {}
        Tab::Workflows => {
            if repo.devops.ci.local.unloaded() {
                load_local(cx, repo);
            }
            if let Some(id) = selected_workflow_id(repo) {
                load_history(cx, repo, id, false);
            }
        }
        Tab::Artifacts => {
            if repo.devops.ci.artifacts.unloaded() {
                load_artifacts(cx, repo);
            }
        }
        Tab::Caches => {
            if repo.devops.ci.caches.unloaded() {
                load_caches(cx, repo);
            }
        }
        Tab::Validate => validate(cx, repo),
        Tab::Local => {
            if repo.devops.ci.act.tools.unloaded() {
                check_tools(cx, repo);
            }
            if repo.devops.ci.local.unloaded() {
                load_local(cx, repo);
            }
        }
    }
}

fn refresh(cx: &mut Ctx, repo: &mut RepoState, tab: Tab) {
    match tab {
        Tab::Runs => cx.action(Action::RefreshGitHub),
        Tab::Workflows => {
            load_local(cx, repo);
            cx.action(Action::RefreshGitHub);
            if let Some(id) = selected_workflow_id(repo) {
                load_history(cx, repo, id, true);
            }
        }
        Tab::Artifacts => load_artifacts(cx, repo),
        Tab::Caches => load_caches(cx, repo),
        Tab::Validate => validate(cx, repo),
        Tab::Local => {
            tools::refresh();
            check_tools(cx, repo);
            load_local(cx, repo);
        }
    }
}

// ---------------------------------------------------------------------------
// Runs: picking, confirmations, changes
// ---------------------------------------------------------------------------

fn pick(repo: &mut RepoState, run: u64, mode: PickMode) {
    let visible = visible_ids(repo);
    let ci = &mut repo.devops.ci;
    match mode {
        PickMode::Toggle => {
            if !ci.picked.remove(&run) {
                ci.picked.insert(run);
            }
            ci.anchor = Some(run);
        }
        PickMode::Range => {
            let from = ci
                .anchor
                .and_then(|a| visible.iter().position(|x| *x == a))
                .or_else(|| {
                    repo.gh_page
                        .selected_run
                        .and_then(|s| visible.iter().position(|x| *x == s))
                });
            let to = visible.iter().position(|x| *x == run);
            match (from, to) {
                (Some(a), Some(b)) => {
                    let (lo, hi) = (a.min(b), a.max(b));
                    ci.picked.extend(visible[lo..=hi].iter().copied());
                }
                _ => {
                    ci.picked.insert(run);
                }
            }
            ci.anchor = Some(run);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn confirm_runs(
    cx: &mut Ctx,
    title: String,
    body: String,
    confirm: &str,
    danger: bool,
    label: String,
    requests: Vec<(u64, Request)>,
    deleted: bool,
) {
    let refs: Vec<&Request> = requests.iter().map(|(_, r)| r).collect();
    let details = request_details(&refs);
    cx.confirm(
        title,
        body,
        confirm,
        danger,
        details,
        Act::Send(Change::Runs {
            label,
            requests,
            deleted,
        })
        .into(),
    );
}

fn list_runs(runs: &[WorkflowRun]) -> String {
    let mut s: Vec<String> = runs
        .iter()
        .take(12)
        .map(|r| format!("• {}", run_label(r)))
        .collect();
    if runs.len() > 12 {
        s.push(format!("• and {} more", runs.len() - 12));
    }
    s.join("\n")
}

fn ask_cancel_runs(cx: &mut Ctx, repo: &RepoState, ids: &[u64], branch: Option<&str>) {
    let Some(gh) = need_gh(cx) else { return };
    let runs: Vec<WorkflowRun> = ids
        .iter()
        .filter_map(|id| find_run(repo, *id))
        .filter(|r| r.is_running())
        .collect();
    if runs.is_empty() {
        cx.toast(
            ToastKind::Info,
            match branch {
                Some(b) => format!("Nothing is running on {b}"),
                None => "None of the picked runs is running".to_owned(),
            },
        );
        return;
    }
    let n = runs.len();
    let requests: Vec<(u64, Request)> = runs
        .iter()
        .map(|r| (r.id, requests::cancel(&gh, r.id)))
        .collect();
    let title = match branch {
        Some(b) => format!("Cancel everything running on {b}?"),
        None => format!("Cancel {}?", plural(n, "running run", "running runs")),
    };
    let body = format!(
        "{}\n\nRunning jobs are stopped and queued jobs are skipped. One request per run.",
        list_runs(&runs)
    );
    confirm_runs(
        cx,
        title,
        body,
        &format!("Cancel {}", plural(n, "run", "runs")),
        true,
        format!("Cancel {}", plural(n, "run", "runs")),
        requests,
        false,
    );
}

fn ask_rerun_failed_runs(cx: &mut Ctx, repo: &RepoState, ids: &[u64]) {
    let Some(gh) = need_gh(cx) else { return };
    let runs: Vec<WorkflowRun> = ids
        .iter()
        .filter_map(|id| find_run(repo, *id))
        .filter(|r| r.is_failure() || r.outcome() == hadion_core::github::Outcome::Cancelled)
        .filter(|r| !r.is_running())
        .collect();
    if runs.is_empty() {
        cx.toast(ToastKind::Info, "None of the picked runs failed".to_owned());
        return;
    }
    let n = runs.len();
    let requests: Vec<(u64, Request)> = runs
        .iter()
        .map(|r| (r.id, requests::rerun_failed(&gh, r.id, false)))
        .collect();
    let body = format!(
        "{}\n\nEach run starts a new attempt with its failed and cancelled jobs and the jobs that depend on them.",
        list_runs(&runs)
    );
    confirm_runs(
        cx,
        format!("Re-run the failed jobs of {}?", plural(n, "run", "runs")),
        body,
        &format!("Re-run {}", plural(n, "run", "runs")),
        false,
        format!("Re-run failed jobs of {}", plural(n, "run", "runs")),
        requests,
        false,
    );
}

fn ask_rerun(cx: &mut Ctx, repo: &RepoState, run: u64, failed_only: bool, debug: bool) {
    let Some(gh) = need_gh(cx) else { return };
    let Some(r) = find_run(repo, run) else { return };
    let req = if failed_only {
        requests::rerun_failed(&gh, run, debug)
    } else {
        requests::rerun(&gh, run, debug)
    };
    let what = if failed_only {
        "the failed jobs"
    } else {
        "every job"
    };
    let mut body = format!(
        "{}\n\nStarts attempt {} with {what}{}.",
        run_label(&r),
        r.attempt + 1,
        if failed_only {
            " (and the jobs that depend on them)"
        } else {
            ""
        }
    );
    if debug {
        body.push_str(
            "\n\nDebug logging turns on runner diagnostic logs and step debug logs for this attempt (like the ACTIONS_RUNNER_DEBUG and ACTIONS_STEP_DEBUG secrets). The logs get much longer.",
        );
    }
    let label = format!(
        "Re-run {what} of {} #{}{}",
        r.name,
        r.run_number,
        if debug { " with debug logging" } else { "" }
    );
    confirm_runs(
        cx,
        format!("{label}?"),
        body,
        if debug {
            "Re-run with debug logging"
        } else {
            "Re-run"
        },
        false,
        label,
        vec![(run, req)],
        false,
    );
}

fn ask_rerun_job(cx: &mut Ctx, repo: &RepoState, run: u64, job: u64, name: &str, debug: bool) {
    let Some(gh) = need_gh(cx) else { return };
    let r = find_run(repo, run);
    let req = requests::rerun_job(&gh, job, debug);
    let mut body = format!(
        "Job {name} of {}.\n\nStarts a new attempt of the run with this job and the jobs that depend on it.",
        r.as_ref()
            .map(run_label)
            .unwrap_or_else(|| format!("run {run}"))
    );
    if debug {
        body.push_str(" Runner diagnostic and step debug logs are turned on.");
    }
    let details = request_details(&[&req]);
    cx.confirm(
        format!(
            "Re-run job {name}{}?",
            if debug { " with debug logging" } else { "" }
        ),
        body,
        "Re-run job",
        false,
        details,
        Act::Send(Change::Job {
            run,
            job,
            request: req,
        })
        .into(),
    );
}

fn ask_cancel(cx: &mut Ctx, repo: &RepoState, run: u64, force: bool) {
    let Some(gh) = need_gh(cx) else { return };
    let Some(r) = find_run(repo, run) else { return };
    let req = if force {
        requests::force_cancel(&gh, run)
    } else {
        requests::cancel(&gh, run)
    };
    let body = if force {
        format!(
            "{}\n\nForce cancel stops the run even when jobs ignore a normal cancel (steps with `if: always()`). Use it when a cancel did not work.",
            run_label(&r)
        )
    } else {
        format!(
            "{}\n\nRunning jobs are stopped; queued jobs are skipped.",
            run_label(&r)
        )
    };
    let what = if force { "Force cancel" } else { "Cancel" };
    confirm_runs(
        cx,
        format!("{what} {} #{}?", r.name, r.run_number),
        body,
        if force { "Force cancel" } else { "Cancel run" },
        true,
        format!("{what} {} #{}", r.name, r.run_number),
        vec![(run, req)],
        false,
    );
}

fn ask_delete_logs(cx: &mut Ctx, repo: &RepoState, run: u64) {
    let Some(gh) = need_gh(cx) else { return };
    let Some(r) = find_run(repo, run) else { return };
    confirm_runs(
        cx,
        format!("Delete the logs of {} #{}?", r.name, r.run_number),
        format!(
            "{}\n\nEvery job log of this run is deleted for good. The run, its jobs, annotations and artifacts stay.",
            run_label(&r)
        ),
        "Delete logs",
        true,
        format!("Delete the logs of {} #{}", r.name, r.run_number),
        vec![(run, requests::delete_logs(&gh, run))],
        false,
    );
}

fn ask_delete_run(cx: &mut Ctx, repo: &RepoState, run: u64) {
    let Some(gh) = need_gh(cx) else { return };
    let Some(r) = find_run(repo, run) else { return };
    if r.is_running() {
        cx.toast(
            ToastKind::Info,
            "Cancel the run before deleting it".to_owned(),
        );
        return;
    }
    confirm_runs(
        cx,
        format!("Delete {} #{}?", r.name, r.run_number),
        format!(
            "{}\n\nThe run, its jobs, logs and artifacts are deleted for good and disappear from the run list. This cannot be undone.",
            run_label(&r)
        ),
        "Delete run",
        true,
        format!("Delete {} #{}", r.name, r.run_number),
        vec![(run, requests::delete_run(&gh, run))],
        true,
    );
}

// ---------------------------------------------------------------------------
// Running jobs: watched while the log viewer shows them
// ---------------------------------------------------------------------------

/// Select a job in the log viewer without downloading its log. Its log
/// state becomes "not asked for" (`loading: false, doc: None`), which also
/// stops the app from requesting it when the run's jobs come in; the view
/// asks for it once the job has finished.
fn open_job(repo: &mut RepoState, run: u64, job: u64) {
    let detail = repo.gh_page.runs.entry(run).or_default();
    detail.selected_job = Some(job);
    detail.expanded.insert(job);
    let keep = detail
        .logs
        .get(&job)
        .is_some_and(|s| s.loading || matches!(s.doc, Some(Ok(_))));
    if !keep {
        detail.logs.insert(
            job,
            LogState {
                loading: false,
                doc: None,
            },
        );
    }
}

/// Read the watched job's progress: the run's jobs (one request for up to
/// 100), or only this job for bigger runs; the run too once the job ended.
fn poll_job(cx: &mut Ctx, repo: &mut RepoState, run: u64, job: u64) {
    let ci = &mut repo.devops.ci;
    match ci.watch.as_mut() {
        Some(w) if w.run == run && w.job == job => {
            if w.in_flight {
                return;
            }
            w.in_flight = true;
            w.polled = Instant::now();
        }
        _ => {
            ci.watch = Some(super::JobWatch {
                run,
                job,
                polled: Instant::now(),
                in_flight: true,
                interval: super::POLL_MIN,
                progress: String::new(),
                error: None,
            });
        }
    }
    let single = repo
        .gh_page
        .runs
        .get(&run)
        .is_some_and(|d| d.jobs().len() > 100);
    let started = cx.gh_read(format!("Progress of job {job}"), move |gh, log| {
        let jobs = if single {
            hadion_core::github::fetch_job(gh.dir(), &gh.owner, &gh.repo, job, &mut log.calls)
                .map(|j| vec![j])
        } else {
            core::fetch_jobs(gh, run, &mut log.calls)
        }
        .map_err(|e| e.to_string());
        let finished = jobs
            .as_ref()
            .ok()
            .and_then(|l| l.iter().find(|j| j.id == job))
            .is_some_and(|j| !j.outcome().is_running());
        let run_data = finished.then(|| {
            core::fetch_run(gh, run, &mut log.calls)
                .map(Box::new)
                .map_err(|e| e.to_string())
        });
        Msg::JobPolled {
            run,
            job,
            single,
            jobs,
            run_data,
        }
    });
    if !started && let Some(w) = repo.devops.ci.watch.as_mut() {
        w.in_flight = false;
    }
}

/// Download a finished job's log here (not through the app's log request)
/// so the few seconds GitHub takes to publish it are retried quietly.
fn fetch_log(cx: &mut Ctx, repo: &mut RepoState, run: u64, job: u64, attempt: u32) {
    let detail = repo.gh_page.runs.entry(run).or_default();
    if attempt == 0
        && detail
            .logs
            .get(&job)
            .is_some_and(|s| s.loading || matches!(s.doc, Some(Ok(_))))
    {
        return;
    }
    detail.logs.insert(
        job,
        LogState {
            loading: true,
            doc: None,
        },
    );
    cx.gh_read(format!("Log of job {job}"), move |gh, log| {
        let result =
            hadion_core::github::fetch_job_log(gh.dir(), &gh.owner, &gh.repo, job, &mut log.calls)
                .map(|raw| Box::new(hadion_core::joblog::parse(&raw)))
                .map_err(|e| e.to_string());
        Msg::JobLog {
            run,
            job,
            attempt,
            result,
        }
    });
}

fn job_polled(
    repo: &mut RepoState,
    run: u64,
    job: u64,
    single: bool,
    jobs: Result<Vec<Job>, String>,
    run_data: Option<Result<Box<WorkflowRun>, String>>,
) {
    if let Some(Ok(r)) = run_data
        && let Some(d) = repo.github.as_mut()
    {
        d.upsert_run(*r);
    }
    let detail = repo.gh_page.runs.entry(run).or_default();
    let (progress, error) = match jobs {
        Ok(list) => {
            let progress = list.iter().find(|j| j.id == job).map(super::progress_key);
            if single {
                if let (Some(Ok(all)), Some(new)) = (&mut detail.jobs, list.into_iter().next())
                    && let Some(slot) = all.iter_mut().find(|j| j.id == new.id)
                {
                    *slot = new;
                }
            } else {
                detail.jobs = Some(Ok(list));
            }
            detail.jobs_at = Some(Instant::now());
            (progress, None)
        }
        Err(e) => (None, Some(e)),
    };
    let Some(w) = repo
        .devops
        .ci
        .watch
        .as_mut()
        .filter(|w| w.run == run && w.job == job)
    else {
        return;
    };
    w.in_flight = false;
    w.polled = Instant::now();
    w.error = error;
    w.interval = match progress {
        Some(p) if p != w.progress => {
            w.progress = p;
            super::POLL_MIN
        }
        Some(_) => (w.interval * 2).min(super::POLL_MAX),
        None => super::POLL_MAX,
    };
}

fn job_log(
    cx: &mut Ctx,
    repo: &mut RepoState,
    run: u64,
    job: u64,
    attempt: u32,
    result: Result<Box<hadion_core::joblog::JobLog>, String>,
) {
    let filter = repo.gh_page.log_filter.clone();
    let detail = repo.gh_page.runs.entry(run).or_default();
    let doc = match result {
        Ok(log) => {
            let mut doc = LogDoc::new(*log);
            doc.set_filter(&filter);
            if let Some(&first) = doc.log.errors.first() {
                doc.scroll_to = Some(first);
            }
            Ok(doc)
        }
        Err(e) if hadion_core::github::is_not_found(&e) && attempt + 1 < super::LOG_TRIES => {
            cx.after(
                super::LOG_RETRY,
                Act::FetchLog {
                    run,
                    job,
                    attempt: attempt + 1,
                },
            );
            return;
        }
        Err(e) => Err(e),
    };
    detail.logs.insert(
        job,
        LogState {
            loading: false,
            doc: Some(doc),
        },
    );
    // The watch ends with its log (or the last answer about it), so a log
    // GitHub never publishes is not asked for again and again.
    if repo
        .devops
        .ci
        .watch
        .as_ref()
        .is_some_and(|w| w.run == run && w.job == job)
    {
        repo.devops.ci.watch = None;
    }
}

fn load_run_artifacts(cx: &mut Ctx, repo: &mut RepoState, run: u64, force: bool) {
    let slot = repo.devops.ci.run_artifacts.entry(run).or_default();
    if (!force && !slot.unloaded()) || !slot.start() {
        return;
    }
    let ok = cx.gh_read(format!("Artifacts of run {run}"), move |gh, log| {
        Msg::RunArtifacts {
            run,
            result: core::fetch_run_artifacts(gh, run, &mut log.calls).map_err(|e| e.to_string()),
        }
    });
    if !ok {
        repo.devops.ci.run_artifacts.remove(&run);
    }
}

/// Run a confirmed change.
fn send(cx: &mut Ctx, repo: &mut RepoState, change: Change) {
    let ci = &mut repo.devops.ci;
    match change {
        Change::Runs {
            label,
            requests,
            deleted,
        } => {
            for (id, _) in &requests {
                ci.busy_runs.insert(*id, label.clone());
            }
            cx.gh_change(label.clone(), move |gh, log| {
                let mut ok = Vec::new();
                let mut failed = Vec::new();
                for (id, req) in &requests {
                    match gh.send(req, &mut log.calls) {
                        Ok(_) => ok.push(*id),
                        Err(e) => failed.push((*id, e.to_string())),
                    }
                }
                let n = requests.len();
                let result = if failed.is_empty() {
                    Ok(if n == 1 {
                        "Done".to_owned()
                    } else {
                        format!("{n} of {n} requests succeeded")
                    })
                } else {
                    Err(format!(
                        "{} of {n} failed: {}",
                        failed.len(),
                        failed
                            .iter()
                            .map(|(id, e)| format!("run {id}: {e}"))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ))
                };
                let (ok, deleted_ids) = if deleted {
                    (Vec::new(), ok)
                } else {
                    (ok, Vec::new())
                };
                (
                    result,
                    Some(Msg::RunsChanged {
                        ok,
                        deleted: deleted_ids,
                        failed,
                    }),
                )
            });
        }
        Change::Job { run, job, request } => {
            ci.busy_jobs.insert(job);
            cx.gh_change(format!("Re-run job {job}"), move |gh, log| {
                let r = gh.send(&request, &mut log.calls);
                let ok = r.is_ok();
                (
                    r.map(|_| "Done".to_owned()).map_err(|e| e.to_string()),
                    Some(Msg::JobChanged { run, job, ok }),
                )
            });
        }
        Change::Workflow {
            id,
            enable,
            request,
        } => {
            ci.wf_busy.insert(id);
            let label = format!(
                "{} workflow {id}",
                if enable { "Enable" } else { "Disable" }
            );
            cx.gh_change(label, move |gh, log| {
                let r = gh.send(&request, &mut log.calls);
                let ok = r.is_ok();
                (
                    r.map(|_| "Done".to_owned()).map_err(|e| e.to_string()),
                    Some(Msg::WorkflowToggled { id, enable, ok }),
                )
            });
        }
        Change::Dispatch {
            request,
            workflow_id,
            git_ref,
        } => {
            if let Some(f) = &mut ci.dispatch {
                f.sending = true;
                f.error = None;
            }
            let label = format!("Dispatch {} on {git_ref}", request.endpoint);
            cx.gh_change(label, move |gh, log| {
                let r = gh.send(&request, &mut log.calls);
                let ok = r.map(|_| ()).map_err(|e| e.to_string());
                (
                    ok.clone().map(|_| {
                        "GitHub accepted the dispatch; the run appears within a few seconds"
                            .to_owned()
                    }),
                    Some(Msg::Dispatched {
                        ok,
                        workflow_id,
                        git_ref,
                    }),
                )
            });
        }
        Change::Artifacts { requests } => {
            for (id, _) in &requests {
                ci.artifact_busy.insert(*id);
            }
            let n = requests.len();
            cx.gh_change(
                format!("Delete {}", plural(n, "artifact", "artifacts")),
                move |gh, log| {
                    let mut ok = Vec::new();
                    let mut failed = Vec::new();
                    for (id, req) in &requests {
                        match gh.send(req, &mut log.calls) {
                            Ok(_) => ok.push(*id),
                            Err(e) => failed.push((*id, e.to_string())),
                        }
                    }
                    let result = if failed.is_empty() {
                        Ok(format!(
                            "{} deleted",
                            plural(ok.len(), "artifact", "artifacts")
                        ))
                    } else {
                        Err(failed
                            .iter()
                            .map(|(id, e)| format!("artifact {id}: {e}"))
                            .collect::<Vec<_>>()
                            .join("; "))
                    };
                    (result, Some(Msg::ArtifactsDeleted { ok, failed }))
                },
            );
        }
        Change::Caches { label, requests } => {
            ci.cache_busy = true;
            cx.gh_change(label, move |gh, log| {
                let mut ok = Vec::new();
                let mut failed = 0usize;
                let mut errors = Vec::new();
                for (req, ids) in &requests {
                    match gh.send(req, &mut log.calls) {
                        Ok(_) => ok.extend(ids.iter().copied()),
                        Err(e) => {
                            failed += ids.len().max(1);
                            errors.push(e.to_string());
                        }
                    }
                }
                let result = if errors.is_empty() {
                    Ok(format!("{} deleted", plural(ok.len(), "cache", "caches")))
                } else {
                    Err(format!(
                        "{} of {} requests failed: {}",
                        errors.len(),
                        requests.len(),
                        errors.join("; ")
                    ))
                };
                (result, Some(Msg::CachesDeleted { ok, failed }))
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Compare with the last green run
// ---------------------------------------------------------------------------

fn compare_open(cx: &mut Ctx, repo: &mut RepoState, bad: u64, auto_start: bool) {
    let Some(bad_run) = find_run(repo, bad) else {
        cx.toast(ToastKind::Info, format!("Run {bad} is not loaded"));
        return;
    };
    if repo
        .devops
        .ci
        .compare
        .as_ref()
        .is_some_and(|c| c.bad == bad)
    {
        if auto_start {
            compare_start(cx, repo);
        }
        return;
    }
    // Known runs: the run list and every loaded workflow history.
    let mut known: Vec<WorkflowRun> = runs_of(repo).to_vec();
    for h in repo.devops.ci.history.values().filter_map(|h| h.ok()) {
        known.extend(h.runs.iter().cloned());
    }
    let good = core_compare::last_green(&known, &bad_run).cloned();
    let finding = good.is_none();
    repo.devops.ci.compare = Some(CompareState {
        bad,
        auto_start,
        finding,
        good,
        missing: None,
        report: Default::default(),
        job: 0,
        lookup_calls: usize::from(finding),
        all_commits: false,
        all_files: false,
    });
    if finding {
        let ok = cx.gh_read(
            format!("Find the last green run before #{}", bad_run.run_number),
            move |gh, log| Msg::GreenFound {
                bad,
                result: core::fetch_last_green(gh, &bad_run, &mut log.calls)
                    .map(|g| g.map(Box::new))
                    .map_err(|e| e.to_string()),
            },
        );
        if !ok && let Some(c) = &mut repo.devops.ci.compare {
            c.finding = false;
            c.missing = Some("This repository has no GitHub remote".to_owned());
        }
    } else if auto_start {
        compare_start(cx, repo);
    }
}

/// API calls a comparison will take: `(calls, exact)`.
pub fn compare_calls(repo: &RepoState, bad: &WorkflowRun, good: &WorkflowRun) -> (usize, bool) {
    let bad_jobs = repo
        .gh_page
        .runs
        .get(&bad.id)
        .and_then(|d| d.jobs.as_ref())
        .and_then(|j| j.as_ref().ok());
    let local = |sha: &str| {
        hadion_core::Oid::from_str(sha)
            .ok()
            .zip(repo.snapshot.as_ref())
            .is_some_and(|(o, s)| s.index_of.contains_key(&o))
    };
    let mut calls = 1; // jobs of the green run
    if !(local(&good.sha) && local(&bad.sha)) {
        calls += 1;
    }
    match bad_jobs {
        Some(jobs) => {
            let failing = jobs
                .iter()
                .filter(|j| j.outcome().is_bad())
                .count()
                .min(COMPARE_MAX_JOBS);
            (calls + 2 * failing, true)
        }
        None => (calls + 1 + 2, false),
    }
}

fn compare_start(cx: &mut Ctx, repo: &mut RepoState) {
    let Some(c) = &repo.devops.ci.compare else {
        return;
    };
    let Some(good) = c.good.clone() else { return };
    let bad_id = c.bad;
    let Some(bad) = find_run(repo, bad_id) else {
        return;
    };
    let bad_jobs = repo
        .gh_page
        .runs
        .get(&bad_id)
        .and_then(|d| d.jobs.as_ref())
        .and_then(|j| j.as_ref().ok())
        .cloned();
    let Some(c) = &mut repo.devops.ci.compare else {
        return;
    };
    if !c.report.start() {
        return;
    }
    let label = format!(
        "Compare #{} with the last green run #{}",
        bad.run_number, good.run_number
    );
    let ok = cx.gh_read(label, move |gh, log| {
        let started = Instant::now();
        let result = compare_task(gh, log, &good, &bad, bad_jobs, started).map(Box::new);
        Msg::Compared {
            bad: bad_id,
            result,
        }
    });
    if !ok && let Some(c) = &mut repo.devops.ci.compare {
        c.report
            .finish(Err("This repository has no GitHub remote".to_owned()));
    }
}

fn compare_task(
    gh: &Gh,
    log: &mut crate::devops::ctx::TaskLog,
    good: &WorkflowRun,
    bad: &WorkflowRun,
    bad_jobs: Option<Vec<hadion_core::github::Job>>,
    started: Instant,
) -> Result<CompareReport, String> {
    let dir = gh.dir().to_path_buf();
    // Commits in between: local git when both commits are here.
    let between =
        if core_compare::has_commit(&dir, &good.sha) && core_compare::has_commit(&dir, &bad.sha) {
            log.command(format!("git cat-file -e {}^{{commit}}", good.sha));
            log.command(format!("git cat-file -e {}^{{commit}}", bad.sha));
            core_compare::local_between(&dir, &good.sha, &bad.sha, &mut log.commands)
                .map_err(|e| e.to_string())
        } else {
            core::fetch_compare(gh, &good.sha, &bad.sha, &mut log.calls).map_err(|e| e.to_string())
        };
    let bad_jobs = match bad_jobs {
        Some(j) => j,
        None => core::fetch_jobs(gh, bad.id, &mut log.calls).map_err(|e| e.to_string())?,
    };
    let good_jobs = core::fetch_jobs(gh, good.id, &mut log.calls).map_err(|e| e.to_string())?;
    let failing: Vec<_> = bad_jobs
        .iter()
        .filter(|j| j.outcome().is_bad())
        .cloned()
        .collect();
    let skipped: Vec<String> = failing
        .iter()
        .skip(COMPARE_MAX_JOBS)
        .map(|j| j.name.clone())
        .collect();
    let mut jobs = Vec::new();
    for bj in failing.into_iter().take(COMPARE_MAX_JOBS) {
        let gj = good_jobs.iter().find(|j| j.name == bj.name).cloned();
        let diff = match &gj {
            None => Err(format!(
                "The green run #{} has no job named {}",
                good.run_number, bj.name
            )),
            Some(gj) => {
                let fetch = |id: u64, log: &mut crate::devops::ctx::TaskLog| {
                    hadion_core::github::fetch_job_log(
                        gh.dir(),
                        &gh.owner,
                        &gh.repo,
                        id,
                        &mut log.calls,
                    )
                    .map(|raw| hadion_core::joblog::parse(&raw))
                    .map_err(|e| e.to_string())
                };
                match (fetch(gj.id, log), fetch(bj.id, log)) {
                    (Ok(gl), Ok(bl)) => {
                        let ids: Vec<String> = [
                            good.id.to_string(),
                            bad.id.to_string(),
                            gj.id.to_string(),
                            bj.id.to_string(),
                            good.sha.clone(),
                            bad.sha.clone(),
                            good.sha.chars().take(7).collect(),
                            bad.sha.chars().take(7).collect(),
                            good.run_number.to_string(),
                            bad.run_number.to_string(),
                        ]
                        .into_iter()
                        .filter(|s| s.len() >= 4)
                        .collect();
                        Ok(core_compare::diff_logs_from(
                            &gl,
                            setup_end(gj, &gl),
                            &bl,
                            setup_end(&bj, &bl),
                            &ids,
                            3,
                        ))
                    }
                    (Err(e), _) => Err(format!("Log of the green job: {e}")),
                    (_, Err(e)) => Err(format!("Log of the failing job: {e}")),
                }
            }
        };
        jobs.push(JobDiff {
            name: bj.name.clone(),
            bad: bj,
            good: gj,
            diff,
        });
    }
    Ok(CompareReport {
        good: good.clone(),
        bad: bad.clone(),
        between,
        jobs,
        skipped,
        calls: log.calls.len(),
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// The first line after a job's "Set up job" step (runner version, image
/// and permissions, which differ in every run), or 0. The log's own
/// `Complete job name:` line marks it; step start times (whole seconds)
/// are the fallback.
fn setup_end(job: &hadion_core::github::Job, log: &hadion_core::joblog::JobLog) -> usize {
    if let Some(end) = core_compare::setup_end(log) {
        return end;
    }
    let Some(first) = job.steps.iter().min_by_key(|s| s.number) else {
        return 0;
    };
    if first.name != "Set up job" {
        return 0;
    }
    let steps: Vec<(u32, Option<i64>)> = job.steps.iter().map(|s| (s.number, s.started)).collect();
    log.step_starts(&steps)
        .into_iter()
        .find(|(n, _)| *n != first.number)
        .map(|(_, line)| line)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Workflows
// ---------------------------------------------------------------------------

/// The GitHub id of the selected workflow.
pub fn selected_workflow_id(repo: &RepoState) -> Option<u64> {
    let path = repo.devops.ci.wf_selected.as_deref()?;
    repo.github
        .as_ref()?
        .workflows
        .iter()
        .find(|w| w.path == path)
        .map(|w| w.id)
}

/// The name of the GitHub remote (`origin` usually).
fn github_remote_name(repo: &RepoState) -> Option<String> {
    let (owner, name) = repo.github_remote.as_ref()?;
    let snap = repo.snapshot.as_ref()?;
    snap.remotes
        .iter()
        .find(|r| {
            hadion_core::github::parse_remote(&r.url)
                .is_some_and(|(o, n)| o.eq_ignore_ascii_case(owner) && n.eq_ignore_ascii_case(name))
        })
        .map(|r| r.name.clone())
}

fn load_local(cx: &mut Ctx, repo: &mut RepoState) {
    if !repo.devops.ci.local.start() {
        return;
    }
    let remote = github_remote_name(repo).unwrap_or_else(|| "origin".to_owned());
    let ok = cx.read("Read the workflow files", move |dir, log| {
        log.command("read .github/workflows/*.yml, *.yaml");
        let files: Vec<_> = workflow::read_dir(dir)
            .into_iter()
            .map(|(p, t)| workflow::parse(&p, &t))
            .collect();
        let head = format!("refs/remotes/{remote}/HEAD");
        log.command(format!("git symbolic-ref --short {head}"));
        let default_branch = hadion_core::cli::run(dir, &["symbolic-ref", "--short", &head], None)
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .map(|s| {
                s.strip_prefix(&format!("{remote}/"))
                    .map(str::to_owned)
                    .unwrap_or(s)
            })
            .filter(|s| !s.is_empty());
        Msg::Local(Ok(LocalInfo {
            files,
            default_branch,
        }))
    });
    if !ok {
        repo.devops.ci.local.loading = false;
    }
}

fn select_workflow(cx: &mut Ctx, repo: &mut RepoState, path: Option<String>) {
    let ci = &mut repo.devops.ci;
    if ci
        .dispatch
        .as_ref()
        .is_some_and(|d| Some(&d.path) != path.as_ref())
    {
        ci.dispatch = None;
    }
    ci.wf_selected = path;
    if let Some(id) = selected_workflow_id(repo) {
        load_history(cx, repo, id, false);
    }
}

fn load_history(cx: &mut Ctx, repo: &mut RepoState, workflow: u64, force: bool) {
    let branch = repo.devops.ci.wf_branch_applied.clone();
    let key = (workflow, branch.clone());
    let slot = repo.devops.ci.history.entry(key.clone()).or_default();
    if (!force && !slot.unloaded()) || !slot.start() {
        return;
    }
    let label = if branch.is_empty() {
        format!("Run history of workflow {workflow}")
    } else {
        format!("Run history of workflow {workflow} on {branch}")
    };
    let ok = cx.gh_read(label, move |gh, log| {
        let b = (!branch.is_empty()).then_some(branch.as_str());
        Msg::History {
            key,
            result: core::fetch_workflow_runs(gh, workflow, b, &mut log.calls)
                .map(|(runs, total)| History { runs, total })
                .map_err(|e| e.to_string()),
        }
    });
    if !ok {
        repo.devops
            .ci
            .history
            .remove(&(workflow, repo.devops.ci.wf_branch_applied.clone()));
    }
}

fn analyse_jobs(cx: &mut Ctx, repo: &mut RepoState, workflow: u64) {
    let branch = repo.devops.ci.wf_branch_applied.clone();
    let key = (workflow, branch);
    let Some(h) = repo.devops.ci.history.get(&key).and_then(|h| h.ok()) else {
        return;
    };
    let runs: Vec<WorkflowRun> = history::analysis_runs(&h.runs, ANALYSE_MAX_RUNS)
        .into_iter()
        .cloned()
        .collect();
    if runs.is_empty() {
        return;
    }
    let slot = repo.devops.ci.analysis.entry(key.clone()).or_default();
    if !slot.start() {
        return;
    }
    let refs: Vec<&WorkflowRun> = runs.iter().collect();
    let calls = history::analysis_calls(&refs);
    let label = format!(
        "Analyse the jobs of {} ({calls} API calls)",
        plural(runs.len(), "run", "runs")
    );
    cx.gh_read(label, move |gh, log| {
        let mut done = Vec::new();
        let mut failed = Vec::new();
        let mut attempts = 0usize;
        for r in &runs {
            let mut per = Vec::new();
            for a in 1..=r.attempt.max(1) {
                match core::fetch_attempt_jobs(gh, r.id, a, &mut log.calls) {
                    Ok(j) => {
                        attempts += 1;
                        per.push(j);
                    }
                    Err(e) => failed.push((r.id, format!("attempt {a}: {e}"))),
                }
            }
            done.push(history::RunJobs {
                run_id: r.id,
                attempts: per,
            });
        }
        Msg::Analysis {
            key,
            result: Ok(Analysis {
                jobs: history::job_stats(&done),
                runs: done.len(),
                attempts,
                calls: log.calls.len(),
                failed,
            }),
        }
    });
}

fn ask_toggle_workflow(cx: &mut Ctx, repo: &RepoState, id: u64, enable: bool) {
    let Some(gh) = need_gh(cx) else { return };
    let name = repo
        .github
        .as_ref()
        .and_then(|d| d.workflows.iter().find(|w| w.id == id))
        .map(|w| format!("{} ({})", w.name, w.path))
        .unwrap_or_else(|| format!("workflow {id}"));
    let request = if enable {
        requests::enable_workflow(&gh, id)
    } else {
        requests::disable_workflow(&gh, id)
    };
    let details = request_details(&[&request]);
    let (title, body, confirm) = if enable {
        (
            format!("Enable {name}?"),
            "Its triggers start runs again from now on. Runs missed while it was disabled are not started.".to_owned(),
            "Enable workflow",
        )
    } else {
        (
            format!("Disable {name}?"),
            "No new runs start (pushes, pull requests, schedules and dispatches are ignored) until it is enabled again. Runs in progress continue.".to_owned(),
            "Disable workflow",
        )
    };
    cx.confirm(
        title,
        body,
        confirm,
        !enable,
        details,
        Act::Send(Change::Workflow {
            id,
            enable,
            request,
        })
        .into(),
    );
}

/// Branches and tags of the GitHub remote, for the ref picker.
pub fn dispatch_refs(repo: &RepoState) -> (Vec<String>, Vec<String>) {
    let Some(snap) = repo.snapshot.as_ref() else {
        return (Vec::new(), Vec::new());
    };
    let remote = github_remote_name(repo).unwrap_or_else(|| "origin".to_owned());
    let mut branches: Vec<String> = snap
        .remotes
        .iter()
        .filter(|r| r.name == remote)
        .flat_map(|r| r.branches.iter())
        .map(|b| b.branch.clone())
        .filter(|b| b != "HEAD")
        .collect();
    branches.sort();
    branches.dedup();
    let tags: Vec<String> = snap.tags.iter().map(|t| t.name.clone()).collect();
    (branches, tags)
}

fn default_ref(repo: &RepoState) -> String {
    let (branches, _) = dispatch_refs(repo);
    let current = repo
        .snapshot
        .as_ref()
        .and_then(|s| s.head.branch().map(str::to_owned));
    if let Some(c) = current
        && branches.contains(&c)
    {
        return c;
    }
    if let Some(d) = repo
        .devops
        .ci
        .local
        .ok()
        .and_then(|l| l.default_branch.clone())
    {
        return d;
    }
    ["main", "master"]
        .iter()
        .find(|b| branches.iter().any(|x| x == *b))
        .map(|b| (*b).to_owned())
        .or_else(|| branches.first().cloned())
        .unwrap_or_else(|| "main".to_owned())
}

fn open_dispatch(cx: &mut Ctx, repo: &mut RepoState, path: &str) {
    let Some(local) = repo.devops.ci.local.ok() else {
        load_local(cx, repo);
        return;
    };
    let Some(file) = local.files.iter().find(|f| f.path == path) else {
        cx.toast(
            ToastKind::Info,
            format!("{path} is not in the working tree, so its inputs are unknown"),
        );
        return;
    };
    let Some(inputs) = file.dispatch.clone() else {
        cx.toast(
            ToastKind::Info,
            format!("{} has no workflow_dispatch trigger", file.name),
        );
        return;
    };
    let workflow_id = repo
        .github
        .as_ref()
        .and_then(|d| d.workflows.iter().find(|w| w.path == path))
        .map(|w| w.id);
    let values = workflow::default_values(&inputs);
    let form = DispatchForm {
        path: path.to_owned(),
        name: file.name.clone(),
        workflow_id,
        inputs,
        values,
        git_ref: default_ref(repo),
        ..Default::default()
    };
    repo.devops.ci.wf_selected = Some(path.to_owned());
    repo.devops.ci.dispatch = Some(form);
    if let Some(id) = workflow_id {
        load_history(cx, repo, id, false);
    }
}

fn ask_dispatch(cx: &mut Ctx, repo: &mut RepoState) {
    let Some(gh) = need_gh(cx) else { return };
    let (branches, tags) = dispatch_refs(repo);
    let Some(form) = &mut repo.devops.ci.dispatch else {
        return;
    };
    let mut errs: Vec<String> = workflow::validate_values(&form.inputs, &form.values)
        .into_iter()
        .map(|(_, e)| e)
        .collect();
    let git_ref = form.git_ref.trim().to_owned();
    if git_ref.is_empty() {
        errs.insert(0, "Pick the branch or tag to run on".to_owned());
    }
    if !errs.is_empty() {
        form.error = Some(errs.join("\n"));
        return;
    }
    form.error = None;
    let workflow = form.workflow_id.map(|i| i.to_string()).unwrap_or_else(|| {
        form.path
            .rsplit('/')
            .next()
            .unwrap_or(&form.path)
            .to_owned()
    });
    let inputs = workflow::request_inputs(&form.inputs, &form.values);
    let request = requests::dispatch(&gh, &workflow, &git_ref, &inputs);
    let mut body = format!(
        "Starts {} ({}) on {git_ref} with {}.",
        form.name,
        form.path,
        if inputs.is_empty() {
            "no inputs".to_owned()
        } else {
            plural(inputs.len(), "input", "inputs")
        }
    );
    if !branches.contains(&git_ref) && !tags.contains(&git_ref) {
        body.push_str(&format!(
            "\n\n{git_ref} is not a branch or tag Hadion knows on the GitHub remote; GitHub rejects refs it does not have."
        ));
    }
    if form.workflow_id.is_none() {
        body.push_str("\n\nThis workflow is not on GitHub yet: the dispatch only works once the file is on the default branch.");
    }
    let details = request_details(&[&request]);
    let workflow_id = form.workflow_id;
    cx.confirm(
        format!("Run {}?", form.name),
        body,
        "Run workflow",
        false,
        details,
        Act::Send(Change::Dispatch {
            request,
            workflow_id,
            git_ref,
        })
        .into(),
    );
}

// ---------------------------------------------------------------------------
// Artifacts
// ---------------------------------------------------------------------------

fn load_artifacts(cx: &mut Ctx, repo: &mut RepoState) {
    if !repo.devops.ci.artifacts.start() {
        return;
    }
    let ok = cx.gh_read("Artifacts of the repository", |gh, log| {
        Msg::Artifacts(
            core::fetch_artifacts(gh, 3, &mut log.calls)
                .map(|(items, total)| ArtifactList { items, total })
                .map_err(|e| e.to_string()),
        )
    });
    if !ok {
        repo.devops.ci.artifacts.loading = false;
    }
}

fn find_artifact(repo: &RepoState, id: u64) -> Option<model::Artifact> {
    let ci = &repo.devops.ci;
    ci.artifacts
        .ok()
        .and_then(|l| l.items.iter().find(|a| a.id == id))
        .or_else(|| {
            ci.run_artifacts
                .values()
                .filter_map(|l| l.ok())
                .flatten()
                .find(|a| a.id == id)
        })
        .cloned()
}

fn download_artifact(cx: &mut Ctx, repo: &mut RepoState, id: u64, dir: Option<PathBuf>) {
    let Some(a) = find_artifact(repo, id) else {
        return;
    };
    let Some(run) = a.run_id() else {
        cx.toast(
            ToastKind::Info,
            format!("{} has no run to download it from", a.name),
        );
        return;
    };
    if a.expired {
        cx.toast(
            ToastKind::Info,
            format!("{} has expired; GitHub no longer has its files", a.name),
        );
        return;
    }
    let dir = match dir {
        Some(d) => d,
        None => match rfd::FileDialog::new()
            .set_title(format!("Download {} into…", a.name))
            .set_directory(&repo.workdir)
            .pick_folder()
        {
            Some(d) => d,
            None => return,
        },
    };
    let mut spec = ProcSpec::new(gh_program(), &repo.workdir)
        .arg("run")
        .arg("download")
        .arg(run.to_string())
        .arg("-n")
        .arg(a.name.clone())
        .arg("-D")
        .arg(dir.display().to_string());
    if let Some((o, n)) = &repo.github_remote {
        spec = spec.arg("-R").arg(format!("{o}/{n}"));
    }
    let console = cx.console(
        format!(
            "Download {} ({})",
            a.name,
            super::widgets::bytes(a.size_in_bytes)
        ),
        spec,
    );
    repo.devops.ci.downloads.insert(id, (console, dir));
}

fn ask_delete_artifact(cx: &mut Ctx, repo: &RepoState, id: u64) {
    let Some(gh) = need_gh(cx) else { return };
    let Some(a) = find_artifact(repo, id) else {
        return;
    };
    let req = requests::delete_artifact(&gh, id);
    let details = request_details(&[&req]);
    let run = a
        .run_id()
        .map(|r| format!("run {r}"))
        .unwrap_or_else(|| "an unknown run".to_owned());
    cx.confirm(
        format!("Delete the artifact {}?", a.name),
        format!(
            "{} from {run}{} is deleted for good; the run keeps its logs.",
            super::widgets::bytes(a.size_in_bytes),
            if a.branch().is_empty() {
                String::new()
            } else {
                format!(" on {}", a.branch())
            }
        ),
        "Delete artifact",
        true,
        details,
        Act::Send(Change::Artifacts {
            requests: vec![(id, req)],
        })
        .into(),
    );
}

// ---------------------------------------------------------------------------
// Caches
// ---------------------------------------------------------------------------

fn load_caches(cx: &mut Ctx, repo: &mut RepoState) {
    if !repo.devops.ci.caches.start() {
        return;
    }
    let sort = repo.devops.ci.cache_sort;
    let ok = cx.gh_read("Caches of the repository", move |gh, log| {
        let usage = core::fetch_cache_usage(gh, &mut log.calls).ok();
        Msg::Caches(
            core::fetch_caches(gh, sort.api(), 5, &mut log.calls)
                .map(|(items, total)| CacheList {
                    usage,
                    items,
                    total,
                    sort,
                })
                .map_err(|e| e.to_string()),
        )
    });
    if !ok {
        repo.devops.ci.caches.loading = false;
    }
}

fn caches_of(repo: &RepoState) -> &[model::Cache] {
    repo.devops
        .ci
        .caches
        .ok()
        .map(|c| c.items.as_slice())
        .unwrap_or(&[])
}

fn ask_delete_cache(cx: &mut Ctx, repo: &RepoState, id: u64) {
    let Some(gh) = need_gh(cx) else { return };
    let Some(c) = caches_of(repo).iter().find(|c| c.id == id).cloned() else {
        return;
    };
    let req = requests::delete_cache(&gh, id);
    let details = request_details(&[&req]);
    cx.confirm(
        "Delete this cache?".to_owned(),
        format!(
            "{} on {} ({}). The next run with this key starts without it and saves a new one.",
            c.key,
            c.git_ref,
            super::widgets::bytes(c.size_in_bytes)
        ),
        "Delete cache",
        true,
        details,
        Act::Send(Change::Caches {
            label: format!("Delete cache {}", c.key),
            requests: vec![(req, vec![id])],
        })
        .into(),
    );
}

fn ask_delete_cache_key(cx: &mut Ctx, repo: &RepoState, key: &str, git_ref: &str) {
    let Some(gh) = need_gh(cx) else { return };
    let hit: Vec<&model::Cache> = caches_of(repo)
        .iter()
        .filter(|c| c.key == key && c.git_ref == git_ref)
        .collect();
    let bytes: u64 = hit.iter().map(|c| c.size_in_bytes).sum();
    let req = requests::delete_caches_by_key(&gh, key, git_ref);
    let details = request_details(&[&req]);
    cx.confirm(
        "Delete every version of this cache key?".to_owned(),
        format!(
            "{key} on {git_ref}: {} listed ({}), every version.",
            plural(hit.len(), "cache", "caches"),
            super::widgets::bytes(bytes)
        ),
        "Delete caches",
        true,
        details,
        Act::Send(Change::Caches {
            label: format!("Delete caches {key} on {git_ref}"),
            requests: vec![(req, hit.iter().map(|c| c.id).collect())],
        })
        .into(),
    );
}

/// Delete every cache of the given refs: one request per distinct key and
/// ref.
fn ask_delete_refs(cx: &mut Ctx, repo: &RepoState, refs: &[String], why: Option<&str>) {
    let Some(gh) = need_gh(cx) else { return };
    let mut groups: BTreeMap<(String, String), Vec<&model::Cache>> = BTreeMap::new();
    for c in caches_of(repo).iter().filter(|c| refs.contains(&c.git_ref)) {
        groups
            .entry((c.key.clone(), c.git_ref.clone()))
            .or_default()
            .push(c);
    }
    if groups.is_empty() {
        cx.toast(ToastKind::Info, "No caches to delete".to_owned());
        return;
    }
    let count: usize = groups.values().map(Vec::len).sum();
    let bytes: u64 = groups.values().flatten().map(|c| c.size_in_bytes).sum();
    let requests: Vec<(Request, Vec<u64>)> = groups
        .iter()
        .map(|((k, r), cs)| {
            (
                requests::delete_caches_by_key(&gh, k, r),
                cs.iter().map(|c| c.id).collect(),
            )
        })
        .collect();
    let refs_set: BTreeSet<&String> = groups.keys().map(|(_, r)| r).collect();
    let mut body = format!(
        "{} ({}) on {}: ",
        plural(count, "cache", "caches"),
        super::widgets::bytes(bytes),
        plural(refs_set.len(), "ref", "refs")
    );
    let shown: Vec<String> = refs_set.iter().take(8).map(|r| (*r).clone()).collect();
    body.push_str(&shown.join(", "));
    if refs_set.len() > 8 {
        body.push_str(&format!(" and {} more", refs_set.len() - 8));
    }
    body.push_str(&format!(
        ".\n\n{} (one per cache key and ref).",
        plural(requests.len(), "request", "requests")
    ));
    if let Some(w) = why {
        body.push_str(&format!(" These refs belong to {w}."));
    }
    let reqs: Vec<&Request> = requests.iter().map(|(r, _)| r).collect();
    let details = request_details(&reqs);
    let title = match why {
        Some(w) => format!("Delete the caches of {w}?"),
        None if refs.len() == 1 => format!("Delete every cache of {}?", refs[0]),
        None => "Delete these caches?".to_owned(),
    };
    cx.confirm(
        title,
        body,
        format!("Delete {}", plural(count, "cache", "caches")),
        true,
        details,
        Act::Send(Change::Caches {
            label: format!("Delete {}", plural(count, "cache", "caches")),
            requests,
        })
        .into(),
    );
}

/// Pull request numbers among the cache refs.
pub fn cache_prs(repo: &RepoState) -> Vec<u64> {
    let set: BTreeSet<u64> = caches_of(repo)
        .iter()
        .filter_map(|c| c.pr_number())
        .collect();
    set.into_iter().collect()
}

/// Cache refs of pull requests GitHub reports closed or merged.
pub fn closed_pr_refs(repo: &RepoState) -> Vec<String> {
    let Some(states) = repo.devops.ci.pr_states.ok() else {
        return Vec::new();
    };
    let set: BTreeSet<String> = caches_of(repo)
        .iter()
        .filter(|c| {
            c.pr_number()
                .and_then(|n| states.get(&n))
                .is_some_and(|s| s != "OPEN")
        })
        .map(|c| c.git_ref.clone())
        .collect();
    set.into_iter().collect()
}

fn check_pr_states(cx: &mut Ctx, repo: &mut RepoState) {
    let numbers = cache_prs(repo);
    if numbers.is_empty() || !repo.devops.ci.pr_states.start() {
        return;
    }
    let calls = numbers.len().div_ceil(100);
    let ok = cx.gh_read(
        format!(
            "State of {} ({} GraphQL {})",
            plural(numbers.len(), "pull request", "pull requests"),
            calls,
            if calls == 1 { "request" } else { "requests" }
        ),
        move |gh, log| {
            Msg::PrStates(
                core::fetch_pr_states(gh, &numbers, &mut log.calls).map_err(|e| e.to_string()),
            )
        },
    );
    if !ok {
        repo.devops.ci.pr_states.loading = false;
    }
}

// ---------------------------------------------------------------------------
// Validate
// ---------------------------------------------------------------------------

fn validate(cx: &mut Ctx, repo: &mut RepoState) {
    if !repo.devops.ci.lint.start() {
        return;
    }
    let previous = repo
        .devops
        .ci
        .lint
        .ok()
        .map(|r| r.files.clone())
        .unwrap_or_default();
    let ok = cx.read("Validate the workflow files", move |dir, log| {
        log.command("lint .github/workflows/*.yml, *.yaml (built-in rules)");
        Msg::Linted(Box::new(core::lint_workdir(
            dir,
            &previous,
            true,
            &mut log.commands,
        )))
    });
    if !ok {
        repo.devops.ci.lint.loading = false;
    }
}

// ---------------------------------------------------------------------------
// Run locally
// ---------------------------------------------------------------------------

fn check_tools(cx: &mut Ctx, repo: &mut RepoState) {
    if !repo.devops.ci.act.tools.start() {
        return;
    }
    let ok = cx.read("Look for act and Docker", |dir, log| {
        let act = tools::detect(Tool::Act);
        let docker = tools::detect(Tool::Docker);
        let docker_running = docker.as_ref().map(|d| {
            let spec = ProcSpec::new(d.path.display().to_string(), dir).args([
                "info",
                "--format",
                "{{.ServerVersion}}",
            ]);
            log.command(spec.display());
            run_capture(&spec, Some(Duration::from_secs(20)))
                .map(|c| c.success() && !c.stdout_text().trim().is_empty())
                .unwrap_or(false)
        });
        Msg::Tools(Box::new(Tools {
            act,
            docker,
            docker_running,
        }))
    });
    if !ok {
        repo.devops.ci.act.tools.loading = false;
    }
}

fn list_act_jobs(cx: &mut Ctx, repo: &mut RepoState) {
    let has_act = repo
        .devops
        .ci
        .act
        .tools
        .ok()
        .is_some_and(|t| t.act.is_some());
    if !has_act || !repo.devops.ci.act.list.start() {
        return;
    }
    let ok = cx.read("act -l", |dir, log| {
        let spec = core_act::list_spec(dir);
        log.command(spec.display());
        Msg::ActList(match run_capture(&spec, Some(Duration::from_secs(60))) {
            Err(e) => Err(e.to_string()),
            Ok(c) if !c.success() => Err(c.error_text()),
            Ok(c) => Ok(core_act::parse_list(&c.stdout_text())),
        })
    });
    if !ok {
        repo.devops.ci.act.list.loading = false;
    }
}

/// The `act` invocation the form describes.
pub fn act_run(state: &ActState, files: &[workflow::WorkflowFile]) -> core_act::ActRun {
    let file = files.iter().find(|f| f.path == state.workflow);
    let dispatch = file.and_then(|f| f.dispatch.as_ref());
    let inputs = if state.event == "workflow_dispatch" {
        dispatch
            .map(|inputs| {
                inputs
                    .iter()
                    .filter_map(|i| {
                        let v = state.inputs.get(&i.name)?.trim().to_owned();
                        (!v.is_empty()).then(|| (i.name.clone(), v))
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let image = if state.image.trim().is_empty() {
        core_act::DEFAULT_IMAGE.to_owned()
    } else {
        state.image.trim().to_owned()
    };
    core_act::ActRun {
        event: state.event.clone(),
        job: state.job.clone(),
        workflow_file: state.workflow.clone(),
        inputs,
        platform: Some(("ubuntu-latest".to_owned(), image)),
        secret_file: state.secret_file.clone(),
    }
}

fn run_act(cx: &mut Ctx, repo: &mut RepoState) {
    let files = repo
        .devops
        .ci
        .local
        .ok()
        .map(|l| l.files.clone())
        .unwrap_or_default();
    let act = &repo.devops.ci.act;
    if act.job.trim().is_empty() || act.workflow.trim().is_empty() {
        cx.toast(
            ToastKind::Info,
            "Pick a workflow and a job first".to_owned(),
        );
        return;
    }
    let run = act_run(act, &files);
    let spec = run.spec(&repo.workdir);
    let id = cx.console(format!("act {} -j {}", run.event, run.job), spec);
    let act = &mut repo.devops.ci.act;
    act.runs.push(id);
    act.shown = Some(id);
}

pub fn console_done(_cx: &mut Ctx, _repo: &mut RepoState, _run: &ConsoleRun) {}

// ---------------------------------------------------------------------------
// Replies
// ---------------------------------------------------------------------------

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    match msg {
        Msg::RunsChanged {
            ok,
            deleted,
            failed,
        } => {
            let ci = &mut repo.devops.ci;
            for id in ok
                .iter()
                .chain(&deleted)
                .chain(failed.iter().map(|(i, _)| i))
            {
                ci.busy_runs.remove(id);
            }
            for id in &deleted {
                ci.picked.remove(id);
                ci.run_artifacts.remove(id);
                if ci.compare.as_ref().is_some_and(|c| c.bad == *id) {
                    ci.compare = None;
                }
            }
            if !deleted.is_empty() {
                if let Some(d) = repo.github.as_mut() {
                    d.runs.retain(|r| !deleted.contains(&r.id));
                }
                for h in repo.devops.ci.history.values_mut() {
                    if let Some(Ok(h)) = &mut h.data {
                        h.runs.retain(|r| !deleted.contains(&r.id));
                    }
                }
                if repo
                    .gh_page
                    .selected_run
                    .is_some_and(|s| deleted.contains(&s))
                {
                    repo.gh_page.selected_run = None;
                }
            }
            if failed.is_empty() && ok.len() > 1 {
                repo.devops.ci.picked.clear();
            }
            if !ok.is_empty() {
                refresh_runs(cx, ok);
            }
        }
        Msg::RunsRefreshed(runs) => {
            let selected = repo.gh_page.selected_run;
            for r in runs.into_iter().flatten() {
                let id = r.id;
                if let Some(d) = repo.github.as_mut() {
                    d.upsert_run(r);
                }
                if selected == Some(id) {
                    cx.action(Action::RefreshRun(id));
                }
            }
        }
        Msg::JobChanged { run, job, ok } => {
            repo.devops.ci.busy_jobs.remove(&job);
            if ok {
                refresh_runs(cx, vec![run]);
            }
        }
        Msg::RunArtifacts { run, result } => {
            repo.devops
                .ci
                .run_artifacts
                .entry(run)
                .or_default()
                .finish(result);
        }
        Msg::JobPolled {
            run,
            job,
            single,
            jobs,
            run_data,
        } => job_polled(repo, run, job, single, jobs, run_data),
        Msg::JobLog {
            run,
            job,
            attempt,
            result,
        } => job_log(cx, repo, run, job, attempt, result),
        Msg::GreenFound { bad, result } => {
            let Some(c) = repo.devops.ci.compare.as_mut().filter(|c| c.bad == bad) else {
                return;
            };
            c.finding = false;
            match result {
                Ok(Some(g)) => {
                    c.good = Some(*g);
                    if c.auto_start {
                        compare_start(cx, repo);
                    }
                }
                Ok(None) => c.missing = Some(
                    "GitHub has no successful run of this workflow on this branch before this one."
                        .to_owned(),
                ),
                Err(e) => c.missing = Some(e),
            }
        }
        Msg::Compared { bad, result } => {
            if let Some(c) = repo.devops.ci.compare.as_mut().filter(|c| c.bad == bad) {
                c.report.finish(result);
                c.job = 0;
            }
        }
        Msg::Local(result) => {
            repo.devops.ci.local.finish(result);
            if let Some(cmd) = repo.devops.ci.pending.take() {
                run_script(cx, repo, cmd);
            }
        }
        Msg::History { key, result } => {
            repo.devops
                .ci
                .history
                .entry(key)
                .or_default()
                .finish(result);
        }
        Msg::Analysis { key, result } => {
            repo.devops
                .ci
                .analysis
                .entry(key)
                .or_default()
                .finish(result);
        }
        Msg::WorkflowToggled { id, enable, ok } => {
            repo.devops.ci.wf_busy.remove(&id);
            if ok
                && let Some(w) = repo
                    .github
                    .as_mut()
                    .and_then(|d| d.workflows.iter_mut().find(|w| w.id == id))
            {
                w.state = if enable {
                    "active".to_owned()
                } else {
                    "disabled_manually".to_owned()
                };
            }
        }
        Msg::Dispatched {
            ok,
            workflow_id,
            git_ref,
        } => {
            if let Some(f) = &mut repo.devops.ci.dispatch {
                f.sending = false;
                match &ok {
                    Ok(()) => {
                        f.sent = Some((git_ref, Instant::now()));
                        f.error = None;
                    }
                    Err(e) => f.error = Some(e.clone()),
                }
            }
            if ok.is_ok() {
                // GitHub takes a few seconds to list a dispatched run.
                cx.after(Duration::from_secs(4), Act::DispatchSettled { workflow_id });
            }
        }
        Msg::Artifacts(result) => repo.devops.ci.artifacts.finish(result),
        Msg::ArtifactsDeleted { ok, failed } => {
            let ci = &mut repo.devops.ci;
            for id in ok.iter().chain(failed.iter().map(|(i, _)| i)) {
                ci.artifact_busy.remove(id);
            }
            if let Some(Ok(l)) = &mut ci.artifacts.data {
                let before = l.items.len();
                l.items.retain(|a| !ok.contains(&a.id));
                l.total = l.total.saturating_sub((before - l.items.len()) as u64);
            }
            for l in ci.run_artifacts.values_mut() {
                if let Some(Ok(v)) = &mut l.data {
                    v.retain(|a| !ok.contains(&a.id));
                }
            }
            if ci.artifact_selected.is_some_and(|s| ok.contains(&s)) {
                ci.artifact_selected = None;
            }
        }
        Msg::Caches(result) => repo.devops.ci.caches.finish(result),
        Msg::CachesDeleted { ok, .. } => {
            let ci = &mut repo.devops.ci;
            ci.cache_busy = false;
            if let Some(Ok(l)) = &mut ci.caches.data {
                let removed: u64 = l
                    .items
                    .iter()
                    .filter(|c| ok.contains(&c.id))
                    .map(|c| c.size_in_bytes)
                    .sum();
                let before = l.items.len();
                l.items.retain(|c| !ok.contains(&c.id));
                let n = (before - l.items.len()) as u64;
                l.total = l.total.saturating_sub(n);
                if let Some(u) = &mut l.usage {
                    u.active_caches_count = u.active_caches_count.saturating_sub(n);
                    u.active_caches_size_in_bytes =
                        u.active_caches_size_in_bytes.saturating_sub(removed);
                }
            }
            if let Some(CacheSel::Entry(id)) = &ci.cache_selected
                && ok.contains(id)
            {
                ci.cache_selected = None;
            }
        }
        Msg::PrStates(result) => repo.devops.ci.pr_states.finish(result),
        Msg::Linted(run) => {
            let ci = &mut repo.devops.ci;
            ci.lint.finish(Ok(*run));
            ci.lint_changed.clear();
            ci.lint_polled = Some(Instant::now());
        }
        Msg::Tools(t) => {
            let has_act = t.act.is_some();
            repo.devops.ci.act.tools.finish(Ok(*t));
            if has_act {
                repo.devops.ci.act.list = Default::default();
                list_act_jobs(cx, repo);
            }
        }
        Msg::ActList(result) => repo.devops.ci.act.list.finish(result),
    }
}

/// Fetch fresh copies of runs a moment after a change (GitHub takes a
/// second to show a re-run as queued).
fn refresh_runs(cx: &mut Ctx, ids: Vec<u64>) {
    let ids: Vec<u64> = ids.into_iter().take(10).collect();
    cx.after(Duration::from_millis(1500), Act::RefreshRuns(ids));
}

fn fetch_runs_now(cx: &mut Ctx, ids: Vec<u64>) {
    cx.gh_read(
        format!(
            "Refresh {} after the change",
            plural(ids.len(), "run", "runs")
        ),
        move |gh, log| {
            Msg::RunsRefreshed(
                ids.iter()
                    .map(|id| core::fetch_run(gh, *id, &mut log.calls).map_err(|e| e.to_string()))
                    .collect(),
            )
        },
    );
}

// ---------------------------------------------------------------------------
// Script driver
// ---------------------------------------------------------------------------

fn run_script(cx: &mut Ctx, repo: &mut RepoState, cmd: ScriptCmd) {
    match cmd {
        ScriptCmd::SelectRuns(n) => {
            let ids: Vec<u64> = visible_ids(repo).into_iter().take(n).collect();
            apply(cx, repo, Act::PickRuns(ids));
        }
        ScriptCmd::Workflow(n) | ScriptCmd::Dispatch(n) => {
            if repo.devops.ci.local.ok().is_none() {
                repo.devops.ci.pending = Some(cmd);
                if repo.devops.ci.local.unloaded() {
                    load_local(cx, repo);
                }
                return;
            }
            let dispatch = matches!(cmd, ScriptCmd::Dispatch(_));
            let entries = super::workflows::entries(repo);
            let path = entries
                .iter()
                .filter(|e| !dispatch || e.file.is_some_and(|f| f.dispatch.is_some()))
                .nth(n)
                .map(|e| e.path.clone());
            match path {
                Some(p) if dispatch => open_dispatch(cx, repo, &p),
                Some(p) => select_workflow(cx, repo, Some(p)),
                None => eprintln!("[script] no workflow {n}"),
            }
        }
        ScriptCmd::Compare | ScriptCmd::ComparePlan => {
            if let Some(run) = repo.gh_page.selected_run {
                compare_open(cx, repo, run, matches!(cmd, ScriptCmd::Compare));
            }
        }
        ScriptCmd::AnalyseJobs => {
            if let Some(id) = selected_workflow_id(repo) {
                analyse_jobs(cx, repo, id);
            }
        }
        ScriptCmd::Act => {
            check_tools(cx, repo);
            if repo.devops.ci.local.unloaded() {
                load_local(cx, repo);
            }
        }
        ScriptCmd::Artifact(n) => {
            let id = super::artifacts::visible(repo).get(n).map(|a| a.id);
            repo.devops.ci.artifact_selected = id;
        }
        ScriptCmd::Cache(n) => {
            repo.devops.ci.cache_view = super::CacheView::Entries;
            let id = super::caches::visible(repo).get(n).map(|c| c.id);
            repo.devops.ci.cache_selected = id.map(CacheSel::Entry);
        }
        ScriptCmd::CacheRefs => {
            repo.devops.ci.cache_view = super::CacheView::Refs;
            let first = super::caches::visible_refs(repo)
                .first()
                .map(|r| r.git_ref.clone());
            repo.devops.ci.cache_selected = first.map(CacheSel::Ref);
        }
        ScriptCmd::LintFile(n) => {
            let f = repo
                .devops
                .ci
                .lint
                .ok()
                .and_then(|r| r.files.get(n))
                .map(|f| f.path.clone());
            repo.devops.ci.lint_file = f;
        }
        ScriptCmd::AskCancelPicked => {
            let ids: Vec<u64> = repo.devops.ci.picked.iter().copied().collect();
            ask_cancel_runs(cx, repo, &ids, None);
        }
        ScriptCmd::AskRerunFailedPicked => {
            let ids: Vec<u64> = repo.devops.ci.picked.iter().copied().collect();
            ask_rerun_failed_runs(cx, repo, &ids);
        }
        ScriptCmd::AskDeleteRun => {
            if let Some(run) = repo.gh_page.selected_run {
                ask_delete_run(cx, repo, run);
            }
        }
        ScriptCmd::AskRerunDebug => {
            if let Some(run) = repo.gh_page.selected_run {
                let failed = find_run(repo, run).is_some_and(|r| r.is_failure());
                ask_rerun(cx, repo, run, failed, true);
            }
        }
        ScriptCmd::ActJob(n) => {
            if repo.devops.ci.local.ok().is_none() {
                repo.devops.ci.pending = Some(cmd);
                if repo.devops.ci.local.unloaded() {
                    load_local(cx, repo);
                }
                return;
            }
            let jobs = super::local::jobs(repo);
            if let Some(j) = jobs.get(n) {
                super::local::pick(&mut repo.devops.ci.act, j);
            }
        }
        ScriptCmd::ScrollTo(s) => repo.devops.ci.scroll_to = Some(s),
        ScriptCmd::ShowJob(n) => {
            let Some(run) = repo.gh_page.selected_run else {
                return;
            };
            let job = repo
                .gh_page
                .runs
                .get(&run)
                .and_then(|d| d.jobs().get(n))
                .cloned();
            // What the job's Log button does.
            if let Some(job) = job {
                if super::no_log_yet(&job) {
                    open_job(repo, run, job.id);
                } else {
                    cx.action(Action::ShowLog {
                        run,
                        job: job.id,
                        step: None,
                    });
                }
            }
        }
        ScriptCmd::AskRerunJob(n) => {
            let Some(run) = repo.gh_page.selected_run else {
                return;
            };
            let job = repo
                .gh_page
                .runs
                .get(&run)
                .and_then(|d| d.jobs().get(n))
                .map(|j| (j.id, j.name.clone()));
            if let Some((job, name)) = job {
                ask_rerun_job(cx, repo, run, job, &name, false);
            }
        }
        ScriptCmd::AskDispatch => ask_dispatch(cx, repo),
        ScriptCmd::AskDelete => match repo.devops.ci.tab {
            Tab::Artifacts => {
                if let Some(id) = repo.devops.ci.artifact_selected {
                    ask_delete_artifact(cx, repo, id);
                }
            }
            Tab::Caches => match repo.devops.ci.cache_selected.clone() {
                Some(CacheSel::Entry(id)) => ask_delete_cache(cx, repo, id),
                Some(CacheSel::Ref(r)) => ask_delete_refs(cx, repo, &[r], None),
                None => {}
            },
            Tab::Workflows => {
                if let Some(id) = selected_workflow_id(repo) {
                    ask_toggle_workflow(cx, repo, id, false);
                }
            }
            _ => {}
        },
        ScriptCmd::DownloadArtifact(n, dir) => {
            if let Some(id) = super::artifacts::visible(repo).get(n).map(|a| a.id) {
                download_artifact(cx, repo, id, Some(dir));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hadion_core::devops::ci::workflow::parse;

    #[test]
    fn act_form_builds_the_command() {
        let wf = parse(
            ".github/workflows/release.yml",
            "on:\n  push:\n  workflow_dispatch:\n    inputs:\n      version:\n        required: true\n      dry_run:\n        type: boolean\njobs:\n  release:\n    runs-on: ubuntu-latest\n    steps: [{run: x}]\n",
        );
        let mut act = ActState {
            workflow: wf.path.clone(),
            job: "release".into(),
            event: "workflow_dispatch".into(),
            secret_file: Some(PathBuf::from("C:/s/.secrets")),
            ..Default::default()
        };
        act.inputs.insert("version".into(), "1.2.3".into());
        act.inputs.insert("dry_run".into(), " ".into());
        act.inputs.insert("stray".into(), "x".into());
        let run = act_run(&act, std::slice::from_ref(&wf));
        assert_eq!(
            run.args(),
            [
                "workflow_dispatch",
                "-j",
                "release",
                "-W",
                ".github/workflows/release.yml",
                "--input",
                "version=1.2.3",
                "-P",
                "ubuntu-latest=catthehacker/ubuntu:act-latest",
                "--secret-file",
                "C:/s/.secrets",
            ]
        );
        // Inputs only go with workflow_dispatch; a custom image replaces the default.
        act.event = "push".into();
        act.image = "node:20".into();
        let run = act_run(&act, &[wf]);
        assert!(!run.args().iter().any(|a| a == "--input"));
        assert!(run.args().contains(&"ubuntu-latest=node:20".to_owned()));
    }

    #[test]
    fn confirmation_details_show_requests_and_bodies() {
        let gh = Gh::new(".", "o", "r");
        let a = requests::rerun(&gh, 1, true);
        let b = requests::cancel(&gh, 2);
        let d = request_details(&[&a, &b]);
        assert_eq!(d.len(), 2);
        assert!(d[0].starts_with(
            "gh api --method POST repos/o/r/actions/runs/1/rerun --input - (JSON body)\n{"
        ));
        assert!(d[0].contains("\"enable_debug_logging\": true"));
        assert_eq!(d[1], "gh api --method POST repos/o/r/actions/runs/2/cancel");
        let many: Vec<Request> = (0..45).map(|i| requests::delete_cache(&gh, i)).collect();
        let refs: Vec<&Request> = many.iter().collect();
        let d = request_details(&refs);
        assert_eq!(d.len(), 41);
        assert!(d[40].contains("5 more requests"));
    }
}
