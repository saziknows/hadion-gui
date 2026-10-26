use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use egui::{Frame, Key, Margin, Modifiers};
use gitvis_core::github::{ApiCall, Outcome, aggregate};
use gitvis_core::insights::search::SearchQuery;
use gitvis_core::insights::{InsightsOptions, InsightsReport};
use gitvis_core::ops::{GitOp, Refresh};
use gitvis_core::osint::{OsintOptions, OsintReport};
use gitvis_core::secrets::{HistoryScope, Report, ScanOptions, Severity};
use gitvis_core::{DiffOptions, DiffTarget, LoadOptions, Oid, Snapshot, github, reflog, repo};

use crate::format;
use crate::script::{Cmd, Script};
use crate::state::{
    Action, ActivityEntry, ActivityKind, ActivityTab, ChecksState, CiInfo, Dialog, Dirty,
    GitHubTab, InsightsTab, LogDoc, LogState, Page, PrState, RepoId, RepoState, RunDetail, ScanRun,
    SecurityTab, Selection, Settings, Toast, ToastKind,
};
use crate::state::{DiffMode, DiffSelection};
use crate::theme;
use crate::views;
use crate::worker::{Event, GhRepo, Job, Worker};

const DEBOUNCE: Duration = Duration::from_millis(250);
const DEBOUNCE_MAX: Duration = Duration::from_millis(1500);
const SUPPRESS_AFTER_OP: Duration = Duration::from_millis(700);
const MAX_COMMITS: usize = 50_000;
/// GitHub data is refreshed at most this often unless asked for.
const GITHUB_REFRESH: Duration = Duration::from_secs(120);
/// Check runs of a commit are re-fetched after this when some were running.
const CHECKS_REFRESH: Duration = Duration::from_secs(60);
/// How long a commit must stay selected before its checks are requested.
const CHECKS_DEBOUNCE: Duration = Duration::from_millis(400);
const MAX_ACTIVITY: usize = 2_000;
/// Failed jobs whose annotations load without asking, per run.
const AUTO_ANNOTATIONS: usize = 8;

pub struct GitVis {
    settings: Settings,
    repos: Vec<RepoState>,
    active: usize,
    worker: Worker,
    dialog: Option<Dialog>,
    dialog_fresh: bool,
    toasts: Vec<Toast>,
    next_id: RepoId,
    focus_search: bool,
    window_title: String,
    /// Name of the repository being cloned, while a clone runs.
    cloning: Option<String>,
    activity: Vec<ActivityEntry>,
    activity_tab: ActivityTab,
    /// `GITVIS_SCRIPT` smoke-test driver, if set.
    script: Option<Script>,
}

impl GitVis {
    /// `open` are repositories given on the command line; they are opened in
    /// addition to the ones restored from the last session.
    pub fn new(cc: &eframe::CreationContext<'_>, open: Vec<PathBuf>) -> Self {
        let settings: Settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, eframe::APP_KEY))
            .unwrap_or_default();
        theme::install_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, settings.dark);
        let worker = Worker::new(cc.egui_ctx.clone());
        let mut app = Self {
            settings,
            repos: Vec::new(),
            active: 0,
            worker,
            dialog: None,
            dialog_fresh: false,
            toasts: Vec::new(),
            next_id: 1,
            focus_search: false,
            window_title: String::new(),
            cloning: None,
            activity: Vec::new(),
            activity_tab: ActivityTab::Log,
            script: Script::from_env(),
        };
        for path in app.settings.open_repos.clone() {
            app.open_repo(path, false);
        }
        app.active = app.settings.active.min(app.repos.len().saturating_sub(1));
        for path in open {
            app.open_repo(path, true);
        }
        app
    }

    fn toast(&mut self, kind: ToastKind, title: impl Into<String>, body: impl Into<String>) {
        self.toasts.push(Toast {
            kind,
            title: title.into(),
            body: body.into(),
            created: Instant::now(),
        });
        if self.toasts.len() > 6 {
            self.toasts.remove(0);
        }
    }

    /// Record something gitvis did. Successful reads and API calls are only
    /// kept in verbose mode; operations and failures always are.
    #[allow(clippy::too_many_arguments)]
    fn log(
        &mut self,
        kind: ActivityKind,
        repo: String,
        cwd: String,
        label: String,
        commands: Vec<String>,
        result: &Result<String, String>,
        elapsed_ms: u64,
        bytes: Option<usize>,
    ) {
        let (ok, output) = match result {
            Ok(o) => (true, o.clone()),
            Err(e) => (false, e.clone()),
        };
        if ok && !kind.always_logged() && !self.settings.verbose {
            return;
        }
        self.activity.push(ActivityEntry {
            at: format::now_clock_ms(),
            kind,
            repo,
            label,
            commands,
            output,
            cwd,
            ok,
            elapsed_ms,
            bytes,
            expanded: !ok || (kind.always_logged() && self.settings.verbose),
        });
        if self.activity.len() > MAX_ACTIVITY {
            self.activity.remove(0);
        }
    }

    /// Log a batch of GitHub API calls as one entry.
    fn log_api(
        &mut self,
        i: usize,
        label: String,
        calls: &[ApiCall],
        summary: Result<String, String>,
    ) {
        let Some(r) = self.repos.get_mut(i) else {
            return;
        };
        if let Some(rate) = calls.iter().rev().find_map(|c| c.rate) {
            r.rate = Some(rate);
        }
        let (name, cwd) = (r.name.clone(), r.workdir.display().to_string());
        let commands = calls.iter().map(|c| c.command.clone()).collect();
        let elapsed: u64 = calls.iter().map(|c| c.elapsed_ms).sum();
        let bytes: usize = calls.iter().map(|c| c.bytes).sum();
        let mut detail = String::new();
        for c in calls {
            let endpoint = c
                .command
                .rsplit(' ')
                .next()
                .unwrap_or(&c.command)
                .to_owned();
            detail.push_str(&format!(
                "\n{} · {} · {}{}",
                endpoint,
                format::bytes(c.bytes),
                format::millis(c.elapsed_ms),
                match c.rate {
                    Some(rate) => format!(" · {} of {} requests left", rate.remaining, rate.limit),
                    None => String::new(),
                }
            ));
            if let Some(e) = &c.error {
                detail.push_str(&format!("\n  {e}"));
            }
        }
        let result = match summary {
            Ok(s) => Ok(format!("{s}{detail}")),
            Err(e) => Err(format!("{e}{detail}")),
        };
        self.log(
            ActivityKind::Api,
            name,
            cwd,
            label,
            commands,
            &result,
            elapsed,
            Some(bytes),
        );
    }

    fn repo_idx(&self, id: RepoId) -> Option<usize> {
        self.repos.iter().position(|r| r.id == id)
    }

    fn gh_repo(&self, i: usize) -> Option<GhRepo> {
        let r = self.repos.get(i)?;
        let (owner, name) = r.github_remote.clone()?;
        Some(GhRepo {
            repo: r.id,
            workdir: r.workdir.clone(),
            owner,
            name,
        })
    }

    fn prefs_key(path: &Path) -> String {
        path.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_owned()
    }

    fn open_repo(&mut self, path: PathBuf, activate: bool) {
        let (workdir, git_dir) = match repo::locate(&path) {
            Ok(x) => x,
            Err(e) => {
                self.toast(
                    ToastKind::Error,
                    format!("Not a git repository: {}", path.display()),
                    e.to_string(),
                );
                return;
            }
        };
        if let Some(i) = self.repos.iter().position(|r| r.workdir == workdir) {
            if activate {
                self.active = i;
            }
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        let mut state = RepoState::new(id, workdir.clone());
        if let Some(p) = self.settings.repo_prefs.get(&Self::prefs_key(&workdir)) {
            state.apply_prefs(p);
        }
        let (tx, ctx) = self.worker.event_sender();
        match crate::watcher::watch(id, &workdir, &git_dir, tx, ctx) {
            Ok(w) => state.watcher = Some(w),
            Err(e) => self.toast(ToastKind::Info, "Automatic refresh unavailable", e),
        }
        self.repos.push(state);
        let i = self.repos.len() - 1;
        if activate {
            self.active = i;
        }
        self.settings.recent.retain(|p| p != &workdir);
        self.settings.recent.insert(0, workdir);
        self.settings.recent.truncate(12);
        self.request_load(i, true);
    }

    fn close_repo(&mut self, i: usize) {
        if i >= self.repos.len() {
            return;
        }
        self.save_prefs(i);
        self.repos.remove(i);
        if self.active >= self.repos.len() {
            self.active = self.repos.len().saturating_sub(1);
        } else if i < self.active {
            self.active -= 1;
        }
    }

    fn save_prefs(&mut self, i: usize) {
        if let Some(r) = self.repos.get(i) {
            self.settings
                .repo_prefs
                .insert(Self::prefs_key(&r.workdir), r.prefs());
        }
    }

    fn request_load(&mut self, i: usize, full: bool) {
        let Some(r) = self.repos.get_mut(i) else {
            return;
        };
        if r.is_busy() {
            r.pending_reload = Some(r.pending_reload.unwrap_or(false) || full);
            return;
        }
        r.generation += 1;
        r.loading = true;
        let job = if full || r.snapshot.is_none() {
            Job::Load {
                repo: r.id,
                path: r.workdir.clone(),
                opts: LoadOptions {
                    max_commits: MAX_COMMITS,
                    filter: r.filter.clone(),
                },
                generation: r.generation,
            }
        } else {
            Job::Status {
                repo: r.id,
                path: r.workdir.clone(),
                generation: r.generation,
            }
        };
        self.worker.send(job);
    }

    fn request_reflog(&mut self, i: usize) {
        if let Some(r) = self.repos.get(i) {
            self.worker.send(Job::Reflog {
                repo: r.id,
                path: r.workdir.clone(),
            });
        }
    }

    /// Fetch PRs and workflow runs when the repository has a GitHub remote
    /// and the data is stale (or `force`).
    fn maybe_fetch_github(&mut self, i: usize, force: bool) {
        let Some(gh) = self.gh_repo(i) else {
            return;
        };
        let r = &mut self.repos[i];
        if r.github_loading {
            return;
        }
        if !force && r.github_at.is_some_and(|t| t.elapsed() < GITHUB_REFRESH) {
            return;
        }
        r.github_loading = true;
        self.worker.fetch_github(gh);
    }

    /// Fetch the check runs of a commit unless they are fresh.
    fn ensure_checks(&mut self, i: usize, sha: Oid, force: bool) {
        let Some(gh) = self.gh_repo(i) else {
            return;
        };
        let r = &mut self.repos[i];
        let entry = r.checks.entry(sha).or_insert(ChecksState {
            loading: false,
            at: None,
            result: None,
        });
        if entry.loading {
            return;
        }
        let stale = match (&entry.result, entry.at) {
            (None, _) => true,
            (Some(Ok(checks)), Some(at)) => {
                at.elapsed() > CHECKS_REFRESH && checks.iter().any(|c| c.outcome().is_running())
            }
            (Some(Err(_)), Some(at)) => at.elapsed() > CHECKS_REFRESH,
            _ => true,
        };
        if !force && !stale {
            return;
        }
        entry.loading = true;
        self.worker.fetch_checks(gh, sha);
    }

    fn request_contains(&mut self, i: usize, id: Oid) {
        let Some(r) = self.repos.get_mut(i) else {
            return;
        };
        if r.contains.contains_key(&id) {
            return;
        }
        r.contains.insert(id, None);
        self.worker.send(Job::Contains {
            repo: r.id,
            path: r.workdir.clone(),
            id,
        });
    }

    /// Show a run on the GitHub page, loading its jobs if needed.
    fn open_run(&mut self, i: usize, run: u64, job: Option<u64>) {
        let Some(gh) = self.gh_repo(i) else {
            return;
        };
        let r = &mut self.repos[i];
        r.page = Page::GitHub;
        r.gh_page.tab = GitHubTab::Actions;
        r.gh_page.selected_run = Some(run);
        let known = r.github.as_ref().is_some_and(|d| d.run(run).is_some());
        if !known {
            r.gh_page.pending_run = Some(run);
        }
        let detail = r.gh_page.runs.entry(run).or_default();
        if detail.jobs.is_none() && !detail.jobs_loading {
            detail.jobs_loading = true;
            self.worker.fetch_jobs(gh, run, !known);
        }
        if let Some(job) = job {
            self.show_log(i, run, job, None);
        }
    }

    fn refresh_run(&mut self, i: usize, run: u64) {
        let Some(gh) = self.gh_repo(i) else {
            return;
        };
        let r = &mut self.repos[i];
        let known = r.github.as_ref().is_some_and(|d| d.run(run).is_some());
        let detail = r.gh_page.runs.entry(run).or_default();
        if detail.jobs_loading {
            return;
        }
        detail.jobs_loading = true;
        detail.annotations.clear();
        self.worker.fetch_jobs(gh, run, !known);
    }

    /// Select a job's log in the viewer, fetching it the first time.
    fn show_log(&mut self, i: usize, run: u64, job: u64, step: Option<u32>) {
        let Some(gh) = self.gh_repo(i) else {
            return;
        };
        let r = &mut self.repos[i];
        let detail = r.gh_page.runs.entry(run).or_default();
        detail.expanded.insert(job);
        detail.selected_job = Some(job);
        r.gh_page.scroll_to_step = step.map(|s| (job, s));
        let fetch = match detail.logs.get(&job) {
            None => true,
            Some(s) => !s.loading && matches!(s.doc, Some(Err(_))),
        };
        if fetch {
            detail.logs.insert(
                job,
                LogState {
                    loading: true,
                    doc: None,
                },
            );
            self.worker.fetch_job_log(gh, run, job);
        } else if let Some(s) = step {
            let steps = detail_steps(detail, job);
            if let Some(LogState {
                doc: Some(Ok(doc)), ..
            }) = detail.logs.get_mut(&job)
            {
                scroll_log_to_step(doc, steps.as_deref(), s);
            }
        }
    }

    fn load_annotations(&mut self, i: usize, run: u64, job: u64) {
        let Some(gh) = self.gh_repo(i) else {
            return;
        };
        let detail = self.repos[i].gh_page.runs.entry(run).or_default();
        if detail.annotations.contains_key(&job) || detail.annotations_loading.contains(&job) {
            return;
        }
        detail.annotations_loading.insert(job);
        self.worker.fetch_annotations(gh, run, job);
    }

    /// Show a pull request on the GitHub page, loading its numbers and the
    /// checks of its head commit.
    fn open_pr(&mut self, i: usize, number: u64) {
        let Some(gh) = self.gh_repo(i) else {
            return;
        };
        let r = &mut self.repos[i];
        r.page = Page::GitHub;
        r.gh_page.tab = GitHubTab::PullRequests;
        r.gh_page.selected_pr = Some(number);
        let head = r
            .github
            .as_ref()
            .and_then(|d| d.pr(number))
            .and_then(|p| Oid::from_str(&p.head_sha).ok());
        let state = r.gh_page.prs.entry(number).or_insert(PrState {
            loading: false,
            detail: None,
        });
        if state.detail.is_none() && !state.loading {
            state.loading = true;
            self.worker.fetch_pr(gh, number);
        }
        if let Some(sha) = head {
            self.ensure_checks(i, sha, false);
        }
    }

    /// Re-scan the staged, unstaged and untracked changes for secrets. A
    /// clean tree needs no git call.
    fn request_quick_scan(&mut self, i: usize) {
        let Some(r) = self.repos.get_mut(i) else {
            return;
        };
        let Some(snap) = r.snapshot.as_ref() else {
            return;
        };
        r.sec_page.quick_gen += 1;
        if snap.status.is_clean() {
            r.sec_page.quick_loading = false;
            r.sec_page.quick = Some(Report::default());
            r.sec_page.quick_at = Some(Instant::now());
            r.sec_page.rebuild();
            return;
        }
        r.sec_page.quick_loading = true;
        self.worker
            .scan_changes(r.id, r.workdir.clone(), r.sec_page.quick_gen);
    }

    /// Start a full scan: every file on disk, plus history when asked.
    fn start_scan(&mut self, i: usize, history: bool, automatic: bool) {
        let Some(r) = self.repos.get_mut(i) else {
            return;
        };
        if r.sec_page.scanning() {
            if !automatic {
                self.toast(
                    ToastKind::Info,
                    "A scan is already running",
                    "Cancel it first, or wait for it to finish.",
                );
            }
            return;
        }
        let opts = ScanOptions {
            worktree: true,
            history: history.then_some(HistoryScope {
                max_commits: r.sec_page.history_depth,
                all_refs: r.sec_page.all_refs,
            }),
        };
        let cancel = Arc::new(AtomicBool::new(false));
        r.sec_page.scan_gen += 1;
        r.sec_page.error = None;
        r.sec_page.scan = Some(ScanRun {
            started: Instant::now(),
            progress: Default::default(),
            cancel: cancel.clone(),
            history,
            automatic,
        });
        self.worker
            .scan_secrets(r.id, r.workdir.clone(), opts, cancel, r.sec_page.scan_gen);
    }

    fn cancel_scan(&mut self, i: usize) {
        if let Some(scan) = self.repos.get(i).and_then(|r| r.sec_page.scan.as_ref()) {
            scan.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Run the OSINT analysis (local git and file reads only).
    fn start_osint(&mut self, i: usize) {
        let Some(r) = self.repos.get_mut(i) else {
            return;
        };
        if r.snapshot.is_none() || r.sec_page.osint.running() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let o = &mut r.sec_page.osint;
        o.generation += 1;
        o.error = None;
        o.run = Some(ScanRun::new(cancel.clone(), true));
        self.worker.run_osint(
            r.id,
            r.workdir.clone(),
            OsintOptions::default(),
            cancel,
            r.sec_page.osint.generation,
        );
    }

    fn start_unreachable(&mut self, i: usize) {
        let Some(r) = self.repos.get_mut(i) else {
            return;
        };
        if r.sec_page.osint.unreachable_run.is_some() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        r.sec_page.osint.unreachable_run = Some(ScanRun::new(cancel.clone(), false));
        self.worker.run_unreachable(r.id, r.workdir.clone(), cancel);
    }

    fn start_osv(&mut self, i: usize) {
        let Some(r) = self.repos.get_mut(i) else {
            return;
        };
        let o = &mut r.sec_page.osint;
        if o.osv_run.is_some() {
            return;
        }
        let packages = o
            .report
            .as_ref()
            .map(|rep| rep.supply.packages.clone())
            .unwrap_or_default();
        if packages.is_empty() {
            self.toast(
                ToastKind::Info,
                "No packages to check",
                "No lockfile or manifest with pinned versions was found.",
            );
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        o.osv_run = Some(ScanRun::new(cancel.clone(), false));
        self.worker.run_osv(r.id, packages, cancel);
    }

    /// Forks, alerts and admin surface from GitHub, once per page visit.
    fn fetch_gh_surface(&mut self, i: usize, force: bool) {
        let Some(gh) = self.gh_repo(i) else {
            return;
        };
        let r = &mut self.repos[i];
        let o = &mut r.sec_page.osint;
        if o.gh_loading {
            return;
        }
        if !force && o.gh_at.is_some_and(|t| t.elapsed() < GITHUB_REFRESH) {
            return;
        }
        o.gh_loading = true;
        let default_branch = r
            .sec_page
            .info
            .as_ref()
            .and_then(|i| i.as_ref().ok())
            .map(|i| i.default_branch.clone())
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| "main".to_owned());
        self.worker.fetch_gh_surface(gh, default_branch);
    }

    fn start_dorks(&mut self, i: usize) {
        let Some(gh) = self.gh_repo(i) else {
            return;
        };
        let r = &mut self.repos[i];
        if r.sec_page.osint.dorks_running {
            return;
        }
        let is_org = r
            .sec_page
            .info
            .as_ref()
            .and_then(|i| i.as_ref().ok())
            .map(|i| i.owner_is_org())
            .unwrap_or(true);
        r.sec_page.osint.dorks_running = true;
        self.worker.run_dorks(gh, is_org);
    }

    fn check_served(&mut self, i: usize, shas: Vec<Oid>) {
        let Some(gh) = self.gh_repo(i) else {
            return;
        };
        let o = &mut self.repos[i].sec_page.osint;
        let shas: Vec<Oid> = shas
            .into_iter()
            .filter(|s| !o.served.contains_key(s))
            .take(30)
            .collect();
        if shas.is_empty() {
            return;
        }
        for s in &shas {
            o.served.insert(*s, None);
        }
        self.worker.check_served(gh, shas);
    }

    fn check_pr_refs(&mut self, i: usize) {
        let Some(r) = self.repos.get_mut(i) else {
            return;
        };
        if r.sec_page.osint.pr_refs_running {
            return;
        }
        let remote = r
            .snapshot
            .as_ref()
            .and_then(|s| {
                s.remotes
                    .iter()
                    .find(|re| re.name == "origin")
                    .or(s.remotes.first())
            })
            .map(|re| re.name.clone());
        let Some(remote) = remote else {
            self.toast(ToastKind::Info, "No remote", "Add a remote first.");
            return;
        };
        r.sec_page.osint.pr_refs_running = true;
        self.worker.check_pr_refs(r.id, r.workdir.clone(), remote);
    }

    /// Run the insights analysis (local git reads only).
    fn start_insights(&mut self, i: usize) {
        let Some(r) = self.repos.get_mut(i) else {
            return;
        };
        if r.snapshot.is_none() || r.ins_page.running() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let p = &mut r.ins_page;
        p.generation += 1;
        p.error = None;
        p.run = Some(ScanRun::new(cancel.clone(), true));
        let opts = InsightsOptions {
            max_commits: p.depth,
            all_refs: p.all_refs,
            ..InsightsOptions::default()
        };
        self.worker
            .run_insights(r.id, r.workdir.clone(), opts, cancel, r.ins_page.generation);
    }

    fn start_weight(&mut self, i: usize) {
        let Some(r) = self.repos.get_mut(i) else {
            return;
        };
        if r.ins_page.weight_run.is_some() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        r.ins_page.weight_run = Some(ScanRun::new(cancel.clone(), false));
        self.worker.run_weight(r.id, r.workdir.clone(), cancel);
    }

    fn start_dating(&mut self, i: usize) {
        let Some(r) = self.repos.get_mut(i) else {
            return;
        };
        if r.ins_page.dating_run.is_some() {
            return;
        }
        let markers = r
            .ins_page
            .report
            .as_ref()
            .map(|rep| rep.debt.markers.clone())
            .unwrap_or_default();
        if markers.is_empty() {
            self.toast(
                ToastKind::Info,
                "No markers to date",
                "The scan found no TODO/FIXME style markers.",
            );
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        r.ins_page.dating_run = Some(ScanRun::new(cancel.clone(), false));
        self.worker
            .run_dating(r.id, r.workdir.clone(), markers, 400, cancel);
    }

    fn start_blame(&mut self, i: usize, path: String) {
        let Some(r) = self.repos.get_mut(i) else {
            return;
        };
        if r.ins_page.blame.contains_key(&path) {
            return;
        }
        r.ins_page.blame.insert(path.clone(), None);
        self.worker.run_blame(r.id, r.workdir.clone(), path);
    }

    fn start_search(&mut self, i: usize) {
        let Some(r) = self.repos.get_mut(i) else {
            return;
        };
        let p = &mut r.ins_page;
        if p.search_run.is_some() {
            return;
        }
        let text = p.search_text.trim().to_owned();
        if text.is_empty() {
            self.toast(ToastKind::Info, "Nothing to search", "Type a text first.");
            return;
        }
        let query = SearchQuery {
            text,
            mode: p.search_mode,
            all_refs: p.search_all,
            path: Some(p.search_path.trim().to_owned()).filter(|s| !s.is_empty()),
            ignore_case: p.search_ignore_case,
            max: 500,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        p.search_run = Some(ScanRun::new(cancel.clone(), false));
        p.selected = None;
        self.worker
            .run_search(r.id, r.workdir.clone(), query, cancel);
    }

    fn start_lost(&mut self, i: usize) {
        let Some(r) = self.repos.get_mut(i) else {
            return;
        };
        if r.ins_page.lost_run.is_some() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        r.ins_page.lost_run = Some(ScanRun::new(cancel.clone(), false));
        self.worker.run_lost(r.id, r.workdir.clone(), cancel);
    }

    fn set_insights_tab(&mut self, i: usize, tab: InsightsTab) {
        let p = &mut self.repos[i].ins_page;
        p.tab = tab;
        p.selected = None;
        if tab.needs_report() && p.report.is_none() {
            self.start_insights(i);
        }
        if tab == InsightsTab::Recovery && self.repos[i].ins_page.lost.is_none() {
            self.start_lost(i);
        }
    }

    fn set_security_tab(&mut self, i: usize, tab: SecurityTab) {
        self.repos[i].sec_page.tab = tab;
        self.repos[i].sec_page.osint.selected = None;
        if tab.needs_osint() && self.repos[i].sec_page.osint.report.is_none() {
            self.start_osint(i);
        }
        if tab == SecurityTab::GitHub {
            self.fetch_exposure(i, false);
            self.fetch_gh_surface(i, false);
        }
    }

    /// Repository visibility and secret-scanning alerts, once per page visit
    /// (or on demand).
    fn fetch_exposure(&mut self, i: usize, force: bool) {
        let Some(gh) = self.gh_repo(i) else {
            return;
        };
        let r = &mut self.repos[i];
        if r.sec_page.gh_loading {
            return;
        }
        if !force
            && r.sec_page
                .gh_at
                .is_some_and(|t| t.elapsed() < GITHUB_REFRESH)
        {
            return;
        }
        r.sec_page.gh_loading = true;
        self.worker.fetch_exposure(gh);
    }

    /// Append a pattern to `.gitignore` (creating it) and refresh.
    fn add_to_gitignore(&mut self, i: usize, pattern: String) {
        let Some(r) = self.repos.get_mut(i) else {
            return;
        };
        let path = r.workdir.join(".gitignore");
        let (name, cwd) = (r.name.clone(), r.workdir.display().to_string());
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if existing.lines().any(|l| l.trim() == pattern) {
            self.toast(
                ToastKind::Info,
                "Already ignored",
                format!("{pattern} is already in .gitignore"),
            );
            return;
        }
        let mut text = existing;
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&pattern);
        text.push('\n');
        let started = Instant::now();
        let result = std::fs::write(&path, text)
            .map(|_| format!("Appended `{pattern}` to .gitignore"))
            .map_err(|e| e.to_string());
        self.log(
            ActivityKind::Op,
            name,
            cwd,
            format!("Ignore {pattern}"),
            vec![format!("echo \"{pattern}\" >> .gitignore")],
            &result,
            started.elapsed().as_millis() as u64,
            None,
        );
        match result {
            Ok(msg) => {
                self.toast(ToastKind::Success, "Added to .gitignore", msg);
                let r = &mut self.repos[i];
                // Untracked files matching the exact path are no longer at risk.
                if let Some(full) = r.sec_page.full.as_mut() {
                    full.files
                        .retain(|f| !(f.untracked && !f.tracked && f.path == pattern));
                    r.sec_page.rebuild();
                }
                r.sec_page.stale = true;
                self.request_load(i, false);
            }
            Err(e) => self.toast(ToastKind::Error, "Could not write .gitignore", e),
        }
    }

    fn select(&mut self, i: usize, sel: Selection) {
        let Some(r) = self.repos.get_mut(i) else {
            return;
        };
        if r.selection == sel {
            return;
        }
        r.selection = sel;
        r.file = None;
        r.diff = None;
        r.diff_selection = None;
        r.checks_wanted = None;
        if let Selection::Commit(id) = sel {
            self.ensure_details(i, id);
            // Checks cost an API request each: wait for the selection to settle.
            let r = &mut self.repos[i];
            if r.github_remote.is_some() && !r.checks.contains_key(&id) {
                r.checks_wanted = Some((id, Instant::now()));
            } else {
                self.ensure_checks(i, id, false);
            }
        }
    }

    /// Fetch the checks of the selected commit once it has stayed selected
    /// for a moment (keyboard navigation would otherwise fire a request per
    /// row).
    fn tick_checks(&mut self, ctx: &egui::Context) {
        for i in 0..self.repos.len() {
            let Some((id, since)) = self.repos[i].checks_wanted else {
                continue;
            };
            if since.elapsed() >= CHECKS_DEBOUNCE {
                self.repos[i].checks_wanted = None;
                if self.repos[i].selection == Selection::Commit(id) {
                    self.ensure_checks(i, id, false);
                }
            } else {
                ctx.request_repaint_after(CHECKS_DEBOUNCE);
            }
        }
    }

    fn ensure_details(&mut self, i: usize, id: Oid) {
        let Some(r) = self.repos.get_mut(i) else {
            return;
        };
        if r.details.contains_key(&id) || r.details_loading == Some(id) {
            return;
        }
        r.details_loading = Some(id);
        self.worker.send(Job::Details {
            repo: r.id,
            path: r.workdir.clone(),
            id,
        });
    }

    fn diff_options(&self) -> DiffOptions {
        DiffOptions {
            ignore_whitespace: self.settings.diff_ignore_ws,
            context: self.settings.diff_context,
        }
    }

    fn open_file(&mut self, i: usize, target: DiffTarget) {
        let opts = self.diff_options();
        let dark = self.settings.dark;
        let Some(r) = self.repos.get_mut(i) else {
            return;
        };
        if r.file.as_ref() != Some(&target) {
            r.diff_selection = None;
        }
        r.file = Some(target.clone());
        r.diff = None;
        r.diff_gen += 1;
        r.page = Page::Graph;
        self.worker.send(Job::Diff {
            repo: r.id,
            path: r.workdir.clone(),
            target,
            opts,
            dark,
            generation: r.diff_gen,
        });
    }

    /// Re-request the open diff (options or theme changed).
    fn reload_diff(&mut self, i: usize) {
        if let Some(t) = self.repos.get(i).and_then(|r| r.file.clone()) {
            self.open_file(i, t);
        }
    }

    fn jump_to(&mut self, i: usize, oid: Oid) {
        let Some(r) = self.repos.get(i) else { return };
        let Some(snap) = r.snapshot.as_ref() else {
            return;
        };
        match snap.index_of.get(&oid) {
            Some(&idx) => {
                let row = snap.row_of_commit(idx);
                self.repos[i].page = Page::Graph;
                self.jump_row(i, row);
            }
            None => self.toast(
                ToastKind::Info,
                "Commit is not in the graph",
                "The branch may be hidden or the history truncated.",
            ),
        }
    }

    fn jump_row(&mut self, i: usize, row: usize) {
        let sel = self.repos[i].selection_for_row(row);
        self.select(i, sel);
        let r = &mut self.repos[i];
        if !r.visible_rows.contains(&row)
            || r.visible_rows.start + 1 >= row
            || row + 2 >= r.visible_rows.end
        {
            r.scroll_to_row = Some(row);
        }
    }

    fn run_ops(&mut self, i: usize, mut ops: Vec<GitOp>) {
        if ops.is_empty() {
            return;
        }
        let Some(r) = self.repos.get_mut(i) else {
            return;
        };
        if r.is_busy() {
            let label = r.busy.clone().unwrap_or_default();
            self.toast(
                ToastKind::Info,
                "Please wait",
                format!("{label} is still running"),
            );
            return;
        }
        let first = ops.remove(0);
        r.queued = ops;
        self.start_op(i, first);
    }

    fn start_op(&mut self, i: usize, op: GitOp) {
        let r = &mut self.repos[i];
        r.busy = Some(op.label());
        self.worker.run_op(r.id, r.workdir.clone(), op);
    }

    fn after_snapshot(&mut self, i: usize, first: bool) {
        let mut reload_diff: Option<DiffTarget> = None;
        let mut want_details: Option<Oid> = None;
        let mut github_changed = false;
        {
            let r = &mut self.repos[i];
            let Some(snap) = r.snapshot.as_ref() else {
                return;
            };
            let valid = match r.selection {
                Selection::None => false,
                Selection::Wip => snap.wip_row,
                Selection::Commit(id) => snap.index_of.contains_key(&id),
            };
            if !valid {
                r.selection = default_selection(snap);
                r.file = None;
                r.diff = None;
                r.diff_selection = None;
                if first {
                    r.scroll_to_row = Some(0);
                }
            }
            match &r.file {
                Some(t) if !file_exists(snap, t) => {
                    r.file = None;
                    r.diff = None;
                    r.diff_selection = None;
                }
                Some(t) if !matches!(t, DiffTarget::Commit { .. }) => reload_diff = Some(t.clone()),
                _ => {}
            }
            // Prefer origin, then any remote on github.com.
            let remote = snap
                .remotes
                .iter()
                .filter(|re| re.name == "origin")
                .chain(snap.remotes.iter())
                .find_map(|re| github::parse_remote(&re.url));
            r.recompute_search();
            if let Selection::Commit(id) = r.selection {
                want_details = Some(id);
            }
            if remote != r.github_remote {
                r.github_remote = remote;
                r.github = None;
                r.github_error = None;
                r.github_at = None;
                r.ci.clear();
                r.checks.clear();
                r.gh_page = Default::default();
                if r.github_remote.is_none() {
                    r.page = Page::Graph;
                }
                github_changed = true;
            }
        }
        if let Some(t) = reload_diff {
            self.open_file(i, t);
        }
        if let Some(id) = want_details {
            self.ensure_details(i, id);
            self.ensure_checks(i, id, false);
        }
        if github_changed {
            self.maybe_fetch_github(i, true);
        }
        self.request_quick_scan(i);
        if first && self.settings.scan_on_open {
            self.start_scan(i, false, true);
        }
        // The Insights page may have been opened before the first snapshot
        // arrived (restored session, or a script): start what it is waiting for.
        if self.repos[i].page == Page::Insights {
            let p = &self.repos[i].ins_page;
            let want_report = p.tab.needs_report() && p.report.is_none() && !p.running();
            let want_lost =
                p.tab == InsightsTab::Recovery && p.lost.is_none() && p.lost_run.is_none();
            if want_report {
                self.start_insights(i);
            }
            if want_lost {
                self.start_lost(i);
            }
        }
    }

    /// Rebuild the per-commit CI map from the run list (fetched check runs
    /// are more complete and win where present).
    fn apply_github(&mut self, i: usize, data: github::GitHubData) {
        let r = &mut self.repos[i];
        r.ci.clear();
        let mut by_sha: std::collections::HashMap<Oid, Vec<&github::WorkflowRun>> =
            std::collections::HashMap::new();
        for run in &data.runs {
            if let Ok(oid) = Oid::from_str(&run.sha) {
                by_sha.entry(oid).or_default().push(run);
            }
        }
        for (oid, runs) in by_sha {
            let outcome = aggregate(runs.iter().map(|r| r.outcome()));
            let open = runs
                .iter()
                .find(|r| r.outcome().is_bad())
                .or(runs.first())
                .map(|r| r.id);
            r.ci.insert(
                oid,
                CiInfo {
                    outcome,
                    run: open,
                    summary: ci_summary("run", runs.iter().map(|r| r.outcome())),
                },
            );
        }
        for (sha, state) in &r.checks {
            if let Some(Ok(checks)) = &state.result {
                r.ci.insert(*sha, ci_from_checks(checks));
            }
        }
        if let Some(rate) = data.rate {
            r.rate = Some(rate);
        }
        r.github = Some(data);
        r.github_error = None;
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Loaded {
                repo,
                generation,
                elapsed_ms,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                if generation != r.generation {
                    return;
                }
                r.loading = false;
                let (name, cwd) = (r.name.clone(), r.workdir.display().to_string());
                match result {
                    Ok(snap) => {
                        let first = r.snapshot.is_none();
                        r.load_error = None;
                        r.load_ms = Some(elapsed_ms);
                        r.loaded_at = Some(Instant::now());
                        let summary = load_summary(&snap, MAX_COMMITS);
                        r.snapshot = Some(*snap);
                        self.log(
                            ActivityKind::Read,
                            name,
                            cwd,
                            "Load repository".into(),
                            vec![
                                format!("libgit2: refs, stashes, revwalk (newest {} commits), graph layout", format::count(MAX_COMMITS)),
                                "git status --porcelain=v2 -z --untracked-files=all --no-renames".into(),
                            ],
                            &Ok(summary),
                            elapsed_ms,
                            None,
                        );
                        self.after_snapshot(i, first);
                        self.request_reflog(i);
                        self.maybe_fetch_github(i, false);
                    }
                    Err(e) => {
                        r.load_error = Some(e.clone());
                        self.log(
                            ActivityKind::Read,
                            name,
                            cwd,
                            "Load repository".into(),
                            Vec::new(),
                            &Err(e.clone()),
                            elapsed_ms,
                            None,
                        );
                        self.toast(ToastKind::Error, "Could not read repository", e);
                    }
                }
            }
            Event::Status {
                repo,
                generation,
                elapsed_ms,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                if generation != r.generation {
                    return;
                }
                r.loading = false;
                let (name, cwd) = (r.name.clone(), r.workdir.display().to_string());
                match result {
                    Ok((tree, pending)) => {
                        let Some(snap) = r.snapshot.as_mut() else {
                            self.request_load(i, true);
                            return;
                        };
                        if snap.wip_row != !tree.is_clean() {
                            self.request_load(i, true);
                            return;
                        }
                        let summary = format!(
                            "{} staged · {} unstaged · {} conflicted · pending operation: {}",
                            tree.staged.len(),
                            tree.unstaged.len(),
                            tree.conflicted.len(),
                            if pending == gitvis_core::Pending::None {
                                "none".to_owned()
                            } else {
                                pending.label().to_lowercase()
                            }
                        );
                        snap.status = tree;
                        snap.pending = pending;
                        self.log(
                            ActivityKind::Read,
                            name,
                            cwd,
                            "Refresh working tree".into(),
                            vec![
                                "git status --porcelain=v2 -z --untracked-files=all --no-renames"
                                    .into(),
                            ],
                            &Ok(summary),
                            elapsed_ms,
                            None,
                        );
                        self.after_snapshot(i, false);
                    }
                    Err(e) => {
                        self.log(
                            ActivityKind::Read,
                            name,
                            cwd,
                            "Refresh working tree".into(),
                            Vec::new(),
                            &Err(e.clone()),
                            elapsed_ms,
                            None,
                        );
                        self.toast(ToastKind::Error, "Could not read working tree", e)
                    }
                }
            }
            Event::Details {
                repo,
                id,
                elapsed_ms,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                if r.details_loading == Some(id) {
                    r.details_loading = None;
                }
                let (name, cwd) = (r.name.clone(), r.workdir.display().to_string());
                let short = repo::short_id(id);
                match result {
                    Ok(d) => {
                        if r.details.len() > 400 {
                            r.details.clear();
                        }
                        let (a, dl) = d
                            .files
                            .iter()
                            .fold((0, 0), |(a, b), f| (a + f.additions, b + f.deletions));
                        let summary = format!(
                            "{} · tree {} · {} parent(s) · {} · +{a} −{dl}{}{}",
                            d.message.lines().next().unwrap_or(""),
                            repo::short_id(d.tree),
                            d.parents.len(),
                            format::plural(d.files.len(), "file", "files"),
                            if d.stats_skipped {
                                " (line stats skipped: very large commit)"
                            } else {
                                ""
                            },
                            match &d.signature {
                                Some(s) => format!(" · {s} signature present"),
                                None => String::new(),
                            }
                        );
                        r.details.insert(id, d);
                        self.log(
                            ActivityKind::Read,
                            name,
                            cwd,
                            format!("Commit details {short}"),
                            vec![format!(
                                "libgit2: diff_tree_to_tree {short}^ {short} with rename detection"
                            )],
                            &Ok(summary),
                            elapsed_ms,
                            None,
                        );
                    }
                    Err(e) => {
                        self.log(
                            ActivityKind::Read,
                            name,
                            cwd,
                            format!("Commit details {short}"),
                            Vec::new(),
                            &Err(e.clone()),
                            elapsed_ms,
                            None,
                        );
                        self.toast(ToastKind::Error, "Could not load commit", e)
                    }
                }
            }
            Event::Diff {
                repo,
                generation,
                elapsed_ms,
                target,
                command,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                let (name, cwd) = (r.name.clone(), r.workdir.display().to_string());
                let summary = match &result {
                    Ok(d) => {
                        let (a, dl) = d.diff.stats();
                        let lines: usize = d.diff.hunks.iter().map(|h| h.lines.len()).sum();
                        Ok(format!(
                            "{} · {} · +{a} −{dl}{}{}{}",
                            format::plural(d.diff.hunks.len(), "hunk", "hunks"),
                            format::plural(lines, "line", "lines"),
                            match &d.language {
                                Some(l) => format!(" · {l}"),
                                None => String::new(),
                            },
                            if d.diff.binary { " · binary" } else { "" },
                            if d.diff.too_large {
                                " · too large to show"
                            } else {
                                ""
                            },
                        ))
                    }
                    Err(e) => Err(e.clone()),
                };
                self.log(
                    ActivityKind::Read,
                    name,
                    cwd,
                    format!("Diff {}", target.path()),
                    command
                        .into_iter()
                        .chain(
                            matches!(target, DiffTarget::Untracked { .. })
                                .then(|| format!("read {} from the working tree", target.path())),
                        )
                        .collect(),
                    &summary,
                    elapsed_ms,
                    None,
                );
                let r = &mut self.repos[i];
                if r.diff_gen == generation && r.file.as_ref() == Some(&target) {
                    r.diff = Some(result.map(|d| *d));
                    r.diff_selection = None;
                }
            }
            Event::Reflog { repo, entries } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                r.undo = reflog::undo_for(&entries);
                r.reflog = entries;
            }
            Event::Contains {
                repo,
                id,
                elapsed_ms,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                let (name, cwd) = (r.name.clone(), r.workdir.display().to_string());
                let summary = match &result {
                    Ok(b) => Ok(format!(
                        "{}: {}",
                        format::plural(b.len(), "branch", "branches"),
                        b.join(", ")
                    )),
                    Err(e) => Err(e.clone()),
                };
                r.contains.insert(id, Some(result.unwrap_or_default()));
                self.log(
                    ActivityKind::Read,
                    name,
                    cwd,
                    format!("Branches containing {}", repo::short_id(id)),
                    vec![format!(
                        "git branch -a --contains {id} --format=%(refname:short)"
                    )],
                    &summary,
                    elapsed_ms,
                    None,
                );
            }
            Event::GitHub {
                repo,
                calls,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                r.github_loading = false;
                r.github_at = Some(Instant::now());
                let summary = match &result {
                    Ok(d) => Ok(format!(
                        "{} open pull requests · {} recent workflow runs ({} failed, {} running) · {} workflows",
                        d.prs.len(),
                        d.runs.len(),
                        d.runs.iter().filter(|r| r.is_failure()).count(),
                        d.runs.iter().filter(|r| r.is_running()).count(),
                        d.workflows.len()
                    )),
                    Err(e) => Err(e.clone()),
                };
                match result {
                    Ok(data) => self.apply_github(i, *data),
                    Err(e) => r.github_error = Some(e),
                }
                self.log_api(i, "GitHub overview".into(), &calls, summary);
            }
            Event::Jobs {
                repo,
                run_id,
                run,
                calls,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                if let Some(run) = run
                    && let Some(d) = r.github.as_mut()
                {
                    d.upsert_run(run);
                }
                if r.gh_page.pending_run == Some(run_id) {
                    r.gh_page.pending_run = None;
                }
                let summary = match &result {
                    Ok(jobs) => Ok(format!(
                        "{} · {} passed · {} failed · {} running · {} steps in total",
                        format::plural(jobs.len(), "job", "jobs"),
                        jobs.iter().filter(|j| j.outcome().is_good()).count(),
                        jobs.iter().filter(|j| j.outcome().is_bad()).count(),
                        jobs.iter().filter(|j| j.outcome().is_running()).count(),
                        jobs.iter().map(|j| j.steps.len()).sum::<usize>()
                    )),
                    Err(e) => Err(e.clone()),
                };
                let mut auto_annotations = Vec::new();
                let detail = r.gh_page.runs.entry(run_id).or_default();
                detail.jobs_loading = false;
                detail.jobs_at = Some(Instant::now());
                if let Ok(jobs) = &result {
                    // Expand failing jobs so the user lands on the problem.
                    for j in jobs.iter().filter(|j| j.outcome().is_bad()) {
                        detail.expanded.insert(j.id);
                    }
                    if detail.selected_job.is_none()
                        && let Some(j) = jobs.iter().find(|j| j.outcome().is_bad())
                    {
                        auto_annotations.push(j.id);
                        detail.selected_job = Some(j.id);
                    }
                    auto_annotations.extend(
                        jobs.iter()
                            .filter(|j| j.outcome().is_bad())
                            .map(|j| j.id)
                            .take(AUTO_ANNOTATIONS),
                    );
                }
                detail.jobs = Some(result);
                let selected = detail.selected_job;
                let label = format!("Jobs of run {run_id}");
                self.log_api(i, label, &calls, summary);
                auto_annotations.sort_unstable();
                auto_annotations.dedup();
                for job in auto_annotations {
                    self.load_annotations(i, run_id, job);
                }
                if let Some(job) = selected
                    && !self.repos[i].gh_page.runs[&run_id].logs.contains_key(&job)
                {
                    self.show_log(i, run_id, job, None);
                }
            }
            Event::JobLog {
                repo,
                run_id,
                job_id,
                calls,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                let filter = r.gh_page.log_filter.clone();
                let scroll = r.gh_page.scroll_to_step.take();
                let summary = match &result {
                    Ok(log) => Ok(format!(
                        "{} · {} groups · {} errors · {} warnings · {}",
                        format::plural(log.lines.len(), "line", "lines"),
                        log.groups.len(),
                        log.errors.len(),
                        log.warnings.len(),
                        format::bytes(log.bytes)
                    )),
                    Err(e) => Err(e.clone()),
                };
                let detail = r.gh_page.runs.entry(run_id).or_default();
                let steps = detail_steps(detail, job_id);
                let doc = result.map(|log| {
                    let mut doc = LogDoc::new(*log);
                    doc.set_filter(&filter);
                    if let Some((job, step)) = scroll
                        && job == job_id
                    {
                        scroll_log_to_step(&mut doc, steps.as_deref(), step);
                    } else if let Some(&first) = doc.log.errors.first() {
                        doc.scroll_to = Some(first);
                    }
                    doc
                });
                detail.logs.insert(
                    job_id,
                    LogState {
                        loading: false,
                        doc: Some(doc),
                    },
                );
                self.log_api(i, format!("Log of job {job_id}"), &calls, summary);
            }
            Event::Annotations {
                repo,
                run_id,
                job_id,
                calls,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                let summary = match &result {
                    Ok(a) => Ok(format!(
                        "{} · {} failures · {} warnings · {} notices",
                        format::plural(a.len(), "annotation", "annotations"),
                        a.iter().filter(|a| a.level == "failure").count(),
                        a.iter().filter(|a| a.level == "warning").count(),
                        a.iter().filter(|a| a.level == "notice").count()
                    )),
                    Err(e) => Err(e.clone()),
                };
                let detail = r.gh_page.runs.entry(run_id).or_default();
                detail.annotations_loading.remove(&job_id);
                detail.annotations.insert(job_id, result);
                self.log_api(i, format!("Annotations of job {job_id}"), &calls, summary);
            }
            Event::Checks {
                repo,
                sha,
                calls,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                let summary = match &result {
                    Ok(c) => Ok(format!(
                        "{} · {}",
                        format::plural(c.len(), "check", "checks"),
                        ci_summary("check", c.iter().map(|c| c.outcome()))
                    )),
                    Err(e) => Err(e.clone()),
                };
                if let Ok(checks) = &result
                    && !checks.is_empty()
                {
                    r.ci.insert(sha, ci_from_checks(checks));
                }
                r.checks.insert(
                    sha,
                    ChecksState {
                        loading: false,
                        at: Some(Instant::now()),
                        result: Some(result),
                    },
                );
                self.log_api(
                    i,
                    format!("Checks of {}", repo::short_id(sha)),
                    &calls,
                    summary,
                );
            }
            Event::PrDetail {
                repo,
                number,
                calls,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                let summary = match &result {
                    Ok(d) => Ok(format!(
                        "+{} −{} in {} files · {} commits · {} comments · {} review comments · {} reviews · mergeable: {}",
                        d.additions,
                        d.deletions,
                        d.changed_files,
                        d.commits,
                        d.comments,
                        d.review_comments,
                        d.reviews.len(),
                        if d.mergeable_state.is_empty() {
                            "unknown"
                        } else {
                            &d.mergeable_state
                        }
                    )),
                    Err(e) => Err(e.clone()),
                };
                r.gh_page.prs.insert(
                    number,
                    PrState {
                        loading: false,
                        detail: Some(result),
                    },
                );
                self.log_api(i, format!("Pull request #{number}"), &calls, summary);
            }
            Event::ScanProgress {
                repo,
                generation,
                progress,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                if r.sec_page.scan_gen == generation
                    && let Some(scan) = r.sec_page.scan.as_mut()
                {
                    scan.progress = progress;
                }
            }
            Event::ScanDone {
                repo,
                generation,
                elapsed_ms,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                if r.sec_page.scan_gen != generation {
                    return;
                }
                let history = r.sec_page.scan.as_ref().is_some_and(|s| s.history);
                let automatic = r.sec_page.scan.as_ref().is_some_and(|s| s.automatic);
                r.sec_page.scan = None;
                let (name, cwd) = (r.name.clone(), r.workdir.display().to_string());
                match result {
                    Ok(report) => {
                        let summary = scan_summary(&report);
                        let commands = report.commands.clone();
                        let urgent =
                            report.count(Severity::Critical) + report.count(Severity::High);
                        let total = report.total();
                        let cancelled = report.stats.cancelled;
                        let dismissed = r.dismissed.clone();
                        let headline: Vec<String> = report
                            .findings
                            .iter()
                            .filter(|f| {
                                f.severity >= Severity::High && !dismissed.contains(&f.fingerprint)
                            })
                            .take(3)
                            .map(|f| {
                                format!(
                                    "{} in {}",
                                    f.name,
                                    f.paths().first().copied().unwrap_or("?")
                                )
                            })
                            .collect();
                        r.sec_page.full = Some(*report);
                        r.sec_page.report_at = Some(Instant::now());
                        r.sec_page.stale = false;
                        r.sec_page.rebuild();
                        let label = if history {
                            "Scan for secrets (working tree and history)"
                        } else {
                            "Scan for secrets (working tree)"
                        };
                        self.log(
                            ActivityKind::Scan,
                            name,
                            cwd,
                            label.into(),
                            commands,
                            &Ok(summary),
                            elapsed_ms,
                            None,
                        );
                        if automatic {
                            // Scan-on-open: the Exposure button and the status
                            // bar carry the result; a toast per opened
                            // repository would pile up over whatever the user
                            // is looking at.
                        } else if cancelled {
                            self.toast(
                                ToastKind::Info,
                                "Scan cancelled",
                                format!("{total} finding(s) so far are shown."),
                            );
                        } else if urgent > 0 {
                            self.toast(
                                ToastKind::Error,
                                format!(
                                    "{} found",
                                    format::plural(urgent, "exposed secret", "exposed secrets")
                                ),
                                headline.join("\n"),
                            );
                        } else if total > 0 {
                            self.toast(
                                ToastKind::Info,
                                format!(
                                    "{} to review",
                                    format::plural(total, "finding", "findings")
                                ),
                                "Nothing critical or high; see the Exposure page.",
                            );
                        } else if self.repos[i].page == Page::Security {
                            self.toast(ToastKind::Success, "No secrets found", "");
                        }
                    }
                    Err(e) => {
                        r.sec_page.error = Some(e.clone());
                        self.log(
                            ActivityKind::Scan,
                            name,
                            cwd,
                            "Scan for secrets".into(),
                            Vec::new(),
                            &Err(e.clone()),
                            elapsed_ms,
                            None,
                        );
                        self.toast(ToastKind::Error, "Secrets scan failed", e);
                    }
                }
            }
            Event::QuickScan {
                repo,
                generation,
                elapsed_ms,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                if r.sec_page.quick_gen != generation {
                    return;
                }
                r.sec_page.quick_loading = false;
                let (name, cwd) = (r.name.clone(), r.workdir.display().to_string());
                match result {
                    Ok(report) => {
                        let before = r.sec_page.staged_count();
                        let summary = format!(
                            "{} · {} sensitive file(s) · staged: {} · {} lines of diff · {} untracked files read",
                            format::plural(report.findings.len(), "secret", "secrets"),
                            report.files.len(),
                            report.staged().count() + report.staged_files().count(),
                            format::count(report.stats.lines as usize),
                            report.stats.files_scanned
                        );
                        let commands = report.commands.clone();
                        r.sec_page.quick = Some(*report);
                        r.sec_page.quick_at = Some(Instant::now());
                        r.sec_page.rebuild();
                        let after = r.sec_page.staged_count();
                        let first_staged = r.sec_page.report.as_ref().and_then(|rep| {
                            rep.staged()
                                .map(|f| {
                                    format!(
                                        "{} in {}",
                                        f.name,
                                        f.paths().first().copied().unwrap_or("?")
                                    )
                                })
                                .next()
                                .or_else(|| {
                                    rep.staged_files()
                                        .map(|f| format!("{} ({})", f.rule.name, f.path))
                                        .next()
                                })
                        });
                        self.log(
                            ActivityKind::Read,
                            name,
                            cwd,
                            "Scan staged and unstaged changes for secrets".into(),
                            commands,
                            &Ok(summary),
                            elapsed_ms,
                            None,
                        );
                        if after > before
                            && let Some(what) = first_staged
                        {
                            self.toast(
                                ToastKind::Error,
                                "Possible secret in the staged changes",
                                format!("{what}\nThe commit button will ask before committing it."),
                            );
                        }
                    }
                    Err(e) => self.log(
                        ActivityKind::Read,
                        name,
                        cwd,
                        "Scan staged and unstaged changes for secrets".into(),
                        Vec::new(),
                        &Err(e),
                        elapsed_ms,
                        None,
                    ),
                }
            }
            Event::Exposure {
                repo,
                calls,
                info,
                alerts,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                r.sec_page.gh_loading = false;
                r.sec_page.gh_at = Some(Instant::now());
                let summary = match (&info, &alerts) {
                    (Ok(info), alerts) => Ok(format!(
                        "{} repository · {} forks · secret scanning: {} · push protection: {} · alerts: {}",
                        info.visibility,
                        info.forks,
                        info.secret_scanning
                            .as_deref()
                            .unwrap_or("not visible to this account"),
                        info.push_protection
                            .as_deref()
                            .unwrap_or("not visible to this account"),
                        match alerts {
                            Ok(a) => format!("{} open", a.len()),
                            Err(e) => format!("unavailable ({})", e.lines().next().unwrap_or("")),
                        }
                    )),
                    (Err(e), _) => Err(e.clone()),
                };
                r.sec_page.info = Some(info);
                r.sec_page.alerts = Some(alerts);
                self.log_api(
                    i,
                    "Repository visibility and secret alerts".into(),
                    &calls,
                    summary,
                );
            }
            Event::OsintProgress {
                repo,
                generation,
                progress,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let o = &mut self.repos[i].sec_page.osint;
                if o.generation == generation
                    && let Some(run) = o.run.as_mut()
                {
                    run.progress = progress;
                }
            }
            Event::OsintDone {
                repo,
                generation,
                elapsed_ms,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                if r.sec_page.osint.generation != generation {
                    return;
                }
                r.sec_page.osint.run = None;
                let (name, cwd) = (r.name.clone(), r.workdir.display().to_string());
                match result {
                    Ok(report) => {
                        let summary = osint_summary(&report);
                        let commands = report.commands.clone();
                        r.sec_page.osint.report = Some(*report);
                        r.sec_page.osint.report_at = Some(Instant::now());
                        self.log(
                            ActivityKind::Scan,
                            name,
                            cwd,
                            "OSINT analysis (people, footprint, metadata, history, supply chain)"
                                .into(),
                            commands,
                            &Ok(summary),
                            elapsed_ms,
                            None,
                        );
                    }
                    Err(e) => {
                        r.sec_page.osint.error = Some(e.clone());
                        self.log(
                            ActivityKind::Scan,
                            name,
                            cwd,
                            "OSINT analysis".into(),
                            Vec::new(),
                            &Err(e.clone()),
                            elapsed_ms,
                            None,
                        );
                        self.toast(ToastKind::Error, "Analysis failed", e);
                    }
                }
            }
            Event::UnreachableProgress { repo, progress } => {
                let Some(i) = self.repo_idx(repo) else { return };
                if let Some(run) = self.repos[i].sec_page.osint.unreachable_run.as_mut() {
                    run.progress = progress;
                }
            }
            Event::UnreachableDone {
                repo,
                elapsed_ms,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                r.sec_page.osint.unreachable_run = None;
                let (name, cwd) = (r.name.clone(), r.workdir.display().to_string());
                let (summary, commands) = match &result {
                    Ok(u) => (
                        Ok(format!(
                            "{} unreachable commits · {} blobs ({}) · {} trees · {} blobs scanned ({}) · {} secret(s) inside{}",
                            format::count(u.commit_count),
                            format::count(u.blobs),
                            format::bytes(u.blob_bytes as usize),
                            format::count(u.trees),
                            format::count(u.scanned_blobs),
                            format::bytes(u.scanned_bytes as usize),
                            u.findings.len(),
                            if u.cancelled { " · cancelled" } else { "" }
                        )),
                        u.commands.clone(),
                    ),
                    Err(e) => (Err(e.clone()), Vec::new()),
                };
                r.sec_page.osint.unreachable = Some(result.map(|u| *u));
                self.log(
                    ActivityKind::Scan,
                    name,
                    cwd,
                    "Unreachable objects (git fsck)".into(),
                    commands,
                    &summary,
                    elapsed_ms,
                    None,
                );
            }
            Event::OsvProgress { repo, progress } => {
                let Some(i) = self.repo_idx(repo) else { return };
                if let Some(run) = self.repos[i].sec_page.osint.osv_run.as_mut() {
                    run.progress = progress;
                }
            }
            Event::OsvDone {
                repo,
                elapsed_ms,
                packages,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                r.sec_page.osint.osv_run = None;
                let (name, cwd) = (r.name.clone(), r.workdir.display().to_string());
                let summary = match &result {
                    Ok(v) => Ok(format!(
                        "{} packages checked · {} known vulnerabilities in {} packages",
                        format::count(packages),
                        v.len(),
                        v.iter()
                            .map(|x| (&x.package.name, &x.package.version))
                            .collect::<std::collections::HashSet<_>>()
                            .len()
                    )),
                    Err(e) => Err(e.clone()),
                };
                r.sec_page.osint.osv = Some(result);
                self.log(
                    ActivityKind::Scan,
                    name,
                    cwd,
                    "osv.dev vulnerability lookup".into(),
                    vec![
                        "curl -sS -X POST https://api.osv.dev/v1/querybatch (packages on stdin)"
                            .into(),
                    ],
                    &summary,
                    elapsed_ms,
                    None,
                );
            }
            Event::GhSurface {
                repo,
                calls,
                forks,
                dependabot,
                code_alerts,
                admin,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let o = &mut self.repos[i].sec_page.osint;
                o.gh_loading = false;
                o.gh_at = Some(Instant::now());
                let part = |r: &Result<Vec<String>, String>| match r {
                    Ok(v) => v.len().to_string(),
                    Err(_) => "n/a".to_owned(),
                };
                let summary = Ok(format!(
                    "forks listed: {} · dependabot alerts: {} · code-scanning alerts: {} · deploy keys: {} · webhooks: {} · environments: {} · secrets: {} · variables: {} · protection: {}",
                    forks
                        .as_ref()
                        .map(|f| f.len().to_string())
                        .unwrap_or_else(|_| "n/a".into()),
                    dependabot
                        .as_ref()
                        .map(|f| f.len().to_string())
                        .unwrap_or_else(|_| "n/a".into()),
                    code_alerts
                        .as_ref()
                        .map(|f| f.len().to_string())
                        .unwrap_or_else(|_| "n/a".into()),
                    admin.deploy_keys.as_ref().map(part).unwrap_or_default(),
                    admin.webhooks.as_ref().map(part).unwrap_or_default(),
                    admin.environments.as_ref().map(part).unwrap_or_default(),
                    admin.secrets.as_ref().map(part).unwrap_or_default(),
                    admin.variables.as_ref().map(part).unwrap_or_default(),
                    admin
                        .protection
                        .as_ref()
                        .and_then(|p| p.as_ref().ok().cloned())
                        .unwrap_or_else(|| "n/a".into()),
                ));
                o.forks = Some(forks);
                o.dependabot = Some(dependabot);
                o.code_alerts = Some(code_alerts);
                o.admin = Some(*admin);
                self.log_api(
                    i,
                    "GitHub forks, alerts and admin surface".into(),
                    &calls,
                    summary,
                );
            }
            Event::Dorks {
                repo,
                calls,
                results,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let o = &mut self.repos[i].sec_page.osint;
                o.dorks_running = false;
                let hits: u64 = results.iter().map(|r| r.total).sum();
                let errors = results.iter().filter(|r| r.error.is_some()).count();
                let summary = Ok(format!(
                    "{} queries · {} matching files in total · {} queries failed",
                    results.len(),
                    hits,
                    errors
                ));
                o.dorks = Some(results);
                self.log_api(i, "GitHub code-search dorks".into(), &calls, summary);
            }
            Event::Served {
                repo,
                calls,
                sha,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let summary = match &result {
                    Ok(true) => Ok("still served by GitHub".to_owned()),
                    Ok(false) => Ok("not found on GitHub".to_owned()),
                    Err(e) => Err(e.clone()),
                };
                self.repos[i]
                    .sec_page
                    .osint
                    .served
                    .insert(sha, Some(result));
                self.log_api(
                    i,
                    format!("Is {} still on GitHub?", repo::short_id(sha)),
                    &calls,
                    summary,
                );
            }
            Event::PrRefs {
                repo,
                elapsed_ms,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                r.sec_page.osint.pr_refs_running = false;
                let (name, cwd) = (r.name.clone(), r.workdir.display().to_string());
                let (summary, command) = match &result {
                    Ok(p) => (
                        Ok(format!(
                            "{} pull-request refs on {} · {} heads not in any branch this clone knows",
                            format::count(p.total),
                            p.remote,
                            format::count(p.unknown_heads)
                        )),
                        vec![p.command.clone()],
                    ),
                    Err(e) => (Err(e.clone()), Vec::new()),
                };
                r.sec_page.osint.pr_refs = Some(result);
                self.log(
                    ActivityKind::Scan,
                    name,
                    cwd,
                    "Pull-request refs on the remote".into(),
                    command,
                    &summary,
                    elapsed_ms,
                    None,
                );
            }
            Event::InsightsProgress {
                repo,
                generation,
                progress,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let p = &mut self.repos[i].ins_page;
                if p.generation == generation
                    && let Some(run) = p.run.as_mut()
                {
                    run.progress = progress;
                }
            }
            Event::InsightsDone {
                repo,
                generation,
                elapsed_ms,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                if r.ins_page.generation != generation {
                    return;
                }
                r.ins_page.run = None;
                let (name, cwd) = (r.name.clone(), r.workdir.display().to_string());
                match result {
                    Ok(report) => {
                        let summary = insights_summary(&report);
                        let commands = report.commands.clone();
                        r.ins_page.report = Some(*report);
                        r.ins_page.report_at = Some(Instant::now());
                        self.log(
                            ActivityKind::Scan,
                            name,
                            cwd,
                            "Insights analysis (health, history, branches, debt, CI, licences)"
                                .into(),
                            commands,
                            &Ok(summary),
                            elapsed_ms,
                            None,
                        );
                    }
                    Err(e) => {
                        r.ins_page.error = Some(e.clone());
                        self.log(
                            ActivityKind::Scan,
                            name,
                            cwd,
                            "Insights analysis".into(),
                            Vec::new(),
                            &Err(e.clone()),
                            elapsed_ms,
                            None,
                        );
                        self.toast(ToastKind::Error, "Analysis failed", e);
                    }
                }
            }
            Event::WeightProgress { repo, progress } => {
                let Some(i) = self.repo_idx(repo) else { return };
                if let Some(run) = self.repos[i].ins_page.weight_run.as_mut() {
                    run.progress = progress;
                }
            }
            Event::WeightDone {
                repo,
                elapsed_ms,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                r.ins_page.weight_run = None;
                let (name, cwd) = (r.name.clone(), r.workdir.display().to_string());
                let (summary, commands) = match &result {
                    Ok(w) => (
                        Ok(format!(
                            "{} blobs ({} uncompressed, {} on disk) · {} commits · {} trees · {} big blobs listed · {} of big blobs no longer in HEAD{}",
                            format::count(w.total_blobs as usize),
                            format::bytes(w.total_bytes as usize),
                            format::bytes(w.total_disk as usize),
                            format::count(w.commits as usize),
                            format::count(w.trees as usize),
                            w.blobs.len(),
                            format::bytes(w.deleted_bytes as usize),
                            if w.cancelled { " · cancelled" } else { "" }
                        )),
                        w.commands.clone(),
                    ),
                    Err(e) => (Err(e.clone()), Vec::new()),
                };
                r.ins_page.weight = Some(result.map(|w| *w));
                self.log(
                    ActivityKind::Scan,
                    name,
                    cwd,
                    "Repository weight (every object in history)".into(),
                    commands,
                    &summary,
                    elapsed_ms,
                    None,
                );
            }
            Event::DatingProgress { repo, progress } => {
                let Some(i) = self.repo_idx(repo) else { return };
                if let Some(run) = self.repos[i].ins_page.dating_run.as_mut() {
                    run.progress = progress;
                }
            }
            Event::DatingDone {
                repo,
                elapsed_ms,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                r.ins_page.dating_run = None;
                let (name, cwd) = (r.name.clone(), r.workdir.display().to_string());
                let (summary, commands) = match result {
                    Ok(d) => {
                        let dated = d.markers.iter().filter(|m| m.time.is_some()).count();
                        let summary = format!(
                            "{} markers dated in {} files ({} files skipped){}",
                            format::count(dated),
                            d.files_blamed,
                            d.files_skipped,
                            if d.cancelled { " · cancelled" } else { "" }
                        );
                        if let Some(rep) = r.ins_page.report.as_mut() {
                            rep.debt.markers = d.markers;
                            rep.debt.dated = true;
                            rep.debt.dated_files = d.files_blamed;
                        }
                        (Ok(summary), d.commands)
                    }
                    Err(e) => {
                        self.toast(ToastKind::Error, "Dating failed", e.clone());
                        (Err(e), Vec::new())
                    }
                };
                self.log(
                    ActivityKind::Scan,
                    name,
                    cwd,
                    "Debt markers dated (git blame)".into(),
                    commands,
                    &summary,
                    elapsed_ms,
                    None,
                );
            }
            Event::BlameDone {
                repo,
                path,
                elapsed_ms,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                let (name, cwd) = (r.name.clone(), r.workdir.display().to_string());
                let (summary, commands) = match &result {
                    Ok(b) => (
                        Ok(format!(
                            "{} lines · {} authors · {} commits · bus factor {}",
                            format::count(b.lines as usize),
                            b.owners.len(),
                            b.commits,
                            b.bus_factor
                        )),
                        vec![b.command.clone()],
                    ),
                    Err(e) => (Err(e.clone()), Vec::new()),
                };
                r.ins_page.blame.insert(path.clone(), Some(result));
                self.log(
                    ActivityKind::Read,
                    name,
                    cwd,
                    format!("Blame {path}"),
                    commands,
                    &summary,
                    elapsed_ms,
                    None,
                );
            }
            Event::SearchDone {
                repo,
                elapsed_ms,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                r.ins_page.search_run = None;
                let (name, cwd) = (r.name.clone(), r.workdir.display().to_string());
                let (summary, commands) = match &result {
                    Ok(s) => (
                        Ok(format!(
                            "{} commits{} · {} files · {} authors{}",
                            format::count(s.hits.len()),
                            if s.truncated { " (more exist)" } else { "" },
                            s.paths.len(),
                            s.authors.len(),
                            if s.cancelled { " · cancelled" } else { "" }
                        )),
                        vec![s.command.clone()],
                    ),
                    Err(e) => (Err(e.clone()), Vec::new()),
                };
                r.ins_page.search = Some(result.map(|s| *s));
                self.log(
                    ActivityKind::Read,
                    name,
                    cwd,
                    "History search".into(),
                    commands,
                    &summary,
                    elapsed_ms,
                    None,
                );
            }
            Event::LostProgress { repo, progress } => {
                let Some(i) = self.repo_idx(repo) else { return };
                if let Some(run) = self.repos[i].ins_page.lost_run.as_mut() {
                    run.progress = progress;
                }
            }
            Event::LostDone {
                repo,
                elapsed_ms,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                r.ins_page.lost_run = None;
                let (name, cwd) = (r.name.clone(), r.workdir.display().to_string());
                let (summary, commands) = match &result {
                    Ok(l) => (
                        Ok(format!(
                            "{} lost commits ({} stashes) · {} still in a reflog · {} dangling · {} blobs · {} trees{}",
                            format::count(l.commits.len()),
                            l.stashes,
                            l.in_reflog,
                            l.dangling,
                            format::count(l.blobs),
                            format::count(l.trees),
                            if l.cancelled { " · cancelled" } else { "" }
                        )),
                        l.commands.clone(),
                    ),
                    Err(e) => (Err(e.clone()), Vec::new()),
                };
                r.ins_page.lost = Some(result.map(|l| *l));
                self.log(
                    ActivityKind::Scan,
                    name,
                    cwd,
                    "Lost commits and stashes (git fsck)".into(),
                    commands,
                    &summary,
                    elapsed_ms,
                    None,
                );
            }
            Event::OpDone {
                repo,
                label,
                refresh,
                network,
                remote_only,
                commands,
                elapsed_ms,
                result,
            } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let (repo_name, cwd) = (
                    self.repos[i].name.clone(),
                    self.repos[i].workdir.display().to_string(),
                );
                let bytes = result.as_ref().map(|s| s.len()).unwrap_or(0);
                self.log(
                    ActivityKind::Op,
                    repo_name,
                    cwd,
                    label.clone(),
                    commands,
                    &result,
                    elapsed_ms,
                    Some(bytes),
                );
                let r = &mut self.repos[i];
                r.busy = None;
                r.suppress_watch_until = Some(Instant::now() + SUPPRESS_AFTER_OP);
                let pending_full = r.pending_reload.take().unwrap_or(false);
                match result {
                    Ok(message) => {
                        if label == "Commit" || label == "Amend commit" {
                            r.commit_message.clear();
                            r.amend = false;
                        }
                        if !remote_only && r.sec_page.full.is_some() {
                            r.sec_page.stale = true;
                        }
                        if r.queued.is_empty() {
                            self.toast(ToastKind::Success, label, trim_output(&message));
                            if remote_only {
                                if pending_full {
                                    self.request_load(i, true);
                                }
                                self.maybe_fetch_github(i, true);
                                if let Some(run) = self.repos[i].gh_page.selected_run {
                                    self.refresh_run(i, run);
                                }
                            } else {
                                self.request_load(i, pending_full || refresh == Refresh::Full);
                                if network {
                                    self.maybe_fetch_github(i, true);
                                }
                            }
                        } else {
                            let next = r.queued.remove(0);
                            self.start_op(i, next);
                        }
                    }
                    Err(e) => {
                        r.queued.clear();
                        self.toast(ToastKind::Error, format!("{label} failed"), e);
                        if !remote_only {
                            self.request_load(i, true);
                        }
                    }
                }
            }
            Event::CloneDone {
                dest,
                command,
                elapsed_ms,
                result,
            } => {
                self.cloning = None;
                let name = dest
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let cwd = dest
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                self.log(
                    ActivityKind::Op,
                    name,
                    cwd,
                    "Clone".into(),
                    vec![command],
                    &result,
                    elapsed_ms,
                    None,
                );
                match result {
                    Ok(_) => {
                        self.toast(
                            ToastKind::Success,
                            "Clone finished",
                            dest.display().to_string(),
                        );
                        self.open_repo(dest, true);
                    }
                    Err(e) => self.toast(ToastKind::Error, "Clone failed", e),
                }
            }
            Event::Watch { repo, full } => {
                let Some(i) = self.repo_idx(repo) else { return };
                let r = &mut self.repos[i];
                let now = Instant::now();
                if r.suppress_watch_until.is_some_and(|t| now < t) {
                    return;
                }
                match &mut r.dirty {
                    Some(d) => {
                        d.last = now;
                        d.full |= full;
                    }
                    None => {
                        r.dirty = Some(Dirty {
                            first: now,
                            last: now,
                            full,
                        })
                    }
                }
            }
        }
    }

    fn tick_dirty(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        for i in 0..self.repos.len() {
            let Some(d) = &self.repos[i].dirty else {
                continue;
            };
            if now.duration_since(d.last) >= DEBOUNCE || now.duration_since(d.first) >= DEBOUNCE_MAX
            {
                let full = d.full;
                self.repos[i].dirty = None;
                self.request_load(i, full);
            } else {
                ctx.request_repaint_after(DEBOUNCE);
            }
        }
    }

    fn shortcuts(&self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let text_focused = ctx.memory(|m| m.focused().is_some());
        let has_dialog = self.dialog.is_some();
        let repo = self.repos.get(self.active);
        let file_open = repo.is_some_and(|r| r.file.is_some());
        let page = repo.map(|r| r.page).unwrap_or_default();
        let on_github = page != Page::Graph;
        let has_github = repo.is_some_and(|r| r.github_remote.is_some());
        let has_repo = !self.repos.is_empty();
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::O) {
                actions.push(Action::CloneDialog);
            }
            if has_github && i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::G) {
                actions.push(Action::ShowPage(if page == Page::GitHub {
                    Page::Graph
                } else {
                    Page::GitHub
                }));
            }
            if has_repo && i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::S) {
                actions.push(Action::ShowPage(if page == Page::Security {
                    Page::Graph
                } else {
                    Page::Security
                }));
            }
            if has_repo && i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::I) {
                actions.push(Action::ShowPage(if page == Page::Insights {
                    Page::Graph
                } else {
                    Page::Insights
                }));
            }
            if i.consume_key(Modifiers::COMMAND, Key::O) {
                actions.push(Action::PickRepo);
            }
            if has_repo && i.consume_key(Modifiers::COMMAND, Key::F) {
                actions.push(Action::FocusSearch);
            }
            if has_repo && i.consume_key(Modifiers::COMMAND, Key::W) {
                actions.push(Action::CloseRepo(self.active));
            }
            if has_repo && i.consume_key(Modifiers::NONE, Key::F5) {
                actions.push(Action::Reload(true));
            }
            if i.consume_key(Modifiers::COMMAND, Key::L) {
                actions.push(Action::ToggleActivity);
            }
            if has_repo && i.consume_key(Modifiers::COMMAND, Key::Z) {
                actions.push(Action::Undo);
            }
            if has_repo && !text_focused && !has_dialog {
                if !on_github && i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                    actions.push(Action::MoveSelection(1));
                }
                if !on_github && i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                    actions.push(Action::MoveSelection(-1));
                }
                if file_open && i.consume_key(Modifiers::NONE, Key::Escape) {
                    actions.push(Action::DiffEscape);
                } else if on_github && i.consume_key(Modifiers::NONE, Key::Escape) {
                    actions.push(Action::ShowPage(Page::Graph));
                }
            }
        });
    }

    /// Where a clone goes by default: next to the current repository, else
    /// next to the last opened one, else the home directory.
    fn default_clone_dir(&self) -> PathBuf {
        self.repos
            .get(self.active)
            .and_then(|r| r.workdir.parent().map(Path::to_path_buf))
            .or_else(|| {
                self.settings
                    .recent
                    .first()
                    .and_then(|p| p.parent().map(Path::to_path_buf))
            })
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .or_else(|| std::env::var_os("HOME"))
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn start_clone(&mut self, url: String, dest: PathBuf) {
        if let Some(name) = &self.cloning {
            let name = name.clone();
            self.toast(
                ToastKind::Info,
                "Please wait",
                format!("{name} is still being cloned"),
            );
            return;
        }
        let name = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repository".to_owned());
        self.cloning = Some(name.clone());
        self.toast(ToastKind::Info, format!("Cloning {name}…"), url.clone());
        self.worker.run_clone(url, dest);
    }

    /// Execute due `GITVIS_SCRIPT` commands.
    fn run_script(&mut self, ctx: &egui::Context) {
        let Some(script) = self.script.as_mut() else {
            return;
        };
        ctx.request_repaint_after(Duration::from_millis(50));
        let mut cmds = Vec::new();
        while let Some(c) = script.next_ready() {
            cmds.push(c);
        }
        let i = self.active;
        for cmd in cmds {
            eprintln!("[script] {cmd:?}");
            match cmd {
                Cmd::Wait(_) => {}
                Cmd::Shot(path) => {
                    ctx.request_screenshot(move |img| crate::script::save_png(&path, &img))
                }
                Cmd::Tab(n) => self.apply(ctx, Action::Activate(n)),
                Cmd::Head => {
                    if let Some(head) = self
                        .repos
                        .get(i)
                        .and_then(|r| r.snapshot.as_ref())
                        .and_then(|s| s.head.target())
                    {
                        self.apply(ctx, Action::JumpTo(head));
                    }
                }
                Cmd::Wip => self.apply(ctx, Action::Select(Selection::Wip)),
                Cmd::Open(path) => {
                    let target = self.repos.get(i).and_then(|r| {
                        let snap = r.snapshot.as_ref()?;
                        let Selection::Commit(id) = r.selection else {
                            return None;
                        };
                        let idx = *snap.index_of.get(&id)?;
                        Some(DiffTarget::Commit {
                            id,
                            parent: snap.commits[idx].parent_ids.first().copied(),
                            path,
                            old_path: None,
                        })
                    });
                    if let Some(t) = target {
                        self.apply(ctx, Action::OpenFile(t));
                    }
                }
                Cmd::OpenWip(path) => {
                    self.apply(ctx, Action::OpenFile(DiffTarget::Unstaged { path }))
                }
                Cmd::Split => self.apply(ctx, Action::SetDiffMode(DiffMode::Split)),
                Cmd::Unified => self.apply(ctx, Action::SetDiffMode(DiffMode::Unified)),
                Cmd::Select(hunk, anchor, end) => {
                    if let Some(r) = self.repos.get_mut(i) {
                        r.diff_selection = Some(DiffSelection {
                            hunk,
                            anchor,
                            end,
                            mode: self.settings.diff_mode,
                            dragging: false,
                        });
                    }
                }
                Cmd::Activity => {
                    self.settings.show_activity = true;
                    self.activity_tab = ActivityTab::Log;
                }
                Cmd::Reflog => {
                    self.settings.show_activity = true;
                    self.activity_tab = ActivityTab::Reflog;
                }
                Cmd::Theme => self.apply(ctx, Action::ToggleTheme),
                Cmd::Collapse(key) => {
                    if let Some(r) = self.repos.get_mut(i)
                        && !r.collapsed.remove(&key)
                    {
                        r.collapsed.insert(key);
                    }
                }
                Cmd::Stage(path) => {
                    self.apply(ctx, Action::Run(GitOp::Stage { paths: vec![path] }))
                }
                Cmd::Undo => self.apply(ctx, Action::Undo),
                Cmd::Github => self.apply(ctx, Action::ShowPage(Page::GitHub)),
                Cmd::Graph => self.apply(ctx, Action::ShowPage(Page::Graph)),
                Cmd::Prs => {
                    self.apply(ctx, Action::ShowPage(Page::GitHub));
                    self.apply(ctx, Action::SetGitHubTab(GitHubTab::PullRequests));
                }
                Cmd::Filter(text) => {
                    if let Some(r) = self.repos.get_mut(i) {
                        r.gh_page.run_filter = text;
                    }
                }
                Cmd::Status(which) => {
                    use crate::state::RunFilter;
                    if let Some(r) = self.repos.get_mut(i) {
                        r.gh_page.run_status = match which.as_str() {
                            "failed" => RunFilter::Failed,
                            "running" => RunFilter::Running,
                            "passed" => RunFilter::Success,
                            _ => RunFilter::All,
                        };
                    }
                }
                Cmd::Run(n) => {
                    let run = self.repos.get(i).and_then(|r| {
                        let data = r.github.as_ref()?;
                        views::github::visible_runs(data, &r.gh_page)
                            .get(n)
                            .map(|run| run.id)
                    });
                    if let Some(run) = run {
                        self.apply(ctx, Action::OpenRun { run, job: None });
                    }
                }
                Cmd::JobLog(n) => {
                    let target = self.repos.get(i).and_then(|r| {
                        let run = r.gh_page.selected_run?;
                        let job = r.gh_page.runs.get(&run)?.jobs().get(n)?.id;
                        Some((run, job))
                    });
                    if let Some((run, job)) = target {
                        self.apply(
                            ctx,
                            Action::ShowLog {
                                run,
                                job,
                                step: None,
                            },
                        );
                    }
                }
                Cmd::Pr(n) => {
                    let number = self
                        .repos
                        .get(i)
                        .and_then(|r| r.github.as_ref())
                        .and_then(|d| d.prs.get(n).map(|p| p.number));
                    if let Some(number) = number {
                        self.apply(ctx, Action::OpenPr(number));
                    }
                }
                Cmd::Verbose => self.apply(ctx, Action::ToggleVerbose),
                Cmd::Security => self.apply(ctx, Action::ShowPage(Page::Security)),
                Cmd::Scan => self.apply(ctx, Action::ScanSecrets { history: false }),
                Cmd::ScanHistory(n) => {
                    self.apply(ctx, Action::SetHistoryDepth(n));
                    self.apply(ctx, Action::ScanSecrets { history: true });
                }
                Cmd::Finding(n) => {
                    let key = self.repos.get(i).and_then(|r| {
                        views::security::visible_items(r)
                            .get(n)
                            .map(|(k, _)| k.clone())
                    });
                    if let Some(key) = key {
                        self.apply(ctx, Action::SelectFinding(Some(key)));
                    }
                }
                Cmd::Reveal => {
                    if let Some(key) = self.repos.get(i).and_then(|r| r.sec_page.selected.clone()) {
                        self.apply(ctx, Action::ToggleReveal(key));
                    }
                }
                Cmd::SecTab(name) => {
                    if let Some(tab) = SecurityTab::from_name(&name) {
                        self.apply(ctx, Action::ShowPage(Page::Security));
                        self.apply(ctx, Action::SetSecurityTab(tab));
                    }
                }
                Cmd::Osint => self.apply(ctx, Action::RunOsint),
                Cmd::Unreachable => self.apply(ctx, Action::RunUnreachable),
                Cmd::Osv => self.apply(ctx, Action::RunOsv),
                Cmd::Dorks => self.apply(ctx, Action::RunDorks),
                Cmd::PrRefs => self.apply(ctx, Action::CheckPrRefs),
                Cmd::Item(n) => {
                    let key = self
                        .repos
                        .get(i)
                        .and_then(|r| views::osint::item_keys(r).get(n).cloned());
                    if let Some(key) = key {
                        self.apply(ctx, Action::SelectOsint(Some(key)));
                    }
                }
                Cmd::Insights => self.apply(ctx, Action::ShowPage(Page::Insights)),
                Cmd::InsTab(name) => {
                    if let Some(tab) = InsightsTab::from_name(&name) {
                        self.apply(ctx, Action::ShowPage(Page::Insights));
                        self.apply(ctx, Action::SetInsightsTab(tab));
                    }
                }
                Cmd::Analyse(n) => {
                    self.apply(ctx, Action::SetInsightsDepth(n));
                    self.apply(ctx, Action::RunInsights);
                }
                Cmd::Weight => self.apply(ctx, Action::RunWeight),
                Cmd::DateDebt => self.apply(ctx, Action::DateDebt),
                Cmd::Pickaxe(_) | Cmd::Grep(_) => {
                    let (grep, text) = match cmd {
                        Cmd::Grep(t) => (true, t),
                        Cmd::Pickaxe(t) => (false, t),
                        _ => unreachable!(),
                    };
                    if let Some(r) = self.repos.get_mut(i) {
                        r.ins_page.search_text = text;
                        r.ins_page.search_mode = if grep {
                            gitvis_core::insights::search::SearchMode::Message
                        } else {
                            gitvis_core::insights::search::SearchMode::Pickaxe
                        };
                    }
                    self.apply(ctx, Action::ShowPage(Page::Insights));
                    self.apply(ctx, Action::SetInsightsTab(InsightsTab::Search));
                    self.apply(ctx, Action::RunSearch);
                }
                Cmd::Recover => {
                    self.apply(ctx, Action::ShowPage(Page::Insights));
                    self.apply(ctx, Action::SetInsightsTab(InsightsTab::Recovery));
                    self.apply(ctx, Action::RunLost);
                }
                Cmd::InsItem(n) => {
                    let key = self
                        .repos
                        .get(i)
                        .and_then(|r| views::insights::item_keys(r).get(n).cloned());
                    if let Some(key) = key {
                        self.apply(ctx, Action::SelectInsight(Some(key)));
                    }
                }
                Cmd::Blame(path) => {
                    self.apply(ctx, Action::ShowPage(Page::Insights));
                    self.apply(ctx, Action::SetInsightsTab(InsightsTab::Ownership));
                    self.apply(ctx, Action::SelectInsight(Some(format!("file:{path}"))));
                    self.apply(ctx, Action::BlameFile(path));
                }
                Cmd::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }
    }

    fn undo(&mut self, i: usize) {
        let Some(undo) = self.repos.get(i).and_then(|r| r.undo.clone()) else {
            self.toast(
                ToastKind::Info,
                "Nothing to undo",
                "No recent HEAD movement found in the reflog.",
            );
            return;
        };
        if undo.destructive {
            let dirty = self.repos[i]
                .snapshot
                .as_ref()
                .is_some_and(|s| !s.status.is_clean());
            let mut body = undo.detail.clone();
            if dirty {
                body.push_str("\n\nYou have uncommitted changes: they will be discarded.");
            }
            self.dialog = Some(Dialog::Confirm {
                title: format!("{}?", undo.label),
                body,
                confirm: undo.label.clone(),
                danger: true,
                ops: undo.ops,
            });
            self.dialog_fresh = true;
        } else {
            self.run_ops(i, undo.ops);
        }
    }

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        let i = self.active;
        let has_repo = i < self.repos.len();
        match action {
            Action::Run(op) if has_repo => self.run_ops(i, vec![op]),
            Action::RunSeq(ops) if has_repo => self.run_ops(i, ops),
            Action::Dialog(d) => {
                self.dialog = Some(d);
                self.dialog_fresh = true;
            }
            Action::Select(sel) if has_repo => self.select(i, sel),
            Action::JumpTo(oid) if has_repo => self.jump_to(i, oid),
            Action::OpenFile(t) if has_repo => self.open_file(i, t),
            Action::CloseFile if has_repo => {
                let r = &mut self.repos[i];
                r.file = None;
                r.diff = None;
                r.diff_selection = None;
            }
            Action::DiffEscape if has_repo => {
                let r = &mut self.repos[i];
                if r.diff_selection.is_some() {
                    r.diff_selection = None;
                } else {
                    r.file = None;
                    r.diff = None;
                }
            }
            Action::Reload(full) if has_repo => self.request_load(i, full),
            Action::CloneDialog => {
                let dir = self.default_clone_dir().display().to_string();
                self.dialog = Some(Dialog::Clone {
                    url: String::new(),
                    dir,
                    name: String::new(),
                    name_edited: false,
                });
                self.dialog_fresh = true;
            }
            Action::Clone { url, dest } => self.start_clone(url, dest),
            Action::PickRepo => {
                let mut dialog = rfd::FileDialog::new().set_title("Open a git repository");
                let start = self
                    .repos
                    .get(i)
                    .and_then(|r| r.workdir.parent().map(Path::to_path_buf))
                    .or_else(|| {
                        self.settings
                            .recent
                            .first()
                            .and_then(|p| p.parent().map(Path::to_path_buf))
                    });
                if let Some(s) = start {
                    dialog = dialog.set_directory(s);
                }
                if let Some(p) = dialog.pick_folder() {
                    self.open_repo(p, true);
                }
            }
            Action::OpenRepo(p) => self.open_repo(p, true),
            Action::CloseRepo(idx) => self.close_repo(idx),
            Action::Activate(idx) => {
                if idx < self.repos.len() {
                    self.active = idx;
                }
            }
            Action::ToggleTheme => {
                self.settings.dark = !self.settings.dark;
                theme::apply(ctx, self.settings.dark);
                // Syntax colours depend on the theme.
                for i in 0..self.repos.len() {
                    self.reload_diff(i);
                }
            }
            Action::ToggleRelativeDates => {
                self.settings.relative_dates = !self.settings.relative_dates
            }
            Action::ToggleAuthor => self.settings.show_author = !self.settings.show_author,
            Action::ToggleVerbose => self.settings.verbose = !self.settings.verbose,
            Action::ToggleStatusBar => {
                self.settings.show_status_bar = !self.settings.show_status_bar
            }
            Action::SetPull(p) => self.settings.pull = p,
            Action::SetDiffMode(m) => {
                self.settings.diff_mode = m;
                if has_repo {
                    self.repos[i].diff_selection = None;
                }
            }
            Action::ToggleDiffWhitespace => {
                self.settings.diff_ignore_ws = !self.settings.diff_ignore_ws;
                if has_repo {
                    self.reload_diff(i);
                }
            }
            Action::SetDiffContext(n) => {
                self.settings.diff_context = n.clamp(0, 50);
                if has_repo {
                    self.reload_diff(i);
                }
            }
            Action::ToggleActivity => self.settings.show_activity = !self.settings.show_activity,
            Action::ClearActivity => self.activity.clear(),
            Action::RefreshGitHub if has_repo => {
                self.maybe_fetch_github(i, true);
                let r = &self.repos[i];
                let selected = r.gh_page.selected_run.filter(|_| r.page == Page::GitHub);
                let sel_commit = match r.selection {
                    Selection::Commit(id) => Some(id),
                    _ => None,
                };
                if let Some(run) = selected {
                    self.refresh_run(i, run);
                }
                if let Some(id) = sel_commit {
                    self.ensure_checks(i, id, true);
                }
            }
            Action::Undo if has_repo => self.undo(i),
            Action::OpenUrl(url) => {
                if let Err(e) = open::that_detached(&url) {
                    self.toast(
                        ToastKind::Error,
                        "Could not open the browser",
                        e.to_string(),
                    );
                }
            }
            Action::Toast(kind, msg) => self.toast(kind, msg, ""),
            Action::Copy(text) => {
                ctx.copy_text(text);
                self.toast(ToastKind::Info, "Copied to clipboard", "");
            }
            Action::OpenPath(p) => {
                if let Err(e) = open::that_detached(&p) {
                    self.toast(ToastKind::Error, "Could not open file", e.to_string());
                }
            }
            Action::ToggleHidden(full) if has_repo => {
                let r = &mut self.repos[i];
                if let Some(pos) = r.filter.hidden.iter().position(|h| *h == full) {
                    r.filter.hidden.remove(pos);
                } else {
                    r.filter.hidden.push(full);
                }
                self.save_prefs(i);
                self.request_load(i, true);
            }
            Action::Solo(solo) if has_repo => {
                self.repos[i].filter.solo = solo;
                self.request_load(i, true);
            }
            Action::ToggleRemotes if has_repo => {
                let r = &mut self.repos[i];
                r.filter.hide_remotes = !r.filter.hide_remotes;
                self.save_prefs(i);
                self.request_load(i, true);
            }
            Action::ToggleTags if has_repo => {
                let r = &mut self.repos[i];
                r.filter.hide_tags = !r.filter.hide_tags;
                self.save_prefs(i);
                self.request_load(i, true);
            }
            Action::FocusSearch => {
                self.focus_search = true;
                if has_repo {
                    self.repos[i].page = Page::Graph;
                }
            }
            Action::SearchChanged if has_repo => {
                let r = &mut self.repos[i];
                r.recompute_search();
                r.search_cursor = 0;
                if let Some(&row) = r.search_hits.first() {
                    self.jump_row(i, row);
                }
            }
            Action::SearchStep(delta) if has_repo => {
                let r = &mut self.repos[i];
                let n = r.search_hits.len();
                if n > 0 {
                    r.search_cursor =
                        (r.search_cursor as i64 + delta as i64).rem_euclid(n as i64) as usize;
                    let row = r.search_hits[r.search_cursor];
                    self.jump_row(i, row);
                }
            }
            Action::MoveSelection(delta) if has_repo => {
                let r = &self.repos[i];
                let Some(snap) = r.snapshot.as_ref() else {
                    return;
                };
                let n = snap.row_count();
                if n == 0 {
                    return;
                }
                let current = r.selected_row().unwrap_or(r.visible_rows.start.min(n - 1));
                let next = (current as i64 + delta as i64).clamp(0, n as i64 - 1) as usize;
                let sel = r.selection_for_row(next);
                self.select(i, sel);
                let r = &mut self.repos[i];
                if !r.visible_rows.contains(&next)
                    || next + 1 >= r.visible_rows.end
                    || (next > 0 && next <= r.visible_rows.start)
                {
                    r.scroll_to_row = Some(next);
                }
            }
            Action::ShowPage(page) if has_repo => {
                let r = &mut self.repos[i];
                if page == Page::GitHub && r.github_remote.is_none() {
                    self.toast(
                        ToastKind::Info,
                        "No GitHub remote",
                        "Add a remote on github.com to see pull requests and workflow runs.",
                    );
                    return;
                }
                r.page = page;
                if page == Page::GitHub {
                    self.maybe_fetch_github(i, false);
                }
                if page == Page::Security {
                    self.fetch_exposure(i, false);
                    let tab = self.repos[i].sec_page.tab;
                    if tab.needs_osint() && self.repos[i].sec_page.osint.report.is_none() {
                        self.start_osint(i);
                    }
                    if tab == SecurityTab::GitHub {
                        self.fetch_gh_surface(i, false);
                    }
                }
                if page == Page::Insights {
                    let tab = self.repos[i].ins_page.tab;
                    if tab.needs_report() && self.repos[i].ins_page.report.is_none() {
                        self.start_insights(i);
                    }
                    if tab == InsightsTab::Recovery && self.repos[i].ins_page.lost.is_none() {
                        self.start_lost(i);
                    }
                }
            }
            Action::ScanSecrets { history } if has_repo => self.start_scan(i, history, false),
            Action::CancelScan if has_repo => self.cancel_scan(i),
            Action::SelectFinding(key) if has_repo => self.repos[i].sec_page.selected = key,
            Action::DismissFinding(fp) if has_repo => {
                let r = &mut self.repos[i];
                r.dismissed.insert(fp.clone());
                if r.sec_page.selected.as_deref() == Some(fp.as_str()) && !r.sec_page.show_dismissed
                {
                    r.sec_page.selected = None;
                }
                self.save_prefs(i);
            }
            Action::RestoreFinding(fp) if has_repo => {
                self.repos[i].dismissed.remove(&fp);
                self.save_prefs(i);
            }
            Action::ToggleReveal(fp) if has_repo => {
                let revealed = &mut self.repos[i].sec_page.revealed;
                if !revealed.remove(&fp) {
                    revealed.insert(fp);
                }
            }
            Action::AddToGitignore(pattern) if has_repo => self.add_to_gitignore(i, pattern),
            Action::FetchExposure if has_repo => self.fetch_exposure(i, true),
            Action::SetHistoryDepth(n) if has_repo => {
                self.repos[i].sec_page.history_depth = n;
                self.save_prefs(i);
            }
            Action::ToggleAllRefs if has_repo => {
                let p = &mut self.repos[i].sec_page;
                p.all_refs = !p.all_refs;
                self.save_prefs(i);
            }
            Action::ToggleScanOnOpen => self.settings.scan_on_open = !self.settings.scan_on_open,
            Action::SetSecurityTab(tab) if has_repo => self.set_security_tab(i, tab),
            Action::RunOsint if has_repo => self.start_osint(i),
            Action::CancelOsint if has_repo => {
                if let Some(run) = &self.repos[i].sec_page.osint.run {
                    run.cancel.store(true, Ordering::Relaxed);
                }
            }
            Action::RunUnreachable if has_repo => self.start_unreachable(i),
            Action::RunOsv if has_repo => self.start_osv(i),
            Action::FetchGhSurface if has_repo => {
                self.fetch_exposure(i, true);
                self.fetch_gh_surface(i, true);
            }
            Action::RunDorks if has_repo => self.start_dorks(i),
            Action::CheckServed(shas) if has_repo => self.check_served(i, shas),
            Action::CheckPrRefs if has_repo => self.check_pr_refs(i),
            Action::SelectOsint(key) if has_repo => self.repos[i].sec_page.osint.selected = key,
            Action::SetInsightsTab(tab) if has_repo => self.set_insights_tab(i, tab),
            Action::RunInsights if has_repo => self.start_insights(i),
            Action::CancelInsights if has_repo => {
                if let Some(run) = &self.repos[i].ins_page.run {
                    run.cancel.store(true, Ordering::Relaxed);
                }
            }
            Action::SelectInsight(key) if has_repo => self.repos[i].ins_page.selected = key,
            Action::SetInsightsDepth(n) if has_repo => {
                self.repos[i].ins_page.depth = n;
                self.save_prefs(i);
            }
            Action::ToggleInsightsAllRefs if has_repo => {
                let p = &mut self.repos[i].ins_page;
                p.all_refs = !p.all_refs;
                self.save_prefs(i);
            }
            Action::RunWeight if has_repo => self.start_weight(i),
            Action::CancelWeight if has_repo => {
                if let Some(run) = &self.repos[i].ins_page.weight_run {
                    run.cancel.store(true, Ordering::Relaxed);
                }
            }
            Action::DateDebt if has_repo => self.start_dating(i),
            Action::CancelDating if has_repo => {
                if let Some(run) = &self.repos[i].ins_page.dating_run {
                    run.cancel.store(true, Ordering::Relaxed);
                }
            }
            Action::BlameFile(path) if has_repo => self.start_blame(i, path),
            Action::RunSearch if has_repo => self.start_search(i),
            Action::CancelSearch if has_repo => {
                if let Some(run) = &self.repos[i].ins_page.search_run {
                    run.cancel.store(true, Ordering::Relaxed);
                }
            }
            Action::RunLost if has_repo => self.start_lost(i),
            Action::CancelLost if has_repo => {
                if let Some(run) = &self.repos[i].ins_page.lost_run {
                    run.cancel.store(true, Ordering::Relaxed);
                }
            }
            Action::SetGitHubTab(tab) if has_repo => self.repos[i].gh_page.tab = tab,
            Action::OpenRun { run, job } if has_repo => self.open_run(i, run, job),
            Action::RefreshRun(run) if has_repo => self.refresh_run(i, run),
            Action::ShowLog { run, job, step } if has_repo => self.show_log(i, run, job, step),
            Action::LoadAnnotations { run, job } if has_repo => self.load_annotations(i, run, job),
            Action::OpenPr(number) if has_repo => self.open_pr(i, number),
            Action::RefreshChecks(sha) if has_repo => self.ensure_checks(i, sha, true),
            Action::FindContaining(id) if has_repo => self.request_contains(i, id),
            _ => {}
        }
    }
}

/// Steps of a job as `(number, started)` pairs, for log attribution.
fn detail_steps(detail: &RunDetail, job: u64) -> Option<Vec<(u32, Option<i64>)>> {
    detail
        .jobs()
        .iter()
        .find(|j| j.id == job)
        .map(|j| j.steps.iter().map(|s| (s.number, s.started)).collect())
}

fn scroll_log_to_step(doc: &mut LogDoc, steps: Option<&[(u32, Option<i64>)]>, step: u32) {
    let Some(steps) = steps else { return };
    if doc.steps.is_none() {
        doc.steps = Some(doc.log.step_starts(steps));
    }
    if let Some(&(_, line)) = doc
        .steps
        .as_ref()
        .and_then(|s| s.iter().find(|(n, _)| *n == step))
    {
        doc.scroll_to = Some(line);
        doc.selected = Some(line);
    }
}

/// `3 checks · 1 failed · 1 running` style text.
fn ci_summary(noun: &str, outcomes: impl Iterator<Item = Outcome>) -> String {
    let outcomes: Vec<Outcome> = outcomes.collect();
    let count = |f: fn(Outcome) -> bool| outcomes.iter().filter(|o| f(**o)).count();
    let mut parts = vec![format::plural(outcomes.len(), noun, &format!("{noun}s"))];
    let good = count(Outcome::is_good);
    let bad = count(Outcome::is_bad);
    let running = count(Outcome::is_running);
    let other = outcomes.len() - good - bad - running;
    if good > 0 {
        parts.push(format!("{good} passed"));
    }
    if bad > 0 {
        parts.push(format!("{bad} failed"));
    }
    if running > 0 {
        parts.push(format!("{running} running"));
    }
    if other > 0 {
        parts.push(format!("{other} skipped or neutral"));
    }
    parts.join(" · ")
}

fn ci_from_checks(checks: &[github::CheckRun]) -> CiInfo {
    let outcome = aggregate(checks.iter().map(|c| c.outcome()));
    let run = checks
        .iter()
        .filter(|c| c.outcome().is_bad())
        .chain(checks.iter())
        .find_map(|c| c.actions_ids().map(|(run, _)| run));
    CiInfo {
        outcome,
        run,
        summary: ci_summary("check", checks.iter().map(|c| c.outcome())),
    }
}

/// Activity-log summary of an insights analysis.
fn insights_summary(r: &InsightsReport) -> String {
    let h = &r.health;
    let hi = &r.history;
    let b = &r.refs.branches;
    let rel = &r.refs.releases;
    let q = &hi.quality;
    let phases: Vec<String> = r
        .stats
        .phases
        .iter()
        .map(|(p, ms)| format!("{p} {}", format::millis(*ms)))
        .collect();
    format!(
        "{} commits read ({} of numstat) · {} files{}\n\
         health: {} landmines · {} CRLF / {} LF files · {} mixed · {} fighting line endings · {} scripts without +x · {} submodules · {} loose objects · {} packs\n\
         history: {} hotspots · {} couplings · {} directories aged · {} directories owned ({} orphaned) · {} wip · {} fixups · {} reverts · {} giant commits · {} bot commits · {} date anomalies\n\
         refs: {} local + {} remote branches · {} merged · {} gone · {} stale · {} tags · {} unreleased since {}\n\
         debt: {} markers in {} files · ci: {} workflows, {} findings · licences: {} ({} deps, {} findings)\n\
         phases: {}",
        format::count(r.stats.commits),
        format::bytes(r.stats.log_bytes as usize),
        format::count(r.stats.files),
        if r.stats.cancelled {
            " · cancelled"
        } else {
            ""
        },
        h.landmines.len(),
        format::count(h.line_endings.crlf),
        format::count(h.line_endings.lf),
        h.line_endings.mixed.len(),
        h.line_endings.fighting.len(),
        h.exec_bits.scripts_without.len(),
        h.submodules.len(),
        format::count(h.weight.loose_objects as usize),
        h.weight.packs,
        hi.hotspots.len(),
        hi.couplings.len(),
        hi.age.len(),
        hi.ownership.len(),
        hi.ownership.iter().filter(|o| o.orphaned).count(),
        q.wip.len(),
        q.fixups.len(),
        q.reverts.len(),
        q.giant.len(),
        q.bot_commits,
        q.date_anomalies.len(),
        b.local.len(),
        b.remote.len(),
        b.merged,
        b.gone,
        b.stale,
        rel.tags.len(),
        rel.unreleased_total,
        rel.last_tag.as_deref().unwrap_or("(no tag)"),
        format::count(r.debt.markers.len()),
        r.debt.files_with_markers,
        r.ci.workflows.len(),
        r.ci.findings.len(),
        r.license.main().unwrap_or("unknown"),
        r.license.dependencies.len(),
        r.license.findings.len(),
        phases.join(", "),
    )
}

/// Activity-log summary of an OSINT analysis.
fn osint_summary(r: &OsintReport) -> String {
    use gitvis_core::osint::EmailKind;
    let p = &r.people;
    let a = &r.archaeology;
    let mut out = format!(
        "{} commits read · {} identities ({} personal, {} corporate, {} noreply, {} invalid) · local user.email: {}{}\n\
         messages: {} trailer kinds · {} ticket projects · {} URL domains · {} internal URLs · {} incident-like messages\n\
         footprint: {} findings in {} files ({}) · metadata: {} files with fields · {} junk files\n\
         history: {} rewritten commits · {} stashes ({} with secrets) · {} backup-looking branches · {} deleted sensitive files · local hooks: {}\n\
         supply chain: {} git-pinned dependencies · {} packages from {} lockfiles · {} hook/driver notes · CODEOWNERS: {}",
        format::count(p.total_commits),
        p.identities.len(),
        p.count(EmailKind::Personal),
        p.count(EmailKind::Corporate),
        p.count(EmailKind::NoReply),
        p.count(EmailKind::Invalid),
        if p.local_email.is_empty() {
            "unset"
        } else {
            &p.local_email
        },
        if p.local_scope.is_empty() {
            String::new()
        } else {
            format!(" ({})", p.local_scope)
        },
        r.messages.trailers.len(),
        r.messages.tickets.len(),
        r.messages.domains.len(),
        r.messages.internal_urls.len(),
        r.messages.incidents.len(),
        r.footprint.len(),
        format::count(r.stats.files),
        format::bytes(r.stats.bytes as usize),
        r.metadata.len(),
        r.junk.len(),
        a.rewritten_total,
        a.stashes.len(),
        a.stashes.iter().filter(|s| s.findings > 0).count(),
        a.backup_branches.len(),
        a.deleted_files.len(),
        if a.local_hooks.is_empty() {
            "none".to_owned()
        } else {
            a.local_hooks.join(", ")
        },
        r.supply.git_deps.len(),
        format::count(r.supply.packages.len()),
        r.supply.lockfiles.len(),
        r.supply.hooks.len(),
        r.codeowners
            .as_ref()
            .map(|c| format!("{} rules, {} owners", c.rules, c.owners.len()))
            .unwrap_or_else(|| "none".into()),
    );
    for f in r.footprint.iter().take(30) {
        out.push_str(&format!(
            "\n[{}] {} · {} · {} occurrence(s)",
            f.severity.label(),
            f.name,
            f.paths()
                .iter()
                .take(3)
                .copied()
                .collect::<Vec<_>>()
                .join(", "),
            f.occurrence_count
        ));
    }
    for inc in r.messages.incidents.iter().take(20) {
        out.push_str(&format!(
            "\n[incident] {} · {} · {}{}",
            repo::short_id(inc.commit.id),
            inc.reason,
            inc.commit.summary,
            if inc.pushed { " · pushed" } else { "" }
        ));
    }
    out
}

/// Activity-log summary of a secrets scan (no secret values).
fn scan_summary(r: &Report) -> String {
    let s = &r.stats;
    let mut out = format!(
        "{} findings: {} critical · {} high · {} medium · {} low ({} secrets, {} sensitive files, {} configuration issues)\n\
         files: {} listed · {} read · {} binary skipped · {} too large · {} lockfiles/media skipped · {} · {} lines",
        r.total(),
        r.count(Severity::Critical),
        r.count(Severity::High),
        r.count(Severity::Medium),
        r.count(Severity::Low),
        r.findings.len(),
        r.files.len(),
        r.config.len(),
        format::count(s.files_listed),
        format::count(s.files_scanned),
        s.skipped_binary,
        s.skipped_large,
        s.skipped_kind,
        format::bytes(s.bytes as usize),
        format::count(s.lines as usize),
    );
    if s.history {
        out.push_str(&format!(
            "\nhistory: {} commits read{}{}",
            format::count(s.commits),
            match s.commits_total {
                Some(t) if t != s.commits => format!(" of {}", format::count(t)),
                _ => String::new(),
            },
            if s.all_refs {
                " (all branches, tags and remotes)"
            } else {
                " (HEAD)"
            }
        ));
    }
    out.push_str(&format!(
        "\nremotes: {} · {} commits reachable from remote-tracking branches · HEAD pushed: {}",
        s.remotes,
        format::count(s.pushed_commits),
        if s.head_pushed { "yes" } else { "no" }
    ));
    for f in r.findings.iter().take(40) {
        out.push_str(&format!(
            "\n[{}] {} · {} · {}{}{}{}",
            f.severity.label(),
            f.name,
            f.paths().join(", "),
            if f.at_head {
                "HEAD"
            } else if f.in_worktree {
                "working tree"
            } else {
                "history"
            },
            if f.staged { " · staged" } else { "" },
            if f.pushed { " · pushed" } else { "" },
            match f.first_commit() {
                Some(c) => format!(" · since {}", repo::short_id(c.id)),
                None => String::new(),
            }
        ));
    }
    if r.findings.len() > 40 {
        out.push_str(&format!("\n… {} more", r.findings.len() - 40));
    }
    for f in &r.files {
        out.push_str(&format!(
            "\n[{}] {} · {}{}",
            f.severity.label(),
            f.rule.name,
            f.path,
            if f.tracked {
                " · tracked"
            } else if f.untracked {
                " · untracked"
            } else {
                " · history"
            }
        ));
    }
    for c in &r.config {
        out.push_str(&format!("\n[{}] {}", c.severity.label(), c.title));
    }
    if s.cancelled {
        out.push_str("\ncancelled before the end");
    }
    out
}

fn load_summary(snap: &Snapshot, max: usize) -> String {
    let head = match &snap.head {
        gitvis_core::Head::Branch { name, target } => {
            format!("{name} @ {}", repo::short_id(*target))
        }
        gitvis_core::Head::Detached(t) => format!("detached @ {}", repo::short_id(*t)),
        gitvis_core::Head::Unborn(n) => format!("{n} (no commits yet)"),
    };
    let remote_branches: usize = snap.remotes.iter().map(|r| r.branches.len()).sum();
    format!(
        "HEAD {head}\n{} commits{} · {} local branches · {} remote branches on {} · {} tags · {} stashes\nworking tree: {} staged · {} unstaged · {} conflicted · pending operation: {}\ngraph: {} rows · {} lanes",
        format::count(snap.commits.len()),
        if snap.truncated {
            format!(" (truncated at {})", format::count(max))
        } else {
            String::new()
        },
        snap.branches.len(),
        format::count(remote_branches),
        format::plural(snap.remotes.len(), "remote", "remotes"),
        format::count(snap.tags.len()),
        snap.stashes.len(),
        snap.status.staged.len(),
        snap.status.unstaged.len(),
        snap.status.conflicted.len(),
        if snap.pending == gitvis_core::Pending::None {
            "none".to_owned()
        } else {
            snap.pending.label().to_lowercase()
        },
        format::count(snap.row_count()),
        snap.graph.width,
    )
}

fn default_selection(snap: &Snapshot) -> Selection {
    if snap.wip_row {
        return Selection::Wip;
    }
    snap.head
        .target()
        .filter(|h| snap.index_of.contains_key(h))
        .or_else(|| snap.commits.first().map(|c| c.id))
        .map(Selection::Commit)
        .unwrap_or(Selection::None)
}

fn file_exists(snap: &Snapshot, target: &DiffTarget) -> bool {
    use gitvis_core::ChangeKind;
    let st = &snap.status;
    match target {
        DiffTarget::Commit { .. } => true,
        DiffTarget::Unstaged { path } => st
            .unstaged
            .iter()
            .any(|e| e.path == *path && e.kind != ChangeKind::Untracked),
        DiffTarget::Untracked { path } => st
            .unstaged
            .iter()
            .any(|e| e.path == *path && e.kind == ChangeKind::Untracked),
        DiffTarget::Staged { path, .. } => st.staged.iter().any(|e| e.path == *path),
        DiffTarget::Conflicted { path } => st.conflicted.iter().any(|e| e.path == *path),
    }
}

/// Keep git's output short enough for a toast.
fn trim_output(message: &str) -> String {
    let lines: Vec<&str> = message
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("hint:"))
        .collect();
    let mut out: Vec<String> = lines
        .iter()
        .take(3)
        .map(|l| {
            if l.chars().count() > 140 {
                format!("{}…", l.chars().take(140).collect::<String>())
            } else {
                l.to_string()
            }
        })
        .collect();
    if lines.len() > 3 {
        out.push(format!("… {} more lines", lines.len() - 3));
    }
    out.join("\n")
}

impl eframe::App for GitVis {
    fn persist_egui_memory(&self) -> bool {
        std::env::var_os("GITVIS_SCRIPT").is_none()
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut events = Vec::new();
        while let Ok(ev) = self.worker.inbox.try_recv() {
            events.push(ev);
        }
        for ev in events {
            self.handle_event(ev);
        }
        self.tick_dirty(ctx);
        self.tick_checks(ctx);
        self.run_script(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let mut actions: Vec<Action> = Vec::new();
        self.active = self.active.min(self.repos.len().saturating_sub(1));
        self.shortcuts(&ctx, &mut actions);

        let title = match self.repos.get(self.active) {
            Some(r) => format!("{} — gitvis", r.name),
            None => "gitvis".to_owned(),
        };
        if self.window_title != title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }

        let chrome = theme::pal(ui).chrome;
        egui::Panel::left("rail")
            .resizable(false)
            .exact_size(views::rail::WIDTH)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(chrome))
            .show(ui, |ui| {
                views::rail::show(
                    ui,
                    self.repos.get(self.active),
                    &self.settings,
                    &mut actions,
                )
            });

        let undo = self.repos.get(self.active).and_then(|r| r.undo.clone());
        views::toolbar::show(
            ui,
            &mut self.repos,
            self.active,
            &self.settings,
            self.cloning.as_deref(),
            undo.as_ref(),
            &mut self.focus_search,
            &mut actions,
        );

        if self.settings.show_status_bar && !self.repos.is_empty() {
            views::statusbar::show(
                ui,
                self.repos.get(self.active),
                &self.settings,
                self.activity.last(),
                &mut actions,
            );
        }

        if self.settings.show_activity {
            // A new id on purpose: earlier builds let egui remember a
            // squashed height for "activity".
            let p = theme::pal(ui);
            egui::Panel::bottom("activity_log")
                .resizable(true)
                .default_size(280.0)
                .size_range(140.0..=760.0)
                .frame(
                    Frame::NONE
                        .fill(p.panel)
                        .stroke(egui::Stroke::new(1.0, p.border))
                        .inner_margin(Margin::symmetric(12, 8)),
                )
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    views::activity::show(
                        ui,
                        &mut self.activity,
                        self.repos.get(self.active),
                        &mut self.activity_tab,
                        &self.settings,
                        &mut actions,
                    );
                });
        }

        if self.repos.is_empty() {
            views::welcome::show(ui, &self.settings, &mut actions);
        } else {
            let settings = &self.settings;
            let dark = settings.dark;
            let panel_fill = ui.visuals().panel_fill;
            let style = ui.style().clone();
            let repo = &mut self.repos[self.active];

            if repo.page == Page::Graph
                || (repo.page == Page::GitHub && repo.github_remote.is_none())
            {
                egui::Panel::left("sidebar")
                    .resizable(true)
                    .default_size(250.0)
                    .size_range(170.0..=520.0)
                    .frame(Frame::NONE.fill(panel_fill))
                    .show(ui, |ui| {
                        // Panels shrink to their content; claim the whole width.
                        ui.set_min_width(ui.available_width());
                        views::sidebar::show(ui, repo, settings, &mut actions)
                    });
            }

            if repo.page == Page::GitHub && repo.github_remote.is_some() {
                egui::CentralPanel::default()
                    .frame(Frame::NONE.fill(panel_fill))
                    .show(ui, |ui| {
                        views::github::show(ui, repo, settings, &mut actions)
                    });
            } else if repo.page == Page::Security {
                egui::CentralPanel::default()
                    .frame(Frame::NONE.fill(panel_fill))
                    .show(ui, |ui| {
                        views::security::show(ui, repo, settings, &mut actions)
                    });
            } else if repo.page == Page::Insights {
                egui::CentralPanel::default()
                    .frame(Frame::NONE.fill(panel_fill))
                    .show(ui, |ui| {
                        views::insights::show(ui, repo, settings, &mut actions)
                    });
            } else {
                if repo.file.is_some() {
                    egui::Panel::right("diff")
                        .resizable(true)
                        .default_size(680.0)
                        .size_range(320.0..=1600.0)
                        .frame(Frame::side_top_panel(&style).inner_margin(Margin::symmetric(6, 4)))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            views::diff::show(ui, repo, settings, &mut actions)
                        });
                } else {
                    egui::Panel::right("details")
                        .resizable(true)
                        .default_size(420.0)
                        .size_range(280.0..=1000.0)
                        .frame(Frame::side_top_panel(&style).inner_margin(Margin::symmetric(10, 6)))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            views::details::show(ui, repo, settings, &mut actions)
                        });
                }

                egui::CentralPanel::default()
                    .frame(Frame::NONE.fill(views::graph::background(dark)))
                    .show(ui, |ui| {
                        views::graph::show(ui, repo, settings, &mut actions)
                    });
            }
        }

        if let Some(dialog) = self.dialog.as_mut() {
            let snap = self
                .repos
                .get(self.active)
                .and_then(|r| r.snapshot.as_ref());
            let keep =
                views::dialogs::show(&ctx, dialog, &mut self.dialog_fresh, snap, &mut actions);
            if !keep {
                self.dialog = None;
            }
        }

        views::toasts::show(&ctx, &mut self.toasts);
        views::graph::drag_overlay(&ctx);

        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        for p in dropped {
            actions.push(Action::OpenRepo(p));
        }

        for action in actions {
            self.apply(&ctx, action);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if std::env::var_os("GITVIS_SCRIPT").is_some() {
            // Scripted smoke tests never write the user's session.
            return;
        }
        self.settings.open_repos = self.repos.iter().map(|r| r.workdir.clone()).collect();
        self.settings.active = self.active;
        for i in 0..self.repos.len() {
            self.save_prefs(i);
        }
        eframe::set_value(storage, eframe::APP_KEY, &self.settings);
    }
}
