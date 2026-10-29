//! "Split this commit": undo a commit while keeping its changes in the
//! working tree, so they can be staged and committed in parts. HEAD is a
//! plain `git reset HEAD~`; an older commit of the current branch is first
//! reached with an interactive rebase that stops on it (`edit`, through the
//! workbench's `GIT_SEQUENCE_EDITOR` runner), then reset the same way; the
//! rebase banner's Continue replays the commits after it.

use std::path::Path;

use crate::devops::workbench::rebase::{self, Outcome, Plan, Step};

use super::{git, git_ok, git_probe, git_raw, resolve_in, short};

/// What will be split, and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitPlan {
    pub sha: String,
    pub subject: String,
    /// The full message (offered again for the first part).
    pub message: String,
    /// The commit is HEAD (no rebase needed).
    pub head: bool,
    pub head_sha: String,
    /// Commits after it on the branch (replayed by Continue).
    pub later: usize,
    /// On a remote-tracking branch already: rewriting it means a force
    /// push for everyone.
    pub pushed: bool,
    /// Uncommitted changes to tracked files (allowed for HEAD only; they
    /// mix with the split changes).
    pub dirty: bool,
    pub branch: Option<String>,
}

impl SplitPlan {
    /// The commands, as shown in the confirmation.
    pub fn commands(&self) -> Vec<String> {
        if self.head {
            return vec!["git reset HEAD~".to_owned()];
        }
        let todo = rebase::work_dir().join("git-rebase-todo");
        let mut v = rebase::command(&format!("{}~", short(&self.sha)), &todo);
        v.insert(
            0,
            format!(
                "# todo: \"edit {} {}\", every other commit \"pick\"",
                short(&self.sha),
                self.subject
            ),
        );
        v.push("git reset HEAD~".to_owned());
        v
    }

    /// What to do once it ran.
    pub fn next_steps(&self) -> Vec<String> {
        let mut v = vec![
            format!(
                "The changes of \"{}\" are back in the working tree, unstaged; its message is in the commit box.",
                self.subject
            ),
            "Stage the first part (files, hunks or lines) and commit it; repeat until nothing is left.".to_owned(),
        ];
        if !self.head {
            v.push(format!(
                "Then press Continue on the rebase banner to replay the {} after it. Abort on the banner puts everything back.",
                if self.later == 1 {
                    "commit".to_owned()
                } else {
                    format!("{} commits", self.later)
                }
            ));
        }
        v
    }
}

fn pending_operation(dir: &Path, cmds: &mut Vec<String>) -> Option<&'static str> {
    for (path, what) in [
        ("rebase-merge", "a rebase"),
        ("rebase-apply", "a rebase or git am"),
        ("MERGE_HEAD", "a merge"),
        ("CHERRY_PICK_HEAD", "a cherry-pick"),
        ("REVERT_HEAD", "a revert"),
        ("BISECT_LOG", "a bisect"),
    ] {
        if let Some(p) = git_probe(dir, &["rev-parse", "--git-path", path], cmds)
            && resolve_in(dir, &p).exists()
        {
            return Some(what);
        }
    }
    None
}

/// Check that `rev` can be split and describe how.
pub fn plan(dir: &Path, rev: &str, cmds: &mut Vec<String>) -> Result<SplitPlan, String> {
    let rev = if rev.trim().eq_ignore_ascii_case("head") {
        "HEAD"
    } else {
        rev.trim()
    };
    let sha = git_probe(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
        cmds,
    )
    .ok_or_else(|| format!("\"{rev}\" is not a commit of this repository."))?;
    let head_sha = git_probe(dir, &["rev-parse", "--verify", "--quiet", "HEAD"], cmds)
        .ok_or_else(|| "HEAD has no commit yet.".to_owned())?;
    if let Some(what) = pending_operation(dir, cmds) {
        return Err(format!(
            "The repository is in the middle of {what}: finish or abort it first."
        ));
    }
    let parents = git(dir, &["rev-list", "--parents", "-n", "1", &sha], cmds)?
        .split_whitespace()
        .count()
        .saturating_sub(1);
    if parents == 0 {
        return Err("This is the first commit of the history: it cannot be split here.".into());
    }
    if parents > 1 {
        return Err("This is a merge commit: only ordinary commits can be split.".into());
    }
    let head = sha == head_sha;
    if !head
        && !git_raw(
            dir,
            &["merge-base", "--is-ancestor", &sha, &head_sha],
            &[],
            cmds,
        )?
        .success
    {
        return Err(
            "This commit is not on the current branch: switch to a branch that contains it first."
                .into(),
        );
    }
    let message = git(dir, &["log", "-1", "--format=%B", &sha], cmds)?
        .trim_end()
        .to_owned();
    let subject = message.lines().next().unwrap_or("").to_owned();
    let pushed = !git(dir, &["branch", "-r", "--contains", &sha], cmds)
        .unwrap_or_default()
        .trim()
        .is_empty();
    let dirty = !git(
        dir,
        &["status", "--porcelain", "--untracked-files=no"],
        cmds,
    )?
    .trim()
    .is_empty();
    let branch = git_probe(dir, &["symbolic-ref", "--short", "-q", "HEAD"], cmds);
    let mut later = 0;
    if !head {
        if dirty {
            return Err("Commit or stash your changes first: splitting an older commit replays the commits after it.".into());
        }
        if branch.is_none() {
            return Err("HEAD is detached: switch to the branch first.".into());
        }
        let range = format!("{sha}..{head_sha}");
        let merges: usize = git(dir, &["rev-list", "--count", "--merges", &range], cmds)?
            .trim()
            .parse()
            .unwrap_or(0);
        if merges > 0 {
            return Err(format!(
                "There {} after this commit; the rebase would flatten {}. Split it from the interactive rebase tab instead.",
                if merges == 1 {
                    "is a merge".to_owned()
                } else {
                    format!("are {merges} merges")
                },
                if merges == 1 { "it" } else { "them" }
            ));
        }
        later = git(dir, &["rev-list", "--count", &range], cmds)?
            .trim()
            .parse()
            .unwrap_or(0);
    }
    Ok(SplitPlan {
        sha,
        subject,
        message,
        head,
        head_sha,
        later,
        pushed,
        dirty,
        branch,
    })
}

/// Carry out a plan made by [`plan`]. Returns the summary line.
pub fn execute(dir: &Path, p: &SplitPlan, cmds: &mut Vec<String>) -> Result<String, String> {
    let now = git_probe(dir, &["rev-parse", "HEAD"], cmds).unwrap_or_default();
    if now != p.head_sha {
        return Err("HEAD moved since the split was planned: try again.".into());
    }
    if !p.head {
        let loaded = rebase::load(dir, &format!("{}~", p.sha), cmds)?;
        let mut plan = Plan::new(loaded.commits);
        let row = plan
            .rows
            .iter()
            .position(|r| r.commit.sha == p.sha)
            .ok_or_else(|| "The commit is not in the range to rebase.".to_owned())?;
        plan.set_step(row, Step::Edit);
        let work = rebase::work_dir();
        match rebase::execute(dir, &loaded.base_sha, &loaded.head_sha, &plan, &work, cmds)? {
            Outcome::Stopped(_) => {}
            Outcome::Done(_) => {
                return Err("The rebase finished without stopping at the commit.".into());
            }
            Outcome::Conflict(t) => return Err(t),
        }
        let at = git_probe(dir, &["rev-parse", "HEAD"], cmds).unwrap_or_default();
        if at != p.sha {
            return Err(format!(
                "The rebase stopped at {} instead of {}: abort it from the banner.",
                short(&at),
                short(&p.sha)
            ));
        }
    }
    git_ok(dir, &["reset", "HEAD~"], cmds)?;
    Ok(if p.head {
        format!(
            "Split ready: the changes of \"{}\" are unstaged; commit them in parts",
            p.subject
        )
    } else {
        format!(
            "Split ready: the rebase stopped at \"{}\" with its changes unstaged; commit them in parts, then Continue",
            p.subject
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_of(head: bool, later: usize) -> SplitPlan {
        SplitPlan {
            sha: "0123456789abcdef0123456789abcdef01234567".into(),
            subject: "Do two things".into(),
            message: "Do two things\n\nBody".into(),
            head,
            head_sha: "f".repeat(40),
            later,
            pushed: false,
            dirty: false,
            branch: Some("main".into()),
        }
    }

    #[test]
    fn commands_and_steps() {
        let h = plan_of(true, 0);
        assert_eq!(h.commands(), vec!["git reset HEAD~"]);
        assert_eq!(h.next_steps().len(), 2);
        let o = plan_of(false, 3);
        let c = o.commands();
        assert!(c[0].contains("edit 0123456789 Do two things"));
        assert!(c[1].starts_with("GIT_SEQUENCE_EDITOR="));
        assert!(c[2].contains("git rebase -i --autostash --no-update-refs 0123456789~"));
        assert_eq!(c[3], "git reset HEAD~");
        assert!(o.next_steps()[2].contains("replay the 3 commits"));
        assert!(plan_of(false, 1).next_steps()[2].contains("replay the commit after"));
    }
}
