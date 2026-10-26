//! End-to-end checks against a repository built with the real git binary.

use std::path::{Path, PathBuf};
use std::process::Command;

use gitvis_core::ops::GitOp;
use gitvis_core::{
    ChangeKind, DiffOptions, DiffTarget, GraphFilter, Head, LoadOptions, RefKind, repo,
};

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("gitvis-test-{}-{nanos}", std::process::id()));
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
        std::fs::write(self.work.join(rel), content).unwrap();
    }

    fn commit(&self, rel: &str, content: &str, message: &str) -> String {
        self.write(rel, content);
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

#[test]
fn loads_a_realistic_repository() {
    let f = Fixture::new();
    let _a = f.commit("a.txt", "a\n", "A: first");
    f.git(&["switch", "-q", "-c", "feature"]);
    let b = f.commit("b.txt", "b\n", "B: feature work");
    f.git(&["switch", "-q", "main"]);
    let c = f.commit("a.txt", "a\nc\n", "C: main work");
    f.git(&[
        "merge",
        "-q",
        "--no-ff",
        "feature",
        "-m",
        "D: merge feature",
    ]);
    let d = f.git(&["rev-parse", "HEAD"]);
    f.git(&["tag", "v1.0"]);
    f.git(&["tag", "-a", "v1.10", "-m", "release"]);
    f.git(&["switch", "-q", "-c", "topic"]);
    let e = f.commit("e.txt", "e\n", "E: topic");
    f.git(&["switch", "-q", "main"]);

    // A remote with everything so far, then one more local commit on main.
    let remote = f.root.join("remote.git");
    f.git_in(
        &f.root,
        &[
            "clone",
            "-q",
            "--bare",
            f.work.to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    );
    f.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    f.git(&["fetch", "-q", "origin"]);
    f.git(&["branch", "-q", "--set-upstream-to=origin/main", "main"]);
    let fcommit = f.commit("f.txt", "f\n", "F: ahead of origin");

    // A stash, then a dirty working tree.
    f.write("stashme.txt", "x\n");
    f.git(&["add", "stashme.txt"]);
    f.git(&["stash", "push", "-q", "-m", "wip stash"]);
    f.write("a.txt", "a\nc\nchange\n");
    f.write("untracked.txt", "new\n");
    f.write("s.txt", "staged\n");
    f.git(&["add", "s.txt"]);

    let snap = repo::load(&f.work, &LoadOptions::default()).expect("load");

    assert_eq!(snap.name, "work");
    assert!(
        matches!(&snap.head, Head::Branch { name, target } if name == "main" && target.to_string() == fcommit)
    );
    assert_eq!(snap.commits.len(), 6, "A B C D E F");
    assert_eq!(snap.commits[0].summary, "F: ahead of origin");
    assert!(snap.wip_row);
    assert_eq!(snap.graph.rows.len(), 7);
    assert!(snap.graph.width >= 2);
    assert!(!snap.truncated);

    // Working tree
    let st = &snap.status;
    assert_eq!(st.staged.len(), 1);
    assert_eq!(
        (st.staged[0].path.as_str(), st.staged[0].kind),
        ("s.txt", ChangeKind::Added)
    );
    let kinds: Vec<(&str, ChangeKind)> = st
        .unstaged
        .iter()
        .map(|e| (e.path.as_str(), e.kind))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("a.txt", ChangeKind::Modified),
            ("untracked.txt", ChangeKind::Untracked)
        ]
    );
    assert!(st.conflicted.is_empty());

    // Branches
    let main = snap.branch("main").unwrap();
    assert!(main.is_head);
    assert_eq!(main.upstream.as_deref(), Some("origin/main"));
    assert_eq!((main.ahead, main.behind), (1, 0));
    assert!(snap.branch("feature").unwrap().upstream.is_none());
    assert_eq!(
        snap.branches
            .iter()
            .map(|b| b.name.as_str())
            .collect::<Vec<_>>(),
        ["feature", "main", "topic"]
    );

    // Remotes
    assert_eq!(snap.remotes.len(), 1);
    let origin = &snap.remotes[0];
    assert_eq!(origin.name, "origin");
    let names: Vec<&str> = origin.branches.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["origin/feature", "origin/main", "origin/topic"]);
    assert_eq!(origin.branches[1].branch, "main");
    assert_eq!(
        snap.remote_branch("origin/main")
            .unwrap()
            .target
            .to_string(),
        e_parent_main(&d, &e)
    );

    // Tags, newest-looking first, annotated tag peeled to the commit.
    assert_eq!(
        snap.tags
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["v1.10", "v1.0"]
    );
    assert!(snap.tags.iter().all(|t| t.target.to_string() == d));

    // Stashes
    assert_eq!(snap.stashes.len(), 1);
    assert!(snap.stashes[0].message.contains("wip stash"));

    // Labels on the merge commit
    let d_oid = git2::Oid::from_str(&d).unwrap();
    let labels = &snap.refs[&d_oid];
    let mut label_names: Vec<&str> = labels.iter().map(|l| l.name.as_str()).collect();
    label_names.sort();
    assert_eq!(label_names, ["origin/main", "v1.0", "v1.10"]);
    assert!(labels.iter().any(|l| l.kind == RefKind::Remote));
    let head_labels = &snap.refs[&snap.commits[0].id];
    assert!(head_labels.iter().any(|l| l.is_head && l.name == "main"));

    // Graph: WIP row sits on HEAD's lane and points at it.
    let head_row = snap.row_of_commit(snap.index_of[&snap.commits[0].id]);
    assert_eq!(head_row, 1);
    assert_eq!(snap.graph.rows[0].bottom.len(), 1);
    assert!(snap.graph.rows[0].bottom[0].dashed);
    // The merge commit has two lines going down.
    let d_row = snap.row_of_commit(snap.index_of[&d_oid]);
    assert_eq!(snap.graph.rows[d_row].bottom.len(), 2);

    // Commit details for the merge: compared against the first parent (C).
    let details = repo::commit_details(&f.work, d_oid).unwrap();
    assert_eq!(details.parents.len(), 2);
    assert_eq!(details.parents[0].to_string(), c);
    assert_eq!(details.parents[1].to_string(), b);
    assert_eq!(details.files.len(), 1);
    assert_eq!(
        (details.files[0].path.as_str(), details.files[0].kind),
        ("b.txt", ChangeKind::Added)
    );
    assert_eq!(
        (details.files[0].additions, details.files[0].deletions),
        (1, 0)
    );
    assert_eq!(details.message, "D: merge feature");
    assert_eq!(details.signature, None, "commit.gpgsign is off");
    assert_eq!(
        details.tree.to_string(),
        f.git(&["rev-parse", &format!("{d}^{{tree}}")])
    );
    // D is on main (and topic, which branched after it) but not on feature.
    let containing = repo::branches_containing(&f.work, d_oid).unwrap();
    assert!(containing.iter().any(|b| b == "main"), "{containing:?}");
    assert!(containing.iter().any(|b| b == "topic"), "{containing:?}");
    assert!(
        containing.iter().any(|b| b == "origin/main"),
        "{containing:?}"
    );
    assert!(!containing.iter().any(|b| b == "feature"), "{containing:?}");

    // Diffs
    let unstaged = gitvis_core::load_diff(
        &f.work,
        &DiffTarget::Unstaged {
            path: "a.txt".into(),
        },
        &DiffOptions::default(),
    )
    .unwrap();
    assert_eq!(unstaged.stats(), (1, 0));
    assert!(unstaged.can_apply_hunks());
    let untracked = gitvis_core::load_diff(
        &f.work,
        &DiffTarget::Untracked {
            path: "untracked.txt".into(),
        },
        &DiffOptions::default(),
    )
    .unwrap();
    assert!(untracked.synthetic);
    assert_eq!(untracked.stats(), (1, 0));
    let staged = gitvis_core::load_diff(
        &f.work,
        &DiffTarget::Staged {
            path: "s.txt".into(),
            old_path: None,
        },
        &DiffOptions::default(),
    )
    .unwrap();
    assert_eq!(staged.stats(), (1, 0));

    // Stage the single unstaged hunk through `git apply --cached`.
    let patch = unstaged.hunk_patch(0).unwrap();
    GitOp::ApplyPatch {
        patch,
        cached: true,
        reverse: false,
    }
    .run(&f.work)
    .expect("apply hunk");
    let (tree, pending) = repo::load_status(&f.work).unwrap();
    assert_eq!(pending, gitvis_core::Pending::None);
    assert!(
        tree.staged
            .iter()
            .any(|e| e.path == "a.txt" && e.kind == ChangeKind::Modified)
    );
    assert!(!tree.unstaged.iter().any(|e| e.path == "a.txt"));

    // Commit through the op layer, then hide a branch and remotes.
    GitOp::Commit {
        message: "G: staged via gitvis".into(),
        amend: false,
    }
    .run(&f.work)
    .expect("commit");
    let opts = LoadOptions {
        filter: GraphFilter {
            hidden: vec!["refs/heads/topic".into()],
            hide_remotes: true,
            hide_tags: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let snap = repo::load(&f.work, &opts).unwrap();
    assert_eq!(snap.commits[0].summary, "G: staged via gitvis");
    assert_eq!(snap.commits.len(), 6, "E on topic is hidden");
    assert!(snap.commits.iter().all(|c| c.summary != "E: topic"));
    assert!(snap.wip_row, "untracked file is still there");
    assert_eq!(snap.branch("main").unwrap().ahead, 2);
}

/// `origin/main` was cloned while main pointed at the merge commit.
fn e_parent_main(d: &str, _e: &str) -> String {
    d.to_owned()
}

#[test]
fn truncation_is_reported() {
    let f = Fixture::new();
    for i in 0..5 {
        f.commit("n.txt", &format!("{i}\n"), &format!("commit {i}"));
    }
    let opts = LoadOptions {
        max_commits: 3,
        ..Default::default()
    };
    let snap = repo::load(&f.work, &opts).unwrap();
    assert_eq!(snap.commits.len(), 3);
    assert!(snap.truncated);
    // The oldest shown commit has a parent outside the list: its lane ends.
    let last = snap.graph.rows.last().unwrap();
    assert!(last.bottom.is_empty());
}

#[test]
fn empty_repository_has_unborn_head() {
    let f = Fixture::new();
    let snap = repo::load(&f.work, &LoadOptions::default()).unwrap();
    assert_eq!(snap.head, Head::Unborn("main".into()));
    assert!(snap.commits.is_empty());
    assert!(!snap.wip_row);
    f.write("x.txt", "x\n");
    let snap = repo::load(&f.work, &LoadOptions::default()).unwrap();
    assert!(snap.wip_row);
    assert_eq!(snap.graph.rows.len(), 1);
    assert!(snap.graph.rows[0].bottom.is_empty());
}

#[test]
fn merge_conflict_is_detected_and_resolvable() {
    let f = Fixture::new();
    f.commit("c.txt", "base\n", "base");
    f.git(&["switch", "-q", "-c", "other"]);
    f.commit("c.txt", "theirs\n", "theirs");
    f.git(&["switch", "-q", "main"]);
    f.commit("c.txt", "ours\n", "ours");

    let err = GitOp::Merge {
        from: "other".into(),
        mode: Default::default(),
    }
    .run(&f.work)
    .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("conflict"), "{err}");

    let snap = repo::load(&f.work, &LoadOptions::default()).unwrap();
    assert_eq!(snap.pending, gitvis_core::Pending::Merge);
    assert_eq!(snap.status.conflicted.len(), 1);
    assert_eq!(snap.status.conflicted[0].kind, ChangeKind::Conflicted);

    let diff = gitvis_core::load_diff(
        &f.work,
        &DiffTarget::Conflicted {
            path: "c.txt".into(),
        },
        &DiffOptions::default(),
    )
    .unwrap();
    assert!(diff.combined);
    assert!(!diff.can_apply_hunks());

    GitOp::TakeTheirs {
        path: "c.txt".into(),
    }
    .run(&f.work)
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(f.work.join("c.txt")).unwrap(),
        "theirs\n"
    );
    GitOp::Continue(gitvis_core::Pending::Merge)
        .run(&f.work)
        .unwrap();
    let snap = repo::load(&f.work, &LoadOptions::default()).unwrap();
    assert_eq!(snap.pending, gitvis_core::Pending::None);
    assert!(snap.status.is_clean());
    assert_eq!(snap.commits[0].parent_ids.len(), 2);
}

#[test]
fn selected_lines_round_trip_through_git_apply() {
    let f = Fixture::new();
    f.commit("t.txt", "a\nb\nc\n", "base");
    f.write("t.txt", "a\nb1\nb\nc\nd\n");
    let opts = DiffOptions::default();
    let target = DiffTarget::Unstaged {
        path: "t.txt".into(),
    };
    let diff = gitvis_core::load_diff(&f.work, &target, &opts).unwrap();
    assert_eq!(diff.stats(), (2, 0));
    // Stage only the first added line ("b1"), leaving "d" unstaged.
    let h = &diff.hunks[0];
    let first_added = h
        .lines
        .iter()
        .position(|l| l.kind == gitvis_core::diff::LineKind::Added)
        .unwrap();
    let patch = diff.selected_lines_patch(0, &[first_added], false).unwrap();
    GitOp::ApplyPatch {
        patch,
        cached: true,
        reverse: false,
    }
    .run(&f.work)
    .expect("apply selected lines");
    let staged = gitvis_core::load_diff(
        &f.work,
        &DiffTarget::Staged {
            path: "t.txt".into(),
            old_path: None,
        },
        &opts,
    )
    .unwrap();
    assert_eq!(staged.stats(), (1, 0));
    let unstaged = gitvis_core::load_diff(&f.work, &target, &opts).unwrap();
    assert_eq!(unstaged.stats(), (1, 0));
    assert_eq!(
        unstaged.hunks[0]
            .lines
            .iter()
            .find(|l| l.kind == gitvis_core::diff::LineKind::Added)
            .unwrap()
            .text,
        "d"
    );

    // Unstage that line again through a reverse selection patch.
    let added = staged.hunks[0]
        .lines
        .iter()
        .position(|l| l.kind == gitvis_core::diff::LineKind::Added)
        .unwrap();
    let patch = staged.selected_lines_patch(0, &[added], true).unwrap();
    GitOp::ApplyPatch {
        patch,
        cached: true,
        reverse: true,
    }
    .run(&f.work)
    .expect("unstage selected lines");
    let (tree, _) = repo::load_status(&f.work).unwrap();
    assert!(tree.staged.is_empty());

    // Whitespace-insensitive diffs are read-only.
    f.write("t.txt", "a\nb \nc\n");
    let ws = gitvis_core::load_diff(
        &f.work,
        &target,
        &DiffOptions {
            ignore_whitespace: true,
            context: 1,
        },
    )
    .unwrap();
    assert!(ws.hunks.is_empty() || ws.approximate);
    assert!(!ws.can_apply_hunks());
}

#[test]
fn secrets_in_tree_history_and_config() {
    use gitvis_core::secrets::{self, HistoryScope, Place, ScanOptions, Severity};
    use std::sync::atomic::AtomicBool;

    let f = Fixture::new();
    let token = "ghp_z1Yx2Wv3Ut4Sr5Qp6On7Ml8Kj9Ih0Hg1Fe2Dc3Ba4";
    f.commit("README.md", "hello\n", "init");
    // A token committed in .env, then removed again: history only.
    let leak = f.commit(
        ".env",
        &format!("GITHUB_TOKEN={token}\nPORT=8080\n"),
        "add env",
    );
    f.git(&["rm", "-q", ".env"]);
    f.git(&["commit", "-q", "-m", "remove env"]);
    // A private key still tracked at HEAD.
    std::fs::create_dir_all(f.work.join("deploy")).unwrap();
    f.commit(
        "deploy/id_rsa",
        "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW\n-----END OPENSSH PRIVATE KEY-----\n",
        "add deploy key",
    );
    // An untracked key file, and an untracked harmless file.
    f.write("server.key", "not really a key\n");
    f.write("notes.txt", "nothing here\n");
    // A remote URL with an embedded password.
    f.git(&[
        "remote",
        "add",
        "leaky",
        "https://bob:Sup3rS3cret@example.com/x.git",
    ]);
    // A staged secret in a source file.
    f.write(
        "src.py",
        "AWS_SECRET_ACCESS_KEY = 'wJalrXUtnFEMI/K7MDENG/bPxRfiCYzQ9vL2mNpQ'\n",
    );
    f.git(&["add", "src.py"]);

    // Quick scan: what is about to be committed.
    let quick = secrets::scan_changes(&f.work).expect("quick scan");
    let staged: Vec<&str> = quick.staged().map(|s| s.rule).collect();
    assert_eq!(staged, ["aws-secret-key"], "{quick:#?}");
    assert!(
        quick
            .files
            .iter()
            .any(|x| x.path == "server.key" && x.untracked && !x.tracked)
    );
    assert!(quick.files.iter().all(|x| x.path != "notes.txt"));

    // Full scan with history.
    let cancel = AtomicBool::new(false);
    let mut phases = Vec::new();
    let report = secrets::scan(
        &f.work,
        &ScanOptions {
            worktree: true,
            history: Some(HistoryScope {
                max_commits: 0,
                all_refs: false,
            }),
        },
        &cancel,
        &mut |p| phases.push(p.phase.clone()),
    )
    .expect("scan");
    assert!(phases.iter().any(|p| p == "Scanning history"), "{phases:?}");
    // 4 commits, but `--diff-filter=AM` skips the deletion-only one.
    assert_eq!(report.stats.commits, 3);
    assert_eq!(report.stats.commits_total, Some(4));
    assert!(!report.stats.cancelled);

    let gh = report
        .findings
        .iter()
        .find(|x| x.rule == "github-token")
        .expect("token found in history");
    assert_eq!(gh.secret, token);
    assert_eq!(gh.severity, Severity::Critical);
    assert!(gh.history_only(), "removed from HEAD: {gh:#?}");
    assert!(!gh.at_head && !gh.in_worktree);
    assert_eq!(gh.first_commit().unwrap().id.to_string(), leak);
    assert_eq!(gh.first_commit().unwrap().author, "Test User");
    assert_eq!(gh.commit_count, 1);
    assert!(
        gh.occurrences
            .iter()
            .all(|o| o.place == Place::History && o.path == ".env" && o.line == 1)
    );
    assert!(!gh.pushed, "no remote-tracking branch has it");

    let key = report
        .findings
        .iter()
        .find(|x| x.rule == "private-key")
        .expect("private key found");
    assert!(key.at_head, "{key:#?}");
    assert!(key.in_worktree);
    assert!(key.occurrences.iter().any(|o| o.place == Place::Head));
    assert_eq!(
        key.first_commit().map(|c| c.summary.as_str()),
        Some("add deploy key")
    );

    let aws = report
        .findings
        .iter()
        .find(|x| x.rule == "aws-secret-key")
        .expect("staged secret found in the working tree");
    assert!(aws.in_worktree && !aws.at_head && aws.commits.is_empty());
    assert!(aws.uncommitted_only());

    let id_rsa = report
        .files
        .iter()
        .find(|x| x.path == "deploy/id_rsa")
        .expect("id_rsa flagged");
    assert_eq!(id_rsa.rule.id, "ssh-private-key");
    assert!(id_rsa.tracked);
    assert_eq!(id_rsa.secrets, 1, "the key inside is counted");
    assert_eq!(
        id_rsa.added_in.as_ref().map(|c| c.summary.as_str()),
        Some("add deploy key")
    );
    let env = report
        .files
        .iter()
        .find(|x| x.path == ".env")
        .expect(".env from history flagged");
    assert!(!env.tracked && !env.untracked && env.added_in.is_some());
    assert!(
        report
            .files
            .iter()
            .any(|x| x.path == "server.key" && x.untracked)
    );

    let remote = report
        .config
        .iter()
        .find(|c| c.id == "remote-url-credentials")
        .expect("leaky remote flagged");
    assert!(remote.title.contains("leaky"));
    assert!(
        !remote.detail.contains("Sup3rS3cret"),
        "password is redacted"
    );
    assert!(
        report.commands.iter().all(|c| !c.contains(token)),
        "secrets never hit a command line"
    );

    // Fix the remote, stop tracking the key, ignore the untracked one.
    GitOp::SetRemoteUrl {
        name: "leaky".into(),
        url: "https://bob@example.com/x.git".into(),
    }
    .run(&f.work)
    .unwrap();
    assert_eq!(
        f.git(&["remote", "get-url", "leaky"]),
        "https://bob@example.com/x.git"
    );
    GitOp::Untrack {
        paths: vec!["deploy/id_rsa".into()],
    }
    .run(&f.work)
    .unwrap();
    assert!(f.work.join("deploy/id_rsa").exists(), "file stays on disk");
    f.write(".gitignore", "server.key\n");
    let again = secrets::scan(
        &f.work,
        &ScanOptions {
            worktree: true,
            history: None,
        },
        &cancel,
        &mut |_| {},
    )
    .unwrap();
    assert!(
        again
            .config
            .iter()
            .all(|c| c.id != "remote-url-credentials")
    );
    assert!(
        again.files.iter().all(|x| x.path != "server.key"),
        "ignored files are not listed"
    );
    let id_rsa = again
        .files
        .iter()
        .find(|x| x.path == "deploy/id_rsa")
        .unwrap();
    assert!(
        !id_rsa.tracked && id_rsa.untracked,
        "untracked now, still on disk"
    );
}

#[test]
fn osint_people_footprint_and_leftovers() {
    use gitvis_core::osint::{self, EmailKind, OsintOptions};
    use std::sync::atomic::AtomicBool;

    let f = Fixture::new();
    f.commit("README.md", "hello\n", "init");
    // A second identity with a personal address and an alias, at a fixed time zone.
    f.git(&["config", "user.email", "ann.dev@gmail.com"]);
    f.git(&["config", "user.name", "Ann Dev"]);
    std::fs::create_dir_all(f.work.join("config")).unwrap();
    std::fs::create_dir_all(f.work.join("src")).unwrap();
    f.commit(
        "config/prod.yml",
        "db_host: 10.20.30.40\nbucket: s3://acme-prod-backups\nowner: maria.lopez@acmecorp.io\nrole: arn:aws:iam::734019283745:role/deploy\n",
        "Add prod config\n\nCo-authored-by: Bob <bob@acmecorp.io>\nFixes ACME-123",
    );
    f.git(&["config", "user.email", "ann@acmecorp.io"]);
    f.commit(
        "src/main.rs",
        "fn main() {}\n",
        "Remove leaked AWS key from config",
    );
    // A deleted sensitive file, a stash with a secret, a backup branch, an amended commit.
    f.commit("deploy.key", "not a real key\n", "add key");
    f.git(&["rm", "-q", "deploy.key"]);
    f.git(&["commit", "-q", "-m", "drop key"]);
    f.write(
        "notes.txt",
        "GITHUB_TOKEN=ghp_z1Yx2Wv3Ut4Sr5Qp6On7Ml8Kj9Ih0Hg1Fe2Dc3Ba4\n",
    );
    f.git(&["add", "notes.txt"]);
    f.git(&["stash", "push", "-q", "-m", "wip notes"]);
    f.git(&["branch", "backup/old-main"]);
    f.commit("a.txt", "a\n", "will be amended");
    f.git(&["commit", "-q", "--amend", "-m", "amended"]);
    // A junk file and a git-pinned dependency.
    f.write(".DS_Store", "junk\n");
    f.git(&["add", "-f", ".DS_Store"]);
    f.commit(
        "package.json",
        r#"{"dependencies":{"left-pad":"^1.0.0","thing":"git+https://github.com/x/thing.git"}}"#,
        "deps",
    );

    let cancel = AtomicBool::new(false);
    let report = osint::analyze(
        &f.work,
        &OsintOptions {
            max_commits: 0,
            all_refs: true,
        },
        &cancel,
        &mut |_| {},
    )
    .expect("analyze");

    // People.
    let p = &report.people;
    assert!(p.total_commits >= 7, "{}", p.total_commits);
    let ann = p
        .identities
        .iter()
        .find(|i| i.email == "ann.dev@gmail.com")
        .expect("gmail identity");
    assert_eq!(ann.kind(), EmailKind::Personal);
    assert_eq!(ann.name(), "Ann Dev");
    assert!(
        ann.aliases.contains(&"ann@acmecorp.io".to_owned()),
        "{:?}",
        ann.aliases
    );
    assert_eq!(ann.commits, 1);
    assert!(ann.heat.iter().flatten().sum::<u32>() == 1);
    let test_user = p
        .identities
        .iter()
        .find(|i| i.email == "test@example.com")
        .expect("fixture identity");
    assert_eq!(
        test_user.kind(),
        EmailKind::Invalid,
        "example.com is not a mailbox"
    );
    assert_eq!(p.local_email, "ann@acmecorp.io");
    assert_eq!(p.local_scope, "repository");

    // Messages.
    let m = &report.messages;
    assert!(
        m.trailers
            .iter()
            .any(|t| t.kind == "Co-authored-by" && t.total == 1)
    );
    assert!(
        m.tickets.is_empty(),
        "single ticket mention is below the threshold"
    );
    assert!(
        m.incidents
            .iter()
            .any(|i| i.commit.summary.contains("leaked AWS key")),
        "{:?}",
        m.incidents
    );

    // Footprint.
    let rules: Vec<&str> = report.footprint.iter().map(|f| f.rule).collect();
    for want in ["private-ip", "s3-bucket", "email-address", "aws-account"] {
        assert!(rules.contains(&want), "{want} missing in {rules:?}");
    }
    let ip = report
        .footprint
        .iter()
        .find(|f| f.rule == "private-ip")
        .unwrap();
    assert_eq!(ip.secret, "10.20.30.40");
    assert_eq!(ip.occurrences[0].path, "config/prod.yml");

    // Junk.
    assert!(report.junk.iter().any(|j| j.path == ".DS_Store"));

    // Archaeology.
    let a = &report.archaeology;
    assert!(
        a.rewritten_total >= 1,
        "the pre-amend commit is only in the reflog"
    );
    assert!(a.rewritten.iter().any(|c| c.summary == "will be amended"));
    assert_eq!(a.stashes.len(), 1);
    assert_eq!(a.stashes[0].findings, 1, "the token inside the stash");
    assert!(a.stash_findings.iter().any(|s| s.rule == "github-token"));
    assert!(
        a.backup_branches
            .iter()
            .any(|b| b.name == "backup/old-main")
    );
    let deleted = a
        .deleted_files
        .iter()
        .find(|d| d.path == "deploy.key")
        .expect("deleted key file");
    assert_eq!(deleted.rule.id, "key-file");
    assert_eq!(deleted.commit.summary, "drop key");
    assert!(deleted.resurrect().starts_with("git show "));

    // Supply chain.
    assert_eq!(report.supply.git_deps.len(), 1);
    assert_eq!(report.supply.git_deps[0].name, "thing");

    // Unreachable objects after the amend.
    let u = osint::unreachable(&f.work, &cancel, &mut |_| {}).expect("fsck");
    assert!(u.commit_count + u.blobs >= 1, "{u:?}");
}

#[test]
fn reflog_and_undo() {
    let f = Fixture::new();
    f.commit("r.txt", "1\n", "first");
    f.commit("r.txt", "2\n", "second");
    let entries = gitvis_core::reflog::read(&f.work, 10).unwrap();
    assert!(entries.len() >= 2);
    assert!(entries[0].message.starts_with("commit: second"));
    let undo = gitvis_core::reflog::undo_for(&entries).unwrap();
    assert_eq!(undo.label, "Undo commit");
    for op in undo.ops {
        op.run(&f.work).unwrap();
    }
    let snap = repo::load(&f.work, &LoadOptions::default()).unwrap();
    assert_eq!(snap.commits.len(), 1);
    assert_eq!(
        snap.status.staged.len(),
        1,
        "changes of the undone commit stay staged"
    );
}

#[test]
fn refs_search_and_recovery() {
    use std::sync::atomic::AtomicBool;

    use gitvis_core::insights::{InsightsOptions, recover, refs, search};

    let f = Fixture::new();
    f.commit("a.txt", "a\n", "init: first");
    f.git(&["tag", "-a", "v1.0.0", "-m", "release 1.0.0"]);
    f.commit("a.txt", "a\nb\n", "feat: second");
    f.git(&["tag", "-a", "v1.1.0", "-m", "release 1.1.0"]);
    // Unreleased work on main: feat, fix, and two non-conventional ones
    // (the cherry-pick and the amended commit below).
    f.commit("lib.rs", "fn needle_marker() {}\n", "feat: add needle");

    // A branch whose commit is cherry-picked onto main (duplicate). Main
    // moves first so the copy gets another parent and therefore another
    // id (otherwise the identical object would make it a fast-forward).
    f.git(&["switch", "-q", "-c", "dup", "main"]);
    let picked = f.commit("dup.txt", "d\n", "Pick me");
    f.commit("dup2.txt", "e\n", "Not picked");
    f.git(&["switch", "-q", "main"]);
    f.commit("a.txt", "a\nb\nc\n", "fix: a typo");
    f.git(&["cherry-pick", &picked]);
    // A feature branch that touches a.txt like main did (conflict).
    f.git(&["switch", "-q", "-c", "feature", "v1.1.0"]);
    f.commit("a.txt", "a\nb\nfeature\n", "Feature: change a");
    f.commit("feature.txt", "f\n", "Feature: own file");
    f.git(&["switch", "-q", "main"]);
    // A throwaway branch with its own commit.
    f.git(&["switch", "-q", "-c", "tmp-old", "v1.0.0"]);
    f.commit("tmp.txt", "t\n", "tmp work");
    f.git(&["switch", "-q", "main"]);
    // Lost work: an amended commit and a dropped stash.
    f.commit("amend.txt", "1\n", "will be amended");
    f.write("amend.txt", "2\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "--amend", "-m", "amended version"]);
    f.write("stashme.txt", "s\n");
    f.git(&["add", "stashme.txt"]);
    f.git(&["stash", "push", "-q", "-m", "dropped stash"]);
    f.git(&["stash", "drop", "-q"]);

    let cancel = AtomicBool::new(false);
    let mut commands = Vec::new();
    let r = refs::analyze(&f.work, &InsightsOptions::default(), &cancel, &mut commands);
    assert!(!commands.is_empty());

    // Branches.
    let b = &r.branches;
    assert_eq!(b.head.as_deref(), Some("main"));
    assert_eq!(
        b.local.len(),
        4,
        "{:?}",
        b.local.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
    assert!(b.local[0].is_head, "HEAD first");
    assert!(b.remote.is_empty());
    let feature = b
        .local
        .iter()
        .find(|n| n.name == "feature")
        .expect("feature");
    assert!(feature.analysed && !feature.merged && !feature.stale);
    assert_eq!(feature.full, "refs/heads/feature");
    assert_eq!(feature.conflicts, ["a.txt"]);
    assert_eq!(feature.changed_files, 2);
    assert_eq!(feature.unique, Some(2));
    assert!(feature.duplicates.is_empty());
    let dup = b.local.iter().find(|n| n.name == "dup").expect("dup");
    assert_eq!(dup.unique, Some(2));
    assert_eq!(dup.duplicates.len(), 1, "{:?}", dup.duplicates);
    assert_eq!(dup.duplicates[0].summary, "Pick me");
    assert_eq!(dup.duplicates[0].equivalent_in, "HEAD");
    assert!(dup.conflicts.is_empty());
    assert_eq!(b.unmerged, 3);
    assert_eq!(b.merged, 0);
    assert!(b.throwaway >= 1, "tmp-old");
    assert_eq!(b.prunable, None, "never a network call");

    // Releases.
    let rel = &r.releases;
    assert_eq!(rel.tags.len(), 2);
    assert_eq!(rel.tags[0].name, "v1.1.0");
    assert_eq!(rel.tags[1].name, "v1.0.0");
    assert!(rel.tags[0].annotated && !rel.tags[0].signed && rel.tags[0].on_branch);
    assert_eq!(rel.tags[0].tagger, "Test User");
    assert_eq!(rel.tags[0].message, "release 1.1.0");
    assert_eq!(rel.tags[0].version, Some((1, 1, 0)));
    assert!(!rel.tags[0].prerelease);
    assert_eq!(rel.tags[0].commits_since_previous, Some(1));
    assert_eq!(
        (rel.annotated, rel.lightweight, rel.signed, rel.orphan),
        (2, 0, 0, 0)
    );
    assert_eq!(rel.last_tag.as_deref(), Some("v1.1.0"));
    assert!(rel.last_tag_time.is_some());
    assert_eq!(rel.since_last_days, Some(0));
    assert_eq!(rel.unreleased_total, 4);
    assert_eq!(rel.unreleased.len(), 4);
    assert_eq!(rel.unreleased[0].1, "amended version");
    assert_eq!(rel.unreleased_by_type[0], ("other".to_owned(), 2));
    assert!(rel.unreleased_by_type.contains(&("feat".to_owned(), 1)));
    assert!(rel.unreleased_by_type.contains(&("fix".to_owned(), 1)));
    assert_eq!(rel.suggested_next.as_deref(), Some("1.2.0"));
    assert_eq!(rel.scheme, "semver");
    assert_eq!(rel.bumps.len(), 1);
    assert_eq!(
        (rel.bumps[0].from.as_str(), rel.bumps[0].to.as_str()),
        ("v1.0.0", "v1.1.0")
    );
    assert_eq!(rel.bumps[0].kind, "minor");
    assert_eq!(rel.bumps[0].commits, Some(1));
    assert!(rel.cadence_days.is_some());

    // Search: pickaxe finds the commit that introduced the text.
    let q = search::SearchQuery {
        text: "needle_marker".into(),
        mode: search::SearchMode::Pickaxe,
        ..Default::default()
    };
    let res = search::search(&f.work, &q, &cancel).expect("search");
    assert_eq!(res.hits.len(), 1, "{:?}", res.hits);
    assert_eq!(res.hits[0].summary, "feat: add needle");
    assert_eq!(res.hits[0].author, "Test User");
    assert_eq!(res.hits[0].paths, ["lib.rs"]);
    assert!(!res.hits[0].is_merge);
    assert_eq!(res.paths, [("lib.rs".to_owned(), 1)]);
    assert!(!res.truncated && !res.cancelled);
    assert!(res.command.contains("-Sneedle_marker"), "{}", res.command);
    assert!(res.span.is_some());
    // Message regex.
    let q = search::SearchQuery {
        text: "^fix: .*typo".into(),
        mode: search::SearchMode::Message,
        ..Default::default()
    };
    let res = search::search(&f.work, &q, &cancel).expect("search");
    assert_eq!(res.hits.len(), 1);
    assert_eq!(res.hits[0].summary, "fix: a typo");
    assert_eq!(res.hits[0].paths, ["a.txt"]);
    // Case folding and the cap.
    let q = search::SearchQuery {
        text: "test user".into(),
        mode: search::SearchMode::Author,
        ignore_case: true,
        max: 2,
        ..Default::default()
    };
    let res = search::search(&f.work, &q, &cancel).expect("search");
    assert_eq!(res.hits.len(), 2);
    assert!(res.truncated);
    assert_eq!(res.authors, [("Test User".to_owned(), 2)]);
    assert!(
        search::search(&f.work, &search::SearchQuery::default(), &cancel).is_err(),
        "empty text"
    );

    // Recovery: the amended commit and the dropped stash.
    let lost = recover::lost(&f.work, &cancel, &mut |_| {}).expect("fsck");
    let amended = lost
        .commits
        .iter()
        .find(|c| c.summary == "will be amended")
        .expect("amended commit is lost");
    assert_eq!(amended.kind, recover::LostKind::Commit);
    assert!(
        amended
            .reflog
            .as_deref()
            .is_some_and(|r| r.contains("will be amended")),
        "{:?}",
        amended.reflog
    );
    assert_eq!(amended.files, ["amend.txt"]);
    assert_eq!(amended.files_total, 1);
    assert!(amended.suggested_branch.starts_with("recovered/"));
    assert!(!amended.has_child);
    let stash = lost
        .commits
        .iter()
        .find(|c| c.kind == recover::LostKind::Stash)
        .expect("dropped stash is lost");
    assert!(stash.summary.contains("dropped stash"), "{}", stash.summary);
    assert!(stash.parents.len() >= 2);
    assert_eq!(stash.files, ["stashme.txt"]);
    assert!(stash.reflog.is_none(), "dropped from the stash reflog");
    assert_eq!(lost.stashes, 1);
    assert!(lost.in_reflog >= 1 && lost.dangling >= 1, "{lost:?}");
    assert!(
        !lost
            .commits
            .iter()
            .any(|c| c.summary.starts_with("index on ")),
        "stash internals are not listed"
    );
    assert_eq!(lost.reflog_expire, "90 days");
    assert_eq!(lost.reflog_expire_unreachable, "30 days");
    assert!(!lost.cancelled);
    assert!(lost.commands.iter().any(|c| c.starts_with("git fsck")));
}

#[test]
fn history_hotspots_ownership_and_quality() {
    use gitvis_core::insights::{self, InsightsOptions, history};
    use std::sync::atomic::AtomicBool;

    let f = Fixture::new();
    std::fs::create_dir_all(f.work.join("src")).unwrap();
    let lines =
        |n: usize, tag: &str| -> String { (0..n).map(|i| format!("{tag} line {i}\n")).collect() };
    let bob = |rel: &str, content: &str, message: &str| {
        f.write(rel, content);
        f.git(&["add", "-A"]);
        f.git(&[
            "-c",
            "user.name=Bob Builder",
            "-c",
            "user.email=bob@example.com",
            "commit",
            "-q",
            "-m",
            message,
        ]);
    };
    let core_v1 = lines(80, "core");
    let test_v1 = lines(40, "test");
    let core_v2 = format!("{core_v1}core extra 1\ncore extra 2\n");
    let test_v2 = format!("{test_v1}test extra\n");
    let core_v3 = format!("{core_v2}handle empty\n");
    let test_v3 = format!("{test_v2}test empty\n");
    let core_v4 = format!("{core_v3}bob again\n");
    let test_v4 = format!("{test_v3}bob test again\n");

    f.commit("README.md", "# Demo\n", "Initial commit");
    // core.rs and core_test.rs always change together.
    f.write("src/core.rs", &core_v1);
    f.commit("src/core_test.rs", &test_v1, "feat: add core module");
    f.write("src/core.rs", &core_v2);
    bob("src/core_test.rs", &test_v2, "Improve core and tests");
    f.write("src/core.rs", &core_v3);
    f.commit("src/core_test.rs", &test_v3, "fix: handle empty input");
    f.write("src/core.rs", &core_v4);
    bob("src/core_test.rs", &test_v4, "More tests");
    f.commit("README.md", "# Demo\nwip\n", "wip");
    f.commit("README.md", "# Demo\nwip\nmore\n", "fixup! Initial commit");
    bob("src/util.rs", &lines(30, "util"), "Add util");
    f.commit(
        "README.md",
        "# Demo\n\nUsage: run it.\n",
        "Update README with usage",
    );
    f.git(&["revert", "--no-edit", "HEAD"]);

    let mut commands = Vec::new();
    let files = insights::tracked_files(&f.work, &mut commands).expect("tracked files");
    assert_eq!(files.len(), 4);
    let cancel = AtomicBool::new(false);
    let mut progress_calls = 0;
    let (h, commits, bytes) = history::analyze(
        &f.work,
        &InsightsOptions::default(),
        &files,
        &cancel,
        &mut |_| progress_calls += 1,
        &mut commands,
    )
    .expect("analyze");
    assert_eq!(commits, 10);
    assert_eq!(h.commits, 10);
    assert!(bytes > 0);
    assert!(progress_calls >= 2);
    assert!(
        commands
            .iter()
            .any(|c| c.starts_with("git log ") && c.contains("--numstat"))
    );

    // Hotspots: the big, often-changed module first.
    assert_eq!(h.hotspots[0].path, "src/core.rs");
    assert_eq!(h.hotspots[1].path, "src/core_test.rs");
    assert_eq!(h.hotspots[0].commits, 4);
    assert_eq!(h.hotspots[0].authors, 2);
    assert_eq!(h.hotspots[0].fixes, 1, "fix: handle empty input");
    assert_eq!(h.hotspots[0].added, 84);
    assert!(h.hotspots[0].size.is_some());
    assert!(h.hotspots[0].score > h.hotspots[1].score);
    assert_eq!(h.paths, 4);

    // Coupling.
    let c = h
        .couplings
        .iter()
        .find(|c| c.a == "src/core.rs" && c.b == "src/core_test.rs")
        .expect("core.rs and core_test.rs are coupled");
    assert_eq!(c.together, 4);
    assert_eq!((c.a_commits, c.b_commits), (4, 4));
    assert!((c.confidence - 1.0).abs() < 1e-12);
    assert!(!c.cross_dir);

    // Hygiene.
    let q = &h.quality;
    assert_eq!(q.total, 10);
    assert_eq!(q.merges, 0);
    assert_eq!(q.reverts.len(), 1);
    assert_eq!(
        q.reverts[0].commit.summary,
        "Revert \"Update README with usage\""
    );
    assert!(!q.reverts[0].double);
    assert_eq!(q.fixups.len(), 1);
    assert_eq!(q.fixups[0].summary, "fixup! Initial commit");
    assert_eq!(q.wip.len(), 1);
    assert_eq!(q.wip[0].summary, "wip");
    assert_eq!(q.conventional, 2);
    assert_eq!(q.conventional_types.len(), 2);
    assert!(q.conventional_types.contains(&("feat".to_owned(), 1)));
    assert_eq!(q.one_word, 1);
    assert_eq!(q.no_body, 9, "only the revert has a body");
    assert_eq!(q.median_files, 1);
    assert!(q.giant.is_empty());
    assert_eq!(q.bot_commits, 0);
    assert_eq!(q.author_committer_differ, 0);
    assert!(q.date_anomalies.is_empty(), "{:?}", q.date_anomalies);
    assert_eq!(q.anomaly_counts.len(), 5);
    let sig = q.signatures.as_ref().expect("signature pass ran");
    assert_eq!(sig.checked, 10);
    assert_eq!(sig.none, 10);
    assert_eq!(sig.by_author.len(), 2);
    assert_eq!(q.merges_by_year.len(), 1);

    // Ownership: Test User wrote most of src.
    let src = h
        .ownership
        .iter()
        .find(|o| o.dir == "src")
        .expect("src ownership");
    assert_eq!(src.files, 3);
    assert_eq!(src.bus_factor, 1);
    assert!(!src.orphaned);
    assert_eq!(src.owners[0].email, "test@example.com");
    assert_eq!(src.owners[0].lines, 80 + 40 + 2);
    assert_eq!(src.owners[1].email, "bob@example.com");
    assert_eq!(src.owners[1].lines, 3 + 2 + 30);
    assert_eq!(src.owners[1].commits, 3);
    assert_eq!(src.commits, 5);
    // The 100-line floor only applies once ten directories clear it; a small
    // repository keeps every directory, so the root (README) is listed too.
    assert!(
        h.ownership.iter().any(|o| o.dir == "."),
        "small repositories keep directories under 100 lines"
    );

    // Age and timeline.
    let src_age = h.age.iter().find(|a| a.dir == "src").expect("src age");
    assert_eq!(src_age.files, 3);
    assert_eq!(src_age.active_files, 3);
    assert_eq!(src_age.stale_files, 0);
    let t = &h.timeline;
    assert!(!t.months.is_empty());
    assert_eq!(t.months.iter().map(|m| m.commits).sum::<u32>(), 10);
    assert_eq!(t.authors.len(), 2);
    assert_eq!(t.authors[0].email, "test@example.com");
    assert_eq!(t.authors[0].commits, 7);
    assert_eq!(t.active_authors, 2);
    assert_eq!(t.total_files, 4);
    let rust = t.languages.iter().find(|l| l.name == "Rust").expect("Rust");
    assert_eq!(rust.files, 3);
    assert_eq!(t.heat.iter().flatten().sum::<u32>(), 10);

    // Blame of one file.
    let fb = history::blame(&f.work, "src/core.rs").expect("blame");
    assert_eq!(fb.lines, 84);
    assert_eq!(fb.commits, 4);
    assert_eq!(fb.owners.len(), 2);
    assert_eq!(fb.owners[0].email, "test@example.com");
    assert_eq!(fb.owners[0].lines, 81);
    assert_eq!(fb.owners[1].name, "Bob Builder");
    assert_eq!(fb.owners[1].lines, 3);
    assert_eq!(fb.bus_factor, 1);
    assert_eq!(fb.by_year.iter().map(|(_, n)| n).sum::<u32>(), 84);
    assert!(fb.command.ends_with("-- src/core.rs"));
    assert!(
        history::blame(&f.work, "nope.txt").is_err(),
        "untracked path is an error"
    );
}

#[test]
fn health_finds_landmines_and_eol() {
    use gitvis_core::insights::health::{self, LandmineKind};
    use gitvis_core::insights::{Severity, tracked_files};
    use std::sync::atomic::AtomicBool;

    let f = Fixture::new();
    f.commit("README.md", "hello\n", "init");
    // A CRLF file, a shell script without the executable bit, a tracked
    // file that .gitignore matches, a text file declared binary.
    f.write("crlf.txt", "a\r\nb\r\n");
    f.write("script.sh", "#!/bin/sh\necho hi\n");
    f.write(".gitignore", "build.log\n");
    f.write("build.log", "log line\n");
    f.write(".gitattributes", "*.bin binary\n");
    f.write("data.bin", "plain text\n");
    f.git(&["add", "-A"]);
    f.git(&["add", "-f", "build.log"]);
    // Names Windows cannot create go through the index directly.
    let hello = f.git(&["hash-object", "-w", "README.md"]);
    f.git(&[
        "-c",
        "core.protectNTFS=false",
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("100644,{hello},aux.txt"),
    ]);
    f.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("100644,{hello},readme.md"),
    ]);
    let empty = f.git(&["hash-object", "-w", "--stdin"]);
    f.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("100755,{empty},run"),
    ]);
    f.git(&["commit", "-q", "-m", "landmines"]);

    // A real submodule, plus a gitlink nobody registered.
    let subrepo = f.root.join("subrepo");
    std::fs::create_dir_all(&subrepo).unwrap();
    f.git_in(&subrepo, &["init", "-q", "-b", "main"]);
    f.git_in(&subrepo, &["config", "user.email", "t@example.com"]);
    f.git_in(&subrepo, &["config", "user.name", "T"]);
    std::fs::write(subrepo.join("lib.txt"), "lib\n").unwrap();
    f.git_in(&subrepo, &["add", "-A"]);
    f.git_in(&subrepo, &["commit", "-q", "-m", "sub"]);
    f.git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        "-q",
        subrepo.to_str().unwrap(),
        "sub",
    ]);
    let head = f.git(&["rev-parse", "HEAD"]);
    f.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{head},loose"),
    ]);
    f.git(&["commit", "-q", "-m", "submodules"]);

    let mut commands = Vec::new();
    let files = tracked_files(&f.work, &mut commands).expect("tracked files");
    let h = health::analyze(&f.work, &files, &mut commands);

    let paths = |kind: LandmineKind| -> Vec<String> {
        h.landmines
            .iter()
            .filter(|l| l.kind == kind)
            .flat_map(|l| l.paths.iter().cloned())
            .collect()
    };
    assert_eq!(paths(LandmineKind::WindowsReserved), ["aux.txt"]);
    assert_eq!(
        paths(LandmineKind::CaseCollision),
        ["README.md", "readme.md"]
    );
    assert_eq!(paths(LandmineKind::EmptyExecutable), ["run"]);
    assert_eq!(paths(LandmineKind::IgnoredTracked), ["build.log"]);
    assert_eq!(paths(LandmineKind::NestedRepository), ["loose"]);
    assert_eq!(h.worst, Some(Severity::High));
    assert_eq!(h.count(LandmineKind::Symlink), 0);
    assert_eq!(h.files, files.len());

    let le = &h.line_endings;
    assert_eq!(le.crlf, 1);
    assert_eq!(le.crlf_examples, ["crlf.txt"]);
    assert!(le.lf >= 5, "{}", le.lf);
    assert!(le.mixed.is_empty());
    assert!(le.fighting.is_empty(), "{:?}", le.fighting);
    assert_eq!(le.autocrlf, "false");
    assert!(le.attributes_rules);
    assert_eq!(le.attributes_files, [".gitattributes"]);
    assert_eq!(le.misattributed, ["data.bin"]);
    assert!(le.bom.is_empty() && le.non_utf8.is_empty());

    assert_eq!(h.exec_bits.scripts_without, ["script.sh"]);
    assert_eq!(h.exec_bits.executables, 1);
    assert!(h.exec_bits.non_scripts_with.is_empty());

    let w = &h.weight;
    assert!(w.head_bytes > 0);
    assert!(w.count_objects.iter().any(|(k, _)| k == "count"));
    assert!(w.loose_objects > 0, "nothing has been packed");
    assert_eq!(w.packs, 0);
    assert!(w.head_blobs.is_empty() && w.lfs_candidates.is_empty());
    assert_eq!(w.lfs_pointers, 0);
    assert!(!w.shallow && !w.alternates && w.partial_clone.is_none());
    assert!(w.worktrees.is_empty());
    assert!(w.refs.0 >= 1, "{:?}", w.refs);
    assert!(w.dirs_by_size.iter().any(|(d, _)| d == "."));
    assert!(w.ext_by_size.iter().any(|(e, n, _)| e == "txt" && *n == 2));

    let by_path = |p: &str| h.submodules.iter().find(|s| s.path == p).unwrap();
    assert_eq!(h.submodules.len(), 2, "{:#?}", h.submodules);
    let sub = by_path("sub");
    assert_eq!(sub.status, "ok", "{sub:#?}");
    assert!(sub.recorded.is_some() && sub.recorded == sub.checked_out);
    assert!(sub.url.ends_with("subrepo"));
    assert_eq!(by_path("loose").status, "not in .gitmodules");
    assert!(
        commands
            .iter()
            .any(|c| c.starts_with("git -C sub rev-parse")),
        "{commands:?}"
    );

    // Move the submodule off the recorded commit: `different commit`.
    std::fs::write(f.work.join("sub").join("more.txt"), "more\n").unwrap();
    f.git_in(&f.work.join("sub"), &["add", "-A"]);
    f.git_in(&f.work.join("sub"), &["commit", "-q", "-m", "ahead"]);
    let again = health::analyze(&f.work, &files, &mut Vec::new());
    let sub = again.submodules.iter().find(|s| s.path == "sub").unwrap();
    assert_eq!(sub.status, "different commit", "{sub:#?}");
    assert!(again.issues() >= again.landmines.len() + 2);

    // The history walk: a big blob committed and removed is dead weight.
    let big = vec![b'x'; 700 * 1024];
    std::fs::write(f.work.join("big.dat"), &big).unwrap();
    f.git(&["add", "big.dat"]);
    f.git(&["commit", "-q", "-m", "big"]);
    f.git(&["rm", "-q", "big.dat"]);
    f.git(&["commit", "-q", "-m", "drop big"]);
    let cancel = AtomicBool::new(false);
    let mut phases = Vec::new();
    let weight =
        health::weight(&f.work, &cancel, &mut |p| phases.push(p.phase.clone())).expect("weight");
    assert!(!weight.cancelled);
    assert_eq!(
        weight.commits, 5,
        "init, landmines, submodules, big, drop big"
    );
    // "drop big" restores the tree of "submodules": four distinct root trees.
    assert!(weight.trees >= 4, "{}", weight.trees);
    assert!(weight.total_blobs >= 9, "{}", weight.total_blobs);
    assert!(weight.total_bytes > 700 * 1024);
    assert!(weight.total_disk > 0);
    assert_eq!(weight.blobs.len(), 1);
    assert_eq!(weight.blobs[0].path, "big.dat");
    assert_eq!(weight.blobs[0].size, 700 * 1024);
    assert!(!weight.blobs[0].in_head);
    assert_eq!(weight.deleted_bytes, 700 * 1024);
    assert_eq!(weight.by_ext[0].0, "dat");
    assert_eq!(weight.by_dir[0].0, ".");
    assert_eq!(weight.commands.len(), 3, "{:?}", weight.commands);

    // Cancellation before the walk starts yields an empty, cancelled report
    // only once enough objects have streamed; with this few it just finishes.
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    let w2 = health::weight(&f.work, &cancel, &mut |_| {}).expect("weight");
    assert_eq!(w2.total_blobs, weight.total_blobs);
}

#[test]
fn debt_ci_and_licences() {
    use gitvis_core::insights::{self, ci, debt, license};
    use std::sync::atomic::AtomicBool;

    let f = Fixture::new();
    std::fs::create_dir_all(f.work.join(".github/workflows")).unwrap();
    std::fs::create_dir_all(f.work.join("src")).unwrap();
    f.write(
        "LICENSE",
        "MIT License\n\nCopyright (c) 2024 Gitvis Authors\n\nPermission is hereby granted, free of charge, to any person obtaining a copy\nof this software and associated documentation files (the \"Software\"), to deal\nin the Software without restriction.\n\nTHE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR\nIMPLIED.\n",
    );
    f.write(
        "package.json",
        "{\"name\": \"fixture\", \"version\": \"1.0.0\", \"license\": \"ISC\"}\n",
    );
    f.write(
        ".github/workflows/ci.yml",
        concat!(
            "name: CI\n",
            "on:\n",
            "  pull_request_target:\n",
            "    types: [opened]\n",
            "jobs:\n",
            "  build:\n",
            "    runs-on: [self-hosted, linux]\n",
            "    steps:\n",
            "      - uses: actions/checkout@v4\n",
            "        with:\n",
            "          ref: ${{ github.event.pull_request.head.sha }}\n",
            "      - uses: some-org/cool-action@v1\n",
            "        with:\n",
            "          token: ${{ secrets.NPM_TOKEN }}\n",
            "      - run: echo \"${{ github.event.pull_request.title }}\"\n",
            "      - run: curl -sSL https://example.com/install.sh | sh\n",
        ),
    );
    f.write(
        "src/lib.rs",
        "// Copyright (c) 2019 Other Corp\n\npub fn a() {}\n// TODO(bob): fix #12\n// FIXME this is slow\n",
    );
    f.write(
        "src/own.rs",
        "// Copyright (c) 2024 Gitvis Authors\npub fn b() {}\n",
    );
    f.write(
        "src/own2.rs",
        "// Copyright (c) 2024 Gitvis Authors\npub fn c() {}\n",
    );
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "fixture"]);
    let head = f.git(&["rev-parse", "HEAD"]);

    let mut commands = Vec::new();
    let files = insights::tracked_files(&f.work, &mut commands).expect("ls-files");
    let cancel = AtomicBool::new(false);

    // Debt markers.
    let mut phases = Vec::new();
    let d = debt::scan(&f.work, &files, &cancel, &mut |p| {
        phases.push(p.phase.clone())
    });
    assert!(
        phases.iter().all(|p| p == "Scanning for debt markers"),
        "{phases:?}"
    );
    assert_eq!(d.markers.len(), 2, "{:?}", d.markers);
    let todo = &d.markers[0];
    assert_eq!(todo.path, "src/lib.rs");
    assert_eq!(todo.line, 4);
    assert_eq!(todo.kind, "TODO");
    assert_eq!(todo.assignee.as_deref(), Some("bob"));
    assert_eq!(todo.ticket.as_deref(), Some("#12"));
    assert_eq!(todo.text, "fix #12");
    let fixme = &d.markers[1];
    assert_eq!(fixme.kind, "FIXME");
    assert_eq!(fixme.line, 5);
    assert_eq!(fixme.text, "this is slow");
    assert_eq!(d.by_kind, vec![("FIXME", 1), ("TODO", 1)]);
    assert_eq!(d.by_assignee, vec![("bob".to_owned(), 1)]);
    assert_eq!(d.by_dir[0], ("src".to_owned(), 2));
    assert_eq!(d.files_with_markers, 1);
    assert!(d.files_scanned >= 6, "{}", d.files_scanned);
    assert!(d.lines_scanned > 10);
    assert!(!d.truncated && !d.cancelled);

    // Dating through blame.
    let dated = debt::date(&f.work, &d.markers, 10, &cancel, &mut |_| {}).expect("blame");
    assert_eq!(dated.files_blamed, 1);
    assert_eq!(dated.files_skipped, 0);
    assert_eq!(dated.markers.len(), 2);
    for m in &dated.markers {
        assert_eq!(m.author.as_deref(), Some("Test User"), "{m:?}");
        assert!(m.time.unwrap() > 1_600_000_000);
        assert_eq!(m.commit.map(|c| c.to_string()), Some(head.clone()));
    }
    assert_eq!(
        dated.commands,
        vec!["git blame --porcelain HEAD -- src/lib.rs"]
    );
    assert!(!dated.cancelled);
    // A cap of zero files blames nothing but keeps the markers.
    let none = debt::date(&f.work, &d.markers, 0, &cancel, &mut |_| {}).expect("blame");
    assert_eq!(none.files_blamed, 0);
    assert_eq!(none.files_skipped, 1);
    assert_eq!(none.markers, d.markers);

    // CI.
    let c = ci::analyze(&f.work, &files);
    assert_eq!(c.workflow_files, 1);
    assert_eq!(c.workflows.len(), 1);
    let wf = &c.workflows[0];
    assert_eq!(wf.name, "CI");
    assert!(wf.parse_error.is_none(), "{:?}", wf.parse_error);
    assert_eq!(wf.triggers, ["pull_request_target"]);
    assert!(wf.fork_triggerable);
    assert_eq!(wf.jobs.len(), 1);
    assert!(wf.jobs[0].self_hosted);
    assert_eq!(wf.jobs[0].steps, 4);
    assert_eq!(wf.secrets, ["NPM_TOKEN"]);
    let ids: Vec<&str> = c.findings.iter().map(|f| f.id).collect();
    for id in [
        "unpinned-action",
        "pull-request-target-checkout",
        "script-injection",
        "no-permissions",
        "curl-pipe-sh",
        "self-hosted-public",
        "secrets-to-third-party",
        "no-timeout",
        "persist-credentials",
    ] {
        assert!(ids.contains(&id), "missing {id} in {ids:?}");
    }
    assert_eq!(wf.findings, c.findings.len());
    assert_eq!(c.unpinned_third_party, 1);
    assert_eq!(c.pinned_sha, 0);
    assert_eq!(c.actions.len(), 2);
    assert_eq!(
        c.secrets,
        vec![("NPM_TOKEN".to_owned(), vec!["CI".to_owned()])]
    );
    assert_eq!(c.runners[0], ("linux".to_owned(), 1));
    assert!(c.other_ci.is_empty() && c.dependabot.is_none() && !c.renovate);
    // Findings come severity first.
    assert!(
        c.findings
            .windows(2)
            .all(|w| w[0].severity >= w[1].severity)
    );

    // Licences.
    let mut phases = Vec::new();
    let l = license::analyze(
        &f.work,
        &files,
        &cancel,
        &mut |p| phases.push(p.phase.clone()),
        &mut commands,
    );
    assert!(
        phases.iter().all(|p| p == "Reading licence headers"),
        "{phases:?}"
    );
    assert_eq!(l.main(), Some("MIT"));
    assert_eq!(l.project.len(), 1);
    assert_eq!(l.project[0].path, "LICENSE");
    assert_eq!(l.project[0].confidence, "exact");
    assert_eq!(l.project[0].holders, vec!["Gitvis Authors".to_owned()]);
    assert_eq!(l.project[0].years, Some((2024, 2024)));
    assert_eq!(
        l.declared,
        vec![("package.json".to_owned(), "ISC".to_owned())]
    );
    assert_eq!(l.files_with_headers, 3);
    assert_eq!(l.holders.len(), 2);
    assert_eq!(l.holders[0].holder, "Gitvis Authors");
    assert_eq!(l.holders[0].files, 2);
    assert_eq!(l.foreign.len(), 1, "{:?}", l.foreign);
    assert_eq!(l.foreign[0].holder, "Other Corp");
    assert_eq!(l.foreign[0].files, 1);
    assert_eq!(l.foreign[0].examples, vec!["src/lib.rs".to_owned()]);
    assert_eq!(l.foreign[0].years, Some((2019, 2019)));
    let ids: Vec<&str> = l.findings.iter().map(|f| f.id).collect();
    assert!(ids.contains(&"license-mismatch"), "{ids:?}");
    assert!(ids.contains(&"foreign-copyright"), "{ids:?}");
    assert!(!ids.contains(&"no-license"), "{ids:?}");
    assert!(!ids.contains(&"multiple-licenses"), "{ids:?}");
    let mismatch = l
        .findings
        .iter()
        .find(|f| f.id == "license-mismatch")
        .unwrap();
    assert_eq!(mismatch.paths, vec!["package.json".to_owned()]);
    assert!(
        mismatch.detail.contains("ISC") && mismatch.detail.contains("MIT"),
        "{}",
        mismatch.detail
    );
    let foreign = l
        .findings
        .iter()
        .find(|f| f.id == "foreign-copyright")
        .unwrap();
    assert!(foreign.detail.contains("Other Corp"), "{}", foreign.detail);
    assert!(
        l.findings
            .windows(2)
            .all(|w| w[0].severity >= w[1].severity)
    );
}
