//! CI configuration: GitHub Actions workflows parsed from
//! `.github/workflows/*.yml`, plus a note of other CI systems present.
//!
//! Findings follow the GitHub security-hardening guide: third-party actions
//! pinned to a mutable tag, `pull_request_target` with a checkout of the PR
//! head, missing or write-all `permissions`, script injection through
//! `${{ github.event.* }}` in `run:` steps, secrets handed to third-party
//! actions, self-hosted runners on fork-triggerable workflows, `curl | sh`
//! installs, no `timeout-minutes`, deprecated commands.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use serde_yaml::{Mapping, Value};

use super::{Severity, TrackedFile};
use crate::secrets::basename;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionRef {
    /// As written: `actions/checkout@v4`, `./.github/actions/x`,
    /// `docker://alpine:3`.
    pub uses: String,
    /// `owner/repo` (without the sub-path).
    pub repo: String,
    /// After the `@`.
    pub version: String,
    /// Pinned to a 40-hex commit SHA.
    pub pinned_sha: bool,
    /// `actions/*`, `github/*`: maintained by GitHub.
    pub first_party: bool,
    pub local: bool,
    pub docker: bool,
    /// Occurrences across all workflows.
    pub count: u32,
    pub workflows: Vec<String>,
}

impl ActionRef {
    /// Neither GitHub's own, nor local, nor a container image.
    pub fn third_party(&self) -> bool {
        !self.first_party && !self.local && !self.docker
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JobInfo {
    pub id: String,
    pub name: String,
    pub runs_on: Vec<String>,
    pub self_hosted: bool,
    pub steps: u32,
    pub uses: Vec<String>,
    /// `secrets.NAME` referenced anywhere in the job.
    pub secrets: Vec<String>,
    /// Rendered `permissions` (`read-all`, `write-all`, `contents: read, …`).
    pub permissions: Option<String>,
    pub timeout_minutes: Option<u32>,
    pub container: Option<String>,
    pub continue_on_error: bool,
    /// Environment (deployment) name.
    pub environment: Option<String>,
    /// A reusable workflow (`uses:` at job level).
    pub reusable: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CiFinding {
    pub severity: Severity,
    pub workflow: String,
    pub job: Option<String>,
    /// Stable id: `unpinned-action`, `pull-request-target-checkout`,
    /// `no-permissions`, `write-all`, `script-injection`, `secrets-to-third-party`,
    /// `self-hosted-public`, `curl-pipe-sh`, `no-timeout`, `deprecated-command`,
    /// `continue-on-error`, `persist-credentials`, `workflow-dispatch-inputs-in-run`,
    /// `cache-poisoning`, `artifact-secrets`.
    pub id: &'static str,
    pub title: String,
    pub detail: String,
    pub why: &'static str,
    /// Lines in the workflow file, when known.
    pub lines: Vec<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Workflow {
    pub path: String,
    pub name: String,
    /// Trigger events (`push`, `pull_request`, `schedule`, …).
    pub triggers: Vec<String>,
    /// Cron expressions of `schedule` triggers.
    pub schedules: Vec<String>,
    pub jobs: Vec<JobInfo>,
    pub permissions: Option<String>,
    pub secrets: Vec<String>,
    pub concurrency: bool,
    /// Reachable from a fork (`pull_request`, `pull_request_target`,
    /// `issue_comment`, …).
    pub fork_triggerable: bool,
    pub findings: usize,
    pub lines: u32,
    /// YAML did not parse; `detail` holds the error.
    pub parse_error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Ci {
    pub workflows: Vec<Workflow>,
    /// Distinct `uses:` targets, desc by count.
    pub actions: Vec<ActionRef>,
    /// Severity desc, then workflow.
    pub findings: Vec<CiFinding>,
    /// `(secret name, workflows)`.
    pub secrets: Vec<(String, Vec<String>)>,
    /// `(runner label, jobs)`.
    pub runners: Vec<(String, u32)>,
    /// Other CI configuration files present (GitLab, Jenkins, Travis,
    /// Azure Pipelines, CircleCI, Bitbucket, Buildkite, Drone, AppVeyor…).
    pub other_ci: Vec<(String, String)>,
    /// `.github/dependabot.yml` ecosystems, when present.
    pub dependabot: Option<Vec<String>>,
    pub renovate: bool,
    /// `.github/workflows/*.yml` files, parsed or not.
    pub workflow_files: usize,
    pub unpinned_third_party: usize,
    pub pinned_sha: usize,
}

impl Ci {
    pub fn count(&self, s: Severity) -> usize {
        self.findings.iter().filter(|f| f.severity == s).count()
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Events an outsider can fire from a fork or an issue.
const FORK_TRIGGERS: &[&str] = &[
    "pull_request",
    "pull_request_target",
    "issue_comment",
    "issues",
    "discussion",
    "discussion_comment",
    "fork",
    "watch",
    "workflow_run",
];

/// Other CI systems: `(path or directory prefix, system)`.
const OTHER_CI: &[(&str, &str)] = &[
    (".gitlab-ci.yml", "GitLab CI"),
    ("Jenkinsfile", "Jenkins"),
    (".travis.yml", "Travis CI"),
    ("azure-pipelines.yml", "Azure Pipelines"),
    ("azure-pipelines.yaml", "Azure Pipelines"),
    (".circleci/config.yml", "CircleCI"),
    ("bitbucket-pipelines.yml", "Bitbucket Pipelines"),
    (".buildkite/pipeline.yml", "Buildkite"),
    (".drone.yml", "Drone"),
    ("appveyor.yml", "AppVeyor"),
    (".appveyor.yml", "AppVeyor"),
    ("cloudbuild.yaml", "Cloud Build"),
    (".teamcity/", "TeamCity"),
    ("Earthfile", "Earthly"),
    (".woodpecker.yml", "Woodpecker"),
    ("codemagic.yaml", "Codemagic"),
];

const RENOVATE_FILES: &[&str] = &[
    "renovate.json",
    "renovate.json5",
    ".renovaterc",
    ".renovaterc.json",
    ".renovaterc.json5",
    ".github/renovate.json",
    ".github/renovate.json5",
];

const WHY_UNPINNED: &str = "A tag or branch can be moved by the action's maintainer or by an attacker who takes over the repository, so the code that runs tomorrow is not the code reviewed today; a commit SHA cannot be moved.";
const WHY_PRT_CHECKOUT: &str = "pull_request_target runs in the context of the base repository with its secrets and a write token; checking out the pull request's head executes the fork's code with that access.";
const WHY_INJECTION: &str = "The expression is expanded into the shell script before it runs; a crafted issue title, comment or branch name injects commands. Pass the value through an environment variable instead.";
const WHY_NO_PERMISSIONS: &str = "Without a permissions block the GITHUB_TOKEN gets the repository default, which is read/write on older repositories; least privilege needs an explicit block (ideally `contents: read`).";
const WHY_WRITE_ALL: &str = "write-all grants the token every permission, including contents, packages and pull requests; a compromised step or action can push code or publish releases.";
const WHY_SECRETS_THIRD_PARTY: &str = "A third-party action receives the secret in plain text and can forward it anywhere; pin such actions to a SHA and give them the narrowest secret possible.";
const WHY_SELF_HOSTED: &str = "Forks and issue comments can start this workflow, so outside code runs on your own infrastructure, where it can persist, read other jobs' files and reach the internal network.";
const WHY_CURL_SH: &str = "Piping a download into a shell runs whatever the server (or a man in the middle) sends, without a checksum or a review; download, verify, then run.";
const WHY_NO_TIMEOUT: &str = "The default timeout is 6 hours; a hung job burns runner minutes and blocks its concurrency group until then.";
const WHY_DEPRECATED: &str = "Deprecated workflow commands and action versions stop working when GitHub removes them, and the old Node runtimes no longer receive security fixes.";
const WHY_CONTINUE_ON_ERROR: &str = "A job that always reports success hides real failures from branch protection and from the people watching the checks.";
const WHY_PERSIST_CREDENTIALS: &str = "actions/checkout stores the token in .git/config; in a workflow an outsider can trigger, later steps (or checked-out code) can read and use it.";
const WHY_CACHE_POISONING: &str = "A cache restored in a privileged workflow can be seeded by a pull request that shares the cache key, so attacker-controlled files reach the trusted job.";

// ---------------------------------------------------------------------------
// YAML helpers
// ---------------------------------------------------------------------------

fn untag(v: &Value) -> &Value {
    match v {
        Value::Tagged(t) => untag(&t.value),
        other => other,
    }
}

fn get<'a>(m: &'a Mapping, key: &str) -> Option<&'a Value> {
    m.get(key).map(untag)
}

/// `on` is a YAML 1.1 boolean; accept both spellings.
fn get_on(m: &Mapping) -> Option<&Value> {
    get(m, "on").or_else(|| m.get(Value::Bool(true)).map(untag))
}

fn scalar(v: &Value) -> Option<String> {
    match untag(v) {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn mapping(v: &Value) -> Option<&Mapping> {
    untag(v).as_mapping()
}

/// A string, a sequence of strings, or the keys of a mapping.
fn str_list(v: &Value) -> Vec<String> {
    match untag(v) {
        Value::Sequence(seq) => seq.iter().filter_map(scalar).collect(),
        Value::Mapping(m) => m.keys().filter_map(scalar).collect(),
        other => scalar(other).into_iter().collect(),
    }
}

fn render_permissions(v: &Value) -> Option<String> {
    match untag(v) {
        Value::Mapping(m) => {
            if m.is_empty() {
                return Some("{}".into());
            }
            Some(
                m.iter()
                    .map(|(k, v)| {
                        format!(
                            "{}: {}",
                            scalar(k).unwrap_or_default(),
                            scalar(v).unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        }
        other => scalar(other),
    }
}

/// Every scalar value below `v` (keys excluded).
fn collect_strings(v: &Value, out: &mut Vec<String>) {
    match untag(v) {
        Value::Sequence(seq) => seq.iter().for_each(|x| collect_strings(x, out)),
        Value::Mapping(m) => m.values().for_each(|x| collect_strings(x, out)),
        other => {
            if let Some(s) = scalar(other) {
                out.push(s);
            }
        }
    }
}

fn is_true(v: &Value) -> bool {
    match untag(v) {
        Value::Bool(b) => *b,
        Value::String(s) => s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn is_false(v: &Value) -> bool {
    match untag(v) {
        Value::Bool(b) => !*b,
        Value::String(s) => s.eq_ignore_ascii_case("false"),
        _ => false,
    }
}

/// 1-based lines of `text` containing `needle` (at most 20).
fn lines_containing(text: &str, needle: &str) -> Vec<u32> {
    if needle.is_empty() {
        return Vec::new();
    }
    text.lines()
        .enumerate()
        .filter(|(_, l)| l.contains(needle))
        .map(|(i, _)| i as u32 + 1)
        .take(20)
        .collect()
}

fn secrets_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"secrets\.([A-Za-z_][A-Za-z0-9_]*)").expect("secrets regex"))
}

fn secrets_inherit_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^\s*secrets:\s*inherit\s*$").expect("inherit regex"))
}

fn injection_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"\$\{\{\s*((?:github\.event\.(?:issue\.title|issue\.body|pull_request\.title|pull_request\.body|comment\.body|review\.body|review_comment\.body|pages\.[A-Za-z0-9_.*\[\]-]*\.page_name|commits\.[A-Za-z0-9_.*\[\]-]*\.message|head_commit\.message|head_commit\.author\.email|head_commit\.author\.name|pull_request\.head\.ref|pull_request\.head\.label|pull_request\.head\.repo\.default_branch|discussion\.title|discussion\.body|inputs\.[A-Za-z0-9_-]+))|github\.head_ref|inputs\.[A-Za-z0-9_-]+)\s*\}\}",
        )
        .expect("injection regex")
    })
}

fn curl_pipe_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:curl|wget)[^|\n]*\|\s*(?:sudo\s+)?(?:ba|z|da)?sh\b").expect("curl regex")
    })
}

fn deprecated_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"::(?:set-output|save-state|set-env|add-path)|actions/(?:upload-artifact|download-artifact|checkout|setup-node|cache|setup-python)@v[12]\b|ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION|using:\s*node1[26]\b",
        )
        .expect("deprecated regex")
    })
}

fn secret_names(text: &str) -> Vec<String> {
    let mut set: BTreeSet<String> = secrets_regex()
        .captures_iter(text)
        .map(|c| c[1].to_owned())
        .collect();
    if secrets_inherit_regex().is_match(text) {
        set.insert("inherit".into());
    }
    set.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Action references
// ---------------------------------------------------------------------------

/// `(repo, version, pinned_sha, first_party, local, docker)` of a `uses:` value.
fn parse_uses(uses: &str) -> (String, String, bool, bool, bool, bool) {
    let local = uses.starts_with("./") || uses.starts_with("../");
    let docker = uses.starts_with("docker://");
    if local || docker {
        return (uses.to_owned(), String::new(), false, false, local, docker);
    }
    let (spec, version) = uses.rsplit_once('@').unwrap_or((uses, ""));
    let repo = spec.split('/').take(2).collect::<Vec<_>>().join("/");
    let pinned = version.len() == 40 && version.bytes().all(|b| b.is_ascii_hexdigit());
    let first_party = repo.starts_with("actions/") || repo.starts_with("github/");
    (repo, version.to_owned(), pinned, first_party, false, false)
}

fn action_ref(uses: &str) -> ActionRef {
    let (repo, version, pinned_sha, first_party, local, docker) = parse_uses(uses);
    ActionRef {
        uses: uses.to_owned(),
        repo,
        version,
        pinned_sha,
        first_party,
        local,
        docker,
        count: 0,
        workflows: Vec::new(),
    }
}

fn is_checkout(uses: &str) -> bool {
    uses.starts_with("actions/checkout@") || uses == "actions/checkout"
}

fn is_cache_restore(uses: &str) -> bool {
    uses.starts_with("actions/cache@") || uses.starts_with("actions/cache/restore@")
}

// ---------------------------------------------------------------------------
// One workflow
// ---------------------------------------------------------------------------

/// Everything derived from one workflow file.
struct Analysis {
    workflow: Workflow,
    findings: Vec<CiFinding>,
    /// Every `uses:` occurrence (steps and reusable jobs).
    uses: Vec<String>,
}

/// A step as far as the findings care.
struct Step {
    uses: Option<String>,
    run: Option<String>,
    /// `with.script` of `actions/github-script`.
    script: Option<String>,
    /// Scalars under `with` and `env`.
    inputs: Vec<String>,
    /// `with.ref`.
    checkout_ref: Option<String>,
    persist_credentials_false: bool,
}

fn parse_step(v: &Value) -> Step {
    let m = mapping(v);
    let field = |k: &str| m.and_then(|m| get(m, k));
    let uses = field("uses").and_then(scalar);
    let with = field("with").and_then(mapping);
    let mut inputs = Vec::new();
    if let Some(w) = field("with") {
        collect_strings(w, &mut inputs);
    }
    if let Some(e) = field("env") {
        collect_strings(e, &mut inputs);
    }
    let script = match &uses {
        Some(u) if u.starts_with("actions/github-script") => {
            with.and_then(|w| get(w, "script")).and_then(scalar)
        }
        _ => None,
    };
    Step {
        run: field("run").and_then(scalar),
        script,
        inputs,
        checkout_ref: with.and_then(|w| get(w, "ref")).and_then(scalar),
        persist_credentials_false: with
            .and_then(|w| get(w, "persist-credentials"))
            .is_some_and(is_false),
        uses,
    }
}

/// `runs-on`: string, sequence, or mapping with `labels` / `group`.
fn runs_on(v: Option<&Value>) -> (Vec<String>, bool) {
    let Some(v) = v else {
        return (Vec::new(), false);
    };
    match untag(v) {
        Value::Mapping(m) => {
            let mut labels: Vec<String> = get(m, "labels").map(str_list).unwrap_or_default();
            let group = get(m, "group").and_then(scalar);
            if let Some(g) = &group {
                labels.push(format!("group:{g}"));
            }
            let self_hosted = group.is_some() || labels.iter().any(|l| l == "self-hosted");
            (labels, self_hosted)
        }
        other => {
            let labels = str_list(other);
            let self_hosted = labels.iter().any(|l| l == "self-hosted");
            (labels, self_hosted)
        }
    }
}

fn file_stem(path: &str) -> String {
    let base = basename(path);
    match base.rfind('.') {
        Some(i) if i > 0 => base[..i].to_owned(),
        _ => base.to_owned(),
    }
}

fn join_names(names: &[String], max: usize) -> String {
    if names.len() <= max {
        names.join(", ")
    } else {
        format!("{} and {} more", names[..max].join(", "), names.len() - max)
    }
}

fn analyze_workflow(path: &str, text: &str) -> Analysis {
    let mut wf = Workflow {
        path: path.to_owned(),
        name: file_stem(path),
        lines: text.lines().count() as u32,
        secrets: secret_names(text),
        ..Workflow::default()
    };
    let mut findings: Vec<CiFinding> = Vec::new();
    let mut uses_all: Vec<String> = Vec::new();

    let root = match serde_yaml::from_str::<Value>(text) {
        Ok(v) => v,
        Err(e) => {
            wf.parse_error = Some(e.to_string());
            return Analysis {
                workflow: wf,
                findings,
                uses: uses_all,
            };
        }
    };
    let Some(root) = mapping(&root) else {
        wf.parse_error = Some("the document is not a mapping".into());
        return Analysis {
            workflow: wf,
            findings,
            uses: uses_all,
        };
    };

    if let Some(n) = get(root, "name").and_then(scalar) {
        wf.name = n;
    }
    if let Some(on) = get_on(root) {
        wf.triggers = str_list(on);
        if let Some(sched) = mapping(on).and_then(|m| get(m, "schedule")) {
            match untag(sched) {
                Value::Sequence(seq) => {
                    for item in seq {
                        if let Some(c) = mapping(item).and_then(|m| get(m, "cron")).and_then(scalar)
                        {
                            wf.schedules.push(c);
                        }
                    }
                }
                other => {
                    if let Some(c) = mapping(other).and_then(|m| get(m, "cron")).and_then(scalar) {
                        wf.schedules.push(c);
                    }
                }
            }
        }
    }
    wf.fork_triggerable = wf
        .triggers
        .iter()
        .any(|t| FORK_TRIGGERS.contains(&t.as_str()));
    let prt = wf.triggers.iter().any(|t| t == "pull_request_target");
    let workflow_run = wf.triggers.iter().any(|t| t == "workflow_run");
    wf.permissions = get(root, "permissions").and_then(render_permissions);
    wf.concurrency = get(root, "concurrency").is_some();

    let name = wf.name.clone();
    let mut push = |severity: Severity,
                    job: Option<&str>,
                    id: &'static str,
                    title: String,
                    detail: String,
                    why: &'static str,
                    lines: Vec<u32>| {
        findings.push(CiFinding {
            severity,
            workflow: name.clone(),
            job: job.map(str::to_owned),
            id,
            title,
            detail,
            why,
            lines,
        });
    };

    if wf.permissions.as_deref() == Some("write-all") {
        push(
            Severity::Medium,
            None,
            "write-all",
            "Workflow permissions are write-all".into(),
            "The workflow-level `permissions: write-all` applies to every job.".into(),
            WHY_WRITE_ALL,
            lines_containing(text, "write-all"),
        );
    }

    // Per-workflow accumulators for the one-per-workflow findings.
    let mut jobs_without_permissions: Vec<String> = Vec::new();
    let mut jobs_without_timeout: Vec<String> = Vec::new();
    let mut unpinned: BTreeMap<String, (ActionRef, Vec<String>)> = BTreeMap::new();
    let mut deprecated: BTreeSet<String> = BTreeSet::new();
    let mut checkout_without_persist: Vec<String> = Vec::new();

    if let Some(jobs) = get(root, "jobs").and_then(mapping) {
        for (jid, jv) in jobs {
            let Some(jid) = scalar(jid) else {
                continue;
            };
            let Some(jm) = mapping(jv) else {
                continue;
            };
            let mut job = JobInfo {
                id: jid.clone(),
                name: get(jm, "name")
                    .and_then(scalar)
                    .unwrap_or_else(|| jid.clone()),
                permissions: get(jm, "permissions").and_then(render_permissions),
                timeout_minutes: get(jm, "timeout-minutes")
                    .and_then(|v| untag(v).as_u64())
                    .map(|n| n as u32),
                container: get(jm, "container").and_then(|c| match untag(c) {
                    Value::Mapping(m) => get(m, "image").and_then(scalar),
                    other => scalar(other),
                }),
                continue_on_error: get(jm, "continue-on-error").is_some_and(is_true),
                environment: get(jm, "environment").and_then(|e| match untag(e) {
                    Value::Mapping(m) => get(m, "name").and_then(scalar),
                    other => scalar(other),
                }),
                reusable: get(jm, "uses").and_then(scalar),
                ..JobInfo::default()
            };
            let (labels, self_hosted) = runs_on(get(jm, "runs-on"));
            job.runs_on = labels;
            job.self_hosted = self_hosted;
            let mut job_strings = Vec::new();
            collect_strings(jv, &mut job_strings);
            let joined = job_strings.join("\n");
            job.secrets = secret_names(&joined);

            let steps: Vec<Step> = get(jm, "steps")
                .and_then(|s| untag(s).as_sequence())
                .map(|seq| seq.iter().map(parse_step).collect())
                .unwrap_or_default();
            job.steps = steps.len() as u32;

            if let Some(reusable) = &job.reusable {
                uses_all.push(reusable.clone());
                let r = action_ref(reusable);
                if !r.local && !r.docker && !r.pinned_sha {
                    unpinned
                        .entry(reusable.clone())
                        .or_insert_with(|| (r, Vec::new()))
                        .1
                        .push(jid.clone());
                }
            }

            if job.permissions.as_deref() == Some("write-all") {
                push(
                    Severity::Medium,
                    Some(&jid),
                    "write-all",
                    format!("Job `{jid}` has permissions: write-all"),
                    "The job-level `permissions: write-all` grants the token every scope.".into(),
                    WHY_WRITE_ALL,
                    lines_containing(text, "write-all"),
                );
            }
            if job.reusable.is_none() {
                if job.permissions.is_none() {
                    jobs_without_permissions.push(jid.clone());
                }
                if job.timeout_minutes.is_none() {
                    jobs_without_timeout.push(jid.clone());
                }
            }
            if job.continue_on_error {
                push(
                    Severity::Low,
                    Some(&jid),
                    "continue-on-error",
                    format!("Job `{jid}` continues on error"),
                    "The job is marked `continue-on-error: true`, so its failures never fail the workflow.".into(),
                    WHY_CONTINUE_ON_ERROR,
                    lines_containing(text, "continue-on-error"),
                );
            }
            if job.self_hosted && wf.fork_triggerable {
                let outside: Vec<&str> = wf
                    .triggers
                    .iter()
                    .map(String::as_str)
                    .filter(|t| FORK_TRIGGERS.contains(t))
                    .collect();
                push(
                    if prt {
                        Severity::High
                    } else {
                        Severity::Medium
                    },
                    Some(&jid),
                    "self-hosted-public",
                    format!(
                        "Job `{jid}` runs on a self-hosted runner in a fork-triggerable workflow"
                    ),
                    format!(
                        "runs-on: {}; triggers reachable from outside: {}.",
                        job.runs_on.join(", "),
                        outside.join(", ")
                    ),
                    WHY_SELF_HOSTED,
                    lines_containing(text, "self-hosted"),
                );
            }

            for step in &steps {
                if let Some(uses) = &step.uses {
                    job.uses.push(uses.clone());
                    uses_all.push(uses.clone());
                    let r = action_ref(uses);
                    if !r.local && !r.docker && !r.pinned_sha {
                        unpinned
                            .entry(uses.clone())
                            .or_insert_with(|| (r.clone(), Vec::new()))
                            .1
                            .push(jid.clone());
                    }
                    if r.third_party() {
                        let mut names: BTreeSet<String> = BTreeSet::new();
                        for input in &step.inputs {
                            if input.contains("${{") && input.contains("secrets.") {
                                names.extend(secret_names(input));
                            }
                        }
                        if !names.is_empty() {
                            let names: Vec<String> = names.into_iter().collect();
                            push(
                                Severity::Medium,
                                Some(&jid),
                                "secrets-to-third-party",
                                format!("Secrets passed to third-party action {}", r.repo),
                                format!(
                                    "`{uses}` receives {} through `with`/`env`.",
                                    names.join(", ")
                                ),
                                WHY_SECRETS_THIRD_PARTY,
                                lines_containing(text, uses),
                            );
                        }
                    }
                    if is_checkout(uses) {
                        if prt
                            && let Some(r) = &step.checkout_ref
                            && (r.contains("github.event.pull_request.head")
                                || r.contains("github.head_ref"))
                        {
                            push(
                                Severity::High,
                                Some(&jid),
                                "pull-request-target-checkout",
                                format!(
                                    "Job `{jid}` checks out the pull request head under pull_request_target"
                                ),
                                format!(
                                    "actions/checkout with `ref: {}` in a workflow triggered by pull_request_target.",
                                    r.trim()
                                ),
                                WHY_PRT_CHECKOUT,
                                lines_containing(text, "github.event.pull_request.head")
                                    .into_iter()
                                    .chain(lines_containing(text, "github.head_ref"))
                                    .collect(),
                            );
                        }
                        if (prt || workflow_run) && !step.persist_credentials_false {
                            checkout_without_persist.push(jid.clone());
                        }
                    }
                    if prt && is_cache_restore(uses) {
                        push(
                            Severity::Low,
                            Some(&jid),
                            "cache-poisoning",
                            format!("Job `{jid}` restores a cache under pull_request_target"),
                            format!(
                                "`{uses}` restores files that a pull request sharing the cache key could have written."
                            ),
                            WHY_CACHE_POISONING,
                            lines_containing(text, uses),
                        );
                    }
                }
                let scripts: Vec<&String> = step.run.iter().chain(step.script.iter()).collect();
                for script in scripts {
                    let mut exprs: BTreeSet<String> = BTreeSet::new();
                    for cap in injection_regex().captures_iter(script) {
                        exprs.insert(cap[1].trim().to_owned());
                    }
                    if !exprs.is_empty() {
                        let only_inputs = exprs.iter().all(|e| e.contains("inputs."));
                        let mut lines = Vec::new();
                        for e in &exprs {
                            lines.extend(lines_containing(text, e));
                        }
                        lines.sort_unstable();
                        lines.dedup();
                        let exprs: Vec<String> = exprs.into_iter().collect();
                        push(
                            if only_inputs {
                                Severity::Medium
                            } else {
                                Severity::High
                            },
                            Some(&jid),
                            "script-injection",
                            format!("Untrusted input expanded into a script in job `{jid}`"),
                            format!(
                                "`${{{{ {} }}}}` is interpolated into a `run:` script.",
                                exprs.join(" }}`, `${{ ")
                            ),
                            WHY_INJECTION,
                            lines,
                        );
                    }
                    if let Some(m) = curl_pipe_regex().find(script) {
                        let snippet: String = m.as_str().chars().take(120).collect();
                        push(
                            Severity::Medium,
                            Some(&jid),
                            "curl-pipe-sh",
                            format!("Download piped into a shell in job `{jid}`"),
                            format!("`{snippet}`"),
                            WHY_CURL_SH,
                            text.lines()
                                .enumerate()
                                .filter(|(_, l)| curl_pipe_regex().is_match(l))
                                .map(|(i, _)| i as u32 + 1)
                                .take(20)
                                .collect(),
                        );
                    }
                    for m in deprecated_regex().find_iter(script) {
                        deprecated.insert(m.as_str().to_owned());
                    }
                }
                if let Some(uses) = &step.uses
                    && deprecated_regex().is_match(uses)
                {
                    deprecated.insert(uses.clone());
                }
            }
            wf.jobs.push(job);
        }
    }
    for m in deprecated_regex().find_iter(text) {
        let s = m.as_str();
        if s.contains("NODE_VERSION") || s.starts_with("using:") {
            deprecated.insert(s.to_owned());
        }
    }

    for (uses, (r, jobs)) in &unpinned {
        let what = if r.version.is_empty() {
            "no version at all".to_owned()
        } else {
            format!("the mutable ref `{}`", r.version)
        };
        let mut jobs = jobs.clone();
        jobs.dedup();
        push(
            if r.first_party {
                Severity::Low
            } else {
                Severity::Medium
            },
            None,
            "unpinned-action",
            format!("Unpinned action {}", r.repo),
            format!(
                "`{uses}` is referenced by {what} in job{} {}; pin it to a commit SHA (`{}@<sha> # {}`).",
                if jobs.len() == 1 { "" } else { "s" },
                join_names(&jobs, 5),
                r.repo,
                if r.version.is_empty() {
                    "version"
                } else {
                    &r.version
                }
            ),
            WHY_UNPINNED,
            lines_containing(text, uses),
        );
    }
    if wf.permissions.is_none() && !jobs_without_permissions.is_empty() {
        push(
            Severity::Low,
            None,
            "no-permissions",
            "No permissions block".into(),
            format!(
                "Neither the workflow nor job{} {} declares `permissions`, so the token gets the repository default.",
                if jobs_without_permissions.len() == 1 {
                    ""
                } else {
                    "s"
                },
                join_names(&jobs_without_permissions, 8)
            ),
            WHY_NO_PERMISSIONS,
            Vec::new(),
        );
    }
    if !jobs_without_timeout.is_empty() {
        push(
            Severity::Low,
            None,
            "no-timeout",
            "Jobs without timeout-minutes".into(),
            format!(
                "Job{} {} run{} with the 6-hour default.",
                if jobs_without_timeout.len() == 1 {
                    ""
                } else {
                    "s"
                },
                join_names(&jobs_without_timeout, 8),
                if jobs_without_timeout.len() == 1 {
                    "s"
                } else {
                    ""
                }
            ),
            WHY_NO_TIMEOUT,
            Vec::new(),
        );
    }
    if !deprecated.is_empty() {
        let items: Vec<String> = deprecated.into_iter().collect();
        let mut lines = Vec::new();
        for item in &items {
            lines.extend(lines_containing(text, item));
        }
        lines.sort_unstable();
        lines.dedup();
        push(
            Severity::Low,
            None,
            "deprecated-command",
            "Deprecated commands or action versions".into(),
            format!("Found: {}.", join_names(&items, 8)),
            WHY_DEPRECATED,
            lines,
        );
    }
    if !checkout_without_persist.is_empty() {
        checkout_without_persist.dedup();
        push(
            Severity::Low,
            None,
            "persist-credentials",
            "Checkout keeps the token in a privileged workflow".into(),
            format!(
                "actions/checkout in job{} {} runs without `persist-credentials: false` while the workflow is triggered by {}.",
                if checkout_without_persist.len() == 1 {
                    ""
                } else {
                    "s"
                },
                join_names(&checkout_without_persist, 8),
                if prt {
                    "pull_request_target"
                } else {
                    "workflow_run"
                }
            ),
            WHY_PERSIST_CREDENTIALS,
            lines_containing(text, "actions/checkout"),
        );
    }

    wf.findings = findings.len();
    Analysis {
        workflow: wf,
        findings,
        uses: uses_all,
    }
}

// ---------------------------------------------------------------------------
// The repository
// ---------------------------------------------------------------------------

fn is_workflow_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix(".github/workflows/") else {
        return false;
    };
    !rest.contains('/') && (rest.ends_with(".yml") || rest.ends_with(".yaml"))
}

fn parse_dependabot(text: &str) -> Vec<String> {
    let Ok(root) = serde_yaml::from_str::<Value>(text) else {
        return Vec::new();
    };
    let mut out: Vec<String> = mapping(&root)
        .and_then(|m| get(m, "updates"))
        .and_then(|u| untag(u).as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|u| {
                    mapping(u)
                        .and_then(|m| get(m, "package-ecosystem"))
                        .and_then(scalar)
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out.dedup();
    out
}

/// Parse the workflows (serde_yaml) and derive the findings. Never fails.
pub fn analyze(workdir: &Path, files: &[TrackedFile]) -> Ci {
    let mut ci = Ci::default();
    let mut actions: HashMap<String, ActionRef> = HashMap::new();
    let mut secrets: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut runners: HashMap<String, u32> = HashMap::new();

    for f in files {
        let path = f.path.as_str();
        if is_workflow_path(path) {
            ci.workflow_files += 1;
            let analysis = match std::fs::read_to_string(workdir.join(path)) {
                Ok(text) => analyze_workflow(path, &text),
                Err(e) => Analysis {
                    workflow: Workflow {
                        path: path.to_owned(),
                        name: file_stem(path),
                        parse_error: Some(format!("could not read the file: {e}")),
                        ..Workflow::default()
                    },
                    findings: Vec::new(),
                    uses: Vec::new(),
                },
            };
            let wf = analysis.workflow;
            for uses in &analysis.uses {
                let entry = actions
                    .entry(uses.clone())
                    .or_insert_with(|| action_ref(uses));
                entry.count += 1;
                if !entry.workflows.contains(&wf.name) {
                    entry.workflows.push(wf.name.clone());
                }
            }
            for s in &wf.secrets {
                secrets
                    .entry(s.clone())
                    .or_default()
                    .insert(wf.name.clone());
            }
            for job in &wf.jobs {
                for label in &job.runs_on {
                    *runners.entry(label.clone()).or_default() += 1;
                }
            }
            ci.findings.extend(analysis.findings);
            ci.workflows.push(wf);
            continue;
        }
        for (pattern, system) in OTHER_CI {
            let hit = if let Some(dir) = pattern.strip_suffix('/') {
                path.starts_with(pattern) || path == dir
            } else {
                path == *pattern
            };
            if hit {
                ci.other_ci.push((path.to_owned(), (*system).to_owned()));
                break;
            }
        }
        if path == ".github/dependabot.yml" || path == ".github/dependabot.yaml" {
            let text = std::fs::read_to_string(workdir.join(path)).unwrap_or_default();
            ci.dependabot = Some(parse_dependabot(&text));
        }
        if RENOVATE_FILES.contains(&path) {
            ci.renovate = true;
        }
    }

    let mut actions: Vec<ActionRef> = actions.into_values().collect();
    actions.sort_by(|a, b| b.count.cmp(&a.count).then(a.uses.cmp(&b.uses)));
    ci.unpinned_third_party = actions
        .iter()
        .filter(|a| a.third_party() && !a.pinned_sha)
        .count();
    ci.pinned_sha = actions.iter().filter(|a| a.pinned_sha).count();
    ci.actions = actions;
    ci.secrets = secrets
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().collect()))
        .collect();
    let mut runners: Vec<(String, u32)> = runners.into_iter().collect();
    runners.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ci.runners = runners;
    ci.other_ci.sort();
    ci.findings.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then(a.workflow.cmp(&b.workflow))
            .then(a.id.cmp(b.id))
            .then(a.job.cmp(&b.job))
    });
    ci
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"name: Risky CI
on:
  push:
    branches: [main]
  pull_request_target:
    types: [opened]
  schedule:
    - cron: "0 3 * * 1"
concurrency: ci-${{ github.ref }}
jobs:
  build:
    name: Build it
    runs-on: [self-hosted, linux]
    timeout-minutes: 30
    steps:
      - uses: actions/checkout@v4
        with:
          ref: ${{ github.event.pull_request.head.sha }}
      - uses: actions/setup-node@b39b52d1213e96004bfcb1c61a8a6fa8ab84f3e8 # v4
      - uses: some-org/cool-action@v1
        with:
          token: ${{ secrets.NPM_TOKEN }}
      - name: Greet
        run: echo "Title is ${{ github.event.issue.title }}"
      - name: Install
        run: curl -sSL https://example.com/install.sh | sudo bash
      - name: Old
        run: echo "::set-output name=x::y"
  deploy:
    runs-on: ubuntu-latest
    continue-on-error: true
    environment:
      name: production
    container:
      image: node:20
    steps:
      - uses: actions/cache@v3
      - run: echo ${{ secrets.DEPLOY_KEY }} > key
  call:
    uses: other/repo/.github/workflows/reuse.yml@main
    secrets: inherit
"#;

    #[test]
    fn parses_the_sample_workflow() {
        let a = analyze_workflow(".github/workflows/risky.yml", SAMPLE);
        let wf = &a.workflow;
        assert_eq!(wf.name, "Risky CI");
        assert!(wf.parse_error.is_none());
        assert_eq!(wf.triggers, ["push", "pull_request_target", "schedule"]);
        assert_eq!(wf.schedules, ["0 3 * * 1"]);
        assert!(wf.concurrency);
        assert!(wf.fork_triggerable);
        assert_eq!(wf.permissions, None);
        assert_eq!(wf.secrets, ["DEPLOY_KEY", "NPM_TOKEN", "inherit"]);
        assert_eq!(wf.jobs.len(), 3);
        let build = &wf.jobs[0];
        assert_eq!(build.name, "Build it");
        assert_eq!(build.runs_on, ["self-hosted", "linux"]);
        assert!(build.self_hosted);
        assert_eq!(build.steps, 6);
        assert_eq!(build.timeout_minutes, Some(30));
        assert_eq!(build.uses.len(), 3);
        assert_eq!(build.secrets, ["NPM_TOKEN"]);
        let deploy = &wf.jobs[1];
        assert!(deploy.continue_on_error);
        assert_eq!(deploy.environment.as_deref(), Some("production"));
        assert_eq!(deploy.container.as_deref(), Some("node:20"));
        assert_eq!(deploy.timeout_minutes, None);
        let call = &wf.jobs[2];
        assert_eq!(
            call.reusable.as_deref(),
            Some("other/repo/.github/workflows/reuse.yml@main")
        );
        assert_eq!(a.uses.len(), 5);
        assert_eq!(wf.findings, a.findings.len());
    }

    #[test]
    fn derives_the_findings() {
        let a = analyze_workflow(".github/workflows/risky.yml", SAMPLE);
        let ids: Vec<&str> = a.findings.iter().map(|f| f.id).collect();
        for id in [
            "unpinned-action",
            "pull-request-target-checkout",
            "script-injection",
            "no-permissions",
            "curl-pipe-sh",
            "self-hosted-public",
            "secrets-to-third-party",
            "no-timeout",
            "deprecated-command",
            "continue-on-error",
            "persist-credentials",
            "cache-poisoning",
        ] {
            assert!(ids.contains(&id), "missing {id} in {ids:?}");
        }
        let find = |id: &str| a.findings.iter().find(|f| f.id == id).unwrap();
        let unpinned: Vec<&CiFinding> = a
            .findings
            .iter()
            .filter(|f| f.id == "unpinned-action")
            .collect();
        // checkout@v4 (Low, first party), cool-action@v1 (Medium), actions/cache@v3 (Low),
        // the reusable workflow @main (Medium); setup-node is SHA-pinned.
        assert_eq!(unpinned.len(), 4, "{unpinned:?}");
        let cool = unpinned
            .iter()
            .find(|f| f.title.contains("some-org/cool-action"))
            .unwrap();
        assert_eq!(cool.severity, Severity::Medium);
        assert_eq!(cool.lines, [20]);
        let checkout = unpinned
            .iter()
            .find(|f| f.title.contains("actions/checkout"))
            .unwrap();
        assert_eq!(checkout.severity, Severity::Low);
        let prt = find("pull-request-target-checkout");
        assert_eq!(prt.severity, Severity::High);
        assert_eq!(prt.job.as_deref(), Some("build"));
        assert_eq!(prt.lines, [18]);
        let inj = find("script-injection");
        assert_eq!(inj.severity, Severity::High);
        assert!(
            inj.detail.contains("github.event.issue.title"),
            "{}",
            inj.detail
        );
        assert_eq!(inj.lines, [24]);
        assert_eq!(find("curl-pipe-sh").lines, [26]);
        assert_eq!(find("self-hosted-public").severity, Severity::High);
        let secrets = find("secrets-to-third-party");
        assert!(secrets.detail.contains("NPM_TOKEN"));
        assert!(find("no-timeout").detail.contains("deploy"));
        assert!(!find("no-timeout").detail.contains("build"));
        assert!(find("deprecated-command").detail.contains("::set-output"));
        assert_eq!(find("no-permissions").severity, Severity::Low);
    }

    #[test]
    fn handles_on_as_boolean_and_bad_yaml() {
        let a = analyze_workflow(
            ".github/workflows/x.yml",
            "true:\n  - push\n  - pull_request\njobs:\n  a:\n    runs-on: ubuntu-latest\n    permissions:\n      contents: read\n    steps: []\n",
        );
        assert_eq!(a.workflow.name, "x");
        assert_eq!(a.workflow.triggers, ["push", "pull_request"]);
        assert!(a.workflow.fork_triggerable);
        assert_eq!(
            a.workflow.jobs[0].permissions.as_deref(),
            Some("contents: read")
        );
        assert!(!a.findings.iter().any(|f| f.id == "no-permissions"));

        let b = analyze_workflow(".github/workflows/bad.yml", "on: [push\njobs: {");
        assert!(b.workflow.parse_error.is_some());
        assert_eq!(b.workflow.lines, 2);
    }

    #[test]
    fn write_all_and_inputs_are_graded() {
        let a = analyze_workflow(
            ".github/workflows/w.yml",
            "on: workflow_dispatch\npermissions: write-all\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ${{ github.event.inputs.name }}\n",
        );
        let ids: Vec<&str> = a.findings.iter().map(|f| f.id).collect();
        assert!(ids.contains(&"write-all"));
        let inj = a
            .findings
            .iter()
            .find(|f| f.id == "script-injection")
            .unwrap();
        assert_eq!(inj.severity, Severity::Medium);
        assert!(!a.workflow.fork_triggerable);
    }

    #[test]
    fn action_refs() {
        let r = action_ref("actions/checkout@v4");
        assert_eq!(r.repo, "actions/checkout");
        assert_eq!(r.version, "v4");
        assert!(r.first_party && !r.pinned_sha && !r.third_party());
        let r = action_ref("owner/repo/sub/path@0123456789abcdef0123456789abcdef01234567");
        assert_eq!(r.repo, "owner/repo");
        assert!(r.pinned_sha && r.third_party());
        let r = action_ref("./.github/actions/local");
        assert!(r.local && !r.third_party());
        let r = action_ref("docker://alpine:3.19");
        assert!(r.docker);
        assert!(is_workflow_path(".github/workflows/ci.yaml"));
        assert!(!is_workflow_path(".github/workflows/sub/ci.yml"));
        assert!(!is_workflow_path(".github/dependabot.yml"));
    }

    #[test]
    fn dependabot_ecosystems() {
        let eco = parse_dependabot(
            "version: 2\nupdates:\n  - package-ecosystem: npm\n    directory: /\n  - package-ecosystem: github-actions\n    directory: /\n",
        );
        assert_eq!(eco, ["github-actions", "npm"]);
    }
}
