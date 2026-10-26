use std::path::Path;

use crate::diff::{self, FileDiff};
use crate::model::Oid;
use crate::{Error, Result, cli};

/// Untracked files larger than this are not previewed.
const MAX_FILE_PREVIEW: u64 = 4 * 1024 * 1024;
/// Diffs larger than this are not parsed.
const MAX_DIFF_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffTarget {
    Commit {
        id: Oid,
        parent: Option<Oid>,
        path: String,
        old_path: Option<String>,
    },
    Unstaged {
        path: String,
    },
    Staged {
        path: String,
        old_path: Option<String>,
    },
    Untracked {
        path: String,
    },
    Conflicted {
        path: String,
    },
}

impl DiffTarget {
    pub fn path(&self) -> &str {
        match self {
            DiffTarget::Commit { path, .. }
            | DiffTarget::Unstaged { path }
            | DiffTarget::Staged { path, .. }
            | DiffTarget::Untracked { path }
            | DiffTarget::Conflicted { path } => path,
        }
    }

    pub fn old_path(&self) -> Option<&str> {
        match self {
            DiffTarget::Commit { old_path, .. } | DiffTarget::Staged { old_path, .. } => {
                old_path.as_deref()
            }
            _ => None,
        }
    }
}

/// How a diff is produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffOptions {
    /// `-w`: hunks then cannot be applied, only read.
    pub ignore_whitespace: bool,
    /// Context lines around each change (`-U`).
    pub context: u32,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            ignore_whitespace: false,
            context: 3,
        }
    }
}

/// The git command that produces the diff, or `None` when the target is an
/// untracked file (read from disk instead).
pub fn diff_args(target: &DiffTarget, opts: &DiffOptions) -> Option<Vec<String>> {
    // Fixed prefixes and no external tools, whatever the user's config says,
    // so the output can be parsed and fed back to `git apply`.
    let mut args: Vec<String> = ["-c", "core.quotePath=false"].map(String::from).to_vec();
    let context = format!("-U{}", opts.context.min(50));
    let mut flags = vec![
        "--no-color",
        "--no-ext-diff",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "--no-relative",
        context.as_str(),
    ];
    if opts.ignore_whitespace {
        flags.push("-w");
    }
    let flags: Vec<String> = flags.into_iter().map(String::from).collect();
    let paths = |args: &mut Vec<String>, path: &str, old: Option<&str>| {
        args.push("--".into());
        if let Some(old) = old.filter(|o| *o != path) {
            args.push(old.into());
        }
        args.push(path.into());
    };

    match target {
        DiffTarget::Untracked { .. } => return None,
        DiffTarget::Commit {
            id,
            parent: None,
            path,
            old_path,
        } => {
            args.extend(["show", "--format="].map(String::from));
            args.extend(flags.iter().cloned());
            args.push(id.to_string());
            paths(&mut args, path, old_path.as_deref());
        }
        DiffTarget::Commit {
            id,
            parent: Some(parent),
            path,
            old_path,
        } => {
            args.push("diff".into());
            args.extend(flags.iter().cloned());
            args.extend(["-M".into(), parent.to_string(), id.to_string()]);
            paths(&mut args, path, old_path.as_deref());
        }
        DiffTarget::Unstaged { path } => {
            args.push("diff".into());
            args.extend(flags.iter().cloned());
            args.push("--no-textconv".into());
            paths(&mut args, path, None);
        }
        DiffTarget::Staged { path, old_path } => {
            args.extend(["diff", "--cached", "-M", "--no-textconv"].map(String::from));
            args.extend(flags.iter().cloned());
            paths(&mut args, path, old_path.as_deref());
        }
        DiffTarget::Conflicted { path } => {
            args.push("diff".into());
            args.extend(flags.iter().cloned());
            paths(&mut args, path, None);
        }
    }
    Some(args)
}

pub fn load_diff(workdir: &Path, target: &DiffTarget, opts: &DiffOptions) -> Result<FileDiff> {
    let Some(args) = diff_args(target, opts) else {
        let path = target.path();
        let full = workdir.join(path);
        let meta = std::fs::metadata(&full).map_err(|e| Error::new(format!("{path}: {e}")))?;
        if meta.is_dir() {
            return Ok(FileDiff::default());
        }
        if meta.len() > MAX_FILE_PREVIEW {
            return Ok(FileDiff::too_large());
        }
        let bytes = std::fs::read(&full).map_err(|e| Error::new(format!("{path}: {e}")))?;
        return Ok(FileDiff::from_new_file(&bytes));
    };

    let out = cli::run(workdir, &args, None)?;
    if out.stdout.len() > MAX_DIFF_BYTES {
        return Ok(FileDiff::too_large());
    }
    let mut diff = diff::parse(&out.stdout)
        .into_iter()
        .find(|f| !f.is_empty())
        .unwrap_or_default();
    diff.approximate = opts.ignore_whitespace;
    Ok(diff)
}
