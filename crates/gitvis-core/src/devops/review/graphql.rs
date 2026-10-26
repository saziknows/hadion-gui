//! GraphQL documents of the review loop: one query per pull request list,
//! one per pull request (conversation, merge state and review threads),
//! and the mutations REST does not offer (auto-merge, draft state, thread
//! resolution).

use serde_json::{Value, json};

use super::model::{PrDetail, PrList, PrSummary, RepoMeta, s, u};
use crate::devops::gh::Gh;
use crate::{Error, Result};

/// Pull requests per list page. The full list query costs ~6 s on a big
/// repository at 25 (mergeability is computed per pull request) and hits
/// GitHub's 10 s limit at 40.
pub const PAGE: u32 = 20;
/// Pull requests per page when falling back to the light query.
pub const PAGE_LIGHT: u32 = 50;

/// Which pull requests a list holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ListKind {
    Open,
    /// Closed and merged, newest update first.
    Closed,
}

impl ListKind {
    fn states(self) -> Value {
        match self {
            ListKind::Open => json!(["OPEN"]),
            ListKind::Closed => json!(["CLOSED", "MERGED"]),
        }
    }
}

/// Fields of a pull request on a list row. `{MERGE}` is replaced by the
/// mergeability fields, or by nothing for the light query.
const PR_FIELDS: &str = "
      id number title url state isDraft createdAt updatedAt closedAt mergedAt
      author { login }
      headRefName baseRefName headRefOid isCrossRepository
      headRepository { nameWithOwner }
      additions deletions changedFiles
      reviewDecision {MERGE}
      labels(first: 20) { nodes { name color } }
      commits(last: 1) { totalCount nodes { commit { statusCheckRollup { state } } } }
      reviewRequests(first: 20) { totalCount nodes { requestedReviewer { __typename ... on User { login } ... on Team { slug name } ... on Mannequin { login } ... on Bot { login } } } }
      assignees(first: 10) { nodes { login } }
      comments { totalCount }
      reviews { totalCount }
      reviewThreads { totalCount }
      totalCommentsCount";

const REPO_FIELDS: &str = "
    id nameWithOwner isPrivate hasIssuesEnabled
    defaultBranchRef { name }
    owner { __typename login }
    mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed autoMergeAllowed deleteBranchOnMerge
    viewerPermission viewerDefaultMergeMethod
    squashMergeCommitTitle squashMergeCommitMessage mergeCommitTitle mergeCommitMessage";

/// The list query: one page of pull requests, the repository's merge
/// settings, the viewer, and the pull requests waiting for the viewer's
/// review. `light` leaves out `mergeable` / `mergeStateStatus`, which are
/// what makes the query slow.
pub fn list_query(light: bool) -> String {
    let fields = PR_FIELDS.replace(
        "{MERGE}",
        if light {
            ""
        } else {
            "mergeable mergeStateStatus"
        },
    );
    format!(
        "query($owner: String!, $name: String!, $states: [PullRequestState!], $first: Int!, $after: String, $search: String!) {{
  rateLimit {{ cost remaining }}
  viewer {{ login }}
  repository(owner: $owner, name: $name) {{{REPO_FIELDS}
    pullRequests(states: $states, first: $first, after: $after, orderBy: {{field: UPDATED_AT, direction: DESC}}) {{
      totalCount
      pageInfo {{ hasNextPage endCursor }}
      nodes {{{fields}
      }}
    }}
  }}
  requested: search(query: $search, type: ISSUE, first: 100) {{
    issueCount
    nodes {{ ... on PullRequest {{ number }} }}
  }}
}}"
    )
}

/// The search behind the "Review requested" filter.
pub fn review_requested_search(gh: &Gh) -> String {
    format!("is:pr is:open review-requested:@me repo:{}", gh.full_name())
}

pub fn list_variables(gh: &Gh, kind: ListKind, first: u32, after: Option<&str>) -> Value {
    json!({
        "owner": gh.owner,
        "name": gh.repo,
        "states": kind.states(),
        "first": first,
        "after": after,
        "search": review_requested_search(gh),
    })
}

/// Read the list query's `data`.
pub fn parse_list(data: &Value, light: bool) -> Result<PrList> {
    let repo = data
        .get("repository")
        .filter(|r| !r.is_null())
        .ok_or_else(|| Error::new("GitHub did not return the repository"))?;
    let prs = repo.get("pullRequests").cloned().unwrap_or(Value::Null);
    let page = prs.get("pageInfo").cloned().unwrap_or(Value::Null);
    let requested = data.get("requested").cloned().unwrap_or(Value::Null);
    Ok(PrList {
        items: prs
            .get("nodes")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(PrSummary::from_json).collect())
            .unwrap_or_default(),
        total: u(&prs, "totalCount"),
        end_cursor: page
            .get("endCursor")
            .and_then(Value::as_str)
            .map(str::to_owned),
        has_more: page
            .get("hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        viewer: data
            .get("viewer")
            .map(|v| s(v, "login"))
            .unwrap_or_default(),
        requested: requested
            .get("nodes")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|n| u(n, "number"))
                    .filter(|n| *n > 0)
                    .collect()
            })
            .unwrap_or_default(),
        requested_total: u(&requested, "issueCount"),
        meta: RepoMeta::from_graphql(repo),
        light,
        cost: data.get("rateLimit").map(|r| u(r, "cost")),
    })
}

/// Everything about one pull request: merge state, auto-merge, the newest
/// 100 timeline entries and the first 100 review threads with up to 50
/// comments each.
pub const DETAIL_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) {
  rateLimit { cost remaining }
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      id number title body url state isDraft locked createdAt updatedAt closedAt mergedAt
      author { login }
      headRefName baseRefName headRefOid baseRefOid isCrossRepository maintainerCanModify
      headRepository { nameWithOwner }
      additions deletions changedFiles
      mergeable mergeStateStatus reviewDecision
      viewerCanUpdate viewerDidAuthor
      autoMergeRequest { enabledAt mergeMethod commitHeadline commitBody enabledBy { login } }
      mergedBy { login }
      labels(first: 30) { nodes { name color } }
      assignees(first: 20) { nodes { login } }
      milestone { title }
      reviewRequests(first: 30) { totalCount nodes { requestedReviewer { __typename ... on User { login } ... on Team { slug name } ... on Mannequin { login } ... on Bot { login } } } }
      commits(last: 1) { totalCount nodes { commit { oid statusCheckRollup { state } } } }
      comments { totalCount }
      reviews { totalCount }
      totalCommentsCount
      timelineItems(last: 100, itemTypes: [ISSUE_COMMENT, PULL_REQUEST_REVIEW, MERGED_EVENT, CLOSED_EVENT, REOPENED_EVENT, READY_FOR_REVIEW_EVENT, CONVERT_TO_DRAFT_EVENT, HEAD_REF_FORCE_PUSHED_EVENT, REVIEW_REQUESTED_EVENT, LABELED_EVENT]) {
        totalCount
        nodes {
          __typename
          ... on IssueComment { id databaseId author { login } body createdAt url }
          ... on PullRequestReview { id databaseId author { login } body state submittedAt createdAt url comments { totalCount } }
          ... on MergedEvent { actor { login } createdAt }
          ... on ClosedEvent { actor { login } createdAt }
          ... on ReopenedEvent { actor { login } createdAt }
          ... on ReadyForReviewEvent { actor { login } createdAt }
          ... on ConvertToDraftEvent { actor { login } createdAt }
          ... on HeadRefForcePushedEvent { actor { login } createdAt }
          ... on ReviewRequestedEvent { actor { login } createdAt requestedReviewer { __typename ... on User { login } ... on Team { slug name } } }
          ... on LabeledEvent { actor { login } createdAt label { name color } }
        }
      }
      reviewThreads(first: 100) {
        totalCount
        nodes {
          id isResolved isOutdated isCollapsed path line originalLine startLine originalStartLine
          diffSide startDiffSide subjectType
          resolvedBy { login }
          comments(first: 50) { totalCount nodes { id databaseId author { login } body createdAt url } }
        }
      }
    }
  }
}";

pub fn detail_variables(gh: &Gh, number: u64) -> Value {
    json!({ "owner": gh.owner, "name": gh.repo, "number": number })
}

/// Read the detail query's `data`.
pub fn parse_detail(data: &Value) -> Result<PrDetail> {
    let pr = data
        .get("repository")
        .and_then(|r| r.get("pullRequest"))
        .filter(|p| !p.is_null())
        .ok_or_else(|| Error::new("GitHub did not return the pull request"))?;
    Ok(PrDetail::from_json(pr))
}

/// Whether a GraphQL failure looks like GitHub's time limit (worth a retry
/// with a lighter query).
pub fn is_timeout(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("http 502")
        || m.contains("http 504")
        || m.contains("timeout")
        || m.contains("timed out")
        || m.contains("something went wrong while executing your query")
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

/// A GraphQL mutation, described without running it.
#[derive(Clone, Debug, PartialEq)]
pub struct Mutation {
    /// The mutation field, e.g. `enablePullRequestAutoMerge`.
    pub name: &'static str,
    pub query: &'static str,
    pub variables: Value,
}

impl Mutation {
    /// The command as logged: `gh api graphql (mutation resolveReviewThread)`.
    pub fn display(&self) -> String {
        format!(
            "gh api --method POST graphql --input - (mutation {})",
            self.name
        )
    }

    /// The variables as pretty JSON, for confirmations.
    pub fn preview(&self) -> String {
        serde_json::to_string_pretty(&self.variables).unwrap_or_default()
    }

    pub fn run(&self, gh: &Gh, calls: &mut Vec<crate::github::ApiCall>) -> Result<Value> {
        gh.graphql(self.query, self.variables.clone(), calls)
    }
}

const ENABLE_AUTO_MERGE: &str = "mutation($input: EnablePullRequestAutoMergeInput!) {
  enablePullRequestAutoMerge(input: $input) { pullRequest { number autoMergeRequest { enabledAt mergeMethod } } }
}";
const DISABLE_AUTO_MERGE: &str = "mutation($input: DisablePullRequestAutoMergeInput!) {
  disablePullRequestAutoMerge(input: $input) { pullRequest { number } }
}";
const MARK_READY: &str = "mutation($input: MarkPullRequestReadyForReviewInput!) {
  markPullRequestReadyForReview(input: $input) { pullRequest { number isDraft } }
}";
const CONVERT_TO_DRAFT: &str = "mutation($input: ConvertPullRequestToDraftInput!) {
  convertPullRequestToDraft(input: $input) { pullRequest { number isDraft } }
}";
const RESOLVE_THREAD: &str = "mutation($input: ResolveReviewThreadInput!) {
  resolveReviewThread(input: $input) { thread { id isResolved } }
}";
const UNRESOLVE_THREAD: &str = "mutation($input: UnresolveReviewThreadInput!) {
  unresolveReviewThread(input: $input) { thread { id isResolved } }
}";

/// Merge automatically once requirements are met. `expected_head` guards
/// against a push between looking and enabling.
pub fn enable_auto_merge(
    pr_id: &str,
    method: super::model::MergeMethod,
    headline: Option<&str>,
    body: Option<&str>,
    expected_head: &str,
) -> Mutation {
    let mut input = json!({
        "pullRequestId": pr_id,
        "mergeMethod": method.graphql(),
    });
    if !expected_head.is_empty() {
        input["expectedHeadOid"] = json!(expected_head);
    }
    if method.has_message() {
        if let Some(h) = headline.filter(|h| !h.trim().is_empty()) {
            input["commitHeadline"] = json!(h);
        }
        if let Some(b) = body.filter(|b| !b.trim().is_empty()) {
            input["commitBody"] = json!(b);
        }
    }
    Mutation {
        name: "enablePullRequestAutoMerge",
        query: ENABLE_AUTO_MERGE,
        variables: json!({ "input": input }),
    }
}

pub fn disable_auto_merge(pr_id: &str) -> Mutation {
    Mutation {
        name: "disablePullRequestAutoMerge",
        query: DISABLE_AUTO_MERGE,
        variables: json!({ "input": { "pullRequestId": pr_id } }),
    }
}

pub fn mark_ready_for_review(pr_id: &str) -> Mutation {
    Mutation {
        name: "markPullRequestReadyForReview",
        query: MARK_READY,
        variables: json!({ "input": { "pullRequestId": pr_id } }),
    }
}

pub fn convert_to_draft(pr_id: &str) -> Mutation {
    Mutation {
        name: "convertPullRequestToDraft",
        query: CONVERT_TO_DRAFT,
        variables: json!({ "input": { "pullRequestId": pr_id } }),
    }
}

pub fn resolve_thread(thread_id: &str) -> Mutation {
    Mutation {
        name: "resolveReviewThread",
        query: RESOLVE_THREAD,
        variables: json!({ "input": { "threadId": thread_id } }),
    }
}

pub fn unresolve_thread(thread_id: &str) -> Mutation {
    Mutation {
        name: "unresolveReviewThread",
        query: UNRESOLVE_THREAD,
        variables: json!({ "input": { "threadId": thread_id } }),
    }
}

/// Count of threads still open on a file, from a detail.
pub fn open_threads_on(detail: &PrDetail, path: &str) -> usize {
    detail.threads_for(path).filter(|t| !t.resolved).count()
}

#[cfg(test)]
mod tests {
    use super::super::model::{MergeMethod, Side, TimelineKind};
    use super::*;

    fn gh() -> Gh {
        Gh::new(".", "o", "r")
    }

    #[test]
    fn list_query_shapes() {
        let full = list_query(false);
        assert!(full.contains("mergeable mergeStateStatus"));
        assert!(full.contains("statusCheckRollup"));
        assert!(full.contains("review-requested") || full.contains("$search"));
        let light = list_query(true);
        assert!(!light.contains("mergeStateStatus"));
        assert!(!light.contains("{MERGE}"));
        let v = list_variables(&gh(), ListKind::Closed, PAGE, Some("abc"));
        assert_eq!(v["states"], json!(["CLOSED", "MERGED"]));
        assert_eq!(v["after"], "abc");
        assert_eq!(v["first"], 20);
        assert_eq!(v["search"], "is:pr is:open review-requested:@me repo:o/r");
        assert_eq!(
            list_variables(&gh(), ListKind::Open, 5, None)["after"],
            Value::Null
        );
    }

    #[test]
    fn parses_a_list_page() {
        let data = json!({
            "rateLimit": {"cost": 1, "remaining": 4973},
            "viewer": {"login": "me"},
            "repository": {
                "defaultBranchRef": {"name": "main"}, "squashMergeAllowed": true, "mergeCommitAllowed": false,
                "rebaseMergeAllowed": true, "viewerPermission": "WRITE", "owner": {"__typename": "User", "login": "o"},
                "pullRequests": {
                    "totalCount": 466,
                    "pageInfo": {"hasNextPage": true, "endCursor": "Y3Vy"},
                    "nodes": [{"number": 2, "title": "A", "isDraft": true}, {"number": 3, "title": "B"}]
                }
            },
            "requested": {"issueCount": 1, "nodes": [{"number": 3}, {}]}
        });
        let l = parse_list(&data, false).unwrap();
        assert_eq!(l.items.len(), 2);
        assert_eq!(l.total, 466);
        assert!(l.has_more);
        assert_eq!(l.end_cursor.as_deref(), Some("Y3Vy"));
        assert_eq!(l.viewer, "me");
        assert_eq!(l.requested, vec![3]);
        assert_eq!(l.meta.default_branch, "main");
        assert_eq!(
            l.meta.allowed_methods(),
            vec![MergeMethod::Squash, MergeMethod::Rebase]
        );
        assert_eq!(l.meta.owner_is_org, Some(false));
        assert_eq!(l.cost, Some(1));
        assert!(parse_list(&json!({"repository": null}), false).is_err());
    }

    #[test]
    fn parses_a_detail() {
        let data = json!({"repository": {"pullRequest": {
            "id": "PR_x", "number": 7, "body": "Hello", "state": "MERGED", "mergedBy": {"login": "m"},
            "autoMergeRequest": {"mergeMethod": "SQUASH", "enabledBy": {"login": "a"}},
            "timelineItems": {"totalCount": 1, "nodes": [{"__typename": "MergedEvent", "actor": {"login": "m"}, "createdAt": "2026-01-01T00:00:00Z"}]},
            "reviewThreads": {"totalCount": 1, "nodes": [{"id": "T1", "path": "a.rs", "line": 4, "diffSide": "RIGHT", "comments": {"totalCount": 1, "nodes": [{"databaseId": 11, "body": "hm"}]}}]}
        }}});
        let d = parse_detail(&data).unwrap();
        assert_eq!(d.pr.number, 7);
        assert_eq!(d.merged_by.as_deref(), Some("m"));
        assert_eq!(
            d.auto_merge.as_ref().and_then(|a| a.method),
            Some(MergeMethod::Squash)
        );
        assert_eq!(d.timeline[0].kind, TimelineKind::Merged);
        assert_eq!(d.threads[0].anchor(), Some((Side::Right, 4)));
        assert_eq!(open_threads_on(&d, "a.rs"), 1);
        assert!(parse_detail(&json!({"repository": {"pullRequest": null}})).is_err());
        let v = detail_variables(&gh(), 7);
        assert_eq!(v, json!({"owner": "o", "name": "r", "number": 7}));
    }

    #[test]
    fn timeouts() {
        assert!(is_timeout("gh: HTTP 502"));
        assert!(is_timeout(
            "Something went wrong while executing your query. This may be the result of a timeout"
        ));
        assert!(!is_timeout("Could not resolve to a Repository (HTTP 404)"));
    }

    #[test]
    fn auto_merge_mutations() {
        let m = enable_auto_merge(
            "PR_1",
            MergeMethod::Squash,
            Some("Title (#3)"),
            Some(""),
            "abc",
        );
        assert_eq!(m.name, "enablePullRequestAutoMerge");
        assert!(
            m.query
                .contains("enablePullRequestAutoMerge(input: $input)")
        );
        assert_eq!(
            m.variables,
            json!({"input": {"pullRequestId": "PR_1", "mergeMethod": "SQUASH", "expectedHeadOid": "abc", "commitHeadline": "Title (#3)"}})
        );
        let r = enable_auto_merge("PR_1", MergeMethod::Rebase, Some("ignored"), None, "");
        assert_eq!(
            r.variables,
            json!({"input": {"pullRequestId": "PR_1", "mergeMethod": "REBASE"}})
        );
        let d = disable_auto_merge("PR_1");
        assert!(d.query.contains("disablePullRequestAutoMerge"));
        assert_eq!(d.variables, json!({"input": {"pullRequestId": "PR_1"}}));
        assert!(d.display().contains("mutation disablePullRequestAutoMerge"));
        assert!(d.preview().contains("\"pullRequestId\": \"PR_1\""));
    }

    #[test]
    fn draft_and_thread_mutations() {
        let r = mark_ready_for_review("PR_1");
        assert!(
            r.query
                .contains("markPullRequestReadyForReview(input: $input)")
        );
        assert_eq!(r.variables, json!({"input": {"pullRequestId": "PR_1"}}));
        let d = convert_to_draft("PR_1");
        assert!(d.query.contains("convertPullRequestToDraft(input: $input)"));
        assert_eq!(d.variables, json!({"input": {"pullRequestId": "PR_1"}}));
        let t = resolve_thread("PRRT_1");
        assert!(t.query.contains("resolveReviewThread(input: $input)"));
        assert_eq!(t.variables, json!({"input": {"threadId": "PRRT_1"}}));
        let t = unresolve_thread("PRRT_1");
        assert!(t.query.contains("unresolveReviewThread(input: $input)"));
        assert_eq!(t.variables, json!({"input": {"threadId": "PRRT_1"}}));
    }
}
