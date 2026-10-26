//! GitLab and Azure DevOps: merge / pull requests and pipelines for
//! repositories whose remote is not on GitHub. Drawn on the Review and
//! Actions pages when `RepoState::forge_remote` is set.
//!
//! GitLab goes through `glab api` (or, for a public project without a
//! sign-in, unauthenticated `curl`, read only); Azure DevOps through `az`.
//! The CLI is checked first (installed, extension, signed in); every
//! missing piece shows the exact command that fixes it.

mod gate;
mod log;
mod pipelines;
mod review;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use gitvis_core::devops::forge::azure::{self, Target, Vote};
use gitvis_core::devops::forge::cli::{self, Call};
use gitvis_core::devops::forge::gitlab::{self, GlRequest};
use gitvis_core::devops::forge::model::{
    Approvals, ChangeRequest, FileChange, Job, Pipeline, Thread,
};
use gitvis_core::devops::forge::tools::{self as ftools, ToolState};
use gitvis_core::devops::forge::trace::{self, ForgeLog};
use gitvis_core::devops::forge::{ForgeKind, ForgeRemote};
use gitvis_core::devops::tools as devtools;

use crate::devops::console::{Console, ConsoleRun};
use crate::devops::ctx::{Ctx, TaskLog};
use crate::format;
use crate::state::Page;
use crate::state::{Action, RepoState, Settings, ToastKind};

/// How long one CLI call may take.
const TIMEOUT: Duration = Duration::from_secs(90);

/// Data loaded on demand, with when and how it went.
pub struct Loaded<T> {
    pub data: Option<T>,
    pub error: Option<String>,
    pub loading: bool,
    /// Epoch seconds of the last answer.
    pub at: Option<i64>,
}

impl<T> Default for Loaded<T> {
    fn default() -> Self {
        Self {
            data: None,
            error: None,
            loading: false,
            at: None,
        }
    }
}

impl<T> Loaded<T> {
    fn done(&mut self, r: Result<T, String>) {
        self.loading = false;
        self.at = Some(format::now_secs());
        match r {
            Ok(d) => {
                self.data = Some(d);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }
}

/// Everything loaded for one merge / pull request.
#[derive(Default)]
pub struct Detail {
    pub loading: bool,
    pub at: Option<i64>,
    pub cr: Option<Result<ChangeRequest, String>>,
    pub approvals: Option<Result<Approvals, String>>,
    pub threads: Option<Result<Vec<Thread>, String>>,
    pub files: Option<Result<Vec<FileChange>, String>>,
    pub pipelines: Option<Result<Vec<Pipeline>, String>>,
}

/// Everything loaded for one pipeline.
#[derive(Default)]
pub struct RunDetail {
    pub loading: bool,
    pub at: Option<i64>,
    pub pipeline: Option<Result<Pipeline, String>>,
    pub jobs: Option<Result<Vec<Job>, String>>,
}

/// A fetched job log and the viewer's state for it.
pub struct LogDoc {
    pub log: ForgeLog,
    pub folded: HashSet<u32>,
    pub selected: Option<usize>,
    pub scroll_to: Option<usize>,
    /// Lines matching `filter` (recomputed when the filter changes).
    pub filter: String,
    pub matches: Vec<usize>,
}

impl LogDoc {
    pub fn new(log: ForgeLog) -> Self {
        let last_error = log.errors.last().copied();
        Self {
            folded: log.initially_folded(),
            scroll_to: last_error,
            selected: last_error,
            matches: (0..log.lines.len()).collect(),
            filter: String::new(),
            log,
        }
    }

    pub fn set_filter(&mut self, f: &str) {
        if self.filter != f {
            self.filter = f.to_owned();
            self.matches = self.log.matching(f);
        }
    }
}

#[derive(Default)]
pub struct LogState {
    pub loading: bool,
    pub doc: Option<Result<LogDoc, String>>,
}

/// Which merge / pull requests the list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ReviewView {
    #[default]
    All,
    Mine,
    Ready,
    Drafts,
}

impl ReviewView {
    pub const ALL: [ReviewView; 4] = [
        ReviewView::All,
        ReviewView::Mine,
        ReviewView::Ready,
        ReviewView::Drafts,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ReviewView::All => "All",
            ReviewView::Mine => "Mine",
            ReviewView::Ready => "Ready",
            ReviewView::Drafts => "Drafts",
        }
    }
}

/// The create form.
#[derive(Clone, Debug, Default)]
pub struct CreateForm {
    pub source: String,
    pub target: String,
    pub title: String,
    pub description: String,
    pub draft: bool,
    pub remove_source: bool,
    pub squash: bool,
    pub assignees: Vec<String>,
    pub assignee_buf: String,
    pub reviewers: Vec<String>,
    pub reviewer_buf: String,
    pub labels: Vec<String>,
    pub label_buf: String,
    pub auto_complete: bool,
    pub error: String,
}

/// The merge / complete form of the selected request.
#[derive(Clone, Debug, Default)]
pub struct MergeForm {
    pub number: Option<u64>,
    pub squash: bool,
    pub remove_source: bool,
    pub when_pipeline_succeeds: bool,
    pub message: String,
}

/// A variable row of the run form.
#[derive(Clone, Debug, Default)]
pub struct VarRow {
    pub key: String,
    pub value: String,
    pub secret: bool,
}

/// The "run a pipeline" form.
#[derive(Clone, Debug, Default)]
pub struct RunForm {
    pub reference: String,
    /// Azure pipeline (definition) name.
    pub definition: String,
    pub vars: Vec<VarRow>,
    pub error: String,
}

#[derive(Default)]
pub struct State {
    pub tool: Option<ToolState>,
    pub checking: bool,
    pub checked_at: Option<i64>,
    /// GitLab: read a public project through unauthenticated curl.
    pub public: bool,
    pub reviews: Loaded<Vec<ChangeRequest>>,
    pub selected: Option<u64>,
    pub details: HashMap<u64, Detail>,
    pub filter: String,
    pub view: ReviewView,
    pub create: Option<CreateForm>,
    pub merge: MergeForm,
    pub comment: String,
    pub pipelines: Loaded<Vec<Pipeline>>,
    pub branch_only: bool,
    pub pipeline_filter: String,
    pub selected_pipeline: Option<u64>,
    pub runs: HashMap<u64, RunDetail>,
    /// `(pipeline, job key)` whose log is shown (GitLab job id, Azure log id).
    pub job: Option<(u64, u64)>,
    pub logs: HashMap<(u64, u64), LogState>,
    pub log_filter: String,
    pub run_form: Option<RunForm>,
    /// Writes in flight (labels).
    pub writing: HashSet<String>,
}

/// What to reload after a write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum After {
    Reviews,
    Review(u64),
    Pipelines,
    Pipeline(u64),
}

#[derive(Clone, Debug)]
pub enum Act {
    /// Detect the CLI again (after installing or signing in).
    CheckTool,
    /// GitLab: read the public project without signing in.
    UsePublic(bool),
    RefreshReviews,
    SetView(ReviewView),
    Select(u64),
    RefreshReview(u64),
    NewReview,
    CloseCreate,
    SubmitCreate,
    Merge(u64),
    Approve(u64),
    Unapprove(u64),
    Vote(u64, Vote),
    Comment(u64),
    RefreshPipelines,
    SetBranchOnly(bool),
    SelectPipeline(u64),
    RefreshPipeline(u64),
    ShowLog {
        pipeline: u64,
        key: u64,
    },
    CloseLog,
    RetryPipeline(u64),
    CancelPipeline(u64),
    RetryJob {
        pipeline: u64,
        job: u64,
    },
    CancelJob {
        pipeline: u64,
        job: u64,
    },
    NewRun,
    /// Pre-fill the run form (Azure "run again").
    RunAgain {
        definition: String,
        reference: String,
    },
    CloseRun,
    SubmitRun,
    /// Run a confirmed write.
    Do {
        call: Call,
        label: String,
        after: After,
    },
    /// Script helpers.
    New,
    SelectNth(usize),
    SelectNthPipeline(usize),
    ShowNthJob(usize),
}

pub enum Msg {
    Tool(ToolState),
    Reviews(Result<Vec<ChangeRequest>, String>),
    Detail {
        number: u64,
        cr: Result<ChangeRequest, String>,
        approvals: Option<Result<Approvals, String>>,
        threads: Option<Result<Vec<Thread>, String>>,
        files: Option<Result<Vec<FileChange>, String>>,
        pipelines: Option<Result<Vec<Pipeline>, String>>,
    },
    Pipelines(Result<Vec<Pipeline>, String>),
    Run {
        id: u64,
        pipeline: Result<Pipeline, String>,
        jobs: Result<Vec<Job>, String>,
    },
    Log {
        pipeline: u64,
        key: u64,
        result: Result<ForgeLog, String>,
    },
    Wrote {
        label: String,
        after: After,
        ok: bool,
    },
}

impl Msg {
    /// One line for the activity log (`Err` when the work failed).
    pub fn summary(&self) -> Result<String, String> {
        match self {
            Msg::Tool(t) => Ok(match t {
                ToolState::Missing => "not installed".into(),
                ToolState::NoExtension { version } => {
                    format!("{version}, azure-devops extension missing")
                }
                ToolState::SignedOut { version, detail } => {
                    format!("{version}, not signed in: {detail}")
                }
                ToolState::Ready { version, user, via } => {
                    format!("{version}, signed in as {user} ({via})")
                }
            }),
            Msg::Reviews(r) => r
                .as_ref()
                .map(|v| format::plural(v.len(), "open request", "open requests"))
                .map_err(Clone::clone),
            Msg::Detail { number, cr, .. } => cr
                .as_ref()
                .map(|c| format!("#{number} {}", c.title))
                .map_err(Clone::clone),
            Msg::Pipelines(r) => r
                .as_ref()
                .map(|v| format::plural(v.len(), "pipeline", "pipelines"))
                .map_err(Clone::clone),
            Msg::Run { id, jobs, .. } => jobs
                .as_ref()
                .map(|j| format!("pipeline {id}: {}", format::plural(j.len(), "job", "jobs")))
                .map_err(Clone::clone),
            Msg::Log { key, result, .. } => result
                .as_ref()
                .map(|l| {
                    format!(
                        "log {key}: {} lines, {}",
                        format::count(l.lines.len()),
                        format::bytes(l.bytes)
                    )
                })
                .map_err(Clone::clone),
            Msg::Wrote { label, ok, .. } => {
                if *ok {
                    Ok(label.clone())
                } else {
                    Err(format!("{label} failed"))
                }
            }
        }
    }
}

/// Persisted per repository.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub view: ReviewView,
    pub branch_only: bool,
}

impl State {
    pub fn prefs(&self) -> Prefs {
        Prefs {
            view: self.view,
            branch_only: self.branch_only,
        }
    }

    pub fn apply_prefs(&mut self, p: &Prefs) {
        self.view = p.view;
        self.branch_only = p.branch_only;
    }

    /// The CLI is signed in.
    pub fn ready(&self) -> bool {
        self.tool.as_ref().is_some_and(ToolState::ready)
    }

    /// Data can be read (signed in, or public GitLab mode).
    pub fn can_read(&self) -> bool {
        self.ready() || self.public
    }

    /// Writes are possible (signed in, not in public mode).
    pub fn can_write(&self) -> bool {
        self.ready() && !self.public
    }

    pub fn me(&self) -> &str {
        self.tool.as_ref().map(ToolState::user).unwrap_or("")
    }
}

/// The current branch of a repository.
pub fn current_branch(repo: &RepoState) -> Option<String> {
    repo.snapshot
        .as_ref()
        .and_then(|s| s.head.branch())
        .map(str::to_owned)
}

/// Run a GitLab read: `glab api`, or `curl` in public mode.
fn gl_read(
    req: &GlRequest,
    public: bool,
    dir: &Path,
    log: &mut TaskLog,
) -> Result<Vec<u8>, String> {
    let r = if public {
        match req.public_call() {
            Some(c) => cli::run_curl(&c, dir, &mut log.calls),
            None => return Err("Writes need a sign-in".into()),
        }
    } else {
        cli::run(&req.call(), dir, TIMEOUT, &mut log.calls)
    };
    r.map_err(|e| e.to_string())
}

fn az_read(call: &Call, dir: &Path, log: &mut TaskLog) -> Result<Vec<u8>, String> {
    cli::run(call, dir, TIMEOUT, &mut log.calls).map_err(|e| e.to_string())
}

fn target(remote: &ForgeRemote) -> Result<Target, String> {
    Target::of(remote).ok_or_else(|| format!("{} is not an Azure DevOps remote", remote.web_url))
}

fn check_tool(cx: &mut Ctx, repo: &mut RepoState) {
    let Some(remote) = repo.forge_remote.clone() else {
        return;
    };
    let st = &mut repo.devops.forge;
    if st.checking {
        return;
    }
    st.checking = true;
    let name = ftools::tool(remote.kind).name();
    cx.read(format!("Check {name}"), move |dir, log| {
        devtools::refresh();
        Msg::Tool(ftools::check(&remote, dir, &mut log.calls))
    });
}

fn load_reviews(cx: &mut Ctx, repo: &mut RepoState) {
    let Some(remote) = repo.forge_remote.clone() else {
        return;
    };
    let st = &mut repo.devops.forge;
    if st.reviews.loading || !st.can_read() {
        return;
    }
    st.reviews.loading = true;
    let public = st.public;
    let label = format!("Load {}s", remote.kind.review_noun());
    cx.read(label, move |dir, log| {
        let r = match remote.kind {
            ForgeKind::GitLab => gl_read(&gitlab::merge_requests(&remote), public, dir, log)
                .and_then(|b| gitlab::parse_mrs(&b).map_err(|e| e.to_string())),
            ForgeKind::Azure => target(&remote).and_then(|t| {
                az_read(&azure::pr_list(&t), dir, log)
                    .and_then(|b| azure::parse_prs(&t, &b).map_err(|e| e.to_string()))
            }),
        };
        Msg::Reviews(r)
    });
}

fn load_detail(cx: &mut Ctx, repo: &mut RepoState, number: u64) {
    let Some(remote) = repo.forge_remote.clone() else {
        return;
    };
    let st = &mut repo.devops.forge;
    if !st.can_read() {
        return;
    }
    let d = st.details.entry(number).or_default();
    if d.loading {
        return;
    }
    d.loading = true;
    let public = st.public;
    cx.read(
        format!("Load {} #{number}", remote.kind.review_noun()),
        move |dir, log| match remote.kind {
            ForgeKind::GitLab => {
                let cr = gl_read(&gitlab::merge_request(&remote, number), public, dir, log)
                    .and_then(|b| gitlab::parse_mr(&b).map_err(|e| e.to_string()));
                let approvals = gl_read(&gitlab::approvals(&remote, number), public, dir, log)
                    .and_then(|b| gitlab::parse_approvals(&b).map_err(|e| e.to_string()));
                let threads = gl_read(&gitlab::discussions(&remote, number), public, dir, log)
                    .and_then(|b| gitlab::parse_discussions(&b).map_err(|e| e.to_string()));
                let files = gl_read(&gitlab::diffs(&remote, number), public, dir, log)
                    .and_then(|b| gitlab::parse_diffs(&b).map_err(|e| e.to_string()));
                let pipelines = gl_read(&gitlab::mr_pipelines(&remote, number), public, dir, log)
                    .and_then(|b| gitlab::parse_pipelines(&b).map_err(|e| e.to_string()));
                Msg::Detail {
                    number,
                    cr,
                    approvals: Some(approvals),
                    threads: Some(threads),
                    files: Some(files),
                    pipelines: Some(pipelines),
                }
            }
            ForgeKind::Azure => {
                let cr = target(&remote).and_then(|t| {
                    az_read(&azure::pr_show(&t, number), dir, log)
                        .and_then(|b| azure::parse_pr(&t, &b).map_err(|e| e.to_string()))
                });
                Msg::Detail {
                    number,
                    cr,
                    approvals: None,
                    threads: None,
                    files: None,
                    pipelines: None,
                }
            }
        },
    );
}

fn load_pipelines(cx: &mut Ctx, repo: &mut RepoState) {
    let Some(remote) = repo.forge_remote.clone() else {
        return;
    };
    let branch = current_branch(repo);
    let st = &mut repo.devops.forge;
    if st.pipelines.loading || !st.can_read() {
        return;
    }
    st.pipelines.loading = true;
    let public = st.public;
    let branch = branch.filter(|_| st.branch_only);
    cx.read("Load pipelines", move |dir, log| {
        let r = match remote.kind {
            ForgeKind::GitLab => gl_read(
                &gitlab::pipelines(&remote, branch.as_deref()),
                public,
                dir,
                log,
            )
            .and_then(|b| gitlab::parse_pipelines(&b).map_err(|e| e.to_string())),
            ForgeKind::Azure => target(&remote).and_then(|t| {
                az_read(&azure::runs_list(&t, branch.as_deref()), dir, log)
                    .and_then(|b| azure::parse_runs(&t, &b).map_err(|e| e.to_string()))
            }),
        };
        Msg::Pipelines(r)
    });
}

fn load_run(cx: &mut Ctx, repo: &mut RepoState, id: u64) {
    let Some(remote) = repo.forge_remote.clone() else {
        return;
    };
    let st = &mut repo.devops.forge;
    if !st.can_read() {
        return;
    }
    let d = st.runs.entry(id).or_default();
    if d.loading {
        return;
    }
    d.loading = true;
    let public = st.public;
    cx.read(
        format!("Load pipeline {id}"),
        move |dir, log| match remote.kind {
            ForgeKind::GitLab => {
                let pipeline = gl_read(&gitlab::pipeline(&remote, id), public, dir, log)
                    .and_then(|b| gitlab::parse_pipeline(&b).map_err(|e| e.to_string()));
                let jobs = gl_read(&gitlab::pipeline_jobs(&remote, id), public, dir, log)
                    .and_then(|b| gitlab::parse_jobs(&b).map_err(|e| e.to_string()));
                Msg::Run { id, pipeline, jobs }
            }
            ForgeKind::Azure => match target(&remote) {
                Ok(t) => {
                    let pipeline = az_read(&azure::run_show(&t, id), dir, log)
                        .and_then(|b| azure::parse_run(&t, &b).map_err(|e| e.to_string()));
                    let jobs = az_read(&azure::timeline(&t, id), dir, log)
                        .and_then(|b| azure::parse_timeline(&t, id, &b).map_err(|e| e.to_string()));
                    Msg::Run { id, pipeline, jobs }
                }
                Err(e) => Msg::Run {
                    id,
                    pipeline: Err(e.clone()),
                    jobs: Err(e),
                },
            },
        },
    );
}

fn load_log(cx: &mut Ctx, repo: &mut RepoState, pipeline: u64, key: u64) {
    let Some(remote) = repo.forge_remote.clone() else {
        return;
    };
    let st = &mut repo.devops.forge;
    if !st.can_read() {
        return;
    }
    let l = st.logs.entry((pipeline, key)).or_default();
    if l.loading {
        return;
    }
    l.loading = true;
    l.doc = None;
    let public = st.public;
    cx.read(format!("Load the log of job {key}"), move |dir, log| {
        let result = match remote.kind {
            ForgeKind::GitLab => {
                let raw = if public {
                    cli::run_curl(&gitlab::public_trace(&remote, key), dir, &mut log.calls)
                        .map_err(|e| e.to_string())
                } else {
                    gl_read(&gitlab::job_trace(&remote, key), false, dir, log)
                };
                raw.map(|b| trace::parse_gitlab(&b))
            }
            ForgeKind::Azure => target(&remote).and_then(|t| {
                az_read(&azure::log(&t, pipeline, key), dir, log)
                    .map(|b| trace::parse_azure(&azure::log_text(&b)))
            }),
        };
        Msg::Log {
            pipeline,
            key,
            result,
        }
    });
}

/// Load what the visible page needs, if not loaded yet.
fn ensure_page(cx: &mut Ctx, repo: &mut RepoState, page: Page) {
    let st = &repo.devops.forge;
    if !st.can_read() {
        return;
    }
    match page {
        Page::Review if st.reviews.data.is_none() && st.reviews.error.is_none() => {
            load_reviews(cx, repo)
        }
        Page::GitHub if st.pipelines.data.is_none() && st.pipelines.error.is_none() => {
            load_pipelines(cx, repo)
        }
        _ => {}
    }
}

/// Ask for confirmation, then run `call`.
#[allow(clippy::too_many_arguments)]
fn confirm(
    cx: &mut Ctx,
    title: impl Into<String>,
    body: impl Into<String>,
    button: &str,
    danger: bool,
    call: Call,
    label: impl Into<String>,
    after: After,
) {
    let details = call.details();
    cx.confirm(
        title,
        body,
        button,
        danger,
        details,
        Act::Do {
            call,
            label: label.into(),
            after,
        }
        .into(),
    );
}

fn noun(remote: &ForgeRemote) -> &'static str {
    remote.kind.review_noun()
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    let Some(remote) = repo.forge_remote.clone() else {
        return;
    };
    let page = repo.page;
    match act {
        Act::CheckTool => {
            repo.devops.forge.tool = None;
            check_tool(cx, repo);
        }
        Act::UsePublic(on) => {
            let st = &mut repo.devops.forge;
            st.public = on && remote.kind == ForgeKind::GitLab;
            st.reviews = Loaded::default();
            st.pipelines = Loaded::default();
            st.details.clear();
            st.runs.clear();
            st.logs.clear();
            st.job = None;
            if st.public {
                ensure_page(cx, repo, page);
            }
        }
        Act::RefreshReviews => {
            load_reviews(cx, repo);
            if let Some(n) = repo.devops.forge.selected {
                load_detail(cx, repo, n);
            }
        }
        Act::SetView(v) => repo.devops.forge.view = v,
        Act::Select(n) => {
            let st = &mut repo.devops.forge;
            st.selected = Some(n);
            st.create = None;
            if st.merge.number != Some(n) {
                let cr = st
                    .reviews
                    .data
                    .as_ref()
                    .and_then(|v| v.iter().find(|c| c.number == n));
                st.merge = MergeForm {
                    number: Some(n),
                    squash: cr.is_some_and(|c| c.squash),
                    remove_source: cr.is_none_or(|c| c.remove_source),
                    when_pipeline_succeeds: false,
                    message: String::new(),
                };
                st.comment.clear();
            }
            if !st.details.get(&n).is_some_and(|d| d.cr.is_some()) {
                load_detail(cx, repo, n);
            }
        }
        Act::RefreshReview(n) => load_detail(cx, repo, n),
        Act::NewReview => {
            let branch = current_branch(repo).unwrap_or_default();
            let default_target = repo
                .snapshot
                .as_ref()
                .and_then(|s| {
                    ["main", "master", "develop"].into_iter().find(|b| {
                        s.branch(b).is_some() || s.remote_branch(&format!("origin/{b}")).is_some()
                    })
                })
                .unwrap_or("main")
                .to_owned();
            let title = repo
                .snapshot
                .as_ref()
                .and_then(|s| s.head.target())
                .and_then(|h| {
                    repo.snapshot
                        .as_ref()
                        .and_then(|s| s.index_of.get(&h).map(|&i| s.commits[i].summary.clone()))
                })
                .unwrap_or_default();
            repo.devops.forge.create = Some(CreateForm {
                source: branch,
                target: default_target,
                title,
                remove_source: true,
                ..Default::default()
            });
        }
        Act::CloseCreate => repo.devops.forge.create = None,
        Act::SubmitCreate => {
            let Some(form) = repo.devops.forge.create.as_mut() else {
                return;
            };
            let built: Result<Call, String> = match remote.kind {
                ForgeKind::GitLab => gitlab::create_mr(
                    &remote,
                    &gitlab::NewMr {
                        source: form.source.clone(),
                        target: form.target.clone(),
                        title: form.title.clone(),
                        description: form.description.clone(),
                        draft: form.draft,
                        remove_source: form.remove_source,
                        squash: form.squash,
                        assignee_ids: form.assignees.clone(),
                        reviewer_ids: form.reviewers.clone(),
                        labels: form.labels.clone(),
                    },
                )
                .map(|r| r.call())
                .map_err(|e| e.to_string()),
                ForgeKind::Azure => target(&remote).and_then(|t| {
                    azure::pr_create(
                        &t,
                        &azure::NewPr {
                            source: form.source.clone(),
                            target: form.target.clone(),
                            title: form.title.clone(),
                            description: form.description.clone(),
                            draft: form.draft,
                            reviewers: form.reviewers.clone(),
                            labels: form.labels.clone(),
                            auto_complete: form.auto_complete,
                            squash: form.squash,
                            delete_source: form.remove_source,
                        },
                    )
                    .map_err(|e| e.to_string())
                }),
            };
            match built {
                Err(e) => form.error = e,
                Ok(call) => {
                    form.error.clear();
                    let title = format!(
                        "Create a {} from {} into {}?",
                        noun(&remote),
                        form.source.trim(),
                        form.target.trim()
                    );
                    confirm(
                        cx,
                        title,
                        format!(
                            "Opens it on {} ({}). The branch must already be on the remote.",
                            remote.host, remote.path
                        ),
                        &format!("Create {}", noun(&remote)),
                        false,
                        call,
                        format!("Create a {}", noun(&remote)),
                        After::Reviews,
                    );
                }
            }
        }
        Act::Merge(n) => {
            let st = &repo.devops.forge;
            let cr = st
                .details
                .get(&n)
                .and_then(|d| d.cr.as_ref())
                .and_then(|r| r.as_ref().ok())
                .cloned()
                .or_else(|| {
                    st.reviews
                        .data
                        .as_ref()
                        .and_then(|v| v.iter().find(|c| c.number == n).cloned())
                });
            let Some(cr) = cr else { return };
            let f = st.merge.clone();
            let (call, what) = match remote.kind {
                ForgeKind::GitLab => (
                    gitlab::merge(
                        &remote,
                        n,
                        &gitlab::MergeOpts {
                            squash: f.squash,
                            remove_source: f.remove_source,
                            when_pipeline_succeeds: f.when_pipeline_succeeds,
                            sha: Some(cr.sha.clone()),
                            message: f.message.clone(),
                        },
                    )
                    .call(),
                    if f.when_pipeline_succeeds {
                        "Merges it when its pipeline succeeds"
                    } else {
                        "Merges it now"
                    },
                ),
                ForgeKind::Azure => match target(&remote) {
                    Ok(t) => (
                        azure::pr_complete(
                            &t,
                            n,
                            &azure::CompleteOpts {
                                squash: f.squash,
                                delete_source: f.remove_source,
                                message: f.message.clone(),
                            },
                        ),
                        "Completes it (merges into the target branch)",
                    ),
                    Err(e) => {
                        cx.toast(ToastKind::Error, e);
                        return;
                    }
                },
            };
            let verb = if remote.kind == ForgeKind::GitLab {
                "Merge"
            } else {
                "Complete"
            };
            confirm(
                cx,
                format!("{verb} {} #{n}?", noun(&remote)),
                format!(
                    "{what}: {} into {}{}{}.",
                    cr.source,
                    cr.target,
                    if f.squash { ", squashed" } else { "" },
                    if f.remove_source {
                        ", deleting the source branch"
                    } else {
                        ""
                    }
                ),
                verb,
                true,
                call,
                format!("{verb} #{n}"),
                After::Review(n),
            );
        }
        Act::Approve(n) => {
            let sha = repo
                .devops
                .forge
                .details
                .get(&n)
                .and_then(|d| d.cr.as_ref())
                .and_then(|r| r.as_ref().ok())
                .map(|c| c.sha.clone());
            let call = gitlab::approve(&remote, n, sha.as_deref()).call();
            confirm(
                cx,
                format!("Approve merge request !{n}?"),
                "Adds your approval, pinned to the current head commit.",
                "Approve",
                false,
                call,
                format!("Approve !{n}"),
                After::Review(n),
            );
        }
        Act::Unapprove(n) => {
            let call = gitlab::unapprove(&remote, n).call();
            confirm(
                cx,
                format!("Remove your approval of !{n}?"),
                "Withdraws the approval you gave.",
                "Remove approval",
                true,
                call,
                format!("Unapprove !{n}"),
                After::Review(n),
            );
        }
        Act::Vote(n, vote) => match target(&remote) {
            Ok(t) => {
                let call = azure::pr_vote(&t, n, vote);
                confirm(
                    cx,
                    format!("{} on pull request #{n}?", vote.label()),
                    format!(
                        "Sets your vote to \"{}\" ({}).",
                        azure::vote_label(vote.value()),
                        vote.value()
                    ),
                    vote.label(),
                    matches!(vote, Vote::Reject),
                    call,
                    format!("Vote on #{n}"),
                    After::Review(n),
                );
            }
            Err(e) => cx.toast(ToastKind::Error, e),
        },
        Act::Comment(n) => {
            let body = repo.devops.forge.comment.clone();
            match gitlab::comment(&remote, n, &body) {
                Ok(r) => confirm(
                    cx,
                    format!("Comment on !{n}?"),
                    "Starts a new discussion thread.",
                    "Comment",
                    false,
                    r.call(),
                    format!("Comment on !{n}"),
                    After::Review(n),
                ),
                Err(e) => cx.toast(ToastKind::Error, e.to_string()),
            }
        }
        Act::RefreshPipelines => {
            load_pipelines(cx, repo);
            if let Some(id) = repo.devops.forge.selected_pipeline {
                load_run(cx, repo, id);
            }
        }
        Act::SetBranchOnly(on) => {
            repo.devops.forge.branch_only = on;
            load_pipelines(cx, repo);
        }
        Act::SelectPipeline(id) => {
            let st = &mut repo.devops.forge;
            st.selected_pipeline = Some(id);
            st.run_form = None;
            if st.job.is_some_and(|(p, _)| p != id) {
                st.job = None;
            }
            if !st.runs.get(&id).is_some_and(|d| d.jobs.is_some()) {
                load_run(cx, repo, id);
            }
        }
        Act::RefreshPipeline(id) => load_run(cx, repo, id),
        Act::ShowLog { pipeline, key } => {
            let st = &mut repo.devops.forge;
            st.job = Some((pipeline, key));
            let have = st
                .logs
                .get(&(pipeline, key))
                .is_some_and(|l| l.loading || matches!(l.doc, Some(Ok(_))));
            if !have {
                load_log(cx, repo, pipeline, key);
            }
        }
        Act::CloseLog => repo.devops.forge.job = None,
        Act::RetryPipeline(id) => {
            let call = gitlab::retry_pipeline(&remote, id).call();
            confirm(
                cx,
                format!("Retry pipeline {id}?"),
                "Runs its failed and cancelled jobs again.",
                "Retry",
                false,
                call,
                format!("Retry pipeline {id}"),
                After::Pipeline(id),
            );
        }
        Act::CancelPipeline(id) => {
            let call = match remote.kind {
                ForgeKind::GitLab => gitlab::cancel_pipeline(&remote, id).call(),
                ForgeKind::Azure => match target(&remote) {
                    Ok(t) => azure_cancel(&t, id),
                    Err(e) => {
                        cx.toast(ToastKind::Error, e);
                        return;
                    }
                },
            };
            confirm(
                cx,
                format!("Cancel pipeline {id}?"),
                "Stops its running and pending jobs.",
                "Cancel pipeline",
                true,
                call,
                format!("Cancel pipeline {id}"),
                After::Pipeline(id),
            );
        }
        Act::RetryJob { pipeline, job } => {
            let call = gitlab::retry_job(&remote, job).call();
            confirm(
                cx,
                format!("Retry job {job}?"),
                "Starts a new attempt of this job in the same pipeline.",
                "Retry job",
                false,
                call,
                format!("Retry job {job}"),
                After::Pipeline(pipeline),
            );
        }
        Act::CancelJob { pipeline, job } => {
            let call = gitlab::cancel_job(&remote, job).call();
            confirm(
                cx,
                format!("Cancel job {job}?"),
                "Stops this job.",
                "Cancel job",
                true,
                call,
                format!("Cancel job {job}"),
                After::Pipeline(pipeline),
            );
        }
        Act::NewRun => {
            let reference = current_branch(repo).unwrap_or_default();
            let st = &mut repo.devops.forge;
            let definition = st
                .pipelines
                .data
                .as_ref()
                .and_then(|v| v.first())
                .map(|p| p.name.clone())
                .unwrap_or_default();
            st.run_form = Some(RunForm {
                reference,
                definition,
                vars: vec![VarRow::default()],
                error: String::new(),
            });
        }
        Act::RunAgain {
            definition,
            reference,
        } => {
            repo.devops.forge.run_form = Some(RunForm {
                reference,
                definition,
                vars: vec![VarRow::default()],
                error: String::new(),
            });
        }
        Act::CloseRun => repo.devops.forge.run_form = None,
        Act::SubmitRun => {
            let Some(form) = repo.devops.forge.run_form.as_mut() else {
                return;
            };
            let built: Result<(Call, String), String> = match remote.kind {
                ForgeKind::GitLab => {
                    let vars: Vec<gitlab::Var> = form
                        .vars
                        .iter()
                        .map(|v| gitlab::Var {
                            key: v.key.clone(),
                            value: v.value.clone(),
                            secret: v.secret,
                        })
                        .collect();
                    gitlab::run_pipeline(&remote, &form.reference, &vars)
                        .map(|r| {
                            (
                                r.call(),
                                format!("Run a pipeline on {}", form.reference.trim()),
                            )
                        })
                        .map_err(|e| e.to_string())
                }
                ForgeKind::Azure => target(&remote).and_then(|t| {
                    let vars: Vec<(String, String)> = form
                        .vars
                        .iter()
                        .map(|v| (v.key.clone(), v.value.clone()))
                        .collect();
                    azure::pipeline_run(&t, &form.definition, &form.reference, &vars)
                        .map(|c| {
                            (
                                c,
                                format!(
                                    "Run {} on {}",
                                    form.definition.trim(),
                                    form.reference.trim()
                                ),
                            )
                        })
                        .map_err(|e| e.to_string())
                }),
            };
            match built {
                Err(e) => form.error = e,
                Ok((call, label)) => {
                    form.error.clear();
                    let secrets = form
                        .vars
                        .iter()
                        .filter(|v| v.secret && !v.key.is_empty())
                        .count();
                    let body = if secrets > 0 {
                        format!(
                            "Starts a new pipeline. {} masked here and in the activity log; the values go to glab on standard input.",
                            format::plural(secrets, "secret variable is", "secret variables are")
                        )
                    } else {
                        "Starts a new pipeline.".to_owned()
                    };
                    confirm(
                        cx,
                        format!("{label}?"),
                        body,
                        "Run pipeline",
                        false,
                        call,
                        label,
                        After::Pipelines,
                    );
                }
            }
        }
        Act::Do { call, label, after } => {
            let st = &mut repo.devops.forge;
            if !st.can_write() || !st.writing.insert(label.clone()) {
                return;
            }
            cx.remote_change(label.clone(), move |dir, log| {
                let r = cli::run(&call, dir, TIMEOUT, &mut log.calls);
                let ok = r.is_ok();
                let result = match r {
                    Ok(out) => {
                        let text = String::from_utf8_lossy(&out);
                        let first: String = text
                            .lines()
                            .next()
                            .unwrap_or("")
                            .chars()
                            .take(200)
                            .collect();
                        Ok(if first.is_empty() {
                            label.clone()
                        } else {
                            format!("{label}\n{first}")
                        })
                    }
                    Err(e) => Err(e.to_string()),
                };
                (result, Some(Msg::Wrote { label, after, ok }))
            });
        }
        Act::New => match page {
            Page::GitHub => apply(cx, repo, Act::NewRun),
            _ => apply(cx, repo, Act::NewReview),
        },
        Act::SelectNth(n) => {
            let keys = review::visible_numbers(repo);
            if let Some(&k) = keys.get(n) {
                apply(cx, repo, Act::Select(k));
            }
        }
        Act::SelectNthPipeline(n) => {
            let ids = pipelines::visible_ids(&repo.devops.forge);
            if let Some(&id) = ids.get(n) {
                apply(cx, repo, Act::SelectPipeline(id));
            }
        }
        Act::ShowNthJob(n) => {
            let st = &repo.devops.forge;
            let Some(p) = st.selected_pipeline else {
                return;
            };
            let key = st
                .runs
                .get(&p)
                .and_then(|d| d.jobs.as_ref())
                .and_then(|j| j.as_ref().ok())
                .and_then(|jobs| jobs.iter().filter_map(|j| j.log_id).nth(n));
            if let Some(key) = key {
                apply(cx, repo, Act::ShowLog { pipeline: p, key });
            }
        }
    }
}

/// `az pipelines build cancel --build-id N`.
fn azure_cancel(t: &Target, id: u64) -> Call {
    Call::new(
        cli::Program::Az,
        vec![
            "pipelines".into(),
            "build".into(),
            "cancel".into(),
            "--build-id".into(),
            id.to_string(),
            "--org".into(),
            t.org_url.clone(),
            "--project".into(),
            t.project.clone(),
            "-o".into(),
            "json".into(),
        ],
    )
    .write()
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    let page = repo.page;
    match msg {
        Msg::Tool(t) => {
            let st = &mut repo.devops.forge;
            st.checking = false;
            st.checked_at = Some(format::now_secs());
            st.tool = Some(t);
            if st.ready() {
                st.public = false;
            }
            ensure_page(cx, repo, page);
        }
        Msg::Reviews(r) => {
            let st = &mut repo.devops.forge;
            st.reviews.done(r);
            // Keep the selection when it is still listed.
            if let (Some(sel), Some(list)) = (st.selected, st.reviews.data.as_ref())
                && !list.iter().any(|c| c.number == sel)
                && !st.details.contains_key(&sel)
            {
                st.selected = None;
            }
        }
        Msg::Detail {
            number,
            cr,
            approvals,
            threads,
            files,
            pipelines,
        } => {
            let d = repo.devops.forge.details.entry(number).or_default();
            d.loading = false;
            d.at = Some(format::now_secs());
            d.cr = Some(cr);
            d.approvals = approvals;
            d.threads = threads;
            d.files = files;
            d.pipelines = pipelines;
        }
        Msg::Pipelines(r) => repo.devops.forge.pipelines.done(r),
        Msg::Run { id, pipeline, jobs } => {
            let d = repo.devops.forge.runs.entry(id).or_default();
            d.loading = false;
            d.at = Some(format::now_secs());
            d.pipeline = Some(pipeline);
            d.jobs = Some(jobs);
        }
        Msg::Log {
            pipeline,
            key,
            result,
        } => {
            let l = repo.devops.forge.logs.entry((pipeline, key)).or_default();
            l.loading = false;
            l.doc = Some(result.map(LogDoc::new));
        }
        Msg::Wrote { label, after, ok } => {
            repo.devops.forge.writing.remove(&label);
            if ok {
                match after {
                    After::Reviews => {
                        repo.devops.forge.create = None;
                        load_reviews(cx, repo);
                    }
                    After::Review(n) => {
                        if label.starts_with("Comment") {
                            repo.devops.forge.comment.clear();
                        }
                        load_detail(cx, repo, n);
                        load_reviews(cx, repo);
                    }
                    After::Pipelines => {
                        repo.devops.forge.run_form = None;
                        load_pipelines(cx, repo);
                    }
                    After::Pipeline(id) => {
                        load_run(cx, repo, id);
                        load_pipelines(cx, repo);
                    }
                }
            }
        }
    }
}

/// A console run started by this area (or any other) ended.
pub fn on_console_done(_cx: &mut Ctx, _repo: &mut RepoState, _run: &ConsoleRun) {}

/// The Review or Actions page was opened on a GitLab / Azure DevOps remote.
pub fn on_page_shown(cx: &mut Ctx, repo: &mut RepoState, page: Page) {
    let st = &repo.devops.forge;
    if st.tool.is_none() {
        if !st.checking {
            check_tool(cx, repo);
        }
        return;
    }
    ensure_page(cx, repo, page);
}

/// `GITVIS_SCRIPT` commands this area understands.
pub fn script(cmd: &str, arg: &str) -> Option<Vec<Action>> {
    let n = || arg.trim().parse::<usize>().ok();
    Some(match cmd {
        "forge-tab" => {
            let page = match arg.trim() {
                "review" | "reviews" | "mr" | "pr" => Page::Review,
                "pipelines" | "pipeline" | "ci" => Page::GitHub,
                _ => return None,
            };
            vec![Action::ShowPage(page)]
        }
        "mr" => vec![Action::ShowPage(Page::Review), Act::SelectNth(n()?).into()],
        "pipeline" => vec![
            Action::ShowPage(Page::GitHub),
            Act::SelectNthPipeline(n()?).into(),
        ],
        "forge-job" => vec![Act::ShowNthJob(n()?).into()],
        "forge-new" => vec![Act::New.into()],
        "forge-public" => vec![Act::UsePublic(true).into()],
        "forge-check" => vec![Act::CheckTool.into()],
        "forge-refresh" => vec![Act::RefreshReviews.into(), Act::RefreshPipelines.into()],
        _ => return None,
    })
}

/// The Review page for a GitLab / Azure DevOps remote.
pub fn show_review(
    ui: &mut egui::Ui,
    repo: &mut RepoState,
    _console: &Console,
    _settings: &Settings,
    actions: &mut Vec<Action>,
) {
    review::show(ui, repo, actions);
}

/// The Actions page for a GitLab / Azure DevOps remote (pipelines).
pub fn show_pipelines(
    ui: &mut egui::Ui,
    repo: &mut RepoState,
    _console: &Console,
    _settings: &Settings,
    actions: &mut Vec<Action>,
) {
    pipelines::show(ui, repo, actions);
}

/// A dot on the Review (`page == Review`) or Actions (`page == GitHub`)
/// rail entry and a tooltip line, when it needs attention.
pub fn rail_badge(
    repo: &RepoState,
    _console: &Console,
    page: Page,
) -> Option<(crate::theme::Tone, String)> {
    if page != Page::GitHub {
        return None;
    }
    let branch = current_branch(repo)?;
    let latest = repo
        .devops
        .forge
        .pipelines
        .data
        .as_ref()?
        .iter()
        .find(|p| p.reference == branch)?;
    if latest.outcome.is_bad() {
        Some((
            crate::theme::Tone::Danger,
            format!("pipeline failing on {branch}"),
        ))
    } else if latest.outcome.is_running() {
        Some((
            crate::theme::Tone::Accent,
            format!("pipeline running on {branch}"),
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_commands() {
        assert!(script("forge-tab", "review").is_some());
        assert!(script("forge-tab", "pipelines").is_some());
        assert!(script("forge-tab", "x").is_none());
        assert_eq!(script("mr", "2").unwrap().len(), 2);
        assert!(script("mr", "x").is_none());
        assert!(script("pipeline", "0").is_some());
        assert!(script("forge-job", "1").is_some());
        assert!(script("forge-new", "").is_some());
        assert!(script("other", "").is_none());
    }

    #[test]
    fn azure_cancel_call() {
        let t = Target::of(
            &gitvis_core::devops::forge::detect("https://dev.azure.com/acme/Web/_git/frontend")
                .unwrap(),
        )
        .unwrap();
        let c = azure_cancel(&t, 9);
        assert!(c.write);
        assert_eq!(
            c.display(),
            "az pipelines build cancel --build-id 9 --org https://dev.azure.com/acme --project Web -o json"
        );
    }
}
