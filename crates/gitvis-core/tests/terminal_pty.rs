//! Real shells in a pseudo-terminal: the ConPTY cursor query is answered
//! (or nothing would ever appear), output reaches the screen, typed input
//! reaches the shell, exit codes are recorded.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use gitvis_core::devops::terminal::{Colors, Session, SpawnOptions, Status};

fn opts(program: &str, args: &[&str]) -> SpawnOptions {
    SpawnOptions {
        program: PathBuf::from(program),
        args: args.iter().map(|s| s.to_string()).collect(),
        cwd: std::env::temp_dir(),
        rows: 24,
        cols: 100,
        scrollback: 1000,
        env: Vec::new(),
        colors: Colors::default(),
    }
}

/// Wait until `done` holds for the session's screen text and status.
fn wait_for(
    session: &Session,
    secs: u64,
    done: impl Fn(&str, &Status) -> bool,
) -> (String, Status) {
    let start = Instant::now();
    loop {
        let (text, status) = {
            let t = session.term();
            (t.screen.all_text(), t.status.clone())
        };
        if done(&text, &status) || start.elapsed() > Duration::from_secs(secs) {
            return (text, status);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(windows)]
fn cmd() -> String {
    std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into())
}

#[test]
fn one_shot_command_output_and_exit_code() {
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    #[cfg(windows)]
    let o = opts(&cmd(), &["/C", "echo hello-from-pty"]);
    #[cfg(not(windows))]
    let o = opts("/bin/sh", &["-c", "echo hello-from-pty"]);
    let session = Session::spawn(o, move || {
        w.fetch_add(1, Ordering::Relaxed);
    });
    let (text, status) = wait_for(&session, 20, |text, status| {
        text.contains("hello-from-pty") && !status.is_alive()
    });
    assert!(
        text.contains("hello-from-pty"),
        "screen: {text:?}, status {status:?}"
    );
    assert!(
        matches!(status, Status::Exited { code: 0, .. }),
        "{status:?}"
    );
    assert!(wakes.load(Ordering::Relaxed) > 0);
    let t = session.term();
    assert!(t.bytes_in > 0);
    // ConPTY asks for the cursor position first; it was answered.
    #[cfg(windows)]
    assert!(t.queries >= 1, "no query answered");
}

#[test]
fn typed_input_reaches_the_shell() {
    #[cfg(windows)]
    let o = opts(&cmd(), &[]);
    #[cfg(not(windows))]
    let o = opts("/bin/sh", &["-i"]);
    let session = Session::spawn(o, || {});
    // Wait for a prompt before typing.
    let (text, _) = wait_for(&session, 20, |text, _| {
        text.contains('>') || text.contains('$')
    });
    assert!(
        text.contains('>') || text.contains('$'),
        "no prompt: {text:?}"
    );
    session.write(b"echo typed-marker-42\r".to_vec());
    let (text, _) = wait_for(&session, 20, |text, _| {
        text.matches("typed-marker-42").count() >= 2
    });
    // Once echoed as typed, once as output.
    assert!(text.matches("typed-marker-42").count() >= 2, "{text:?}");
    // Resizing does not break the session.
    session.resize(30, 120);
    assert_eq!(session.term().screen.size(), (30, 120));
    session.write(b"exit 3\r".to_vec());
    let (_, status) = wait_for(&session, 20, |_, s| !s.is_alive());
    assert!(
        matches!(status, Status::Exited { code: 3, .. }),
        "{status:?}"
    );
}

#[test]
fn kill_ends_a_running_shell() {
    #[cfg(windows)]
    let o = opts(&cmd(), &[]);
    #[cfg(not(windows))]
    let o = opts("/bin/sh", &["-i"]);
    let session = Session::spawn(o, || {});
    let (_, status) = wait_for(&session, 20, |_, s| matches!(s, Status::Running { .. }));
    assert!(matches!(status, Status::Running { .. }), "{status:?}");
    session.kill();
    let (_, status) = wait_for(&session, 20, |_, s| !s.is_alive());
    assert!(matches!(status, Status::Exited { .. }), "{status:?}");
}

#[test]
fn a_missing_program_fails_clearly() {
    let session = Session::spawn(opts("definitely-not-a-shell-xyz.exe", &[]), || {});
    let (_, status) = wait_for(&session, 20, |_, s| !s.is_alive());
    match status {
        Status::Failed(e) => assert!(e.contains("definitely-not-a-shell-xyz"), "{e}"),
        other => panic!("expected a failure, got {other:?}"),
    }
}
