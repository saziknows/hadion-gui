//! A stand-in for `docker` (`HADION_DOCKER=<this exe>`): answers the reads
//! of the Ship page's Containers tab with synthetic data, prints plausible
//! output for writes and exits 0, and records every command line.
//!
//! - `FAKE_DOCKER_SCENARIO`: `normal` (default), `down` (the engine does not
//!   answer), `empty` (no containers), `remote` (an ssh context, current,
//!   plus an unreachable tcp one), `no-compose` (the Compose plugin is
//!   missing).
//! - `FAKE_DOCKER_REPO`: the repository the compose projects were started
//!   from (their working dir and config files); default: the current folder.
//! - `FAKE_DOCKER_REVISION`: the commit the standalone `api-preview`
//!   container's (and `shop-web`'s) image revision label names: pass a real
//!   commit of the opened repository.
//! - `FAKE_DOCKER_SOURCE`: the images' `org.opencontainers.image.source`
//!   (default `https://github.com/acme/shop`): one of the repository's
//!   remotes makes them "this repository".
//! - `FAKE_DOCKER_LOG`: append every argv here, one line each.
//! - `FAKE_DOCKER_EVENTS=end`: `events` fails after ~3 s (the app then
//!   polls every 10 s).
//!
//! `events` prints a container event every ~2 s until killed; `logs -f`
//! prints its tail, then a line a second until killed; `exec -it` is a tiny
//! interactive shell (`exit` ends it). Every value here is invented: the
//! environment variables carry `example-only-value` so tests can show that
//! no value reaches the app's models.
//!
//! `cargo build -p hadion-core --example fake_docker`

use std::io::{BufRead, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.trim().is_empty())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `YYYY-MM-DDTHH:MM:SSZ` of epoch seconds.
fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60
    )
}

/// A fake 64-hex id from a name.
fn id_of(name: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut out = String::new();
    while out.len() < 64 {
        for b in name.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        out.push_str(&format!("{h:016x}"));
    }
    out.truncate(64);
    out
}

fn sep() -> &'static str {
    if cfg!(windows) { "\\" } else { "/" }
}

struct World {
    scenario: String,
    repo: String,
    revision: String,
    source: String,
}

impl World {
    fn path(&self, rel: &str) -> String {
        let rel = rel.replace('/', sep());
        if rel.is_empty() {
            self.repo.clone()
        } else {
            format!("{}{}{rel}", self.repo.trim_end_matches(['/', '\\']), sep())
        }
    }

    fn elsewhere(&self, rel: &str) -> String {
        if cfg!(windows) {
            format!("D:\\sites\\{}", rel.replace('/', "\\"))
        } else {
            format!("/srv/sites/{rel}")
        }
    }
}

/// One container of the fake engine.
struct Ctr {
    name: &'static str,
    image: String,
    image_id: String,
    state: &'static str,
    exit: i64,
    health: Option<Value>,
    project: Option<(&'static str, &'static str, String, Vec<String>, bool)>,
    ports: Vec<(u16, u16)>,
    labels: Vec<(String, String)>,
    env: Vec<&'static str>,
    restarts: i64,
    oom: bool,
    cmd: Vec<&'static str>,
    started_ago: u64,
    finished_ago: Option<u64>,
    mounts: Vec<(&'static str, String, &'static str, bool)>,
}

struct Img {
    id: String,
    tags: Vec<String>,
    digests: Vec<String>,
    labels: Vec<(String, String)>,
    size: u64,
    age: u64,
}

fn images(w: &World) -> Vec<Img> {
    let rev7: String = w.revision.chars().take(7).collect();
    let compose_labels = |svc: &str| {
        vec![
            ("com.docker.compose.project".to_owned(), "shop".to_owned()),
            ("com.docker.compose.service".to_owned(), svc.to_owned()),
            ("com.docker.compose.version".to_owned(), "2.40.0".to_owned()),
        ]
    };
    let mut web_labels = compose_labels("web");
    web_labels.extend([
        (
            "org.opencontainers.image.revision".to_owned(),
            w.revision.clone(),
        ),
        (
            "org.opencontainers.image.source".to_owned(),
            w.source.clone(),
        ),
        (
            "org.opencontainers.image.created".to_owned(),
            rfc3339(now() - 3600),
        ),
    ]);
    vec![
        Img {
            id: format!("sha256:{}", id_of("img-web")),
            tags: vec!["shop-web:latest".into()],
            digests: vec![],
            labels: web_labels,
            size: 48_300_000,
            age: 3600,
        },
        Img {
            id: format!("sha256:{}", id_of("img-worker")),
            tags: vec!["shop-worker:latest".into()],
            digests: vec![],
            labels: compose_labels("worker"),
            size: 162_000_000,
            age: 7200,
        },
        Img {
            id: format!("sha256:{}", id_of("img-migrate")),
            tags: vec!["shop-migrate:latest".into()],
            digests: vec![],
            labels: compose_labels("migrate"),
            size: 38_000_000,
            age: 7200,
        },
        Img {
            id: format!("sha256:{}", id_of("img-postgres")),
            tags: vec!["postgres:16-alpine".into()],
            digests: vec![format!("postgres@sha256:{}", id_of("digest-postgres"))],
            labels: vec![],
            size: 116_000_000,
            age: 86_400 * 9,
        },
        Img {
            id: format!("sha256:{}", id_of("img-redis")),
            tags: vec!["redis:7".into()],
            digests: vec![format!("redis@sha256:{}", id_of("digest-redis"))],
            labels: vec![],
            size: 41_000_000,
            age: 86_400 * 20,
        },
        Img {
            id: format!("sha256:{}", id_of("img-mailpit")),
            tags: vec!["axllent/mailpit:v1.21".into()],
            digests: vec![format!(
                "axllent/mailpit@sha256:{}",
                id_of("digest-mailpit")
            )],
            labels: vec![("org.opencontainers.image.version".into(), "v1.21".into())],
            size: 28_000_000,
            age: 86_400 * 30,
        },
        Img {
            id: format!("sha256:{}", id_of("img-api")),
            tags: vec![format!("ghcr.io/acme/shop-api:sha-{rev7}")],
            digests: vec![format!(
                "ghcr.io/acme/shop-api@sha256:{}",
                id_of("digest-api")
            )],
            labels: vec![
                (
                    "org.opencontainers.image.revision".into(),
                    w.revision.clone(),
                ),
                ("org.opencontainers.image.source".into(), w.source.clone()),
                (
                    "org.opencontainers.image.created".into(),
                    rfc3339(now() - 86_400),
                ),
            ],
            size: 52_000_000,
            age: 86_400,
        },
        Img {
            id: format!("sha256:{}", id_of("img-blog")),
            tags: vec!["ghost:5".into()],
            digests: vec![format!("ghost@sha256:{}", id_of("digest-ghost"))],
            labels: vec![],
            size: 580_000_000,
            age: 86_400 * 60,
        },
    ]
}

fn image_id(name: &str) -> String {
    format!("sha256:{}", id_of(name))
}

fn containers(w: &World) -> Vec<Ctr> {
    if w.scenario == "empty" {
        return Vec::new();
    }
    let rev7: String = w.revision.chars().take(7).collect();
    let shop = |svc: &'static str, oneoff: bool| {
        Some((
            "shop",
            svc,
            w.path(""),
            vec![w.path("compose.yaml")],
            oneoff,
        ))
    };
    let devtools = |svc: &'static str| {
        Some((
            "devtools",
            svc,
            w.path("ops"),
            vec![w.path("ops/compose.yaml")],
            false,
        ))
    };
    let blog = |svc: &'static str| {
        Some((
            "blog",
            svc,
            w.elsewhere("blog"),
            vec![w.elsewhere("blog/compose.yaml")],
            false,
        ))
    };
    let t = now();
    let probe = |ago: u64, code: i64, out: &str| {
        json!({
            "Start": rfc3339(t - ago),
            "End": rfc3339(t - ago + 1),
            "ExitCode": code,
            "Output": out,
        })
    };
    // The worker restarts: its count grows every 6 seconds.
    let restarts = 7 + ((t / 6) % 5) as i64;
    vec![
        Ctr {
            name: "shop-web-1",
            image: "shop-web".into(),
            image_id: image_id("img-web"),
            state: "running",
            exit: 0,
            health: Some(json!({
                "Status": "healthy",
                "FailingStreak": 0,
                "Log": [probe(30, 0, "ok"), probe(20, 0, "ok"), probe(10, 0, "ok")],
            })),
            project: shop("web", false),
            ports: vec![(80, 8080), (443, 8443)],
            labels: vec![],
            env: vec!["API_TOKEN", "NGINX_PORT", "PATH"],
            restarts: 0,
            oom: false,
            cmd: vec!["nginx", "-g", "daemon off;"],
            started_ago: 3 * 3600,
            finished_ago: None,
            mounts: vec![
                ("bind", w.path("web/static"), "/usr/share/nginx/html", true),
                ("volume", "shop_certs".into(), "/etc/certs", false),
            ],
        },
        Ctr {
            name: "shop-worker-1",
            image: "shop-worker".into(),
            image_id: image_id("img-worker"),
            state: "restarting",
            exit: 137,
            health: None,
            project: shop("worker", false),
            ports: vec![],
            labels: vec![],
            env: vec!["QUEUE_URL", "WORKER_THREADS"],
            restarts,
            oom: true,
            cmd: vec!["worker", "--queue", "jobs"],
            started_ago: 4,
            finished_ago: Some(2),
            mounts: vec![],
        },
        Ctr {
            name: "shop-db-1",
            image: "postgres:16-alpine".into(),
            image_id: image_id("img-postgres"),
            state: "running",
            exit: 0,
            health: Some(json!({
                "Status": "unhealthy",
                "FailingStreak": 3,
                "Log": [
                    probe(50, 0, "/var/run/postgresql:5432 - accepting connections"),
                    probe(40, 0, "/var/run/postgresql:5432 - accepting connections"),
                    probe(30, 1, "/var/run/postgresql:5432 - no response"),
                    probe(20, 1, "/var/run/postgresql:5432 - no response"),
                    probe(10, 2, "pg_isready: could not connect to server: Connection refused"),
                ],
            })),
            project: shop("db", false),
            ports: vec![(5432, 5433)],
            labels: vec![],
            env: vec!["PGDATA", "POSTGRES_PASSWORD", "POSTGRES_USER"],
            restarts: 0,
            oom: false,
            cmd: vec![
                "docker-entrypoint.sh",
                "postgres",
                "--password=example-only-value",
            ],
            started_ago: 3 * 3600 + 60,
            finished_ago: None,
            mounts: vec![(
                "volume",
                "shop_pgdata".into(),
                "/var/lib/postgresql/data",
                false,
            )],
        },
        Ctr {
            name: "shop-migrate-1",
            image: "shop-migrate".into(),
            image_id: image_id("img-migrate"),
            state: "exited",
            exit: 1,
            health: None,
            project: shop("migrate", true),
            ports: vec![],
            labels: vec![],
            env: vec!["DATABASE_URL"],
            restarts: 0,
            oom: false,
            cmd: vec!["./migrate", "up"],
            started_ago: 2 * 3600,
            finished_ago: Some(2 * 3600 - 9),
            mounts: vec![],
        },
        Ctr {
            name: "devtools-cache-1",
            image: "redis:7".into(),
            image_id: image_id("img-redis"),
            state: "paused",
            exit: 0,
            health: None,
            project: devtools("cache"),
            ports: vec![(6379, 6379)],
            labels: vec![],
            env: vec![],
            restarts: 0,
            oom: false,
            cmd: vec!["redis-server"],
            started_ago: 5 * 3600,
            finished_ago: None,
            mounts: vec![],
        },
        Ctr {
            name: "devtools-mail-1",
            image: "axllent/mailpit:v1.21".into(),
            image_id: image_id("img-mailpit"),
            state: "running",
            exit: 0,
            health: None,
            project: devtools("mail"),
            ports: vec![(8025, 8025), (1025, 1025)],
            labels: vec![],
            env: vec!["MP_SMTP_AUTH"],
            restarts: 0,
            oom: false,
            cmd: vec!["/mailpit"],
            started_ago: 5 * 3600,
            finished_ago: None,
            mounts: vec![],
        },
        Ctr {
            name: "api-preview",
            image: format!("ghcr.io/acme/shop-api:sha-{rev7}"),
            image_id: image_id("img-api"),
            state: "running",
            exit: 0,
            health: Some(json!({"Status": "starting", "FailingStreak": 0, "Log": []})),
            project: None,
            ports: vec![(3000, 3001)],
            labels: vec![],
            env: vec!["API_KEY", "NODE_ENV"],
            restarts: 0,
            oom: false,
            cmd: vec!["node", "server.js"],
            started_ago: 40,
            finished_ago: None,
            mounts: vec![],
        },
        Ctr {
            name: "blog-web-1",
            image: "ghost:5".into(),
            image_id: image_id("img-blog"),
            state: "running",
            exit: 0,
            health: None,
            project: blog("web"),
            ports: vec![(2368, 2368)],
            labels: vec![],
            env: vec!["url"],
            restarts: 0,
            oom: false,
            cmd: vec!["node", "current/index.js"],
            started_ago: 86_400 * 2,
            finished_ago: None,
            mounts: vec![],
        },
        Ctr {
            name: "scratch-postgres",
            image: "postgres:16-alpine".into(),
            image_id: image_id("img-postgres"),
            state: "exited",
            exit: 0,
            health: None,
            project: None,
            ports: vec![],
            labels: vec![],
            env: vec!["POSTGRES_PASSWORD"],
            restarts: 0,
            oom: false,
            cmd: vec!["postgres"],
            started_ago: 86_400 * 4,
            finished_ago: Some(86_400 * 3),
            mounts: vec![],
        },
    ]
}

fn labels_of(c: &Ctr, images: &[Img]) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    if let Some(img) = images.iter().find(|i| i.id == c.image_id) {
        for (k, v) in &img.labels {
            m.insert(k.clone(), json!(v));
        }
    }
    if let Some((project, svc, dir, files, oneoff)) = &c.project {
        m.insert("com.docker.compose.project".into(), json!(project));
        m.insert("com.docker.compose.service".into(), json!(svc));
        m.insert("com.docker.compose.container-number".into(), json!("1"));
        m.insert(
            "com.docker.compose.oneoff".into(),
            json!(if *oneoff { "True" } else { "False" }),
        );
        m.insert("com.docker.compose.project.working_dir".into(), json!(dir));
        m.insert(
            "com.docker.compose.project.config_files".into(),
            json!(files.join(",")),
        );
        m.insert("com.docker.compose.version".into(), json!("2.40.0"));
    }
    for (k, v) in &c.labels {
        m.insert(k.clone(), json!(v));
    }
    m
}

fn inspect_json(c: &Ctr, images: &[Img]) -> Value {
    let t = now();
    let running = matches!(c.state, "running" | "paused" | "restarting");
    let bindings: serde_json::Map<String, Value> = c
        .ports
        .iter()
        .map(|(cp, hp)| {
            (
                format!("{cp}/tcp"),
                json!([{"HostIp": "0.0.0.0", "HostPort": hp.to_string()}, {"HostIp": "::", "HostPort": hp.to_string()}]),
            )
        })
        .collect();
    let mut state = json!({
        "Status": c.state,
        "Running": running,
        "Paused": c.state == "paused",
        "Restarting": c.state == "restarting",
        "OOMKilled": c.oom,
        "Dead": false,
        "Pid": if c.state == "running" { 4000 + c.name.len() as i64 } else { 0 },
        "ExitCode": c.exit,
        "Error": "",
        "StartedAt": rfc3339(t - c.started_ago),
        "FinishedAt": c.finished_ago.map(|a| rfc3339(t - a)).unwrap_or_else(|| "0001-01-01T00:00:00Z".into()),
    });
    if let Some(h) = &c.health {
        state["Health"] = h.clone();
    }
    let health_cfg = match c.name {
        "shop-db-1" => {
            json!({"Test": ["CMD-SHELL", "pg_isready -U shop -d shop"], "Interval": 10_000_000_000i64, "Timeout": 5_000_000_000i64, "Retries": 3, "StartPeriod": 20_000_000_000i64})
        }
        "shop-web-1" => {
            json!({"Test": ["CMD", "curl", "-fs", "http://localhost/healthz"], "Interval": 10_000_000_000i64, "Timeout": 2_000_000_000i64, "Retries": 3})
        }
        "api-preview" => {
            json!({"Test": ["CMD-SHELL", "wget -qO- http://localhost:3000/health"], "Interval": 15_000_000_000i64, "Timeout": 3_000_000_000i64, "Retries": 3, "StartPeriod": 60_000_000_000i64})
        }
        _ => Value::Null,
    };
    let env: Vec<String> = c
        .env
        .iter()
        .map(|k| format!("{k}=example-only-value"))
        .collect();
    let net = match &c.project {
        Some((p, ..)) => format!("{p}_default"),
        None => "bridge".into(),
    };
    let image_ref = c.image.clone();
    let live_ports: serde_json::Map<String, Value> = if running && c.state != "restarting" {
        bindings.clone()
    } else {
        serde_json::Map::new()
    };
    let ip = if running {
        format!("172.20.0.{}", 2 + c.name.len() % 200)
    } else {
        String::new()
    };
    let mut networks = serde_json::Map::new();
    networks.insert(net.clone(), json!({ "IPAddress": ip }));
    let mounts: Vec<Value> = c
        .mounts
        .iter()
        .map(|(kind, src, dst, rw)| {
            let volume = *kind == "volume";
            json!({
                "Type": kind,
                "Name": if volume { src.clone() } else { String::new() },
                "Source": if volume { format!("/var/lib/docker/volumes/{src}/_data") } else { src.clone() },
                "Destination": dst,
                "RW": rw,
            })
        })
        .collect();
    let exposed: serde_json::Map<String, Value> = c
        .ports
        .iter()
        .map(|(cp, _)| (format!("{cp}/tcp"), json!({})))
        .collect();
    let policy = if c.project.is_some() {
        "unless-stopped"
    } else {
        "no"
    };
    json!({
        "Id": id_of(c.name),
        "Created": rfc3339(t - c.started_ago - 30),
        "Path": c.cmd.first().copied().unwrap_or(""),
        "Args": c.cmd.iter().skip(1).copied().collect::<Vec<_>>(),
        "State": state,
        "Image": c.image_id,
        "Name": format!("/{}", c.name),
        "RestartCount": c.restarts,
        "Platform": "linux",
        "HostConfig": {
            "RestartPolicy": {"Name": policy, "MaximumRetryCount": 0},
            "PortBindings": bindings,
            "AutoRemove": false,
            "NetworkMode": net,
        },
        "Mounts": mounts,
        "Config": {
            "Image": image_ref,
            "Env": env,
            "Labels": labels_of(c, images),
            "ExposedPorts": exposed,
            "Healthcheck": health_cfg,
            "WorkingDir": "/app",
            "User": "",
        },
        "NetworkSettings": {
            "Ports": live_ports,
            "Networks": networks,
        },
    })
}

fn ps_row(c: &Ctr) -> Value {
    let status = match c.state {
        "running" => match c.health.as_ref().and_then(|h| h["Status"].as_str()) {
            Some(h) => format!("Up {} ({h})", ago_text(c.started_ago)),
            None => format!("Up {}", ago_text(c.started_ago)),
        },
        "paused" => format!("Up {} (Paused)", ago_text(c.started_ago)),
        "restarting" => format!(
            "Restarting ({}) {} ago",
            c.exit,
            ago_text(c.finished_ago.unwrap_or(1))
        ),
        "exited" => format!(
            "Exited ({}) {} ago",
            c.exit,
            ago_text(c.finished_ago.unwrap_or(60))
        ),
        other => other.to_owned(),
    };
    json!({
        "ID": id_of(c.name),
        "Names": c.name,
        "Image": c.image,
        "State": c.state,
        "Status": status,
        "RunningFor": format!("{} ago", ago_text(c.started_ago)),
    })
}

/// docker's way: `40 seconds`, `About a minute`, `3 minutes`, `About an
/// hour`, `3 hours`, `2 days`.
fn ago_text(s: u64) -> String {
    match s {
        0..60 => format!("{s} seconds"),
        60..120 => "About a minute".into(),
        120..3600 => format!("{} minutes", s / 60),
        3600..7200 => "About an hour".into(),
        7200..172_800 => format!("{} hours", s / 3600),
        _ => format!("{} days", s / 86_400),
    }
}

fn image_json(i: &Img) -> Value {
    json!({
        "Id": i.id,
        "RepoTags": i.tags,
        "RepoDigests": i.digests,
        "Created": rfc3339(now() - i.age),
        "Size": i.size,
        "Os": "linux",
        "Architecture": "amd64",
        "Config": {
            "Labels": i.labels.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<serde_json::Map<String, Value>>(),
            "Env": ["PATH=/usr/local/bin:/usr/bin", "SECRET_SEED=example-only-value"],
        },
    })
}

fn compose_config(file: &str) -> Value {
    let f = file.replace('\\', "/");
    if f.contains("/ops/") {
        return json!({
            "name": "devtools",
            "services": {
                "cache": {"image": "redis:7", "ports": [{"mode": "ingress", "target": 6379, "published": "6379", "protocol": "tcp"}]},
                "mail": {"image": "axllent/mailpit:v1.21", "environment": {"MP_SMTP_AUTH": "example-only-value"},
                         "ports": [{"mode": "ingress", "target": 8025, "published": "8025", "protocol": "tcp"}, {"mode": "ingress", "target": 1025, "published": "1025", "protocol": "tcp"}]}
            }
        });
    }
    if f.contains("/extras/") {
        return json!({
            "name": "extras",
            "services": {
                "grafana": {"image": "grafana/grafana:11.2.0", "environment": {"GF_SECURITY_ADMIN_PASSWORD": "example-only-value"},
                            "ports": [{"mode": "ingress", "target": 3000, "published": "3300", "protocol": "tcp"}],
                            "depends_on": {"prometheus": {"condition": "service_started", "required": true}}},
                "prometheus": {"image": "prom/prometheus:v2.54.1", "ports": [{"mode": "ingress", "target": 9090, "published": "9090", "protocol": "tcp"}],
                               "healthcheck": {"test": ["CMD", "wget", "-qO-", "http://localhost:9090/-/ready"], "interval": "15s"}}
            },
            "volumes": {"grafana": {"name": "extras_grafana"}}
        });
    }
    json!({
        "name": "shop",
        "services": {
            "web": {
                "build": {"context": file_dir(file, "web"), "dockerfile": "Dockerfile", "args": {"NPM_TOKEN": "example-only-value"}},
                "image": "shop-web",
                "depends_on": {"db": {"condition": "service_healthy", "required": true}},
                "environment": {"API_TOKEN": "example-only-value", "NGINX_PORT": "80"},
                "healthcheck": {"test": ["CMD", "curl", "-fs", "http://localhost/healthz"], "interval": "10s"},
                "ports": [{"mode": "ingress", "target": 80, "published": "8080", "protocol": "tcp"},
                          {"mode": "ingress", "target": 443, "published": "8443", "protocol": "tcp"}]
            },
            "worker": {
                "build": {"context": file_dir(file, "worker"), "dockerfile": "Dockerfile"},
                "depends_on": {"db": {"condition": "service_healthy", "required": true}},
                "environment": {"QUEUE_URL": "example-only-value", "WORKER_THREADS": "4"}
            },
            "db": {
                "image": "postgres:16-alpine",
                "environment": {"POSTGRES_PASSWORD": "example-only-value", "POSTGRES_USER": "shop", "PGDATA": "/var/lib/postgresql/data"},
                "healthcheck": {"test": ["CMD-SHELL", "pg_isready -U shop -d shop"], "interval": "10s", "retries": 3},
                "ports": [{"mode": "ingress", "target": 5432, "published": "5433", "protocol": "tcp"}],
                "volumes": [{"type": "volume", "source": "pgdata", "target": "/var/lib/postgresql/data"}]
            },
            "migrate": {"build": {"context": file_dir(file, "migrate"), "dockerfile": "Dockerfile"}, "profiles": ["tools"]},
            "docs": {"image": "nginx:1.27", "profiles": ["docs"], "ports": [{"mode": "ingress", "target": 80, "published": "8081", "protocol": "tcp"}]}
        },
        "volumes": {"pgdata": {"name": "shop_pgdata"}, "certs": {"name": "shop_certs"}}
    })
}

fn file_dir(file: &str, sub: &str) -> String {
    let dir = std::path::Path::new(file)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!("{dir}{}{sub}", sep())
}

fn contexts(w: &World) -> Vec<Value> {
    let local = if cfg!(windows) {
        "npipe:////./pipe/docker_engine"
    } else {
        "unix:///var/run/docker.sock"
    };
    let desktop = if cfg!(windows) {
        "npipe:////./pipe/dockerDesktopLinuxEngine"
    } else {
        "unix:///home/me/.docker/desktop/docker.sock"
    };
    let remote = w.scenario == "remote";
    let mut v = vec![
        json!({"Name": "default", "Description": "Current DOCKER_HOST based configuration", "DockerEndpoint": local, "Current": false, "Error": ""}),
        json!({"Name": "desktop-linux", "Description": "Docker Desktop", "DockerEndpoint": desktop, "Current": !remote, "Error": ""}),
    ];
    if remote {
        v.push(json!({"Name": "staging", "Description": "Staging box (fake)", "DockerEndpoint": "ssh://deploy@staging.example.com", "Current": true, "Error": ""}));
        v.push(json!({"Name": "prod", "Description": "Production (fake, unreachable)", "DockerEndpoint": "tcp://10.20.0.5:2376", "Current": false, "Error": ""}));
    }
    v
}

const DOWN: &str = "failed to connect to the docker API at npipe:////./pipe/dockerDesktopLinuxEngine; check if the path is correct and if the daemon is running: open //./pipe/dockerDesktopLinuxEngine: The system cannot find the file specified.";
const DOWN_UNIX: &str = "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?";
const UNREACHABLE: &str = "error during connect: Get \"http://10.20.0.5:2376/v1.51/version\": dial tcp 10.20.0.5:2376: connectex: A connection attempt failed because the connected party did not properly respond after a period of time.";
const NO_COMPOSE: &str = "docker: 'compose' is not a docker command.\nSee 'docker --help'";

fn fail(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1);
}

fn out(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn slow_lines(lines: &[String], pause_ms: u64) {
    let mut o = std::io::stdout();
    for l in lines {
        let _ = writeln!(o, "{l}");
        let _ = o.flush();
        std::thread::sleep(Duration::from_millis(pause_ms));
    }
}

/// Every option value of the given flags (`-f a -f b`).
fn values(args: &[String], flags: &[&str]) -> Vec<String> {
    let mut v = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if flags.contains(&args[i].as_str()) {
            if let Some(x) = args.get(i + 1) {
                v.push(x.clone());
            }
            i += 2;
            continue;
        }
        i += 1;
    }
    v
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(log) = env("FAKE_DOCKER_LOG")
        && let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
    {
        let _ = writeln!(f, "docker {}", args.join(" "));
    }
    let w = World {
        scenario: env("FAKE_DOCKER_SCENARIO").unwrap_or_else(|| "normal".into()),
        repo: env("FAKE_DOCKER_REPO").unwrap_or_else(|| {
            std::env::current_dir()
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_default()
        }),
        revision: env("FAKE_DOCKER_REVISION")
            .unwrap_or_else(|| "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b".into()),
        source: env("FAKE_DOCKER_SOURCE").unwrap_or_else(|| "https://github.com/acme/shop".into()),
    };
    // Global flags.
    let mut context = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--context" | "-c" => {
                context = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "-H" | "--host" | "--config" | "-l" | "--log-level" => i += 2,
            "--version" | "-v" => {
                println!("Docker version 29.6.2-fake, build fake0001");
                return;
            }
            a if a.starts_with('-') => i += 1,
            _ => break,
        }
    }
    let rest: Vec<String> = args[i.min(args.len())..].to_vec();
    let verb = rest.first().map(String::as_str).unwrap_or("");
    let down = w.scenario == "down";
    let unreachable = context == "prod";
    let down_msg = if cfg!(windows) { DOWN } else { DOWN_UNIX };
    let images = images(&w);
    let all = containers(&w);
    match verb {
        "context" if rest.get(1).map(String::as_str) == Some("ls") => {
            for c in contexts(&w) {
                println!("{c}");
            }
        }
        "version" => {
            let client = json!({"Version": "29.6.2-fake", "ApiVersion": "1.52", "Os": std::env::consts::OS, "Arch": "amd64", "Context": context});
            if down || unreachable {
                println!("{}", json!({"Client": client, "Server": null}));
                fail(if unreachable { UNREACHABLE } else { down_msg });
            }
            println!(
                "{}",
                json!({"Client": client, "Server": {"Platform": {"Name": "Docker Engine - Fake"}, "Version": "29.6.2-fake", "ApiVersion": "1.52", "Os": "linux", "Arch": "amd64", "KernelVersion": "6.18.0-fake"}})
            );
        }
        _ if down || unreachable => fail(if unreachable { UNREACHABLE } else { down_msg }),
        "ps" => {
            for c in &all {
                println!("{}", ps_row(c));
            }
        }
        "inspect" => {
            let ids: Vec<&String> = rest
                .iter()
                .skip(1)
                .filter(|a| !a.starts_with('-') && *a != "container")
                .collect();
            let found: Vec<Value> = all
                .iter()
                .filter(|c| ids.iter().any(|id| **id == id_of(c.name) || **id == c.name))
                .map(|c| inspect_json(c, &images))
                .collect();
            out(&Value::Array(found.clone()));
            if found.len() < ids.len() {
                fail("Error: No such container: (fake)");
            }
        }
        "image" if rest.get(1).map(String::as_str) == Some("inspect") => {
            let ids: Vec<&String> = rest.iter().skip(2).collect();
            let found: Vec<Value> = images
                .iter()
                .filter(|i| ids.iter().any(|id| **id == i.id))
                .map(image_json)
                .collect();
            out(&Value::Array(found));
        }
        "compose" => {
            if w.scenario == "no-compose" {
                fail(NO_COMPOSE);
            }
            compose(&w, &rest[1..], &all);
        }
        "stats" => {
            let t = now();
            for c in all.iter().filter(|c| c.state == "running") {
                let wanted: Vec<&String> = rest
                    .iter()
                    .skip(1)
                    .filter(|a| !a.starts_with('-') && *a != "json")
                    .collect();
                if !wanted.is_empty() && !wanted.iter().any(|id| **id == id_of(c.name)) {
                    continue;
                }
                let wobble = ((t % 7) as f64) * 1.7 + c.name.len() as f64;
                println!(
                    "{}",
                    json!({
                        "BlockIO": "12.3MB / 4.1MB",
                        "CPUPerc": format!("{:.2}%", wobble),
                        "Container": &id_of(c.name)[..12],
                        "ID": id_of(c.name),
                        "MemPerc": format!("{:.2}%", 2.0 + wobble / 10.0),
                        "MemUsage": format!("{:.1}MiB / 7.62GiB", 40.0 + wobble * 3.0),
                        "Name": c.name,
                        "NetIO": format!("{:.1}kB / {:.1}kB", 120.0 + wobble, 88.0 + wobble),
                        "PIDs": (3 + c.name.len() % 9).to_string(),
                    })
                );
            }
        }
        "events" => events(&all),
        "logs" => logs(&rest, None),
        "exec" => shell(&rest, &all),
        "start" | "stop" | "restart" | "pause" | "unpause" | "kill" | "rm" => {
            std::thread::sleep(Duration::from_millis(400));
            for a in rest.iter().skip(1).filter(|a| !a.starts_with('-')) {
                let name = all
                    .iter()
                    .find(|c| id_of(c.name) == *a)
                    .map(|c| c.name.to_owned())
                    .unwrap_or_else(|| a.clone());
                println!("{name}");
            }
        }
        "build" => build(&rest),
        _ => fail(&format!(
            "fake docker: unknown command: docker {}",
            rest.join(" ")
        )),
    }
}

fn compose(w: &World, rest: &[String], all: &[Ctr]) {
    let value_flags = [
        "-f",
        "--file",
        "-p",
        "--project-name",
        "--project-directory",
        "--env-file",
        "--profile",
        "--ansi",
        "--progress",
    ];
    let mut i = 0;
    while i < rest.len() && rest[i].starts_with('-') {
        i += if value_flags.contains(&rest[i].as_str()) {
            2
        } else {
            1
        };
    }
    let verb = rest.get(i).map(String::as_str).unwrap_or("");
    let files = values(rest, &["-f", "--file"]);
    let project = values(rest, &["-p", "--project-name"])
        .pop()
        .unwrap_or_else(|| {
            let f = files
                .first()
                .cloned()
                .unwrap_or_default()
                .replace('\\', "/");
            if f.contains("/ops/") {
                "devtools".into()
            } else if f.contains("/extras/") {
                "extras".into()
            } else {
                "shop".into()
            }
        });
    let services: Vec<&str> = all
        .iter()
        .filter_map(|c| c.project.as_ref())
        .filter(|(p, ..)| *p == project)
        .map(|(_, s, ..)| *s)
        .collect();
    let say = |what: &str| -> Vec<String> {
        services
            .iter()
            .map(|s| format!(" Container {project}-{s}-1  {what}"))
            .collect()
    };
    match verb {
        "ls" => {
            if w.scenario == "empty" {
                println!("[]");
                return;
            }
            let list = json!([
                {"Name": "blog", "Status": "running(1)", "ConfigFiles": w.elsewhere("blog/compose.yaml")},
                {"Name": "devtools", "Status": "paused(1), running(1)", "ConfigFiles": w.path("ops/compose.yaml")},
                {"Name": "shop", "Status": "exited(1), restarting(1), running(2)", "ConfigFiles": w.path("compose.yaml")},
            ]);
            println!("{list}");
        }
        "config" => {
            let file = files
                .first()
                .cloned()
                .unwrap_or_else(|| w.path("compose.yaml"));
            out(&compose_config(&file));
        }
        "up" => {
            let mut lines = vec![format!("[+] Running {}/{}", services.len(), services.len())];
            if rest.iter().any(|a| a == "--build") {
                lines.insert(0, format!("[+] Building 3.2s ({project})"));
            }
            lines.extend(say("Started"));
            slow_lines(&lines, 250);
        }
        "down" => {
            let mut lines = say("Stopped");
            lines.extend(say("Removed"));
            lines.push(format!(" Network {project}_default  Removed"));
            if rest.iter().any(|a| a == "--volumes" || a == "-v") {
                lines.push(format!(" Volume {project}_pgdata  Removed"));
            }
            slow_lines(&lines, 200);
        }
        "start" => slow_lines(&say("Started"), 200),
        "stop" => slow_lines(&say("Stopped"), 200),
        "restart" => slow_lines(&say("Restarted"), 200),
        "pull" => slow_lines(&[" db Pulled".to_owned(), " cache Pulled".to_owned()], 300),
        "build" => slow_lines(
            &[
                "[+] Building 4.1s (12/12) FINISHED".to_owned(),
                " => [web internal] load build definition from Dockerfile".to_owned(),
                " => [web 4/4] RUN npm run build".to_owned(),
                " => [web] exporting to image".to_owned(),
            ],
            300,
        ),
        "logs" => logs(rest, Some(&services)),
        other => fail(&format!("fake docker compose: unknown command {other}")),
    }
}

fn events(all: &[Ctr]) {
    let mut n = 0u64;
    let mut o = std::io::stdout();
    if all.is_empty() {
        // Nothing runs: nothing happens, until killed.
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    let ends = env("FAKE_DOCKER_EVENTS").as_deref() == Some("end");
    loop {
        std::thread::sleep(Duration::from_secs(2));
        if ends && n == 1 {
            fail("error during connect: the events stream was closed by the engine (fake)");
        }
        let (name, action, extra) = match n % 3 {
            0 => ("shop-worker-1", "die", json!({"exitCode": "137"})),
            1 => ("shop-worker-1", "start", json!({})),
            _ => ("shop-db-1", "health_status: unhealthy", json!({})),
        };
        let c = all.iter().find(|c| c.name == name);
        let mut attrs =
            json!({"name": name, "image": c.map(|c| c.image.clone()).unwrap_or_default()});
        if let (Some(m), Some(x)) = (attrs.as_object_mut(), extra.as_object()) {
            for (k, v) in x {
                m.insert(k.clone(), v.clone());
            }
        }
        let ev = json!({
            "Type": "container",
            "Action": action,
            "Actor": {"ID": id_of(name), "Attributes": attrs},
            "scope": "local",
            "time": now(),
            "timeNano": now() * 1_000_000_000,
        });
        if writeln!(o, "{ev}").is_err() || o.flush().is_err() {
            return;
        }
        n += 1;
    }
}

fn logs(rest: &[String], services: Option<&[&str]>) {
    let follow = rest.iter().any(|a| a == "-f" || a == "--follow");
    let tail: usize = values(rest, &["--tail", "-n"])
        .pop()
        .and_then(|t| t.parse().ok())
        .unwrap_or(100)
        .min(40);
    let stamps = rest.iter().any(|a| a == "--timestamps" || a == "-t");
    let prefix = |i: u64| -> String {
        let svc = services
            .map(|s| {
                format!(
                    "{}-1  | ",
                    s.get(i as usize % s.len().max(1)).copied().unwrap_or("app")
                )
            })
            .unwrap_or_default();
        let ts = if stamps {
            format!("{} ", rfc3339(now()))
        } else {
            String::new()
        };
        format!("{svc}{ts}")
    };
    let messages = [
        "GET /healthz 200 0.4ms",
        "GET /api/orders?page=2 200 12.1ms",
        "POST /api/cart 201 31.7ms",
        "worker: picked job 4182 (send-receipt)",
        "WARN slow query: 812ms SELECT * FROM orders WHERE …",
        "GET /static/app.js 304 0.2ms",
    ];
    let mut o = std::io::stdout();
    for i in 0..tail as u64 {
        let _ = writeln!(o, "{}{}", prefix(i), messages[i as usize % messages.len()]);
    }
    let _ = o.flush();
    if !follow {
        return;
    }
    let mut i = tail as u64;
    loop {
        std::thread::sleep(Duration::from_secs(1));
        if writeln!(o, "{}{}", prefix(i), messages[i as usize % messages.len()]).is_err()
            || o.flush().is_err()
        {
            return;
        }
        i += 1;
    }
}

fn shell(rest: &[String], all: &[Ctr]) {
    let id = rest
        .iter()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .cloned()
        .unwrap_or_default();
    let name = all
        .iter()
        .find(|c| id_of(c.name) == id || c.name == id)
        .map(|c| c.name)
        .unwrap_or("container");
    let mut o = std::io::stdout();
    let _ = writeln!(
        o,
        "fake docker exec: a pretend shell in {name} (type exit to leave)"
    );
    let stdin = std::io::stdin();
    loop {
        let _ = write!(o, "root@{}:/app# ", &id[..id.len().min(12)]);
        let _ = o.flush();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let cmd = line.trim();
        match cmd {
            "" => {}
            "exit" | "logout" => return,
            "ls" => {
                let _ = writeln!(o, "Dockerfile  node_modules  package.json  server.js");
            }
            "whoami" => {
                let _ = writeln!(o, "root");
            }
            other => {
                let _ = writeln!(o, "fake shell: {other}");
            }
        }
    }
}

fn build(rest: &[String]) {
    let tag = values(rest, &["-t", "--tag"])
        .pop()
        .unwrap_or_else(|| "image:latest".into());
    let labels = values(rest, &["--label"]);
    let mut lines = vec![
        "#0 building with \"desktop-linux\" instance using docker driver".to_owned(),
        "#1 [internal] load build definition from Dockerfile".to_owned(),
        "#1 transferring dockerfile: 412B done".to_owned(),
        "#2 [internal] load metadata for docker.io/library/node:22-alpine".to_owned(),
        "#3 [1/4] FROM docker.io/library/node:22-alpine".to_owned(),
        "#4 [2/4] COPY package*.json ./".to_owned(),
        "#5 [3/4] RUN npm ci --omit=dev".to_owned(),
        "#6 [4/4] COPY . .".to_owned(),
        "#7 exporting to image".to_owned(),
        format!("#7 writing image sha256:{} done", id_of(&tag)),
    ];
    for l in &labels {
        lines.push(format!("#7 label {l}"));
    }
    lines.push(format!("#7 naming to docker.io/library/{tag} done"));
    slow_lines(&lines, 150);
}
