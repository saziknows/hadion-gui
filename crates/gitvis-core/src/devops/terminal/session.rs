//! One shell in a pseudo-terminal.
//!
//! [`Session::spawn`] returns at once; a `terminal-pty` thread opens the
//! pseudo-terminal (ConPTY on Windows), starts the shell and then writes
//! input and resizes in order. A `terminal-read` thread feeds the output
//! through the [`Filter`] into the shared [`Screen`], answers the terminal
//! queries with the real screen state, and calls the wake callback. A
//! `terminal-wait` thread waits for the shell to exit, records the exit
//! code and closes the pseudo-terminal, which ends the reader. The UI
//! thread only takes the lock to read the screen (and can use
//! [`Session::try_term`] so it never waits).

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::Instant;

use portable_pty::{ChildKiller, CommandBuilder, PtySize, native_pty_system};

use super::screen::Screen;
use super::stream::{self, Colors, Filter, Item};

/// How to start a session.
#[derive(Clone, Debug)]
pub struct SpawnOptions {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub rows: u16,
    pub cols: u16,
    /// Lines kept above the screen.
    pub scrollback: usize,
    /// Added to the inherited environment (after `TERM`, `COLORTERM`).
    pub env: Vec<(String, String)>,
    pub colors: Colors,
}

/// Where a session is in its life.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Starting,
    Running {
        pid: Option<u32>,
    },
    /// The shell ended (`code` as the OS reports it).
    Exited {
        code: u32,
        after_ms: u64,
    },
    /// The pseudo-terminal or the shell could not be started.
    Failed(String),
}

impl Status {
    pub fn is_alive(&self) -> bool {
        matches!(self, Status::Starting | Status::Running { .. })
    }
}

/// The screen and counters, behind the session's lock.
pub struct Term {
    pub screen: Screen,
    pub status: Status,
    pub colors: Colors,
    /// Bytes read from / written to the pseudo-terminal.
    pub bytes_in: u64,
    pub bytes_out: u64,
    /// Terminal queries answered (ConPTY's cursor query among them).
    pub queries: u64,
    pub started: Instant,
    filter: Filter,
}

impl Term {
    fn new(rows: u16, cols: u16, scrollback: usize, colors: Colors) -> Self {
        let mut filter = Filter::new();
        filter.dim_bg = colors.bg;
        Self {
            screen: Screen::new(rows, cols, scrollback),
            status: Status::Starting,
            colors,
            bytes_in: 0,
            bytes_out: 0,
            queries: 0,
            started: Instant::now(),
            filter,
        }
    }

    /// Feed output; returns the answers to send back.
    fn feed(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.bytes_in += bytes.len() as u64;
        self.filter.dim_bg = self.colors.bg;
        let mut items = Vec::new();
        self.filter.feed(bytes, &mut items);
        let mut answers = Vec::new();
        for item in items {
            match item {
                Item::Data(d) => self.screen.process(&d),
                Item::Query(q) => {
                    self.queries += 1;
                    answers.push(stream::answer(q, self.screen.vt(), &self.colors));
                }
                Item::ClearScrollback => self.screen.clear_scrollback(),
            }
        }
        answers
    }
}

enum Control {
    Input(Vec<u8>),
    Resize(u16, u16),
    /// The shell exited (or the session was dropped): close the
    /// pseudo-terminal.
    Exited,
}

struct Shared {
    term: Mutex<Term>,
    /// Bumped on every change, so the UI can skip unchanged frames
    /// without taking the lock.
    generation: AtomicU64,
    wake: Box<dyn Fn() + Send + Sync>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Term> {
        self.term.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn changed(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        (self.wake)();
    }
}

type Killer = Arc<Mutex<Option<Box<dyn ChildKiller + Send + Sync>>>>;

/// A running (or finished) shell.
pub struct Session {
    shared: Arc<Shared>,
    tx: Sender<Control>,
    killer: Killer,
}

impl Session {
    /// Start a shell; never blocks. `wake` is called (from other threads)
    /// whenever the screen or the status changed.
    pub fn spawn(opts: SpawnOptions, wake: impl Fn() + Send + Sync + 'static) -> Session {
        let shared = Arc::new(Shared {
            term: Mutex::new(Term::new(
                opts.rows,
                opts.cols,
                opts.scrollback,
                opts.colors,
            )),
            generation: AtomicU64::new(0),
            wake: Box::new(wake),
        });
        let (tx, rx) = mpsc::channel();
        let killer: Killer = Arc::new(Mutex::new(None));
        let (s, t, k) = (shared.clone(), tx.clone(), killer.clone());
        let spawned = std::thread::Builder::new()
            .name("terminal-pty".into())
            .spawn(move || run_pty(opts, s, t, rx, k));
        if let Err(e) = spawned {
            shared.lock().status = Status::Failed(format!("Could not start a thread: {e}"));
            shared.changed();
        }
        Session { shared, tx, killer }
    }

    /// Send bytes to the shell (keys, pastes); never blocks.
    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        let bytes = bytes.into();
        if !bytes.is_empty() {
            let _ = self.tx.send(Control::Input(bytes));
        }
    }

    /// Resize the screen now and the pseudo-terminal right after. Never
    /// waits: returns false when the reader holds the screen (call again).
    pub fn resize(&self, rows: u16, cols: u16) -> bool {
        let (rows, cols) = (rows.max(2), cols.max(2));
        {
            let Some(mut t) = self.try_term() else {
                return false;
            };
            if t.screen.size() == (rows, cols) {
                return true;
            }
            t.screen.resize(rows, cols);
        }
        let _ = self.tx.send(Control::Resize(rows, cols));
        self.shared.changed();
        true
    }

    /// End the shell (and with the pseudo-terminal everything it started).
    pub fn kill(&self) {
        if let Some(k) = self
            .killer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            let _ = k.kill();
        }
    }

    /// Changes so far (cheap; no lock).
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Relaxed)
    }

    /// The screen and counters (waits for the reader if it is busy).
    pub fn term(&self) -> MutexGuard<'_, Term> {
        self.shared.lock()
    }

    /// The screen, unless the reader holds it right now.
    pub fn try_term(&self) -> Option<MutexGuard<'_, Term>> {
        match self.shared.term.try_lock() {
            Ok(g) => Some(g),
            Err(TryLockError::Poisoned(e)) => Some(e.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    pub fn status(&self) -> Status {
        self.shared.lock().status.clone()
    }

    /// The colours answered to `OSC 10/11` queries and dimmed towards.
    pub fn set_colors(&self, colors: Colors) {
        self.shared.lock().colors = colors;
    }

    /// Mark the screen changed (after the UI moved the viewport).
    pub fn touch(&self) {
        self.shared.generation.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // End the shell and close the pseudo-terminal; the threads finish
        // on their own.
        self.kill();
        let _ = self.tx.send(Control::Exited);
    }
}

fn run_pty(
    opts: SpawnOptions,
    shared: Arc<Shared>,
    tx: Sender<Control>,
    rx: Receiver<Control>,
    killer: Killer,
) {
    let size = PtySize {
        rows: opts.rows.max(2),
        cols: opts.cols.max(2),
        pixel_width: 0,
        pixel_height: 0,
    };
    let opened = (|| {
        let pair = native_pty_system()
            .openpty(size)
            .map_err(|e| format!("Could not open a pseudo-terminal: {e:#}"))?;
        let mut cmd = CommandBuilder::new(&opts.program);
        cmd.args(&opts.args);
        cmd.cwd(&opts.cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "gitvis");
        cmd.env_remove("GITVIS_SCRIPT");
        for (k, v) in &opts.env {
            cmd.env(k, v);
        }
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("Could not read the pseudo-terminal: {e:#}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("Could not write to the pseudo-terminal: {e:#}"))?;
        let child = pair.slave.spawn_command(cmd).map_err(|e| {
            format!(
                "Could not start {} in {}: {e:#}",
                opts.program.display(),
                opts.cwd.display()
            )
        })?;
        // The child has its own handle; ours would keep the terminal open
        // after it exits (on Unix the reader would never see the end).
        drop(pair.slave);
        Ok::<_, String>((pair.master, reader, writer, child))
    })();
    let (master, reader, mut writer, mut child) = match opened {
        Ok(v) => v,
        Err(e) => {
            shared.lock().status = Status::Failed(e);
            shared.changed();
            return;
        }
    };
    let pid = child.process_id();
    *killer.lock().unwrap_or_else(|e| e.into_inner()) = Some(child.clone_killer());
    shared.lock().status = Status::Running { pid };
    shared.changed();

    let (s, t) = (shared.clone(), tx.clone());
    let _ = std::thread::Builder::new()
        .name("terminal-read".into())
        .spawn(move || read_loop(reader, s, t));

    let s = shared.clone();
    let _ = std::thread::Builder::new()
        .name("terminal-wait".into())
        .spawn(move || {
            let code = match child.wait() {
                Ok(status) => status.exit_code(),
                Err(_) => 1,
            };
            {
                let mut term = s.lock();
                let after_ms = term.started.elapsed().as_millis() as u64;
                term.status = Status::Exited { code, after_ms };
            }
            s.changed();
            let _ = tx.send(Control::Exited);
        });

    for msg in rx {
        match msg {
            Control::Input(bytes) => {
                if writer
                    .write_all(&bytes)
                    .and_then(|_| writer.flush())
                    .is_ok()
                {
                    shared.lock().bytes_out += bytes.len() as u64;
                }
            }
            Control::Resize(rows, cols) => {
                let _ = master.resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
            Control::Exited => break,
        }
    }
    // Closing the pseudo-terminal ends the reader (ConPTY keeps the output
    // pipe open until then, even after the shell has gone).
    drop(writer);
    drop(master);
}

fn read_loop(mut reader: Box<dyn Read + Send>, shared: Arc<Shared>, tx: Sender<Control>) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let answers = shared.lock().feed(&buf[..n]);
        for a in answers {
            let _ = tx.send(Control::Input(a));
        }
        shared.changed();
    }
    // All output is in: say how the shell ended, after its last line (the
    // wait thread records the code just before the terminal closes; on
    // Unix the end of output can come first, so give it a moment).
    for _ in 0..20 {
        let code = match &shared.lock().status {
            Status::Exited { code, .. } => Some(*code),
            _ => None,
        };
        if let Some(code) = code {
            shared.lock().feed(exit_marker(code).as_bytes());
            shared.changed();
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The line written under a shell's last output when it ends.
pub fn exit_marker(code: u32) -> String {
    format!("\r\n\x1b[0;2m[Process exited with code {code}]\x1b[0m\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_are_answered_from_the_screen() {
        let mut t = Term::new(10, 40, 100, Colors::default());
        let answers = t.feed(b"abc\x1b[6n");
        assert_eq!(answers, vec![b"\x1b[1;4R".to_vec()]);
        let answers = t.feed(b"\r\n\x1b[2;7H\x1b[6n\x1b[c");
        assert_eq!(answers, vec![b"\x1b[2;7R".to_vec(), b"\x1b[?1;2c".to_vec()]);
        assert_eq!(t.queries, 3);
        // The query itself never reaches the screen.
        assert!(!t.screen.contents().contains("6n"));
    }

    #[test]
    fn clear_drops_the_history_and_keeps_the_screen() {
        let mut t = Term::new(3, 20, 100, Colors::default());
        let text: String = (0..10).map(|i| format!("line {i}\r\n")).collect();
        t.feed(text.as_bytes());
        t.feed(b"\x1b[31mprompt> ");
        assert!(t.screen.scrollback_len() > 0);
        let before = t.screen.contents();
        t.feed(b"\x1b[3J");
        assert_eq!(t.screen.scrollback_len(), 0);
        assert_eq!(t.screen.contents(), before);
    }
}
