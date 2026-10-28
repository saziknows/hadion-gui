//! The Claude Code runner against `examples/fake_claude.rs` (no network, no
//! account): the material only on standard input, the environment cleaned,
//! the working folder, and how answers, refusals, limits, retries, sign-in
//! errors, cut streams, enabled tools and cancelling end.
//!
//! The fake is built with the examples (`cargo test -p gitvis-core` builds
//! them; for this file alone: `cargo build -p gitvis-core --example
//! fake_claude` first).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use gitvis_core::devops::ai::cli::{self, CliCall, CliErrorKind, Install};
use gitvis_core::devops::ai::{CliModel, Effort, Failure, Live, Prompt, Via};

fn fake() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let debug = exe.parent().and_then(Path::parent).unwrap();
    let name = if cfg!(windows) {
        "fake_claude.exe"
    } else {
        "fake_claude"
    };
    let p = debug.join("examples").join(name);
    assert!(
        p.is_file(),
        "{} is missing: run `cargo build -p gitvis-core --example fake_claude` first",
        p.display()
    );
    p
}

/// Variables a parent shell (or a parent Claude Code) may have set; the
/// runner must not pass them on.
fn dirty_environment() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: set once, before any test of this file spawns a process
        // that reads them; these names are read only by the child.
        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test-must-not-leak");
            std::env::set_var("ANTHROPIC_BASE_URL", "https://example.invalid");
            std::env::set_var("CLAUDE_CODE_USE_BEDROCK", "1");
            std::env::set_var("CLAUDECODE", "1");
            std::env::set_var("CLAUDE_CODE_SESSION_ID", "parent-session");
            std::env::set_var("CLAUDE_CONFIG_DIR", "C:/claude-config-stays");
        }
    });
}

struct Case {
    dir: PathBuf,
}

impl Case {
    fn new(name: &str, scenario: &str) -> Case {
        dirty_environment();
        let dir = std::env::temp_dir().join(format!(
            "gitvis-ai-cli-{name}-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("fake-log")).unwrap();
        std::fs::write(dir.join("fake-scenario.txt"), scenario).unwrap();
        Case { dir }
    }

    fn install(&self) -> Install {
        Install {
            program: fake(),
            prefix: Vec::new(),
            how: "test",
            shim: None,
        }
    }

    fn run_with(
        &self,
        model: CliModel,
        cancel: &AtomicBool,
        live: &Arc<Mutex<Live>>,
    ) -> Result<gitvis_core::devops::ai::runner::Done, Failure> {
        let install = self.install();
        let call = CliCall {
            install: &install,
            model,
            effort: Effort::Low,
            account: "Claude Max plan".to_owned(),
            cwd: Some(&self.dir),
        };
        cli::send(&call, &prompt(), cancel, live)
    }

    fn run(&self) -> Result<gitvis_core::devops::ai::runner::Done, Failure> {
        self.run_with(
            CliModel::Opus,
            &AtomicBool::new(false),
            &Arc::new(Mutex::new(Live::default())),
        )
    }

    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join("fake-log").join(name)).unwrap_or_default()
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const MATERIAL: &str = "<staged_diff>\ndiff --git a/src/a.rs b/src/a.rs\n+let marker = \"MATERIAL-7731\";\n</staged_diff>";

fn prompt() -> Prompt {
    Prompt {
        system: "You write git commit messages for the staged changes.".to_owned(),
        user: MATERIAL.to_owned(),
    }
}

#[test]
fn answer_material_on_stdin_and_a_clean_environment() {
    let c = Case::new("ok", "ok");
    let done = c.run().expect("an answer");
    assert!(
        done.answer
            .text
            .starts_with("feat(config): read the S3 bucket")
    );
    // The assistant message repeats the text; it is not appended twice.
    assert_eq!(done.answer.text.matches("feat(config)").count(), 1);
    assert_eq!(done.via, Via::ClaudeCode);
    assert_eq!(done.answer.model, "claude-opus-5-5");
    assert!(done.answer.usage.input_tokens > 0 && done.answer.usage.output_tokens > 0);
    let run = done.cli.clone().unwrap();
    assert_eq!(run.cost_estimate, Some(0.0123));
    assert_eq!(run.version, "2.1.281");
    assert_eq!(run.plan.unwrap().five_hour.map(|w| w.0), Some(0.17));
    // The material went on standard input, never in the arguments.
    assert_eq!(c.log("stdin.txt"), MATERIAL);
    let argv = c.log("argv.json");
    assert!(!argv.contains("MATERIAL-7731"), "{argv}");
    let args: Vec<String> = serde_json::from_str(&argv).unwrap();
    assert_eq!(args[0], "-p");
    for f in [
        "--safe-mode",
        "--strict-mcp-config",
        "--no-session-persistence",
    ] {
        assert!(args.iter().any(|a| a == f), "{f}");
    }
    let after = |f: &str| args[args.iter().position(|a| a == f).unwrap() + 1].clone();
    assert_eq!(after("--tools"), "");
    assert_eq!(after("--model"), "opus");
    assert_eq!(after("--effort"), "low");
    assert_eq!(after("--max-turns"), "1");
    // The working folder is the one given.
    assert_eq!(
        PathBuf::from(c.log("cwd.txt").trim())
            .canonicalize()
            .unwrap(),
        c.dir.canonicalize().unwrap()
    );
    // Inherited ANTHROPIC_* / CLAUDE* variables are gone, CLAUDE_CONFIG_DIR stays.
    let env: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&c.log("env.json")).unwrap();
    let has = |k: &str| env.keys().any(|e| e.eq_ignore_ascii_case(k));
    for k in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDECODE",
        "CLAUDE_CODE_SESSION_ID",
    ] {
        assert!(!has(k), "{k} leaked into Claude Code's environment");
    }
    assert!(has("CLAUDE_CONFIG_DIR"));
    for (k, v) in cli::SET_ENV {
        assert_eq!(env.get(k).and_then(|x| x.as_str()), Some(v), "{k}");
    }
}

#[test]
fn default_model_passes_no_model_flag() {
    let c = Case::new("default", "ok");
    c.run_with(
        CliModel::Default,
        &AtomicBool::new(false),
        &Arc::new(Mutex::new(Live::default())),
    )
    .unwrap();
    assert!(!c.log("argv.json").contains("--model"));
}

#[test]
fn refusals_and_max_tokens_are_answers() {
    let c = Case::new("refusal", "refusal");
    let done = c.run().unwrap();
    assert!(done.answer.refused());
    assert_eq!(done.answer.refusal_category.as_deref(), Some("cyber"));
    let c = Case::new("max", "max_tokens");
    assert!(c.run().unwrap().answer.hit_max_tokens());
}

#[test]
fn sign_in_and_plan_limit_errors() {
    for (scenario, kind) in [
        ("signed_out", CliErrorKind::NotSignedIn),
        ("session_limit", CliErrorKind::SessionLimit),
        ("bad_model", CliErrorKind::ModelNotFound),
    ] {
        let c = Case::new(scenario, scenario);
        match c.run() {
            Err(Failure::ClaudeCode(e)) => {
                assert_eq!(e.kind, kind, "{scenario}: {e:?}");
                assert_eq!(e.request_id.as_deref(), Some("req_fake"));
            }
            other => panic!("{scenario}: expected a Claude Code error, got {other:?}"),
        }
    }
}

#[test]
fn retries_show_a_note_while_waiting() {
    let c = Case::new("retry", "retry");
    let live = Arc::new(Mutex::new(Live::default()));
    let seen = Arc::new(Mutex::new(None::<String>));
    let (l2, s2) = (live.clone(), seen.clone());
    let done_flag = Arc::new(AtomicBool::new(false));
    let d2 = done_flag.clone();
    let watcher = std::thread::spawn(move || {
        while !d2.load(Ordering::Relaxed) {
            if let Some(n) = l2.lock().unwrap().note.clone() {
                *s2.lock().unwrap() = Some(n);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let r = c.run_with(CliModel::Opus, &AtomicBool::new(false), &live);
    done_flag.store(true, Ordering::Relaxed);
    watcher.join().unwrap();
    assert!(r.is_ok(), "{r:?}");
    let note = seen
        .lock()
        .unwrap()
        .clone()
        .expect("a retry note while waiting");
    assert_eq!(
        note,
        "Retrying after a rate limit (HTTP 429): attempt 1 of 10, in 2 s"
    );
    // Once the answer streams, the note is gone.
    assert_eq!(live.lock().unwrap().note, None);
}

#[test]
fn a_cut_stream_is_incomplete() {
    let c = Case::new("cut", "cut");
    match c.run() {
        Err(Failure::Incomplete(m)) => assert!(m.contains("exit 1"), "{m}"),
        other => panic!("expected Incomplete, got {other:?}"),
    }
}

#[test]
fn enabled_tools_stop_the_run() {
    let c = Case::new("tools", "tools");
    let t = Instant::now();
    match c.run() {
        Err(Failure::ClaudeCode(e)) => {
            assert_eq!(e.kind, CliErrorKind::ToolsEnabled);
            assert!(e.message.contains("Bash, Read"), "{}", e.message);
        }
        other => panic!("expected ToolsEnabled, got {other:?}"),
    }
    assert!(
        t.elapsed() < Duration::from_secs(4),
        "it waited for the fake"
    );
}

#[test]
fn cancel_kills_it() {
    let c = Case::new("hang", "hang");
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(600));
        c2.store(true, Ordering::Relaxed);
    });
    let t = Instant::now();
    let r = c.run_with(
        CliModel::Opus,
        &cancel,
        &Arc::new(Mutex::new(Live::default())),
    );
    assert!(matches!(r, Err(Failure::Cancelled)), "{r:?}");
    assert!(t.elapsed() < Duration::from_secs(10));
}

#[test]
fn detection_reads_version_and_sign_in() {
    for (auth, plan, label) in [
        ("plan", true, "Claude Max plan"),
        ("env_key", false, "API key (ANTHROPIC_API_KEY)"),
        ("api_key", false, "API key"),
        ("none", false, "Not signed in"),
    ] {
        let c = Case::new(auth, "ok");
        std::fs::write(c.dir.join("fake-auth.txt"), auth).unwrap();
        let s = cli::detect_in(Some(c.install()), &c.dir);
        assert_eq!(s.version, Some(cli::Version(2, 1, 281)), "{s:?}");
        assert!(!s.old());
        let a = s.auth.clone().expect("auth status read");
        assert_eq!(a.plan(), plan, "{auth}");
        assert_eq!(a.label(), label);
        assert_eq!(s.plan_ready(), plan);
    }
    let s = cli::detect_in(None, &std::env::temp_dir());
    assert!(!s.installed());
    assert_eq!(s.summary(), "Claude Code is not installed");
}
