//! Per-workflow history: success rate, duration and queue percentiles,
//! runs by branch and event, flaky runs (from the run list alone), and the
//! on-demand per-job analysis (from the jobs of each attempt).

use std::collections::{BTreeMap, HashMap};

use crate::github::{Job, Outcome, WorkflowRun};

/// The `p`-th percentile (0..=100) by nearest rank of unsorted values.
pub fn percentile(values: &[i64], p: f64) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    let rank = ((p / 100.0) * v.len() as f64).ceil() as usize;
    Some(v[rank.clamp(1, v.len()) - 1])
}

pub fn median(values: &[i64]) -> Option<i64> {
    percentile(values, 50.0)
}

/// A finished run that counts towards the success rate: passed, failed or
/// timed out (cancelled and skipped runs say nothing about health).
fn decisive(o: Outcome) -> bool {
    o.is_good() || o.is_bad()
}

/// Headline numbers of a set of runs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub runs: usize,
    pub passed: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub skipped: usize,
    pub running: usize,
    /// passed / (passed + failed), when any run was decisive.
    pub success_rate: Option<f64>,
    pub median_secs: Option<i64>,
    pub p90_secs: Option<i64>,
    pub median_queue_secs: Option<i64>,
    /// `(run id, run number, time)` of the newest failure and success.
    pub last_failure: Option<(u64, u64, i64)>,
    pub last_success: Option<(u64, u64, i64)>,
}

/// Duration of a finished run (`None` while running).
pub fn finished_secs(r: &WorkflowRun) -> Option<i64> {
    if r.is_running() {
        return None;
    }
    r.duration_secs(0)
}

pub fn stats(runs: &[WorkflowRun]) -> Stats {
    let mut s = Stats {
        runs: runs.len(),
        ..Default::default()
    };
    let mut durations = Vec::new();
    let mut queues = Vec::new();
    for r in runs {
        let o = r.outcome();
        match o {
            o if o.is_good() => s.passed += 1,
            o if o.is_bad() => s.failed += 1,
            Outcome::Cancelled => s.cancelled += 1,
            Outcome::Skipped => s.skipped += 1,
            o if o.is_running() => s.running += 1,
            _ => {}
        }
        if decisive(o)
            && let Some(d) = finished_secs(r)
        {
            durations.push(d);
        }
        if let Some(q) = r.queue_secs() {
            queues.push(q);
        }
        let when = r.started.or(r.created).unwrap_or(0);
        let slot = if o.is_bad() {
            Some(&mut s.last_failure)
        } else if o.is_good() {
            Some(&mut s.last_success)
        } else {
            None
        };
        if let Some(slot) = slot
            && slot.is_none_or(|(_, _, t)| when > t)
        {
            *slot = Some((r.id, r.run_number, when));
        }
    }
    let decided = s.passed + s.failed;
    s.success_rate = (decided > 0).then(|| s.passed as f64 / decided as f64);
    s.median_secs = median(&durations);
    s.p90_secs = percentile(&durations, 90.0);
    s.median_queue_secs = median(&queues);
    s
}

/// Runs grouped by a key (branch, event).
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub key: String,
    pub runs: usize,
    pub passed: usize,
    pub failed: usize,
    pub success_rate: Option<f64>,
    pub median_secs: Option<i64>,
}

fn group_by(runs: &[WorkflowRun], key: impl Fn(&WorkflowRun) -> String) -> Vec<Group> {
    let mut map: BTreeMap<String, Vec<WorkflowRun>> = BTreeMap::new();
    for r in runs {
        map.entry(key(r)).or_default().push(r.clone());
    }
    let mut out: Vec<Group> = map
        .into_iter()
        .map(|(k, rs)| {
            let s = stats(&rs);
            Group {
                key: k,
                runs: s.runs,
                passed: s.passed,
                failed: s.failed,
                success_rate: s.success_rate,
                median_secs: s.median_secs,
            }
        })
        .collect();
    out.sort_by(|a, b| b.runs.cmp(&a.runs).then(a.key.cmp(&b.key)));
    out
}

pub fn by_branch(runs: &[WorkflowRun]) -> Vec<Group> {
    group_by(runs, |r| {
        if r.branch.is_empty() {
            "(no branch)".to_owned()
        } else {
            r.branch.clone()
        }
    })
}

pub fn by_event(runs: &[WorkflowRun]) -> Vec<Group> {
    group_by(runs, |r| r.event.clone())
}

/// A commit that both failed and passed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MixedSha {
    pub sha: String,
    pub branch: String,
    /// Run ids, newest first.
    pub failed: Vec<u64>,
    pub passed: Vec<u64>,
}

/// Flakiness visible without extra calls.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Flaky {
    /// Runs that passed only after a re-run (`run_attempt > 1`).
    pub passed_on_retry: Vec<u64>,
    /// Commits with both a failed and a passed run of the workflow.
    pub mixed_shas: Vec<MixedSha>,
}

pub fn flaky(runs: &[WorkflowRun]) -> Flaky {
    let passed_on_retry = runs
        .iter()
        .filter(|r| r.attempt > 1 && r.is_success())
        .map(|r| r.id)
        .collect();
    // Per commit: its runs and the newest creation time.
    let mut by_sha: BTreeMap<&str, (MixedSha, i64)> = BTreeMap::new();
    for r in runs {
        if r.sha.is_empty() {
            continue;
        }
        let (m, t) = by_sha.entry(&r.sha).or_insert_with(|| {
            (
                MixedSha {
                    sha: r.sha.clone(),
                    branch: r.branch.clone(),
                    failed: Vec::new(),
                    passed: Vec::new(),
                },
                0,
            )
        });
        if r.is_failure() {
            m.failed.push(r.id);
        } else if r.is_success() {
            m.passed.push(r.id);
        }
        *t = (*t).max(r.created.unwrap_or(0));
    }
    let mut mixed: Vec<(MixedSha, i64)> = by_sha
        .into_values()
        .filter(|(m, _)| !m.failed.is_empty() && !m.passed.is_empty())
        .collect();
    mixed.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
    Flaky {
        passed_on_retry,
        mixed_shas: mixed.into_iter().map(|(m, _)| m).collect(),
    }
}

/// The runs worth analysing job by job: the newest `max` finished ones.
pub fn analysis_runs(runs: &[WorkflowRun], max: usize) -> Vec<&WorkflowRun> {
    let mut v: Vec<&WorkflowRun> = runs.iter().filter(|r| !r.is_running()).collect();
    v.sort_by_key(|r| std::cmp::Reverse(r.created));
    v.truncate(max);
    v
}

/// API calls the job analysis of `runs` takes: one jobs listing per
/// attempt (more for a run with over 100 jobs).
pub fn analysis_calls(runs: &[&WorkflowRun]) -> usize {
    runs.iter().map(|r| r.attempt.max(1) as usize).sum()
}

/// Per-job numbers across the analysed runs.
#[derive(Clone, Debug, PartialEq)]
pub struct JobStats {
    pub name: String,
    /// Runs the job appeared in.
    pub runs: usize,
    /// Attempts (a job re-run twice counts three times).
    pub attempts: usize,
    pub failures: usize,
    /// failures / attempts with a decisive outcome.
    pub failure_rate: Option<f64>,
    /// Runs where the job failed in one attempt and passed in a later one.
    pub failed_then_passed: usize,
    pub median_secs: Option<i64>,
    pub max_secs: Option<i64>,
    /// `(step name, median secs, max secs)` of the slowest step by median.
    pub slowest_step: Option<(String, i64, i64)>,
}

/// One analysed run: its id and the jobs of every attempt (attempt 1
/// first).
#[derive(Clone, Debug, Default)]
pub struct RunJobs {
    pub run_id: u64,
    pub attempts: Vec<Vec<Job>>,
}

pub fn job_stats(runs: &[RunJobs]) -> Vec<JobStats> {
    #[derive(Default)]
    struct Acc {
        runs: usize,
        attempts: usize,
        failures: usize,
        decisive: usize,
        failed_then_passed: usize,
        durations: Vec<i64>,
        steps: HashMap<String, Vec<i64>>,
    }
    let mut acc: BTreeMap<String, Acc> = BTreeMap::new();
    for run in runs {
        // Outcomes of each job name across the attempts of this run.
        let mut seen: BTreeMap<&str, Vec<Outcome>> = BTreeMap::new();
        for attempt in &run.attempts {
            for job in attempt {
                let a = acc.entry(job.name.clone()).or_default();
                let o = job.outcome();
                a.attempts += 1;
                if o.is_bad() {
                    a.failures += 1;
                }
                if decisive(o) {
                    a.decisive += 1;
                    if let Some(d) = job.duration_secs(0) {
                        a.durations.push(d);
                    }
                    for s in &job.steps {
                        if let Some(d) = s.duration_secs(0) {
                            a.steps.entry(s.name.clone()).or_default().push(d);
                        }
                    }
                }
                seen.entry(&job.name).or_default().push(o);
            }
        }
        for (name, outcomes) in seen {
            let a = acc.entry(name.to_owned()).or_default();
            a.runs += 1;
            let first_fail = outcomes.iter().position(|o| o.is_bad());
            if let Some(i) = first_fail
                && outcomes[i + 1..].iter().any(|o| o.is_good())
            {
                a.failed_then_passed += 1;
            }
        }
    }
    let mut out: Vec<JobStats> = acc
        .into_iter()
        .map(|(name, a)| {
            let slowest_step = a
                .steps
                .iter()
                .filter_map(|(n, d)| Some((n.clone(), median(d)?, *d.iter().max()?)))
                .max_by(|x, y| x.1.cmp(&y.1).then(y.0.cmp(&x.0)));
            JobStats {
                name,
                runs: a.runs,
                attempts: a.attempts,
                failures: a.failures,
                failure_rate: (a.decisive > 0).then(|| a.failures as f64 / a.decisive as f64),
                failed_then_passed: a.failed_then_passed,
                median_secs: median(&a.durations),
                max_secs: a.durations.iter().max().copied(),
                slowest_step,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.failed_then_passed
            .cmp(&a.failed_then_passed)
            .then(
                b.failure_rate
                    .unwrap_or(0.0)
                    .partial_cmp(&a.failure_rate.unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(a.name.cmp(&b.name))
    });
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::github::Step;

    pub fn run(id: u64, conclusion: Option<&str>, secs: i64, queue: i64) -> WorkflowRun {
        WorkflowRun {
            id,
            name: "CI".into(),
            status: if conclusion.is_some() {
                "completed".into()
            } else {
                "in_progress".into()
            },
            conclusion: conclusion.map(str::to_owned),
            branch: "main".into(),
            sha: format!("sha{id}"),
            url: String::new(),
            event: "push".into(),
            created: Some(1_000 + id as i64 * 100),
            started: Some(1_000 + id as i64 * 100 + queue),
            updated: Some(1_000 + id as i64 * 100 + queue + secs),
            run_number: id,
            attempt: 1,
            title: format!("run {id}"),
            actor: "ada".into(),
            triggering_actor: "ada".into(),
            path: ".github/workflows/ci.yml".into(),
            workflow_id: 9,
            head_message: String::new(),
            head_author: String::new(),
        }
    }

    #[test]
    fn percentiles_by_nearest_rank() {
        let v = [15, 20, 35, 40, 50];
        assert_eq!(percentile(&v, 30.0), Some(20));
        assert_eq!(percentile(&v, 40.0), Some(20));
        assert_eq!(percentile(&v, 50.0), Some(35));
        assert_eq!(percentile(&v, 100.0), Some(50));
        assert_eq!(percentile(&v, 0.0), Some(15));
        let ten: Vec<i64> = (1..=10).collect();
        assert_eq!(median(&ten), Some(5));
        assert_eq!(percentile(&ten, 90.0), Some(9));
        assert_eq!(percentile(&[7], 90.0), Some(7));
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[3, 1, 2]), Some(2));
    }

    #[test]
    fn workflow_stats() {
        let runs = vec![
            run(1, Some("success"), 100, 10),
            run(2, Some("failure"), 200, 20),
            run(3, Some("success"), 300, 30),
            run(4, Some("cancelled"), 50, 40),
            run(5, None, 0, 5),
            run(6, Some("success"), 400, 50),
        ];
        let s = stats(&runs);
        assert_eq!(s.runs, 6);
        assert_eq!((s.passed, s.failed, s.cancelled, s.running), (3, 1, 1, 1));
        assert_eq!(s.success_rate, Some(0.75));
        // Decisive durations: 100, 200, 300, 400.
        assert_eq!(s.median_secs, Some(200));
        assert_eq!(s.p90_secs, Some(400));
        // Queues: 10, 20, 30, 40, 5, 50 → sorted 5 10 20 30 40 50, median rank 3.
        assert_eq!(s.median_queue_secs, Some(20));
        assert_eq!(s.last_failure.map(|f| f.0), Some(2));
        assert_eq!(s.last_success.map(|f| f.0), Some(6));
        assert_eq!(stats(&[]).success_rate, None);
    }

    #[test]
    fn groups_by_branch_and_event() {
        let mut a = run(1, Some("success"), 10, 0);
        a.branch = "dev".into();
        let mut b = run(2, Some("failure"), 20, 0);
        b.event = "pull_request".into();
        let c = run(3, Some("success"), 30, 0);
        let g = by_branch(&[a.clone(), b.clone(), c.clone()]);
        assert_eq!(g[0].key, "main");
        assert_eq!((g[0].runs, g[0].passed, g[0].failed), (2, 1, 1));
        assert_eq!(g[0].success_rate, Some(0.5));
        assert_eq!(g[1].key, "dev");
        let e = by_event(&[a, b, c]);
        assert_eq!(e[0].key, "push");
        assert_eq!(e[0].runs, 2);
        assert_eq!(e[1].key, "pull_request");
    }

    #[test]
    fn flaky_detection() {
        let mut retried = run(1, Some("success"), 10, 0);
        retried.attempt = 2;
        let mut f = run(2, Some("failure"), 10, 0);
        f.sha = "same".into();
        let mut p = run(3, Some("success"), 10, 0);
        p.sha = "same".into();
        let other = run(4, Some("failure"), 10, 0);
        let fl = flaky(&[retried, f, p, other]);
        assert_eq!(fl.passed_on_retry, [1]);
        assert_eq!(fl.mixed_shas.len(), 1);
        assert_eq!(fl.mixed_shas[0].sha, "same");
        assert_eq!(fl.mixed_shas[0].failed, [2]);
        assert_eq!(fl.mixed_shas[0].passed, [3]);
    }

    fn job(name: &str, conclusion: &str, secs: i64, steps: &[(&str, i64)]) -> Job {
        Job {
            id: 0,
            run_id: 0,
            name: name.into(),
            status: "completed".into(),
            conclusion: Some(conclusion.into()),
            started: Some(0),
            completed: Some(secs),
            runner: None,
            runner_group: None,
            labels: Vec::new(),
            url: String::new(),
            attempt: 1,
            steps: steps
                .iter()
                .enumerate()
                .map(|(i, (n, d))| Step {
                    number: i as u32 + 1,
                    name: (*n).into(),
                    status: "completed".into(),
                    conclusion: Some("success".into()),
                    started: Some(0),
                    completed: Some(*d),
                })
                .collect(),
        }
    }

    #[test]
    fn job_analysis() {
        let runs = vec![
            RunJobs {
                run_id: 1,
                attempts: vec![
                    vec![
                        job("test", "failure", 100, &[("cargo test", 90), ("setup", 5)]),
                        job("lint", "success", 20, &[("clippy", 15)]),
                    ],
                    vec![job(
                        "test",
                        "success",
                        120,
                        &[("cargo test", 110), ("setup", 6)],
                    )],
                ],
            },
            RunJobs {
                run_id: 2,
                attempts: vec![vec![
                    job("test", "success", 80, &[("cargo test", 70), ("setup", 4)]),
                    job("lint", "failure", 30, &[("clippy", 25)]),
                ]],
            },
        ];
        let s = job_stats(&runs);
        assert_eq!(s[0].name, "test");
        assert_eq!(s[0].runs, 2);
        assert_eq!(s[0].attempts, 3);
        assert_eq!(s[0].failures, 1);
        assert_eq!(s[0].failed_then_passed, 1);
        assert_eq!(s[0].median_secs, Some(100));
        assert_eq!(s[0].max_secs, Some(120));
        assert_eq!(s[0].slowest_step, Some(("cargo test".to_owned(), 90, 110)));
        let lint = s.iter().find(|j| j.name == "lint").unwrap();
        assert_eq!(lint.failure_rate, Some(0.5));
        assert_eq!(lint.failed_then_passed, 0);
        let r1 = run(1, Some("success"), 1, 0);
        let mut r2 = run(2, Some("failure"), 1, 0);
        r2.attempt = 3;
        let r3 = run(3, None, 1, 0);
        let all = [r1, r2, r3];
        let picked = analysis_runs(&all, 30);
        assert_eq!(picked.len(), 2);
        assert_eq!(picked[0].id, 2);
        assert_eq!(analysis_calls(&picked), 4);
    }
}
