//! More of git: stashes (list, show, apply / pop / drop / branch, stash
//! selected files), submodules (status, init, update, sync, add, remove),
//! Git LFS (tracked patterns, files, status, locks), patches (format-patch,
//! apply, am), hooks (list, edit, enable / disable, the gitvis secret check
//! as a pre-commit hook, pre-commit and semgrep), and splitting a commit.
//!
//! Every function that runs git pushes the command lines it ran into a
//! `cmds` list, which the app shows in the activity log. Writes are argument
//! builders with unit tests; the runners only call git with them.

pub mod githooks;
pub mod lfs;
pub mod patches;
pub mod precommit;
pub mod secret_hook;
pub mod split;
pub mod stash;
pub mod submodules;

use std::path::{Path, PathBuf};

pub use crate::devops::workbench::{git, git_line, git_probe, git_raw, output_text};

/// Run git and turn a failure (or a non-zero exit) into its output text.
pub fn git_ok<S: AsRef<str>>(
    dir: &Path,
    args: &[S],
    cmds: &mut Vec<String>,
) -> Result<String, String> {
    let out = git_raw(dir, args, &[], cmds)?;
    let text = output_text(&out);
    if out.success {
        Ok(text)
    } else if text.is_empty() {
        Err(format!(
            "git {} exited with {}",
            args.first().map(AsRef::as_ref).unwrap_or(""),
            out.code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "a signal".into())
        ))
    } else {
        Err(text)
    }
}

/// A path git printed relative to `dir` (or absolute), made absolute.
pub fn resolve_in(dir: &Path, printed: &str) -> PathBuf {
    let p = PathBuf::from(printed.trim());
    if p.is_absolute() { p } else { dir.join(p) }
}

/// Undo git's C-style quoting of a path (`"caf\303\251 \"x\".txt"`).
/// Unquoted text is returned as it is.
pub fn unquote(s: &str) -> String {
    let s = s.trim_end_matches(['\r', '\n']);
    let Some(inner) = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) else {
        return s.to_owned();
    };
    let bytes = inner.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b != b'\\' || i + 1 >= bytes.len() {
            out.push(b);
            i += 1;
            continue;
        }
        let n = bytes[i + 1];
        let octal = &bytes[i + 1..(i + 4).min(bytes.len())];
        if octal.len() == 3 && octal.iter().all(|d| (b'0'..=b'7').contains(d)) {
            let v = octal
                .iter()
                .fold(0u32, |acc, d| acc * 8 + (d - b'0') as u32);
            out.push(v as u8);
            i += 4;
            continue;
        }
        out.push(match n {
            b'n' => b'\n',
            b't' => b'\t',
            b'r' => b'\r',
            b'a' => 7,
            b'b' => 8,
            b'f' => 12,
            b'v' => 11,
            other => other,
        });
        i += 2;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What happened to a file in a patch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl FileStatus {
    pub fn label(self) -> &'static str {
        match self {
            FileStatus::Added => "Added",
            FileStatus::Modified => "Modified",
            FileStatus::Deleted => "Deleted",
            FileStatus::Renamed => "Renamed",
        }
    }
}

/// One file of a multi-file patch, with its raw text (so it can be parsed
/// into hunks by `crate::diff::parse`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchFile {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub added: u64,
    pub deleted: u64,
    pub binary: bool,
    pub raw: Vec<u8>,
}

/// Split `git diff` / `git stash show -p` output (made with `a/` and `b/`
/// prefixes) into one chunk per file, reading the paths from the `---` /
/// `+++` lines (or `rename to`, or the `diff --git` line for binaries).
pub fn split_patch(bytes: &[u8]) -> Vec<PatchFile> {
    let mut out: Vec<PatchFile> = Vec::new();
    // The file being read, and whether its hunks have started.
    let mut cur: Option<(PatchFile, bool)> = None;
    for line in bytes.split_inclusive(|&b| b == b'\n') {
        if line.starts_with(b"diff --git ") {
            if let Some((f, _)) = cur.take() {
                out.push(f);
            }
            let text = String::from_utf8_lossy(line);
            let (a, b) = diff_git_paths(text.trim_end());
            let renamed = a != b;
            cur = Some((
                PatchFile {
                    path: b,
                    old_path: renamed.then_some(a),
                    status: if renamed {
                        FileStatus::Renamed
                    } else {
                        FileStatus::Modified
                    },
                    added: 0,
                    deleted: 0,
                    binary: false,
                    raw: line.to_vec(),
                },
                false,
            ));
            continue;
        }
        let Some((f, in_hunks)) = cur.as_mut() else {
            continue;
        };
        f.raw.extend_from_slice(line);
        if line.starts_with(b"@@") {
            *in_hunks = true;
            continue;
        }
        if *in_hunks {
            match line.first() {
                Some(b'+') => f.added += 1,
                Some(b'-') => f.deleted += 1,
                _ => {}
            }
            continue;
        }
        let text = String::from_utf8_lossy(line);
        let t = text.trim_end_matches(['\r', '\n']);
        if t.starts_with("new file mode") {
            f.status = FileStatus::Added;
        } else if t.starts_with("deleted file mode") {
            f.status = FileStatus::Deleted;
        } else if let Some(p) = t.strip_prefix("rename from ") {
            f.old_path = Some(unquote(p));
            f.status = FileStatus::Renamed;
        } else if let Some(p) = t.strip_prefix("rename to ") {
            f.path = unquote(p);
            f.status = FileStatus::Renamed;
        } else if let Some(p) = t.strip_prefix("--- ") {
            // Names with spaces get a trailing tab.
            let p = unquote(p.trim_end_matches('\t'));
            if f.status != FileStatus::Renamed
                && let Some(rest) = p.strip_prefix("a/")
            {
                f.path = rest.to_owned();
            }
        } else if let Some(p) = t.strip_prefix("+++ ") {
            let p = unquote(p.trim_end_matches('\t'));
            if let Some(rest) = p.strip_prefix("b/") {
                f.path = rest.to_owned();
            }
        } else if t.starts_with("Binary files ") || t.starts_with("GIT binary patch") {
            f.binary = true;
        }
    }
    if let Some((f, _)) = cur.take() {
        out.push(f);
    }
    out
}

/// `a/x b/y` of a `diff --git` line. Ambiguous with spaces in names; the
/// `---` / `+++` lines that follow correct it for text files.
fn diff_git_paths(line: &str) -> (String, String) {
    let rest = line.trim_start_matches("diff --git ");
    if rest.starts_with('"') {
        // `"a/x y" "b/x y"`
        if let Some(i) = rest.find("\" \"") {
            let a = unquote(&rest[..=i]);
            let b = unquote(&rest[i + 2..]);
            return (strip_ab(&a, "a/"), strip_ab(&b, "b/"));
        }
    }
    // Same path on both sides: `a/P b/P` splits in the middle.
    let n = rest.len();
    if n > 4 && (n - 1).is_multiple_of(2) && rest.is_char_boundary((n - 1) / 2) {
        let half = (n - 1) / 2;
        let (a, b) = (&rest[..half], &rest[half + 1..]);
        if a.strip_prefix("a/").is_some() && a.strip_prefix("a/") == b.strip_prefix("b/") {
            return (strip_ab(a, "a/"), strip_ab(b, "b/"));
        }
    }
    match rest.find(" b/") {
        Some(i) => (strip_ab(&rest[..i], "a/"), strip_ab(&rest[i + 1..], "b/")),
        None => (rest.to_owned(), rest.to_owned()),
    }
}

fn strip_ab(s: &str, prefix: &str) -> String {
    s.strip_prefix(prefix).unwrap_or(s).to_owned()
}

/// `git --numstat` lines: `added<TAB>deleted<TAB>path` (`-` for binary).
/// Renames come as `old => new` or `dir/{old => new}/x`; the new path is kept.
pub fn parse_numstat(text: &str) -> Vec<(String, Option<u64>, Option<u64>)> {
    let mut out = Vec::new();
    for l in text.lines() {
        let mut p = l.splitn(3, '\t');
        let (Some(a), Some(d), Some(path)) = (p.next(), p.next(), p.next()) else {
            continue;
        };
        let path = rename_target(&unquote(path));
        if path.is_empty() {
            continue;
        }
        out.push((path, a.parse().ok(), d.parse().ok()));
    }
    out
}

/// The new path of a numstat rename: `a => b` → `b`, `src/{a => b}/x` →
/// `src/b/x`.
pub fn rename_target(path: &str) -> String {
    if let (Some(open), Some(close)) = (path.find('{'), path.rfind('}'))
        && open < close
        && let Some(arrow) = path[open..close].find(" => ")
    {
        let new = &path[open + arrow + 4..close];
        let mut s = format!("{}{}{}", &path[..open], new, &path[close + 1..]);
        while s.contains("//") {
            s = s.replace("//", "/");
        }
        return s.trim_start_matches('/').to_owned();
    }
    match path.find(" => ") {
        Some(i) => path[i + 4..].to_owned(),
        None => path.to_owned(),
    }
}

/// Shorten a sha for display.
pub fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unquotes_git_paths() {
        assert_eq!(unquote("plain.txt"), "plain.txt");
        assert_eq!(unquote("\"a b.txt\""), "a b.txt");
        assert_eq!(unquote("\"caf\\303\\251.txt\""), "café.txt");
        assert_eq!(unquote("\"say \\\"hi\\\".md\""), "say \"hi\".md");
        assert_eq!(unquote("\"tab\\there\""), "tab\there");
    }

    #[test]
    fn numstat_with_renames_and_binaries() {
        let text = "3\t1\tsrc/a.rs\n-\t-\timg.png\n0\t0\told.txt => new.txt\n2\t0\tsrc/{x => y}/m.rs\n1\t1\t\"sp ace.txt\"\n";
        let v = parse_numstat(text);
        assert_eq!(v[0], ("src/a.rs".into(), Some(3), Some(1)));
        assert_eq!(v[1], ("img.png".into(), None, None));
        assert_eq!(v[2].0, "new.txt");
        assert_eq!(v[3].0, "src/y/m.rs");
        assert_eq!(v[4].0, "sp ace.txt");
        assert_eq!(rename_target("{a => }/x"), "x");
    }

    #[test]
    fn splits_a_multi_file_patch() {
        let patch = b"diff --git a/a.txt b/a.txt\n\
index 1..2 100644\n\
--- a/a.txt\n\
+++ b/a.txt\n\
@@ -1 +1,2 @@\n\
 a\n\
+b\n\
diff --git a/new file.txt b/new file.txt\n\
new file mode 100644\n\
index 0000000..3\n\
--- /dev/null\n\
+++ b/new file.txt\t\n\
@@ -0,0 +1 @@\n\
+n\n\
diff --git a/old.txt b/renamed.txt\n\
similarity index 100%\n\
rename from old.txt\n\
rename to renamed.txt\n\
diff --git a/gone.txt b/gone.txt\n\
deleted file mode 100644\n\
--- a/gone.txt\n\
+++ /dev/null\n\
@@ -1,2 +0,0 @@\n\
-x\n\
-y\n\
diff --git a/img.png b/img.png\n\
Binary files a/img.png and b/img.png differ\n";
        let files = split_patch(patch);
        assert_eq!(files.len(), 5);
        assert_eq!(files[0].path, "a.txt");
        assert_eq!((files[0].added, files[0].deleted), (1, 0));
        assert_eq!(files[0].status, FileStatus::Modified);
        assert_eq!(files[1].path, "new file.txt");
        assert_eq!(files[1].status, FileStatus::Added);
        assert_eq!(files[2].path, "renamed.txt");
        assert_eq!(files[2].old_path.as_deref(), Some("old.txt"));
        assert_eq!(files[2].status, FileStatus::Renamed);
        assert_eq!(files[3].path, "gone.txt");
        assert_eq!(files[3].status, FileStatus::Deleted);
        assert_eq!((files[3].added, files[3].deleted), (0, 2));
        assert!(files[4].binary);
        assert_eq!(files[4].path, "img.png");
        // Each chunk parses on its own.
        let d = crate::diff::parse(&files[0].raw);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].hunks.len(), 1);
    }

    #[test]
    fn diff_git_line_paths() {
        assert_eq!(
            diff_git_paths("diff --git a/x y.txt b/x y.txt"),
            ("x y.txt".into(), "x y.txt".into())
        );
        assert_eq!(
            diff_git_paths("diff --git a/old b/new"),
            ("old".into(), "new".into())
        );
        assert_eq!(
            diff_git_paths("diff --git \"a/caf\\303\\251\" \"b/caf\\303\\251\""),
            ("café".into(), "café".into())
        );
    }
}
