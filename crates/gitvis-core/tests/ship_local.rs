//! The local parts of the Ship features (release preparation, where is my
//! commit, environment facts, DORA history) against a throwaway repository
//! built with the real git binary.

use std::path::{Path, PathBuf};
use std::process::Command;

use gitvis_core::devops::ship::whereis::{EnvDeploys, Mark};
use gitvis_core::devops::ship::{deploy, dora, local, notes, whereis};

const T0: i64 = 1_790_000_000;

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
        let root = std::env::temp_dir().join(format!("gitvis-ship-{}-{nanos}", std::process::id()));
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main"], None);
        for (k, v) in [
            ("user.name", "Test User"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
            ("core.autocrlf", "false"),
        ] {
            f.git(&["config", k, v], None);
        }
        f
    }

    fn git_in(&self, dir: &Path, args: &[&str], time: Option<i64>) -> String {
        let mut cmd = Command::new("git");
        cmd.args(args)
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0");
        if let Some(t) = time {
            let d = format!("@{t} +0000");
            cmd.env("GIT_AUTHOR_DATE", &d).env("GIT_COMMITTER_DATE", &d);
        }
        let out = cmd.output().expect("git runs");
        assert!(
            out.status.success(),
            "git {:?} failed:\n{}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn git(&self, args: &[&str], time: Option<i64>) -> String {
        self.git_in(&self.work, args, time)
    }

    /// A commit at `T0 + hours` hours.
    fn commit(&self, file: &str, message: &str, hours: i64) -> String {
        std::fs::write(self.work.join(file), message).unwrap();
        self.git(&["add", "-A"], None);
        let t = T0 + hours * 3_600;
        self.git(&["commit", "-q", "-m", message], Some(t));
        self.git(&["rev-parse", "HEAD"], None)
    }

    fn tag(&self, name: &str, hours: i64) {
        self.git(&["tag", "-a", name, "-m", name], Some(T0 + hours * 3_600));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct Repo {
    f: Fixture,
    c: Vec<String>,
}

/// main: c1 (v1.0.0) – c2 – c3 (v1.1.0) – c4 (breaking) – c5, a feature
/// branch from c3, and an `origin` with main pushed.
fn repo() -> Repo {
    let f = Fixture::new();
    let c1 = f.commit("a", "chore: init", 0);
    f.tag("v1.0.0", 1);
    let c2 = f.commit("b", "feat: add login (#4)", 10);
    let c3 = f.commit("c", "fix: crash on start", 20);
    f.tag("v1.1.0", 21);
    let c4 = f.commit("d", "feat(api)!: new endpoint", 30);
    let c5 = f.commit("e", "docs: readme", 40);
    f.git(&["branch", "feature", &c3], None);
    let bare = f.root.join("origin.git");
    f.git_in(
        &f.root,
        &["init", "-q", "--bare", bare.to_str().unwrap()],
        None,
    );
    f.git(&["remote", "add", "origin", bare.to_str().unwrap()], None);
    f.git(&["push", "-q", "origin", "main"], None);
    f.git(&["fetch", "-q", "origin"], None);
    Repo {
        f,
        c: vec![c1, c2, c3, c4, c5],
    }
}

#[test]
fn prepares_a_release_from_local_history() {
    let r = repo();
    let dir = &r.f.work;
    let mut commands = Vec::new();
    let tags = vec!["v1.0.0".to_owned(), "v1.1.0".to_owned()];
    let compare = |a: &str, b: &str| format!("https://github.com/o/r/compare/{a}...{b}");
    let p = notes::prepare(dir, &tags, None, "main", Some(&compare), &mut commands).unwrap();
    assert_eq!(p.suggestion.base_tag.as_deref(), Some("v1.1.0"));
    assert_eq!(p.suggestion.next, "v2.0.0");
    assert_eq!(p.suggestion.bump, notes::Bump::Major);
    assert_eq!(p.suggestion.commits, 2);
    assert!(
        p.notes
            .starts_with("## Breaking changes\n\n- **api:** new endpoint by Test User\n"),
        "{}",
        p.notes
    );
    assert!(
        p.notes
            .contains("## Documentation\n\n- readme by Test User\n")
    );
    assert!(
        p.notes
            .ends_with("**Full changelog**: https://github.com/o/r/compare/v1.1.0...v2.0.0\n")
    );
    assert!(commands.iter().any(|c| c.starts_with("git log")));
    // From an explicit previous tag.
    let p = notes::prepare(dir, &tags, Some("v1.0.0"), "v1.1.0", None, &mut commands).unwrap();
    assert_eq!(p.suggestion.next, "v1.1.0", "feat since v1.0.0 → minor");
    assert!(p.notes.contains("- add login (#4) by Test User"));
    // A target that does not exist.
    assert!(notes::prepare(dir, &tags, None, "nope", None, &mut commands).is_err());

    let facts = notes::release_facts(
        dir,
        &[
            ("v1.1.0".into(), Some("v1.0.0".into())),
            ("v9.9.9".into(), Some("v1.1.0".into())),
        ],
        &mut commands,
    );
    assert_eq!(facts["v1.1.0"], (Some(r.c[2].clone()), Some(2)));
    assert_eq!(facts["v9.9.9"], (None, None));
}

#[test]
fn finds_where_a_commit_is() {
    let r = repo();
    let dir = &r.f.work;
    let c = &r.c;
    let mut commands = Vec::new();
    let envs = vec![
        EnvDeploys {
            env: "production".into(),
            current: Some(c[2].clone()),
            successes: vec![(1, c[0].clone(), T0), (2, c[2].clone(), T0 + 100)],
        },
        EnvDeploys {
            env: "staging".into(),
            current: Some(c[4].clone()),
            successes: vec![(3, c[3].clone(), T0 + 200), (4, c[4].clone(), T0 + 300)],
        },
        EnvDeploys {
            env: "dev".into(),
            current: Some("0123456789abcdef0123456789abcdef01234567".into()),
            successes: vec![],
        },
        EnvDeploys {
            env: "empty".into(),
            current: None,
            successes: vec![],
        },
    ];
    let short = &c[1][..8];
    let w = whereis::locate(dir, short, &envs, &mut commands).unwrap();
    assert_eq!(w.sha, c[1]);
    assert_eq!(w.summary, "feat: add login (#4)");
    assert_eq!(w.author, "Test User");
    assert_eq!(w.commit_time, T0 + 10 * 3_600);
    let mut local = w.local_branches.clone();
    local.sort();
    assert_eq!(local, vec!["feature", "main"]);
    assert_eq!(w.remote_branches, vec!["origin/main"]);
    assert_eq!(w.first_tag().unwrap().0, "v1.1.0");
    assert_eq!(w.tags_total, 1);
    let hit = |name: &str| w.envs.iter().find(|e| e.env == name).unwrap().clone();
    assert_eq!(hit("production").mark, Mark::Done);
    assert_eq!(hit("production").since.as_ref().unwrap().1, 2);
    assert_eq!(hit("staging").mark, Mark::Done);
    assert_eq!(hit("staging").since.as_ref().unwrap().1, 3);
    assert_eq!(hit("dev").mark, Mark::Unknown);
    assert!(hit("dev").note.contains("not in the local history"));
    assert_eq!(hit("empty").mark, Mark::Unknown);

    // The breaking change is on staging but not in production yet.
    let w = whereis::locate(dir, &c[3], &envs, &mut commands).unwrap();
    assert_eq!(w.envs[0].mark, Mark::NotYet);
    assert_eq!(w.envs[1].mark, Mark::Done);
    assert!(w.tags.is_empty());
    assert_eq!(w.local_branches, vec!["main"]);
    assert!(whereis::locate(dir, "no-such-ref", &envs, &mut commands).is_err());
    assert!(whereis::locate(dir, "--all", &envs, &mut commands).is_err());
}

#[test]
fn environment_facts_against_the_default_branch() {
    let r = repo();
    let dir = &r.f.work;
    let c = &r.c;
    let mut commands = Vec::new();
    let default = local::default_branch(dir, None, &mut commands);
    assert_eq!(default.as_deref(), Some("origin/main"));
    assert_eq!(
        local::default_branch(dir, Some("main"), &mut commands).as_deref(),
        Some("origin/main")
    );
    let facts = deploy::local_facts(
        dir,
        &[
            ("staging".into(), c[4].clone()),
            ("production".into(), c[2].clone()),
            (
                "gone".into(),
                "0123456789abcdef0123456789abcdef01234567".into(),
            ),
            ("github-pages".into(), c[0].clone()),
        ],
        default.as_deref(),
        &mut commands,
    );
    let staging = &facts["staging"];
    assert!(staging.present);
    assert_eq!(staging.behind, Some(0));
    let (next, n, list) = staging.ahead_of_next.clone().unwrap();
    assert_eq!((next.as_str(), n), ("production", 2));
    assert_eq!(list[0].sha, c[4]);
    assert_eq!(list[1].subject, "feat(api)!: new endpoint");
    assert_eq!(facts["production"].behind, Some(2));
    // github-pages is GitHub's own environment, not the next stage.
    assert!(facts["production"].ahead_of_next.is_none());
    assert!(facts["github-pages"].ahead_of_next.is_none());
    assert_eq!(facts["github-pages"].behind, Some(4));
    assert!(!facts["gone"].present);
    assert_eq!(facts["gone"].behind, None);
}

#[test]
fn dora_reads_the_commits_each_deployment_brought() {
    let r = repo();
    let dir = &r.f.work;
    let c = &r.c;
    let mut commands = Vec::new();
    let ev = |id: &str, sha: &str, hours: i64, outcome: dora::Outcome| dora::Event {
        id: id.into(),
        label: id.into(),
        time: T0 + hours * 3_600,
        rev: Some(sha.into()),
        outcome,
        creator: String::new(),
    };
    let events = dora::prepare(vec![
        ev("d1", &c[0], 5, dora::Outcome::Success),
        ev("d2", &c[2], 25, dora::Outcome::Success),
        ev("d3", &c[3], 35, dora::Outcome::Failure),
        ev("d4", &c[4], 45, dora::Outcome::Success),
        ev("d5", &c[2], 50, dora::Outcome::Success),
        ev(
            "d6",
            "0123456789abcdef0123456789abcdef01234567",
            60,
            dora::Outcome::Success,
        ),
    ]);
    let local = dora::collect_local(dir, &events, &mut commands);
    assert_eq!(local.resolved[0].as_deref(), Some(c[0].as_str()));
    assert!(local.resolved[5].is_none());
    let shas = |i: usize| -> Vec<String> {
        local.new_commits[&i]
            .iter()
            .map(|(s, _, _)| s.clone())
            .collect()
    };
    assert_eq!(shas(1), vec![c[2].clone(), c[1].clone()]);
    assert_eq!(shas(3), vec![c[4].clone(), c[3].clone()]);
    // The roll-back to c3 brings nothing new.
    assert!(shas(4).is_empty());
    let again = dora::prepare(vec![
        ev("r1", &c[2], 5, dora::Outcome::Success),
        ev("r2", &c[2], 30, dora::Outcome::Success),
    ]);
    let l2 = dora::collect_local(dir, &again, &mut commands);
    assert_eq!(
        l2.new_commits.get(&1).map(Vec::len),
        Some(0),
        "a redeploy brings nothing"
    );
    assert!(!local.new_commits.contains_key(&0), "no predecessor");
    // Lead times: c3 and c5 deployed 5 h after they were committed, c2
    // and c4 15 h: the median is 10 h.
    let now = T0 + 70 * 3_600;
    let rep = dora::compute(&events, &local, 30, now, 0);
    assert_eq!(rep.successes, 5);
    assert_eq!(rep.failures, 1);
    assert_eq!(rep.coverage.missing_local, 1);
    let lead = rep.lead.unwrap();
    assert_eq!(lead.n, 4);
    assert_eq!(lead.median, 10 * 3_600);
    assert_eq!(lead.p90, 15 * 3_600);
    assert_eq!(rep.restore.unwrap().median, 10 * 3_600);

    let tags = dora::events_from_tags(dir, "v*", &mut commands).unwrap();
    let names: Vec<&str> = tags.iter().map(|e| e.label.as_str()).collect();
    assert_eq!(names, vec!["v1.0.0", "v1.1.0"]);
    assert_eq!(tags[1].time, T0 + 21 * 3_600);
    assert!(dora::events_from_tags(dir, "-x", &mut commands).is_err());
}
