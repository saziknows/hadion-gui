//! Workbench features against throwaway repositories built with the real
//! git binary: interactive rebase from a plan, bisect (manual and `git
//! bisect run`), worktrees and file history.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

use gitvis_core::devops::proc::run_streaming;
use gitvis_core::devops::workbench::bisect::{self, Shell, StepResult, Verdict};
use gitvis_core::devops::workbench::rebase::{self, Outcome, Plan, Step};
use gitvis_core::devops::workbench::{history, worktrees};

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("gitvis-wb-{name}-{}-{nanos}", std::process::id()));
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main"]);
        for (k, v) in [
            ("user.name", "Test User"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
            ("core.autocrlf", "false"),
        ] {
            f.git(&["config", k, v]);
        }
        f
    }

    fn git_in(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {:?} failed:\n{}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn git(&self, args: &[&str]) -> String {
        self.git_in(&self.work, args)
    }

    fn write(&self, rel: &str, content: &str) {
        let p = self.work.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    fn commit(&self, rel: &str, content: &str, message: &str) -> String {
        self.write(rel, content);
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn subjects(&self) -> Vec<String> {
        self.git(&["log", "--reverse", "--format=%s"])
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn rebase_reorder_squash_reword_drop() {
    let f = Fixture::new("rebase");
    let base = f.commit("root.txt", "root\n", "root");
    for i in 1..=5 {
        f.commit(
            &format!("f{i}.txt"),
            &format!("file {i}\n"),
            &format!("commit {i}"),
        );
    }
    // A dirty file must survive (--autostash).
    f.write("root.txt", "root\nlocal edit\n");

    let mut cmds = Vec::new();
    let loaded = rebase::load(&f.work, &base[..8], &mut cmds).unwrap();
    assert_eq!(loaded.commits.len(), 5);
    assert_eq!(loaded.commits[0].subject, "commit 1");
    assert_eq!(loaded.commits[0].files, 1);
    assert!(!loaded.not_ancestor);
    assert_eq!(loaded.merges_skipped, 0);
    assert!(
        cmds.iter()
            .any(|c| c.starts_with("git -c core.quotepath=false log --reverse"))
    );

    let mut plan = Plan::new(loaded.commits.clone());
    // Order: 1, 3, 2, 4, 5 → 2 squashed into 3 with a new message, 4
    // reworded, 5 dropped.
    plan.move_row(2, 1);
    plan.set_step(2, Step::Squash);
    *plan.squash_buffer(1).unwrap() = "Three and two\n\nCombined by the test".to_owned();
    plan.set_step(3, Step::Reword);
    plan.rows[3].message = "Four, reworded".to_owned();
    plan.set_step(4, Step::Drop);
    assert!(rebase::validate(&plan, rebase::RepoCheck::default()).is_empty());

    let work = f.root.join("rebase-work");
    let outcome = rebase::execute(
        &f.work,
        &loaded.base_sha,
        &loaded.head_sha,
        &plan,
        &work,
        &mut cmds,
    )
    .unwrap();
    assert!(matches!(outcome, Outcome::Done(_)), "{outcome:?}");
    assert!(
        !work.exists(),
        "the work directory is removed after success"
    );

    assert_eq!(
        f.subjects(),
        vec!["root", "commit 1", "Three and two", "Four, reworded"]
    );
    let combined = f.git(&["log", "-1", "--format=%B", "HEAD~1"]);
    assert_eq!(combined, "Three and two\n\nCombined by the test");
    let files = f.git(&["show", "--name-only", "--format=", "HEAD~1"]);
    let mut files: Vec<&str> = files.lines().collect();
    files.sort();
    assert_eq!(
        files,
        vec!["f2.txt", "f3.txt"],
        "squashed changes land in one commit"
    );
    for i in 1..=4 {
        assert_eq!(
            std::fs::read_to_string(f.work.join(format!("f{i}.txt"))).unwrap(),
            format!("file {i}\n")
        );
    }
    assert!(
        !f.work.join("f5.txt").exists(),
        "the dropped commit's file is gone"
    );
    assert_eq!(
        std::fs::read_to_string(f.work.join("root.txt")).unwrap(),
        "root\nlocal edit\n",
        "autostash restored the local edit"
    );
}

#[test]
fn rebase_edit_stops_and_moved_head_is_refused() {
    let f = Fixture::new("rebase-edit");
    let base = f.commit("a.txt", "a\n", "a");
    f.commit("b.txt", "b\n", "b");
    f.commit("c.txt", "c\n", "c");
    let mut cmds = Vec::new();
    let loaded = rebase::load(&f.work, &base, &mut cmds).unwrap();
    let mut plan = Plan::new(loaded.commits.clone());
    plan.set_step(0, Step::Edit);
    plan.set_step(1, Step::Reword);
    plan.rows[1].message = "c, renamed".into();

    // HEAD moves after loading: refused.
    f.commit("d.txt", "d\n", "d");
    let err = rebase::execute(
        &f.work,
        &loaded.base_sha,
        &loaded.head_sha,
        &plan,
        &f.root.join("w0"),
        &mut cmds,
    )
    .unwrap_err();
    assert!(err.contains("HEAD moved"), "{err}");
    f.git(&["reset", "-q", "--hard", "HEAD~1"]);

    let work = f.root.join("w1");
    let outcome = rebase::execute(
        &f.work,
        &loaded.base_sha,
        &loaded.head_sha,
        &plan,
        &work,
        &mut cmds,
    )
    .unwrap();
    assert!(matches!(outcome, Outcome::Stopped(_)), "{outcome:?}");
    assert!(
        work.exists(),
        "message files stay for the rest of the rebase"
    );
    let git_dir = PathBuf::from(f.git(&["rev-parse", "--absolute-git-dir"]));
    assert!(git_dir.join("rebase-merge").is_dir(), "mid-rebase");
    // What the banner's Continue runs.
    f.git(&["rebase", "--continue"]);
    assert_eq!(f.subjects(), vec!["a", "b", "c, renamed"]);
}

#[test]
fn suggestions_offer_the_last_commits() {
    let f = Fixture::new("suggest");
    for i in 0..7 {
        f.commit(&format!("x{i}.txt"), "x\n", &format!("c{i}"));
        if i == 1 {
            f.git(&["tag", "v0.1"]);
        }
    }
    let mut cmds = Vec::new();
    let s = rebase::suggestions(&f.work, &mut cmds);
    let revs: Vec<&str> = s.iter().map(|c| c.rev.as_str()).collect();
    assert!(revs.contains(&"v0.1"), "{revs:?}");
    assert!(revs.contains(&"HEAD~5"), "{revs:?}");
    let tag = s.iter().find(|c| c.rev == "v0.1").unwrap();
    assert_eq!(tag.why, "5 commits since v0.1");
}

/// 20 commits writing their number into value.txt; v1.0 is the first.
fn bisect_repo(name: &str) -> (Fixture, Vec<String>) {
    let f = Fixture::new(name);
    let mut shas = Vec::new();
    for i in 1..=20 {
        shas.push(f.commit("value.txt", &format!("{i}\n"), &format!("commit {i}")));
        if i == 1 {
            f.git(&["tag", "v1.0"]);
        }
    }
    (f, shas)
}

#[test]
fn bisect_run_finds_the_first_bad_commit() {
    let (f, shas) = bisect_repo("bisect-run");
    let mut cmds = Vec::new();
    let info = bisect::read(&f.work, &mut cmds).unwrap();
    assert!(!info.active);
    assert_eq!(info.latest_tag.as_deref(), Some("v1.0"));

    let start = bisect::start_args("HEAD", &["v1.0".into()]);
    let start: Vec<&str> = start.iter().map(String::as_str).collect();
    let out = f.git(&start);
    assert!(
        matches!(bisect::parse_step(&out), StepResult::Next { .. }),
        "{out}"
    );
    let info = bisect::read(&f.work, &mut cmds).unwrap();
    assert!(info.active);
    assert!(info.has_bad);
    assert_eq!(info.good_count, 1);
    let vars = info.vars.clone().expect("bisect vars");
    assert_eq!(vars.candidates, 19);
    assert!(vars.steps >= 3);
    assert_eq!(info.start_ref.as_deref(), Some("main"));

    // Fails from commit 13 on.
    let shell = bisect::Shell::default_for_platform();
    let cmd = match shell {
        Shell::Sh(_) => "test $(cat value.txt) -lt 13".to_owned(),
        Shell::Cmd => {
            bisect::write_cmd_script("set /p v=<value.txt\r\nif %v% GEQ 13 exit /b 1\r\nexit /b 0")
                .unwrap()
                .to_string_lossy()
                .into_owned()
        }
    };
    let spec = bisect::run_spec(&f.work, &shell, &cmd);
    let cancel = AtomicBool::new(false);
    let mut lines = Vec::new();
    let exit = run_streaming(&spec, &cancel, |_| {}, |b| lines.extend(b)).unwrap();
    let text: String = lines.iter().map(|l| format!("{}\n", l.text)).collect();
    assert!(exit.success(), "{text}");
    assert_eq!(
        bisect::first_bad_in(&text).as_deref(),
        Some(shas[12].as_str()),
        "{text}"
    );

    let info = bisect::read(&f.work, &mut cmds).unwrap();
    let (sha, subject) = info.log.first_bad.clone().expect("first bad in the log");
    assert_eq!(sha, shas[12]);
    assert_eq!(subject, "commit 13");
    let fb = info.first_bad.expect("details");
    assert_eq!(fb.subject, "commit 13");
    assert_eq!(fb.files[0].0, "value.txt");
    assert!(info.log.entries.len() >= 4);
    assert!(info.log.entries.iter().any(|e| e.verdict == Verdict::Good));
    f.git(&["bisect", "reset"]);
    assert!(!bisect::active(&f.work));
}

#[cfg(windows)]
#[test]
fn bisect_run_through_cmd() {
    let (f, shas) = bisect_repo("bisect-cmd");
    f.git(&["bisect", "start", "HEAD", "v1.0"]);
    // Quotes in the command are what cmd and git's sh disagree on.
    let script =
        bisect::write_cmd_script("set /p v=<\"value.txt\"\r\nif %v% GEQ 7 exit /b 1\r\nexit /b 0")
            .unwrap();
    let spec = bisect::run_spec(&f.work, &Shell::Cmd, &script.to_string_lossy());
    let cancel = AtomicBool::new(false);
    let mut lines = Vec::new();
    run_streaming(&spec, &cancel, |_| {}, |b| lines.extend(b)).unwrap();
    let text: String = lines.iter().map(|l| format!("{}\n", l.text)).collect();
    assert_eq!(
        bisect::first_bad_in(&text).as_deref(),
        Some(shas[6].as_str()),
        "{text}"
    );
    f.git(&["bisect", "reset"]);
}

#[test]
fn bisect_manual_steps() {
    let (f, shas) = bisect_repo("bisect-manual");
    f.git(&["bisect", "start", "HEAD", "v1.0"]);
    let terms = bisect::Terms::default();
    let mut cmds = Vec::new();
    // The culprit is commit 5: answer by reading value.txt.
    let mut found = None;
    for _ in 0..10 {
        let v: u32 = std::fs::read_to_string(f.work.join("value.txt"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let verdict = if v >= 5 { Verdict::Bad } else { Verdict::Good };
        let args = bisect::mark_args(&terms, verdict, None);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = f.git(&args);
        match bisect::parse_step(&out) {
            StepResult::FirstBad { sha } => {
                found = Some(sha);
                break;
            }
            StepResult::Next { .. } => {}
            other => panic!("unexpected {other:?}: {out}"),
        }
    }
    assert_eq!(found.as_deref(), Some(shas[4].as_str()));
    let info = bisect::read(&f.work, &mut cmds).unwrap();
    assert_eq!(info.first_bad.unwrap().subject, "commit 5");
    f.git(&["bisect", "reset"]);
}

#[test]
fn worktrees_list_lock_and_remove() {
    let f = Fixture::new("worktrees");
    f.commit("a.txt", "a\n", "a");
    let wt = f.root.join("work-feature");
    let path = wt.to_string_lossy().replace('\\', "/");
    let add = worktrees::add_args(
        &path,
        &worktrees::AddMode::NewBranch {
            name: "feature".into(),
            start: String::new(),
        },
    );
    let add: Vec<&str> = add.iter().map(String::as_str).collect();
    f.git(&add);
    std::fs::write(wt.join("a.txt"), "changed\n").unwrap();
    std::fs::write(wt.join("new.txt"), "new\n").unwrap();
    let lock = worktrees::lock_args(&path, "kept on purpose");
    let lock: Vec<&str> = lock.iter().map(String::as_str).collect();
    f.git(&lock);

    let mut cmds = Vec::new();
    let list = worktrees::list(&f.work, &mut cmds).unwrap();
    assert_eq!(list.len(), 2);
    assert!(list[0].wt.main);
    assert_eq!(list[0].wt.branch.as_deref(), Some("main"));
    let e = &list[1];
    assert_eq!(e.wt.branch.as_deref(), Some("feature"));
    assert_eq!(e.wt.locked.as_deref(), Some("kept on purpose"));
    let st = e.status.clone().unwrap().unwrap();
    assert_eq!((st.unstaged, st.untracked), (1, 1));
    assert_eq!(e.subject.as_deref(), Some("a"));
    assert!(cmds.iter().any(|c| c.starts_with("git -C ")));

    f.git(&["worktree", "unlock", &path]);
    let rm = worktrees::remove_args(&path, true);
    let rm: Vec<&str> = rm.iter().map(String::as_str).collect();
    f.git(&rm);
    assert!(!wt.exists());
    let prune = f.git(&["worktree", "prune", "--dry-run", "-v"]);
    assert!(worktrees::parse_prune(&prune).is_empty());
}

#[test]
fn file_history_follows_renames() {
    let f = Fixture::new("history");
    f.commit("src/old/file.rs", "a\nb\nc\n", "add file");
    f.commit("src/old/file.rs", "a\nb\nc\nd\n", "edit file");
    std::fs::create_dir_all(f.work.join("src/new")).unwrap();
    f.git(&["mv", "src/old/file.rs", "src/new/file.rs"]);
    f.git(&["commit", "-q", "-m", "move dir"]);
    f.git(&["mv", "src/new/file.rs", "top.rs"]);
    f.write("top.rs", "a\nb\nc\nd\ne\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "rename and edit"]);
    f.commit("other.txt", "x\n", "unrelated");

    let mut cmds = Vec::new();
    let h = history::history(&f.work, "top.rs", history::MAX_COMMITS, &mut cmds).unwrap();
    let subjects: Vec<&str> = h.commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(
        subjects,
        vec!["rename and edit", "move dir", "edit file", "add file"]
    );
    assert_eq!(
        h.commits[0].renamed_from.as_deref(),
        Some("src/new/file.rs")
    );
    assert_eq!(h.commits[1].path, "src/new/file.rs");
    assert_eq!(
        h.commits[1].renamed_from.as_deref(),
        Some("src/old/file.rs")
    );
    assert_eq!(h.commits[3].path, "src/old/file.rs");
    assert_eq!(h.commits[3].added, 3);
    assert_eq!(h.renames(), 2);
    assert_eq!(
        h.names,
        vec!["top.rs", "src/new/file.rs", "src/old/file.rs"]
    );
    assert!(!h.truncated);

    // The diff of the rename commit, both names given.
    let c = &h.commits[0];
    let args = history::show_args(&c.sha, &c.path, c.renamed_from.as_deref());
    let out = gitvis_core::cli::run(&f.work, &args, None).unwrap();
    let diffs = gitvis_core::diff::parse(&out.stdout);
    assert_eq!(diffs.len(), 1);
    assert!(!diffs[0].hunks.is_empty());

    let capped = history::history(&f.work, "top.rs", 2, &mut cmds).unwrap();
    assert_eq!(capped.commits.len(), 2);
    assert!(capped.truncated);
    let err = history::history(&f.work, "nope.rs", 10, &mut cmds).unwrap_err();
    assert!(err.contains("No commit touches"), "{err}");
    let files = history::tracked_files(&f.work, &mut cmds).unwrap();
    assert_eq!(files, vec!["other.txt", "top.rs"]);
}
