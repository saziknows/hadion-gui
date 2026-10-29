//! Issue lookups through a fake `curl` on PATH: the credential travels only
//! on standard input (`curl --config -`), never on the command line.
//! (Its own test binary: it changes PATH for the process.)

use std::path::Path;

use hadion_core::devops::automation::links::Tracker;
use hadion_core::devops::automation::lookup::{self, base64};

fn fake_curl(dir: &Path, response: &str) {
    std::fs::write(dir.join("response.txt"), response).unwrap();
    if cfg!(windows) {
        std::fs::write(
            dir.join("curl.cmd"),
            "@echo off\r\necho %*> \"%~dp0args.txt\"\r\nmore > \"%~dp0stdin.txt\"\r\ntype \"%~dp0response.txt\"\r\n",
        )
        .unwrap();
    } else {
        let p = dir.join("curl");
        std::fs::write(
            &p,
            "#!/bin/sh\nd=$(dirname \"$0\")\necho \"$@\" > \"$d/args.txt\"\ncat > \"$d/stdin.txt\"\ncat \"$d/response.txt\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

#[test]
fn lookups_send_credentials_on_stdin_only() {
    let dir = std::env::temp_dir().join(format!("hadion-fake-curl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![dir.clone()];
    paths.extend(std::env::split_paths(&path));
    // SAFETY: this test binary has a single test; nothing else reads the
    // environment concurrently.
    unsafe {
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
        std::env::remove_var("HADION_CURL");
    }
    assert_eq!(lookup::curl_program(), "curl");

    // Jira.
    let token = "ATATT-very-secret-token";
    fake_curl(
        &dir,
        "{\"key\":\"ABC-1\",\"fields\":{\"summary\":\"Login fails\",\"status\":{\"name\":\"In Progress\"}}}\n200",
    );
    let req = lookup::jira_request("acme.atlassian.net", "ABC-1", "me@example.com", token);
    let mut commands = Vec::new();
    let info = lookup::lookup(Tracker::Jira, &req, "curl", &dir, &mut commands).unwrap();
    assert_eq!(info.title, "Login fails");
    assert_eq!(info.status, "In Progress");
    let args = std::fs::read_to_string(dir.join("args.txt")).unwrap();
    assert_eq!(args.trim(), "--config -");
    let stdin = std::fs::read_to_string(dir.join("stdin.txt")).unwrap();
    let creds = base64(format!("me@example.com:{token}").as_bytes());
    assert!(
        stdin.contains(&format!("Authorization: Basic {creds}")),
        "{stdin}"
    );
    assert!(stdin.contains("https://acme.atlassian.net/rest/api/3/issue/ABC-1"));
    let shown = commands.join("\n");
    assert!(!shown.contains(token) && !shown.contains(&creds), "{shown}");
    assert!(shown.starts_with("curl --config -"));

    // Linear, and an HTTP error with the tracker's message.
    let key = "lin_api_secret_key";
    fake_curl(
        &dir,
        "{\"data\":{\"issue\":{\"identifier\":\"ENG-42\",\"title\":\"Speed up\",\"state\":{\"name\":\"Todo\"}}}}\n200",
    );
    let req = lookup::linear_request("ENG-42", key);
    let info = lookup::lookup(Tracker::Linear, &req, "curl", &dir, &mut commands).unwrap();
    assert_eq!(
        (info.key.as_str(), info.title.as_str()),
        ("ENG-42", "Speed up")
    );
    let stdin = std::fs::read_to_string(dir.join("stdin.txt")).unwrap();
    assert!(stdin.contains(&format!("Authorization: {key}")));
    assert_eq!(
        std::fs::read_to_string(dir.join("args.txt"))
            .unwrap()
            .trim(),
        "--config -"
    );
    fake_curl(&dir, "{\"errorMessages\":[\"Issue does not exist\"]}\n404");
    let req = lookup::jira_request("acme.atlassian.net", "ABC-9", "me@example.com", token);
    let err = lookup::lookup(Tracker::Jira, &req, "curl", &dir, &mut commands).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("HTTP 404") && msg.contains("Issue does not exist"),
        "{msg}"
    );
    assert!(!msg.contains(token));
    let _ = std::fs::remove_dir_all(&dir);
}
