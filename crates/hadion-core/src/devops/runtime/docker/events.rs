//! Live updates: `docker events` followed in the background.
//!
//! [`follow`] runs one `docker --context C events --format json …` for as
//! long as the Containers tab is shown. Its output only marks the listing
//! dirty ([`Follow::events`] grows and `wake` asks the UI for a frame); the
//! app re-lists after a quiet moment. Three ways it ends:
//! - the app stops it ([`Follow::stop`]: the context changed, Refresh);
//! - the view stops beating ([`Follow::beat`] every frame it draws the
//!   tab): after [`Follow::hidden_after`] without a beat the watchdog stops
//!   it, so nothing runs while the tab is not shown (another page, another
//!   repository tab, a minimised window);
//! - docker exits by itself (the daemon stopped, the connection dropped):
//!   the app falls back to polling.
//!
//! Stopping kills the process tree (`proc::kill_tree`, through
//! `run_streaming`'s cancel flag). [`Follow::kill_now`] does it on the
//! calling thread, for a repository tab that closes or an app that exits.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::devops::proc::{ProcSpec, Stream, kill_tree, run_streaming};

/// One container event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Event {
    /// `start`, `die`, `health_status`.
    pub action: String,
    /// `healthy` for `health_status: healthy`.
    pub detail: String,
    pub id: String,
    pub name: String,
    pub project: Option<String>,
    pub exit_code: Option<i64>,
    pub time: i64,
}

impl Event {
    /// `shop-db-1 became unhealthy`, `shop-worker-1 died (exit 137)`.
    pub fn text(&self) -> String {
        let n = if self.name.is_empty() {
            &self.id[..self.id.len().min(12)]
        } else {
            &self.name
        };
        match self.action.as_str() {
            "health_status" => format!("{n} became {}", self.detail),
            "die" => match self.exit_code {
                Some(c) => format!("{n} stopped (exit {c})"),
                None => format!("{n} stopped"),
            },
            "start" => format!("{n} started"),
            "restart" => format!("{n} restarted"),
            "stop" => format!("{n} was stopped"),
            "kill" => format!("{n} was killed"),
            "oom" => format!("{n} ran out of memory"),
            "pause" => format!("{n} was paused"),
            "unpause" => format!("{n} was unpaused"),
            "destroy" => format!("{n} was removed"),
            "create" => format!("{n} was created"),
            "rename" => format!("{n} was renamed"),
            "update" => format!("{n} had its settings changed"),
            other => format!("{n}: {other}"),
        }
    }

    /// Changes what the list shows (health probes' `exec_*` do not).
    pub fn relevant(&self) -> bool {
        !self.action.starts_with("exec_") && !self.action.is_empty()
    }
}

/// One line of `docker events --format json`. Newer docker writes only
/// `Action`, older also `status` / `id`; `Action` may carry a detail after
/// a colon (`health_status: healthy`, `exec_start: <command>`; the
/// command is dropped, it may hold a secret).
pub fn parse_event(line: &str) -> Option<Event> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    if let Some(t) = v.get("Type").and_then(Value::as_str)
        && t != "container"
    {
        return None;
    }
    let raw = v
        .get("Action")
        .or_else(|| v.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let (action, detail) = match raw.split_once(':') {
        Some((a, d)) if a == "health_status" => (a.trim(), d.trim()),
        Some((a, _)) => (a.trim(), ""),
        None => (raw.trim(), ""),
    };
    let actor = v.get("Actor");
    let attr = |k: &str| {
        actor
            .and_then(|a| a.get("Attributes"))
            .and_then(|a| a.get(k))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let id = actor
        .and_then(|a| a.get("ID"))
        .or_else(|| v.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    Some(Event {
        action: action.to_owned(),
        detail: detail.to_owned(),
        id,
        name: attr("name").unwrap_or_default(),
        project: attr(super::model::LABEL_PROJECT),
        exit_code: attr("exitCode").and_then(|c| c.parse().ok()),
        time: v.get("time").and_then(Value::as_i64).unwrap_or(0),
    })
}

/// Why the app stopped a stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// The context changed, Refresh, or a new stream replaced it.
    Stopped,
    /// No frame drew the tab for a while.
    Hidden,
    /// The repository tab closed or the app exits.
    Closed,
}

impl StopReason {
    fn code(self) -> u8 {
        match self {
            StopReason::Stopped => 1,
            StopReason::Hidden => 2,
            StopReason::Closed => 3,
        }
    }

    fn from_code(c: u8) -> Option<Self> {
        match c {
            1 => Some(StopReason::Stopped),
            2 => Some(StopReason::Hidden),
            3 => Some(StopReason::Closed),
            _ => None,
        }
    }
}

/// What the UI and the stream share.
pub struct Follow {
    cancel: AtomicBool,
    reason: AtomicU8,
    epoch: Instant,
    beat_ms: AtomicU64,
    pid: AtomicU32,
    events: AtomicU64,
    last: Mutex<Option<(Event, Instant)>>,
    /// The watchdog stops the stream after this long without a beat.
    pub hidden_after: Duration,
    wake: Box<dyn Fn() + Send + Sync>,
}

impl std::fmt::Debug for Follow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Follow")
            .field("events", &self.events())
            .field("pid", &self.pid())
            .field("stopped", &self.stopped())
            .finish()
    }
}

/// How long the tab may go without a frame before its stream stops.
pub const HIDDEN_AFTER: Duration = Duration::from_secs(5);

impl Follow {
    /// `wake` is called (from the stream's thread) for each relevant event.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            cancel: AtomicBool::new(false),
            reason: AtomicU8::new(0),
            epoch: Instant::now(),
            beat_ms: AtomicU64::new(0),
            pid: AtomicU32::new(0),
            events: AtomicU64::new(0),
            last: Mutex::new(None),
            hidden_after: HIDDEN_AFTER,
            wake: Box::new(wake),
        })
    }

    /// The same with another watchdog delay (tests).
    pub fn with_hidden_after(wake: impl Fn() + Send + Sync + 'static, d: Duration) -> Arc<Self> {
        let mut f = Self::new(wake);
        if let Some(inner) = Arc::get_mut(&mut f) {
            inner.hidden_after = d;
        }
        f
    }

    /// The tab was drawn (call every frame it is).
    pub fn beat(&self) {
        self.beat_ms
            .store(self.epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    fn quiet_for(&self) -> Duration {
        let last = self.beat_ms.load(Ordering::Relaxed);
        self.epoch
            .elapsed()
            .saturating_sub(Duration::from_millis(last))
    }

    /// Ask the stream to end (the process tree is killed within ~30 ms).
    pub fn stop(&self, why: StopReason) {
        let _ = self
            .reason
            .compare_exchange(0, why.code(), Ordering::Relaxed, Ordering::Relaxed);
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Stop and kill the process tree on this thread, now: the stream's
    /// thread may not get to it (the app is exiting).
    pub fn kill_now(&self, why: StopReason) {
        self.stop(why);
        if let Some(pid) = self.pid() {
            kill_tree(pid);
        }
    }

    pub fn stopped(&self) -> Option<StopReason> {
        StopReason::from_code(self.reason.load(Ordering::Relaxed))
    }

    /// The process id while docker runs.
    pub fn pid(&self) -> Option<u32> {
        Some(self.pid.load(Ordering::Relaxed)).filter(|p| *p != 0)
    }

    /// Relevant events seen so far.
    pub fn events(&self) -> u64 {
        self.events.load(Ordering::Relaxed)
    }

    /// The last relevant event and when it arrived.
    pub fn last(&self) -> Option<(Event, Instant)> {
        self.last.lock().ok().and_then(|l| l.clone())
    }

    fn seen(&self, e: Event) {
        if let Ok(mut l) = self.last.lock() {
            *l = Some((e, Instant::now()));
        }
        self.events.fetch_add(1, Ordering::Relaxed);
        (self.wake)();
    }
}

/// How a stream ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ended {
    /// The app (or the watchdog) stopped it.
    Stopped(StopReason),
    /// docker exited on its own; its last error lines.
    Exited { code: Option<i32>, stderr: String },
    /// It could not start.
    Failed(String),
}

impl Ended {
    /// One line for the activity log.
    pub fn text(&self) -> String {
        match self {
            Ended::Stopped(StopReason::Stopped) => "stopped".into(),
            Ended::Stopped(StopReason::Hidden) => "stopped: the tab was hidden".into(),
            Ended::Stopped(StopReason::Closed) => "stopped: the repository tab closed".into(),
            Ended::Exited { code, stderr } => {
                let c = code.map_or("no exit code".to_owned(), |c| format!("exit {c}"));
                if stderr.is_empty() {
                    format!("docker events ended ({c})")
                } else {
                    format!("docker events ended ({c}): {stderr}")
                }
            }
            Ended::Failed(e) => format!("docker events could not start: {e}"),
        }
    }
}

/// Run `spec` (a `docker events` command) until it is stopped, hidden or
/// ends. Blocks; call it from a background task. A second thread runs the
/// process; this one is the watchdog (it wakes every 250 ms).
pub fn follow(spec: &ProcSpec, f: &Arc<Follow>) -> Ended {
    f.beat();
    let (tx, rx) = mpsc::channel();
    let (f2, spec2) = (f.clone(), spec.clone());
    let stderr: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let err2 = stderr.clone();
    let spawned = std::thread::Builder::new()
        .name("docker-events".into())
        .spawn(move || {
            let r = run_streaming(
                &spec2,
                &f2.cancel,
                |pid| f2.pid.store(pid, Ordering::Relaxed),
                |lines| {
                    for l in lines {
                        if l.stream == Stream::Stderr {
                            if let Ok(mut e) = err2.lock() {
                                e.push(l.text.trim().to_owned());
                                let n = e.len();
                                if n > 6 {
                                    e.drain(..n - 6);
                                }
                            }
                            continue;
                        }
                        if let Some(ev) = parse_event(&l.text)
                            && ev.relevant()
                        {
                            f2.seen(ev);
                        }
                    }
                },
            );
            f2.pid.store(0, Ordering::Relaxed);
            let _ = tx.send(r);
        });
    if let Err(e) = spawned {
        return Ended::Failed(format!("could not start a thread: {e}"));
    }
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Ok(exit)) => {
                if let Some(why) = f.stopped() {
                    return Ended::Stopped(why);
                }
                let tail = stderr
                    .lock()
                    .map(|e| super::triage::clean_error(&e.join("\n")))
                    .unwrap_or_default();
                return Ended::Exited {
                    code: exit.code,
                    stderr: tail,
                };
            }
            Ok(Err(e)) => return Ended::Failed(e.to_string()),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if f.stopped().is_none() && f.quiet_for() > f.hidden_after {
                    f.stop(StopReason::Hidden);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Ended::Failed("the events thread ended".into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_parsed_and_probes_ignored() {
        let start = r#"{"Type":"container","Action":"start","Actor":{"ID":"abc","Attributes":{"name":"shop-web-1","com.docker.compose.project":"shop","image":"shop-web"}},"scope":"local","time":1793347200,"timeNano":1793347200000000000}"#;
        let e = parse_event(start).unwrap();
        assert_eq!(
            (e.action.as_str(), e.name.as_str()),
            ("start", "shop-web-1")
        );
        assert_eq!(e.project.as_deref(), Some("shop"));
        assert!(e.relevant());
        assert_eq!(e.text(), "shop-web-1 started");
        let health = r#"{"Type":"container","Action":"health_status: unhealthy","Actor":{"ID":"def","Attributes":{"name":"shop-db-1"}},"time":1}"#;
        let e = parse_event(health).unwrap();
        assert_eq!(
            (e.action.as_str(), e.detail.as_str()),
            ("health_status", "unhealthy")
        );
        assert_eq!(e.text(), "shop-db-1 became unhealthy");
        let probe = r#"{"Type":"container","Action":"exec_start: psql -c 'select 1' --password=hunter2","Actor":{"ID":"def","Attributes":{"name":"shop-db-1"}}}"#;
        let e = parse_event(probe).unwrap();
        assert!(!e.relevant());
        assert!(
            !format!("{e:?}").contains("hunter2"),
            "the probe command is dropped"
        );
        let die = r#"{"status":"die","id":"ghi","Type":"container","Actor":{"Attributes":{"name":"w","exitCode":"137"}}}"#;
        let e = parse_event(die).unwrap();
        assert_eq!(e.id, "ghi");
        assert_eq!(e.text(), "w stopped (exit 137)");
        assert!(parse_event(r#"{"Type":"network","Action":"connect"}"#).is_none());
        assert!(parse_event("not json").is_none());
    }

    /// A process that runs for a while without printing.
    fn long_running() -> ProcSpec {
        if cfg!(windows) {
            ProcSpec::new("ping", ".").args(["-n", "30", "127.0.0.1"])
        } else {
            ProcSpec::new("sleep", ".").arg("30")
        }
    }

    #[test]
    fn the_watchdog_stops_a_stream_nobody_draws() {
        let f = Follow::with_hidden_after(|| {}, Duration::from_millis(300));
        let started = Instant::now();
        let ended = follow(&long_running(), &f);
        assert_eq!(ended, Ended::Stopped(StopReason::Hidden));
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(f.pid().is_none(), "the process is gone");
    }

    #[test]
    fn stop_ends_it_and_a_quick_exit_is_reported() {
        let f = Follow::new(|| {});
        let f2 = f.clone();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            f2.stop(StopReason::Stopped);
        });
        assert_eq!(
            follow(&long_running(), &f),
            Ended::Stopped(StopReason::Stopped)
        );
        stopper.join().unwrap();
        let quick = Follow::new(|| {});
        let ended = follow(&ProcSpec::new("git", ".").arg("--version"), &quick);
        assert!(
            matches!(ended, Ended::Exited { code: Some(0), .. }),
            "{ended:?}"
        );
        let missing = Follow::new(|| {});
        let ended = follow(&ProcSpec::new("definitely-not-docker-xyz", "."), &missing);
        assert!(matches!(ended, Ended::Failed(_)), "{ended:?}");
    }
}
