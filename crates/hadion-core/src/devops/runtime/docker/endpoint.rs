//! Where a docker command goes, and whether that is this machine.
//!
//! docker picks its endpoint in this order: `-H/--host`, then `--context`,
//! then `DOCKER_HOST`, then `DOCKER_CONTEXT`, then the config's
//! `currentContext`, then `default`. A named context's endpoint is in
//! `<config dir>/contexts/meta/<hex sha256 of the name>/meta.json`
//! (`Endpoints.docker.Host`); the config dir is `$DOCKER_CONFIG` (or
//! `--config`), else `~/.docker`. The `default` context is `DOCKER_HOST`, or
//! the local socket / pipe.
//!
//! Remote means `tcp://`, `ssh://` or `http(s)://` (anything but `npipe://`,
//! `unix://` and `fd://`). A named context whose meta file cannot be read is
//! treated as remote: safe mode would rather refuse than guess, and docker
//! itself fails on a context it cannot find.
//!
//! Every function takes the environment as a [`DockerEnv`], so tests never
//! set process-wide variables.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The local endpoint of the `default` context without `DOCKER_HOST`.
pub fn local_default() -> &'static str {
    if cfg!(windows) {
        "npipe:////./pipe/docker_engine"
    } else {
        "unix:///var/run/docker.sock"
    }
}

/// What docker reads from the process environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DockerEnv {
    /// `$DOCKER_CONFIG`, else `~/.docker`.
    pub config_dir: Option<PathBuf>,
    pub docker_host: Option<String>,
    pub docker_context: Option<String>,
}

fn var(k: &str) -> Option<String> {
    std::env::var(k)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

impl DockerEnv {
    /// This process's environment.
    pub fn from_process() -> Self {
        let home = var("USERPROFILE")
            .or_else(|| var("HOME"))
            .map(PathBuf::from);
        Self {
            config_dir: var("DOCKER_CONFIG")
                .map(PathBuf::from)
                .or_else(|| home.map(|h| h.join(".docker"))),
            docker_host: var("DOCKER_HOST"),
            docker_context: var("DOCKER_CONTEXT"),
        }
    }
}

/// `tcp://`, `ssh://`, `http(s)://` and anything else that is not a local
/// socket or pipe.
pub fn is_remote(endpoint: &str) -> bool {
    let e = endpoint.trim().to_ascii_lowercase();
    !e.is_empty()
        && !(e.starts_with("npipe://") || e.starts_with("unix://") || e.starts_with("fd://"))
}

/// The host to open published ports on: `localhost` for a local endpoint,
/// the remote host otherwise (`ssh://deploy@staging:22` → `staging`).
pub fn port_host(endpoint: &str) -> String {
    if !is_remote(endpoint) {
        return "localhost".into();
    }
    let rest = endpoint
        .split_once("://")
        .map_or(endpoint, |(_, r)| r)
        .trim_end_matches('/');
    let authority = rest.split('/').next().unwrap_or(rest);
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if host.starts_with('[') {
        host.split_once(']')
            .map_or(host, |(h, _)| h)
            .trim_start_matches('[')
    } else {
        host.split(':').next().unwrap_or(host)
    };
    if host.is_empty() {
        "localhost".into()
    } else {
        host.to_owned()
    }
}

/// `<config dir>/contexts/meta/<sha256(name)>/meta.json`.
pub fn meta_path(config_dir: &Path, name: &str) -> PathBuf {
    let digest = Sha256::digest(name.as_bytes());
    config_dir
        .join("contexts")
        .join("meta")
        .join(crate::devops::packaging::checksum::hex(&digest))
        .join("meta.json")
}

/// A named context's endpoint (`Endpoints.docker.Host` of its meta file).
pub fn context_endpoint(config_dir: &Path, name: &str) -> Option<String> {
    let bytes = std::fs::read(meta_path(config_dir, name)).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("Endpoints")?
        .get("docker")?
        .get("Host")?
        .as_str()
        .map(str::to_owned)
}

/// `currentContext` of `<config dir>/config.json`.
pub fn current_context(config_dir: &Path) -> Option<String> {
    let bytes = std::fs::read(config_dir.join("config.json")).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("currentContext")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Where a command goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// The endpoint, or `None` when a named context could not be read.
    pub endpoint: Option<String>,
    /// How it was chosen: `context staging`, `-H`, `DOCKER_HOST`.
    pub via: String,
}

impl Resolved {
    /// Remote, or unknown (treated as remote).
    pub fn remote(&self) -> bool {
        self.endpoint.as_deref().is_none_or(is_remote)
    }

    /// `ssh://deploy@staging (context staging)`.
    pub fn describe(&self) -> String {
        match &self.endpoint {
            Some(e) => format!("{e} ({})", self.via),
            None => format!("an endpoint Hadion could not read ({})", self.via),
        }
    }
}

/// The endpoint docker would use for these global flags and environment.
pub fn resolve(
    context: Option<&str>,
    host: Option<&str>,
    config_flag: Option<&str>,
    env: &DockerEnv,
) -> Resolved {
    if let Some(h) = host.map(str::trim).filter(|h| !h.is_empty()) {
        return Resolved {
            endpoint: Some(h.to_owned()),
            via: "-H".into(),
        };
    }
    let config_dir = config_flag
        .map(PathBuf::from)
        .or_else(|| env.config_dir.clone());
    let named = |name: &str, via: String| -> Resolved {
        if name == "default" {
            return Resolved {
                endpoint: Some(
                    env.docker_host
                        .clone()
                        .unwrap_or_else(|| local_default().to_owned()),
                ),
                via,
            };
        }
        Resolved {
            endpoint: config_dir
                .as_deref()
                .and_then(|d| context_endpoint(d, name)),
            via,
        }
    };
    if let Some(c) = context.map(str::trim).filter(|c| !c.is_empty()) {
        return named(c, format!("context {c}"));
    }
    if let Some(h) = &env.docker_host {
        return Resolved {
            endpoint: Some(h.clone()),
            via: "DOCKER_HOST".into(),
        };
    }
    if let Some(c) = &env.docker_context {
        return named(c, format!("DOCKER_CONTEXT={c}"));
    }
    if let Some(c) = config_dir.as_deref().and_then(current_context) {
        return named(&c, format!("current context {c}"));
    }
    named("default", "context default".into())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A scratch docker config folder with contexts (no process env).
    pub(crate) fn scratch_config(
        tag: &str,
        contexts: &[(&str, &str)],
        current: Option<&str>,
    ) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hadion-docker-cfg-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for (name, host) in contexts {
            let p = meta_path(&dir, name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(
                &p,
                format!(
                    r#"{{"Name":"{name}","Metadata":{{}},"Endpoints":{{"docker":{{"Host":"{host}","SkipTLSVerify":false}}}}}}"#
                ),
            )
            .unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(c) = current {
            std::fs::write(
                dir.join("config.json"),
                format!(r#"{{"currentContext":"{c}"}}"#),
            )
            .unwrap();
        }
        dir
    }

    #[test]
    fn meta_paths_hash_the_name() {
        let p = meta_path(Path::new("cfg"), "desktop-linux");
        assert!(
            p.to_string_lossy()
                .replace('\\', "/")
                .ends_with("contexts/meta/fe9c6bd7a66301f49ca9b6a70b217107cd1284598bfc254700c989b916da791e/meta.json"),
            "{}",
            p.display()
        );
    }

    #[test]
    fn local_and_remote_endpoints() {
        assert!(!is_remote("npipe:////./pipe/dockerDesktopLinuxEngine"));
        assert!(!is_remote("unix:///var/run/docker.sock"));
        assert!(is_remote("tcp://10.0.0.5:2376"));
        assert!(is_remote("ssh://deploy@staging"));
        assert!(is_remote("https://docker.example.com"));
        assert!(!is_remote(""));
        assert_eq!(port_host("npipe:////./pipe/docker_engine"), "localhost");
        assert_eq!(
            port_host("ssh://deploy@staging.example.com:22"),
            "staging.example.com"
        );
        assert_eq!(port_host("tcp://10.0.0.5:2376"), "10.0.0.5");
        assert_eq!(port_host("tcp://[fd00::5]:2376"), "fd00::5");
    }

    #[test]
    fn resolution_order() {
        let dir = scratch_config(
            "order",
            &[
                ("desktop-linux", "npipe:////./pipe/dockerDesktopLinuxEngine"),
                ("staging", "ssh://deploy@staging.example.com"),
            ],
            Some("desktop-linux"),
        );
        let env = DockerEnv {
            config_dir: Some(dir.clone()),
            ..DockerEnv::default()
        };
        let r = resolve(Some("staging"), None, None, &env);
        assert_eq!(
            r.endpoint.as_deref(),
            Some("ssh://deploy@staging.example.com")
        );
        assert!(r.remote());
        let r = resolve(Some("desktop-linux"), None, None, &env);
        assert!(!r.remote());
        // No flag: the config's current context.
        let r = resolve(None, None, None, &env);
        assert_eq!(r.via, "current context desktop-linux");
        assert!(!r.remote());
        // -H wins over everything.
        assert!(
            resolve(
                Some("desktop-linux"),
                Some("tcp://1.2.3.4:2375"),
                None,
                &env
            )
            .remote()
        );
        // DOCKER_HOST without --context.
        let with_host = DockerEnv {
            docker_host: Some("tcp://build-box:2376".into()),
            ..env.clone()
        };
        assert!(resolve(None, None, None, &with_host).remote());
        // `default` is DOCKER_HOST, or local.
        assert!(resolve(Some("default"), None, None, &with_host).remote());
        assert!(!resolve(Some("default"), None, None, &env).remote());
        // DOCKER_CONTEXT.
        let with_ctx = DockerEnv {
            docker_context: Some("staging".into()),
            ..env.clone()
        };
        assert!(resolve(None, None, None, &with_ctx).remote());
        // A context without a meta file counts as remote.
        let r = resolve(Some("nowhere"), None, None, &env);
        assert!(r.endpoint.is_none() && r.remote());
        // --config points elsewhere.
        let other = scratch_config("other", &[("staging", "unix:///run/docker.sock")], None);
        assert!(!resolve(Some("staging"), None, Some(other.to_str().unwrap()), &env).remote());
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(other);
    }
}
