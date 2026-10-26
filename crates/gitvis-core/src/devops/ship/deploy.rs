//! Environments and deployments on GitHub: one GraphQL query for the
//! environments and their deployments (REST when GraphQL fails), what is
//! deployed where, pending approvals, and the writes (approve / reject,
//! re-run, create a deployment) as pure request builders.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use super::local::{self, LogLine};
use crate::devops::gh::{Gh, Method, Request, encode};
use crate::github::{ApiCall, parse_time};
use crate::{Error, Result};

/// Deployments in the list query (newest first, all environments).
pub const LIST_DEPLOYMENTS: u32 = 100;
/// Deployments per environment in a history query.
pub const HISTORY_DEPLOYMENTS: u32 = 50;
/// Environments whose current deployment is fetched in a follow-up query.
pub const FOLLOW_UP_ENVS: usize = 8;
/// REST fallback caps.
pub const REST_ENVS: usize = 8;
pub const REST_DEPLOYMENTS: usize = 5;
/// Waiting runs whose pending deployments are read.
pub const WAITING_RUNS: usize = 10;

// ---------------------------------------------------------------------------
// States
// ---------------------------------------------------------------------------

/// A deployment's (or deployment status's) state, from GraphQL
/// (`ACTIVE`) or REST (`success`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum DeployState {
    Success,
    /// The live deployment of its environment.
    Active,
    /// Succeeded once, replaced by a newer deployment since.
    Inactive,
    Failure,
    Error,
    Pending,
    Queued,
    InProgress,
    /// Waiting for an approval or a wait timer.
    Waiting,
    /// Never completed (e.g. its run was cancelled).
    Abandoned,
    /// Its environment was torn down after it succeeded.
    Destroyed,
    #[default]
    Unknown,
}

impl DeployState {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "success" => DeployState::Success,
            "active" => DeployState::Active,
            "inactive" => DeployState::Inactive,
            "failure" => DeployState::Failure,
            "error" => DeployState::Error,
            "pending" => DeployState::Pending,
            "queued" => DeployState::Queued,
            "in_progress" => DeployState::InProgress,
            "waiting" => DeployState::Waiting,
            "abandoned" => DeployState::Abandoned,
            "destroyed" => DeployState::Destroyed,
            _ => DeployState::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DeployState::Success => "Success",
            DeployState::Active => "Active",
            DeployState::Inactive => "Inactive",
            DeployState::Failure => "Failure",
            DeployState::Error => "Error",
            DeployState::Pending => "Pending",
            DeployState::Queued => "Queued",
            DeployState::InProgress => "In progress",
            DeployState::Waiting => "Waiting",
            DeployState::Abandoned => "Abandoned",
            DeployState::Destroyed => "Destroyed",
            DeployState::Unknown => "Unknown",
        }
    }

    /// It reached the environment (now or before).
    pub fn succeeded(self) -> bool {
        matches!(
            self,
            DeployState::Success
                | DeployState::Active
                | DeployState::Inactive
                | DeployState::Destroyed
        )
    }

    pub fn failed(self) -> bool {
        matches!(self, DeployState::Failure | DeployState::Error)
    }

    pub fn running(self) -> bool {
        matches!(
            self,
            DeployState::Pending
                | DeployState::Queued
                | DeployState::InProgress
                | DeployState::Waiting
        )
    }

    /// Completed one way or the other (counts in failure rates).
    pub fn completed(self) -> bool {
        self.succeeded() || self.failed()
    }
}

/// The state that describes a deployment: `ACTIVE` / `INACTIVE` /
/// `DESTROYED` from the deployment itself (they say whether it is live),
/// else the latest status (`SUCCESS`, `FAILURE`, …), else the deployment
/// state.
pub fn effective_state(deployment: DeployState, latest_status: Option<DeployState>) -> DeployState {
    match deployment {
        DeployState::Active | DeployState::Inactive | DeployState::Destroyed => deployment,
        _ => match latest_status {
            Some(s) if s != DeployState::Unknown => s,
            _ => deployment,
        },
    }
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeployStatus {
    pub state: DeployState,
    pub created: Option<i64>,
    /// The Actions job (or other log) behind the status.
    pub log_url: String,
    /// Where the deployed app lives.
    pub environment_url: String,
    pub description: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Deployment {
    pub id: u64,
    pub environment: String,
    pub sha: String,
    pub ref_name: String,
    pub task: String,
    pub description: String,
    pub creator: String,
    pub created: i64,
    pub updated: Option<i64>,
    /// The deployment's own state (GraphQL), or the latest status's (REST).
    pub state: DeployState,
    pub status: Option<DeployStatus>,
    /// Subject of the deployed commit (GraphQL only).
    pub commit_summary: String,
}

impl Deployment {
    pub fn effective(&self) -> DeployState {
        effective_state(self.state, self.status.as_ref().map(|s| s.state))
    }

    /// The Actions run that produced it, from the status log URL.
    pub fn run_id(&self) -> Option<u64> {
        self.status
            .as_ref()
            .and_then(|s| run_id_from_url(&s.log_url))
    }

    /// Seconds from creation to a successful status.
    pub fn duration_secs(&self) -> Option<i64> {
        let s = self.status.as_ref()?;
        if !s.state.succeeded() {
            return None;
        }
        Some((s.created? - self.created).max(0))
    }

    pub fn short_sha(&self) -> &str {
        &self.sha[..self.sha.len().min(8)]
    }
}

/// `…/actions/runs/123/job/456` → 123.
pub fn run_id_from_url(url: &str) -> Option<u64> {
    let (_, rest) = url.split_once("/actions/runs/")?;
    let id: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    id.parse().ok()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Environment {
    pub id: u64,
    pub name: String,
    /// The latest completed deployment (any outcome), from GraphQL.
    pub latest: Option<(u64, String, i64, DeployState)>,
    /// Protection rules, from REST (`Required reviewers: alice, ops`,
    /// `Wait timer: 5 min`, `Branch policy: protected branches`).
    pub protection: Vec<String>,
    pub html_url: String,
}

/// Where the environment data came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Source {
    #[default]
    GraphQl,
    Rest,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvData {
    pub environments: Vec<Environment>,
    /// Newest first, all environments.
    pub deployments: Vec<Deployment>,
    pub total_deployments: u64,
    pub default_branch: Option<String>,
    pub source: Source,
    /// Why the data came from REST, what was capped.
    pub notes: Vec<String>,
}

impl EnvData {
    /// Deployments of one environment, newest first.
    pub fn of<'a>(&'a self, env: &'a str) -> impl Iterator<Item = &'a Deployment> + 'a {
        self.deployments
            .iter()
            .filter(move |d| d.environment == env)
    }

    /// Add deployments (from a history query), keeping newest first and
    /// each id once.
    pub fn merge(&mut self, more: Vec<Deployment>) {
        for d in more {
            if !self.deployments.iter().any(|x| x.id == d.id) {
                self.deployments.push(d);
            }
        }
        self.deployments
            .sort_by_key(|d| std::cmp::Reverse((d.created, d.id)));
    }

    pub fn env(&self, name: &str) -> Option<&Environment> {
        self.environments.iter().find(|e| e.name == name)
    }
}

/// What runs in an environment: the newest `ACTIVE` deployment, else the
/// newest that succeeded. `deployments` is newest first.
pub fn current<'a>(deployments: &[&'a Deployment]) -> Option<&'a Deployment> {
    deployments
        .iter()
        .find(|d| d.state == DeployState::Active)
        .or_else(|| deployments.iter().find(|d| d.effective().succeeded()))
        .copied()
}

/// The roll-back target: the newest successful deployment older than
/// `from` with a different commit.
pub fn previous_success<'a>(
    deployments: &[&'a Deployment],
    from: &Deployment,
) -> Option<&'a Deployment> {
    deployments
        .iter()
        .filter(|d| d.created < from.created || (d.created == from.created && d.id < from.id))
        .find(|d| d.effective().succeeded() && d.sha != from.sha)
        .copied()
}

// ---------------------------------------------------------------------------
// GraphQL
// ---------------------------------------------------------------------------

const DEPLOYMENT_FIELDS: &str = "databaseId environment createdAt updatedAt commitOid task description state \
     ref { name } creator { login } commit { messageHeadline } \
     latestStatus { state createdAt logUrl environmentUrl description }";

/// The one query of the Environments tab: the default branch, the
/// environments (with their latest completed deployment) and the newest
/// deployments of all environments.
pub fn list_query() -> String {
    format!(
        "query($owner: String!, $name: String!, $n: Int!) {{\n  repository(owner: $owner, name: $name) {{\n    \
         defaultBranchRef {{ name }}\n    \
         environments(first: 50) {{ totalCount nodes {{ databaseId name \
         latestCompletedDeployment {{ databaseId commitOid createdAt state }} }} }}\n    \
         deployments(first: $n, orderBy: {{field: CREATED_AT, direction: DESC}}) {{ totalCount nodes {{ {DEPLOYMENT_FIELDS} }} }}\n  }}\n}}"
    )
}

pub fn list_variables(gh: &Gh, n: u32) -> Value {
    json!({ "owner": gh.owner, "name": gh.repo, "n": n })
}

/// Deployments of several environments in one query (`e0`, `e1`, … as
/// aliases), newest first; with `after`, one environment's next page.
pub fn history_query(envs: usize, paged: bool) -> String {
    let mut vars = String::from("$owner: String!, $name: String!, $n: Int!");
    let mut body = String::new();
    for i in 0..envs.max(1) {
        vars.push_str(&format!(", $e{i}: [String!]"));
        let after = if paged && i == 0 {
            ", after: $after"
        } else {
            ""
        };
        body.push_str(&format!(
            "    e{i}: deployments(first: $n, environments: $e{i}{after}, orderBy: {{field: CREATED_AT, direction: DESC}}) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ {DEPLOYMENT_FIELDS} }} }}\n"
        ));
    }
    if paged {
        vars.push_str(", $after: String");
    }
    format!("query({vars}) {{\n  repository(owner: $owner, name: $name) {{\n{body}  }}\n}}")
}

pub fn history_variables(gh: &Gh, envs: &[String], n: u32, after: Option<&str>) -> Value {
    let mut v = json!({ "owner": gh.owner, "name": gh.repo, "n": n });
    for (i, e) in envs.iter().enumerate() {
        v[format!("e{i}")] = json!([e]);
    }
    if let Some(a) = after {
        v["after"] = json!(a);
    }
    v
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GqlStatus {
    state: String,
    created_at: Option<String>,
    log_url: Option<String>,
    environment_url: Option<String>,
    description: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GqlName {
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GqlLogin {
    login: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GqlCommit {
    message_headline: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GqlDeployment {
    database_id: u64,
    environment: Option<String>,
    created_at: String,
    updated_at: Option<String>,
    commit_oid: Option<String>,
    task: Option<String>,
    description: Option<String>,
    state: Option<String>,
    #[serde(rename = "ref")]
    git_ref: Option<GqlName>,
    creator: Option<GqlLogin>,
    commit: Option<GqlCommit>,
    latest_status: Option<GqlStatus>,
}

impl GqlDeployment {
    fn into_model(self) -> Deployment {
        Deployment {
            id: self.database_id,
            environment: self.environment.unwrap_or_default(),
            sha: self.commit_oid.unwrap_or_default(),
            ref_name: self.git_ref.map(|r| r.name).unwrap_or_default(),
            task: self.task.unwrap_or_default(),
            description: self.description.unwrap_or_default(),
            creator: self.creator.map(|c| c.login).unwrap_or_default(),
            created: parse_time(&self.created_at).unwrap_or(0),
            updated: self.updated_at.as_deref().and_then(parse_time),
            state: DeployState::parse(self.state.as_deref().unwrap_or("")),
            status: self.latest_status.map(|s| DeployStatus {
                state: DeployState::parse(&s.state),
                created: s.created_at.as_deref().and_then(parse_time),
                log_url: s.log_url.unwrap_or_default(),
                environment_url: s.environment_url.unwrap_or_default(),
                description: s.description.unwrap_or_default(),
            }),
            commit_summary: self.commit.map(|c| c.message_headline).unwrap_or_default(),
        }
    }
}

fn deployments_of(v: &Value) -> Vec<Deployment> {
    v.get("nodes")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|n| !n.is_null())
                .filter_map(|n| serde_json::from_value::<GqlDeployment>(n.clone()).ok())
                .map(GqlDeployment::into_model)
                .collect()
        })
        .unwrap_or_default()
}

/// Parse the `data` of [`list_query`].
pub fn parse_list(data: &Value) -> Result<EnvData> {
    let repo = data
        .get("repository")
        .filter(|r| !r.is_null())
        .ok_or_else(|| Error::new("GitHub did not return the repository (no access?)"))?;
    let environments = repo
        .pointer("/environments/nodes")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|n| !n.is_null())
                .map(|n| {
                    let latest = n
                        .get("latestCompletedDeployment")
                        .filter(|l| !l.is_null())
                        .map(|l| {
                            (
                                l.get("databaseId").and_then(Value::as_u64).unwrap_or(0),
                                l.get("commitOid")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_owned(),
                                l.get("createdAt")
                                    .and_then(Value::as_str)
                                    .and_then(parse_time)
                                    .unwrap_or(0),
                                DeployState::parse(
                                    l.get("state").and_then(Value::as_str).unwrap_or(""),
                                ),
                            )
                        });
                    Environment {
                        id: n.get("databaseId").and_then(Value::as_u64).unwrap_or(0),
                        name: n
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        latest,
                        ..Default::default()
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let deployments = repo
        .get("deployments")
        .map(deployments_of)
        .unwrap_or_default();
    Ok(EnvData {
        environments,
        deployments,
        total_deployments: repo
            .pointer("/deployments/totalCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        default_branch: repo
            .pointer("/defaultBranchRef/name")
            .and_then(Value::as_str)
            .map(str::to_owned),
        source: Source::GraphQl,
        notes: Vec::new(),
    })
}

/// Parse the `data` of [`history_query`]: deployments per alias, and the
/// next-page cursor of `e0`.
pub fn parse_history(data: &Value, envs: usize) -> (Vec<Vec<Deployment>>, Option<String>) {
    let repo = data.get("repository").cloned().unwrap_or(Value::Null);
    let lists = (0..envs)
        .map(|i| {
            repo.get(format!("e{i}"))
                .map(deployments_of)
                .unwrap_or_default()
        })
        .collect();
    let next = repo
        .pointer("/e0/pageInfo")
        .filter(|p| p.get("hasNextPage").and_then(Value::as_bool) == Some(true))
        .and_then(|p| p.get("endCursor").and_then(Value::as_str))
        .map(str::to_owned);
    (lists, next)
}

/// Environments that need a follow-up for their current deployment: none
/// of their deployments in `data` succeeded.
pub fn missing_current(data: &EnvData) -> Vec<String> {
    data.environments
        .iter()
        .filter(|e| {
            let list: Vec<&Deployment> = data.of(&e.name).collect();
            current(&list).is_none() && e.latest.is_some()
        })
        .map(|e| e.name.clone())
        .take(FOLLOW_UP_ENVS)
        .collect()
}

/// Environments and deployments: the GraphQL query, then one follow-up
/// query for environments whose current deployment is older than the
/// newest page. Falls back to REST when GraphQL fails.
pub fn fetch(gh: &Gh, calls: &mut Vec<ApiCall>) -> Result<EnvData> {
    let gql = gh
        .graphql(&list_query(), list_variables(gh, LIST_DEPLOYMENTS), calls)
        .and_then(|d| parse_list(&d));
    let mut data = match gql {
        Ok(d) => d,
        Err(e) => {
            let mut d = fetch_rest(gh, calls)?;
            d.notes.insert(
                0,
                format!("GraphQL failed ({e}); read through REST instead."),
            );
            return Ok(d);
        }
    };
    let missing = missing_current(&data);
    if !missing.is_empty()
        && let Ok(d) = gh.graphql(
            &history_query(missing.len(), false),
            history_variables(gh, &missing, 20, None),
            calls,
        )
    {
        let (lists, _) = parse_history(&d, missing.len());
        data.merge(lists.into_iter().flatten().collect());
    }
    Ok(data)
}

/// One environment's history, newest first (`max_pages` pages of
/// `per_page`), stopping at deployments older than `since`.
pub fn fetch_history(
    gh: &Gh,
    env: &str,
    per_page: u32,
    max_pages: usize,
    since: Option<i64>,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<Deployment>> {
    let envs = vec![env.to_owned()];
    let mut out: Vec<Deployment> = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..max_pages.max(1) {
        let data = gh.graphql(
            &history_query(1, true),
            history_variables(gh, &envs, per_page, after.as_deref()),
            calls,
        )?;
        let (mut lists, next) = parse_history(&data, 1);
        let page = lists.pop().unwrap_or_default();
        let older = since.is_some_and(|s| page.last().is_some_and(|d| d.created < s));
        out.extend(page);
        match next {
            Some(n) if !older => after = Some(n),
            _ => break,
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// REST fallback
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct RestReviewer {
    #[serde(rename = "type")]
    kind: String,
    reviewer: Value,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RestRule {
    #[serde(rename = "type")]
    kind: String,
    wait_timer: Option<u64>,
    reviewers: Vec<RestReviewer>,
    prevent_self_review: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RestEnv {
    id: u64,
    name: String,
    html_url: String,
    protection_rules: Vec<RestRule>,
    deployment_branch_policy: Option<Value>,
}

fn reviewer_name(v: &Value) -> String {
    v.get("login")
        .or_else(|| v.get("slug"))
        .or_else(|| v.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_owned()
}

/// `Required reviewers: alice, ops (team)`, `Wait timer: 5 min`, …
fn protection_lines(e: &RestEnv) -> Vec<String> {
    let mut out = Vec::new();
    for r in &e.protection_rules {
        match r.kind.as_str() {
            "required_reviewers" => {
                let names: Vec<String> = r
                    .reviewers
                    .iter()
                    .map(|x| {
                        let n = reviewer_name(&x.reviewer);
                        if x.kind == "Team" {
                            format!("{n} (team)")
                        } else {
                            n
                        }
                    })
                    .collect();
                let mut line = format!("Required reviewers: {}", names.join(", "));
                if r.prevent_self_review == Some(true) {
                    line.push_str(" (no self-review)");
                }
                out.push(line);
            }
            "wait_timer" => out.push(format!("Wait timer: {} min", r.wait_timer.unwrap_or(0))),
            "branch_policy" => {}
            other => out.push(format!("Rule: {other}")),
        }
    }
    if let Some(p) = &e.deployment_branch_policy
        && !p.is_null()
    {
        let protected = p.get("protected_branches").and_then(Value::as_bool) == Some(true);
        out.push(if protected {
            "Branch policy: protected branches only".to_owned()
        } else {
            "Branch policy: selected branches and tags".to_owned()
        });
    }
    out
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RestDeployment {
    id: u64,
    sha: String,
    #[serde(rename = "ref")]
    git_ref: String,
    task: String,
    environment: String,
    description: Option<String>,
    creator: Option<GqlLogin>,
    created_at: String,
    updated_at: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RestStatus {
    state: String,
    created_at: Option<String>,
    log_url: Option<String>,
    target_url: Option<String>,
    environment_url: Option<String>,
    description: Option<String>,
}

fn rest_status(s: RestStatus) -> DeployStatus {
    DeployStatus {
        state: DeployState::parse(&s.state),
        created: s.created_at.as_deref().and_then(parse_time),
        log_url: s
            .log_url
            .filter(|u| !u.is_empty())
            .or(s.target_url)
            .unwrap_or_default(),
        environment_url: s.environment_url.unwrap_or_default(),
        description: s.description.unwrap_or_default(),
    }
}

/// Parse `GET environments`.
pub fn parse_rest_environments(v: &Value) -> Vec<Environment> {
    let list: Vec<RestEnv> = v
        .get("environments")
        .cloned()
        .and_then(|e| serde_json::from_value(e).ok())
        .unwrap_or_default();
    list.into_iter()
        .map(|e| Environment {
            id: e.id,
            protection: protection_lines(&e),
            html_url: e.html_url.clone(),
            name: e.name,
            latest: None,
        })
        .collect()
}

/// Parse one `GET deployments` item plus its newest status.
pub fn parse_rest_deployment(d: &Value, status: Option<&Value>) -> Option<Deployment> {
    let d: RestDeployment = serde_json::from_value(d.clone()).ok()?;
    let status = status
        .and_then(|s| serde_json::from_value::<RestStatus>(s.clone()).ok())
        .map(rest_status);
    Some(Deployment {
        id: d.id,
        environment: d.environment,
        sha: d.sha,
        ref_name: d.git_ref,
        task: d.task,
        description: d.description.unwrap_or_default(),
        creator: d.creator.map(|c| c.login).unwrap_or_default(),
        created: parse_time(&d.created_at).unwrap_or(0),
        updated: d.updated_at.as_deref().and_then(parse_time),
        // REST deployments have no state of their own.
        state: status.as_ref().map(|s| s.state).unwrap_or_default(),
        status,
        commit_summary: String::new(),
    })
}

/// The REST endpoints of the fallback (for tests and the activity log).
pub fn rest_endpoints(gh: &Gh, env: &str) -> (String, String) {
    (
        gh.path("environments?per_page=100"),
        gh.path(&format!(
            "deployments?environment={}&per_page={REST_DEPLOYMENTS}",
            encode(env)
        )),
    )
}

/// `deployments/{id}/statuses?per_page=1`.
pub fn status_endpoint(gh: &Gh, id: u64) -> String {
    gh.path(&format!("deployments/{id}/statuses?per_page=1"))
}

/// Environments and deployments through REST: `GET environments`, then
/// for each environment (capped) `GET deployments?environment=X` and the
/// newest status of each deployment (capped).
pub fn fetch_rest(gh: &Gh, calls: &mut Vec<ApiCall>) -> Result<EnvData> {
    let (envs_ep, _) = rest_endpoints(gh, "");
    let v: Value = gh.get_json(&envs_ep, "environments", calls)?;
    let mut environments = parse_rest_environments(&v);
    let mut notes = Vec::new();
    if environments.len() > REST_ENVS {
        notes.push(format!(
            "Deployments read for the first {REST_ENVS} of {} environments.",
            environments.len()
        ));
    }
    let mut deployments = Vec::new();
    for e in environments.iter_mut().take(REST_ENVS) {
        let (_, dep_ep) = rest_endpoints(gh, &e.name);
        let list: Vec<Value> = gh.get_json(&dep_ep, "deployments", calls)?;
        for d in &list {
            let id = d.get("id").and_then(Value::as_u64).unwrap_or(0);
            let statuses: Vec<Value> = gh
                .get_json(&status_endpoint(gh, id), "deployment statuses", calls)
                .unwrap_or_default();
            if let Some(dep) = parse_rest_deployment(d, statuses.first()) {
                deployments.push(dep);
            }
        }
        if let Some(first) = deployments.iter().find(|d| d.environment == e.name) {
            e.latest = Some((
                first.id,
                first.sha.clone(),
                first.created,
                first.effective(),
            ));
        }
    }
    deployments.sort_by_key(|d| std::cmp::Reverse(d.created));
    notes.push(format!(
        "REST shows the newest {REST_DEPLOYMENTS} deployments per environment."
    ));
    Ok(EnvData {
        total_deployments: deployments.len() as u64,
        environments,
        deployments,
        default_branch: None,
        source: Source::Rest,
        notes,
    })
}

// ---------------------------------------------------------------------------
// Pending approvals
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct WaitingRun {
    pub id: u64,
    pub name: String,
    pub display_title: String,
    pub head_sha: String,
    pub head_branch: Option<String>,
    pub html_url: String,
    pub run_number: u64,
    pub created_at: Option<String>,
    pub actor: Option<GhLogin>,
    pub event: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct GhLogin {
    pub login: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct PendingEnv {
    pub id: u64,
    pub name: String,
    pub html_url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reviewer {
    /// `User` or `Team`.
    pub kind: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PendingDeployment {
    pub environment: PendingEnv,
    /// Minutes.
    pub wait_timer: u64,
    pub wait_timer_started: Option<i64>,
    pub current_user_can_approve: bool,
    pub reviewers: Vec<Reviewer>,
}

/// A run waiting for approvals.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Approval {
    pub run: WaitingRun,
    pub pending: Vec<PendingDeployment>,
}

impl Approval {
    /// Environments the user can approve.
    pub fn approvable(&self) -> Vec<&PendingDeployment> {
        self.pending
            .iter()
            .filter(|p| p.current_user_can_approve)
            .collect()
    }
}

/// `actions/runs?status=waiting&per_page=N`.
pub fn waiting_runs_endpoint(gh: &Gh) -> String {
    gh.path(&format!(
        "actions/runs?status=waiting&per_page={WAITING_RUNS}"
    ))
}

/// `actions/runs/{id}/pending_deployments`.
pub fn pending_endpoint(gh: &Gh, run: u64) -> String {
    gh.path(&format!("actions/runs/{run}/pending_deployments"))
}

/// Parse `GET actions/runs/{id}/pending_deployments`.
pub fn parse_pending(v: &Value) -> Vec<PendingDeployment> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|p| PendingDeployment {
                    environment: p
                        .get("environment")
                        .cloned()
                        .and_then(|e| serde_json::from_value(e).ok())
                        .unwrap_or_default(),
                    wait_timer: p.get("wait_timer").and_then(Value::as_u64).unwrap_or(0),
                    wait_timer_started: p
                        .get("wait_timer_started_at")
                        .and_then(Value::as_str)
                        .and_then(parse_time),
                    current_user_can_approve: p
                        .get("current_user_can_approve")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    reviewers: p
                        .get("reviewers")
                        .and_then(Value::as_array)
                        .map(|r| {
                            r.iter()
                                .map(|x| Reviewer {
                                    kind: x
                                        .get("type")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_owned(),
                                    name: x.get("reviewer").map(reviewer_name).unwrap_or_default(),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Runs waiting for a deployment review and what each waits for.
pub fn fetch_approvals(gh: &Gh, calls: &mut Vec<ApiCall>) -> Result<Vec<Approval>> {
    let v: Value = gh.get_json(&waiting_runs_endpoint(gh), "waiting runs", calls)?;
    let runs: Vec<WaitingRun> = v
        .get("workflow_runs")
        .cloned()
        .and_then(|r| serde_json::from_value(r).ok())
        .unwrap_or_default();
    let mut out = Vec::new();
    for run in runs.into_iter().take(WAITING_RUNS) {
        let p: Value = gh.get_json(&pending_endpoint(gh, run.id), "pending deployments", calls)?;
        out.push(Approval {
            pending: parse_pending(&p),
            run,
        });
    }
    Ok(out)
}

/// POST `actions/runs/{id}/pending_deployments`: approve or reject the
/// run's deployments to these environments.
pub fn review_request(gh: &Gh, run: u64, env_ids: &[u64], approve: bool, comment: &str) -> Request {
    Request::post(
        pending_endpoint(gh, run),
        json!({
            "environment_ids": env_ids,
            "state": if approve { "approved" } else { "rejected" },
            "comment": comment,
        }),
    )
}

/// POST `actions/runs/{id}/rerun`: run the workflow again on the same
/// commit (it deploys what it deployed before).
pub fn rerun_request(gh: &Gh, run: u64) -> Request {
    Request::new(Method::Post, gh.path(&format!("actions/runs/{run}/rerun")))
}

/// POST `deployments`: ask for a deployment of `sha` to `env`, for
/// pipelines that listen to the `deployment` event.
pub fn create_deployment_request(gh: &Gh, sha: &str, env: &str, description: &str) -> Request {
    Request::post(
        gh.path("deployments"),
        json!({
            "ref": sha,
            "environment": env,
            "auto_merge": false,
            "required_contexts": [],
            "description": description,
        }),
    )
}

// ---------------------------------------------------------------------------
// Ordering
// ---------------------------------------------------------------------------

/// Where an environment sits in a typical pipeline, by name: development,
/// test, staging, production (others between test and staging).
pub fn stage_rank(name: &str) -> u8 {
    let n = name.to_ascii_lowercase();
    let has = |words: &[&str]| {
        words.iter().any(|w| {
            n == *w
                || n.split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|part| part == *w)
        })
    };
    // Pre-production first: `pre-prod` also contains the word `prod`.
    if has(&["preprod", "uat"]) || n.contains("pre-prod") || n.contains("pre_prod") {
        5
    } else if has(&[
        "prod",
        "production",
        "live",
        "prd",
        "release",
        "publish",
        "pypi",
    ]) {
        6
    } else if has(&["staging", "stage", "stg"]) {
        4
    } else if has(&["test", "testing", "qa", "integration", "int", "sandbox"]) {
        2
    } else if has(&["dev", "development", "local", "preview", "review"]) {
        1
    } else {
        3
    }
}

/// Environments GitHub creates for its own features (`github-pages`,
/// `copilot`): not stages of the delivery pipeline. They sort last and are
/// not compared with their neighbours.
pub fn is_side(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "github-pages" || n == "copilot" || n.starts_with("copilot-") || n == "pages"
}

/// Does the name look like production?
pub fn is_production(name: &str) -> bool {
    if stage_rank(name) != 6 {
        return false;
    }
    let n = name.to_ascii_lowercase();
    matches!(n.as_str(), "production" | "prod" | "live" | "prd")
        || n.split(|c: char| !c.is_ascii_alphanumeric())
            .any(|p| matches!(p, "production" | "prod" | "live"))
}

/// Pipeline order: the user's order first (names that exist), then the
/// rest by [`stage_rank`] and name.
pub fn order(names: &[String], user: &[String]) -> Vec<String> {
    let mut out: Vec<String> = user.iter().filter(|u| names.contains(u)).cloned().collect();
    let mut rest: Vec<&String> = names.iter().filter(|n| !out.contains(n)).collect();
    rest.sort_by(|a, b| {
        (is_side(a), stage_rank(a))
            .cmp(&(is_side(b), stage_rank(b)))
            .then_with(|| a.to_lowercase().cmp(&b.to_lowercase()))
    });
    out.extend(rest.into_iter().cloned());
    out
}

// ---------------------------------------------------------------------------
// Local facts
// ---------------------------------------------------------------------------

/// What local history says about one environment's current commit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvLocal {
    pub sha: String,
    /// The commit exists locally.
    pub present: bool,
    /// Commits on the default branch that are not deployed here.
    pub behind: Option<u32>,
    /// Against the next environment in the pipeline: commits here that
    /// are not there yet (count and the newest few).
    pub ahead_of_next: Option<(String, u32, Vec<LogLine>)>,
}

/// Local facts for each `(env, sha)` in pipeline order. `default` is the
/// remote default branch (`origin/main`).
pub fn local_facts(
    dir: &Path,
    ordered: &[(String, String)],
    default: Option<&str>,
    commands: &mut Vec<String>,
) -> HashMap<String, EnvLocal> {
    let shas: Vec<String> = ordered.iter().map(|(_, s)| s.clone()).collect();
    let present = local::resolve_many(dir, &shas, commands);
    let mut out = HashMap::new();
    for (i, (env, sha)) in ordered.iter().enumerate() {
        let here = present.get(i).cloned().flatten().is_some();
        let behind = match (here, default) {
            (true, Some(d)) => local::count(dir, &format!("{sha}..{d}"), commands),
            _ => None,
        };
        // The next pipeline environment downstream that has a local commit
        // (GitHub's own environments are not stages).
        let ahead_of_next = if here && !is_side(env) {
            ordered
                .iter()
                .enumerate()
                .skip(i + 1)
                .filter(|(_, (e, _))| !is_side(e))
                .find(|(j, _)| present.get(*j).cloned().flatten().is_some())
                .and_then(|(_, (next_env, next_sha))| {
                    let range = format!("{next_sha}..{sha}");
                    let n = local::count(dir, &range, commands)?;
                    let list =
                        local::log(dir, &range, 30, false, false, commands).unwrap_or_default();
                    Some((next_env.clone(), n, list))
                })
        } else {
            None
        };
        out.insert(
            env.clone(),
            EnvLocal {
                sha: sha.clone(),
                present: here,
                behind,
                ahead_of_next,
            },
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gh() -> Gh {
        Gh::new(".", "o", "r")
    }

    #[test]
    fn state_mapping() {
        assert_eq!(DeployState::parse("ACTIVE"), DeployState::Active);
        assert_eq!(DeployState::parse("in_progress"), DeployState::InProgress);
        assert_eq!(DeployState::parse("IN_PROGRESS"), DeployState::InProgress);
        assert_eq!(DeployState::parse("weird"), DeployState::Unknown);
        assert!(DeployState::Inactive.succeeded());
        assert!(DeployState::Destroyed.succeeded());
        assert!(DeployState::Error.failed());
        assert!(DeployState::Waiting.running());
        assert!(!DeployState::Abandoned.completed());
        // The deployment's own ACTIVE/INACTIVE wins; otherwise the status.
        assert_eq!(
            effective_state(DeployState::Active, Some(DeployState::Success)),
            DeployState::Active
        );
        assert_eq!(
            effective_state(DeployState::Failure, Some(DeployState::Error)),
            DeployState::Error
        );
        assert_eq!(
            effective_state(DeployState::Pending, None),
            DeployState::Pending
        );
        assert_eq!(
            effective_state(DeployState::Unknown, Some(DeployState::Success)),
            DeployState::Success
        );
    }

    #[test]
    fn run_ids_from_urls() {
        assert_eq!(
            run_id_from_url("https://github.com/cli/cli/actions/runs/40017149075/job/120636223290"),
            Some(40017149075)
        );
        assert_eq!(
            run_id_from_url("https://github.com/o/r/actions/runs/12"),
            Some(12)
        );
        assert_eq!(run_id_from_url("https://example.com/deploy/1"), None);
    }

    fn list_sample() -> Value {
        serde_json::from_str(r#"{"repository":{"defaultBranchRef":{"name":"trunk"},
          "environments":{"totalCount":2,"nodes":[
            {"databaseId":996140977,"name":"production","latestCompletedDeployment":{"databaseId":7287944018,"commitOid":"4f2cfe92e3499f8581bf39db77e0a1431fa7b55c","createdAt":"2026-10-23T20:19:52Z","state":"ACTIVE"}},
            {"databaseId":1026944764,"name":"staging","latestCompletedDeployment":{"databaseId":3785921393,"commitOid":"9c844a7ce7b2898df602d16efddaa842633905aa","createdAt":"2026-02-06T15:10:19Z","state":"ERROR"}}]},
          "deployments":{"totalCount":3630,"nodes":[
            {"databaseId":7287944018,"environment":"production","createdAt":"2026-10-23T20:19:52Z","updatedAt":"2026-10-23T20:24:33Z",
             "commitOid":"4f2cfe92e3499f8581bf39db77e0a1431fa7b55c","task":"deploy","description":null,"state":"ACTIVE","ref":{"name":"trunk"},
             "creator":{"login":"BagToad"},"commit":{"messageHeadline":"Merge pull request #14805"},
             "latestStatus":{"state":"SUCCESS","createdAt":"2026-10-23T20:24:33Z","logUrl":"https://github.com/cli/cli/actions/runs/40017149075/job/120636223290","environmentUrl":null,"description":null}},
            {"databaseId":7283149239,"environment":"production","createdAt":"2026-10-23T16:17:47Z","commitOid":"64d175234b00000000000000000000000000000","state":"INACTIVE","ref":{"name":"trunk"},
             "creator":{"login":"babakks"},"latestStatus":{"state":"INACTIVE","createdAt":"2026-10-23T20:24:33Z"}},
            null]}}}"#).unwrap()
    }

    #[test]
    fn parses_the_list_query() {
        let d = parse_list(&list_sample()).unwrap();
        assert_eq!(d.default_branch.as_deref(), Some("trunk"));
        assert_eq!(d.total_deployments, 3630);
        assert_eq!(d.environments.len(), 2);
        assert_eq!(
            d.environments[1].latest.as_ref().unwrap().3,
            DeployState::Error
        );
        assert_eq!(d.deployments.len(), 2);
        let first = &d.deployments[0];
        assert_eq!(first.creator, "BagToad");
        assert_eq!(first.ref_name, "trunk");
        assert_eq!(first.run_id(), Some(40017149075));
        assert_eq!(first.duration_secs(), Some(281));
        assert_eq!(first.effective(), DeployState::Active);
        let prod: Vec<&Deployment> = d.of("production").collect();
        assert_eq!(current(&prod).unwrap().id, 7287944018);
        assert_eq!(previous_success(&prod, prod[0]).unwrap().id, 7283149239);
        // Staging's current deployment is not in the page: follow up.
        assert_eq!(missing_current(&d), vec!["staging".to_owned()]);
        assert!(parse_list(&json!({"repository": null})).is_err());
    }

    #[test]
    fn queries_are_well_formed() {
        let q = list_query();
        assert!(q.contains("environments(first: 50)"));
        assert!(
            q.contains("deployments(first: $n, orderBy: {field: CREATED_AT, direction: DESC})")
        );
        assert!(q.contains("latestStatus { state createdAt logUrl environmentUrl description }"));
        assert_eq!(
            list_variables(&gh(), 100),
            json!({"owner": "o", "name": "r", "n": 100})
        );
        let h = history_query(2, false);
        assert!(h.contains("$e0: [String!], $e1: [String!]"));
        assert!(h.contains("e1: deployments(first: $n, environments: $e1, orderBy"));
        assert!(!h.contains("$after"));
        let p = history_query(1, true);
        assert!(p.contains("environments: $e0, after: $after"));
        assert!(p.contains(", $after: String)"));
        assert_eq!(
            history_variables(&gh(), &["staging".into()], 50, Some("CUR")),
            json!({"owner": "o", "name": "r", "n": 50, "e0": ["staging"], "after": "CUR"})
        );
        // Braces balance.
        for q in [q, h, p] {
            assert_eq!(q.matches('{').count(), q.matches('}').count(), "{q}");
        }
    }

    #[test]
    fn parses_history_pages() {
        let v = json!({"repository": {
            "e0": {"pageInfo": {"hasNextPage": true, "endCursor": "abc"}, "nodes": [
                {"databaseId": 1, "environment": "staging", "createdAt": "2026-01-01T00:00:00Z", "commitOid": "aa", "state": "INACTIVE"}]},
            "e1": {"pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": []}}});
        let (lists, next) = parse_history(&v, 2);
        assert_eq!(lists[0].len(), 1);
        assert!(lists[1].is_empty());
        assert_eq!(next.as_deref(), Some("abc"));
        let mut d = parse_list(&list_sample()).unwrap();
        d.merge(lists.into_iter().flatten().collect());
        let staging: Vec<&Deployment> = d.of("staging").collect();
        assert_eq!(current(&staging).unwrap().sha, "aa");
        assert!(missing_current(&d).is_empty());
    }

    #[test]
    fn rest_fallback_parsing() {
        let envs = json!({"total_count": 1, "environments": [{"id": 161088068, "name": "staging",
            "html_url": "https://github.com/o/r/deployments/activity_log?environments_filter=staging",
            "protection_rules": [
                {"type": "required_reviewers", "prevent_self_review": true, "reviewers": [
                    {"type": "User", "reviewer": {"login": "octocat"}},
                    {"type": "Team", "reviewer": {"slug": "ops", "name": "Ops"}}]},
                {"type": "wait_timer", "wait_timer": 30},
                {"type": "branch_policy"}],
            "deployment_branch_policy": {"protected_branches": true, "custom_branch_policies": false}}]});
        let e = parse_rest_environments(&envs);
        assert_eq!(e[0].name, "staging");
        assert_eq!(
            e[0].protection,
            vec![
                "Required reviewers: octocat, ops (team) (no self-review)",
                "Wait timer: 30 min",
                "Branch policy: protected branches only"
            ]
        );
        let dep = json!({"id": 6033283091u64, "sha": "8b19813f2bfca99f1018a587a8cf54fc959f2e5d", "ref": "8.5.0",
            "task": "deploy", "environment": "publish", "description": null,
            "creator": {"login": "Rowlando13"}, "created_at": "2026-08-22T04:42:00Z", "updated_at": "2026-08-26T13:33:18Z"});
        let status = json!({"state": "success", "created_at": "2026-08-26T13:33:18Z", "description": "",
            "environment": "publish", "log_url": "",
            "target_url": "https://github.com/pallets/click/actions/runs/32552371593/job/96981108675"});
        let d = parse_rest_deployment(&dep, Some(&status)).unwrap();
        assert_eq!(d.state, DeployState::Success);
        assert_eq!(d.run_id(), Some(32552371593));
        assert_eq!(d.creator, "Rowlando13");
        assert_eq!(
            d.duration_secs(),
            Some(4 * 86_400 + 8 * 3600 + 51 * 60 + 18)
        );
        let none = parse_rest_deployment(&dep, None).unwrap();
        assert_eq!(none.effective(), DeployState::Unknown);
        let (a, b) = rest_endpoints(&gh(), "pre prod");
        assert_eq!(a, "repos/o/r/environments?per_page=100");
        assert_eq!(b, "repos/o/r/deployments?environment=pre%20prod&per_page=5");
        assert_eq!(
            status_endpoint(&gh(), 7),
            "repos/o/r/deployments/7/statuses?per_page=1"
        );
    }

    #[test]
    fn pending_deployments_and_review_body() {
        let v = json!([{
            "environment": {"id": 161088068, "node_id": "x", "name": "staging",
                "url": "https://api.github.com/repos/o/r/environments/staging",
                "html_url": "https://github.com/o/r/deployments/activity_log?environments_filter=staging"},
            "wait_timer": 30, "wait_timer_started_at": "2020-11-23T22:00:40Z",
            "current_user_can_approve": true,
            "reviewers": [{"type": "User", "reviewer": {"login": "octocat", "id": 1}},
                          {"type": "Team", "reviewer": {"id": 1, "name": "Justice League", "slug": "justice-league"}}]}]);
        let p = parse_pending(&v);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].environment.id, 161088068);
        assert_eq!(p[0].wait_timer, 30);
        assert!(p[0].current_user_can_approve);
        assert_eq!(p[0].reviewers[1].name, "justice-league");
        let a = Approval {
            run: WaitingRun::default(),
            pending: p,
        };
        assert_eq!(a.approvable().len(), 1);
        assert_eq!(
            waiting_runs_endpoint(&gh()),
            "repos/o/r/actions/runs?status=waiting&per_page=10"
        );
        let r = review_request(&gh(), 99, &[161088068], true, "Ship it!");
        assert_eq!(r.method.as_str(), "POST");
        assert_eq!(r.endpoint, "repos/o/r/actions/runs/99/pending_deployments");
        assert_eq!(
            r.body,
            Some(
                json!({"environment_ids": [161088068], "state": "approved", "comment": "Ship it!"})
            )
        );
        let r = review_request(&gh(), 99, &[1, 2], false, "");
        assert_eq!(r.body.unwrap()["state"], "rejected");
    }

    #[test]
    fn rerun_and_create_deployment() {
        let r = rerun_request(&gh(), 40017149075);
        assert_eq!(r.method.as_str(), "POST");
        assert_eq!(r.endpoint, "repos/o/r/actions/runs/40017149075/rerun");
        assert_eq!(r.body, None);
        assert_eq!(
            r.display(),
            "gh api --method POST repos/o/r/actions/runs/40017149075/rerun"
        );
        let c = create_deployment_request(&gh(), "abc123", "production", "Promote from staging");
        assert_eq!(c.endpoint, "repos/o/r/deployments");
        assert_eq!(
            c.body,
            Some(
                json!({"ref": "abc123", "environment": "production", "auto_merge": false,
                        "required_contexts": [], "description": "Promote from staging"})
            )
        );
    }

    #[test]
    fn pipeline_order() {
        let names: Vec<String> = [
            "production",
            "dev",
            "staging",
            "copilot",
            "qa",
            "github-pages",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            order(&names, &[]),
            vec![
                "dev",
                "qa",
                "staging",
                "production",
                "copilot",
                "github-pages"
            ]
        );
        assert!(is_side("github-pages"));
        assert!(is_side("Copilot"));
        assert!(!is_side("pages-preview"));
        assert_eq!(
            order(&names, &["staging".into(), "gone".into()])[0],
            "staging"
        );
        assert_eq!(stage_rank("pre-prod"), 5);
        assert_eq!(stage_rank("prod-eu"), 6);
        assert_eq!(stage_rank("publish"), 6);
        assert!(is_production("Production"));
        assert!(is_production("prod-eu"));
        assert!(!is_production("preprod"));
        assert!(!is_production("pre-prod"));
        assert!(!is_production("products"));
    }
}
