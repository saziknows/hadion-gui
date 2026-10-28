//! Which operation stopped on the conflict, and what "ours" and "theirs"
//! are in it.
//!
//! - Merge: ours is HEAD (the branch you are on), theirs is `MERGE_HEAD`.
//! - Rebase: reversed. Ours is the upstream being rebased onto (plus the
//!   commits already replayed), theirs is the commit being applied. The
//!   step ("3 of 7") and the commit come from `rebase-merge/` (`msgnum`,
//!   `end`, `stopped-sha`, `message`) or `rebase-apply/` (`next`, `last`).
//! - Cherry-pick and revert: ours is HEAD, theirs is `CHERRY_PICK_HEAD` /
//!   `REVERT_HEAD` (for a revert, the inverse of that commit).
//! - `git am`: theirs is the patch being applied.
//! - Anything else (a stash applied with conflicts): the markers' labels.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::devops::workbench::git;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OpKind {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    /// `git am` (a mailbox being applied).
    Am,
    /// No operation in progress (a stash apply, `checkout -m`).
    #[default]
    None,
}

impl OpKind {
    pub fn label(self) -> &'static str {
        match self {
            OpKind::Merge => "Merge",
            OpKind::Rebase => "Rebase",
            OpKind::CherryPick => "Cherry-pick",
            OpKind::Revert => "Revert",
            OpKind::Am => "Apply mailbox",
            OpKind::None => "Conflicts",
        }
    }
}

/// A commit as shown in labels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rev {
    pub sha: String,
    pub short: String,
    pub subject: String,
    /// Local and remote branches pointing at it.
    pub names: Vec<String>,
}

impl Rev {
    /// `feature (5d6e7f8)` or `5d6e7f8`.
    pub fn named(&self) -> String {
        match self.names.first() {
            Some(n) => format!("{n} ({})", self.short),
            None => self.short.clone(),
        }
    }

    /// `5d6e7f8 Fix the parser`.
    pub fn with_subject(&self) -> String {
        if self.subject.is_empty() {
            self.short.clone()
        } else {
            format!("{} {}", self.short, self.subject)
        }
    }
}

/// The files of an operation in progress, read from the git directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpFiles {
    pub merge_head: Option<String>,
    pub merge_msg: Option<String>,
    pub cherry_pick_head: Option<String>,
    pub revert_head: Option<String>,
    pub rebase_head: Option<String>,
    /// `rebase-merge/` (the default backend).
    pub rebase_merge: Option<RebaseFiles>,
    /// `rebase-apply/` (`rebase --apply` or `git am`).
    pub rebase_apply: Option<RebaseFiles>,
    /// `git am` rather than a rebase (`rebase-apply/applying`).
    pub am: bool,
    /// Lines left in `sequencer/todo` (cherry-pick / revert of several
    /// commits).
    pub sequencer_todo: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RebaseFiles {
    /// `refs/heads/feature`, or `detached HEAD`.
    pub head_name: Option<String>,
    pub onto: Option<String>,
    pub step: Option<u32>,
    pub total: Option<u32>,
    /// The commit being applied.
    pub stopped: Option<String>,
    /// First line of the commit message being applied.
    pub subject: Option<String>,
}

fn read_trim(p: &Path) -> Option<String> {
    let s = std::fs::read_to_string(p).ok()?;
    let s = s.trim().to_owned();
    (!s.is_empty()).then_some(s)
}

fn first_line(s: &str) -> Option<String> {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
}

/// Read the operation files of `git_dir`.
pub fn read_op_files(git_dir: &Path) -> OpFiles {
    let f = |name: &str| read_trim(&git_dir.join(name));
    let first_sha = |name: &str| f(name).and_then(|s| s.lines().next().map(str::to_owned));
    let rebase = |dir: &str, apply: bool| -> Option<RebaseFiles> {
        let d = git_dir.join(dir);
        if !d.is_dir() {
            return None;
        }
        let r = |n: &str| read_trim(&d.join(n));
        let num = |n: &str| r(n).and_then(|s| s.parse::<u32>().ok());
        Some(if apply {
            RebaseFiles {
                head_name: r("head-name"),
                onto: r("onto"),
                step: num("next"),
                total: num("last"),
                stopped: r("original-commit"),
                subject: r("final-commit")
                    .or_else(|| r("msg"))
                    .and_then(|m| first_line(&m)),
            }
        } else {
            RebaseFiles {
                head_name: r("head-name"),
                onto: r("onto"),
                step: num("msgnum"),
                total: num("end"),
                stopped: r("stopped-sha"),
                subject: r("message").and_then(|m| first_line(&m)),
            }
        })
    };
    let todo = std::fs::read_to_string(git_dir.join("sequencer/todo"))
        .map(|s| {
            s.lines()
                .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
                .count()
        })
        .unwrap_or(0);
    OpFiles {
        merge_head: first_sha("MERGE_HEAD"),
        merge_msg: f("MERGE_MSG"),
        cherry_pick_head: first_sha("CHERRY_PICK_HEAD"),
        revert_head: first_sha("REVERT_HEAD"),
        rebase_head: first_sha("REBASE_HEAD"),
        rebase_merge: rebase("rebase-merge", false),
        rebase_apply: rebase("rebase-apply", true),
        am: git_dir.join("rebase-apply/applying").exists(),
        sequencer_todo: todo,
    }
}

/// What a pane is called.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SideLabel {
    /// `Ours`, `Theirs`.
    pub role: &'static str,
    /// What it is in this operation: `HEAD`, `upstream`, `MERGE_HEAD`,
    /// `being applied`.
    pub tag: &'static str,
    /// `main`, `feature`, `5d6e7f8 Fix the parser`.
    pub name: String,
    /// One sentence on what this side is in this operation.
    pub detail: String,
}

/// The operation in progress and its sides.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Operation {
    pub kind: OpKind,
    /// The branch HEAD is on (`None` when detached, as during a rebase).
    pub head_branch: Option<String>,
    pub head: Option<Rev>,
    /// MERGE_HEAD / the commit being applied / picked / reverted.
    pub other: Option<Rev>,
    /// The branch name a merge brought in, from `MERGE_MSG`.
    pub merge_name: Option<String>,
    /// Rebase: the branch being rebased.
    pub rebasing: Option<String>,
    /// Rebase: the commit it is rebased onto.
    pub onto: Option<Rev>,
    /// Rebase / am: `(step, total)`.
    pub step: Option<(u32, u32)>,
    /// Commits left after this one (cherry-pick / revert sequences).
    pub left: usize,
}

/// `Merge branch 'feature'` / `Merge branch 'x' of url` / `Merge
/// remote-tracking branch 'origin/x'` / `Merge commit 'abc'` → the name.
pub fn merge_msg_name(msg: &str) -> Option<String> {
    let line = msg.lines().next()?.trim();
    let rest = line.strip_prefix("Merge ")?;
    let start = rest.find('\'')?;
    let end = rest[start + 1..].find('\'')? + start + 1;
    let name = &rest[start + 1..end];
    (!name.is_empty()).then(|| name.to_owned())
}

/// Describe the operation from its files and looked-up commits (pure).
pub fn describe(
    files: &OpFiles,
    head_branch: Option<String>,
    revs: &HashMap<String, Rev>,
) -> Operation {
    let rev = |sha: &Option<String>| -> Option<Rev> {
        let sha = sha.as_ref()?;
        Some(revs.get(sha).cloned().unwrap_or_else(|| Rev {
            sha: sha.clone(),
            short: sha.chars().take(7).collect(),
            ..Default::default()
        }))
    };
    let head = revs.get("HEAD").cloned();
    let mut op = Operation {
        head_branch,
        head,
        ..Default::default()
    };
    let rebase = files.rebase_merge.as_ref().or(files.rebase_apply.as_ref());
    if let Some(rb) = rebase {
        op.kind = if files.am && files.rebase_merge.is_none() {
            OpKind::Am
        } else {
            OpKind::Rebase
        };
        op.rebasing = rb
            .head_name
            .as_deref()
            .filter(|h| *h != "detached HEAD")
            .map(|h| h.strip_prefix("refs/heads/").unwrap_or(h).to_owned());
        op.onto = rev(&rb.onto);
        op.step = rb.step.zip(rb.total);
        let stopped = rb.stopped.clone().or_else(|| files.rebase_head.clone());
        op.other = rev(&stopped).map(|mut r| {
            if r.subject.is_empty()
                && let Some(s) = &rb.subject
            {
                r.subject = s.clone();
            }
            r
        });
        if op.other.is_none()
            && let Some(s) = &rb.subject
        {
            op.other = Some(Rev {
                subject: s.clone(),
                ..Default::default()
            });
        }
    } else if files.merge_head.is_some() {
        op.kind = OpKind::Merge;
        op.other = rev(&files.merge_head);
        op.merge_name = files.merge_msg.as_deref().and_then(merge_msg_name);
    } else if files.cherry_pick_head.is_some() {
        op.kind = OpKind::CherryPick;
        op.other = rev(&files.cherry_pick_head);
        op.left = files.sequencer_todo.saturating_sub(1);
    } else if files.revert_head.is_some() {
        op.kind = OpKind::Revert;
        op.other = rev(&files.revert_head);
        op.left = files.sequencer_todo.saturating_sub(1);
    }
    op
}

impl Operation {
    fn head_name(&self) -> String {
        match (&self.head_branch, &self.head) {
            (Some(b), _) => b.clone(),
            (None, Some(h)) => format!("HEAD ({})", h.short),
            (None, None) => "HEAD".to_owned(),
        }
    }

    fn other_name(&self) -> String {
        match (&self.merge_name, &self.other) {
            (Some(n), _) => n.clone(),
            (None, Some(r)) => r.names.first().cloned().unwrap_or_else(|| r.short.clone()),
            (None, None) => "the other side".to_owned(),
        }
    }

    /// `Merge`, `Rebase · step 3 of 7`, `Cherry-pick`.
    pub fn title(&self) -> String {
        match self.step {
            Some((s, t)) if matches!(self.kind, OpKind::Rebase | OpKind::Am) => {
                format!("{} · step {s} of {t}", self.kind.label())
            }
            _ => self.kind.label().to_owned(),
        }
    }

    /// One sentence: what is happening.
    pub fn summary(&self) -> String {
        match self.kind {
            OpKind::Merge => format!("Merging {} into {}", self.other_name(), self.head_name()),
            OpKind::Rebase => {
                let what = self.rebasing.clone().unwrap_or_else(|| "HEAD".to_owned());
                let onto = self
                    .onto
                    .as_ref()
                    .map(Rev::named)
                    .unwrap_or_else(|| "its upstream".to_owned());
                let applying = self
                    .other
                    .as_ref()
                    .map(|r| format!(": applying {}", r.with_subject()))
                    .unwrap_or_default();
                format!("Rebasing {what} onto {onto}{applying}")
            }
            OpKind::CherryPick => format!(
                "Cherry-picking {} onto {}",
                self.other
                    .as_ref()
                    .map(Rev::with_subject)
                    .unwrap_or_else(|| "a commit".into()),
                self.head_name()
            ),
            OpKind::Revert => format!(
                "Reverting {} on {}",
                self.other
                    .as_ref()
                    .map(Rev::with_subject)
                    .unwrap_or_else(|| "a commit".into()),
                self.head_name()
            ),
            OpKind::Am => format!(
                "Applying {} on {}",
                self.other
                    .as_ref()
                    .filter(|r| !r.subject.is_empty())
                    .map(|r| format!("the patch \u{201c}{}\u{201d}", r.subject))
                    .unwrap_or_else(|| "a patch".into()),
                self.head_name()
            ),
            OpKind::None => format!("Conflicts on {}", self.head_name()),
        }
    }

    /// Ours and theirs are swapped compared to a merge.
    pub fn swapped(&self) -> bool {
        matches!(self.kind, OpKind::Rebase | OpKind::Am)
    }

    /// The left pane. `marker` is the label git wrote on `<<<<<<<`.
    pub fn ours(&self, marker: &str) -> SideLabel {
        match self.kind {
            OpKind::Rebase | OpKind::Am => {
                let onto = self.onto.as_ref().map(Rev::named);
                SideLabel {
                    role: "Ours",
                    tag: "upstream",
                    name: onto.clone().unwrap_or_else(|| "Upstream".to_owned()),
                    detail: format!(
                        "The commit being rebased onto{}, plus the commits already replayed on it. In a rebase, ours is the upstream, not your branch.",
                        onto.map(|o| format!(", {o}")).unwrap_or_default()
                    ),
                }
            }
            OpKind::None => SideLabel {
                role: "Ours",
                tag: "",
                name: if marker.is_empty() {
                    self.head_name()
                } else {
                    marker.to_owned()
                },
                detail: "The version in the working tree before the change was applied.".into(),
            },
            _ => SideLabel {
                role: "Ours",
                tag: "HEAD",
                name: self.head_name(),
                detail: format!(
                    "HEAD: the branch you are on{}.",
                    self.head
                        .as_ref()
                        .map(|h| format!(", at {}", h.with_subject()))
                        .unwrap_or_default()
                ),
            },
        }
    }

    /// The right pane. `marker` is the label git wrote on `>>>>>>>`.
    pub fn theirs(&self, marker: &str) -> SideLabel {
        let other = self.other.as_ref();
        match self.kind {
            OpKind::Merge => SideLabel {
                role: "Theirs",
                tag: "MERGE_HEAD",
                name: self.other_name(),
                detail: format!(
                    "MERGE_HEAD: the branch being merged in{}.",
                    other
                        .map(|r| format!(", at {}", r.with_subject()))
                        .unwrap_or_default()
                ),
            },
            OpKind::Rebase | OpKind::Am => SideLabel {
                role: "Theirs",
                tag: "being applied",
                name: other
                    .map(Rev::with_subject)
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| marker.to_owned()),
                detail: format!(
                    "The commit being applied{}{}.",
                    self.rebasing
                        .as_ref()
                        .map(|b| format!(" from {b}"))
                        .unwrap_or_default(),
                    self.step
                        .map(|(s, t)| format!(" (step {s} of {t})"))
                        .unwrap_or_default()
                ),
            },
            OpKind::CherryPick => SideLabel {
                role: "Theirs",
                tag: "CHERRY_PICK_HEAD",
                name: other
                    .map(Rev::with_subject)
                    .unwrap_or_else(|| marker.to_owned()),
                detail: "CHERRY_PICK_HEAD: the commit being picked.".into(),
            },
            OpKind::Revert => SideLabel {
                role: "Theirs",
                tag: "REVERT_HEAD",
                name: other
                    .map(|r| format!("Revert of {}", r.with_subject()))
                    .unwrap_or_else(|| marker.to_owned()),
                detail: "REVERT_HEAD: the inverse of the commit being reverted.".into(),
            },
            OpKind::None => SideLabel {
                role: "Theirs",
                tag: "",
                name: if marker.is_empty() {
                    "Theirs".to_owned()
                } else {
                    marker.to_owned()
                },
                detail: "The change being applied (a stash, a checkout -m).".into(),
            },
        }
    }
}

/// `git rev-parse --absolute-git-dir`.
pub fn git_dir(dir: &Path, cmds: &mut Vec<String>) -> Result<PathBuf, String> {
    git(dir, &["rev-parse", "--absolute-git-dir"], cmds).map(|s| PathBuf::from(s.trim()))
}

/// Parse `git log --no-walk --format=%H%x1f%h%x1f%s` output.
pub fn parse_revs(out: &str) -> Vec<Rev> {
    out.lines()
        .filter_map(|l| {
            let mut it = l.split('\x1f');
            let sha = it.next()?.trim();
            if sha.is_empty() {
                return None;
            }
            Some(Rev {
                sha: sha.to_owned(),
                short: it.next().unwrap_or_default().to_owned(),
                subject: it.next().unwrap_or_default().to_owned(),
                names: Vec::new(),
            })
        })
        .collect()
}

/// Parse `git for-each-ref --format=%(objectname)%1f%(refname:short)`
/// (for-each-ref writes hex escapes as `%1f`, not `%x1f`).
pub fn parse_refs(out: &str) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for l in out.lines() {
        if let Some((sha, name)) = l.split_once('\x1f')
            && !name.ends_with("/HEAD")
        {
            map.entry(sha.to_owned()).or_default().push(name.to_owned());
        }
    }
    // Local branches before remote ones.
    for v in map.values_mut() {
        v.sort_by_key(|n| n.contains('/'));
    }
    map
}

/// Read the operation in progress in `dir`.
pub fn operation(dir: &Path, cmds: &mut Vec<String>) -> Result<Operation, String> {
    let gd = git_dir(dir, cmds)?;
    let files = read_op_files(&gd);
    cmds.push(format!(
        "read {} (MERGE_HEAD, MERGE_MSG, CHERRY_PICK_HEAD, REVERT_HEAD, REBASE_HEAD, rebase-merge/, rebase-apply/, sequencer/todo)",
        gd.display()
    ));
    let head_branch = git(dir, &["symbolic-ref", "--short", "-q", "HEAD"], cmds)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let mut shas: Vec<String> = Vec::new();
    let rb = files.rebase_merge.as_ref().or(files.rebase_apply.as_ref());
    for s in [
        files.merge_head.clone(),
        files.cherry_pick_head.clone(),
        files.revert_head.clone(),
        files.rebase_head.clone(),
        rb.and_then(|r| r.onto.clone()),
        rb.and_then(|r| r.stopped.clone()),
    ]
    .into_iter()
    .flatten()
    {
        if !shas.contains(&s) {
            shas.push(s);
        }
    }
    let mut args: Vec<String> = vec![
        "log".into(),
        "--no-walk=unsorted".into(),
        "--format=%H%x1f%h%x1f%s".into(),
        "HEAD".into(),
    ];
    args.extend(shas.iter().cloned());
    let mut revs: HashMap<String, Rev> = HashMap::new();
    if let Ok(out) = git(dir, &args, cmds) {
        let list = parse_revs(&out);
        let names = git(
            dir,
            &[
                "for-each-ref",
                "--format=%(objectname)%1f%(refname:short)",
                "refs/heads",
                "refs/remotes",
            ],
            cmds,
        )
        .map(|o| parse_refs(&o))
        .unwrap_or_default();
        for (i, mut r) in list.into_iter().enumerate() {
            r.names = names.get(&r.sha).cloned().unwrap_or_default();
            if i == 0 {
                revs.insert("HEAD".into(), r.clone());
            }
            revs.insert(r.sha.clone(), r);
        }
    }
    Ok(describe(&files, head_branch, &revs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rev(sha: &str, subject: &str, names: &[&str]) -> Rev {
        Rev {
            sha: sha.into(),
            short: sha[..7].into(),
            subject: subject.into(),
            names: names.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn revs() -> HashMap<String, Rev> {
        let mut m = HashMap::new();
        m.insert("HEAD".into(), rev("1111111aaaa", "Main work", &["main"]));
        m.insert(
            "2222222bbbb".into(),
            rev("2222222bbbb", "Feature work", &["feature"]),
        );
        m.insert(
            "3333333cccc".into(),
            rev("3333333cccc", "Upstream tip", &["origin/main"]),
        );
        m
    }

    #[test]
    fn merge() {
        let files = OpFiles {
            merge_head: Some("2222222bbbb".into()),
            merge_msg: Some("Merge branch 'feature'\n\n# Conflicts:\n#\tf.txt".into()),
            ..Default::default()
        };
        let op = describe(&files, Some("main".into()), &revs());
        assert_eq!(op.kind, OpKind::Merge);
        assert_eq!(op.title(), "Merge");
        assert_eq!(op.summary(), "Merging feature into main");
        assert_eq!(op.ours("HEAD").name, "main");
        assert!(op.ours("HEAD").detail.contains("1111111 Main work"));
        assert_eq!(op.theirs("feature").name, "feature");
        assert!(!op.swapped());
    }

    #[test]
    fn rebase_is_reversed() {
        let files = OpFiles {
            rebase_merge: Some(RebaseFiles {
                head_name: Some("refs/heads/topic".into()),
                onto: Some("3333333cccc".into()),
                step: Some(3),
                total: Some(7),
                stopped: Some("2222222bbbb".into()),
                subject: Some("Feature work".into()),
            }),
            rebase_head: Some("2222222bbbb".into()),
            ..Default::default()
        };
        let op = describe(&files, None, &revs());
        assert_eq!(op.kind, OpKind::Rebase);
        assert!(op.swapped());
        assert_eq!(op.title(), "Rebase · step 3 of 7");
        assert_eq!(
            op.summary(),
            "Rebasing topic onto origin/main (3333333): applying 2222222 Feature work"
        );
        assert_eq!(op.ours("HEAD").name, "origin/main (3333333)");
        assert_eq!(op.ours("HEAD").tag, "upstream");
        assert!(op.ours("HEAD").detail.contains("not your branch"));
        assert_eq!(op.theirs("x").name, "2222222 Feature work");
        assert!(op.theirs("x").detail.contains("step 3 of 7"));
        assert!(op.theirs("x").detail.contains("from topic"));
    }

    #[test]
    fn rebase_without_lookups_uses_the_files() {
        let files = OpFiles {
            rebase_merge: Some(RebaseFiles {
                head_name: Some("detached HEAD".into()),
                onto: Some("abcdef0123".into()),
                step: Some(1),
                total: Some(1),
                stopped: Some("9999999999".into()),
                subject: Some("Subject from message".into()),
            }),
            ..Default::default()
        };
        let op = describe(&files, None, &HashMap::new());
        assert_eq!(op.rebasing, None);
        assert_eq!(op.theirs("").name, "9999999 Subject from message");
        assert_eq!(op.ours("").name, "abcdef0");
    }

    #[test]
    fn cherry_pick_revert_am_and_none() {
        let cp = OpFiles {
            cherry_pick_head: Some("2222222bbbb".into()),
            sequencer_todo: 3,
            ..Default::default()
        };
        let op = describe(&cp, Some("main".into()), &revs());
        assert_eq!(op.kind, OpKind::CherryPick);
        assert_eq!(op.left, 2);
        assert_eq!(
            op.summary(),
            "Cherry-picking 2222222 Feature work onto main"
        );
        assert_eq!(op.theirs("").name, "2222222 Feature work");
        let rv = OpFiles {
            revert_head: Some("2222222bbbb".into()),
            ..Default::default()
        };
        let op = describe(&rv, Some("main".into()), &revs());
        assert_eq!(op.kind, OpKind::Revert);
        assert_eq!(op.theirs("").name, "Revert of 2222222 Feature work");
        let am = OpFiles {
            rebase_apply: Some(RebaseFiles {
                step: Some(2),
                total: Some(4),
                subject: Some("Add a thing".into()),
                ..Default::default()
            }),
            am: true,
            ..Default::default()
        };
        let op = describe(&am, Some("main".into()), &revs());
        assert_eq!(op.kind, OpKind::Am);
        assert_eq!(op.title(), "Apply mailbox · step 2 of 4");
        assert_eq!(
            op.summary(),
            "Applying the patch \u{201c}Add a thing\u{201d} on main"
        );
        let none = describe(&OpFiles::default(), Some("main".into()), &revs());
        assert_eq!(none.kind, OpKind::None);
        assert_eq!(none.ours("Updated upstream").name, "Updated upstream");
        assert_eq!(none.theirs("Stashed changes").name, "Stashed changes");
        assert_eq!(none.summary(), "Conflicts on main");
    }

    #[test]
    fn parsers() {
        assert_eq!(
            merge_msg_name("Merge branch 'feature'\n"),
            Some("feature".into())
        );
        assert_eq!(
            merge_msg_name("Merge remote-tracking branch 'origin/x' into main"),
            Some("origin/x".into())
        );
        assert_eq!(merge_msg_name("Something else"), None);
        let revs = parse_revs("aaaa\x1faaa\x1fOne\nbbbb\x1fbbb\x1fTwo words\n\n");
        assert_eq!(revs.len(), 2);
        assert_eq!(revs[1].subject, "Two words");
        let refs = parse_refs("aaaa\x1forigin/main\naaaa\x1fmain\nbbbb\x1forigin/HEAD\n");
        assert_eq!(refs["aaaa"], vec!["main", "origin/main"]);
        assert!(!refs.contains_key("bbbb"));
    }

    #[test]
    fn reads_files_from_a_git_dir() {
        let dir = std::env::temp_dir().join(format!(
            "gitvis-conflict-labels-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("rebase-merge")).unwrap();
        std::fs::write(dir.join("rebase-merge/head-name"), "refs/heads/topic\n").unwrap();
        std::fs::write(dir.join("rebase-merge/onto"), "3333333cccc\n").unwrap();
        std::fs::write(dir.join("rebase-merge/msgnum"), "2\n").unwrap();
        std::fs::write(dir.join("rebase-merge/end"), "5\n").unwrap();
        std::fs::write(dir.join("rebase-merge/stopped-sha"), "2222222bbbb\n").unwrap();
        std::fs::write(
            dir.join("rebase-merge/message"),
            "Feature work\n\nbody\n# Conflicts:\n",
        )
        .unwrap();
        std::fs::write(dir.join("REBASE_HEAD"), "2222222bbbb\n").unwrap();
        let f = read_op_files(&dir);
        let rb = f.rebase_merge.clone().unwrap();
        assert_eq!(rb.step, Some(2));
        assert_eq!(rb.total, Some(5));
        assert_eq!(rb.subject.as_deref(), Some("Feature work"));
        assert_eq!(f.rebase_head.as_deref(), Some("2222222bbbb"));
        assert!(f.merge_head.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
