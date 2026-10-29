//! Deployment environments: protection rules (required reviewers, wait
//! timer, self-review, deployment branch policy), custom branch / tag
//! policies, and the create / edit / delete requests.

use serde::Deserialize;

use crate::Result;
use crate::devops::gh::{Gh, Request, encode};
use crate::github::{ApiCall, parse_time};

/// The longest wait timer GitHub accepts, in minutes (30 days).
pub const MAX_WAIT: u32 = 43_200;
/// At most six required reviewers.
pub const MAX_REVIEWERS: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewerKind {
    User,
    Team,
}

impl ReviewerKind {
    pub fn api(self) -> &'static str {
        match self {
            ReviewerKind::User => "User",
            ReviewerKind::Team => "Team",
        }
    }
}

/// A required reviewer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reviewer {
    pub kind: ReviewerKind,
    pub id: u64,
    /// `@login` or `org/slug`.
    pub handle: String,
    pub name: String,
}

/// Which branches may deploy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PolicyMode {
    /// No restriction.
    #[default]
    All,
    /// Branches with protection rules only.
    Protected,
    /// Branches and tags matching the custom policies.
    Custom,
}

impl PolicyMode {
    pub fn label(self) -> &'static str {
        match self {
            PolicyMode::All => "No restriction",
            PolicyMode::Protected => "Protected branches only",
            PolicyMode::Custom => "Selected branches and tags",
        }
    }
}

/// A custom deployment branch or tag policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchPolicy {
    pub id: u64,
    pub name: String,
    /// `branch` or `tag`.
    pub kind: String,
}

/// One environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Environment {
    pub id: u64,
    pub name: String,
    pub html_url: String,
    pub created: Option<i64>,
    pub updated: Option<i64>,
    pub can_admins_bypass: bool,
    pub wait_timer: u32,
    pub prevent_self_review: bool,
    pub reviewers: Vec<Reviewer>,
    pub policy: PolicyMode,
    /// Other protection rule types (custom deployment protection apps).
    pub other_rules: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawReviewerInner {
    id: u64,
    login: Option<String>,
    slug: Option<String>,
    name: Option<String>,
    organization: Option<serde_json::Value>,
    html_url: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawReviewer {
    #[serde(rename = "type")]
    kind: String,
    reviewer: Option<RawReviewerInner>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawRule {
    #[serde(rename = "type")]
    kind: String,
    wait_timer: Option<u32>,
    prevent_self_review: Option<bool>,
    reviewers: Vec<RawReviewer>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawPolicy {
    protected_branches: bool,
    custom_branch_policies: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawEnvironment {
    id: u64,
    name: String,
    html_url: String,
    created_at: Option<String>,
    updated_at: Option<String>,
    can_admins_bypass: Option<bool>,
    protection_rules: Vec<RawRule>,
    deployment_branch_policy: Option<RawPolicy>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawEnvironments {
    environments: Vec<RawEnvironment>,
}

/// The org of a team reviewer, from its `html_url`
/// (`https://github.com/orgs/acme/teams/sec`).
fn team_org(r: &RawReviewerInner) -> String {
    if let Some(o) = r
        .organization
        .as_ref()
        .and_then(|o| o.get("login"))
        .and_then(|l| l.as_str())
    {
        return o.to_owned();
    }
    r.html_url
        .as_deref()
        .and_then(|u| u.split("/orgs/").nth(1))
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("")
        .to_owned()
}

fn env_from_raw(e: RawEnvironment) -> Environment {
    let mut out = Environment {
        id: e.id,
        name: e.name,
        html_url: e.html_url,
        created: e.created_at.as_deref().and_then(parse_time),
        updated: e.updated_at.as_deref().and_then(parse_time),
        can_admins_bypass: e.can_admins_bypass.unwrap_or(true),
        policy: match e.deployment_branch_policy {
            None => PolicyMode::All,
            Some(p) if p.custom_branch_policies => PolicyMode::Custom,
            Some(p) if p.protected_branches => PolicyMode::Protected,
            Some(_) => PolicyMode::All,
        },
        ..Default::default()
    };
    for r in e.protection_rules {
        match r.kind.as_str() {
            "wait_timer" => out.wait_timer = r.wait_timer.unwrap_or(0),
            "required_reviewers" => {
                out.prevent_self_review = r.prevent_self_review.unwrap_or(false);
                for rv in r.reviewers {
                    let Some(inner) = rv.reviewer else { continue };
                    if rv.kind == "Team" {
                        let org = team_org(&inner);
                        let slug = inner.slug.clone().unwrap_or_default();
                        out.reviewers.push(Reviewer {
                            kind: ReviewerKind::Team,
                            id: inner.id,
                            handle: if org.is_empty() {
                                slug.clone()
                            } else {
                                format!("{org}/{slug}")
                            },
                            name: inner.name.clone().unwrap_or(slug),
                        });
                    } else {
                        let login = inner.login.clone().unwrap_or_default();
                        out.reviewers.push(Reviewer {
                            kind: ReviewerKind::User,
                            id: inner.id,
                            handle: format!("@{login}"),
                            name: login,
                        });
                    }
                }
            }
            "branch_policy" => {}
            other => out.other_rules.push(other.to_owned()),
        }
    }
    out
}

pub fn parse_environments(body: &[u8]) -> Result<Vec<Environment>> {
    let raw: RawEnvironments = crate::devops::gh::decode(body, "environments")?;
    let mut v: Vec<Environment> = raw.environments.into_iter().map(env_from_raw).collect();
    v.sort_by_key(|a| a.name.to_lowercase());
    Ok(v)
}

/// `GET environments?per_page=100` (one page: 100 environments).
pub fn fetch_environments(gh: &Gh, calls: &mut Vec<ApiCall>) -> Result<Vec<Environment>> {
    parse_environments(&gh.get(&gh.path("environments?per_page=100"), calls)?)
}

fn env_path(gh: &Gh, name: &str) -> String {
    gh.path(&format!("environments/{}", encode(name)))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawBranchPolicy {
    id: u64,
    name: String,
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawBranchPolicies {
    branch_policies: Vec<RawBranchPolicy>,
}

pub fn parse_branch_policies(body: &[u8]) -> Result<Vec<BranchPolicy>> {
    let raw: RawBranchPolicies = crate::devops::gh::decode(body, "deployment branch policies")?;
    Ok(raw
        .branch_policies
        .into_iter()
        .map(|p| BranchPolicy {
            id: p.id,
            name: p.name,
            kind: p.kind.unwrap_or_else(|| "branch".into()),
        })
        .collect())
}

/// `GET environments/{name}/deployment-branch-policies`.
pub fn fetch_branch_policies(
    gh: &Gh,
    env: &str,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<BranchPolicy>> {
    parse_branch_policies(&gh.get(
        &format!(
            "{}/deployment-branch-policies?per_page=100",
            env_path(gh, env)
        ),
        calls,
    )?)
}

/// The create / edit form of an environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvForm {
    pub name: String,
    pub wait_timer: u32,
    pub prevent_self_review: bool,
    pub can_admins_bypass: bool,
    /// `@login` or `org/team` handles.
    pub reviewers: Vec<String>,
    pub policy: PolicyMode,
}

impl EnvForm {
    pub fn from_env(e: &Environment) -> Self {
        Self {
            name: e.name.clone(),
            wait_timer: e.wait_timer,
            prevent_self_review: e.prevent_self_review,
            can_admins_bypass: e.can_admins_bypass,
            reviewers: e.reviewers.iter().map(|r| r.handle.clone()).collect(),
            policy: e.policy,
        }
    }

    pub fn new_env() -> Self {
        Self {
            can_admins_bypass: true,
            ..Default::default()
        }
    }

    /// Problems that block saving.
    pub fn problems(&self) -> Vec<String> {
        let mut p = Vec::new();
        let n = self.name.trim();
        if n.is_empty() {
            p.push("Enter a name.".into());
        } else if n.len() > 255 || n.contains(['/', '\\', '?', '#', '%']) {
            p.push("A name can be at most 255 characters and cannot contain / \\ ? # or %.".into());
        }
        if self.wait_timer > MAX_WAIT {
            p.push(format!(
                "The wait timer can be at most {MAX_WAIT} minutes (30 days)."
            ));
        }
        if self.reviewers.len() > MAX_REVIEWERS {
            p.push(format!("At most {MAX_REVIEWERS} required reviewers."));
        }
        for r in &self.reviewers {
            if reviewer_lookup(r).is_none() {
                p.push(format!(
                    "`{r}` is not a user (@login) or a team (org/team)."
                ));
            }
        }
        p
    }
}

/// How to resolve a reviewer handle to its numeric id: `(kind, endpoint)`.
/// `@alice` or `alice` → `users/alice`; `acme/sec` or `@acme/sec` →
/// `orgs/acme/teams/sec`.
pub fn reviewer_lookup(handle: &str) -> Option<(ReviewerKind, String)> {
    let h = handle.trim().trim_start_matches('@');
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    match h.split_once('/') {
        Some((org, team)) if ok(org) && ok(team) => Some((
            ReviewerKind::Team,
            format!("orgs/{}/teams/{}", encode(org), encode(team)),
        )),
        None if ok(h) => Some((ReviewerKind::User, format!("users/{}", encode(h)))),
        _ => None,
    }
}

/// The numeric id from `GET users/{login}` or `GET orgs/{org}/teams/{slug}`.
pub fn parse_id(body: &[u8]) -> Result<u64> {
    #[derive(Deserialize)]
    struct WithId {
        id: u64,
    }
    let v: WithId = crate::devops::gh::decode(body, "user or team")?;
    Ok(v.id)
}

/// Resolve every reviewer handle (already known ids are reused).
pub fn resolve_reviewers(
    gh: &Gh,
    handles: &[String],
    known: &[Reviewer],
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<(ReviewerKind, u64, String)>> {
    let mut out = Vec::new();
    for h in handles {
        if let Some(k) = known.iter().find(|k| k.handle.eq_ignore_ascii_case(h)) {
            out.push((k.kind, k.id, h.clone()));
            continue;
        }
        let Some((kind, endpoint)) = reviewer_lookup(h) else {
            return Err(crate::Error::new(format!("`{h}` is not a user or a team")));
        };
        let id = parse_id(
            &gh.get(&endpoint, calls)
                .map_err(|e| crate::Error::new(format!("Could not find {h}: {e}")))?,
        )?;
        out.push((kind, id, h.clone()));
    }
    Ok(out)
}

/// `PUT environments/{name}` creating or updating an environment.
/// `reviewers` are the resolved `(kind, id)` pairs.
pub fn environment_put(gh: &Gh, form: &EnvForm, reviewers: &[(ReviewerKind, u64)]) -> Request {
    let policy = match form.policy {
        PolicyMode::All => serde_json::Value::Null,
        PolicyMode::Protected => {
            serde_json::json!({"protected_branches": true, "custom_branch_policies": false})
        }
        PolicyMode::Custom => {
            serde_json::json!({"protected_branches": false, "custom_branch_policies": true})
        }
    };
    let reviewers: Vec<serde_json::Value> = reviewers
        .iter()
        .map(|(k, id)| serde_json::json!({"type": k.api(), "id": id}))
        .collect();
    Request::put(
        env_path(gh, form.name.trim()),
        serde_json::json!({
            "wait_timer": form.wait_timer.min(MAX_WAIT),
            "prevent_self_review": form.prevent_self_review,
            "reviewers": reviewers,
            "deployment_branch_policy": policy,
            "can_admins_bypass": form.can_admins_bypass,
        }),
    )
}

/// `DELETE environments/{name}`.
pub fn environment_delete(gh: &Gh, name: &str) -> Request {
    Request::delete(env_path(gh, name))
}

/// `POST environments/{env}/deployment-branch-policies {name, type}`.
pub fn branch_policy_create(gh: &Gh, env: &str, pattern: &str, tag: bool) -> Request {
    Request::post(
        format!("{}/deployment-branch-policies", env_path(gh, env)),
        serde_json::json!({
            "name": pattern.trim(),
            "type": if tag { "tag" } else { "branch" },
        }),
    )
}

/// `DELETE environments/{env}/deployment-branch-policies/{id}`.
pub fn branch_policy_delete(gh: &Gh, env: &str, id: u64) -> Request {
    Request::delete(format!(
        "{}/deployment-branch-policies/{id}",
        env_path(gh, env)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::gh::Method;

    const ENVS: &str = r#"{"total_count": 2, "environments": [
      {"id": 161088068, "name": "staging", "html_url": "https://github.com/o/r/deployments/activity_log?environments_filter=staging",
       "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z", "protection_rules": [], "deployment_branch_policy": null},
      {"id": 161088069, "name": "Production", "can_admins_bypass": false,
       "protection_rules": [
         {"id": 1, "type": "wait_timer", "wait_timer": 30},
         {"id": 2, "type": "required_reviewers", "prevent_self_review": true, "reviewers": [
           {"type": "User", "reviewer": {"login": "octocat", "id": 1}},
           {"type": "Team", "reviewer": {"id": 9, "slug": "sec", "name": "Security", "html_url": "https://github.com/orgs/acme/teams/sec"}}]},
         {"id": 3, "type": "branch_policy"},
         {"id": 4, "type": "deployment_protection_rule"}],
       "deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}]}"#;

    #[test]
    fn parse_envs() {
        let v = parse_environments(ENVS.as_bytes()).unwrap();
        assert_eq!(v.len(), 2);
        let p = &v[0];
        assert_eq!(p.name, "Production");
        assert_eq!(p.wait_timer, 30);
        assert!(p.prevent_self_review);
        assert!(!p.can_admins_bypass);
        assert_eq!(p.policy, PolicyMode::Custom);
        assert_eq!(p.reviewers.len(), 2);
        assert_eq!(p.reviewers[0].handle, "@octocat");
        assert_eq!(p.reviewers[1].handle, "acme/sec");
        assert_eq!(p.reviewers[1].kind, ReviewerKind::Team);
        assert_eq!(p.other_rules, vec!["deployment_protection_rule"]);
        assert_eq!(v[1].policy, PolicyMode::All);
        assert!(v[1].can_admins_bypass);
    }

    #[test]
    fn put_bodies() {
        let gh = Gh::new(".", "o", "r");
        let mut f = EnvForm::new_env();
        f.name = "production eu".into();
        f.wait_timer = 15;
        f.prevent_self_review = true;
        f.policy = PolicyMode::Protected;
        let r = environment_put(&gh, &f, &[(ReviewerKind::User, 1), (ReviewerKind::Team, 9)]);
        assert_eq!(r.method, Method::Put);
        assert_eq!(r.endpoint, "repos/o/r/environments/production%20eu");
        assert_eq!(
            r.body.unwrap(),
            serde_json::json!({
                "wait_timer": 15,
                "prevent_self_review": true,
                "reviewers": [{"type": "User", "id": 1}, {"type": "Team", "id": 9}],
                "deployment_branch_policy": {"protected_branches": true, "custom_branch_policies": false},
                "can_admins_bypass": true
            })
        );
        f.policy = PolicyMode::All;
        let r = environment_put(&gh, &f, &[]);
        let b = r.body.unwrap();
        assert!(b["deployment_branch_policy"].is_null());
        assert_eq!(b["reviewers"], serde_json::json!([]));
        f.policy = PolicyMode::Custom;
        let b = environment_put(&gh, &f, &[]).body.unwrap();
        assert_eq!(
            b["deployment_branch_policy"],
            serde_json::json!({"protected_branches": false, "custom_branch_policies": true})
        );
        let r = environment_delete(&gh, "staging");
        assert_eq!(r.method, Method::Delete);
        assert_eq!(r.endpoint, "repos/o/r/environments/staging");
    }

    #[test]
    fn branch_policies() {
        let gh = Gh::new(".", "o", "r");
        let r = branch_policy_create(&gh, "prod", " release/* ", false);
        assert_eq!(r.method, Method::Post);
        assert_eq!(
            r.endpoint,
            "repos/o/r/environments/prod/deployment-branch-policies"
        );
        assert_eq!(
            r.body.unwrap(),
            serde_json::json!({"name": "release/*", "type": "branch"})
        );
        let r = branch_policy_create(&gh, "prod", "v*", true);
        assert_eq!(r.body.unwrap()["type"], "tag");
        let r = branch_policy_delete(&gh, "prod", 361471);
        assert_eq!(r.method, Method::Delete);
        assert_eq!(
            r.endpoint,
            "repos/o/r/environments/prod/deployment-branch-policies/361471"
        );
        let body = br#"{"total_count":2,"branch_policies":[{"id":1,"node_id":"x","name":"release/*","type":"branch"},{"id":2,"name":"v*","type":"tag"}]}"#;
        let p = parse_branch_policies(body).unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(p[1].kind, "tag");
    }

    #[test]
    fn reviewers_and_validation() {
        assert_eq!(
            reviewer_lookup("@alice"),
            Some((ReviewerKind::User, "users/alice".into()))
        );
        assert_eq!(
            reviewer_lookup("acme/sec-team"),
            Some((ReviewerKind::Team, "orgs/acme/teams/sec-team".into()))
        );
        assert_eq!(reviewer_lookup("a b"), None);
        assert_eq!(reviewer_lookup("a/b/c"), None);
        assert_eq!(parse_id(br#"{"login":"alice","id":42}"#).unwrap(), 42);
        let mut f = EnvForm::new_env();
        assert!(!f.problems().is_empty());
        f.name = "prod".into();
        assert!(f.problems().is_empty());
        f.wait_timer = MAX_WAIT + 1;
        f.reviewers = vec!["bad handle".into()];
        assert_eq!(f.problems().len(), 2);
    }
}
