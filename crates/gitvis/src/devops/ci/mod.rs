//! CI control on the Actions page: dispatch with inputs, debug re-runs,
//! bulk cancel, artifacts, caches, per-workflow history and flaky jobs,
//! comparing with the last green run, workflow validation, local runs (act).
//!
//! The Runs tab is `views/github.rs` (it calls the pieces in [`runs`]);
//! every other tab is drawn by [`show_tab`]. The controller is in
//! `control.rs`; each tab's view has its own file.

mod artifacts;
mod caches;
mod compare;
mod control;
mod local;
pub mod runs;
mod validate;
mod widgets;
mod workflows;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use gitvis_core::devops::ci::LintRun;
use gitvis_core::devops::ci::act::ActJob;
use gitvis_core::devops::ci::compare::LogDiff;
use gitvis_core::devops::ci::history::JobStats;
use gitvis_core::devops::ci::lint::Severity;
use gitvis_core::devops::ci::model::{Artifact, Between, Cache, CacheUsage};
use gitvis_core::devops::ci::workflow::{DispatchInput, WorkflowFile};
use gitvis_core::devops::gh::Request;
use gitvis_core::devops::tools::Installed;
use gitvis_core::github::{Job, WorkflowRun};

use crate::devops::console::{Console, ConsoleRun};
use crate::devops::ctx::Ctx;
use crate::state::{Action, RepoState, Settings};
use crate::theme::Tone;

pub use control::{apply, on_msg};

/// Tabs of the Actions page. `Runs` is the run list in `views/github.rs`;
/// every other tab is drawn by [`show_tab`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Runs,
    Workflows,
    Artifacts,
    Caches,
    Validate,
    Local,
}

impl Tab {
    pub const ALL: [Tab; 6] = [
        Tab::Runs,
        Tab::Workflows,
        Tab::Artifacts,
        Tab::Caches,
        Tab::Validate,
        Tab::Local,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Runs => "Runs",
            Tab::Workflows => "Workflows",
            Tab::Artifacts => "Artifacts",
            Tab::Caches => "Caches",
            Tab::Validate => "Validate",
            Tab::Local => "Run locally",
        }
    }

    /// Names accepted by the `actions-tab` script command.
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "runs" => Tab::Runs,
            "workflows" | "workflow" => Tab::Workflows,
            "artifacts" | "artifact" => Tab::Artifacts,
            "caches" | "cache" => Tab::Caches,
            "validate" | "lint" => Tab::Validate,
            "local" | "act" | "run-locally" => Tab::Local,
            _ => return None,
        })
    }
}

/// Something loaded in the background: a loading flag, when it last
/// finished, and the result.
pub struct Load<T> {
    pub loading: bool,
    pub at: Option<Instant>,
    pub data: Option<Result<T, String>>,
}

impl<T> Default for Load<T> {
    fn default() -> Self {
        Self {
            loading: false,
            at: None,
            data: None,
        }
    }
}

impl<T> Load<T> {
    /// Mark as loading; false when it already is (so clicks do not stack).
    pub fn start(&mut self) -> bool {
        if self.loading {
            return false;
        }
        self.loading = true;
        true
    }

    pub fn finish(&mut self, r: Result<T, String>) {
        self.loading = false;
        self.at = Some(Instant::now());
        self.data = Some(r);
    }

    pub fn ok(&self) -> Option<&T> {
        self.data.as_ref().and_then(|d| d.as_ref().ok())
    }

    pub fn err(&self) -> Option<&str> {
        self.data
            .as_ref()
            .and_then(|d| d.as_ref().err())
            .map(String::as_str)
    }

    /// Never loaded and not loading.
    pub fn unloaded(&self) -> bool {
        !self.loading && self.data.is_none()
    }
}

/// How a click in the run list picks runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickMode {
    /// Ctrl-click or the checkbox.
    Toggle,
    /// Shift-click: from the last picked run to this one.
    Range,
}

/// Workflow files of the working tree and the refs a dispatch can use.
#[derive(Clone, Debug, Default)]
pub struct LocalInfo {
    pub files: Vec<WorkflowFile>,
    /// The GitHub remote's default branch (`origin/HEAD`), when known.
    pub default_branch: Option<String>,
}

/// One workflow's run history (one page of up to 100 runs).
#[derive(Clone, Debug, Default)]
pub struct History {
    pub runs: Vec<WorkflowRun>,
    pub total: u64,
}

/// The per-job analysis of a workflow's recent runs.
#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub jobs: Vec<JobStats>,
    pub runs: usize,
    pub attempts: usize,
    pub calls: usize,
    /// Runs whose jobs could not be fetched, with the error.
    pub failed: Vec<(u64, String)>,
}

/// The dispatch form of one workflow.
#[derive(Clone, Debug, Default)]
pub struct DispatchForm {
    pub path: String,
    pub name: String,
    /// GitHub's id, when the workflow is on GitHub (else the file name is
    /// used in the request).
    pub workflow_id: Option<u64>,
    pub inputs: Vec<DispatchInput>,
    pub values: BTreeMap<String, String>,
    pub git_ref: String,
    pub sending: bool,
    pub error: Option<String>,
    /// The last dispatch that went through: `(ref, when)`.
    pub sent: Option<(String, Instant)>,
}

/// The comparison of a failed run with the last green one.
pub struct CompareState {
    pub bad: u64,
    /// Start comparing as soon as the green run is known (script driver).
    pub auto_start: bool,
    /// Looking the green run up through the API.
    pub finding: bool,
    pub good: Option<WorkflowRun>,
    /// Why there is no green run to compare with.
    pub missing: Option<String>,
    pub report: Load<Box<CompareReport>>,
    pub job: usize,
    /// API calls spent finding the green run.
    pub lookup_calls: usize,
    /// Show every commit / file instead of the first few.
    pub all_commits: bool,
    pub all_files: bool,
}

/// What the comparison found.
#[derive(Clone, Debug)]
pub struct CompareReport {
    pub good: WorkflowRun,
    pub bad: WorkflowRun,
    pub between: Result<Between, String>,
    pub jobs: Vec<JobDiff>,
    /// Failing jobs left out to stay within the cap.
    pub skipped: Vec<String>,
    pub calls: usize,
    pub elapsed_ms: u64,
}

/// One failing job compared with the same-named job of the green run.
#[derive(Clone, Debug)]
pub struct JobDiff {
    pub name: String,
    pub bad: Job,
    pub good: Option<Job>,
    pub diff: Result<LogDiff, String>,
}

/// The artifact list.
#[derive(Clone, Debug, Default)]
pub struct ArtifactList {
    pub items: Vec<Artifact>,
    pub total: u64,
}

/// Which artifacts the list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ArtifactScope {
    #[default]
    All,
    /// Expiring within three days.
    Expiring,
    Expired,
}

/// Artifacts expiring within this many seconds are highlighted.
pub const EXPIRES_SOON: i64 = 3 * 86_400;

/// How the cache list is sorted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CacheSort {
    #[default]
    LastUsed,
    Size,
    Created,
}

impl CacheSort {
    pub fn api(self) -> &'static str {
        match self {
            CacheSort::LastUsed => "last_accessed_at",
            CacheSort::Size => "size_in_bytes",
            CacheSort::Created => "created_at",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CacheSort::LastUsed => "Last used",
            CacheSort::Size => "Size",
            CacheSort::Created => "Created",
        }
    }
}

/// Caches as listed, plus the usage numbers.
#[derive(Clone, Debug, Default)]
pub struct CacheList {
    pub usage: Option<CacheUsage>,
    pub items: Vec<Cache>,
    pub total: u64,
    pub sort: CacheSort,
}

/// Which cache list is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CacheView {
    #[default]
    Entries,
    Refs,
}

/// What is selected on the Caches tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CacheSel {
    Entry(u64),
    Ref(String),
}

/// Progress reads of a watched job: soon after a step changes, then less
/// often while one step keeps running (a 40-minute build step costs one
/// request a minute).
pub const POLL_MIN: Duration = Duration::from_secs(10);
pub const POLL_MAX: Duration = Duration::from_secs(60);
/// Below this many API requests left, watching pauses.
pub const POLL_FLOOR: u32 = 100;
/// Tries to download a finished job's log: GitHub publishes it a few
/// seconds after the job ends and answers HTTP 404 until then.
pub const LOG_TRIES: u32 = 6;
pub const LOG_RETRY: Duration = Duration::from_secs(5);

/// A running or queued job shown in the log viewer. Its progress is read
/// again while the viewer shows it, and its log is downloaded once it
/// finishes (GitHub publishes a job's log only then).
#[derive(Clone, Debug)]
pub struct JobWatch {
    pub run: u64,
    pub job: u64,
    /// When the last progress read finished (or started, while in flight).
    pub polled: Instant,
    pub in_flight: bool,
    /// The wait before the next read.
    pub interval: Duration,
    /// Status and step progress at the last read, to see whether it moved.
    pub progress: String,
    /// The last read failed.
    pub error: Option<String>,
}

/// GitHub has no log for this job: it is running or queued (the log is
/// published when it finishes), or it never ran. Asking would fail with
/// HTTP 404, so the log viewer shows its progress or a note instead.
pub fn no_log_yet(job: &Job) -> bool {
    job.outcome().is_running() || job.never_ran()
}

/// What a progress read compares: the job's status and how far its steps got.
pub fn progress_key(job: &Job) -> String {
    format!(
        "{}/{}/{}/{}",
        job.status,
        job.conclusion.as_deref().unwrap_or(""),
        job.steps_done(),
        job.current_step().map(|s| s.number).unwrap_or(0)
    )
}

/// `act` and Docker on this machine.
#[derive(Clone, Debug, Default)]
pub struct Tools {
    pub act: Option<Installed>,
    pub docker: Option<Installed>,
    /// `docker info` answered (the daemon runs); `None` without Docker.
    pub docker_running: Option<bool>,
}

/// The Run locally tab.
#[derive(Default)]
pub struct ActState {
    pub tools: Load<Tools>,
    pub list: Load<Vec<ActJob>>,
    pub workflow: String,
    pub job: String,
    pub event: String,
    pub inputs: BTreeMap<String, String>,
    pub image: String,
    pub secret_file: Option<PathBuf>,
    /// Console runs started from this tab, newest last.
    pub runs: Vec<u64>,
    pub shown: Option<u64>,
}

#[derive(Default)]
pub struct State {
    pub tab: Tab,
    // Runs tab.
    /// Runs picked for a bulk action.
    pub picked: BTreeSet<u64>,
    pub anchor: Option<u64>,
    pub run_artifacts: HashMap<u64, Load<Vec<Artifact>>>,
    /// Runs with a change in flight, and what it is.
    pub busy_runs: HashMap<u64, String>,
    pub busy_jobs: HashSet<u64>,
    /// The running job in the log viewer, if any.
    pub watch: Option<JobWatch>,
    pub compare: Option<CompareState>,
    // Workflows tab.
    pub local: Load<LocalInfo>,
    pub wf_selected: Option<String>,
    pub wf_filter: String,
    /// The branch filter as typed, and as applied to the history.
    pub wf_branch: String,
    pub wf_branch_applied: String,
    pub history: HashMap<(u64, String), Load<History>>,
    pub analysis: HashMap<(u64, String), Load<Analysis>>,
    pub dispatch: Option<DispatchForm>,
    pub wf_busy: HashSet<u64>,
    // Artifacts tab.
    pub artifacts: Load<ArtifactList>,
    pub artifact_filter: String,
    pub artifact_scope: ArtifactScope,
    pub artifact_selected: Option<u64>,
    pub artifact_busy: HashSet<u64>,
    /// Console runs of downloads and their folders, by artifact id.
    pub downloads: HashMap<u64, (u64, PathBuf)>,
    // Caches tab.
    pub caches: Load<CacheList>,
    pub cache_sort: CacheSort,
    pub cache_filter: String,
    pub cache_view: CacheView,
    pub cache_selected: Option<CacheSel>,
    pub pr_states: Load<HashMap<u64, String>>,
    pub cache_busy: bool,
    // Validate tab.
    pub lint: Load<LintRun>,
    /// Files whose stamp changed since the last check.
    pub lint_changed: Vec<String>,
    pub lint_polled: Option<Instant>,
    pub lint_file: Option<String>,
    pub lint_filter: String,
    pub lint_min: Option<Severity>,
    // Run locally tab.
    pub act: ActState,
    /// A script command waiting for data (the workflow files).
    pub pending: Option<ScriptCmd>,
    /// A section to scroll to on the next frame (`jobs`, `flaky`,
    /// `branches`, `runs`), for the script driver.
    pub scroll_to: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Act {
    SetTab(Tab),
    /// A dispatched run should be visible by now: refresh runs and history.
    DispatchSettled {
        workflow_id: Option<u64>,
    },
    /// Re-fetch runs changed a moment ago (re-run, cancel, …).
    RefreshRuns(Vec<u64>),
    /// The refresh button of the page header on a tab.
    Refresh(Tab),
    // Runs tab.
    Pick {
        run: u64,
        mode: PickMode,
    },
    PickRuns(Vec<u64>),
    ClearPicks,
    AskCancelRuns(Vec<u64>),
    AskRerunFailedRuns(Vec<u64>),
    AskCancelBranch(String),
    AskRerun {
        run: u64,
        failed_only: bool,
        debug: bool,
    },
    AskRerunJob {
        run: u64,
        job: u64,
        name: String,
        debug: bool,
    },
    AskCancel {
        run: u64,
        force: bool,
    },
    AskDeleteLogs(u64),
    AskDeleteRun(u64),
    LoadRunArtifacts {
        run: u64,
        force: bool,
    },
    /// Show a job in the log viewer without downloading its log: it has
    /// none yet (running, queued) or never will (never started), and
    /// asking GitHub would only fail with HTTP 404.
    OpenJob {
        run: u64,
        job: u64,
    },
    /// Read the progress of a running job the log viewer shows (the view
    /// asks while it is on screen).
    PollJob {
        run: u64,
        job: u64,
    },
    /// Download the log of a watched job that has finished.
    FetchLog {
        run: u64,
        job: u64,
        attempt: u32,
    },
    /// The confirmed change.
    Send(Change),
    // Compare.
    CompareOpen(u64),
    CompareStart,
    CompareClose,
    CompareJob(usize),
    /// Show every commit (`false`: every file) of the comparison.
    CompareExpand {
        commits: bool,
    },
    // Workflows tab.
    SelectWorkflow(Option<String>),
    LoadHistory {
        workflow: u64,
        force: bool,
    },
    ApplyBranch,
    AnalyseJobs {
        workflow: u64,
    },
    AskToggleWorkflow {
        id: u64,
        enable: bool,
    },
    OpenDispatch(String),
    CloseDispatch,
    AskDispatch,
    // Artifacts tab.
    SelectArtifact(Option<u64>),
    /// Pick a folder (or use the given one) and run `gh run download`.
    DownloadArtifact {
        id: u64,
        dir: Option<PathBuf>,
    },
    AskDeleteArtifact(u64),
    // Caches tab.
    SetCacheSort(CacheSort),
    SetCacheView(CacheView),
    SelectCache(Option<CacheSel>),
    AskDeleteCache(u64),
    AskDeleteCacheKey {
        key: String,
        git_ref: String,
    },
    AskDeleteRef(String),
    CheckPrStates,
    AskDeleteClosedPrCaches,
    // Validate tab.
    Validate,
    SelectLintFile(Option<String>),
    // Run locally tab.
    CheckTools,
    ListActJobs,
    PickSecretFile,
    ClearSecretFile,
    RunLocally,
    ShowActRun(Option<u64>),
    // Script driver.
    Script(ScriptCmd),
}

/// A confirmed change, carrying the exact requests shown in the dialog.
#[derive(Clone, Debug)]
pub enum Change {
    /// Run-level writes (re-runs, cancels, deletes). With `deleted` the
    /// runs are removed from the list afterwards; else they are refreshed.
    Runs {
        label: String,
        requests: Vec<(u64, Request)>,
        deleted: bool,
    },
    Job {
        run: u64,
        job: u64,
        request: Request,
    },
    Workflow {
        id: u64,
        enable: bool,
        request: Request,
    },
    Dispatch {
        request: Request,
        workflow_id: Option<u64>,
        git_ref: String,
    },
    Artifacts {
        requests: Vec<(u64, Request)>,
    },
    /// Cache deletions, each request with the cache ids it removes.
    Caches {
        label: String,
        requests: Vec<(Request, Vec<u64>)>,
    },
}

/// Script commands that need the repository's state.
#[derive(Clone, Debug)]
pub enum ScriptCmd {
    SelectRuns(usize),
    Workflow(usize),
    Dispatch(usize),
    Compare,
    /// Open the comparison without starting it (shows the plan).
    ComparePlan,
    AnalyseJobs,
    Act,
    Artifact(usize),
    Cache(usize),
    CacheRefs,
    LintFile(usize),
    AskCancelPicked,
    AskRerunFailedPicked,
    AskDeleteRun,
    AskRerunDebug,
    /// Re-run the N-th job of the selected run (stops at the confirmation).
    AskRerunJob(usize),
    /// Show the N-th job of the selected run the way its Log button does
    /// (a running job shows its progress; the log is not requested).
    ShowJob(usize),
    /// Point the Run locally form at the N-th job.
    ActJob(usize),
    /// Scroll the workflow detail to a section.
    ScrollTo(String),
    /// The dispatch form's "Run workflow…" (stops at the confirmation).
    AskDispatch,
    /// Delete the selected artifact or cache / ref, or disable the selected
    /// workflow (stops at the confirmation).
    AskDelete,
    DownloadArtifact(usize, PathBuf),
}

pub enum Msg {
    /// Run-level writes finished: `ok` runs to refresh, `deleted` to drop.
    RunsChanged {
        ok: Vec<u64>,
        deleted: Vec<u64>,
        failed: Vec<(u64, String)>,
    },
    /// Fresh copies of runs after a change.
    RunsRefreshed(Vec<Result<WorkflowRun, String>>),
    JobChanged {
        run: u64,
        job: u64,
        ok: bool,
    },
    RunArtifacts {
        run: u64,
        result: Result<Vec<Artifact>, String>,
    },
    /// A watched job's progress: the run's jobs (or only that job, for a
    /// run with more than 100), and the run once the job has finished.
    JobPolled {
        run: u64,
        job: u64,
        single: bool,
        jobs: Result<Vec<Job>, String>,
        run_data: Option<Result<Box<WorkflowRun>, String>>,
    },
    /// The log of a watched job that finished.
    JobLog {
        run: u64,
        job: u64,
        attempt: u32,
        result: Result<Box<gitvis_core::joblog::JobLog>, String>,
    },
    GreenFound {
        bad: u64,
        result: Result<Option<Box<WorkflowRun>>, String>,
    },
    Compared {
        bad: u64,
        result: Result<Box<CompareReport>, String>,
    },
    Local(Result<LocalInfo, String>),
    History {
        key: (u64, String),
        result: Result<History, String>,
    },
    Analysis {
        key: (u64, String),
        result: Result<Analysis, String>,
    },
    WorkflowToggled {
        id: u64,
        enable: bool,
        ok: bool,
    },
    Dispatched {
        ok: Result<(), String>,
        workflow_id: Option<u64>,
        git_ref: String,
    },
    Artifacts(Result<ArtifactList, String>),
    ArtifactsDeleted {
        ok: Vec<u64>,
        failed: Vec<(u64, String)>,
    },
    Caches(Result<CacheList, String>),
    CachesDeleted {
        ok: Vec<u64>,
        failed: usize,
    },
    PrStates(Result<HashMap<u64, String>, String>),
    Linted(Box<LintRun>),
    Tools(Box<Tools>),
    ActList(Result<Vec<ActJob>, String>),
}

impl Msg {
    /// One line for the activity log (`Err` when the work failed).
    pub fn summary(&self) -> Result<String, String> {
        use gitvis_core::devops::plural_bytes;
        let plural = crate::format::plural;
        let count = crate::format::count;
        match self {
            Msg::RunsChanged {
                ok,
                deleted,
                failed,
            } => {
                if failed.is_empty() {
                    Ok(format!(
                        "{} changed{}",
                        plural(ok.len() + deleted.len(), "run", "runs"),
                        if deleted.is_empty() {
                            String::new()
                        } else {
                            format!(", {} deleted", deleted.len())
                        }
                    ))
                } else {
                    Err(format!(
                        "{} of {} failed: {}",
                        failed.len(),
                        failed.len() + ok.len() + deleted.len(),
                        failed
                            .iter()
                            .map(|(id, e)| format!("run {id}: {e}"))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ))
                }
            }
            Msg::RunsRefreshed(rs) => Ok(format!(
                "{} refreshed",
                plural(rs.iter().filter(|r| r.is_ok()).count(), "run", "runs")
            )),
            Msg::JobChanged { job, ok, .. } => {
                if *ok {
                    Ok(format!("job {job} re-run"))
                } else {
                    Err(format!("job {job} could not be re-run"))
                }
            }
            Msg::RunArtifacts { run, result } => match result {
                Ok(a) => Ok(format!(
                    "{} of run {run}, {}",
                    plural(a.len(), "artifact", "artifacts"),
                    plural_bytes(a.iter().map(|x| x.size_in_bytes as usize).sum())
                )),
                Err(e) => Err(e.clone()),
            },
            Msg::JobPolled { job, jobs, .. } => match jobs {
                Ok(list) => Ok(match list.iter().find(|j| j.id == *job) {
                    Some(j) if j.outcome().is_running() => match j.current_step() {
                        Some(s) => format!(
                            "{}: step {} of {} ({})",
                            j.outcome().label(),
                            s.number,
                            j.steps.len(),
                            s.name
                        ),
                        None => j.outcome().label().to_owned(),
                    },
                    Some(j) => format!("finished: {}", j.outcome().label()),
                    None => format!("job {job} is no longer listed"),
                }),
                Err(e) => Err(e.clone()),
            },
            Msg::JobLog {
                attempt, result, ..
            } => match result {
                Ok(log) => Ok(format!(
                    "{} · {} errors · {} warnings · {}",
                    plural(log.lines.len(), "line", "lines"),
                    log.errors.len(),
                    log.warnings.len(),
                    plural_bytes(log.bytes)
                )),
                // GitHub publishes the log a few seconds after the job ends.
                Err(e) if gitvis_core::github::is_not_found(e) && attempt + 1 < LOG_TRIES => {
                    Ok(format!(
                        "not published yet, trying again in {} s",
                        LOG_RETRY.as_secs()
                    ))
                }
                Err(e) => Err(e.clone()),
            },
            Msg::GreenFound { result, .. } => match result {
                Ok(Some(r)) => Ok(format!("last green run: #{} ({})", r.run_number, r.id)),
                Ok(None) => Ok("no earlier green run on this branch".to_owned()),
                Err(e) => Err(e.clone()),
            },
            Msg::Compared { result, .. } => match result {
                Ok(r) => Ok(format!(
                    "compared #{} with #{}: {} with log diffs, {} API calls",
                    r.bad.run_number,
                    r.good.run_number,
                    plural(r.jobs.len(), "failing job", "failing jobs"),
                    r.calls
                )),
                Err(e) => Err(e.clone()),
            },
            Msg::Local(r) => match r {
                Ok(l) => Ok(format!(
                    "{} in .github/workflows, {} dispatchable",
                    plural(l.files.len(), "workflow file", "workflow files"),
                    l.files.iter().filter(|f| f.dispatch.is_some()).count()
                )),
                Err(e) => Err(e.clone()),
            },
            Msg::History { result, .. } => match result {
                Ok(h) => Ok(format!(
                    "{} of {} loaded",
                    plural(h.runs.len(), "run", "runs"),
                    count(h.total as usize)
                )),
                Err(e) => Err(e.clone()),
            },
            Msg::Analysis { result, .. } => match result {
                Ok(a) => Ok(format!(
                    "{} across {} ({} attempts, {} API calls)",
                    plural(a.jobs.len(), "job", "jobs"),
                    plural(a.runs, "run", "runs"),
                    a.attempts,
                    a.calls
                )),
                Err(e) => Err(e.clone()),
            },
            Msg::WorkflowToggled { id, enable, ok } => {
                let what = if *enable { "enabled" } else { "disabled" };
                if *ok {
                    Ok(format!("workflow {id} {what}"))
                } else {
                    Err(format!("workflow {id} not {what}"))
                }
            }
            Msg::Dispatched { ok, git_ref, .. } => match ok {
                Ok(()) => Ok(format!("dispatched on {git_ref}")),
                Err(e) => Err(e.clone()),
            },
            Msg::Artifacts(r) => match r {
                Ok(a) => Ok(format!(
                    "{} of {}, {}",
                    plural(a.items.len(), "artifact", "artifacts"),
                    count(a.total as usize),
                    plural_bytes(a.items.iter().map(|x| x.size_in_bytes as usize).sum())
                )),
                Err(e) => Err(e.clone()),
            },
            Msg::ArtifactsDeleted { ok, failed } => {
                if failed.is_empty() {
                    Ok(format!(
                        "{} deleted",
                        plural(ok.len(), "artifact", "artifacts")
                    ))
                } else {
                    Err(format!("{} could not be deleted", failed.len()))
                }
            }
            Msg::Caches(r) => match r {
                Ok(c) => Ok(format!(
                    "{} of {}, {}",
                    plural(c.items.len(), "cache", "caches"),
                    count(c.total as usize),
                    plural_bytes(c.items.iter().map(|x| x.size_in_bytes as usize).sum())
                )),
                Err(e) => Err(e.clone()),
            },
            Msg::CachesDeleted { ok, failed } => {
                if *failed == 0 {
                    Ok(format!("{} deleted", plural(ok.len(), "cache", "caches")))
                } else {
                    Err(format!("{failed} could not be deleted"))
                }
            }
            Msg::PrStates(r) => match r {
                Ok(m) => Ok(format!(
                    "{}: {} open, {} closed, {} merged",
                    plural(m.len(), "pull request", "pull requests"),
                    m.values().filter(|s| *s == "OPEN").count(),
                    m.values().filter(|s| *s == "CLOSED").count(),
                    m.values().filter(|s| *s == "MERGED").count()
                )),
                Err(e) => Err(e.clone()),
            },
            Msg::Linted(run) => {
                let n = |s: Severity| {
                    run.files
                        .iter()
                        .flat_map(|f| &f.findings)
                        .filter(|x| x.severity == s)
                        .count()
                };
                Ok(format!(
                    "{} checked ({} again, {} unchanged) in {} ms: {} errors, {} warnings, {} notes",
                    plural(run.files.len(), "workflow file", "workflow files"),
                    run.linted,
                    run.reused,
                    run.elapsed_ms,
                    n(Severity::Error),
                    n(Severity::Warning),
                    n(Severity::Info)
                ))
            }
            Msg::Tools(t) => Ok(format!(
                "act {}, Docker {}",
                t.act
                    .as_ref()
                    .map(|a| a.version.as_str())
                    .unwrap_or("not installed"),
                match (&t.docker, t.docker_running) {
                    (None, _) => "not installed".to_owned(),
                    (Some(d), Some(false)) => format!("{} (not running)", d.version),
                    (Some(d), _) => d.version.clone(),
                }
            )),
            Msg::ActList(r) => match r {
                Ok(j) => Ok(plural(j.len(), "job", "jobs")),
                Err(e) => Err(e.clone()),
            },
        }
    }
}

/// Persisted per repository.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub cache_sort: CacheSort,
    /// Image for `ubuntu-latest` in `act` runs.
    pub act_image: String,
    /// A dotenv file passed to `act --secret-file` (only the path is kept).
    pub act_secret_file: Option<String>,
}

impl State {
    pub fn prefs(&self) -> Prefs {
        Prefs {
            cache_sort: self.cache_sort,
            act_image: self.act.image.clone(),
            act_secret_file: self
                .act
                .secret_file
                .as_ref()
                .map(|p| p.display().to_string()),
        }
    }

    pub fn apply_prefs(&mut self, p: &Prefs) {
        self.cache_sort = p.cache_sort;
        self.act.image = p.act_image.clone();
        self.act.secret_file = p.act_secret_file.as_ref().map(PathBuf::from);
    }

    /// Workflow files with errors in the last validation.
    pub fn lint_error_files(&self) -> usize {
        self.lint
            .ok()
            .map(|r| {
                r.files
                    .iter()
                    .filter(|f| f.findings.iter().any(|x| x.severity == Severity::Error))
                    .count()
            })
            .unwrap_or(0)
    }
}

/// A console run started by this area (or any other) ended.
pub fn on_console_done(cx: &mut Ctx, repo: &mut RepoState, run: &ConsoleRun) {
    control::console_done(cx, repo, run);
}

/// The Actions page was opened.
pub fn on_page_shown(cx: &mut Ctx, repo: &mut RepoState) {
    let tab = repo.devops.ci.tab;
    control::tab_shown(cx, repo, tab);
}

/// `GITVIS_SCRIPT` commands this area understands: `actions-tab NAME`,
/// `workflow N`, `dispatch N`, `select-runs N`, `compare`, `analyse-jobs`,
/// `validate`, `act`, plus `compare-plan`, `act-job N`, `ci-scroll SECTION`, `artifact N`,
/// `cache N`, `cache-refs`,
/// `lint-file N`, `cancel-picked`, `rerun-picked`, `delete-run`,
/// `rerun-debug`, `rerun-job N`, `ask-dispatch`, `ask-delete`, `ask-delete-closed` (these
/// stop at their confirmation), `check-prs`, `download-artifact N DIR` and
/// `show-job N` (the N-th job's Log button: a running job shows its
/// progress instead of requesting a log GitHub does not have yet).
pub fn script(cmd: &str, arg: &str) -> Option<Vec<Action>> {
    use crate::state::Page;
    let page = Action::ShowPage(Page::GitHub);
    let n = || arg.trim().parse::<usize>().ok();
    let tab = |t: Tab| -> Action { Act::SetTab(t).into() };
    let s = |c: ScriptCmd| -> Action { Act::Script(c).into() };
    Some(match cmd {
        "actions-tab" => vec![page, tab(Tab::from_name(arg)?)],
        "workflow" => vec![page, tab(Tab::Workflows), s(ScriptCmd::Workflow(n()?))],
        "dispatch" => vec![page, tab(Tab::Workflows), s(ScriptCmd::Dispatch(n()?))],
        "select-runs" => vec![page, tab(Tab::Runs), s(ScriptCmd::SelectRuns(n()?))],
        // `compare A B` is the history area's.
        "compare" if arg.trim().is_empty() => vec![page, s(ScriptCmd::Compare)],
        "compare-plan" => vec![page, s(ScriptCmd::ComparePlan)],
        "analyse-jobs" | "analyze-jobs" => vec![page, s(ScriptCmd::AnalyseJobs)],
        "validate" => vec![page, tab(Tab::Validate), Act::Validate.into()],
        "act" => vec![page, tab(Tab::Local), s(ScriptCmd::Act)],
        "act-job" => vec![page, tab(Tab::Local), s(ScriptCmd::ActJob(n()?))],
        "ci-scroll" => vec![s(ScriptCmd::ScrollTo(arg.trim().to_owned()))],
        "artifact" => vec![page, tab(Tab::Artifacts), s(ScriptCmd::Artifact(n()?))],
        "cache" => vec![page, tab(Tab::Caches), s(ScriptCmd::Cache(n()?))],
        "cache-refs" => vec![page, tab(Tab::Caches), s(ScriptCmd::CacheRefs)],
        "lint-file" => vec![page, tab(Tab::Validate), s(ScriptCmd::LintFile(n()?))],
        "cancel-picked" => vec![page, s(ScriptCmd::AskCancelPicked)],
        "rerun-picked" => vec![page, s(ScriptCmd::AskRerunFailedPicked)],
        "delete-run" => vec![page, s(ScriptCmd::AskDeleteRun)],
        "rerun-debug" => vec![page, s(ScriptCmd::AskRerunDebug)],
        "rerun-job" => vec![page, s(ScriptCmd::AskRerunJob(n()?))],
        "show-job" => vec![page, tab(Tab::Runs), s(ScriptCmd::ShowJob(n()?))],
        "ask-dispatch" => vec![page, s(ScriptCmd::AskDispatch)],
        "ask-delete" => vec![page, s(ScriptCmd::AskDelete)],
        "check-prs" => vec![page, tab(Tab::Caches), Act::CheckPrStates.into()],
        "ask-delete-closed" => vec![page, tab(Tab::Caches), Act::AskDeleteClosedPrCaches.into()],
        "download-artifact" => {
            let (i, dir) = arg.trim().split_once(' ')?;
            vec![
                page,
                tab(Tab::Artifacts),
                s(ScriptCmd::DownloadArtifact(
                    i.parse().ok()?,
                    PathBuf::from(dir.trim()),
                )),
            ]
        }
        _ => return None,
    })
}

/// Every tab but `Runs`.
pub fn show_tab(
    ui: &mut egui::Ui,
    repo: &mut RepoState,
    tab: Tab,
    console: &Console,
    settings: &Settings,
    actions: &mut Vec<Action>,
) {
    match tab {
        Tab::Runs => {}
        Tab::Workflows => workflows::show(ui, repo, settings, actions),
        Tab::Artifacts => artifacts::show(ui, repo, console, actions),
        Tab::Caches => caches::show(ui, repo, actions),
        Tab::Validate => validate::show(ui, repo, actions),
        Tab::Local => local::show(ui, repo, console, actions),
    }
}

/// The comparison view, drawn by the Runs tab in place of the run detail
/// when a comparison of the selected run is open. Returns false (drawing
/// nothing) otherwise.
pub fn show_compare(ui: &mut egui::Ui, repo: &mut RepoState, actions: &mut Vec<Action>) -> bool {
    compare::show(ui, repo, actions)
}

/// A dot on the rail entry and a line for its tooltip, when this area
/// needs attention: workflow files with errors after a check, local `act`
/// runs going. `None` when nothing does.
pub fn rail_badge(repo: &RepoState, console: &Console) -> Option<(Tone, String)> {
    let ci = &repo.devops.ci;
    let running_act = ci
        .act
        .runs
        .iter()
        .filter(|id| console.get(**id).is_some_and(|r| r.running()))
        .count();
    let errors = ci.lint_error_files();
    let act_line = (running_act > 0).then(|| {
        format!(
            "{} running locally",
            crate::format::plural(running_act, "act job", "act jobs")
        )
    });
    if errors > 0 {
        let mut line = if errors == 1 {
            "1 workflow file has errors".to_owned()
        } else {
            format!("{errors} workflow files have errors")
        };
        if let Some(a) = act_line {
            line.push('\n');
            line.push_str(&a);
        }
        return Some((Tone::Warning, line));
    }
    act_line.map(|l| (Tone::Accent, l))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_names() {
        for t in Tab::ALL {
            let name = match t {
                Tab::Local => "local",
                other => other.label(),
            };
            assert_eq!(Tab::from_name(name), Some(t));
        }
        assert_eq!(Tab::from_name("nope"), None);
    }

    #[test]
    fn script_commands() {
        assert_eq!(script("actions-tab", "caches").map(|a| a.len()), Some(2));
        assert_eq!(script("workflow", "2").map(|a| a.len()), Some(3));
        assert!(script("workflow", "x").is_none());
        assert!(script("select-runs", "3").is_some());
        assert!(script("compare", "").is_some());
        assert!(script("analyse-jobs", "").is_some());
        assert!(script("validate", "").is_some());
        assert!(script("act", "").is_some());
        assert!(script("dispatch", "0").is_some());
        assert!(script("download-artifact", "0 C:/tmp/x").is_some());
        assert!(script("unknown", "").is_none());
    }
}
