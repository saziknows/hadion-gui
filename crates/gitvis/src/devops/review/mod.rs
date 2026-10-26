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

mod changes;
mod control;
mod conversation;
mod files;
mod forms;
mod issues;
mod reads;
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

pub use control::{apply, on_msg};
pub use widgets::pr_for_commit;

use changes::*;
use reads::*;

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
    /// A thread to scroll to in the diff (taken when drawn).
    pub jump: Option<String>,
    /// The last thread "Next thread" went to.
    pub last_jump: Option<String>,
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
    /// The base branch of the New pull request form changed.
    SetBase(String),
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

/// Replies of the area's background work. Large payloads are boxed so the
/// app's message enum stays small.
pub enum Msg {
    Meta(Box<Result<RepoMeta, String>>),
    List {
        kind: ListKind,
        append: bool,
        result: Box<Result<PrList, String>>,
    },
    Detail(u64, Box<Result<PrDetail, String>>),
    Files(u64, Result<Vec<PrFile>, String>),
    Commits(u64, Result<Vec<PrCommit>, String>),
    Suggestions(Box<Result<Suggestions, String>>),
    Local(Box<LocalInfo>),
    Issues {
        key: IssueKey,
        login: Option<String>,
        result: Result<Vec<Issue>, String>,
    },
    Issue(u64, Box<Result<(Issue, Vec<IssueComment>), String>>),
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
            Msg::Meta(r) => (**r).clone().map(|m| {
                format!(
                    "default branch {}, your permission {}",
                    m.default_branch,
                    m.permission.to_lowercase()
                )
            }),
            Msg::List { kind, result, .. } => {
                result.as_ref().as_ref().map_err(Clone::clone).map(|l| {
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
                })
            }
            Msg::Detail(n, r) => r.as_ref().as_ref().map_err(Clone::clone).map(|d| {
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
            Msg::Suggestions(r) => r.as_ref().as_ref().map_err(Clone::clone).map(|s| {
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
        | "pr-next-thread" | "create-pr" | "pr-title" | "generate-body" | "start-branch" => {
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
