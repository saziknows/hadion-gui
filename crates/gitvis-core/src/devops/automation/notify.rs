//! Desktop notifications: what one poll asks GitHub, how two polls are
//! compared (only new events notify, nothing on the first poll), quiet
//! hours, and delivery through `notify-rust`.
//!
//! One poll is a cheap, fixed set of requests:
//! - `GET notifications?all=false&since=…` (deployment approvals waiting for
//!   you, mentions in threads you already follow);
//! - the home inbox GraphQL search (review requests, mentions), cost 1;
//! - `GET repos/{o}/{r}/actions/runs?actor={login}&per_page=20` for each open
//!   repository with a GitHub remote, capped at [`MAX_REPOS`].
//!
//! Delivery honours `GITVIS_NOTIFY`: `log` appends to a file
//! (`GITVIS_NOTIFY_LOG`, default `gitvis-notify.log` in the temp directory)
//! instead of showing anything, `off` drops them. Scripted runs
//! (`GITVIS_SCRIPT`) and this crate's tests always log.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::devops::gh::{Gh, Request, decode, encode};
use crate::devops::home::notify::{self as inbox, Notification, Reason};
use crate::devops::home::search::{self, Item};
use crate::github::{ApiCall, Outcome, RateLimit, parse_time};

/// Runs are polled for at most this many repositories.
pub const MAX_REPOS: usize = 8;
/// Runs per repository per poll.
pub const RUNS_PER_REPO: u32 = 20;
/// Polling pauses below this many REST requests left.
pub const LOW_RATE: u32 = 300;
/// Remembered thread ids (oldest dropped first).
const MAX_THREADS: usize = 2_000;

/// Which finished runs notify.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunWatch {
    Off,
    /// Failed, timed out, startup failure.
    #[default]
    Failures,
    /// Every completed run (cancelled and skipped ones too).
    All,
}

/// What the user wants to hear about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Watch {
    pub runs: RunWatch,
    pub reviews: bool,
    pub mentions: bool,
    pub approvals: bool,
}

impl Watch {
    pub fn notifications_needed(&self) -> bool {
        self.mentions || self.approvals
    }

    pub fn search_needed(&self) -> bool {
        self.reviews || self.mentions
    }
}

/// A workflow run as one poll saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSeen {
    /// `owner/repo`.
    pub repo: String,
    pub id: u64,
    pub attempt: u32,
    /// Workflow name.
    pub name: String,
    pub branch: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub url: String,
    /// Commit or pull request title.
    pub title: String,
    pub updated: Option<i64>,
}

impl RunSeen {
    pub fn outcome(&self) -> Outcome {
        Outcome::from_parts(&self.status, self.conclusion.as_deref())
    }

    pub fn completed(&self) -> bool {
        self.status == "completed"
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawRun {
    id: u64,
    name: String,
    status: String,
    conclusion: Option<String>,
    head_branch: Option<String>,
    html_url: String,
    run_attempt: u32,
    display_title: String,
    updated_at: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawRuns {
    workflow_runs: Vec<RawRun>,
}

/// `GET notifications?all=false&per_page=50[&since=…]`.
pub fn notifications_request(since: Option<&str>) -> Request {
    let mut r = inbox::list_request(50);
    if let Some(s) = since {
        r.endpoint.push_str(&format!("&since={}", encode(s)));
    }
    r
}

/// `GET repos/{o}/{r}/actions/runs?actor={login}&per_page=20&exclude_pull_requests=true`.
pub fn runs_request(owner: &str, repo: &str, login: &str) -> Request {
    Request::get(format!(
        "repos/{owner}/{repo}/actions/runs?actor={}&per_page={RUNS_PER_REPO}&exclude_pull_requests=true",
        encode(login)
    ))
}

/// Parse an `actions/runs` answer.
pub fn parse_runs(body: &[u8], repo: &str) -> Result<Vec<RunSeen>> {
    let raw: RawRuns = decode(body, "workflow runs")?;
    Ok(raw
        .workflow_runs
        .into_iter()
        .map(|r| RunSeen {
            repo: repo.to_owned(),
            id: r.id,
            attempt: r.run_attempt.max(1),
            name: r.name,
            branch: r.head_branch.unwrap_or_default(),
            status: r.status,
            conclusion: r.conclusion,
            url: r.html_url,
            title: r.display_title,
            updated: parse_time(&r.updated_at),
        })
        .collect())
}

/// What a poll is asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub watch: Watch,
    /// `since` for the notifications (ISO 8601), `None` on the first poll.
    pub since: Option<String>,
    /// `(owner, repo)` of the repositories whose runs are read.
    pub repos: Vec<(String, String)>,
    /// The GitHub login, when known (runs need it).
    pub login: Option<String>,
}

impl Plan {
    /// The requests as the settings page and the log show them.
    pub fn describe(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.watch.notifications_needed() {
            out.push(match &self.since {
                Some(s) => notifications_request(Some(s)).display(),
                None => format!(
                    "{}&since=<last check>",
                    notifications_request(None).display()
                ),
            });
        }
        if self.watch.search_needed() {
            out.push("gh api graphql (inbox search: review requests, mentions; cost 1)".into());
        } else if self.login.is_none() && self.watch.runs != RunWatch::Off {
            out.push("gh api user".into());
        }
        if self.watch.runs != RunWatch::Off {
            let login = self.login.as_deref().unwrap_or("<login>");
            for (o, r) in &self.repos {
                out.push(runs_request(o, r, login).display());
            }
        }
        out
    }

    /// Number of requests one poll makes.
    pub fn cost(&self) -> usize {
        self.describe().len()
    }
}

/// What one poll saw. `None`: not asked, or the request failed (the
/// previous state is kept then).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Poll {
    pub at: i64,
    pub login: Option<String>,
    pub reviews: Option<Vec<Item>>,
    pub mentions: Option<Vec<Item>>,
    pub threads: Option<Vec<Notification>>,
    /// Per repository that answered (`owner/repo`).
    pub runs: Vec<(String, Vec<RunSeen>)>,
    /// Errors, one line each (`notifications: HTTP 401`).
    pub errors: Vec<String>,
    /// REST rate limit after the last REST call.
    pub rate: Option<RateLimit>,
    /// GraphQL points left after the search.
    pub graphql_left: Option<u64>,
    /// The poll stopped early because the REST budget ran low.
    pub stopped_low: bool,
}

/// Run one poll.
pub fn poll(gh: &Gh, plan: &Plan, now: i64, calls: &mut Vec<ApiCall>) -> Poll {
    let mut p = Poll {
        at: now,
        login: plan.login.clone(),
        ..Default::default()
    };
    let rate_of = |calls: &[ApiCall]| calls.iter().rev().find_map(|c| c.rate);
    if plan.watch.notifications_needed() {
        match gh
            .send(&notifications_request(plan.since.as_deref()), calls)
            .and_then(|b| inbox::parse(&b))
        {
            Ok(n) => p.threads = Some(n),
            Err(e) => p.errors.push(format!("notifications: {e}")),
        }
        p.rate = rate_of(calls).or(p.rate);
    }
    if plan.watch.search_needed() {
        match gh
            .graphql(search::QUERY, search::variables(50), calls)
            .and_then(|d| search::parse(&d))
        {
            Ok(r) => {
                if !r.login.is_empty() {
                    p.login = Some(r.login.clone());
                }
                p.graphql_left = r.rate.as_ref().map(|g| g.remaining);
                p.reviews = Some(r.review.items);
                p.mentions = Some(r.mentions.items);
            }
            Err(e) => p.errors.push(format!("search: {e}")),
        }
    }
    if plan.watch.runs == RunWatch::Off || plan.repos.is_empty() {
        return p;
    }
    if p.login.is_none() {
        match gh.login(calls) {
            Ok(l) => p.login = Some(l),
            Err(e) => {
                p.errors.push(format!("user: {e}"));
                return p;
            }
        }
        p.rate = rate_of(calls).or(p.rate);
    }
    let login = p.login.clone().unwrap_or_default();
    for (owner, repo) in plan.repos.iter().take(MAX_REPOS) {
        if p.rate.is_some_and(|r| r.remaining < LOW_RATE) {
            p.stopped_low = true;
            break;
        }
        let full = format!("{owner}/{repo}");
        match gh
            .send(&runs_request(owner, repo, &login), calls)
            .and_then(|b| parse_runs(&b, &full))
        {
            Ok(r) => p.runs.push((full, r)),
            Err(e) => p.errors.push(format!("{full} runs: {e}")),
        }
        p.rate = rate_of(calls).or(p.rate);
    }
    p
}

/// What happened since the previous poll.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    RunFailed,
    /// Finished without failing (success, cancelled, skipped, …).
    RunFinished,
    ReviewRequested,
    Mentioned,
    ApprovalRequested,
}

impl EventKind {
    pub fn label(self) -> &'static str {
        match self {
            EventKind::RunFailed => "Run failed",
            EventKind::RunFinished => "Run finished",
            EventKind::ReviewRequested => "Review requested",
            EventKind::Mentioned => "Mentioned",
            EventKind::ApprovalRequested => "Approval waiting",
        }
    }
}

/// One new event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub kind: EventKind,
    /// Unique per event (`run:owner/repo:123:1`, `review:owner/repo#42`).
    pub key: String,
    /// `owner/repo`.
    pub repo: String,
    /// One line: `CI failed on feature/login`, `#42 Add retry`.
    pub title: String,
    /// Second line: who, which branch, the conclusion.
    pub detail: String,
    pub url: String,
    /// Pull request or issue number.
    pub number: Option<u64>,
    pub pr: bool,
    pub run: Option<u64>,
    /// When it happened (epoch seconds), or when it was seen.
    pub at: i64,
}

/// What earlier polls saw.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    /// A first poll has been absorbed.
    pub primed: bool,
    reviews: HashSet<String>,
    mentions: HashSet<String>,
    /// Thread id → last `updated_at` seen.
    threads: HashMap<String, Option<i64>>,
    /// Per repository: completed `(run id, attempt)`.
    runs: HashMap<String, HashSet<(u64, u32)>>,
}

impl Seen {
    /// Forget everything (the next poll is a first poll again).
    pub fn reset(&mut self) {
        *self = Seen::default();
    }

    /// Repositories whose runs are being followed.
    pub fn run_repos(&self) -> usize {
        self.runs.len()
    }
}

fn item_key(i: &Item) -> String {
    format!("{}#{}", i.repo.to_ascii_lowercase(), i.number)
}

fn is_failure(o: Outcome) -> bool {
    matches!(
        o,
        Outcome::Failure | Outcome::TimedOut | Outcome::ActionRequired
    )
}

/// Compare a poll with what earlier polls saw, remember it, and return the
/// new events the user asked for, newest first. The first poll only
/// remembers (no events); a repository seen for the first time is absorbed
/// the same way, so opening a repository does not replay its history.
/// `open`: lower-case `owner/repo` the events are limited to (`None`: all).
pub fn diff(
    seen: &mut Seen,
    poll: &Poll,
    watch: &Watch,
    open: Option<&HashSet<String>>,
) -> Vec<Event> {
    let first = !seen.primed;
    let mut events: Vec<Event> = Vec::new();
    let in_scope = |repo: &str| open.is_none_or(|o| o.contains(&repo.to_ascii_lowercase()));

    // Runs.
    for (repo, runs) in &poll.runs {
        let key = repo.to_ascii_lowercase();
        let known = seen.runs.contains_key(&key);
        let set = seen.runs.entry(key).or_default();
        for r in runs.iter().filter(|r| r.completed()) {
            let id = (r.id, r.attempt);
            if !set.insert(id) || first || !known {
                continue;
            }
            let failed = is_failure(r.outcome());
            let wanted = match watch.runs {
                RunWatch::Off => false,
                RunWatch::Failures => failed,
                RunWatch::All => true,
            };
            if !wanted || !in_scope(repo) {
                continue;
            }
            let verb = if failed {
                "failed"
            } else {
                r.outcome().label()
            };
            let mut detail = r.title.clone();
            if r.attempt > 1 {
                detail = format!("attempt {} · {detail}", r.attempt);
            }
            events.push(Event {
                kind: if failed {
                    EventKind::RunFailed
                } else {
                    EventKind::RunFinished
                },
                key: format!("run:{}:{}:{}", repo.to_ascii_lowercase(), r.id, r.attempt),
                repo: repo.clone(),
                title: if r.branch.is_empty() {
                    format!("{} {verb}", r.name)
                } else {
                    format!("{} {verb} on {}", r.name, r.branch)
                },
                detail,
                url: r.url.clone(),
                number: None,
                pr: false,
                run: Some(r.id),
                at: r.updated.unwrap_or(poll.at),
            });
        }
        // Keep only what the window still shows.
        let current: HashSet<(u64, u32)> = runs.iter().map(|r| (r.id, r.attempt)).collect();
        if set.len() > 4 * RUNS_PER_REPO as usize {
            set.retain(|k| current.contains(k));
        }
    }

    // Review requests.
    if let Some(items) = &poll.reviews {
        let now: HashSet<String> = items.iter().map(item_key).collect();
        for i in items {
            let k = item_key(i);
            if first || seen.reviews.contains(&k) || !watch.reviews || !in_scope(&i.repo) {
                continue;
            }
            events.push(Event {
                kind: EventKind::ReviewRequested,
                key: format!("review:{k}"),
                repo: i.repo.clone(),
                title: format!("#{} {}", i.number, i.title),
                detail: if i.author.is_empty() {
                    "Review requested".to_owned()
                } else {
                    format!("Review requested by @{}", i.author)
                },
                url: i.url.clone(),
                number: Some(i.number),
                pr: i.pr,
                run: None,
                at: i.updated.unwrap_or(poll.at),
            });
        }
        seen.reviews = now;
    }

    // Mentions: new items in the search, new mention threads.
    let mut mention_keys: HashSet<String> = HashSet::new();
    if let Some(items) = &poll.mentions {
        let now: HashSet<String> = items.iter().map(item_key).collect();
        for i in items {
            let k = item_key(i);
            if first || seen.mentions.contains(&k) || !watch.mentions || !in_scope(&i.repo) {
                continue;
            }
            mention_keys.insert(k.clone());
            events.push(Event {
                kind: EventKind::Mentioned,
                key: format!("mention:{k}"),
                repo: i.repo.clone(),
                title: format!("#{} {}", i.number, i.title),
                detail: if i.author.is_empty() {
                    "You were mentioned".to_owned()
                } else {
                    format!("You were mentioned · opened by @{}", i.author)
                },
                url: i.url.clone(),
                number: Some(i.number),
                pr: i.pr,
                run: None,
                at: i.updated.unwrap_or(poll.at),
            });
        }
        seen.mentions = now;
    }
    if let Some(threads) = &poll.threads {
        for n in threads {
            let before = seen.threads.insert(n.id.clone(), n.updated);
            let is_new = before.is_none();
            let updated = before.is_some_and(|b| b != n.updated);
            if first || !in_scope(&n.repo) {
                continue;
            }
            match n.reason {
                Reason::Mention | Reason::TeamMention if is_new && watch.mentions => {
                    let k = n
                        .number
                        .map(|num| format!("{}#{num}", n.repo.to_ascii_lowercase()))
                        .unwrap_or_else(|| n.id.clone());
                    if !mention_keys.insert(k.clone()) {
                        continue;
                    }
                    events.push(Event {
                        kind: EventKind::Mentioned,
                        key: format!("thread:{}:{:?}", n.id, n.updated),
                        repo: n.repo.clone(),
                        title: match n.number {
                            Some(num) => format!("#{num} {}", n.title),
                            None => n.title.clone(),
                        },
                        detail: n.reason.explain().to_owned(),
                        url: n.web_url.clone(),
                        number: n.number,
                        pr: n.kind == inbox::SubjectKind::PullRequest,
                        run: None,
                        at: n.updated.unwrap_or(poll.at),
                    });
                }
                Reason::ApprovalRequested if (is_new || updated) && watch.approvals => {
                    events.push(Event {
                        kind: EventKind::ApprovalRequested,
                        key: format!("approval:{}:{:?}", n.id, n.updated),
                        repo: n.repo.clone(),
                        title: n.title.clone(),
                        detail: "A deployment is waiting for your approval".to_owned(),
                        url: n.web_url.clone(),
                        number: n.number,
                        pr: false,
                        run: None,
                        at: n.updated.unwrap_or(poll.at),
                    });
                }
                _ => {}
            }
        }
        if seen.threads.len() > MAX_THREADS {
            let mut by_age: Vec<(String, Option<i64>)> =
                seen.threads.iter().map(|(k, v)| (k.clone(), *v)).collect();
            by_age.sort_by_key(|(_, t)| *t);
            for (k, _) in by_age.into_iter().take(seen.threads.len() - MAX_THREADS) {
                seen.threads.remove(&k);
            }
        }
    }
    // Only a poll that read something primes; a failed first poll does not.
    if poll.threads.is_some() || poll.reviews.is_some() || !poll.runs.is_empty() {
        seen.primed = true;
    }
    events.sort_by_key(|e| std::cmp::Reverse(e.at));
    events
}

/// `22:30` → minutes after midnight.
pub fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.trim().parse().ok()?, m.trim().parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// Is `minute` (after local midnight) inside quiet hours `from`..`to`?
/// Windows may wrap past midnight (22:00–07:00); `from == to` is never quiet.
pub fn in_quiet_hours(minute: u32, from: u32, to: u32) -> bool {
    if from == to {
        return false;
    }
    if from < to {
        minute >= from && minute < to
    } else {
        minute >= from || minute < to
    }
}

/// One desktop notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Desktop {
    pub title: String,
    pub body: String,
}

/// What to show for a batch of events: one notification each for up to
/// three, else one summary (stacked toasts read as noise).
pub fn desktop_for(events: &[Event]) -> Vec<Desktop> {
    if events.len() <= 3 {
        return events
            .iter()
            .map(|e| Desktop {
                title: format!("{} · {}", e.kind.label(), e.repo),
                body: format!("{}\n{}", e.title, e.detail),
            })
            .collect();
    }
    let mut counts: Vec<(EventKind, usize)> = Vec::new();
    for e in events {
        match counts.iter_mut().find(|(k, _)| *k == e.kind) {
            Some((_, n)) => *n += 1,
            None => counts.push((e.kind, 1)),
        }
    }
    let parts: Vec<String> = counts
        .iter()
        .map(|(k, n)| format!("{n} × {}", k.label().to_lowercase()))
        .collect();
    let mut body = parts.join(", ");
    for e in events.iter().take(3) {
        body.push_str(&format!("\n{} · {}", e.repo, e.title));
    }
    if events.len() > 3 {
        body.push_str(&format!("\n… and {} more in gitvis", events.len() - 3));
    }
    vec![Desktop {
        title: format!("gitvis · {} new", events.len()),
        body,
    }]
}

/// Where notifications go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// The operating system's notification centre.
    Desktop,
    /// Appended to a file (tests, scripted runs, `GITVIS_NOTIFY=log`).
    Log(PathBuf),
    /// Dropped (`GITVIS_NOTIFY=off`).
    Off,
}

impl Delivery {
    pub fn describe(&self) -> String {
        match self {
            Delivery::Desktop => "Desktop notifications".to_owned(),
            Delivery::Log(p) => format!("Written to {} (GITVIS_NOTIFY=log)", p.display()),
            Delivery::Off => "Not shown (GITVIS_NOTIFY=off)".to_owned(),
        }
    }
}

fn log_path() -> PathBuf {
    std::env::var_os("GITVIS_NOTIFY_LOG")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("gitvis-notify.log"))
}

/// The delivery from the environment (see the module docs).
pub fn delivery() -> Delivery {
    let mode = std::env::var("GITVIS_NOTIFY").unwrap_or_default();
    match mode.trim().to_ascii_lowercase().as_str() {
        "log" => Delivery::Log(log_path()),
        "off" | "0" | "none" => Delivery::Off,
        _ if cfg!(test) || std::env::var_os("GITVIS_SCRIPT").is_some() => Delivery::Log(log_path()),
        _ => Delivery::Desktop,
    }
}

/// Show (or log) notifications; returns where they went.
pub fn deliver(notes: &[Desktop], now_iso: &str) -> std::result::Result<String, String> {
    deliver_to(&delivery(), notes, now_iso)
}

/// [`deliver`] with an explicit delivery.
pub fn deliver_to(
    d: &Delivery,
    notes: &[Desktop],
    now_iso: &str,
) -> std::result::Result<String, String> {
    match d {
        Delivery::Off => Ok(format!("{} dropped (GITVIS_NOTIFY=off)", notes.len())),
        Delivery::Log(path) => {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            for n in notes {
                writeln!(f, "{now_iso}\t{}\t{}", n.title, n.body.replace('\n', " | "))
                    .map_err(|e| format!("{}: {e}", path.display()))?;
            }
            Ok(format!("{} written to {}", notes.len(), path.display()))
        }
        Delivery::Desktop => {
            for n in notes {
                notify_rust::Notification::new()
                    .appname("gitvis")
                    .summary(&n.title)
                    .body(&n.body)
                    .show()
                    .map(|_| ())
                    .map_err(|e| format!("Desktop notification: {e}"))?;
            }
            Ok(format!("{} shown", notes.len()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(repo: &str, number: u64, title: &str) -> Item {
        Item {
            pr: true,
            number,
            title: title.into(),
            url: format!("https://github.com/{repo}/pull/{number}"),
            repo: repo.into(),
            author: "dana".into(),
            created: None,
            updated: Some(1_000 + number as i64),
            draft: false,
            review_decision: None,
            checks: None,
            head: None,
            base: None,
            comments: 0,
            labels: vec![],
        }
    }

    fn run(repo: &str, id: u64, status: &str, conclusion: Option<&str>) -> RunSeen {
        RunSeen {
            repo: repo.into(),
            id,
            attempt: 1,
            name: "CI".into(),
            branch: "feature/login".into(),
            status: status.into(),
            conclusion: conclusion.map(str::to_owned),
            url: format!("https://github.com/{repo}/actions/runs/{id}"),
            title: "Fix login".into(),
            updated: Some(2_000 + id as i64),
        }
    }

    fn thread(id: &str, reason: &str, updated: &str) -> Notification {
        let json = format!(
            r#"[{{"id":"{id}","unread":true,"reason":"{reason}","updated_at":"{updated}",
               "subject":{{"title":"Deploy to production","url":null,"type":"CheckSuite"}},
               "repository":{{"full_name":"acme/web","html_url":"https://github.com/acme/web"}}}}]"#
        );
        inbox::parse(json.as_bytes()).unwrap().remove(0)
    }

    const ALL: Watch = Watch {
        runs: RunWatch::All,
        reviews: true,
        mentions: true,
        approvals: true,
    };

    #[test]
    fn first_poll_only_remembers() {
        let mut seen = Seen::default();
        let p = Poll {
            at: 10,
            reviews: Some(vec![item("acme/web", 1, "A")]),
            mentions: Some(vec![]),
            threads: Some(vec![thread(
                "9",
                "approval_requested",
                "2026-10-27T10:00:00Z",
            )]),
            runs: vec![(
                "acme/web".into(),
                vec![run("acme/web", 5, "completed", Some("failure"))],
            )],
            ..Default::default()
        };
        assert!(diff(&mut seen, &p, &ALL, None).is_empty());
        assert!(seen.primed);
        // The same poll again: still nothing.
        assert!(diff(&mut seen, &p, &ALL, None).is_empty());
    }

    #[test]
    fn a_failed_first_poll_does_not_prime() {
        let mut seen = Seen::default();
        let p = Poll {
            errors: vec!["notifications: HTTP 500".into()],
            ..Default::default()
        };
        assert!(diff(&mut seen, &p, &ALL, None).is_empty());
        assert!(!seen.primed);
    }

    #[test]
    fn new_completed_runs_and_review_requests() {
        let mut seen = Seen::default();
        let first = Poll {
            reviews: Some(vec![item("acme/web", 1, "Old")]),
            runs: vec![(
                "acme/web".into(),
                vec![
                    run("acme/web", 5, "completed", Some("failure")),
                    run("acme/web", 6, "in_progress", None),
                ],
            )],
            ..Default::default()
        };
        diff(&mut seen, &first, &ALL, None);
        let second = Poll {
            at: 100,
            reviews: Some(vec![item("acme/web", 1, "Old"), item("Acme/Web", 2, "New")]),
            runs: vec![(
                "acme/web".into(),
                vec![
                    run("acme/web", 7, "completed", Some("success")),
                    run("acme/web", 6, "completed", Some("failure")),
                    run("acme/web", 5, "completed", Some("failure")),
                ],
            )],
            ..Default::default()
        };
        let ev = diff(&mut seen, &second, &ALL, None);
        let kinds: Vec<(EventKind, &str)> = ev.iter().map(|e| (e.kind, e.key.as_str())).collect();
        assert_eq!(
            kinds,
            vec![
                (EventKind::RunFinished, "run:acme/web:7:1"),
                (EventKind::RunFailed, "run:acme/web:6:1"),
                (EventKind::ReviewRequested, "review:acme/web#2"),
            ]
        );
        assert_eq!(ev[1].title, "CI failed on feature/login");
        assert_eq!(ev[1].run, Some(6));
        assert_eq!(ev[2].number, Some(2));
        assert_eq!(ev[2].detail, "Review requested by @dana");
        // Failures only.
        let mut seen2 = seen.clone();
        let mut third = second.clone();
        third.runs[0]
            .1
            .insert(0, run("acme/web", 8, "completed", Some("success")));
        third.runs[0]
            .1
            .insert(0, run("acme/web", 9, "completed", Some("timed_out")));
        let w = Watch {
            runs: RunWatch::Failures,
            ..ALL
        };
        let ev = diff(&mut seen2, &third, &w, None);
        let ids: Vec<Option<u64>> = ev.iter().map(|e| e.run).collect();
        assert_eq!(ids, vec![Some(9)]);
        // Nothing new the next time.
        assert!(diff(&mut seen2, &third, &w, None).is_empty());
    }

    #[test]
    fn reruns_and_new_repositories() {
        let mut seen = Seen::default();
        let p1 = Poll {
            runs: vec![(
                "acme/web".into(),
                vec![run("acme/web", 5, "completed", Some("failure"))],
            )],
            ..Default::default()
        };
        diff(&mut seen, &p1, &ALL, None);
        // A second attempt of run 5, and a repository that was just opened.
        let mut again = run("acme/web", 5, "completed", Some("success"));
        again.attempt = 2;
        let p2 = Poll {
            runs: vec![
                ("acme/web".into(), vec![again]),
                (
                    "acme/api".into(),
                    vec![run("acme/api", 1, "completed", Some("failure"))],
                ),
            ],
            ..Default::default()
        };
        let ev = diff(&mut seen, &p2, &ALL, None);
        assert_eq!(ev.len(), 1, "the new repository is absorbed: {ev:?}");
        assert_eq!(ev[0].key, "run:acme/web:5:2");
        assert!(ev[0].detail.starts_with("attempt 2"));
        assert_eq!(seen.run_repos(), 2);
        // The new repository's next failure does notify.
        let p3 = Poll {
            runs: vec![(
                "acme/api".into(),
                vec![
                    run("acme/api", 2, "completed", Some("failure")),
                    run("acme/api", 1, "completed", Some("failure")),
                ],
            )],
            ..Default::default()
        };
        let ev = diff(&mut seen, &p3, &ALL, None);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].repo, "acme/api");
    }

    #[test]
    fn mentions_approvals_and_scope() {
        let mut seen = Seen::default();
        let base = Poll {
            mentions: Some(vec![]),
            threads: Some(vec![thread(
                "1",
                "approval_requested",
                "2026-10-27T10:00:00Z",
            )]),
            ..Default::default()
        };
        diff(&mut seen, &base, &ALL, None);
        let p = Poll {
            mentions: Some(vec![
                item("acme/web", 3, "Ping"),
                item("other/x", 4, "Elsewhere"),
            ]),
            threads: Some(vec![
                // Updated again: a new approval request on the same thread.
                thread("1", "approval_requested", "2026-10-27T11:00:00Z"),
                thread("2", "subscribed", "2026-10-27T11:00:00Z"),
            ]),
            ..Default::default()
        };
        let open: HashSet<String> = ["acme/web".to_owned()].into_iter().collect();
        let ev = diff(&mut seen, &p, &ALL, Some(&open));
        let kinds: Vec<EventKind> = ev.iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&EventKind::Mentioned));
        assert!(kinds.contains(&EventKind::ApprovalRequested));
        assert_eq!(
            ev.len(),
            2,
            "other/x is out of scope, subscribed is not watched: {ev:?}"
        );
        // Unwatched kinds are remembered silently.
        let mut s2 = Seen::default();
        diff(&mut s2, &base, &ALL, None);
        let quiet = Watch {
            mentions: false,
            approvals: false,
            ..ALL
        };
        assert!(diff(&mut s2, &p, &quiet, None).is_empty());
        let mut later = p.clone();
        later.at = 999;
        assert!(diff(&mut s2, &later, &ALL, None).is_empty(), "already seen");
    }

    #[test]
    fn mention_threads_dedupe_with_the_search() {
        let mut seen = Seen::default();
        diff(
            &mut seen,
            &Poll {
                mentions: Some(vec![]),
                threads: Some(vec![]),
                ..Default::default()
            },
            &ALL,
            None,
        );
        let json = r#"[{"id":"77","unread":true,"reason":"mention","updated_at":"2026-10-27T10:00:00Z",
            "subject":{"title":"Ping","url":"https://api.github.com/repos/acme/web/issues/3","type":"Issue"},
            "repository":{"full_name":"acme/web","html_url":"https://github.com/acme/web"}}]"#;
        let t = inbox::parse(json.as_bytes()).unwrap();
        let p = Poll {
            mentions: Some(vec![item("acme/web", 3, "Ping")]),
            threads: Some(t),
            ..Default::default()
        };
        let ev = diff(&mut seen, &p, &ALL, None);
        assert_eq!(ev.len(), 1, "{ev:?}");
    }

    #[test]
    fn requests_and_plan() {
        assert_eq!(
            notifications_request(Some("2026-10-27T10:00:00Z")).endpoint,
            "notifications?all=false&per_page=50&since=2026-10-27T10%3A00%3A00Z"
        );
        assert_eq!(
            notifications_request(None).endpoint,
            "notifications?all=false&per_page=50"
        );
        assert_eq!(
            runs_request("acme", "web", "octo-cat").endpoint,
            "repos/acme/web/actions/runs?actor=octo-cat&per_page=20&exclude_pull_requests=true"
        );
        let plan = Plan {
            watch: ALL,
            since: None,
            repos: vec![("acme".into(), "web".into()), ("acme".into(), "api".into())],
            login: Some("octo".into()),
        };
        assert_eq!(plan.cost(), 4);
        let w = Watch {
            runs: RunWatch::Off,
            reviews: false,
            mentions: false,
            approvals: true,
        };
        let only = Plan { watch: w, ..plan };
        assert_eq!(only.cost(), 1);
    }

    #[test]
    fn parses_runs() {
        let body = br#"{"total_count":1,"workflow_runs":[{"id":11,"name":"CI","status":"completed",
            "conclusion":"failure","head_branch":"main","html_url":"https://github.com/a/b/actions/runs/11",
            "run_attempt":2,"display_title":"Fix","updated_at":"2026-10-27T10:00:00Z","actor":{"login":"octo"}}]}"#;
        let r = parse_runs(body, "a/b").unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].id, r[0].attempt), (11, 2));
        assert!(r[0].completed());
        assert_eq!(r[0].outcome(), Outcome::Failure);
        assert_eq!(r[0].updated, parse_time("2026-10-27T10:00:00Z"));
    }

    #[test]
    fn quiet_hours() {
        assert_eq!(parse_hhmm("22:30"), Some(22 * 60 + 30));
        assert_eq!(parse_hhmm("7:05"), Some(425));
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("nope"), None);
        let (from, to) = (22 * 60, 7 * 60);
        assert!(in_quiet_hours(23 * 60, from, to));
        assert!(in_quiet_hours(60, from, to));
        assert!(!in_quiet_hours(12 * 60, from, to));
        assert!(!in_quiet_hours(7 * 60, from, to));
        assert!(in_quiet_hours(13 * 60, 12 * 60, 14 * 60));
        assert!(!in_quiet_hours(13 * 60, 600, 600));
    }

    #[test]
    fn desktop_batches() {
        let e = |i: u64, kind| Event {
            kind,
            key: format!("k{i}"),
            repo: "acme/web".into(),
            title: format!("t{i}"),
            detail: "d".into(),
            url: String::new(),
            number: None,
            pr: false,
            run: None,
            at: 0,
        };
        let few = vec![e(1, EventKind::RunFailed), e(2, EventKind::ReviewRequested)];
        let d = desktop_for(&few);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].title, "Run failed · acme/web");
        let many: Vec<Event> = (0..5)
            .map(|i| {
                e(
                    i,
                    if i < 3 {
                        EventKind::RunFailed
                    } else {
                        EventKind::Mentioned
                    },
                )
            })
            .collect();
        let d = desktop_for(&many);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].title, "gitvis · 5 new");
        assert!(d[0].body.starts_with("3 × run failed, 2 × mentioned"));
        assert!(d[0].body.ends_with("… and 2 more in gitvis"));
    }

    #[test]
    fn tests_never_show_desktop_notifications() {
        // cfg(test) forces the log unless GITVIS_NOTIFY says otherwise.
        assert_ne!(delivery(), Delivery::Desktop);
        let dir = std::env::temp_dir().join(format!("gitvis-notify-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("n.log");
        let d = Delivery::Log(path.clone());
        let r = deliver_to(
            &d,
            &[Desktop {
                title: "T".into(),
                body: "a\nb".into(),
            }],
            "2026-10-27T10:00:00Z",
        )
        .unwrap();
        assert!(r.contains("1 written"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "2026-10-27T10:00:00Z\tT\ta | b\n");
        assert!(
            deliver_to(&Delivery::Off, &[], "x")
                .unwrap()
                .contains("dropped")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
