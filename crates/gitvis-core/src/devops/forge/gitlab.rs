//! GitLab through `glab api <endpoint>` (the user's `glab auth`), or, for a
//! public project without a sign-in, the same REST endpoints through
//! unauthenticated `curl` (read only).
//!
//! The project id is the URL-encoded project path (`group%2Fsub%2Fapp`);
//! self-hosted hosts get `--hostname`. Request bodies go to
//! `glab api --input -` on standard input with a JSON content type.

use serde::Deserialize;
use serde_json::{Value, json};

use super::ForgeRemote;
use super::cli::{Call, MASK, Program, curl_get};
use super::model::{
    Approvals, ChangeRequest, FileChange, Job, Note, Person, Pipeline, PipelineRef, Reviewer,
    Thread, count_diff, gitlab_outcome, short_ref, span, time, words,
};
use crate::devops::gh::{Method, encode};
use crate::{Error, Result};

/// One GitLab REST request.
#[derive(Clone, PartialEq)]
pub struct GlRequest {
    pub method: Method,
    /// Relative to `/api/v4/`, e.g. `projects/a%2Fb/merge_requests`.
    pub endpoint: String,
    pub body: Option<Value>,
    /// What to show of the body (secret variable values masked).
    pub shown: Option<Value>,
    /// The GitLab host (`gitlab.com`, `gitlab.example.org`).
    pub host: String,
}

/// Debug output shows the masked body only.
impl std::fmt::Debug for GlRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlRequest")
            .field("method", &self.method)
            .field("endpoint", &self.endpoint)
            .field("body", &self.shown)
            .field("host", &self.host)
            .finish()
    }
}

impl GlRequest {
    pub fn new(method: Method, host: &str, endpoint: impl Into<String>) -> Self {
        Self {
            method,
            endpoint: endpoint.into(),
            body: None,
            shown: None,
            host: host.to_owned(),
        }
    }

    pub fn body(mut self, body: Value) -> Self {
        self.shown = Some(body.clone());
        self.body = Some(body);
        self
    }

    /// `glab` arguments: `api [--hostname H] [--method M] [-H … --input -] endpoint`.
    pub fn args(&self) -> Vec<String> {
        let mut a = vec!["api".to_owned()];
        if !self.host.eq_ignore_ascii_case("gitlab.com") {
            a.push("--hostname".into());
            a.push(self.host.clone());
        }
        if self.method != Method::Get {
            a.push("--method".into());
            a.push(self.method.as_str().into());
        }
        if self.body.is_some() {
            a.push("-H".into());
            a.push("Content-Type: application/json".into());
            a.push("--input".into());
            a.push("-".into());
        }
        a.push(self.endpoint.clone());
        a
    }

    /// The `glab api` call.
    pub fn call(&self) -> Call {
        let mut c = Call::new(Program::Glab, self.args());
        if let Some(b) = &self.body {
            c = c.body(b.clone());
            if let Some(s) = &self.shown {
                c = c.shown(s.clone());
            }
        }
        if self.method != Method::Get {
            c = c.write();
        }
        c
    }

    /// `https://host/api/v4/endpoint`.
    pub fn public_url(&self) -> String {
        format!("https://{}/api/v4/{}", self.host, self.endpoint)
    }

    /// The unauthenticated `curl` equivalent (GET only).
    pub fn public_call(&self) -> Option<Call> {
        (self.method == Method::Get).then(|| curl_get(&self.public_url()))
    }
}

/// The URL-encoded project path used as `:id`.
pub fn project_id(remote: &ForgeRemote) -> String {
    encode(&remote.path)
}

fn project(remote: &ForgeRemote, rest: &str) -> String {
    format!("projects/{}/{rest}", project_id(remote))
}

fn get(remote: &ForgeRemote, rest: &str) -> GlRequest {
    GlRequest::new(Method::Get, &remote.host, project(remote, rest))
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// The signed-in user.
pub fn current_user(remote: &ForgeRemote) -> GlRequest {
    GlRequest::new(Method::Get, &remote.host, "user")
}

/// `glab auth status --hostname HOST`.
pub fn auth_status(remote: &ForgeRemote) -> Call {
    Call::new(
        Program::Glab,
        vec![
            "auth".into(),
            "status".into(),
            "--hostname".into(),
            remote.host.clone(),
        ],
    )
}

/// `@user` from `glab auth status` output (`Logged in to gitlab.com as ada`).
pub fn login_from_status(text: &str) -> Option<String> {
    let (_, rest) = text.split_once(" as ")?;
    let name: String = rest
        .trim()
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '(')
        .collect();
    (!name.is_empty()).then_some(name)
}

pub fn merge_requests(remote: &ForgeRemote) -> GlRequest {
    get(remote, "merge_requests?state=opened&per_page=50")
}

pub fn merge_request(remote: &ForgeRemote, iid: u64) -> GlRequest {
    get(remote, &format!("merge_requests/{iid}"))
}

pub fn approvals(remote: &ForgeRemote, iid: u64) -> GlRequest {
    get(remote, &format!("merge_requests/{iid}/approvals"))
}

pub fn discussions(remote: &ForgeRemote, iid: u64) -> GlRequest {
    get(
        remote,
        &format!("merge_requests/{iid}/discussions?per_page=100"),
    )
}

pub fn diffs(remote: &ForgeRemote, iid: u64) -> GlRequest {
    get(remote, &format!("merge_requests/{iid}/diffs?per_page=100"))
}

pub fn mr_pipelines(remote: &ForgeRemote, iid: u64) -> GlRequest {
    get(
        remote,
        &format!("merge_requests/{iid}/pipelines?per_page=20"),
    )
}

/// Pipelines, newest first; only those of `reference` when given.
pub fn pipelines(remote: &ForgeRemote, reference: Option<&str>) -> GlRequest {
    match reference {
        Some(r) => get(remote, &format!("pipelines?per_page=50&ref={}", encode(r))),
        None => get(remote, "pipelines?per_page=50"),
    }
}

pub fn pipeline(remote: &ForgeRemote, id: u64) -> GlRequest {
    get(remote, &format!("pipelines/{id}"))
}

pub fn pipeline_jobs(remote: &ForgeRemote, id: u64) -> GlRequest {
    get(remote, &format!("pipelines/{id}/jobs?per_page=100"))
}

pub fn job_trace(remote: &ForgeRemote, job: u64) -> GlRequest {
    get(remote, &format!("jobs/{job}/trace"))
}

/// The job log of a public project on the web (the API's trace endpoint
/// needs a sign-in even for public projects).
pub fn public_trace(remote: &ForgeRemote, job: u64) -> Call {
    curl_get(&format!("{}/-/jobs/{job}/raw", remote.web_url))
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// What the create form collected.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewMr {
    pub source: String,
    pub target: String,
    pub title: String,
    pub description: String,
    pub draft: bool,
    pub remove_source: bool,
    pub squash: bool,
    /// Numeric user ids.
    pub assignee_ids: Vec<String>,
    pub reviewer_ids: Vec<String>,
    pub labels: Vec<String>,
}

fn ids(list: &[String], what: &str) -> Result<Vec<u64>> {
    list.iter()
        .map(|s| {
            s.trim().parse::<u64>().map_err(|_| {
                Error::new(format!(
                    "{what} must be numeric GitLab user ids, `{}` is not",
                    s.trim()
                ))
            })
        })
        .collect()
}

/// `Draft: ` in front of the title, once.
pub fn draft_title(title: &str, draft: bool) -> String {
    let t = title.trim();
    let bare = t
        .strip_prefix("Draft:")
        .or_else(|| t.strip_prefix("draft:"))
        .map(str::trim)
        .unwrap_or(t);
    if draft {
        format!("Draft: {bare}")
    } else {
        t.to_owned()
    }
}

/// `POST projects/:id/merge_requests`.
pub fn create_mr(remote: &ForgeRemote, mr: &NewMr) -> Result<GlRequest> {
    if mr.source.trim().is_empty() || mr.target.trim().is_empty() {
        return Err(Error::new("Pick a source and a target branch"));
    }
    if mr.source.trim() == mr.target.trim() {
        return Err(Error::new("The source and target branch are the same"));
    }
    if mr.title.trim().is_empty() {
        return Err(Error::new("A merge request needs a title"));
    }
    let mut body = json!({
        "source_branch": mr.source.trim(),
        "target_branch": mr.target.trim(),
        "title": draft_title(&mr.title, mr.draft),
        "description": mr.description,
        "remove_source_branch": mr.remove_source,
        "squash": mr.squash,
    });
    let a = ids(&mr.assignee_ids, "Assignees")?;
    if !a.is_empty() {
        body["assignee_ids"] = json!(a);
    }
    let r = ids(&mr.reviewer_ids, "Reviewers")?;
    if !r.is_empty() {
        body["reviewer_ids"] = json!(r);
    }
    if !mr.labels.is_empty() {
        body["labels"] = json!(mr.labels.join(","));
    }
    Ok(GlRequest::new(
        Method::Post,
        &remote.host,
        project(remote, "merge_requests"),
    )
    .body(body))
}

/// Merge options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergeOpts {
    pub squash: bool,
    pub remove_source: bool,
    pub when_pipeline_succeeds: bool,
    /// Merge only if the head is still this commit.
    pub sha: Option<String>,
    /// Merge (or squash) commit message; empty keeps GitLab's default.
    pub message: String,
}

/// `PUT projects/:id/merge_requests/:iid/merge`.
pub fn merge(remote: &ForgeRemote, iid: u64, o: &MergeOpts) -> GlRequest {
    let mut body = json!({
        "squash": o.squash,
        "should_remove_source_branch": o.remove_source,
        "merge_when_pipeline_succeeds": o.when_pipeline_succeeds,
    });
    if let Some(sha) = o.sha.as_deref().filter(|s| !s.is_empty()) {
        body["sha"] = json!(sha);
    }
    if !o.message.trim().is_empty() {
        let key = if o.squash {
            "squash_commit_message"
        } else {
            "merge_commit_message"
        };
        body[key] = json!(o.message.trim());
    }
    GlRequest::new(
        Method::Put,
        &remote.host,
        project(remote, &format!("merge_requests/{iid}/merge")),
    )
    .body(body)
}

/// `POST …/approve`, pinned to the head commit when known.
pub fn approve(remote: &ForgeRemote, iid: u64, sha: Option<&str>) -> GlRequest {
    let r = GlRequest::new(
        Method::Post,
        &remote.host,
        project(remote, &format!("merge_requests/{iid}/approve")),
    );
    match sha.filter(|s| !s.is_empty()) {
        Some(s) => r.body(json!({ "sha": s })),
        None => r,
    }
}

/// `POST …/unapprove`.
pub fn unapprove(remote: &ForgeRemote, iid: u64) -> GlRequest {
    GlRequest::new(
        Method::Post,
        &remote.host,
        project(remote, &format!("merge_requests/{iid}/unapprove")),
    )
}

/// `POST …/discussions` (a new thread).
pub fn comment(remote: &ForgeRemote, iid: u64, body: &str) -> Result<GlRequest> {
    if body.trim().is_empty() {
        return Err(Error::new("Write a comment first"));
    }
    Ok(GlRequest::new(
        Method::Post,
        &remote.host,
        project(remote, &format!("merge_requests/{iid}/discussions")),
    )
    .body(json!({ "body": body })))
}

pub fn retry_pipeline(remote: &ForgeRemote, id: u64) -> GlRequest {
    GlRequest::new(
        Method::Post,
        &remote.host,
        project(remote, &format!("pipelines/{id}/retry")),
    )
}

pub fn cancel_pipeline(remote: &ForgeRemote, id: u64) -> GlRequest {
    GlRequest::new(
        Method::Post,
        &remote.host,
        project(remote, &format!("pipelines/{id}/cancel")),
    )
}

pub fn retry_job(remote: &ForgeRemote, job: u64) -> GlRequest {
    GlRequest::new(
        Method::Post,
        &remote.host,
        project(remote, &format!("jobs/{job}/retry")),
    )
}

pub fn cancel_job(remote: &ForgeRemote, job: u64) -> GlRequest {
    GlRequest::new(
        Method::Post,
        &remote.host,
        project(remote, &format!("jobs/{job}/cancel")),
    )
}

/// A pipeline variable for a new run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Var {
    pub key: String,
    pub value: String,
    /// Masked in confirmations and the activity log.
    pub secret: bool,
}

/// `POST projects/:id/pipeline?ref=X` with variables in the body (on
/// standard input; secret values are masked in what is shown and logged).
pub fn run_pipeline(remote: &ForgeRemote, reference: &str, vars: &[Var]) -> Result<GlRequest> {
    if reference.trim().is_empty() {
        return Err(Error::new("Pick a branch or tag to run the pipeline on"));
    }
    let vars: Vec<&Var> = vars.iter().filter(|v| !v.key.trim().is_empty()).collect();
    let body_vars: Vec<Value> = vars
        .iter()
        .map(|v| json!({"key": v.key.trim(), "value": v.value, "variable_type": "env_var"}))
        .collect();
    let shown_vars: Vec<Value> = vars
        .iter()
        .map(|v| {
            json!({
                "key": v.key.trim(),
                "value": if v.secret { MASK.to_owned() } else { v.value.clone() },
                "variable_type": "env_var"
            })
        })
        .collect();
    let mut r = GlRequest::new(
        Method::Post,
        &remote.host,
        project(
            remote,
            &format!("pipeline?ref={}", encode(reference.trim())),
        ),
    )
    .body(json!({ "variables": body_vars }));
    r.shown = Some(json!({ "variables": shown_vars }));
    Ok(r)
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawUser {
    id: u64,
    username: Option<String>,
    name: Option<String>,
}

impl RawUser {
    fn person(&self) -> Person {
        Person::new(
            self.name.clone().unwrap_or_default(),
            self.username.clone().unwrap_or_default(),
        )
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawTasks {
    count: u64,
    completed_count: u64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawCanMerge {
    can_merge: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawPipeline {
    id: u64,
    iid: Option<u64>,
    status: Option<String>,
    source: Option<String>,
    #[serde(rename = "ref")]
    reference: Option<String>,
    sha: Option<String>,
    web_url: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
    duration: Option<f64>,
    queued_duration: Option<f64>,
    user: Option<RawUser>,
    name: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawMr {
    iid: u64,
    title: Option<String>,
    description: Option<String>,
    state: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    target_branch: Option<String>,
    source_branch: Option<String>,
    user_notes_count: u64,
    upvotes: u64,
    downvotes: u64,
    author: Option<RawUser>,
    assignees: Option<Vec<RawUser>>,
    reviewers: Option<Vec<RawUser>>,
    labels: Option<Vec<String>>,
    draft: Option<bool>,
    work_in_progress: Option<bool>,
    merge_when_pipeline_succeeds: Option<bool>,
    merge_status: Option<String>,
    detailed_merge_status: Option<String>,
    sha: Option<String>,
    should_remove_source_branch: Option<bool>,
    force_remove_source_branch: Option<bool>,
    web_url: Option<String>,
    squash: Option<bool>,
    squash_on_merge: Option<bool>,
    task_completion_status: Option<RawTasks>,
    has_conflicts: Option<bool>,
    blocking_discussions_resolved: Option<bool>,
    head_pipeline: Option<RawPipeline>,
    changes_count: Option<String>,
    user: Option<RawCanMerge>,
    merge_error: Option<String>,
}

fn convert_mr(m: RawMr) -> ChangeRequest {
    let s = |o: Option<String>| o.unwrap_or_default();
    ChangeRequest {
        number: m.iid,
        title: s(m.title),
        description: s(m.description),
        author: m.author.as_ref().map(RawUser::person).unwrap_or_default(),
        source: s(m.source_branch),
        target: s(m.target_branch),
        draft: m.draft.or(m.work_in_progress).unwrap_or(false),
        state: s(m.state),
        created: time(m.created_at.as_deref()),
        updated: time(m.updated_at.as_deref()),
        web_url: s(m.web_url),
        sha: s(m.sha),
        labels: m.labels.unwrap_or_default(),
        assignees: m
            .assignees
            .unwrap_or_default()
            .iter()
            .map(RawUser::person)
            .collect(),
        reviewers: m
            .reviewers
            .unwrap_or_default()
            .iter()
            .map(|u| Reviewer {
                person: u.person(),
                ..Default::default()
            })
            .collect(),
        merge_status: m
            .detailed_merge_status
            .filter(|x| !x.is_empty())
            .or(m.merge_status)
            .unwrap_or_default(),
        conflicts: m.has_conflicts.unwrap_or(false),
        comments: m.user_notes_count,
        upvotes: m.upvotes,
        downvotes: m.downvotes,
        squash: m.squash.or(m.squash_on_merge).unwrap_or(false),
        remove_source: m
            .should_remove_source_branch
            .or(m.force_remove_source_branch)
            .unwrap_or(false),
        auto_merge: m.merge_when_pipeline_succeeds.unwrap_or(false),
        pipeline: m.head_pipeline.map(|p| PipelineRef {
            id: p.id,
            status: p.status.unwrap_or_default(),
            web_url: p.web_url.unwrap_or_default(),
        }),
        tasks: m
            .task_completion_status
            .filter(|t| t.count > 0)
            .map(|t| (t.completed_count, t.count)),
        changes_count: m.changes_count,
        can_merge: m.user.and_then(|u| u.can_merge),
        merge_error: m.merge_error.filter(|e| !e.is_empty()),
        blocking_discussions_resolved: m.blocking_discussions_resolved.unwrap_or(true),
    }
}

fn decode<T: serde::de::DeserializeOwned>(body: &[u8], what: &str) -> Result<T> {
    serde_json::from_slice(body).map_err(|e| Error::new(format!("GitLab {what}: {e}")))
}

/// The signed-in user's username.
pub fn parse_user(body: &[u8]) -> Result<Person> {
    let u: RawUser = decode(body, "user")?;
    let _ = u.id;
    Ok(u.person())
}

pub fn parse_mrs(body: &[u8]) -> Result<Vec<ChangeRequest>> {
    let raw: Vec<RawMr> = decode(body, "merge requests")?;
    Ok(raw.into_iter().map(convert_mr).collect())
}

pub fn parse_mr(body: &[u8]) -> Result<ChangeRequest> {
    Ok(convert_mr(decode(body, "merge request")?))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawApprover {
    user: RawUser,
    approved_at: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawApprovals {
    approved: Option<bool>,
    approvals_required: Option<u64>,
    approvals_left: Option<u64>,
    approved_by: Option<Vec<RawApprover>>,
}

pub fn parse_approvals(body: &[u8]) -> Result<Approvals> {
    let r: RawApprovals = decode(body, "approvals")?;
    let by: Vec<(Person, Option<i64>)> = r
        .approved_by
        .unwrap_or_default()
        .iter()
        .map(|a| (a.user.person(), time(a.approved_at.as_deref())))
        .collect();
    Ok(Approvals {
        approved: r.approved.unwrap_or(!by.is_empty()),
        required: r.approvals_required,
        left: r.approvals_left,
        by,
    })
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawNote {
    body: Option<String>,
    author: Option<RawUser>,
    created_at: Option<String>,
    system: bool,
    resolvable: bool,
    resolved: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawDiscussion {
    id: String,
    notes: Vec<RawNote>,
}

pub fn parse_discussions(body: &[u8]) -> Result<Vec<Thread>> {
    let raw: Vec<RawDiscussion> = decode(body, "discussions")?;
    Ok(raw
        .into_iter()
        .map(|d| {
            let resolvable = d.notes.iter().any(|n| n.resolvable);
            let resolved = resolvable
                && d.notes
                    .iter()
                    .filter(|n| n.resolvable)
                    .all(|n| n.resolved.unwrap_or(false));
            Thread {
                id: d.id,
                resolvable,
                resolved,
                notes: d
                    .notes
                    .into_iter()
                    .map(|n| Note {
                        author: n.author.as_ref().map(RawUser::person).unwrap_or_default(),
                        body: n.body.unwrap_or_default(),
                        created: time(n.created_at.as_deref()),
                        system: n.system,
                    })
                    .collect(),
            }
        })
        .collect())
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawDiff {
    old_path: String,
    new_path: String,
    diff: Option<String>,
    new_file: bool,
    renamed_file: bool,
    deleted_file: bool,
}

pub fn parse_diffs(body: &[u8]) -> Result<Vec<FileChange>> {
    let raw: Vec<RawDiff> = decode(body, "changes")?;
    Ok(raw
        .into_iter()
        .map(|d| {
            let (a, del) = count_diff(d.diff.as_deref().unwrap_or(""));
            FileChange {
                old_path: d.old_path,
                new_path: d.new_path,
                new_file: d.new_file,
                deleted: d.deleted_file,
                renamed: d.renamed_file,
                additions: a,
                deletions: del,
            }
        })
        .collect())
}

fn convert_pipeline(p: RawPipeline) -> Pipeline {
    let status = p.status.unwrap_or_default();
    let started = time(p.started_at.as_deref());
    let finished = time(p.finished_at.as_deref());
    Pipeline {
        id: p.id,
        number: p.iid.map(|i| i.to_string()).unwrap_or_default(),
        name: p.name.unwrap_or_default(),
        definition_id: None,
        outcome: gitlab_outcome(&status),
        status: words(&status),
        reference: short_ref(p.reference.as_deref().unwrap_or("")),
        sha: p.sha.unwrap_or_default(),
        source: words(p.source.as_deref().unwrap_or("")),
        created: time(p.created_at.as_deref()),
        started,
        finished: finished.or_else(|| {
            // Lists carry no finish time: the last update of a finished
            // pipeline is close enough for "how long ago".
            (!gitlab_outcome(&status).is_running())
                .then(|| time(p.updated_at.as_deref()))
                .flatten()
        }),
        duration: p
            .duration
            .map(|d| d as i64)
            .or_else(|| span(started, finished)),
        queued: p.queued_duration.map(|d| d as i64),
        user: p.user.map(|u| u.person().display()).unwrap_or_default(),
        web_url: p.web_url.unwrap_or_default(),
    }
}

pub fn parse_pipelines(body: &[u8]) -> Result<Vec<Pipeline>> {
    let raw: Vec<RawPipeline> = decode(body, "pipelines")?;
    Ok(raw.into_iter().map(convert_pipeline).collect())
}

pub fn parse_pipeline(body: &[u8]) -> Result<Pipeline> {
    Ok(convert_pipeline(decode(body, "pipeline")?))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawRunner {
    description: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawJob {
    id: u64,
    status: Option<String>,
    stage: Option<String>,
    name: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
    duration: Option<f64>,
    queued_duration: Option<f64>,
    allow_failure: bool,
    failure_reason: Option<String>,
    web_url: Option<String>,
    runner: Option<RawRunner>,
}

/// Jobs of a pipeline in stage order as GitLab lists them (newest first
/// within the API answer; sorted here by stage appearance, then id).
pub fn parse_jobs(body: &[u8]) -> Result<Vec<Job>> {
    let raw: Vec<RawJob> = decode(body, "jobs")?;
    let mut jobs: Vec<Job> = raw
        .into_iter()
        .map(|j| {
            let status = j.status.unwrap_or_default();
            let started = time(j.started_at.as_deref());
            let finished = time(j.finished_at.as_deref());
            Job {
                id: j.id,
                name: j.name.unwrap_or_default(),
                stage: j.stage.unwrap_or_default(),
                outcome: gitlab_outcome(&status),
                status: words(&status),
                started,
                finished,
                duration: j
                    .duration
                    .map(|d| d as i64)
                    .or_else(|| span(started, finished)),
                queued: j.queued_duration.map(|d| d as i64),
                allow_failure: j.allow_failure,
                failure_reason: words(j.failure_reason.as_deref().unwrap_or("")),
                web_url: j.web_url.unwrap_or_default(),
                runner: j.runner.and_then(|r| r.description).unwrap_or_default(),
                log_id: Some(j.id),
                ..Default::default()
            }
        })
        .collect();
    // Stages in the order of their first (oldest) job.
    let mut order: Vec<(String, u64)> = Vec::new();
    for j in &jobs {
        match order.iter_mut().find(|(s, _)| *s == j.stage) {
            Some((_, min)) => *min = (*min).min(j.id),
            None => order.push((j.stage.clone(), j.id)),
        }
    }
    order.sort_by_key(|(_, min)| *min);
    let rank = |s: &str| order.iter().position(|(x, _)| x == s).unwrap_or(usize::MAX);
    jobs.sort_by(|a, b| rank(&a.stage).cmp(&rank(&b.stage)).then(a.id.cmp(&b.id)));
    Ok(jobs)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::devops::forge::detect;
    use crate::github::Outcome;

    pub(crate) fn remote() -> ForgeRemote {
        detect("https://gitlab.com/gitlab-org/gitlab-runner.git").unwrap()
    }

    fn own() -> ForgeRemote {
        detect("git@gitlab.example.org:team/sub/app.git").unwrap()
    }

    pub(crate) const MRS: &str = r#"[{"id":555685286,"iid":7713,"project_id":250833,
      "title":"Add Magalu Cloud to the community fleeting plugins","description":null,
      "state":"opened","created_at":"2026-10-26T15:13:38.604Z","updated_at":"2026-10-26T17:00:02.270Z",
      "merged_by":null,"target_branch":"main","source_branch":"docs-add-magalu","user_notes_count":7,
      "upvotes":1,"downvotes":0,
      "author":{"id":1356891,"username":"danilo","name":"Danilo D","state":"active"},
      "assignees":[{"id":1356891,"username":"danilo","name":"Danilo D"}],
      "reviewers":[{"id":21826781,"username":"GitLabDuo","name":"GitLab Duo"},{"id":2,"username":"z_painter","name":"Zachary Painter"}],
      "labels":["Community contribution","docs-only"],"draft":false,"work_in_progress":false,
      "milestone":null,"merge_when_pipeline_succeeds":false,"merge_status":"can_be_merged",
      "detailed_merge_status":"not_approved","sha":"8c70a72e95ccfe3b9dac33426f094603146dbd3d",
      "should_remove_source_branch":null,"force_remove_source_branch":true,
      "web_url":"https://gitlab.com/gitlab-org/gitlab-runner/-/merge_requests/7713",
      "squash":true,"squash_on_merge":true,"task_completion_status":{"count":8,"completed_count":1},
      "has_conflicts":false,"blocking_discussions_resolved":true},
      {"iid":12,"title":"Draft: wip","draft":true,"state":"opened","source_branch":"x","target_branch":"main",
       "author":null,"labels":[],"assignees":[],"reviewers":[]}]"#;

    #[test]
    fn request_builders() {
        let r = remote();
        assert_eq!(project_id(&r), "gitlab-org%2Fgitlab-runner");
        let l = merge_requests(&r);
        assert_eq!(
            l.endpoint,
            "projects/gitlab-org%2Fgitlab-runner/merge_requests?state=opened&per_page=50"
        );
        assert_eq!(
            l.args(),
            vec![
                "api",
                "projects/gitlab-org%2Fgitlab-runner/merge_requests?state=opened&per_page=50"
            ]
        );
        assert_eq!(
            l.public_url(),
            "https://gitlab.com/api/v4/projects/gitlab-org%2Fgitlab-runner/merge_requests?state=opened&per_page=50"
        );
        assert!(!l.call().write);
        assert!(l.public_call().is_some());

        let o = own();
        assert_eq!(project_id(&o), "team%2Fsub%2Fapp");
        let p = pipelines(&o, Some("feature/x"));
        assert_eq!(
            p.args(),
            vec![
                "api",
                "--hostname",
                "gitlab.example.org",
                "projects/team%2Fsub%2Fapp/pipelines?per_page=50&ref=feature%2Fx"
            ]
        );
        assert_eq!(
            job_trace(&r, 9).endpoint,
            "projects/gitlab-org%2Fgitlab-runner/jobs/9/trace"
        );
        assert_eq!(
            pipeline_jobs(&r, 5).endpoint,
            "projects/gitlab-org%2Fgitlab-runner/pipelines/5/jobs?per_page=100"
        );
        assert_eq!(
            public_trace(&r, 9).args.last().unwrap(),
            "https://gitlab.com/gitlab-org/gitlab-runner/-/jobs/9/raw"
        );
        assert_eq!(
            auth_status(&o).display(),
            "glab auth status --hostname gitlab.example.org"
        );
        for g in [
            approvals(&r, 7),
            discussions(&r, 7),
            diffs(&r, 7),
            mr_pipelines(&r, 7),
            merge_request(&r, 7),
        ] {
            assert!(
                g.endpoint
                    .starts_with("projects/gitlab-org%2Fgitlab-runner/merge_requests/7"),
                "{}",
                g.endpoint
            );
            assert_eq!(g.method, Method::Get);
        }
    }

    #[test]
    fn write_builders() {
        let r = own();
        let new = NewMr {
            source: "feature".into(),
            target: "main".into(),
            title: "Fix login".into(),
            description: "Line 1\nLine 2".into(),
            draft: true,
            remove_source: true,
            squash: false,
            assignee_ids: vec!["12".into()],
            reviewer_ids: vec![" 34 ".into(), "56".into()],
            labels: vec!["bug".into(), "ui".into()],
        };
        let c = create_mr(&r, &new).unwrap();
        assert_eq!(c.method, Method::Post);
        assert_eq!(c.endpoint, "projects/team%2Fsub%2Fapp/merge_requests");
        assert_eq!(
            c.body,
            Some(json!({
                "source_branch": "feature", "target_branch": "main", "title": "Draft: Fix login",
                "description": "Line 1\nLine 2", "remove_source_branch": true, "squash": false,
                "assignee_ids": [12], "reviewer_ids": [34, 56], "labels": "bug,ui"
            }))
        );
        assert_eq!(
            c.args(),
            vec![
                "api",
                "--hostname",
                "gitlab.example.org",
                "--method",
                "POST",
                "-H",
                "Content-Type: application/json",
                "--input",
                "-",
                "projects/team%2Fsub%2Fapp/merge_requests"
            ]
        );
        let call = c.call();
        assert!(call.write);
        assert!(call.display().contains("--input -"));
        assert!(
            !call.display().contains("Fix login"),
            "the body never reaches the command line"
        );
        let bad = NewMr {
            assignee_ids: vec!["ada".into()],
            ..new.clone()
        };
        assert!(
            create_mr(&r, &bad)
                .unwrap_err()
                .to_string()
                .contains("`ada`")
        );
        assert!(
            create_mr(
                &r,
                &NewMr {
                    target: "feature".into(),
                    ..new.clone()
                }
            )
            .is_err()
        );
        assert!(
            create_mr(
                &r,
                &NewMr {
                    title: " ".into(),
                    ..new.clone()
                }
            )
            .is_err()
        );

        let m = merge(
            &r,
            7,
            &MergeOpts {
                squash: true,
                remove_source: true,
                when_pipeline_succeeds: true,
                sha: Some("abc".into()),
                message: "Squashed".into(),
            },
        );
        assert_eq!(m.method, Method::Put);
        assert_eq!(
            m.endpoint,
            "projects/team%2Fsub%2Fapp/merge_requests/7/merge"
        );
        assert_eq!(
            m.body,
            Some(
                json!({"squash": true, "should_remove_source_branch": true, "merge_when_pipeline_succeeds": true, "sha": "abc", "squash_commit_message": "Squashed"})
            )
        );
        let plain = merge(
            &r,
            7,
            &MergeOpts {
                message: "Merge it".into(),
                ..Default::default()
            },
        );
        assert_eq!(plain.body.unwrap()["merge_commit_message"], "Merge it");

        let a = approve(&r, 7, Some("abc"));
        assert_eq!(
            (a.method, a.endpoint.as_str()),
            (
                Method::Post,
                "projects/team%2Fsub%2Fapp/merge_requests/7/approve"
            )
        );
        assert_eq!(a.body, Some(json!({"sha": "abc"})));
        assert_eq!(approve(&r, 7, None).body, None);
        assert!(
            unapprove(&r, 7)
                .endpoint
                .ends_with("/merge_requests/7/unapprove")
        );
        let cm = comment(&r, 7, "LGTM").unwrap();
        assert_eq!(cm.body, Some(json!({"body": "LGTM"})));
        assert!(cm.endpoint.ends_with("/merge_requests/7/discussions"));
        assert!(comment(&r, 7, "  ").is_err());

        assert!(
            retry_pipeline(&r, 5)
                .endpoint
                .ends_with("/pipelines/5/retry")
        );
        assert!(
            cancel_pipeline(&r, 5)
                .endpoint
                .ends_with("/pipelines/5/cancel")
        );
        assert!(retry_job(&r, 9).endpoint.ends_with("/jobs/9/retry"));
        assert!(cancel_job(&r, 9).endpoint.ends_with("/jobs/9/cancel"));
        for w in [
            retry_pipeline(&r, 5),
            cancel_pipeline(&r, 5),
            retry_job(&r, 9),
        ] {
            assert_eq!(w.method, Method::Post);
            assert!(w.public_call().is_none(), "writes never go through curl");
        }

        let run = run_pipeline(
            &r,
            "release/1.0",
            &[
                Var {
                    key: "DEPLOY".into(),
                    value: "yes".into(),
                    secret: false,
                },
                Var {
                    key: "TOKEN".into(),
                    value: "hunter2".into(),
                    secret: true,
                },
                Var {
                    key: " ".into(),
                    value: "ignored".into(),
                    secret: false,
                },
            ],
        )
        .unwrap();
        assert_eq!(
            run.endpoint,
            "projects/team%2Fsub%2Fapp/pipeline?ref=release%2F1.0"
        );
        let body = run.body.clone().unwrap();
        assert_eq!(body["variables"].as_array().unwrap().len(), 2);
        assert_eq!(body["variables"][1]["value"], "hunter2");
        let call = run.call();
        let shown = call.body_preview().unwrap();
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("yes") && shown.contains(MASK));
        assert!(!call.display().contains("hunter2"));
        assert!(run_pipeline(&r, " ", &[]).is_err());
        assert_eq!(draft_title("Draft: x", true), "Draft: x");
        assert_eq!(draft_title("x", false), "x");
    }

    #[test]
    fn parses_merge_requests() {
        let mrs = parse_mrs(MRS.as_bytes()).unwrap();
        assert_eq!(mrs.len(), 2);
        let m = &mrs[0];
        assert_eq!(m.number, 7713);
        assert_eq!(m.description, "");
        assert_eq!(m.author, Person::new("Danilo D", "danilo"));
        assert_eq!(m.reviewers.len(), 2);
        assert_eq!(m.reviewers[1].person.login, "z_painter");
        assert_eq!(m.merge_status, "not_approved");
        assert!(m.squash && m.remove_source && !m.auto_merge && !m.draft);
        assert_eq!(m.tasks, Some((1, 8)));
        assert_eq!(m.comments, 7);
        assert_eq!(m.upvotes, 1);
        assert!(m.created.is_some());
        assert!(mrs[1].draft);
        assert_eq!(mrs[1].author, Person::default());

        let detail = r#"{"iid":7,"title":"t","head_pipeline":{"id":99,"status":"failed","web_url":"https://x/p/99"},
          "changes_count":"1000+","user":{"can_merge":false},"merge_error":"","detailed_merge_status":"ci_must_pass"}"#;
        let d = parse_mr(detail.as_bytes()).unwrap();
        assert_eq!(
            d.pipeline,
            Some(PipelineRef {
                id: 99,
                status: "failed".into(),
                web_url: "https://x/p/99".into()
            })
        );
        assert_eq!(d.changes_count.as_deref(), Some("1000+"));
        assert_eq!(d.can_merge, Some(false));
        assert_eq!(d.merge_error, None);
    }

    #[test]
    fn parses_approvals_threads_and_diffs() {
        let a = r#"{"id":1,"iid":7713,"approved":false,"approvals_required":2,"approvals_left":1,
          "approved_by":[{"user":{"id":3,"username":"GitLabDuo","name":"GitLab Duo"},"approved_at":"2026-10-26T16:28:06.381Z"}]}"#;
        let ap = parse_approvals(a.as_bytes()).unwrap();
        assert!(!ap.approved);
        assert_eq!((ap.required, ap.left), (Some(2), Some(1)));
        assert_eq!(ap.by[0].0.login, "GitLabDuo");
        assert!(ap.by[0].1.is_some());

        let d = r#"[
          {"id":"6a9c","individual_note":false,"notes":[
            {"id":1,"body":"Please rename","author":{"id":1,"username":"ada","name":"Ada"},"created_at":"2026-10-26T10:00:00Z","system":false,"resolvable":true,"resolved":false},
            {"id":2,"body":"Done","author":{"id":2,"username":"bob","name":"Bob"},"created_at":"2026-10-26T11:00:00Z","system":false,"resolvable":true,"resolved":false}]},
          {"id":"77aa","individual_note":true,"notes":[
            {"id":3,"body":"added 1 commit","author":{"id":2,"username":"bob","name":"Bob"},"system":true,"resolvable":false}]}
        ]"#;
        let t = parse_discussions(d.as_bytes()).unwrap();
        assert_eq!(t.len(), 2);
        assert!(t[0].resolvable && !t[0].resolved);
        assert_eq!(t[0].notes[1].author.login, "bob");
        assert!(t[1].notes[0].system);
        let s = super::super::model::thread_stats(&t);
        assert_eq!((s.threads, s.unresolved, s.system), (1, 1, 1));

        let f = r#"[{"old_path":"a.md","new_path":"a.md","diff":"@@ -1 +1,2 @@\n-x\n+y\n+z\n","new_file":false,"renamed_file":false,"deleted_file":false},
                    {"old_path":"b","new_path":"c","diff":null,"new_file":false,"renamed_file":true,"deleted_file":false}]"#;
        let files = parse_diffs(f.as_bytes()).unwrap();
        assert_eq!((files[0].additions, files[0].deletions), (2, 1));
        assert!(files[1].renamed);
    }

    pub(crate) const PIPELINES: &str = r#"[
      {"id":2991293961,"iid":54071,"project_id":250833,"sha":"5e92a1de","ref":"refs/merge-requests/6712/merge","status":"running",
       "source":"merge_request_event","created_at":"2026-10-26T18:56:31.347Z","updated_at":"2026-10-26T18:56:33.882Z",
       "web_url":"https://gitlab.com/gitlab-org/gitlab-runner/-/pipelines/2991293961","name":null},
      {"id":2990778905,"iid":54067,"sha":"5fdcb391","ref":"main","status":"failed","source":"push",
       "created_at":"2026-10-26T16:38:47.051Z","updated_at":"2026-10-26T18:36:51.742Z",
       "web_url":"https://gitlab.com/gitlab-org/gitlab-runner/-/pipelines/2990778905"}]"#;

    #[test]
    fn parses_pipelines_and_jobs() {
        let p = parse_pipelines(PIPELINES.as_bytes()).unwrap();
        assert_eq!(p[0].outcome, Outcome::InProgress);
        assert_eq!(p[0].status, "running");
        assert_eq!(p[0].number, "54071");
        assert_eq!(p[0].source, "merge request event");
        assert_eq!(p[0].finished, None, "running: no finish time");
        assert_eq!(p[1].outcome, Outcome::Failure);
        assert_eq!(p[1].reference, "main");
        assert!(p[1].finished.is_some());

        let detail = r#"{"id":2990778905,"iid":54067,"status":"failed","ref":"main","sha":"5fdc",
          "user":{"id":1,"username":"rasam","name":"Rasam H"},"started_at":"2026-10-26T16:38:50.105Z",
          "finished_at":"2026-10-26T18:36:51.716Z","duration":7076,"queued_duration":3}"#;
        let d = parse_pipeline(detail.as_bytes()).unwrap();
        assert_eq!(d.duration, Some(7076));
        assert_eq!(d.queued, Some(3));
        assert_eq!(d.user, "Rasam H (rasam)");

        let jobs = r#"[
          {"id":30,"status":"failed","stage":"test","name":"unit 1/2","started_at":"2026-10-26T18:02:50.625Z",
           "finished_at":"2026-10-26T18:14:44.781Z","duration":714.15,"queued_duration":0.4,"allow_failure":false,
           "failure_reason":"script_failure","web_url":"https://gitlab.com/x/-/jobs/30","runner":{"description":"windows 1809"}},
          {"id":10,"status":"success","stage":"build","name":"compile","duration":60.0,"allow_failure":false,"web_url":"w"},
          {"id":31,"status":"manual","stage":"deploy","name":"release","allow_failure":true,"web_url":"w"},
          {"id":20,"status":"skipped","stage":"test","name":"lint","allow_failure":true,"web_url":"w"}
        ]"#;
        let j = parse_jobs(jobs.as_bytes()).unwrap();
        let names: Vec<&str> = j.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["compile", "lint", "unit 1/2", "release"]);
        let unit = &j[2];
        assert_eq!(unit.duration, Some(714));
        assert_eq!(unit.failure_reason, "script failure");
        assert_eq!(unit.runner, "windows 1809");
        assert_eq!(unit.log_id, Some(30));
        assert_eq!(j[3].outcome, Outcome::Neutral);
        assert!(j[3].allow_failure);
    }

    #[test]
    fn user_and_status() {
        let u = parse_user(br#"{"id":5,"username":"ada","name":"Ada Lovelace"}"#).unwrap();
        assert_eq!(u.login, "ada");
        assert_eq!(
            login_from_status(
                "gitlab.com\n  ✓ Logged in to gitlab.com as ada (C:\\Users\\a\\.config\\glab-cli\\config.yml)\n"
            ),
            Some("ada".into())
        );
        assert_eq!(login_from_status("not logged in"), None);
    }
}
