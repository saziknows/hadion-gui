//! Running the `git` executable.
//!
//! Mutations go through the real git so hooks, signing, credential helpers,
//! LFS filters and the user's config behave exactly as on the command line.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::{Error, Result};

pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: String,
}

fn command(program: &str, dir: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.current_dir(dir)
        // Never block on a terminal prompt: there is no terminal.
        .env("GIT_TERMINAL_PROMPT", "0")
        // Continue/merge commands keep the prepared message instead of opening an editor.
        .env("GIT_EDITOR", "true")
        .env("GIT_MERGE_AUTOEDIT", "no")
        // Paths are file names, never globs (think `app/[slug]/page.tsx`).
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

pub fn run<S: AsRef<str>>(dir: &Path, args: &[S], stdin: Option<&[u8]>) -> Result<Output> {
    run_program("git", dir, args, stdin)
}

/// Run any program (`gh` for GitHub) with the same conventions as `run`.
pub fn run_program<S: AsRef<str>>(
    program: &str,
    dir: &Path,
    args: &[S],
    stdin: Option<&[u8]>,
) -> Result<Output> {
    safe_mode_check(program, args, stdin)?;
    let mut cmd = command(program, dir);
    cmd.args(args.iter().map(AsRef::as_ref));
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd.spawn().map_err(|e| {
        Error::new(format!(
            "Could not start {program} ({e}). Is it installed and on PATH?"
        ))
    })?;
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // Write from another thread so a chatty child can't deadlock us.
        let data = data.to_vec();
        std::thread::spawn(move || {
            let _ = pipe.write_all(&data);
        });
    }
    let out = child
        .wait_with_output()
        .map_err(|e| Error::new(format!("git failed: {e}")))?;
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    if out.status.success() {
        return Ok(Output {
            stdout: out.stdout,
            stderr,
        });
    }
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let message = match (stderr.is_empty(), stdout.is_empty()) {
        (false, true) => stderr,
        (true, false) => stdout,
        (false, false) => format!("{stdout}\n{stderr}"),
        (true, true) => format!(
            "{program} {} exited with {}",
            args.first().map(AsRef::as_ref).unwrap_or(""),
            out.status
        ),
    };
    Err(Error::new(message))
}

/// Refuse remote writes while safe mode is on ([`crate::devops::safety`]).
fn safe_mode_check<S: AsRef<str>>(program: &str, args: &[S], stdin: Option<&[u8]>) -> Result<()> {
    let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
    crate::devops::safety::check(program, &args, stdin).map_err(Error::new)
}

/// What a program printed and how it exited, whatever the exit code.
pub struct RawOutput {
    pub success: bool,
    /// `None` when the process was killed by a signal.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

/// Run a program and return its output even when it fails, so callers can
/// read error bodies (`gh api` prints the response on stdout on HTTP errors).
/// `env` adds variables on top of the usual conventions. Fails only when the
/// program cannot be started.
pub fn run_raw<S: AsRef<str>>(
    program: &str,
    dir: &Path,
    args: &[S],
    stdin: Option<&[u8]>,
    env: &[(&str, &str)],
) -> Result<RawOutput> {
    safe_mode_check(program, args, stdin)?;
    let mut cmd = command(program, dir);
    cmd.args(args.iter().map(AsRef::as_ref));
    for (k, v) in env {
        cmd.env(k, v);
    }
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd.spawn().map_err(|e| {
        Error::new(format!(
            "Could not start {program} ({e}). Is it installed and on PATH?"
        ))
    })?;
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let data = data.to_vec();
        std::thread::spawn(move || {
            let _ = pipe.write_all(&data);
        });
    }
    let out = child
        .wait_with_output()
        .map_err(|e| Error::new(format!("{program} failed: {e}")))?;
    Ok(RawOutput {
        success: out.status.success(),
        code: out.status.code(),
        stdout: out.stdout,
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
    })
}

/// Start `git args…` with stdout piped for streaming reads (large outputs
/// such as `git log -p`). Stderr is discarded; the caller waits on the child.
pub fn spawn<S: AsRef<str>>(dir: &Path, args: &[S]) -> Result<std::process::Child> {
    let mut cmd = command("git", dir);
    cmd.args(args.iter().map(AsRef::as_ref));
    cmd.stderr(Stdio::null());
    cmd.spawn().map_err(|e| {
        Error::new(format!(
            "Could not start git ({e}). Is it installed and on PATH?"
        ))
    })
}

pub fn version() -> Option<String> {
    let out = run(Path::new("."), &["--version"], None).ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}
