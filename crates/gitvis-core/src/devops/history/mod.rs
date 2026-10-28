//! History browsing: compare two revisions (commits, files, stats), commit
//! sets for graph filters (author, path, date), the file tree at a commit,
//! blame of a file at any commit with "blame the previous version", and
//! signature verification of commits (`%G?`, `%GS`, `%GK`, `%GF`).
//!
//! - [`compare`]: `rev-parse`, `merge-base`, `rev-list --left-right
//!   --count`, `log --left-right` and `diff --raw --numstat -M -z` of two
//!   revisions (two-dot or three-dot, or a revision against the working
//!   tree), and the diff of one file.
//! - [`numstat`]: the `--raw --numstat -z` parser (renames, copies, binary).
//! - [`filter`]: graph filters; author and dates are matched in memory, path
//!   and message through one streamed, capped, cancellable `git log --all
//!   --format=%H`.
//! - [`tree`]: `ls-tree -r -l -z` into a folder tree with sizes, search,
//!   and a file's content at a commit (binary and size guards).
//! - [`blame`]: streamed `blame -w -M -C --porcelain` into lines and
//!   commits, with the previous version of each commit.
//! - [`signature`]: `%G?` and friends, the signature kind from the commit
//!   object (libgit2, no process), and how to fix "can't check".
//! - [`multi`]: several picked commits: order, totals, cherry-pick,
//!   format-patch and the marks of an interactive rebase.
//!
//! Everything here only reads, except the command builders in [`multi`],
//! whose commands run through the app's confirmation first.

pub mod blame;
pub mod compare;
pub mod filter;
pub mod multi;
pub mod numstat;
pub mod signature;
pub mod tree;

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};

pub use crate::devops::workbench::git_line;

/// Run git, record the command line, return stdout.
pub fn git_bytes<S: AsRef<str>>(
    dir: &Path,
    args: &[S],
    cmds: &mut Vec<String>,
) -> Result<Vec<u8>, String> {
    cmds.push(git_line(args));
    crate::cli::run(dir, args, None)
        .map(|o| o.stdout)
        .map_err(|e| e.to_string())
}

/// [`git_bytes`] as text.
pub fn git_text<S: AsRef<str>>(
    dir: &Path,
    args: &[S],
    cmds: &mut Vec<String>,
) -> Result<String, String> {
    git_bytes(dir, args, cmds).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Run git with `stdin`, record it (the input is a list of revisions, not
/// a secret, so only its size is noted).
pub fn git_stdin<S: AsRef<str>>(
    dir: &Path,
    args: &[S],
    stdin: &[u8],
    cmds: &mut Vec<String>,
) -> Result<Vec<u8>, String> {
    cmds.push(format!(
        "{}  (standard input: {} revisions)",
        git_line(args),
        stdin
            .split(|&b| b == b'\n')
            .filter(|l| !l.is_empty())
            .count()
    ));
    crate::cli::run(dir, args, Some(stdin))
        .map(|o| o.stdout)
        .map_err(|e| e.to_string())
}

/// A git process whose stdout is read as it comes (large outputs) and
/// whose stderr is collected for the error message. Same conventions as
/// [`crate::cli`]: no prompts, no editor, literal pathspecs, no window.
pub struct Streamed {
    pub child: Child,
    stderr: Option<std::thread::JoinHandle<String>>,
}

impl Streamed {
    pub fn start<S: AsRef<str>>(dir: &Path, args: &[S]) -> Result<Streamed, String> {
        let mut cmd = Command::new("git");
        cmd.current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .env("GIT_LITERAL_PATHSPECS", "1")
            .args(args.iter().map(AsRef::as_ref))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Could not start git ({e}). Is it installed and on PATH?"))?;
        let stderr = child.stderr.take().map(|mut e| {
            std::thread::spawn(move || {
                let mut s = String::new();
                let _ = e.read_to_string(&mut s);
                s
            })
        });
        Ok(Streamed { child, stderr })
    }

    /// Stop early (cap reached or cancelled).
    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }

    /// Wait for the end; `Err` with git's message when it failed and was
    /// not stopped by us.
    pub fn finish(mut self, killed: bool) -> Result<(), String> {
        let status = self.child.wait().map_err(|e| format!("git failed: {e}"))?;
        let err = self
            .stderr
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default();
        if status.success() || killed {
            Ok(())
        } else {
            let e = err.trim();
            Err(if e.is_empty() {
                format!("git exited with {status}")
            } else {
                e.to_owned()
            })
        }
    }
}

/// The first ten characters of a SHA.
pub fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_shas() {
        assert_eq!(short("0123456789abcdef"), "0123456789");
        assert_eq!(short("abc"), "abc");
    }
}
