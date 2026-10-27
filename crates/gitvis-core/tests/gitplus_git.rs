//! Git extras against throwaway repositories built with the real git
//! binary: stashing selected files, the stash list, patches round trip,
//! submodules from a local bare repository, Git LFS (`git lfs install
//! --local` only), the pre-commit secret check and splitting a commit.

use std::path::{Path, PathBuf};
use std::process::Command;

use gitvis_core::devops::gitplus::submodules::SubState;
use gitvis_core::devops::gitplus::{githooks, lfs, patches, secret_hook, split, stash, submodules};

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn temp_root(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("gitvis-gp-{name}-{}-{nanos}", std::process::id()))
}

fn configure(dir: &Path) {
    for (k, v) in [
        ("user.name", "Test User"),
        ("user.email", "test@example.com"),
        ("commit.gpgsign", "false"),
        ("tag.gpgsign", "false"),
        ("core.autocrlf", "false"),
    ] {
        run_git(dir, &["config", k, v]);
    }
}

fn run_git(dir: &Path, args: &[&str]) -> String {
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

impl Fixture {
    fn new(name: &str) -> Fixture {
        let root = temp_root(name);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main"]);
        configure(&f.work);
        f
    }

    fn git(&self, args: &[&str]) -> String {
        run_git(&self.work, args)
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
}

#[test]
fn stash_selected_files_and_the_list() {
    let f = Fixture::new("stash");
    f.commit("a.txt", "a\n", "one");
    f.commit("b file.txt", "b\n", "two");
    f.write("a.txt", "a\nchanged\n");
    f.write("b file.txt", "b\nchanged\n");
    f.write("new.txt", "untracked\n");
    let mut cmds = Vec::new();
    // Stash only "b file.txt" and the untracked file.
    let args = stash::push_args(
        "partial",
        false,
        true,
        &["b file.txt".to_owned(), "new.txt".to_owned()],
    );
    gitvis_core::devops::gitplus::git_ok(&f.work, &args, &mut cmds).unwrap();
    let status = f.git(&["status", "--porcelain"]);
    assert_eq!(status, "M a.txt", "only a.txt is left changed: {status}");
    // A second stash of everything else.
    gitvis_core::devops::gitplus::git_ok(
        &f.work,
        &stash::push_args("", false, false, &[]),
        &mut cmds,
    )
    .unwrap();
    let list = stash::load(&f.work, &mut cmds).unwrap();
    assert_eq!(list.len(), 2);
    assert!(list[0].wip, "no message: git's WIP text");
    assert_eq!(list[0].branch.as_deref(), Some("main"));
    let partial = &list[1];
    assert_eq!(partial.message, "partial");
    assert!(partial.has_untracked);
    let files = partial.files.as_ref().unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["b file.txt", "new.txt"]);
    assert!(files[1].untracked && !files[0].untracked);
    assert_eq!(partial.added(), 2);
    // Its patch, file by file.
    let patch = stash::patch(&f.work, 1, &mut cmds).unwrap();
    assert_eq!(patch.len(), 2);
    assert_eq!(patch[0].path, "b file.txt");
    assert_eq!(patch[1].path, "new.txt");
    assert!(cmds.iter().any(|c| c.starts_with("git stash list")));
    // Apply checks the sha first: a wrong one is refused.
    let err = stash::run_checked(
        &f.work,
        1,
        &list[0].sha,
        &stash::apply_args(1, false),
        &mut cmds,
    )
    .unwrap_err();
    assert!(err.contains("no longer"), "{err}");
    stash::run_checked(
        &f.work,
        1,
        &partial.sha,
        &stash::pop_args(1, false),
        &mut cmds,
    )
    .unwrap();
    assert!(f.work.join("new.txt").exists());
    assert_eq!(stash::load(&f.work, &mut cmds).unwrap().len(), 1);
    // Branch from the remaining stash.
    stash::valid_branch_name(&f.work, "from-stash", &mut cmds).unwrap();
    assert!(stash::valid_branch_name(&f.work, "bad name..", &mut cmds).is_err());
    f.git(&["stash", "push", "--include-untracked", "-m", "keep"]);
    let list = stash::load(&f.work, &mut cmds).unwrap();
    stash::run_checked(
        &f.work,
        0,
        &list[0].sha,
        &stash::branch_args("from-stash", 0),
        &mut cmds,
    )
    .unwrap();
    assert_eq!(f.git(&["branch", "--show-current"]), "from-stash");
}

#[test]
fn patches_round_trip() {
    let f = Fixture::new("patch");
    f.commit("a.txt", "a\n", "one");
    let base = f.git(&["rev-parse", "HEAD"]);
    f.commit("a.txt", "a\nb\n", "two: extend a");
    f.write("dir/new file.txt", "n\n");
    f.git(&["add", "-A"]);
    f.git(&[
        "commit",
        "-q",
        "-m",
        "three: add a file",
        "-m",
        "With a body.",
    ]);
    let out = f.root.join("out dir");
    let mut cmds = Vec::new();
    let source = patches::Source::Range(format!("{base}..HEAD"));
    let commits = patches::range_commits(&f.work, &source, &mut cmds).unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].subject, "two: extend a");
    let created = patches::create(&f.work, &out, &source, &mut cmds).unwrap();
    assert_eq!(created.len(), 2);
    assert!(created.iter().all(|p| p.exists()), "{created:?}");
    let last = patches::create(
        &f.work,
        &f.root.join("last"),
        &patches::Source::Last(1),
        &mut cmds,
    )
    .unwrap();
    assert_eq!(last.len(), 1);

    // A clone at the base receives them.
    let other = f.root.join("other");
    run_git(&f.root, &["clone", "-q", f.work.to_str().unwrap(), "other"]);
    configure(&other);
    run_git(&other, &["reset", "-q", "--hard", &base]);
    let p1 = patches::preview(&other, &created[0], &mut cmds).unwrap();
    assert!(p1.is_mailbox());
    assert_eq!(p1.mails[0].subject, "two: extend a");
    assert_eq!(p1.mails[0].number.as_deref(), Some("1/2"));
    assert_eq!(p1.files[0].path, "a.txt");
    assert_eq!((p1.added(), p1.deleted()), (1, 0));
    assert_eq!(p1.applies, patches::Applies::Cleanly);
    // The second one needs the first.
    let p2 = patches::preview(&other, &created[1], &mut cmds).unwrap();
    assert!(p2.summary.iter().any(|s| s.contains("create mode")));
    assert_eq!(p2.files[0].path, "dir/new file.txt");
    gitvis_core::devops::gitplus::git_ok(&other, &patches::am_args(&created, true), &mut cmds)
        .unwrap();
    assert_eq!(
        run_git(&other, &["log", "--format=%s", "-2"]),
        "three: add a file\ntwo: extend a"
    );
    assert_eq!(
        run_git(&other, &["log", "-1", "--format=%b"]),
        "With a body."
    );

    // A plain diff applies with git apply.
    let third = f.root.join("third");
    run_git(&f.root, &["clone", "-q", f.work.to_str().unwrap(), "third"]);
    configure(&third);
    run_git(&third, &["reset", "-q", "--hard", &base]);
    let diff = f.git(&["diff", &base, "HEAD"]);
    let diff_file = f.root.join("changes.diff");
    std::fs::write(&diff_file, format!("{diff}\n")).unwrap();
    let pd = patches::preview(&third, &diff_file, &mut cmds).unwrap();
    assert!(!pd.is_mailbox());
    assert_eq!(pd.files.len(), 2);
    gitvis_core::devops::gitplus::git_ok(
        &third,
        &patches::apply_args(std::slice::from_ref(&diff_file), true),
        &mut cmds,
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(third.join("a.txt")).unwrap(),
        "a\nb\n"
    );
    // Applying it again does not go in.
    let again = patches::preview(&third, &diff_file, &mut cmds).unwrap();
    assert!(!matches!(again.applies, patches::Applies::Cleanly));
}

#[test]
fn submodules_from_a_local_bare_repository() {
    let root = temp_root("sub");
    let src = root.join("lib-src");
    std::fs::create_dir_all(&src).unwrap();
    run_git(&src, &["init", "-q", "-b", "main"]);
    configure(&src);
    std::fs::write(src.join("lib.txt"), "lib\n").unwrap();
    run_git(&src, &["add", "."]);
    run_git(&src, &["commit", "-q", "-m", "lib one"]);
    run_git(&root, &["clone", "-q", "--bare", "lib-src", "lib.git"]);
    let sup = root.join("super");
    std::fs::create_dir_all(&sup).unwrap();
    run_git(&sup, &["init", "-q", "-b", "main"]);
    configure(&sup);
    std::fs::write(sup.join("top.txt"), "top\n").unwrap();
    run_git(&sup, &["add", "."]);
    run_git(&sup, &["commit", "-q", "-m", "top"]);
    let add = submodules::add_args("../lib.git", "libs/lib", "main");
    let mut args = vec!["-c", "protocol.file.allow=always"];
    args.extend(add.iter().map(String::as_str));
    run_git(&sup, &args);
    let add2 = submodules::add_args("../lib.git", "other dir/two", "");
    let mut args = vec!["-c", "protocol.file.allow=always"];
    args.extend(add2.iter().map(String::as_str));
    run_git(&sup, &args);
    run_git(&sup, &["commit", "-q", "-m", "add submodules"]);

    let mut cmds = Vec::new();
    let o = submodules::load(&sup, &mut cmds).unwrap();
    assert!(o.has_gitmodules);
    assert_eq!(o.modules.len(), 2, "{o:#?}");
    let lib = o.modules.iter().find(|m| m.path == "libs/lib").unwrap();
    assert_eq!(lib.state, SubState::Current);
    assert_eq!(lib.url, "../lib.git");
    assert_eq!(lib.branch.as_deref(), Some("main"));
    assert_eq!(lib.recorded, lib.checked_out);
    assert_eq!(lib.head_subject.as_deref(), Some("lib one"));
    assert_eq!(lib.ahead_behind, Some((0, 0)));
    assert!(lib.dirty.is_empty());

    // A local commit and a dirty file inside the second one.
    let two = sup.join("other dir/two");
    configure(&two);
    std::fs::write(two.join("x.txt"), "x\n").unwrap();
    run_git(&two, &["add", "x.txt"]);
    run_git(&two, &["commit", "-q", "-m", "local change"]);
    std::fs::write(two.join("lib.txt"), "edited\n").unwrap();
    let o = submodules::load(&sup, &mut cmds).unwrap();
    let t = o
        .modules
        .iter()
        .find(|m| m.path == "other dir/two")
        .unwrap();
    assert_eq!(t.state, SubState::Modified);
    assert_ne!(t.recorded, t.checked_out);
    assert_eq!(t.dirty.len(), 1);
    assert_eq!(t.head_subject.as_deref(), Some("local change"));
    assert!(
        cmds.iter()
            .any(|c| c.starts_with("git -C \"other dir/two\" status"))
    );

    // A fresh clone has them uninitialised; update clones them.
    run_git(&root, &["clone", "-q", "super", "clone"]);
    let clone = root.join("clone");
    let o = submodules::load(&clone, &mut cmds).unwrap();
    assert!(
        o.modules
            .iter()
            .all(|m| m.state == SubState::NotInitialised)
    );
    assert!(o.modules.iter().all(|m| m.checked_out.is_none()));
    let update = submodules::update_args(&["libs/lib".to_owned()], false);
    let mut args = vec!["-c", "protocol.file.allow=always"];
    args.extend(update.iter().map(String::as_str));
    run_git(&clone, &args);
    let o = submodules::load(&clone, &mut cmds).unwrap();
    let lib = o.modules.iter().find(|m| m.path == "libs/lib").unwrap();
    assert_eq!(lib.state, SubState::Current);
    assert!(lib.configured_url.is_some());

    // Sync and init are local.
    submodules::run(&clone, &submodules::sync_args(&[]), &mut cmds).unwrap();
    submodules::run(&clone, &submodules::init_args(&[]), &mut cmds).unwrap();

    // Remove one completely.
    let msg = submodules::remove(&sup, "libs/lib", "libs/lib", &mut cmds).unwrap();
    assert!(msg.contains("Removed"));
    assert!(!sup.join("libs/lib").exists());
    assert!(!sup.join(".git/modules/libs/lib").exists());
    let o = submodules::load(&sup, &mut cmds).unwrap();
    assert_eq!(o.modules.len(), 1);
    assert!(submodules::remove(&sup, "../x", "x", &mut cmds).is_err());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn lfs_patterns_files_and_status() {
    let f = Fixture::new("lfs");
    let mut cmds = Vec::new();
    let o = lfs::load(&f.work, &mut cmds);
    if !o.installed() {
        eprintln!("git lfs is not installed: skipped ({})", o.missing_reason);
        return;
    }
    gitvis_core::devops::gitplus::git_ok(&f.work, &lfs::install_local_args(), &mut cmds).unwrap();
    gitvis_core::devops::gitplus::git_ok(&f.work, &lfs::track_args("*.bin", false), &mut cmds)
        .unwrap();
    gitvis_core::devops::gitplus::git_ok(&f.work, &lfs::track_args("art/*.psd", true), &mut cmds)
        .unwrap();
    f.write("big.bin", &"x".repeat(3000));
    f.write("art/a.psd", "psd");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "lfs files"]);
    f.write("two.bin", "two");
    f.git(&["add", "two.bin"]);
    let o = lfs::load(&f.work, &mut cmds);
    assert!(
        o.hooks_installed,
        "git lfs install --local writes the hooks"
    );
    assert!(o.filter_configured);
    let pats: Vec<&str> = o.patterns.iter().map(|p| p.pattern.as_str()).collect();
    assert_eq!(pats, vec!["*.bin", "art/*.psd"]);
    assert!(o.patterns[1].lockable);
    // The index lists the staged two.bin too.
    assert_eq!(o.files.len(), 3);
    let big = o.files.iter().find(|x| x.path == "big.bin").unwrap();
    assert_eq!(big.size, Some(3000));
    assert!(big.downloaded);
    assert!(
        o.status
            .iter()
            .any(|s| s.entries.iter().any(|(p, _)| p == "two.bin")),
        "{:?}",
        o.status
    );
    gitvis_core::devops::gitplus::git_ok(&f.work, &lfs::untrack_args("art/*.psd"), &mut cmds)
        .unwrap();
    let o = lfs::load(&f.work, &mut cmds);
    assert_eq!(o.patterns.len(), 1);
}

fn fake_token() -> String {
    // Built at run time so this file holds no token-shaped string.
    format!("ghp_{}{}", "z1Yx2Wv3Ut4Sr5Qp6On7Ml8", "Kj9Ih0Hg1Fe2Dc3Ba4")
}

#[test]
fn pre_commit_secret_check_exit_codes() {
    let f = Fixture::new("hook");
    f.commit("README.md", "hello\n", "init");
    // Clean staged change: exit 0.
    f.write("README.md", "hello\nworld\n");
    f.git(&["add", "README.md"]);
    let (text, code) = secret_hook::run(&f.work);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("no secrets in 1 staged file"), "{text}");
    // A staged fake token: exit 1, printed redacted.
    let token = fake_token();
    f.write("config/app.env", &format!("TOKEN={token}\n"));
    f.git(&["add", "config/app.env"]);
    let (text, code) = secret_hook::run(&f.work);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("config/app.env:1"), "{text}");
    assert!(text.contains("GitHub token"), "{text}");
    assert!(!text.contains(&token), "the secret itself is never printed");
    assert!(text.contains("--no-verify"));
    // Unstaged secrets do not count.
    f.git(&["reset", "-q", "config/app.env"]);
    let (_, code) = secret_hook::run(&f.work);
    assert_eq!(code, 0);
    // Dismissed in gitvis: not blocking.
    f.git(&["add", "config/app.env"]);
    let scan = secret_hook::scan_staged(&f.work).unwrap();
    let fp: Vec<String> = scan
        .findings
        .iter()
        .map(|x| x.fingerprint.clone())
        .collect();
    let common = secret_hook::common_dir(&f.work).unwrap();
    secret_hook::write_dismissed(&common, &fp).unwrap();
    let (text, code) = secret_hook::run(&f.work);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("dismissed in gitvis"));
    let written = std::fs::read_to_string(common.join(secret_hook::DISMISSED_FILE)).unwrap();
    assert!(!written.contains(&token));
}

#[test]
fn hooks_folder_follows_core_hooks_path() {
    let f = Fixture::new("hooksdir");
    let mut cmds = Vec::new();
    let (dir, custom) = githooks::hooks_dir(&f.work, &mut cmds).unwrap();
    assert!(dir.ends_with("hooks"), "{}", dir.display());
    assert!(custom.is_none() || custom.as_deref() == Some(""));
    f.git(&["config", "core.hooksPath", "my hooks"]);
    let (dir, custom) = githooks::hooks_dir(&f.work, &mut cmds).unwrap();
    assert_eq!(custom.as_deref(), Some("my hooks"));
    assert_eq!(dir, f.work.join("my hooks"));
    // Installing the check there works, and git runs it.
    let plan = githooks::plan_install(&dir, Path::new("C:/nowhere/gitvis.exe")).unwrap();
    githooks::install(&plan).unwrap();
    let listed = githooks::list(&dir).unwrap();
    assert!(listed.iter().any(|h| h.gitvis && h.enabled));
    // The exe is missing: the script says so and lets the commit through.
    f.write("a.txt", "a\n");
    f.git(&["add", "a.txt"]);
    let out = Command::new("git")
        .args(["commit", "-q", "-m", "with hook"])
        .current_dir(&f.work)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("is missing"));
}

#[test]
fn split_head_and_an_older_commit() {
    let f = Fixture::new("split");
    f.commit("a.txt", "a\n", "one");
    f.write("a.txt", "a\nb\n");
    f.write("c.txt", "c\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "two things"]);
    let older = f.git(&["rev-parse", "HEAD"]);
    f.commit("d.txt", "d\n", "three");
    let mut cmds = Vec::new();
    // HEAD: reset, changes unstaged.
    let p = split::plan(&f.work, "head", &mut cmds).unwrap();
    assert!(p.head);
    assert_eq!(p.subject, "three");
    split::execute(&f.work, &p, &mut cmds).unwrap();
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "two things");
    assert_eq!(f.git(&["status", "--porcelain"]), "?? d.txt");
    f.git(&["add", "d.txt"]);
    f.git(&["commit", "-q", "-m", "three"]);
    // Older: refused while dirty.
    f.write("a.txt", "dirty\n");
    assert!(split::plan(&f.work, &older, &mut cmds).is_err());
    f.git(&["checkout", "--", "a.txt"]);
    let p = split::plan(&f.work, &older, &mut cmds).unwrap();
    assert!(!p.head);
    assert_eq!(p.later, 1);
    assert!(p.commands().iter().any(|c| c.contains("rebase -i")));
    split::execute(&f.work, &p, &mut cmds).unwrap();
    let state = f.git(&["rev-parse", "--git-path", "rebase-merge"]);
    assert!(f.work.join(state).is_dir(), "mid-rebase");
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "one");
    let status = f.git(&["status", "--porcelain"]);
    assert!(
        status.contains(" M a.txt") || status.contains("M a.txt"),
        "{status}"
    );
    assert!(status.contains("?? c.txt"), "{status}");
    // Commit in two parts, then continue.
    f.git(&["add", "a.txt"]);
    f.git(&["commit", "-q", "-m", "part one"]);
    f.git(&["add", "c.txt"]);
    f.git(&["commit", "-q", "-m", "part two"]);
    f.git(&["-c", "core.editor=true", "rebase", "--continue"]);
    assert_eq!(
        f.git(&["log", "--format=%s"]),
        "three\npart two\npart one\none"
    );
    // The first commit and merges are refused.
    let first = f.git(&["rev-list", "--max-parents=0", "HEAD"]);
    assert!(
        split::plan(&f.work, &first, &mut cmds)
            .unwrap_err()
            .contains("first commit")
    );
}
