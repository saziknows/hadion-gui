//! Repository mutations, each mapped onto one or more git commands.

use std::path::Path;

use crate::Result;
use crate::cli;
use crate::model::Pending;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MergeMode {
    /// Fast-forward when possible, otherwise create a merge commit.
    #[default]
    Default,
    NoFastForward,
    FastForwardOnly,
    Squash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PullMode {
    #[default]
    FastForwardOrMerge,
    FastForwardOnly,
    Rebase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetMode {
    Soft,
    Mixed,
    Hard,
}

/// What the UI should reload after an operation succeeds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refresh {
    Status,
    Full,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitOp {
    /// `gh pr checkout N` (GitHub CLI, not git).
    GhPrCheckout {
        number: u64,
    },
    /// `gh run rerun ID [--failed]`.
    GhRunRerun {
        id: u64,
        failed_only: bool,
    },
    /// `gh run cancel ID`.
    GhRunCancel {
        id: u64,
    },
    Switch {
        branch: String,
    },
    Detach {
        commit: String,
    },
    /// Create a local branch tracking a remote one and switch to it.
    SwitchTracking {
        local: String,
        remote_branch: String,
    },
    CreateBranch {
        name: String,
        start: String,
        switch: bool,
    },
    RenameBranch {
        from: String,
        to: String,
    },
    DeleteBranch {
        name: String,
        force: bool,
    },
    DeleteRemoteBranch {
        remote: String,
        branch: String,
    },
    SetUpstream {
        branch: String,
        upstream: String,
    },
    /// Fast-forward a branch that is not checked out (full ref names).
    FastForward {
        branch_ref: String,
        to_ref: String,
    },
    Merge {
        from: String,
        mode: MergeMode,
    },
    /// Rebase `branch` (HEAD when `None`) onto `onto`.
    Rebase {
        onto: String,
        branch: Option<String>,
    },
    CherryPick {
        commit: String,
    },
    Revert {
        commit: String,
    },
    Reset {
        target: String,
        mode: ResetMode,
    },
    CreateTag {
        name: String,
        target: String,
        message: Option<String>,
    },
    DeleteTag {
        name: String,
    },
    PushTag {
        remote: String,
        name: String,
    },
    Fetch,
    Pull {
        mode: PullMode,
    },
    Push {
        remote: String,
        branch: String,
        set_upstream: bool,
        force: bool,
    },
    Stage {
        paths: Vec<String>,
    },
    StageAll,
    Unstage {
        paths: Vec<String>,
        unborn: bool,
    },
    UnstageAll {
        unborn: bool,
    },
    Discard {
        paths: Vec<String>,
    },
    DeleteUntracked {
        paths: Vec<String>,
    },
    /// `git rm --cached`: stop tracking files but keep them on disk.
    Untrack {
        paths: Vec<String>,
    },
    /// `git remote set-url NAME URL` (used to strip embedded credentials).
    SetRemoteUrl {
        name: String,
        url: String,
    },
    ApplyPatch {
        patch: Vec<u8>,
        cached: bool,
        reverse: bool,
    },
    Commit {
        message: String,
        amend: bool,
    },
    StashSave {
        message: Option<String>,
        include_untracked: bool,
    },
    StashApply {
        index: usize,
    },
    StashPop {
        index: usize,
    },
    StashDrop {
        index: usize,
    },
    /// `git stash store -m MESSAGE COMMIT`: bring a dropped stash back.
    StashStore {
        commit: String,
        message: String,
    },
    Continue(Pending),
    Abort(Pending),
    SkipRebaseStep,
    TakeOurs {
        path: String,
    },
    TakeTheirs {
        path: String,
    },
    MarkResolved {
        paths: Vec<String>,
    },
}

fn v<const N: usize>(items: [&str; N]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn with_paths(mut args: Vec<String>, paths: &[String]) -> Vec<String> {
    args.push("--".into());
    args.extend(paths.iter().cloned());
    args
}

impl GitOp {
    /// The git invocations, run in order; the first failure stops the rest.
    pub fn commands(&self) -> Vec<Vec<String>> {
        use GitOp::*;
        let one = |a: Vec<String>| vec![a];
        match self {
            GhPrCheckout { number } => one(v(["pr", "checkout", &number.to_string()])),
            GhRunRerun { id, failed_only } => {
                let mut args = v(["run", "rerun", &id.to_string()]);
                if *failed_only {
                    args.push("--failed".into());
                }
                one(args)
            }
            GhRunCancel { id } => one(v(["run", "cancel", &id.to_string()])),
            Switch { branch } => one(v(["switch", branch])),
            Detach { commit } => one(v(["switch", "--detach", commit])),
            SwitchTracking {
                local,
                remote_branch,
            } => one(v(["switch", "-c", local, "--track", remote_branch])),
            CreateBranch {
                name,
                start,
                switch,
            } => {
                if *switch {
                    one(v(["switch", "-c", name, start]))
                } else {
                    one(v(["branch", name, start]))
                }
            }
            RenameBranch { from, to } => one(v(["branch", "-m", from, to])),
            Untrack { paths } => one(with_paths(v(["rm", "--cached", "-r", "-q"]), paths)),
            SetRemoteUrl { name, url } => one(v(["remote", "set-url", name, url])),
            DeleteBranch { name, force } => {
                one(v(["branch", if *force { "-D" } else { "-d" }, name]))
            }
            DeleteRemoteBranch { remote, branch } => one(v(["push", remote, "--delete", branch])),
            SetUpstream { branch, upstream } => {
                one(v(["branch", "--set-upstream-to", upstream, branch]))
            }
            FastForward { branch_ref, to_ref } => {
                one(v(["fetch", ".", &format!("{to_ref}:{branch_ref}")]))
            }
            Merge { from, mode } => {
                let mut a = v(["merge", "--no-edit"]);
                match mode {
                    MergeMode::Default => {}
                    MergeMode::NoFastForward => a.push("--no-ff".into()),
                    MergeMode::FastForwardOnly => a.push("--ff-only".into()),
                    MergeMode::Squash => a.push("--squash".into()),
                }
                a.push(from.clone());
                one(a)
            }
            Rebase { onto, branch } => {
                let mut a = v(["rebase", onto]);
                a.extend(branch.clone());
                one(a)
            }
            CherryPick { commit } => one(v(["cherry-pick", commit])),
            Revert { commit } => one(v(["revert", "--no-edit", commit])),
            Reset { target, mode } => {
                let flag = match mode {
                    ResetMode::Soft => "--soft",
                    ResetMode::Mixed => "--mixed",
                    ResetMode::Hard => "--hard",
                };
                one(v(["reset", flag, target]))
            }
            CreateTag {
                name,
                target,
                message,
            } => match message {
                Some(m) if !m.trim().is_empty() => one(v(["tag", "-a", name, target, "-m", m])),
                _ => one(v(["tag", name, target])),
            },
            DeleteTag { name } => one(v(["tag", "-d", name])),
            PushTag { remote, name } => one(v(["push", remote, &format!("refs/tags/{name}")])),
            Fetch => one(v(["fetch", "--all", "--prune"])),
            Pull { mode } => one(v([
                "pull",
                "--no-edit",
                match mode {
                    PullMode::FastForwardOrMerge => "--no-rebase",
                    PullMode::FastForwardOnly => "--ff-only",
                    PullMode::Rebase => "--rebase",
                },
            ])),
            Push {
                remote,
                branch,
                set_upstream,
                force,
            } => {
                let mut a = v(["push"]);
                if *force {
                    a.push("--force-with-lease".into());
                }
                if *set_upstream {
                    a.push("--set-upstream".into());
                }
                a.push(remote.clone());
                a.push(format!("refs/heads/{branch}:refs/heads/{branch}"));
                one(a)
            }
            Stage { paths } => one(with_paths(v(["add", "-A"]), paths)),
            StageAll => one(v(["add", "-A"])),
            Unstage { paths, unborn } => {
                if *unborn {
                    one(with_paths(v(["rm", "--cached", "-r", "-q"]), paths))
                } else {
                    one(with_paths(v(["restore", "--staged"]), paths))
                }
            }
            UnstageAll { unborn } => {
                if *unborn {
                    one(v(["rm", "--cached", "-r", "-q", "."]))
                } else {
                    one(v(["reset", "-q"]))
                }
            }
            Discard { paths } => one(with_paths(v(["restore", "--worktree"]), paths)),
            DeleteUntracked { paths } => one(with_paths(v(["clean", "-f", "-q"]), paths)),
            ApplyPatch {
                cached, reverse, ..
            } => {
                let mut a = v(["apply", "--whitespace=nowarn"]);
                if *cached {
                    a.push("--cached".into());
                }
                if *reverse {
                    a.push("-R".into());
                }
                a.push("-".into());
                one(a)
            }
            Commit { message, amend } => {
                if *amend && message.trim().is_empty() {
                    one(v(["commit", "--amend", "--no-edit"]))
                } else if *amend {
                    one(v(["commit", "--amend", "-F", "-"]))
                } else {
                    one(v(["commit", "-F", "-"]))
                }
            }
            StashSave {
                message,
                include_untracked,
            } => {
                let mut a = v(["stash", "push"]);
                if *include_untracked {
                    a.push("--include-untracked".into());
                }
                if let Some(m) = message.as_ref().filter(|m| !m.trim().is_empty()) {
                    a.push("-m".into());
                    a.push(m.clone());
                }
                one(a)
            }
            StashApply { index } => one(v(["stash", "apply", &format!("stash@{{{index}}}")])),
            StashPop { index } => one(v(["stash", "pop", &format!("stash@{{{index}}}")])),
            StashDrop { index } => one(v(["stash", "drop", &format!("stash@{{{index}}}")])),
            StashStore { commit, message } => one(v(["stash", "store", "-m", message, commit])),
            Continue(p) => one(match p {
                Pending::Merge | Pending::None => v(["commit", "--no-edit"]),
                Pending::Rebase => v(["rebase", "--continue"]),
                Pending::CherryPick => v(["cherry-pick", "--continue"]),
                Pending::Revert => v(["revert", "--continue"]),
                Pending::ApplyMailbox => v(["am", "--continue"]),
                Pending::Bisect => v(["bisect", "reset"]),
            }),
            Abort(p) => one(match p {
                Pending::Merge | Pending::None => v(["merge", "--abort"]),
                Pending::Rebase => v(["rebase", "--abort"]),
                Pending::CherryPick => v(["cherry-pick", "--abort"]),
                Pending::Revert => v(["revert", "--abort"]),
                Pending::ApplyMailbox => v(["am", "--abort"]),
                Pending::Bisect => v(["bisect", "reset"]),
            }),
            SkipRebaseStep => one(v(["rebase", "--skip"])),
            TakeOurs { path } => vec![
                v(["checkout", "--ours", "--", path]),
                v(["add", "--", path]),
            ],
            TakeTheirs { path } => vec![
                v(["checkout", "--theirs", "--", path]),
                v(["add", "--", path]),
            ],
            MarkResolved { paths } => one(with_paths(v(["add"]), paths)),
        }
    }

    fn stdin(&self) -> Option<&[u8]> {
        match self {
            GitOp::ApplyPatch { patch, .. } => Some(patch),
            GitOp::Commit { message, .. } if !message.trim().is_empty() => Some(message.as_bytes()),
            _ => None,
        }
    }

    pub fn is_network(&self) -> bool {
        matches!(
            self,
            GitOp::Fetch
                | GitOp::Pull { .. }
                | GitOp::Push { .. }
                | GitOp::DeleteRemoteBranch { .. }
                | GitOp::PushTag { .. }
                | GitOp::GhPrCheckout { .. }
                | GitOp::GhRunRerun { .. }
                | GitOp::GhRunCancel { .. }
        )
    }

    /// The executable the operation runs: `gh` for GitHub CLI operations.
    pub fn program(&self) -> &'static str {
        match self {
            GitOp::GhPrCheckout { .. } | GitOp::GhRunRerun { .. } | GitOp::GhRunCancel { .. } => {
                "gh"
            }
            _ => "git",
        }
    }

    /// Operations that only talk to GitHub and change nothing locally.
    pub fn is_remote_only(&self) -> bool {
        matches!(self, GitOp::GhRunRerun { .. } | GitOp::GhRunCancel { .. })
    }

    pub fn refresh(&self) -> Refresh {
        use GitOp::*;
        match self {
            GhRunRerun { .. }
            | GhRunCancel { .. }
            | Stage { .. }
            | StageAll
            | Unstage { .. }
            | UnstageAll { .. }
            | Discard { .. }
            | DeleteUntracked { .. }
            | Untrack { .. }
            | ApplyPatch { .. }
            | MarkResolved { .. }
            | TakeOurs { .. }
            | TakeTheirs { .. } => Refresh::Status,
            _ => Refresh::Full,
        }
    }

    /// Short description for progress and result messages.
    pub fn label(&self) -> String {
        use GitOp::*;
        match self {
            GhPrCheckout { number } => format!("Check out PR #{number}"),
            GhRunRerun {
                id,
                failed_only: true,
            } => format!("Re-run failed jobs of run {id}"),
            GhRunRerun { id, .. } => format!("Re-run run {id}"),
            GhRunCancel { id } => format!("Cancel run {id}"),
            Switch { branch } => format!("Switch to {branch}"),
            Detach { commit } => format!("Check out {}", short(commit)),
            SwitchTracking {
                local,
                remote_branch,
            } => format!("Check out {remote_branch} as {local}"),
            CreateBranch { name, .. } => format!("Create branch {name}"),
            RenameBranch { from, to } => format!("Rename {from} to {to}"),
            Untrack { paths } => format!("Stop tracking {}", files(paths)),
            SetRemoteUrl { name, .. } => format!("Set URL of remote {name}"),
            DeleteBranch { name, .. } => format!("Delete branch {name}"),
            DeleteRemoteBranch { remote, branch } => format!("Delete {remote}/{branch}"),
            SetUpstream { branch, upstream } => format!("Track {upstream} from {branch}"),
            FastForward { branch_ref, .. } => format!(
                "Fast-forward {}",
                branch_ref.trim_start_matches("refs/heads/")
            ),
            Merge {
                from,
                mode: MergeMode::Squash,
            } => format!("Squash {from}"),
            Merge { from, .. } => format!("Merge {from}"),
            Rebase { onto, branch: None } => format!("Rebase onto {}", short(onto)),
            Rebase {
                onto,
                branch: Some(b),
            } => format!("Rebase {b} onto {}", short(onto)),
            CherryPick { commit } => format!("Cherry-pick {}", short(commit)),
            Revert { commit } => format!("Revert {}", short(commit)),
            Reset { target, mode } => {
                let mode = match mode {
                    ResetMode::Soft => "soft",
                    ResetMode::Mixed => "mixed",
                    ResetMode::Hard => "hard",
                };
                format!("Reset ({mode}) to {}", short(target))
            }
            CreateTag { name, .. } => format!("Create tag {name}"),
            DeleteTag { name } => format!("Delete tag {name}"),
            PushTag { name, remote } => format!("Push tag {name} to {remote}"),
            Fetch => "Fetch".into(),
            Pull { .. } => "Pull".into(),
            Push {
                branch,
                remote,
                force,
                ..
            } => {
                format!(
                    "{} {branch} to {remote}",
                    if *force { "Force push" } else { "Push" }
                )
            }
            Stage { paths } => format!("Stage {}", files(paths)),
            StageAll => "Stage all changes".into(),
            Unstage { paths, .. } => format!("Unstage {}", files(paths)),
            UnstageAll { .. } => "Unstage all changes".into(),
            Discard { paths } => format!("Discard changes in {}", files(paths)),
            DeleteUntracked { paths } => format!("Delete {}", files(paths)),
            ApplyPatch {
                cached: true,
                reverse: false,
                ..
            } => "Stage hunk".into(),
            ApplyPatch {
                cached: true,
                reverse: true,
                ..
            } => "Unstage hunk".into(),
            ApplyPatch { cached: false, .. } => "Discard hunk".into(),
            Commit { amend: true, .. } => "Amend commit".into(),
            Commit { .. } => "Commit".into(),
            StashSave { .. } => "Stash changes".into(),
            StashApply { index } => format!("Apply stash@{{{index}}}"),
            StashPop { index } => format!("Pop stash@{{{index}}}"),
            StashDrop { index } => format!("Drop stash@{{{index}}}"),
            StashStore { commit, .. } => format!("Restore stash {}", short(commit)),
            Continue(p) => format!("Continue {}", p.label().to_lowercase()),
            Abort(p) => format!("Abort {}", p.label().to_lowercase()),
            SkipRebaseStep => "Skip commit".into(),
            TakeOurs { path } | TakeTheirs { path } => format!("Resolve {path}"),
            MarkResolved { paths } => format!("Mark {} resolved", files(paths)),
        }
    }

    /// Run the operation. Returns everything git printed (stdout, then
    /// stderr) on success.
    pub fn run(&self, workdir: &Path) -> Result<String> {
        let mut messages = Vec::new();
        let program = match self.program() {
            "gh" => crate::github::gh_program(),
            p => p.to_owned(),
        };
        for args in self.commands() {
            let out = cli::run_program(&program, workdir, &args, self.stdin())?;
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            match (stdout.is_empty(), out.stderr.is_empty()) {
                (true, true) => {}
                (false, true) => messages.push(stdout),
                (true, false) => messages.push(out.stderr),
                (false, false) => messages.push(format!("{stdout}\n{}", out.stderr)),
            }
        }
        Ok(messages.join("\n"))
    }
}

fn short(rev: &str) -> &str {
    if rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit()) {
        &rev[..7]
    } else {
        rev
    }
}

fn files(paths: &[String]) -> String {
    match paths {
        [one] => one.rsplit('/').next().unwrap_or(one).to_owned(),
        _ => format!("{} files", paths.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_uses_explicit_refspec() {
        let op = GitOp::Push {
            remote: "origin".into(),
            branch: "feat/x".into(),
            set_upstream: true,
            force: true,
        };
        assert_eq!(
            op.commands(),
            vec![v([
                "push",
                "--force-with-lease",
                "--set-upstream",
                "origin",
                "refs/heads/feat/x:refs/heads/feat/x"
            ])]
        );
        assert!(op.is_network());
    }

    #[test]
    fn paths_follow_double_dash() {
        let op = GitOp::Stage {
            paths: vec!["-weird".into()],
        };
        assert_eq!(op.commands(), vec![v(["add", "-A", "--", "-weird"])]);
        assert_eq!(op.refresh(), Refresh::Status);
    }

    #[test]
    fn commit_message_goes_through_stdin() {
        let op = GitOp::Commit {
            message: "hello".into(),
            amend: false,
        };
        assert_eq!(op.commands(), vec![v(["commit", "-F", "-"])]);
        assert_eq!(op.stdin(), Some(&b"hello"[..]));
        let amend = GitOp::Commit {
            message: " ".into(),
            amend: true,
        };
        assert_eq!(
            amend.commands(),
            vec![v(["commit", "--amend", "--no-edit"])]
        );
        assert_eq!(amend.stdin(), None);
    }

    #[test]
    fn labels_shorten_hashes() {
        let sha = "0123456789abcdef0123456789abcdef01234567".to_string();
        assert_eq!(
            GitOp::CherryPick { commit: sha }.label(),
            "Cherry-pick 0123456"
        );
    }
}
