//! GitHub notifications: the list (`GET notifications`), why each one
//! arrived (its reason), where it leads, filters, and the mark-as-read
//! requests.
//!
//! Marking a thread as read cannot be undone through the API, so the
//! requests are only built here; the app sends them on an explicit click.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::json;

use crate::Result;
use crate::devops::gh::{Method, Request, decode};
use crate::github::parse_time;

/// Why GitHub sent a notification (the `reason` field).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Reason {
    ReviewRequested,
    Mention,
    TeamMention,
    Assign,
    Author,
    Comment,
    CiActivity,
    StateChange,
    SecurityAlert,
    ApprovalRequested,
    Invitation,
    Manual,
    Subscribed,
    Other,
}

impl Reason {
    pub const ALL: [Reason; 14] = [
        Reason::ReviewRequested,
        Reason::Mention,
        Reason::TeamMention,
        Reason::Assign,
        Reason::Author,
        Reason::Comment,
        Reason::CiActivity,
        Reason::StateChange,
        Reason::SecurityAlert,
        Reason::ApprovalRequested,
        Reason::Invitation,
        Reason::Manual,
        Reason::Subscribed,
        Reason::Other,
    ];

    pub fn from_api(s: &str) -> Self {
        match s {
            "review_requested" => Reason::ReviewRequested,
            "mention" => Reason::Mention,
            "team_mention" => Reason::TeamMention,
            "assign" => Reason::Assign,
            "author" => Reason::Author,
            "comment" => Reason::Comment,
            "ci_activity" => Reason::CiActivity,
            "state_change" => Reason::StateChange,
            "security_alert" | "security_advisory_credit" => Reason::SecurityAlert,
            "approval_requested" => Reason::ApprovalRequested,
            "invitation" | "member_feature_requested" => Reason::Invitation,
            "manual" => Reason::Manual,
            "subscribed" => Reason::Subscribed,
            _ => Reason::Other,
        }
    }

    /// The API name (`review_requested`).
    pub fn api(self) -> &'static str {
        match self {
            Reason::ReviewRequested => "review_requested",
            Reason::Mention => "mention",
            Reason::TeamMention => "team_mention",
            Reason::Assign => "assign",
            Reason::Author => "author",
            Reason::Comment => "comment",
            Reason::CiActivity => "ci_activity",
            Reason::StateChange => "state_change",
            Reason::SecurityAlert => "security_alert",
            Reason::ApprovalRequested => "approval_requested",
            Reason::Invitation => "invitation",
            Reason::Manual => "manual",
            Reason::Subscribed => "subscribed",
            Reason::Other => "other",
        }
    }

    /// Sentence-case label for chips and badges.
    pub fn label(self) -> &'static str {
        match self {
            Reason::ReviewRequested => "Review requested",
            Reason::Mention => "Mentioned",
            Reason::TeamMention => "Team mentioned",
            Reason::Assign => "Assigned",
            Reason::Author => "Author",
            Reason::Comment => "Commented",
            Reason::CiActivity => "CI activity",
            Reason::StateChange => "State change",
            Reason::SecurityAlert => "Security alert",
            Reason::ApprovalRequested => "Approval requested",
            Reason::Invitation => "Invitation",
            Reason::Manual => "Subscribed by you",
            Reason::Subscribed => "Watching",
            Reason::Other => "Other",
        }
    }

    /// One sentence on why it arrived.
    pub fn explain(self) -> &'static str {
        match self {
            Reason::ReviewRequested => "You, or a team you are on, were asked to review it.",
            Reason::Mention => "You were @mentioned in it.",
            Reason::TeamMention => "A team you belong to was @mentioned.",
            Reason::Assign => "You were assigned to it.",
            Reason::Author => "You opened it.",
            Reason::Comment => "You commented on it.",
            Reason::CiActivity => "A workflow run you triggered finished.",
            Reason::StateChange => "You changed its state (closed, merged, reopened).",
            Reason::SecurityAlert => "GitHub found a vulnerability in the repository.",
            Reason::ApprovalRequested => "A deployment is waiting for your approval.",
            Reason::Invitation => "You were invited to contribute.",
            Reason::Manual => "You subscribed to this thread yourself.",
            Reason::Subscribed => "You are watching the repository.",
            Reason::Other => "GitHub gave a reason Hadion does not know.",
        }
    }

    /// Names accepted by the `inbox-filter` script command: API names,
    /// labels and short forms (`review`, `mention`, `ci`).
    pub fn from_name(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase().replace([' ', '-'], "_");
        Some(match s.as_str() {
            "review" | "reviews" | "review_requested" => Reason::ReviewRequested,
            "mention" | "mentions" | "mentioned" => Reason::Mention,
            "team" | "team_mention" | "team_mentioned" => Reason::TeamMention,
            "assign" | "assigned" => Reason::Assign,
            "author" => Reason::Author,
            "comment" | "commented" => Reason::Comment,
            "ci" | "ci_activity" => Reason::CiActivity,
            "state" | "state_change" => Reason::StateChange,
            "security" | "security_alert" => Reason::SecurityAlert,
            "approval" | "approval_requested" => Reason::ApprovalRequested,
            "invitation" => Reason::Invitation,
            "manual" => Reason::Manual,
            "subscribed" | "watching" => Reason::Subscribed,
            "other" => Reason::Other,
            _ => return None,
        })
    }
}

/// What a notification is about (`subject.type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SubjectKind {
    PullRequest,
    Issue,
    Commit,
    Release,
    CheckSuite,
    Discussion,
    SecurityAlert,
    Other,
}

impl SubjectKind {
    pub fn from_api(s: &str) -> Self {
        match s {
            "PullRequest" => SubjectKind::PullRequest,
            "Issue" => SubjectKind::Issue,
            "Commit" => SubjectKind::Commit,
            "Release" => SubjectKind::Release,
            "CheckSuite" | "WorkflowRun" => SubjectKind::CheckSuite,
            "Discussion" => SubjectKind::Discussion,
            "RepositoryVulnerabilityAlert"
            | "RepositoryDependabotAlertsThread"
            | "SecretScanningAlert"
            | "RepositoryAdvisory" => SubjectKind::SecurityAlert,
            _ => SubjectKind::Other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SubjectKind::PullRequest => "Pull request",
            SubjectKind::Issue => "Issue",
            SubjectKind::Commit => "Commit",
            SubjectKind::Release => "Release",
            SubjectKind::CheckSuite => "Workflow run",
            SubjectKind::Discussion => "Discussion",
            SubjectKind::SecurityAlert => "Security alert",
            SubjectKind::Other => "Other",
        }
    }
}

/// One notification thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    /// Thread id (for `notifications/threads/{id}`).
    pub id: String,
    pub reason: Reason,
    /// The reason as the API sent it.
    pub reason_api: String,
    pub unread: bool,
    pub updated: Option<i64>,
    pub last_read: Option<i64>,
    /// `owner/repo`.
    pub repo: String,
    /// `https://github.com/owner/repo`.
    pub repo_url: String,
    pub private: bool,
    pub kind: SubjectKind,
    pub kind_api: String,
    pub title: String,
    /// The subject's API URL (`…/repos/o/r/pulls/12`), absent for some kinds.
    pub api_url: Option<String>,
    pub comment_url: Option<String>,
    /// Pull request or issue number.
    pub number: Option<u64>,
    /// Where the subject is on the web.
    pub web_url: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawRepo {
    full_name: String,
    html_url: String,
    private: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSubject {
    title: String,
    url: Option<String>,
    latest_comment_url: Option<String>,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawNotification {
    id: String,
    reason: String,
    unread: bool,
    updated_at: String,
    last_read_at: Option<String>,
    repository: RawRepo,
    subject: RawSubject,
}

/// The unread notifications, newest first (`all=false`).
pub fn list_request(per_page: u32) -> Request {
    Request::get(format!(
        "notifications?all=false&per_page={}",
        per_page.clamp(1, 50)
    ))
}

/// Mark one thread as read. Not reversible through the API.
pub fn mark_read_request(thread_id: &str) -> Request {
    Request::new(
        Method::Patch,
        format!(
            "notifications/threads/{}",
            crate::devops::gh::encode(thread_id)
        ),
    )
}

/// Mark every notification last updated at or before `last_read_at` (ISO
/// 8601) as read. Not reversible through the API.
pub fn mark_all_read_request(last_read_at: &str) -> Request {
    Request::put(
        "notifications",
        json!({ "last_read_at": last_read_at, "read": true }),
    )
}

/// `12` from `https://api.github.com/repos/o/r/pulls/12`.
pub fn number_from_url(url: &str) -> Option<u64> {
    let mut parts = url.trim_end_matches('/').rsplit('/');
    let last = parts.next()?;
    let kind = parts.next()?;
    if matches!(kind, "pulls" | "issues" | "discussions") {
        last.parse().ok()
    } else {
        None
    }
}

/// Where a subject lives on the web, from the repository page and the
/// subject's API URL.
pub fn web_url(repo_url: &str, kind: SubjectKind, api_url: Option<&str>) -> String {
    let base = repo_url.trim_end_matches('/');
    let tail = api_url.and_then(|u| {
        let (_, rest) = u.split_once("/repos/")?;
        // owner/repo/<kind>/<id>
        let mut it = rest.splitn(4, '/');
        let (_, _, k, id) = (it.next()?, it.next()?, it.next()?, it.next()?);
        Some((k.to_owned(), id.trim_end_matches('/').to_owned()))
    });
    match (kind, tail) {
        (SubjectKind::PullRequest, Some((_, id))) => format!("{base}/pull/{id}"),
        (SubjectKind::Issue, Some((_, id))) => format!("{base}/issues/{id}"),
        (SubjectKind::Discussion, Some((_, id))) => format!("{base}/discussions/{id}"),
        (SubjectKind::Discussion, None) => format!("{base}/discussions"),
        (SubjectKind::Commit, Some((_, id))) => format!("{base}/commit/{id}"),
        (SubjectKind::Release, _) => format!("{base}/releases"),
        (SubjectKind::CheckSuite, _) => format!("{base}/actions"),
        (SubjectKind::SecurityAlert, _) => format!("{base}/security"),
        _ => base.to_owned(),
    }
}

fn convert(r: RawNotification) -> Notification {
    let kind = SubjectKind::from_api(&r.subject.kind);
    let api_url = r.subject.url.filter(|u| !u.is_empty());
    let repo_url = if r.repository.html_url.is_empty() {
        format!("https://github.com/{}", r.repository.full_name)
    } else {
        r.repository.html_url.clone()
    };
    Notification {
        reason: Reason::from_api(&r.reason),
        reason_api: r.reason,
        unread: r.unread,
        updated: parse_time(&r.updated_at),
        last_read: r.last_read_at.as_deref().and_then(parse_time),
        number: api_url.as_deref().and_then(number_from_url),
        web_url: web_url(&repo_url, kind, api_url.as_deref()),
        repo: r.repository.full_name,
        repo_url,
        private: r.repository.private,
        kind,
        kind_api: r.subject.kind,
        title: r.subject.title,
        comment_url: r.subject.latest_comment_url,
        api_url,
        id: r.id,
    }
}

/// Parse a `GET notifications` answer, newest first.
pub fn parse(body: &[u8]) -> Result<Vec<Notification>> {
    let raw: Vec<RawNotification> = decode(body, "notifications")?;
    let mut out: Vec<Notification> = raw.into_iter().map(convert).collect();
    out.sort_by_key(|n| std::cmp::Reverse(n.updated));
    Ok(out)
}

/// Which notifications a list shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    /// Empty: every reason.
    pub reasons: Vec<Reason>,
    pub unread_only: bool,
    /// Only repositories open in Hadion (lower-case `owner/repo` names).
    pub only_open: bool,
    /// Case-insensitive text in the title or repository.
    pub text: String,
}

impl Filter {
    pub fn accepts(&self, n: &Notification, open: &HashSet<String>) -> bool {
        if !self.reasons.is_empty() && !self.reasons.contains(&n.reason) {
            return false;
        }
        if self.unread_only && !n.unread {
            return false;
        }
        if self.only_open && !open.contains(&n.repo.to_ascii_lowercase()) {
            return false;
        }
        let t = self.text.trim().to_lowercase();
        t.is_empty() || n.title.to_lowercase().contains(&t) || n.repo.to_lowercase().contains(&t)
    }
}

/// The notifications a filter keeps, in order.
pub fn filtered<'a>(
    items: &'a [Notification],
    f: &Filter,
    open: &HashSet<String>,
) -> Vec<&'a Notification> {
    items.iter().filter(|n| f.accepts(n, open)).collect()
}

/// How many notifications have each reason (reasons with none left out),
/// in [`Reason::ALL`] order.
pub fn reason_counts(items: &[Notification]) -> Vec<(Reason, usize)> {
    Reason::ALL
        .iter()
        .map(|r| (*r, items.iter().filter(|n| n.reason == *r).count()))
        .filter(|(_, n)| *n > 0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const SAMPLE: &str = r#"[
      {
        "id": "1001",
        "unread": true,
        "reason": "review_requested",
        "updated_at": "2026-10-26T10:00:00Z",
        "last_read_at": null,
        "subject": {
          "title": "Add retry to the uploader",
          "url": "https://api.github.com/repos/acme/web/pulls/42",
          "latest_comment_url": "https://api.github.com/repos/acme/web/pulls/42",
          "type": "PullRequest"
        },
        "repository": {"id": 1, "full_name": "acme/web", "private": true,
                       "html_url": "https://github.com/acme/web"},
        "url": "https://api.github.com/notifications/threads/1001"
      },
      {
        "id": "1002",
        "unread": false,
        "reason": "mention",
        "updated_at": "2026-10-26T12:30:00Z",
        "last_read_at": "2026-10-26T12:31:00Z",
        "subject": {
          "title": "Crash on start",
          "url": "https://api.github.com/repos/acme/api/issues/7",
          "latest_comment_url": null,
          "type": "Issue"
        },
        "repository": {"full_name": "acme/api", "private": false,
                       "html_url": "https://github.com/acme/api"}
      },
      {
        "id": "1003",
        "unread": true,
        "reason": "ci_activity",
        "updated_at": "2026-10-25T08:00:00Z",
        "subject": {"title": "CI workflow run failed for main branch", "url": null,
                    "type": "CheckSuite"},
        "repository": {"full_name": "acme/web", "html_url": "https://github.com/acme/web"}
      },
      {
        "id": "1004",
        "unread": true,
        "reason": "brand_new_reason",
        "updated_at": "2026-10-24T08:00:00Z",
        "subject": {"title": "v2.0.0", "url": "https://api.github.com/repos/o/r/releases/99",
                    "type": "Release"},
        "repository": {"full_name": "o/r"}
      }
    ]"#;

    #[test]
    fn parses_and_sorts_newest_first() {
        let items = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].id, "1002", "newest first");
        let pr = items.iter().find(|n| n.id == "1001").unwrap();
        assert_eq!(pr.reason, Reason::ReviewRequested);
        assert_eq!(pr.kind, SubjectKind::PullRequest);
        assert_eq!(pr.number, Some(42));
        assert_eq!(pr.web_url, "https://github.com/acme/web/pull/42");
        assert!(pr.unread && pr.private);
        assert_eq!(pr.updated, parse_time("2026-10-26T10:00:00Z"));
        let issue = &items[0];
        assert_eq!(issue.web_url, "https://github.com/acme/api/issues/7");
        assert_eq!(issue.last_read, parse_time("2026-10-26T12:31:00Z"));
        let ci = items.iter().find(|n| n.id == "1003").unwrap();
        assert_eq!(ci.kind, SubjectKind::CheckSuite);
        assert_eq!(ci.number, None);
        assert_eq!(ci.web_url, "https://github.com/acme/web/actions");
        let other = items.iter().find(|n| n.id == "1004").unwrap();
        assert_eq!(other.reason, Reason::Other);
        assert_eq!(other.reason_api, "brand_new_reason");
        assert_eq!(other.repo_url, "https://github.com/o/r");
        assert_eq!(other.web_url, "https://github.com/o/r/releases");
    }

    #[test]
    fn request_builders() {
        let l = list_request(50);
        assert_eq!(l.method, Method::Get);
        assert_eq!(l.endpoint, "notifications?all=false&per_page=50");
        assert_eq!(
            list_request(500).endpoint,
            "notifications?all=false&per_page=50"
        );

        let one = mark_read_request("1001");
        assert_eq!(one.method, Method::Patch);
        assert_eq!(one.endpoint, "notifications/threads/1001");
        assert!(one.body.is_none());
        assert_eq!(
            one.display(),
            "gh api --method PATCH notifications/threads/1001"
        );

        let all = mark_all_read_request("2026-10-26T12:00:00Z");
        assert_eq!(all.method, Method::Put);
        assert_eq!(all.endpoint, "notifications");
        assert_eq!(
            all.body,
            Some(json!({"last_read_at": "2026-10-26T12:00:00Z", "read": true}))
        );
        assert!(!all.withheld);
        assert!(all.body_preview().unwrap().contains("last_read_at"));
    }

    #[test]
    fn filters_and_counts() {
        let items = parse(SAMPLE.as_bytes()).unwrap();
        let open: HashSet<String> = ["acme/web".to_owned()].into_iter().collect();
        let mut f = Filter::default();
        assert_eq!(filtered(&items, &f, &open).len(), 4);
        f.only_open = true;
        let ids: Vec<&str> = filtered(&items, &f, &open)
            .iter()
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(ids, vec!["1001", "1003"]);
        f.only_open = false;
        f.reasons = vec![Reason::Mention, Reason::ReviewRequested];
        assert_eq!(filtered(&items, &f, &open).len(), 2);
        f.unread_only = true;
        assert_eq!(filtered(&items, &f, &open).len(), 1);
        f = Filter {
            text: "CRASH".into(),
            ..Filter::default()
        };
        assert_eq!(filtered(&items, &f, &open)[0].id, "1002");
        let counts = reason_counts(&items);
        assert_eq!(
            counts,
            vec![
                (Reason::ReviewRequested, 1),
                (Reason::Mention, 1),
                (Reason::CiActivity, 1),
                (Reason::Other, 1)
            ]
        );
    }

    #[test]
    fn reason_names() {
        for r in Reason::ALL {
            assert_eq!(Reason::from_api(r.api()), r, "{r:?}");
            assert_eq!(Reason::from_name(r.api()), Some(r));
        }
        assert_eq!(
            Reason::from_name("Review requested"),
            Some(Reason::ReviewRequested)
        );
        assert_eq!(Reason::from_name("ci"), Some(Reason::CiActivity));
        assert_eq!(Reason::from_name("nope"), None);
    }

    #[test]
    fn urls() {
        assert_eq!(
            number_from_url("https://api.github.com/repos/o/r/pulls/12"),
            Some(12)
        );
        assert_eq!(
            number_from_url("https://api.github.com/repos/o/r/issues/3/"),
            Some(3)
        );
        assert_eq!(
            number_from_url("https://api.github.com/repos/o/r/commits/abc"),
            None
        );
        assert_eq!(
            web_url(
                "https://github.com/o/r",
                SubjectKind::Commit,
                Some("https://api.github.com/repos/o/r/commits/abc123")
            ),
            "https://github.com/o/r/commit/abc123"
        );
        assert_eq!(
            web_url(
                "https://ghe.example.com/o/r",
                SubjectKind::PullRequest,
                Some("https://ghe.example.com/api/v3/repos/o/r/pulls/5")
            ),
            "https://ghe.example.com/o/r/pull/5"
        );
        assert_eq!(
            web_url("https://github.com/o/r", SubjectKind::SecurityAlert, None),
            "https://github.com/o/r/security"
        );
    }
}
