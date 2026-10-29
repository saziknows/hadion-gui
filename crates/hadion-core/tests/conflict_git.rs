//! The conflict editor's core against real conflicts made by the git
//! binary in throwaway repositories: a merge with three hunks, diff3 and
//! zdiff3 styles, a rebase stopped on a conflict, delete / modify, a
//! binary conflict, CRLF and a missing final newline, and a custom
//! `conflict-marker-size`.

use std::path::{Path, PathBuf};
use std::process::Command;

use hadion_core::devops::conflict::labels::OpKind;
use hadion_core::devops::conflict::load::{self, Presentation, Working};
use hadion_core::devops::conflict::markers::Style;
use hadion_core::devops::conflict::resolve::{self, Resolution};
use hadion_core::devops::conflict::{self, ConflictKind, actions};

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
            "hadion-conflict-{name}-{}-{nanos}",
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
            ("merge.conflictStyle", "merge"),
        ] {
            f.git(&["config", k, v]);
        }
        f
    }

    fn dir(&self) -> &Path {
        &self.root
    }

    /// Run git; returns (success, stdout).
    fn try_git(&self, args: &[&str]) -> (bool, String) {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .output()
            .expect("git runs");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        )
    }

    fn git(&self, args: &[&str]) -> String {
        let (ok, out) = self.try_git(args);
        assert!(ok, "git {args:?} failed");
        out
    }

    fn write(&self, rel: &str, content: &[u8]) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    fn read(&self, rel: &str) -> Vec<u8> {
        std::fs::read(self.root.join(rel)).unwrap()
    }

    fn commit_all(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Changed lines are more than three lines apart, so git keeps three
/// separate hunks in every style.
const BASE: &str = "fn one() {\n    1\n}\n\n// two\n// a\nfn two() {\n    2\n}\n\n// three\n// b\nfn three() {\n    3\n}\n\n// four\n// c\nfn four() {\n    4\n}\n";

fn three_hunks(f: &Fixture) {
    f.write("src/lib.rs", BASE.as_bytes());
    f.commit_all("base");
    f.git(&["checkout", "-q", "-b", "feature"]);
    f.write(
        "src/lib.rs",
        BASE.replace("    1\n", "    10\n")
            .replace("    2\n", "    20\n")
            .replace("    4\n", "    40\n")
            .as_bytes(),
    );
    f.commit_all("feature numbers");
    f.git(&["checkout", "-q", "main"]);
    f.write(
        "src/lib.rs",
        BASE.replace("    1\n", "    100\n")
            .replace("    2\n", "    200\n")
            .replace("    4\n", "    400\n")
            .as_bytes(),
    );
    f.commit_all("main numbers");
}

fn entry(f: &Fixture, path: &str) -> conflict::ConflictEntry {
    let mut cmds = Vec::new();
    conflict::list(f.dir(), &mut cmds)
        .unwrap()
        .into_iter()
        .find(|e| e.path == path)
        .unwrap_or_else(|| panic!("{path} is conflicted"))
}

#[test]
fn merge_with_three_hunks() {
    let f = Fixture::new("merge");
    three_hunks(&f);
    let (ok, _) = f.try_git(&["merge", "feature"]);
    assert!(!ok, "the merge stops on conflicts");

    let mut cmds = Vec::new();
    let ov = conflict::overview(f.dir(), &mut cmds).unwrap();
    assert_eq!(ov.files.len(), 1);
    assert_eq!(ov.files[0].kind, ConflictKind::BothModified);
    assert_eq!(ov.op.kind, OpKind::Merge);
    assert_eq!(ov.op.summary(), "Merging feature into main");
    assert_eq!(ov.op.ours("HEAD").name, "main");
    assert_eq!(ov.op.theirs("feature").name, "feature");
    assert!(
        cmds.iter()
            .any(|c| c.starts_with("git status --porcelain=v2"))
    );

    let mut cmds = Vec::new();
    let fl = load::load_file(f.dir(), &ov.files[0], &mut cmds).unwrap();
    assert_eq!(fl.presentation, Presentation::Text);
    let doc = fl.doc.clone().unwrap();
    assert_eq!(doc.hunk_count(), 3);
    assert_eq!(doc.style(), Style::Merge);
    let hunks: Vec<_> = doc.hunks().cloned().collect();
    assert_eq!(hunks[0].ours, b"    100\n");
    assert_eq!(hunks[0].theirs, b"    10\n");
    assert_eq!(hunks[0].ours_label, "HEAD");
    assert_eq!(hunks[0].theirs_label, "feature");
    // The base comes from the stage blobs for merge-style markers.
    for h in &hunks {
        assert!(h.base.is_some(), "base found for {:?}", h.ours);
        assert!(h.base_from_stages);
    }
    assert_eq!(hunks[1].base.as_deref(), Some(&b"    2\n"[..]));
    assert!(cmds.iter().any(|c| c.contains("merge-file -p --object-id")));
    assert!(cmds.iter().any(|c| c.contains("cat-file --batch")));

    // Resolve the three hunks three ways and write the result.
    let res = vec![Resolution::Ours, Resolution::Theirs, Resolution::Base];
    let out = resolve::assemble(&doc, &res, fl.tails);
    let expected = BASE
        .replace("    1\n", "    100\n")
        .replace("    2\n", "    20\n");
    assert_eq!(String::from_utf8(out.clone()).unwrap(), expected);
    assert_eq!(resolve::remaining(&out, 7).0, Vec::<usize>::new());
    let mut cmds = Vec::new();
    actions::write(f.dir(), "src/lib.rs", &out, &mut cmds).unwrap();
    actions::run(f.dir(), &actions::mark_resolved("src/lib.rs"), &mut cmds).unwrap();
    assert!(conflict::list(f.dir(), &mut cmds).unwrap().is_empty());
    assert_eq!(f.read("src/lib.rs"), expected.as_bytes());
}

#[test]
fn diff3_and_zdiff3_styles() {
    for style in ["diff3", "zdiff3"] {
        let f = Fixture::new(style);
        three_hunks(&f);
        let (ok, _) = f.try_git(&[
            "-c",
            &format!("merge.conflictStyle={style}"),
            "merge",
            "feature",
        ]);
        assert!(!ok);
        let e = entry(&f, "src/lib.rs");
        let mut cmds = Vec::new();
        let fl = load::load_file(f.dir(), &e, &mut cmds).unwrap();
        let doc = fl.doc.unwrap();
        assert_eq!(doc.style(), Style::Diff3, "{style}");
        assert_eq!(doc.hunk_count(), 3, "{style}");
        let h = doc.hunks().nth(2).unwrap();
        assert_eq!(h.base.as_deref(), Some(&b"    4\n"[..]), "{style}");
        assert!(!h.base_from_stages);
        let both = resolve::assemble(
            &doc,
            &[
                Resolution::OursThenTheirs,
                Resolution::TheirsThenOurs,
                Resolution::Custom(b"    4000\n".to_vec()),
            ],
            fl.tails,
        );
        let s = String::from_utf8(both).unwrap();
        assert!(s.contains("    100\n    10\n"), "{style}: {s}");
        assert!(s.contains("    20\n    200\n"), "{style}: {s}");
        assert!(s.contains("    4000\n"), "{style}: {s}");
        // Recreating the markers in another style.
        let mut cmds = Vec::new();
        actions::run(
            f.dir(),
            &actions::recreate("src/lib.rs", "merge"),
            &mut cmds,
        )
        .unwrap();
        let again = load::load_file(f.dir(), &e, &mut cmds).unwrap();
        assert_eq!(again.doc.unwrap().style(), Style::Merge);
    }
}

#[test]
fn rebase_labels_and_step() {
    let f = Fixture::new("rebase");
    f.write("a.txt", b"one\ntwo\nthree\n");
    f.commit_all("base");
    f.git(&["checkout", "-q", "-b", "topic"]);
    f.write("b.txt", b"independent\n");
    f.commit_all("Topic adds b");
    f.write("a.txt", b"one\nTWO from topic\nthree\n");
    f.commit_all("Topic changes two");
    f.write("c.txt", b"later\n");
    f.commit_all("Topic adds c");
    f.git(&["checkout", "-q", "main"]);
    f.write("a.txt", b"one\nTWO from main\nthree\n");
    f.commit_all("Main changes two");
    f.git(&["checkout", "-q", "topic"]);
    let (ok, _) = f.try_git(&["rebase", "main"]);
    assert!(!ok, "the rebase stops at step 2");

    let mut cmds = Vec::new();
    let ov = conflict::overview(f.dir(), &mut cmds).unwrap();
    let op = &ov.op;
    assert_eq!(op.kind, OpKind::Rebase);
    assert_eq!(op.step, Some((2, 3)));
    assert_eq!(op.rebasing.as_deref(), Some("topic"));
    assert!(op.swapped());
    assert_eq!(op.title(), "Rebase · step 2 of 3");
    // Ours is the upstream, theirs the commit being applied.
    assert!(
        op.ours("HEAD").name.starts_with("main ("),
        "{:?}",
        op.ours("HEAD")
    );
    assert_eq!(op.ours("HEAD").tag, "upstream");
    let theirs = op.theirs("x").name;
    assert!(theirs.ends_with("Topic changes two"), "{theirs}");
    assert!(
        op.summary().starts_with("Rebasing topic onto main ("),
        "{}",
        op.summary()
    );

    let fl = load::load_file(f.dir(), &ov.files[0], &mut cmds).unwrap();
    let doc = fl.doc.unwrap();
    let h = doc.hunks().next().unwrap();
    assert_eq!(h.ours, b"TWO from main\n", "ours is the upstream");
    assert_eq!(h.theirs, b"TWO from topic\n", "theirs is the commit");
    assert!(h.theirs_label.contains("Topic changes two"));
}

#[test]
fn delete_modify_binary_crlf_and_eof() {
    let f = Fixture::new("kinds");
    f.write("gone.txt", b"keep\n");
    f.write("img.bin", b"\x00\x01base");
    f.write("win.txt", b"a\r\nb\r\nc\r\n");
    f.write("eof.txt", b"first\nlast");
    f.write(".gitattributes", b"*.rst conflict-marker-size=12\n");
    f.write("doc.rst", b"Title\n=====\n\nbody\n");
    f.commit_all("base");
    f.git(&["checkout", "-q", "-b", "feature"]);
    f.git(&["rm", "-q", "gone.txt"]);
    f.write("img.bin", b"\x00\x02theirs");
    f.write("win.txt", b"a\r\nB theirs\r\nc\r\n");
    f.write("eof.txt", b"first\nlast theirs");
    f.write(
        "doc.rst",
        b"Title\n=====\n\nTheirs heading\n--------------\n",
    );
    f.commit_all("feature");
    f.git(&["checkout", "-q", "main"]);
    f.write("gone.txt", b"keep, changed\n");
    f.write("img.bin", b"\x00\x03ours");
    f.write("win.txt", b"a\r\nB ours\r\nc\r\n");
    f.write("eof.txt", b"first\nlast ours");
    f.write("doc.rst", b"Title\n=====\n\nOurs h\n=======\n");
    f.commit_all("main");
    let (ok, _) = f.try_git(&["merge", "feature"]);
    assert!(!ok);

    let mut cmds = Vec::new();
    let files = conflict::list(f.dir(), &mut cmds).unwrap();
    let kinds: Vec<(&str, ConflictKind)> =
        files.iter().map(|e| (e.path.as_str(), e.kind)).collect();
    assert!(kinds.contains(&("gone.txt", ConflictKind::DeletedByThem)));
    assert!(kinds.contains(&("img.bin", ConflictKind::BothModified)));

    // Delete / modify: a choice card; taking theirs deletes the file.
    let gone = load::load_file(f.dir(), &entry(&f, "gone.txt"), &mut cmds).unwrap();
    assert_eq!(gone.presentation, Presentation::OneSided);
    assert!(gone.stages[2].is_none());
    let specs = actions::choice_specs(ConflictKind::DeletedByThem, "gone.txt", "main", "feature");
    let theirs = specs
        .iter()
        .find(|s| s.choice == actions::Choice::Theirs)
        .unwrap();
    actions::run(f.dir(), &theirs.commands, &mut cmds).unwrap();
    assert!(!f.dir().join("gone.txt").exists());

    // Binary: a card; the working file is ours, untouched by git.
    let img = load::load_file(f.dir(), &entry(&f, "img.bin"), &mut cmds).unwrap();
    assert_eq!(img.presentation, Presentation::Binary);
    assert!(matches!(img.working, Working::Binary { size: 6 }));
    assert!(img.stages.iter().flatten().all(|b| b.binary));
    let take = actions::choice_specs(ConflictKind::BothModified, "img.bin", "main", "feature")
        .into_iter()
        .find(|s| s.choice == actions::Choice::Theirs)
        .unwrap();
    actions::run(f.dir(), &take.commands, &mut cmds).unwrap();
    assert_eq!(f.read("img.bin"), b"\x00\x02theirs");

    // CRLF: markers and sides keep \r\n; choices write \r\n back.
    let win = load::load_file(f.dir(), &entry(&f, "win.txt"), &mut cmds).unwrap();
    let doc = win.doc.clone().unwrap();
    let h = doc.hunks().next().unwrap();
    assert_eq!(h.eol, hadion_core::devops::conflict::markers::Eol::CrLf);
    assert_eq!(h.ours, b"B ours\r\n");
    let out = resolve::assemble(&doc, &[Resolution::Theirs], win.tails);
    assert_eq!(out, b"a\r\nB theirs\r\nc\r\n");
    let typed = resolve::custom_bytes("B mine\nB more", h.eol, false);
    let out = resolve::assemble(&doc, &[Resolution::Custom(typed)], win.tails);
    assert_eq!(out, b"a\r\nB mine\r\nB more\r\nc\r\n");

    // A conflict at the end of a file without a final newline.
    let eof = load::load_file(f.dir(), &entry(&f, "eof.txt"), &mut cmds).unwrap();
    let doc = eof.doc.clone().unwrap();
    assert!(doc.ends_with_conflict());
    assert_eq!(eof.tails.ours, Some(false));
    assert_eq!(
        resolve::assemble(&doc, &[Resolution::Ours], eof.tails),
        b"first\nlast ours"
    );
    assert_eq!(
        resolve::assemble(&doc, &[Resolution::OursThenTheirs], eof.tails),
        b"first\nlast ours\nlast theirs"
    );

    // conflict-marker-size=12: the "=======" lines are content.
    let rst = load::load_file(f.dir(), &entry(&f, "doc.rst"), &mut cmds).unwrap();
    assert_eq!(rst.attrs.marker_size, 12);
    let doc = rst.doc.clone().unwrap();
    assert_eq!(doc.hunk_count(), 1);
    let h = doc.hunks().next().unwrap();
    assert_eq!(h.ours, b"Ours h\n=======\n");
    assert_eq!(h.theirs, b"Theirs heading\n--------------\n");
    assert_eq!(doc.to_bytes(), rst.working_bytes().unwrap());
}

#[test]
fn merged_hunks_still_get_a_base() {
    // Two changes two lines apart: the merge style joins them into one
    // conflict, which the zdiff3 hint matches.
    let f = Fixture::new("joined");
    f.write("a.txt", b"x\n1\nkeep\nkeep\n2\ny\n");
    f.commit_all("base");
    f.git(&["checkout", "-q", "-b", "feature"]);
    f.write("a.txt", b"x\n1 theirs\nkeep\nkeep\n2 theirs\ny\n");
    f.commit_all("feature");
    f.git(&["checkout", "-q", "main"]);
    f.write("a.txt", b"x\n1 ours\nkeep\nkeep\n2 ours\ny\n");
    f.commit_all("main");
    let (ok, _) = f.try_git(&["merge", "feature"]);
    assert!(!ok);
    let mut cmds = Vec::new();
    let fl = load::load_file(f.dir(), &entry(&f, "a.txt"), &mut cmds).unwrap();
    let doc = fl.doc.unwrap();
    assert_eq!(doc.hunk_count(), 1, "joined into one conflict");
    let h = doc.hunks().next().unwrap();
    assert_eq!(h.ours, b"1 ours\nkeep\nkeep\n2 ours\n");
    assert_eq!(h.base.as_deref(), Some(&b"1\nkeep\nkeep\n2\n"[..]));
}

#[test]
fn add_add_has_an_empty_base() {
    let f = Fixture::new("addadd");
    f.write("seed.txt", b"seed\n");
    f.commit_all("base");
    f.git(&["checkout", "-q", "-b", "feature"]);
    f.write("new.txt", b"added theirs\n");
    f.commit_all("feature adds");
    f.git(&["checkout", "-q", "main"]);
    f.write("new.txt", b"added ours\n");
    f.commit_all("main adds");
    let (ok, _) = f.try_git(&["merge", "feature"]);
    assert!(!ok);
    let e = entry(&f, "new.txt");
    assert_eq!(e.kind, ConflictKind::BothAdded);
    let mut cmds = Vec::new();
    let fl = load::load_file(f.dir(), &e, &mut cmds).unwrap();
    let doc = fl.doc.unwrap();
    assert_eq!(doc.hunks().next().unwrap().base.as_deref(), Some(&b""[..]));
    assert_eq!(
        resolve::assemble(&doc, &[Resolution::Base], fl.tails),
        b"",
        "the base of an add/add is empty"
    );
}
