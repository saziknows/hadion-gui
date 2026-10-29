//! CI control for GitHub Actions: `workflow_dispatch` inputs parsed from
//! the workflow file and dispatching a run, re-runs with debug logging,
//! bulk cancel, artifacts (list, download, delete), caches (list, usage,
//! delete), per-workflow history (duration trend, success rate, queue time,
//! flaky jobs), comparing a failed run with the last green one (commits in
//! between, normalised log diff), workflow YAML validation (built-in rules
//! plus `actionlint` when installed) and local runs through `act`.
//!
//! - [`requests`]: every write as a pure [`Request`] builder, and the read
//!   endpoints.
//! - [`model`]: JSON models (runs, jobs, artifacts, caches, compare).
//! - [`workflow`]: workflow files in the working tree and their dispatch
//!   inputs.
//! - [`history`]: success rate, percentiles, groups, flaky runs, per-job
//!   analysis.
//! - [`compare`]: the last green run, commits in between, log diff.
//! - [`lint`]: the validator and actionlint.
//! - [`act`]: `act -l` and the command of a local run.
//!
//! The fetch functions below run the reads through [`Gh`] so every call is
//! recorded for the activity log.

pub mod act;
pub mod compare;
pub mod history;
pub mod lint;
pub mod model;
pub mod requests;
pub mod workflow;
pub mod yaml;

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::Result;
use crate::devops::gh::{Gh, Request};
use crate::github::{ApiCall, Job, WorkflowRun};

use model::{Artifact, ArtifactsPage, Between, Cache, CacheUsage, CompareJson, JobsPage, RunsPage};

/// The newest runs of a workflow (one page of up to 100), and the total.
pub fn fetch_workflow_runs(
    gh: &Gh,
    workflow: u64,
    branch: Option<&str>,
    calls: &mut Vec<ApiCall>,
) -> Result<(Vec<WorkflowRun>, u64)> {
    let page: RunsPage = gh.get_json(
        &requests::workflow_runs(gh, workflow, branch, 100),
        "workflow runs",
        calls,
    )?;
    Ok((
        page.workflow_runs
            .into_iter()
            .map(model::RunJson::into_run)
            .collect(),
        page.total_count,
    ))
}

/// One run by id.
pub fn fetch_run(gh: &Gh, run: u64, calls: &mut Vec<ApiCall>) -> Result<WorkflowRun> {
    let r: model::RunJson = gh.get_json(
        &gh.path(&format!("actions/runs/{run}")),
        "workflow run",
        calls,
    )?;
    Ok(r.into_run())
}

/// Artifacts of one run.
pub fn fetch_run_artifacts(gh: &Gh, run: u64, calls: &mut Vec<ApiCall>) -> Result<Vec<Artifact>> {
    let page: ArtifactsPage = gh.get_json(&requests::run_artifacts(gh, run), "artifacts", calls)?;
    Ok(page.artifacts)
}

/// The repository's artifacts, newest first, up to `max_pages` pages of
/// 100, and the total GitHub reports.
pub fn fetch_artifacts(
    gh: &Gh,
    max_pages: usize,
    calls: &mut Vec<ApiCall>,
) -> Result<(Vec<Artifact>, u64)> {
    gh.get_wrapped_pages(
        &requests::artifacts(gh),
        "artifacts",
        "artifacts",
        max_pages,
        calls,
    )
}

pub fn fetch_cache_usage(gh: &Gh, calls: &mut Vec<ApiCall>) -> Result<CacheUsage> {
    gh.get_json(&requests::cache_usage(gh), "cache usage", calls)
}

/// Cache entries sorted by `sort`, up to `max_pages` pages of 100.
pub fn fetch_caches(
    gh: &Gh,
    sort: &str,
    max_pages: usize,
    calls: &mut Vec<ApiCall>,
) -> Result<(Vec<Cache>, u64)> {
    gh.get_wrapped_pages(
        &requests::caches(gh, sort),
        "actions_caches",
        "caches",
        max_pages,
        calls,
    )
}

/// Jobs of one attempt of a run (the first 100).
pub fn fetch_attempt_jobs(
    gh: &Gh,
    run: u64,
    attempt: u32,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<Job>> {
    let page: JobsPage = gh.get_json(&requests::attempt_jobs(gh, run, attempt), "jobs", calls)?;
    Ok(page
        .jobs
        .into_iter()
        .map(model::JobJson::into_job)
        .collect())
}

/// Jobs of the latest attempt (all pages).
pub fn fetch_jobs(gh: &Gh, run: u64, calls: &mut Vec<ApiCall>) -> Result<Vec<Job>> {
    crate::github::fetch_jobs(gh.dir(), &gh.owner, &gh.repo, run, calls)
}

/// Commits and files between two commits through the compare API.
pub fn fetch_compare(gh: &Gh, base: &str, head: &str, calls: &mut Vec<ApiCall>) -> Result<Between> {
    let c: CompareJson = gh.get_json(&requests::compare(gh, base, head), "comparison", calls)?;
    Ok(c.into_between())
}

/// The newest successful run of `bad`'s workflow on its branch that was
/// created before it, from the API.
pub fn fetch_last_green(
    gh: &Gh,
    bad: &WorkflowRun,
    calls: &mut Vec<ApiCall>,
) -> Result<Option<WorkflowRun>> {
    let before = bad.created.map(rfc3339).unwrap_or_default();
    let page: RunsPage = gh.get_json(
        &requests::last_green(gh, bad.workflow_id, &bad.branch, &before),
        "workflow runs",
        calls,
    )?;
    let runs: Vec<WorkflowRun> = page
        .workflow_runs
        .into_iter()
        .map(model::RunJson::into_run)
        .collect();
    Ok(compare::last_green(&runs, bad).cloned())
}

/// The state (`OPEN`, `CLOSED`, `MERGED`) of pull requests by number, in
/// one GraphQL request per 100 numbers. Numbers GitHub cannot resolve are
/// left out.
pub fn fetch_pr_states(
    gh: &Gh,
    numbers: &[u64],
    calls: &mut Vec<ApiCall>,
) -> Result<HashMap<u64, String>> {
    let mut out = HashMap::new();
    for chunk in numbers.chunks(100) {
        let req = Request::post("graphql", pr_states_body(&gh.owner, &gh.repo, chunk));
        let v: Value = gh.send_json(&req, "pull request states", calls)?;
        let Some(repo) = v.get("data").and_then(|d| d.get("repository")) else {
            let msg = v
                .get("errors")
                .and_then(Value::as_array)
                .and_then(|e| e.first())
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("GitHub returned no repository")
                .to_owned();
            return Err(crate::Error::new(msg));
        };
        for n in chunk {
            if let Some(state) = repo
                .get(format!("p{n}"))
                .and_then(|p| p.get("state"))
                .and_then(Value::as_str)
            {
                out.insert(*n, state.to_owned());
            }
        }
    }
    Ok(out)
}

/// The GraphQL body asking for the state of each pull request number (one
/// alias per number, `issueOrPullRequest` so an issue number does not fail
/// the whole query).
pub fn pr_states_body(owner: &str, repo: &str, numbers: &[u64]) -> Value {
    let fields: String = numbers
        .iter()
        .map(|n| {
            format!("p{n}: issueOrPullRequest(number: {n}) {{ ... on PullRequest {{ state }} }} ")
        })
        .collect();
    let query = format!(
        "query($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{ {fields}}} }}"
    );
    json!({ "query": query, "variables": { "owner": owner, "name": repo } })
}

/// `YYYY-MM-DDTHH:MM:SSZ` of epoch seconds (UTC).
pub fn rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil from days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60
    )
}

// ---------------------------------------------------------------------------
// Validation of the working tree's workflow files
// ---------------------------------------------------------------------------

/// `(modified ms since the epoch, size)` of a file, to know when to lint
/// it again.
pub type Stamp = (u128, u64);

pub fn stamp(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    let t = m
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis();
    Some((t, m.len()))
}

/// One linted file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LintedFile {
    pub path: String,
    pub stamp: Option<Stamp>,
    pub lines: usize,
    /// The text as checked, for showing the lines of findings.
    pub text: String,
    pub findings: Vec<lint::Finding>,
}

/// The result of checking every workflow file.
#[derive(Clone, Debug, Default)]
pub struct LintRun {
    pub files: Vec<LintedFile>,
    /// Files linted again (changed or new) and files taken from the cache.
    pub linted: usize,
    pub reused: usize,
    /// How many findings actionlint added, or why it failed; `None` when
    /// it is not installed (or not asked for).
    pub actionlint: Option<std::result::Result<usize, String>>,
    pub actionlint_version: Option<String>,
    pub elapsed_ms: u64,
}

/// Lint every `.github/workflows/*.yml|yaml` file, reusing `previous`
/// results for files whose stamp did not change. Runs actionlint over all
/// files when it is installed (`commands` receives its command line).
pub fn lint_workdir(
    workdir: &Path,
    previous: &[LintedFile],
    use_actionlint: bool,
    commands: &mut Vec<String>,
) -> LintRun {
    let started = std::time::Instant::now();
    let mut run = LintRun::default();
    for (path, text) in workflow::read_dir(workdir) {
        let st = stamp(&workdir.join(&path));
        if let Some(prev) = previous
            .iter()
            .find(|p| p.path == path && p.stamp.is_some() && p.stamp == st)
        {
            let mut keep = prev.clone();
            keep.findings.retain(|f| f.source == lint::Source::Builtin);
            run.files.push(keep);
            run.reused += 1;
            continue;
        }
        run.linted += 1;
        run.files.push(LintedFile {
            findings: lint::lint(&path, &text),
            lines: text.lines().count(),
            stamp: st,
            path,
            text,
        });
    }
    if use_actionlint
        && !run.files.is_empty()
        && let Some(found) = crate::devops::tools::detect(crate::devops::tools::Tool::Actionlint)
    {
        run.actionlint_version = Some(found.version.clone());
        let files: Vec<String> = run.files.iter().map(|f| f.path.clone()).collect();
        let spec = lint::actionlint_spec(workdir, &files);
        commands.push(spec.display());
        run.actionlint = Some(
            match crate::devops::proc::run_capture(&spec, Some(Duration::from_secs(90))) {
                Err(e) => Err(e.to_string()),
                Ok(out) if out.timed_out => Err(out.error_text()),
                // 0: clean, 1: problems found; anything else is a failure.
                Ok(out) if !matches!(out.code, Some(0) | Some(1)) => Err(out.error_text()),
                Ok(out) => match lint::parse_actionlint(&out.stdout_text()) {
                    Err(e) => Err(e),
                    Ok(found) => {
                        let n = found.len();
                        for f in found {
                            if let Some(file) = run.files.iter_mut().find(|x| x.path == f.file) {
                                file.findings.push(f);
                            }
                        }
                        for file in &mut run.files {
                            file.findings.sort_by(|a, b| {
                                a.line.cmp(&b.line).then(a.severity.cmp(&b.severity))
                            });
                        }
                        Ok(n)
                    }
                },
            },
        );
    }
    run.elapsed_ms = started.elapsed().as_millis() as u64;
    run
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_from_epoch() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_793_043_531), "2026-10-26T19:38:51Z");
        let t = crate::github::parse_time("2024-02-29T23:59:59Z").unwrap();
        assert_eq!(rfc3339(t), "2024-02-29T23:59:59Z");
    }

    #[test]
    fn pr_states_query() {
        let b = pr_states_body("o", "r", &[12, 40994]);
        let q = b["query"].as_str().unwrap();
        assert!(q.contains("p12: issueOrPullRequest(number: 12) { ... on PullRequest { state } }"));
        assert!(q.contains("p40994: issueOrPullRequest(number: 40994)"));
        assert_eq!(b["variables"]["owner"], "o");
        assert_eq!(b["variables"]["name"], "r");
    }

    #[test]
    fn lint_workdir_caches_by_stamp() {
        let dir = std::env::temp_dir().join(format!("hadion-ci-lint-{}", std::process::id()));
        let wf = dir.join(".github").join("workflows");
        std::fs::create_dir_all(&wf).unwrap();
        std::fs::write(
            wf.join("a.yml"),
            "on: push\njobs:\n  a:\n    steps: [{run: x}]\n",
        )
        .unwrap();
        std::fs::write(wf.join("b.yaml"), "on: [push\n").unwrap();
        std::fs::write(wf.join("notes.txt"), "not a workflow").unwrap();
        let mut cmds = Vec::new();
        let first = lint_workdir(&dir, &[], false, &mut cmds);
        assert_eq!(first.files.len(), 2);
        assert_eq!((first.linted, first.reused), (2, 0));
        assert_eq!(first.files[0].path, ".github/workflows/a.yml");
        assert!(
            first.files[0]
                .findings
                .iter()
                .any(|f| f.rule == "runs-on-missing")
        );
        assert_eq!(first.files[1].findings[0].rule, "yaml-syntax");
        let again = lint_workdir(&dir, &first.files, false, &mut cmds);
        assert_eq!((again.linted, again.reused), (0, 2));
        assert_eq!(again.files, first.files);
        std::fs::remove_dir_all(&dir).ok();
    }
}

/// `HADION_LINT_DIR=<repo> cargo test -p hadion-core --lib lint_report -- --ignored --nocapture`
/// prints the findings for a repository's workflows.
#[cfg(test)]
#[test]
#[ignore]
fn lint_report() {
    let Ok(dir) = std::env::var("HADION_LINT_DIR") else {
        return;
    };
    let run = lint_workdir(Path::new(&dir), &[], false, &mut Vec::new());
    let mut by_rule: std::collections::BTreeMap<String, usize> = Default::default();
    for f in &run.files {
        for x in &f.findings {
            *by_rule
                .entry(format!("{:?} {}", x.severity, x.rule))
                .or_default() += 1;
            if std::env::var("HADION_LINT_ALL").is_ok() || x.severity != lint::Severity::Info {
                println!(
                    "{}:{}:{} {:?} {} — {}",
                    x.file, x.line, x.column, x.severity, x.rule, x.message
                );
            }
        }
    }
    println!("{} files in {} ms", run.files.len(), run.elapsed_ms);
    for (k, v) in by_rule {
        println!("{v:5} {k}");
    }
}
