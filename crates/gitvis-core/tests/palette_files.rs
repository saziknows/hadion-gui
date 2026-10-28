//! The palette's file scope against a throwaway repository built with the
//! real git binary: `git ls-files -z` keeps unusual names intact, and the
//! ranking finds them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use gitvis_core::devops::palette::sources::file_items;
use gitvis_core::devops::palette::{Query, Ranker, Scope, files};

fn temp_repo() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "gitvis-palette-files-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    for (k, v) in [
        ("user.name", "Test User"),
        ("user.email", "test@example.com"),
        ("commit.gpgsign", "false"),
        ("core.quotepath", "true"),
    ] {
        git(&["config", k, v]);
    }
    for p in [
        "src/main.rs",
        "src/view/file history.rs",
        "docs/déjà vu.md",
        "maintenance/x.rs",
        "README.md",
    ] {
        let path = dir.join(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "x\n").unwrap();
    }
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "files"]);
    // Untracked files are not listed.
    std::fs::write(dir.join("untracked.txt"), "x\n").unwrap();
    dir
}

#[test]
fn lists_tracked_files_with_their_real_names() {
    let dir = temp_repo();
    let mut commands = Vec::new();
    let mut paths = files::list(&dir, &mut commands).expect("ls-files");
    paths.sort();
    assert_eq!(
        paths,
        [
            "README.md",
            "docs/déjà vu.md",
            "maintenance/x.rs",
            "src/main.rs",
            "src/view/file history.rs",
        ]
    );
    assert_eq!(commands, [files::COMMAND]);

    let items = file_items(&paths);
    let src = [(items.as_slice(), 1u64)];
    let mut r = Ranker::new();
    let top = |r: &mut Ranker, q: &str| {
        let res = r.rank(&src, Scope::Files, &Query::new(q), &HashMap::new(), 10);
        res.hits
            .first()
            .map(|h| items[h.index as usize].title.clone())
            .unwrap_or_default()
    };
    assert_eq!(top(&mut r, "main"), "src/main.rs");
    assert_eq!(top(&mut r, "fh"), "src/view/file history.rs");
    assert_eq!(top(&mut r, "deja"), "");
    assert_eq!(top(&mut r, "déjà"), "docs/déjà vu.md");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn outside_a_repository_the_error_is_returned() {
    let dir = std::env::temp_dir().join(format!("gitvis-palette-none-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut commands = Vec::new();
    let err = files::list(&dir, &mut commands).unwrap_err();
    assert!(!err.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
