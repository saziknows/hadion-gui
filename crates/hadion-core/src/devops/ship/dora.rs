//! DORA metrics: deployment frequency, lead time for changes, change
//! failure rate and time to restore, from a list of deployment events
//! (GitHub deployments, releases, tags or workflow runs) plus local
//! history for the commits each deployment brought.
//!
//! Collecting is split from computing: [`collect_local`] runs git,
//! [`compute`] is pure.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

use super::deploy::Deployment;
use super::local::{self, US};
use super::release::Release;
use crate::devops::gh::{Gh, encode};
use crate::github::{ApiCall, parse_time};
use crate::{Error, Result};

const DAY: i64 = 86_400;
const WEEK: i64 = 7 * DAY;
/// Repeated deployments of one commit within this many seconds count once
/// (parallel jobs of one run deploying to the same environment).
pub const SAME_DEPLOY_SECS: i64 = 3_600;
/// A deployment followed within this many days by a revert or hotfix
/// counts for the heuristic failure rate.
pub const FIX_WINDOW_DAYS: i64 = 3;
/// Commits read per deployment for lead time.
pub const MAX_COMMITS_PER_DEPLOY: usize = 2_000;
/// Earlier deployed commits excluded from a deployment's new commits (so a
/// redeploy after a roll-back does not count old commits again).
pub const EXCLUDE_PREVIOUS: usize = 5;

/// How a deployment ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failure,
    /// Running, cancelled, abandoned: not counted.
    Other,
}

/// One deployment (or release, tag, workflow run) in time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub id: String,
    pub label: String,
    /// When it reached the environment.
    pub time: i64,
    /// A commit id or tag name to resolve locally.
    pub rev: Option<String>,
    pub outcome: Outcome,
    pub creator: String,
}

/// Sort oldest first and fold repeated deployments of one commit within
/// [`SAME_DEPLOY_SECS`] into one (it succeeded when any of them did).
pub fn prepare(mut events: Vec<Event>) -> Vec<Event> {
    events.sort_by(|a, b| a.time.cmp(&b.time).then_with(|| a.id.cmp(&b.id)));
    let mut out: Vec<Event> = Vec::with_capacity(events.len());
    for e in events {
        if let Some(last) = out.last_mut()
            && e.rev.is_some()
            && last.rev == e.rev
            && e.time - last.time <= SAME_DEPLOY_SECS
            && e.outcome != Outcome::Other
            && last.outcome != Outcome::Other
        {
            if e.outcome == Outcome::Success {
                last.outcome = Outcome::Success;
                last.time = e.time;
            }
            continue;
        }
        out.push(e);
    }
    out
}

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

/// Deployments to one environment as events: the time is when the
/// deployment succeeded (its success status), else when it was created.
pub fn events_from_deployments(deps: &[Deployment]) -> Vec<Event> {
    deps.iter()
        .map(|d| {
            let state = d.effective();
            let outcome = if state.succeeded() {
                Outcome::Success
            } else if state.failed() {
                Outcome::Failure
            } else {
                Outcome::Other
            };
            let time = match (&d.status, outcome) {
                (Some(s), Outcome::Success | Outcome::Failure) => s.created.unwrap_or(d.created),
                _ => d.created,
            }
            .max(d.created);
            Event {
                id: d.id.to_string(),
                label: format!(
                    "{}{}",
                    d.short_sha(),
                    if d.ref_name.is_empty() || d.ref_name == d.sha {
                        String::new()
                    } else {
                        format!(" ({})", d.ref_name)
                    }
                ),
                time,
                rev: (!d.sha.is_empty()).then(|| d.sha.clone()),
                outcome,
                creator: d.creator.clone(),
            }
        })
        .collect()
}

/// Published releases (not drafts; pre-releases only with
/// `prereleases`) as successful events of their tags.
pub fn events_from_releases(releases: &[Release], prereleases: bool) -> Vec<Event> {
    releases
        .iter()
        .filter(|r| !r.draft && (prereleases || !r.prerelease))
        .filter_map(|r| {
            Some(Event {
                id: r.id.to_string(),
                label: r.tag_name.clone(),
                time: r.published()?,
                rev: Some(format!("refs/tags/{}", r.tag_name)),
                outcome: Outcome::Success,
                creator: r.author_login().to_owned(),
            })
        })
        .collect()
}

/// Local tags matching `pattern` (a `git for-each-ref` glob such as
/// `v*`), dated by their creator date.
pub fn events_from_tags(
    dir: &Path,
    pattern: &str,
    commands: &mut Vec<String>,
) -> Result<Vec<Event>> {
    let pattern = pattern.trim();
    if pattern.starts_with('-') {
        return Err(Error::new("A tag pattern cannot start with a dash."));
    }
    let glob = format!(
        "refs/tags/{}",
        if pattern.is_empty() { "*" } else { pattern }
    );
    let out = local::git(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname:short)%1f%(creatordate:unix)%1f%(taggername)%(authorname)",
            &glob,
        ],
        commands,
    )?;
    Ok(out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let mut f = l.splitn(3, US);
            let name = f.next()?.to_owned();
            let time = f.next()?.trim().parse().ok()?;
            Some(Event {
                id: name.clone(),
                label: name.clone(),
                time,
                rev: Some(format!("refs/tags/{name}")),
                outcome: Outcome::Success,
                creator: f.next().unwrap_or("").to_owned(),
            })
        })
        .collect())
}

/// `actions/workflows/{id}/runs?branch=B&status=completed&per_page=100&page=N`.
pub fn runs_endpoint(gh: &Gh, workflow: u64, branch: &str, page: u32) -> String {
    gh.path(&format!(
        "actions/workflows/{workflow}/runs?branch={}&status=completed&per_page=100&page={page}",
        encode(branch)
    ))
}

/// Completed workflow runs from one `runs` page as events: success →
/// success, failure / timed out / startup failure → failure.
pub fn events_from_runs(v: &Value) -> Vec<Event> {
    v.get("workflow_runs")
        .and_then(Value::as_array)
        .map(|runs| {
            runs.iter()
                .filter_map(|r| {
                    let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("");
                    let time =
                        parse_time(s("updated_at")).or_else(|| parse_time(s("created_at")))?;
                    let outcome = match s("conclusion") {
                        "success" => Outcome::Success,
                        "failure" | "timed_out" | "startup_failure" => Outcome::Failure,
                        _ => Outcome::Other,
                    };
                    Some(Event {
                        id: r.get("id").and_then(Value::as_u64).unwrap_or(0).to_string(),
                        label: format!(
                            "#{} {}",
                            r.get("run_number").and_then(Value::as_u64).unwrap_or(0),
                            s("display_title")
                        ),
                        time,
                        rev: Some(s("head_sha").to_owned()).filter(|x| !x.is_empty()),
                        outcome,
                        creator: r
                            .pointer("/actor/login")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Completed runs of a workflow on `branch` back to `since` (5 pages at most).
pub fn fetch_run_events(
    gh: &Gh,
    workflow: u64,
    branch: &str,
    since: i64,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<Event>> {
    let mut out = Vec::new();
    for page in 1..=5 {
        let v: Value = gh.get_json(
            &runs_endpoint(gh, workflow, branch, page),
            "workflow runs",
            calls,
        )?;
        let events = events_from_runs(&v);
        let n = v
            .get("workflow_runs")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        let older = events.iter().any(|e| e.time < since);
        out.extend(events);
        if n < 100 || older {
            break;
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Local history
// ---------------------------------------------------------------------------

/// A commit a deployment brought: `(sha, commit time, subject)`.
pub type NewCommit = (String, i64, String);

/// What local history says about the events (in [`prepare`]d order).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalHistory {
    /// Commit id of each event's revision, `None` when not here.
    pub resolved: Vec<Option<String>>,
    /// For a successful event: the commits it deployed that the previous
    /// successful deployments did not have.
    pub new_commits: HashMap<usize, Vec<NewCommit>>,
    /// Events whose list hit [`MAX_COMMITS_PER_DEPLOY`].
    pub truncated: Vec<usize>,
}

/// Resolve every event and read the new commits of each successful one
/// (`git log cur ^prev ^prev2 …`), four ranges at a time.
pub fn collect_local(dir: &Path, events: &[Event], commands: &mut Vec<String>) -> LocalHistory {
    let revs: Vec<String> = events
        .iter()
        .map(|e| e.rev.clone().unwrap_or_default())
        .collect();
    let mut resolved = local::resolve_many(dir, &revs, commands);
    for (i, e) in events.iter().enumerate() {
        if e.rev.is_none() {
            resolved[i] = None;
        }
    }
    // (event index, its commit, the previous distinct deployed commits)
    let mut jobs: Vec<(usize, String, Vec<String>)> = Vec::new();
    let mut previous: Vec<String> = Vec::new();
    // Redeploys of the commit already live bring nothing new.
    let mut same: Vec<usize> = Vec::new();
    for (i, e) in events.iter().enumerate() {
        if e.outcome != Outcome::Success {
            continue;
        }
        let Some(sha) = resolved[i].clone() else {
            continue;
        };
        if previous.last() == Some(&sha) {
            same.push(i);
        }
        if !previous.is_empty() && previous.last() != Some(&sha) {
            let excl: Vec<String> = previous
                .iter()
                .rev()
                .filter(|p| **p != sha)
                .take(EXCLUDE_PREVIOUS)
                .cloned()
                .collect();
            jobs.push((i, sha.clone(), excl));
        }
        if previous.last() != Some(&sha) {
            previous.retain(|p| *p != sha);
            previous.push(sha);
        }
    }
    let max = format!("--max-count={MAX_COMMITS_PER_DEPLOY}");
    let results: Vec<(usize, Option<Vec<NewCommit>>)> = std::thread::scope(|s| {
        let chunks: Vec<&[(usize, String, Vec<String>)]> =
            jobs.chunks(jobs.len().div_ceil(4).max(1)).collect();
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                let max = max.clone();
                s.spawn(move || {
                    chunk
                        .iter()
                        .map(|(i, sha, excl)| {
                            let mut args: Vec<String> = vec![
                                "log".into(),
                                "--format=%H%x1f%ct%x1f%s".into(),
                                max.clone(),
                                sha.clone(),
                            ];
                            args.extend(excl.iter().map(|p| format!("^{p}")));
                            args.push("--".into());
                            let out = crate::cli::run(dir, &args, None).ok().map(|o| {
                                String::from_utf8_lossy(&o.stdout)
                                    .lines()
                                    .filter_map(|l| {
                                        let mut f = l.splitn(3, US);
                                        Some((
                                            f.next()?.to_owned(),
                                            f.next()?.trim().parse().ok()?,
                                            f.next().unwrap_or("").to_owned(),
                                        ))
                                    })
                                    .collect()
                            });
                            (*i, out)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    });
    let mut tally = local::Tally::new("git log ranges (one per deployment)");
    for (_, sha, excl) in &jobs {
        let mut c = format!("git log --format=%H%x1f%ct%x1f%s {max} {sha}");
        for p in excl {
            c.push_str(&format!(" ^{p}"));
        }
        tally.record(c, commands);
    }
    tally.finish(commands);
    let mut new_commits: HashMap<usize, Vec<NewCommit>> =
        same.into_iter().map(|i| (i, Vec::new())).collect();
    let mut truncated = Vec::new();
    for (i, list) in results {
        if let Some(list) = list {
            if list.len() >= MAX_COMMITS_PER_DEPLOY {
                truncated.push(i);
            }
            new_commits.insert(i, list);
        }
    }
    truncated.sort_unstable();
    LocalHistory {
        resolved,
        new_commits,
        truncated,
    }
}

// ---------------------------------------------------------------------------
// Computation
// ---------------------------------------------------------------------------

/// DORA performance band.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Band {
    Elite,
    High,
    Medium,
    Low,
}

impl Band {
    pub fn label(self) -> &'static str {
        match self {
            Band::Elite => "Elite",
            Band::High => "High",
            Band::Medium => "Medium",
            Band::Low => "Low",
        }
    }
}

/// Thresholds (DORA State of DevOps 2023 clusters), as shown on the page.
pub const THRESHOLDS: [(&str, [&str; 4]); 4] = [
    (
        "Deployment frequency",
        [
            "at least once a day (on demand)",
            "at least once a week",
            "at least once a month",
            "less than once a month",
        ],
    ),
    (
        "Lead time for changes (median)",
        [
            "under a day",
            "under a week",
            "under a month",
            "a month or more",
        ],
    ),
    (
        "Change failure rate",
        ["5% or less", "10% or less", "15% or less", "more than 15%"],
    ),
    (
        "Time to restore (median)",
        [
            "under an hour",
            "under a day",
            "under a week",
            "a week or more",
        ],
    ),
];

pub fn band_frequency(per_day: f64) -> Band {
    if per_day >= 1.0 {
        Band::Elite
    } else if per_day >= 1.0 / 7.0 {
        Band::High
    } else if per_day >= 1.0 / 30.0 {
        Band::Medium
    } else {
        Band::Low
    }
}

pub fn band_lead(secs: i64) -> Band {
    if secs < DAY {
        Band::Elite
    } else if secs < WEEK {
        Band::High
    } else if secs < 30 * DAY {
        Band::Medium
    } else {
        Band::Low
    }
}

pub fn band_failure(rate: f64) -> Band {
    if rate <= 0.05 {
        Band::Elite
    } else if rate <= 0.10 {
        Band::High
    } else if rate <= 0.15 {
        Band::Medium
    } else {
        Band::Low
    }
}

pub fn band_restore(secs: i64) -> Band {
    if secs < 3_600 {
        Band::Elite
    } else if secs < DAY {
        Band::High
    } else if secs < WEEK {
        Band::Medium
    } else {
        Band::Low
    }
}

/// Median, 90th percentile (nearest rank) and mean of durations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dist {
    pub n: usize,
    pub median: i64,
    pub p90: i64,
    pub mean: i64,
}

impl Dist {
    pub fn of(values: &[i64]) -> Option<Dist> {
        if values.is_empty() {
            return None;
        }
        let mut v = values.to_vec();
        v.sort_unstable();
        let n = v.len();
        let median = if n % 2 == 1 {
            v[n / 2]
        } else {
            (v[n / 2 - 1] + v[n / 2]) / 2
        };
        let rank = ((0.9 * n as f64).ceil() as usize).clamp(1, n);
        let mean = (v.iter().map(|x| *x as i128).sum::<i128>() / n as i128) as i64;
        Some(Dist {
            n,
            median,
            p90: v[rank - 1],
            mean,
        })
    }
}

/// Monday 00:00 UTC of the week holding `t`.
pub fn week_start(t: i64) -> i64 {
    let day = t.div_euclid(DAY);
    // 1970-01-01 was a Thursday: Monday-based weekday = (day + 3) mod 7.
    (day - (day + 3).rem_euclid(7)) * DAY
}

/// A revert or a hotfix, by its subject.
pub fn is_fix_commit(subject: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i)(^revert\b|\bhot[- _]?fix)").expect("valid regex"));
    re.is_match(subject.trim())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Week {
    pub start: i64,
    pub deploys: u32,
    pub failures: u32,
    pub lead_median: Option<i64>,
}

/// One event of the period, for the table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub index: usize,
    pub label: String,
    pub time: i64,
    pub outcome: Outcome,
    pub sha: Option<String>,
    pub creator: String,
    pub new_commits: Option<usize>,
    pub lead_median: Option<i64>,
    /// A revert or hotfix followed within the window.
    pub fix_followed: bool,
    /// Seconds to the next success, for a failure that started an incident.
    pub restored_after: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    pub events: usize,
    pub with_rev: usize,
    /// Events whose commit (or tag) is not in the local history.
    pub missing_local: usize,
    /// The first success of the period has no earlier success, so its
    /// commits cannot be told apart from all of history.
    pub first_without_predecessor: bool,
    pub lead_commits: usize,
    pub truncated: usize,
    pub folded: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub start: i64,
    pub end: i64,
    pub days: u32,
    pub successes: usize,
    pub failures: usize,
    /// Running, cancelled or abandoned events of the period.
    pub other: usize,
    pub per_day: f64,
    pub per_week: f64,
    pub weeks: Vec<Week>,
    pub lead: Option<Dist>,
    /// Failed over completed.
    pub failure_rate: Option<f64>,
    /// Successes followed by a revert or hotfix within the window.
    pub fix_followed: usize,
    pub fix_rate: Option<f64>,
    /// `(event label, commit subject)` of the first few.
    pub fix_examples: Vec<(String, String)>,
    pub restore: Option<Dist>,
    pub incidents: usize,
    pub unresolved: usize,
    pub band_frequency: Option<Band>,
    pub band_lead: Option<Band>,
    pub band_failure: Option<Band>,
    pub band_restore: Option<Band>,
    pub coverage: Coverage,
    /// Newest first.
    pub rows: Vec<Row>,
}

/// Compute the metrics for the `days` before `now`. `events` are
/// [`prepare`]d; `local` comes from [`collect_local`] on the same list;
/// `folded` is how many raw events `prepare` merged.
pub fn compute(
    events: &[Event],
    local: &LocalHistory,
    days: u32,
    now: i64,
    folded: usize,
) -> Report {
    let days = days.max(1);
    let start = now - days as i64 * DAY;
    let in_period = |e: &Event| e.time >= start && e.time <= now;
    let mut r = Report {
        start,
        end: now,
        days,
        ..Default::default()
    };
    r.coverage.folded = folded;

    // Lead time samples per event.
    let mut lead_by_event: HashMap<usize, Vec<i64>> = HashMap::new();
    for (i, e) in events.iter().enumerate() {
        if !in_period(e) || e.outcome != Outcome::Success {
            continue;
        }
        if let Some(list) = local.new_commits.get(&i) {
            let v: Vec<i64> = list.iter().map(|(_, t, _)| (e.time - t).max(0)).collect();
            lead_by_event.insert(i, v);
        }
    }

    // Incidents: a failure starts one, the next success ends it.
    let mut restored: HashMap<usize, i64> = HashMap::new();
    let mut restore_samples = Vec::new();
    let mut open: Option<usize> = None;
    for (i, e) in events.iter().enumerate() {
        match e.outcome {
            Outcome::Failure if open.is_none() => open = Some(i),
            Outcome::Success => {
                if let Some(f) = open.take()
                    && in_period(&events[f])
                {
                    let secs = (e.time - events[f].time).max(0);
                    restored.insert(f, secs);
                    restore_samples.push(secs);
                }
            }
            _ => {}
        }
    }
    if let Some(f) = open
        && in_period(&events[f])
    {
        r.unresolved = 1;
    }
    r.incidents = restore_samples.len() + r.unresolved;

    // Fix-followed heuristic.
    let window = FIX_WINDOW_DAYS * DAY;
    let mut fix_followed: HashMap<usize, String> = HashMap::new();
    for (i, e) in events.iter().enumerate() {
        if !in_period(e) || e.outcome != Outcome::Success {
            continue;
        }
        let hit = events
            .iter()
            .enumerate()
            .skip(i + 1)
            .take_while(|(_, n)| n.time - e.time <= window)
            .filter(|(_, n)| n.outcome == Outcome::Success)
            .find_map(|(j, _)| {
                local
                    .new_commits
                    .get(&j)?
                    .iter()
                    .find(|(_, _, s)| is_fix_commit(s))
                    .map(|(_, _, s)| s.clone())
            });
        if let Some(s) = hit {
            fix_followed.insert(i, s);
        }
    }

    // Rows, counts, weeks.
    let mut week_map: HashMap<i64, (u32, u32, Vec<i64>)> = HashMap::new();
    let mut ws = week_start(start);
    while ws <= now {
        week_map.insert(ws, (0, 0, Vec::new()));
        ws += WEEK;
    }
    let mut all_leads = Vec::new();
    let mut first_success_seen = false;
    for (i, e) in events.iter().enumerate() {
        if !in_period(e) {
            continue;
        }
        r.coverage.events += 1;
        if e.rev.is_some() {
            r.coverage.with_rev += 1;
            if local.resolved.get(i).cloned().flatten().is_none() {
                r.coverage.missing_local += 1;
            }
        }
        let entry = week_map.entry(week_start(e.time)).or_default();
        match e.outcome {
            Outcome::Success => {
                r.successes += 1;
                entry.0 += 1;
                if !first_success_seen {
                    first_success_seen = true;
                    let has_prev = events[..i].iter().enumerate().any(|(j, p)| {
                        p.outcome == Outcome::Success
                            && local.resolved.get(j).cloned().flatten().is_some()
                    });
                    if !has_prev {
                        r.coverage.first_without_predecessor = true;
                    }
                }
            }
            Outcome::Failure => {
                r.failures += 1;
                entry.1 += 1;
            }
            Outcome::Other => r.other += 1,
        }
        let leads = lead_by_event.get(&i);
        if let Some(l) = leads {
            entry.2.extend(l.iter().copied());
            all_leads.extend(l.iter().copied());
        }
        r.rows.push(Row {
            index: i,
            label: e.label.clone(),
            time: e.time,
            outcome: e.outcome,
            sha: local.resolved.get(i).cloned().flatten(),
            creator: e.creator.clone(),
            new_commits: local.new_commits.get(&i).map(Vec::len),
            lead_median: leads.and_then(|l| Dist::of(l)).map(|d| d.median),
            fix_followed: fix_followed.contains_key(&i),
            restored_after: restored.get(&i).copied(),
        });
    }
    r.rows.reverse();
    r.coverage.lead_commits = all_leads.len();
    r.coverage.truncated = local
        .truncated
        .iter()
        .filter(|i| events.get(**i).is_some_and(in_period))
        .count();
    let mut weeks: Vec<Week> = week_map
        .into_iter()
        .map(|(start, (d, f, leads))| Week {
            start,
            deploys: d,
            failures: f,
            lead_median: Dist::of(&leads).map(|x| x.median),
        })
        .collect();
    weeks.sort_by_key(|w| w.start);
    r.weeks = weeks;

    r.per_day = r.successes as f64 / days as f64;
    r.per_week = r.per_day * 7.0;
    r.lead = Dist::of(&all_leads);
    let completed = r.successes + r.failures;
    r.failure_rate = (completed > 0).then(|| r.failures as f64 / completed as f64);
    r.fix_followed = fix_followed.len();
    r.fix_rate = (r.successes > 0).then(|| r.fix_followed as f64 / r.successes as f64);
    let mut ex: Vec<(usize, String)> = fix_followed.into_iter().collect();
    ex.sort_by_key(|(i, _)| std::cmp::Reverse(*i));
    r.fix_examples = ex
        .into_iter()
        .take(5)
        .map(|(i, s)| (events[i].label.clone(), s))
        .collect();
    r.restore = Dist::of(&restore_samples);
    r.band_frequency = (r.coverage.events > 0).then(|| band_frequency(r.per_day));
    r.band_lead = r.lead.map(|d| band_lead(d.median));
    r.band_failure = r.failure_rate.map(band_failure);
    r.band_restore = r.restore.map(|d| band_restore(d.median));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn ev(id: &str, days_ago: f64, rev: &str, outcome: Outcome) -> Event {
        Event {
            id: id.into(),
            label: id.into(),
            time: NOW - (days_ago * DAY as f64) as i64,
            rev: Some(rev.into()),
            outcome,
            creator: "ada".into(),
        }
    }

    fn resolved(events: &[Event]) -> Vec<Option<String>> {
        events.iter().map(|e| e.rev.clone()).collect()
    }

    #[test]
    fn empty_data() {
        let r = compute(&[], &LocalHistory::default(), 30, NOW, 0);
        assert_eq!(r.successes, 0);
        assert_eq!(r.per_day, 0.0);
        assert!(r.lead.is_none());
        assert!(r.failure_rate.is_none());
        assert!(r.restore.is_none());
        assert!(r.band_frequency.is_none());
        assert!(r.rows.is_empty());
        // 30 days span five Monday-based weeks or so, all empty.
        assert!(r.weeks.len() >= 4 && r.weeks.iter().all(|w| w.deploys == 0));
    }

    #[test]
    fn single_deployment() {
        let events = prepare(vec![ev("d1", 2.0, "aaa", Outcome::Success)]);
        let local = LocalHistory {
            resolved: resolved(&events),
            ..Default::default()
        };
        let r = compute(&events, &local, 30, NOW, 0);
        assert_eq!(r.successes, 1);
        assert!((r.per_day - 1.0 / 30.0).abs() < 1e-9);
        assert_eq!(r.band_frequency, Some(Band::Medium));
        assert!(r.lead.is_none(), "no predecessor, no lead time");
        assert!(r.coverage.first_without_predecessor);
        assert_eq!(r.failure_rate, Some(0.0));
        assert_eq!(r.band_failure, Some(Band::Elite));
        assert_eq!(r.rows.len(), 1);
    }

    #[test]
    fn prepare_folds_parallel_deployments() {
        let mut a = ev("1", 1.0, "aaa", Outcome::Failure);
        let mut b = ev("2", 1.0, "aaa", Outcome::Success);
        b.time = a.time + 60;
        let c = ev("3", 0.5, "aaa", Outcome::Success);
        a.time -= 1;
        let p = prepare(vec![c, b, a]);
        assert_eq!(p.len(), 2, "{p:?}");
        assert_eq!(p[0].outcome, Outcome::Success);
        assert_eq!(p[1].id, "3");
    }

    #[test]
    fn frequency_lead_failure_and_restore() {
        // Ten days: success, failure, success (restore), success with a
        // revert after the first one.
        let events = prepare(vec![
            ev("d0", 40.0, "s0", Outcome::Success),
            ev("d1", 10.0, "s1", Outcome::Success),
            ev("d2", 9.0, "s2", Outcome::Failure),
            ev("d3", 8.5, "s3", Outcome::Success),
            ev("d4", 8.0, "s4", Outcome::Success),
            ev("d5", 3.0, "s5", Outcome::Other),
        ]);
        let t = |days_ago: f64| NOW - (days_ago * DAY as f64) as i64;
        let mut new_commits = HashMap::new();
        new_commits.insert(1, vec![("c1".into(), t(12.0), "feat: one".into())]);
        new_commits.insert(
            3,
            vec![
                ("c3".into(), t(9.0), "fix: two".into()),
                ("c3b".into(), t(8.75), "Revert \"feat: one\"".into()),
            ],
        );
        new_commits.insert(4, vec![("c4".into(), t(8.25), "docs".into())]);
        let local = LocalHistory {
            resolved: resolved(&events),
            new_commits,
            truncated: vec![],
        };
        let r = compute(&events, &local, 30, NOW, 0);
        assert_eq!((r.successes, r.failures, r.other), (3, 1, 1));
        assert!((r.per_day - 0.1).abs() < 1e-9);
        assert!((r.per_week - 0.7).abs() < 1e-9);
        assert_eq!(r.band_frequency, Some(Band::Medium));
        // Leads: 2 days, 0.5 day, 0.25 day, 0.25 day.
        let lead = r.lead.unwrap();
        assert_eq!(lead.n, 4);
        assert_eq!(lead.median, ((0.25 + 0.5) / 2.0 * DAY as f64) as i64);
        assert_eq!(lead.p90, 2 * DAY);
        assert_eq!(r.band_lead, Some(Band::Elite));
        assert_eq!(r.failure_rate, Some(0.25));
        assert_eq!(r.band_failure, Some(Band::Low));
        // Restore: failure at 9 days, success at 8.5 days.
        let restore = r.restore.unwrap();
        assert_eq!(restore.median, DAY / 2);
        assert_eq!(r.band_restore, Some(Band::High));
        assert_eq!((r.incidents, r.unresolved), (1, 0));
        // d1 was followed by the revert in d3, within three days.
        assert_eq!(r.fix_followed, 1);
        assert_eq!(r.fix_examples[0].0, "d1");
        assert!(
            !r.coverage.first_without_predecessor,
            "d0 precedes the period"
        );
        // Rows newest first; d0 is outside the period.
        assert_eq!(r.rows.len(), 5);
        assert_eq!(r.rows[0].label, "d5");
        assert_eq!(r.rows.last().unwrap().label, "d1");
        assert!(r.rows.last().unwrap().fix_followed);
        let fail_row = r.rows.iter().find(|x| x.label == "d2").unwrap();
        assert_eq!(fail_row.restored_after, Some(DAY / 2));
        // Weekly bars add up.
        let total: u32 = r.weeks.iter().map(|w| w.deploys).sum();
        assert_eq!(total, 3);
    }

    #[test]
    fn unresolved_incident_and_coverage() {
        let events = prepare(vec![
            ev("d1", 5.0, "s1", Outcome::Success),
            ev("d2", 1.0, "s2", Outcome::Failure),
        ]);
        let local = LocalHistory {
            resolved: vec![None, Some("s2".into())],
            ..Default::default()
        };
        let r = compute(&events, &local, 30, NOW, 2);
        assert_eq!(r.unresolved, 1);
        assert_eq!(r.incidents, 1);
        assert!(r.restore.is_none());
        assert_eq!(r.coverage.missing_local, 1);
        assert_eq!(r.coverage.events, 2);
        assert_eq!(r.coverage.folded, 2);
    }

    #[test]
    fn bands_and_helpers() {
        assert_eq!(band_frequency(3.0), Band::Elite);
        assert_eq!(band_frequency(0.2), Band::High);
        assert_eq!(band_frequency(0.05), Band::Medium);
        assert_eq!(band_frequency(0.01), Band::Low);
        assert_eq!(band_lead(3_600), Band::Elite);
        assert_eq!(band_lead(3 * DAY), Band::High);
        assert_eq!(band_lead(10 * DAY), Band::Medium);
        assert_eq!(band_lead(40 * DAY), Band::Low);
        assert_eq!(band_failure(0.05), Band::Elite);
        assert_eq!(band_failure(0.08), Band::High);
        assert_eq!(band_failure(0.15), Band::Medium);
        assert_eq!(band_failure(0.5), Band::Low);
        assert_eq!(band_restore(600), Band::Elite);
        assert_eq!(band_restore(7_200), Band::High);
        assert_eq!(band_restore(2 * DAY), Band::Medium);
        assert_eq!(band_restore(9 * DAY), Band::Low);
        let d = Dist::of(&[5, 1, 3, 2, 4, 10, 6, 7, 8, 9]).unwrap();
        assert_eq!((d.n, d.median, d.p90, d.mean), (10, 5, 9, 5));
        assert_eq!(Dist::of(&[7]).unwrap().p90, 7);
        // 2026-10-26 (a Monday) 12:00 UTC → that day at 00:00.
        let monday = 1_792_972_800; // 2026-10-26T00:00:00Z
        assert_eq!(week_start(monday + 12 * 3_600), monday);
        assert_eq!(week_start(monday + 6 * DAY + 3_600), monday);
        assert_eq!(week_start(monday - 1), monday - WEEK);
        assert!(is_fix_commit("Revert \"feat: x\""));
        assert!(is_fix_commit("Hotfix for login"));
        assert!(is_fix_commit("fix(api): hot-fix rate limit"));
        assert!(!is_fix_commit("fix: typo"));
        assert!(!is_fix_commit("Reverting is hard (docs)"));
    }

    #[test]
    fn source_conversions() {
        let v = serde_json::json!({"workflow_runs": [
            {"id": 7, "run_number": 12, "display_title": "Deploy", "head_sha": "abc",
             "conclusion": "success", "updated_at": "2026-10-20T10:00:00Z", "actor": {"login": "ada"}},
            {"id": 8, "run_number": 13, "head_sha": "def", "conclusion": "timed_out",
             "updated_at": "2026-10-21T10:00:00Z"},
            {"id": 9, "head_sha": "ghi", "conclusion": "cancelled", "updated_at": "2026-10-22T10:00:00Z"}]});
        let e = events_from_runs(&v);
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].outcome, Outcome::Success);
        assert_eq!(e[0].label, "#12 Deploy");
        assert_eq!(e[1].outcome, Outcome::Failure);
        assert_eq!(e[2].outcome, Outcome::Other);
        let gh = Gh::new(".", "o", "r");
        assert_eq!(
            runs_endpoint(&gh, 5, "main", 2),
            "repos/o/r/actions/workflows/5/runs?branch=main&status=completed&per_page=100&page=2"
        );
        let rel = vec![
            Release {
                id: 1,
                tag_name: "v1".into(),
                published_at: Some("2026-10-01T00:00:00Z".into()),
                ..Default::default()
            },
            Release {
                id: 2,
                tag_name: "v2-rc".into(),
                prerelease: true,
                published_at: Some("2026-10-02T00:00:00Z".into()),
                ..Default::default()
            },
            Release {
                id: 3,
                tag_name: "v3".into(),
                draft: true,
                ..Default::default()
            },
        ];
        let e = events_from_releases(&rel, false);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].rev.as_deref(), Some("refs/tags/v1"));
        assert_eq!(events_from_releases(&rel, true).len(), 2);
        use super::super::deploy::{DeployState, DeployStatus};
        let dep = Deployment {
            id: 4,
            sha: "4f2cfe92e3499f".into(),
            ref_name: "trunk".into(),
            created: 100,
            state: DeployState::Active,
            status: Some(DeployStatus {
                state: DeployState::Success,
                created: Some(400),
                ..Default::default()
            }),
            ..Default::default()
        };
        let e = events_from_deployments(&[dep]);
        assert_eq!(e[0].time, 400);
        assert_eq!(e[0].outcome, Outcome::Success);
        assert_eq!(e[0].label, "4f2cfe92 (trunk)");
    }
}
