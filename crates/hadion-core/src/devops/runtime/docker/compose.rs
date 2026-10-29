//! Compose projects: `docker compose config --format json` parsed into
//! services, and the compose files and Dockerfiles of the repository.
//!
//! `compose config` resolves `.env` files and variable interpolation, so
//! its output holds secrets. [`parse_config`] keeps the NAMES of
//! environment variables and build arguments and drops every value while
//! decoding; the raw output is never kept.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use super::model::{json_rows, redact};
use crate::devops::proc::{ProcSpec, run_capture};
use crate::devops::workbench::tasks::COMPOSE_FILES;

/// Override files docker compose merges by default, per base name.
pub const OVERRIDES: [&str; 4] = [
    "compose.override.yaml",
    "compose.override.yml",
    "docker-compose.override.yml",
    "docker-compose.override.yaml",
];

/// A service's `build:` section.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildSpec {
    /// The context directory (absolute, as compose resolves it).
    pub context: String,
    /// The Dockerfile, relative to the context unless absolute.
    pub dockerfile: String,
    pub target: Option<String>,
    /// Names of the build arguments (values dropped).
    pub arg_names: Vec<String>,
}

/// A published port of a service.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServicePort {
    pub target: u16,
    /// `8080`, `8000-8010`; `None` when only exposed to the project.
    pub published: Option<String>,
    pub protocol: String,
    pub host_ip: String,
}

impl ServicePort {
    /// `8080 → 80/tcp`, `127.0.0.1:8443 → 443/tcp`, `80/tcp`.
    pub fn text(&self) -> String {
        match &self.published {
            Some(p) if !self.host_ip.is_empty() && self.host_ip != "0.0.0.0" => {
                format!("{}:{p} → {}/{}", self.host_ip, self.target, self.protocol)
            }
            Some(p) => format!("{p} → {}/{}", self.target, self.protocol),
            None => format!("{}/{}", self.target, self.protocol),
        }
    }
}

/// A service of a compose project.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub image: Option<String>,
    pub build: Option<BuildSpec>,
    pub ports: Vec<ServicePort>,
    /// The health check command (redacted).
    pub healthcheck: Option<String>,
    pub health_interval: Option<String>,
    pub profiles: Vec<String>,
    /// `(service, condition)`: `("db", "service_healthy")`.
    pub depends_on: Vec<(String, String)>,
    /// Names of the environment variables (values dropped).
    pub env_names: Vec<String>,
    pub container_name: Option<String>,
    pub restart: Option<String>,
    pub replicas: Option<u64>,
}

/// What `compose config` says about a project.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ComposeConfig {
    /// The project name (`name:`, else the directory name).
    pub name: String,
    pub services: Vec<Service>,
    pub networks: Vec<String>,
    pub volumes: Vec<String>,
}

impl ComposeConfig {
    pub fn service(&self, name: &str) -> Option<&Service> {
        self.services.iter().find(|s| s.name == name)
    }
}

fn str_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Names of `environment:` / `args:` as a map or a `NAME=value` list.
fn names(v: Option<&Value>) -> Vec<String> {
    let mut out: Vec<String> = match v {
        Some(Value::Object(m)) => m.keys().cloned().collect(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(|kv| kv.split_once('=').map_or(kv, |(k, _)| k).to_owned())
            .collect(),
        _ => Vec::new(),
    };
    out.retain(|k| !k.is_empty());
    out.sort();
    out.dedup();
    out
}

fn service_of(name: &str, v: &Value) -> Service {
    let get = |k: &str| v.get(k);
    let build = get("build").map(|b| match b {
        Value::String(ctx) => BuildSpec {
            context: ctx.clone(),
            dockerfile: "Dockerfile".into(),
            ..BuildSpec::default()
        },
        _ => BuildSpec {
            context: b
                .get("context")
                .and_then(str_of)
                .unwrap_or_else(|| ".".into()),
            dockerfile: b
                .get("dockerfile")
                .and_then(str_of)
                .unwrap_or_else(|| "Dockerfile".into()),
            target: b.get("target").and_then(str_of),
            arg_names: names(b.get("args")),
        },
    });
    let ports = get("ports")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|p| match p {
                    Value::Object(_) => Some(ServicePort {
                        target: p
                            .get("target")
                            .and_then(str_of)
                            .and_then(|t| t.parse().ok())
                            .unwrap_or(0),
                        published: p
                            .get("published")
                            .and_then(str_of)
                            .filter(|s| !s.is_empty()),
                        protocol: p
                            .get("protocol")
                            .and_then(str_of)
                            .unwrap_or_else(|| "tcp".into()),
                        host_ip: p.get("host_ip").and_then(str_of).unwrap_or_default(),
                    }),
                    // Short syntax (older compose): `8080:80/tcp`.
                    Value::String(s) => {
                        let (spec, proto) = s.split_once('/').unwrap_or((s, "tcp"));
                        let parts: Vec<&str> = spec.rsplitn(3, ':').collect();
                        Some(ServicePort {
                            target: parts.first()?.parse().ok()?,
                            published: parts.get(1).map(|p| (*p).to_owned()),
                            protocol: proto.to_owned(),
                            host_ip: parts.get(2).map(|p| (*p).to_owned()).unwrap_or_default(),
                        })
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let hc = get("healthcheck");
    let disabled = hc
        .and_then(|h| h.get("disable"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let healthcheck = hc.filter(|_| !disabled).and_then(|h| {
        let test = h.get("test")?;
        let text = match test {
            Value::String(s) => s.clone(),
            Value::Array(a) => {
                let words: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
                match words.split_first() {
                    Some((&"NONE", _)) => return None,
                    Some((&("CMD" | "CMD-SHELL"), rest)) => rest.join(" "),
                    _ => words.join(" "),
                }
            }
            _ => return None,
        };
        Some(redact(&text))
    });
    let depends_on = match get("depends_on") {
        Some(Value::Object(m)) => m
            .iter()
            .map(|(k, d)| {
                (
                    k.clone(),
                    d.get("condition")
                        .and_then(str_of)
                        .unwrap_or_else(|| "service_started".into()),
                )
            })
            .collect(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(|k| (k.to_owned(), "service_started".to_owned()))
            .collect(),
        _ => Vec::new(),
    };
    Service {
        name: name.to_owned(),
        image: get("image").and_then(str_of),
        build,
        ports,
        health_interval: hc.and_then(|h| h.get("interval")).and_then(str_of),
        healthcheck,
        profiles: get("profiles")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(str_of).collect())
            .unwrap_or_default(),
        depends_on,
        env_names: names(get("environment")),
        container_name: get("container_name").and_then(str_of),
        restart: get("restart").and_then(str_of),
        replicas: get("deploy")
            .and_then(|d| d.get("replicas"))
            .and_then(Value::as_u64),
    }
}

/// `docker compose config --format json`, values dropped (see the module
/// docs).
pub fn parse_config(bytes: &[u8]) -> Result<ComposeConfig, String> {
    let v: Value = json_rows(bytes, "compose config")?
        .into_iter()
        .next()
        .ok_or_else(|| "docker compose config printed nothing".to_owned())?;
    let mut services: Vec<Service> = v
        .get("services")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, s)| service_of(k, s)).collect())
        .unwrap_or_default();
    services.sort_by(|a, b| a.name.cmp(&b.name));
    let keys = |k: &str| -> Vec<String> {
        v.get(k)
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    };
    Ok(ComposeConfig {
        name: v.get("name").and_then(str_of).unwrap_or_default(),
        services,
        networks: keys("networks"),
        volumes: keys("volumes"),
    })
}

/// The image compose gives a service it builds (no `image:`): compose v2
/// names it `<project>-<service>`.
pub fn built_image_name(project: &str, service: &str) -> String {
    super::commands::image_name(&format!("{project}-{service}"))
}

// ---------------------------------------------------------------------------
// The repository's compose files and Dockerfiles
// ---------------------------------------------------------------------------

/// A compose file of the repository (paths relative to the root, `/`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ComposeFile {
    pub rel: String,
    /// The override file docker merges with it by default, if present.
    pub overrides: Vec<String>,
}

impl ComposeFile {
    /// The directory, relative (`""` for the root).
    pub fn dir(&self) -> &str {
        self.rel.rsplit_once('/').map_or("", |(d, _)| d)
    }
}

/// Compose files and Dockerfiles found in the repository.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoFiles {
    pub compose: Vec<ComposeFile>,
    pub dockerfiles: Vec<String>,
}

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

fn skipped(p: &str) -> bool {
    p.split('/').any(|seg| {
        matches!(
            seg,
            "node_modules" | "vendor" | "target" | ".git" | "dist" | ".venv" | "venv"
        )
    })
}

/// Group listed paths into compose files (one per directory, the first of
/// [`COMPOSE_FILES`], with its override) and Dockerfiles.
pub fn group_files(paths: &[String]) -> RepoFiles {
    let mut by_dir: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut dockerfiles = Vec::new();
    for p in paths {
        let p = p.replace('\\', "/");
        if skipped(&p) {
            continue;
        }
        let name = basename(&p).to_ascii_lowercase();
        let dir = p
            .rsplit_once('/')
            .map_or(String::new(), |(d, _)| d.to_owned());
        if COMPOSE_FILES.contains(&name.as_str()) || OVERRIDES.contains(&name.as_str()) {
            by_dir.entry(dir).or_default().push(p.clone());
        } else if crate::devops::govern::docker::is_dockerfile(&p) {
            dockerfiles.push(p.clone());
        }
    }
    let mut compose = Vec::new();
    for (_, files) in by_dir {
        let name_of = |f: &String| basename(f).to_ascii_lowercase();
        let Some(main) = COMPOSE_FILES
            .iter()
            .find_map(|n| files.iter().find(|f| name_of(f) == *n))
        else {
            continue;
        };
        let overrides = OVERRIDES
            .iter()
            .filter_map(|n| files.iter().find(|f| name_of(f) == *n))
            .take(1)
            .cloned()
            .collect();
        compose.push(ComposeFile {
            rel: main.clone(),
            overrides,
        });
    }
    dockerfiles.sort();
    dockerfiles.dedup();
    RepoFiles {
        compose,
        dockerfiles,
    }
}

/// `git ls-files` (tracked and untracked, not ignored) of the compose
/// files and Dockerfiles, or a two-level walk (the Tasks discovery) when
/// `root` is not a repository.
pub fn find_files(root: &Path, commands: &mut Vec<String>) -> RepoFiles {
    let patterns = [
        "*compose.yaml",
        "*compose.yml",
        "*compose.override.yaml",
        "*compose.override.yml",
        "*Dockerfile*",
        "*dockerfile*",
        "*Containerfile*",
    ];
    let mut args = vec![
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
    ];
    args.extend(patterns);
    commands.push(format!("git {}", args.join(" ")));
    let spec = ProcSpec::new("git", root).args(args.iter().copied());
    match run_capture(&spec, Some(Duration::from_secs(30))) {
        Ok(out) if out.success() => {
            let paths: Vec<String> = out
                .stdout
                .split(|b| *b == 0)
                .filter(|p| !p.is_empty())
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect();
            group_files(&paths)
        }
        _ => {
            let mut paths = Vec::new();
            for info in crate::devops::workbench::tasks::walk(root, 2) {
                for f in &info.files {
                    let rel = if info.rel.is_empty() {
                        f.clone()
                    } else {
                        format!("{}/{f}", info.rel)
                    };
                    paths.push(rel);
                }
            }
            group_files(&paths)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = include_str!("fixtures/compose_config.json");

    #[test]
    fn config_keeps_names_and_drops_values() {
        let c = parse_config(CONFIG.as_bytes()).unwrap();
        assert!(
            !format!("{c:?}").contains("example-only-value"),
            "a value from .env reached the model"
        );
        assert_eq!(c.name, "shop");
        assert_eq!(
            c.services
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["db", "migrate", "web", "worker"]
        );
        let web = c.service("web").unwrap();
        let b = web.build.as_ref().unwrap();
        assert_eq!(b.dockerfile, "Dockerfile");
        assert_eq!(b.target.as_deref(), Some("prod"));
        assert_eq!(b.arg_names, ["NPM_TOKEN"]);
        assert_eq!(web.env_names, ["API_TOKEN", "NGINX_PORT"]);
        assert_eq!(web.ports[0].text(), "8080 → 80/tcp");
        assert_eq!(web.ports[1].text(), "127.0.0.1:8443 → 443/tcp");
        assert_eq!(
            web.healthcheck.as_deref(),
            Some("curl -fs http://localhost/healthz")
        );
        assert_eq!(
            web.depends_on,
            [("db".to_owned(), "service_healthy".to_owned())]
        );
        let db = c.service("db").unwrap();
        assert_eq!(db.image.as_deref(), Some("postgres:16-alpine"));
        assert_eq!(
            db.healthcheck.as_deref(),
            Some("pg_isready -U shop -d shop")
        );
        assert_eq!(db.env_names, ["POSTGRES_PASSWORD", "POSTGRES_USER"]);
        let migrate = c.service("migrate").unwrap();
        assert_eq!(migrate.profiles, ["tools"]);
        assert!(migrate.healthcheck.is_none(), "disabled");
        assert_eq!(c.service("worker").unwrap().replicas, Some(2));
        assert_eq!(c.volumes, ["pgdata"]);
        assert!(parse_config(b"").is_err());
    }

    #[test]
    fn files_are_grouped_per_directory() {
        let paths: Vec<String> = [
            "compose.yaml",
            "compose.override.yaml",
            "web/Dockerfile",
            "web/Dockerfile.dev",
            "ops/docker-compose.yml",
            "node_modules/x/compose.yaml",
            "docs/compose.md",
            "worker/worker.dockerfile",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let f = group_files(&paths);
        assert_eq!(f.compose.len(), 2);
        assert_eq!(f.compose[0].rel, "compose.yaml");
        assert_eq!(f.compose[0].overrides, ["compose.override.yaml"]);
        assert_eq!(f.compose[0].dir(), "");
        assert_eq!(f.compose[1].rel, "ops/docker-compose.yml");
        assert_eq!(f.compose[1].dir(), "ops");
        assert_eq!(
            f.dockerfiles,
            [
                "web/Dockerfile",
                "web/Dockerfile.dev",
                "worker/worker.dockerfile"
            ]
        );
        assert_eq!(built_image_name("Shop", "web"), "shop-web");
    }
}
