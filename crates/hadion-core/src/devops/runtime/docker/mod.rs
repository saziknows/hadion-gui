//! Docker containers and compose projects of the repository, through the
//! `docker` CLI (the Ship page's Containers tab).
//!
//! - [`commands`]: argument builders. Every command carries an explicit
//!   `--context NAME`; Hadion never changes the user's docker
//!   configuration or current context. Writes are pure builders.
//! - [`model`]: contexts, version, `ps`, `inspect`, images, `compose ls`,
//!   stats. Environment variables keep their names only.
//! - [`compose`]: `compose config` (names only again) and the compose files
//!   and Dockerfiles of the repository.
//! - [`scope`]: which projects and containers belong to the repository.
//! - [`revision`]: which commit a container runs (labels, tags, digests).
//! - [`triage`]: errors as plain states with the exact fix.
//! - [`events`]: `docker events` followed while the tab is shown.
//! - [`endpoint`]: where a command goes (local or remote), for safe mode.
//!
//! `HADION_DOCKER` replaces the executable (the `fake_docker` example is a
//! test double that answers every read and records every command).

pub mod commands;
pub mod compose;
pub mod endpoint;
pub mod events;
pub mod model;
pub mod revision;
pub mod scope;
pub mod triage;

use std::path::Path;
use std::time::{Duration, Instant};

use crate::devops::proc::{ProcSpec, run_capture};
use compose::{ComposeConfig, RepoFiles};
use model::{ComposeLs, Container, Context, Image, Stats, Version};

/// Overrides the docker executable (a recording double for tests).
pub const PROGRAM_ENV: &str = "HADION_DOCKER";
/// A read is killed after this, whatever docker does (an ssh endpoint can
/// hang on its connection timeout).
pub const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
/// IDs per `inspect` call (keeps the command line well under Windows'
/// 32,767 characters).
const CHUNK: usize = 100;

pub fn program() -> String {
    super::program_override(PROGRAM_ENV, "docker")
}

/// docker's path and client version (honours [`PROGRAM_ENV`]).
pub fn detect() -> Option<crate::devops::tools::Installed> {
    super::detect(
        crate::devops::tools::Tool::Docker,
        PROGRAM_ENV,
        &["--version"],
    )
}

/// Whether docker can be started (a PATH lookup).
pub fn available() -> bool {
    super::program_available(&program())
}

/// The output of one docker call.
#[derive(Clone, Debug, Default)]
pub struct Call {
    pub stdout: Vec<u8>,
    /// Cleaned stderr.
    pub stderr: String,
    pub code: Option<i32>,
    pub timed_out: bool,
}

impl Call {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }

    /// What to show when it failed.
    pub fn error(&self) -> String {
        if self.timed_out {
            return format!(
                "docker did not finish within {} s and was stopped (timed out).",
                PROCESS_TIMEOUT.as_secs()
            );
        }
        if !self.stderr.is_empty() {
            return self.stderr.clone();
        }
        format!("docker exited with {:?}", self.code)
    }
}

/// Run docker with `args` (recorded in `commands`). `Err` only when it
/// could not start (not installed, safe mode).
pub fn call(dir: &Path, args: &[String], commands: &mut Vec<String>) -> Result<Call, String> {
    commands.push(commands::display(args));
    let spec = ProcSpec::new(program(), dir).args(args.iter().cloned());
    let out = run_capture(&spec, Some(PROCESS_TIMEOUT)).map_err(|e| e.to_string())?;
    Ok(Call {
        stderr: triage::clean_error(&out.stderr),
        timed_out: out.timed_out,
        code: out.code,
        stdout: out.stdout,
    })
}

/// [`call`], stdout when it succeeded, the cleaned error otherwise.
pub fn run(dir: &Path, args: &[String], commands: &mut Vec<String>) -> Result<Vec<u8>, String> {
    let c = call(dir, args, commands)?;
    if c.success() {
        Ok(c.stdout)
    } else {
        Err(c.error())
    }
}

/// The contexts of the docker configuration.
pub fn load_contexts(dir: &Path, commands: &mut Vec<String>) -> Result<Vec<Context>, String> {
    run(dir, &commands::context_ls_args(), commands).and_then(|b| model::parse_contexts(&b))
}

/// What one context runs.
#[derive(Clone, Debug)]
pub struct Listing {
    pub context: String,
    /// `Err` when the daemon did not answer (the client still printed its
    /// version: [`Listing::client`]).
    pub version: Result<Version, String>,
    pub client: Option<String>,
    /// `None` when skipped (the daemon did not answer).
    pub containers: Option<Result<Vec<Container>, String>>,
    pub images: Vec<Image>,
    pub images_error: Option<String>,
    pub projects: Option<Result<Vec<ComposeLs>, String>>,
    pub ms: u64,
}

impl Listing {
    pub fn containers(&self) -> &[Container] {
        match &self.containers {
            Some(Ok(c)) => c,
            _ => &[],
        }
    }

    pub fn projects(&self) -> &[ComposeLs] {
        match &self.projects {
            Some(Ok(p)) => p,
            _ => &[],
        }
    }

    pub fn image(&self, id: &str) -> Option<&Image> {
        self.images.iter().find(|i| i.id == id)
    }

    pub fn container(&self, id: &str) -> Option<&Container> {
        self.containers().iter().find(|c| c.id == id)
    }

    /// The error that stops everything (the daemon did not answer).
    pub fn fatal(&self) -> Option<&str> {
        match (&self.version, &self.containers) {
            (Err(e), _) => Some(e),
            (_, Some(Err(e))) => Some(e),
            _ => None,
        }
    }
}

/// `inspect` IDs in chunks; containers that vanished meanwhile are missing
/// from the answer (docker prints the others and exits 1).
fn inspect_all(
    dir: &Path,
    context: &str,
    ids: &[String],
    commands: &mut Vec<String>,
    image: bool,
) -> Result<Vec<u8>, String> {
    let mut merged: Vec<serde_json::Value> = Vec::new();
    for chunk in ids.chunks(CHUNK) {
        let args = if image {
            commands::image_inspect_args(context, chunk)
        } else {
            commands::inspect_args(context, chunk)
        };
        let c = call(dir, &args, commands)?;
        match model::json_rows(&c.stdout, "inspect") {
            Ok(rows) if !rows.is_empty() || c.success() => merged.extend(rows),
            _ => return Err(c.error()),
        }
    }
    serde_json::to_vec(&merged).map_err(|e| e.to_string())
}

/// Version, containers (`ps` then `inspect`), their images and the compose
/// projects of one context. When the daemon does not answer, the rest is
/// skipped.
pub fn load_listing(dir: &Path, context: &str, commands: &mut Vec<String>) -> Listing {
    let started = Instant::now();
    let mut l = Listing {
        context: context.to_owned(),
        version: Err(String::new()),
        client: None,
        containers: None,
        images: Vec::new(),
        images_error: None,
        projects: None,
        ms: 0,
    };
    let v = match call(dir, &commands::version_args(context), commands) {
        Ok(v) => v,
        Err(e) => {
            l.version = Err(e);
            l.ms = started.elapsed().as_millis() as u64;
            return l;
        }
    };
    let parsed = model::parse_version(&v.stdout);
    l.client = parsed
        .as_ref()
        .map(|p| p.client.clone())
        .filter(|c| !c.is_empty());
    l.version = match parsed {
        Some(p) if v.success() && p.server.is_some() => Ok(p),
        _ => Err(v.error()),
    };
    if l.version.is_err() {
        l.ms = started.elapsed().as_millis() as u64;
        return l;
    }
    let containers = run(dir, &commands::ps_args(context), commands)
        .and_then(|b| model::parse_ps(&b))
        .and_then(|ps| {
            if ps.is_empty() {
                return Ok(Vec::new());
            }
            let ids: Vec<String> = ps.iter().map(|r| r.id.clone()).collect();
            let bytes = inspect_all(dir, context, &ids, commands, false)?;
            let mut c = model::parse_inspect(&bytes)?;
            model::merge_status(&mut c, &ps);
            Ok(c)
        });
    if let Ok(c) = &containers {
        let mut ids: Vec<String> = c.iter().map(|c| c.image_id.clone()).collect();
        ids.retain(|i| !i.is_empty());
        ids.sort();
        ids.dedup();
        if !ids.is_empty() {
            match inspect_all(dir, context, &ids, commands, true)
                .and_then(|b| model::parse_images(&b))
            {
                Ok(i) => l.images = i,
                Err(e) => l.images_error = Some(e),
            }
        }
    }
    l.containers = Some(containers);
    l.projects = Some(
        run(dir, &commands::compose_ls_args(context), commands)
            .and_then(|b| model::parse_compose_ls(&b)),
    );
    l.ms = started.elapsed().as_millis() as u64;
    l
}

/// Everything the tab loads when it opens or on Refresh.
#[derive(Clone, Debug)]
pub struct Full {
    pub contexts: Result<Vec<Context>, String>,
    /// The context used.
    pub context: String,
    pub listing: Option<Listing>,
    pub files: RepoFiles,
    /// `Docker version 29.6.2, build dfc4efb`.
    pub tool: Option<String>,
    pub ms: u64,
}

/// Contexts, the chosen context's listing, and the repository's compose
/// files and Dockerfiles.
pub fn load_full(dir: &Path, wanted: Option<&str>, commands: &mut Vec<String>) -> Full {
    let started = Instant::now();
    commands.push(format!("{} --version", program()));
    let tool = detect().map(|i| i.version).filter(|v| !v.is_empty());
    let contexts = load_contexts(dir, commands);
    let env_ctx = std::env::var("DOCKER_CONTEXT").ok();
    let context = match &contexts {
        Ok(list) => model::choose_context(list, wanted, env_ctx.as_deref()),
        Err(_) => wanted.unwrap_or("default").to_owned(),
    };
    let files = compose::find_files(dir, commands);
    let listing = match &contexts {
        Err(e) if triage::classify(e, false) == triage::Problem::NotInstalled => None,
        _ => Some(load_listing(dir, &context, commands)),
    };
    Full {
        contexts,
        context,
        listing,
        files,
        tool,
        ms: started.elapsed().as_millis() as u64,
    }
}

/// `compose config` of a project or a compose file.
pub fn load_config(
    dir: &Path,
    context: &str,
    target: &commands::ComposeTarget,
    commands: &mut Vec<String>,
) -> Result<ComposeConfig, String> {
    let cwd = target.cwd(dir);
    run(
        &cwd,
        &commands::compose_config_args(context, target),
        commands,
    )
    .and_then(|b| compose::parse_config(&b))
}

/// One `stats --no-stream` sample of some containers.
pub fn load_stats(
    dir: &Path,
    context: &str,
    ids: &[String],
    commands: &mut Vec<String>,
) -> Result<Vec<Stats>, String> {
    run(dir, &commands::stats_args(context, ids), commands).and_then(|b| model::parse_stats(&b))
}

/// `inspect ID` with every environment value masked (see
/// [`model::mask_inspect`]).
pub fn inspect_masked(
    dir: &Path,
    context: &str,
    id: &str,
    commands: &mut Vec<String>,
) -> Result<String, String> {
    run(
        dir,
        &commands::inspect_args(context, &[id.to_owned()]),
        commands,
    )
    .and_then(|b| model::mask_inspect(&b))
}
