//! What the AI requests gather, against throwaway repositories built with
//! the real git binary: the staged diff (redacted, sensitive files
//! withheld), one commit, the commits of a branch with or without its base
//! in the clone.

use std::path::{Path, PathBuf};
use std::process::Command;

use hadion_core::devops::ai::inputs;
use hadion_core::devops::ai::prompts;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "hadion-ai-in-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.git(&["init", "-q", "-b", "main"]);
        for (k, v) in [
            ("user.name", "Test User"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.autocrlf", "false"),
        ] {
            f.git(&["config", k, v]);
        }
        f
    }

    fn dir(&self) -> &Path {
        &self.root
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn write(&self, path: &str, text: &str) {
        let p = self.root.join(path);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(p, text).unwrap();
    }

    fn commit(&self, path: &str, text: &str, msg: &str) {
        self.write(path, text);
        self.git(&["add", path]);
        self.git(&["commit", "-q", "-m", msg]);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn aws() -> String {
    format!("AKIA{}", "Q7ZK4WJ2MX9PLR3T")
}

#[test]
fn staged_changes_are_redacted_and_fitted() {
    let f = Fixture::new("staged");
    f.commit("src/a.rs", "fn a() {}\n", "feat: add a");
    f.commit("src/b.rs", "fn b() {}\n", "fix(b): handle b");
    f.write(
        "src/a.rs",
        "fn a() { let k = \"KEY\"; }\n"
            .replace("KEY", &aws())
            .as_str(),
    );
    f.write(".env", "TOKEN=abc\n");
    f.write("notes.txt", "not staged\n");
    f.git(&["add", "src/a.rs", ".env"]);
    let mut commands = Vec::new();
    let (input, m) = inputs::staged(f.dir(), 64 * 1024, &mut commands).unwrap();
    assert_eq!(input.branch, "main");
    assert_eq!(
        input.recent_subjects,
        vec!["fix(b): handle b", "feat: add a"]
    );
    assert!(input.files.contains("src/a.rs") && input.files.contains(".env"));
    assert!(!input.files.contains("notes.txt"));
    assert!(input.stat.contains("2 files changed"), "{}", input.stat);
    assert!(!input.diff.contains(&aws()), "{}", input.diff);
    assert!(
        input.diff.contains("[REDACTED:aws-access-key]"),
        "{}",
        input.diff
    );
    assert!(!input.diff.contains("TOKEN=abc"));
    assert_eq!(m.fitted.included, vec!["src/a.rs"]);
    assert_eq!(m.redactions.values(), 1);
    assert!(
        commands
            .iter()
            .any(|c| c.starts_with("git diff --cached -U3"))
    );
    let p = prompts::commit_message(&input);
    assert!(!p.user.contains(&aws()));
}

#[test]
fn nothing_staged_is_an_error() {
    let f = Fixture::new("empty");
    f.commit("a.txt", "a\n", "init");
    let err = inputs::staged(f.dir(), 1024, &mut Vec::new()).unwrap_err();
    assert!(err.contains("Nothing is staged"), "{err}");
}

#[test]
fn one_commit() {
    let f = Fixture::new("commit");
    f.commit("a.txt", "a\n", "init");
    f.commit("a.txt", "a\nb\n", "Add b\n\nBecause b matters.");
    let sha = f.git(&["rev-parse", "HEAD"]);
    let (input, m) = inputs::commit(f.dir(), &sha, 64 * 1024, &mut Vec::new()).unwrap();
    assert!(input.header.contains("Because b matters."));
    assert!(input.header.contains("Author: Test User"));
    assert!(input.diff.contains("+b"));
    assert_eq!(m.fitted.included, vec!["a.txt"]);
    assert!(inputs::commit(f.dir(), "not-a-sha;rm", 1024, &mut Vec::new()).is_err());
}

#[test]
fn branch_commits_with_and_without_the_base() {
    let f = Fixture::new("branch");
    f.commit("a.txt", "a\n", "init");
    f.git(&["checkout", "-q", "-b", "feature/x"]);
    f.commit("b.txt", "b\n", "Add b\n\nBody of b.");
    f.commit("c.txt", "c\n", "Add c");
    // The base exists locally.
    let mut cmds = Vec::new();
    let (base, note) =
        inputs::resolve_base(f.dir(), "main", "origin/main", "feature/x", &mut cmds).unwrap();
    assert_eq!(base, "main");
    assert_eq!(note, "main..feature/x");
    let commits = inputs::commits_between(f.dir(), &base, "feature/x", &mut cmds).unwrap();
    assert_eq!(
        commits,
        vec![
            ("Add b".to_owned(), "Body of b.".to_owned()),
            ("Add c".to_owned(), String::new())
        ]
    );
    let stat = inputs::stat_between(f.dir(), &base, "feature/x", &mut cmds).unwrap();
    assert!(stat.contains("2 files changed"), "{stat}");
    // A base that is not in this clone: the commits only on the branch.
    let (base, note) = inputs::resolve_base(f.dir(), "master", "", "feature/x", &mut cmds).unwrap();
    assert!(base.ends_with('^'), "{base}");
    assert!(note.contains("2 commits only on feature/x"), "{note}");
    let commits = inputs::commits_between(f.dir(), &base, "feature/x", &mut cmds).unwrap();
    assert_eq!(commits.len(), 2);
}
