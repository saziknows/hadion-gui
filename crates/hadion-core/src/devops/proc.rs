//! Running tools other than git with streamed output: task runners, `act`,
//! `kubectl`, `terraform`, `git bisect run`, image scanners.
//!
//! - Program names resolve through `PATH` and `PATHEXT`, so `npm` finds
//!   `npm.cmd` on Windows (std only tries `.exe`).
//! - Output arrives line by line from both pipes, batched, ANSI escapes kept
//!   (the console renders them).
//! - Cancelling kills the whole process tree (`taskkill /T /F` on Windows,
//!   the process group elsewhere): `npm run x` would otherwise leave `node`
//!   running.
//! - Standard input is written once and closed; it is never logged.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::{Error, Result};

/// A process to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcSpec {
    /// A name looked up on `PATH` (`npm`, `kubectl`) or a path.
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Added to the inherited environment.
    pub env: Vec<(String, String)>,
    /// Written to standard input, then closed. Never shown or logged.
    pub stdin: Option<Vec<u8>>,
    /// Exit codes besides 0 that mean success (`terraform plan
    /// -detailed-exitcode` exits 2 when there are changes).
    pub ok_codes: Vec<i32>,
}

impl ProcSpec {
    pub fn new(program: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            env: Vec::new(),
            stdin: None,
            ok_codes: Vec::new(),
        }
    }

    /// Exit codes besides 0 that mean success.
    pub fn ok_codes(mut self, codes: &[i32]) -> Self {
        self.ok_codes = codes.to_vec();
        self
    }

    /// Did a process with this exit code succeed?
    pub fn is_success(&self, code: Option<i32>) -> bool {
        code.is_some_and(|c| c == 0 || self.ok_codes.contains(&c))
    }

    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }

    pub fn stdin(mut self, data: Vec<u8>) -> Self {
        self.stdin = Some(data);
        self
    }

    /// Ask common tools for colour even though stdout is a pipe.
    pub fn with_color(self) -> Self {
        self.env("FORCE_COLOR", "1")
            .env("CLICOLOR_FORCE", "1")
            .env("CARGO_TERM_COLOR", "always")
            .env("TERM", "xterm-256color")
    }

    /// The command line as shown to the user (`npm run "build all"`).
    /// Environment variables set by the spec are listed first, values of
    /// names that look secret are masked.
    pub fn display(&self) -> String {
        let mut s = String::new();
        for (k, v) in &self.env {
            if matches!(
                k.as_str(),
                "FORCE_COLOR" | "CLICOLOR_FORCE" | "CARGO_TERM_COLOR" | "TERM"
            ) {
                continue;
            }
            let upper = k.to_ascii_uppercase();
            let secret = ["TOKEN", "SECRET", "PASSWORD", "KEY", "AUTH"]
                .iter()
                .any(|w| upper.contains(w));
            s.push_str(&format!(
                "{k}={} ",
                if secret {
                    "•••".to_owned()
                } else {
                    quote(v)
                }
            ));
        }
        s.push_str(&quote(&self.program));
        for a in &self.args {
            s.push(' ');
            s.push_str(&quote(a));
        }
        if let Some(b) = &self.stdin {
            s.push_str(&format!(
                "  (standard input: {}, not logged)",
                super::plural_bytes(b.len())
            ));
        }
        s
    }
}

fn quote(a: &str) -> String {
    if a.is_empty() || a.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
        format!("\"{}\"", a.replace('"', "\\\""))
    } else {
        a.to_owned()
    }
}

/// `a "b c"` rendering of arguments.
pub fn render_args(args: &[&str]) -> String {
    args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
}

/// Which pipe a line came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// One line of output, ANSI escapes included, without the newline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub stream: Stream,
    pub text: String,
}

/// How a process ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exit {
    /// `None` when killed (or by a signal).
    pub code: Option<i32>,
    pub cancelled: bool,
    pub elapsed_ms: u64,
}

impl Exit {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.cancelled
    }
}

/// Find a program on `PATH`, trying each `PATHEXT` extension on Windows.
/// Paths (anything with a separator) are returned as they are when they exist.
pub fn resolve(program: &str) -> Option<PathBuf> {
    let p = Path::new(program);
    if p.components().count() > 1 || p.is_absolute() {
        return p.exists().then(|| p.to_path_buf());
    }
    let exts: Vec<String> = if cfg!(windows) {
        let mut e: Vec<String> = std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned())
            .split(';')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_lowercase())
            .collect();
        // A bare name with an extension already (`gh.exe`) is tried as is.
        e.insert(0, String::new());
        e
    } else {
        vec![String::new()]
    };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let candidate = dir.join(format!("{program}{ext}"));
            if candidate.is_file() {
                if cfg!(windows) && ext.is_empty() && Path::new(program).extension().is_none() {
                    // Extensionless files on Windows are usually shell
                    // scripts for Git Bash, not something CreateProcess runs.
                    continue;
                }
                return Some(candidate);
            }
        }
    }
    None
}

fn command(spec: &ProcSpec) -> Result<Command> {
    // Safe mode: refuse remote writes before anything starts.
    let args: Vec<&str> = spec.args.iter().map(String::as_str).collect();
    super::safety::check(&spec.program, &args, spec.stdin.as_deref()).map_err(Error::new)?;
    let program = resolve(&spec.program).ok_or_else(|| {
        Error::new(format!(
            "{} is not installed (not found on PATH)",
            spec.program
        ))
    })?;
    let mut cmd = Command::new(&program);
    cmd.args(&spec.args)
        .current_dir(&spec.cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    Ok(cmd)
}

/// Kill a process and everything it started.
pub fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000)
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Split a chunk of output into lines; a line rewritten with `\r`
/// (progress bars) keeps its last state.
fn clean_line(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    let s = s.strip_suffix('\n').unwrap_or(&s);
    let s = s.strip_suffix('\r').unwrap_or(s);
    match s.rfind('\r') {
        Some(i) if i + 1 < s.len() => s[i + 1..].to_owned(),
        _ => s.replace('\r', ""),
    }
}

fn pump(pipe: impl Read + Send + 'static, stream: Stream, tx: mpsc::Sender<Line>) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(pipe);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx
                        .send(Line {
                            stream,
                            text: clean_line(&buf),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
}

/// Run a process to the end, delivering output in batches to `sink` as it
/// arrives (at most every 60 ms). `on_start` receives the process id. When
/// `cancel` becomes true the process tree is killed.
pub fn run_streaming(
    spec: &ProcSpec,
    cancel: &AtomicBool,
    mut on_start: impl FnMut(u32),
    mut sink: impl FnMut(Vec<Line>),
) -> Result<Exit> {
    let started = Instant::now();
    let mut child = command(spec)?
        .spawn()
        .map_err(|e| Error::new(format!("Could not start {} ({e})", spec.program)))?;
    on_start(child.id());
    if let (Some(data), Some(mut pipe)) = (spec.stdin.clone(), child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = pipe.write_all(&data);
        });
    }
    let (tx, rx) = mpsc::channel::<Line>();
    if let Some(out) = child.stdout.take() {
        pump(out, Stream::Stdout, tx.clone());
    }
    if let Some(err) = child.stderr.take() {
        pump(err, Stream::Stderr, tx.clone());
    }
    drop(tx);
    let mut batch = Vec::new();
    let mut last_flush = Instant::now();
    let mut cancelled = false;
    let mut code = None;
    let mut exited = false;
    let mut exited_at: Option<Instant> = None;
    loop {
        // A daemon started by the process may keep the pipes open forever;
        // stop reading a little after the process itself has exited.
        if exited_at.is_some_and(|t| t.elapsed() > Duration::from_millis(1500)) {
            if !batch.is_empty() {
                sink(std::mem::take(&mut batch));
            }
            break;
        }
        match rx.recv_timeout(Duration::from_millis(30)) {
            Ok(line) => batch.push(line),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // Both pipes closed: the process (and its children) are done
            // writing.
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if !batch.is_empty() {
                    sink(std::mem::take(&mut batch));
                }
                break;
            }
        }
        if !batch.is_empty()
            && (last_flush.elapsed() >= Duration::from_millis(60) || batch.len() > 500)
        {
            sink(std::mem::take(&mut batch));
            last_flush = Instant::now();
        }
        if !cancelled && cancel.load(Ordering::Relaxed) {
            cancelled = true;
            kill_tree(child.id());
            let _ = child.kill();
        }
        if !exited && let Ok(Some(status)) = child.try_wait() {
            exited = true;
            exited_at = Some(Instant::now());
            code = status.code();
        }
    }
    if !exited {
        code = child.wait().ok().and_then(|s| s.code());
    }
    Ok(Exit {
        code: if cancelled { None } else { code },
        cancelled,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// Output of [`run_capture`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Captured {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub elapsed_ms: u64,
    /// The timeout fired and the process was killed.
    pub timed_out: bool,
}

impl Captured {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }

    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// stderr, else stdout, else the exit code: what to show when it failed.
    pub fn error_text(&self) -> String {
        if self.timed_out {
            return format!("Timed out after {} ms", self.elapsed_ms);
        }
        let e = self.stderr.trim();
        if !e.is_empty() {
            return e.to_owned();
        }
        let o = String::from_utf8_lossy(&self.stdout).trim().to_owned();
        if !o.is_empty() {
            return o;
        }
        format!("exited with {:?}", self.code)
    }
}

/// Run a process to the end and capture its output. With a `timeout`, the
/// process tree is killed when it runs longer.
pub fn run_capture(spec: &ProcSpec, timeout: Option<Duration>) -> Result<Captured> {
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let mut stdout = Vec::new();
    let mut stderr = String::new();
    let mut child = command(spec)?
        .spawn()
        .map_err(|e| Error::new(format!("Could not start {} ({e})", spec.program)))?;
    if let (Some(data), Some(mut pipe)) = (spec.stdin.clone(), child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = pipe.write_all(&data);
        });
    }
    let out_pipe = child.stdout.take();
    let err_pipe = child.stderr.take();
    let out_h = std::thread::spawn(move || {
        let mut v = Vec::new();
        if let Some(mut p) = out_pipe {
            let _ = p.read_to_end(&mut v);
        }
        v
    });
    let err_h = std::thread::spawn(move || {
        let mut v = Vec::new();
        if let Some(mut p) = err_pipe {
            let _ = p.read_to_end(&mut v);
        }
        v
    });
    let mut timed_out = false;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {}
            Err(e) => return Err(Error::new(format!("{} failed: {e}", spec.program))),
        }
        if let Some(t) = timeout
            && started.elapsed() > t
            && !cancel.swap(true, Ordering::Relaxed)
        {
            timed_out = true;
            kill_tree(child.id());
            let _ = child.kill();
        }
        std::thread::sleep(Duration::from_millis(15));
    };
    if let Ok(v) = out_h.join() {
        stdout = v;
    }
    if let Ok(v) = err_h.join() {
        stderr = String::from_utf8_lossy(&v).into_owned();
    }
    Ok(Captured {
        code,
        stdout,
        stderr,
        elapsed_ms: started.elapsed().as_millis() as u64,
        timed_out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_quotes_and_masks() {
        let s = ProcSpec::new("npm", ".")
            .args(["run", "build all"])
            .env("API_TOKEN", "abc")
            .env("NODE_ENV", "production")
            .with_color()
            .stdin(b"hunter2".to_vec());
        let d = s.display();
        assert!(d.starts_with("API_TOKEN=••• NODE_ENV=production npm run \"build all\""));
        assert!(d.contains("standard input: 7 bytes, not logged"));
        assert!(!d.contains("hunter2"));
        assert!(!d.contains("abc"));
    }

    #[test]
    fn progress_lines_keep_last_state() {
        assert_eq!(clean_line(b"10%\r50%\r100%\n"), "100%");
        assert_eq!(clean_line(b"done\r\n"), "done");
        assert_eq!(clean_line(b"plain"), "plain");
    }

    #[test]
    fn git_runs_and_streams() {
        let cancel = AtomicBool::new(false);
        let mut lines = Vec::new();
        let spec = ProcSpec::new("git", ".").arg("--version");
        let exit = run_streaming(&spec, &cancel, |_| {}, |batch| lines.extend(batch)).unwrap();
        assert!(exit.success());
        assert!(lines.iter().any(|l| l.text.starts_with("git version")));
        let cap = run_capture(&spec, Some(Duration::from_secs(20))).unwrap();
        assert!(cap.success());
        assert!(cap.stdout_text().starts_with("git version"));
    }

    #[test]
    fn missing_program_is_a_clear_error() {
        let spec = ProcSpec::new("definitely-not-a-real-tool-xyz", ".");
        let err = run_capture(&spec, None).unwrap_err().to_string();
        assert!(err.contains("not installed"), "{err}");
    }
}
