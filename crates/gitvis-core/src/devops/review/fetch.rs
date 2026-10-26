//! Reads of the review loop through `gh api` (every call recorded).

use serde_json::Value;

use super::graphql::{self, ListKind};
use super::model::{
    Issue, IssueComment, Label, PrCommit, PrDetail, PrFile, PrList, RepoMeta, Suggestions,
};
use crate::Result;
use crate::devops::gh::{Gh, encode};
use crate::github::ApiCall;

/// `GET repos/o/r`: default branch, issues on or off, permissions and (for
/// users with push access) the allowed merge methods.
pub fn fetch_repo_meta(gh: &Gh, calls: &mut Vec<ApiCall>) -> Result<RepoMeta> {
    let v: Value = gh.get_json(&gh.path(""), "repository", calls)?;
    Ok(RepoMeta::from_rest(&v))
}

/// One page of a pull request list. When the full query runs into
/// GitHub's time limit, the light one (no mergeability) is tried once.
pub fn fetch_pr_list(
    gh: &Gh,
    kind: ListKind,
    after: Option<&str>,
    calls: &mut Vec<ApiCall>,
) -> Result<PrList> {
    let full = graphql::list_query(false);
    let vars = graphql::list_variables(gh, kind, graphql::PAGE, after);
    match gh.graphql(&full, vars, calls) {
        Ok(data) => graphql::parse_list(&data, false),
        Err(e) if graphql::is_timeout(&e.to_string()) => {
            let light = graphql::list_query(true);
            let vars = graphql::list_variables(gh, kind, graphql::PAGE_LIGHT, after);
            let data = gh.graphql(&light, vars, calls)?;
            graphql::parse_list(&data, true)
        }
        Err(e) => Err(e),
    }
}

/// Conversation, merge state, auto-merge and review threads of one pull
/// request.
pub fn fetch_pr_detail(gh: &Gh, number: u64, calls: &mut Vec<ApiCall>) -> Result<PrDetail> {
    let data = gh.graphql(
        graphql::DETAIL_QUERY,
        graphql::detail_variables(gh, number),
        calls,
    )?;
    graphql::parse_detail(&data)
}

/// Most files GitHub lists for a pull request (it stops at 3,000).
pub const MAX_FILE_PAGES: usize = 30;

/// `GET pulls/n/files`, every page.
pub fn fetch_pr_files(gh: &Gh, number: u64, calls: &mut Vec<ApiCall>) -> Result<Vec<PrFile>> {
    let items: Vec<Value> = gh.get_pages(
        &gh.path(&format!("pulls/{number}/files")),
        "changed files",
        MAX_FILE_PAGES,
        calls,
    )?;
    Ok(items.iter().map(PrFile::from_json).collect())
}

/// `GET pulls/n/commits` (GitHub lists at most 250).
pub fn fetch_pr_commits(gh: &Gh, number: u64, calls: &mut Vec<ApiCall>) -> Result<Vec<PrCommit>> {
    let items: Vec<Value> = gh.get_pages(
        &gh.path(&format!("pulls/{number}/commits")),
        "commits",
        3,
        calls,
    )?;
    Ok(items.iter().map(PrCommit::from_json).collect())
}

/// Server-side filters of the issue list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct IssueQuery {
    pub open: bool,
    pub assignee: Option<String>,
    pub creator: Option<String>,
    pub mentioned: Option<String>,
    pub labels: Vec<String>,
}

/// `repos/o/r/issues?state=…&sort=updated…` for a query.
pub fn issues_endpoint(gh: &Gh, q: &IssueQuery) -> String {
    let mut params = vec![
        format!("state={}", if q.open { "open" } else { "closed" }),
        "sort=updated".to_owned(),
        "direction=desc".to_owned(),
    ];
    if let Some(a) = &q.assignee {
        params.push(format!("assignee={}", encode(a)));
    }
    if let Some(c) = &q.creator {
        params.push(format!("creator={}", encode(c)));
    }
    if let Some(m) = &q.mentioned {
        params.push(format!("mentioned={}", encode(m)));
    }
    if !q.labels.is_empty() {
        let joined = q
            .labels
            .iter()
            .map(|l| encode(l))
            .collect::<Vec<_>>()
            .join(",");
        params.push(format!("labels={joined}"));
    }
    format!("{}?{}", gh.path("issues"), params.join("&"))
}

/// Issues matching `q` (pull requests, which the endpoint also returns, are
/// left out), at most `max_pages` pages of 100.
pub fn fetch_issues(
    gh: &Gh,
    q: &IssueQuery,
    max_pages: usize,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<Issue>> {
    let items: Vec<Value> = gh.get_pages(&issues_endpoint(gh, q), "issues", max_pages, calls)?;
    Ok(items
        .iter()
        .map(Issue::from_json)
        .filter(|i| !i.is_pr)
        .collect())
}

/// One issue and its comments.
pub fn fetch_issue(
    gh: &Gh,
    number: u64,
    calls: &mut Vec<ApiCall>,
) -> Result<(Issue, Vec<IssueComment>)> {
    let v: Value = gh.get_json(&gh.path(&format!("issues/{number}")), "issue", calls)?;
    let comments: Vec<Value> = gh.get_pages(
        &gh.path(&format!("issues/{number}/comments")),
        "comments",
        5,
        calls,
    )?;
    Ok((
        Issue::from_json(&v),
        comments.iter().map(IssueComment::from_json).collect(),
    ))
}

/// Users, labels and teams to suggest. `collaborators` needs push access;
/// on HTTP 403 the assignable users are used instead.
pub fn fetch_suggestions(gh: &Gh, org: bool, calls: &mut Vec<ApiCall>) -> Result<Suggestions> {
    let mut out = Suggestions::default();
    match gh.get_pages::<Value>(&gh.path("collaborators"), "collaborators", 3, calls) {
        Ok(users) => {
            out.users = logins(&users);
            out.users_source = "collaborators".to_owned();
        }
        Err(e) if e.to_string().contains("HTTP 403") || e.to_string().contains("HTTP 404") => {
            let users: Vec<Value> = gh.get_pages(&gh.path("assignees"), "assignees", 3, calls)?;
            out.users = logins(&users);
            out.users_source = "assignees".to_owned();
        }
        Err(e) => return Err(e),
    }
    let labels: Vec<Value> = gh.get_pages(&gh.path("labels"), "labels", 3, calls)?;
    out.labels = labels.iter().map(Label::from_json).collect();
    if org {
        match gh.get_pages::<Value>(&format!("orgs/{}/teams", gh.owner), "teams", 2, calls) {
            Ok(teams) => {
                out.teams = teams
                    .iter()
                    .filter_map(|t| t.get("slug").and_then(Value::as_str))
                    .map(str::to_owned)
                    .collect();
            }
            Err(e) => out.teams_note = e.to_string(),
        }
    } else {
        out.teams_note = "Teams exist only in organisations.".to_owned();
    }
    Ok(out)
}

fn logins(users: &[Value]) -> Vec<String> {
    let mut v: Vec<String> = users
        .iter()
        .filter_map(|u| u.get("login").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    v.sort_by_key(|s| s.to_lowercase());
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_list_endpoints() {
        let gh = Gh::new(".", "o", "r");
        let q = IssueQuery {
            open: true,
            ..Default::default()
        };
        assert_eq!(
            issues_endpoint(&gh, &q),
            "repos/o/r/issues?state=open&sort=updated&direction=desc"
        );
        let q = IssueQuery {
            open: false,
            assignee: Some("me".into()),
            labels: vec!["bug".into(), "good first issue".into()],
            ..Default::default()
        };
        assert_eq!(
            issues_endpoint(&gh, &q),
            "repos/o/r/issues?state=closed&sort=updated&direction=desc&assignee=me&labels=bug,good%20first%20issue"
        );
        let q = IssueQuery {
            open: true,
            creator: Some("a".into()),
            mentioned: Some("b".into()),
            ..Default::default()
        };
        assert!(issues_endpoint(&gh, &q).ends_with("&creator=a&mentioned=b"));
    }
}
