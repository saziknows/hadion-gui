//! The review loop on GitHub: pull requests (list with filters, detail,
//! conversation, files and patches, review threads with resolution state,
//! create with template / reviewers / labels / draft, merge with
//! merge / squash / rebase, auto-merge, ready for review, close and reopen,
//! update branch), reviews (pending comments on lines and ranges,
//! suggestions, replies, approve / request changes / comment, resolve and
//! unresolve threads) and issues (list, detail with comments, create,
//! comment, close and reopen, labels and assignees, a branch name for an
//! issue, commits that reference it).
//!
//! - [`model`]: what is read (tolerant of GitHub's `null`s).
//! - [`graphql`]: the list and detail queries (one query per list), and the
//!   mutations REST lacks (auto-merge, draft state, thread resolution).
//! - [`rest`]: every REST write as a pure [`super::gh::Request`] builder,
//!   and [`rest::Step`] / [`rest::run_steps`] to run a change.
//! - [`fetch`]: the reads, through `gh api`.
//! - [`patch`]: GitHub's bare per-file hunks with old and new line numbers,
//!   and where review comments anchor.
//! - [`local`]: templates, commits between branches, generated titles and
//!   bodies, issue branch names, `#N` references, plain-text bodies.

pub mod fetch;
pub mod graphql;
pub mod local;
pub mod model;
pub mod patch;
pub mod rest;

pub use graphql::ListKind;
pub use model::*;
pub use rest::{DraftComment, ReviewEvent, Step};

/// Why a pull request can or cannot be merged, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeStatus {
    pub level: MergeLevel,
    pub title: &'static str,
    pub detail: String,
}

/// How a merge box looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeLevel {
    /// Can be merged now.
    Ready,
    /// Can be merged, with something to know (failing optional checks).
    Caution,
    /// Cannot be merged until something changes.
    Blocked,
    /// Already merged or closed.
    Done,
    /// GitHub has not computed it yet.
    Unknown,
}

/// Explain `mergeable` + `mergeStateStatus` (with the review decision and
/// the checks, which say *why* a pull request is blocked).
pub fn merge_status(pr: &PrSummary) -> MergeStatus {
    use MergeLevel::*;
    match pr.state {
        PrState::Merged => {
            return MergeStatus {
                level: Done,
                title: "Merged",
                detail: format!("{} was merged into {}.", pr.head, pr.base),
            };
        }
        PrState::Closed => {
            return MergeStatus {
                level: Done,
                title: "Closed without merging",
                detail: "Reopen it to continue the review or to merge it.".to_owned(),
            };
        }
        PrState::Open => {}
    }
    if pr.mergeable == Mergeable::Conflicting || pr.merge_state == MergeState::Dirty {
        return MergeStatus {
            level: Blocked,
            title: "Conflicts with the base branch",
            detail: format!(
                "{} and {} changed the same lines. Resolve the conflicts (merge {} into the branch locally, or use GitHub's editor), then push.",
                pr.head, pr.base, pr.base
            ),
        };
    }
    if pr.draft || pr.merge_state == MergeState::Draft {
        return MergeStatus {
            level: Blocked,
            title: "Draft",
            detail: "Marked as a draft: it cannot be merged until it is ready for review."
                .to_owned(),
        };
    }
    match pr.merge_state {
        MergeState::Clean => MergeStatus {
            level: Ready,
            title: "Ready to merge",
            detail: "No conflicts with the base branch, and every required check and review is in place."
                .to_owned(),
        },
        MergeState::HasHooks => MergeStatus {
            level: Ready,
            title: "Ready to merge",
            detail: "Checks pass; the repository runs pre-receive hooks on merge.".to_owned(),
        },
        MergeState::Unstable => MergeStatus {
            level: Caution,
            title: "Mergeable, but checks are failing",
            detail: "No conflicts and the required checks pass, but other checks failed. Look at them before merging."
                .to_owned(),
        },
        MergeState::Behind => MergeStatus {
            level: Blocked,
            title: "Behind the base branch",
            detail: format!(
                "{} has commits that {} does not, and branch protection requires it to be up to date. Update the branch first.",
                pr.base, pr.head
            ),
        },
        MergeState::Blocked => {
            let mut why: Vec<&str> = Vec::new();
            match pr.review_decision {
                Some(ReviewDecision::ChangesRequested) => why.push("a reviewer requested changes"),
                Some(ReviewDecision::ReviewRequired) => why.push("an approving review is required"),
                _ => {}
            }
            match pr.checks {
                Some(c) if c.is_bad() => why.push("required checks are failing"),
                Some(c) if c.is_running() => why.push("required checks have not finished"),
                _ => {}
            }
            let detail = if why.is_empty() {
                "Branch protection or a ruleset blocks it (required reviews, checks, signed commits or a merge queue).".to_owned()
            } else {
                let mut s = why.join(" and ");
                let up = s[..1].to_uppercase();
                s.replace_range(..1, &up);
                format!("{s}.")
            };
            MergeStatus {
                level: Blocked,
                title: "Blocked",
                detail,
            }
        }
        // Handled above; listed so the match stays exhaustive.
        MergeState::Dirty | MergeState::Draft => MergeStatus {
            level: Blocked,
            title: "Blocked",
            detail: String::new(),
        },
        MergeState::Unknown => MergeStatus {
            level: Unknown,
            title: "Mergeability not known yet",
            detail: if pr.mergeable == Mergeable::Unknown {
                "GitHub computes it in the background after a push. Refresh in a moment.".to_owned()
            } else {
                "No conflicts; GitHub has not reported the merge state yet. Refresh in a moment."
                    .to_owned()
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr() -> PrSummary {
        PrSummary {
            number: 1,
            head: "feature".into(),
            base: "main".into(),
            mergeable: Mergeable::Mergeable,
            merge_state: MergeState::Clean,
            ..Default::default()
        }
    }

    #[test]
    fn merge_status_explains() {
        assert_eq!(merge_status(&pr()).level, MergeLevel::Ready);
        let mut p = pr();
        p.merge_state = MergeState::Dirty;
        p.mergeable = Mergeable::Conflicting;
        assert_eq!(merge_status(&p).title, "Conflicts with the base branch");
        let mut p = pr();
        p.merge_state = MergeState::Blocked;
        p.review_decision = Some(ReviewDecision::ReviewRequired);
        p.checks = Some(CheckRollup::Failure);
        let s = merge_status(&p);
        assert_eq!(s.level, MergeLevel::Blocked);
        assert_eq!(
            s.detail,
            "An approving review is required and required checks are failing."
        );
        let mut p = pr();
        p.merge_state = MergeState::Blocked;
        assert!(merge_status(&p).detail.contains("Branch protection"));
        let mut p = pr();
        p.merge_state = MergeState::Behind;
        assert!(merge_status(&p).detail.contains("Update the branch"));
        let mut p = pr();
        p.merge_state = MergeState::Unstable;
        assert_eq!(merge_status(&p).level, MergeLevel::Caution);
        let mut p = pr();
        p.draft = true;
        p.merge_state = MergeState::Draft;
        assert_eq!(merge_status(&p).title, "Draft");
        let mut p = pr();
        p.state = PrState::Merged;
        assert_eq!(merge_status(&p).level, MergeLevel::Done);
        let mut p = pr();
        p.merge_state = MergeState::Unknown;
        p.mergeable = Mergeable::Unknown;
        assert_eq!(merge_status(&p).level, MergeLevel::Unknown);
    }
}
