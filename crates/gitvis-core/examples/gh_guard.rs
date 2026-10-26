//! A `gh` stand-in for UI testing: reads go to the real `gh`, every write
//! is refused and logged. Point gitvis at it with `GITVIS_GH`:
//!
//! ```text
//! cargo build -p gitvis-core --example gh_guard
//! GITVIS_GH=target/debug/examples/gh_guard.exe GH_GUARD_LOG=guard.log gitvis-test.exe <repo>
//! ```
//!
//! Passed through: `gh api` GET requests and GraphQL queries. Refused:
//! any other REST method, GraphQL mutations, and every other `gh`
//! subcommand (`secret set`, `release create`, `pr checkout`, `run download`,
//! …). Each call is appended to `GH_GUARD_LOG` as `pass …` or `BLOCKED …`.
//! `GH_GUARD_REAL` names the real executable (default `gh`);
//! `GH_GUARD_FAIL=message` makes every call fail with that message, for
//! error states.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

fn log(line: &str) {
    if let Ok(path) = std::env::var("GH_GUARD_LOG")
        && let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

fn block(why: &str) -> ! {
    log(&format!("BLOCKED {why}"));
    eprintln!("gh-guard: blocked a write while testing ({why})");
    std::process::exit(1)
}

/// Is this GraphQL request body a mutation (or something unreadable,
/// which is treated as one)?
fn is_mutation(body: &[u8]) -> bool {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return true;
    };
    let query = v.get("query").and_then(|q| q.as_str()).unwrap_or("");
    // Skip comments and whitespace; the first keyword decides.
    let first = query
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join(" ");
    first.trim_start().starts_with("mutation") || first.trim_start().starts_with("subscription")
}

/// The endpoint of `gh api [flags] ENDPOINT [flags]`: the first argument
/// that is neither a flag nor a flag's value.
fn endpoint_of(args: &[String]) -> String {
    const WITH_VALUE: [&str; 16] = [
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
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if WITH_VALUE.contains(&a) {
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            i += 1;
            continue;
        }
        return a.to_owned();
    }
    String::new()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Ok(msg) = std::env::var("GH_GUARD_FAIL") {
        log(&format!("FAIL {}", args.join(" ")));
        eprintln!("{msg}");
        std::process::exit(4);
    }
    let real = std::env::var("GH_GUARD_REAL").unwrap_or_else(|_| "gh".into());
    let has_input = args.windows(2).any(|w| w[0] == "--input" && w[1] == "-");
    let mut input = Vec::new();
    if has_input {
        let _ = std::io::stdin().read_to_end(&mut input);
    }
    // `gh auth status` and `gh --version` are reads too.
    let read_only_cli = matches!(
        args.iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["auth", "status", ..] | ["--version"] | ["version"]
    );
    if args.first().map(String::as_str) != Some("api") && !read_only_cli {
        block(&format!("gh {}", args.join(" ")));
    }
    if args.first().map(String::as_str) == Some("api") {
        let method = args
            .windows(2)
            .find(|w| w[0] == "--method" || w[0] == "-X")
            .map(|w| w[1].to_uppercase())
            .unwrap_or_else(|| "GET".into());
        let endpoint = endpoint_of(&args[1..]);
        if endpoint == "graphql" {
            if is_mutation(&input) {
                block("GraphQL mutation");
            }
        } else if method != "GET" {
            block(&format!("{method} {endpoint}"));
        }
        log(&format!("pass {method} {endpoint}"));
    } else {
        log(&format!("pass gh {}", args.join(" ")));
    }
    let mut child = match Command::new(&real)
        .args(&args)
        .stdin(if has_input {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("gh-guard: could not start {real}: {e}");
            std::process::exit(1);
        }
    };
    if has_input && let Some(mut s) = child.stdin.take() {
        let _ = s.write_all(&input);
    }
    let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(1);
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_is_the_first_positional() {
        let a = |s: &str| s.split(' ').map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(
            endpoint_of(&a("-i --method POST --input - graphql")),
            "graphql"
        );
        assert_eq!(endpoint_of(&a("-i graphql --input -")), "graphql");
        assert_eq!(
            endpoint_of(&a(
                "-i --allow-escape-sequences repos/o/r/actions/jobs/1/logs"
            )),
            "repos/o/r/actions/jobs/1/logs"
        );
    }

    #[test]
    fn mutations_are_recognised() {
        assert!(is_mutation(br#"{"query":"mutation($id:ID!){ x }"}"#));
        assert!(is_mutation(
            br#"{"query":"mutation { resolveReviewThread }"}"#
        ));
        assert!(is_mutation(br#"{"query":"  # c\n mutation M { x }"}"#));
        assert!(!is_mutation(br#"{"query":"query { viewer { login } }"}"#));
        assert!(!is_mutation(br#"{"query":"{ viewer { login } }"}"#));
        assert!(is_mutation(b"not json"));
    }
}
