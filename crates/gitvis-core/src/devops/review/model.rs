//! What the review loop works with: pull requests (list rows and detail),
//! reviews, review threads, changed files, commits, issues and comments,
//! plus the repository's merge settings.
//!
//! Everything is read from `serde_json::Value` with tolerant accessors:
//! GitHub returns `null` for many fields (a deleted author, a team the
//! token cannot see, a thread on a line that no longer exists), and a
//! missing or null field must never fail a whole list.

use serde_json::Value;

use crate::github::parse_time;

// ---------------------------------------------------------------------------
// Tolerant accessors
// ---------------------------------------------------------------------------

/// `v[k]` as a string, `""` when absent, null or not a string.
pub(crate) fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// `v[k]` as a non-empty string.
pub(crate) fn os(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|x| !x.is_empty())
        .map(str::to_owned)
}

pub(crate) fn u(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

pub(crate) fn ou32(v: &Value, k: &str) -> Option<u32> {
    v.get(k).and_then(Value::as_u64).map(|n| n as u32)
}

pub(crate) fn b(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}

pub(crate) fn ob(v: &Value, k: &str) -> Option<bool> {
    v.get(k).and_then(Value::as_bool)
}

/// An ISO timestamp field as epoch seconds.
pub(crate) fn t(v: &Value, k: &str) -> Option<i64> {
    v.get(k).and_then(Value::as_str).and_then(parse_time)
}

/// `v[k].login` (GraphQL actors and REST users), `""` for a deleted user.
pub(crate) fn login(v: &Value, k: &str) -> String {
    v.get(k).map(|a| s(a, "login")).unwrap_or_default()
}

/// `v[k].nodes` (GraphQL connections), empty when absent.
pub(crate) fn nodes<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k)
        .and_then(|c| c.get("nodes"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// `v[k].totalCount` (GraphQL connections).
pub(crate) fn total(v: &Value, k: &str) -> u64 {
    v.get(k).map(|c| u(c, "totalCount")).unwrap_or(0)
}

/// A JSON array field, empty when absent.
pub(crate) fn arr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

// ---------------------------------------------------------------------------
// Small vocabularies
// ---------------------------------------------------------------------------

/// A label with its colour (`d93f0b`, no `#`).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Label {
    pub name: String,
    pub color: String,
    pub description: String,
}

impl Label {
    pub fn from_json(v: &Value) -> Self {
        Self {
            name: s(v, "name"),
            color: s(v, "color").trim_start_matches('#').to_owned(),
            description: s(v, "description"),
        }
    }

    /// The colour as RGB, when it is a valid six-digit hex value.
    pub fn rgb(&self) -> Option<(u8, u8, u8)> {
        let c = self.color.trim_start_matches('#');
        if c.len() != 6 || !c.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&c[i..i + 2], 16).ok();
        Some((byte(0)?, byte(2)?, byte(4)?))
    }
}

/// Who a review was requested from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reviewer {
    User(String),
    Team {
        slug: String,
        name: String,
    },
    /// GitHub answered `null`: a team or user the token cannot see.
    Hidden,
}

impl Reviewer {
    pub fn from_json(v: &Value) -> Self {
        let Some(r) = v.get("requestedReviewer").filter(|r| !r.is_null()) else {
            return Reviewer::Hidden;
        };
        if s(r, "__typename") == "Team" || r.get("slug").is_some() {
            return Reviewer::Team {
                slug: s(r, "slug"),
                name: s(r, "name"),
            };
        }
        match os(r, "login") {
            Some(l) => Reviewer::User(l),
            None => Reviewer::Hidden,
        }
    }

    /// `alice`, `team core`, `someone hidden from this token`.
    pub fn display(&self) -> String {
        match self {
            Reviewer::User(l) => l.clone(),
            Reviewer::Team { slug, name } => {
                format!("team {}", if name.is_empty() { slug } else { name })
            }
            Reviewer::Hidden => "a reviewer this token cannot see".to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PrState {
    #[default]
    Open,
    Closed,
    Merged,
}

impl PrState {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_uppercase().as_str() {
            "MERGED" => PrState::Merged,
            "CLOSED" => PrState::Closed,
            _ => PrState::Open,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PrState::Open => "Open",
            PrState::Closed => "Closed",
            PrState::Merged => "Merged",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

impl ReviewDecision {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "APPROVED" => Some(ReviewDecision::Approved),
            "CHANGES_REQUESTED" => Some(ReviewDecision::ChangesRequested),
            "REVIEW_REQUIRED" => Some(ReviewDecision::ReviewRequired),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ReviewDecision::Approved => "Approved",
            ReviewDecision::ChangesRequested => "Changes requested",
            ReviewDecision::ReviewRequired => "Review required",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mergeable {
    Mergeable,
    Conflicting,
    /// Not computed yet (GitHub computes it lazily), or not fetched.
    #[default]
    Unknown,
}

impl Mergeable {
    pub fn parse(s: &str) -> Self {
        match s {
            "MERGEABLE" => Mergeable::Mergeable,
            "CONFLICTING" => Mergeable::Conflicting,
            _ => Mergeable::Unknown,
        }
    }
}

/// GitHub's `mergeStateStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MergeState {
    /// The head is out of date with the base.
    Behind,
    /// Blocked by required reviews, checks or rules.
    Blocked,
    /// Mergeable, checks passing.
    Clean,
    /// Conflicts.
    Dirty,
    Draft,
    /// Mergeable with passing checks and pre-receive hooks.
    HasHooks,
    /// Mergeable, but non-required checks are failing.
    Unstable,
    #[default]
    Unknown,
}

impl MergeState {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_uppercase().as_str() {
            "BEHIND" => MergeState::Behind,
            "BLOCKED" => MergeState::Blocked,
            "CLEAN" => MergeState::Clean,
            "DIRTY" => MergeState::Dirty,
            "DRAFT" => MergeState::Draft,
            "HAS_HOOKS" => MergeState::HasHooks,
            "UNSTABLE" => MergeState::Unstable,
            _ => MergeState::Unknown,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            MergeState::Behind => "behind",
            MergeState::Blocked => "blocked",
            MergeState::Clean => "clean",
            MergeState::Dirty => "dirty",
            MergeState::Draft => "draft",
            MergeState::HasHooks => "has hooks",
            MergeState::Unstable => "unstable",
            MergeState::Unknown => "unknown",
        }
    }
}

/// `statusCheckRollup.state` of the head commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckRollup {
    Success,
    Failure,
    Error,
    Pending,
    Expected,
}

impl CheckRollup {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "SUCCESS" => Some(CheckRollup::Success),
            "FAILURE" => Some(CheckRollup::Failure),
            "ERROR" => Some(CheckRollup::Error),
            "PENDING" => Some(CheckRollup::Pending),
            "EXPECTED" => Some(CheckRollup::Expected),
            _ => None,
        }
    }

    pub fn is_bad(self) -> bool {
        matches!(self, CheckRollup::Failure | CheckRollup::Error)
    }

    pub fn is_running(self) -> bool {
        matches!(self, CheckRollup::Pending | CheckRollup::Expected)
    }

    pub fn label(self) -> &'static str {
        match self {
            CheckRollup::Success => "Checks passed",
            CheckRollup::Failure => "Checks failing",
            CheckRollup::Error => "Checks errored",
            CheckRollup::Pending => "Checks running",
            CheckRollup::Expected => "Checks expected",
        }
    }
}

/// How a pull request is merged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    pub const ALL: [MergeMethod; 3] =
        [MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase];

    /// The REST `merge_method` value.
    pub fn rest(self) -> &'static str {
        match self {
            MergeMethod::Merge => "merge",
            MergeMethod::Squash => "squash",
            MergeMethod::Rebase => "rebase",
        }
    }

    /// The GraphQL `PullRequestMergeMethod` value.
    pub fn graphql(self) -> &'static str {
        match self {
            MergeMethod::Merge => "MERGE",
            MergeMethod::Squash => "SQUASH",
            MergeMethod::Rebase => "REBASE",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "MERGE" => Some(MergeMethod::Merge),
            "SQUASH" => Some(MergeMethod::Squash),
            "REBASE" => Some(MergeMethod::Rebase),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MergeMethod::Merge => "Create a merge commit",
            MergeMethod::Squash => "Squash and merge",
            MergeMethod::Rebase => "Rebase and merge",
        }
    }

    /// Whether GitHub takes a commit title and message for this method.
    pub fn has_message(self) -> bool {
        !matches!(self, MergeMethod::Rebase)
    }
}

/// Which side of a diff a comment sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    /// The base (removed lines, old numbers).
    Left,
    /// The head (added and context lines, new numbers).
    Right,
}

impl Side {
    pub fn parse(s: &str) -> Self {
        if s.eq_ignore_ascii_case("LEFT") {
            Side::Left
        } else {
            Side::Right
        }
    }

    pub fn api(self) -> &'static str {
        match self {
            Side::Left => "LEFT",
            Side::Right => "RIGHT",
        }
    }
}

// ---------------------------------------------------------------------------
// Repository settings
// ---------------------------------------------------------------------------

/// What the repository allows and what the viewer may do. REST
/// (`GET repos/o/r`) hides the `allow_*` fields from users without push
/// access; GraphQL shows them to everyone, so the two are merged with
/// [`RepoMeta::fill`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoMeta {
    pub default_branch: String,
    pub has_issues: Option<bool>,
    pub owner_is_org: Option<bool>,
    pub private: Option<bool>,
    pub allow_merge_commit: Option<bool>,
    pub allow_squash_merge: Option<bool>,
    pub allow_rebase_merge: Option<bool>,
    pub allow_auto_merge: Option<bool>,
    pub delete_branch_on_merge: Option<bool>,
    /// `PR_TITLE` or `COMMIT_OR_PR_TITLE`.
    pub squash_title: String,
    /// `PR_BODY`, `COMMIT_MESSAGES` or `BLANK`.
    pub squash_message: String,
    /// `PR_TITLE` or `MERGE_MESSAGE`.
    pub merge_title: String,
    /// `PR_BODY`, `PR_TITLE` or `BLANK`.
    pub merge_message: String,
    /// `ADMIN`, `MAINTAIN`, `WRITE`, `TRIAGE`, `READ` (upper case).
    pub permission: String,
    pub default_method: Option<MergeMethod>,
}

impl RepoMeta {
    /// From `GET repos/o/r`.
    pub fn from_rest(v: &Value) -> Self {
        let perms = v.get("permissions");
        let permission = perms.map(|p| {
            if b(p, "admin") {
                "ADMIN"
            } else if b(p, "maintain") {
                "MAINTAIN"
            } else if b(p, "push") {
                "WRITE"
            } else if b(p, "triage") {
                "TRIAGE"
            } else {
                "READ"
            }
        });
        Self {
            default_branch: s(v, "default_branch"),
            has_issues: ob(v, "has_issues"),
            owner_is_org: v.get("owner").map(|o| s(o, "type") == "Organization"),
            private: ob(v, "private"),
            allow_merge_commit: ob(v, "allow_merge_commit"),
            allow_squash_merge: ob(v, "allow_squash_merge"),
            allow_rebase_merge: ob(v, "allow_rebase_merge"),
            allow_auto_merge: ob(v, "allow_auto_merge"),
            delete_branch_on_merge: ob(v, "delete_branch_on_merge"),
            squash_title: s(v, "squash_merge_commit_title"),
            squash_message: s(v, "squash_merge_commit_message"),
            merge_title: s(v, "merge_commit_title"),
            merge_message: s(v, "merge_commit_message"),
            permission: permission.unwrap_or_default().to_owned(),
            default_method: None,
        }
    }

    /// From the GraphQL `repository` object of the list query.
    pub fn from_graphql(v: &Value) -> Self {
        Self {
            default_branch: v
                .get("defaultBranchRef")
                .map(|r| s(r, "name"))
                .unwrap_or_default(),
            has_issues: ob(v, "hasIssuesEnabled"),
            owner_is_org: v
                .get("owner")
                .filter(|o| !o.is_null())
                .map(|o| s(o, "__typename") == "Organization"),
            private: ob(v, "isPrivate"),
            allow_merge_commit: ob(v, "mergeCommitAllowed"),
            allow_squash_merge: ob(v, "squashMergeAllowed"),
            allow_rebase_merge: ob(v, "rebaseMergeAllowed"),
            allow_auto_merge: ob(v, "autoMergeAllowed"),
            delete_branch_on_merge: ob(v, "deleteBranchOnMerge"),
            squash_title: s(v, "squashMergeCommitTitle"),
            squash_message: s(v, "squashMergeCommitMessage"),
            merge_title: s(v, "mergeCommitTitle"),
            merge_message: s(v, "mergeCommitMessage"),
            permission: s(v, "viewerPermission"),
            default_method: v
                .get("viewerDefaultMergeMethod")
                .and_then(Value::as_str)
                .and_then(MergeMethod::parse),
        }
    }

    /// Fill what this one lacks from `other`.
    pub fn fill(&mut self, other: &RepoMeta) {
        fn str_fill(a: &mut String, b: &str) {
            if a.is_empty() {
                *a = b.to_owned();
            }
        }
        fn opt_fill<T: Clone>(a: &mut Option<T>, b: &Option<T>) {
            if a.is_none() {
                *a = b.clone();
            }
        }
        str_fill(&mut self.default_branch, &other.default_branch);
        opt_fill(&mut self.has_issues, &other.has_issues);
        opt_fill(&mut self.owner_is_org, &other.owner_is_org);
        opt_fill(&mut self.private, &other.private);
        opt_fill(&mut self.allow_merge_commit, &other.allow_merge_commit);
        opt_fill(&mut self.allow_squash_merge, &other.allow_squash_merge);
        opt_fill(&mut self.allow_rebase_merge, &other.allow_rebase_merge);
        opt_fill(&mut self.allow_auto_merge, &other.allow_auto_merge);
        opt_fill(
            &mut self.delete_branch_on_merge,
            &other.delete_branch_on_merge,
        );
        str_fill(&mut self.squash_title, &other.squash_title);
        str_fill(&mut self.squash_message, &other.squash_message);
        str_fill(&mut self.merge_title, &other.merge_title);
        str_fill(&mut self.merge_message, &other.merge_message);
        str_fill(&mut self.permission, &other.permission);
        opt_fill(&mut self.default_method, &other.default_method);
    }

    /// The merge methods the repository allows; all three when GitHub did
    /// not say.
    pub fn allowed_methods(&self) -> Vec<MergeMethod> {
        let flags = [
            (MergeMethod::Merge, self.allow_merge_commit),
            (MergeMethod::Squash, self.allow_squash_merge),
            (MergeMethod::Rebase, self.allow_rebase_merge),
        ];
        if flags.iter().all(|(_, f)| f.is_none()) {
            return MergeMethod::ALL.to_vec();
        }
        flags
            .iter()
            .filter(|(_, f)| *f == Some(true))
            .map(|(m, _)| *m)
            .collect()
    }

    /// The method to preselect: the viewer's last one when allowed, else
    /// the first allowed.
    pub fn preferred_method(&self) -> Option<MergeMethod> {
        let allowed = self.allowed_methods();
        self.default_method
            .filter(|m| allowed.contains(m))
            .or_else(|| allowed.first().copied())
    }

    /// Whether the viewer can push (merge, update branches, close).
    pub fn can_write(&self) -> bool {
        matches!(self.permission.as_str(), "ADMIN" | "MAINTAIN" | "WRITE")
    }
}

// ---------------------------------------------------------------------------
// Pull requests
// ---------------------------------------------------------------------------

/// One pull request as the list shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrSummary {
    /// GraphQL node id (mutations need it).
    pub id: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: PrState,
    pub draft: bool,
    pub author: String,
    pub created: Option<i64>,
    pub updated: Option<i64>,
    pub closed: Option<i64>,
    pub merged: Option<i64>,
    pub head: String,
    pub base: String,
    pub head_oid: String,
    /// `owner/repo` of the head repository (`None` when it was deleted).
    pub head_repo: Option<String>,
    pub cross_repo: bool,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub commits: u64,
    pub review_decision: Option<ReviewDecision>,
    pub mergeable: Mergeable,
    pub merge_state: MergeState,
    pub labels: Vec<Label>,
    pub checks: Option<CheckRollup>,
    pub review_requests: Vec<Reviewer>,
    pub assignees: Vec<String>,
    /// Conversation comments.
    pub comments: u64,
    pub reviews: u64,
    pub threads: u64,
    /// Every comment: conversation, reviews and review comments.
    pub total_comments: u64,
}

impl PrSummary {
    pub fn from_json(v: &Value) -> Self {
        let last_commit = nodes(v, "commits").last().and_then(|n| n.get("commit"));
        Self {
            id: s(v, "id"),
            number: u(v, "number"),
            title: s(v, "title"),
            url: s(v, "url"),
            state: PrState::parse(&s(v, "state")),
            draft: b(v, "isDraft"),
            author: login(v, "author"),
            created: t(v, "createdAt"),
            updated: t(v, "updatedAt"),
            closed: t(v, "closedAt"),
            merged: t(v, "mergedAt"),
            head: s(v, "headRefName"),
            base: s(v, "baseRefName"),
            head_oid: s(v, "headRefOid"),
            head_repo: v.get("headRepository").and_then(|r| os(r, "nameWithOwner")),
            cross_repo: b(v, "isCrossRepository"),
            additions: u(v, "additions"),
            deletions: u(v, "deletions"),
            changed_files: u(v, "changedFiles"),
            commits: total(v, "commits"),
            review_decision: v
                .get("reviewDecision")
                .and_then(Value::as_str)
                .and_then(ReviewDecision::parse),
            mergeable: Mergeable::parse(&s(v, "mergeable")),
            merge_state: MergeState::parse(&s(v, "mergeStateStatus")),
            labels: nodes(v, "labels").iter().map(Label::from_json).collect(),
            checks: last_commit
                .and_then(|c| c.get("statusCheckRollup"))
                .and_then(|r| r.get("state"))
                .and_then(Value::as_str)
                .and_then(CheckRollup::parse),
            review_requests: nodes(v, "reviewRequests")
                .iter()
                .map(Reviewer::from_json)
                .collect(),
            assignees: nodes(v, "assignees")
                .iter()
                .map(|a| s(a, "login"))
                .filter(|l| !l.is_empty())
                .collect(),
            comments: total(v, "comments"),
            reviews: total(v, "reviews"),
            threads: total(v, "reviewThreads"),
            total_comments: u(v, "totalCommentsCount"),
        }
    }

    /// The head branch lives in this repository (it can be deleted after
    /// merging, and updated without fork permissions).
    pub fn head_in_repo(&self, full_name: &str) -> bool {
        !self.cross_repo
            && self
                .head_repo
                .as_deref()
                .is_none_or(|r| r.eq_ignore_ascii_case(full_name))
    }

    pub fn is_open(&self) -> bool {
        self.state == PrState::Open
    }
}

/// A page of pull requests plus what the list query also answers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrList {
    pub items: Vec<PrSummary>,
    /// How many match the state filter on GitHub.
    pub total: u64,
    pub end_cursor: Option<String>,
    pub has_more: bool,
    /// The authenticated user.
    pub viewer: String,
    /// Open pull requests whose review is requested from the viewer
    /// (directly or through a team), without mergeability.
    pub requested: Vec<PrSummary>,
    pub requested_total: u64,
    /// The viewer's open pull requests, without mergeability.
    pub mine: Vec<PrSummary>,
    pub mine_total: u64,
    /// Repository settings from GraphQL.
    pub meta: RepoMeta,
    /// Fetched without mergeability (the full query timed out).
    pub light: bool,
    /// GraphQL rate-limit cost of the query, when reported.
    pub cost: Option<u64>,
}

/// An auto-merge request on a pull request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AutoMerge {
    pub enabled_at: Option<i64>,
    pub enabled_by: String,
    pub method: Option<MergeMethod>,
    pub headline: String,
    pub body: String,
}

/// The state a review left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    Pending,
}

impl ReviewState {
    pub fn parse(s: &str) -> Self {
        match s {
            "APPROVED" => ReviewState::Approved,
            "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
            "DISMISSED" => ReviewState::Dismissed,
            "PENDING" => ReviewState::Pending,
            _ => ReviewState::Commented,
        }
    }

    /// The verb in a timeline line: `alice approved`.
    pub fn verb(self) -> &'static str {
        match self {
            ReviewState::Approved => "approved",
            ReviewState::ChangesRequested => "requested changes",
            ReviewState::Commented => "reviewed",
            ReviewState::Dismissed => "reviewed (dismissed)",
            ReviewState::Pending => "has a pending review",
        }
    }
}

/// What a timeline entry is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimelineKind {
    Comment,
    Review(ReviewState),
    Merged,
    Closed,
    Reopened,
    ReadyForReview,
    ConvertedToDraft,
    ForcePushed,
    ReviewRequested,
    Labeled,
    Other(String),
}

/// One entry of a pull request's conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineItem {
    pub kind: TimelineKind,
    pub author: String,
    pub at: Option<i64>,
    pub body: String,
    pub url: String,
    pub database_id: u64,
    /// Review comments left with a review.
    pub review_comments: u64,
    /// Extra words for events: the label, the requested reviewer.
    pub detail: String,
}

impl TimelineItem {
    pub fn from_json(v: &Value) -> Self {
        let typename = s(v, "__typename");
        let kind = match typename.as_str() {
            "IssueComment" => TimelineKind::Comment,
            "PullRequestReview" => TimelineKind::Review(ReviewState::parse(&s(v, "state"))),
            "MergedEvent" => TimelineKind::Merged,
            "ClosedEvent" => TimelineKind::Closed,
            "ReopenedEvent" => TimelineKind::Reopened,
            "ReadyForReviewEvent" => TimelineKind::ReadyForReview,
            "ConvertToDraftEvent" => TimelineKind::ConvertedToDraft,
            "HeadRefForcePushedEvent" => TimelineKind::ForcePushed,
            "ReviewRequestedEvent" => TimelineKind::ReviewRequested,
            "LabeledEvent" => TimelineKind::Labeled,
            other => TimelineKind::Other(other.to_owned()),
        };
        let author = match v.get("author").filter(|a| !a.is_null()) {
            Some(a) => s(a, "login"),
            None => login(v, "actor"),
        };
        let detail = match &kind {
            TimelineKind::ReviewRequested => Reviewer::from_json(v).display(),
            TimelineKind::Labeled => v.get("label").map(|l| s(l, "name")).unwrap_or_default(),
            _ => String::new(),
        };
        Self {
            kind,
            author,
            at: t(v, "submittedAt")
                .or_else(|| t(v, "createdAt"))
                .or_else(|| t(v, "updatedAt")),
            body: s(v, "body"),
            url: s(v, "url"),
            database_id: u(v, "databaseId"),
            review_comments: total(v, "comments"),
            detail,
        }
    }

    /// A comment or review with words in it (events and empty reviews are
    /// shown as one line).
    pub fn has_body(&self) -> bool {
        !self.body.trim().is_empty()
    }
}

/// One comment of a review thread.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadComment {
    pub id: String,
    pub database_id: u64,
    pub author: String,
    pub body: String,
    pub created: Option<i64>,
    pub url: String,
}

impl ThreadComment {
    pub fn from_json(v: &Value) -> Self {
        Self {
            id: s(v, "id"),
            database_id: u(v, "databaseId"),
            author: login(v, "author"),
            body: s(v, "body"),
            created: t(v, "createdAt"),
            url: s(v, "url"),
        }
    }
}

/// A review thread on a line or range of a file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReviewThread {
    pub id: String,
    pub resolved: bool,
    pub outdated: bool,
    pub collapsed: bool,
    pub path: String,
    /// The line in the current diff (`None` when outdated).
    pub line: Option<u32>,
    pub original_line: Option<u32>,
    pub start_line: Option<u32>,
    pub original_start_line: Option<u32>,
    pub side: Option<Side>,
    pub start_side: Option<Side>,
    /// A comment on the whole file rather than a line.
    pub file_level: bool,
    pub resolved_by: Option<String>,
    pub comments: Vec<ThreadComment>,
    pub comments_total: u64,
}

impl ReviewThread {
    pub fn from_json(v: &Value) -> Self {
        Self {
            id: s(v, "id"),
            resolved: b(v, "isResolved"),
            outdated: b(v, "isOutdated"),
            collapsed: b(v, "isCollapsed"),
            path: s(v, "path"),
            line: ou32(v, "line"),
            original_line: ou32(v, "originalLine"),
            start_line: ou32(v, "startLine"),
            original_start_line: ou32(v, "originalStartLine"),
            side: os(v, "diffSide").map(|x| Side::parse(&x)),
            start_side: os(v, "startDiffSide").map(|x| Side::parse(&x)),
            file_level: s(v, "subjectType") == "FILE",
            resolved_by: v
                .get("resolvedBy")
                .filter(|r| !r.is_null())
                .and_then(|r| os(r, "login")),
            comments: nodes(v, "comments")
                .iter()
                .map(ThreadComment::from_json)
                .collect(),
            comments_total: total(v, "comments"),
        }
    }

    /// Where the thread sits in the current diff, if it still does.
    pub fn anchor(&self) -> Option<(Side, u32)> {
        if self.file_level {
            return None;
        }
        self.line.map(|l| (self.side.unwrap_or(Side::Right), l))
    }

    /// The comment replies go to (GitHub replies to the first comment).
    pub fn reply_to(&self) -> Option<u64> {
        self.comments
            .first()
            .map(|c| c.database_id)
            .filter(|id| *id > 0)
    }

    /// `L12`, `L10–L14`, `original L8`.
    pub fn line_label(&self) -> String {
        match (self.start_line, self.line) {
            (Some(a), Some(b)) if a != b => format!("lines {a}–{b}"),
            (_, Some(b)) => format!("line {b}"),
            _ => match (self.original_start_line, self.original_line) {
                (Some(a), Some(b)) if a != b => format!("originally lines {a}–{b}"),
                (_, Some(b)) => format!("originally line {b}"),
                _ => "the file".to_owned(),
            },
        }
    }
}

/// Everything the detail pane shows about one pull request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrDetail {
    pub pr: PrSummary,
    pub body: String,
    pub base_oid: String,
    pub locked: bool,
    pub maintainer_can_modify: bool,
    pub viewer_can_update: bool,
    pub viewer_did_author: bool,
    pub auto_merge: Option<AutoMerge>,
    pub merged_by: Option<String>,
    pub milestone: Option<String>,
    /// Oldest first.
    pub timeline: Vec<TimelineItem>,
    pub timeline_total: u64,
    pub threads: Vec<ReviewThread>,
    pub threads_total: u64,
}

impl PrDetail {
    pub fn from_json(v: &Value) -> Self {
        let auto_merge = v
            .get("autoMergeRequest")
            .filter(|a| !a.is_null())
            .map(|a| AutoMerge {
                enabled_at: t(a, "enabledAt"),
                enabled_by: login(a, "enabledBy"),
                method: a
                    .get("mergeMethod")
                    .and_then(Value::as_str)
                    .and_then(MergeMethod::parse),
                headline: s(a, "commitHeadline"),
                body: s(a, "commitBody"),
            });
        let merged_by = v
            .get("mergedBy")
            .filter(|m| !m.is_null())
            .and_then(|m| os(m, "login"));
        Self {
            pr: PrSummary::from_json(v),
            body: s(v, "body"),
            base_oid: s(v, "baseRefOid"),
            locked: b(v, "locked"),
            maintainer_can_modify: b(v, "maintainerCanModify"),
            viewer_can_update: b(v, "viewerCanUpdate"),
            viewer_did_author: b(v, "viewerDidAuthor"),
            auto_merge,
            merged_by,
            milestone: v
                .get("milestone")
                .filter(|m| !m.is_null())
                .and_then(|m| os(m, "title")),
            timeline: nodes(v, "timelineItems")
                .iter()
                .map(TimelineItem::from_json)
                .collect(),
            timeline_total: total(v, "timelineItems"),
            threads: nodes(v, "reviewThreads")
                .iter()
                .map(ReviewThread::from_json)
                .collect(),
            threads_total: total(v, "reviewThreads"),
        }
    }

    /// Threads on one file.
    pub fn threads_for<'a>(&'a self, path: &'a str) -> impl Iterator<Item = &'a ReviewThread> + 'a {
        self.threads.iter().filter(move |t| t.path == path)
    }

    pub fn unresolved_threads(&self) -> usize {
        self.threads.iter().filter(|t| !t.resolved).count()
    }
}

/// How a file changed in a pull request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FileStatus {
    Added,
    Removed,
    #[default]
    Modified,
    Renamed,
    Copied,
    Changed,
    Unchanged,
}

impl FileStatus {
    pub fn parse(s: &str) -> Self {
        match s {
            "added" => FileStatus::Added,
            "removed" => FileStatus::Removed,
            "renamed" => FileStatus::Renamed,
            "copied" => FileStatus::Copied,
            "changed" => FileStatus::Changed,
            "unchanged" => FileStatus::Unchanged,
            _ => FileStatus::Modified,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FileStatus::Added => "Added",
            FileStatus::Removed => "Deleted",
            FileStatus::Modified => "Modified",
            FileStatus::Renamed => "Renamed",
            FileStatus::Copied => "Copied",
            FileStatus::Changed => "Mode changed",
            FileStatus::Unchanged => "Unchanged",
        }
    }
}

/// A file changed by a pull request (`GET pulls/n/files`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrFile {
    /// Blob id of the new version (changes when the file changes).
    pub sha: String,
    pub path: String,
    pub previous: Option<String>,
    pub status: FileStatus,
    pub additions: u64,
    pub deletions: u64,
    /// GitHub's bare hunks; `None` for binary files and very large diffs.
    pub patch: Option<String>,
}

impl PrFile {
    pub fn from_json(v: &Value) -> Self {
        Self {
            sha: s(v, "sha"),
            path: s(v, "filename"),
            previous: os(v, "previous_filename"),
            status: FileStatus::parse(&s(v, "status")),
            additions: u(v, "additions"),
            deletions: u(v, "deletions"),
            patch: os(v, "patch"),
        }
    }
}

/// A commit of a pull request (`GET pulls/n/commits`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrCommit {
    pub sha: String,
    pub message: String,
    pub author_name: String,
    /// The GitHub account, when GitHub matched the e-mail address.
    pub author_login: Option<String>,
    pub date: Option<i64>,
    pub verified: bool,
    pub parents: usize,
}

impl PrCommit {
    pub fn from_json(v: &Value) -> Self {
        let c = v.get("commit").cloned().unwrap_or(Value::Null);
        let author = c.get("author").cloned().unwrap_or(Value::Null);
        Self {
            sha: s(v, "sha"),
            message: s(&c, "message"),
            author_name: s(&author, "name"),
            author_login: v
                .get("author")
                .filter(|a| !a.is_null())
                .and_then(|a| os(a, "login")),
            date: t(&author, "date"),
            verified: c
                .get("verification")
                .map(|x| b(x, "verified"))
                .unwrap_or(false),
            parents: arr(v, "parents").len(),
        }
    }

    pub fn subject(&self) -> &str {
        self.message.lines().next().unwrap_or("")
    }
}

// ---------------------------------------------------------------------------
// Issues
// ---------------------------------------------------------------------------

/// An issue (`GET issues`, `GET issues/n`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub open: bool,
    /// `completed`, `not_planned`, `reopened` or empty.
    pub state_reason: String,
    pub author: String,
    pub labels: Vec<Label>,
    pub assignees: Vec<String>,
    pub milestone: Option<String>,
    pub comments: u64,
    pub created: Option<i64>,
    pub updated: Option<i64>,
    pub closed: Option<i64>,
    pub closed_by: Option<String>,
    pub url: String,
    pub locked: bool,
    /// The REST issues list also returns pull requests.
    pub is_pr: bool,
}

impl Issue {
    pub fn from_json(v: &Value) -> Self {
        Self {
            number: u(v, "number"),
            title: s(v, "title"),
            body: s(v, "body"),
            open: s(v, "state") != "closed",
            state_reason: s(v, "state_reason"),
            author: login(v, "user"),
            labels: arr(v, "labels").iter().map(Label::from_json).collect(),
            assignees: arr(v, "assignees")
                .iter()
                .map(|a| s(a, "login"))
                .filter(|l| !l.is_empty())
                .collect(),
            milestone: v
                .get("milestone")
                .filter(|m| !m.is_null())
                .and_then(|m| os(m, "title")),
            comments: u(v, "comments"),
            created: t(v, "created_at"),
            updated: t(v, "updated_at"),
            closed: t(v, "closed_at"),
            closed_by: v
                .get("closed_by")
                .filter(|c| !c.is_null())
                .and_then(|c| os(c, "login")),
            url: s(v, "html_url"),
            locked: b(v, "locked"),
            is_pr: v.get("pull_request").is_some_and(|p| !p.is_null()),
        }
    }

    /// `Open`, `Closed as completed`, `Closed as not planned`.
    pub fn state_label(&self) -> &'static str {
        if self.open {
            "Open"
        } else if self.state_reason == "not_planned" {
            "Closed as not planned"
        } else {
            "Closed as completed"
        }
    }
}

/// A comment on an issue or on a pull request's conversation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IssueComment {
    pub id: u64,
    pub author: String,
    pub body: String,
    pub created: Option<i64>,
    pub updated: Option<i64>,
    pub url: String,
}

impl IssueComment {
    pub fn from_json(v: &Value) -> Self {
        Self {
            id: u(v, "id"),
            author: login(v, "user"),
            body: s(v, "body"),
            created: t(v, "created_at"),
            updated: t(v, "updated_at"),
            url: s(v, "html_url"),
        }
    }
}

/// Names offered while typing reviewers, assignees and labels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Suggestions {
    pub users: Vec<String>,
    /// `collaborators`, or `assignees` when collaborators were refused.
    pub users_source: String,
    pub labels: Vec<Label>,
    /// Team slugs (organisations only).
    pub teams: Vec<String>,
    /// Why teams are missing, if they are.
    pub teams_note: String,
}

impl Suggestions {
    pub fn label_names(&self) -> Vec<String> {
        self.labels.iter().map(|l| l.name.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pr_summary_from_graphql() {
        let v = json!({
            "id": "PR_1", "number": 40847, "title": "Update provider", "url": "https://github.com/o/r/pull/40847",
            "state": "OPEN", "isDraft": false, "createdAt": "2026-10-15T21:07:53Z", "updatedAt": "2026-10-26T17:28:16Z",
            "closedAt": null, "mergedAt": null, "author": {"login": "cmazakas"},
            "headRefName": "polkadot/x", "baseRefName": "master", "headRefOid": "29ca9d1",
            "isCrossRepository": false, "headRepository": {"nameWithOwner": "o/r"},
            "additions": 1052, "deletions": 13, "changedFiles": 19,
            "reviewDecision": "REVIEW_REQUIRED", "mergeable": "MERGEABLE", "mergeStateStatus": "BLOCKED",
            "labels": {"nodes": [{"name": "QA/No", "color": "4139e5"}]},
            "commits": {"totalCount": 6, "nodes": [{"commit": {"statusCheckRollup": {"state": "FAILURE"}}}]},
            "reviewRequests": {"nodes": [{"requestedReviewer": null}, {"requestedReviewer": {"__typename": "User", "login": "atuchin-m"}}, {"requestedReviewer": {"__typename": "Team", "slug": "core", "name": "Core"}}]},
            "assignees": {"nodes": [{"login": "cmazakas"}]},
            "comments": {"totalCount": 3}, "reviews": {"totalCount": 15}, "reviewThreads": {"totalCount": 18},
            "totalCommentsCount": 40
        });
        let pr = PrSummary::from_json(&v);
        assert_eq!(pr.number, 40847);
        assert_eq!(pr.state, PrState::Open);
        assert_eq!(pr.commits, 6);
        assert_eq!(pr.checks, Some(CheckRollup::Failure));
        assert!(pr.checks.unwrap().is_bad());
        assert_eq!(pr.merge_state, MergeState::Blocked);
        assert_eq!(pr.mergeable, Mergeable::Mergeable);
        assert_eq!(pr.review_decision, Some(ReviewDecision::ReviewRequired));
        assert_eq!(
            pr.review_requests,
            vec![
                Reviewer::Hidden,
                Reviewer::User("atuchin-m".into()),
                Reviewer::Team {
                    slug: "core".into(),
                    name: "Core".into()
                }
            ]
        );
        assert_eq!(pr.labels[0].rgb(), Some((0x41, 0x39, 0xe5)));
        assert_eq!(pr.threads, 18);
        assert!(pr.head_in_repo("o/r"));
        assert!(!pr.head_in_repo("x/y"));
        assert!(pr.created.is_some() && pr.closed.is_none());
    }

    #[test]
    fn null_fields_do_not_fail() {
        let pr = PrSummary::from_json(
            &json!({"number": 1, "author": null, "labels": null, "commits": null, "headRepository": null}),
        );
        assert_eq!(pr.author, "");
        assert!(pr.labels.is_empty() && pr.checks.is_none() && pr.head_repo.is_none());
        let t = ReviewThread::from_json(
            &json!({"line": null, "diffSide": "LEFT", "comments": {"nodes": [{"databaseId": 7, "author": null}]}}),
        );
        assert_eq!(t.anchor(), None);
        assert_eq!(t.reply_to(), Some(7));
        assert_eq!(t.line_label(), "the file");
    }

    #[test]
    fn repo_meta_merges_rest_and_graphql() {
        let mut rest = RepoMeta::from_rest(&json!({
            "default_branch": "master", "has_issues": false, "private": false,
            "owner": {"type": "Organization"},
            "permissions": {"admin": false, "maintain": false, "push": false, "triage": false, "pull": true}
        }));
        assert_eq!(rest.permission, "READ");
        assert!(!rest.can_write());
        assert_eq!(rest.allowed_methods(), MergeMethod::ALL.to_vec());
        let gql = RepoMeta::from_graphql(&json!({
            "mergeCommitAllowed": false, "squashMergeAllowed": true, "rebaseMergeAllowed": false,
            "autoMergeAllowed": true, "deleteBranchOnMerge": true, "viewerPermission": "READ",
            "viewerDefaultMergeMethod": "SQUASH", "squashMergeCommitTitle": "PR_TITLE",
            "defaultBranchRef": {"name": "master"}, "owner": {"__typename": "Organization"}
        }));
        rest.fill(&gql);
        assert_eq!(rest.allowed_methods(), vec![MergeMethod::Squash]);
        assert_eq!(rest.preferred_method(), Some(MergeMethod::Squash));
        assert_eq!(rest.delete_branch_on_merge, Some(true));
        assert_eq!(rest.has_issues, Some(false), "REST wins where it answered");
        assert_eq!(rest.squash_title, "PR_TITLE");
    }

    #[test]
    fn timeline_items() {
        let review = TimelineItem::from_json(&json!({
            "__typename": "PullRequestReview", "author": {"login": "bob"}, "state": "CHANGES_REQUESTED",
            "submittedAt": "2026-10-15T21:10:56Z", "body": "Please fix", "comments": {"totalCount": 4}, "databaseId": 9
        }));
        assert_eq!(
            review.kind,
            TimelineKind::Review(ReviewState::ChangesRequested)
        );
        assert_eq!(review.review_comments, 4);
        assert!(review.has_body());
        let label = TimelineItem::from_json(&json!({
            "__typename": "LabeledEvent", "actor": {"login": "bot"}, "createdAt": "2026-10-15T21:08:09Z",
            "label": {"name": "QA/No", "color": "4139e5"}
        }));
        assert_eq!(label.kind, TimelineKind::Labeled);
        assert_eq!(label.author, "bot");
        assert_eq!(label.detail, "QA/No");
    }

    #[test]
    fn rest_models() {
        let f = PrFile::from_json(
            &json!({"sha": "abc", "filename": "b.rs", "previous_filename": "a.rs", "status": "renamed", "additions": 2, "deletions": 1}),
        );
        assert_eq!(f.status, FileStatus::Renamed);
        assert_eq!(f.previous.as_deref(), Some("a.rs"));
        assert!(f.patch.is_none());
        let c = PrCommit::from_json(
            &json!({"sha": "27c8", "commit": {"message": "Fix it\n\nBody", "author": {"name": "Ann", "date": "2026-09-11T16:55:03Z"}, "verification": {"verified": true}}, "author": null, "parents": [{}]}),
        );
        assert_eq!(c.subject(), "Fix it");
        assert!(c.verified && c.author_login.is_none() && c.parents == 1);
        let i = Issue::from_json(
            &json!({"number": 5, "state": "closed", "state_reason": "not_planned", "user": {"login": "x"}, "labels": [{"name": "bug", "color": "ee0701"}], "pull_request": {"url": "u"}}),
        );
        assert!(!i.open && i.is_pr);
        assert_eq!(i.state_label(), "Closed as not planned");
    }
}
