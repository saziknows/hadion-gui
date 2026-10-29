//! Local daily workflow: task discovery and running (package.json scripts
//! with the right package manager, Makefile, Cargo, justfile, Taskfile,
//! composer, deno, Go, .NET, Gradle / Maven wrappers, Python, compose),
//! interactive rebase (plan editing: pick / reword / edit / squash / fixup
//! / drop / reorder, executed through `GIT_SEQUENCE_EDITOR`), bisect
//! (manual good / bad / skip and `git bisect run` with a test command),
//! worktrees (list, add, remove, prune, lock / unlock, move) and file
//! history (`git log --follow` with renames and per-commit changes).
//!
//! Every function that runs git pushes the command lines it ran into a
//! `cmds` list, which the app shows in the activity log.

pub mod bisect;
pub mod history;
pub mod rebase;
pub mod shell;
pub mod tasks;
pub mod toml;
pub mod worktrees;

use std::path::Path;

use crate::cli;
use crate::devops::proc::render_args;

/// `git a b c` as typed.
pub fn git_line<S: AsRef<str>>(args: &[S]) -> String {
    let mut v: Vec<&str> = vec!["git"];
    v.extend(args.iter().map(AsRef::as_ref));
    render_args(&v)
}

/// Run git, record the command line, return stdout as text.
pub fn git<S: AsRef<str>>(
    dir: &Path,
    args: &[S],
    cmds: &mut Vec<String>,
) -> Result<String, String> {
    cmds.push(git_line(args));
    cli::run(dir, args, None)
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .map_err(|e| e.to_string())
}

/// Run git, record it, and return whatever it printed and how it exited.
pub fn git_raw<S: AsRef<str>>(
    dir: &Path,
    args: &[S],
    env: &[(&str, &str)],
    cmds: &mut Vec<String>,
) -> Result<cli::RawOutput, String> {
    cmds.push(git_line(args));
    cli::run_raw("git", dir, args, None, env).map_err(|e| e.to_string())
}

/// Stdout of a successful git command, `None` otherwise (probes such as
/// `rev-parse @{u}` that fail when there is nothing to find).
pub fn git_probe<S: AsRef<str>>(dir: &Path, args: &[S], cmds: &mut Vec<String>) -> Option<String> {
    git(dir, args, cmds)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// stdout and stderr of a raw run, joined for messages.
pub fn output_text(out: &cli::RawOutput) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    match (stdout.is_empty(), out.stderr.is_empty()) {
        (false, false) => format!("{stdout}\n{}", out.stderr),
        (false, true) => stdout,
        (true, false) => out.stderr.clone(),
        (true, true) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_lines_are_quoted() {
        assert_eq!(
            git_line(&["commit", "-m", "two words"]),
            "git commit -m \"two words\""
        );
        let mut cmds = Vec::new();
        let v = git(Path::new("."), &["--version"], &mut cmds).unwrap();
        assert!(v.starts_with("git version"));
        assert_eq!(cmds, vec!["git --version"]);
    }
}
