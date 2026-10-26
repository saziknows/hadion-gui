//! Azure DevOps through `az` (the azure-devops extension) with
//! `--org https://dev.azure.com/ORG --project P -o json`.
//!
//! `az` has no standard-input option for these commands, so every value is
//! an argument: titles and descriptions are fine, secrets are not (the run
//! form says so and offers no secret variables). Multi-line descriptions are
//! passed as one value per line (`--description` takes a list), because
//! `az.cmd` is a batch file and Windows cannot pass a newline to one.

use serde::Deserialize;
use serde_json::Value;

use super::ForgeRemote;
use super::cli::{Call, Program};
use super::model::{
    ChangeRequest, Job, Person, Pipeline, Reviewer, azure_outcome, short_ref, span, time, words,
};
use crate::{Error, Result};

/// Organisation, project and repository of a remote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// `https://dev.azure.com/ORG`.
    pub org_url: String,
    pub project: String,
    pub repo: String,
}

impl Target {
    pub fn of(remote: &ForgeRemote) -> Option<Self> {
        Some(Self {
            org_url: format!("https://dev.azure.com/{}", remote.org.as_deref()?),
            project: remote.project.clone()?,
            repo: remote.repo.clone(),
        })
    }

    fn web(&self) -> String {
        format!("{}/{}", self.org_url, self.project.replace(' ', "%20"))
    }

    pub fn pr_url(&self, id: u64) -> String {
        format!(
            "{}/_git/{}/pullrequest/{id}",
            self.web(),
            self.repo.replace(' ', "%20")
        )
    }

    pub fn build_url(&self, id: u64) -> String {
        format!("{}/_build/results?buildId={id}", self.web())
    }

    pub fn pipelines_url(&self) -> String {
        format!("{}/_build", self.web())
    }

    pub fn prs_url(&self) -> String {
        format!(
            "{}/_git/{}/pullrequests",
            self.web(),
            self.repo.replace(' ', "%20")
        )
    }

    fn org(&self) -> [String; 2] {
        ["--org".into(), self.org_url.clone()]
    }

    fn org_project(&self) -> [String; 4] {
        [
            "--org".into(),
            self.org_url.clone(),
            "--project".into(),
            self.project.clone(),
        ]
    }
}

fn az(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

fn json_out(mut v: Vec<String>) -> Vec<String> {
    v.push("-o".into());
    v.push("json".into());
    v
}

// ---------------------------------------------------------------------------
// Tool checks
// ---------------------------------------------------------------------------

/// `az account show -o json` (is anyone signed in).
pub fn account_show() -> Call {
    Call::new(Program::Az, json_out(az(&["account", "show"])))
}

/// `az extension show --name azure-devops -o json`.
pub fn extension_show() -> Call {
    Call::new(
        Program::Az,
        json_out(az(&["extension", "show", "--name", "azure-devops"])),
    )
}

/// The signed-in user of `az account show`.
pub fn parse_account(body: &[u8]) -> Result<String> {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct User {
        name: Option<String>,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Account {
        user: Option<User>,
    }
    let a: Account = decode(body, "account")?;
    Ok(a.user.and_then(|u| u.name).unwrap_or_default())
}

/// The version of `az extension show`.
pub fn parse_extension(body: &[u8]) -> Result<String> {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Ext {
        version: Option<String>,
    }
    let e: Ext = decode(body, "extension")?;
    Ok(e.version.unwrap_or_default())
}

// ---------------------------------------------------------------------------
// Pull requests
// ---------------------------------------------------------------------------

pub fn pr_list(t: &Target) -> Call {
    let mut a = az(&["repos", "pr", "list"]);
    a.extend(t.org_project());
    a.extend(az(&[
        "--repository",
        &t.repo,
        "--status",
        "active",
        "--top",
        "50",
    ]));
    Call::new(Program::Az, json_out(a))
}

pub fn pr_show(t: &Target, id: u64) -> Call {
    let mut a = az(&["repos", "pr", "show", "--id", &id.to_string()]);
    a.extend(t.org());
    Call::new(Program::Az, json_out(a))
}

/// What the create form collected.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewPr {
    pub source: String,
    pub target: String,
    pub title: String,
    pub description: String,
    pub draft: bool,
    /// Names or e-mail addresses.
    pub reviewers: Vec<String>,
    pub labels: Vec<String>,
    pub auto_complete: bool,
    pub squash: bool,
    pub delete_source: bool,
}

fn no_newlines(s: &str) -> String {
    s.split(['\r', '\n'])
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `az repos pr create …`.
pub fn pr_create(t: &Target, pr: &NewPr) -> Result<Call> {
    if pr.source.trim().is_empty() || pr.target.trim().is_empty() {
        return Err(Error::new("Pick a source and a target branch"));
    }
    if pr.source.trim() == pr.target.trim() {
        return Err(Error::new("The source and target branch are the same"));
    }
    if pr.title.trim().is_empty() {
        return Err(Error::new("A pull request needs a title"));
    }
    let mut a = az(&["repos", "pr", "create"]);
    a.extend(t.org_project());
    a.extend(az(&[
        "--repository",
        &t.repo,
        "--source-branch",
        pr.source.trim(),
        "--target-branch",
        pr.target.trim(),
        "--title",
        &no_newlines(&pr.title),
    ]));
    let lines: Vec<&str> = pr.description.lines().collect();
    if !pr.description.trim().is_empty() {
        a.push("--description".into());
        for l in lines {
            // An empty value would end the list; keep blank lines visible.
            a.push(if l.trim().is_empty() {
                " ".into()
            } else {
                l.trim_end().to_owned()
            });
        }
    }
    if pr.draft {
        a.extend(az(&["--draft", "true"]));
    }
    let reviewers: Vec<String> = pr
        .reviewers
        .iter()
        .map(|r| r.trim().to_owned())
        .filter(|r| !r.is_empty())
        .collect();
    if !reviewers.is_empty() {
        a.push("--reviewers".into());
        a.extend(reviewers);
    }
    let labels: Vec<String> = pr
        .labels
        .iter()
        .map(|r| r.trim().to_owned())
        .filter(|r| !r.is_empty())
        .collect();
    if !labels.is_empty() {
        a.push("--labels".into());
        a.extend(labels);
    }
    if pr.auto_complete {
        a.extend(az(&["--auto-complete", "true"]));
    }
    if pr.squash {
        a.extend(az(&["--squash", "true"]));
    }
    if pr.delete_source {
        a.extend(az(&["--delete-source-branch", "true"]));
    }
    Ok(Call::new(Program::Az, json_out(a)).write())
}

/// Completion options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompleteOpts {
    pub squash: bool,
    pub delete_source: bool,
    /// One line (az takes it as a single argument).
    pub message: String,
}

/// `az repos pr update --id N --status completed …`.
pub fn pr_complete(t: &Target, id: u64, o: &CompleteOpts) -> Call {
    let mut a = az(&[
        "repos",
        "pr",
        "update",
        "--id",
        &id.to_string(),
        "--status",
        "completed",
    ]);
    a.extend(az(&["--squash", if o.squash { "true" } else { "false" }]));
    a.extend(az(&[
        "--delete-source-branch",
        if o.delete_source { "true" } else { "false" },
    ]));
    let msg = no_newlines(&o.message);
    if !msg.is_empty() {
        a.extend(az(&["--merge-commit-message", &msg]));
    }
    a.extend(t.org());
    Call::new(Program::Az, json_out(a)).write()
}

/// A reviewer's vote.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Vote {
    Approve,
    ApproveWithSuggestions,
    WaitForAuthor,
    Reject,
    Reset,
}

impl Vote {
    pub const ALL: [Vote; 5] = [
        Vote::Approve,
        Vote::ApproveWithSuggestions,
        Vote::WaitForAuthor,
        Vote::Reject,
        Vote::Reset,
    ];

    /// The `--vote` value.
    pub fn arg(self) -> &'static str {
        match self {
            Vote::Approve => "approve",
            Vote::ApproveWithSuggestions => "approve-with-suggestions",
            Vote::WaitForAuthor => "wait-for-author",
            Vote::Reject => "reject",
            Vote::Reset => "reset",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Vote::Approve => "Approve",
            Vote::ApproveWithSuggestions => "Approve with suggestions",
            Vote::WaitForAuthor => "Wait for author",
            Vote::Reject => "Reject",
            Vote::Reset => "Reset vote",
        }
    }

    /// The numeric vote Azure stores.
    pub fn value(self) -> i64 {
        match self {
            Vote::Approve => 10,
            Vote::ApproveWithSuggestions => 5,
            Vote::WaitForAuthor => -5,
            Vote::Reject => -10,
            Vote::Reset => 0,
        }
    }
}

/// Label of a stored vote value.
pub fn vote_label(v: i64) -> &'static str {
    match v {
        10 => "Approved",
        5 => "Approved with suggestions",
        -5 => "Waiting for author",
        -10 => "Rejected",
        _ => "No vote",
    }
}

/// `az repos pr set-vote --id N --vote V`.
pub fn pr_vote(t: &Target, id: u64, vote: Vote) -> Call {
    let mut a = az(&[
        "repos",
        "pr",
        "set-vote",
        "--id",
        &id.to_string(),
        "--vote",
        vote.arg(),
    ]);
    a.extend(t.org());
    Call::new(Program::Az, json_out(a)).write()
}

// ---------------------------------------------------------------------------
// Pipelines
// ---------------------------------------------------------------------------

/// `az pipelines runs list --top 50 [--branch refs/heads/B]`.
pub fn runs_list(t: &Target, branch: Option<&str>) -> Call {
    let mut a = az(&["pipelines", "runs", "list", "--top", "50"]);
    if let Some(b) = branch.filter(|b| !b.is_empty()) {
        a.push("--branch".into());
        a.push(if b.starts_with("refs/") {
            b.to_owned()
        } else {
            format!("refs/heads/{b}")
        });
    }
    a.extend(t.org_project());
    Call::new(Program::Az, json_out(a))
}

pub fn run_show(t: &Target, id: u64) -> Call {
    let mut a = az(&["pipelines", "runs", "show", "--id", &id.to_string()]);
    a.extend(t.org_project());
    Call::new(Program::Az, json_out(a))
}

/// `az pipelines run --name X --branch Y [--variables k=v …]`. The values
/// are arguments: never pass secrets here.
pub fn pipeline_run(
    t: &Target,
    name: &str,
    branch: &str,
    vars: &[(String, String)],
) -> Result<Call> {
    if name.trim().is_empty() {
        return Err(Error::new("Pick the pipeline to run"));
    }
    if branch.trim().is_empty() {
        return Err(Error::new("Pick a branch to run on"));
    }
    let mut a = az(&[
        "pipelines",
        "run",
        "--name",
        name.trim(),
        "--branch",
        branch.trim(),
    ]);
    let vars: Vec<String> = vars
        .iter()
        .filter(|(k, _)| !k.trim().is_empty())
        .map(|(k, v)| format!("{}={}", k.trim(), no_newlines(v)))
        .collect();
    if !vars.is_empty() {
        a.push("--variables".into());
        a.extend(vars);
    }
    a.extend(t.org_project());
    Ok(Call::new(Program::Az, json_out(a)).write())
}

/// The run's timeline (stages, jobs, tasks with their log ids) through
/// `az devops invoke`.
pub fn timeline(t: &Target, build: u64) -> Call {
    let mut a = az(&[
        "devops",
        "invoke",
        "--area",
        "build",
        "--resource",
        "timeline",
        "--route-parameters",
    ]);
    a.push(format!("project={}", t.project));
    a.push(format!("buildId={build}"));
    a.extend(t.org());
    Call::new(Program::Az, json_out(a))
}

/// One log of a run through `az devops invoke`.
pub fn log(t: &Target, build: u64, log_id: u64) -> Call {
    let mut a = az(&[
        "devops",
        "invoke",
        "--area",
        "build",
        "--resource",
        "logs",
        "--route-parameters",
    ]);
    a.push(format!("project={}", t.project));
    a.push(format!("buildId={build}"));
    a.push(format!("logId={log_id}"));
    a.extend(t.org());
    Call::new(Program::Az, json_out(a))
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

fn decode<T: serde::de::DeserializeOwned>(body: &[u8], what: &str) -> Result<T> {
    serde_json::from_slice(body).map_err(|e| Error::new(format!("Azure DevOps {what}: {e}")))
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Identity {
    display_name: Option<String>,
    unique_name: Option<String>,
}

impl Identity {
    fn person(&self) -> Person {
        Person::new(
            self.display_name.clone().unwrap_or_default(),
            self.unique_name.clone().unwrap_or_default(),
        )
    }
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct RawReviewer {
    display_name: Option<String>,
    unique_name: Option<String>,
    vote: Option<i64>,
    is_required: Option<bool>,
    has_declined: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct CommitRef {
    commit_id: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Label {
    name: Option<String>,
    active: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Completion {
    squash_merge: Option<bool>,
    delete_source_branch: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct RawPr {
    pull_request_id: u64,
    title: Option<String>,
    description: Option<String>,
    status: Option<String>,
    is_draft: Option<bool>,
    created_by: Option<Identity>,
    creation_date: Option<String>,
    source_ref_name: Option<String>,
    target_ref_name: Option<String>,
    reviewers: Option<Vec<RawReviewer>>,
    merge_status: Option<String>,
    labels: Option<Vec<Label>>,
    last_merge_source_commit: Option<CommitRef>,
    auto_complete_set_by: Option<Identity>,
    completion_options: Option<Completion>,
    merge_failure_message: Option<String>,
}

fn convert_pr(t: &Target, p: RawPr) -> ChangeRequest {
    let s = |o: Option<String>| o.unwrap_or_default();
    let merge_status = s(p.merge_status);
    let completion = p.completion_options.unwrap_or_default();
    ChangeRequest {
        number: p.pull_request_id,
        title: s(p.title),
        description: s(p.description),
        author: p
            .created_by
            .as_ref()
            .map(Identity::person)
            .unwrap_or_default(),
        source: short_ref(p.source_ref_name.as_deref().unwrap_or("")),
        target: short_ref(p.target_ref_name.as_deref().unwrap_or("")),
        draft: p.is_draft.unwrap_or(false),
        state: s(p.status),
        created: time(p.creation_date.as_deref()),
        updated: None,
        web_url: t.pr_url(p.pull_request_id),
        sha: p
            .last_merge_source_commit
            .and_then(|c| c.commit_id)
            .unwrap_or_default(),
        labels: p
            .labels
            .unwrap_or_default()
            .into_iter()
            .filter(|l| l.active.unwrap_or(true))
            .filter_map(|l| l.name)
            .collect(),
        assignees: Vec::new(),
        reviewers: p
            .reviewers
            .unwrap_or_default()
            .into_iter()
            .map(|r| Reviewer {
                person: Person::new(s(r.display_name), s(r.unique_name)),
                vote: Some(r.vote.unwrap_or(0)),
                required: r.is_required.unwrap_or(false),
                declined: r.has_declined.unwrap_or(false),
            })
            .collect(),
        conflicts: merge_status == "conflicts",
        merge_status: words(&merge_status),
        comments: 0,
        upvotes: 0,
        downvotes: 0,
        squash: completion.squash_merge.unwrap_or(false),
        remove_source: completion.delete_source_branch.unwrap_or(false),
        auto_merge: p.auto_complete_set_by.is_some(),
        pipeline: None,
        tasks: None,
        changes_count: None,
        can_merge: None,
        merge_error: p.merge_failure_message.filter(|m| !m.is_empty()),
        blocking_discussions_resolved: true,
    }
}

pub fn parse_prs(t: &Target, body: &[u8]) -> Result<Vec<ChangeRequest>> {
    let raw: Vec<RawPr> = decode(body, "pull requests")?;
    Ok(raw.into_iter().map(|p| convert_pr(t, p)).collect())
}

pub fn parse_pr(t: &Target, body: &[u8]) -> Result<ChangeRequest> {
    Ok(convert_pr(t, decode(body, "pull request")?))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Definition {
    id: Option<u64>,
    name: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Href {
    href: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Links {
    web: Option<Href>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct RawBuild {
    id: u64,
    build_number: Option<String>,
    status: Option<String>,
    result: Option<String>,
    definition: Option<Definition>,
    source_branch: Option<String>,
    source_version: Option<String>,
    queue_time: Option<String>,
    start_time: Option<String>,
    finish_time: Option<String>,
    requested_for: Option<Identity>,
    reason: Option<String>,
    #[serde(rename = "_links")]
    links: Option<Links>,
}

fn convert_build(t: &Target, b: RawBuild) -> Pipeline {
    let status = b.status.unwrap_or_default();
    let result = b.result.unwrap_or_default();
    let queued_at = time(b.queue_time.as_deref());
    let started = time(b.start_time.as_deref());
    let finished = time(b.finish_time.as_deref());
    let def = b.definition.unwrap_or_default();
    Pipeline {
        id: b.id,
        number: b.build_number.unwrap_or_default(),
        name: def.name.unwrap_or_default(),
        definition_id: def.id,
        outcome: azure_outcome(&status, &result),
        status: if status == "completed" && !result.is_empty() {
            words(&result)
        } else {
            words(&status)
        },
        reference: short_ref(b.source_branch.as_deref().unwrap_or("")),
        sha: b.source_version.unwrap_or_default(),
        source: words(b.reason.as_deref().unwrap_or("")),
        created: queued_at,
        started,
        finished,
        duration: span(started, finished),
        queued: span(queued_at, started),
        user: b
            .requested_for
            .map(|i| i.person().display())
            .unwrap_or_default(),
        web_url: b
            .links
            .and_then(|l| l.web)
            .and_then(|w| w.href)
            .unwrap_or_else(|| t.build_url(b.id)),
    }
}

pub fn parse_runs(t: &Target, body: &[u8]) -> Result<Vec<Pipeline>> {
    let raw: Vec<RawBuild> = decode(body, "pipeline runs")?;
    Ok(raw.into_iter().map(|b| convert_build(t, b)).collect())
}

pub fn parse_run(t: &Target, body: &[u8]) -> Result<Pipeline> {
    Ok(convert_build(t, decode(body, "pipeline run")?))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LogRef {
    id: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Issue {
    #[serde(rename = "type")]
    kind: Option<String>,
    message: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Record {
    id: String,
    parent_id: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    name: Option<String>,
    state: Option<String>,
    result: Option<String>,
    start_time: Option<String>,
    finish_time: Option<String>,
    log: Option<LogRef>,
    error_count: Option<u32>,
    warning_count: Option<u32>,
    issues: Option<Vec<Issue>>,
    order: Option<i64>,
    worker_name: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Timeline {
    records: Vec<Record>,
}

/// The timeline as a tree flattened in display order: stages (depth 0),
/// their jobs (1) and the jobs' tasks (2). Phases and checkpoints are
/// folded away; without stages, jobs start at depth 0.
pub fn parse_timeline(t: &Target, build: u64, body: &[u8]) -> Result<Vec<Job>> {
    let tl: Timeline = decode(body, "timeline")?;
    let recs = tl.records;
    let kind = |r: &Record| r.kind.clone().unwrap_or_default();
    let children = |parent: Option<&str>, kinds: &[&str]| -> Vec<&Record> {
        let mut v: Vec<&Record> = recs
            .iter()
            .filter(|r| r.parent_id.as_deref() == parent && kinds.contains(&kind(r).as_str()))
            .collect();
        v.sort_by_key(|r| r.order.unwrap_or(0));
        v
    };
    let convert = |r: &Record, depth: u8| -> Job {
        let state = r.state.clone().unwrap_or_default();
        let result = r.result.clone().unwrap_or_default();
        let started = time(r.start_time.as_deref());
        let finished = time(r.finish_time.as_deref());
        Job {
            id: r.log.as_ref().and_then(|l| l.id).unwrap_or(0),
            record: r.id.clone(),
            name: r.name.clone().unwrap_or_default(),
            stage: kind(r),
            outcome: azure_outcome(&state, &result),
            status: if state == "completed" && !result.is_empty() {
                words(&result)
            } else {
                words(&state)
            },
            started,
            finished,
            duration: span(started, finished),
            web_url: t.build_url(build),
            runner: r.worker_name.clone().unwrap_or_default(),
            depth,
            log_id: r.log.as_ref().and_then(|l| l.id),
            errors: r.error_count.unwrap_or(0),
            warnings: r.warning_count.unwrap_or(0),
            issues: r
                .issues
                .as_ref()
                .map(|v| {
                    v.iter()
                        .filter_map(|i| {
                            let m = i.message.clone()?;
                            Some(match i.kind.as_deref() {
                                Some(k) if !k.is_empty() => format!("{k}: {m}"),
                                _ => m,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            ..Default::default()
        }
    };
    let mut out = Vec::new();
    // Jobs hang off phases; a phase's parent is the stage (or nothing).
    let jobs_under = |parent: Option<&str>, depth: u8, out: &mut Vec<Job>| {
        for phase in children(parent, &["Phase"]) {
            for job in children(Some(&phase.id), &["Job"]) {
                out.push(convert(job, depth));
                for task in children(Some(&job.id), &["Task"]) {
                    out.push(convert(task, depth + 1));
                }
            }
        }
        // Jobs directly under the parent (older timelines).
        for job in children(parent, &["Job"]) {
            out.push(convert(job, depth));
            for task in children(Some(&job.id), &["Task"]) {
                out.push(convert(task, depth + 1));
            }
        }
    };
    let stages = children(None, &["Stage"]);
    if stages.is_empty() {
        jobs_under(None, 0, &mut out);
    } else {
        for s in stages {
            out.push(convert(s, 0));
            jobs_under(Some(&s.id), 1, &mut out);
        }
    }
    Ok(out)
}

/// The text of a log answer: `{"count": n, "value": ["line", …]}` from
/// `az devops invoke`, a JSON string, or plain text.
pub fn log_text(body: &[u8]) -> Vec<u8> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(o)) => match o.get("value") {
            Some(Value::Array(lines)) => lines
                .iter()
                .map(|l| l.as_str().unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n")
                .into_bytes(),
            _ => body.to_vec(),
        },
        Ok(Value::String(s)) => s.into_bytes(),
        _ => body.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::forge::detect;
    use crate::github::Outcome;

    fn target() -> Target {
        Target::of(&detect("https://dev.azure.com/acme/Web Shop/_git/frontend").unwrap()).unwrap()
    }

    #[test]
    fn targets_and_urls() {
        let t = target();
        assert_eq!(t.org_url, "https://dev.azure.com/acme");
        assert_eq!(t.project, "Web Shop");
        assert_eq!(t.repo, "frontend");
        assert_eq!(
            t.pr_url(22),
            "https://dev.azure.com/acme/Web%20Shop/_git/frontend/pullrequest/22"
        );
        assert_eq!(
            t.build_url(7),
            "https://dev.azure.com/acme/Web%20Shop/_build/results?buildId=7"
        );
        assert!(Target::of(&detect("https://gitlab.com/a/b").unwrap()).is_none());
    }

    #[test]
    fn read_arguments() {
        let t = target();
        assert_eq!(
            pr_list(&t).args,
            vec![
                "repos",
                "pr",
                "list",
                "--org",
                "https://dev.azure.com/acme",
                "--project",
                "Web Shop",
                "--repository",
                "frontend",
                "--status",
                "active",
                "--top",
                "50",
                "-o",
                "json"
            ]
        );
        assert_eq!(
            pr_show(&t, 22).args,
            vec![
                "repos",
                "pr",
                "show",
                "--id",
                "22",
                "--org",
                "https://dev.azure.com/acme",
                "-o",
                "json"
            ]
        );
        assert_eq!(
            runs_list(&t, Some("main")).args,
            vec![
                "pipelines",
                "runs",
                "list",
                "--top",
                "50",
                "--branch",
                "refs/heads/main",
                "--org",
                "https://dev.azure.com/acme",
                "--project",
                "Web Shop",
                "-o",
                "json"
            ]
        );
        assert!(!runs_list(&t, None).args.contains(&"--branch".to_owned()));
        assert_eq!(
            timeline(&t, 99).args,
            vec![
                "devops",
                "invoke",
                "--area",
                "build",
                "--resource",
                "timeline",
                "--route-parameters",
                "project=Web Shop",
                "buildId=99",
                "--org",
                "https://dev.azure.com/acme",
                "-o",
                "json"
            ]
        );
        assert!(log(&t, 99, 5).args.contains(&"logId=5".to_owned()));
        assert_eq!(account_show().display(), "az account show -o json");
        assert_eq!(
            extension_show().display(),
            "az extension show --name azure-devops -o json"
        );
        for c in [
            pr_list(&t),
            pr_show(&t, 1),
            runs_list(&t, None),
            run_show(&t, 1),
            timeline(&t, 1),
        ] {
            assert!(!c.write);
            assert_eq!(c.program, Program::Az);
        }
    }

    #[test]
    fn write_arguments() {
        let t = target();
        let pr = NewPr {
            source: "feature".into(),
            target: "main".into(),
            title: "Fix\nlogin".into(),
            description: "First line\n\nThird line".into(),
            draft: true,
            reviewers: vec!["ada@acme.com".into(), " ".into()],
            labels: vec!["ui".into()],
            auto_complete: true,
            squash: true,
            delete_source: true,
        };
        let c = pr_create(&t, &pr).unwrap();
        assert!(c.write);
        assert!(
            c.args.iter().all(|a| !a.contains('\n')),
            "no newline reaches az.cmd"
        );
        let i = c.args.iter().position(|a| a == "--description").unwrap();
        assert_eq!(&c.args[i + 1..i + 4], &["First line", " ", "Third line"]);
        let title = c.args.iter().position(|a| a == "--title").unwrap();
        assert_eq!(c.args[title + 1], "Fix login");
        for flag in [
            "--draft",
            "--auto-complete",
            "--squash",
            "--delete-source-branch",
        ] {
            let p = c.args.iter().position(|a| a == flag).unwrap();
            assert_eq!(c.args[p + 1], "true", "{flag}");
        }
        let r = c.args.iter().position(|a| a == "--reviewers").unwrap();
        assert_eq!(c.args[r + 1], "ada@acme.com");
        assert_eq!(c.args[r + 2], "--labels");
        assert!(
            pr_create(
                &t,
                &NewPr {
                    target: "feature".into(),
                    ..pr.clone()
                }
            )
            .is_err()
        );

        let done = pr_complete(
            &t,
            22,
            &CompleteOpts {
                squash: true,
                delete_source: false,
                message: "Merged\nPR 22".into(),
            },
        );
        assert_eq!(
            done.args,
            vec![
                "repos",
                "pr",
                "update",
                "--id",
                "22",
                "--status",
                "completed",
                "--squash",
                "true",
                "--delete-source-branch",
                "false",
                "--merge-commit-message",
                "Merged PR 22",
                "--org",
                "https://dev.azure.com/acme",
                "-o",
                "json"
            ]
        );
        let v = pr_vote(&t, 22, Vote::ApproveWithSuggestions);
        assert_eq!(
            v.args,
            vec![
                "repos",
                "pr",
                "set-vote",
                "--id",
                "22",
                "--vote",
                "approve-with-suggestions",
                "--org",
                "https://dev.azure.com/acme",
                "-o",
                "json"
            ]
        );
        assert!(v.write);
        for vote in Vote::ALL {
            assert_eq!(vote_label(vote.value()).is_empty(), false);
        }
        assert_eq!(vote_label(Vote::Reject.value()), "Rejected");

        let run = pipeline_run(
            &t,
            "frontend-ci",
            "main",
            &[("DEPLOY".into(), "yes".into()), ("".into(), "x".into())],
        )
        .unwrap();
        assert_eq!(
            run.args,
            vec![
                "pipelines",
                "run",
                "--name",
                "frontend-ci",
                "--branch",
                "main",
                "--variables",
                "DEPLOY=yes",
                "--org",
                "https://dev.azure.com/acme",
                "--project",
                "Web Shop",
                "-o",
                "json"
            ]
        );
        assert!(run.write);
        assert!(pipeline_run(&t, "", "main", &[]).is_err());
    }

    const PRS: &str = r#"[{
      "artifactId": "vstfs:///Git/PullRequestId/1%2f2%2f22",
      "autoCompleteSetBy": null,
      "closedBy": null,
      "codeReviewId": 22,
      "completionOptions": {"squashMerge": true, "deleteSourceBranch": true},
      "createdBy": {"displayName": "Jamal Hartnett", "uniqueName": "jamal@acme.com", "id": "d6245f20"},
      "creationDate": "2026-10-20T09:12:41.123Z",
      "description": "Adds the login page",
      "isDraft": false,
      "labels": [{"id": "1", "name": "ui", "active": true}, {"id": "2", "name": "old", "active": false}],
      "lastMergeSourceCommit": {"commitId": "b60280bc6e62e2f880f1b63c1e24987664d3bda3"},
      "mergeStatus": "conflicts",
      "pullRequestId": 22,
      "repository": {"name": "frontend", "project": {"name": "Web Shop"}},
      "reviewers": [
        {"displayName": "Normal Paulk", "uniqueName": "normal@acme.com", "vote": 10, "isRequired": true},
        {"displayName": "[Web Shop]\\Team", "uniqueName": "vstfs:///Classification/TeamProject/x", "vote": 0}
      ],
      "sourceRefName": "refs/heads/feature/login",
      "status": "active",
      "targetRefName": "refs/heads/main",
      "title": "Login page"
    }]"#;

    #[test]
    fn parses_pull_requests() {
        let t = target();
        let prs = parse_prs(&t, PRS.as_bytes()).unwrap();
        let p = &prs[0];
        assert_eq!(p.number, 22);
        assert_eq!(p.source, "feature/login");
        assert_eq!(p.target, "main");
        assert_eq!(p.author.login, "jamal@acme.com");
        assert_eq!(p.labels, vec!["ui"]);
        assert!(p.conflicts);
        assert_eq!(p.merge_status, "conflicts");
        assert!(p.squash && p.remove_source && !p.auto_merge);
        assert_eq!(p.reviewers[0].vote, Some(10));
        assert!(p.reviewers[0].required);
        assert_eq!(p.reviewers[1].vote, Some(0));
        assert_eq!(p.sha, "b60280bc6e62e2f880f1b63c1e24987664d3bda3");
        assert_eq!(
            p.web_url,
            "https://dev.azure.com/acme/Web%20Shop/_git/frontend/pullrequest/22"
        );
        let one = parse_pr(
            &t,
            br#"{"pullRequestId": 5, "autoCompleteSetBy": {"displayName": "Ada"}}"#,
        )
        .unwrap();
        assert!(one.auto_merge);
    }

    #[test]
    fn parses_runs_and_timeline() {
        let t = target();
        let runs = r#"[
          {"id": 123, "buildNumber": "20261026.1", "status": "completed", "result": "partiallySucceeded",
           "definition": {"id": 4, "name": "frontend-ci"}, "sourceBranch": "refs/heads/main", "sourceVersion": "abc",
           "queueTime": "2026-10-26T04:50:00.0000000Z", "startTime": "2026-10-26T04:50:27.3645819Z",
           "finishTime": "2026-10-26T04:55:27.3645819Z", "requestedFor": {"displayName": "Ada", "uniqueName": "ada@acme.com"},
           "reason": "individualCI", "_links": {"web": {"href": "https://dev.azure.com/acme/web/_build/results?buildId=123"}}},
          {"id": 124, "status": "inProgress", "definition": {"name": "frontend-ci"}, "sourceBranch": "refs/pull/5/merge",
           "queueTime": "2026-10-26T05:00:00Z", "startTime": "0001-01-01T00:00:00"}
        ]"#;
        let r = parse_runs(&t, runs.as_bytes()).unwrap();
        assert_eq!(r[0].status, "partially succeeded");
        assert_eq!(r[0].outcome, Outcome::Neutral);
        assert_eq!(r[0].duration, Some(300));
        assert_eq!(r[0].queued, Some(27));
        assert_eq!(r[0].source, "individual CI");
        assert_eq!(r[0].user, "Ada (ada@acme.com)");
        assert_eq!(r[0].definition_id, Some(4));
        assert_eq!(r[1].outcome, Outcome::InProgress);
        assert_eq!(r[1].started, None);
        assert_eq!(r[1].reference, "refs/pull/5/merge");
        assert_eq!(
            r[1].web_url,
            "https://dev.azure.com/acme/Web%20Shop/_build/results?buildId=124"
        );

        let tl = r#"{"records": [
          {"id": "s1", "parentId": null, "type": "Stage", "name": "Build", "state": "completed", "result": "failed", "order": 1},
          {"id": "p1", "parentId": "s1", "type": "Phase", "name": "Build", "order": 1},
          {"id": "j1", "parentId": "p1", "type": "Job", "name": "Compile", "state": "completed", "result": "failed",
           "startTime": "2026-10-26T04:50:30Z", "finishTime": "2026-10-26T04:52:30Z", "log": {"id": 9},
           "errorCount": 1, "warningCount": 2, "workerName": "Hosted Agent", "order": 1},
          {"id": "t2", "parentId": "j1", "type": "Task", "name": "npm test", "state": "completed", "result": "failed",
           "log": {"id": 11}, "order": 2, "issues": [{"type": "error", "message": "Process completed with exit code 1."}]},
          {"id": "t1", "parentId": "j1", "type": "Task", "name": "Checkout", "state": "completed", "result": "succeeded",
           "log": {"id": 10}, "order": 1},
          {"id": "s2", "parentId": null, "type": "Stage", "name": "Deploy", "state": "completed", "result": "skipped", "order": 2},
          {"id": "c1", "parentId": "s2", "type": "Checkpoint", "name": "Checkpoint", "order": 1}
        ]}"#;
        let jobs = parse_timeline(&t, 123, tl.as_bytes()).unwrap();
        let names: Vec<(&str, u8)> = jobs.iter().map(|j| (j.name.as_str(), j.depth)).collect();
        assert_eq!(
            names,
            vec![
                ("Build", 0),
                ("Compile", 1),
                ("Checkout", 2),
                ("npm test", 2),
                ("Deploy", 0)
            ]
        );
        let compile = &jobs[1];
        assert_eq!(compile.log_id, Some(9));
        assert_eq!(compile.duration, Some(120));
        assert_eq!((compile.errors, compile.warnings), (1, 2));
        assert_eq!(compile.runner, "Hosted Agent");
        assert_eq!(
            jobs[3].issues,
            vec!["error: Process completed with exit code 1."]
        );
        assert_eq!(jobs[4].outcome, Outcome::Skipped);
        assert_eq!(jobs[0].log_id, None);
    }

    #[test]
    fn log_answers() {
        assert_eq!(log_text(br#"{"count": 2, "value": ["a", "b"]}"#), b"a\nb");
        assert_eq!(log_text(br#""x\ny""#), b"x\ny");
        assert_eq!(log_text(b"plain\ntext"), b"plain\ntext");
    }

    #[test]
    fn account_and_extension() {
        assert_eq!(
            parse_account(br#"{"user": {"name": "ada@acme.com", "type": "user"}}"#).unwrap(),
            "ada@acme.com"
        );
        assert_eq!(
            parse_extension(br#"{"name": "azure-devops", "version": "1.0.1"}"#).unwrap(),
            "1.0.1"
        );
    }
}
