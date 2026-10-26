//! GitHub pull requests, workflow runs, jobs, logs and checks through the
//! `gh` CLI, which handles authentication so gitvis never stores a token.
//!
//! Every call is recorded as an [`ApiCall`] (command, size, time, remaining
//! rate limit) so the UI can show exactly what was asked of GitHub.

use std::path::Path;
use std::time::Instant;

use serde::Deserialize;

use crate::{Error, Result, cli};

/// `(owner, repo)` for a github.com remote URL in any common form.
pub fn parse_remote(url: &str) -> Option<(String, String)> {
    let u = url.trim();
    let rest = u
        .strip_prefix("https://github.com/")
        .or_else(|| u.strip_prefix("http://github.com/"))
        .or_else(|| u.strip_prefix("ssh://git@github.com/"))
        .or_else(|| u.strip_prefix("git@github.com:"))
        .or_else(|| u.strip_prefix("github.com/"))?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let mut parts = rest.split('/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim();
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return None;
    }
    Some((owner.to_owned(), repo.to_owned()))
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `2026-10-26T04:50:27Z` (optionally with a fraction and a `±HH:MM` offset)
/// as seconds since the epoch plus the millisecond part.
pub fn parse_timestamp(s: &str) -> Option<(i64, u32)> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut rest = &s[19..];
    let mut millis = 0u32;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits: String = frac.chars().take_while(|c| c.is_ascii_digit()).collect();
        let mut ms = digits.clone();
        ms.truncate(3);
        while ms.len() < 3 {
            ms.push('0');
        }
        millis = ms.parse().unwrap_or(0);
        rest = &frac[digits.len()..];
    }
    let offset = match rest {
        "" | "Z" | "z" => 0,
        o if o.len() == 6 && (o.starts_with('+') || o.starts_with('-')) => {
            let oh: i64 = o[1..3].parse().ok()?;
            let om: i64 = o[4..6].parse().ok()?;
            let sign = if o.starts_with('-') { -1 } else { 1 };
            sign * (oh * 3600 + om * 60)
        }
        _ => return None,
    };
    let days = days_from_civil(y, mo as u32, d as u32);
    Some((days * 86_400 + h * 3600 + mi * 60 + se - offset, millis))
}

/// Seconds since the epoch for an ISO timestamp, `None` when absent or odd.
pub fn parse_time(s: &str) -> Option<i64> {
    parse_timestamp(s).map(|(secs, _)| secs)
}

/// The state of a run, job, step or check, folded from GitHub's `status` +
/// `conclusion` pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Outcome {
    Queued,
    InProgress,
    Success,
    Failure,
    Cancelled,
    Skipped,
    Neutral,
    TimedOut,
    ActionRequired,
    Stale,
    Unknown,
}

impl Outcome {
    pub fn from_parts(status: &str, conclusion: Option<&str>) -> Self {
        match status {
            "queued" | "waiting" | "pending" | "requested" => Outcome::Queued,
            "in_progress" => Outcome::InProgress,
            _ => match conclusion {
                Some("success") => Outcome::Success,
                Some("failure") | Some("startup_failure") => Outcome::Failure,
                Some("cancelled") => Outcome::Cancelled,
                Some("skipped") => Outcome::Skipped,
                Some("neutral") => Outcome::Neutral,
                Some("timed_out") => Outcome::TimedOut,
                Some("action_required") => Outcome::ActionRequired,
                Some("stale") => Outcome::Stale,
                None if status.is_empty() || status == "completed" => Outcome::Unknown,
                None => Outcome::Queued,
                Some(_) => Outcome::Unknown,
            },
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Outcome::Queued => "queued",
            Outcome::InProgress => "in progress",
            Outcome::Success => "success",
            Outcome::Failure => "failure",
            Outcome::Cancelled => "cancelled",
            Outcome::Skipped => "skipped",
            Outcome::Neutral => "neutral",
            Outcome::TimedOut => "timed out",
            Outcome::ActionRequired => "action required",
            Outcome::Stale => "stale",
            Outcome::Unknown => "unknown",
        }
    }

    pub fn is_running(self) -> bool {
        matches!(self, Outcome::Queued | Outcome::InProgress)
    }

    pub fn is_bad(self) -> bool {
        matches!(
            self,
            Outcome::Failure | Outcome::TimedOut | Outcome::ActionRequired | Outcome::Stale
        )
    }

    pub fn is_good(self) -> bool {
        self == Outcome::Success
    }
}

/// The worst outcome wins: failure > running > success > the rest.
pub fn aggregate(outcomes: impl Iterator<Item = Outcome>) -> Outcome {
    let mut agg: Option<Outcome> = None;
    for o in outcomes {
        agg = Some(match agg {
            None => o,
            Some(a) => {
                let rank = |x: Outcome| {
                    if x.is_bad() {
                        4
                    } else if x.is_running() {
                        3
                    } else if x == Outcome::Cancelled {
                        2
                    } else if x.is_good() {
                        1
                    } else {
                        0
                    }
                };
                if rank(o) > rank(a) { o } else { a }
            }
        });
    }
    agg.unwrap_or(Outcome::Unknown)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub head: String,
    pub head_sha: String,
    /// `owner/repo` of the head when it lives in a fork.
    pub head_repo: Option<String>,
    pub base: String,
    pub draft: bool,
    pub url: String,
    pub created: Option<i64>,
    pub updated: Option<i64>,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub reviewers: Vec<String>,
    pub milestone: Option<String>,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Review {
    pub user: String,
    /// `APPROVED`, `CHANGES_REQUESTED`, `COMMENTED`, `DISMISSED`, `PENDING`.
    pub state: String,
    pub submitted: Option<i64>,
}

/// Numbers the pull request list does not include.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PrDetail {
    pub number: u64,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub commits: u64,
    pub comments: u64,
    pub review_comments: u64,
    pub mergeable: Option<bool>,
    pub mergeable_state: String,
    pub reviews: Vec<Review>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowRun {
    pub id: u64,
    /// Workflow name.
    pub name: String,
    /// `queued`, `in_progress`, `completed`.
    pub status: String,
    /// `success`, `failure`, `cancelled`, … once completed.
    pub conclusion: Option<String>,
    pub branch: String,
    pub sha: String,
    pub url: String,
    pub event: String,
    pub created: Option<i64>,
    pub started: Option<i64>,
    pub updated: Option<i64>,
    pub run_number: u64,
    pub attempt: u32,
    /// Commit or pull request title the run was started for.
    pub title: String,
    pub actor: String,
    pub triggering_actor: String,
    /// Workflow file, e.g. `.github/workflows/ci.yml`.
    pub path: String,
    pub workflow_id: u64,
    pub head_message: String,
    pub head_author: String,
}

impl WorkflowRun {
    pub fn outcome(&self) -> Outcome {
        Outcome::from_parts(&self.status, self.conclusion.as_deref())
    }

    pub fn is_success(&self) -> bool {
        self.outcome().is_good()
    }

    pub fn is_failure(&self) -> bool {
        self.outcome().is_bad()
    }

    pub fn is_running(&self) -> bool {
        self.outcome().is_running()
    }

    /// Seconds between the start and the last update (or `now` while running).
    pub fn duration_secs(&self, now: i64) -> Option<i64> {
        let start = self.started.or(self.created)?;
        let end = if self.is_running() {
            now
        } else {
            self.updated.unwrap_or(now)
        };
        Some((end - start).max(0))
    }

    /// Seconds the run waited for a runner.
    pub fn queue_secs(&self) -> Option<i64> {
        Some((self.started? - self.created?).max(0))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub number: u32,
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub started: Option<i64>,
    pub completed: Option<i64>,
}

impl Step {
    pub fn outcome(&self) -> Outcome {
        Outcome::from_parts(&self.status, self.conclusion.as_deref())
    }

    pub fn duration_secs(&self, now: i64) -> Option<i64> {
        let s = self.started?;
        let e = if self.outcome().is_running() {
            now
        } else {
            self.completed?
        };
        Some((e - s).max(0))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub id: u64,
    pub run_id: u64,
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub started: Option<i64>,
    pub completed: Option<i64>,
    pub runner: Option<String>,
    pub runner_group: Option<String>,
    pub labels: Vec<String>,
    pub url: String,
    pub attempt: u32,
    pub steps: Vec<Step>,
}

impl Job {
    pub fn outcome(&self) -> Outcome {
        Outcome::from_parts(&self.status, self.conclusion.as_deref())
    }

    pub fn duration_secs(&self, now: i64) -> Option<i64> {
        let s = self.started?;
        let e = if self.outcome().is_running() {
            now
        } else {
            self.completed?
        };
        Some((e - s).max(0))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Annotation {
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    /// `failure`, `warning`, `notice`.
    pub level: String,
    pub title: String,
    pub message: String,
}

/// One check on a commit: an Actions job or an external CI app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckRun {
    pub id: u64,
    pub name: String,
    /// App slug, e.g. `github-actions`, `socket-security`.
    pub app: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub started: Option<i64>,
    pub completed: Option<i64>,
    pub details_url: String,
    pub html_url: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub annotations: u32,
}

impl CheckRun {
    pub fn outcome(&self) -> Outcome {
        Outcome::from_parts(&self.status, self.conclusion.as_deref())
    }

    pub fn duration_secs(&self, now: i64) -> Option<i64> {
        let s = self.started?;
        let e = if self.outcome().is_running() {
            now
        } else {
            self.completed?
        };
        Some((e - s).max(0))
    }

    /// `(run id, job id)` when this check is a GitHub Actions job.
    pub fn actions_ids(&self) -> Option<(u64, u64)> {
        actions_ids_from_url(&self.details_url).or_else(|| actions_ids_from_url(&self.html_url))
    }
}

fn actions_ids_from_url(url: &str) -> Option<(u64, u64)> {
    let (_, rest) = url.split_once("/actions/runs/")?;
    let (run, rest) = rest.split_once("/job/")?;
    let job: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    Some((run.parse().ok()?, job.parse().ok()?))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workflow {
    pub id: u64,
    pub name: String,
    pub path: String,
    /// `active`, `disabled_manually`, …
    pub state: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimit {
    pub limit: u32,
    pub remaining: u32,
    /// Epoch seconds when the window resets.
    pub reset: i64,
}

/// One request to the GitHub API, for the activity log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiCall {
    pub command: String,
    pub elapsed_ms: u64,
    pub bytes: usize,
    pub rate: Option<RateLimit>,
    pub error: Option<String>,
    /// What the request sent on standard input: the JSON body for logged
    /// requests, only its size for withheld ones (secret values).
    pub input: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct GitHubData {
    pub owner: String,
    pub repo: String,
    pub prs: Vec<PullRequest>,
    pub runs: Vec<WorkflowRun>,
    pub workflows: Vec<Workflow>,
    pub rate: Option<RateLimit>,
}

impl GitHubData {
    pub fn repo_url(&self) -> String {
        format!("https://github.com/{}/{}", self.owner, self.repo)
    }

    pub fn commit_url(&self, sha: &str) -> String {
        format!("{}/commit/{sha}", self.repo_url())
    }

    pub fn branch_url(&self, branch: &str) -> String {
        format!("{}/tree/{branch}", self.repo_url())
    }

    pub fn file_url(&self, sha: &str, path: &str) -> String {
        format!("{}/blob/{sha}/{path}", self.repo_url())
    }

    pub fn run_url(&self, run_id: u64) -> String {
        format!("{}/actions/runs/{run_id}", self.repo_url())
    }

    pub fn job_url(&self, run_id: u64, job_id: u64) -> String {
        format!("{}/actions/runs/{run_id}/job/{job_id}", self.repo_url())
    }

    /// The most relevant run for a commit: the newest one.
    pub fn run_for_sha(&self, sha: &str) -> Option<&WorkflowRun> {
        self.runs.iter().find(|r| r.sha == sha)
    }

    pub fn runs_for_sha<'a>(&'a self, sha: &'a str) -> impl Iterator<Item = &'a WorkflowRun> + 'a {
        self.runs.iter().filter(move |r| r.sha == sha)
    }

    pub fn run(&self, id: u64) -> Option<&WorkflowRun> {
        self.runs.iter().find(|r| r.id == id)
    }

    pub fn pr(&self, number: u64) -> Option<&PullRequest> {
        self.prs.iter().find(|p| p.number == number)
    }

    /// Insert or replace a run, keeping the list newest-first.
    pub fn upsert_run(&mut self, run: WorkflowRun) {
        match self.runs.iter_mut().find(|r| r.id == run.id) {
            Some(slot) => *slot = run,
            None => {
                let pos = self
                    .runs
                    .iter()
                    .position(|r| r.created.unwrap_or(0) < run.created.unwrap_or(0))
                    .unwrap_or(self.runs.len());
                self.runs.insert(pos, run);
            }
        }
    }
}

#[derive(Deserialize)]
struct RawUser {
    login: String,
}

#[derive(Deserialize)]
struct RawRepo {
    full_name: String,
}

#[derive(Deserialize)]
struct RawRef {
    #[serde(rename = "ref")]
    name: String,
    #[serde(default)]
    sha: String,
    repo: Option<RawRepo>,
}

#[derive(Deserialize)]
struct RawLabel {
    name: String,
}

#[derive(Deserialize)]
struct RawMilestone {
    title: String,
}

#[derive(Deserialize)]
struct RawPr {
    number: u64,
    title: String,
    user: Option<RawUser>,
    head: RawRef,
    base: RawRef,
    #[serde(default)]
    draft: bool,
    html_url: String,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    labels: Vec<RawLabel>,
    #[serde(default)]
    assignees: Vec<RawUser>,
    #[serde(default)]
    requested_reviewers: Vec<RawUser>,
    milestone: Option<RawMilestone>,
    body: Option<String>,
    // Only present on the single-PR endpoint.
    additions: Option<u64>,
    deletions: Option<u64>,
    changed_files: Option<u64>,
    commits: Option<u64>,
    comments: Option<u64>,
    review_comments: Option<u64>,
    mergeable: Option<bool>,
    mergeable_state: Option<String>,
}

#[derive(Deserialize)]
struct RawReview {
    user: Option<RawUser>,
    #[serde(default)]
    state: String,
    submitted_at: Option<String>,
}

#[derive(Deserialize)]
struct RawRuns {
    #[serde(default)]
    workflow_runs: Vec<RawRun>,
}

#[derive(Deserialize)]
struct RawCommitAuthor {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct RawHeadCommit {
    #[serde(default)]
    message: String,
    author: Option<RawCommitAuthor>,
}

#[derive(Deserialize)]
struct RawRun {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: String,
    conclusion: Option<String>,
    #[serde(default)]
    head_branch: Option<String>,
    #[serde(default)]
    head_sha: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    event: String,
    #[serde(default)]
    created_at: String,
    run_started_at: Option<String>,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    run_number: u64,
    #[serde(default)]
    run_attempt: u32,
    #[serde(default)]
    display_title: String,
    actor: Option<RawUser>,
    triggering_actor: Option<RawUser>,
    #[serde(default)]
    path: String,
    #[serde(default)]
    workflow_id: u64,
    head_commit: Option<RawHeadCommit>,
}

impl RawRun {
    fn into_run(self) -> WorkflowRun {
        let head = self.head_commit;
        WorkflowRun {
            id: self.id,
            name: self.name,
            status: self.status,
            conclusion: self.conclusion,
            branch: self.head_branch.unwrap_or_default(),
            sha: self.head_sha,
            url: self.html_url,
            event: self.event,
            created: parse_time(&self.created_at),
            started: self.run_started_at.as_deref().and_then(parse_time),
            updated: parse_time(&self.updated_at),
            run_number: self.run_number,
            attempt: self.run_attempt.max(1),
            title: self.display_title,
            actor: self.actor.map(|u| u.login).unwrap_or_default(),
            triggering_actor: self.triggering_actor.map(|u| u.login).unwrap_or_default(),
            path: self.path,
            workflow_id: self.workflow_id,
            head_message: head
                .as_ref()
                .map(|h| h.message.lines().next().unwrap_or("").to_owned())
                .unwrap_or_default(),
            head_author: head
                .and_then(|h| h.author)
                .map(|a| a.name)
                .unwrap_or_default(),
        }
    }
}

#[derive(Deserialize)]
struct RawJobs {
    #[serde(default)]
    total_count: u64,
    #[serde(default)]
    jobs: Vec<RawJob>,
}

#[derive(Deserialize)]
struct RawStep {
    #[serde(default)]
    number: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: String,
    conclusion: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
}

#[derive(Deserialize)]
struct RawJob {
    id: u64,
    #[serde(default)]
    run_id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: String,
    conclusion: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
    runner_name: Option<String>,
    runner_group_name: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    run_attempt: u32,
    #[serde(default)]
    steps: Vec<RawStep>,
}

impl RawJob {
    fn into_job(self) -> Job {
        Job {
            id: self.id,
            run_id: self.run_id,
            name: self.name,
            status: self.status,
            conclusion: self.conclusion,
            started: self.started_at.as_deref().and_then(parse_time),
            completed: self.completed_at.as_deref().and_then(parse_time),
            runner: self.runner_name.filter(|s| !s.is_empty()),
            runner_group: self.runner_group_name.filter(|s| !s.is_empty()),
            labels: self.labels,
            url: self.html_url,
            attempt: self.run_attempt.max(1),
            steps: self
                .steps
                .into_iter()
                .map(|s| Step {
                    number: s.number,
                    name: s.name,
                    status: s.status,
                    conclusion: s.conclusion,
                    started: s.started_at.as_deref().and_then(parse_time),
                    completed: s.completed_at.as_deref().and_then(parse_time),
                })
                .collect(),
        }
    }
}

#[derive(Deserialize)]
struct RawAnnotation {
    #[serde(default)]
    path: String,
    #[serde(default)]
    start_line: u32,
    #[serde(default)]
    end_line: u32,
    #[serde(default)]
    annotation_level: String,
    title: Option<String>,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct RawCheckRuns {
    #[serde(default)]
    check_runs: Vec<RawCheckRun>,
}

#[derive(Deserialize)]
struct RawApp {
    #[serde(default)]
    slug: String,
}

#[derive(Deserialize)]
struct RawCheckOutput {
    title: Option<String>,
    summary: Option<String>,
    #[serde(default)]
    annotations_count: u32,
}

#[derive(Deserialize)]
struct RawCheckRun {
    id: u64,
    #[serde(default)]
    name: String,
    app: Option<RawApp>,
    #[serde(default)]
    status: String,
    conclusion: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
    details_url: Option<String>,
    #[serde(default)]
    html_url: String,
    output: Option<RawCheckOutput>,
}

#[derive(Deserialize)]
struct RawWorkflows {
    #[serde(default)]
    workflows: Vec<RawWorkflow>,
}

#[derive(Deserialize)]
struct RawWorkflow {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    state: String,
}

fn header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

/// Split `gh api -i` output into (headers, body).
fn split_response(raw: &[u8]) -> (String, &[u8]) {
    let sep = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4))
        .into_iter()
        .chain(raw.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2)))
        .min_by_key(|(i, _)| *i);
    match sep {
        Some((i, n)) => (
            String::from_utf8_lossy(&raw[..i]).into_owned(),
            &raw[i + n..],
        ),
        None => (String::new(), raw),
    }
}

fn rate_limit(headers: &str) -> Option<RateLimit> {
    Some(RateLimit {
        limit: header_value(headers, "X-Ratelimit-Limit")?.parse().ok()?,
        remaining: header_value(headers, "X-Ratelimit-Remaining")?
            .parse()
            .ok()?,
        reset: header_value(headers, "X-Ratelimit-Reset")?.parse().ok()?,
    })
}

/// The GitHub CLI executable: `gh`, or the program named by `GITVIS_GH`
/// (a recording test double) when that is set.
pub fn gh_program() -> String {
    std::env::var("GITVIS_GH")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "gh".to_owned())
}

/// A request body sent to `gh api --input -` on standard input.
#[derive(Clone, Copy, Debug)]
pub enum Body<'a> {
    /// Shown in the activity log (pretty-printed, truncated).
    Logged(&'a [u8]),
    /// Never shown anywhere: only its size is recorded.
    Withheld(&'a [u8]),
}

impl<'a> Body<'a> {
    fn bytes(&self) -> &'a [u8] {
        match self {
            Body::Logged(b) | Body::Withheld(b) => b,
        }
    }

    fn preview(&self) -> String {
        match self {
            Body::Withheld(b) => format!(
                "{} sent on standard input (not logged)",
                crate::devops::plural_bytes(b.len())
            ),
            Body::Logged(b) => {
                let pretty = serde_json::from_slice::<serde_json::Value>(b)
                    .ok()
                    .and_then(|v| serde_json::to_string_pretty(&v).ok())
                    .unwrap_or_else(|| String::from_utf8_lossy(b).into_owned());
                const MAX: usize = 4_000;
                if pretty.len() > MAX {
                    let mut cut = MAX;
                    while !pretty.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    format!("{}\n… ({} more bytes)", &pretty[..cut], pretty.len() - cut)
                } else {
                    pretty
                }
            }
        }
    }
}

/// `HTTP 422 Unprocessable Entity` from the status line of `gh api -i`.
fn status_line(headers: &str) -> Option<(u16, String)> {
    let first = headers.lines().next()?.trim();
    let mut parts = first.splitn(3, ' ');
    let proto = parts.next()?;
    if !proto.starts_with("HTTP/") {
        return None;
    }
    let code: u16 = parts.next()?.parse().ok()?;
    Some((code, parts.next().unwrap_or("").trim().to_owned()))
}

/// A readable message for a failed request: GitHub's `message`, every entry
/// of `errors`, the documentation link, and the HTTP status, e.g.
/// `Validation Failed: head is invalid (HTTP 422)`.
fn api_error(headers: &str, body: &[u8], stderr: &str) -> String {
    let status = status_line(headers);
    let mut msg = String::new();
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) {
        if let Some(m) = v.get("message").and_then(|m| m.as_str()) {
            msg.push_str(m);
        }
        if let Some(errs) = v.get("errors").and_then(|e| e.as_array()) {
            let parts: Vec<String> = errs
                .iter()
                .filter_map(|e| {
                    if let Some(s) = e.as_str() {
                        return Some(s.to_owned());
                    }
                    if let Some(m) = e.get("message").and_then(|m| m.as_str()) {
                        return Some(m.to_owned());
                    }
                    let field = e.get("field").and_then(|f| f.as_str());
                    let code = e.get("code").and_then(|c| c.as_str());
                    match (field, code) {
                        (Some(f), Some(c)) => Some(format!("{f} is {}", c.replace('_', " "))),
                        (None, Some(c)) => Some(c.replace('_', " ")),
                        _ => None,
                    }
                })
                .collect();
            if !parts.is_empty() {
                if !msg.is_empty() {
                    msg.push_str(": ");
                }
                msg.push_str(&parts.join("; "));
            }
        }
        if let Some(doc) = v.get("documentation_url").and_then(|d| d.as_str()) {
            msg.push_str(&format!("\nSee {doc}"));
        }
    }
    if msg.is_empty() {
        // No JSON body: gh's own `gh: Not Found (HTTP 404)` line, or the text.
        let lines: Vec<&str> = stderr
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        msg = lines.join("\n");
        if msg.is_empty() {
            msg = String::from_utf8_lossy(body).trim().to_owned();
        }
        if let Some(rest) = msg.strip_prefix("gh: ") {
            msg = rest.to_owned();
        }
    }
    match status {
        Some((code, _)) if !msg.contains(&format!("HTTP {code}")) => {
            if let Some(nl) = msg.find('\n') {
                msg.insert_str(nl, &format!(" (HTTP {code})"));
            } else {
                msg.push_str(&format!(" (HTTP {code})"));
            }
        }
        _ => {}
    }
    if msg.is_empty() {
        "The GitHub CLI failed without a message".to_owned()
    } else {
        msg
    }
}

/// Run `gh api ENDPOINT` and record the call. Response headers are included
/// (`-i`) so the rate limit can be read.
fn api(dir: &Path, endpoint: &str, extra: &[&str], calls: &mut Vec<ApiCall>) -> Result<Vec<u8>> {
    api_request(dir, endpoint, extra, None, calls)
}

/// `gh api -i [extra…] [--input -] ENDPOINT`, recording an [`ApiCall`].
/// `extra` holds flags such as `--method POST`; a `body` goes to standard
/// input, so nothing in it ever appears on a command line.
pub fn api_request(
    dir: &Path,
    endpoint: &str,
    extra: &[&str],
    body: Option<Body>,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<u8>> {
    let mut args = vec!["api", "-i"];
    args.extend_from_slice(extra);
    if body.is_some() {
        args.extend(["--input", "-"]);
    }
    args.push(endpoint);
    let command = format!("gh {}", args.join(" "));
    let input = body.map(|b| b.preview());
    let program = gh_program();
    let started = Instant::now();
    let out = cli::run_raw(&program, dir, &args, body.map(|b| b.bytes()), &[]);
    let elapsed_ms = started.elapsed().as_millis() as u64;
    match out {
        Ok(out) => {
            let (headers, resp) = split_response(&out.stdout);
            let rate = rate_limit(&headers);
            if out.success {
                calls.push(ApiCall {
                    command,
                    elapsed_ms,
                    bytes: resp.len(),
                    rate,
                    error: None,
                    input,
                });
                Ok(resp.to_vec())
            } else {
                let message = api_error(&headers, resp, &out.stderr);
                calls.push(ApiCall {
                    command,
                    elapsed_ms,
                    bytes: resp.len(),
                    rate,
                    error: Some(message.clone()),
                    input,
                });
                Err(Error::new(message))
            }
        }
        Err(e) => {
            let m = e.to_string();
            let message = if m.contains("Could not start") {
                "GitHub CLI (gh) is not installed. Install it and run `gh auth login`.".to_owned()
            } else {
                m
            };
            calls.push(ApiCall {
                command,
                elapsed_ms,
                bytes: 0,
                rate: None,
                error: Some(message.clone()),
                input,
            });
            Err(Error::new(message))
        }
    }
}

fn json<T: for<'de> Deserialize<'de>>(body: &[u8], what: &str) -> Result<T> {
    serde_json::from_slice(body).map_err(|e| Error::new(format!("GitHub {what}: {e}")))
}

fn pr_from_raw(p: RawPr) -> PullRequest {
    PullRequest {
        number: p.number,
        title: p.title,
        author: p.user.map(|u| u.login).unwrap_or_default(),
        head: p.head.name,
        head_sha: p.head.sha,
        head_repo: p.head.repo.map(|r| r.full_name),
        base: p.base.name,
        draft: p.draft,
        url: p.html_url,
        created: parse_time(&p.created_at),
        updated: parse_time(&p.updated_at),
        labels: p.labels.into_iter().map(|l| l.name).collect(),
        assignees: p.assignees.into_iter().map(|u| u.login).collect(),
        reviewers: p.requested_reviewers.into_iter().map(|u| u.login).collect(),
        milestone: p.milestone.map(|m| m.title),
        body: p.body.unwrap_or_default(),
    }
}

/// Open pull requests, recent workflow runs and the workflow list.
pub fn fetch(dir: &Path, owner: &str, repo: &str, calls: &mut Vec<ApiCall>) -> Result<GitHubData> {
    let prs: Vec<RawPr> = json(
        &api(
            dir,
            &format!(
                "repos/{owner}/{repo}/pulls?state=open&per_page=50&sort=updated&direction=desc"
            ),
            &[],
            calls,
        )?,
        "pull requests",
    )?;
    let runs: RawRuns = json(
        &api(
            dir,
            &format!("repos/{owner}/{repo}/actions/runs?per_page=100"),
            &[],
            calls,
        )?,
        "workflow runs",
    )?;
    let workflows: RawWorkflows = json(
        &api(
            dir,
            &format!("repos/{owner}/{repo}/actions/workflows?per_page=100"),
            &[],
            calls,
        )?,
        "workflows",
    )?;
    Ok(GitHubData {
        owner: owner.to_owned(),
        repo: repo.to_owned(),
        prs: prs.into_iter().map(pr_from_raw).collect(),
        runs: runs
            .workflow_runs
            .into_iter()
            .map(RawRun::into_run)
            .collect(),
        workflows: workflows
            .workflows
            .into_iter()
            .map(|w| Workflow {
                id: w.id,
                name: w.name,
                path: w.path,
                state: w.state,
            })
            .collect(),
        rate: calls.iter().rev().find_map(|c| c.rate),
    })
}

/// One workflow run by id (for runs older than the list).
pub fn fetch_run(
    dir: &Path,
    owner: &str,
    repo: &str,
    run_id: u64,
    calls: &mut Vec<ApiCall>,
) -> Result<WorkflowRun> {
    let raw: RawRun = json(
        &api(
            dir,
            &format!("repos/{owner}/{repo}/actions/runs/{run_id}"),
            &[],
            calls,
        )?,
        "workflow run",
    )?;
    Ok(raw.into_run())
}

/// All jobs of a run's latest attempt, with their steps.
pub fn fetch_jobs(
    dir: &Path,
    owner: &str,
    repo: &str,
    run_id: u64,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<Job>> {
    let mut jobs = Vec::new();
    for page in 1..=10 {
        let raw: RawJobs = json(
            &api(
                dir,
                &format!(
                    "repos/{owner}/{repo}/actions/runs/{run_id}/jobs?per_page=100&page={page}"
                ),
                &[],
                calls,
            )?,
            "jobs",
        )?;
        let total = raw.total_count as usize;
        jobs.extend(raw.jobs.into_iter().map(RawJob::into_job));
        if jobs.len() >= total || total == 0 {
            break;
        }
    }
    Ok(jobs)
}

/// The raw log of one job (timestamps, `##[group]` markers and ANSI colours
/// included; see [`crate::joblog`]).
pub fn fetch_job_log(
    dir: &Path,
    owner: &str,
    repo: &str,
    job_id: u64,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<u8>> {
    api(
        dir,
        &format!("repos/{owner}/{repo}/actions/jobs/{job_id}/logs"),
        &["--allow-escape-sequences"],
        calls,
    )
}

/// Annotations of a check run (an Actions job's id is its check run id).
pub fn fetch_annotations(
    dir: &Path,
    owner: &str,
    repo: &str,
    check_run_id: u64,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<Annotation>> {
    let raw: Vec<RawAnnotation> = json(
        &api(
            dir,
            &format!("repos/{owner}/{repo}/check-runs/{check_run_id}/annotations?per_page=100"),
            &[],
            calls,
        )?,
        "annotations",
    )?;
    Ok(raw
        .into_iter()
        .map(|a| Annotation {
            path: a.path,
            start_line: a.start_line,
            end_line: a.end_line.max(a.start_line),
            level: a.annotation_level,
            title: a.title.unwrap_or_default(),
            message: a.message,
        })
        .collect())
}

/// Every check run on a commit, from Actions and from other CI apps.
pub fn fetch_check_runs(
    dir: &Path,
    owner: &str,
    repo: &str,
    sha: &str,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<CheckRun>> {
    let raw: RawCheckRuns = json(
        &api(
            dir,
            &format!("repos/{owner}/{repo}/commits/{sha}/check-runs?per_page=100"),
            &[],
            calls,
        )?,
        "check runs",
    )?;
    Ok(raw
        .check_runs
        .into_iter()
        .map(|c| CheckRun {
            id: c.id,
            name: c.name,
            app: c.app.map(|a| a.slug).unwrap_or_default(),
            status: c.status,
            conclusion: c.conclusion,
            started: c.started_at.as_deref().and_then(parse_time),
            completed: c.completed_at.as_deref().and_then(parse_time),
            details_url: c.details_url.unwrap_or_default(),
            html_url: c.html_url,
            title: c.output.as_ref().and_then(|o| o.title.clone()),
            summary: c.output.as_ref().and_then(|o| o.summary.clone()),
            annotations: c.output.map(|o| o.annotations_count).unwrap_or(0),
        })
        .collect())
}

/// Size, merge state and reviews of one pull request.
pub fn fetch_pr(
    dir: &Path,
    owner: &str,
    repo: &str,
    number: u64,
    calls: &mut Vec<ApiCall>,
) -> Result<PrDetail> {
    let raw: RawPr = json(
        &api(
            dir,
            &format!("repos/{owner}/{repo}/pulls/{number}"),
            &[],
            calls,
        )?,
        "pull request",
    )?;
    let reviews: Vec<RawReview> = json(
        &api(
            dir,
            &format!("repos/{owner}/{repo}/pulls/{number}/reviews?per_page=100"),
            &[],
            calls,
        )?,
        "reviews",
    )?;
    Ok(PrDetail {
        number,
        additions: raw.additions.unwrap_or(0),
        deletions: raw.deletions.unwrap_or(0),
        changed_files: raw.changed_files.unwrap_or(0),
        commits: raw.commits.unwrap_or(0),
        comments: raw.comments.unwrap_or(0),
        review_comments: raw.review_comments.unwrap_or(0),
        mergeable: raw.mergeable,
        mergeable_state: raw.mergeable_state.unwrap_or_default(),
        reviews: reviews
            .into_iter()
            .map(|r| Review {
                user: r.user.map(|u| u.login).unwrap_or_default(),
                state: r.state,
                submitted: r.submitted_at.as_deref().and_then(parse_time),
            })
            .collect(),
    })
}

// ---------------------------------------------------------------------------
// Repository exposure: visibility, secret scanning, alerts
// ---------------------------------------------------------------------------

/// What GitHub says about who can see the repository and whether it scans
/// pushes for secrets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoInfo {
    pub private: bool,
    /// `public`, `private` or `internal`.
    pub visibility: String,
    pub forks: u64,
    pub stars: u64,
    pub watchers: u64,
    pub archived: bool,
    pub fork: bool,
    pub default_branch: String,
    pub url: String,
    pub pushed_at: Option<i64>,
    /// `enabled` / `disabled`; `None` when the token cannot see security settings.
    pub secret_scanning: Option<String>,
    pub push_protection: Option<String>,
    pub dependabot_alerts: Option<String>,
    /// `Organization` or `User`.
    pub owner_type: String,
}

impl RepoInfo {
    pub fn is_public(&self) -> bool {
        !self.private && self.visibility != "internal"
    }

    pub fn owner_is_org(&self) -> bool {
        self.owner_type.eq_ignore_ascii_case("organization")
    }
}

/// One GitHub secret-scanning alert.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretAlert {
    pub number: u64,
    pub state: String,
    pub secret_type: String,
    pub display_name: String,
    pub created: Option<i64>,
    pub updated: Option<i64>,
    pub url: String,
    pub resolution: Option<String>,
    /// `active`, `inactive` or `unknown`.
    pub validity: String,
    pub push_protection_bypassed: bool,
    pub publicly_leaked: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSecurityStatus {
    status: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSecurity {
    secret_scanning: Option<RawSecurityStatus>,
    secret_scanning_push_protection: Option<RawSecurityStatus>,
    dependabot_security_updates: Option<RawSecurityStatus>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawOwner {
    login: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawRepoInfo {
    private: bool,
    visibility: String,
    forks_count: u64,
    stargazers_count: u64,
    subscribers_count: u64,
    archived: bool,
    fork: bool,
    default_branch: String,
    html_url: String,
    pushed_at: String,
    security_and_analysis: Option<RawSecurity>,
    owner: Option<RawOwner>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawAlert {
    number: u64,
    state: String,
    secret_type: String,
    secret_type_display_name: String,
    created_at: String,
    updated_at: Option<String>,
    html_url: String,
    resolution: Option<String>,
    validity: String,
    push_protection_bypassed: Option<bool>,
    publicly_leaked: Option<bool>,
}

pub fn fetch_repo_info(
    dir: &Path,
    owner: &str,
    repo: &str,
    calls: &mut Vec<ApiCall>,
) -> Result<RepoInfo> {
    let raw: RawRepoInfo = json(
        &api(dir, &format!("repos/{owner}/{repo}"), &[], calls)?,
        "repository",
    )?;
    let sec = raw.security_and_analysis.unwrap_or_default();
    let status = |s: Option<RawSecurityStatus>| s.map(|s| s.status);
    Ok(RepoInfo {
        private: raw.private,
        visibility: if raw.visibility.is_empty() {
            if raw.private { "private" } else { "public" }.to_owned()
        } else {
            raw.visibility
        },
        forks: raw.forks_count,
        stars: raw.stargazers_count,
        watchers: raw.subscribers_count,
        archived: raw.archived,
        fork: raw.fork,
        default_branch: raw.default_branch,
        url: raw.html_url,
        pushed_at: parse_time(&raw.pushed_at),
        secret_scanning: status(sec.secret_scanning),
        push_protection: status(sec.secret_scanning_push_protection),
        dependabot_alerts: status(sec.dependabot_security_updates),
        owner_type: raw.owner.map(|o| o.kind).unwrap_or_default(),
    })
}

// ---------------------------------------------------------------------------
// Forks, alerts, admin surface, code search (OSINT)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fork {
    pub full_name: String,
    pub url: String,
    pub stars: u64,
    pub pushed: Option<i64>,
    pub created: Option<i64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawFork {
    full_name: String,
    html_url: String,
    stargazers_count: u64,
    pushed_at: Option<String>,
    created_at: Option<String>,
}

/// The newest 100 forks (the repository info has the total).
pub fn fetch_forks(
    dir: &Path,
    owner: &str,
    repo: &str,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<Fork>> {
    let raw: Vec<RawFork> = json(
        &api(
            dir,
            &format!("repos/{owner}/{repo}/forks?per_page=100&sort=newest"),
            &[],
            calls,
        )?,
        "forks",
    )?;
    Ok(raw
        .into_iter()
        .map(|f| Fork {
            full_name: f.full_name,
            url: f.html_url,
            stars: f.stargazers_count,
            pushed: f.pushed_at.as_deref().and_then(parse_time),
            created: f.created_at.as_deref().and_then(parse_time),
        })
        .collect())
}

/// Does GitHub still serve this commit (in this repository's object
/// network)? `Ok(false)` on 404 or 422, which is what a purged commit
/// eventually returns.
pub fn commit_served(
    dir: &Path,
    owner: &str,
    repo: &str,
    sha: &str,
    calls: &mut Vec<ApiCall>,
) -> Result<bool> {
    match api(
        dir,
        &format!("repos/{owner}/{repo}/commits/{sha}"),
        &[],
        calls,
    ) {
        Ok(_) => Ok(true),
        Err(e) => {
            let m = e.to_string();
            if m.contains("404") || m.contains("422") || m.contains("No commit found") {
                Ok(false)
            } else {
                Err(e)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DependabotAlert {
    pub number: u64,
    pub state: String,
    pub severity: String,
    pub package: String,
    pub ecosystem: String,
    pub summary: String,
    pub advisory: String,
    pub url: String,
    pub created: Option<i64>,
    pub manifest: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawDepPackage {
    ecosystem: String,
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawDependency {
    package: Option<RawDepPackage>,
    manifest_path: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawAdvisory {
    ghsa_id: String,
    cve_id: Option<String>,
    summary: String,
    severity: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawDependabotAlert {
    number: u64,
    state: String,
    dependency: Option<RawDependency>,
    security_advisory: Option<RawAdvisory>,
    html_url: String,
    created_at: String,
}

pub fn fetch_dependabot_alerts(
    dir: &Path,
    owner: &str,
    repo: &str,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<DependabotAlert>> {
    let raw: Vec<RawDependabotAlert> = json(
        &api(
            dir,
            &format!("repos/{owner}/{repo}/dependabot/alerts?state=open&per_page=100"),
            &[],
            calls,
        )?,
        "dependabot alerts",
    )?;
    Ok(raw
        .into_iter()
        .map(|a| {
            let dep = a.dependency.unwrap_or_default();
            let pkg = dep.package.unwrap_or_default();
            let adv = a.security_advisory.unwrap_or_default();
            DependabotAlert {
                number: a.number,
                state: a.state,
                severity: adv.severity,
                package: pkg.name,
                ecosystem: pkg.ecosystem,
                summary: adv.summary,
                advisory: adv.cve_id.unwrap_or(adv.ghsa_id),
                url: a.html_url,
                created: parse_time(&a.created_at),
                manifest: dep.manifest_path,
            }
        })
        .collect())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeAlert {
    pub number: u64,
    pub severity: String,
    pub rule: String,
    pub description: String,
    pub tool: String,
    pub path: String,
    pub line: u64,
    pub url: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawCodeRule {
    id: String,
    severity: String,
    security_severity_level: Option<String>,
    description: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawCodeTool {
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawCodeLocation {
    path: String,
    start_line: u64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawCodeInstance {
    location: Option<RawCodeLocation>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawCodeAlert {
    number: u64,
    rule: Option<RawCodeRule>,
    tool: Option<RawCodeTool>,
    most_recent_instance: Option<RawCodeInstance>,
    html_url: String,
}

pub fn fetch_code_alerts(
    dir: &Path,
    owner: &str,
    repo: &str,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<CodeAlert>> {
    let raw: Vec<RawCodeAlert> = json(
        &api(
            dir,
            &format!("repos/{owner}/{repo}/code-scanning/alerts?state=open&per_page=100"),
            &[],
            calls,
        )?,
        "code scanning alerts",
    )?;
    Ok(raw
        .into_iter()
        .map(|a| {
            let rule = a.rule.unwrap_or_default();
            let loc = a
                .most_recent_instance
                .and_then(|i| i.location)
                .unwrap_or_default();
            CodeAlert {
                number: a.number,
                severity: rule.security_severity_level.unwrap_or(rule.severity),
                rule: rule.id,
                description: rule.description,
                tool: a.tool.map(|t| t.name).unwrap_or_default(),
                path: loc.path,
                line: loc.start_line,
                url: a.html_url,
            }
        })
        .collect())
}

/// What an administrator can see about the repository's attack surface.
/// Each part fails on its own (most need admin rights).
/// One part of the admin surface: not fetched yet, fetched, or refused.
pub type Part<T> = Option<std::result::Result<T, String>>;

#[derive(Clone, Debug, Default)]
pub struct AdminSurface {
    pub deploy_keys: Part<Vec<String>>,
    pub webhooks: Part<Vec<String>>,
    pub environments: Part<Vec<String>>,
    pub secrets: Part<Vec<String>>,
    pub variables: Part<Vec<String>>,
    pub protection: Part<String>,
    pub collaborators: Part<usize>,
}

pub fn fetch_admin_surface(
    dir: &Path,
    owner: &str,
    repo: &str,
    default_branch: &str,
    calls: &mut Vec<ApiCall>,
) -> AdminSurface {
    let list = |endpoint: &str,
                pick: &dyn Fn(&serde_json::Value) -> String,
                what: &str,
                calls: &mut Vec<ApiCall>|
     -> Result<Vec<String>> {
        let body = api(dir, endpoint, &[], calls)?;
        let v: serde_json::Value = json(&body, what)?;
        let arr = v
            .as_array()
            .cloned()
            .or_else(|| v.get(what).and_then(|x| x.as_array()).cloned())
            .unwrap_or_default();
        Ok(arr.iter().map(pick).collect())
    };
    let s =
        |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_owned();
    let deploy_keys = list(
        &format!("repos/{owner}/{repo}/keys?per_page=100"),
        &|k| {
            format!(
                "{} · {} · added {}{}",
                s(k, "title"),
                if k.get("read_only").and_then(|b| b.as_bool()).unwrap_or(true) {
                    "read-only"
                } else {
                    "read-write"
                },
                s(k, "created_at"),
                if k.get("verified").and_then(|b| b.as_bool()).unwrap_or(false) {
                    ""
                } else {
                    " · unverified"
                }
            )
        },
        "keys",
        calls,
    )
    .map_err(|e| e.to_string());
    let webhooks = list(
        &format!("repos/{owner}/{repo}/hooks?per_page=100"),
        &|h| {
            let url = h
                .get("config")
                .and_then(|c| c.get("url"))
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .to_owned();
            let events: Vec<String> = h
                .get("events")
                .and_then(|e| e.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            format!(
                "{} · {} · {}",
                url,
                events.join(","),
                if h.get("active").and_then(|b| b.as_bool()).unwrap_or(false) {
                    "active"
                } else {
                    "inactive"
                }
            )
        },
        "hooks",
        calls,
    )
    .map_err(|e| e.to_string());
    let environments = list(
        &format!("repos/{owner}/{repo}/environments?per_page=100"),
        &|e| s(e, "name"),
        "environments",
        calls,
    )
    .map_err(|e| e.to_string());
    let secrets = list(
        &format!("repos/{owner}/{repo}/actions/secrets?per_page=100"),
        &|e| format!("{} · updated {}", s(e, "name"), s(e, "updated_at")),
        "secrets",
        calls,
    )
    .map_err(|e| e.to_string());
    let variables = list(
        &format!("repos/{owner}/{repo}/actions/variables?per_page=100"),
        &|e| format!("{} = {}", s(e, "name"), s(e, "value")),
        "variables",
        calls,
    )
    .map_err(|e| e.to_string());
    let protection = api(
        dir,
        &format!("repos/{owner}/{repo}/branches/{default_branch}/protection"),
        &[],
        calls,
    )
    .and_then(|b| json::<serde_json::Value>(&b, "branch protection"))
    .map(|v| {
        let reviews = v
            .get("required_pull_request_reviews")
            .and_then(|r| r.get("required_approving_review_count"))
            .and_then(|n| n.as_u64());
        let checks = v
            .get("required_status_checks")
            .and_then(|c| c.get("contexts"))
            .and_then(|c| c.as_array())
            .map(|a| a.len());
        let force = v
            .get("allow_force_pushes")
            .and_then(|f| f.get("enabled"))
            .and_then(|b| b.as_bool());
        let signed = v
            .get("required_signatures")
            .and_then(|f| f.get("enabled"))
            .and_then(|b| b.as_bool());
        format!(
            "protected · {} · {} · force pushes {} · signed commits {}",
            match reviews {
                Some(n) => format!("{n} approving review(s) required"),
                None => "no review requirement".to_owned(),
            },
            match checks {
                Some(n) => format!("{n} required status check(s)"),
                None => "no required checks".to_owned(),
            },
            match force {
                Some(true) => "allowed",
                _ => "blocked",
            },
            match signed {
                Some(true) => "required",
                _ => "not required",
            }
        )
    })
    .map_err(|e| {
        let m = e.to_string();
        if m.contains("404") || m.contains("Branch not protected") {
            "not protected".to_owned()
        } else {
            m
        }
    });
    let collaborators = list(
        &format!("repos/{owner}/{repo}/collaborators?per_page=100"),
        &|c| s(c, "login"),
        "collaborators",
        calls,
    )
    .map(|v| v.len())
    .map_err(|e| e.to_string());
    AdminSurface {
        deploy_keys: Some(deploy_keys),
        webhooks: Some(webhooks),
        environments: Some(environments),
        secrets: Some(secrets),
        variables: Some(variables),
        protection: Some(protection),
        collaborators: Some(collaborators),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeSearchHit {
    pub repo: String,
    pub path: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DorkResult {
    pub label: String,
    pub query: String,
    pub total: u64,
    pub hits: Vec<CodeSearchHit>,
    pub error: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSearchRepo {
    full_name: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSearchItem {
    path: String,
    html_url: String,
    repository: Option<RawSearchRepo>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSearch {
    total_count: u64,
    items: Vec<RawSearchItem>,
}

/// GitHub code search (needs an authenticated `gh`; 10 requests a minute).
pub fn search_code(
    dir: &Path,
    query: &str,
    calls: &mut Vec<ApiCall>,
) -> Result<(u64, Vec<CodeSearchHit>)> {
    let encoded: String = query
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' | b'/' => {
                (b as char).to_string()
            }
            b' ' => "+".to_owned(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    let raw: RawSearch = json(
        &api(
            dir,
            &format!("search/code?q={encoded}&per_page=30"),
            &[],
            calls,
        )?,
        "code search",
    )?;
    Ok((
        raw.total_count,
        raw.items
            .into_iter()
            .map(|i| CodeSearchHit {
                repo: i.repository.map(|r| r.full_name).unwrap_or_default(),
                path: i.path,
                url: i.html_url,
            })
            .collect(),
    ))
}

/// The code-search queries an attacker runs against an owner first.
pub fn dorks_for(owner: &str, is_org: bool) -> Vec<(String, String)> {
    let scope = if is_org {
        format!("org:{owner}")
    } else {
        format!("user:{owner}")
    };
    [
        ("Environment files", "filename:.env"),
        ("SSH private keys", "filename:id_rsa OR filename:id_ed25519 OR filename:id_ecdsa"),
        ("Private key blocks", "\"BEGIN PRIVATE KEY\" OR \"BEGIN RSA PRIVATE KEY\" OR \"BEGIN OPENSSH PRIVATE KEY\""),
        ("npm registry tokens", "filename:.npmrc _authToken"),
        ("Cloud credential files", "filename:credentials.json OR filename:service-account.json OR path:.aws filename:credentials"),
        ("AWS access keys", "AKIA extension:yml OR AKIA extension:json OR AKIA extension:env OR AKIA extension:py"),
        ("Passwords in compose files", "filename:docker-compose.yml password"),
        ("Terraform state", "filename:terraform.tfstate"),
        ("Slack tokens", "xoxb- OR xoxp-"),
        ("Git credential stores", "filename:.git-credentials OR filename:.netrc password"),
        ("Hard-coded database URLs", "\"postgres://\" password OR \"mysql://\" password OR \"mongodb://\" password"),
        ("Kubernetes configs", "filename:kubeconfig OR filename:config path:.kube"),
    ]
    .iter()
    .map(|(label, q)| ((*label).to_owned(), format!("{scope} {q}")))
    .collect()
}

/// Open secret-scanning alerts. Needs admin or security-manager access;
/// otherwise GitHub answers 403/404 and the error says so.
pub fn fetch_secret_alerts(
    dir: &Path,
    owner: &str,
    repo: &str,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<SecretAlert>> {
    let raw: Vec<RawAlert> = json(
        &api(
            dir,
            &format!("repos/{owner}/{repo}/secret-scanning/alerts?state=open&per_page=100"),
            &[],
            calls,
        )?,
        "secret scanning alerts",
    )?;
    Ok(raw
        .into_iter()
        .map(|a| SecretAlert {
            number: a.number,
            state: a.state,
            secret_type: a.secret_type,
            display_name: a.secret_type_display_name,
            created: parse_time(&a.created_at),
            updated: a.updated_at.as_deref().and_then(parse_time),
            url: a.html_url,
            resolution: a.resolution,
            validity: if a.validity.is_empty() {
                "unknown".to_owned()
            } else {
                a.validity
            },
            push_protection_bypassed: a.push_protection_bypassed.unwrap_or(false),
            publicly_leaked: a.publicly_leaked.unwrap_or(false),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_info_and_alerts_parse() {
        let body =
            br#"{"private":false,"visibility":"public","forks_count":12,"stargazers_count":3,
            "subscribers_count":2,"archived":false,"fork":false,"default_branch":"main",
            "html_url":"https://github.com/o/r","pushed_at":"2026-10-20T10:00:00Z",
            "security_and_analysis":{"secret_scanning":{"status":"enabled"},
            "secret_scanning_push_protection":{"status":"disabled"}}}"#;
        let raw: RawRepoInfo = json(body, "repository").unwrap();
        assert!(!raw.private);
        assert_eq!(raw.forks_count, 12);
        assert_eq!(
            raw.security_and_analysis
                .as_ref()
                .and_then(|s| s.secret_scanning.as_ref())
                .map(|s| s.status.as_str()),
            Some("enabled")
        );
        let alerts: Vec<RawAlert> = json(
            br#"[{"number":4,"state":"open","secret_type":"github_personal_access_token",
                 "secret_type_display_name":"GitHub Personal Access Token",
                 "created_at":"2026-10-01T00:00:00Z","html_url":"https://github.com/o/r/security/secret-scanning/4",
                 "validity":"active","push_protection_bypassed":true}]"#,
            "alerts",
        )
        .unwrap();
        assert_eq!(alerts[0].number, 4);
        assert_eq!(alerts[0].validity, "active");
        assert_eq!(alerts[0].push_protection_bypassed, Some(true));
    }

    #[test]
    fn remote_forms() {
        let want = Some(("brave".to_owned(), "brave-core".to_owned()));
        assert_eq!(
            parse_remote("https://github.com/brave/brave-core.git"),
            want
        );
        assert_eq!(parse_remote("https://github.com/brave/brave-core"), want);
        assert_eq!(parse_remote("git@github.com:brave/brave-core.git"), want);
        assert_eq!(parse_remote("ssh://git@github.com/brave/brave-core/"), want);
        assert_eq!(parse_remote("https://gitlab.com/a/b"), None);
        assert_eq!(parse_remote("https://github.com/a"), None);
    }

    #[test]
    fn timestamps() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2000-01-01T00:00:00Z"), Some(946_684_800));
        assert_eq!(parse_time("2024-02-29T00:00:00Z"), Some(1_709_164_800));
        assert_eq!(
            parse_timestamp("2026-10-26T04:50:27.3645819Z"),
            Some((1_792_990_227, 364))
        );
        assert_eq!(
            parse_time("2022-06-09T19:23:00.000+02:00"),
            Some(1_654_795_380)
        );
        assert_eq!(parse_time(""), None);
        assert_eq!(parse_time("yesterday"), None);
    }

    #[test]
    fn outcomes() {
        assert_eq!(
            Outcome::from_parts("completed", Some("success")),
            Outcome::Success
        );
        assert_eq!(
            Outcome::from_parts("in_progress", None),
            Outcome::InProgress
        );
        assert_eq!(Outcome::from_parts("queued", None), Outcome::Queued);
        assert!(Outcome::from_parts("completed", Some("timed_out")).is_bad());
        assert_eq!(
            aggregate([Outcome::Success, Outcome::InProgress, Outcome::Skipped].into_iter()),
            Outcome::InProgress
        );
        assert_eq!(
            aggregate([Outcome::Success, Outcome::Failure].into_iter()),
            Outcome::Failure
        );
        assert_eq!(aggregate(std::iter::empty()), Outcome::Unknown);
    }

    #[test]
    fn parses_api_shapes() {
        let prs: Vec<RawPr> = serde_json::from_str(
            r#"[{"number":7,"title":"T","user":{"login":"u"},"head":{"ref":"feat","sha":"abc","repo":{"full_name":"x/y"}},"base":{"ref":"main"},"draft":true,"html_url":"https://x","updated_at":"2026-01-01T00:00:00Z","labels":[{"name":"bug"}]}]"#,
        )
        .unwrap();
        let pr = pr_from_raw(prs.into_iter().next().unwrap());
        assert_eq!(pr.head, "feat");
        assert_eq!(pr.head_repo.as_deref(), Some("x/y"));
        assert_eq!(pr.labels, vec!["bug".to_owned()]);
        let runs: RawRuns = serde_json::from_str(
            r#"{"workflow_runs":[{"id":1,"name":"CI","status":"completed","conclusion":"success","head_branch":"main","head_sha":"abc","html_url":"h","event":"push","created_at":"2026-10-26T04:50:00Z","run_started_at":"2026-10-26T04:50:30Z","updated_at":"2026-10-26T04:55:00Z","run_number":3,"run_attempt":2,"display_title":"Fix","actor":{"login":"a"},"path":".github/workflows/ci.yml","head_commit":{"message":"Fix\n\nbody","author":{"name":"A"}}}]}"#,
        )
        .unwrap();
        let run = runs.workflow_runs.into_iter().next().unwrap().into_run();
        assert_eq!(run.run_number, 3);
        assert_eq!(run.attempt, 2);
        assert_eq!(run.head_message, "Fix");
        assert_eq!(run.queue_secs(), Some(30));
        assert_eq!(run.duration_secs(0), Some(270));
        let jobs: RawJobs = serde_json::from_str(
            r#"{"total_count":1,"jobs":[{"id":9,"run_id":1,"name":"build","status":"completed","conclusion":"failure","started_at":"2026-10-26T04:50:30Z","completed_at":"2026-10-26T04:52:30Z","runner_name":"r","labels":["ubuntu-latest"],"html_url":"u","steps":[{"number":1,"name":"Set up job","status":"completed","conclusion":"success","started_at":"2026-10-26T04:50:30Z","completed_at":"2026-10-26T04:50:35Z"}]}]}"#,
        )
        .unwrap();
        let job = jobs.jobs.into_iter().next().unwrap().into_job();
        assert_eq!(job.duration_secs(0), Some(120));
        assert_eq!(job.steps[0].duration_secs(0), Some(5));
        assert!(job.outcome().is_bad());
    }

    #[test]
    fn check_run_ids() {
        let c = CheckRun {
            id: 1,
            name: String::new(),
            app: "github-actions".into(),
            status: "completed".into(),
            conclusion: None,
            started: None,
            completed: None,
            details_url: "https://github.com/o/r/actions/runs/40201546376/job/121193596815".into(),
            html_url: String::new(),
            title: None,
            summary: None,
            annotations: 0,
        };
        assert_eq!(c.actions_ids(), Some((40_201_546_376, 121_193_596_815)));
        assert_eq!(actions_ids_from_url("https://socket.dev/x"), None);
    }

    #[test]
    fn response_headers() {
        let raw = b"HTTP/2.0 200 OK\r\nX-Ratelimit-Limit: 5000\r\nX-Ratelimit-Remaining: 4992\r\nX-Ratelimit-Reset: 1792995172\r\n\r\n{\"a\":1}";
        let (headers, body) = split_response(raw);
        assert_eq!(body, b"{\"a\":1}");
        let rate = rate_limit(&headers).unwrap();
        assert_eq!(rate.remaining, 4992);
        assert_eq!(rate.reset, 1_792_995_172);
        let (h, b) = split_response(b"no headers here");
        assert!(h.is_empty());
        assert_eq!(b, b"no headers here");
    }

    #[test]
    fn upsert_keeps_newest_first() {
        let mk = |id, created| WorkflowRun {
            id,
            created: Some(created),
            ..mk_run()
        };
        let mut d = GitHubData {
            runs: vec![mk(3, 300), mk(1, 100)],
            ..Default::default()
        };
        d.upsert_run(mk(2, 200));
        assert_eq!(d.runs.iter().map(|r| r.id).collect::<Vec<_>>(), [3, 2, 1]);
        d.upsert_run(mk(2, 250));
        assert_eq!(d.runs.len(), 3);
        assert_eq!(d.run(2).unwrap().created, Some(250));
    }

    fn mk_run() -> WorkflowRun {
        WorkflowRun {
            id: 0,
            name: String::new(),
            status: String::new(),
            conclusion: None,
            branch: String::new(),
            sha: String::new(),
            url: String::new(),
            event: String::new(),
            created: None,
            started: None,
            updated: None,
            run_number: 0,
            attempt: 1,
            title: String::new(),
            actor: String::new(),
            triggering_actor: String::new(),
            path: String::new(),
            workflow_id: 0,
            head_message: String::new(),
            head_author: String::new(),
        }
    }
}
