//! The home page: every open repository at a glance and the GitHub inbox
//! (notifications, review requests, mentions, assignments, failed runs on
//! the user's branches). Global: its state lives in the app, not in a
//! repository.
//!
//! Network: the inbox (one `GET notifications` plus one GraphQL search) is
//! fetched when the page opens and the last fetch is older than five
//! minutes, and on Refresh. Everything about the repositories comes from
//! data they already loaded.

mod inbox;
mod view;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use gitvis_core::devops::gh::Gh;
use gitvis_core::devops::home::notify::{self, Notification, Reason, SubjectKind};
use gitvis_core::devops::home::{fetch, iso_utc, runs, search};
use gitvis_core::github::RateLimit;

use crate::devops::ctx::Ctx;
use crate::format;
use crate::state::{Action, Page, RepoState, ToastKind};

pub use view::show;

/// Refetch on page open when the last fetch is older than this.
const STALE: Duration = Duration::from_secs(300);
/// "Failed runs on my branches" look back this far.
pub const FAILED_WINDOW_SECS: i64 = 86_400;
/// How long one repository's fetch may take.
const FETCH_TIMEOUT: Duration = Duration::from_secs(180);

/// Sections of the home page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Tab {
    #[default]
    Overview,
    Notifications,
    Reviews,
    Mine,
    Mentions,
    Assigned,
    Failed,
}

impl Tab {
    pub const ALL: [Tab; 7] = [
        Tab::Overview,
        Tab::Notifications,
        Tab::Reviews,
        Tab::Mine,
        Tab::Mentions,
        Tab::Assigned,
        Tab::Failed,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Overview => "Repositories",
            Tab::Notifications => "Notifications",
            Tab::Reviews => "Review requests",
            Tab::Mine => "My pull requests",
            Tab::Mentions => "Mentions",
            Tab::Assigned => "Assigned",
            Tab::Failed => "Failed runs",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Tab::Overview => "Every open repository: branch, changes, CI, pull requests, findings",
            Tab::Notifications => "Unread GitHub notifications, why each arrived, mark as read",
            Tab::Reviews => "Open pull requests waiting for your review",
            Tab::Mine => "Your open pull requests with review decision and checks",
            Tab::Mentions => "Open issues and pull requests that mention you",
            Tab::Assigned => "Open issues and pull requests assigned to you",
            Tab::Failed => "Failed workflow runs on your branches in the last 24 hours",
        }
    }

    pub fn bucket(self) -> Option<search::Bucket> {
        match self {
            Tab::Reviews => Some(search::Bucket::ReviewRequests),
            Tab::Mine => Some(search::Bucket::Mine),
            Tab::Mentions => Some(search::Bucket::Mentions),
            Tab::Assigned => Some(search::Bucket::Assigned),
            _ => None,
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "overview" | "repos" | "repositories" => Tab::Overview,
            "notifications" | "inbox" => Tab::Notifications,
            "reviews" | "review" | "review-requests" => Tab::Reviews,
            "mine" | "my-prs" | "prs" => Tab::Mine,
            "mentions" => Tab::Mentions,
            "assigned" => Tab::Assigned,
            "failed" | "runs" | "failed-runs" => Tab::Failed,
            _ => return None,
        })
    }
}

/// A running "Fetch all".
pub struct FetchRun {
    pub started: Instant,
    /// Repositories in order.
    pub names: Vec<String>,
    pub progress: Arc<Mutex<FetchProgress>>,
}

#[derive(Default)]
pub struct FetchProgress {
    /// The repository being fetched.
    pub current: Option<String>,
    pub done: Vec<fetch::Outcome>,
}

/// The last finished "Fetch all".
pub struct FetchDone {
    pub at: i64,
    pub elapsed_ms: u64,
    pub outcomes: Vec<fetch::Outcome>,
    pub skipped: Vec<String>,
}

#[derive(Default)]
pub struct State {
    pub tab: Tab,
    /// GitHub login from the last fetch (persisted).
    pub login: Option<String>,
    pub loading: bool,
    /// When the last fetch finished (`Instant` for "how long ago",
    /// epoch for the absolute time and mark-all-read).
    pub fetched_at: Option<Instant>,
    pub fetched_epoch: Option<i64>,
    pub notifications: Option<Vec<Notification>>,
    pub notif_error: Option<String>,
    pub search: Option<search::Results>,
    pub search_error: Option<String>,
    /// REST rate limit after the notifications call.
    pub rate: Option<RateLimit>,
    pub filter: notify::Filter,
    /// Only repositories open in gitvis, on the search tabs.
    pub search_only_open: bool,
    pub group: bool,
    pub text: String,
    /// Threads being marked as read.
    pub marking: HashSet<String>,
    pub marking_all: bool,
    pub fetch: Option<FetchRun>,
    pub fetched: Option<FetchDone>,
}

#[derive(Clone, Debug)]
pub enum Act {
    SetTab(Tab),
    Refresh,
    /// Switch to a repository's history.
    Activate(usize),
    /// Ask to fetch every open repository.
    FetchAll,
    /// Fetch these `(name, workdir)` (after the confirmation).
    FetchAllConfirmed(Vec<(String, PathBuf)>, Vec<String>),
    /// Script: fetch only the open repositories under this directory.
    FetchAllIn(PathBuf),
    ToggleReason(Reason),
    SetReasons(Vec<Reason>),
    SetGroup(bool),
    ClearFilters,
    OpenNotification(String),
    OpenItem {
        repo: String,
        number: u64,
        pr: bool,
        url: String,
    },
    OpenRun {
        repo: usize,
        run: u64,
    },
    MarkRead(String),
    MarkAllRead,
    MarkAllReadConfirmed(String),
    /// Script: open the N-th item of the current list.
    OpenNth(usize),
}

pub enum Msg {
    Inbox {
        notifications: Result<Vec<Notification>, String>,
        search: Result<search::Results, String>,
        rate: Option<RateLimit>,
        epoch: i64,
    },
    MarkedRead {
        id: String,
        result: Result<(), String>,
    },
    MarkedAllRead {
        result: Result<(), String>,
    },
    FetchedAll {
        outcomes: Vec<fetch::Outcome>,
        skipped: Vec<String>,
        elapsed_ms: u64,
    },
}

impl Msg {
    /// One line for the activity log (`Err` when the work failed).
    pub fn summary(&self) -> Result<String, String> {
        match self {
            Msg::Inbox {
                notifications,
                search,
                ..
            } => match (notifications, search) {
                (Err(a), Err(b)) => Err(format!("{a}\n{b}")),
                _ => {
                    let mut parts = Vec::new();
                    match notifications {
                        Ok(n) => parts.push(format!(
                            "{} ({} unread)",
                            format::plural(n.len(), "notification", "notifications"),
                            n.iter().filter(|x| x.unread).count()
                        )),
                        Err(e) => parts.push(format!("notifications failed: {e}")),
                    }
                    match search {
                        Ok(s) => parts.push(format!(
                            "{} review requests, {} of my pull requests, {} mentions, {} assigned",
                            s.review.total, s.mine.total, s.mentions.total, s.assigned.total
                        )),
                        Err(e) => parts.push(format!("search failed: {e}")),
                    }
                    Ok(parts.join(", "))
                }
            },
            Msg::MarkedRead { result, .. } => match result {
                Ok(()) => Ok("Marked as read".into()),
                Err(e) => Err(e.clone()),
            },
            Msg::MarkedAllRead { result } => match result {
                Ok(()) => Ok("Marked every notification as read".into()),
                Err(e) => Err(e.clone()),
            },
            Msg::FetchedAll { outcomes, .. } => Ok(fetch::summary(outcomes)),
        }
    }
}

/// Persisted in the global settings.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub tab: Tab,
    pub login: Option<String>,
    /// Reason filter (API names); empty = all.
    pub reasons: Vec<String>,
    pub unread_only: bool,
    pub only_open: bool,
    pub search_only_open: bool,
    pub group: bool,
}

impl State {
    pub fn prefs(&self) -> Prefs {
        Prefs {
            tab: self.tab,
            login: self.login.clone(),
            reasons: self
                .filter
                .reasons
                .iter()
                .map(|r| r.api().to_owned())
                .collect(),
            unread_only: self.filter.unread_only,
            only_open: self.filter.only_open,
            search_only_open: self.search_only_open,
            group: self.group,
        }
    }

    pub fn apply_prefs(&mut self, p: &Prefs) {
        self.tab = p.tab;
        self.login = p.login.clone().filter(|l| !l.is_empty());
        self.filter.reasons = p.reasons.iter().map(|r| Reason::from_api(r)).collect();
        self.filter.unread_only = p.unread_only;
        self.filter.only_open = p.only_open;
        self.search_only_open = p.search_only_open;
        self.group = p.group;
    }

    pub fn unread(&self) -> usize {
        self.notifications
            .as_ref()
            .map(|n| n.iter().filter(|x| x.unread).count())
            .unwrap_or(0)
    }

    pub fn review_requests(&self) -> usize {
        self.search
            .as_ref()
            .map(|s| s.review.total as usize)
            .unwrap_or(0)
    }

    pub fn login(&self) -> &str {
        self.login.as_deref().unwrap_or("")
    }

    pub fn fetching(&self) -> bool {
        self.fetch.is_some()
    }
}

/// Lower-case `owner/repo` of every open repository with a GitHub remote.
pub fn open_names(repos: &[RepoState]) -> HashSet<String> {
    repos
        .iter()
        .filter_map(|r| r.github_remote.as_ref())
        .map(|(o, n)| format!("{o}/{n}").to_ascii_lowercase())
        .collect()
}

/// The open repository whose GitHub remote is `owner/repo`.
pub fn repo_index(repos: &[RepoState], full_name: &str) -> Option<usize> {
    repos.iter().position(|r| {
        r.github_remote
            .as_ref()
            .is_some_and(|(o, n)| format!("{o}/{n}").eq_ignore_ascii_case(full_name))
    })
}

/// A failed run on the user's branches in one open repository.
pub struct FailedRun<'a> {
    pub repo: usize,
    pub failure: runs::MyFailure<'a>,
}

/// Failed runs on the user's branches across the open repositories, newest
/// first.
pub fn failed_runs<'a>(repos: &'a [RepoState], login: &str, now: i64) -> Vec<FailedRun<'a>> {
    let mut out: Vec<FailedRun<'a>> = Vec::new();
    for (i, r) in repos.iter().enumerate() {
        let Some(gh) = &r.github else { continue };
        let full = format!("{}/{}", gh.owner, gh.repo);
        for f in
            runs::failed_on_my_branches(&gh.runs, &gh.prs, &full, login, now, FAILED_WINDOW_SECS)
        {
            out.push(FailedRun {
                repo: i,
                failure: f,
            });
        }
    }
    out.sort_by(|a, b| {
        let t = |f: &FailedRun| f.failure.run.updated.or(f.failure.run.created);
        t(b).cmp(&t(a))
    });
    out
}

fn fetch_inbox(cx: &mut Ctx, state: &mut State) {
    if state.loading {
        return;
    }
    state.loading = true;
    cx.user_read("Fetch the GitHub inbox", move |gh: &Gh, log| {
        let notifications = gh
            .send(&notify::list_request(50), &mut log.calls)
            .map_err(|e| e.to_string())
            .and_then(|b| notify::parse(&b).map_err(|e| e.to_string()));
        let rate = log.calls.last().and_then(|c| c.rate);
        let search = gh
            .graphql(search::QUERY, search::variables(50), &mut log.calls)
            .map_err(|e| e.to_string())
            .and_then(|d| search::parse(&d).map_err(|e| e.to_string()));
        Msg::Inbox {
            notifications,
            search,
            rate,
            epoch: format::now_secs(),
        }
    });
}

/// Keys of the current list in display order (for `inbox-item N`).
pub(crate) enum Target {
    Notification(String),
    Item {
        repo: String,
        number: u64,
        pr: bool,
        url: String,
    },
    Run {
        repo: usize,
        run: u64,
    },
    Repo(usize),
}

fn open_target(cx: &mut Ctx, state: &mut State, repos: &[RepoState], t: Target) {
    match t {
        Target::Notification(id) => open_notification(cx, state, repos, &id),
        Target::Item {
            repo,
            number,
            pr,
            url,
        } => match repo_index(repos, &repo) {
            Some(i) if pr => {
                cx.action(Action::Activate(i));
                cx.action(Action::OpenPr(number));
            }
            _ => cx.action(Action::OpenUrl(url)),
        },
        Target::Run { repo, run } => {
            cx.action(Action::Activate(repo));
            cx.action(Action::OpenRun { run, job: None });
        }
        Target::Repo(i) => {
            cx.action(Action::Activate(i));
            cx.action(Action::ShowPage(Page::Graph));
        }
    }
}

/// `main` from "CI workflow run failed for main branch".
fn branch_of_ci_title(title: &str) -> Option<&str> {
    let (_, rest) = title.rsplit_once(" for ")?;
    rest.strip_suffix(" branch").map(str::trim)
}

fn open_notification(cx: &mut Ctx, state: &State, repos: &[RepoState], id: &str) {
    let Some(n) = state
        .notifications
        .as_ref()
        .and_then(|v| v.iter().find(|n| n.id == id))
    else {
        return;
    };
    let open = repo_index(repos, &n.repo);
    match (n.kind, open, n.number) {
        (SubjectKind::PullRequest, Some(i), Some(num)) => {
            cx.action(Action::Activate(i));
            cx.action(Action::OpenPr(num));
        }
        (SubjectKind::CheckSuite, Some(i), _) => {
            let run = branch_of_ci_title(&n.title).and_then(|b| {
                repos[i]
                    .github
                    .as_ref()
                    .and_then(|g| g.runs.iter().find(|r| r.branch == b))
                    .map(|r| r.id)
            });
            cx.action(Action::Activate(i));
            match run {
                Some(run) => cx.action(Action::OpenRun { run, job: None }),
                None => cx.action(Action::ShowPage(Page::GitHub)),
            }
        }
        _ => cx.action(Action::OpenUrl(n.web_url.clone())),
    }
}

pub fn apply(cx: &mut Ctx, state: &mut State, repos: &[RepoState], act: Act) {
    match act {
        Act::SetTab(t) => {
            state.tab = t;
            if t != Tab::Overview && t != Tab::Failed && state.fetched_at.is_none() {
                fetch_inbox(cx, state);
            }
        }
        Act::Refresh => fetch_inbox(cx, state),
        Act::Activate(i) => open_target(cx, state, repos, Target::Repo(i)),
        Act::FetchAll => {
            if state.fetching() {
                return;
            }
            let (list, skipped) = fetch_list(repos, None);
            if list.is_empty() {
                cx.toast(
                    ToastKind::Info,
                    "Nothing to fetch: every open repository is busy or has no remote",
                );
                return;
            }
            let mut details = vec![format!(
                "{} in each repository, one after another:",
                fetch::command_line()
            )];
            for (name, dir) in &list {
                details.push(format!("  {name}  ({})", dir.display()));
            }
            if !skipped.is_empty() {
                details.push(format!("Skipped: {}", skipped.join(", ")));
            }
            cx.confirm(
                format!(
                    "Fetch {}?",
                    format::plural(list.len(), "repository", "repositories")
                ),
                "Downloads new commits, branches and tags from every remote and removes remote-tracking branches deleted on the remote (--prune). Local branches and working trees are not touched.",
                "Fetch all",
                false,
                details,
                Act::FetchAllConfirmed(list, skipped).into(),
            );
        }
        Act::FetchAllIn(prefix) => {
            let (list, skipped) = fetch_list(repos, Some(&prefix));
            if !list.is_empty() {
                start_fetch(cx, state, list, skipped);
            }
        }
        Act::FetchAllConfirmed(list, skipped) => start_fetch(cx, state, list, skipped),
        Act::ToggleReason(r) => {
            if let Some(p) = state.filter.reasons.iter().position(|x| *x == r) {
                state.filter.reasons.remove(p);
            } else {
                state.filter.reasons.push(r);
            }
        }
        Act::SetReasons(rs) => state.filter.reasons = rs,
        Act::SetGroup(on) => state.group = on,
        Act::ClearFilters => {
            state.filter = notify::Filter::default();
            state.search_only_open = false;
            state.text.clear();
        }
        Act::OpenNotification(id) => open_notification(cx, state, repos, &id),
        Act::OpenItem {
            repo,
            number,
            pr,
            url,
        } => open_target(
            cx,
            state,
            repos,
            Target::Item {
                repo,
                number,
                pr,
                url,
            },
        ),
        Act::OpenRun { repo, run } => open_target(cx, state, repos, Target::Run { repo, run }),
        Act::MarkRead(id) => {
            if !state.marking.insert(id.clone()) {
                return;
            }
            let req = notify::mark_read_request(&id);
            cx.remote_change("Mark a notification as read", move |dir, log| {
                let r = Gh::user(dir).send(&req, &mut log.calls);
                let result = r.map(|_| ()).map_err(|e| e.to_string());
                (
                    result
                        .clone()
                        .map(|_| format!("Marked thread {id} as read")),
                    Some(Msg::MarkedRead { id, result }),
                )
            });
        }
        Act::MarkAllRead => {
            let Some(epoch) = state.fetched_epoch else {
                cx.toast(ToastKind::Info, "Fetch the notifications first");
                return;
            };
            let at = iso_utc(epoch);
            let req = notify::mark_all_read_request(&at);
            let mut details = vec![req.display()];
            if let Some(b) = req.body_preview() {
                details.push(b);
            }
            let unread = state.unread();
            cx.confirm(
                "Mark every notification as read?",
                format!(
                    "Marks all notifications updated before the last fetch ({}) as read on GitHub, {} of them listed here. GitHub cannot mark them unread again through the API.",
                    format::absolute(epoch),
                    format::plural(unread, "unread notification", "unread notifications")
                ),
                "Mark all as read",
                true,
                details,
                Act::MarkAllReadConfirmed(at).into(),
            );
        }
        Act::MarkAllReadConfirmed(at) => {
            if state.marking_all {
                return;
            }
            state.marking_all = true;
            let req = notify::mark_all_read_request(&at);
            cx.remote_change("Mark every notification as read", move |dir, log| {
                let r = Gh::user(dir).send(&req, &mut log.calls);
                let result = r.map(|_| ()).map_err(|e| e.to_string());
                (
                    result
                        .clone()
                        .map(|_| format!("Marked notifications up to {at} as read")),
                    Some(Msg::MarkedAllRead { result }),
                )
            });
        }
        Act::OpenNth(n) => {
            let targets = inbox::targets(state, repos);
            if let Some(t) = targets.into_iter().nth(n) {
                open_target(cx, state, repos, t);
            }
        }
    }
}

/// Repositories to fetch (`name`, workdir) and those skipped, optionally
/// only under `prefix`.
fn fetch_list(
    repos: &[RepoState],
    prefix: Option<&std::path::Path>,
) -> (Vec<(String, PathBuf)>, Vec<String>) {
    let prefix = prefix.map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned()));
    let mut list = Vec::new();
    let mut skipped = Vec::new();
    for r in repos {
        if let Some(p) = &prefix {
            let dir = std::fs::canonicalize(&r.workdir).unwrap_or_else(|_| r.workdir.clone());
            if !dir.starts_with(p) {
                continue;
            }
        }
        let has_remote = r.snapshot.as_ref().is_some_and(|s| !s.remotes.is_empty());
        if r.is_busy() {
            skipped.push(format!("{} (busy)", r.name));
        } else if !has_remote {
            skipped.push(format!("{} (no remote)", r.name));
        } else {
            list.push((r.name.clone(), r.workdir.clone()));
        }
    }
    (list, skipped)
}

fn start_fetch(
    cx: &mut Ctx,
    state: &mut State,
    list: Vec<(String, PathBuf)>,
    skipped: Vec<String>,
) {
    if state.fetching() {
        return;
    }
    let progress = Arc::new(Mutex::new(FetchProgress::default()));
    state.fetch = Some(FetchRun {
        started: Instant::now(),
        names: list.iter().map(|(n, _)| n.clone()).collect(),
        progress: progress.clone(),
    });
    let label = format!(
        "Fetch {}",
        format::plural(list.len(), "repository", "repositories")
    );
    cx.remote_change(label, move |_dir, log| {
        let started = Instant::now();
        let mut outcomes = Vec::new();
        for (name, dir) in &list {
            if let Ok(mut p) = progress.lock() {
                p.current = Some(name.clone());
            }
            log.command(format!(
                "{}    (in {})",
                fetch::command_line(),
                dir.display()
            ));
            let o = fetch::run(name, dir, FETCH_TIMEOUT);
            if let Ok(mut p) = progress.lock() {
                p.done.push(o.clone());
            }
            outcomes.push(o);
        }
        if let Ok(mut p) = progress.lock() {
            p.current = None;
        }
        let mut text = fetch::summary(&outcomes);
        for o in &outcomes {
            text.push_str(&format!(
                "\n{}: {} · {} · {}",
                o.name,
                if o.ok { "ok" } else { "failed" },
                if o.ok {
                    o.changes.summary()
                } else {
                    o.output.lines().next().unwrap_or_default().to_owned()
                },
                format::millis(o.elapsed_ms)
            ));
        }
        let failed = outcomes.iter().any(|o| !o.ok);
        let result = if failed { Err(text) } else { Ok(text) };
        (
            result,
            Some(Msg::FetchedAll {
                outcomes,
                skipped,
                elapsed_ms: started.elapsed().as_millis() as u64,
            }),
        )
    });
}

pub fn on_msg(cx: &mut Ctx, state: &mut State, _repos: &[RepoState], msg: Msg) {
    let _ = cx;
    match msg {
        Msg::Inbox {
            notifications,
            search,
            rate,
            epoch,
        } => {
            state.loading = false;
            state.fetched_at = Some(Instant::now());
            state.fetched_epoch = Some(epoch);
            if rate.is_some() {
                state.rate = rate;
            }
            match notifications {
                Ok(n) => {
                    state.notifications = Some(n);
                    state.notif_error = None;
                    state.marking.clear();
                }
                Err(e) => state.notif_error = Some(e),
            }
            match search {
                Ok(s) => {
                    if !s.login.is_empty() {
                        state.login = Some(s.login.clone());
                    }
                    state.search = Some(s);
                    state.search_error = None;
                }
                Err(e) => state.search_error = Some(e),
            }
        }
        Msg::MarkedRead { id, result } => {
            state.marking.remove(&id);
            if result.is_ok()
                && let Some(list) = state.notifications.as_mut()
                && let Some(n) = list.iter_mut().find(|n| n.id == id)
            {
                n.unread = false;
            }
        }
        Msg::MarkedAllRead { result } => {
            state.marking_all = false;
            if result.is_ok()
                && let Some(list) = state.notifications.as_mut()
            {
                for n in list.iter_mut() {
                    n.unread = false;
                }
            }
        }
        Msg::FetchedAll {
            outcomes,
            skipped,
            elapsed_ms,
        } => {
            state.fetch = None;
            state.fetched = Some(FetchDone {
                at: format::now_secs(),
                elapsed_ms,
                outcomes,
                skipped,
            });
        }
    }
}

/// The home page was opened.
pub fn on_page_shown(cx: &mut Ctx, state: &mut State, _repos: &[RepoState]) {
    let stale = state.fetched_at.is_none_or(|t| t.elapsed() > STALE);
    if stale {
        fetch_inbox(cx, state);
    }
}

/// `GITVIS_SCRIPT` commands this area understands.
pub fn script(cmd: &str, arg: &str) -> Option<Vec<Action>> {
    let home = Action::ShowPage(Page::Home);
    Some(match cmd {
        "home" => vec![home],
        "home-tab" => vec![home, Act::SetTab(Tab::from_name(arg)?).into()],
        "inbox" => vec![
            home,
            Act::SetTab(Tab::Notifications).into(),
            Act::Refresh.into(),
        ],
        "inbox-filter" => {
            let reasons = match arg.trim() {
                "" | "all" => Vec::new(),
                a => a
                    .split(',')
                    .map(Reason::from_name)
                    .collect::<Option<Vec<_>>>()?,
            };
            vec![
                home,
                Act::SetTab(Tab::Notifications).into(),
                Act::SetReasons(reasons).into(),
            ]
        }
        "inbox-item" => vec![Act::OpenNth(arg.trim().parse().ok()?).into()],
        "inbox-group" => vec![Act::SetGroup(!matches!(arg.trim(), "off" | "no" | "0")).into()],
        "fetch-all" => vec![home, Act::FetchAll.into()],
        "fetch-all-in" => vec![home, Act::FetchAllIn(PathBuf::from(arg.trim())).into()],
        _ => return None,
    })
}

/// A dot on the Home rail entry and a tooltip line: unread notifications
/// and review requests from the last fetch.
pub fn rail_badge(state: &State) -> Option<(crate::theme::Tone, String)> {
    let unread = state.unread();
    let reviews = state.review_requests();
    if unread == 0 && reviews == 0 {
        return None;
    }
    Some((
        crate::theme::Tone::Accent,
        format!(
            "{}, {}",
            format::plural(unread, "unread notification", "unread notifications"),
            format::plural(reviews, "review request", "review requests")
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ci_titles() {
        assert_eq!(
            branch_of_ci_title("CI workflow run failed for main branch"),
            Some("main")
        );
        assert_eq!(
            branch_of_ci_title("Deploy workflow run cancelled for feature/x branch"),
            Some("feature/x")
        );
        assert_eq!(branch_of_ci_title("Something else"), None);
    }

    #[test]
    fn script_commands() {
        assert_eq!(script("home", "").unwrap().len(), 1);
        assert_eq!(script("inbox", "").unwrap().len(), 3);
        assert!(script("inbox-filter", "review,mention").is_some());
        assert!(script("inbox-filter", "nonsense").is_none());
        assert!(script("inbox-item", "x").is_none());
        assert!(script("home-tab", "failed").is_some());
        assert!(script("nope", "").is_none());
    }

    #[test]
    fn prefs_round_trip() {
        let mut s = State::default();
        s.filter.reasons = vec![Reason::Mention];
        s.filter.unread_only = true;
        s.group = true;
        s.login = Some("octo".into());
        s.tab = Tab::Reviews;
        let p = s.prefs();
        let mut t = State::default();
        t.apply_prefs(&p);
        assert_eq!(t.filter.reasons, vec![Reason::Mention]);
        assert!(t.filter.unread_only && t.group);
        assert_eq!(t.login(), "octo");
        assert_eq!(t.tab, Tab::Reviews);
    }
}
