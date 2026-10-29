//! The Containers tab (Ship page): the repository's Docker containers and
//! compose projects, through the `docker` CLI.
//!
//! - Loading: the tab's first show (and Refresh) reads the contexts, the
//!   chosen context's version, containers (`ps` + `inspect`), their images
//!   and compose projects, and the repository's compose files and
//!   Dockerfiles, in one background read logged with every command.
//! - Live updates: while the tab is drawn, one `docker events` stream per
//!   repository tab marks the listing dirty; the view re-lists after a
//!   quiet half second ([`frame`]). The view beats the stream's heartbeat
//!   every frame; without a beat for five seconds (another page, another
//!   repository tab, a minimised window) the stream's watchdog kills it,
//!   and it starts again when the tab is drawn. A context change, a page
//!   change and closing the repository tab stop it at once (the last two
//!   kill the process tree on the spot). If docker ends the stream itself,
//!   the tab polls every 10 s while it is shown and says so.
//! - Writes: start / stop / restart / pause / unpause, compose up / start
//!   / stop / restart / pull / build, logs and builds run in the console;
//!   kill, remove and compose down go through a confirmation showing the
//!   exact command, and so does every write against a remote context
//!   (saying "remote"). Shell opens a terminal tab running `docker exec`.
//! - Environment variables are names only, everywhere (the core models
//!   never hold a value).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hadion_core::devops::govern::docker::{ScanSummary, Scanner};
use hadion_core::devops::runtime::docker::commands::{
    self, BuildInputs, BuildPlan, ComposeOp, ComposeTarget, ContainerOp,
};
use hadion_core::devops::runtime::docker::compose::{ComposeConfig, RepoFiles};
use hadion_core::devops::runtime::docker::events::{self, Ended, Follow, StopReason};
use hadion_core::devops::runtime::docker::model::{Container, Context, Stats};
use hadion_core::devops::runtime::docker::scope::{self, RepoScope, Scoped};
use hadion_core::devops::runtime::docker::triage::{self, Problem};
use hadion_core::devops::runtime::docker::{self, Full, Listing, revision};
use hadion_core::devops::tools;

use super::RefCache;
use crate::devops::console::ConsoleRun;
use crate::devops::ctx::Ctx;
use crate::state::{RepoState, ToastKind};
use crate::theme::Tone;

/// Reload everything when the tab opens and the last full load is older.
pub const STALE_SECS: i64 = 120;
/// Re-list after the events went quiet for this long…
pub const DEBOUNCE: Duration = Duration::from_millis(500);
/// …or this long after the first one, whichever comes first.
pub const MAX_WAIT: Duration = Duration::from_secs(2);
/// Without a live stream, re-list this often while the tab is shown.
pub const POLL: Duration = Duration::from_secs(10);
/// Sample the selected running container's stats this often.
pub const STATS_EVERY: Duration = Duration::from_secs(3);
/// While live, draw at least this often (the stream's heartbeat).
pub const BEAT_EVERY: Duration = Duration::from_millis(1500);

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// What is selected in the list.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Sel {
    /// A compose project, by name.
    Project(String),
    /// A compose file of the repository that is not started (relative path).
    File(String),
    /// A container, by id.
    Container(String),
}

/// The running `docker events` of this tab.
pub struct Stream {
    pub id: u64,
    pub context: String,
    pub follow: Arc<Follow>,
    pub started: Instant,
}

impl Drop for Stream {
    /// A repository tab that closes, or an app that exits, drops its state:
    /// kill the process tree now (the stream's thread may not get to it).
    /// A stream that already ended has no process left, so this is free.
    fn drop(&mut self) {
        self.follow.kill_now(StopReason::Closed);
    }
}

/// How the tab stays current.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum LiveMode {
    /// No stream (not started yet, or stopped because the tab was hidden).
    #[default]
    Off,
    Live,
    /// The stream ended by itself: re-list every [`POLL`], with why.
    Polling(String),
}

#[derive(Default)]
pub struct Live {
    pub stream: Option<Stream>,
    /// Streams asked to stop, kept until their task reports back.
    retired: Vec<Stream>,
    pub mode: LiveMode,
    /// The events counter the last re-list consumed.
    seen: u64,
    first_dirty: Option<Instant>,
    last_noticed: Option<Instant>,
    /// Events seen in this session, all streams.
    pub total_events: u64,
}

impl Live {
    /// Stop the stream (it ends within ~30 ms on its own thread).
    pub fn stop(&mut self, why: StopReason) {
        if let Some(s) = self.stream.take() {
            s.follow.stop(why);
            self.retired.push(s);
        }
        if self.mode == LiveMode::Live {
            self.mode = LiveMode::Off;
        }
        self.first_dirty = None;
        self.last_noticed = None;
    }

    /// The last event the stream saw, with when.
    pub fn last_event(&self) -> Option<(events::Event, Instant)> {
        self.stream.as_ref().and_then(|s| s.follow.last())
    }
}

/// A compose project's or file's `compose config`.
#[derive(Default)]
pub struct ConfigState {
    pub loading: bool,
    pub result: Option<Result<ComposeConfig, String>>,
}

/// The last stats sample of a container.
#[derive(Default)]
pub struct StatsState {
    pub loading: bool,
    pub sample: Option<Result<Stats, String>>,
    pub at: Option<Instant>,
}

/// Where a build form came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildSource {
    /// A Dockerfile of the repository (relative path).
    Dockerfile(String),
    /// A compose service with `build:`.
    Service {
        project: String,
        service: String,
        /// The image compose runs for it (`image:`, else `<project>-<service>`).
        image: String,
    },
}

/// The build form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildForm {
    pub source: BuildSource,
    /// Relative to the repository root (or absolute).
    pub dockerfile: String,
    pub context_dir: String,
    pub tag: String,
    pub scan: bool,
    /// Trivy / Grype on PATH (looked up when the form opened).
    pub scanners: Vec<Scanner>,
}

/// What a console run of this tab was for.
#[derive(Clone, Debug)]
pub enum RunKind {
    /// A container or compose write: re-list when it ends.
    Write,
    Logs,
    Build {
        tag: String,
        scan: Option<Scanner>,
    },
    Scan {
        scanner: Scanner,
        report: PathBuf,
    },
}

/// A scan after a build.
#[derive(Clone, Debug)]
pub struct ScanResult {
    pub run: u64,
    pub tag: String,
    pub scanner: Scanner,
    pub result: Option<Result<ScanSummary, String>>,
}

/// The Containers tab.
#[derive(Default)]
pub struct State {
    /// docker is on PATH (`None` until the tab was opened).
    pub tool: Option<bool>,
    pub tool_version: Option<String>,
    pub loading: bool,
    pub listing_loading: bool,
    pub contexts: Option<Result<Vec<Context>, String>>,
    /// The chosen context (persisted); docker's current one when `None`.
    pub context: Option<String>,
    pub listing: Option<Listing>,
    pub files: RepoFiles,
    /// Real-name keys of the paths in the listing (computed off the UI
    /// thread), and the repository root's.
    pub keys: HashMap<String, String>,
    pub root_key: String,
    /// When the last full load arrived (seconds since the epoch).
    pub fetched: Option<i64>,
    /// When the last listing arrived.
    pub listed_at: Option<Instant>,
    pub load_ms: u64,
    /// Bumped on every context change: answers for an older one are dropped.
    pub generation: u64,
    /// Show the other containers of this Docker too (persisted).
    pub show_all: bool,
    pub filter: String,
    pub scoped: Option<Scoped>,
    scoped_at: Option<Option<Instant>>,
    pub selected: Option<Sel>,
    pub configs: HashMap<ComposeTarget, ConfigState>,
    pub stats: HashMap<String, StatsState>,
    /// The masked `inspect` of a container.
    pub inspect: Option<(String, Option<Result<String, String>>)>,
    pub live: Live,
    next_stream: u64,
    /// The console run in the bottom panel.
    pub console_run: Option<u64>,
    pub runs: HashMap<u64, RunKind>,
    pub build: Option<BuildForm>,
    pub scans: Vec<ScanResult>,
    /// "Also remove volumes" for Down.
    pub down_volumes: bool,
    /// "Force" for removing a running container.
    pub force_remove: bool,
    /// For the stream to wake the UI (set by the view).
    pub egui: Option<egui::Context>,
}

impl State {
    /// The context the tab shows.
    pub fn context_name(&self) -> Option<String> {
        if let Some(l) = &self.listing {
            return Some(l.context.clone());
        }
        let Some(Ok(list)) = &self.contexts else {
            return self.context.clone();
        };
        Some(docker::model::choose_context(
            list,
            self.context.as_deref(),
            std::env::var("DOCKER_CONTEXT").ok().as_deref(),
        ))
    }

    pub fn context_info(&self) -> Option<&Context> {
        let name = self.context_name()?;
        match &self.contexts {
            Some(Ok(list)) => list.iter().find(|c| c.name == name),
            _ => None,
        }
    }

    /// The chosen context's endpoint is on another machine.
    pub fn remote(&self) -> bool {
        self.context_info().is_some_and(Context::remote)
    }

    pub fn endpoint(&self) -> String {
        self.context_info()
            .map(|c| c.endpoint.clone())
            .unwrap_or_default()
    }

    pub fn containers(&self) -> &[Container] {
        self.listing
            .as_ref()
            .map(Listing::containers)
            .unwrap_or(&[])
    }

    pub fn container(&self, id: &str) -> Option<&Container> {
        self.containers().iter().find(|c| c.id == id)
    }

    /// The daemon answered the last listing.
    pub fn reachable(&self) -> bool {
        self.listing.as_ref().is_some_and(|l| l.fatal().is_none())
    }

    /// The problem of the last load, classified.
    pub fn problem(&self) -> Option<(Problem, String)> {
        if let Some(Err(e)) = &self.contexts
            && triage::classify(e, false) == Problem::NotInstalled
        {
            return Some((Problem::NotInstalled, e.clone()));
        }
        let e = self.listing.as_ref()?.fatal()?;
        Some((triage::classify(e, self.remote()), e.to_owned()))
    }

    /// The compose ls / label projects need the compose plugin.
    pub fn compose_problem(&self) -> Option<String> {
        match self.listing.as_ref().and_then(|l| l.projects.as_ref()) {
            Some(Err(e)) => Some(e.clone()),
            _ => None,
        }
    }

    /// Indexes of the repository's containers that need attention.
    pub fn troubled(&self) -> Vec<usize> {
        let Some(s) = &self.scoped else {
            return Vec::new();
        };
        let all = self.containers();
        s.repo_containers()
            .into_iter()
            .filter(|i| all.get(*i).is_some_and(Container::troubled))
            .collect()
    }

    /// The target of a project (`-p NAME`, its files and directory).
    pub fn project_target(&self, name: &str) -> Option<ComposeTarget> {
        let p = self.scoped.as_ref()?.project(name)?;
        Some(ComposeTarget {
            project: Some(p.name.clone()),
            files: p.config_files.clone(),
            project_dir: (!p.working_dir.is_empty()).then(|| p.working_dir.clone()),
        })
    }

    /// The target of a repository compose file that is not started.
    pub fn file_target(&self, workdir: &Path, rel: &str) -> Option<ComposeTarget> {
        let f = self.files.compose.iter().find(|f| f.rel == rel)?;
        let mut files = vec![workdir.join(&f.rel).to_string_lossy().into_owned()];
        files.extend(
            f.overrides
                .iter()
                .map(|o| workdir.join(o).to_string_lossy().into_owned()),
        );
        Some(ComposeTarget {
            project: None,
            files,
            project_dir: None,
        })
    }

    pub fn config(&self, t: &ComposeTarget) -> Option<&ConfigState> {
        self.configs.get(t)
    }
}

// ---------------------------------------------------------------------------
// The list
// ---------------------------------------------------------------------------

/// A row of the list pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// `This repository`, `Other containers on this Docker`.
    Group(&'static str, usize),
    Project {
        name: String,
        in_repo: bool,
    },
    /// A container (`nested` under its project).
    Container {
        index: usize,
        nested: bool,
    },
    /// A compose file of the repository that is not started.
    File(String),
    /// A count of hidden containers (click shows all).
    Hidden(usize),
    /// Nothing of the repository runs here.
    Empty,
}

fn matches_filter(c: &Container, needle: &str) -> bool {
    needle.is_empty()
        || c.name.to_lowercase().contains(needle)
        || c.image.to_lowercase().contains(needle)
        || c.compose
            .as_ref()
            .is_some_and(|i| i.service.to_lowercase().contains(needle))
}

/// The list in order (the same for the view and the `container N` /
/// `compose N` script commands).
pub fn rows(st: &State) -> Vec<Row> {
    let Some(s) = &st.scoped else {
        return Vec::new();
    };
    let all = st.containers();
    let needle = st.filter.trim().to_lowercase();
    let mut out = Vec::new();
    let mut mine = Vec::new();
    let project_rows = |p: &scope::Project, out: &mut Vec<Row>| {
        let kids: Vec<usize> = p
            .containers
            .iter()
            .copied()
            .filter(|i| all.get(*i).is_some_and(|c| matches_filter(c, &needle)))
            .collect();
        if !needle.is_empty() && kids.is_empty() && !p.name.to_lowercase().contains(&needle) {
            return;
        }
        out.push(Row::Project {
            name: p.name.clone(),
            in_repo: p.in_repo,
        });
        out.extend(kids.into_iter().map(|index| Row::Container {
            index,
            nested: true,
        }));
    };
    for p in &s.projects {
        project_rows(p, &mut mine);
    }
    for (i, _) in &s.standalone {
        if all.get(*i).is_some_and(|c| matches_filter(c, &needle)) {
            mine.push(Row::Container {
                index: *i,
                nested: false,
            });
        }
    }
    for fi in &s.not_started {
        if let Some(f) = st.files.compose.get(*fi)
            && (needle.is_empty() || f.rel.to_lowercase().contains(&needle))
        {
            mine.push(Row::File(f.rel.clone()));
        }
    }
    let count = s.repo_containers().len();
    out.push(Row::Group("This repository", count));
    if mine.is_empty() {
        out.push(Row::Empty);
    }
    out.extend(mine);
    let others: usize = s
        .other_projects
        .iter()
        .map(|p| p.containers.len())
        .sum::<usize>()
        + s.other_standalone.len();
    if others == 0 {
        return out;
    }
    if !st.show_all {
        out.push(Row::Hidden(others));
        return out;
    }
    out.push(Row::Group("Other containers on this Docker", others));
    for p in &s.other_projects {
        project_rows(p, &mut out);
    }
    for i in &s.other_standalone {
        if all.get(*i).is_some_and(|c| matches_filter(c, &needle)) {
            out.push(Row::Container {
                index: *i,
                nested: false,
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Actions and messages
// ---------------------------------------------------------------------------

/// A write the script driver (or a keyboard path) asks for on the
/// selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    Start,
    Stop,
    Restart,
    Pause,
    Unpause,
    Kill,
    Remove,
    Down,
    DownVolumes,
    Up,
    UpBuild,
    Build,
}

impl Ask {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "start" => Ask::Start,
            "stop" => Ask::Stop,
            "restart" => Ask::Restart,
            "pause" => Ask::Pause,
            "unpause" => Ask::Unpause,
            "kill" => Ask::Kill,
            "rm" | "remove" => Ask::Remove,
            "down" => Ask::Down,
            "down-volumes" => Ask::DownVolumes,
            "up" => Ask::Up,
            "up-build" => Ask::UpBuild,
            "build" => Ask::Build,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    CheckTool,
    /// Contexts, listing, files: everything again.
    Refresh,
    /// The listing of the current context again (events, polling).
    Relist,
    Context(String),
    ShowAll(bool),
    /// The list's filter (scripts; the view edits it in place).
    Filter(String),
    Select(Option<Sel>),
    /// The N-th container row (scripts).
    SelectContainerRow(usize),
    /// The N-th project or compose file row (scripts).
    SelectProjectRow(usize),
    /// Start following `docker events` (the view asks while it is drawn).
    LiveStart,
    Stats(String),
    Inspect(String),
    HideInspect,
    /// A container write: run it, or confirm first.
    Container {
        id: String,
        op: ContainerOp,
    },
    /// Run it (confirmed, or needing no confirmation).
    ContainerRun {
        context: String,
        id: String,
        name: String,
        op: ContainerOp,
    },
    /// A compose write on a project or a file: run it, or confirm first.
    Compose {
        target: ComposeTarget,
        label: String,
        op: ComposeOp,
    },
    ComposeRun {
        context: String,
        target: ComposeTarget,
        label: String,
        op: ComposeOp,
    },
    Logs(String),
    Shell(String),
    ShellRun {
        context: String,
        id: String,
        name: String,
    },
    DownVolumes(bool),
    ForceRemove(bool),
    /// Open the build form.
    BuildForm(BuildSource),
    /// The first Dockerfile's (or buildable service's) form (scripts).
    BuildFormFirst,
    /// The form's fields as edited.
    BuildEdit(BuildForm),
    BuildClose,
    /// Build with the form (confirm first on a remote context).
    Build,
    BuildRun {
        context: String,
        plan: BuildPlan,
        scan: Option<Scanner>,
    },
    HideConsole,
    /// A write on the selection (scripts: `docker-ask …`).
    Ask(Ask),
    /// Logs of the selected container, or of every service of the selected
    /// project (scripts).
    SelectedLogs,
    /// A shell in the selected container (scripts).
    SelectedShell,
    /// The masked `inspect` of the selected container (scripts).
    SelectedInspect,
}

pub enum Msg {
    Full {
        generation: u64,
        full: Box<Full>,
        keys: HashMap<String, String>,
        root: String,
    },
    Listed {
        generation: u64,
        listing: Box<Listing>,
        keys: HashMap<String, String>,
    },
    Config {
        generation: u64,
        target: ComposeTarget,
        result: Result<ComposeConfig, String>,
    },
    Stats {
        id: String,
        result: Result<Stats, String>,
    },
    Inspected {
        id: String,
        result: Result<String, String>,
    },
    EventsEnded {
        id: u64,
        context: String,
        ended: Ended,
        events: u64,
        ms: u64,
    },
    Scanned {
        run: u64,
        result: Result<Box<ScanSummary>, String>,
    },
}

fn n_of(n: usize, one: &str, many: &str) -> String {
    crate::format::plural(n, one, many)
}

fn listing_summary(l: &Listing) -> Result<String, String> {
    if let Some(e) = l.fatal() {
        return Err(format!("{}: {e}", l.context));
    }
    let c = l.containers();
    let running = c.iter().filter(|c| c.state.running()).count();
    let unhealthy = c.iter().filter(|c| c.unhealthy()).count();
    let mut s = format!(
        "{}: {} ({running} running{}), {}, {} in {}",
        l.context,
        n_of(c.len(), "container", "containers"),
        if unhealthy > 0 {
            format!(", {unhealthy} unhealthy")
        } else {
            String::new()
        },
        n_of(l.images.len(), "image", "images"),
        n_of(l.projects().len(), "compose project", "compose projects"),
        crate::format::millis(l.ms)
    );
    if let Some(Err(e)) = &l.projects {
        s.push_str(&format!(
            " (compose ls failed: {})",
            e.lines().next().unwrap_or(e)
        ));
    }
    Ok(s)
}

impl Msg {
    pub fn summary(&self) -> Result<String, String> {
        match self {
            Msg::Full { full, .. } => {
                let files = format!(
                    "{}, {} in the repository",
                    n_of(full.files.compose.len(), "compose file", "compose files"),
                    n_of(full.files.dockerfiles.len(), "Dockerfile", "Dockerfiles")
                );
                match (&full.contexts, &full.listing) {
                    (Err(e), _) => Err(e.clone()),
                    (Ok(ctx), Some(l)) => listing_summary(l).map(|s| {
                        format!("{} · {s} · {files}", n_of(ctx.len(), "context", "contexts"))
                    }),
                    (Ok(ctx), None) => Ok(format!(
                        "{} · {files}",
                        n_of(ctx.len(), "context", "contexts")
                    )),
                }
            }
            Msg::Listed { listing, .. } => listing_summary(listing),
            Msg::Config { result, .. } => match result {
                Ok(c) => Ok(format!(
                    "{}: {}",
                    if c.name.is_empty() {
                        "compose"
                    } else {
                        &c.name
                    },
                    n_of(c.services.len(), "service", "services")
                )),
                Err(e) => Err(e.clone()),
            },
            Msg::Stats { result, .. } => match result {
                Ok(s) => Ok(format!(
                    "{}: CPU {}, memory {}",
                    s.name,
                    s.cpu.map_or("?".into(), |c| format!("{c:.1}%")),
                    s.mem
                )),
                Err(e) => Err(e.clone()),
            },
            Msg::Inspected { result, .. } => match result {
                Ok(t) => Ok(format!(
                    "{} of JSON, environment values masked",
                    crate::format::bytes(t.len())
                )),
                Err(e) => Err(e.clone()),
            },
            Msg::EventsEnded {
                context,
                ended,
                events,
                ms,
                ..
            } => {
                let line = format!(
                    "{context}: {} in {}, {}",
                    n_of(*events as usize, "change", "changes"),
                    crate::format::millis(*ms),
                    ended.text()
                );
                match ended {
                    Ended::Failed(_) => Err(line),
                    _ => Ok(line),
                }
            }
            Msg::Scanned { result, .. } => match result {
                Ok(s) => Ok(format!(
                    "{} of {}: {} vulnerabilities ({} critical, {} high)",
                    s.scanner, s.image, s.total, s.counts[0], s.counts[1]
                )),
                Err(e) => Err(e.clone()),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

fn now_secs() -> i64 {
    crate::format::now_secs()
}

fn stale(at: Option<i64>) -> bool {
    at.is_none_or(|t| now_secs() - t > STALE_SECS)
}

/// Real-name keys of every path the scope rules compare (off the UI
/// thread: `canonicalize` touches the disk).
fn path_keys(listing: Option<&Listing>, files: &RepoFiles, dir: &Path) -> HashMap<String, String> {
    let mut paths: Vec<String> = Vec::new();
    if let Some(l) = listing {
        for c in l.containers() {
            if let Some(i) = &c.compose {
                paths.push(i.working_dir.clone());
                paths.extend(i.config_files.iter().cloned());
            }
        }
        for p in l.projects() {
            paths.extend(p.config_files.iter().cloned());
        }
    }
    for f in &files.compose {
        paths.push(dir.join(&f.rel).to_string_lossy().into_owned());
    }
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .filter(|p| !p.is_empty())
        .map(|p| {
            let k = scope::real_key(&p);
            (p, k)
        })
        .collect()
}

fn load_full(cx: &mut Ctx, st: &mut State) {
    if st.loading {
        return;
    }
    let wanted = st.context.clone();
    let generation = st.generation;
    st.loading = cx.read(
        "Docker: contexts, containers, images, compose projects",
        move |dir, log| {
            let full = docker::load_full(dir, wanted.as_deref(), &mut log.commands);
            let keys = path_keys(full.listing.as_ref(), &full.files, dir);
            Msg::Full {
                generation,
                full: Box::new(full),
                keys,
                root: scope::real_key(&dir.to_string_lossy()),
            }
            .into_runtime()
        },
    );
}

fn relist(cx: &mut Ctx, st: &mut State) {
    if st.listing_loading || st.loading {
        return;
    }
    let Some(context) = st.context_name() else {
        return;
    };
    let generation = st.generation;
    let files = st.files.clone();
    st.listing_loading = cx.read(
        format!("Docker: list containers ({context})"),
        move |dir, log| {
            let listing = docker::load_listing(dir, &context, &mut log.commands);
            let keys = path_keys(Some(&listing), &files, dir);
            Msg::Listed {
                generation,
                listing: Box::new(listing),
                keys,
            }
            .into_runtime()
        },
    );
}

fn load_config(cx: &mut Ctx, st: &mut State, target: ComposeTarget, label: &str) {
    let Some(context) = st.context_name() else {
        return;
    };
    let entry = st.configs.entry(target.clone()).or_default();
    if entry.loading || entry.result.is_some() {
        return;
    }
    let generation = st.generation;
    entry.loading = cx.read(
        format!("Docker: compose config of {label}"),
        move |dir, log| {
            let result = docker::load_config(dir, &context, &target, &mut log.commands);
            Msg::Config {
                generation,
                target,
                result,
            }
            .into_runtime()
        },
    );
}

fn load_stats(cx: &mut Ctx, st: &mut State, id: String) {
    let Some(context) = st.context_name() else {
        return;
    };
    let name = st
        .container(&id)
        .map(|c| c.name.clone())
        .unwrap_or_default();
    let entry = st.stats.entry(id.clone()).or_default();
    if entry.loading {
        return;
    }
    entry.loading = cx.read(format!("Docker: stats of {name}"), move |dir, log| {
        let result =
            docker::load_stats(dir, &context, std::slice::from_ref(&id), &mut log.commands)
                .and_then(|mut v| {
                    v.pop().ok_or_else(|| {
                        "docker stats printed nothing (the container stopped?)".to_owned()
                    })
                });
        Msg::Stats { id, result }.into_runtime()
    });
}

impl Msg {
    fn into_runtime(self) -> super::Msg {
        super::Msg::Docker(self)
    }
}

/// Start following `docker events` (no-op when it runs).
fn start_live(cx: &mut Ctx, st: &mut State) {
    if st.live.stream.is_some() || !st.reachable() {
        return;
    }
    let Some(context) = st.context_name() else {
        return;
    };
    let Some(dir) = cx.workdir.clone() else {
        return;
    };
    let egui = st.egui.clone();
    let follow = Follow::new(move || {
        if let Some(c) = &egui {
            c.request_repaint();
        }
    });
    st.next_stream += 1;
    let id = st.next_stream;
    let args = commands::events_args(&context);
    let line = commands::display(&args);
    let spec = commands::spec(&dir, args);
    let f2 = follow.clone();
    let ctx2 = context.clone();
    let started = cx.read(format!("Docker: live events ({context})"), move |_, log| {
        log.command(line);
        let t0 = Instant::now();
        let ended = events::follow(&spec, &f2);
        Msg::EventsEnded {
            id,
            context: ctx2,
            ended,
            events: f2.events(),
            ms: t0.elapsed().as_millis() as u64,
        }
        .into_runtime()
    });
    if started {
        st.live.stream = Some(Stream {
            id,
            context,
            follow,
            started: Instant::now(),
        });
        st.live.mode = LiveMode::Live;
        st.live.seen = 0;
        st.live.first_dirty = None;
        st.live.last_noticed = None;
        // Changes made while nobody followed are missing from the list.
        if st
            .listed_at
            .is_none_or(|t| t.elapsed() > Duration::from_secs(2))
        {
            relist(cx, st);
        }
    }
}

/// What a frame of the view should ask for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameNeeds {
    pub acts: Vec<Act>,
    /// Draw again within this long.
    pub repaint: Option<Duration>,
}

fn sooner(a: Option<Duration>, b: Duration) -> Option<Duration> {
    Some(a.map_or(b, |a| a.min(b)))
}

/// Called by the view every frame it draws the tab: beats the stream's
/// heartbeat, notices events, and says what to load (a debounced re-list,
/// a poll, the stream, a stats sample) and when to draw again.
pub fn frame(st: &mut State, now: Instant) -> FrameNeeds {
    let mut n = FrameNeeds::default();
    if st.tool != Some(true) || st.loading || st.listing.is_none() {
        return n;
    }
    if let Some(s) = &st.live.stream {
        s.follow.beat();
        n.repaint = sooner(n.repaint, BEAT_EVERY);
        let seen = s.follow.events();
        if seen > st.live.seen {
            st.live.total_events += seen - st.live.seen;
            st.live.seen = seen;
            st.live.first_dirty.get_or_insert(now);
            st.live.last_noticed = Some(now);
        }
    }
    if !st.reachable() {
        return n;
    }
    match &st.live.mode {
        LiveMode::Off if st.live.stream.is_none() => n.acts.push(Act::LiveStart),
        LiveMode::Polling(_) => {
            let due = st
                .listed_at
                .map_or(Duration::ZERO, |t| POLL.saturating_sub(now - t));
            if due.is_zero() && !st.listing_loading {
                n.acts.push(Act::Relist);
            } else {
                n.repaint = sooner(n.repaint, due.max(Duration::from_millis(200)));
            }
        }
        _ => {}
    }
    if let (Some(first), Some(last)) = (st.live.first_dirty, st.live.last_noticed) {
        let quiet = now.saturating_duration_since(last);
        let waited = now.saturating_duration_since(first);
        if (quiet >= DEBOUNCE || waited >= MAX_WAIT) && !st.listing_loading {
            n.acts.push(Act::Relist);
            st.live.first_dirty = None;
            st.live.last_noticed = None;
        } else {
            let left = DEBOUNCE
                .saturating_sub(quiet)
                .min(MAX_WAIT.saturating_sub(waited));
            n.repaint = sooner(n.repaint, left.max(Duration::from_millis(50)));
        }
    }
    if let Some(Sel::Container(id)) = &st.selected
        && st.container(id).is_some_and(|c| c.state.running())
    {
        let s = st.stats.get(id);
        let loading = s.is_some_and(|s| s.loading);
        let due = s
            .and_then(|s| s.at)
            .map_or(Duration::ZERO, |t| STATS_EVERY.saturating_sub(now - t));
        if due.is_zero() && !loading {
            n.acts.push(Act::Stats(id.clone()));
        } else if !loading {
            n.repaint = sooner(n.repaint, due.max(Duration::from_millis(100)));
        }
    }
    n
}

/// Recompute what belongs to the repository (after a listing, or when the
/// history reloaded).
pub fn rescope(
    st: &mut State,
    refs: &RefCache,
    remotes: Vec<String>,
    workdir: &Path,
    loaded_at: Option<Instant>,
) {
    let Some(l) = &st.listing else {
        st.scoped = None;
        return;
    };
    let scope = RepoScope {
        workdir: workdir.to_path_buf(),
        root: if st.root_key.is_empty() {
            scope::path_key(&workdir.to_string_lossy())
        } else {
            st.root_key.clone()
        },
        remotes,
    };
    let keys = &st.keys;
    let key = |p: &str| keys.get(p).cloned().unwrap_or_else(|| scope::path_key(p));
    st.scoped = Some(scope::classify(
        l.containers(),
        &l.images,
        l.projects(),
        &st.files,
        &scope,
        refs.idx.as_ref(),
        &key,
    ));
    st.scoped_at = Some(loaded_at);
    // A selection that is gone falls back to the overview.
    let keep = match &st.selected {
        Some(Sel::Container(id)) => st.container(id).is_some(),
        Some(Sel::Project(n)) => st.scoped.as_ref().is_some_and(|s| s.project(n).is_some()),
        Some(Sel::File(rel)) => st.files.compose.iter().any(|f| &f.rel == rel),
        None => true,
    };
    if !keep {
        st.selected = None;
    }
}

/// Rescope when the history reloaded since the last classification.
pub fn rescope_if_stale(
    st: &mut State,
    refs: &RefCache,
    remotes: impl FnOnce() -> Vec<String>,
    workdir: &Path,
    loaded_at: Option<Instant>,
) {
    if st.listing.is_some() && st.scoped_at != Some(loaded_at) {
        rescope(st, refs, remotes(), workdir, loaded_at);
    }
}

fn remotes_of(repo: &RepoState) -> Vec<String> {
    repo.snapshot
        .as_ref()
        .map(|s| s.remotes.iter().map(|r| r.url.clone()).collect())
        .unwrap_or_default()
}

fn rescope_repo(repo: &mut RepoState) {
    let remotes = remotes_of(repo);
    let RepoState {
        snapshot,
        loaded_at,
        devops,
        workdir,
        ..
    } = repo;
    let rt = &mut devops.runtime;
    rt.refs.refresh(snapshot.as_ref(), *loaded_at);
    rescope(&mut rt.docker, &rt.refs, remotes, workdir, *loaded_at);
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// `Context staging (remote: ssh://deploy@staging)`.
fn where_text(st: &State, context: &str) -> String {
    let ep = st.endpoint();
    if st.remote() {
        format!("Context {context} (remote: {ep}).")
    } else if ep.is_empty() {
        format!("Context {context}.")
    } else {
        format!("Context {context} ({ep}, this machine).")
    }
}

/// A note for the confirmation when safe mode will refuse the command.
fn safe_mode_note(args: &[String]) -> String {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match hadion_core::devops::safety::check(&docker::program(), &refs, None) {
        Ok(()) => String::new(),
        Err(_) => "\nSafe mode is on: Hadion will refuse to send this to the remote Docker (Settings → Safety).".to_owned(),
    }
}

fn run_console(
    cx: &mut Ctx,
    st: &mut State,
    title: String,
    spec: hadion_core::devops::proc::ProcSpec,
    kind: RunKind,
) -> u64 {
    let id = cx.console(title, spec);
    st.runs.insert(id, kind);
    st.console_run = Some(id);
    id
}

fn container_write(cx: &mut Ctx, st: &mut State, id: &str, op: ContainerOp) {
    let Some(context) = st.context_name() else {
        return;
    };
    let Some(c) = st.container(id) else {
        return;
    };
    let name = c.name.clone();
    let remote = st.remote();
    let run = Act::ContainerRun {
        context: context.clone(),
        id: id.to_owned(),
        name: name.clone(),
        op,
    };
    if !op.destructive() && !remote {
        apply_docker(cx, st, run, None);
        return;
    }
    let args = op.args(&context, id);
    let mut body = format!("{}\n\n{}", op.explain(), where_text(st, &context));
    if remote {
        body.push_str(" This changes a container on another machine.");
    }
    if let ContainerOp::Remove { force: true } = op {
        body.push_str(" It is running: force removal kills it first.");
    }
    body.push_str(" Hadion runs exactly the command below.");
    body.push_str(&safe_mode_note(&args));
    crate::devops::pages::explain::confirm(
        cx,
        format!(
            "{}{}?",
            op.title(&name),
            if remote { " on the remote Docker" } else { "" }
        ),
        body,
        if remote {
            format!("{} (remote)", op.verb())
        } else {
            op.verb().to_owned()
        },
        op.destructive(),
        vec![commands::display(&args)],
        super::Act::Docker(run).into(),
    );
}

fn compose_write(
    cx: &mut Ctx,
    st: &mut State,
    target: ComposeTarget,
    label: String,
    op: ComposeOp,
) {
    let Some(context) = st.context_name() else {
        return;
    };
    let remote = st.remote();
    let missing = !target.missing_files().is_empty();
    if missing && op.needs_files() {
        cx.toast(
            ToastKind::Error,
            format!(
                "{} needs the compose files, and {} no longer exists",
                op.title(&label),
                target.missing_files().join(", ")
            ),
        );
        return;
    }
    let run = Act::ComposeRun {
        context: context.clone(),
        target: target.clone(),
        label: label.clone(),
        op: op.clone(),
    };
    let reads_only = op == ComposeOp::Logs;
    if reads_only || (!op.destructive() && !remote) {
        apply_docker(cx, st, run, None);
        return;
    }
    let args = op.args(&context, &target, !missing);
    let mut body = op.explain().to_owned();
    if let ComposeOp::Down { volumes: true, .. } = op
        && let Some(Some(Ok(cfg))) = st.config(&target).map(|c| c.result.as_ref())
        && !cfg.volumes.is_empty()
    {
        body.push_str(&format!(
            " The compose file names these volumes: {}.",
            cfg.volumes.join(", ")
        ));
    }
    body.push_str(&format!("\n\n{}", where_text(st, &context)));
    if remote {
        body.push_str(" This changes containers on another machine.");
    }
    body.push_str(" Hadion runs exactly the command below.");
    body.push_str(&safe_mode_note(&args));
    let confirm = match &op {
        ComposeOp::Down { volumes: true, .. } => "Down and delete volumes".to_owned(),
        ComposeOp::Down { .. } => "Down".to_owned(),
        other => other
            .title(&label)
            .split(' ')
            .next()
            .unwrap_or("Run")
            .to_owned(),
    };
    crate::devops::pages::explain::confirm(
        cx,
        format!(
            "{}{}?",
            op.title(&label),
            if remote { " on the remote Docker" } else { "" }
        ),
        body,
        if remote {
            format!("{confirm} (remote)")
        } else {
            confirm
        },
        op.destructive(),
        vec![
            commands::display(&args),
            format!(
                "  in {}",
                target
                    .cwd(&cx.workdir.clone().unwrap_or_default())
                    .display()
            ),
        ],
        super::Act::Docker(run).into(),
    );
}

fn head_info(repo: &RepoState) -> Option<(String, String, Option<String>, bool)> {
    let s = repo.snapshot.as_ref()?;
    let head = s.head.target()?;
    let full = head.to_string();
    let short = full[..7].to_owned();
    let tag = s
        .tags
        .iter()
        .filter(|t| t.target == head)
        .map(|t| t.name.clone())
        .max_by(|a, b| hadion_core::devops::runtime::matching::version_cmp(a, b));
    Some((full, short, tag, !s.status.is_clean()))
}

/// The `org.opencontainers.image.source` of this repository: the https URL
/// of `origin` (else of the first remote that has one), without
/// credentials.
pub fn source_url(repo: &RepoState) -> Option<String> {
    let s = repo.snapshot.as_ref()?;
    let origin = s.remotes.iter().filter(|r| r.name == "origin");
    let others = s.remotes.iter().filter(|r| r.name != "origin");
    origin
        .chain(others)
        .find_map(|r| revision::https_url(&r.url))
}

/// Relative to the repository when inside it, `/` separators.
fn rel_to(workdir: &Path, p: &str) -> String {
    let path = Path::new(p);
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workdir.join(path)
    };
    match abs.strip_prefix(workdir) {
        Ok(r) => r.to_string_lossy().replace('\\', "/"),
        Err(_) => p.replace('\\', "/"),
    }
}

fn open_build_form(repo: &mut RepoState, source: BuildSource) {
    let name = repo
        .snapshot
        .as_ref()
        .map(|s| s.name.clone())
        .unwrap_or_else(|| repo.name.clone());
    let (_, short, _, dirty) = head_info(repo).unwrap_or_default();
    let workdir = repo.workdir.clone();
    let st = &mut repo.devops.runtime.docker;
    let (dockerfile, context_dir, tag) = match &source {
        BuildSource::Dockerfile(rel) => {
            let dir = rel
                .rsplit_once('/')
                .map_or(String::new(), |(d, _)| d.to_owned());
            (
                rel.clone(),
                if dir.is_empty() { ".".into() } else { dir },
                commands::default_tag(
                    &name,
                    if short.is_empty() { "latest" } else { &short },
                    dirty,
                ),
            )
        }
        BuildSource::Service {
            project,
            service,
            image,
        } => {
            let cfg = st
                .project_target(project)
                .and_then(|t| st.configs.get(&t))
                .and_then(|c| c.result.as_ref())
                .and_then(|r| r.as_ref().ok())
                .and_then(|c| c.service(service).cloned());
            let (ctx, df) = cfg
                .and_then(|s| s.build)
                .map(|b| {
                    let ctx_rel = rel_to(&workdir, &b.context);
                    let df_path = Path::new(&b.dockerfile);
                    let df = if df_path.is_absolute() {
                        rel_to(&workdir, &b.dockerfile)
                    } else if ctx_rel == "." || ctx_rel.is_empty() {
                        b.dockerfile.replace('\\', "/")
                    } else {
                        format!("{ctx_rel}/{}", b.dockerfile.replace('\\', "/"))
                    };
                    (
                        if ctx_rel.is_empty() {
                            ".".into()
                        } else {
                            ctx_rel
                        },
                        df,
                    )
                })
                .unwrap_or_else(|| (".".into(), "Dockerfile".into()));
            (df, ctx, image.clone())
        }
    };
    let scanners: Vec<Scanner> = [Scanner::Trivy, Scanner::Grype]
        .into_iter()
        .filter(|s| tools::available(s.tool()))
        .collect();
    st.build = Some(BuildForm {
        source,
        dockerfile,
        context_dir,
        tag,
        scan: false,
        scanners,
    });
}

fn build(cx: &mut Ctx, repo: &mut RepoState) {
    let Some((head, _, version, dirty)) = head_info(repo) else {
        cx.toast(
            ToastKind::Error,
            "The repository has no commit to label the image with",
        );
        return;
    };
    let source = source_url(repo);
    let st = &mut repo.devops.runtime.docker;
    let Some(form) = st.build.clone() else {
        return;
    };
    let Some(context) = st.context_name() else {
        return;
    };
    let tag = form.tag.trim().to_owned();
    if tag.is_empty() || tag.contains(char::is_whitespace) || tag.starts_with('-') {
        cx.toast(ToastKind::Error, "Enter an image tag such as shop:1a2b3c4");
        return;
    }
    let plan = BuildPlan {
        dockerfile: form.dockerfile.trim().to_owned(),
        context_dir: form.context_dir.trim().to_owned(),
        tag,
        labels: commands::build_labels(&BuildInputs {
            head,
            source,
            version,
            dirty,
            created: hadion_core::devops::ci::rfc3339(now_secs()),
        }),
    };
    let scan = form.scan.then(|| form.scanners.first().copied()).flatten();
    let run = Act::BuildRun {
        context: context.clone(),
        plan: plan.clone(),
        scan,
    };
    if !st.remote() {
        apply_docker(cx, st, run, Some(&repo.workdir.clone()));
        return;
    }
    let args = plan.args(&context);
    let body = format!(
        "docker builds the image on the remote Docker, sending the build context ({}) over the network.\n\n{} This changes images on another machine. Hadion runs exactly the command below.{}",
        plan.context_dir,
        where_text(st, &context),
        safe_mode_note(&args)
    );
    crate::devops::pages::explain::confirm(
        cx,
        format!("Build {} on the remote Docker?", plan.tag),
        body,
        "Build (remote)",
        false,
        vec![commands::display(&args)],
        super::Act::Docker(run).into(),
    );
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

/// The tab was opened.
pub fn on_tab_shown(cx: &mut Ctx, repo: &mut RepoState) {
    let st = &mut repo.devops.runtime.docker;
    if st.tool.is_none() {
        st.tool = Some(docker::available());
    }
    if st.tool == Some(true) && (st.listing.is_none() || stale(st.fetched)) {
        load_full(cx, st);
    }
}

/// The tab is no longer shown (another Ship tab, another page): stop the
/// stream now rather than when the watchdog notices.
pub fn on_tab_hidden(repo: &mut RepoState) {
    repo.devops.runtime.docker.live.stop(StopReason::Hidden);
}

fn select(cx: &mut Ctx, repo: &mut RepoState, sel: Option<Sel>) {
    let workdir = repo.workdir.clone();
    let st = &mut repo.devops.runtime.docker;
    if st.selected != sel {
        st.inspect = None;
        st.force_remove = false;
    }
    st.selected = sel.clone();
    match sel {
        Some(Sel::Project(name)) => {
            if let Some(t) = st.project_target(&name)
                && t.missing_files().is_empty()
                && !t.files.is_empty()
            {
                load_config(cx, st, t, &name);
            }
        }
        Some(Sel::File(rel)) => {
            if let Some(t) = st.file_target(&workdir, &rel) {
                load_config(cx, st, t, &rel);
            }
        }
        Some(Sel::Container(id)) => {
            if let Some(name) = st
                .container(&id)
                .and_then(|c| c.compose.as_ref())
                .map(|c| c.project.clone())
                && let Some(t) = st.project_target(&name)
                && t.missing_files().is_empty()
                && !t.files.is_empty()
            {
                load_config(cx, st, t, &name);
            }
        }
        None => {}
    }
}

/// Apply an act that needs only the tab's state (and the work dir).
fn apply_docker(cx: &mut Ctx, st: &mut State, act: Act, workdir: Option<&Path>) {
    let dir = workdir
        .map(Path::to_path_buf)
        .or_else(|| cx.workdir.clone())
        .unwrap_or_else(|| PathBuf::from("."));
    match act {
        Act::ContainerRun {
            context,
            id,
            name,
            op,
        } => {
            let spec = commands::spec(&dir, op.args(&context, &id));
            run_console(cx, st, op.title(&name), spec, RunKind::Write);
        }
        Act::ComposeRun {
            context,
            target,
            label,
            op,
        } => {
            let with_files = target.missing_files().is_empty();
            let spec = commands::spec(&target.cwd(&dir), op.args(&context, &target, with_files));
            let kind = if op == ComposeOp::Logs {
                RunKind::Logs
            } else {
                RunKind::Write
            };
            run_console(cx, st, op.title(&label), spec, kind);
        }
        Act::BuildRun {
            context,
            plan,
            scan,
        } => {
            let spec = commands::spec(&dir, plan.args(&context));
            run_console(
                cx,
                st,
                format!("Build {}", plan.tag),
                spec,
                RunKind::Build {
                    tag: plan.tag.clone(),
                    scan,
                },
            );
        }
        Act::ShellRun { context, id, name } => {
            let args = commands::exec_shell_args(&context, &id);
            cx.action(crate::devops::terminal::Act::Exec {
                title: format!("{name} · docker exec"),
                program: docker::program(),
                args,
                cwd: Some(dir),
            });
        }
        _ => {}
    }
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::CheckTool => {
            tools::refresh();
            let st = &mut repo.devops.runtime.docker;
            st.tool = Some(docker::available());
            if st.tool == Some(true) {
                load_full(cx, st);
            }
        }
        Act::Refresh => {
            let st = &mut repo.devops.runtime.docker;
            st.tool = Some(docker::available());
            st.configs.retain(|_, c| c.loading);
            st.stats.clear();
            // A fresh start for the stream too (it may have fallen back to
            // polling).
            st.live.stop(StopReason::Stopped);
            st.live.mode = LiveMode::Off;
            if st.tool == Some(true) {
                load_full(cx, st);
            }
        }
        Act::Relist => relist(cx, &mut repo.devops.runtime.docker),
        Act::Context(c) => {
            let st = &mut repo.devops.runtime.docker;
            if st.context_name().as_deref() == Some(c.as_str()) && st.listing.is_some() {
                return;
            }
            st.live.stop(StopReason::Stopped);
            st.live.mode = LiveMode::Off;
            st.context = Some(c);
            st.generation += 1;
            st.listing = None;
            st.scoped = None;
            st.selected = None;
            st.configs.clear();
            st.stats.clear();
            st.inspect = None;
            st.loading = false;
            st.listing_loading = false;
            load_full(cx, st);
        }
        Act::ShowAll(on) => repo.devops.runtime.docker.show_all = on,
        Act::Filter(text) => repo.devops.runtime.docker.filter = text,
        Act::Select(sel) => select(cx, repo, sel),
        Act::SelectContainerRow(n) => {
            let pick = rows(&repo.devops.runtime.docker)
                .into_iter()
                .filter_map(|r| match r {
                    Row::Container { index, .. } => Some(index),
                    _ => None,
                })
                .nth(n)
                .and_then(|i| repo.devops.runtime.docker.containers().get(i))
                .map(|c| c.id.clone());
            if let Some(id) = pick {
                select(cx, repo, Some(Sel::Container(id)));
            }
        }
        Act::SelectProjectRow(n) => {
            let pick = rows(&repo.devops.runtime.docker)
                .into_iter()
                .filter_map(|r| match r {
                    Row::Project { name, .. } => Some(Sel::Project(name)),
                    Row::File(rel) => Some(Sel::File(rel)),
                    _ => None,
                })
                .nth(n);
            if pick.is_some() {
                select(cx, repo, pick);
            }
        }
        Act::LiveStart => start_live(cx, &mut repo.devops.runtime.docker),
        Act::Stats(id) => load_stats(cx, &mut repo.devops.runtime.docker, id),
        Act::Inspect(id) => {
            let st = &mut repo.devops.runtime.docker;
            let Some(context) = st.context_name() else {
                return;
            };
            let name = st
                .container(&id)
                .map(|c| c.name.clone())
                .unwrap_or_default();
            st.inspect = Some((id.clone(), None));
            cx.read(
                format!("Docker: inspect {name} (environment values masked)"),
                move |dir, log| {
                    let result = docker::inspect_masked(dir, &context, &id, &mut log.commands);
                    Msg::Inspected { id, result }.into_runtime()
                },
            );
        }
        Act::HideInspect => repo.devops.runtime.docker.inspect = None,
        Act::Container { id, op } => container_write(cx, &mut repo.devops.runtime.docker, &id, op),
        Act::Compose { target, label, op } => {
            compose_write(cx, &mut repo.devops.runtime.docker, target, label, op)
        }
        a @ (Act::ContainerRun { .. }
        | Act::ComposeRun { .. }
        | Act::BuildRun { .. }
        | Act::ShellRun { .. }) => {
            let dir = repo.workdir.clone();
            apply_docker(cx, &mut repo.devops.runtime.docker, a, Some(&dir));
        }
        Act::Logs(id) => {
            let dir = repo.workdir.clone();
            let st = &mut repo.devops.runtime.docker;
            let (Some(context), Some(c)) = (st.context_name(), st.container(&id)) else {
                return;
            };
            let title = format!("Logs · {} · following", c.name);
            let args = commands::logs_args(&context, &id, true, commands::LOG_TAIL, true);
            let spec = commands::spec(&dir, args);
            run_console(cx, st, title, spec, RunKind::Logs);
        }
        Act::Shell(id) => {
            let st = &mut repo.devops.runtime.docker;
            let (Some(context), Some(c)) = (st.context_name(), st.container(&id)) else {
                return;
            };
            let name = c.name.clone();
            if !c.state.running() {
                cx.toast(
                    ToastKind::Info,
                    format!("{name} is not running: start it to open a shell"),
                );
                return;
            }
            let args = commands::exec_shell_args(&context, &id);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            // The terminal is not covered by safe mode: check here.
            if let Err(e) = hadion_core::devops::safety::check(&docker::program(), &refs, None) {
                cx.toast(ToastKind::Error, e);
                return;
            }
            let run = Act::ShellRun {
                context: context.clone(),
                id: id.clone(),
                name: name.clone(),
            };
            if !st.remote() {
                let dir = repo.workdir.clone();
                apply_docker(cx, st, run, Some(&dir));
                return;
            }
            crate::devops::pages::explain::confirm(
                cx,
                format!("Open a shell in {name} on the remote Docker?"),
                format!(
                    "A terminal tab runs bash (or sh) inside the container; what you type there runs on the remote machine.\n\n{}",
                    where_text(st, &context)
                ),
                "Open shell (remote)",
                false,
                vec![commands::display(&args)],
                super::Act::Docker(run).into(),
            );
        }
        Act::DownVolumes(on) => repo.devops.runtime.docker.down_volumes = on,
        Act::ForceRemove(on) => repo.devops.runtime.docker.force_remove = on,
        Act::BuildForm(source) => open_build_form(repo, source),
        Act::BuildFormFirst => {
            let st = &repo.devops.runtime.docker;
            let source = st
                .files
                .dockerfiles
                .first()
                .map(|d| BuildSource::Dockerfile(d.clone()));
            if let Some(s) = source {
                open_build_form(repo, s);
            }
        }
        Act::BuildEdit(form) => repo.devops.runtime.docker.build = Some(form),
        Act::BuildClose => repo.devops.runtime.docker.build = None,
        Act::Build => build(cx, repo),
        Act::HideConsole => repo.devops.runtime.docker.console_run = None,
        Act::Ask(kind) => ask(cx, repo, kind),
        Act::SelectedLogs | Act::SelectedShell | Act::SelectedInspect => {
            let st = &repo.devops.runtime.docker;
            let next = match (&st.selected, &act) {
                (Some(Sel::Container(id)), Act::SelectedLogs) => Some(Act::Logs(id.clone())),
                (Some(Sel::Container(id)), Act::SelectedShell) => Some(Act::Shell(id.clone())),
                (Some(Sel::Container(id)), Act::SelectedInspect) => Some(Act::Inspect(id.clone())),
                (Some(Sel::Project(n)), Act::SelectedLogs) => {
                    st.project_target(n).map(|target| Act::Compose {
                        target,
                        label: n.clone(),
                        op: ComposeOp::Logs,
                    })
                }
                _ => None,
            };
            if let Some(a) = next {
                apply(cx, repo, a);
            }
        }
    }
}

/// A write on the selection (scripts).
fn ask(cx: &mut Ctx, repo: &mut RepoState, kind: Ask) {
    let workdir = repo.workdir.clone();
    let st = &repo.devops.runtime.docker;
    let target = match &st.selected {
        Some(Sel::Project(n)) => st.project_target(n).map(|t| (t, n.clone())),
        Some(Sel::File(rel)) => st.file_target(&workdir, rel).map(|t| (t, rel.clone())),
        _ => None,
    };
    let container = match &st.selected {
        Some(Sel::Container(id)) => st.container(id).map(|c| (c.id.clone(), c.state.up())),
        _ => None,
    };
    let compose_op = match kind {
        Ask::Down => Some(ComposeOp::Down {
            volumes: false,
            remove_orphans: false,
        }),
        Ask::DownVolumes => Some(ComposeOp::Down {
            volumes: true,
            remove_orphans: false,
        }),
        Ask::Up => Some(ComposeOp::Up {
            build: false,
            service: None,
        }),
        Ask::UpBuild => Some(ComposeOp::Up {
            build: true,
            service: None,
        }),
        Ask::Build => Some(ComposeOp::Build(None)),
        Ask::Start if target.is_some() => Some(ComposeOp::Start(None)),
        Ask::Stop if target.is_some() => Some(ComposeOp::Stop(None)),
        Ask::Restart if target.is_some() => Some(ComposeOp::Restart(None)),
        _ => None,
    };
    if let (Some(op), Some((t, label))) = (compose_op, target) {
        if let ComposeOp::Down { volumes, .. } = op {
            repo.devops.runtime.docker.down_volumes = volumes;
        }
        apply(
            cx,
            repo,
            Act::Compose {
                target: t,
                label,
                op,
            },
        );
        return;
    }
    let Some((id, up)) = container else {
        return;
    };
    let op = match kind {
        Ask::Start => ContainerOp::Start,
        Ask::Stop => ContainerOp::Stop,
        Ask::Restart => ContainerOp::Restart,
        Ask::Pause => ContainerOp::Pause,
        Ask::Unpause => ContainerOp::Unpause,
        Ask::Kill => ContainerOp::Kill,
        Ask::Remove => ContainerOp::Remove { force: up },
        _ => return,
    };
    apply(cx, repo, Act::Container { id, op });
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    match msg {
        Msg::Full {
            generation,
            full,
            keys,
            root,
        } => {
            let st = &mut repo.devops.runtime.docker;
            st.loading = false;
            if generation != st.generation {
                return;
            }
            let full = *full;
            st.fetched = Some(now_secs());
            st.load_ms = full.ms;
            if full.tool.is_some() {
                st.tool_version = full.tool;
            }
            st.contexts = Some(full.contexts);
            st.files = full.files;
            st.keys = keys;
            st.root_key = root;
            if let Some(l) = full.listing {
                if st
                    .live
                    .stream
                    .as_ref()
                    .is_some_and(|s| s.context != l.context)
                {
                    st.live.stop(StopReason::Stopped);
                }
                st.listing = Some(l);
                st.listed_at = Some(Instant::now());
            } else {
                st.listing = None;
            }
            rescope_repo(repo);
        }
        Msg::Listed {
            generation,
            listing,
            keys,
        } => {
            let st = &mut repo.devops.runtime.docker;
            st.listing_loading = false;
            if generation != st.generation {
                return;
            }
            st.keys.extend(keys);
            let fatal = listing.fatal().is_some();
            st.listing = Some(*listing);
            st.listed_at = Some(Instant::now());
            if fatal {
                st.live.stop(StopReason::Stopped);
            }
            rescope_repo(repo);
        }
        Msg::Config {
            generation,
            target,
            result,
        } => {
            let st = &mut repo.devops.runtime.docker;
            if generation != st.generation {
                st.configs.remove(&target);
                return;
            }
            let e = st.configs.entry(target).or_default();
            e.loading = false;
            e.result = Some(result);
        }
        Msg::Stats { id, result } => {
            let e = repo.devops.runtime.docker.stats.entry(id).or_default();
            e.loading = false;
            e.at = Some(Instant::now());
            // Keep the last good sample while a new one failed.
            if result.is_ok() || e.sample.is_none() {
                e.sample = Some(result);
            }
        }
        Msg::Inspected { id, result } => {
            let st = &mut repo.devops.runtime.docker;
            if st.inspect.as_ref().is_some_and(|(i, _)| *i == id) {
                st.inspect = Some((id, Some(result)));
            }
        }
        Msg::EventsEnded { id, ended, .. } => {
            let live = &mut repo.devops.runtime.docker.live;
            live.retired.retain(|s| s.id != id);
            if live.stream.as_ref().is_some_and(|s| s.id == id) {
                live.stream = None;
                live.first_dirty = None;
                live.last_noticed = None;
                live.mode = match &ended {
                    Ended::Stopped(_) => LiveMode::Off,
                    Ended::Exited { stderr, code } => LiveMode::Polling(if stderr.is_empty() {
                        format!(
                            "docker events ended ({})",
                            code.map_or("no exit code".into(), |c| format!("exit {c}"))
                        )
                    } else {
                        format!("docker events ended: {stderr}")
                    }),
                    Ended::Failed(e) => {
                        LiveMode::Polling(format!("docker events could not start: {e}"))
                    }
                };
                if matches!(ended, Ended::Exited { .. } | Ended::Failed(_)) {
                    relist(cx, &mut repo.devops.runtime.docker);
                }
            }
        }
        Msg::Scanned { run, result } => {
            if let Some(s) = repo
                .devops
                .runtime
                .docker
                .scans
                .iter_mut()
                .find(|s| s.run == run)
            {
                s.result = Some(result.map(|b| *b));
            }
        }
    }
}

/// A console run ended: re-list after a write, scan after a build, read a
/// scan's report.
pub fn on_console_done(cx: &mut Ctx, repo: &mut RepoState, run: &ConsoleRun) {
    let workdir = repo.workdir.clone();
    let st = &mut repo.devops.runtime.docker;
    let Some(kind) = st.runs.get(&run.id).cloned() else {
        return;
    };
    match kind {
        RunKind::Write => relist(cx, st),
        RunKind::Logs => {}
        RunKind::Build { tag, scan } => {
            relist(cx, st);
            if let Some(scanner) = scan.filter(|_| run.succeeded()) {
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let report = scanner.report_path(stamp);
                let spec = scanner.spec(&tag, &report, &workdir);
                let id = run_console(
                    cx,
                    st,
                    format!("{} scan of {tag}", scanner.label()),
                    spec,
                    RunKind::Scan { scanner, report },
                );
                st.scans.insert(
                    0,
                    ScanResult {
                        run: id,
                        tag,
                        scanner,
                        result: None,
                    },
                );
                st.scans.truncate(6);
            }
        }
        RunKind::Scan { scanner, report } => {
            if !run.succeeded() {
                let why = match &run.exit {
                    Some(Err(e)) => e.clone(),
                    _ => format!("{}\n{}", run.status_text(), run.tail(12)),
                };
                if let Some(s) = st.scans.iter_mut().find(|s| s.run == run.id) {
                    s.result = Some(Err(why));
                }
                let _ = std::fs::remove_file(&report);
                return;
            }
            let id = run.id;
            cx.read(
                format!("Read the {} report", scanner.label()),
                move |_, log| {
                    log.command(format!("read {}", report.display()));
                    let r = std::fs::read(&report)
                        .map_err(|e| format!("Could not read {}: {e}", report.display()))
                        .and_then(|b| scanner.parse(&b).map(Box::new).map_err(|e| e.to_string()));
                    let _ = std::fs::remove_file(&report);
                    Msg::Scanned { run: id, result: r }.into_runtime()
                },
            );
        }
    }
}

/// `HADION_SCRIPT` commands: `docker-context NAME`, `docker-all on|off`,
/// `container N`, `compose N`, `docker-refresh`, `docker-relist`,
/// `docker-ask start|stop|restart|pause|unpause|kill|rm|down|down-volumes|up|up-build|build`,
/// `docker-logs`, `docker-shell`, `docker-inspect`, `docker-build-form`,
/// `docker-build` (the form's Build), `docker-build-close`, `docker-overview`, `docker-filter
/// TEXT`, `docker-hide-console`.
pub fn script(cmd: &str, arg: &str) -> Option<Act> {
    let n = arg.trim().parse::<usize>().ok();
    Some(match cmd {
        "docker-context" => Act::Context(arg.trim().to_owned()),
        "docker-all" => Act::ShowAll(!matches!(arg.trim(), "off" | "0" | "false")),
        "container" => Act::SelectContainerRow(n?),
        "compose" => Act::SelectProjectRow(n?),
        "docker-refresh" => Act::Refresh,
        "docker-relist" => Act::Relist,
        "docker-ask" => Act::Ask(Ask::parse(arg)?),
        "docker-logs" => Act::SelectedLogs,
        "docker-shell" => Act::SelectedShell,
        "docker-inspect" => Act::SelectedInspect,
        "docker-build-form" => Act::BuildFormFirst,
        "docker-build" => Act::Build,
        "docker-build-close" => Act::BuildClose,
        "docker-filter" => Act::Filter(arg.trim().to_owned()),
        "docker-overview" => Act::Select(None),
        "docker-hide-console" => Act::HideConsole,
        _ => return None,
    })
}

/// The rail's dot: a container of this repository that is unhealthy,
/// restarting, or ended with an error after running (loaded data only).
pub fn rail_badge(st: &State) -> Option<(Tone, String)> {
    let all = st.containers();
    let bad: Vec<&Container> = st
        .troubled()
        .into_iter()
        .filter_map(|i| all.get(i))
        .collect();
    let first = bad.first()?;
    let why = |c: &Container| -> String {
        if c.unhealthy() {
            "unhealthy".into()
        } else if c.state == docker::model::State::Restarting {
            "restarting".into()
        } else if let docker::model::State::Exited(code) = c.state {
            format!("exited ({code})")
        } else {
            c.state.label().to_lowercase()
        }
    };
    let tone = if bad
        .iter()
        .any(|c| c.unhealthy() || c.state == docker::model::State::Restarting)
    {
        Tone::Danger
    } else {
        Tone::Warning
    };
    let more = if bad.len() > 1 {
        format!(" (and {} more)", bad.len() - 1)
    } else {
        String::new()
    };
    Some((
        tone,
        format!("container {} {}{more}", first.name, why(first)),
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use hadion_core::devops::runtime::docker::{compose, model};

    pub(crate) fn listing() -> Listing {
        let containers = model::parse_inspect(include_bytes!(
            "../../../../hadion-core/src/devops/runtime/docker/fixtures/inspect.json"
        ))
        .unwrap();
        Listing {
            context: "desktop-linux".into(),
            version: Ok(model::Version {
                client: "29.6.2".into(),
                server: Some(model::Server::default()),
                ..model::Version::default()
            }),
            client: Some("29.6.2".into()),
            containers: Some(Ok(containers)),
            images: Vec::new(),
            images_error: None,
            projects: Some(Ok(Vec::new())),
            ms: 5,
        }
    }

    pub(crate) fn loaded() -> State {
        let mut st = State {
            tool: Some(true),
            listing: Some(listing()),
            files: RepoFiles {
                compose: vec![compose::ComposeFile {
                    rel: "extras/docker-compose.yml".into(),
                    overrides: vec![],
                }],
                dockerfiles: vec!["web/Dockerfile".into()],
            },
            contexts: Some(Ok(vec![Context {
                name: "desktop-linux".into(),
                endpoint: "npipe:////./pipe/dockerDesktopLinuxEngine".into(),
                current: true,
                ..Context::default()
            }])),
            ..State::default()
        };
        rescope(
            &mut st,
            &RefCache::default(),
            vec![],
            Path::new("C:/repo"),
            None,
        );
        st
    }

    #[test]
    fn rows_group_the_repository_first() {
        let mut st = loaded();
        let r = rows(&st);
        assert_eq!(r[0], Row::Group("This repository", 5));
        assert_eq!(
            r[1],
            Row::Project {
                name: "devtools".into(),
                in_repo: true
            }
        );
        assert!(matches!(r[2], Row::Container { nested: true, .. }));
        assert!(r.contains(&Row::File("extras/docker-compose.yml".into())));
        assert_eq!(r.last(), Some(&Row::Hidden(1)), "never-started is not ours");
        st.show_all = true;
        let r = rows(&st);
        assert!(r.contains(&Row::Group("Other containers on this Docker", 1)));
        st.filter = "worker".into();
        let r = rows(&st);
        let containers = r
            .iter()
            .filter(|r| matches!(r, Row::Container { .. }))
            .count();
        assert_eq!(containers, 1);
    }

    #[test]
    fn the_badge_names_the_trouble() {
        let st = loaded();
        let (tone, text) = rail_badge(&st).unwrap();
        assert_eq!(tone, Tone::Danger);
        assert!(text.starts_with("container shop-db-1 unhealthy"), "{text}");
        assert!(text.ends_with("(and 2 more)"), "{text}");
        assert!(
            rail_badge(&State::default()).is_none(),
            "never loads for the badge"
        );
    }

    #[test]
    fn frames_debounce_events_and_poll_when_the_stream_ended() {
        let mut st = loaded();
        let t0 = Instant::now();
        // Not live yet: ask for the stream.
        assert_eq!(frame(&mut st, t0).acts, [Act::LiveStart]);
        // Polling: a re-list every 10 s.
        st.live.mode = LiveMode::Polling("ended".into());
        st.listed_at = Some(t0);
        let n = frame(&mut st, t0 + Duration::from_secs(3));
        assert!(n.acts.is_empty());
        assert!(n.repaint.unwrap() <= Duration::from_secs(7));
        assert_eq!(
            frame(&mut st, t0 + Duration::from_secs(11)).acts,
            [Act::Relist]
        );
        // Events: wait for half a second of quiet.
        st.live.mode = LiveMode::Live;
        let f = Follow::new(|| {});
        st.live.stream = Some(Stream {
            id: 1,
            context: "desktop-linux".into(),
            follow: f.clone(),
            started: t0,
        });
        f.beat();
        st.live.seen = 0;
        st.listed_at = Some(t0);
        let t1 = t0 + Duration::from_secs(20);
        assert!(frame(&mut st, t1).acts.is_empty(), "nothing happened");
        st.live.first_dirty = Some(t1);
        st.live.last_noticed = Some(t1);
        let n = frame(&mut st, t1 + Duration::from_millis(100));
        assert!(!n.acts.contains(&Act::Relist), "still within the debounce");
        let n = frame(&mut st, t1 + Duration::from_millis(700));
        assert!(n.acts.contains(&Act::Relist));
        // Selected running container: stats every 3 s.
        st.selected = Some(Sel::Container("1".repeat(64)));
        let n = frame(&mut st, t1 + Duration::from_secs(1));
        assert!(n.acts.contains(&Act::Stats("1".repeat(64))));
        st.live.stream = None;
    }

    #[test]
    fn scripts_map_to_acts() {
        assert_eq!(script("container", "2"), Some(Act::SelectContainerRow(2)));
        assert_eq!(script("compose", "0"), Some(Act::SelectProjectRow(0)));
        assert_eq!(
            script("docker-ask", "down-volumes"),
            Some(Act::Ask(Ask::DownVolumes))
        );
        assert!(script("docker-ask", "explode").is_none());
        assert_eq!(script("docker-logs", ""), Some(Act::SelectedLogs));
        assert_eq!(script("docker-shell", ""), Some(Act::SelectedShell));
        assert_eq!(script("docker-all", "off"), Some(Act::ShowAll(false)));
        assert_eq!(
            script("docker-context", " staging "),
            Some(Act::Context("staging".into()))
        );
        assert!(script("container", "x").is_none());
        assert!(script("nope", "").is_none());
    }

    #[test]
    fn targets_of_projects_and_files() {
        let st = loaded();
        let t = st.project_target("shop").unwrap();
        assert_eq!(t.project.as_deref(), Some("shop"));
        assert_eq!(t.project_dir.as_deref(), Some(r"C:\repo"));
        assert_eq!(t.files.len(), 2);
        let f = st
            .file_target(Path::new("C:/repo"), "extras/docker-compose.yml")
            .unwrap();
        assert!(f.project.is_none());
        assert!(
            f.files[0]
                .replace('\\', "/")
                .ends_with("extras/docker-compose.yml")
        );
        assert_eq!(
            st.troubled().len(),
            3,
            "db unhealthy, worker restarting, migrate exited 1"
        );
    }
}
