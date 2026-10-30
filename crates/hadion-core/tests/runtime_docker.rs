//! The Containers tab's loaders against `examples/fake_docker.rs`: contexts,
//! the listing, scope, `compose config`, stats, the masked inspect, the
//! events stream and its watchdog, a write, error states, and safe mode
//! for a remote context. No real docker is touched.
//!
//! The fake is built with the examples (`cargo test -p hadion-core` builds
//! them; for this file alone: `cargo build -p hadion-core --example
//! fake_docker` first). One test function: it sets process environment
//! (`HADION_DOCKER`, `DOCKER_CONFIG`, the fake's variables), which the other
//! test binaries do not share.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use hadion_core::Oid;
use hadion_core::devops::proc::{ProcSpec, run_capture};
use hadion_core::devops::runtime::docker::commands::{self, ComposeTarget, ContainerOp};
use hadion_core::devops::runtime::docker::events::{self, Ended, Follow, StopReason};
use hadion_core::devops::runtime::docker::scope::{self, RepoScope};
use hadion_core::devops::runtime::docker::triage::{self, Problem};
use hadion_core::devops::runtime::docker::{self, endpoint};
use hadion_core::devops::runtime::matching::RefIndex;

fn fake() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let debug = exe.parent().and_then(Path::parent).unwrap();
    let name = if cfg!(windows) {
        "fake_docker.exe"
    } else {
        "fake_docker"
    };
    let p = debug.join("examples").join(name);
    assert!(
        p.is_file(),
        "{} is missing: run `cargo build -p hadion-core --example fake_docker` first",
        p.display()
    );
    p
}

/// The temp folder by its real name (CI runners use an 8.3 short one).
fn temp_root() -> PathBuf {
    let t = std::env::temp_dir();
    let real = t.canonicalize().unwrap_or(t);
    match real.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(s) if !s.starts_with("UNC") => PathBuf::from(s),
        _ => real,
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn set(k: &str, v: impl AsRef<std::ffi::OsStr>) {
    // SAFETY: this binary's only test sets these before it starts any
    // process that reads them, and no other thread runs meanwhile.
    unsafe { std::env::set_var(k, v) }
}

#[test]
fn the_containers_tab_against_the_fake_docker() {
    let exe = fake();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = temp_root().join(format!("hadion-docker-{}-{nanos}", std::process::id()));
    let repo = root.join("shop");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    for (k, v) in [
        ("user.name", "Test"),
        ("user.email", "test@example.com"),
        ("commit.gpgsign", "false"),
        ("core.autocrlf", "false"),
    ] {
        git(&repo, &["config", k, v]);
    }
    write(
        &repo,
        "compose.yaml",
        "services:\n  web:\n    build: ./web\n",
    );
    write(
        &repo,
        "ops/compose.yaml",
        "services:\n  cache:\n    image: redis:7\n",
    );
    write(
        &repo,
        "extras/docker-compose.yml",
        "services:\n  grafana:\n    image: grafana/grafana\n",
    );
    write(&repo, "web/Dockerfile", "FROM nginx:1.27-alpine\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "compose projects"]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://git.example.com/acme/shop.git",
        ],
    );
    let head = git(&repo, &["rev-parse", "HEAD"]);

    // A docker configuration of its own: a local and a remote context.
    let cfg = root.join("docker-config");
    let local = if cfg!(windows) {
        "npipe:////./pipe/dockerDesktopLinuxEngine"
    } else {
        "unix:///var/run/docker.sock"
    };
    for (name, host) in [
        ("desktop-linux", local),
        ("staging", "ssh://deploy@staging.example.com"),
    ] {
        let p = endpoint::meta_path(&cfg, name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            p,
            format!(r#"{{"Name":"{name}","Endpoints":{{"docker":{{"Host":"{host}"}}}}}}"#),
        )
        .unwrap();
    }
    let log = root.join("fake.log");
    set("HADION_DOCKER", &exe);
    set("DOCKER_CONFIG", &cfg);
    set("FAKE_DOCKER_REPO", &repo);
    set("FAKE_DOCKER_REVISION", &head);
    set("FAKE_DOCKER_SOURCE", "https://git.example.com/acme/shop");
    set("FAKE_DOCKER_LOG", &log);
    set("FAKE_DOCKER_SCENARIO", "normal");

    // Everything the tab loads when it opens.
    let mut cmds = Vec::new();
    let full = docker::load_full(&repo, None, &mut cmds);
    assert!(
        !format!("{full:?}").contains("example-only-value"),
        "an environment value reached the model"
    );
    let contexts = full.contexts.clone().unwrap();
    assert_eq!(contexts.len(), 2);
    assert_eq!(full.context, "desktop-linux", "the context marked current");
    assert!(full.tool.as_deref().unwrap().contains("29.6.2-fake"));
    let listing = full.listing.clone().unwrap();
    assert!(listing.fatal().is_none(), "{:?}", listing.version);
    assert_eq!(listing.containers().len(), 9);
    assert_eq!(listing.projects().len(), 3);
    assert!(!listing.images.is_empty());
    assert!(
        listing
            .containers()
            .iter()
            .all(|c| !c.status_text.is_empty())
    );
    assert_eq!(full.files.compose.len(), 3);
    assert_eq!(full.files.dockerfiles, ["web/Dockerfile"]);
    assert!(
        cmds.iter()
            .any(|c| c.contains("--context desktop-linux inspect --type container"))
    );

    // What belongs to the repository.
    let scope = RepoScope::new(&repo, vec!["https://git.example.com/acme/shop.git".into()]);
    let head_oid = Oid::from_str(&head).unwrap();
    let idx = RefIndex::new([head_oid], vec![], vec![], vec![], Some(head_oid), None);
    let s = scope::classify(
        listing.containers(),
        &listing.images,
        listing.projects(),
        &full.files,
        &scope,
        Some(&idx),
        &scope::real_key,
    );
    let names: Vec<&str> = s.projects.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["devtools", "shop"]);
    assert_eq!(s.other_projects.len(), 1, "blog lives elsewhere");
    let mine: Vec<&str> = s
        .standalone
        .iter()
        .map(|(i, _)| listing.containers()[*i].name.as_str())
        .collect();
    assert_eq!(mine, ["api-preview"]);
    assert_eq!(s.not_started.len(), 1);
    assert_eq!(
        full.files.compose[s.not_started[0]].rel,
        "extras/docker-compose.yml"
    );
    let web = listing
        .containers()
        .iter()
        .find(|c| c.name == "shop-web-1")
        .unwrap();
    let rev = hadion_core::devops::runtime::docker::revision::revision(
        Some(&idx),
        web,
        listing.image(&web.image_id),
        &scope.remotes,
    );
    assert_eq!(rev.commit(), Some(head_oid));
    assert!(rev.source_matches);

    // compose config of a project: names only.
    let shop = s.projects.iter().find(|p| p.name == "shop").unwrap();
    let target = ComposeTarget {
        project: Some("shop".into()),
        files: shop.config_files.clone(),
        project_dir: Some(shop.working_dir.clone()),
    };
    let cfg_shop = docker::load_config(&repo, "desktop-linux", &target, &mut cmds).unwrap();
    assert!(!format!("{cfg_shop:?}").contains("example-only-value"));
    assert!(cfg_shop.service("web").unwrap().build.is_some());
    assert_eq!(
        cfg_shop.service("db").unwrap().env_names,
        ["PGDATA", "POSTGRES_PASSWORD", "POSTGRES_USER"]
    );

    // Stats and the masked inspect.
    let stats = docker::load_stats(
        &repo,
        "desktop-linux",
        std::slice::from_ref(&web.id),
        &mut cmds,
    )
    .unwrap();
    assert_eq!(stats.len(), 1);
    assert!(stats[0].cpu.is_some());
    let db = listing
        .containers()
        .iter()
        .find(|c| c.name == "shop-db-1")
        .unwrap();
    let masked = docker::inspect_masked(&repo, "desktop-linux", &db.id, &mut cmds).unwrap();
    assert!(masked.contains("POSTGRES_PASSWORD=•••"));
    assert!(!masked.contains("example-only-value"));

    // The events stream: events arrive, and the watchdog stops it when
    // nobody draws the tab.
    let follow = Follow::with_hidden_after(|| {}, Duration::from_millis(4500));
    let spec = commands::spec(&repo, commands::events_args("desktop-linux"));
    let started = Instant::now();
    let ended = events::follow(&spec, &follow);
    assert_eq!(ended, Ended::Stopped(StopReason::Hidden));
    assert!(started.elapsed() < Duration::from_secs(15));
    assert!(follow.events() >= 1, "no event in 4.5 s");
    assert!(follow.pid().is_none());

    // A write goes through the same program.
    let out = docker::run(
        &repo,
        &ContainerOp::Stop.args("desktop-linux", &web.id),
        &mut cmds,
    )
    .unwrap();
    assert_eq!(String::from_utf8_lossy(&out).trim(), "shop-web-1");

    // Safe mode: a remote context's write is refused before it starts, a
    // local one runs.
    hadion_core::devops::safety::set_safe_mode(true);
    let remote = ProcSpec::new(exe.to_string_lossy().into_owned(), &repo)
        .args(ContainerOp::Stop.args("staging", &web.id));
    let e = run_capture(&remote, Some(Duration::from_secs(20)))
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("Safe mode is on") && e.contains("ssh://deploy@staging.example.com"),
        "{e}"
    );
    let local_stop = ProcSpec::new(exe.to_string_lossy().into_owned(), &repo)
        .args(ContainerOp::Stop.args("desktop-linux", &web.id));
    assert!(
        run_capture(&local_stop, Some(Duration::from_secs(20)))
            .unwrap()
            .success()
    );
    let read =
        ProcSpec::new(exe.to_string_lossy().into_owned(), &repo).args(commands::ps_args("staging"));
    assert!(
        run_capture(&read, Some(Duration::from_secs(20)))
            .unwrap()
            .success(),
        "reads stay allowed"
    );
    hadion_core::devops::safety::set_safe_mode(false);

    // The engine is down: the client still prints its version.
    set("FAKE_DOCKER_SCENARIO", "down");
    let down = docker::load_listing(&repo, "desktop-linux", &mut cmds);
    let e = down.fatal().unwrap().to_owned();
    assert_eq!(triage::classify(&e, false), Problem::DaemonDown);
    assert_eq!(down.client.as_deref(), Some("29.6.2-fake"));
    assert!(down.containers.is_none(), "nothing else was asked");

    // The Compose plugin is missing: containers still list.
    set("FAKE_DOCKER_SCENARIO", "no-compose");
    let nc = docker::load_listing(&repo, "desktop-linux", &mut cmds);
    assert_eq!(nc.containers().len(), 9);
    let e = match &nc.projects {
        Some(Err(e)) => e.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(triage::classify(&e, false), Problem::NoCompose);

    // A remote context: the current one is staging; prod does not answer.
    set("FAKE_DOCKER_SCENARIO", "remote");
    let r = docker::load_full(&repo, None, &mut cmds);
    assert_eq!(r.context, "staging");
    let staging = r
        .contexts
        .as_ref()
        .unwrap()
        .iter()
        .find(|c| c.name == "staging")
        .unwrap();
    assert!(staging.remote());
    let prod = docker::load_listing(&repo, "prod", &mut cmds);
    assert_eq!(
        triage::classify(prod.fatal().unwrap(), true),
        Problem::Unreachable
    );

    // An empty engine.
    set("FAKE_DOCKER_SCENARIO", "empty");
    let empty = docker::load_listing(&repo, "desktop-linux", &mut cmds);
    assert!(empty.fatal().is_none());
    assert!(empty.containers().is_empty() && empty.projects().is_empty());

    // Every command named its context, except the two local ones.
    let lines = std::fs::read_to_string(&log).unwrap();
    for l in lines.lines() {
        assert!(
            l.contains("--context")
                || l == "docker --version"
                || l == "docker context ls --format json",
            "{l}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}
