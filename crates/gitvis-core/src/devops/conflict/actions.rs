//! The changes the editor makes, as exact git command lines (shown in the
//! confirmation and the activity log before they run), and writing the
//! result file.

use std::path::Path;

use super::ConflictKind;
use crate::devops::workbench::{git_line, git_raw, output_text};

/// A whole-file choice for conflicts without text hunks (binary,
/// delete / modify, one-sided adds, both deleted).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Choice {
    Ours,
    Theirs,
    Delete,
}

impl Choice {
    pub fn from_name(s: &str) -> Option<Choice> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "ours" | "keep" | "keep-ours" => Choice::Ours,
            "theirs" | "take-theirs" => Choice::Theirs,
            "delete" | "rm" | "remove" => Choice::Delete,
            _ => return None,
        })
    }
}

/// One option on the choice card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChoiceSpec {
    pub choice: Choice,
    pub title: String,
    /// What it means, in a sentence.
    pub explain: String,
    /// git arguments, one command per entry.
    pub commands: Vec<Vec<String>>,
    /// Loses a version (shown in the danger colour).
    pub danger: bool,
}

fn args(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn keep(side: &str, path: &str) -> Vec<Vec<String>> {
    vec![
        args(&["checkout", side, "--", path]),
        args(&["add", "--", path]),
    ]
}

fn remove(path: &str) -> Vec<Vec<String>> {
    vec![args(&["rm", "--quiet", "--", path])]
}

/// The options for a file, with the names of the two sides (`main`,
/// `feature`) for the explanations.
pub fn choice_specs(kind: ConflictKind, path: &str, ours: &str, theirs: &str) -> Vec<ChoiceSpec> {
    let spec = |choice, title: &str, explain: String, commands, danger| ChoiceSpec {
        choice,
        title: title.to_owned(),
        explain,
        commands,
        danger,
    };
    match kind {
        ConflictKind::BothModified | ConflictKind::BothAdded => vec![
            spec(
                Choice::Ours,
                "Keep ours",
                format!("Use the file as it is on {ours}; their changes to it are dropped."),
                keep("--ours", path),
                false,
            ),
            spec(
                Choice::Theirs,
                "Take theirs",
                format!("Use the file from {theirs}; our changes to it are dropped."),
                keep("--theirs", path),
                false,
            ),
            spec(
                Choice::Delete,
                "Delete the file",
                "Remove it from the working tree and the index; neither version is kept.".into(),
                remove(path),
                true,
            ),
        ],
        ConflictKind::DeletedByThem => vec![
            spec(
                Choice::Ours,
                "Keep the file (ours)",
                format!("Keep it with the changes from {ours}; their deletion is dropped."),
                keep("--ours", path),
                false,
            ),
            spec(
                Choice::Theirs,
                "Delete it (theirs)",
                format!("Accept the deletion from {theirs}; the changes from {ours} are dropped."),
                remove(path),
                true,
            ),
        ],
        ConflictKind::DeletedByUs => vec![
            spec(
                Choice::Ours,
                "Keep it deleted (ours)",
                format!("Keep the deletion from {ours}; the changes from {theirs} are dropped."),
                remove(path),
                true,
            ),
            spec(
                Choice::Theirs,
                "Restore it (theirs)",
                format!("Bring the file back with the changes from {theirs}."),
                keep("--theirs", path),
                false,
            ),
        ],
        ConflictKind::AddedByUs => vec![
            spec(
                Choice::Ours,
                "Keep it (ours)",
                format!("Keep the file as {ours} has it."),
                keep("--ours", path),
                false,
            ),
            spec(
                Choice::Delete,
                "Delete it",
                "Leave this path out of the result.".into(),
                remove(path),
                true,
            ),
        ],
        ConflictKind::AddedByThem => vec![
            spec(
                Choice::Theirs,
                "Keep it (theirs)",
                format!("Keep the file as {theirs} has it."),
                keep("--theirs", path),
                false,
            ),
            spec(
                Choice::Delete,
                "Delete it",
                "Leave this path out of the result.".into(),
                remove(path),
                true,
            ),
        ],
        ConflictKind::BothDeleted => vec![spec(
            Choice::Delete,
            "Delete it (both sides did)",
            "Record the deletion both sides agree on.".into(),
            remove(path),
            false,
        )],
    }
}

/// `git add -- <path>`: record the file on disk as the resolution.
pub fn mark_resolved(path: &str) -> Vec<Vec<String>> {
    vec![args(&["add", "--", path])]
}

/// Conflict styles for [`recreate`].
pub const STYLES: [&str; 3] = ["merge", "diff3", "zdiff3"];

/// `git checkout --conflict=<style> -- <path>`: write the conflict markers
/// again from the index stages (discards edits to the file).
pub fn recreate(path: &str, style: &str) -> Vec<Vec<String>> {
    vec![vec![
        "checkout".into(),
        format!("--conflict={style}"),
        "--".into(),
        path.into(),
    ]]
}

/// The command lines as typed.
pub fn render(commands: &[Vec<String>]) -> Vec<String> {
    commands.iter().map(|c| git_line(c)).collect()
}

/// Run commands in order; stops at the first failure.
pub fn run(dir: &Path, commands: &[Vec<String>], cmds: &mut Vec<String>) -> Result<(), String> {
    for c in commands {
        let out = git_raw(dir, c, &[], cmds)?;
        if !out.success {
            let text = output_text(&out);
            return Err(if text.is_empty() {
                format!("{} exited with {}", git_line(c), out.code.unwrap_or(-1))
            } else {
                text
            });
        }
    }
    Ok(())
}

/// The log line of a write.
pub fn write_line(path: &str, bytes: usize) -> String {
    format!("write {path} ({})", crate::devops::plural_bytes(bytes))
}

/// Write the result file (contents exactly as given; permissions kept).
pub fn write(dir: &Path, path: &str, bytes: &[u8], cmds: &mut Vec<String>) -> Result<(), String> {
    cmds.push(write_line(path, bytes.len()));
    let full = dir.join(path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Could not write {path}: {e}"))?;
    }
    std::fs::write(&full, bytes).map_err(|e| format!("Could not write {path}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(kind: ConflictKind, c: Choice) -> Vec<String> {
        let s = choice_specs(kind, "dir/a b.png", "main", "feature");
        render(&s.iter().find(|s| s.choice == c).unwrap().commands)
    }

    #[test]
    fn choices_per_kind() {
        assert_eq!(
            lines(ConflictKind::BothModified, Choice::Ours),
            vec![
                "git checkout --ours -- \"dir/a b.png\"",
                "git add -- \"dir/a b.png\""
            ]
        );
        assert_eq!(
            lines(ConflictKind::BothAdded, Choice::Theirs),
            vec![
                "git checkout --theirs -- \"dir/a b.png\"",
                "git add -- \"dir/a b.png\""
            ]
        );
        assert_eq!(
            lines(ConflictKind::BothModified, Choice::Delete),
            vec!["git rm --quiet -- \"dir/a b.png\""]
        );
        // Deleted by them: keeping ours restores it, theirs deletes.
        assert_eq!(
            lines(ConflictKind::DeletedByThem, Choice::Theirs),
            vec!["git rm --quiet -- \"dir/a b.png\""]
        );
        assert_eq!(lines(ConflictKind::DeletedByThem, Choice::Ours).len(), 2);
        // Deleted by us: ours deletes, theirs restores.
        assert_eq!(
            lines(ConflictKind::DeletedByUs, Choice::Ours),
            vec!["git rm --quiet -- \"dir/a b.png\""]
        );
        assert_eq!(
            lines(ConflictKind::DeletedByUs, Choice::Theirs)[0],
            "git checkout --theirs -- \"dir/a b.png\""
        );
        let dd = choice_specs(ConflictKind::BothDeleted, "x", "a", "b");
        assert_eq!(dd.len(), 1);
        assert_eq!(dd[0].choice, Choice::Delete);
        let au = choice_specs(ConflictKind::AddedByUs, "x", "a", "b");
        assert_eq!(
            au.iter().map(|s| s.choice).collect::<Vec<_>>(),
            vec![Choice::Ours, Choice::Delete]
        );
        let ua = choice_specs(ConflictKind::AddedByThem, "x", "a", "b");
        assert_eq!(
            ua.iter().map(|s| s.choice).collect::<Vec<_>>(),
            vec![Choice::Theirs, Choice::Delete]
        );
        for k in [
            ConflictKind::BothModified,
            ConflictKind::DeletedByThem,
            ConflictKind::DeletedByUs,
        ] {
            for s in choice_specs(k, "x", "main", "feature") {
                assert!(!s.explain.is_empty());
                assert_eq!(s.danger, s.commands[0][0] == "rm");
            }
        }
        assert!(
            choice_specs(ConflictKind::DeletedByThem, "x", "main", "feature")[0]
                .explain
                .contains("main")
        );
    }

    #[test]
    fn other_commands() {
        assert_eq!(render(&mark_resolved("a.txt")), vec!["git add -- a.txt"]);
        assert_eq!(
            render(&recreate("a.txt", "zdiff3")),
            vec!["git checkout --conflict=zdiff3 -- a.txt"]
        );
        assert_eq!(write_line("a.txt", 2048), "write a.txt (2.0 KB)");
        assert_eq!(Choice::from_name("theirs"), Some(Choice::Theirs));
        assert_eq!(Choice::from_name("delete"), Some(Choice::Delete));
        assert_eq!(Choice::from_name("?"), None);
    }

    #[test]
    fn writes_bytes_exactly() {
        let dir = std::env::temp_dir().join(format!(
            "gitvis-conflict-write-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cmds = Vec::new();
        write(&dir, "sub/x.txt", b"a\r\nb", &mut cmds).unwrap();
        assert_eq!(std::fs::read(dir.join("sub/x.txt")).unwrap(), b"a\r\nb");
        assert_eq!(cmds, vec!["write sub/x.txt (4 bytes)"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
