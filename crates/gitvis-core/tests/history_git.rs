//! History browsing against throwaway repositories built with the real git
//! binary: compare (two-dot, three-dot, working tree, renames), filter
//! queries, the file tree and contents, blame with the previous version,
//! multi-pick commands (cherry-pick, format-patch, rebase marks) and SSH
//! signature verification. Every config value is repo-local.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

use gitvis_core::devops::history::compare::{self, Mode, Request};
use gitvis_core::devops::history::numstat::Change;
use gitvis_core::devops::history::signature::{self, Kind, Status};
use gitvis_core::devops::history::tree::{self, Content};
use gitvis_core::devops::history::{blame, filter, multi};
use gitvis_core::devops::workbench::rebase::{self, Plan, Step};

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
            std::env::temp_dir().join(format!("gitvis-hist-{name}-{}-{nanos}", std::process::id()));
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

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.work)
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

    fn write(&self, rel: &str, content: &[u8]) {
        let p = self.work.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    fn commit(&self, rel: &str, content: &str, message: &str) -> String {
        self.write(rel, content.as_bytes());
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn dir(f: &Fixture) -> &Path {
    &f.work
}

/// main: root, a, b; feature (from a): rename + edit, new file.
fn branches() -> (Fixture, String) {
    let f = Fixture::new("cmp");
    let long: String = (0..40).map(|i| format!("line {i}\n")).collect();
    let root = f.commit("src/old name.rs", &long, "root");
    f.commit("README.md", "hello\n", "add readme");
    f.git(&["checkout", "-q", "-b", "feature"]);
    f.git(&["mv", "src/old name.rs", "src/new name.rs"]);
    let edited = long.replace("line 3\n", "line three\n");
    f.write("src/new name.rs", edited.as_bytes());
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "rename and edit"]);
    f.commit("docs/guide.md", "guide\n", "add guide");
    f.git(&["checkout", "-q", "main"]);
    f.commit("README.md", "hello\nworld\n", "main moves on");
    (f, root)
}

#[test]
fn compare_two_and_three_dots() {
    let (f, root) = branches();
    let mut cmds = Vec::new();
    let two = compare::run(
        dir(&f),
        &Request {
            a: "main".into(),
            b: Some("feature".into()),
            mode: Mode::TwoDot,
        },
        &mut cmds,
    )
    .unwrap();
    assert_eq!((two.count_a, two.count_b), (1, 2));
    assert_eq!(two.only_b.len(), 2);
    assert_eq!(two.only_b[0].subject, "add guide", "newest first");
    assert_eq!(two.only_a[0].subject, "main moves on");
    let base = two.merge_base.as_ref().unwrap();
    assert_eq!(base.subject, "add readme");
    // Two dots: README differs too (main changed it after the fork).
    let paths: Vec<&str> = two.files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"README.md"), "{paths:?}");
    let renamed = two
        .files
        .iter()
        .find(|f| f.path == "src/new name.rs")
        .unwrap();
    assert!(matches!(renamed.change, Change::Renamed(s) if s > 50));
    assert_eq!(renamed.old_path.as_deref(), Some("src/old name.rs"));
    assert_eq!((renamed.added, renamed.deleted), (1, 1));
    assert!(
        cmds.iter()
            .any(|c| c.contains("rev-list --left-right --count"))
    );

    let three = compare::run(
        dir(&f),
        &Request {
            a: "main".into(),
            b: Some("feature".into()),
            mode: Mode::ThreeDot,
        },
        &mut cmds,
    )
    .unwrap();
    let paths: Vec<&str> = three.files.iter().map(|f| f.path.as_str()).collect();
    assert!(!paths.contains(&"README.md"), "only B's changes: {paths:?}");
    assert!(paths.contains(&"docs/guide.md"));

    // The file diff of the rename (both names passed to git diff).
    let d = compare::file_diff(dir(&f), &two, renamed, &mut cmds).unwrap();
    assert_eq!(d.stats(), (1, 1));

    // Bad revisions explain themselves.
    let e = compare::run(
        dir(&f),
        &Request {
            a: "nope".into(),
            b: Some("main".into()),
            mode: Mode::TwoDot,
        },
        &mut cmds,
    )
    .unwrap_err();
    assert!(e.contains("not a branch, tag or commit"), "{e}");

    // Against the working tree.
    f.write("README.md", b"hello\nworld\nlocal\n");
    let wt = compare::run(
        dir(&f),
        &Request {
            a: root[..10].to_owned(),
            b: None,
            mode: Mode::TwoDot,
        },
        &mut cmds,
    )
    .unwrap();
    assert!(wt.b.is_none());
    assert_eq!(wt.count_b, 2, "root..HEAD: two commits on main");
    let readme = wt.files.iter().find(|f| f.path == "README.md").unwrap();
    assert_eq!(readme.change, Change::Added);
    assert_eq!(readme.added, 3, "includes the uncommitted line");
}

#[test]
fn filter_queries() {
    let (f, _) = branches();
    let cancel = AtomicBool::new(false);
    let mut cmds = Vec::new();
    let by_path = filter::query(
        dir(&f),
        &filter::Spec {
            path: "docs".into(),
            ..Default::default()
        },
        filter::MAX_IDS,
        &cancel,
        &mut cmds,
    )
    .unwrap();
    assert_eq!(by_path.ids.len(), 1, "only the guide commit touched docs/");
    let by_msg = filter::query(
        dir(&f),
        &filter::Spec {
            message: "README".into(),
            ..Default::default()
        },
        filter::MAX_IDS,
        &cancel,
        &mut cmds,
    )
    .unwrap();
    assert_eq!(by_msg.ids.len(), 1, "case-insensitive: add readme");
    let capped = filter::query(
        dir(&f),
        &filter::Spec {
            path: "src".into(),
            ..Default::default()
        },
        1,
        &cancel,
        &mut cmds,
    )
    .unwrap();
    assert!(capped.truncated);
    assert_eq!(capped.ids.len(), 1);
    let cancelled = AtomicBool::new(true);
    assert!(
        filter::query(
            dir(&f),
            &filter::Spec {
                path: "src".into(),
                ..Default::default()
            },
            10,
            &cancelled,
            &mut cmds
        )
        .is_err()
    );
}

#[test]
fn tree_and_contents() {
    let (f, _) = branches();
    f.write("bin/logo.png", &[0x89, b'P', b'N', b'G', 0, 0, 1, 2]);
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "binary"]);
    let mut cmds = Vec::new();
    let t = tree::load(dir(&f), "HEAD", &mut cmds).unwrap();
    assert_eq!(t.totals().1, 3, "readme, src file, logo");
    let n = t.find("src/old name.rs").unwrap();
    let e = &t.entries[t.nodes[n].entry.unwrap()];
    match tree::content(dir(&f), &t.sha, e, &mut cmds).unwrap() {
        Content::Text(b) => assert!(String::from_utf8_lossy(&b).starts_with("line 0")),
        other => panic!("{other:?}"),
    }
    let n = t.find("bin/logo.png").unwrap();
    let e = &t.entries[t.nodes[n].entry.unwrap()];
    assert!(matches!(
        tree::content(dir(&f), &t.sha, e, &mut cmds).unwrap(),
        Content::Binary { .. }
    ));
    let feature = tree::load(dir(&f), "feature", &mut cmds).unwrap();
    assert!(feature.find("docs/guide.md").is_some());
    assert!(tree::load(dir(&f), "missing-branch", &mut cmds).is_err());
}

#[test]
fn blame_and_previous_version() {
    let f = Fixture::new("blame");
    f.commit("a.txt", "one\ntwo\nthree\n", "first");
    let second = f.commit("a.txt", "one\nTWO\nthree\nfour\n", "second");
    let cancel = AtomicBool::new(false);
    let mut cmds = Vec::new();
    let b = blame::run(dir(&f), "HEAD", "a.txt", &cancel, &mut cmds).unwrap();
    assert_eq!(b.lines.len(), 4);
    let line2 = &b.lines[1];
    let c = &b.commits[line2.commit as usize];
    assert_eq!(c.sha, second);
    assert_eq!(c.summary, "second");
    let (prev, prev_path) = c.previous.clone().unwrap();
    assert_eq!(prev_path, "a.txt");
    // Blame the previous version, at the same position.
    let older = blame::run(dir(&f), &prev, &prev_path, &cancel, &mut cmds).unwrap();
    assert_eq!(older.lines.len(), 3);
    assert_eq!(older.lines[line2.orig as usize - 1].text, "two");
    assert!(older.commits.iter().all(|c| c.summary == "first"));
    let e = blame::run(dir(&f), "HEAD", "missing.txt", &cancel, &mut cmds).unwrap_err();
    assert!(e.contains("missing.txt"), "{e}");
}

#[test]
fn picked_commits_cherry_pick_patches_and_marks() {
    let f = Fixture::new("multi");
    let base = f.commit("base.txt", "base\n", "base");
    f.git(&["checkout", "-q", "-b", "topic"]);
    let c1 = f.commit("t1.txt", "1\n", "topic one");
    f.commit("t2.txt", "2\n", "topic two");
    let c3 = f.commit("t3.txt", "3\n", "topic three");
    f.git(&["checkout", "-q", "main"]);

    // Totals.
    let mut cmds = Vec::new();
    let t = multi::totals(dir(&f), &[c1.clone(), c3.clone()], &mut cmds).unwrap();
    assert_eq!((t.commits, t.files, t.added), (2, 2, 2));

    // One cherry-pick of two non-adjacent commits, oldest first.
    let args = multi::cherry_pick_args(&[c1.clone(), c3.clone()]);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    f.git(&refs);
    assert_eq!(
        f.git(&["log", "--format=%s", "-2"]),
        "topic three\ntopic one"
    );

    // A numbered patch series.
    let out = f.root.join("patches");
    std::fs::create_dir_all(&out).unwrap();
    for a in multi::format_patch_commands(&out, &[c1.clone(), c3.clone()]) {
        let refs: Vec<&str> = a.iter().map(String::as_str).collect();
        f.git(&refs);
    }
    let mut names: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names.len(), 2);
    assert!(names[0].starts_with("0001-topic-one"), "{names:?}");
    assert!(names[1].starts_with("0002-topic-three"), "{names:?}");

    // Rebase marks on a real plan: squash 1 and 3 together.
    f.git(&["checkout", "-q", "topic"]);
    let loaded = rebase::load(dir(&f), &format!("{c1}^"), &mut cmds).unwrap();
    assert_eq!(loaded.base_sha, base);
    let mut plan = Plan::new(loaded.commits);
    let marks = multi::rebase_marks(&[c1, c3], multi::RebaseAction::Squash);
    assert!(multi::apply_marks(&mut plan, &marks).is_empty());
    let subjects: Vec<&str> = plan
        .rows
        .iter()
        .map(|r| r.commit.subject.as_str())
        .collect();
    assert_eq!(subjects, vec!["topic one", "topic three", "topic two"]);
    assert_eq!(plan.rows[1].step, Step::Squash);
}

fn have_ssh_keygen() -> bool {
    Command::new("ssh-keygen")
        .arg("-?")
        .output()
        .map(|_| true)
        .unwrap_or(false)
}

#[test]
fn ssh_signatures() {
    if !have_ssh_keygen() {
        eprintln!("ssh-keygen not found; skipping");
        return;
    }
    let f = Fixture::new("sig");
    let key = f.root.join("key");
    let out = Command::new("ssh-keygen")
        .args([
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            "test@example.com",
            "-f",
        ])
        .arg(&key)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let unsigned = f.commit("a.txt", "a\n", "unsigned");
    let pubkey = key.with_extension("pub");
    // Repo-local configuration only.
    f.git(&["config", "gpg.format", "ssh"]);
    f.git(&["config", "user.signingkey", pubkey.to_str().unwrap()]);
    f.git(&["config", "commit.gpgsign", "true"]);
    let signed = f.commit("b.txt", "b\n", "signed");
    let mut cmds = Vec::new();

    // Nothing configured here: git reads it as unsigned (%G? = N) and
    // complains on stderr; the commit object says it is SSH-signed. (A
    // global allowedSignersFile of the user's would make it U instead.)
    let global = Command::new("git")
        .args(["config", "--global", "--get", "gpg.ssh.allowedSignersFile"])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    let v = signature::verify(
        dir(&f),
        &[unsigned.clone(), signed.clone()],
        false,
        &mut cmds,
    )
    .unwrap();
    assert_eq!(v.sigs[0].1.status, Status::Unsigned);
    assert_eq!(v.sigs[1].1.kind, Some(Kind::Ssh));
    if !global {
        assert_eq!(v.sigs[1].1.status, Status::CantCheck);
        assert!(v.stderr.contains("allowedSignersFile"), "{}", v.stderr);
    }

    // Configured, but the file is missing: a good signature nobody vouches
    // for (U), and the same advice.
    let missing = f.root.join("no-such-allowed-signers");
    f.git(&[
        "config",
        "gpg.ssh.allowedSignersFile",
        missing.to_str().unwrap(),
    ]);
    let v = signature::verify(dir(&f), std::slice::from_ref(&signed), false, &mut cmds).unwrap();
    assert_eq!(v.sigs[0].1.status, Status::Untrusted);
    let v = signature::verify(
        dir(&f),
        &[unsigned.clone(), signed.clone()],
        false,
        &mut cmds,
    )
    .unwrap();
    let setup = signature::setup(dir(&f), &mut cmds);
    assert_eq!(setup.format, "ssh");
    assert!(!setup.allowed_signers_exists);
    let advice = signature::advise(&v.sigs[1].1, &setup, "test@example.com").unwrap();
    assert!(advice.title.contains("allowed-signers"));

    // With the key allowed for this e-mail: verified.
    let allowed = f.root.join("allowed_signers");
    let pk = std::fs::read_to_string(&pubkey).unwrap();
    std::fs::write(&allowed, format!("test@example.com {}", pk.trim())).unwrap();
    f.git(&[
        "config",
        "gpg.ssh.allowedSignersFile",
        allowed.to_str().unwrap(),
    ]);
    let v = signature::verify(dir(&f), std::slice::from_ref(&signed), true, &mut cmds).unwrap();
    let s = &v.sigs[0].1;
    assert_eq!(s.status, Status::Good, "{s:?}");
    assert_eq!(s.signer, "test@example.com");
    assert!(s.key.starts_with("SHA256:"));
    assert!(s.message.contains("Good"), "{}", s.message);
    assert!(signature::advise(s, &signature::setup(dir(&f), &mut cmds), "").is_none());

    // The file lists only someone else's key: good but untrusted.
    let other = f.root.join("other");
    let out = Command::new("ssh-keygen")
        .args([
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            "other@example.com",
            "-f",
        ])
        .arg(&other)
        .output()
        .unwrap();
    assert!(out.status.success());
    let other_pk = std::fs::read_to_string(other.with_extension("pub")).unwrap();
    std::fs::write(&allowed, format!("other@example.com {}", other_pk.trim())).unwrap();
    let v = signature::verify(dir(&f), std::slice::from_ref(&signed), false, &mut cmds).unwrap();
    let s = &v.sigs[0].1;
    assert_eq!(s.status, Status::Untrusted);
    assert_eq!(s.status.badge(), Some("Untrusted"));
    let a =
        signature::advise(s, &signature::setup(dir(&f), &mut cmds), "test@example.com").unwrap();
    assert!(
        a.title.contains("not in your allowed signers"),
        "{}",
        a.title
    );
}
