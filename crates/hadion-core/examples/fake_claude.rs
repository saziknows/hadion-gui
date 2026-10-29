//! A Claude Code stand-in for tests and UI screenshots: no network, no
//! account. Point Hadion at it with `HADION_CLAUDE`:
//!
//! ```text
//! cargo build -p hadion-core --example fake_claude
//! HADION_CLAUDE=target/debug/examples/fake_claude.exe FAKE_CLAUDE_SCENARIO=slow hadion-test.exe <repo>
//! ```
//!
//! - `--version` prints `FAKE_CLAUDE_VERSION` (default `2.1.281 (Claude Code)`).
//! - `auth status` prints the JSON of `FAKE_CLAUDE_AUTH`: `plan` (default),
//!   `env_key` (ANTHROPIC_API_KEY would be used), `api_key` (an API login),
//!   `none` (signed out; exit 1).
//! - A request (`-p …`) replays a scenario chosen by `FAKE_CLAUDE_SCENARIO`
//!   (or the next entry of the comma-separated `FAKE_CLAUDE_SEQ`): `ok`,
//!   `slow`, `refusal`, `max_tokens`, `retry`, `signed_out`,
//!   `session_limit`, `bad_model`, `tools`, `cut`, `hang`. A path to a
//!   `.jsonl` file replays that file. The answer's text depends on the
//!   feature named in `--system-prompt`.
//! - With `FAKE_CLAUDE_LOG=<dir>` each request writes `argv.json`,
//!   `env.json` (the `ANTHROPIC_*` / `CLAUDE*` / `DISABLE_*` variables it
//!   saw), `cwd.txt` and `stdin.txt` there.
//! - Without those variables it reads `fake-scenario.txt` and
//!   `fake-auth.txt` from its working directory and logs to `fake-log/`
//!   there when that folder exists (tests run in parallel, each in its own
//!   folder).

use std::io::{Read, Write};
use std::time::Duration;

use serde_json::json;

fn out(v: serde_json::Value) {
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{v}");
    let _ = o.flush();
}

fn sleep(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// An environment variable, else a file in the working directory.
fn setting(var: &str, file: &str) -> Option<String> {
    std::env::var(var)
        .ok()
        .or_else(|| std::fs::read_to_string(file).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn auth(kind: &str) -> (serde_json::Value, i32) {
    match kind {
        "none" => (
            json!({"loggedIn": false, "authMethod": "none", "apiProvider": "firstParty"}),
            1,
        ),
        "env_key" => (
            json!({"loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty",
                   "apiKeySource": "ANTHROPIC_API_KEY", "email": null, "subscriptionType": null}),
            0,
        ),
        "api_key" => (
            json!({"loggedIn": true, "authMethod": "api_key", "apiProvider": "firstParty"}),
            0,
        ),
        _ => (
            json!({"loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty",
                   "email": "ada@example.test", "orgName": "Ada's Organization",
                   "subscriptionType": "max"}),
            0,
        ),
    }
}

/// The scenario for this request: `FAKE_CLAUDE_SEQ` (one entry per request,
/// counted in the log folder) or `FAKE_CLAUDE_SCENARIO`.
fn scenario(log: Option<&std::path::Path>) -> String {
    if let Ok(seq) = std::env::var("FAKE_CLAUDE_SEQ") {
        let items: Vec<&str> = seq.split(',').filter(|s| !s.is_empty()).collect();
        if !items.is_empty() {
            let counter = log
                .map(|d| d.join("seq.txt"))
                .unwrap_or_else(|| std::env::temp_dir().join("fake-claude-seq.txt"));
            let i: usize = std::fs::read_to_string(&counter)
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            let _ = std::fs::write(&counter, (i + 1).to_string());
            return items[i % items.len()].to_owned();
        }
    }
    setting("FAKE_CLAUDE_SCENARIO", "fake-scenario.txt").unwrap_or_else(|| "ok".to_owned())
}

fn answer_for(system: &str) -> String {
    if system.contains("commit messages") {
        return "feat(config): read the S3 bucket from the settings file\n\nThe upload job took the bucket name from a constant. It now reads\n`storage.bucket` from settings.toml, so staging and production can\nuse different buckets without a rebuild.\n\n- add the `storage` section to the settings loader\n- fall back to the old default when the key is missing".to_owned();
    }
    if system.contains("title and description of a GitHub pull request") {
        return "Title: Read the upload bucket from the settings\n\n## What\n\nThe upload job now reads `storage.bucket` from `settings.toml` instead of a constant.\n\n## Why\n\nStaging and production need different buckets.\n\n## How to test\n\n- [ ] Run `cargo test -p uploader`".to_owned();
    }
    if system.contains("GitHub Actions job failed") {
        return "## Likely cause\nThe `cargo test` step failed because `tests/upload.rs` expects `UPLOAD_BUCKET`, which the workflow does not set.\n## Evidence\n> 1187 | thread 'upload::uses_bucket' panicked at tests/upload.rs:42:10\n> 1203 | ##[error] Process completed with exit code 101.\n## Suggested fix\n```yaml\nenv:\n  UPLOAD_BUCKET: test-bucket\n```\n## Flaky?\nNo — the variable is missing on every run.".to_owned();
    }
    if system.contains("reviewer understand") {
        return "## Summary\n- Moves the upload bucket into `settings.toml`.\n- Falls back to the previous default.\n## Risky areas\n- `src/settings.rs`: a typo silently selects the default bucket.\n## Questions for the author\n1. Should a missing bucket be an error in production?".to_owned();
    }
    if system.contains("single git commit") {
        return "## What it does\n- Adds the `storage` section to the settings loader.\n## Risk\nLow — a missing key keeps the previous behaviour.".to_owned();
    }
    "OK".to_owned()
}

fn init(model: &str, tools: &[&str]) {
    out(
        json!({"type": "system", "subtype": "init", "cwd": std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default(),
               "session_id": "fake-session", "tools": tools, "mcp_servers": [], "model": model,
               "permissionMode": "default", "slash_commands": [], "apiKeySource": "none",
               "claude_code_version": "2.1.281", "output_style": "default", "skills": []}),
    );
    out(
        json!({"type": "system", "subtype": "status", "status": "requesting", "session_id": "fake-session"}),
    );
}

fn ev(e: serde_json::Value) {
    out(
        json!({"type": "stream_event", "event": e, "session_id": "fake-session", "parent_tool_use_id": null}),
    );
}

fn result(text: &str, stop: &str, is_error: bool, status: Option<u16>, input: u64, output: u64) {
    out(
        json!({"type": "result", "subtype": "success", "is_error": is_error, "duration_ms": 1200,
               "duration_api_ms": 1100, "num_turns": 1, "result": text, "stop_reason": stop,
               "session_id": "fake-session", "total_cost_usd": if is_error { 0.0 } else { 0.0123 },
               "usage": {"input_tokens": input, "cache_creation_input_tokens": 0,
                         "cache_read_input_tokens": 0, "output_tokens": output},
               "terminal_reason": if is_error { "api_error" } else { "completed" },
               "api_error_status": status}),
    );
}

fn synthetic_error(text: &str, code: &str, status: Option<u16>) -> i32 {
    out(
        json!({"type": "assistant", "message": {"id": "m", "model": "<synthetic>", "role": "assistant",
               "stop_reason": "stop_sequence", "content": [{"type": "text", "text": text}]},
               "session_id": "fake-session", "error": code, "request_id": "req_fake", "is_api_error_message": true}),
    );
    result(text, "stop_sequence", true, status, 0, 0);
    1
}

fn stream_answer(model: &str, text: &str, slow: bool, stop: &str, input: u64) {
    ev(
        json!({"type": "message_start", "message": {"model": model, "id": "msg_fake", "type": "message",
              "role": "assistant", "content": [], "stop_reason": null,
              "usage": {"input_tokens": input, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0, "output_tokens": 1}}}),
    );
    ev(
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
    );
    ev(
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "c2ln"}}),
    );
    if slow {
        sleep(1500);
    }
    ev(json!({"type": "content_block_stop", "index": 0}));
    ev(
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
    );
    let words: Vec<&str> = text.split(' ').collect();
    let step = if slow { 1 } else { 6 };
    for (i, chunk) in words.chunks(step).enumerate() {
        let mut t = chunk.join(" ");
        if (i + 1) * step < words.len() {
            t.push(' ');
        }
        ev(
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": t}}),
        );
        if slow {
            sleep(120);
        }
    }
    // The assistant message repeats the text; Hadion must not show it twice.
    out(
        json!({"type": "assistant", "message": {"model": model, "id": "msg_fake", "content": [{"type": "text", "text": text}]}, "session_id": "fake-session"}),
    );
    ev(json!({"type": "content_block_stop", "index": 1}));
    let out_tokens = (text.len() / 4).max(1) as u64;
    ev(
        json!({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null},
              "usage": {"input_tokens": input, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0, "output_tokens": out_tokens}}),
    );
    ev(json!({"type": "message_stop"}));
    out(
        json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed", "resetsAt": 1893456000,
               "rateLimitType": "five_hour", "utilization": 0.17, "isUsingOverage": false,
               "unifiedWindows": {"five_hour": {"utilization": 0.17, "resetsAt": 1893456000},
                                  "seven_day": {"utilization": 0.27, "resetsAt": 1893800000}}},
               "session_id": "fake-session"}),
    );
    result(text, stop, false, None, input, out_tokens);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-v") {
        println!(
            "{}",
            std::env::var("FAKE_CLAUDE_VERSION")
                .unwrap_or_else(|_| "2.1.281 (Claude Code)".to_owned())
        );
        return;
    }
    if args.first().map(String::as_str) == Some("auth") {
        let (v, code) = auth(&setting("FAKE_CLAUDE_AUTH", "fake-auth.txt").unwrap_or_default());
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        std::process::exit(code);
    }
    // A request: the material arrives on standard input.
    let mut stdin = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut stdin);
    let log = std::env::var_os("FAKE_CLAUDE_LOG")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            let d = std::path::PathBuf::from("fake-log");
            d.is_dir().then_some(d)
        });
    if let Some(dir) = &log {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(
            dir.join("argv.json"),
            serde_json::to_string_pretty(&args).unwrap_or_default(),
        );
        let env: serde_json::Map<String, serde_json::Value> = std::env::vars()
            .filter(|(k, _)| {
                let u = k.to_ascii_uppercase();
                u.starts_with("ANTHROPIC") || u.starts_with("CLAUDE") || u.starts_with("DISABLE_")
            })
            .map(|(k, v)| {
                // Never write a key's value, only that it was there.
                let shown = if k.to_ascii_uppercase().contains("KEY")
                    || k.to_ascii_uppercase().contains("TOKEN")
                {
                    "(set)".to_owned()
                } else {
                    v
                };
                (k, serde_json::Value::String(shown))
            })
            .collect();
        let _ = std::fs::write(
            dir.join("env.json"),
            serde_json::to_string_pretty(&env).unwrap_or_default(),
        );
        let _ = std::fs::write(
            dir.join("cwd.txt"),
            std::env::current_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_default(),
        );
        let _ = std::fs::write(dir.join("stdin.txt"), &stdin);
    }
    let after = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_default()
    };
    let system = after("--system-prompt");
    let model = match after("--model").as_str() {
        "" | "opus" => "claude-opus-5-5".to_owned(),
        "sonnet" => "claude-sonnet-5-5".to_owned(),
        "haiku" => "claude-haiku-4-5-20251001".to_owned(),
        m => m.to_owned(),
    };
    let input = (stdin.len() / 4) as u64 + 400;
    let name = scenario(log.as_deref());
    if name.ends_with(".jsonl") {
        let text = std::fs::read_to_string(&name).unwrap_or_default();
        for line in text.lines() {
            println!("{line}");
        }
        std::process::exit(0);
    }
    let text = answer_for(&system);
    let code = match name.as_str() {
        "tools" => {
            init(&model, &["Bash", "Read"]);
            sleep(5000);
            0
        }
        "hang" => {
            init(&model, &[]);
            sleep(60_000);
            0
        }
        "signed_out" => {
            init(&model, &[]);
            synthetic_error(
                "Not logged in · Please run /login",
                "authentication_failed",
                None,
            )
        }
        "session_limit" => {
            init(&model, &[]);
            synthetic_error(
                "You've hit your session limit · resets 7pm",
                "rate_limit",
                Some(429),
            )
        }
        "bad_model" => {
            init(&model, &[]);
            synthetic_error(
                &format!(
                    "There's an issue with the selected model ({model}). It may not exist or you may not have access to it. Run --model to pick a different model."
                ),
                "model_not_found",
                Some(404),
            )
        }
        "refusal" => {
            init(&model, &[]);
            ev(
                json!({"type": "message_start", "message": {"model": model, "usage": {"input_tokens": input, "output_tokens": 0}}}),
            );
            ev(
                json!({"type": "message_delta", "delta": {"stop_reason": "refusal",
                      "stop_details": {"type": "refusal", "category": "cyber", "explanation": "The request looks like exploit development."}},
                      "usage": {"output_tokens": 0}}),
            );
            ev(json!({"type": "message_stop"}));
            result("", "refusal", false, None, input, 0);
            0
        }
        "retry" => {
            init(&model, &[]);
            out(
                json!({"type": "system", "subtype": "api_retry", "attempt": 1, "max_retries": 10,
                       "retry_delay_ms": 1500, "error_status": 429, "error": "rate_limit", "session_id": "fake-session"}),
            );
            sleep(2500);
            stream_answer(&model, &text, false, "end_turn", input);
            0
        }
        "cut" => {
            init(&model, &[]);
            ev(
                json!({"type": "message_start", "message": {"model": model, "usage": {"input_tokens": input}}}),
            );
            ev(
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            );
            ev(
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Partial "}}),
            );
            1
        }
        "max_tokens" => {
            init(&model, &[]);
            stream_answer(&model, &text, false, "max_tokens", input);
            0
        }
        "slow" => {
            init(&model, &[]);
            stream_answer(&model, &text, true, "end_turn", input);
            0
        }
        _ => {
            init(&model, &[]);
            stream_answer(&model, &text, false, "end_turn", input);
            0
        }
    };
    std::process::exit(code);
}
