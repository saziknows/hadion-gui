//! Automation against throwaway repositories built with the real git
//! binary: group batch operations (fetch, pull with its skip rules, branch)
//! against a local bare remote, and the release commit (manifest discovery,
//! bumps, CHANGELOG.md, a commit of only those files).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use hadion_core::devops::automation::groups::{self, Batch, Outcome};
use hadion_core::devops::automation::release::{self, FileEdit};

fn root(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let r = std::env::temp_dir().join(format!("hadion-auto-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&r).unwrap();
    r
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}:\n{}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn configure(dir: &Path) {
    for (k, v) in [
        ("user.name", "Test User"),
        ("user.email", "test@example.com"),
        ("commit.gpgsign", "false"),
        ("core.autocrlf", "false"),
    ] {
        git(dir, &["config", k, v]);
    }
}

fn clone(root: &Path, bare: &Path, name: &str) -> PathBuf {
    git(
        root,
        &[
            "clone",
            "-q",
            "-c",
            "core.autocrlf=false",
            bare.to_str().unwrap(),
            name,
        ],
    );
    let d = root.join(name);
    configure(&d);
    d
}

fn commit_file(dir: &Path, file: &str, text: &str, msg: &str) {
    std::fs::write(dir.join(file), text).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", msg]);
}

#[test]
fn group_batches_fetch_pull_and_branch() {
    let root = root("groups");
    let bare = root.join("remote.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "--bare", "-q", "-b", "main"]);
    let seed = clone(&root, &bare, "seed");
    commit_file(&seed, "a.txt", "one\n", "one");
    git(&seed, &["push", "-q", "origin", "HEAD:main"]);

    let clean = clone(&root, &bare, "clean");
    let dirty = clone(&root, &bare, "dirty");
    let diverged = clone(&root, &bare, "diverged");
    // Upstream moves on.
    commit_file(&seed, "a.txt", "two\n", "two");
    git(&seed, &["push", "-q", "origin", "HEAD:main"]);
    // dirty: a tracked change; diverged: a local commit.
    std::fs::write(dirty.join("a.txt"), "local edit\n").unwrap();
    commit_file(&diverged, "b.txt", "mine\n", "local");
    let missing = root.join("not-there");

    let mut log = Vec::new();
    let list = [
        ("clean", clean.clone()),
        ("dirty", dirty.clone()),
        ("diverged", diverged.clone()),
        ("missing", missing.clone()),
    ];
    let t = Duration::from_secs(60);

    // Fetch: everyone who exists.
    let fetched: Vec<_> = list
        .iter()
        .map(|(n, d)| groups::run_one(&Batch::Fetch, n, d, t, &mut log))
        .collect();
    assert!(
        matches!(fetched[0].outcome, Outcome::Ok(_)),
        "{:?}",
        fetched[0]
    );
    assert!(matches!(fetched[3].outcome, Outcome::Skipped(ref s) if s == "folder not found"));
    // After the fetch the status knows about the new commit.
    let s = groups::read_status(&clean, &mut log).unwrap();
    assert_eq!((s.ahead, s.behind), (0, 1));
    assert_eq!(s.branch.as_deref(), Some("main"));
    assert_eq!(s.upstream.as_deref(), Some("origin/main"));
    assert!(s.last_commit.is_some());
    let d = groups::read_status(&diverged, &mut log).unwrap();
    assert!(d.diverged(), "{d:?}");

    // Pull: clean fast-forwards, dirty and diverged are skipped.
    let pulled: Vec<_> = list
        .iter()
        .map(|(n, d)| groups::run_one(&Batch::Pull, n, d, t, &mut log))
        .collect();
    match &pulled[0].outcome {
        Outcome::Ok(s) => assert!(s.starts_with("fast-forwarded"), "{s}"),
        o => panic!("clean: {o:?} {}", pulled[0].output),
    }
    assert_eq!(
        std::fs::read_to_string(clean.join("a.txt")).unwrap(),
        "two\n"
    );
    assert!(
        matches!(&pulled[1].outcome, Outcome::Skipped(s) if s.starts_with("uncommitted changes (1 changed)")),
        "{:?}",
        pulled[1].outcome
    );
    assert!(
        matches!(&pulled[2].outcome, Outcome::Skipped(s) if s.contains("diverged from origin/main: 1 ahead, 1 behind")),
        "{:?}",
        pulled[2].outcome
    );
    assert_eq!(
        groups::summary(&Batch::Pull, &pulled),
        "Pull in 4 repositories: 1 ok, 3 skipped"
    );
    // Pulling again: already up to date.
    let again = groups::run_one(&Batch::Pull, "clean", &clean, t, &mut log);
    assert_eq!(again.outcome, Outcome::Ok("already up to date".into()));

    // Branch: created at HEAD everywhere, keeping the dirty change; a second
    // time it exists already.
    let b = Batch::Branch {
        name: "release/1.2".into(),
        base: None,
    };
    let made = groups::run_one(&b, "dirty", &dirty, t, &mut log);
    assert!(
        matches!(made.outcome, Outcome::Ok(ref s) if s.starts_with("switched to new branch release/1.2")),
        "{made:?}"
    );
    assert_eq!(git(&dirty, &["branch", "--show-current"]), "release/1.2");
    assert_eq!(
        std::fs::read_to_string(dirty.join("a.txt")).unwrap(),
        "local edit\n"
    );
    let twice = groups::run_one(&b, "dirty", &dirty, t, &mut log);
    assert!(matches!(twice.outcome, Outcome::Skipped(ref s) if s.contains("exists already")));
    // From a base: unknown base is skipped, a known one works (clean tree).
    let based = Batch::Branch {
        name: "hotfix".into(),
        base: Some("origin/main".into()),
    };
    let ok = groups::run_one(&based, "clean", &clean, t, &mut log);
    assert!(matches!(ok.outcome, Outcome::Ok(_)), "{ok:?}");
    let unknown = Batch::Branch {
        name: "hotfix2".into(),
        base: Some("no-such-ref".into()),
    };
    let skip = groups::run_one(&unknown, "clean", &clean, t, &mut log);
    assert!(
        matches!(skip.outcome, Outcome::Skipped(ref s) if s == "no-such-ref is not known here")
    );
    // Every command was recorded with its folder.
    assert!(
        log.iter()
            .any(|c| c.starts_with("git pull --ff-only    (in "))
    );
    assert!(
        log.iter()
            .any(|c| c.starts_with("git status --porcelain=v2 --branch"))
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn release_commit_bumps_manifests_and_changelog() {
    let root = root("release");
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    configure(&work);
    let pkg = "{\n  \"name\": \"demo\",\n  \"version\": \"1.1.0\",\n  \"private\": true\n}\n";
    let cargo =
        "[package]\nname = \"demo\"\nversion = \"1.1.0\" # bumped by hadion\nedition = \"2024\"\n";
    let py = "[project]\nname = \"demo\"\nversion = \"1.1.0\"\n";
    let changelog = "# Changelog\n\n## [Unreleased]\n\n## [1.1.0] - 2026-09-01\n\n- First\n\n[Unreleased]: https://github.com/o/demo/compare/v1.1.0...HEAD\n[1.1.0]: https://github.com/o/demo/compare/v1.0.0...v1.1.0\n";
    std::fs::create_dir_all(work.join("node_modules/dep")).unwrap();
    for (f, c) in [
        ("package.json", pkg),
        ("Cargo.toml", cargo),
        ("pyproject.toml", py),
        ("CHANGELOG.md", changelog),
        ("other.txt", "x\n"),
    ] {
        std::fs::write(work.join(f), c).unwrap();
    }
    git(&work, &["add", "."]);
    git(&work, &["commit", "-q", "-m", "init"]);
    // An unrelated staged change must stay out of the release commit.
    std::fs::write(work.join("other.txt"), "staged but not released\n").unwrap();
    git(&work, &["add", "other.txt"]);

    let mut log = Vec::new();
    let (found, cl) = release::discover(&work, &mut log).unwrap();
    let paths: Vec<&str> = found.iter().map(|d| d.path.as_str()).collect();
    assert_eq!(paths, vec!["Cargo.toml", "package.json", "pyproject.toml"]);
    let (cl_path, cl_text) = cl.unwrap();
    assert_eq!(cl_path, "CHANGELOG.md");
    let sel = release::default_selection(&found, "v1.2.0");
    assert_eq!(sel.len(), 3);

    let mut edits: Vec<FileEdit> = found
        .iter()
        .map(|d| {
            let content = std::fs::read_to_string(work.join(&d.path)).unwrap();
            let fields: Vec<&release::Field> = d.fields.iter().collect();
            FileEdit {
                path: d.path.clone(),
                new: release::bump(&content, &fields, "1.2.0"),
                old: Some(content),
                what: "1.1.0 → 1.2.0".into(),
            }
        })
        .collect();
    let ce =
        release::changelog_insert(&cl_text, "1.2.0", "v1.2.0", "2026-10-27", "- Groups", false)
            .unwrap();
    edits.push(FileEdit {
        path: cl_path,
        old: Some(cl_text),
        new: ce.content,
        what: "section".into(),
    });
    let msg = release::commit_message("chore(release): v{version}", "1.2.0", "v1.2.0");
    let out = release::apply(&work, &edits, &msg, &mut log).unwrap();
    assert!(out.ends_with("chore(release): v1.2.0"), "{out}");
    // Exactly the four files, formatting kept.
    let files = git(&work, &["show", "--name-only", "--format=", "HEAD"]);
    let mut files: Vec<&str> = files.lines().collect();
    files.sort();
    assert_eq!(
        files,
        vec![
            "CHANGELOG.md",
            "Cargo.toml",
            "package.json",
            "pyproject.toml"
        ]
    );
    assert_eq!(
        std::fs::read_to_string(work.join("Cargo.toml")).unwrap(),
        cargo.replace("\"1.1.0\"", "\"1.2.0\"")
    );
    assert_eq!(
        std::fs::read_to_string(work.join("package.json")).unwrap(),
        pkg.replace("1.1.0", "1.2.0")
    );
    let cl_now = std::fs::read_to_string(work.join("CHANGELOG.md")).unwrap();
    assert!(cl_now.contains("## [1.2.0] - 2026-10-27\n\n- Groups\n\n## [1.1.0]"));
    assert!(cl_now.contains("[Unreleased]: https://github.com/o/demo/compare/v1.2.0...HEAD"));
    // The unrelated change is still staged, not committed.
    assert_eq!(
        git(&work, &["diff", "--cached", "--name-only"]),
        "other.txt"
    );

    // A file changed after the preview: refused, nothing written.
    let stale = vec![FileEdit {
        path: "package.json".into(),
        old: Some(pkg.to_owned()),
        new: "{}".into(),
        what: String::new(),
    }];
    let e = release::apply(&work, &stale, "x", &mut log).unwrap_err();
    assert!(e.to_string().contains("changed since the preview"), "{e}");
    assert!(
        std::fs::read_to_string(work.join("package.json"))
            .unwrap()
            .contains("1.2.0")
    );
    let escape = vec![FileEdit {
        path: "../outside.txt".into(),
        old: None,
        new: "x".into(),
        what: String::new(),
    }];
    assert!(release::apply(&work, &escape, "x", &mut log).is_err());
    let _ = std::fs::remove_dir_all(&root);
}
