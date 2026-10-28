//! The Claude API runner against a fake `curl` (a batch file): no network,
//! no real key. Checks that the key travels only on standard input, that
//! the request file exists while curl runs and is deleted afterwards, and
//! how answers, refusals, error events, HTTP errors and cancelling end.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gitvis_core::devops::ai::runner::{self, Call, KeySource, Live};
use gitvis_core::devops::ai::{Effort, Failure, Model, Prompt};

const FAKE: &str = r#"@echo off
setlocal
set "D=%~dp0"
echo %*> "%D%argv.txt"
findstr "^" > "%D%stdin.txt"
set "P="
for /f "usebackq tokens=1,* delims=@" %%a in (`findstr /b "data-binary" "%D%stdin.txt"`) do set "P=%%b"
if not defined P (echo no data-binary in the config 1>&2 & exit /b 26)
set "P=%P:~0,-1%"
set "P=%P:\\=\%"
if exist "%P%" (copy /y "%P%" "%D%body.json" >nul) else (echo missing body file 1>&2 & exit /b 26)
if exist "%D%slow.txt" ping -n 6 127.0.0.1 >nul
type "%D%response.txt"
type "%D%status.txt" 1>&2
set E=0
if exist "%D%exit.txt" set /p E=<"%D%exit.txt"
exit /b %E%
"#;

const KEY: &str = "sk-ant-test-fake-0123456789";

fn fake(name: &str, response: &str, status: &str, exit: Option<i32>) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gitvis-ai-fake-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("curl.cmd"), FAKE.replace('\n', "\r\n")).unwrap();
    std::fs::write(dir.join("response.txt"), response).unwrap();
    std::fs::write(dir.join("status.txt"), format!("{status}\r\n")).unwrap();
    if let Some(code) = exit {
        std::fs::write(dir.join("exit.txt"), code.to_string()).unwrap();
    }
    dir
}

fn prompt() -> Prompt {
    Prompt {
        system: "You write commit messages.".into(),
        user: "<staged_diff>\n+x\n</staged_diff>".into(),
    }
}

fn run(dir: &Path, model: Model, cancel: &AtomicBool) -> Result<runner::Done, Failure> {
    let program = dir.join("curl.cmd");
    let program = program.to_string_lossy();
    let live = Arc::new(Mutex::new(Live::default()));
    runner::send_with(
        &Call {
            program: &program,
            dir,
            key: KEY,
            source: KeySource::Env,
            model,
            effort: Effort::Low,
            temp_dir: Some(dir),
        },
        &prompt(),
        cancel,
        &live,
    )
}

/// Request files left in a test's directory.
fn leftovers(dir: &Path) -> Vec<PathBuf> {
    let prefix = format!("gitvis-ai-{}-", std::process::id());
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(&prefix))
                && p.extension().is_some_and(|e| e == "json")
        })
        .collect()
}

const ANSWER: &str = "event: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":120,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0,\"output_tokens\":1}}}\r\n\r\nevent: content_block_start\r\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\r\n\r\nevent: content_block_delta\r\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Add the fake\"}}\r\n\r\nevent: message_delta\r\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":9}}\r\n\r\nevent: message_stop\r\ndata: {\"type\":\"message_stop\"}\r\n\r\n";

#[test]
fn answer_key_on_stdin_only_and_body_file_removed() {
    let dir = fake(
        "ok",
        ANSWER,
        "gitvis-http: 200 retry-after= request-id=req_1",
        None,
    );
    let done = run(&dir, Model::Opus55, &AtomicBool::new(false)).expect("answer");
    assert_eq!(done.answer.text, "Add the fake");
    assert_eq!(done.answer.usage.input_tokens, 120);
    assert_eq!(done.answer.usage.output_tokens, 9);
    // argv: the fixed arguments, never the key.
    let argv = std::fs::read_to_string(dir.join("argv.txt")).unwrap();
    assert!(!argv.contains(KEY), "{argv}");
    assert!(argv.contains("--config -"), "{argv}");
    assert!(argv.contains("https://api.anthropic.com/v1/messages"));
    // stdin: the key header and the other headers.
    let stdin = std::fs::read_to_string(dir.join("stdin.txt")).unwrap();
    assert!(stdin.contains(&format!("header = \"x-api-key: {KEY}\"")));
    assert!(stdin.contains("anthropic-version: 2026-10-21"));
    assert!(stdin.contains("anthropic-beta: server-side-fallback-2026-07-01"));
    // The body file existed while curl ran (the fake copied it) …
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("body.json")).unwrap()).unwrap();
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["output_config"]["effort"], "low");
    assert!(body.get("temperature").is_none());
    assert!(!body.to_string().contains(KEY));
    // … and is gone now.
    assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn http_401_is_an_api_error() {
    let body = "{\"type\":\"error\",\"error\":{\"type\":\"authentication_error\",\"message\":\"invalid x-api-key\"},\"request_id\":\"req_401\"}\r\n";
    let dir = fake(
        "401",
        body,
        "gitvis-http: 401 retry-after= request-id=req_401",
        Some(22),
    );
    match run(&dir, Model::Haiku55, &AtomicBool::new(false)) {
        Err(Failure::Api(e)) => {
            assert_eq!(e.kind, "authentication_error");
            assert_eq!(e.status, Some(401));
            assert_eq!(e.request_id.as_deref(), Some("req_401"));
            assert!(!e.retryable());
        }
        other => panic!("expected an API error, got {other:?}"),
    }
    // Haiku: no fallback beta header.
    let stdin = std::fs::read_to_string(dir.join("stdin.txt")).unwrap();
    assert!(!stdin.contains("anthropic-beta"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rate_limit_carries_retry_after() {
    let body = "{\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"slow down\"}}\r\n";
    let dir = fake(
        "429",
        body,
        "gitvis-http: 429 retry-after=17 request-id=req_9",
        Some(22),
    );
    match run(&dir, Model::Opus55, &AtomicBool::new(false)) {
        Err(Failure::Api(e)) => {
            assert!(e.retryable());
            assert_eq!(e.retry_after, Some(17));
            assert_eq!(e.request_id.as_deref(), Some("req_9"));
        }
        other => panic!("expected a rate limit, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn error_event_and_refusal() {
    let err = "event: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":5}}}\r\n\r\nevent: error\r\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\r\n\r\n";
    let dir = fake(
        "err",
        err,
        "gitvis-http: 200 retry-after= request-id=req_e",
        None,
    );
    match run(&dir, Model::Opus55, &AtomicBool::new(false)) {
        Err(Failure::Api(e)) => {
            assert_eq!(e.kind, "overloaded_error");
            assert!(e.retryable());
            assert_eq!(e.request_id.as_deref(), Some("req_e"));
        }
        other => panic!("expected an error event, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
    let refusal = "event: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":5}}}\r\n\r\nevent: message_delta\r\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\",\"stop_details\":{\"category\":\"cyber\"}},\"usage\":{\"output_tokens\":0}}\r\n\r\nevent: message_stop\r\ndata: {\"type\":\"message_stop\"}\r\n\r\n";
    let dir = fake(
        "refusal",
        refusal,
        "gitvis-http: 200 retry-after= request-id=",
        None,
    );
    let done = run(&dir, Model::Opus55, &AtomicBool::new(false)).expect("a refusal is an answer");
    assert!(done.answer.refused());
    assert_eq!(done.answer.refusal_category.as_deref(), Some("cyber"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn truncated_stream_is_incomplete() {
    let cut = &ANSWER[..ANSWER.find("event: message_delta").unwrap()];
    let dir = fake(
        "cut",
        cut,
        "gitvis-http: 200 retry-after= request-id=",
        None,
    );
    assert!(matches!(
        run(&dir, Model::Opus55, &AtomicBool::new(false)),
        Err(Failure::Incomplete(_))
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancel_stops_and_removes_the_file() {
    let dir = fake(
        "slow",
        ANSWER,
        "gitvis-http: 200 retry-after= request-id=",
        None,
    );
    std::fs::write(dir.join("slow.txt"), "1").unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(800));
        c.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    let started = std::time::Instant::now();
    let r = run(&dir, Model::Opus55, &cancel);
    assert!(matches!(r, Err(Failure::Cancelled)), "{r:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_curl() {
    let live = Arc::new(Mutex::new(Live::default()));
    let r = runner::send_with(
        &Call {
            program: "definitely-not-curl-xyz",
            dir: Path::new("."),
            key: KEY,
            source: KeySource::Env,
            model: Model::Opus55,
            effort: Effort::Low,
            temp_dir: None,
        },
        &prompt(),
        &AtomicBool::new(false),
        &live,
    );
    assert!(matches!(r, Err(Failure::NoCurl)));
}
