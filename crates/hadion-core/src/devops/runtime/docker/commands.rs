//! docker command lines.
//!
//! Rules:
//! - Every command carries `--context NAME` before the subcommand, so what
//!   runs never depends on the user's current context; Hadion never runs
//!   `docker context use / create / rm`. The one exception is
//!   [`context_ls_args`]: listing the contexts reads the local
//!   configuration, and with `--context` docker would mark that one as
//!   current instead of the user's.
//! - Writes ([`ContainerOp`], [`ComposeOp`], [`BuildPlan`]) are built here
//!   and run by the app (in the console, or after a confirmation).
//! - [`parse_cli`] and [`mutation`] read a docker command line back, for
//!   safe mode.

use std::path::{Path, PathBuf};

use crate::devops::proc::{ProcSpec, render_args};

/// `--tail` of a container's logs.
pub const LOG_TAIL: u32 = 500;
/// `--tail` of a compose project's logs (per service).
pub const COMPOSE_LOG_TAIL: u32 = 200;
/// What `docker exec` runs for "Shell": bash when the image has it.
pub const SHELL_PROBE: &str = "command -v bash >/dev/null 2>&1 && exec bash || exec sh";
/// Events that change what the list shows (health probes' `exec_*` events
/// are left out: they come every few seconds per container).
pub const EVENTS: [&str; 13] = [
    "create",
    "start",
    "restart",
    "stop",
    "die",
    "kill",
    "oom",
    "pause",
    "unpause",
    "destroy",
    "rename",
    "update",
    "health_status",
];

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_owned()).collect()
}

/// `--context C`.
pub fn base(context: &str) -> Vec<String> {
    vec!["--context".into(), context.to_owned()]
}

fn with(context: &str, rest: &[&str]) -> Vec<String> {
    let mut a = base(context);
    a.extend(s(rest));
    a
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// `context ls --format json` (local; see the module docs).
pub fn context_ls_args() -> Vec<String> {
    s(&["context", "ls", "--format", "json"])
}

pub fn version_args(context: &str) -> Vec<String> {
    with(context, &["version", "--format", "json"])
}

pub fn ps_args(context: &str) -> Vec<String> {
    with(context, &["ps", "-a", "--no-trunc", "--format", "json"])
}

pub fn inspect_args(context: &str, ids: &[String]) -> Vec<String> {
    let mut a = with(context, &["inspect", "--type", "container"]);
    a.extend(ids.iter().cloned());
    a
}

pub fn image_inspect_args(context: &str, ids: &[String]) -> Vec<String> {
    let mut a = with(context, &["image", "inspect"]);
    a.extend(ids.iter().cloned());
    a
}

pub fn compose_ls_args(context: &str) -> Vec<String> {
    with(context, &["compose", "ls", "-a", "--format", "json"])
}

/// `compose … config --format json` (works without a daemon; it resolves
/// `.env`, so its output holds secrets: parse it with
/// [`super::compose::parse_config`], which keeps names only).
pub fn compose_config_args(context: &str, target: &ComposeTarget) -> Vec<String> {
    let mut a = with(context, &["compose"]);
    a.extend(target.args(true));
    a.extend(s(&["config", "--format", "json"]));
    a
}

/// `stats --no-stream --format json ID…`.
pub fn stats_args(context: &str, ids: &[String]) -> Vec<String> {
    let mut a = with(context, &["stats", "--no-stream", "--format", "json"]);
    a.extend(ids.iter().cloned());
    a
}

/// `events --format json --filter type=container --filter event=…`
/// (streams until killed).
pub fn events_args(context: &str) -> Vec<String> {
    let mut a = with(
        context,
        &["events", "--format", "json", "--filter", "type=container"],
    );
    for e in EVENTS {
        a.push("--filter".into());
        a.push(format!("event={e}"));
    }
    a
}

/// `logs [-f] --tail N [--timestamps] ID`.
pub fn logs_args(
    context: &str,
    id: &str,
    follow: bool,
    tail: u32,
    timestamps: bool,
) -> Vec<String> {
    let mut a = with(context, &["logs"]);
    if follow {
        a.push("-f".into());
    }
    a.extend(["--tail".into(), tail.to_string()]);
    if timestamps {
        a.push("--timestamps".into());
    }
    a.push(id.to_owned());
    a
}

/// `exec -it ID sh -c "<bash, else sh>"` (for a terminal tab).
pub fn exec_shell_args(context: &str, id: &str) -> Vec<String> {
    let mut a = with(context, &["exec", "-it"]);
    a.extend([id.to_owned(), "sh".into(), "-c".into(), SHELL_PROBE.into()]);
    a
}

// ---------------------------------------------------------------------------
// Compose projects
// ---------------------------------------------------------------------------

/// Which compose project a command addresses.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ComposeTarget {
    /// `-p NAME`: set for a project that exists (its containers carry the
    /// name), so the command reaches the same containers whatever the file
    /// says.
    pub project: Option<String>,
    /// `-f FILE`, in order (absolute paths).
    pub files: Vec<String>,
    /// `--project-directory DIR` (the project's working directory).
    pub project_dir: Option<String>,
}

impl ComposeTarget {
    /// The global compose flags. Without `with_files` only `-p NAME` (for
    /// a project whose files are gone: compose finds its containers by
    /// label).
    pub fn args(&self, with_files: bool) -> Vec<String> {
        let mut a = Vec::new();
        if let Some(p) = &self.project {
            a.extend(["-p".into(), p.clone()]);
        }
        if with_files {
            if let Some(d) = &self.project_dir {
                a.extend(["--project-directory".into(), d.clone()]);
            }
            for f in &self.files {
                a.extend(["-f".into(), f.clone()]);
            }
        }
        a
    }

    /// Config files that do not exist (any longer).
    pub fn missing_files(&self) -> Vec<&str> {
        self.files
            .iter()
            .filter(|f| !Path::new(f.as_str()).is_file())
            .map(String::as_str)
            .collect()
    }

    /// Where to run compose: the project directory when it exists.
    pub fn cwd(&self, fallback: &Path) -> PathBuf {
        self.project_dir
            .as_deref()
            .map(PathBuf::from)
            .filter(|d| d.is_dir())
            .or_else(|| {
                self.files
                    .first()
                    .and_then(|f| Path::new(f).parent().map(Path::to_path_buf))
                    .filter(|d| d.is_dir())
            })
            .unwrap_or_else(|| fallback.to_path_buf())
    }
}

/// A change to one container.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContainerOp {
    Start,
    Stop,
    Restart,
    Pause,
    Unpause,
    Kill,
    /// `rm`, or `rm -f` (stops a running container first).
    Remove {
        force: bool,
    },
}

impl ContainerOp {
    pub fn args(self, context: &str, id: &str) -> Vec<String> {
        let mut a = base(context);
        a.extend(match self {
            ContainerOp::Start => s(&["start"]),
            ContainerOp::Stop => s(&["stop"]),
            ContainerOp::Restart => s(&["restart"]),
            ContainerOp::Pause => s(&["pause"]),
            ContainerOp::Unpause => s(&["unpause"]),
            ContainerOp::Kill => s(&["kill"]),
            ContainerOp::Remove { force: false } => s(&["rm"]),
            ContainerOp::Remove { force: true } => s(&["rm", "-f"]),
        });
        a.push(id.to_owned());
        a
    }

    /// `Start`, `Force remove`.
    pub fn verb(self) -> &'static str {
        match self {
            ContainerOp::Start => "Start",
            ContainerOp::Stop => "Stop",
            ContainerOp::Restart => "Restart",
            ContainerOp::Pause => "Pause",
            ContainerOp::Unpause => "Unpause",
            ContainerOp::Kill => "Kill",
            ContainerOp::Remove { force: false } => "Remove",
            ContainerOp::Remove { force: true } => "Force remove",
        }
    }

    /// Kill and remove go through a confirmation.
    pub fn destructive(self) -> bool {
        matches!(self, ContainerOp::Kill | ContainerOp::Remove { .. })
    }

    /// `Stop shop-web-1`.
    pub fn title(self, name: &str) -> String {
        format!("{} {name}", self.verb())
    }

    pub fn explain(self) -> &'static str {
        match self {
            ContainerOp::Start => "The container starts again with its existing configuration.",
            ContainerOp::Stop => {
                "docker sends SIGTERM and, after 10 seconds, SIGKILL. The container and its data stay; Start brings it back."
            }
            ContainerOp::Restart => "docker stops the container and starts it again.",
            ContainerOp::Pause => {
                "Every process of the container is frozen (cgroup freezer). Nothing is lost; Unpause continues where it stopped."
            }
            ContainerOp::Unpause => "The frozen processes continue.",
            ContainerOp::Kill => {
                "docker sends SIGKILL right away: the processes get no chance to finish writes or close connections. The container stays (stopped) and can be started again."
            }
            ContainerOp::Remove { force: false } => {
                "The stopped container is deleted, with its writable layer (files written inside it that are not on a volume). Named volumes stay."
            }
            ContainerOp::Remove { force: true } => {
                "The container is killed (SIGKILL) and deleted, with its writable layer (files written inside it that are not on a volume). Named volumes stay."
            }
        }
    }
}

/// A change to a compose project.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ComposeOp {
    /// `up -d [--build] [SERVICE]`.
    Up {
        build: bool,
        service: Option<String>,
    },
    /// `down [--volumes] [--remove-orphans]`.
    Down {
        volumes: bool,
        remove_orphans: bool,
    },
    Start(Option<String>),
    Stop(Option<String>),
    Restart(Option<String>),
    Pull(Option<String>),
    Build(Option<String>),
    /// `logs -f --tail 200 --timestamps` of every service.
    Logs,
}

impl ComposeOp {
    pub fn args(&self, context: &str, target: &ComposeTarget, with_files: bool) -> Vec<String> {
        let mut a = with(context, &["compose"]);
        a.extend(target.args(with_files || self.needs_files()));
        let svc = |a: &mut Vec<String>, s: &Option<String>| {
            if let Some(s) = s.as_ref().filter(|s| !s.is_empty()) {
                a.push(s.clone());
            }
        };
        match self {
            ComposeOp::Up { build, service } => {
                a.extend(s(&["up", "-d"]));
                if *build {
                    a.push("--build".into());
                }
                svc(&mut a, service);
            }
            ComposeOp::Down {
                volumes,
                remove_orphans,
            } => {
                a.push("down".into());
                if *volumes {
                    a.push("--volumes".into());
                }
                if *remove_orphans {
                    a.push("--remove-orphans".into());
                }
            }
            ComposeOp::Start(x) => {
                a.push("start".into());
                svc(&mut a, x);
            }
            ComposeOp::Stop(x) => {
                a.push("stop".into());
                svc(&mut a, x);
            }
            ComposeOp::Restart(x) => {
                a.push("restart".into());
                svc(&mut a, x);
            }
            ComposeOp::Pull(x) => {
                a.push("pull".into());
                svc(&mut a, x);
            }
            ComposeOp::Build(x) => {
                a.push("build".into());
                svc(&mut a, x);
            }
            ComposeOp::Logs => a.extend([
                "logs".into(),
                "-f".into(),
                "--tail".into(),
                COMPOSE_LOG_TAIL.to_string(),
                "--timestamps".into(),
            ]),
        }
        a
    }

    /// Reads the compose files (the others work on the project by name).
    pub fn needs_files(&self) -> bool {
        matches!(
            self,
            ComposeOp::Up { .. } | ComposeOp::Pull(_) | ComposeOp::Build(_)
        )
    }

    /// Down goes through a confirmation.
    pub fn destructive(&self) -> bool {
        matches!(self, ComposeOp::Down { .. })
    }

    /// `Up shop (with build)`, `Down shop and its volumes`.
    pub fn title(&self, project: &str) -> String {
        let on = |verb: &str, s: &Option<String>| match s {
            Some(s) => format!("{verb} {s} of {project}"),
            None => format!("{verb} {project}"),
        };
        match self {
            ComposeOp::Up {
                build: false,
                service,
            } => on("Up", service),
            ComposeOp::Up {
                build: true,
                service,
            } => format!("{} (with build)", on("Up", service)),
            ComposeOp::Down { volumes: true, .. } => format!("Down {project} and its volumes"),
            ComposeOp::Down { .. } => format!("Down {project}"),
            ComposeOp::Start(s) => on("Start", s),
            ComposeOp::Stop(s) => on("Stop", s),
            ComposeOp::Restart(s) => on("Restart", s),
            ComposeOp::Pull(s) => on("Pull", s),
            ComposeOp::Build(s) => on("Build", s),
            ComposeOp::Logs => format!("Logs · {project} · following"),
        }
    }

    pub fn explain(&self) -> &'static str {
        match self {
            ComposeOp::Up { build: false, .. } => {
                "Creates and starts the containers in the background (-d); containers whose configuration or image changed are recreated."
            }
            ComposeOp::Up { build: true, .. } => {
                "Builds the services that have a build section first, then creates and starts the containers in the background."
            }
            ComposeOp::Down { volumes: false, .. } => {
                "Stops and removes the project's containers and networks. Named volumes stay, so databases keep their data; Up creates the containers again."
            }
            ComposeOp::Down { volumes: true, .. } => {
                "Stops and removes the project's containers and networks AND its named volumes. Data kept in those volumes (databases, uploads, caches) is deleted and cannot be brought back."
            }
            ComposeOp::Start(_) => "Starts the existing containers again.",
            ComposeOp::Stop(_) => "Stops the containers; they and their data stay.",
            ComposeOp::Restart(_) => "Stops and starts the containers.",
            ComposeOp::Pull(_) => "Downloads the newest images for the services that use an image.",
            ComposeOp::Build(_) => "Builds the images of the services that have a build section.",
            ComposeOp::Logs => "Streams the logs of every service until stopped.",
        }
    }
}

// ---------------------------------------------------------------------------
// Builds with OCI labels
// ---------------------------------------------------------------------------

pub const LABEL_REVISION: &str = "org.opencontainers.image.revision";
pub const LABEL_SOURCE: &str = "org.opencontainers.image.source";
pub const LABEL_CREATED: &str = "org.opencontainers.image.created";
pub const LABEL_VERSION: &str = "org.opencontainers.image.version";
pub const LABEL_DIRTY: &str = "hadion.dirty";

/// A docker image name from free text: lower case, `[a-z0-9._/-]`.
pub fn image_name(s: &str) -> String {
    let mut out: String = s
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/') {
                c
            } else {
                '-'
            }
        })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches(['-', '.', '_', '/']).to_owned();
    if out.is_empty() { "image".into() } else { out }
}

/// `<repo-name>:<short-sha>`, `-dirty` appended when the working tree has
/// changes.
pub fn default_tag(name: &str, short_sha: &str, dirty: bool) -> String {
    format!(
        "{}:{short_sha}{}",
        image_name(name),
        if dirty { "-dirty" } else { "" }
    )
}

/// What the labels say about the build.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildInputs {
    /// HEAD, the full id.
    pub head: String,
    /// The https URL of the remote, without credentials.
    pub source: Option<String>,
    /// The tag HEAD is exactly at.
    pub version: Option<String>,
    pub dirty: bool,
    /// RFC 3339, when the build starts.
    pub created: String,
}

/// The labels in the order they are passed.
pub fn build_labels(i: &BuildInputs) -> Vec<(String, String)> {
    let mut v = vec![(LABEL_REVISION.to_owned(), i.head.clone())];
    if let Some(src) = &i.source {
        v.push((LABEL_SOURCE.to_owned(), src.clone()));
    }
    v.push((LABEL_CREATED.to_owned(), i.created.clone()));
    if let Some(ver) = &i.version {
        v.push((LABEL_VERSION.to_owned(), ver.clone()));
    }
    if i.dirty {
        v.push((LABEL_DIRTY.to_owned(), "true".to_owned()));
    }
    v
}

/// `docker build -f … -t … --label … CONTEXT`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildPlan {
    pub dockerfile: String,
    pub context_dir: String,
    pub tag: String,
    pub labels: Vec<(String, String)>,
}

impl BuildPlan {
    pub fn args(&self, context: &str) -> Vec<String> {
        let mut a = with(context, &["build"]);
        a.extend([
            "-f".into(),
            self.dockerfile.clone(),
            "-t".into(),
            self.tag.clone(),
        ]);
        for (k, v) in &self.labels {
            a.push("--label".into());
            a.push(format!("{k}={v}"));
        }
        a.push(if self.context_dir.is_empty() {
            ".".into()
        } else {
            self.context_dir.clone()
        });
        a
    }
}

// ---------------------------------------------------------------------------
// Display and processes
// ---------------------------------------------------------------------------

/// The command line as shown: `docker --context desktop-linux ps -a …`.
pub fn display(args: &[String]) -> String {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    format!("docker {}", render_args(&refs))
}

/// A process spec for the console.
pub fn spec(dir: &Path, args: Vec<String>) -> ProcSpec {
    ProcSpec::new(super::program(), dir).args(args)
}

// ---------------------------------------------------------------------------
// Reading a command line back (safe mode)
// ---------------------------------------------------------------------------

/// Global flags that take a value.
const GLOBAL_VALUE_FLAGS: [&str; 10] = [
    "--context",
    "-c",
    "--host",
    "-H",
    "--config",
    "--log-level",
    "-l",
    "--tlscacert",
    "--tlscert",
    "--tlskey",
];
/// `docker compose` flags that take a value.
const COMPOSE_VALUE_FLAGS: [&str; 10] = [
    "-f",
    "--file",
    "-p",
    "--project-name",
    "--project-directory",
    "--env-file",
    "--profile",
    "--ansi",
    "--progress",
    "--parallel",
];

/// A docker command line: its global flags and the rest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cli<'a> {
    pub context: Option<&'a str>,
    pub host: Option<&'a str>,
    pub config: Option<&'a str>,
    /// From the subcommand on.
    pub rest: Vec<&'a str>,
}

pub fn parse_cli<'a>(args: &[&'a str]) -> Cli<'a> {
    let mut cli = Cli::default();
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        if !a.starts_with('-') || a == "-" {
            break;
        }
        if let Some((k, v)) = a.split_once('=') {
            match k {
                "--context" | "-c" => cli.context = Some(v),
                "--host" | "-H" => cli.host = Some(v),
                "--config" => cli.config = Some(v),
                _ => {}
            }
            i += 1;
            continue;
        }
        if GLOBAL_VALUE_FLAGS.contains(&a) {
            let v = args.get(i + 1).copied();
            match a {
                "--context" | "-c" => cli.context = v,
                "--host" | "-H" => cli.host = v,
                "--config" => cli.config = v,
                _ => {}
            }
            i += 2;
            continue;
        }
        i += 1;
    }
    cli.rest = args[i.min(args.len())..].to_vec();
    cli
}

/// Positional words, skipping flags (and the values of `value_flags`).
fn positional<'a>(args: &[&'a str], value_flags: &[&str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        if value_flags.contains(&a) {
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            i += 1;
            continue;
        }
        out.push(a);
        i += 1;
    }
    out
}

/// Pushes to a registry (a remote write wherever the daemon is).
pub fn pushes(cli: &Cli) -> bool {
    let verb = cli.rest.first().copied().unwrap_or("");
    let words = positional(cli.rest.get(1..).unwrap_or(&[]), &COMPOSE_VALUE_FLAGS);
    let second = words.first().copied().unwrap_or("");
    verb == "push"
        || (verb == "image" && second == "push")
        || (verb == "compose" && second == "push")
        || (matches!(verb, "buildx" | "build" | "builder") && cli.rest.contains(&"--push"))
}

/// The subcommand when it changes something on the daemon (`stop`,
/// `compose up`, `image rm`); `None` for reads and local configuration.
pub fn mutation(cli: &Cli) -> Option<String> {
    const TOP: [&str; 22] = [
        "start", "stop", "restart", "pause", "unpause", "kill", "rm", "rmi", "run", "exec",
        "create", "build", "pull", "push", "tag", "update", "rename", "commit", "cp", "attach",
        "load", "import",
    ];
    const CONTAINER: [&str; 17] = [
        "start", "stop", "restart", "pause", "unpause", "kill", "rm", "run", "exec", "create",
        "update", "rename", "commit", "cp", "attach", "prune", "export",
    ];
    const IMAGE: [&str; 8] = [
        "build", "pull", "push", "rm", "tag", "load", "import", "prune",
    ];
    const COMPOSE: [&str; 18] = [
        "up", "down", "start", "stop", "restart", "rm", "build", "pull", "push", "run", "exec",
        "create", "kill", "pause", "unpause", "cp", "scale", "watch",
    ];
    let verb = cli.rest.first().copied()?;
    if TOP.contains(&verb) {
        return Some(verb.to_owned());
    }
    let rest = cli.rest.get(1..).unwrap_or(&[]);
    let second = |flags: &[&str]| positional(rest, flags).first().copied();
    let pick = |list: &[&str], flags: &[&str]| -> Option<String> {
        second(flags)
            .filter(|w| list.contains(w))
            .map(|w| format!("{verb} {w}"))
    };
    match verb {
        "container" => pick(&CONTAINER, &[]),
        "image" => pick(&IMAGE, &[]),
        "compose" => pick(&COMPOSE, &COMPOSE_VALUE_FLAGS),
        "volume" => pick(&["create", "rm", "prune", "update"], &[]),
        "network" => pick(&["create", "rm", "connect", "disconnect", "prune"], &[]),
        "system" => pick(&["prune"], &[]),
        "builder" => pick(&["build", "prune"], &[]),
        "buildx" => pick(&["build", "bake", "prune", "rm", "create"], &[]),
        "service" => pick(&["create", "update", "rm", "scale", "rollback"], &[]),
        "stack" => pick(&["deploy", "rm"], &[]),
        "secret" | "config" => pick(&["create", "rm"], &[]),
        "node" => pick(&["update", "rm", "promote", "demote"], &[]),
        "swarm" => pick(&["init", "join", "leave", "update", "unlock"], &[]),
        "plugin" => pick(
            &["install", "rm", "enable", "disable", "upgrade", "set"],
            &[],
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard(a: &[String], ctx: &str) {
        assert_eq!(a[0], "--context", "{a:?}");
        assert_eq!(a[1], ctx, "{a:?}");
    }

    #[test]
    fn reads_carry_the_context_first() {
        let ids = vec!["abc".to_owned(), "def".to_owned()];
        for a in [
            version_args("dl"),
            ps_args("dl"),
            inspect_args("dl", &ids),
            image_inspect_args("dl", &ids),
            compose_ls_args("dl"),
            stats_args("dl", &ids),
            events_args("dl"),
            logs_args("dl", "abc", true, LOG_TAIL, true),
            exec_shell_args("dl", "abc"),
            compose_config_args("dl", &ComposeTarget::default()),
        ] {
            guard(&a, "dl");
        }
        assert_eq!(
            display(&ps_args("desktop-linux")),
            "docker --context desktop-linux ps -a --no-trunc --format json"
        );
        assert_eq!(
            display(&logs_args("dl", "abc", true, 500, true)),
            "docker --context dl logs -f --tail 500 --timestamps abc"
        );
        assert_eq!(
            display(&exec_shell_args("dl", "abc")),
            "docker --context dl exec -it abc sh -c \"command -v bash >/dev/null 2>&1 && exec bash || exec sh\""
        );
        let ev = display(&events_args("dl"));
        assert!(ev.contains("--filter type=container --filter event=create"));
        assert!(ev.contains("--filter event=health_status"));
        assert!(!ev.contains("exec_"), "health probes are not followed");
        assert_eq!(context_ls_args(), ["context", "ls", "--format", "json"]);
    }

    #[test]
    fn container_writes() {
        let w = |op: ContainerOp| display(&op.args("dl", "abc"));
        assert_eq!(w(ContainerOp::Start), "docker --context dl start abc");
        assert_eq!(w(ContainerOp::Stop), "docker --context dl stop abc");
        assert_eq!(w(ContainerOp::Restart), "docker --context dl restart abc");
        assert_eq!(w(ContainerOp::Pause), "docker --context dl pause abc");
        assert_eq!(w(ContainerOp::Unpause), "docker --context dl unpause abc");
        assert_eq!(w(ContainerOp::Kill), "docker --context dl kill abc");
        assert_eq!(
            w(ContainerOp::Remove { force: false }),
            "docker --context dl rm abc"
        );
        assert_eq!(
            w(ContainerOp::Remove { force: true }),
            "docker --context dl rm -f abc"
        );
        assert!(ContainerOp::Kill.destructive());
        assert!(ContainerOp::Remove { force: false }.destructive());
        assert!(!ContainerOp::Stop.destructive());
        assert_eq!(
            ContainerOp::Remove { force: true }.title("web"),
            "Force remove web"
        );
    }

    fn target() -> ComposeTarget {
        ComposeTarget {
            project: Some("shop".into()),
            files: vec![
                "C:/r/compose.yaml".into(),
                "C:/r/compose.override.yaml".into(),
            ],
            project_dir: Some("C:/r".into()),
        }
    }

    #[test]
    fn compose_writes() {
        let t = target();
        let w = |op: ComposeOp| display(&op.args("dl", &t, true));
        let pre = "docker --context dl compose -p shop --project-directory C:/r -f C:/r/compose.yaml -f C:/r/compose.override.yaml";
        assert_eq!(
            w(ComposeOp::Up {
                build: false,
                service: None
            }),
            format!("{pre} up -d")
        );
        assert_eq!(
            w(ComposeOp::Up {
                build: true,
                service: Some("web".into())
            }),
            format!("{pre} up -d --build web")
        );
        assert_eq!(
            w(ComposeOp::Down {
                volumes: false,
                remove_orphans: false
            }),
            format!("{pre} down")
        );
        assert_eq!(
            w(ComposeOp::Down {
                volumes: true,
                remove_orphans: true
            }),
            format!("{pre} down --volumes --remove-orphans")
        );
        assert_eq!(w(ComposeOp::Start(None)), format!("{pre} start"));
        assert_eq!(
            w(ComposeOp::Stop(Some("db".into()))),
            format!("{pre} stop db")
        );
        assert_eq!(w(ComposeOp::Restart(None)), format!("{pre} restart"));
        assert_eq!(w(ComposeOp::Pull(None)), format!("{pre} pull"));
        assert_eq!(
            w(ComposeOp::Build(Some("web".into()))),
            format!("{pre} build web")
        );
        assert_eq!(
            w(ComposeOp::Logs),
            format!("{pre} logs -f --tail 200 --timestamps")
        );
        // Files gone: the project by name, except for what needs them.
        assert_eq!(
            display(&ComposeOp::Stop(None).args("dl", &t, false)),
            "docker --context dl compose -p shop stop"
        );
        assert!(
            display(&ComposeOp::Pull(None).args("dl", &t, false)).contains(" -f C:/r/compose.yaml")
        );
        // A compose file that is not started has no project name yet.
        let file = ComposeTarget {
            project: None,
            files: vec!["C:/r/extras/docker-compose.yml".into()],
            project_dir: None,
        };
        assert_eq!(
            display(&compose_config_args("dl", &file)),
            "docker --context dl compose -f C:/r/extras/docker-compose.yml config --format json"
        );
        assert!(
            ComposeOp::Down {
                volumes: false,
                remove_orphans: false
            }
            .destructive()
        );
        assert_eq!(
            ComposeOp::Down {
                volumes: true,
                remove_orphans: false
            }
            .title("shop"),
            "Down shop and its volumes"
        );
        assert_eq!(
            ComposeOp::Up {
                build: true,
                service: None
            }
            .title("shop"),
            "Up shop (with build)"
        );
    }

    #[test]
    fn builds_carry_oci_labels() {
        assert_eq!(
            default_tag("BackOffice", "1a2b3c4", false),
            "backoffice:1a2b3c4"
        );
        assert_eq!(
            default_tag("My App!", "1a2b3c4", true),
            "my-app:1a2b3c4-dirty"
        );
        assert_eq!(image_name("  "), "image");
        let labels = build_labels(&BuildInputs {
            head: "1a2b3c4d5e6f".into(),
            source: Some("https://github.com/acme/shop".into()),
            version: Some("v1.2.3".into()),
            dirty: true,
            created: "2026-10-30T08:00:00Z".into(),
        });
        let plan = BuildPlan {
            dockerfile: "web/Dockerfile".into(),
            context_dir: "web".into(),
            tag: "shop:1a2b3c4-dirty".into(),
            labels,
        };
        assert_eq!(
            display(&plan.args("dl")),
            "docker --context dl build -f web/Dockerfile -t shop:1a2b3c4-dirty --label org.opencontainers.image.revision=1a2b3c4d5e6f --label org.opencontainers.image.source=https://github.com/acme/shop --label org.opencontainers.image.created=2026-10-30T08:00:00Z --label org.opencontainers.image.version=v1.2.3 --label hadion.dirty=true web"
        );
        let clean = build_labels(&BuildInputs {
            head: "abc".into(),
            created: "t".into(),
            ..BuildInputs::default()
        });
        assert_eq!(clean.len(), 2, "no source, version or dirty label");
    }

    #[test]
    fn command_lines_read_back() {
        let p = |line: &str| -> (Option<String>, bool, Option<String>) {
            let args: Vec<&str> = line.split(' ').filter(|s| !s.is_empty()).collect();
            let cli = parse_cli(&args);
            (cli.context.map(str::to_owned), pushes(&cli), mutation(&cli))
        };
        assert_eq!(p("--context dl ps -a"), (Some("dl".into()), false, None));
        assert_eq!(
            p("-c dl stop abc"),
            (Some("dl".into()), false, Some("stop".into()))
        );
        assert_eq!(p("--context=dl rm -f abc").2.as_deref(), Some("rm"));
        assert_eq!(
            p("--context dl compose -p shop -f a.yml up -d")
                .2
                .as_deref(),
            Some("compose up")
        );
        assert_eq!(
            p("--context dl compose -f a.yml config --format json").2,
            None
        );
        assert_eq!(p("--context dl compose ls -a").2, None);
        assert_eq!(p("--context dl image inspect x").2, None);
        assert_eq!(p("--context dl image rm x").2.as_deref(), Some("image rm"));
        assert_eq!(
            p("--context dl container stop x").2.as_deref(),
            Some("container stop")
        );
        assert_eq!(p("--context dl logs -f abc").2, None);
        assert_eq!(p("--context dl events --format json").2, None);
        assert_eq!(p("--context dl exec -it abc sh").2.as_deref(), Some("exec"));
        assert_eq!(p("context use other").2, None, "local configuration");
        assert!(p("push ghcr.io/o/x:1").1);
        assert!(p("--context dl image push x").1);
        assert!(p("buildx build --push .").1);
        assert!(p("--context dl compose push").1);
        assert!(!p("--context dl build .").1);
        let cli = parse_cli(&["-H", "tcp://1.2.3.4:2375", "--config", "C:/cfg", "ps"]);
        assert_eq!(cli.host, Some("tcp://1.2.3.4:2375"));
        assert_eq!(cli.config, Some("C:/cfg"));
        assert_eq!(cli.rest, ["ps"]);
    }
}
