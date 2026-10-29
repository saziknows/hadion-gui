//! "Split this commit…" from the commit menu: check the commit, explain
//! the steps in a confirmation, then undo it with its changes left in the
//! working tree (an interactive rebase that stops on it first when it is
//! not HEAD). The original message goes into the commit box.

use hadion_core::devops::gitplus::split::{self as core, SplitPlan};

use super::{Act, Msg};
use crate::devops::ctx::Ctx;
use crate::state::{RepoState, ToastKind};

#[derive(Default)]
pub struct Split {
    /// A plan is being made or carried out.
    pub busy: bool,
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::Split(rev) => {
            if repo.devops.gitplus.split.busy || repo.is_busy() {
                cx.toast(
                    ToastKind::Error,
                    "Wait for the running operation to finish.",
                );
                return;
            }
            repo.devops.gitplus.split.busy = true;
            cx.read("Check the commit to split", move |dir, log| {
                let mut cmds = Vec::new();
                let r = core::plan(dir, &rev, &mut cmds);
                log.commands.extend(cmds);
                Msg::SplitPlanned(r.map(Box::new))
            });
        }
        Act::SplitConfirmed(plan) => {
            if repo.devops.gitplus.split.busy {
                return;
            }
            repo.devops.gitplus.split.busy = true;
            cx.local_change(format!("Split \"{}\"", plan.subject), move |dir, log| {
                let mut cmds = Vec::new();
                let r = core::execute(dir, &plan, &mut cmds);
                log.commands.extend(cmds);
                let ok = r.is_ok();
                (r, Some(Msg::SplitDone(ok.then_some(plan))))
            });
        }
        _ => {}
    }
}

/// The confirmation's text: what happens, the warnings, the next steps.
pub fn body(plan: &SplitPlan) -> String {
    let mut s = if plan.head {
        format!(
            "Undoes the last commit, \"{}\", and keeps its changes in the working tree, unstaged, so you can commit them in smaller parts.\nNothing is lost: the commit stays in the reflog.",
            plan.subject
        )
    } else {
        format!(
            "Starts an interactive rebase that stops at \"{}\", then undoes that commit and keeps its changes in the working tree, unstaged. The {} after it are replayed when you continue the rebase.",
            plan.subject,
            if plan.later == 1 {
                "commit".to_owned()
            } else {
                format!("{} commits", plan.later)
            }
        )
    };
    if plan.pushed {
        s.push_str(" It is on a remote branch already: the new commits replace it, so the branch will need a force push.");
    }
    if plan.head && plan.dirty {
        s.push_str(" Your uncommitted changes stay too and mix with the split ones.");
    }
    s.push_str("\n\nNext steps:");
    for (i, step) in plan.next_steps().iter().enumerate() {
        s.push_str(&format!("\n{}. {step}", i + 1));
    }
    s
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    match msg {
        Msg::SplitPlanned(r) => {
            repo.devops.gitplus.split.busy = false;
            match r {
                Err(e) => cx.toast(ToastKind::Error, format!("Cannot split: {e}")),
                Ok(plan) => crate::devops::pages::explain::confirm(
                    cx,
                    if plan.head {
                        "Split the last commit?"
                    } else {
                        "Split this commit?"
                    },
                    body(&plan),
                    "Split",
                    plan.pushed,
                    plan.commands(),
                    Act::SplitConfirmed(plan).into(),
                ),
            }
        }
        Msg::SplitDone(plan) => {
            repo.devops.gitplus.split.busy = false;
            if let Some(plan) = plan
                && repo.commit_message.trim().is_empty()
            {
                repo.commit_message = plan.message.clone();
                repo.amend = false;
            }
        }
        _ => {}
    }
}
