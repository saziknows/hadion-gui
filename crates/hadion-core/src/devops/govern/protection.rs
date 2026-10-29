//! Classic branch protection (read, edit as a form, PUT, DELETE), the
//! protected branch list, rulesets and the rules in effect on a branch.
//!
//! The PUT body always carries GitHub's four required keys
//! (`required_status_checks`, `enforce_admins`,
//! `required_pull_request_reviews`, `restrictions`), each `null` when the
//! setting is off. Restrictions, dismissal restrictions and bypass
//! allowances are not edited here: their current values are sent back as
//! they are so a save does not drop them.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::Result;
use crate::devops::gh::{Gh, Request, encode};
use crate::github::{ApiCall, parse_time};

/// Who may do something: logins, team slugs and app slugs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Actors {
    pub users: Vec<String>,
    pub teams: Vec<String>,
    pub apps: Vec<String>,
}

impl Actors {
    pub fn is_empty(&self) -> bool {
        self.users.is_empty() && self.teams.is_empty() && self.apps.is_empty()
    }

    pub fn body(&self) -> Value {
        json!({"users": self.users, "teams": self.teams, "apps": self.apps})
    }

    /// `@alice, acme/sec (team), dependabot (app)`.
    pub fn describe(&self) -> String {
        let mut v: Vec<String> = self.users.iter().map(|u| format!("@{u}")).collect();
        v.extend(self.teams.iter().map(|t| format!("{t} (team)")));
        v.extend(self.apps.iter().map(|a| format!("{a} (app)")));
        if v.is_empty() {
            "nobody".into()
        } else {
            v.join(", ")
        }
    }
}

/// Branch protection as a form.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProtectionForm {
    pub require_pr: bool,
    /// 0–6.
    pub approvals: u8,
    pub dismiss_stale: bool,
    pub code_owner_reviews: bool,
    pub last_push_approval: bool,
    pub dismissal_restrictions: Option<Actors>,
    pub bypass_allowances: Option<Actors>,
    pub require_checks: bool,
    /// Require the branch to be up to date before merging.
    pub strict: bool,
    pub contexts: Vec<String>,
    /// App ids GitHub reported per context (sent back with `checks`).
    pub check_apps: Vec<(String, i64)>,
    pub enforce_admins: bool,
    pub linear_history: bool,
    pub allow_force_pushes: bool,
    pub allow_deletions: bool,
    pub conversation_resolution: bool,
    pub lock_branch: bool,
    pub block_creations: bool,
    pub allow_fork_syncing: bool,
    /// Read-only: set through its own endpoint.
    pub required_signatures: bool,
    /// Read-only here (organisation repositories only).
    pub restrictions: Option<Actors>,
}

impl ProtectionForm {
    /// The settings GitHub suggests for a newly protected branch: a pull
    /// request with one approval, no force pushes, no deletions.
    pub fn recommended() -> Self {
        Self {
            require_pr: true,
            approvals: 1,
            dismiss_stale: true,
            ..Default::default()
        }
    }

    /// Add a status check context (trimmed, no duplicates).
    pub fn add_context(&mut self, c: &str) -> bool {
        let c = c.trim();
        if c.is_empty() || self.contexts.iter().any(|x| x == c) {
            return false;
        }
        self.contexts.push(c.to_owned());
        true
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Enabled {
    enabled: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawLogin {
    login: String,
    slug: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawActors {
    users: Vec<RawLogin>,
    teams: Vec<RawLogin>,
    apps: Vec<RawLogin>,
}

impl RawActors {
    fn actors(self) -> Actors {
        Actors {
            users: self.users.into_iter().map(|u| u.login).collect(),
            teams: self.teams.into_iter().map(|t| t.slug).collect(),
            apps: self.apps.into_iter().map(|a| a.slug).collect(),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawReviews {
    dismissal_restrictions: Option<RawActors>,
    bypass_pull_request_allowances: Option<RawActors>,
    dismiss_stale_reviews: bool,
    require_code_owner_reviews: bool,
    required_approving_review_count: u8,
    require_last_push_approval: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawCheck {
    context: String,
    app_id: Option<i64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawChecks {
    strict: bool,
    contexts: Vec<String>,
    checks: Vec<RawCheck>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawProtection {
    required_status_checks: Option<RawChecks>,
    enforce_admins: Option<Enabled>,
    required_pull_request_reviews: Option<RawReviews>,
    restrictions: Option<RawActors>,
    required_linear_history: Option<Enabled>,
    allow_force_pushes: Option<Enabled>,
    allow_deletions: Option<Enabled>,
    block_creations: Option<Enabled>,
    required_conversation_resolution: Option<Enabled>,
    lock_branch: Option<Enabled>,
    allow_fork_syncing: Option<Enabled>,
    required_signatures: Option<Enabled>,
}

fn on(e: &Option<Enabled>) -> bool {
    e.as_ref().is_some_and(|e| e.enabled)
}

/// Decode `GET branches/{b}/protection` into the form.
pub fn parse_protection(body: &[u8]) -> Result<ProtectionForm> {
    let raw: RawProtection = crate::devops::gh::decode(body, "branch protection")?;
    let mut f = ProtectionForm {
        enforce_admins: on(&raw.enforce_admins),
        linear_history: on(&raw.required_linear_history),
        allow_force_pushes: on(&raw.allow_force_pushes),
        allow_deletions: on(&raw.allow_deletions),
        block_creations: on(&raw.block_creations),
        conversation_resolution: on(&raw.required_conversation_resolution),
        lock_branch: on(&raw.lock_branch),
        allow_fork_syncing: on(&raw.allow_fork_syncing),
        required_signatures: on(&raw.required_signatures),
        restrictions: raw.restrictions.map(RawActors::actors),
        ..Default::default()
    };
    if let Some(r) = raw.required_pull_request_reviews {
        f.require_pr = true;
        f.approvals = r.required_approving_review_count.min(6);
        f.dismiss_stale = r.dismiss_stale_reviews;
        f.code_owner_reviews = r.require_code_owner_reviews;
        f.last_push_approval = r.require_last_push_approval;
        f.dismissal_restrictions = r.dismissal_restrictions.map(RawActors::actors);
        f.bypass_allowances = r.bypass_pull_request_allowances.map(RawActors::actors);
    }
    if let Some(c) = raw.required_status_checks {
        f.require_checks = true;
        f.strict = c.strict;
        let mut contexts = c.contexts;
        for ch in &c.checks {
            if !contexts.contains(&ch.context) {
                contexts.push(ch.context.clone());
            }
            if let Some(id) = ch.app_id {
                f.check_apps.push((ch.context.clone(), id));
            }
        }
        f.contexts = contexts;
    }
    Ok(f)
}

fn protection_path(gh: &Gh, branch: &str) -> String {
    gh.path(&format!("branches/{}/protection", encode(branch)))
}

/// `GET branches/{b}/protection`.
pub fn fetch_protection(gh: &Gh, branch: &str, calls: &mut Vec<ApiCall>) -> Result<ProtectionForm> {
    parse_protection(&gh.get(&protection_path(gh, branch), calls)?)
}

/// The PUT body. All four required keys are always present.
pub fn protection_body(f: &ProtectionForm) -> Value {
    let checks = if f.require_checks {
        if f.check_apps.is_empty() {
            json!({"strict": f.strict, "contexts": f.contexts})
        } else {
            let checks: Vec<Value> = f
                .contexts
                .iter()
                .map(|c| match f.check_apps.iter().find(|(n, _)| n == c) {
                    Some((_, id)) => json!({"context": c, "app_id": id}),
                    None => json!({"context": c}),
                })
                .collect();
            json!({"strict": f.strict, "checks": checks})
        }
    } else {
        Value::Null
    };
    let reviews = if f.require_pr {
        let mut r = json!({
            "dismiss_stale_reviews": f.dismiss_stale,
            "require_code_owner_reviews": f.code_owner_reviews,
            "required_approving_review_count": f.approvals.min(6),
            "require_last_push_approval": f.last_push_approval,
        });
        if let Some(d) = &f.dismissal_restrictions {
            r["dismissal_restrictions"] = d.body();
        }
        if let Some(b) = &f.bypass_allowances {
            r["bypass_pull_request_allowances"] = b.body();
        }
        r
    } else {
        Value::Null
    };
    let mut body = json!({
        "required_status_checks": checks,
        "enforce_admins": if f.enforce_admins { Value::Bool(true) } else { Value::Null },
        "required_pull_request_reviews": reviews,
        "restrictions": match &f.restrictions {
            Some(r) => r.body(),
            None => Value::Null,
        },
        "required_linear_history": f.linear_history,
        "allow_force_pushes": f.allow_force_pushes,
        "allow_deletions": f.allow_deletions,
        "required_conversation_resolution": f.conversation_resolution,
        "lock_branch": f.lock_branch,
        "allow_fork_syncing": f.allow_fork_syncing,
    });
    if f.restrictions.is_some() {
        body["block_creations"] = Value::Bool(f.block_creations);
    }
    body
}

/// `PUT branches/{b}/protection`.
pub fn protection_put(gh: &Gh, branch: &str, f: &ProtectionForm) -> Request {
    Request::put(protection_path(gh, branch), protection_body(f))
}

/// `DELETE branches/{b}/protection`.
pub fn protection_delete(gh: &Gh, branch: &str) -> Request {
    Request::delete(protection_path(gh, branch))
}

/// What a save changes, as `setting: old → new` lines (for confirmations).
pub fn changes(old: Option<&ProtectionForm>, new: &ProtectionForm) -> Vec<String> {
    let base = old.cloned().unwrap_or_default();
    let mut out = Vec::new();
    let mut flag = |name: &str, a: bool, b: bool| {
        if a != b {
            out.push(format!(
                "{name}: {} → {}",
                if a { "on" } else { "off" },
                if b { "on" } else { "off" }
            ));
        }
    };
    flag("Require a pull request", base.require_pr, new.require_pr);
    flag(
        "Dismiss stale approvals",
        base.dismiss_stale,
        new.dismiss_stale,
    );
    flag(
        "Require code owner review",
        base.code_owner_reviews,
        new.code_owner_reviews,
    );
    flag(
        "Require approval of the last push",
        base.last_push_approval,
        new.last_push_approval,
    );
    flag(
        "Require status checks",
        base.require_checks,
        new.require_checks,
    );
    flag("Require up-to-date branches", base.strict, new.strict);
    flag(
        "Include administrators",
        base.enforce_admins,
        new.enforce_admins,
    );
    flag(
        "Require linear history",
        base.linear_history,
        new.linear_history,
    );
    flag(
        "Allow force pushes",
        base.allow_force_pushes,
        new.allow_force_pushes,
    );
    flag("Allow deletions", base.allow_deletions, new.allow_deletions);
    flag(
        "Require conversation resolution",
        base.conversation_resolution,
        new.conversation_resolution,
    );
    flag("Lock branch", base.lock_branch, new.lock_branch);
    flag(
        "Allow fork syncing",
        base.allow_fork_syncing,
        new.allow_fork_syncing,
    );
    if base.approvals != new.approvals && new.require_pr {
        out.push(format!(
            "Required approvals: {} → {}",
            base.approvals, new.approvals
        ));
    }
    for c in &new.contexts {
        if !base.contexts.contains(c) {
            out.push(format!("Add required check `{c}`"));
        }
    }
    for c in &base.contexts {
        if !new.contexts.contains(c) {
            out.push(format!("Remove required check `{c}`"));
        }
    }
    out
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawBranch {
    name: String,
    protected: bool,
}

/// `GET branches?protected=true` (first 100), names only.
pub fn fetch_protected_branches(gh: &Gh, calls: &mut Vec<ApiCall>) -> Result<Vec<String>> {
    let raw: Vec<RawBranch> = gh.get_json(
        &gh.path("branches?protected=true&per_page=100"),
        "protected branches",
        calls,
    )?;
    Ok(raw
        .into_iter()
        .filter(|b| b.protected)
        .map(|b| b.name)
        .collect())
}

/// The branch picker: the default branch first, then protected branches.
pub fn branch_choices(default_branch: &str, protected: &[String]) -> Vec<String> {
    let mut v = Vec::new();
    if !default_branch.is_empty() {
        v.push(default_branch.to_owned());
    }
    for b in protected {
        if !v.contains(b) {
            v.push(b.clone());
        }
    }
    v
}

/// Status check names worth suggesting: names seen on recent commits,
/// without the ones already required, sorted.
pub fn suggest_contexts<'a>(
    current: &[String],
    seen: impl Iterator<Item = &'a str>,
) -> Vec<String> {
    let mut v: Vec<String> = seen
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter(|s| !current.iter().any(|c| c == s))
        .map(str::to_owned)
        .collect();
    v.sort();
    v.dedup();
    v
}

/// A repository or organisation ruleset (list entry).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ruleset {
    pub id: u64,
    pub name: String,
    /// `branch`, `tag`, `push`.
    pub target: String,
    /// `Repository` or `Organization`.
    pub source_type: String,
    pub source: String,
    /// `active`, `evaluate`, `disabled`.
    pub enforcement: String,
    pub html_url: String,
    pub updated: Option<i64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawLinks {
    html: Option<RawHref>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawHref {
    href: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawRuleset {
    id: u64,
    name: String,
    target: String,
    source_type: String,
    source: String,
    enforcement: String,
    #[serde(rename = "_links")]
    links: Option<RawLinks>,
    updated_at: Option<String>,
}

pub fn parse_rulesets(body: &[u8]) -> Result<Vec<Ruleset>> {
    let raw: Vec<RawRuleset> = crate::devops::gh::decode(body, "rulesets")?;
    Ok(raw
        .into_iter()
        .map(|r| Ruleset {
            id: r.id,
            name: r.name,
            target: r.target,
            source_type: r.source_type,
            source: r.source,
            enforcement: r.enforcement,
            html_url: r
                .links
                .and_then(|l| l.html)
                .map(|h| h.href)
                .unwrap_or_default(),
            updated: r.updated_at.as_deref().and_then(parse_time),
        })
        .collect())
}

/// `GET rulesets?includes_parents=true` (first 100).
pub fn fetch_rulesets(gh: &Gh, calls: &mut Vec<ApiCall>) -> Result<Vec<Ruleset>> {
    parse_rulesets(&gh.get(
        &gh.path("rulesets?includes_parents=true&per_page=100"),
        calls,
    )?)
}

/// A rule in effect on a branch.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectiveRule {
    pub kind: String,
    pub parameters: Value,
    pub source_type: String,
    pub source: String,
    pub ruleset_id: u64,
}

impl EffectiveRule {
    /// A plain-language line: "Pull request required: 2 approvals, code
    /// owner review".
    pub fn describe(&self) -> String {
        let p = &self.parameters;
        let b = |k: &str| p.get(k).and_then(Value::as_bool).unwrap_or(false);
        match self.kind.as_str() {
            "deletion" => "Deleting the branch is blocked".into(),
            "non_fast_forward" => "Force pushes are blocked".into(),
            "creation" => "Creating matching branches is restricted".into(),
            "update" => "Pushing to the branch is restricted".into(),
            "required_linear_history" => "Linear history required (no merge commits)".into(),
            "required_signatures" => "Signed commits required".into(),
            "pull_request" => {
                let n = p
                    .get("required_approving_review_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let mut parts = vec![if n == 1 {
                    "1 approval".to_owned()
                } else {
                    format!("{n} approvals")
                }];
                if b("require_code_owner_review") {
                    parts.push("code owner review".into());
                }
                if b("dismiss_stale_reviews_on_push") {
                    parts.push("stale approvals dismissed".into());
                }
                if b("require_last_push_approval") {
                    parts.push("last push approved".into());
                }
                if b("required_review_thread_resolution") {
                    parts.push("conversations resolved".into());
                }
                if let Some(m) = p.get("allowed_merge_methods").and_then(Value::as_array) {
                    let m: Vec<&str> = m.iter().filter_map(Value::as_str).collect();
                    if !m.is_empty() {
                        parts.push(format!("merge methods {}", m.join(" / ")));
                    }
                }
                format!("Pull request required: {}", parts.join(", "))
            }
            "required_status_checks" => {
                let checks: Vec<String> = p
                    .get("required_status_checks")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|c| c.get("context").and_then(Value::as_str))
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                format!(
                    "Status checks required{}: {}",
                    if b("strict_required_status_checks_policy") {
                        " (branch up to date)"
                    } else {
                        ""
                    },
                    if checks.is_empty() {
                        "none listed".to_owned()
                    } else {
                        checks.join(", ")
                    }
                )
            }
            "required_deployments" => {
                let envs: Vec<&str> = p
                    .get("required_deployment_environments")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                format!("Successful deployment required to: {}", envs.join(", "))
            }
            "merge_queue" => "Merges go through a merge queue".into(),
            "code_scanning" => "Code scanning results required".into(),
            "workflows" => "Required workflows must pass".into(),
            "commit_message_pattern"
            | "commit_author_email_pattern"
            | "committer_email_pattern"
            | "branch_name_pattern"
            | "tag_name_pattern" => {
                let op = p.get("operator").and_then(Value::as_str).unwrap_or("");
                let pat = p.get("pattern").and_then(Value::as_str).unwrap_or("");
                format!("{}: {op} `{pat}`", self.kind.replace('_', " "))
            }
            other => other.replace('_', " "),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawEffective {
    #[serde(rename = "type")]
    kind: String,
    parameters: Value,
    ruleset_source_type: String,
    ruleset_source: String,
    ruleset_id: u64,
}

pub fn parse_effective(body: &[u8]) -> Result<Vec<EffectiveRule>> {
    let raw: Vec<RawEffective> = crate::devops::gh::decode(body, "branch rules")?;
    Ok(raw
        .into_iter()
        .map(|r| EffectiveRule {
            kind: r.kind,
            parameters: r.parameters,
            source_type: r.ruleset_source_type,
            source: r.ruleset_source,
            ruleset_id: r.ruleset_id,
        })
        .collect())
}

/// `GET rules/branches/{b}`: every rule in effect, from all rulesets.
pub fn fetch_effective(
    gh: &Gh,
    branch: &str,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<EffectiveRule>> {
    parse_effective(&gh.get(
        &gh.path(&format!("rules/branches/{}?per_page=100", encode(branch))),
        calls,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::gh::Method;

    const FULL: &str = r#"{
      "url": "https://api.github.com/repos/o/r/branches/main/protection",
      "required_status_checks": {"strict": true, "contexts": ["build"], "checks": [{"context": "build", "app_id": 15368}, {"context": "lint", "app_id": null}]},
      "enforce_admins": {"enabled": true},
      "required_pull_request_reviews": {
        "dismissal_restrictions": {"users": [{"login": "alice"}], "teams": [{"slug": "core"}], "apps": []},
        "dismiss_stale_reviews": true, "require_code_owner_reviews": true,
        "required_approving_review_count": 2, "require_last_push_approval": false},
      "restrictions": {"users": [{"login": "bob"}], "teams": [], "apps": [{"slug": "deploy-bot"}]},
      "required_linear_history": {"enabled": true},
      "allow_force_pushes": {"enabled": false},
      "allow_deletions": {"enabled": false},
      "block_creations": {"enabled": true},
      "required_conversation_resolution": {"enabled": true},
      "lock_branch": {"enabled": false},
      "allow_fork_syncing": {"enabled": false},
      "required_signatures": {"enabled": true}
    }"#;

    #[test]
    fn parse_full() {
        let f = parse_protection(FULL.as_bytes()).unwrap();
        assert!(f.require_pr);
        assert_eq!(f.approvals, 2);
        assert!(f.dismiss_stale && f.code_owner_reviews && !f.last_push_approval);
        assert!(f.require_checks && f.strict);
        assert_eq!(f.contexts, vec!["build", "lint"]);
        assert_eq!(f.check_apps, vec![("build".to_string(), 15368)]);
        assert!(f.enforce_admins && f.linear_history && f.conversation_resolution);
        assert!(f.block_creations && f.required_signatures);
        assert_eq!(
            f.restrictions.as_ref().unwrap().describe(),
            "@bob, deploy-bot (app)"
        );
        assert_eq!(
            f.dismissal_restrictions.as_ref().unwrap().users,
            vec!["alice"]
        );
    }

    #[test]
    fn put_body_round_trip_keeps_restrictions() {
        let gh = Gh::new(".", "o", "r");
        let f = parse_protection(FULL.as_bytes()).unwrap();
        let r = protection_put(&gh, "release/1.x", &f);
        assert_eq!(r.method, Method::Put);
        assert_eq!(r.endpoint, "repos/o/r/branches/release%2F1.x/protection");
        let b = r.body.unwrap();
        assert_eq!(
            b["required_status_checks"],
            json!({"strict": true, "checks": [{"context": "build", "app_id": 15368}, {"context": "lint"}]})
        );
        assert_eq!(b["enforce_admins"], json!(true));
        assert_eq!(
            b["required_pull_request_reviews"],
            json!({
                "dismiss_stale_reviews": true,
                "require_code_owner_reviews": true,
                "required_approving_review_count": 2,
                "require_last_push_approval": false,
                "dismissal_restrictions": {"users": ["alice"], "teams": ["core"], "apps": []}
            })
        );
        assert_eq!(
            b["restrictions"],
            json!({"users": ["bob"], "teams": [], "apps": ["deploy-bot"]})
        );
        assert_eq!(b["block_creations"], json!(true));
        assert_eq!(b["required_linear_history"], json!(true));
    }

    #[test]
    fn put_body_all_disabled() {
        let b = protection_body(&ProtectionForm::default());
        for key in [
            "required_status_checks",
            "enforce_admins",
            "required_pull_request_reviews",
            "restrictions",
        ] {
            assert!(b.get(key).is_some(), "{key} missing");
            assert!(b[key].is_null(), "{key} should be null");
        }
        assert_eq!(b["allow_force_pushes"], json!(false));
        assert_eq!(b["allow_deletions"], json!(false));
        assert_eq!(b["lock_branch"], json!(false));
        assert!(b.get("block_creations").is_none());
    }

    #[test]
    fn put_body_combinations() {
        let mut f = ProtectionForm::recommended();
        f.approvals = 9; // clamped
        let b = protection_body(&f);
        assert_eq!(
            b["required_pull_request_reviews"],
            json!({
                "dismiss_stale_reviews": true,
                "require_code_owner_reviews": false,
                "required_approving_review_count": 6,
                "require_last_push_approval": false
            })
        );
        assert!(b["required_status_checks"].is_null());
        f.require_pr = false;
        f.require_checks = true;
        f.add_context(" ci / test ");
        assert!(!f.add_context("ci / test"));
        f.enforce_admins = true;
        f.allow_force_pushes = true;
        let b = protection_body(&f);
        assert!(b["required_pull_request_reviews"].is_null());
        assert_eq!(
            b["required_status_checks"],
            json!({"strict": false, "contexts": ["ci / test"]})
        );
        assert_eq!(b["enforce_admins"], json!(true));
        assert_eq!(b["allow_force_pushes"], json!(true));
        let gh = Gh::new(".", "o", "r");
        let d = protection_delete(&gh, "main");
        assert_eq!(d.method, Method::Delete);
        assert_eq!(d.endpoint, "repos/o/r/branches/main/protection");
        assert!(d.body.is_none());
    }

    #[test]
    fn changes_and_suggestions() {
        let old = ProtectionForm::recommended();
        let mut new = old.clone();
        new.approvals = 2;
        new.require_checks = true;
        new.add_context("build");
        let c = changes(Some(&old), &new);
        assert!(c.contains(&"Require status checks: off → on".to_string()));
        assert!(c.contains(&"Required approvals: 1 → 2".to_string()));
        assert!(c.contains(&"Add required check `build`".to_string()));
        let s = suggest_contexts(
            &["build".to_string()],
            ["lint", "build", "test", "lint", " "].into_iter(),
        );
        assert_eq!(s, vec!["lint", "test"]);
        assert_eq!(
            branch_choices("main", &["release".into(), "main".into()]),
            vec!["main", "release"]
        );
    }

    #[test]
    fn rules_and_rulesets() {
        let body = br#"[{"type":"deletion","ruleset_source_type":"Organization","ruleset_source":"brave","ruleset_id":476878},{"type":"non_fast_forward","ruleset_source_type":"Organization","ruleset_source":"brave","ruleset_id":476878},{"type":"pull_request","parameters":{"required_approving_review_count":0,"dismiss_stale_reviews_on_push":false,"required_reviewers":[],"require_code_owner_review":false,"require_last_push_approval":false,"required_review_thread_resolution":false,"allowed_merge_methods":["merge","squash","rebase"]},"ruleset_source_type":"Organization","ruleset_source":"brave","ruleset_id":476878},{"type":"required_status_checks","parameters":{"strict_required_status_checks_policy":true,"required_status_checks":[{"context":"ci"}]},"ruleset_source_type":"Repository","ruleset_source":"o/r","ruleset_id":1}]"#;
        let r = parse_effective(body).unwrap();
        assert_eq!(r.len(), 4);
        assert_eq!(r[0].describe(), "Deleting the branch is blocked");
        assert_eq!(r[1].describe(), "Force pushes are blocked");
        assert_eq!(
            r[2].describe(),
            "Pull request required: 0 approvals, merge methods merge / squash / rebase"
        );
        assert_eq!(
            r[3].describe(),
            "Status checks required (branch up to date): ci"
        );
        let rs = br#"[{"id":476878,"name":"security-base","target":"branch","source_type":"Organization","source":"brave","enforcement":"active","node_id":"x","_links":{"self":{"href":"a"},"html":{"href":"https://github.com/brave/brave-core/rules/476878"}},"created_at":"2024-02-29T22:49:04.831+02:00","updated_at":"2026-09-03T23:40:00.554+02:00"}]"#;
        let rs = parse_rulesets(rs).unwrap();
        assert_eq!(rs[0].name, "security-base");
        assert_eq!(
            rs[0].html_url,
            "https://github.com/brave/brave-core/rules/476878"
        );
        assert!(rs[0].updated.is_some());
    }
}
