//! One GraphQL query for the inbox searches: review requests, mentions,
//! assigned issues and pull requests, and the user's own open pull requests
//! (with review decision and the checks of their last commit), plus the
//! viewer's login and the GraphQL rate limit. One request, cost 1.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::github::parse_time;
use crate::{Error, Result};

/// GitHub search syntax of each bucket (the same strings work for
/// `GET search/issues?q=…`).
pub const REVIEW_QUERY: &str = "is:open is:pr review-requested:@me archived:false";
pub const MENTIONS_QUERY: &str = "is:open mentions:@me archived:false";
pub const ASSIGNED_QUERY: &str = "is:open assignee:@me archived:false";
pub const MINE_QUERY: &str = "is:open is:pr author:@me archived:false";

/// The GraphQL document.
pub const QUERY: &str = r#"query Inbox($review: String!, $mentions: String!, $assigned: String!, $mine: String!, $first: Int!) {
  viewer { login }
  rateLimit { cost remaining limit resetAt }
  review: search(query: $review, type: ISSUE, first: $first) { issueCount nodes { ...Item } }
  mentions: search(query: $mentions, type: ISSUE, first: $first) { issueCount nodes { ...Item } }
  assigned: search(query: $assigned, type: ISSUE, first: $first) { issueCount nodes { ...Item } }
  mine: search(query: $mine, type: ISSUE, first: $first) { issueCount nodes { ...Item } }
}
fragment Item on SearchResultItem {
  __typename
  ... on PullRequest {
    number title url isDraft createdAt updatedAt reviewDecision headRefName baseRefName
    repository { nameWithOwner }
    author { login }
    comments { totalCount }
    labels(first: 6) { nodes { name } }
    commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
  }
  ... on Issue {
    number title url createdAt updatedAt
    repository { nameWithOwner }
    author { login }
    comments { totalCount }
    labels(first: 6) { nodes { name } }
  }
}"#;

/// Variables for [`QUERY`]; `first` is capped at 100 (GitHub's limit).
pub fn variables(first: u32) -> Value {
    json!({
        "review": REVIEW_QUERY,
        "mentions": MENTIONS_QUERY,
        "assigned": ASSIGNED_QUERY,
        "mine": MINE_QUERY,
        "first": first.clamp(1, 100),
    })
}

/// Which list a search result belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Bucket {
    ReviewRequests,
    Mentions,
    Assigned,
    Mine,
}

impl Bucket {
    pub const ALL: [Bucket; 4] = [
        Bucket::ReviewRequests,
        Bucket::Mine,
        Bucket::Mentions,
        Bucket::Assigned,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Bucket::ReviewRequests => "Review requests",
            Bucket::Mentions => "Mentions",
            Bucket::Assigned => "Assigned",
            Bucket::Mine => "My pull requests",
        }
    }

    /// The GitHub search behind it.
    pub fn query(self) -> &'static str {
        match self {
            Bucket::ReviewRequests => REVIEW_QUERY,
            Bucket::Mentions => MENTIONS_QUERY,
            Bucket::Assigned => ASSIGNED_QUERY,
            Bucket::Mine => MINE_QUERY,
        }
    }

    /// `https://github.com/pulls?q=…` style page with the same search.
    pub fn web_url(self) -> String {
        let q = crate::devops::gh::encode(self.query());
        match self {
            Bucket::ReviewRequests | Bucket::Mine => format!("https://github.com/pulls?q={q}"),
            _ => format!("https://github.com/issues?q={q}"),
        }
    }
}

/// A pull request or issue found by a search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub pr: bool,
    pub number: u64,
    pub title: String,
    pub url: String,
    /// `owner/repo`.
    pub repo: String,
    pub author: String,
    pub created: Option<i64>,
    pub updated: Option<i64>,
    pub draft: bool,
    /// `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED` (pull requests with
    /// required reviews only).
    pub review_decision: Option<String>,
    /// Checks of the last commit: `SUCCESS`, `FAILURE`, `PENDING`, `ERROR`,
    /// `EXPECTED`.
    pub checks: Option<String>,
    pub head: Option<String>,
    pub base: Option<String>,
    pub comments: u64,
    pub labels: Vec<String>,
}

/// One bucket: what GitHub counted and the first items.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// GitHub's `issueCount` (may exceed `items.len()`).
    pub total: u64,
    pub items: Vec<Item>,
}

/// GraphQL rate limit after the query.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GqlRate {
    pub cost: u64,
    pub remaining: u64,
    pub limit: u64,
    pub reset: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Results {
    pub login: String,
    pub review: Found,
    pub mentions: Found,
    pub assigned: Found,
    pub mine: Found,
    pub rate: Option<GqlRate>,
}

impl Results {
    pub fn bucket(&self, b: Bucket) -> &Found {
        match b {
            Bucket::ReviewRequests => &self.review,
            Bucket::Mentions => &self.mentions,
            Bucket::Assigned => &self.assigned,
            Bucket::Mine => &self.mine,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Login {
    login: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Count {
    #[serde(rename = "totalCount")]
    total: u64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Name {
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Repo {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Rollup {
    state: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CommitNode {
    #[serde(rename = "statusCheckRollup")]
    status_check_rollup: Option<Rollup>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CommitWrap {
    commit: CommitNode,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct RawItem {
    #[serde(rename = "__typename")]
    typename: String,
    number: u64,
    title: String,
    url: String,
    is_draft: bool,
    created_at: String,
    updated_at: String,
    review_decision: Option<String>,
    head_ref_name: Option<String>,
    base_ref_name: Option<String>,
    repository: Repo,
    author: Option<Login>,
    comments: Count,
    labels: Nodes<Name>,
    commits: Nodes<CommitWrap>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct RawSearch {
    issue_count: u64,
    nodes: Vec<Option<RawItem>>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct RawRate {
    cost: u64,
    remaining: u64,
    limit: u64,
    reset_at: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct RawData {
    viewer: Login,
    rate_limit: Option<RawRate>,
    review: RawSearch,
    mentions: RawSearch,
    assigned: RawSearch,
    mine: RawSearch,
}

fn convert(s: RawSearch) -> Found {
    let items = s
        .nodes
        .into_iter()
        .flatten()
        // Nodes the token cannot see come back as empty objects.
        .filter(|n| n.number > 0 && !n.typename.is_empty())
        .map(|n| Item {
            pr: n.typename == "PullRequest",
            number: n.number,
            title: n.title,
            url: n.url,
            repo: n.repository.name_with_owner,
            author: n.author.map(|a| a.login).unwrap_or_default(),
            created: parse_time(&n.created_at),
            updated: parse_time(&n.updated_at),
            draft: n.is_draft,
            review_decision: n.review_decision.filter(|s| !s.is_empty()),
            checks: n
                .commits
                .nodes
                .into_iter()
                .next_back()
                .and_then(|c| c.commit.status_check_rollup)
                .and_then(|r| r.state),
            head: n.head_ref_name,
            base: n.base_ref_name,
            comments: n.comments.total,
            labels: n.labels.nodes.into_iter().map(|l| l.name).collect(),
        })
        .collect();
    Found {
        total: s.issue_count,
        items,
    }
}

/// Parse the `data` of [`QUERY`].
pub fn parse(data: &Value) -> Result<Results> {
    if data.is_null() {
        return Err(Error::new("GitHub search: empty answer"));
    }
    let raw: RawData = serde_json::from_value(data.clone())
        .map_err(|e| Error::new(format!("GitHub search: {e}")))?;
    Ok(Results {
        login: raw.viewer.login,
        rate: raw.rate_limit.map(|r| GqlRate {
            cost: r.cost,
            remaining: r.remaining,
            limit: r.limit,
            reset: parse_time(&r.reset_at),
        }),
        review: convert(raw.review),
        mentions: convert(raw.mentions),
        assigned: convert(raw.assigned),
        mine: convert(raw.mine),
    })
}

/// `Approved`, `Changes requested`, `Review required`.
pub fn decision_label(d: &str) -> &'static str {
    match d {
        "APPROVED" => "Approved",
        "CHANGES_REQUESTED" => "Changes requested",
        "REVIEW_REQUIRED" => "Review required",
        _ => "Reviewed",
    }
}

/// `Checks passing`, `Checks failing`, `Checks running`.
pub fn checks_label(s: &str) -> &'static str {
    match s {
        "SUCCESS" => "Checks passing",
        "FAILURE" | "ERROR" => "Checks failing",
        "PENDING" | "EXPECTED" => "Checks running",
        _ => "Checks",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DATA: &str = r#"{
      "viewer": {"login": "octo"},
      "rateLimit": {"cost": 1, "remaining": 4926, "limit": 5000, "resetAt": "2026-10-26T19:43:07Z"},
      "review": {"issueCount": 3, "nodes": [
        {"__typename": "PullRequest", "number": 42, "title": "Add retry", "url": "https://github.com/acme/web/pull/42",
         "isDraft": false, "createdAt": "2026-10-20T09:00:00Z", "updatedAt": "2026-10-26T10:00:00Z",
         "reviewDecision": "REVIEW_REQUIRED", "headRefName": "retry", "baseRefName": "main",
         "repository": {"nameWithOwner": "acme/web"}, "author": {"login": "dana"},
         "comments": {"totalCount": 4}, "labels": {"nodes": [{"name": "backend"}]},
         "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "FAILURE"}}}]}},
        {},
        null
      ]},
      "mentions": {"issueCount": 1, "nodes": [
        {"__typename": "Issue", "number": 7, "title": "Crash on start", "url": "https://github.com/acme/api/issues/7",
         "createdAt": "2026-10-25T09:00:00Z", "updatedAt": "2026-10-25T10:00:00Z",
         "repository": {"nameWithOwner": "acme/api"}, "author": null,
         "comments": {"totalCount": 0}, "labels": {"nodes": []}}
      ]},
      "assigned": {"issueCount": 0, "nodes": []},
      "mine": {"issueCount": 1, "nodes": [
        {"__typename": "PullRequest", "number": 3, "title": "Preview env", "url": "https://github.com/octo/db/pull/3",
         "isDraft": true, "createdAt": "2026-09-20T09:55:56Z", "updatedAt": "2026-09-25T20:37:42Z",
         "reviewDecision": null, "headRefName": "preview/e2e", "baseRefName": "main",
         "repository": {"nameWithOwner": "octo/db"}, "author": {"login": "octo"},
         "comments": {"totalCount": 2}, "labels": {"nodes": []},
         "commits": {"nodes": [{"commit": {"statusCheckRollup": null}}]}}
      ]}
    }"#;

    #[test]
    fn parses_every_bucket() {
        let data: Value = serde_json::from_str(DATA).unwrap();
        let r = parse(&data).unwrap();
        assert_eq!(r.login, "octo");
        let rate = r.rate.clone().unwrap();
        assert_eq!((rate.cost, rate.remaining, rate.limit), (1, 4926, 5000));
        assert_eq!(r.review.total, 3);
        assert_eq!(r.review.items.len(), 1, "empty and null nodes are dropped");
        let pr = &r.review.items[0];
        assert!(pr.pr);
        assert_eq!(pr.repo, "acme/web");
        assert_eq!(pr.author, "dana");
        assert_eq!(pr.checks.as_deref(), Some("FAILURE"));
        assert_eq!(pr.review_decision.as_deref(), Some("REVIEW_REQUIRED"));
        assert_eq!(pr.head.as_deref(), Some("retry"));
        assert_eq!(pr.labels, vec!["backend"]);
        assert_eq!(pr.comments, 4);
        let issue = &r.mentions.items[0];
        assert!(!issue.pr);
        assert_eq!(issue.author, "");
        assert_eq!(issue.checks, None);
        assert!(r.assigned.items.is_empty());
        let mine = &r.bucket(Bucket::Mine).items[0];
        assert!(mine.draft);
        assert_eq!(mine.checks, None);
        assert_eq!(mine.updated, parse_time("2026-09-25T20:37:42Z"));
    }

    #[test]
    fn variables_and_queries() {
        let v = variables(500);
        assert_eq!(v["first"], 100);
        assert_eq!(v["review"], REVIEW_QUERY);
        assert_eq!(v["mine"], "is:open is:pr author:@me archived:false");
        for b in Bucket::ALL {
            assert!(QUERY.contains(&format!("{}: search", key(b))));
            assert!(b.web_url().starts_with("https://github.com/"));
        }
        assert!(
            Bucket::ReviewRequests
                .web_url()
                .contains("review-requested%3A%40me")
        );
        assert!(parse(&Value::Null).is_err());
    }

    fn key(b: Bucket) -> &'static str {
        match b {
            Bucket::ReviewRequests => "review",
            Bucket::Mentions => "mentions",
            Bucket::Assigned => "assigned",
            Bucket::Mine => "mine",
        }
    }

    #[test]
    fn labels() {
        assert_eq!(decision_label("APPROVED"), "Approved");
        assert_eq!(checks_label("ERROR"), "Checks failing");
        assert_eq!(checks_label("PENDING"), "Checks running");
    }
}
