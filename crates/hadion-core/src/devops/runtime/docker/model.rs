//! What docker answers, parsed into models that keep what the Containers
//! tab shows and nothing else.
//!
//! - Every model is decoded with `#[serde(default)]` and tolerates missing
//!   and `null` fields (docker versions differ, and stopped containers leave
//!   whole sections empty).
//! - Environment variables are reduced to their NAMES while decoding
//!   ([`env_names`]); a value never reaches a model, so it cannot reach the
//!   log, a toast, an error or the screen.
//! - Command lines, health-check commands and probe output go through the
//!   exposure rules (`ai::redact`) and a flag mask, since people put
//!   passwords on command lines too.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::github::parse_timestamp;

/// `null` → the default value.
pub(crate) fn nullable<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// `["NAME=value", …]` → `["NAME", …]`: the values are dropped here.
pub(crate) fn env_names<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw: Option<Vec<String>> = Option::deserialize(d)?;
    let mut names: Vec<String> = raw
        .unwrap_or_default()
        .iter()
        .map(|kv| {
            kv.split_once('=')
                .map_or(kv.as_str(), |(k, _)| k)
                .to_owned()
        })
        .filter(|k| !k.is_empty())
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

/// Seconds since the epoch of a docker timestamp; docker writes
/// `0001-01-01T00:00:00Z` for "never".
pub fn time(s: &str) -> Option<i64> {
    parse_timestamp(s).map(|(secs, _)| secs).filter(|t| *t > 0)
}

/// NDJSON lines or one JSON array: `docker … --format json` prints one
/// object per line for lists, some versions (and `compose ls`) an array.
pub fn json_rows(bytes: &[u8], what: &str) -> Result<Vec<Value>, String> {
    let text = String::from_utf8_lossy(bytes);
    let t = text.trim().trim_start_matches('\u{feff}');
    if t.is_empty() {
        return Ok(Vec::new());
    }
    // One document (an array, or a pretty-printed object such as `compose
    // config`), else one object per line.
    match serde_json::from_str::<Value>(t) {
        Ok(Value::Array(a)) => return Ok(a),
        Ok(v @ Value::Object(_)) => return Ok(vec![v]),
        _ => {}
    }
    let mut out = Vec::new();
    for line in t.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match serde_json::from_str::<Value>(line) {
            Ok(Value::Array(a)) => out.extend(a),
            Ok(v @ Value::Object(_)) => out.push(v),
            Ok(_) => {}
            Err(e) => return Err(format!("docker {what}: not JSON ({e})")),
        }
    }
    Ok(out)
}

/// Decode every row, skipping rows that do not fit the model.
fn rows<T: for<'de> Deserialize<'de>>(bytes: &[u8], what: &str) -> Result<Vec<T>, String> {
    Ok(json_rows(bytes, what)?
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect())
}

// ---------------------------------------------------------------------------
// Secrets on command lines
// ---------------------------------------------------------------------------

/// Words that make a `--flag=value` or `NAME=value` look secret.
const SECRET_WORDS: [&str; 8] = [
    "PASSWORD",
    "PASSWD",
    "SECRET",
    "TOKEN",
    "APIKEY",
    "API_KEY",
    "CREDENTIAL",
    "AUTH",
];

fn looks_secret(name: &str) -> bool {
    let u = name.to_ascii_uppercase().replace('-', "_");
    SECRET_WORDS.iter().any(|w| u.contains(w)) || u.ends_with("_KEY") || u == "KEY"
}

/// Mask the values of secret-looking `--flag=value` / `NAME=value` words and
/// of the word after a secret-looking flag (`--password hunter2`), then run
/// the exposure rules over the result.
pub fn mask_command(words: &[String]) -> String {
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut hide_next = false;
    for w in words {
        if hide_next {
            out.push("•••".into());
            hide_next = false;
            continue;
        }
        if let Some((k, _)) = w.split_once('=')
            && looks_secret(k.trim_start_matches('-'))
        {
            out.push(format!("{k}=•••"));
            continue;
        }
        if w.starts_with('-') && w.len() > 2 && looks_secret(w.trim_start_matches('-')) {
            hide_next = true;
        }
        out.push(quote(w));
    }
    redact(&out.join(" "))
}

/// The exposure rules over free text (probe output, labels).
pub fn redact(text: &str) -> String {
    crate::devops::ai::redact::text(text, "docker.txt").0
}

fn quote(a: &str) -> String {
    if a.is_empty() || a.contains(char::is_whitespace) {
        format!("\"{}\"", a.replace('"', "\\\""))
    } else {
        a.to_owned()
    }
}

// ---------------------------------------------------------------------------
// Contexts
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawContext {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Description")]
    description: String,
    #[serde(rename = "DockerEndpoint")]
    endpoint: String,
    #[serde(rename = "Current")]
    current: Value,
    #[serde(rename = "Error")]
    error: String,
}

/// A docker context (`docker context ls`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Context {
    pub name: String,
    pub description: String,
    /// `npipe:////./pipe/dockerDesktopLinuxEngine`, `unix:///var/run/docker.sock`,
    /// `ssh://deploy@staging`, `tcp://10.0.0.5:2376`.
    pub endpoint: String,
    /// Marked current by docker (the config's `currentContext`, or
    /// `DOCKER_CONTEXT`).
    pub current: bool,
    /// What docker reported about a broken context.
    pub error: String,
}

impl Context {
    /// The endpoint is on another machine (`tcp://`, `ssh://`, `http(s)://`).
    pub fn remote(&self) -> bool {
        super::endpoint::is_remote(&self.endpoint)
    }
}

/// `docker context ls --format json`: NDJSON lines or an array, `Current`
/// as a bool (or `"*"` in old versions).
pub fn parse_contexts(bytes: &[u8]) -> Result<Vec<Context>, String> {
    let raw: Vec<RawContext> = rows(bytes, "context ls")?;
    Ok(raw
        .into_iter()
        .filter(|c| !c.name.is_empty())
        .map(|c| Context {
            current: match &c.current {
                Value::Bool(b) => *b,
                Value::String(s) => s == "*" || s.eq_ignore_ascii_case("true"),
                _ => false,
            },
            name: c.name,
            description: c.description,
            endpoint: c.endpoint,
            error: c.error,
        })
        .collect())
}

/// The context to use: `wanted` when it exists, else the one marked
/// current, else `DOCKER_CONTEXT` when it exists, else `default`.
pub fn choose_context(
    contexts: &[Context],
    wanted: Option<&str>,
    docker_context: Option<&str>,
) -> String {
    let exists = |n: &str| contexts.iter().any(|c| c.name == n);
    wanted
        .filter(|w| exists(w))
        .map(str::to_owned)
        .or_else(|| contexts.iter().find(|c| c.current).map(|c| c.name.clone()))
        .or_else(|| {
            docker_context
                .map(str::trim)
                .filter(|d| !d.is_empty() && exists(d))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "default".into())
}

// ---------------------------------------------------------------------------
// Version
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "PascalCase")]
struct RawPlatform {
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "PascalCase")]
struct RawSide {
    version: String,
    api_version: String,
    os: String,
    arch: String,
    kernel_version: String,
    context: String,
    platform: Option<RawPlatform>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "PascalCase")]
struct RawVersion {
    client: Option<RawSide>,
    server: Option<RawSide>,
}

/// The daemon side of `docker version`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Server {
    pub version: String,
    pub api: String,
    pub os: String,
    pub arch: String,
    pub kernel: String,
    /// `Docker Desktop 4.84.0 (234817)`, `Docker Engine - Community`.
    pub platform: String,
}

/// `docker version --format json`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Version {
    pub client: String,
    pub client_api: String,
    pub client_os: String,
    /// `None` when the daemon did not answer (the client part still prints).
    pub server: Option<Server>,
}

impl Version {
    /// `29.6.2 · linux/amd64 · Docker Desktop 4.84.0`.
    pub fn server_text(&self) -> String {
        match &self.server {
            Some(s) => {
                let mut t = format!("{} · {}/{}", s.version, s.os, s.arch);
                if !s.platform.is_empty() {
                    t.push_str(&format!(" · {}", s.platform));
                }
                t
            }
            None => "not answering".into(),
        }
    }
}

/// Parse `docker version --format json`; docker prints the client part
/// even when the daemon is down (and exits 1), so this reads that too.
pub fn parse_version(bytes: &[u8]) -> Option<Version> {
    let text = String::from_utf8_lossy(bytes);
    let start = text.find('{')?;
    let raw: RawVersion = serde_json::from_str(text[start..].trim()).ok()?;
    let c = raw.client.unwrap_or_default();
    Some(Version {
        client: c.version,
        client_api: c.api_version,
        client_os: c.os,
        server: raw.server.map(|s| Server {
            version: s.version,
            api: s.api_version,
            os: s.os,
            arch: s.arch,
            kernel: s.kernel_version,
            platform: s.platform.map(|p| p.name).unwrap_or_default(),
        }),
    })
}

// ---------------------------------------------------------------------------
// ps
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawPs {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Names")]
    names: String,
    #[serde(rename = "Image")]
    image: String,
    #[serde(rename = "State")]
    state: String,
    #[serde(rename = "Status")]
    status: String,
    #[serde(rename = "RunningFor")]
    running_for: String,
}

/// One row of `docker ps -a --no-trunc --format json`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PsRow {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    /// `Up 3 minutes (healthy)`, `Exited (1) 2 hours ago`.
    pub status: String,
    pub running_for: String,
}

pub fn parse_ps(bytes: &[u8]) -> Result<Vec<PsRow>, String> {
    let raw: Vec<RawPs> = rows(bytes, "ps")?;
    Ok(raw
        .into_iter()
        .filter(|r| !r.id.is_empty())
        .map(|r| PsRow {
            id: r.id,
            name: r.names.split(',').next().unwrap_or("").to_owned(),
            image: r.image,
            state: r.state,
            status: r.status,
            running_for: r.running_for,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Containers (inspect)
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "PascalCase")]
struct RawProbe {
    start: String,
    end: String,
    exit_code: i64,
    output: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "PascalCase")]
struct RawHealth {
    status: String,
    failing_streak: i64,
    #[serde(deserialize_with = "nullable")]
    log: Vec<RawProbe>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "PascalCase")]
struct RawState {
    status: String,
    running: bool,
    paused: bool,
    restarting: bool,
    #[serde(rename = "OOMKilled")]
    oom_killed: bool,
    dead: bool,
    pid: i64,
    exit_code: i64,
    error: String,
    started_at: String,
    finished_at: String,
    health: Option<RawHealth>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default, rename_all = "PascalCase")]
struct RawBinding {
    host_ip: String,
    host_port: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "PascalCase")]
struct RawRestart {
    name: String,
    maximum_retry_count: i64,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "PascalCase")]
struct RawHostConfig {
    restart_policy: RawRestart,
    #[serde(deserialize_with = "nullable")]
    port_bindings: BTreeMap<String, Option<Vec<RawBinding>>>,
    auto_remove: bool,
    network_mode: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawMount {
    #[serde(rename = "Type")]
    kind: String,
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Source")]
    source: String,
    #[serde(rename = "Destination")]
    destination: String,
    #[serde(rename = "RW")]
    rw: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "PascalCase")]
struct RawHealthcheck {
    #[serde(deserialize_with = "nullable")]
    test: Vec<String>,
    interval: i64,
    timeout: i64,
    retries: i64,
    start_period: i64,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "PascalCase")]
struct RawConfig {
    image: String,
    #[serde(deserialize_with = "env_names")]
    env: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    labels: BTreeMap<String, String>,
    #[serde(deserialize_with = "nullable")]
    exposed_ports: BTreeMap<String, Value>,
    healthcheck: Option<RawHealthcheck>,
    working_dir: String,
    user: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawNet {
    #[serde(rename = "IPAddress")]
    ip: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "PascalCase")]
struct RawNetworkSettings {
    #[serde(deserialize_with = "nullable")]
    ports: BTreeMap<String, Option<Vec<RawBinding>>>,
    #[serde(deserialize_with = "nullable")]
    networks: BTreeMap<String, RawNet>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "PascalCase")]
struct RawInspect {
    id: String,
    created: String,
    path: String,
    #[serde(deserialize_with = "nullable")]
    args: Vec<String>,
    state: RawState,
    image: String,
    name: String,
    restart_count: i64,
    platform: String,
    host_config: RawHostConfig,
    #[serde(deserialize_with = "nullable")]
    mounts: Vec<RawMount>,
    config: RawConfig,
    network_settings: RawNetworkSettings,
}

/// What a container is doing.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum State {
    Created,
    Running,
    Paused,
    Restarting,
    Removing,
    Exited(i64),
    Dead,
    #[default]
    Unknown,
}

impl State {
    /// `Running`, `Exited (1)`, `Restarting`.
    pub fn label(&self) -> String {
        match self {
            State::Created => "Created".into(),
            State::Running => "Running".into(),
            State::Paused => "Paused".into(),
            State::Restarting => "Restarting".into(),
            State::Removing => "Removing".into(),
            State::Exited(c) => format!("Exited ({c})"),
            State::Dead => "Dead".into(),
            State::Unknown => "Unknown".into(),
        }
    }

    pub fn running(&self) -> bool {
        matches!(self, State::Running)
    }

    /// The process exists (running, paused or between restarts).
    pub fn up(&self) -> bool {
        matches!(self, State::Running | State::Paused | State::Restarting)
    }
}

/// A health check's verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HealthStatus {
    Healthy,
    Unhealthy,
    /// Within the start period, or no probe finished yet.
    Starting,
    #[default]
    None,
}

impl HealthStatus {
    fn parse(s: &str) -> Self {
        match s {
            "healthy" => HealthStatus::Healthy,
            "unhealthy" => HealthStatus::Unhealthy,
            "starting" => HealthStatus::Starting,
            _ => HealthStatus::None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            HealthStatus::Healthy => "Healthy",
            HealthStatus::Unhealthy => "Unhealthy",
            HealthStatus::Starting => "Starting",
            HealthStatus::None => "No health check",
        }
    }
}

/// One health probe.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Probe {
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub exit_code: i64,
    /// What the probe printed (trimmed, redacted, at most 600 characters).
    pub output: String,
}

/// `State.Health`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Health {
    pub status: HealthStatus,
    /// Probes failed in a row.
    pub failing_streak: i64,
    /// The last probes docker kept (up to five), oldest first.
    pub log: Vec<Probe>,
}

/// `Config.Healthcheck`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HealthcheckConfig {
    /// `pg_isready -U postgres` (`CMD-SHELL` / `CMD` dropped), redacted.
    pub test: String,
    pub interval_s: f64,
    pub timeout_s: f64,
    pub retries: i64,
    pub start_period_s: f64,
}

/// A port of a container.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Port {
    pub container_port: u16,
    /// `tcp`, `udp`, `sctp`.
    pub proto: String,
    /// `0.0.0.0`, `127.0.0.1`, `::`, or empty for "all".
    pub host_ip: String,
    /// `None`: exposed by the image but not published.
    pub host_port: Option<u16>,
    /// Published on IPv6 as well.
    pub ipv6: bool,
}

impl Port {
    pub fn published(&self) -> bool {
        self.host_port.is_some()
    }

    /// `http://localhost:8080` for a published TCP port; `host` replaces
    /// `localhost` for a remote daemon.
    pub fn url(&self, host: &str) -> Option<String> {
        if self.proto != "tcp" {
            return None;
        }
        let p = self.host_port?;
        let h = match self.host_ip.as_str() {
            "" | "0.0.0.0" | "::" => host.to_owned(),
            ip if ip.contains(':') => format!("[{ip}]"),
            ip => ip.to_owned(),
        };
        Some(format!("http://{h}:{p}"))
    }

    /// `0.0.0.0:8080 → 80/tcp`, `80/tcp (not published)`.
    pub fn text(&self) -> String {
        match self.host_port {
            Some(hp) => {
                let ip = if self.host_ip.is_empty() {
                    "0.0.0.0"
                } else {
                    &self.host_ip
                };
                format!(
                    "{ip}:{hp} → {}/{}{}",
                    self.container_port,
                    self.proto,
                    if self.ipv6 { " (and IPv6)" } else { "" }
                )
            }
            None => format!("{}/{} (not published)", self.container_port, self.proto),
        }
    }
}

/// `8080/tcp` → `(8080, "tcp")`.
fn port_key(k: &str) -> Option<(u16, String)> {
    let (n, proto) = k.split_once('/').unwrap_or((k, "tcp"));
    Some((n.trim().parse().ok()?, proto.trim().to_ascii_lowercase()))
}

fn ports_of(
    live: &BTreeMap<String, Option<Vec<RawBinding>>>,
    configured: &BTreeMap<String, Option<Vec<RawBinding>>>,
    exposed: &BTreeMap<String, Value>,
) -> Vec<Port> {
    // Running containers report what is bound; stopped ones only what was
    // asked for.
    let map = if live.is_empty() { configured } else { live };
    let mut out: Vec<Port> = Vec::new();
    for (k, binds) in map {
        let Some((cp, proto)) = port_key(k) else {
            continue;
        };
        let binds = binds.clone().unwrap_or_default();
        if binds.is_empty() {
            out.push(Port {
                container_port: cp,
                proto: proto.clone(),
                ..Port::default()
            });
            continue;
        }
        for b in binds {
            let hp = b.host_port.trim().parse::<u16>().ok();
            let v6 = b.host_ip.contains(':');
            if let Some(same) = out.iter_mut().find(|p| {
                p.container_port == cp
                    && p.proto == proto
                    && p.host_port == hp
                    && p.host_port.is_some()
            }) {
                if v6 {
                    same.ipv6 = true;
                } else if same.host_ip.contains(':') {
                    // Keep the IPv4 address in front.
                    same.host_ip = b.host_ip.clone();
                    same.ipv6 = true;
                }
                continue;
            }
            out.push(Port {
                container_port: cp,
                proto: proto.clone(),
                host_ip: b.host_ip.clone(),
                host_port: hp,
                ipv6: false,
            });
        }
    }
    for k in exposed.keys() {
        if let Some((cp, proto)) = port_key(k)
            && !out
                .iter()
                .any(|p| p.container_port == cp && p.proto == proto)
        {
            out.push(Port {
                container_port: cp,
                proto,
                ..Port::default()
            });
        }
    }
    out.sort();
    out
}

/// A mount.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mount {
    /// `bind`, `volume`, `tmpfs`, `npipe`.
    pub kind: String,
    /// The host path, or the volume's name.
    pub source: String,
    pub destination: String,
    pub read_only: bool,
}

/// A network the container is attached to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Network {
    pub name: String,
    pub ip: String,
}

/// The compose labels of a container.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ComposeInfo {
    pub project: String,
    pub service: String,
    pub number: Option<u32>,
    /// Started with `compose run` (a one-off).
    pub oneoff: bool,
    pub working_dir: String,
    /// `com.docker.compose.project.config_files`, split.
    pub config_files: Vec<String>,
    /// The compose version that created it.
    pub version: String,
}

pub const LABEL_PROJECT: &str = "com.docker.compose.project";
pub const LABEL_SERVICE: &str = "com.docker.compose.service";
pub const LABEL_NUMBER: &str = "com.docker.compose.container-number";
pub const LABEL_ONEOFF: &str = "com.docker.compose.oneoff";
pub const LABEL_WORKING_DIR: &str = "com.docker.compose.project.working_dir";
pub const LABEL_CONFIG_FILES: &str = "com.docker.compose.project.config_files";
pub const LABEL_COMPOSE_VERSION: &str = "com.docker.compose.version";

/// `a.yml,b.yml` → both (paths with a comma would be ambiguous; compose
/// writes them this way).
pub fn split_files(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(str::to_owned)
        .collect()
}

impl ComposeInfo {
    pub fn from_labels(l: &BTreeMap<String, String>) -> Option<Self> {
        let project = l.get(LABEL_PROJECT)?.trim().to_owned();
        if project.is_empty() {
            return None;
        }
        Some(Self {
            project,
            service: l.get(LABEL_SERVICE).cloned().unwrap_or_default(),
            number: l.get(LABEL_NUMBER).and_then(|n| n.trim().parse().ok()),
            oneoff: l
                .get(LABEL_ONEOFF)
                .is_some_and(|v| v.eq_ignore_ascii_case("true")),
            working_dir: l.get(LABEL_WORKING_DIR).cloned().unwrap_or_default(),
            config_files: l
                .get(LABEL_CONFIG_FILES)
                .map(|s| split_files(s))
                .unwrap_or_default(),
            version: l.get(LABEL_COMPOSE_VERSION).cloned().unwrap_or_default(),
        })
    }
}

/// A container, from `docker inspect`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Container {
    /// The full id.
    pub id: String,
    /// Without the leading `/`.
    pub name: String,
    pub created: Option<i64>,
    pub state: State,
    /// `ps`'s STATUS column: `Up 3 minutes (healthy)`.
    pub status_text: String,
    pub health: Option<Health>,
    pub healthcheck: Option<HealthcheckConfig>,
    pub oom_killed: bool,
    /// Why docker could not start it (`exec format error`, a port in use).
    pub error: String,
    pub exit_code: i64,
    pub pid: i64,
    pub restart_count: i64,
    pub started: Option<i64>,
    pub finished: Option<i64>,
    pub ports: Vec<Port>,
    pub mounts: Vec<Mount>,
    pub networks: Vec<Network>,
    pub network_mode: String,
    /// `no`, `always`, `unless-stopped`, `on-failure`.
    pub restart_policy: String,
    pub restart_max: i64,
    pub auto_remove: bool,
    /// The reference it was created from (`Config.Image`): `postgres:16`.
    pub image: String,
    /// `sha256:…` of the image.
    pub image_id: String,
    pub labels: BTreeMap<String, String>,
    /// Names of the environment variables (never their values).
    pub env_names: Vec<String>,
    pub compose: Option<ComposeInfo>,
    /// Entrypoint and arguments, with secret-looking values masked.
    pub command: String,
    pub platform: String,
    pub user: String,
    pub working_dir: String,
}

impl Container {
    /// 12 characters, as `docker ps` shows it.
    pub fn short_id(&self) -> &str {
        &self.id[..self.id.len().min(12)]
    }

    /// The health check's verdict while the container is up; `None` once it
    /// stopped (docker keeps reporting `starting` for a stopped container,
    /// which means nothing).
    pub fn health_status(&self) -> HealthStatus {
        if !self.state.up() {
            return HealthStatus::None;
        }
        self.health.as_ref().map(|h| h.status).unwrap_or_default()
    }

    pub fn unhealthy(&self) -> bool {
        self.health_status() == HealthStatus::Unhealthy
    }

    /// It ran and then ended with a non-zero exit code (not "created and
    /// never started").
    pub fn failed(&self) -> bool {
        matches!(self.state, State::Exited(c) if c != 0) && self.started.is_some()
            || self.state == State::Dead
    }

    /// Needs attention: unhealthy, restarting, or ended with an error.
    pub fn troubled(&self) -> bool {
        self.unhealthy() || self.state == State::Restarting || self.failed()
    }

    /// `web-1` for `shop-web-1` (compose), else the name.
    pub fn display_name(&self) -> &str {
        &self.name
    }

    pub fn label(&self, k: &str) -> Option<&str> {
        self.labels.get(k).map(String::as_str)
    }
}

fn container_from_raw(r: RawInspect) -> Container {
    let health = r.state.health.map(|h| Health {
        status: HealthStatus::parse(&h.status),
        failing_streak: h.failing_streak,
        log: h
            .log
            .into_iter()
            .map(|p| {
                let mut out = redact(p.output.trim());
                if out.chars().count() > 600 {
                    out = format!("{}…", out.chars().take(600).collect::<String>());
                }
                Probe {
                    start: time(&p.start),
                    end: time(&p.end),
                    exit_code: p.exit_code,
                    output: out,
                }
            })
            .collect(),
    });
    let healthcheck = r.config.healthcheck.as_ref().and_then(|h| {
        let (kind, rest) = h.test.split_first()?;
        if kind == "NONE" {
            return None;
        }
        let test = if kind == "CMD-SHELL" {
            redact(&rest.join(" "))
        } else {
            mask_command(rest)
        };
        let secs = |ns: i64| ns as f64 / 1e9;
        Some(HealthcheckConfig {
            test,
            interval_s: secs(h.interval),
            timeout_s: secs(h.timeout),
            retries: h.retries,
            start_period_s: secs(h.start_period),
        })
    });
    let state = match r.state.status.as_str() {
        "running" if r.state.paused => State::Paused,
        "running" if r.state.restarting => State::Restarting,
        "running" => State::Running,
        "paused" => State::Paused,
        "restarting" => State::Restarting,
        "created" => State::Created,
        "removing" => State::Removing,
        "exited" => State::Exited(r.state.exit_code),
        "dead" => State::Dead,
        _ if r.state.dead => State::Dead,
        _ if r.state.running => State::Running,
        _ => State::Unknown,
    };
    let mut command = Vec::new();
    if !r.path.is_empty() {
        command.push(r.path.clone());
    }
    command.extend(r.args.iter().cloned());
    let labels = r.config.labels.clone();
    Container {
        id: r.id,
        name: r.name.trim_start_matches('/').to_owned(),
        created: time(&r.created),
        state,
        status_text: String::new(),
        health,
        healthcheck,
        oom_killed: r.state.oom_killed,
        error: r.state.error,
        exit_code: r.state.exit_code,
        pid: r.state.pid,
        restart_count: r.restart_count,
        started: time(&r.state.started_at),
        finished: time(&r.state.finished_at),
        ports: ports_of(
            &r.network_settings.ports,
            &r.host_config.port_bindings,
            &r.config.exposed_ports,
        ),
        mounts: r
            .mounts
            .into_iter()
            .map(|m| Mount {
                source: if m.kind == "volume" && !m.name.is_empty() {
                    m.name
                } else {
                    m.source
                },
                kind: m.kind,
                destination: m.destination,
                read_only: m.rw == Some(false),
            })
            .collect(),
        networks: r
            .network_settings
            .networks
            .into_iter()
            .map(|(name, n)| Network { name, ip: n.ip })
            .collect(),
        network_mode: r.host_config.network_mode,
        restart_policy: if r.host_config.restart_policy.name.is_empty() {
            "no".into()
        } else {
            r.host_config.restart_policy.name
        },
        restart_max: r.host_config.restart_policy.maximum_retry_count,
        auto_remove: r.host_config.auto_remove,
        image: r.config.image,
        image_id: r.image,
        compose: ComposeInfo::from_labels(&labels),
        labels,
        env_names: r.config.env,
        command: mask_command(&command),
        platform: r.platform,
        user: r.config.user,
        working_dir: r.config.working_dir,
    }
}

/// `docker inspect --type container ID…` (an array). Containers removed
/// between `ps` and `inspect` are simply missing.
pub fn parse_inspect(bytes: &[u8]) -> Result<Vec<Container>, String> {
    let v = json_rows(bytes, "inspect")?;
    let mut out: Vec<Container> = v
        .into_iter()
        .filter_map(|x| serde_json::from_value::<RawInspect>(x).ok())
        .filter(|r| !r.id.is_empty())
        .map(container_from_raw)
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Copy `ps`'s STATUS text into the inspected containers.
pub fn merge_status(containers: &mut [Container], ps: &[PsRow]) {
    for c in containers {
        if let Some(r) = ps.iter().find(|r| r.id == c.id) {
            c.status_text = r.status.clone();
        }
    }
}

/// `docker inspect` JSON with every environment value replaced by `•••`
/// (keys named `Env`, anywhere), pretty-printed, then run through the
/// exposure rules: for the "Inspect" view.
pub fn mask_inspect(bytes: &[u8]) -> Result<String, String> {
    let mut v: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("docker inspect: not JSON ({e})"))?;
    fn walk(v: &mut Value) {
        match v {
            Value::Object(m) => {
                for (k, x) in m.iter_mut() {
                    if k == "Env"
                        && let Value::Array(items) = x
                    {
                        for it in items.iter_mut() {
                            if let Value::String(s) = it {
                                let name = s.split_once('=').map_or(s.as_str(), |(k, _)| k);
                                *it = Value::String(format!("{name}=•••"));
                            }
                        }
                        continue;
                    }
                    walk(x);
                }
            }
            Value::Array(a) => a.iter_mut().for_each(walk),
            _ => {}
        }
    }
    walk(&mut v);
    let pretty = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    Ok(redact(&pretty))
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "PascalCase")]
struct RawImageConfig {
    #[serde(deserialize_with = "nullable")]
    labels: BTreeMap<String, String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "PascalCase")]
struct RawImage {
    id: String,
    #[serde(deserialize_with = "nullable")]
    repo_tags: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    repo_digests: Vec<String>,
    created: String,
    size: i64,
    os: String,
    architecture: String,
    config: Option<RawImageConfig>,
}

/// An image, from `docker image inspect`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Image {
    /// `sha256:…`.
    pub id: String,
    /// `ghcr.io/acme/shop:sha-1a2b3c4`.
    pub tags: Vec<String>,
    /// `ghcr.io/acme/shop@sha256:…`.
    pub digests: Vec<String>,
    pub created: Option<i64>,
    pub size: u64,
    pub os: String,
    pub arch: String,
    pub labels: BTreeMap<String, String>,
}

impl Image {
    pub fn label(&self, k: &str) -> Option<&str> {
        self.labels
            .get(k)
            .map(String::as_str)
            .filter(|v| !v.trim().is_empty())
    }

    /// `sha256:48fd573913…` → `48fd57391398`.
    pub fn short_id(&self) -> &str {
        let id = self.id.strip_prefix("sha256:").unwrap_or(&self.id);
        &id[..id.len().min(12)]
    }
}

pub fn parse_images(bytes: &[u8]) -> Result<Vec<Image>, String> {
    let raw: Vec<RawImage> = rows(bytes, "image inspect")?;
    Ok(raw
        .into_iter()
        .filter(|r| !r.id.is_empty())
        .map(|r| Image {
            id: r.id,
            tags: r.repo_tags,
            digests: r.repo_digests,
            created: time(&r.created),
            size: r.size.max(0) as u64,
            os: r.os,
            arch: r.architecture,
            labels: r.config.map(|c| c.labels).unwrap_or_default(),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Compose projects (compose ls)
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawComposeLs {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Status")]
    status: String,
    #[serde(rename = "ConfigFiles")]
    config_files: String,
}

/// A project of `docker compose ls -a`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ComposeLs {
    pub name: String,
    /// `running(2), exited(1)`.
    pub status: String,
    pub config_files: Vec<String>,
}

pub fn parse_compose_ls(bytes: &[u8]) -> Result<Vec<ComposeLs>, String> {
    let raw: Vec<RawComposeLs> = rows(bytes, "compose ls")?;
    Ok(raw
        .into_iter()
        .filter(|r| !r.name.is_empty())
        .map(|r| ComposeLs {
            name: r.name,
            status: r.status,
            config_files: split_files(&r.config_files),
        })
        .collect())
}

/// `running(2), exited(1)` → `[("running", 2), ("exited", 1)]`.
pub fn status_counts(s: &str) -> Vec<(String, usize)> {
    s.split(',')
        .filter_map(|part| {
            let part = part.trim();
            let (state, rest) = part.split_once('(')?;
            let n = rest.trim_end_matches(')').trim().parse().ok()?;
            Some((state.trim().to_owned(), n))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawStats {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Container")]
    container: String,
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "CPUPerc")]
    cpu: String,
    #[serde(rename = "MemUsage")]
    mem: String,
    #[serde(rename = "MemPerc")]
    mem_perc: String,
    #[serde(rename = "NetIO")]
    net: String,
    #[serde(rename = "BlockIO")]
    block: String,
    #[serde(rename = "PIDs")]
    pids: String,
}

/// One sample of `docker stats --no-stream`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub id: String,
    pub name: String,
    /// Percent of one CPU (can pass 100 on several cores).
    pub cpu: Option<f32>,
    /// `20.5MiB / 7.6GiB`.
    pub mem: String,
    pub mem_perc: Option<f32>,
    pub net: String,
    pub block: String,
    pub pids: Option<u32>,
}

fn percent(s: &str) -> Option<f32> {
    s.trim().trim_end_matches('%').trim().parse().ok()
}

pub fn parse_stats(bytes: &[u8]) -> Result<Vec<Stats>, String> {
    let raw: Vec<RawStats> = rows(bytes, "stats")?;
    Ok(raw
        .into_iter()
        .map(|r| Stats {
            id: if r.id.is_empty() { r.container } else { r.id },
            name: r.name,
            cpu: percent(&r.cpu),
            mem: r.mem,
            mem_perc: percent(&r.mem_perc),
            net: r.net,
            block: r.block,
            pids: r.pids.trim().parse().ok(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic fixtures only: no real `docker inspect` output (it holds
    // environment values).

    pub(crate) const INSPECT: &str = include_str!("fixtures/inspect.json");
    const CONTEXTS_NDJSON: &str = "{\"Current\":false,\"Description\":\"Current DOCKER_HOST based configuration\",\"DockerEndpoint\":\"npipe:////./pipe/docker_engine\",\"Error\":\"\",\"Name\":\"default\"}\n{\"Current\":true,\"Description\":\"Docker Desktop\",\"DockerEndpoint\":\"npipe:////./pipe/dockerDesktopLinuxEngine\",\"Error\":\"\",\"Name\":\"desktop-linux\"}\n";

    #[test]
    fn contexts_from_lines_or_an_array() {
        let a = parse_contexts(CONTEXTS_NDJSON.as_bytes()).unwrap();
        assert_eq!(a.len(), 2);
        assert!(a[1].current && !a[0].current);
        assert!(!a[1].remote());
        let arr = r#"[{"Name":"staging","DockerEndpoint":"ssh://deploy@staging","Current":"*"},{"Name":"default","DockerEndpoint":"unix:///var/run/docker.sock"}]"#;
        let b = parse_contexts(arr.as_bytes()).unwrap();
        assert_eq!(b[0].name, "staging");
        assert!(b[0].current && b[0].remote());
        assert_eq!(choose_context(&a, Some("default"), None), "default");
        assert_eq!(choose_context(&a, Some("gone"), None), "desktop-linux");
        let none_current: Vec<Context> = a
            .iter()
            .map(|c| Context {
                current: false,
                ..c.clone()
            })
            .collect();
        assert_eq!(
            choose_context(&none_current, None, Some("desktop-linux")),
            "desktop-linux"
        );
        assert_eq!(choose_context(&none_current, None, Some("nope")), "default");
        assert_eq!(choose_context(&[], None, None), "default");
    }

    #[test]
    fn version_with_and_without_a_daemon() {
        let up = r#"{"Client":{"Version":"29.6.2","ApiVersion":"1.52","Os":"windows","Arch":"amd64","Context":"desktop-linux"},"Server":{"Platform":{"Name":"Docker Desktop 4.84.0 (234817)"},"Version":"29.6.2","ApiVersion":"1.52","Os":"linux","Arch":"amd64","KernelVersion":"6.18.33"}}"#;
        let v = parse_version(up.as_bytes()).unwrap();
        assert_eq!(v.client, "29.6.2");
        let s = v.server.as_ref().unwrap();
        assert_eq!((s.os.as_str(), s.arch.as_str()), ("linux", "amd64"));
        assert!(v.server_text().contains("Docker Desktop 4.84.0"));
        let down = r#"{"Client":{"Version":"29.6.2","Os":"windows"},"Server":null}"#;
        let v = parse_version(down.as_bytes()).unwrap();
        assert!(v.server.is_none());
        assert_eq!(v.server_text(), "not answering");
        assert!(parse_version(b"garbage").is_none());
    }

    #[test]
    fn ps_rows() {
        let ps = "{\"ID\":\"aaa\",\"Names\":\"shop-web-1\",\"Image\":\"shop-web\",\"State\":\"running\",\"Status\":\"Up 3 minutes (healthy)\",\"RunningFor\":\"3 minutes ago\",\"Labels\":\"a=b,c=d\"}\n";
        let r = parse_ps(ps.as_bytes()).unwrap();
        assert_eq!(r[0].name, "shop-web-1");
        assert_eq!(r[0].status, "Up 3 minutes (healthy)");
        assert!(parse_ps(b"").unwrap().is_empty());
        assert!(parse_ps(b"not json").is_err());
    }

    #[test]
    fn inspect_keeps_names_of_environment_variables_only() {
        let c = parse_inspect(INSPECT.as_bytes()).unwrap();
        let text = format!("{c:?}");
        assert!(
            !text.contains("example-only-value"),
            "an environment value reached the model"
        );
        let db = c.iter().find(|c| c.name == "shop-db-1").unwrap();
        assert_eq!(
            db.env_names,
            ["PGDATA", "POSTGRES_PASSWORD", "POSTGRES_USER"]
        );
        // The command's password flag is masked.
        assert!(!db.command.contains("hunter2"), "{}", db.command);
        assert!(db.command.contains("--password=•••"), "{}", db.command);
    }

    #[test]
    fn inspect_states_health_ports_and_compose_labels() {
        let c = parse_inspect(INSPECT.as_bytes()).unwrap();
        let web = c.iter().find(|c| c.name == "shop-web-1").unwrap();
        assert_eq!(web.state, State::Running);
        assert_eq!(web.health_status(), HealthStatus::Healthy);
        let compose = web.compose.as_ref().unwrap();
        assert_eq!(compose.project, "shop");
        assert_eq!(compose.service, "web");
        assert_eq!(compose.number, Some(1));
        assert_eq!(compose.config_files.len(), 2);
        // 0.0.0.0 and :: of the same port are one entry.
        let p80 = web.ports.iter().find(|p| p.container_port == 80).unwrap();
        assert_eq!(p80.host_port, Some(8080));
        assert!(p80.ipv6);
        assert_eq!(
            p80.url("localhost").as_deref(),
            Some("http://localhost:8080")
        );
        assert_eq!(p80.text(), "0.0.0.0:8080 → 80/tcp (and IPv6)");
        let exposed = web.ports.iter().find(|p| p.container_port == 9000).unwrap();
        assert!(!exposed.published());
        assert_eq!(exposed.url("localhost"), None);
        assert_eq!(web.restart_policy, "unless-stopped");
        assert_eq!(web.mounts[0].kind, "bind");
        assert!(web.mounts[1].read_only);
        assert_eq!(web.networks[0].name, "shop_default");

        let db = c.iter().find(|c| c.name == "shop-db-1").unwrap();
        let h = db.health.as_ref().unwrap();
        assert_eq!(h.status, HealthStatus::Unhealthy);
        assert_eq!(h.failing_streak, 4);
        assert_eq!(h.log.len(), 2);
        assert_eq!(h.log[1].exit_code, 1);
        assert!(h.log[1].output.contains("no response"));
        assert!(db.unhealthy() && db.troubled());
        assert_eq!(
            db.healthcheck.as_ref().unwrap().test,
            "pg_isready -U shop -d shop"
        );
        assert_eq!(db.healthcheck.as_ref().unwrap().interval_s, 5.0);

        let job = c.iter().find(|c| c.name == "shop-migrate-1").unwrap();
        assert_eq!(job.state, State::Exited(1));
        assert!(job.failed());
        assert!(job.compose.as_ref().unwrap().oneoff);
        // A stopped container: the configured binding, no IPv6 duplicate.
        assert_eq!(job.ports[0].host_port, Some(5000));
        let created = c.iter().find(|c| c.name == "never-started").unwrap();
        assert_eq!(created.state, State::Created);
        assert!(created.started.is_none() && !created.failed());

        let worker = c.iter().find(|c| c.name == "shop-worker-1").unwrap();
        assert_eq!(worker.state, State::Restarting);
        assert_eq!(worker.restart_count, 7);
        assert!(worker.oom_killed);
        let cache = c.iter().find(|c| c.name == "devtools-cache-1").unwrap();
        assert_eq!(cache.state, State::Paused);
        assert!(cache.state.up() && !cache.state.running());
    }

    #[test]
    fn masked_inspect_hides_values() {
        let m = mask_inspect(INSPECT.as_bytes()).unwrap();
        assert!(!m.contains("example-only-value"));
        assert!(m.contains("POSTGRES_PASSWORD=•••"));
        assert!(mask_inspect(b"{").is_err());
    }

    #[test]
    fn command_masking() {
        let w = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            mask_command(&w(&["server", "--api-token", "abc", "--port", "80"])),
            "server --api-token ••• --port 80"
        );
        assert_eq!(
            mask_command(&w(&["env", "DB_PASSWORD=x", "MODE=dev"])),
            "env DB_PASSWORD=••• MODE=dev"
        );
        assert_eq!(
            mask_command(&w(&["sh", "-c", "echo hi"])),
            "sh -c \"echo hi\""
        );
    }

    #[test]
    fn images_compose_ls_and_stats() {
        let img = r#"[{"Id":"sha256:abcdef0123456789","RepoTags":["ghcr.io/acme/shop:sha-1a2b3c4"],"RepoDigests":null,"Created":"2026-10-28T17:09:55.908676436Z","Size":1048576,"Os":"linux","Architecture":"amd64","Config":{"Labels":{"org.opencontainers.image.revision":"1a2b3c4d"}}}]"#;
        let i = parse_images(img.as_bytes()).unwrap();
        assert_eq!(i[0].short_id(), "abcdef012345");
        assert_eq!(
            i[0].label("org.opencontainers.image.revision"),
            Some("1a2b3c4d")
        );
        assert!(i[0].digests.is_empty());
        assert_eq!(i[0].created, Some(1_793_207_395));
        let ls = r#"[{"Name":"shop","Status":"running(2), exited(1)","ConfigFiles":"C:\\r\\compose.yaml,C:\\r\\compose.override.yaml"}]"#;
        let p = parse_compose_ls(ls.as_bytes()).unwrap();
        assert_eq!(p[0].config_files.len(), 2);
        assert_eq!(
            status_counts(&p[0].status),
            [("running".to_owned(), 2), ("exited".to_owned(), 1)]
        );
        let st = "{\"BlockIO\":\"1MB / 0B\",\"CPUPerc\":\"12.50%\",\"Container\":\"aaa\",\"ID\":\"aaa\",\"MemPerc\":\"1.20%\",\"MemUsage\":\"20.5MiB / 7.6GiB\",\"Name\":\"shop-web-1\",\"NetIO\":\"1.2kB / 0B\",\"PIDs\":\"5\"}\n";
        let s = parse_stats(st.as_bytes()).unwrap();
        assert_eq!(s[0].cpu, Some(12.5));
        assert_eq!(s[0].pids, Some(5));
        assert_eq!(time("0001-01-01T00:00:00Z"), None);
    }
}
