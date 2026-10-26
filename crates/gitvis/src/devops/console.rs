//! Processes streamed into the console: task runs, `act`, `terraform plan`,
//! `kubectl logs`, `git bisect run`, image scans. Shared by every page; the
//! view is [`crate::views::console`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use gitvis_core::devops::proc::{Exit, Line, ProcSpec, Stream};
use gitvis_core::joblog::{ColorSpan, strip_ansi};

use crate::state::RepoId;

/// Lines kept per run; older ones are dropped (and counted).
pub const MAX_LINES: usize = 60_000;
/// Finished runs kept; the oldest finished run goes first.
pub const MAX_RUNS: usize = 40;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A fresh console run id.
pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// One output line, escapes turned into colour spans.
#[derive(Clone, Debug)]
pub struct ConsoleLine {
    pub text: String,
    pub spans: Vec<ColorSpan>,
    pub stderr: bool,
}

impl ConsoleLine {
    pub fn from_raw(line: &Line) -> Self {
        let (text, spans) = strip_ansi(&line.text);
        Self {
            text,
            spans,
            stderr: line.stream == Stream::Stderr,
        }
    }
}

pub struct ConsoleRun {
    pub id: u64,
    pub repo: Option<RepoId>,
    pub title: String,
    pub spec: ProcSpec,
    /// The command line as shown (secrets in the environment masked).
    pub command: String,
    pub lines: Vec<ConsoleLine>,
    /// Lines dropped from the front to stay under [`MAX_LINES`].
    pub dropped: usize,
    pub bytes: usize,
    pub started: Instant,
    /// `HH:MM:SS` wall clock when it started.
    pub started_clock: String,
    pub ended: Option<Instant>,
    /// `Err` when the process could not be started.
    pub exit: Option<Result<Exit, String>>,
    pub pid: Option<u32>,
    pub cancel: Arc<AtomicBool>,
    /// How many times it has been started (1 for the first run).
    pub attempt: u32,
}

impl ConsoleRun {
    pub fn running(&self) -> bool {
        self.exit.is_none()
    }

    pub fn succeeded(&self) -> bool {
        matches!(&self.exit, Some(Ok(e)) if e.success())
    }

    pub fn failed(&self) -> bool {
        matches!(&self.exit, Some(Ok(e)) if !e.success() && !e.cancelled)
            || matches!(&self.exit, Some(Err(_)))
    }

    pub fn cancelled(&self) -> bool {
        matches!(&self.exit, Some(Ok(e)) if e.cancelled)
    }

    pub fn exit_code(&self) -> Option<i32> {
        match &self.exit {
            Some(Ok(e)) => e.code,
            _ => None,
        }
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.ended
            .unwrap_or_else(Instant::now)
            .duration_since(self.started)
            .as_millis() as u64
    }

    /// `Running`, `Passed`, `Failed (exit 2)`, `Stopped`, `Could not start`.
    pub fn status_text(&self) -> String {
        match &self.exit {
            None => "Running".to_owned(),
            Some(Err(_)) => "Could not start".to_owned(),
            Some(Ok(e)) if e.cancelled => "Stopped".to_owned(),
            Some(Ok(e)) if e.success() => "Passed".to_owned(),
            Some(Ok(e)) => match e.code {
                Some(c) => format!("Failed (exit {c})"),
                None => "Killed".to_owned(),
            },
        }
    }

    /// All output as plain text.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for l in &self.lines {
            s.push_str(&l.text);
            s.push('\n');
        }
        s
    }

    /// The last `n` lines as plain text (for summaries and errors).
    pub fn tail(&self, n: usize) -> String {
        let start = self.lines.len().saturating_sub(n);
        self.lines[start..]
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Every console run of the session.
#[derive(Default)]
pub struct Console {
    /// Oldest first.
    pub runs: Vec<ConsoleRun>,
}

impl Console {
    pub fn get(&self, id: u64) -> Option<&ConsoleRun> {
        self.runs.iter().find(|r| r.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut ConsoleRun> {
        self.runs.iter_mut().find(|r| r.id == id)
    }

    /// Runs of one repository, newest first.
    pub fn for_repo(&self, repo: RepoId) -> impl Iterator<Item = &ConsoleRun> {
        self.runs.iter().rev().filter(move |r| r.repo == Some(repo))
    }

    pub fn running(&self) -> usize {
        self.runs.iter().filter(|r| r.running()).count()
    }

    /// Register a run (the app starts the process right after).
    pub fn add(&mut self, id: u64, repo: Option<RepoId>, title: String, spec: ProcSpec) {
        let command = spec.display();
        self.runs.push(ConsoleRun {
            id,
            repo,
            title,
            command,
            spec,
            lines: Vec::new(),
            dropped: 0,
            bytes: 0,
            started: Instant::now(),
            started_clock: crate::format::now_clock_ms()
                .split('.')
                .next()
                .unwrap_or_default()
                .to_owned(),
            ended: None,
            exit: None,
            pid: None,
            cancel: Arc::new(AtomicBool::new(false)),
            attempt: 1,
        });
        self.trim();
    }

    /// Reset a finished run for another attempt with the same command.
    /// Returns its spec and cancel flag, or `None` when it is still running.
    pub fn restart(&mut self, id: u64) -> Option<(ProcSpec, Arc<AtomicBool>)> {
        let run = self.get_mut(id)?;
        if run.running() {
            return None;
        }
        run.lines.clear();
        run.dropped = 0;
        run.bytes = 0;
        run.started = Instant::now();
        run.started_clock = crate::format::now_clock_ms()
            .split('.')
            .next()
            .unwrap_or_default()
            .to_owned();
        run.ended = None;
        run.exit = None;
        run.pid = None;
        run.cancel = Arc::new(AtomicBool::new(false));
        run.attempt += 1;
        Some((run.spec.clone(), run.cancel.clone()))
    }

    pub fn stop(&self, id: u64) {
        if let Some(r) = self.get(id) {
            r.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn remove(&mut self, id: u64) {
        if let Some(pos) = self.runs.iter().position(|r| r.id == id) {
            self.runs[pos].cancel.store(true, Ordering::Relaxed);
            self.runs.remove(pos);
        }
    }

    /// Drop every finished run of a repository.
    pub fn clear_finished(&mut self, repo: Option<RepoId>) {
        self.runs
            .retain(|r| r.running() || (repo.is_some() && r.repo != repo));
    }

    pub fn started(&mut self, id: u64, pid: u32) {
        if let Some(r) = self.get_mut(id) {
            r.pid = Some(pid);
        }
    }

    pub fn output(&mut self, id: u64, lines: Vec<Line>) {
        let Some(r) = self.get_mut(id) else {
            return;
        };
        for l in &lines {
            r.bytes += l.text.len() + 1;
            r.lines.push(ConsoleLine::from_raw(l));
        }
        if r.lines.len() > MAX_LINES {
            let extra = r.lines.len() - MAX_LINES;
            r.lines.drain(..extra);
            r.dropped += extra;
        }
    }

    pub fn finished(&mut self, id: u64, exit: Result<Exit, String>) {
        if let Some(r) = self.get_mut(id) {
            if let Err(e) = &exit {
                r.lines.push(ConsoleLine {
                    text: e.clone(),
                    spans: Vec::new(),
                    stderr: true,
                });
            }
            r.exit = Some(exit);
            r.ended = Some(Instant::now());
        }
        self.trim();
    }

    fn trim(&mut self) {
        while self.runs.len() > MAX_RUNS {
            match self.runs.iter().position(|r| !r.running()) {
                Some(pos) => {
                    self.runs.remove(pos);
                }
                None => break,
            }
        }
    }
}

/// Console commands from the views.
#[derive(Clone, Debug)]
pub enum ConsoleAction {
    Stop(u64),
    Rerun(u64),
    Remove(u64),
    /// Drop the finished runs of the active repository.
    ClearFinished,
    /// Copy the output to the clipboard.
    Copy(u64),
}
