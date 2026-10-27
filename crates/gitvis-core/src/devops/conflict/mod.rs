//! Merge conflicts: the three versions of a conflicted file (base, ours,
//! theirs from the index stages), conflict hunks parsed from the working
//! file's markers (including diff3 / zdiff3 style), per-hunk resolution
//! (ours, theirs, both, base, edited), writing the result and marking it
//! resolved.
//!
//! - [`parse_status`]: the conflicted files and their kind from `git status
//!   --porcelain=v2 -z` `u` records (UU, AA, DU, UD, AU, UA, DD).
//! - [`markers`]: the marker parser ([`markers::Doc`], [`markers::Hunk`]).
//! - [`resolve`]: resolutions and assembling the result.
//! - [`load`]: stage blobs through `git cat-file --batch`, binary
//!   detection, attributes (`conflict-marker-size`, `merge`), hunks from
//!   `git merge-file` for the base of merge-style conflicts, and one
//!   [`load::FileLoad`] per file.
//! - [`labels`]: which operation is in progress and what ours / theirs are.
//! - [`actions`]: the git command lines of every change (keep ours, take
//!   theirs, delete, mark resolved, recreate the markers) and writing the
//!   result file.
//!
//! Every function that runs git records its command lines in `cmds`.

pub mod actions;
pub mod labels;
pub mod load;
pub mod markers;
pub mod resolve;

use std::path::Path;

use crate::devops::workbench::git;

/// What kind of conflict a path has (`git status` XY of a `u` record).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConflictKind {
    /// UU
    BothModified,
    /// AA
    BothAdded,
    /// DU
    DeletedByUs,
    /// UD
    DeletedByThem,
    /// AU
    AddedByUs,
    /// UA
    AddedByThem,
    /// DD
    BothDeleted,
}

impl ConflictKind {
    pub fn from_xy(xy: &str) -> Option<ConflictKind> {
        Some(match xy {
            "UU" => ConflictKind::BothModified,
            "AA" => ConflictKind::BothAdded,
            "DU" => ConflictKind::DeletedByUs,
            "UD" => ConflictKind::DeletedByThem,
            "AU" => ConflictKind::AddedByUs,
            "UA" => ConflictKind::AddedByThem,
            "DD" => ConflictKind::BothDeleted,
            _ => return None,
        })
    }

    /// From which index stages exist (`[base, ours, theirs]`).
    pub fn from_stages(base: bool, ours: bool, theirs: bool) -> Option<ConflictKind> {
        Some(match (base, ours, theirs) {
            (true, true, true) => ConflictKind::BothModified,
            (false, true, true) => ConflictKind::BothAdded,
            (true, false, true) => ConflictKind::DeletedByUs,
            (true, true, false) => ConflictKind::DeletedByThem,
            (false, true, false) => ConflictKind::AddedByUs,
            (false, false, true) => ConflictKind::AddedByThem,
            (true, false, false) => ConflictKind::BothDeleted,
            (false, false, false) => return None,
        })
    }

    pub fn code(self) -> &'static str {
        match self {
            ConflictKind::BothModified => "UU",
            ConflictKind::BothAdded => "AA",
            ConflictKind::DeletedByUs => "DU",
            ConflictKind::DeletedByThem => "UD",
            ConflictKind::AddedByUs => "AU",
            ConflictKind::AddedByThem => "UA",
            ConflictKind::BothDeleted => "DD",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ConflictKind::BothModified => "Both modified",
            ConflictKind::BothAdded => "Both added",
            ConflictKind::DeletedByUs => "Deleted by us",
            ConflictKind::DeletedByThem => "Deleted by them",
            ConflictKind::AddedByUs => "Added by us",
            ConflictKind::AddedByThem => "Added by them",
            ConflictKind::BothDeleted => "Both deleted",
        }
    }

    /// What happened, in one or two sentences.
    pub fn explain(self) -> &'static str {
        match self {
            ConflictKind::BothModified => {
                "Both sides changed this file since their common ancestor, in overlapping places."
            }
            ConflictKind::BothAdded => {
                "Both sides added a file at this path (or renamed different files to it), with different contents. There is no common ancestor."
            }
            ConflictKind::DeletedByUs => {
                "Our side deleted (or renamed) this file; their side changed it. The working tree holds their version."
            }
            ConflictKind::DeletedByThem => {
                "Their side deleted (or renamed) this file; our side changed it. The working tree holds our version."
            }
            ConflictKind::AddedByUs => {
                "Only our side has this path, usually after a rename that the other side deleted or renamed differently."
            }
            ConflictKind::AddedByThem => {
                "Only their side has this path, usually after a rename that our side deleted or renamed differently."
            }
            ConflictKind::BothDeleted => {
                "Both sides removed this path, usually because each renamed it to a different name. Look for the file under its new names."
            }
        }
    }

    /// The working file can hold conflict markers (both sides have text).
    pub fn two_sided(self) -> bool {
        matches!(self, ConflictKind::BothModified | ConflictKind::BothAdded)
    }
}

/// One index stage of a conflicted path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageRef {
    pub mode: String,
    pub oid: String,
}

/// A conflicted path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictEntry {
    pub path: String,
    pub kind: ConflictKind,
    /// Stages 1 (base), 2 (ours), 3 (theirs).
    pub stages: [Option<StageRef>; 3],
    /// Mode of the working tree file (`000000` when absent).
    pub worktree_mode: String,
}

impl ConflictEntry {
    pub fn base(&self) -> Option<&StageRef> {
        self.stages[0].as_ref()
    }
    pub fn ours(&self) -> Option<&StageRef> {
        self.stages[1].as_ref()
    }
    pub fn theirs(&self) -> Option<&StageRef> {
        self.stages[2].as_ref()
    }
}

/// The `u` records of `git status --porcelain=v2 -z` output:
/// `u XY sub m1 m2 m3 mW h1 h2 h3 path`.
pub fn parse_status(out: &[u8]) -> Vec<ConflictEntry> {
    let mut v = Vec::new();
    for rec in out.split(|&b| b == 0) {
        if !rec.starts_with(b"u ") {
            continue;
        }
        let text = String::from_utf8_lossy(rec);
        let f: Vec<&str> = text.splitn(11, ' ').collect();
        if f.len() < 11 {
            continue;
        }
        let Some(kind) = ConflictKind::from_xy(f[1]) else {
            continue;
        };
        let stage = |m: &str, h: &str| -> Option<StageRef> {
            (m != "000000" && !h.chars().all(|c| c == '0')).then(|| StageRef {
                mode: m.to_owned(),
                oid: h.to_owned(),
            })
        };
        v.push(ConflictEntry {
            path: f[10].to_owned(),
            kind,
            stages: [stage(f[3], f[7]), stage(f[4], f[8]), stage(f[5], f[9])],
            worktree_mode: f[6].to_owned(),
        });
    }
    v.sort_by(|a, b| a.path.cmp(&b.path));
    v
}

/// The arguments of the status read.
pub const STATUS_ARGS: [&str; 4] = ["status", "--porcelain=v2", "-z", "--untracked-files=no"];

/// The conflicted files of the repository at `dir`.
pub fn list(dir: &Path, cmds: &mut Vec<String>) -> Result<Vec<ConflictEntry>, String> {
    git(dir, &STATUS_ARGS, cmds).map(|out| parse_status(out.as_bytes()))
}

/// The conflicted files and the operation in progress.
#[derive(Clone, Debug, Default)]
pub struct Overview {
    pub files: Vec<ConflictEntry>,
    pub op: labels::Operation,
}

/// [`list`] plus [`labels::operation`].
pub fn overview(dir: &Path, cmds: &mut Vec<String>) -> Result<Overview, String> {
    let files = list(dir, cmds)?;
    let op = labels::operation(dir, cmds)?;
    Ok(Overview { files, op })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_records() {
        let out = b"1 .M N... 100644 100644 100644 aaaa bbbb plain.txt\0u UU N... 100644 100644 100644 100644 1111111111 2222222222 3333333333 src/a b.rs\0u UD N... 100644 100644 000000 100644 1111111111 2222222222 0000000000000000000000000000000000000000 gone.txt\0u DU N... 100644 000000 100644 100644 1111111111 0000000000000000000000000000000000000000 3333333333 mod.txt\0u AA N... 000000 100644 100644 100644 0000000000000000000000000000000000000000 2222222222 3333333333 new.txt\0u DD N... 100644 000000 000000 000000 1111111111 0000000000000000000000000000000000000000 0000000000000000000000000000000000000000 old.txt\0";
        let v = parse_status(out);
        let kinds: Vec<(&str, &str)> = v.iter().map(|e| (e.path.as_str(), e.kind.code())).collect();
        assert_eq!(
            kinds,
            vec![
                ("gone.txt", "UD"),
                ("mod.txt", "DU"),
                ("new.txt", "AA"),
                ("old.txt", "DD"),
                ("src/a b.rs", "UU"),
            ]
        );
        let uu = &v[4];
        assert_eq!(uu.base().unwrap().oid, "1111111111");
        assert_eq!(uu.ours().unwrap().oid, "2222222222");
        assert_eq!(uu.theirs().unwrap().oid, "3333333333");
        assert!(v[0].theirs().is_none());
        assert!(v[1].ours().is_none());
        assert!(v[2].base().is_none());
        assert_eq!(v[3].worktree_mode, "000000");
    }

    #[test]
    fn kinds() {
        for code in ["UU", "AA", "DU", "UD", "AU", "UA", "DD"] {
            let k = ConflictKind::from_xy(code).unwrap();
            assert_eq!(k.code(), code);
            assert!(!k.label().is_empty() && !k.explain().is_empty());
        }
        assert_eq!(ConflictKind::from_xy("M."), None);
        assert_eq!(
            ConflictKind::from_stages(true, true, true),
            Some(ConflictKind::BothModified)
        );
        assert_eq!(
            ConflictKind::from_stages(true, false, true),
            Some(ConflictKind::DeletedByUs)
        );
        assert_eq!(
            ConflictKind::from_stages(false, false, true),
            Some(ConflictKind::AddedByThem)
        );
        assert_eq!(ConflictKind::from_stages(false, false, false), None);
        assert!(ConflictKind::BothAdded.two_sided());
        assert!(!ConflictKind::DeletedByThem.two_sided());
    }
}
