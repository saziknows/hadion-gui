//! REST requests of the review loop, built by pure functions so tests can
//! check method, endpoint and body, plus [`Step`]: one REST request or
//! GraphQL mutation of a change, run in order by [`run_steps`].

use std::fmt;

use serde_json::{Value, json};

use super::graphql::Mutation;
use super::model::{MergeMethod, Side};
use crate::devops::gh::{Gh, Request, encode};
use crate::github::ApiCall;

/// The number of the pull request in an endpoint: known, or the one the
/// first step of the change creates (written `{number}` until it exists).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Num {
    Is(u64),
    Created,
}

/// What stands for the created pull request's number in an endpoint.
pub const NEW_NUMBER: &str = "{number}";

impl From<u64> for Num {
    fn from(n: u64) -> Self {
        Num::Is(n)
    }
}

impl fmt::Display for Num {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Num::Is(n) => write!(f, "{n}"),
            Num::Created => f.write_str(NEW_NUMBER),
        }
    }
}

/// `feature/x y` → `feature/x%20y`: each path segment encoded, slashes kept.
pub fn encode_ref(branch: &str) -> String {
    branch.split('/').map(encode).collect::<Vec<_>>().join("/")
}

// ---------------------------------------------------------------------------
// Pull requests
// ---------------------------------------------------------------------------

/// What a new pull request is made of.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct NewPr {
    pub title: String,
    /// `branch`, or `owner:branch` for a fork.
    pub head: String,
    pub base: String,
    pub body: String,
    pub draft: bool,
}

/// `POST pulls`.
pub fn create_pr(gh: &Gh, pr: &NewPr) -> Request {
    Request::post(
        gh.path("pulls"),
        json!({
            "title": pr.title.trim(),
            "head": pr.head,
            "base": pr.base,
            "body": pr.body,
            "draft": pr.draft,
            "maintainer_can_modify": true,
        }),
    )
}

/// `POST issues/n/labels` (adds to the existing labels).
pub fn add_labels(gh: &Gh, n: impl Into<Num>, labels: &[String]) -> Request {
    Request::post(
        gh.path(&format!("issues/{}/labels", n.into())),
        json!({ "labels": labels }),
    )
}

/// `POST pulls/n/requested_reviewers`.
pub fn request_reviewers(
    gh: &Gh,
    n: impl Into<Num>,
    users: &[String],
    teams: &[String],
) -> Request {
    Request::post(
        gh.path(&format!("pulls/{}/requested_reviewers", n.into())),
        json!({ "reviewers": users, "team_reviewers": teams }),
    )
}

/// `POST issues/n/assignees` (adds to the existing assignees).
pub fn add_assignees(gh: &Gh, n: impl Into<Num>, users: &[String]) -> Request {
    Request::post(
        gh.path(&format!("issues/{}/assignees", n.into())),
        json!({ "assignees": users }),
    )
}

/// `PUT pulls/n/merge`. `sha` makes GitHub refuse when the head moved
/// since it was shown. Title and message are left out for rebase merges
/// and when empty (GitHub then uses the repository's defaults).
pub fn merge_pr(
    gh: &Gh,
    n: u64,
    method: MergeMethod,
    sha: &str,
    title: Option<&str>,
    message: Option<&str>,
) -> Request {
    let mut body = json!({ "merge_method": method.rest(), "sha": sha });
    if method.has_message() {
        if let Some(t) = title.map(str::trim).filter(|t| !t.is_empty()) {
            body["commit_title"] = json!(t);
        }
        if let Some(m) = message.filter(|m| !m.trim().is_empty()) {
            body["commit_message"] = json!(m);
        }
    }
    Request::put(gh.path(&format!("pulls/{n}/merge")), body)
}

/// `DELETE git/refs/heads/BRANCH`.
pub fn delete_branch(gh: &Gh, branch: &str) -> Request {
    Request::delete(gh.path(&format!("git/refs/heads/{}", encode_ref(branch))))
}

/// `PUT pulls/n/update-branch`: merge the base into the head on GitHub.
pub fn update_branch(gh: &Gh, n: u64, expected_head_sha: &str) -> Request {
    Request::put(
        gh.path(&format!("pulls/{n}/update-branch")),
        json!({ "expected_head_sha": expected_head_sha }),
    )
}

/// `PATCH pulls/n {state}`: close or reopen.
pub fn set_pr_state(gh: &Gh, n: u64, open: bool) -> Request {
    Request::patch(
        gh.path(&format!("pulls/{n}")),
        json!({ "state": if open { "open" } else { "closed" } }),
    )
}

// ---------------------------------------------------------------------------
// Reviews
// ---------------------------------------------------------------------------

/// A comment on a line or a range of lines, kept locally until it is sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftComment {
    pub path: String,
    /// The last line of the range.
    pub line: u32,
    pub side: Side,
    /// The first line, for a multi-line comment.
    pub start_line: Option<u32>,
    pub start_side: Option<Side>,
    pub body: String,
}

impl DraftComment {
    /// The comment object GitHub takes in a review's `comments` and in
    /// `POST pulls/n/comments`.
    pub fn json(&self) -> Value {
        let mut v = json!({
            "path": self.path,
            "line": self.line,
            "side": self.side.api(),
            "body": self.body,
        });
        if let Some(start) = self
            .start_line
            .filter(|s| *s != self.line || self.start_side != Some(self.side))
        {
            v["start_line"] = json!(start);
            v["start_side"] = json!(self.start_side.unwrap_or(self.side).api());
        }
        v
    }

    /// `line 12`, `lines 10–14`.
    pub fn range_label(&self) -> String {
        match self.start_line {
            Some(s) if s != self.line => format!("lines {s}–{}", self.line),
            _ => format!("line {}", self.line),
        }
    }

    /// Whether this comment covers `(side, line)` at its end line.
    pub fn ends_at(&self, side: Side, line: u32) -> bool {
        self.side == side && self.line == line
    }
}

/// What a submitted review says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ReviewEvent {
    #[default]
    Comment,
    Approve,
    RequestChanges,
}

impl ReviewEvent {
    pub const ALL: [ReviewEvent; 3] = [
        ReviewEvent::Comment,
        ReviewEvent::Approve,
        ReviewEvent::RequestChanges,
    ];

    pub fn api(self) -> &'static str {
        match self {
            ReviewEvent::Comment => "COMMENT",
            ReviewEvent::Approve => "APPROVE",
            ReviewEvent::RequestChanges => "REQUEST_CHANGES",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ReviewEvent::Comment => "Comment",
            ReviewEvent::Approve => "Approve",
            ReviewEvent::RequestChanges => "Request changes",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            ReviewEvent::Comment => "General feedback without explicit approval",
            ReviewEvent::Approve => "Approve merging these changes",
            ReviewEvent::RequestChanges => "Feedback that must be addressed before merging",
        }
    }
}

/// Why a review cannot be sent as it is, if it cannot.
pub fn review_problem(event: ReviewEvent, body: &str, comments: usize) -> Option<&'static str> {
    let empty = body.trim().is_empty();
    match event {
        ReviewEvent::RequestChanges if empty => {
            Some("Say what has to change: requesting changes needs a summary.")
        }
        ReviewEvent::Comment if empty && comments == 0 => {
            Some("Write a summary or add comments on lines first.")
        }
        _ => None,
    }
}

/// `POST pulls/n/reviews`: submit a review with its line comments in one go.
pub fn submit_review(
    gh: &Gh,
    n: u64,
    commit_id: &str,
    body: &str,
    event: ReviewEvent,
    comments: &[DraftComment],
) -> Request {
    let mut v = json!({
        "commit_id": commit_id,
        "event": event.api(),
        "comments": comments.iter().map(DraftComment::json).collect::<Vec<_>>(),
    });
    if !body.trim().is_empty() {
        v["body"] = json!(body);
    }
    Request::post(gh.path(&format!("pulls/{n}/reviews")), v)
}

/// `POST pulls/n/comments`: one comment outside a review.
pub fn single_comment(gh: &Gh, n: u64, commit_id: &str, c: &DraftComment) -> Request {
    let mut v = c.json();
    v["commit_id"] = json!(commit_id);
    Request::post(gh.path(&format!("pulls/{n}/comments")), v)
}

/// `POST pulls/n/comments/ID/replies`: answer a review thread.
pub fn reply_to_comment(gh: &Gh, n: u64, comment_id: u64, body: &str) -> Request {
    Request::post(
        gh.path(&format!("pulls/{n}/comments/{comment_id}/replies")),
        json!({ "body": body }),
    )
}

// ---------------------------------------------------------------------------
// Issues (and pull request conversations, which are issues)
// ---------------------------------------------------------------------------

/// `POST issues/n/comments`.
pub fn issue_comment(gh: &Gh, n: u64, body: &str) -> Request {
    Request::post(
        gh.path(&format!("issues/{n}/comments")),
        json!({ "body": body }),
    )
}

/// Why an issue is closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CloseReason {
    #[default]
    Completed,
    NotPlanned,
}

impl CloseReason {
    pub fn api(self) -> &'static str {
        match self {
            CloseReason::Completed => "completed",
            CloseReason::NotPlanned => "not_planned",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CloseReason::Completed => "Close as completed",
            CloseReason::NotPlanned => "Close as not planned",
        }
    }
}

/// `PATCH issues/n {state, state_reason}`: close with a reason or reopen.
pub fn set_issue_state(gh: &Gh, n: u64, open: bool, reason: CloseReason) -> Request {
    let body = if open {
        json!({ "state": "open", "state_reason": "reopened" })
    } else {
        json!({ "state": "closed", "state_reason": reason.api() })
    };
    Request::patch(gh.path(&format!("issues/{n}")), body)
}

/// `PATCH issues/n {labels, assignees}`: replace both lists.
pub fn edit_issue(gh: &Gh, n: u64, labels: &[String], assignees: &[String]) -> Request {
    Request::patch(
        gh.path(&format!("issues/{n}")),
        json!({ "labels": labels, "assignees": assignees }),
    )
}

/// `POST issues`.
pub fn create_issue(
    gh: &Gh,
    title: &str,
    body: &str,
    labels: &[String],
    assignees: &[String],
) -> Request {
    Request::post(
        gh.path("issues"),
        json!({
            "title": title.trim(),
            "body": body,
            "labels": labels,
            "assignees": assignees,
        }),
    )
}

// ---------------------------------------------------------------------------
// Steps of a change
// ---------------------------------------------------------------------------

/// One request of a change.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Rest(Request),
    Gql(Mutation),
}

impl Step {
    /// The command line as logged and confirmed.
    pub fn display(&self) -> String {
        match self {
            Step::Rest(r) => r.display(),
            Step::Gql(m) => m.display(),
        }
    }

    /// What a confirmation shows: the command, then the body.
    pub fn confirmation(&self) -> String {
        let body = match self {
            Step::Rest(r) => r.body_preview(),
            Step::Gql(m) => Some(m.preview()),
        };
        let mut s = match self {
            Step::Rest(r) => format!("{} {}", r.method.as_str(), r.endpoint),
            Step::Gql(m) => format!("POST graphql  (mutation {})", m.name),
        };
        if let Some(b) = body {
            s.push('\n');
            s.push_str(&b);
        }
        s
    }

    /// The step with `{number}` replaced by the created pull request's.
    pub fn with_number(&self, n: u64) -> Step {
        match self {
            Step::Rest(r) => {
                let mut r = r.clone();
                r.endpoint = r.endpoint.replace(NEW_NUMBER, &n.to_string());
                Step::Rest(r)
            }
            other => other.clone(),
        }
    }

    pub fn run(&self, gh: &Gh, calls: &mut Vec<ApiCall>) -> crate::Result<Value> {
        match self {
            Step::Rest(r) => gh.send_json::<Value>(r, "answer", calls),
            Step::Gql(m) => m.run(gh, calls),
        }
    }
}

/// How a change went.
#[derive(Clone, Debug, PartialEq)]
pub struct StepsOutcome {
    /// Steps that succeeded, in order.
    pub done: usize,
    /// The answer of the first step (e.g. the created pull request).
    pub first: Option<Value>,
    /// The number `{number}` stood for.
    pub number: Option<u64>,
    /// The failing step and its error.
    pub error: Option<(usize, String)>,
}

/// Run steps in order, stopping at the first failure. A `number` in the
/// first answer replaces `{number}` in later endpoints.
pub fn run_steps(gh: &Gh, steps: &[Step], calls: &mut Vec<ApiCall>) -> StepsOutcome {
    let mut out = StepsOutcome {
        done: 0,
        first: None,
        number: None,
        error: None,
    };
    for (i, step) in steps.iter().enumerate() {
        let step = match out.number {
            Some(n) => step.with_number(n),
            None => step.clone(),
        };
        if let Step::Rest(r) = &step
            && r.endpoint.contains(NEW_NUMBER)
        {
            out.error = Some((
                i,
                "The pull request number is unknown: the first request did not answer one"
                    .to_owned(),
            ));
            return out;
        }
        match step.run(gh, calls) {
            Ok(v) => {
                if i == 0 {
                    out.number = v.get("number").and_then(Value::as_u64);
                    out.first = Some(v);
                }
                out.done += 1;
            }
            Err(e) => {
                out.error = Some((i, e.to_string()));
                return out;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gh() -> Gh {
        Gh::new(".", "o", "r")
    }

    fn body(r: &Request) -> &Value {
        r.body.as_ref().expect("a body")
    }

    #[test]
    fn create_pr_request() {
        let r = create_pr(
            &gh(),
            &NewPr {
                title: "  Add login  ".into(),
                head: "me:feature/login".into(),
                base: "main".into(),
                body: "Body".into(),
                draft: true,
            },
        );
        assert_eq!(r.method, crate::devops::gh::Method::Post);
        assert_eq!(r.endpoint, "repos/o/r/pulls");
        assert_eq!(
            body(&r),
            &json!({"title": "Add login", "head": "me:feature/login", "base": "main", "body": "Body", "draft": true, "maintainer_can_modify": true})
        );
    }

    #[test]
    fn follow_up_requests_of_a_new_pr() {
        let labels = add_labels(&gh(), Num::Created, &["bug".into()]);
        assert_eq!(labels.endpoint, "repos/o/r/issues/{number}/labels");
        assert_eq!(body(&labels), &json!({"labels": ["bug"]}));
        let rev = request_reviewers(&gh(), 12, &["ann".into()], &["core".into()]);
        assert_eq!(rev.method, crate::devops::gh::Method::Post);
        assert_eq!(rev.endpoint, "repos/o/r/pulls/12/requested_reviewers");
        assert_eq!(
            body(&rev),
            &json!({"reviewers": ["ann"], "team_reviewers": ["core"]})
        );
        let asg = add_assignees(&gh(), 12, &["bob".into()]);
        assert_eq!(asg.endpoint, "repos/o/r/issues/12/assignees");
        assert_eq!(body(&asg), &json!({"assignees": ["bob"]}));
        let s = Step::Rest(labels).with_number(41);
        assert_eq!(
            s.display(),
            "gh api --method POST repos/o/r/issues/41/labels --input - (JSON body)"
        );
    }

    #[test]
    fn merge_requests_per_method() {
        let m = merge_pr(
            &gh(),
            7,
            MergeMethod::Merge,
            "abc",
            Some("Merge it (#7)"),
            Some("Body"),
        );
        assert_eq!(m.method, crate::devops::gh::Method::Put);
        assert_eq!(m.endpoint, "repos/o/r/pulls/7/merge");
        assert_eq!(
            body(&m),
            &json!({"merge_method": "merge", "sha": "abc", "commit_title": "Merge it (#7)", "commit_message": "Body"})
        );
        let s = merge_pr(&gh(), 7, MergeMethod::Squash, "abc", Some(" "), Some(""));
        assert_eq!(body(&s), &json!({"merge_method": "squash", "sha": "abc"}));
        let s = merge_pr(&gh(), 7, MergeMethod::Squash, "abc", Some("T"), None);
        assert_eq!(
            body(&s),
            &json!({"merge_method": "squash", "sha": "abc", "commit_title": "T"})
        );
        let r = merge_pr(&gh(), 7, MergeMethod::Rebase, "abc", Some("T"), Some("M"));
        assert_eq!(body(&r), &json!({"merge_method": "rebase", "sha": "abc"}));
    }

    #[test]
    fn branch_and_state_requests() {
        let d = delete_branch(&gh(), "feature/a b");
        assert_eq!(d.method, crate::devops::gh::Method::Delete);
        assert_eq!(d.endpoint, "repos/o/r/git/refs/heads/feature/a%20b");
        assert!(d.body.is_none());
        let u = update_branch(&gh(), 7, "abc");
        assert_eq!(u.method, crate::devops::gh::Method::Put);
        assert_eq!(u.endpoint, "repos/o/r/pulls/7/update-branch");
        assert_eq!(body(&u), &json!({"expected_head_sha": "abc"}));
        let c = set_pr_state(&gh(), 7, false);
        assert_eq!(c.method, crate::devops::gh::Method::Patch);
        assert_eq!(c.endpoint, "repos/o/r/pulls/7");
        assert_eq!(body(&c), &json!({"state": "closed"}));
        assert_eq!(
            body(&set_pr_state(&gh(), 7, true)),
            &json!({"state": "open"})
        );
    }

    fn single_line() -> DraftComment {
        DraftComment {
            path: "src/a.rs".into(),
            line: 12,
            side: Side::Right,
            start_line: None,
            start_side: None,
            body: "Typo".into(),
        }
    }

    fn range() -> DraftComment {
        DraftComment {
            path: "src/a.rs".into(),
            line: 14,
            side: Side::Right,
            start_line: Some(10),
            start_side: Some(Side::Left),
            body: "```suggestion\nlet x = 1;\n```".into(),
        }
    }

    #[test]
    fn review_with_single_and_multi_line_comments() {
        let r = submit_review(
            &gh(),
            7,
            "head",
            "Looks good",
            ReviewEvent::Approve,
            &[single_line(), range()],
        );
        assert_eq!(r.method, crate::devops::gh::Method::Post);
        assert_eq!(r.endpoint, "repos/o/r/pulls/7/reviews");
        assert_eq!(
            body(&r),
            &json!({
                "commit_id": "head",
                "event": "APPROVE",
                "body": "Looks good",
                "comments": [
                    {"path": "src/a.rs", "line": 12, "side": "RIGHT", "body": "Typo"},
                    {"path": "src/a.rs", "line": 14, "side": "RIGHT", "start_line": 10, "start_side": "LEFT", "body": "```suggestion\nlet x = 1;\n```"}
                ]
            })
        );
        let c = submit_review(&gh(), 7, "head", "", ReviewEvent::Comment, &[single_line()]);
        assert!(body(&c).get("body").is_none());
        assert_eq!(body(&c)["event"], "COMMENT");
        assert_eq!(
            body(&submit_review(
                &gh(),
                7,
                "h",
                "No",
                ReviewEvent::RequestChanges,
                &[]
            ))["event"],
            "REQUEST_CHANGES"
        );
        assert_eq!(range().range_label(), "lines 10–14");
        assert_eq!(single_line().range_label(), "line 12");
    }

    #[test]
    fn review_validation() {
        assert!(review_problem(ReviewEvent::RequestChanges, " ", 3).is_some());
        assert!(review_problem(ReviewEvent::Comment, "", 0).is_some());
        assert!(review_problem(ReviewEvent::Comment, "", 1).is_none());
        assert!(review_problem(ReviewEvent::Approve, "", 0).is_none());
    }

    #[test]
    fn single_comment_and_reply() {
        let s = single_comment(&gh(), 7, "head", &range());
        assert_eq!(s.endpoint, "repos/o/r/pulls/7/comments");
        assert_eq!(
            body(&s),
            &json!({"path": "src/a.rs", "line": 14, "side": "RIGHT", "start_line": 10, "start_side": "LEFT", "body": "```suggestion\nlet x = 1;\n```", "commit_id": "head"})
        );
        let one = single_comment(&gh(), 7, "head", &single_line());
        assert!(body(&one).get("start_line").is_none());
        let r = reply_to_comment(&gh(), 7, 4288317385, "Done");
        assert_eq!(r.method, crate::devops::gh::Method::Post);
        assert_eq!(r.endpoint, "repos/o/r/pulls/7/comments/4288317385/replies");
        assert_eq!(body(&r), &json!({"body": "Done"}));
    }

    #[test]
    fn issue_requests() {
        let c = issue_comment(&gh(), 5, "Hi");
        assert_eq!(c.endpoint, "repos/o/r/issues/5/comments");
        assert_eq!(body(&c), &json!({"body": "Hi"}));
        let close = set_issue_state(&gh(), 5, false, CloseReason::NotPlanned);
        assert_eq!(close.method, crate::devops::gh::Method::Patch);
        assert_eq!(close.endpoint, "repos/o/r/issues/5");
        assert_eq!(
            body(&close),
            &json!({"state": "closed", "state_reason": "not_planned"})
        );
        assert_eq!(
            body(&set_issue_state(&gh(), 5, false, CloseReason::Completed))["state_reason"],
            "completed"
        );
        assert_eq!(
            body(&set_issue_state(&gh(), 5, true, CloseReason::Completed)),
            &json!({"state": "open", "state_reason": "reopened"})
        );
        let e = edit_issue(&gh(), 5, &["bug".into()], &[]);
        assert_eq!(e.method, crate::devops::gh::Method::Patch);
        assert_eq!(body(&e), &json!({"labels": ["bug"], "assignees": []}));
        let n = create_issue(&gh(), " Crash ", "Steps", &["bug".into()], &["me".into()]);
        assert_eq!(n.endpoint, "repos/o/r/issues");
        assert_eq!(
            body(&n),
            &json!({"title": "Crash", "body": "Steps", "labels": ["bug"], "assignees": ["me"]})
        );
    }

    #[test]
    fn step_confirmation_text() {
        let s = Step::Rest(update_branch(&gh(), 7, "abc"));
        let text = s.confirmation();
        assert!(text.starts_with("PUT repos/o/r/pulls/7/update-branch\n"));
        assert!(text.contains("\"expected_head_sha\": \"abc\""));
        let g = Step::Gql(super::super::graphql::resolve_thread("T"));
        assert!(
            g.confirmation()
                .starts_with("POST graphql  (mutation resolveReviewThread)")
        );
        assert!(g.confirmation().contains("\"threadId\": \"T\""));
        assert_eq!(
            Step::Rest(delete_branch(&gh(), "x")).confirmation(),
            "DELETE repos/o/r/git/refs/heads/x"
        );
    }
}
