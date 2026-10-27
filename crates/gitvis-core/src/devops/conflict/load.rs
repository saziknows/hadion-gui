//! Everything the editor needs about one conflicted file: the working
//! file, the three stage blobs, the attributes that change how git merges
//! it, and the parsed conflict.
//!
//! Commands, in order (each recorded):
//! - `git check-attr -z conflict-marker-size merge binary -- <path>`
//! - `git cat-file --batch-check` then `git cat-file --batch` for the stage
//!   blobs up to [`MAX_TEXT`] bytes (sizes first, so a large binary is
//!   never read)
//! - for both-modified text: `git merge-file -p --object-id --zdiff3` and
//!   `--diff3` over the stage blobs, whose hunks settle ambiguous markers
//!   and give merge-style hunks their base.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::time::Instant;

use super::markers::{self, Doc, Hints};
use super::resolve::Tails;
use super::{ConflictEntry, ConflictKind};
use crate::cli;
use crate::devops::workbench::{git_line, git_raw};

/// Larger files are not loaded into the editor (a choice card instead).
pub const MAX_TEXT: usize = 8 * 1024 * 1024;
/// Bytes probed for a NUL, like git (`FIRST_FEW_BYTES`).
pub const BINARY_PROBE: usize = 8000;

/// git's binary test: a NUL in the first 8,000 bytes.
pub fn is_binary(b: &[u8]) -> bool {
    b[..b.len().min(BINARY_PROBE)].contains(&0)
}

/// A stable hash of file contents (change detection).
pub fn hash(b: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
}

/// One stage blob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blob {
    pub oid: String,
    pub mode: String,
    pub size: usize,
    /// `None` when larger than [`MAX_TEXT`] (not read).
    pub bytes: Option<Vec<u8>>,
    pub binary: bool,
    pub final_newline: bool,
}

impl Blob {
    pub fn is_symlink(&self) -> bool {
        self.mode == "120000"
    }
    pub fn is_submodule(&self) -> bool {
        self.mode == "160000"
    }
}

/// Sizes from `git cat-file --batch-check` (`<oid> blob <size>` or
/// `<oid> missing`), in input order.
pub fn parse_batch_check(out: &str) -> Vec<Option<usize>> {
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let mut it = l.split_whitespace();
            let _oid = it.next();
            match (it.next(), it.next()) {
                (Some(_kind), Some(size)) => size.parse().ok(),
                _ => None,
            }
        })
        .collect()
}

/// Contents from `git cat-file --batch`, in input order.
pub fn parse_batch(out: &[u8]) -> Vec<Option<Vec<u8>>> {
    let mut v = Vec::new();
    let mut i = 0;
    while i < out.len() {
        let Some(nl) = out[i..].iter().position(|&b| b == b'\n') else {
            break;
        };
        let header = String::from_utf8_lossy(&out[i..i + nl]).into_owned();
        i += nl + 1;
        let parts: Vec<&str> = header.split_whitespace().collect();
        match parts.as_slice() {
            [_, _, size] => {
                let Ok(size) = size.parse::<usize>() else {
                    break;
                };
                let end = (i + size).min(out.len());
                v.push(Some(out[i..end].to_vec()));
                // Content, then a newline.
                i = end + 1;
            }
            _ => v.push(None),
        }
    }
    v
}

/// Read stage blobs: sizes for all, contents for those up to
/// [`MAX_TEXT`].
pub fn read_blobs(
    dir: &Path,
    stages: &[Option<super::StageRef>; 3],
    cmds: &mut Vec<String>,
) -> Result<[Option<Blob>; 3], String> {
    let refs: Vec<&super::StageRef> = stages.iter().flatten().collect();
    if refs.is_empty() {
        return Ok([None, None, None]);
    }
    let input: String = refs.iter().map(|s| format!("{}\n", s.oid)).collect();
    cmds.push(format!(
        "{} < ({} object ids)",
        git_line(&["cat-file", "--batch-check"]),
        refs.len()
    ));
    let out = cli::run(dir, &["cat-file", "--batch-check"], Some(input.as_bytes()))
        .map_err(|e| e.to_string())?;
    let sizes = parse_batch_check(&String::from_utf8_lossy(&out.stdout));
    let small: Vec<&str> = refs
        .iter()
        .zip(&sizes)
        .filter(|(_, s)| s.is_some_and(|s| s <= MAX_TEXT))
        .map(|(r, _)| r.oid.as_str())
        .collect();
    let mut contents = std::collections::HashMap::new();
    if !small.is_empty() {
        let input: String = small.iter().map(|o| format!("{o}\n")).collect();
        cmds.push(format!(
            "{} < ({} object ids)",
            git_line(&["cat-file", "--batch"]),
            small.len()
        ));
        let out = cli::run(dir, &["cat-file", "--batch"], Some(input.as_bytes()))
            .map_err(|e| e.to_string())?;
        for (oid, bytes) in small.iter().zip(parse_batch(&out.stdout)) {
            if let Some(b) = bytes {
                contents.insert(oid.to_string(), b);
            }
        }
    }
    let mut result: [Option<Blob>; 3] = [None, None, None];
    let mut k = 0;
    for (slot, stage) in stages.iter().enumerate() {
        let Some(s) = stage else { continue };
        let size = sizes.get(k).copied().flatten();
        k += 1;
        let Some(size) = size else { continue };
        // Two stages can be the same blob.
        let bytes = contents.get(&s.oid).cloned();
        result[slot] = Some(Blob {
            oid: s.oid.clone(),
            mode: s.mode.clone(),
            size,
            binary: bytes.as_deref().map(is_binary).unwrap_or(true),
            final_newline: bytes
                .as_deref()
                .map(|b| b.is_empty() || b.ends_with(b"\n"))
                .unwrap_or(true),
            bytes,
        });
    }
    Ok(result)
}

/// The attributes that change how git merges a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attrs {
    pub marker_size: usize,
    /// `merge` unset (`-merge`) or `binary`: git did not merge the text.
    pub no_text_merge: bool,
    /// A custom merge driver (`merge=name`).
    pub driver: Option<String>,
}

impl Default for Attrs {
    fn default() -> Self {
        Self {
            marker_size: markers::DEFAULT_MARKER_SIZE,
            no_text_merge: false,
            driver: None,
        }
    }
}

/// Parse `git check-attr -z` output (`path NUL attr NUL value NUL`).
pub fn parse_check_attr(out: &[u8]) -> Attrs {
    let mut a = Attrs::default();
    let fields: Vec<String> = out
        .split(|&b| b == 0)
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect();
    for t in fields.chunks(3) {
        let [_, attr, value] = t else { continue };
        match (attr.as_str(), value.as_str()) {
            ("conflict-marker-size", v) => {
                if let Ok(n) = v.parse::<usize>()
                    && (1..=200).contains(&n)
                {
                    a.marker_size = n;
                }
            }
            ("merge", "unset") | ("binary", "set") => a.no_text_merge = true,
            ("merge", "binary") => a.no_text_merge = true,
            ("merge", "set" | "unspecified" | "text") => {}
            ("merge", d) => a.driver = Some(d.to_owned()),
            _ => {}
        }
    }
    a
}

/// The working tree file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Working {
    Text(Vec<u8>),
    Binary { size: usize },
    TooLarge { size: usize },
    Missing,
}

/// How the editor shows a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presentation {
    /// Ours | result | theirs with hunks.
    Text,
    /// Binary content on either side: a choice card.
    Binary,
    /// One side deleted, renamed or never had it: a choice card.
    OneSided,
    /// Too large to load.
    TooLarge,
    /// Text that git did not merge (`-merge`, `binary`, a merge driver):
    /// the working file has no markers.
    NotMerged,
    /// A symbolic link or a submodule.
    Special,
}

impl Presentation {
    pub fn is_text(self) -> bool {
        self == Presentation::Text
    }
}

/// One conflicted file, ready for the editor.
#[derive(Clone, Debug)]
pub struct FileLoad {
    pub entry: ConflictEntry,
    /// `[base, ours, theirs]`.
    pub stages: [Option<Blob>; 3],
    pub working: Working,
    pub attrs: Attrs,
    pub presentation: Presentation,
    /// Parsed markers (text only), bases filled in from the stages.
    pub doc: Option<Doc>,
    /// Conflicts `git merge-file --diff3` finds in the stage blobs
    /// (both modified only).
    pub stage_conflicts: Option<usize>,
    pub tails: Tails,
    /// Hash of the working file (0 when missing).
    pub hash: u64,
    /// The working file is valid UTF-8 (otherwise it cannot be edited as
    /// text, only resolved by choosing sides).
    pub utf8: bool,
    pub elapsed_ms: u64,
}

impl FileLoad {
    pub fn path(&self) -> &str {
        &self.entry.path
    }

    pub fn working_bytes(&self) -> Option<&[u8]> {
        match &self.working {
            Working::Text(b) => Some(b),
            _ => None,
        }
    }
}

/// `git merge-file` arguments that print the merge of the three stage
/// blobs in `style` (`--diff3` or `--zdiff3`).
pub fn merge_file_args(
    ours: &str,
    base: &str,
    theirs: &str,
    style: &str,
    size: usize,
) -> Vec<String> {
    vec![
        "merge-file".into(),
        "-p".into(),
        "--object-id".into(),
        style.into(),
        format!("--marker-size={size}"),
        "-L".into(),
        "ours".into(),
        "-L".into(),
        "base".into(),
        "-L".into(),
        "theirs".into(),
        ours.into(),
        base.into(),
        theirs.into(),
    ]
}

/// Marker size asked of `git merge-file` for the hints: longer than any
/// the file uses, so marker-like content lines cannot confuse the parse.
pub fn hint_marker_size(size: usize) -> usize {
    size.max(markers::DEFAULT_MARKER_SIZE) + 25
}

/// The stage blobs merged in diff3 style (both modified only).
pub fn hints(dir: &Path, entry: &ConflictEntry, size: usize, cmds: &mut Vec<String>) -> Hints {
    let (Some(o), Some(b), Some(t)) = (entry.ours(), entry.base(), entry.theirs()) else {
        return Hints::default();
    };
    let size = hint_marker_size(size);
    let args = merge_file_args(&o.oid, &b.oid, &t.oid, "--diff3", size);
    // The exit code is the number of conflicts; only a failure to run (or
    // a status above 127) is an error.
    match git_raw(dir, &args, &[], cmds) {
        Ok(r) if r.code.is_some_and(|c| (0..=127).contains(&c)) => {
            Hints::from_diff3(&r.stdout, size)
        }
        _ => Hints::default(),
    }
}

/// Read one conflicted file.
pub fn load_file(
    dir: &Path,
    entry: &ConflictEntry,
    cmds: &mut Vec<String>,
) -> Result<FileLoad, String> {
    let started = Instant::now();
    let path = entry.path.as_str();
    let attrs = git_raw(
        dir,
        &[
            "check-attr",
            "-z",
            "conflict-marker-size",
            "merge",
            "binary",
            "--",
            path,
        ],
        &[],
        cmds,
    )
    .ok()
    .filter(|r| r.success)
    .map(|r| parse_check_attr(&r.stdout))
    .unwrap_or_default();
    let stages = read_blobs(dir, &entry.stages, cmds)?;
    let full = dir.join(path);
    cmds.push(format!("read {path}"));
    let working = match std::fs::metadata(&full) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Working::Missing,
        Err(e) => return Err(format!("Could not read {path}: {e}")),
        Ok(m) if m.is_dir() => Working::Missing,
        Ok(m) if m.len() as usize > MAX_TEXT => Working::TooLarge {
            size: m.len() as usize,
        },
        Ok(_) => {
            let b = std::fs::read(&full).map_err(|e| format!("Could not read {path}: {e}"))?;
            if is_binary(&b) {
                Working::Binary { size: b.len() }
            } else {
                Working::Text(b)
            }
        }
    };
    let any_binary = stages
        .iter()
        .flatten()
        .any(|b| b.binary && b.bytes.is_some());
    let any_large = stages.iter().flatten().any(|b| b.bytes.is_none());
    let special = stages
        .iter()
        .flatten()
        .any(|b| b.is_symlink() || b.is_submodule());
    let presentation = if special {
        Presentation::Special
    } else if !entry.kind.two_sided() {
        Presentation::OneSided
    } else if matches!(working, Working::TooLarge { .. }) || any_large {
        Presentation::TooLarge
    } else if matches!(working, Working::Binary { .. }) || any_binary {
        Presentation::Binary
    } else {
        Presentation::Text
    };
    let mut stage_conflicts = None;
    let mut doc = None;
    let mut presentation = presentation;
    if let (Presentation::Text, Working::Text(bytes)) = (presentation, &working) {
        let mut h = Hints::default();
        if entry.kind == ConflictKind::BothModified {
            h = hints(dir, entry, attrs.marker_size, cmds);
            stage_conflicts = (!h.is_empty()).then_some(h.conflicts);
        }
        let mut d = markers::parse_with_hints(bytes, attrs.marker_size, &h);
        d.fill_bases(&h);
        if entry.kind == ConflictKind::BothAdded {
            d.empty_bases();
        }
        if d.hunk_count() == 0 && (attrs.no_text_merge || attrs.driver.is_some()) {
            presentation = Presentation::NotMerged;
        }
        doc = Some(d);
    }
    let tails = Tails {
        base: stages[0].as_ref().map(|b| b.final_newline),
        ours: stages[1].as_ref().map(|b| b.final_newline),
        theirs: stages[2].as_ref().map(|b| b.final_newline),
    };
    let (hash_v, utf8) = match &working {
        Working::Text(b) => (hash(b), std::str::from_utf8(b).is_ok()),
        _ => (0, true),
    };
    Ok(FileLoad {
        entry: entry.clone(),
        stages,
        working,
        attrs,
        presentation,
        doc,
        stage_conflicts,
        tails,
        hash: hash_v,
        utf8,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_probe() {
        assert!(is_binary(b"\x00abc"));
        assert!(!is_binary(b"plain text\n"));
        let mut late = vec![b'a'; BINARY_PROBE];
        late.push(0);
        assert!(!is_binary(&late), "only the first 8,000 bytes count");
        assert_eq!(hash(b"x"), hash(b"x"));
        assert_ne!(hash(b"x"), hash(b"y"));
    }

    #[test]
    fn batch_output() {
        let check = "aaaa blob 12\nbbbb missing\ncccc blob 0\n";
        assert_eq!(parse_batch_check(check), vec![Some(12), None, Some(0)]);
        let batch = b"aaaa blob 3\nab\n\nbbbb missing\ncccc blob 0\n\ndddd blob 2\n\x00\x01\n";
        assert_eq!(
            parse_batch(batch),
            vec![
                Some(b"ab\n".to_vec()),
                None,
                Some(Vec::new()),
                Some(vec![0, 1])
            ]
        );
    }

    #[test]
    fn attributes() {
        let out = b"a.rst\0conflict-marker-size\012\0a.rst\0merge\0unspecified\0a.rst\0binary\0unspecified\0";
        let a = parse_check_attr(out);
        assert_eq!(a.marker_size, 12);
        assert!(!a.no_text_merge);
        let b = parse_check_attr(
            b"x\0conflict-marker-size\0unspecified\0x\0merge\0unset\0x\0binary\0unspecified\0",
        );
        assert_eq!(b.marker_size, 7);
        assert!(b.no_text_merge);
        let c = parse_check_attr(b"x\0merge\0union\0x\0binary\0unspecified\0");
        assert_eq!(c.driver.as_deref(), Some("union"));
        let d = parse_check_attr(b"x\0binary\0set\0");
        assert!(d.no_text_merge);
        assert_eq!(parse_check_attr(b""), Attrs::default());
    }

    #[test]
    fn merge_file_command() {
        let a = merge_file_args("o", "b", "t", "--diff3", 9);
        assert_eq!(
            git_line(&a),
            "git merge-file -p --object-id --diff3 --marker-size=9 -L ours -L base -L theirs o b t"
        );
    }
}
