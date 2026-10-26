//! Review loop (the Review page, `Page::Review`): pull requests (list with
//! filters, detail with conversation, files, commits and checks; merge box
//! with merge / squash / rebase, auto-merge, update branch, draft state,
//! close and reopen; reviewing in the diff with threads, pending comments,
//! suggestions and replies; creating a pull request) and issues (list,
//! detail, comments, close with a reason, labels and assignees, a branch
//! for an issue, creating an issue).
//!
//! Reads go through `Ctx::gh_read` and land in [`State`] as [`Fetch`]
//! slots (one in-flight guard each). Every change is a [`Change`]: its
//! steps (REST requests and GraphQL mutations built in
//! `gitvis_core::devops::review`) are shown in a confirmation first and run
//! in order by `rest::run_steps` once confirmed.

mod conversation;
mod files;
mod forms;
mod issues;
mod view;
mod widgets;

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use gitvis_core::Oid;
use gitvis_core::devops::gh::Gh;
use gitvis_core::devops::review::rest::{self, CloseReason, Num};
use gitvis_core::devops::review::{
    DraftComment, Issue, IssueComment, ListKind, MergeMethod, PrCommit, PrDetail, PrFile, PrList,
    PrSummary, RepoMeta, ReviewEvent, Step, Suggestions, fetch, graphql, local, patch,
};
use gitvis_core::ops::GitOp;
use serde::{Deserialize, Serialize};

use crate::devops::console::{Console, ConsoleRun};
use crate::devops::ctx::{Ctx, TaskLog};
use crate::format;
use crate::state::{Action, Dialog, Page, RepoState, Settings, Span, ToastKind};

pub use widgets::pr_for_commit;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// A fetched value with its loading flag, error and time.
pub struct Fetch<T> {
    pub data: Option<T>,
    pub error: Option<String>,
    pub loading: bool,
    pub at: Option<Instant>,
}

impl<T> Default for Fetch<T> {
    fn default() -> Self {
        Self {
            data: None,
            error: None,
            loading: false,
            at: None,
        }
    }
}

impl<T> Fetch<T> {
    /// Mark as loading; false when a request is already in flight.
    pub fn begin(&mut self) -> bool {
        if self.loading {
            return false;
        }
        self.loading = true;
        true
    }

    /// Store a result; an error keeps the previous data.
    pub fn finish(&mut self, r: Result<T, String>) {
        self.loading = false;
        self.at = Some(Instant::now());
        match r {
            Ok(d) => {
                self.data = Some(d);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Never asked for (nothing loaded, nothing failed, nothing running).
    pub fn wanted(&self) -> bool {
        self.data.is_none() && self.error.is_none() && !self.loading
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Mode {
    #[default]
    Prs,
    Issues,
}

/// The chips above the pull request list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PrFilter {
    #[default]
    Open,
    Draft,
    ReviewRequested,
    Mine,
    Closed,
}

impl PrFilter {
    pub const ALL: [PrFilter; 5] = [
        PrFilter::Open,
        PrFilter::Draft,
        PrFilter::ReviewRequested,
        PrFilter::Mine,
        PrFilter::Closed,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PrFilter::Open => "Open",
            PrFilter::Draft => "Draft",
            PrFilter::ReviewRequested => "Review requested",
            PrFilter::Mine => "Mine",
            PrFilter::Closed => "Closed / merged",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "open" => PrFilter::Open,
            "draft" | "drafts" => PrFilter::Draft,
            "requested" | "review-requested" | "review" => PrFilter::ReviewRequested,
            "mine" | "me" => PrFilter::Mine,
            "closed" | "merged" => PrFilter::Closed,
            _ => return None,
        })
    }

    fn kind(self) -> ListKind {
        if self == PrFilter::Closed {
            ListKind::Closed
        } else {
            ListKind::Open
        }
    }
}

/// Sub-tabs of a pull request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PrTab {
    #[default]
    Conversation,
    Files,
    Commits,
    Checks,
}

impl PrTab {
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "conversation" | "conv" => PrTab::Conversation,
            "files" | "diff" => PrTab::Files,
            "commits" => PrTab::Commits,
            "checks" | "ci" => PrTab::Checks,
            _ => return None,
        })
    }
}

/// The chips above the issue list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
pub enum IssueFilter {
    #[default]
    Open,
    Closed,
    Assigned,
    Created,
    Mentioned,
}

impl IssueFilter {
    pub const ALL: [IssueFilter; 5] = [
        IssueFilter::Open,
        IssueFilter::Closed,
        IssueFilter::Assigned,
        IssueFilter::Created,
        IssueFilter::Mentioned,
    ];

    pub fn label(self) -> &'static str {
        match self {
            IssueFilter::Open => "Open",
            IssueFilter::Closed => "Closed",
            IssueFilter::Assigned => "Assigned to me",
            IssueFilter::Created => "Created by me",
            IssueFilter::Mentioned => "Mentioning me",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "open" => IssueFilter::Open,
            "closed" => IssueFilter::Closed,
            "assigned" | "mine" => IssueFilter::Assigned,
            "created" => IssueFilter::Created,
            "mentioned" => IssueFilter::Mentioned,
            _ => return None,
        })
    }

    fn needs_login(self) -> bool {
        matches!(
            self,
            IssueFilter::Assigned | IssueFilter::Created | IssueFilter::Mentioned
        )
    }
}

/// Which issue list a fetch fills.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct IssueKey {
    pub filter: IssueFilter,
    pub label: Option<String>,
}

/// Lines selected in a file's diff (positions in its parsed patch).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineSel {
    pub path: String,
    pub anchor: patch::LinePos,
    pub end: patch::LinePos,
}

impl LineSel {
    pub fn range(&self) -> (patch::LinePos, patch::LinePos) {
        if self.anchor <= self.end {
            (self.anchor, self.end)
        } else {
            (self.end, self.anchor)
        }
    }

    pub fn contains(&self, p: patch::LinePos) -> bool {
        let (a, b) = self.range();
        a <= p && p <= b
    }
}

/// A file's patch parsed for display, with syntax colours and word
/// emphasis when the core diff parser agrees on the lines.
pub struct Parsed {
    pub sha: String,
    pub dark: bool,
    pub hunks: Vec<patch::PatchHunk>,
    /// hunk → line → spans.
    pub spans: Vec<Vec<Vec<Span>>>,
    /// hunk → line → changed char ranges.
    pub emph: Vec<Vec<Vec<(u32, u32)>>>,
}

/// The merge box's choices.
#[derive(Default)]
pub struct MergeForm {
    pub method: Option<MergeMethod>,
    pub title: String,
    pub message: String,
    pub delete_branch: Option<bool>,
    /// The title the repository's defaults give; the title follows it
    /// until the user edits it.
    pub default_title: String,
}

/// Everything about one pull request the page holds.
#[derive(Default)]
pub struct PrView {
    pub detail: Fetch<PrDetail>,
    pub files: Fetch<Vec<PrFile>>,
    pub commits: Fetch<Vec<PrCommit>>,
    /// Selected file path.
    pub file: Option<String>,
    pub file_filter: String,
    pub parsed: HashMap<String, Parsed>,
    pub sel: Option<LineSel>,
    pub dragging: bool,
    /// The comment being written on the selected lines.
    pub composer: String,
    pub composer_error: String,
    /// Review comments kept locally until the review is submitted.
    pub pending: Vec<DraftComment>,
    pub review_open: bool,
    pub review_body: String,
    pub review_event: ReviewEvent,
    /// Reply text per thread id.
    pub replies: HashMap<String, String>,
    /// Threads the user opened (resolved and outdated start closed).
    pub opened: HashSet<String>,
    /// Threads the user closed.
    pub closed: HashSet<String>,
    /// The conversation's comment box.
    pub comment: String,
    pub merge: MergeForm,
}

/// The New pull request form.
#[derive(Default)]
pub struct NewPrForm {
    pub base: String,
    /// The local branch.
    pub head: String,
    /// What the API gets as `head`: the remote branch, `owner:branch` for a fork.
    pub head_ref: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
    pub reviewers: Vec<String>,
    pub reviewer_buf: String,
    pub teams: Vec<String>,
    pub team_buf: String,
    pub assignees: Vec<String>,
    pub assignee_buf: String,
    pub labels: Vec<String>,
    pub label_buf: String,
    pub templates: Option<Result<Vec<local::Template>, String>>,
    pub template: Option<usize>,
    /// Commits in head but not in base (newest first), from the local log.
    pub commits: Option<Result<Vec<(String, String)>, String>>,
    /// The ref the log compared against (`origin/main`).
    pub base_ref: String,
    pub title_edited: bool,
    pub error: String,
}

/// The New issue form.
#[derive(Default)]
pub struct NewIssueForm {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub label_buf: String,
    pub assignees: Vec<String>,
    pub assignee_buf: String,
    pub templates: Option<Vec<local::IssueTemplate>>,
    pub template: Option<usize>,
    pub error: String,
}

/// Editing an issue's labels and assignees.
pub struct IssueEdit {
    pub number: u64,
    pub labels: Vec<String>,
    pub label_buf: String,
    pub assignees: Vec<String>,
    pub assignee_buf: String,
}

/// "Start a branch" for an issue.
pub struct BranchForm {
    pub number: u64,
    pub name: String,
    pub from_current: bool,
}

#[derive(Default)]
pub struct IssuesState {
    pub filter: IssueFilter,
    pub label: Option<String>,
    pub search: String,
    pub lists: HashMap<IssueKey, Fetch<Vec<Issue>>>,
    pub selected: Option<u64>,
    pub details: HashMap<u64, Fetch<(Issue, Vec<IssueComment>)>>,
    pub comment: HashMap<u64, String>,
    pub reason: CloseReason,
    pub edit: Option<IssueEdit>,
    pub branch: Option<BranchForm>,
    pub form: Option<NewIssueForm>,
    pub form_open: bool,
    /// The authenticated user (for the "me" filters).
    pub login: Option<String>,
    /// A script asked for the N-th issue before the list arrived.
    pub pending_listed: Option<usize>,
}

impl IssuesState {
    pub fn key(&self) -> IssueKey {
        IssueKey {
            filter: self.filter,
            label: self.label.clone(),
        }
    }

    pub fn current(&self) -> Option<&Fetch<Vec<Issue>>> {
        self.lists.get(&self.key())
    }

    /// Issues of the current list that match the search box.
    pub fn visible(&self) -> Vec<&Issue> {
        let needle = self.search.trim().to_lowercase();
        let Some(list) = self.current().and_then(|f| f.data.as_ref()) else {
            return Vec::new();
        };
        list.iter()
            .filter(|i| {
                needle.is_empty()
                    || i.title.to_lowercase().contains(&needle)
                    || i.author.to_lowercase().contains(&needle)
                    || i.number.to_string() == needle.trim_start_matches('#')
                    || i.labels
                        .iter()
                        .any(|l| l.name.to_lowercase().contains(&needle))
                    || i.assignees
                        .iter()
                        .any(|a| a.to_lowercase().contains(&needle))
            })
            .collect()
    }

    /// An issue from the loaded detail or any loaded list.
    pub fn find(&self, n: u64) -> Option<&Issue> {
        self.details
            .get(&n)
            .and_then(|f| f.data.as_ref())
            .map(|(i, _)| i)
            .or_else(|| {
                self.lists
                    .values()
                    .filter_map(|f| f.data.as_ref())
                    .flat_map(|l| l.iter())
                    .find(|i| i.number == n)
            })
    }
}

#[derive(Default)]
pub struct State {
    pub mode: Mode,
    pub filter: PrFilter,
    pub search: String,
    pub selected: Option<u64>,
    pub tab: PrTab,
    /// `GET repos/o/r`.
    pub meta: Fetch<RepoMeta>,
    pub open: Fetch<PrList>,
    pub closed: Fetch<PrList>,
    pub more_loading: bool,
    pub prs: HashMap<u64, PrView>,
    pub new_pr: Option<NewPrForm>,
    pub form_open: bool,
    pub suggestions: Fetch<Suggestions>,
    pub issues: IssuesState,
    /// `number:path:blob` of files marked as viewed.
    pub viewed: HashSet<String>,
    pub viewed_order: Vec<String>,
    /// Label of a change that is running.
    pub changing: Option<String>,
    /// A script asked for the N-th pull request before the list arrived.
    pub pending_listed: Option<usize>,
    /// A script asked for the N-th file before the files arrived.
    pub pending_file: Option<usize>,
}

/// Files remembered as viewed (oldest dropped first).
const MAX_VIEWED: usize = 4_000;

impl State {
    pub fn prefs(&self) -> Prefs {
        Prefs {
            mode: self.mode,
            filter: self.filter,
            issue_filter: self.issues.filter,
            viewed: self.viewed_order.clone(),
        }
    }

    pub fn apply_prefs(&mut self, p: &Prefs) {
        self.mode = p.mode;
        self.filter = p.filter;
        self.issues.filter = p.issue_filter;
        self.viewed_order = p.viewed.clone();
        self.viewed = p.viewed.iter().cloned().collect();
    }

    /// REST settings completed by the GraphQL ones of the list query.
    pub fn meta(&self) -> RepoMeta {
        let mut m = self.meta.data.clone().unwrap_or_default();
        if let Some(l) = self.open.data.as_ref().or(self.closed.data.as_ref()) {
            m.fill(&l.meta);
        }
        m
    }

    pub fn viewer(&self) -> Option<&str> {
        self.open
            .data
            .as_ref()
            .or(self.closed.data.as_ref())
            .map(|l| l.viewer.as_str())
            .filter(|v| !v.is_empty())
            .or(self.issues.login.as_deref())
    }

    pub fn list(&self, kind: ListKind) -> &Fetch<PrList> {
        match kind {
            ListKind::Open => &self.open,
            ListKind::Closed => &self.closed,
        }
    }

    fn list_mut(&mut self, kind: ListKind) -> &mut Fetch<PrList> {
        match kind {
            ListKind::Open => &mut self.open,
            ListKind::Closed => &mut self.closed,
        }
    }

    /// The list the current chip reads from.
    pub fn current_list(&self) -> &Fetch<PrList> {
        self.list(self.filter.kind())
    }

    /// Pull requests the chips and the search box let through.
    pub fn visible(&self) -> Vec<&PrSummary> {
        let needle = self.search.trim().to_lowercase();
        let open = self.open.data.as_ref();
        let base: Vec<&PrSummary> = match self.filter {
            PrFilter::Open => open.iter().flat_map(|l| l.items.iter()).collect(),
            PrFilter::Draft => open
                .iter()
                .flat_map(|l| l.items.iter())
                .filter(|p| p.draft)
                .collect(),
            PrFilter::ReviewRequested => open.iter().flat_map(|l| l.requested.iter()).collect(),
            PrFilter::Mine => open.iter().flat_map(|l| l.mine.iter()).collect(),
            PrFilter::Closed => self
                .closed
                .data
                .iter()
                .flat_map(|l| l.items.iter())
                .collect(),
        };
        base.into_iter()
            .filter(|p| {
                needle.is_empty()
                    || p.title.to_lowercase().contains(&needle)
                    || p.author.to_lowercase().contains(&needle)
                    || p.head.to_lowercase().contains(&needle)
                    || p.base.to_lowercase().contains(&needle)
                    || p.number.to_string() == needle.trim_start_matches('#')
                    || p.labels
                        .iter()
                        .any(|l| l.name.to_lowercase().contains(&needle))
            })
            .collect()
    }

    /// A pull request from its detail or any list.
    pub fn find(&self, n: u64) -> Option<&PrSummary> {
        if let Some(d) = self.prs.get(&n).and_then(|v| v.detail.data.as_ref()) {
            return Some(&d.pr);
        }
        [&self.open, &self.closed]
            .into_iter()
            .filter_map(|f| f.data.as_ref())
            .flat_map(|l| l.items.iter().chain(&l.requested).chain(&l.mine))
            .find(|p| p.number == n)
    }

    /// Whether the viewer's review is requested on `n`.
    pub fn is_requested(&self, n: u64) -> bool {
        self.open
            .data
            .as_ref()
            .is_some_and(|l| l.requested.iter().any(|p| p.number == n))
    }

    pub fn is_viewed(&self, n: u64, f: &PrFile) -> bool {
        self.viewed.contains(&viewed_key(n, f))
    }

    fn toggle_viewed(&mut self, key: String) {
        if self.viewed.remove(&key) {
            self.viewed_order.retain(|k| *k != key);
        } else {
            self.viewed.insert(key.clone());
            self.viewed_order.push(key);
            if self.viewed_order.len() > MAX_VIEWED {
                let extra = self.viewed_order.len() - MAX_VIEWED;
                for k in self.viewed_order.drain(..extra) {
                    self.viewed.remove(&k);
                }
            }
        }
    }
}

pub fn viewed_key(n: u64, f: &PrFile) -> String {
    format!("{n}:{}:{}", f.path, f.sha)
}

/// Persisted per repository.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub mode: Mode,
    pub filter: PrFilter,
    pub issue_filter: IssueFilter,
    /// Files marked as viewed (`number:path:blob`), oldest first.
    pub viewed: Vec<String>,
}

// ---------------------------------------------------------------------------
// Actions and replies
// ---------------------------------------------------------------------------

/// What happens after a change, success or not.
#[derive(Clone, Debug)]
pub enum After {
    /// Reload the pull request.
    Pr(u64),
    /// A review went out: clear the pending comments.
    Review(u64),
    /// A conversation comment went out.
    Comment(u64),
    /// A single line comment went out.
    LineComment(u64),
    /// A reply went out on a thread.
    Reply(u64, String),
    /// A pull request was created (its number comes back).
    Created,
    Issue(u64),
    IssueComment(u64),
    IssueEdit(u64),
    IssueCreated,
}

/// A change on GitHub: shown in a confirmation, then run step by step.
#[derive(Clone, Debug)]
pub struct Change {
    pub label: String,
    /// The toast on success.
    pub success: String,
    pub steps: Vec<Step>,
    pub after: After,
}

#[derive(Clone, Debug)]
pub enum Act {
    /// Show a pull request (from the graph, the checks section, a script).
    OpenPr(u64),
    Refresh,
    LoadMore,
    SetMode(Mode),
    SetFilter(PrFilter),
    SetTab(PrTab),
    /// The N-th pull request of the visible list (script).
    OpenListed(usize),
    RefreshPr(u64),
    SelectFile(String),
    /// The N-th file of the selected pull request (script).
    SelectFileIndex(usize),
    ToggleViewed(String),
    /// The composer's text as a pending review comment.
    AddPending(u64),
    RemovePending(u64, usize),
    /// The composer's text as a comment outside a review.
    SingleComment(u64),
    /// Prefill the composer with a suggestion of the selected lines.
    Suggest(u64),
    SubmitReview(u64),
    Reply {
        pr: u64,
        thread: String,
    },
    Resolve {
        pr: u64,
        thread: String,
        resolve: bool,
    },
    /// The conversation's comment box.
    PostComment(u64),
    Merge(u64),
    AutoMerge {
        pr: u64,
        enable: bool,
    },
    UpdateBranch(u64),
    SetDraft {
        pr: u64,
        draft: bool,
    },
    SetOpen {
        pr: u64,
        open: bool,
    },
    Checkout(u64),
    /// Run a confirmed change.
    Run(Change),
    /// Run confirmed git operations.
    RunOps(Vec<GitOp>),
    NewPr,
    CloseForm,
    DiscardForm,
    GenerateBody,
    UseTemplate(usize),
    CreatePr,
    PushFirst,
    LoadSuggestions,
    // Issues.
    OpenIssue(u64),
    OpenListedIssue(usize),
    SetIssueFilter(IssueFilter),
    SetIssueLabel(Option<String>),
    RefreshIssue(u64),
    IssueComment(u64),
    SetIssueState {
        number: u64,
        open: bool,
        reason: CloseReason,
    },
    EditIssue(u64),
    SaveIssueEdit(u64),
    StartBranch(u64),
    NewIssue,
    UseIssueTemplate(usize),
    CreateIssue,
    /// Script commands that need the state.
    Script(String, String),
}

/// What the local read for the forms found.
pub struct LocalInfo {
    pub templates: Vec<local::Template>,
    pub issue_templates: Vec<local::IssueTemplate>,
    pub commits: Option<Result<Vec<(String, String)>, String>>,
    pub base_ref: String,
}

pub enum Msg {
    Meta(Result<RepoMeta, String>),
    List {
        kind: ListKind,
        append: bool,
        result: Result<PrList, String>,
    },
    Detail(u64, Result<PrDetail, String>),
    Files(u64, Result<Vec<PrFile>, String>),
    Commits(u64, Result<Vec<PrCommit>, String>),
    Suggestions(Result<Suggestions, String>),
    Local(Box<LocalInfo>),
    Issues {
        key: IssueKey,
        login: Option<String>,
        result: Result<Vec<Issue>, String>,
    },
    Issue(u64, Result<(Issue, Vec<IssueComment>), String>),
    Changed {
        after: After,
        ok: bool,
        number: Option<u64>,
    },
}

impl Msg {
    /// One line for the activity log (`Err` when the work failed).
    pub fn summary(&self) -> Result<String, String> {
        match self {
            Msg::Meta(r) => r.clone().map(|m| {
                format!(
                    "default branch {}, your permission {}",
                    m.default_branch,
                    m.permission.to_lowercase()
                )
            }),
            Msg::List { kind, result, .. } => result.clone().map(|l| {
                let what = match kind {
                    ListKind::Open => "open",
                    ListKind::Closed => "closed or merged",
                };
                format!(
                    "{} of {} {what} pull requests{}, {} waiting for your review, {} yours",
                    l.items.len(),
                    format::count(l.total as usize),
                    if l.light {
                        " (without mergeability: the full query timed out)"
                    } else {
                        ""
                    },
                    l.requested_total,
                    l.mine_total
                )
            }),
            Msg::Detail(n, r) => r.clone().map(|d| {
                format!(
                    "#{n}: {} timeline entries, {} review threads ({} unresolved)",
                    d.timeline.len(),
                    d.threads.len(),
                    d.unresolved_threads()
                )
            }),
            Msg::Files(n, r) => r
                .clone()
                .map(|f| format!("#{n}: {}", format::plural(f.len(), "file", "files"))),
            Msg::Commits(n, r) => r
                .clone()
                .map(|c| format!("#{n}: {}", format::plural(c.len(), "commit", "commits"))),
            Msg::Suggestions(r) => r.clone().map(|s| {
                format!(
                    "{} people (from {}), {} labels, {} teams",
                    s.users.len(),
                    s.users_source,
                    s.labels.len(),
                    s.teams.len()
                )
            }),
            Msg::Local(info) => Ok(format!(
                "{} pull request templates, {} issue templates{}",
                info.templates.len(),
                info.issue_templates.len(),
                match &info.commits {
                    Some(Ok(c)) => format!(
                        ", {} since {}",
                        format::plural(c.len(), "commit", "commits"),
                        info.base_ref
                    ),
                    _ => String::new(),
                }
            )),
            Msg::Issues { result, .. } => result
                .as_ref()
                .map(|l| format::plural(l.len(), "issue", "issues"))
                .map_err(Clone::clone),
            Msg::Issue(n, r) => r
                .as_ref()
                .map(|(_, c)| format!("#{n}: {}", format::plural(c.len(), "comment", "comments")))
                .map_err(Clone::clone),
            Msg::Changed { ok, .. } => {
                if *ok {
                    Ok("done".to_owned())
                } else {
                    Err("failed".to_owned())
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

fn err(e: gitvis_core::Error) -> String {
    e.to_string()
}

const NO_REMOTE: &str = "This repository has no GitHub remote.";

/// Start a GitHub read into `slot` unless one is running.
fn gh_fetch<T>(
    cx: &mut Ctx,
    slot: &mut Fetch<T>,
    label: String,
    f: impl FnOnce(&Gh, &mut TaskLog) -> Msg + Send + 'static,
) {
    if !slot.begin() {
        return;
    }
    if !cx.gh_read(label, f) {
        slot.loading = false;
        slot.error = Some(NO_REMOTE.to_owned());
    }
}

fn fetch_meta(cx: &mut Ctx, st: &mut State) {
    gh_fetch(
        cx,
        &mut st.meta,
        "Repository settings".to_owned(),
        |gh, log| Msg::Meta(fetch::fetch_repo_meta(gh, &mut log.calls).map_err(err)),
    );
}

fn fetch_list(cx: &mut Ctx, st: &mut State, kind: ListKind, append: bool) {
    let after = if append {
        let Some(c) = st
            .list(kind)
            .data
            .as_ref()
            .and_then(|l| l.end_cursor.clone())
        else {
            return;
        };
        if st.more_loading {
            return;
        }
        st.more_loading = true;
        Some(c)
    } else {
        if !st.list_mut(kind).begin() {
            return;
        }
        None
    };
    let label = match (kind, append) {
        (ListKind::Open, false) => "Open pull requests",
        (ListKind::Open, true) => "More open pull requests",
        (ListKind::Closed, false) => "Closed pull requests",
        (ListKind::Closed, true) => "More closed pull requests",
    };
    let started = cx.gh_read(label, move |gh, log| Msg::List {
        kind,
        append,
        result: fetch::fetch_pr_list(gh, kind, after.as_deref(), &mut log.calls).map_err(err),
    });
    if !started {
        st.more_loading = false;
        let l = st.list_mut(kind);
        l.loading = false;
        l.error = Some(NO_REMOTE.to_owned());
    }
}

fn fetch_detail(cx: &mut Ctx, st: &mut State, n: u64) {
    let v = st.prs.entry(n).or_default();
    gh_fetch(
        cx,
        &mut v.detail,
        format!("Pull request #{n}"),
        move |gh, log| {
            Msg::Detail(
                n,
                fetch::fetch_pr_detail(gh, n, &mut log.calls).map_err(err),
            )
        },
    );
}

fn fetch_files(cx: &mut Ctx, st: &mut State, n: u64) {
    let v = st.prs.entry(n).or_default();
    gh_fetch(
        cx,
        &mut v.files,
        format!("Files of pull request #{n}"),
        move |gh, log| Msg::Files(n, fetch::fetch_pr_files(gh, n, &mut log.calls).map_err(err)),
    );
}

fn fetch_commits(cx: &mut Ctx, st: &mut State, n: u64) {
    let v = st.prs.entry(n).or_default();
    gh_fetch(
        cx,
        &mut v.commits,
        format!("Commits of pull request #{n}"),
        move |gh, log| {
            Msg::Commits(
                n,
                fetch::fetch_pr_commits(gh, n, &mut log.calls).map_err(err),
            )
        },
    );
}

fn fetch_suggestions(cx: &mut Ctx, st: &mut State) {
    let org = st.meta().owner_is_org.unwrap_or(false);
    gh_fetch(
        cx,
        &mut st.suggestions,
        "People, labels and teams".to_owned(),
        move |gh, log| {
            Msg::Suggestions(fetch::fetch_suggestions(gh, org, &mut log.calls).map_err(err))
        },
    );
}

fn fetch_issues(cx: &mut Ctx, st: &mut State) {
    let key = st.issues.key();
    let login = st.viewer().map(str::to_owned);
    let label = format!(
        "Issues: {}{}",
        key.filter.label().to_lowercase(),
        key.label
            .as_deref()
            .map(|l| format!(", label {l}"))
            .unwrap_or_default()
    );
    let slot = st.issues.lists.entry(key.clone()).or_default();
    gh_fetch(cx, slot, label, move |gh, log| {
        let me = if key.filter.needs_login() && login.is_none() {
            gh.login(&mut log.calls).ok()
        } else {
            login
        };
        let pick = |f: IssueFilter| (key.filter == f).then(|| me.clone()).flatten();
        let q = fetch::IssueQuery {
            open: key.filter != IssueFilter::Closed,
            assignee: pick(IssueFilter::Assigned),
            creator: pick(IssueFilter::Created),
            mentioned: pick(IssueFilter::Mentioned),
            labels: key.label.iter().cloned().collect(),
        };
        let result = if key.filter.needs_login() && me.is_none() {
            Err("Could not tell who you are on GitHub (gh api user failed).".to_owned())
        } else {
            fetch::fetch_issues(gh, &q, 2, &mut log.calls).map_err(err)
        };
        Msg::Issues {
            key,
            login: me,
            result,
        }
    });
}

fn fetch_issue(cx: &mut Ctx, st: &mut State, n: u64) {
    let slot = st.issues.details.entry(n).or_default();
    gh_fetch(cx, slot, format!("Issue #{n}"), move |gh, log| {
        Msg::Issue(n, fetch::fetch_issue(gh, n, &mut log.calls).map_err(err))
    });
}

/// Templates (and, for the New pull request form, the commits between base
/// and head) from the working tree.
fn read_local(cx: &mut Ctx, base_candidates: Vec<String>, head: Option<String>) {
    cx.read("Templates and commits", move |dir, log| {
        let templates = local::pr_templates(dir);
        let issue_templates = local::issue_templates(dir);
        let base_ref = local::first_resolving(dir, &base_candidates).unwrap_or_default();
        let commits = match (&head, base_ref.is_empty()) {
            (Some(h), false) => {
                log.command(local::commits_between_command(&base_ref, h, 200));
                Some(local::commits_between(dir, &base_ref, h, 200).map_err(err))
            }
            (Some(_), true) => Some(Err(format!(
                "None of {} exists locally; fetch first.",
                base_candidates.join(", ")
            ))),
            (None, _) => None,
        };
        Msg::Local(Box::new(LocalInfo {
            templates,
            issue_templates,
            commits,
            base_ref,
        }))
    });
}

/// Fetch what the current tab of the selected pull request needs.
fn tab_shown(cx: &mut Ctx, repo: &mut RepoState) {
    let st = &mut repo.devops.review;
    let Some(n) = st.selected else {
        return;
    };
    match st.tab {
        PrTab::Conversation => {}
        PrTab::Files => {
            if st.prs.get(&n).is_none_or(|v| v.files.wanted()) {
                fetch_files(cx, st, n);
            }
        }
        PrTab::Commits => {
            if st.prs.get(&n).is_none_or(|v| v.commits.wanted()) {
                fetch_commits(cx, st, n);
            }
        }
        PrTab::Checks => {
            if let Some(oid) = st.find(n).and_then(|p| Oid::from_str(&p.head_oid).ok())
                && !repo.checks.contains_key(&oid)
            {
                cx.action(Action::RefreshChecks(oid));
            }
        }
    }
}

fn select_pr(cx: &mut Ctx, repo: &mut RepoState, n: u64) {
    let st = &mut repo.devops.review;
    st.mode = Mode::Prs;
    st.form_open = false;
    st.selected = Some(n);
    if st.prs.get(&n).is_none_or(|v| v.detail.wanted()) {
        fetch_detail(cx, st, n);
    }
    tab_shown(cx, repo);
}

fn select_issue(cx: &mut Ctx, st: &mut State, n: u64) {
    st.mode = Mode::Issues;
    st.issues.form_open = false;
    st.issues.selected = Some(n);
    if st.issues.details.get(&n).is_none_or(|f| f.wanted()) {
        fetch_issue(cx, st, n);
    }
}

// ---------------------------------------------------------------------------
// Changes
// ---------------------------------------------------------------------------

fn confirm_change(
    cx: &mut Ctx,
    title: impl Into<String>,
    body: impl Into<String>,
    confirm: &str,
    danger: bool,
    change: Change,
) {
    let details = change.steps.iter().map(Step::confirmation).collect();
    cx.confirm(
        title,
        body,
        confirm,
        danger,
        details,
        Act::Run(change).into(),
    );
}

fn run_change(cx: &mut Ctx, st: &mut State, change: Change) {
    if let Some(running) = &st.changing {
        cx.toast(
            ToastKind::Info,
            format!("Wait for “{running}” to finish first."),
        );
        return;
    }
    let Change {
        label,
        success,
        steps,
        after,
    } = change;
    st.changing = Some(label.clone());
    let started = cx.gh_change(label, move |gh, log| {
        let out = rest::run_steps(gh, &steps, &mut log.calls);
        let ok = out.error.is_none();
        let result = match &out.error {
            None => Ok(success),
            Some((i, e)) => {
                let mut msg = String::new();
                if steps.len() > 1 {
                    msg.push_str(&format!("Request {} of {} failed: ", i + 1, steps.len()));
                }
                msg.push_str(e);
                if out.done > 0 {
                    let done: Vec<String> = steps[..out.done].iter().map(Step::display).collect();
                    msg.push_str(&format!("\nAlready done: {}", done.join("; ")));
                }
                Err(msg)
            }
        };
        (
            result,
            Some(Msg::Changed {
                after,
                ok,
                number: out.number,
            }),
        )
    });
    if !started {
        st.changing = None;
    }
}

/// The detail of `n`, or a toast saying it is not loaded.
fn loaded_detail(cx: &mut Ctx, st: &State, n: u64) -> Option<PrDetail> {
    let d = st.prs.get(&n).and_then(|v| v.detail.data.clone());
    if d.is_none() {
        cx.toast(
            ToastKind::Info,
            format!("Pull request #{n} is still loading."),
        );
    }
    d
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

/// The comment the composer would send, anchored on the selected lines.
fn composed(st: &State, n: u64) -> Result<DraftComment, String> {
    let v = st.prs.get(&n).ok_or("Nothing selected.")?;
    let sel = v.sel.as_ref().ok_or("Select a line first.")?;
    let file = v
        .files
        .data
        .as_ref()
        .and_then(|f| f.iter().find(|f| f.path == sel.path))
        .ok_or("The file is not loaded.")?;
    let hunks = patch::parse_patch(file.patch.as_deref().unwrap_or(""));
    let (a, b) = sel.range();
    let (start, end) = patch::range_anchor(&hunks, a, b).map_err(str::to_owned)?;
    let body = v.composer.trim_end().to_owned();
    if body.trim().is_empty() {
        return Err("Write the comment first.".to_owned());
    }
    Ok(DraftComment {
        path: sel.path.clone(),
        line: end.1,
        side: end.0,
        start_line: start.map(|s| s.1),
        start_side: start.map(|s| s.0),
        body,
    })
}

fn merge_change(cx: &mut Ctx, st: &State, gh: &Gh, n: u64, auto: bool) -> Option<Change> {
    let d = loaded_detail(cx, st, n)?;
    let meta = st.meta();
    let form = &st.prs.get(&n)?.merge;
    let method = form
        .method
        .or(meta.preferred_method())
        .unwrap_or(MergeMethod::Merge);
    let title = method.has_message().then_some(form.title.as_str());
    let message = method.has_message().then_some(form.message.as_str());
    let mut steps = Vec::new();
    if auto {
        steps.push(Step::Gql(graphql::enable_auto_merge(
            &d.pr.id,
            method,
            title,
            message,
            &d.pr.head_oid,
        )));
    } else {
        steps.push(Step::Rest(rest::merge_pr(
            gh,
            n,
            method,
            &d.pr.head_oid,
            title,
            message,
        )));
        let repo_deletes = meta.delete_branch_on_merge == Some(true);
        let delete = form.delete_branch.unwrap_or(repo_deletes);
        if delete && !repo_deletes && d.pr.head_in_repo(&gh.full_name()) {
            steps.push(Step::Rest(rest::delete_branch(gh, &d.pr.head)));
        }
    }
    Some(Change {
        label: if auto {
            format!("Enable auto-merge on #{n}")
        } else {
            format!("Merge #{n}")
        },
        success: if auto {
            format!(
                "#{n} will be merged ({}) once its requirements are met",
                method.rest()
            )
        } else {
            format!("Merged #{n} into {} ({})", d.pr.base, method.rest())
        },
        steps,
        after: After::Pr(n),
    })
}

fn new_pr_change(st: &mut State, gh: &Gh) -> Option<Change> {
    let f = st.new_pr.as_mut()?;
    f.error.clear();
    if f.title.trim().is_empty() {
        f.error = "Give the pull request a title.".to_owned();
        return None;
    }
    if f.head_ref.is_empty() || f.base.is_empty() {
        f.error = "Choose the base and head branches.".to_owned();
        return None;
    }
    if f.head == f.base {
        f.error = format!("{} cannot be merged into itself.", f.head);
        return None;
    }
    let mut steps = vec![Step::Rest(rest::create_pr(
        gh,
        &rest::NewPr {
            title: f.title.clone(),
            head: f.head_ref.clone(),
            base: f.base.clone(),
            body: f.body.clone(),
            draft: f.draft,
        },
    ))];
    if !f.labels.is_empty() {
        steps.push(Step::Rest(rest::add_labels(gh, Num::Created, &f.labels)));
    }
    if !f.reviewers.is_empty() || !f.teams.is_empty() {
        steps.push(Step::Rest(rest::request_reviewers(
            gh,
            Num::Created,
            &f.reviewers,
            &f.teams,
        )));
    }
    if !f.assignees.is_empty() {
        steps.push(Step::Rest(rest::add_assignees(
            gh,
            Num::Created,
            &f.assignees,
        )));
    }
    Some(Change {
        label: format!("Open pull request {} → {}", f.head, f.base),
        success: format!(
            "Opened {} pull request {} → {}",
            if f.draft { "a draft" } else { "a" },
            f.head,
            f.base
        ),
        steps,
        after: After::Created,
    })
}

/// Defaults of the New pull request form from the repository.
fn new_pr_form(repo: &RepoState, meta: &RepoMeta) -> NewPrForm {
    let snap = repo.snapshot.as_ref();
    let current = snap.and_then(|s| s.current_branch());
    let head = current.map(|b| b.name.clone()).unwrap_or_default();
    let mut head_ref = head.clone();
    if let (Some(b), Some(snap)) = (current, snap)
        && let Some(up) = b.upstream.as_deref().filter(|_| !b.upstream_gone)
        && let Some(rb) = snap.remote_branch(up)
    {
        head_ref = rb.branch.clone();
        let owner = snap
            .remotes
            .iter()
            .find(|r| r.name == rb.remote)
            .and_then(|r| gitvis_core::github::parse_remote(&r.url))
            .map(|(o, _)| o);
        if let (Some(owner), Some((repo_owner, _))) = (owner, repo.github_remote.as_ref())
            && !owner.eq_ignore_ascii_case(repo_owner)
        {
            head_ref = format!("{owner}:{head_ref}");
        }
    }
    NewPrForm {
        base: meta.default_branch.clone(),
        head,
        head_ref,
        ..Default::default()
    }
}

/// Where `git log base..head` looks for the base: the GitHub remote's
/// branch first, then a local one.
fn base_candidates(repo: &RepoState, base: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(snap) = repo.snapshot.as_ref() {
        for r in &snap.remotes {
            if gitvis_core::github::parse_remote(&r.url).as_ref() == repo.github_remote.as_ref() {
                out.push(format!("{}/{base}", r.name));
            }
        }
    }
    out.push(format!("origin/{base}"));
    out.push(base.to_owned());
    let mut seen = HashSet::new();
    out.retain(|c| seen.insert(c.clone()));
    out
}

/// The push dialog for the current branch (as the toolbar opens it).
fn push_dialog(repo: &RepoState) -> Option<Dialog> {
    let snap = repo.snapshot.as_ref()?;
    let b = snap.current_branch()?;
    let remotes: Vec<String> = snap.remotes.iter().map(|r| r.name.clone()).collect();
    if remotes.is_empty() {
        return None;
    }
    let upstream_remote = b
        .upstream
        .as_deref()
        .filter(|_| !b.upstream_gone)
        .and_then(|u| snap.remote_branch(u))
        .map(|rb| rb.remote.clone());
    let remote = upstream_remote
        .as_ref()
        .and_then(|r| remotes.iter().position(|x| x == r))
        .or_else(|| remotes.iter().position(|r| r == "origin"))
        .unwrap_or(0);
    Some(Dialog::Push {
        branch: b.name.clone(),
        remotes,
        remote,
        set_upstream: upstream_remote.is_none(),
        force: false,
    })
}

/// The git operation's command lines, for a confirmation.
fn op_details(op: &GitOp) -> Vec<String> {
    op.commands()
        .iter()
        .map(|c| format!("{} {}", op.program(), c.join(" ")))
        .collect()
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    let gh = cx.gh.clone();
    match act {
        Act::OpenPr(n) => {
            let st = &mut repo.devops.review;
            if st.meta.wanted() {
                fetch_meta(cx, st);
            }
            select_pr(cx, repo, n);
        }
        Act::Refresh => refresh(cx, repo),
        Act::LoadMore => {
            let st = &mut repo.devops.review;
            let kind = st.filter.kind();
            fetch_list(cx, st, kind, true);
        }
        Act::SetMode(m) => {
            repo.devops.review.mode = m;
            on_page_shown(cx, repo);
        }
        Act::SetFilter(f) => {
            let st = &mut repo.devops.review;
            st.filter = f;
            let kind = f.kind();
            if st.list(kind).wanted() {
                fetch_list(cx, st, kind, false);
            }
        }
        Act::SetTab(t) => {
            repo.devops.review.tab = t;
            tab_shown(cx, repo);
        }
        Act::OpenListed(i) => {
            let st = &mut repo.devops.review;
            match st.visible().get(i).map(|p| p.number) {
                Some(n) => select_pr(cx, repo, n),
                None => st.pending_listed = Some(i),
            }
        }
        Act::RefreshPr(n) => {
            let st = &mut repo.devops.review;
            fetch_detail(cx, st, n);
            let v = st.prs.entry(n).or_default();
            let (files, commits) = (v.files.data.is_some(), v.commits.data.is_some());
            if files {
                fetch_files(cx, st, n);
            }
            if commits {
                fetch_commits(cx, st, n);
            }
            if st.tab == PrTab::Checks
                && let Some(oid) = st.find(n).and_then(|p| Oid::from_str(&p.head_oid).ok())
            {
                cx.action(Action::RefreshChecks(oid));
            }
        }
        Act::SelectFile(path) => {
            let st = &mut repo.devops.review;
            if let Some(n) = st.selected {
                let v = st.prs.entry(n).or_default();
                if v.file.as_deref() != Some(path.as_str()) {
                    v.file = Some(path);
                    v.sel = None;
                    v.composer_error.clear();
                }
            }
        }
        Act::SelectFileIndex(i) => {
            let st = &mut repo.devops.review;
            st.tab = PrTab::Files;
            let Some(n) = st.selected else {
                return;
            };
            let path = st
                .prs
                .get(&n)
                .and_then(|v| v.files.data.as_ref())
                .and_then(|f| f.get(i))
                .map(|f| f.path.clone());
            match path {
                Some(p) => st.prs.entry(n).or_default().file = Some(p),
                None => {
                    st.pending_file = Some(i);
                    tab_shown(cx, repo);
                }
            }
        }
        Act::ToggleViewed(key) => repo.devops.review.toggle_viewed(key),
        Act::AddPending(n) => {
            let st = &mut repo.devops.review;
            match composed(st, n) {
                Ok(c) => {
                    let v = st.prs.entry(n).or_default();
                    v.pending.push(c);
                    v.composer.clear();
                    v.composer_error.clear();
                    v.sel = None;
                }
                Err(e) => st.prs.entry(n).or_default().composer_error = e,
            }
        }
        Act::RemovePending(n, i) => {
            if let Some(v) = repo.devops.review.prs.get_mut(&n)
                && i < v.pending.len()
            {
                v.pending.remove(i);
            }
        }
        Act::Suggest(n) => suggest(&mut repo.devops.review, n),
        Act::SingleComment(n) => {
            let Some(gh) = gh else { return };
            let st = &mut repo.devops.review;
            let c = match composed(st, n) {
                Ok(c) => c,
                Err(e) => {
                    st.prs.entry(n).or_default().composer_error = e;
                    return;
                }
            };
            let Some(d) = loaded_detail(cx, st, n) else {
                return;
            };
            let change = Change {
                label: format!("Comment on #{n}"),
                success: format!("Commented on {} {}", c.path, c.range_label()),
                steps: vec![Step::Rest(rest::single_comment(&gh, n, &d.pr.head_oid, &c))],
                after: After::LineComment(n),
            };
            confirm_change(
                cx,
                format!("Comment on {} {}?", c.path, c.range_label()),
                "A single comment is published right away, outside a review.",
                "Comment",
                false,
                change,
            );
        }
        Act::SubmitReview(n) => {
            let Some(gh) = gh else { return };
            let st = &mut repo.devops.review;
            let Some(d) = loaded_detail(cx, st, n) else {
                return;
            };
            let v = st.prs.entry(n).or_default();
            if let Some(problem) =
                rest::review_problem(v.review_event, &v.review_body, v.pending.len())
            {
                cx.toast(ToastKind::Info, problem);
                v.review_open = true;
                return;
            }
            let event = v.review_event;
            let change = Change {
                label: format!("Review #{n}"),
                success: format!(
                    "Submitted a review on #{n}: {} with {}",
                    event.label().to_lowercase(),
                    format::plural(v.pending.len(), "comment", "comments")
                ),
                steps: vec![Step::Rest(rest::submit_review(
                    &gh,
                    n,
                    &d.pr.head_oid,
                    &v.review_body,
                    event,
                    &v.pending,
                ))],
                after: After::Review(n),
            };
            let body = format!(
                "Publishes your review with {} on {}.",
                format::plural(v.pending.len(), "line comment", "line comments"),
                short(&d.pr.head_oid)
            );
            confirm_change(
                cx,
                format!("{} on #{n}?", event.label()),
                body,
                event.label(),
                event == ReviewEvent::RequestChanges,
                change,
            );
        }
        Act::Reply { pr, thread } => {
            let Some(gh) = gh else { return };
            let st = &repo.devops.review;
            let Some(d) = loaded_detail(cx, st, pr) else {
                return;
            };
            let Some(t) = d.threads.iter().find(|t| t.id == thread) else {
                return;
            };
            let Some(to) = t.reply_to() else {
                cx.toast(ToastKind::Error, "This thread has no comment to reply to.");
                return;
            };
            let body = st
                .prs
                .get(&pr)
                .and_then(|v| v.replies.get(&thread))
                .cloned()
                .unwrap_or_default();
            if body.trim().is_empty() {
                return;
            }
            let change = Change {
                label: format!("Reply on #{pr}"),
                success: format!("Replied on {} {}", t.path, t.line_label()),
                steps: vec![Step::Rest(rest::reply_to_comment(&gh, pr, to, &body))],
                after: After::Reply(pr, thread.clone()),
            };
            confirm_change(
                cx,
                format!("Reply on {}?", t.path),
                format!("Published right away in the thread on {}.", t.line_label()),
                "Reply",
                false,
                change,
            );
        }
        Act::Resolve {
            pr,
            thread,
            resolve,
        } => {
            let st = &repo.devops.review;
            let Some(d) = loaded_detail(cx, st, pr) else {
                return;
            };
            let place = d
                .threads
                .iter()
                .find(|t| t.id == thread)
                .map(|t| format!("{} {}", t.path, t.line_label()))
                .unwrap_or_default();
            let m = if resolve {
                graphql::resolve_thread(&thread)
            } else {
                graphql::unresolve_thread(&thread)
            };
            let verb = if resolve { "Resolve" } else { "Unresolve" };
            confirm_change(
                cx,
                format!("{verb} the thread on {place}?"),
                "",
                verb,
                false,
                Change {
                    label: format!("{verb} a thread on #{pr}"),
                    success: format!(
                        "{} the thread on {place}",
                        if resolve { "Resolved" } else { "Reopened" }
                    ),
                    steps: vec![Step::Gql(m)],
                    after: After::Pr(pr),
                },
            );
        }
        Act::PostComment(n) => {
            let Some(gh) = gh else { return };
            let body = repo
                .devops
                .review
                .prs
                .get(&n)
                .map(|v| v.comment.clone())
                .unwrap_or_default();
            if body.trim().is_empty() {
                return;
            }
            confirm_change(
                cx,
                format!("Comment on #{n}?"),
                "Published right away in the conversation.",
                "Comment",
                false,
                Change {
                    label: format!("Comment on #{n}"),
                    success: format!("Commented on #{n}"),
                    steps: vec![Step::Rest(rest::issue_comment(&gh, n, &body))],
                    after: After::Comment(n),
                },
            );
        }
        Act::Merge(n) => {
            let Some(gh) = gh else { return };
            let st = &repo.devops.review;
            let Some(change) = merge_change(cx, st, &gh, n, false) else {
                return;
            };
            let pr = st.find(n).cloned().unwrap_or_default();
            let deletes = change.steps.len() > 1;
            confirm_change(
                cx,
                format!("Merge #{n} into {}?", pr.base),
                format!(
                    "GitHub refuses if {} moved past {} since it was loaded.{}",
                    pr.head,
                    short(&pr.head_oid),
                    if deletes {
                        format!(" Then {} is deleted on GitHub.", pr.head)
                    } else {
                        String::new()
                    }
                ),
                "Merge",
                false,
                change,
            );
        }
        Act::AutoMerge { pr: n, enable } => {
            let Some(gh) = gh else { return };
            let st = &repo.devops.review;
            if enable {
                let Some(change) = merge_change(cx, st, &gh, n, true) else {
                    return;
                };
                confirm_change(
                    cx,
                    format!("Enable auto-merge on #{n}?"),
                    "GitHub merges it as soon as the required reviews and checks pass.",
                    "Enable auto-merge",
                    false,
                    change,
                );
            } else {
                let Some(d) = loaded_detail(cx, st, n) else {
                    return;
                };
                confirm_change(
                    cx,
                    format!("Disable auto-merge on #{n}?"),
                    "",
                    "Disable auto-merge",
                    false,
                    Change {
                        label: format!("Disable auto-merge on #{n}"),
                        success: format!("Auto-merge disabled on #{n}"),
                        steps: vec![Step::Gql(graphql::disable_auto_merge(&d.pr.id))],
                        after: After::Pr(n),
                    },
                );
            }
        }
        Act::UpdateBranch(n) => {
            let Some(gh) = gh else { return };
            let st = &repo.devops.review;
            let Some(d) = loaded_detail(cx, st, n) else {
                return;
            };
            confirm_change(
                cx,
                format!("Update {} with {}?", d.pr.head, d.pr.base),
                format!(
                    "GitHub merges {} into {} (a merge commit on the branch). Refused if the head moved past {}.",
                    d.pr.base,
                    d.pr.head,
                    short(&d.pr.head_oid)
                ),
                "Update branch",
                false,
                Change {
                    label: format!("Update the branch of #{n}"),
                    success: format!("Updating {} with {}", d.pr.head, d.pr.base),
                    steps: vec![Step::Rest(rest::update_branch(&gh, n, &d.pr.head_oid))],
                    after: After::Pr(n),
                },
            );
        }
        Act::SetDraft { pr: n, draft } => {
            let st = &repo.devops.review;
            let Some(d) = loaded_detail(cx, st, n) else {
                return;
            };
            let (title, verb, m) = if draft {
                (
                    format!("Convert #{n} to a draft?"),
                    "Convert to draft",
                    graphql::convert_to_draft(&d.pr.id),
                )
            } else {
                (
                    format!("Mark #{n} as ready for review?"),
                    "Ready for review",
                    graphql::mark_ready_for_review(&d.pr.id),
                )
            };
            confirm_change(
                cx,
                title,
                if draft {
                    "Reviewers are no longer notified, and it cannot be merged until it is ready again."
                } else {
                    "Requested reviewers are notified."
                },
                verb,
                false,
                Change {
                    label: format!("{verb}: #{n}"),
                    success: if draft {
                        format!("#{n} is a draft")
                    } else {
                        format!("#{n} is ready for review")
                    },
                    steps: vec![Step::Gql(m)],
                    after: After::Pr(n),
                },
            );
        }
        Act::SetOpen { pr: n, open } => {
            let Some(gh) = gh else { return };
            let verb = if open { "Reopen" } else { "Close" };
            confirm_change(
                cx,
                format!("{verb} pull request #{n}?"),
                if open {
                    ""
                } else {
                    "It stays on GitHub and can be reopened."
                },
                verb,
                !open,
                Change {
                    label: format!("{verb} #{n}"),
                    success: format!("{} #{n}", if open { "Reopened" } else { "Closed" }),
                    steps: vec![Step::Rest(rest::set_pr_state(&gh, n, open))],
                    after: After::Pr(n),
                },
            );
        }
        Act::Checkout(n) => {
            let op = GitOp::GhPrCheckout { number: n };
            cx.confirm(
                format!("Check out pull request #{n}?"),
                "Fetches the pull request's branch and switches to it. Uncommitted changes stay in the working tree unless they conflict.",
                "Check out",
                false,
                op_details(&op),
                Act::RunOps(vec![op]).into(),
            );
        }
        Act::Run(change) => run_change(cx, &mut repo.devops.review, change),
        Act::RunOps(ops) => cx.ops(ops),
        Act::NewPr => {
            let meta = repo.devops.review.meta();
            let st = &mut repo.devops.review;
            st.mode = Mode::Prs;
            st.form_open = true;
            if st.meta.wanted() {
                fetch_meta(cx, st);
            }
            if st.suggestions.wanted() {
                fetch_suggestions(cx, st);
            }
            if st.new_pr.is_none() {
                let form = new_pr_form(repo, &meta);
                let candidates = base_candidates(repo, &form.base);
                let head = (!form.head.is_empty()).then(|| form.head.clone());
                read_local(cx, candidates, head);
                repo.devops.review.new_pr = Some(form);
            }
        }
        Act::CloseForm => {
            let st = &mut repo.devops.review;
            match st.mode {
                Mode::Prs => st.form_open = false,
                Mode::Issues => st.issues.form_open = false,
            }
        }
        Act::DiscardForm => {
            let st = &mut repo.devops.review;
            match st.mode {
                Mode::Prs => {
                    st.form_open = false;
                    st.new_pr = None;
                }
                Mode::Issues => {
                    st.issues.form_open = false;
                    st.issues.form = None;
                }
            }
        }
        Act::GenerateBody => {
            if let Some(f) = repo.devops.review.new_pr.as_mut()
                && let Some(Ok(commits)) = &f.commits
            {
                let bullets = local::body_from_commits(commits);
                if f.body.trim().is_empty() {
                    f.body = bullets;
                } else {
                    if !f.body.ends_with('\n') {
                        f.body.push('\n');
                    }
                    f.body.push('\n');
                    f.body.push_str(&bullets);
                }
            }
        }
        Act::UseTemplate(i) => {
            if let Some(f) = repo.devops.review.new_pr.as_mut()
                && let Some(Ok(t)) = &f.templates
                && let Some(t) = t.get(i)
            {
                f.body = t.body.clone();
                f.template = Some(i);
            }
        }
        Act::CreatePr => {
            let Some(gh) = gh else { return };
            let st = &mut repo.devops.review;
            let Some(change) = new_pr_change(st, &gh) else {
                return;
            };
            let draft = st.new_pr.as_ref().is_some_and(|f| f.draft);
            let extra = change.steps.len() - 1;
            confirm_change(
                cx,
                format!(
                    "Open {} pull request on {}?",
                    if draft { "a draft" } else { "a" },
                    gh.full_name()
                ),
                if extra > 0 {
                    format!(
                        "{} more {} follow; {{number}} stands for the number GitHub gives the new pull request.",
                        extra,
                        if extra == 1 { "request" } else { "requests" }
                    )
                } else {
                    String::new()
                },
                "Open pull request",
                false,
                change,
            );
        }
        Act::PushFirst => match push_dialog(repo) {
            Some(d) => cx.action(Action::Dialog(d)),
            None => cx.toast(ToastKind::Info, "There is no branch or remote to push to."),
        },
        Act::LoadSuggestions => {
            let st = &mut repo.devops.review;
            if !st.suggestions.loading {
                st.suggestions.error = None;
                fetch_suggestions(cx, st);
            }
        }
        Act::OpenIssue(n) => select_issue(cx, &mut repo.devops.review, n),
        Act::OpenListedIssue(i) => {
            let st = &mut repo.devops.review;
            st.mode = Mode::Issues;
            match st.issues.visible().get(i).map(|x| x.number) {
                Some(n) => select_issue(cx, st, n),
                None => st.issues.pending_listed = Some(i),
            }
        }
        Act::SetIssueFilter(f) => {
            let st = &mut repo.devops.review;
            st.issues.filter = f;
            if st.issues.current().is_none_or(|l| l.wanted()) {
                fetch_issues(cx, st);
            }
        }
        Act::SetIssueLabel(l) => {
            let st = &mut repo.devops.review;
            st.issues.label = l;
            if st.issues.current().is_none_or(|l| l.wanted()) {
                fetch_issues(cx, st);
            }
        }
        Act::RefreshIssue(n) => fetch_issue(cx, &mut repo.devops.review, n),
        Act::IssueComment(n) => {
            let Some(gh) = gh else { return };
            let body = repo
                .devops
                .review
                .issues
                .comment
                .get(&n)
                .cloned()
                .unwrap_or_default();
            if body.trim().is_empty() {
                return;
            }
            confirm_change(
                cx,
                format!("Comment on issue #{n}?"),
                "Published right away.",
                "Comment",
                false,
                Change {
                    label: format!("Comment on issue #{n}"),
                    success: format!("Commented on issue #{n}"),
                    steps: vec![Step::Rest(rest::issue_comment(&gh, n, &body))],
                    after: After::IssueComment(n),
                },
            );
        }
        Act::SetIssueState {
            number: n,
            open,
            reason,
        } => {
            let Some(gh) = gh else { return };
            let title = if open {
                format!("Reopen issue #{n}?")
            } else {
                format!("{} #{n}?", reason.label())
            };
            let verb = if open { "Reopen" } else { reason.label() };
            confirm_change(
                cx,
                title,
                "",
                verb,
                false,
                Change {
                    label: format!("{} issue #{n}", if open { "Reopen" } else { "Close" }),
                    success: if open {
                        format!("Reopened issue #{n}")
                    } else {
                        format!("Closed issue #{n} as {}", reason.api().replace('_', " "))
                    },
                    steps: vec![Step::Rest(rest::set_issue_state(&gh, n, open, reason))],
                    after: After::Issue(n),
                },
            );
        }
        Act::EditIssue(n) => {
            let st = &mut repo.devops.review;
            if let Some(i) = st.issues.find(n).cloned() {
                st.issues.edit = Some(IssueEdit {
                    number: n,
                    labels: i.labels.iter().map(|l| l.name.clone()).collect(),
                    label_buf: String::new(),
                    assignees: i.assignees.clone(),
                    assignee_buf: String::new(),
                });
            }
            if st.suggestions.wanted() {
                fetch_suggestions(cx, st);
            }
        }
        Act::SaveIssueEdit(n) => {
            let Some(gh) = gh else { return };
            let st = &repo.devops.review;
            let Some(e) = st.issues.edit.as_ref().filter(|e| e.number == n) else {
                return;
            };
            confirm_change(
                cx,
                format!("Save the labels and assignees of #{n}?"),
                "Replaces both lists on GitHub with these.",
                "Save",
                false,
                Change {
                    label: format!("Edit issue #{n}"),
                    success: format!(
                        "Issue #{n}: {} and {}",
                        format::plural(e.labels.len(), "label", "labels"),
                        format::plural(e.assignees.len(), "assignee", "assignees")
                    ),
                    steps: vec![Step::Rest(rest::edit_issue(
                        &gh,
                        n,
                        &e.labels,
                        &e.assignees,
                    ))],
                    after: After::IssueEdit(n),
                },
            );
        }
        Act::StartBranch(n) => {
            let st = &repo.devops.review;
            let Some(b) = st.issues.branch.as_ref().filter(|b| b.number == n) else {
                return;
            };
            let name = b.name.trim().to_owned();
            if name.is_empty() {
                return;
            }
            let Some(start) = branch_start(repo, b.from_current) else {
                cx.toast(
                    ToastKind::Info,
                    "No start point: the default branch is not known yet, or HEAD is detached.",
                );
                return;
            };
            let op = GitOp::CreateBranch {
                name: name.clone(),
                start: start.clone(),
                switch: true,
            };
            cx.confirm(
                format!("Start {name} for issue #{n}?"),
                format!("Creates the branch at {start} and switches to it."),
                "Create branch",
                false,
                op_details(&op),
                Act::RunOps(vec![op]).into(),
            );
        }
        Act::NewIssue => {
            let st = &mut repo.devops.review;
            st.mode = Mode::Issues;
            st.issues.form_open = true;
            if st.suggestions.wanted() {
                fetch_suggestions(cx, st);
            }
            if st.issues.form.is_none() {
                st.issues.form = Some(NewIssueForm::default());
                read_local(cx, Vec::new(), None);
            }
        }
        Act::UseIssueTemplate(i) => {
            if let Some(f) = repo.devops.review.issues.form.as_mut()
                && let Some(t) = f.templates.as_ref().and_then(|t| t.get(i)).cloned()
            {
                f.body = t.body;
                if f.title.trim().is_empty() {
                    f.title = t.title;
                }
                for l in t.labels {
                    if !f.labels.contains(&l) {
                        f.labels.push(l);
                    }
                }
                for a in t.assignees {
                    if !f.assignees.contains(&a) {
                        f.assignees.push(a);
                    }
                }
                f.template = Some(i);
            }
        }
        Act::CreateIssue => {
            let Some(gh) = gh else { return };
            let Some(f) = repo.devops.review.issues.form.as_mut() else {
                return;
            };
            if f.title.trim().is_empty() {
                f.error = "Give the issue a title.".to_owned();
                return;
            }
            f.error.clear();
            let change = Change {
                label: "Open an issue".to_owned(),
                success: format!("Opened issue “{}”", f.title.trim()),
                steps: vec![Step::Rest(rest::create_issue(
                    &gh,
                    &f.title,
                    &f.body,
                    &f.labels,
                    &f.assignees,
                ))],
                after: After::IssueCreated,
            };
            confirm_change(
                cx,
                format!("Open an issue on {}?", gh.full_name()),
                "",
                "Open issue",
                false,
                change,
            );
        }
        Act::Script(cmd, arg) => script_act(cx, repo, &cmd, &arg),
    }
}

/// Add a ```suggestion block of the selected lines to the composer.
fn suggest(st: &mut State, n: u64) {
    let Some(v) = st.prs.get_mut(&n) else {
        return;
    };
    let Some(sel) = v.sel.clone() else {
        return;
    };
    let Some(file) = v
        .files
        .data
        .as_ref()
        .and_then(|f| f.iter().find(|f| f.path == sel.path))
    else {
        return;
    };
    let hunks = patch::parse_patch(file.patch.as_deref().unwrap_or(""));
    let (a, b) = sel.range();
    match patch::suggestion_lines(&hunks, a, b) {
        Some(text) => {
            if !v.composer.is_empty() && !v.composer.ends_with('\n') {
                v.composer.push('\n');
            }
            v.composer.push_str(&patch::suggestion_block(&text));
            v.composer_error.clear();
        }
        None => {
            v.composer_error =
                "Suggestions replace lines of the new version: select added or unchanged lines."
                    .to_owned()
        }
    }
}

/// Where a branch for an issue starts: the default branch (local if it
/// exists, else the GitHub remote's), or the current branch.
fn branch_start(repo: &RepoState, from_current: bool) -> Option<String> {
    let snap = repo.snapshot.as_ref()?;
    if from_current {
        return snap
            .current_branch()
            .map(|b| b.name.clone())
            .or_else(|| snap.head.target().map(|t| t.to_string()));
    }
    let base = repo.devops.review.meta().default_branch;
    if base.is_empty() {
        return None;
    }
    if snap.branch(&base).is_some() {
        return Some(base);
    }
    base_candidates(repo, &base)
        .into_iter()
        .find(|c| snap.remote_branch(c).is_some())
}

fn refresh(cx: &mut Ctx, repo: &mut RepoState) {
    let st = &mut repo.devops.review;
    st.meta.error = None;
    fetch_meta(cx, st);
    match st.mode {
        Mode::Prs => {
            let kind = st.filter.kind();
            fetch_list(cx, st, kind, false);
            if let Some(n) = st.selected {
                apply(cx, repo, Act::RefreshPr(n));
            }
        }
        Mode::Issues => {
            fetch_issues(cx, st);
            if let Some(n) = st.issues.selected {
                fetch_issue(cx, st, n);
            }
        }
    }
}

fn script_act(cx: &mut Ctx, repo: &mut RepoState, cmd: &str, arg: &str) {
    let st = &mut repo.devops.review;
    let selected = st.selected;
    let mode = st.mode;
    match cmd {
        "review-filter" => match mode {
            Mode::Prs => {
                if let Some(f) = PrFilter::from_name(arg) {
                    apply(cx, repo, Act::SetFilter(f));
                }
            }
            Mode::Issues => {
                if let Some(f) = IssueFilter::from_name(arg) {
                    apply(cx, repo, Act::SetIssueFilter(f));
                }
            }
        },
        "review-scroll" => {
            if let Ok(y) = arg.trim().parse::<f32>() {
                view::request_scroll(y);
            }
        }
        "review-search" => match mode {
            Mode::Prs => st.search = arg.to_owned(),
            Mode::Issues => st.issues.search = arg.to_owned(),
        },
        "pr-lines" => {
            let mut it = arg
                .split_whitespace()
                .filter_map(|x| x.parse::<usize>().ok());
            let Some(a) = it.next() else {
                return;
            };
            let b = it.next().unwrap_or(a);
            let Some(n) = selected else { return };
            let v = st.prs.entry(n).or_default();
            let Some(path) = v.file.clone() else { return };
            let Some(file) = v
                .files
                .data
                .as_ref()
                .and_then(|f| f.iter().find(|f| f.path == path))
            else {
                return;
            };
            let hunks = patch::parse_patch(file.patch.as_deref().unwrap_or(""));
            let flat: Vec<patch::LinePos> = hunks
                .iter()
                .enumerate()
                .flat_map(|(h, hk)| {
                    (0..hk.lines.len()).map(move |l| patch::LinePos { hunk: h, line: l })
                })
                .collect();
            if let (Some(pa), Some(pb)) = (flat.get(a), flat.get(b)) {
                v.sel = Some(LineSel {
                    path,
                    anchor: *pa,
                    end: *pb,
                });
            }
        }
        "pr-compose" | "pr-comment" => {
            let Some(n) = selected else { return };
            st.prs.entry(n).or_default().composer = arg.replace("\\n", "\n");
            if cmd == "pr-comment" {
                apply(cx, repo, Act::AddPending(n));
            }
        }
        "pr-suggest" => {
            if let Some(n) = selected {
                suggest(st, n);
            }
        }
        "pr-review" => {
            let Some(n) = selected else { return };
            let v = st.prs.entry(n).or_default();
            v.review_open = true;
            let (event, body) = arg.split_once(' ').unwrap_or((arg, ""));
            v.review_event = match event {
                "approve" => ReviewEvent::Approve,
                "changes" => ReviewEvent::RequestChanges,
                _ => ReviewEvent::Comment,
            };
            if !body.is_empty() {
                v.review_body = body.to_owned();
            }
        }
        "pr-submit" => {
            if let Some(n) = selected {
                apply(cx, repo, Act::SubmitReview(n));
            }
        }
        "pr-merge" => {
            let Some(n) = selected else { return };
            if let Some(m) = MergeMethod::parse(arg) {
                st.prs.entry(n).or_default().merge.method = Some(m);
            }
            apply(cx, repo, Act::Merge(n));
        }
        "pr-threads" => {
            // Open every thread of the selected pull request.
            let Some(n) = selected else { return };
            let v = st.prs.entry(n).or_default();
            if let Some(d) = v.detail.data.as_ref() {
                v.opened = d.threads.iter().map(|t| t.id.clone()).collect();
                v.closed.clear();
            }
        }
        "create-pr" => apply(cx, repo, Act::CreatePr),
        "pr-title" => {
            if let Some(f) = st.new_pr.as_mut() {
                f.title = arg.to_owned();
                f.title_edited = true;
            }
        }
        "generate-body" => apply(cx, repo, Act::GenerateBody),
        "start-branch" => {
            let Some(n) = st.issues.selected else { return };
            if st.issues.branch.as_ref().is_none_or(|b| b.number != n) {
                let title = st
                    .issues
                    .find(n)
                    .map(|i| i.title.clone())
                    .unwrap_or_default();
                st.issues.branch = Some(BranchForm {
                    number: n,
                    name: local::issue_branch_name(n, &title),
                    from_current: arg == "current",
                });
            }
            apply(cx, repo, Act::StartBranch(n));
        }
        _ => {}
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    let st = &mut repo.devops.review;
    match msg {
        Msg::Meta(r) => st.meta.finish(r),
        Msg::List {
            kind,
            append,
            result,
        } => {
            if append {
                st.more_loading = false;
                match result {
                    Ok(page) => {
                        if let Some(l) = st.list_mut(kind).data.as_mut() {
                            for item in page.items {
                                if !l.items.iter().any(|x| x.number == item.number) {
                                    l.items.push(item);
                                }
                            }
                            l.end_cursor = page.end_cursor;
                            l.has_more = page.has_more;
                            l.total = page.total;
                            l.light |= page.light;
                        }
                    }
                    Err(e) => cx.toast(ToastKind::Error, format!("Could not load more: {e}")),
                }
            } else {
                st.list_mut(kind).finish(result);
            }
            if let Some(i) = st.pending_listed
                && kind == st.filter.kind()
                && let Some(n) = st.visible().get(i).map(|p| p.number)
            {
                st.pending_listed = None;
                select_pr(cx, repo, n);
            }
        }
        Msg::Detail(n, r) => {
            let fresh = r.as_ref().ok().map(|d| d.pr.clone());
            st.prs.entry(n).or_default().detail.finish(r);
            // Keep the list rows in step with the fresher detail.
            if let Some(pr) = fresh {
                for f in [&mut st.open, &mut st.closed] {
                    if let Some(l) = f.data.as_mut() {
                        for item in l
                            .items
                            .iter_mut()
                            .chain(l.requested.iter_mut())
                            .chain(l.mine.iter_mut())
                        {
                            if item.number == n {
                                *item = pr.clone();
                            }
                        }
                    }
                }
            }
            if st.selected == Some(n) && st.tab == PrTab::Checks {
                tab_shown(cx, repo);
            }
        }
        Msg::Files(n, r) => {
            let selected = st.selected;
            let pending = st.pending_file;
            let v = st.prs.entry(n).or_default();
            v.files.finish(r);
            v.parsed.clear();
            if let Some(files) = v.files.data.as_ref() {
                let keep = v
                    .file
                    .as_ref()
                    .is_some_and(|p| files.iter().any(|f| &f.path == p));
                if let Some(i) = pending.filter(|_| selected == Some(n)) {
                    v.file = files.get(i).map(|f| f.path.clone());
                    st.pending_file = None;
                } else if !keep {
                    v.file = files.first().map(|f| f.path.clone());
                }
            }
        }
        Msg::Commits(n, r) => st.prs.entry(n).or_default().commits.finish(r),
        Msg::Suggestions(r) => st.suggestions.finish(r),
        Msg::Local(info) => {
            let info = *info;
            if let Some(f) = st.new_pr.as_mut() {
                f.base_ref = info.base_ref.clone();
                if f.body.is_empty()
                    && let Some(t) = info.templates.first()
                {
                    f.body = t.body.clone();
                    f.template = Some(0);
                }
                if !f.title_edited {
                    f.title = match &info.commits {
                        Some(Ok(c)) => local::default_title(c, &f.head),
                        _ => local::humanize_branch(&f.head),
                    };
                }
                f.templates = Some(Ok(info.templates.clone()));
                f.commits = info.commits.clone();
            }
            if let Some(f) = st.issues.form.as_mut() {
                f.templates = Some(info.issue_templates);
            }
        }
        Msg::Issues { key, login, result } => {
            if login.is_some() && st.issues.login.is_none() {
                st.issues.login = login;
            }
            st.issues
                .lists
                .entry(key.clone())
                .or_default()
                .finish(result);
            if key == st.issues.key()
                && let Some(i) = st.issues.pending_listed
                && let Some(n) = st.issues.visible().get(i).map(|x| x.number)
            {
                st.issues.pending_listed = None;
                select_issue(cx, st, n);
            }
        }
        Msg::Issue(n, r) => st.issues.details.entry(n).or_default().finish(r),
        Msg::Changed { after, ok, number } => {
            st.changing = None;
            match after {
                After::Pr(n) => fetch_detail(cx, st, n),
                After::Review(n) => {
                    if ok {
                        let v = st.prs.entry(n).or_default();
                        v.pending.clear();
                        v.review_body.clear();
                        v.review_open = false;
                        v.review_event = ReviewEvent::Comment;
                    }
                    fetch_detail(cx, st, n);
                }
                After::Comment(n) => {
                    if ok {
                        st.prs.entry(n).or_default().comment.clear();
                    }
                    fetch_detail(cx, st, n);
                }
                After::LineComment(n) => {
                    if ok {
                        let v = st.prs.entry(n).or_default();
                        v.composer.clear();
                        v.sel = None;
                    }
                    fetch_detail(cx, st, n);
                }
                After::Reply(n, thread) => {
                    if ok {
                        st.prs.entry(n).or_default().replies.remove(&thread);
                    }
                    fetch_detail(cx, st, n);
                }
                After::Created => {
                    if ok {
                        st.new_pr = None;
                        st.form_open = false;
                        st.filter = PrFilter::Open;
                        fetch_list(cx, st, ListKind::Open, false);
                        if let Some(n) = number {
                            st.tab = PrTab::Conversation;
                            select_pr(cx, repo, n);
                        }
                    }
                }
                After::Issue(n) => {
                    fetch_issue(cx, st, n);
                    st.issues.lists.retain(|_, f| f.loading);
                    fetch_issues(cx, st);
                }
                After::IssueComment(n) => {
                    if ok {
                        st.issues.comment.remove(&n);
                    }
                    fetch_issue(cx, st, n);
                }
                After::IssueEdit(n) => {
                    if ok {
                        st.issues.edit = None;
                    }
                    fetch_issue(cx, st, n);
                }
                After::IssueCreated => {
                    if ok {
                        st.issues.form = None;
                        st.issues.form_open = false;
                        st.issues.filter = IssueFilter::Open;
                        st.issues.lists.retain(|_, f| f.loading);
                        fetch_issues(cx, st);
                        if let Some(n) = number {
                            select_issue(cx, st, n);
                        }
                    }
                }
            }
        }
    }
}

/// A console run started by this area (or any other) ended.
pub fn on_console_done(_cx: &mut Ctx, _repo: &mut RepoState, _run: &ConsoleRun) {}

/// The page was opened (fetch what it needs the first time).
pub fn on_page_shown(cx: &mut Ctx, repo: &mut RepoState) {
    let st = &mut repo.devops.review;
    if st.meta.wanted() {
        fetch_meta(cx, st);
    }
    match st.mode {
        Mode::Prs => {
            if st.open.wanted() {
                fetch_list(cx, st, ListKind::Open, false);
            }
            if st.filter == PrFilter::Closed && st.closed.wanted() {
                fetch_list(cx, st, ListKind::Closed, false);
            }
            if let Some(n) = st.selected {
                if st.prs.get(&n).is_none_or(|v| v.detail.wanted()) {
                    fetch_detail(cx, st, n);
                }
                tab_shown(cx, repo);
            }
        }
        Mode::Issues => {
            if st.issues.current().is_none_or(|l| l.wanted()) {
                fetch_issues(cx, st);
            }
            if let Some(n) = st.issues.selected
                && st.issues.details.get(&n).is_none_or(|f| f.wanted())
            {
                fetch_issue(cx, st, n);
            }
        }
    }
}

/// `GITVIS_SCRIPT` commands this area understands.
pub fn script(cmd: &str, arg: &str) -> Option<Vec<Action>> {
    let page = Action::ShowPage(Page::Review);
    let num = arg.trim().parse::<u64>().ok();
    let a = |x: Act| -> Action { x.into() };
    Some(match cmd {
        "review" | "prs" => vec![page, a(Act::SetMode(Mode::Prs))],
        "issues" => vec![page, a(Act::SetMode(Mode::Issues))],
        "pr" => vec![
            page,
            a(Act::SetMode(Mode::Prs)),
            a(Act::OpenListed(num? as usize)),
        ],
        "pr-number" => vec![Action::OpenPr(num?)],
        "pr-tab" => vec![a(Act::SetTab(PrTab::from_name(arg)?))],
        "pr-file" => vec![a(Act::SelectFileIndex(num? as usize))],
        "issue" => vec![
            page,
            a(Act::SetMode(Mode::Issues)),
            a(Act::OpenListedIssue(num? as usize)),
        ],
        "issue-number" => vec![page, a(Act::OpenIssue(num?))],
        "new-pr" => vec![page, a(Act::NewPr)],
        "new-issue" => vec![page, a(Act::NewIssue)],
        "review-refresh" => vec![a(Act::Refresh)],
        "review-filter" | "review-search" | "review-scroll" | "pr-lines" | "pr-comment"
        | "pr-compose" | "pr-suggest" | "pr-review" | "pr-submit" | "pr-merge" | "pr-threads"
        | "create-pr" | "pr-title" | "generate-body" | "start-branch" => {
            vec![a(Act::Script(cmd.to_owned(), arg.to_owned()))]
        }
        _ => return None,
    })
}

pub fn show(
    ui: &mut egui::Ui,
    repo: &mut RepoState,
    console: &Console,
    settings: &Settings,
    actions: &mut Vec<Action>,
) {
    view::show(ui, repo, console, settings, actions);
}

/// A dot on the rail entry and a line for its tooltip, when this area
/// needs attention (e.g. pending approvals, critical alerts, a running
/// task). `None` when nothing does.
pub fn rail_badge(repo: &RepoState, _console: &Console) -> Option<(crate::theme::Tone, String)> {
    let l = repo.devops.review.open.data.as_ref()?;
    (l.requested_total > 0).then(|| {
        (
            crate::theme::Tone::Warning,
            format::plural(
                l.requested_total as usize,
                "review request",
                "review requests",
            ),
        )
    })
}
