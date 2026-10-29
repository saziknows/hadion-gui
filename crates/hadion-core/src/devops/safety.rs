//! Safe mode: when on, every command that would change something outside
//! this machine is refused before it starts: GitHub API writes (non-GET
//! requests and GraphQL mutations), `gh` subcommands that write, `git push`,
//! `glab` / `az` writes, `kubectl` / `helm` changes, `terraform apply` and
//! friends, `docker push`, and docker commands that change containers,
//! images or compose projects on a remote endpoint (the context's endpoint
//! is `tcp://`, `ssh://` or `http(s)://`, see
//! [`super::runtime::docker::endpoint`]). Local container actions stay
//! allowed.
//!
//! Enforcement sits at the lowest level ([`crate::cli::run_raw`],
//! [`crate::cli::run_program`], [`super::proc::run_streaming`],
//! [`super::proc::run_capture`], [`crate::github::api_request`]), so no area
//! can bypass it. Arbitrary task scripts (`npm run deploy`) cannot be
//! classified and are not blocked; the settings page says so.

use std::sync::atomic::{AtomicBool, Ordering};

static SAFE_MODE: AtomicBool = AtomicBool::new(false);

/// Turn safe mode on or off for the whole process.
pub fn set_safe_mode(on: bool) {
    SAFE_MODE.store(on, Ordering::Relaxed);
}

pub fn safe_mode() -> bool {
    SAFE_MODE.load(Ordering::Relaxed)
}

/// The error message for a blocked command.
pub fn blocked_message(what: &str) -> String {
    format!(
        "Safe mode is on: {what} was not sent. Turn safe mode off in Settings → Safety to make changes outside this machine."
    )
}

/// `Err` with [`blocked_message`] when safe mode is on and the command
/// writes somewhere remote.
pub fn check(program: &str, args: &[&str], stdin: Option<&[u8]>) -> Result<(), String> {
    check_with(safe_mode(), program, args, stdin)
}

fn check_with(on: bool, program: &str, args: &[&str], stdin: Option<&[u8]>) -> Result<(), String> {
    if !on {
        return Ok(());
    }
    match remote_write(program, args, stdin) {
        Some(what) => Err(blocked_message(&what)),
        None => Ok(()),
    }
}

fn base_name(program: &str) -> String {
    let name = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".com"] {
        if let Some(stem) = name.strip_suffix(ext) {
            return stem.to_owned();
        }
    }
    name
}

/// Is a GraphQL request body a mutation (unreadable bodies count as one)?
pub fn graphql_is_mutation(body: &[u8]) -> bool {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return true;
    };
    let query = v.get("query").and_then(|q| q.as_str()).unwrap_or("");
    let first: String = query
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join(" ");
    let first = first.trim_start();
    first.starts_with("mutation") || first.starts_with("subscription")
}

/// The value after a flag (`--method POST`, `-X POST`, `--method=POST`).
fn flag_value<'a>(args: &[&'a str], names: &[&str]) -> Option<&'a str> {
    for (i, a) in args.iter().enumerate() {
        for n in names {
            if a == n {
                return args.get(i + 1).copied();
            }
            if let Some(v) = a.strip_prefix(&format!("{n}=")) {
                return Some(v);
            }
        }
    }
    None
}

/// The first argument that is not a flag (or a flag's value).
fn first_positional<'a>(args: &[&'a str], with_value: &[&str]) -> Option<&'a str> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        if with_value.contains(&a) {
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            i += 1;
            continue;
        }
        return Some(a);
    }
    None
}

const GH_API_FLAGS: [&str; 16] = [
    "--method",
    "-X",
    "--input",
    "-H",
    "--header",
    "-f",
    "--raw-field",
    "-F",
    "--field",
    "--jq",
    "-q",
    "--template",
    "-t",
    "--hostname",
    "--cache",
    "-p",
];

/// A short description of the remote change a command would make, or
/// `None` for reads and local work.
pub fn remote_write(program: &str, args: &[&str], stdin: Option<&[u8]>) -> Option<String> {
    let name = base_name(program);
    let sub: Vec<&str> = args
        .iter()
        .copied()
        .filter(|a| !a.starts_with('-'))
        .collect();
    let words = |n: usize| sub.iter().take(n).copied().collect::<Vec<_>>().join(" ");
    match name.as_str() {
        "git" => match (sub.first().copied(), sub.get(1).copied()) {
            (Some("push"), _) => Some(format!("git {}", args.join(" "))),
            (Some("lfs"), Some("push" | "lock" | "unlock")) => {
                Some(format!("git {}", args.join(" ")))
            }
            _ => None,
        },
        "gh" | "gh_guard" => match sub.first().copied() {
            Some("api") => {
                let rest = &args[1..];
                let method = flag_value(rest, &["--method", "-X"])
                    .unwrap_or("GET")
                    .to_ascii_uppercase();
                let endpoint = first_positional(rest, &GH_API_FLAGS).unwrap_or("");
                if endpoint == "graphql" {
                    stdin
                        .is_some_and(graphql_is_mutation)
                        .then(|| "a GitHub GraphQL mutation".to_owned())
                } else if method != "GET" {
                    Some(format!("{method} {endpoint}"))
                } else {
                    None
                }
            }
            // switch and setup-git only change gh's and git's config here.
            Some("auth")
                if matches!(
                    sub.get(1),
                    Some(&"status") | Some(&"switch") | Some(&"setup-git")
                ) =>
            {
                None
            }
            Some("version") | None => None,
            Some("run") if sub.get(1) == Some(&"download") => None,
            Some("release") if sub.get(1) == Some(&"download") => None,
            Some("run") if matches!(sub.get(1), Some(&"view") | Some(&"list")) => None,
            Some("pr")
                if matches!(
                    sub.get(1),
                    Some(&"checkout") | Some(&"view") | Some(&"list") | Some(&"diff")
                ) =>
            {
                None
            }
            Some(_) => Some(format!("gh {}", words(3))),
        },
        "glab" => match sub.first().copied() {
            Some("api") => {
                let rest = &args[1..];
                let method = flag_value(rest, &["--method", "-X"])
                    .unwrap_or("GET")
                    .to_ascii_uppercase();
                (method != "GET").then(|| {
                    format!(
                        "glab api {method} {}",
                        first_positional(rest, &GH_API_FLAGS).unwrap_or("")
                    )
                })
            }
            Some("auth") | Some("version") | None => None,
            Some(_) => {
                let verb = sub.get(1).copied().unwrap_or("");
                (!matches!(verb, "list" | "view" | "status" | "ci-view" | "diff"))
                    .then(|| format!("glab {}", words(3)))
            }
        },
        "az" => {
            let joined = sub.join(" ");
            let writes = [
                "repos pr create",
                "repos pr update",
                "repos pr set-vote",
                "repos pr reviewer add",
                "repos pr work-item add",
                "pipelines run",
                "pipelines runs tag add",
                "pipelines build queue",
                "pipelines build cancel",
            ];
            if writes.iter().any(|w| joined.starts_with(w)) {
                return Some(format!("az {joined}"));
            }
            if joined.starts_with("devops invoke") {
                let method = flag_value(args, &["--http-method"])
                    .unwrap_or("GET")
                    .to_ascii_uppercase();
                return (method != "GET")
                    .then(|| format!("az devops invoke --http-method {method}"));
            }
            None
        }
        "kubectl" => {
            // The first non-flag word is the verb (flag values such as
            // `--context NAME` are skipped by `first_positional`).
            let verb = first_positional(
                args,
                &[
                    "--context",
                    "-n",
                    "--namespace",
                    "--kubeconfig",
                    "--request-timeout",
                    "-o",
                    "--output",
                    "-l",
                    "--selector",
                    "-c",
                    "--container",
                    "--tail",
                    "--field-selector",
                    "--cluster",
                    "--user",
                ],
            )
            .unwrap_or("");
            let rest_words: Vec<&str> = sub.iter().copied().skip_while(|w| *w != verb).collect();
            let second = rest_words.get(1).copied().unwrap_or("");
            let write = match verb {
                "apply" | "create" | "delete" | "replace" | "patch" | "edit" | "scale"
                | "autoscale" | "annotate" | "label" | "taint" | "cordon" | "uncordon"
                | "drain" | "expose" | "run" | "set" | "cp" | "exec" | "attach"
                | "port-forward" => true,
                "rollout" => matches!(second, "restart" | "undo" | "pause" | "resume"),
                "config" => matches!(
                    second,
                    "use-context"
                        | "set-context"
                        | "set"
                        | "unset"
                        | "delete-context"
                        | "rename-context"
                        | "set-cluster"
                        | "set-credentials"
                ),
                _ => false,
            };
            write.then(|| format!("kubectl {verb} {second}").trim_end().to_owned())
        }
        "helm" => {
            let verb = sub.first().copied().unwrap_or("");
            matches!(
                verb,
                "install" | "upgrade" | "uninstall" | "delete" | "rollback" | "push"
            )
            .then(|| format!("helm {verb}"))
        }
        "terraform" | "tofu" => {
            let verb = sub.first().copied().unwrap_or("");
            let second = sub.get(1).copied().unwrap_or("");
            let write = match verb {
                "apply" | "destroy" | "import" | "taint" | "untaint" | "force-unlock" => true,
                "state" => matches!(second, "mv" | "rm" | "push" | "replace-provider"),
                "workspace" => matches!(second, "new" | "delete"),
                _ => false,
            };
            write.then(|| format!("{name} {verb}"))
        }
        // `fake_docker` is the test double (`HADION_DOCKER`).
        "docker" | "fake_docker" => docker_write(
            args,
            &super::runtime::docker::endpoint::DockerEnv::from_process(),
        ),
        _ => None,
    }
}

/// A docker command's remote change: a push (to a registry, wherever the
/// daemon is), or a command that changes containers, images, volumes or
/// compose projects on a remote endpoint. `env` is the environment docker
/// reads (`DOCKER_CONFIG`, `DOCKER_HOST`, `DOCKER_CONTEXT`).
fn docker_write(
    args: &[&str],
    env: &super::runtime::docker::endpoint::DockerEnv,
) -> Option<String> {
    use super::runtime::docker::{commands, endpoint};
    let cli = commands::parse_cli(args);
    if commands::pushes(&cli) {
        return Some("docker push".to_owned());
    }
    let what = commands::mutation(&cli)?;
    let target = endpoint::resolve(cli.context, cli.host, cli.config, env);
    target
        .remote()
        .then(|| format!("docker {what} on {}", target.describe()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(program: &str, line: &str) -> Option<String> {
        let args: Vec<&str> = line.split(' ').filter(|s| !s.is_empty()).collect();
        remote_write(program, &args, None)
    }

    #[test]
    fn classifies_writes_and_reads() {
        // git
        assert!(w("git", "push origin main").is_some());
        assert!(w("git", "fetch --all --prune").is_none());
        assert!(w("git", "lfs lock big.psd").is_some());
        assert!(w("git", "lfs ls-files -l").is_none());
        // gh api
        assert!(w("gh", "api -i repos/o/r/pulls").is_none());
        assert!(w("gh", "api -i --method POST --input - repos/o/r/pulls").is_some());
        assert!(w("C:/x/gh.exe", "api -i -X DELETE repos/o/r/actions/caches/1").is_some());
        let q = br#"{"query":"query { viewer { login } }"}"#;
        let m = br#"{"query":"mutation { x }"}"#;
        let args = ["api", "-i", "--method", "POST", "--input", "-", "graphql"];
        assert!(remote_write("gh", &args, Some(q)).is_none());
        assert!(remote_write("gh", &args, Some(m)).is_some());
        // gh subcommands
        assert!(w("gh", "secret set TOKEN --repo o/r").is_some());
        assert!(w("gh", "release create v1 --notes-file -").is_some());
        assert!(w("gh", "release download v1 -p x").is_none());
        assert!(w("gh", "run download 5 -n a -D d").is_none());
        assert!(w("gh", "auth status").is_none());
        assert!(w("gh", "auth switch --hostname github.com --user x").is_none());
        assert!(w("gh", "auth setup-git").is_none());
        assert!(w("gh", "auth logout").is_some());
        assert!(w("gh", "pr checkout 12").is_none());
        // glab / az
        assert!(w("glab", "api projects/1/merge_requests").is_none());
        assert!(w("glab", "api --method POST projects/1/merge_requests").is_some());
        assert!(w("az", "repos pr list --org x").is_none());
        assert!(w("az", "repos pr create --title t").is_some());
        assert!(w("az", "pipelines run --name ci").is_some());
        // kubectl
        assert!(w("kubectl", "--context k get pods -o json").is_none());
        assert!(w("kubectl", "--context k -n ns logs pod -f").is_none());
        assert!(w("kubectl", "--context k rollout restart deploy/x").is_some());
        assert!(w("kubectl", "--context k rollout status deploy/x").is_none());
        assert!(w("kubectl", "--context k -n ns scale deploy/x --replicas=2").is_some());
        assert!(w("kubectl", "config use-context k").is_some());
        assert!(w("kubectl", "config get-contexts -o name").is_none());
        // terraform / helm / docker
        assert!(w("terraform", "plan -input=false").is_none());
        assert!(w("terraform", "apply -auto-approve").is_some());
        assert!(w("tofu", "state rm x").is_some());
        assert!(w("helm", "upgrade x chart").is_some());
        assert!(w("helm", "list").is_none());
        assert!(w("docker", "push ghcr.io/o/x:1").is_some());
        assert!(w("docker", "scout cves x").is_none());
        // others are not classified
        assert!(w("npm", "run deploy").is_none());
        assert!(w("curl", "-sS https://api.anthropic.com/v1/messages").is_none());
    }

    #[test]
    fn docker_writes_count_only_on_remote_endpoints() {
        use crate::devops::runtime::docker::endpoint::{DockerEnv, tests::scratch_config};
        let dir = scratch_config(
            "safety",
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
        let split = |line: &str| -> Vec<String> {
            line.split(' ')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        };
        let d = |line: &str, env: &DockerEnv| {
            let a = split(line);
            let refs: Vec<&str> = a.iter().map(String::as_str).collect();
            docker_write(&refs, env)
        };
        // Local: allowed in safe mode.
        assert!(d("--context desktop-linux stop abc", &env).is_none());
        assert!(
            d(
                "--context desktop-linux compose -p shop down --volumes",
                &env
            )
            .is_none()
        );
        assert!(d("--context desktop-linux build -t x .", &env).is_none());
        // Remote writes, with the endpoint named.
        let w = d("--context staging stop abc", &env).unwrap();
        assert!(
            w.contains("docker stop") && w.contains("ssh://deploy@staging.example.com"),
            "{w}"
        );
        assert!(d("--context staging compose -p shop up -d", &env).is_some());
        assert!(d("--context staging exec -it abc sh", &env).is_some());
        assert!(d("--context staging rm -f abc", &env).is_some());
        assert!(d("--context staging build -t x .", &env).is_some());
        // Remote reads stay allowed.
        assert!(d("--context staging ps -a --no-trunc --format json", &env).is_none());
        assert!(d("--context staging logs -f --tail 500 abc", &env).is_none());
        assert!(
            d(
                "--context staging compose -f a.yml config --format json",
                &env
            )
            .is_none()
        );
        // Pushes always count.
        assert!(d("--context desktop-linux push ghcr.io/o/x:1", &env).is_some());
        assert!(d("--context desktop-linux image push x", &env).is_some());
        // -H, unknown contexts, DOCKER_HOST.
        assert!(d("-H tcp://10.0.0.5:2376 rm -f abc", &env).is_some());
        assert!(
            d("--context nowhere stop abc", &env).is_some(),
            "unreadable means remote"
        );
        assert!(
            d("stop abc", &env).is_none(),
            "the config's current context is local"
        );
        let host = DockerEnv {
            docker_host: Some("ssh://build-box".into()),
            ..env.clone()
        };
        assert!(d("stop abc", &host).is_some());
        assert!(
            d("--context desktop-linux stop abc", &host).is_none(),
            "--context wins"
        );
        assert!(
            d("--context default stop abc", &host).is_some(),
            "default is DOCKER_HOST"
        );
        // The test double is classified like docker.
        assert_eq!(base_name("C:/t/fake_docker.exe"), "fake_docker");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn check_respects_the_switch() {
        // The process-wide switch is not touched: tests run in parallel.
        assert!(check_with(false, "git", &["push", "origin", "main"], None).is_ok());
        let e = check_with(true, "git", &["push", "origin", "main"], None).unwrap_err();
        assert!(e.contains("Safe mode is on"), "{e}");
        assert!(check_with(true, "git", &["status"], None).is_ok());
    }
}
