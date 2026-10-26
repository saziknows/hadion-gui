//! Failed workflow runs on the user's branches, picked from the runs every
//! open repository already loaded (no extra API calls).
//!
//! A run is "mine" when the user started it (actor or triggering actor) or
//! when it ran on the head branch of one of the user's open pull requests.

use crate::github::{PullRequest, WorkflowRun};

/// Why a run counts as the user's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// The user started it.
    Actor,
    /// It ran on the head branch of the user's pull request.
    PrBranch(u64),
}

/// A failed run on one of the user's branches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MyFailure<'a> {
    pub run: &'a WorkflowRun,
    pub why: Why,
    /// A newer run of the same workflow on the same branch did not fail (it
    /// passed, or is still running): the failure may be fixed already.
    pub superseded: bool,
}

/// The newest time we know for a run.
fn when(run: &WorkflowRun) -> Option<i64> {
    run.updated.or(run.started).or(run.created)
}

/// Failed runs on the user's branches that ended within `window_secs` of
/// `now`, newest first (the order of `runs`, which GitHub sends newest
/// first). `login` is compared case-insensitively.
pub fn failed_on_my_branches<'a>(
    runs: &'a [WorkflowRun],
    prs: &[PullRequest],
    login: &str,
    now: i64,
    window_secs: i64,
) -> Vec<MyFailure<'a>> {
    if login.is_empty() {
        return Vec::new();
    }
    let me = |s: &str| s.eq_ignore_ascii_case(login);
    let mut out = Vec::new();
    for (i, run) in runs.iter().enumerate() {
        if !run.is_failure() {
            continue;
        }
        let Some(t) = when(run) else { continue };
        if now - t > window_secs {
            continue;
        }
        let why = if me(&run.actor) || me(&run.triggering_actor) {
            Why::Actor
        } else if let Some(pr) = prs
            .iter()
            .find(|p| me(&p.author) && p.head == run.branch && p.head_repo.is_none())
        {
            Why::PrBranch(pr.number)
        } else {
            continue;
        };
        // Runs before index i are newer.
        let superseded = runs[..i].iter().any(|newer| {
            newer.workflow_id == run.workflow_id
                && newer.branch == run.branch
                && !newer.is_failure()
                && newer.outcome() != crate::github::Outcome::Cancelled
        });
        out.push(MyFailure {
            run,
            why,
            superseded,
        });
    }
    out
}

/// Open pull requests the user authored.
pub fn my_prs<'a>(prs: &'a [PullRequest], login: &str) -> Vec<&'a PullRequest> {
    if login.is_empty() {
        return Vec::new();
    }
    prs.iter()
        .filter(|p| p.author.eq_ignore_ascii_case(login))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(
        id: u64,
        workflow: u64,
        branch: &str,
        actor: &str,
        conclusion: Option<&str>,
        t: i64,
    ) -> WorkflowRun {
        WorkflowRun {
            id,
            name: format!("wf{workflow}"),
            status: if conclusion.is_some() {
                "completed"
            } else {
                "in_progress"
            }
            .into(),
            conclusion: conclusion.map(str::to_owned),
            branch: branch.into(),
            sha: format!("{id:040}"),
            url: String::new(),
            event: "push".into(),
            created: Some(t - 60),
            started: Some(t - 50),
            updated: Some(t),
            run_number: id,
            attempt: 1,
            title: String::new(),
            actor: actor.into(),
            triggering_actor: actor.into(),
            path: String::new(),
            workflow_id: workflow,
            head_message: String::new(),
            head_author: String::new(),
        }
    }

    fn pr(number: u64, author: &str, head: &str) -> PullRequest {
        PullRequest {
            number,
            title: String::new(),
            author: author.into(),
            head: head.into(),
            head_sha: String::new(),
            head_repo: None,
            base: "main".into(),
            draft: false,
            url: String::new(),
            created: None,
            updated: None,
            labels: vec![],
            assignees: vec![],
            reviewers: vec![],
            milestone: None,
            body: String::new(),
        }
    }

    #[test]
    fn picks_my_recent_failures() {
        let now = 1_800_000_000;
        let day = 86_400;
        // Newest first, as GitHub sends them.
        let runs = vec![
            run(9, 1, "feature", "octo", Some("success"), now - 100), // fixes run 6
            run(8, 2, "main", "dana", Some("failure"), now - 200),    // not mine
            run(
                7,
                2,
                "review-me",
                "dependabot[bot]",
                Some("failure"),
                now - 300,
            ), // my PR's branch
            run(6, 1, "feature", "Octo", Some("failure"), now - 400), // mine, superseded
            run(5, 3, "feature", "octo", Some("cancelled"), now - 500), // not a failure
            run(4, 3, "other", "octo", None, now - 600),              // running
            run(3, 1, "old", "octo", Some("failure"), now - 2 * day), // too old
            run(2, 4, "feature", "octo", Some("timed_out"), now - 700), // mine, still failing
        ];
        let prs = vec![pr(12, "OCTO", "review-me"), pr(13, "dana", "main")];
        let got = failed_on_my_branches(&runs, &prs, "octo", now, day);
        let ids: Vec<(u64, Why, bool)> = got
            .iter()
            .map(|f| (f.run.id, f.why, f.superseded))
            .collect();
        assert_eq!(
            ids,
            vec![
                (7, Why::PrBranch(12), false),
                (6, Why::Actor, true),
                (2, Why::Actor, false),
            ]
        );
        assert!(failed_on_my_branches(&runs, &prs, "", now, day).is_empty());
        assert_eq!(my_prs(&prs, "octo").len(), 1);
    }
}
