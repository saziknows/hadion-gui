//! docker errors as plain states with the exact fix.

/// What went wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// No `docker` on PATH.
    NotInstalled,
    /// The local engine is not running (Docker Desktop closed, the service
    /// stopped).
    DaemonDown,
    /// The user may not open the socket (`/var/run/docker.sock`).
    PermissionDenied,
    /// The context does not exist (any more).
    UnknownContext,
    /// The Compose plugin is missing.
    NoCompose,
    /// A remote context's endpoint does not answer.
    Unreachable,
    Timeout,
    /// Safe mode refused a remote write.
    SafeMode,
    Other,
}

/// Classify an error message; `remote` says whether the context's endpoint
/// is on another machine (the same "cannot connect" means "start Docker"
/// locally and "check the network" remotely).
pub fn classify(err: &str, remote: bool) -> Problem {
    let l = err.to_ascii_lowercase();
    if l.contains("safe mode is on") {
        Problem::SafeMode
    } else if l.contains("is not installed (not found on path)") {
        Problem::NotInstalled
    } else if l.contains("'compose' is not a docker command")
        || l.contains("unknown command: docker compose")
        || l.contains("unknown shorthand flag: 'f' in -f")
        || l.contains("unknown shorthand flag: 'p' in -p")
        || l.contains("unknown flag: --project-directory")
    {
        Problem::NoCompose
    } else if l.contains("context not found")
        || (l.contains("context \"") && l.contains("does not exist"))
        || l.contains("unable to resolve docker endpoint")
    {
        Problem::UnknownContext
    } else if l.contains("permission denied")
        && (l.contains("docker.sock") || l.contains("docker daemon socket"))
    {
        Problem::PermissionDenied
    } else if l.contains("did not finish within")
        || l.contains("context deadline exceeded")
        || l.contains("i/o timeout")
    {
        if remote {
            Problem::Unreachable
        } else {
            Problem::Timeout
        }
    } else if l.contains("cannot connect to the docker daemon")
        || l.contains("failed to connect to the docker api")
        || l.contains("error during connect")
        || l.contains("the system cannot find the file specified")
        || l.contains("docker desktop is not running")
        || l.contains("is the docker daemon running")
    {
        if remote {
            Problem::Unreachable
        } else {
            Problem::DaemonDown
        }
    } else if remote
        && (l.contains("connection refused")
            || l.contains("no route to host")
            || l.contains("could not resolve hostname")
            || l.contains("ssh:")
            || l.contains("exit status 255")
            || l.contains("dial tcp"))
    {
        Problem::Unreachable
    } else {
        Problem::Other
    }
}

impl Problem {
    pub fn title(self) -> &'static str {
        match self {
            Problem::NotInstalled => "docker is not installed",
            Problem::DaemonDown => "Docker is not running",
            Problem::PermissionDenied => "Not allowed to talk to Docker",
            Problem::UnknownContext => "That docker context does not exist",
            Problem::NoCompose => "Docker Compose is missing",
            Problem::Unreachable => "The remote Docker does not answer",
            Problem::Timeout => "Docker did not answer in time",
            Problem::SafeMode => "Safe mode refused it",
            Problem::Other => "docker failed",
        }
    }

    /// A word for tables and the header.
    pub fn short(self) -> &'static str {
        match self {
            Problem::NotInstalled => "not installed",
            Problem::DaemonDown => "engine not running",
            Problem::PermissionDenied => "permission denied",
            Problem::UnknownContext => "no such context",
            Problem::NoCompose => "compose missing",
            Problem::Unreachable => "unreachable",
            Problem::Timeout => "timed out",
            Problem::SafeMode => "safe mode",
            Problem::Other => "failed",
        }
    }

    /// Nothing else can work until it is fixed.
    pub fn fatal(self) -> bool {
        matches!(
            self,
            Problem::NotInstalled
                | Problem::DaemonDown
                | Problem::PermissionDenied
                | Problem::UnknownContext
                | Problem::Unreachable
                | Problem::Timeout
        )
    }
}

/// What to do: advice, and a command the user can copy (Hadion never runs
/// it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fix {
    pub text: String,
    pub command: Option<String>,
}

pub fn fix(p: Problem, context: &str, endpoint: &str) -> Fix {
    let windows = cfg!(windows);
    let mac = cfg!(target_os = "macos");
    match p {
        Problem::NotInstalled => Fix {
            text: format!(
                "{} Then press Check again.",
                crate::devops::tools::Tool::Docker.install_hint()
            ),
            command: None,
        },
        Problem::DaemonDown if windows || mac => Fix {
            text: format!(
                "Start Docker Desktop: it runs the engine that {endpoint} points to. The first start takes a minute; the list fills in as soon as it answers (Refresh)."
            ),
            command: Some("docker desktop start".into()),
        },
        Problem::DaemonDown => Fix {
            text: format!(
                "Start the docker service (it listens on {endpoint}), or Docker Desktop if you use it."
            ),
            command: Some("sudo systemctl start docker".into()),
        },
        Problem::PermissionDenied => Fix {
            text: "Your user may not open the docker socket. Add it to the docker group, then log out and in again (or run `newgrp docker` in a terminal).".into(),
            command: Some("sudo usermod -aG docker $USER".into()),
        },
        Problem::UnknownContext => Fix {
            text: format!(
                "The context {context} is not in your docker configuration (any more). Pick another one above; this lists them:"
            ),
            command: Some("docker context ls".into()),
        },
        Problem::NoCompose => Fix {
            text: if windows || mac {
                "The Compose plugin ships with Docker Desktop: update or repair Docker Desktop. The containers still show; compose actions need the plugin.".into()
            } else {
                "Install the Compose plugin. The containers still show; compose actions need the plugin.".into()
            },
            command: (!windows && !mac).then(|| "sudo apt-get install docker-compose-plugin".into()),
        },
        Problem::Unreachable | Problem::Timeout => Fix {
            text: if super::endpoint::is_remote(endpoint) {
                format!(
                    "{endpoint} does not answer. Check the network or VPN, that the host runs docker, and for ssh:// that `ssh` reaches it without a password prompt. This checks it:"
                )
            } else {
                "The engine is slow or stuck. Restart Docker Desktop (or the docker service), then Refresh.".into()
            },
            command: Some(format!("docker --context {context} version")),
        },
        Problem::SafeMode => Fix {
            text: "Turn safe mode off in Settings → Safety to change containers on a remote Docker.".into(),
            command: None,
        },
        Problem::Other => Fix::default(),
    }
}

/// docker's stderr without its noise (`See 'docker --help'.`, blank lines,
/// repeats).
pub fn clean_error(stderr: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for l in stderr.lines().map(str::trim) {
        if l.is_empty() || l.starts_with("See 'docker") || l.starts_with("Run 'docker") {
            continue;
        }
        if !out.contains(&l) {
            out.push(l);
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_become_problems() {
        let win_down = "error during connect: Get \"http://%2F%2F.%2Fpipe%2FdockerDesktopLinuxEngine/v1.51/version\": open //./pipe/dockerDesktopLinuxEngine: The system cannot find the file specified.";
        assert_eq!(classify(win_down, false), Problem::DaemonDown);
        let new_down = "failed to connect to the docker API at npipe:////./pipe/dockerDesktopLinuxEngine; check if the path is correct and if the daemon is running: open //./pipe/dockerDesktopLinuxEngine: The system cannot find the file specified.";
        assert_eq!(classify(new_down, false), Problem::DaemonDown);
        let linux = "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?";
        assert_eq!(classify(linux, false), Problem::DaemonDown);
        assert_eq!(classify(linux, true), Problem::Unreachable);
        let perm = "permission denied while trying to connect to the Docker daemon socket at unix:///var/run/docker.sock: Get \"http://%2Fvar%2Frun%2Fdocker.sock/v1.24/version\": dial unix /var/run/docker.sock: connect: permission denied";
        assert_eq!(classify(perm, false), Problem::PermissionDenied);
        let ctx = "Failed to initialize: unable to resolve docker endpoint: context \"gone\": context not found: open C:\\Users\\x\\.docker\\contexts\\meta\\abc\\meta.json: The system cannot find the path specified.";
        assert_eq!(classify(ctx, false), Problem::UnknownContext);
        assert_eq!(
            classify(
                "docker: 'compose' is not a docker command.\nSee 'docker --help'",
                false
            ),
            Problem::NoCompose
        );
        assert_eq!(
            classify("unknown shorthand flag: 'p' in -p", false),
            Problem::NoCompose
        );
        let ssh = "error during connect: Get \"http://docker.example.com/v1.24/version\": command [ssh -o ConnectTimeout=30 -T -- staging docker system dial-stdio] has exited with exit status 255, make sure the URL is valid: stderr=ssh: connect to host staging port 22: Connection timed out";
        assert_eq!(classify(ssh, true), Problem::Unreachable);
        assert_eq!(
            classify("docker is not installed (not found on PATH)", false),
            Problem::NotInstalled
        );
        assert_eq!(
            classify(
                "Safe mode is on: docker stop on ssh://x was not sent.",
                true
            ),
            Problem::SafeMode
        );
        assert_eq!(
            classify("Error: No such container: abc", false),
            Problem::Other
        );
        assert!(Problem::DaemonDown.fatal() && !Problem::NoCompose.fatal());
    }

    #[test]
    fn fixes_name_the_command() {
        let f = fix(Problem::UnknownContext, "gone", "");
        assert_eq!(f.command.as_deref(), Some("docker context ls"));
        assert!(f.text.contains("gone"));
        let f = fix(Problem::Unreachable, "staging", "ssh://deploy@staging");
        assert_eq!(
            f.command.as_deref(),
            Some("docker --context staging version")
        );
        assert!(f.text.contains("ssh://deploy@staging"));
        assert!(
            fix(Problem::DaemonDown, "desktop-linux", "npipe:////./pipe/x")
                .command
                .is_some()
        );
        assert_eq!(
            clean_error("docker: 'compose' is not a docker command.\nSee 'docker --help'\n\n"),
            "docker: 'compose' is not a docker command."
        );
    }
}
