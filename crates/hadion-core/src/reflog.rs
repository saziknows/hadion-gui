//! HEAD reflog and the "undo" derived from it.

use std::path::Path;

use crate::model::Oid;
use crate::ops::{GitOp, ResetMode};
use crate::{Result, repo};

#[derive(Clone, Debug)]
pub struct ReflogEntry {
    /// `HEAD@{index}`.
    pub index: usize,
    pub old: Oid,
    pub new: Oid,
    pub message: String,
    pub time: i64,
}

pub fn read(path: &Path, limit: usize) -> Result<Vec<ReflogEntry>> {
    let repo = repo::open(path)?;
    let log = match repo.reflog("HEAD") {
        Ok(l) => l,
        Err(_) => return Ok(Vec::new()),
    };
    Ok(log
        .iter()
        .take(limit)
        .enumerate()
        .map(|(index, e)| ReflogEntry {
            index,
            old: e.id_old(),
            new: e.id_new(),
            message: e
                .message_bytes()
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default(),
            time: e.committer().when().seconds(),
        })
        .collect())
}

/// What undoing the most recent HEAD movement means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Undo {
    /// e.g. `Undo merge`.
    pub label: String,
    /// Longer explanation for the confirmation dialog.
    pub detail: String,
    pub ops: Vec<GitOp>,
    /// A hard reset: uncommitted changes would be lost.
    pub destructive: bool,
}

/// Derive the undo for the newest reflog entry. `entries[0]` is newest.
pub fn undo_for(entries: &[ReflogEntry]) -> Option<Undo> {
    let latest = entries.first()?;
    let msg = latest.message.as_str();
    let old = latest.old.to_string();
    let short = |oid: &Oid| repo::short_id(*oid);
    let hard = |label: &str, detail: String, target: String| Undo {
        label: label.to_owned(),
        detail,
        ops: vec![GitOp::Reset {
            target,
            mode: ResetMode::Hard,
        }],
        destructive: true,
    };

    if let Some(rest) = msg.strip_prefix("commit (amend): ") {
        return Some(Undo {
            label: "Undo amend".into(),
            detail: format!(
                "Restores the previous commit {} and keeps the amended changes staged.\n\n{rest}",
                short(&latest.old)
            ),
            ops: vec![GitOp::Reset {
                target: old,
                mode: ResetMode::Soft,
            }],
            destructive: false,
        });
    }
    if let Some(rest) = msg
        .strip_prefix("commit: ")
        .or_else(|| msg.strip_prefix("commit (merge): "))
    {
        return Some(Undo {
            label: "Undo commit".into(),
            detail: format!("Removes the commit and keeps its changes staged.\n\n{rest}"),
            ops: vec![GitOp::Reset {
                target: old,
                mode: ResetMode::Soft,
            }],
            destructive: false,
        });
    }
    if msg.starts_with("commit (initial)") {
        return None;
    }
    if let Some(rest) = msg.strip_prefix("checkout: moving from ") {
        let from = rest.split(" to ").next().unwrap_or("").trim();
        if from.is_empty() {
            return None;
        }
        let is_sha = from.len() == 40 && from.chars().all(|c| c.is_ascii_hexdigit());
        let op = if is_sha {
            GitOp::Detach {
                commit: from.to_owned(),
            }
        } else {
            GitOp::Switch {
                branch: from.to_owned(),
            }
        };
        return Some(Undo {
            label: format!(
                "Undo checkout (back to {})",
                if is_sha { &from[..7] } else { from }
            ),
            detail: format!("Switches back to {from}."),
            ops: vec![op],
            destructive: false,
        });
    }
    if msg.starts_with("merge ") || msg.starts_with("pull") {
        let what = if msg.starts_with("pull") {
            "pull"
        } else {
            "merge"
        };
        return Some(hard(
            &format!("Undo {what}"),
            format!(
                "Resets the branch to {} (before the {what}). Uncommitted changes are lost.",
                short(&latest.old)
            ),
            old,
        ));
    }
    if msg.starts_with("rebase") {
        // Walk back to the state before the rebase started.
        let start = entries.iter().find(|e| {
            e.message.starts_with("rebase (start)") || e.message.starts_with("rebase: checkout")
        })?;
        return Some(hard(
            "Undo rebase",
            format!(
                "Resets the branch to {} (before the rebase). Uncommitted changes are lost.",
                short(&start.old)
            ),
            start.old.to_string(),
        ));
    }
    if msg.starts_with("reset: ") {
        return Some(hard(
            "Undo reset",
            format!(
                "Resets the branch back to {}. Uncommitted changes are lost.",
                short(&latest.old)
            ),
            old,
        ));
    }
    if let Some(rest) = msg.strip_prefix("cherry-pick: ") {
        return Some(hard(
            "Undo cherry-pick",
            format!("Removes the cherry-picked commit. Uncommitted changes are lost.\n\n{rest}"),
            old,
        ));
    }
    if let Some(rest) = msg.strip_prefix("revert: ") {
        return Some(hard(
            "Undo revert",
            format!("Removes the revert commit. Uncommitted changes are lost.\n\n{rest}"),
            old,
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(index: usize, message: &str) -> ReflogEntry {
        ReflogEntry {
            index,
            old: Oid::from_str("1111111111111111111111111111111111111111").unwrap(),
            new: Oid::from_str("2222222222222222222222222222222222222222").unwrap(),
            message: message.to_owned(),
            time: 0,
        }
    }

    #[test]
    fn undo_kinds() {
        let u = undo_for(&[entry(0, "commit: add thing")]).unwrap();
        assert_eq!(u.label, "Undo commit");
        assert!(!u.destructive);
        assert!(matches!(
            u.ops[0],
            GitOp::Reset {
                mode: ResetMode::Soft,
                ..
            }
        ));

        let u = undo_for(&[entry(0, "merge feature: Merge made by the 'ort' strategy.")]).unwrap();
        assert_eq!(u.label, "Undo merge");
        assert!(u.destructive);

        let u = undo_for(&[entry(0, "checkout: moving from main to feature")]).unwrap();
        assert_eq!(u.label, "Undo checkout (back to main)");
        assert_eq!(
            u.ops[0],
            GitOp::Switch {
                branch: "main".into()
            }
        );

        let u = undo_for(&[
            entry(0, "rebase (finish): returning to refs/heads/x"),
            entry(1, "rebase (pick): c"),
            entry(2, "rebase (start): checkout main"),
        ])
        .unwrap();
        assert_eq!(u.label, "Undo rebase");

        assert!(undo_for(&[entry(0, "commit (initial): root")]).is_none());
        assert!(undo_for(&[]).is_none());
    }
}
