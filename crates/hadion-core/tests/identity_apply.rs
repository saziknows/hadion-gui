//! The identity area's plans run for real against a scratch global config
//! (`GIT_CONFIG_GLOBAL`, `GIT_CONFIG_NOSYSTEM`, a scratch `HOME`): a config
//! edit, an identity per folder (add, edit, remove) and an SSH signing
//! setup, each checked with the real git binary, down to a signed commit
//! that verifies against the allowed-signers file.
//!
//! One test function: it changes the process environment, which the other
//! test binaries do not share.

use std::path::{Path, PathBuf};
use std::process::Command;

use hadion_core::devops::identity::config::{self, Snapshot};
use hadion_core::devops::identity::includes::{self, NewIdentity};
use hadion_core::devops::identity::{
    Scope, Step, global_config_file, run_steps, signing, slash, ssh,
};

fn git(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git runs");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
        .trim()
        .to_owned(),
    )
}

fn get(dir: &Path, key: &str) -> Option<String> {
    let (ok, out) = git(dir, &["config", "--get", key]);
    ok.then_some(out)
}

/// `scope<TAB>file:path<TAB>value` of the effective value.
fn origin_of(dir: &Path, key: &str) -> String {
    git(
        dir,
        &["config", "--show-origin", "--show-scope", "--get", key],
    )
    .1
}

fn run(steps: &[Step], dir: &Path) {
    let (mut cmds, mut calls) = (Vec::new(), Vec::new());
    if let Err(e) = run_steps(steps, dir, &mut cmds, &mut calls) {
        panic!("steps failed: {e}\ncommands: {cmds:#?}");
    }
    assert!(calls.is_empty(), "no GitHub call in a local plan");
}

fn repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    assert!(git(dir, &["init", "-q", "-b", "main"]).0);
    assert!(git(dir, &["config", "core.autocrlf", "false"]).0);
}

/// The temp folder by its real name. GitHub's Windows runners set TEMP to an
/// 8.3 short path (`C:\Users\RUNNER~1\…`), and git for Windows sees the
/// repository under the long name, so a `gitdir:` pattern built from the
/// short one never matches.
fn temp_root() -> PathBuf {
    let t = std::env::temp_dir();
    let real = t.canonicalize().unwrap_or(t);
    // `canonicalize` gives a verbatim `\\?\C:\…` path on Windows.
    match real.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(s) if !s.starts_with("UNC") => PathBuf::from(s),
        _ => real,
    }
}

fn rules_now(dir: &Path, home: &Path) -> (Snapshot, Vec<includes::Rule>) {
    let mut cmds = Vec::new();
    let snap = config::load(dir, &mut cmds).unwrap();
    let mut rules = includes::rules(&snap.entries, Some(home));
    includes::read_targets(&mut rules, dir, &mut cmds);
    (snap, rules)
}

#[test]
fn identity_signing_and_config_edits_apply() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = temp_root().join(format!("hadion-identity-{}-{nanos}", std::process::id()));
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let global = home.join(".gitconfig");
    std::fs::write(
        &global,
        "[user]\n\tname = Home Me\n\temail = me@home.example\n[init]\n\tdefaultBranch = main\n",
    )
    .unwrap();
    // SAFETY: this test binary has a single test, so nothing else reads the
    // environment while it is changed.
    unsafe {
        std::env::set_var("GIT_CONFIG_GLOBAL", &global);
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
    }
    assert_eq!(global_config_file().as_deref(), Some(global.as_path()));

    let work = root.join("work");
    let api = work.join("api");
    let site = root.join("play").join("site");
    repo(&api);
    repo(&site);

    // ------------------------------------------------------------------
    // Config edits: global set and unset, a local value in one repository.
    // ------------------------------------------------------------------
    run(
        &[Step::Git(
            config::set_args(Scope::Global, "pull.ff", "only").unwrap(),
        )],
        &api,
    );
    assert_eq!(get(&site, "pull.ff").as_deref(), Some("only"));
    assert!(origin_of(&site, "pull.ff").starts_with("global"));
    run(
        &[Step::Git(
            config::set_args(Scope::Local, "merge.conflictStyle", "zdiff3").unwrap(),
        )],
        &api,
    );
    assert_eq!(get(&api, "merge.conflictstyle").as_deref(), Some("zdiff3"));
    assert_eq!(get(&site, "merge.conflictstyle"), None, "local stays local");
    // The listing shows it with its scope, file and line.
    let mut cmds = Vec::new();
    let snap = config::load(&api, &mut cmds).unwrap();
    let e = snap.effective("merge.conflictStyle").unwrap();
    assert_eq!(e.scope, Scope::Local);
    assert!(e.line.is_some(), "line located in {:?}", e.file);
    let ff = snap.effective("pull.ff").unwrap();
    assert_eq!(ff.scope, Scope::Global);
    assert!(hadion_core::devops::identity::same_path(
        ff.file.as_deref().unwrap(),
        &global
    ));
    assert_eq!(ff.line, Some(7), "in the [pull] section git appended");
    // Overrides: a local pull.ff wins over the global one.
    run(
        &[Step::Git(
            config::set_args(Scope::Local, "pull.ff", "true").unwrap(),
        )],
        &api,
    );
    let snap = config::load(&api, &mut cmds).unwrap();
    let gi = snap
        .entries
        .iter()
        .position(|e| e.key == "pull.ff" && e.scope == Scope::Global)
        .unwrap();
    assert_eq!(
        config::overridden_by(&snap.entries, gi).map(|o| o.scope),
        Some(Scope::Local)
    );
    run(
        &[
            Step::Git(config::unset_args(Scope::Global, "pull.ff", None).unwrap()),
            Step::Git(config::unset_args(Scope::Local, "pull.ff", None).unwrap()),
        ],
        &api,
    );
    assert_eq!(get(&api, "pull.ff"), None);

    // ------------------------------------------------------------------
    // An identity for everything under work/.
    // ------------------------------------------------------------------
    let (_, rules) = rules_now(&api, &home);
    assert!(rules.is_empty());
    let new = NewIdentity {
        folder: slash(&work),
        icase: true,
        name: "Work Me".into(),
        email: "me@corp.example".into(),
        signing_key: String::new(),
        file: includes::identity_file(&home, &includes::slug_for(&slash(&work))),
    };
    let plan = includes::plan_add(&new, &rules, &global, Some(&home)).unwrap();
    assert_eq!(plan.include_value, "~/.gitconfig-work");
    assert!(!plan.file_exists);
    run(&plan.steps, &api);
    assert!(home.join(".gitconfig-work").is_file());
    assert_eq!(get(&api, "user.email").as_deref(), Some("me@corp.example"));
    assert_eq!(get(&api, "user.name").as_deref(), Some("Work Me"));
    assert!(
        origin_of(&api, "user.email").contains(".gitconfig-work"),
        "{}",
        origin_of(&api, "user.email")
    );
    assert_eq!(get(&site, "user.email").as_deref(), Some("me@home.example"));
    // Hadion reads it back: the rule, its file's identity, where it applies.
    let (snap, rules) = rules_now(&api, &home);
    assert_eq!(rules.len(), 1);
    let r = &rules[0];
    assert!(r.exists);
    assert_eq!(r.identity.email.as_deref(), Some("me@corp.example"));
    assert_eq!(r.siblings, 1);
    let mut cmds = Vec::new();
    let a = includes::applied(&api, &rules, &snap.entries, Some(&home), &mut cmds);
    assert!(a.repo);
    assert_eq!(a.matched, vec![0]);
    assert_eq!(
        a.email
            .as_ref()
            .map(|e| e.value_text().to_owned())
            .as_deref(),
        Some("me@corp.example")
    );
    let b = includes::applied(&site, &rules, &snap.entries, Some(&home), &mut cmds);
    assert!(b.matched.is_empty());
    assert_eq!(b.email.unwrap().scope, Scope::Global);
    // A folder that is not a repository yet: Hadion evaluates the pattern.
    let future = work.join("future");
    std::fs::create_dir_all(&future).unwrap();
    let c = includes::applied(&future, &rules, &snap.entries, Some(&home), &mut cmds);
    assert!(!c.repo);
    assert_eq!(c.matched, vec![0]);
    assert_eq!(c.email.unwrap().value_text(), "me@corp.example");
    // A folder that does not exist yet is evaluated the same way.
    let ghost = work.join("not-created-yet");
    let d = includes::applied(&ghost, &rules, &snap.entries, Some(&home), &mut cmds);
    assert!(!d.repo && d.error.is_none());
    assert_eq!(d.matched, vec![0]);
    assert!(!ghost.exists(), "nothing is created");

    // Edit: another address, case-sensitive matching.
    let mut edit = new.clone();
    edit.email = "me2@corp.example".into();
    edit.icase = false;
    let plan = includes::plan_edit(&rules[0], &edit, &global).unwrap();
    run(&plan.steps, &api);
    assert_eq!(get(&api, "user.email").as_deref(), Some("me2@corp.example"));
    let (_, rules) = rules_now(&api, &home);
    assert_eq!(rules.len(), 1);
    assert!(matches!(
        &rules[0].condition,
        includes::Condition::GitDir { icase: false, .. }
    ));
    let text = std::fs::read_to_string(&global).unwrap();
    assert_eq!(text.matches("includeIf").count(), 1, "{text}");

    // Remove, with the file.
    run(&includes::plan_remove(&rules[0], true, &global), &api);
    assert!(!home.join(".gitconfig-work").exists());
    assert_eq!(get(&api, "user.email").as_deref(), Some("me@home.example"));
    let text = std::fs::read_to_string(&global).unwrap();
    assert!(!text.contains("includeIf"), "{text}");

    // ------------------------------------------------------------------
    // SSH signing: a new key, the setup, a signed commit that verifies.
    // ------------------------------------------------------------------
    let Some(keygen) = hadion_core::devops::proc::resolve("ssh-keygen") else {
        eprintln!("ssh-keygen not on PATH: signing part skipped");
        return;
    };
    let keygen = keygen.to_string_lossy().into_owned();
    let ssh_dir = home.join(".ssh");
    let key_path = ssh::next_key_path(&ssh_dir);
    let steps = ssh::plan_generate(&keygen, &key_path, "me@home.example").unwrap();
    assert!(
        matches!(steps[0], Step::MakeDir(_)),
        "the folder is created"
    );
    run(&steps, &api);
    assert!(ssh::plan_generate(&keygen, &key_path, "me@home.example").is_err());
    let mut cmds = Vec::new();
    let keys = ssh::list(&ssh_dir, Some(&keygen), &mut cmds);
    assert_eq!(keys.len(), 1);
    let k = &keys[0];
    assert!(k.has_private);
    assert_eq!(k.kind(), "ED25519");
    assert_eq!(k.fingerprint.comment, "me@home.example");
    assert_eq!(
        Some(k.fingerprint.hash.clone()),
        ssh::fingerprint_of(&k.blob),
        "Hadion computes the same fingerprint as ssh-keygen"
    );
    let setup = signing::ssh_setup(
        k,
        true,
        "me@home.example",
        Scope::Global,
        true,
        &ssh_dir,
        Some(&home),
    );
    run(&signing::plan(&setup).unwrap(), &site);
    assert_eq!(get(&site, "gpg.format").as_deref(), Some("ssh"));
    assert_eq!(get(&site, "commit.gpgsign").as_deref(), Some("true"));
    assert_eq!(get(&site, "tag.gpgsign").as_deref(), Some("true"));
    assert_eq!(
        get(&site, "gpg.ssh.allowedsignersfile").as_deref(),
        Some("~/.ssh/allowed_signers")
    );
    let allowed = std::fs::read_to_string(ssh_dir.join("allowed_signers")).unwrap();
    assert!(allowed.starts_with("me@home.example namespaces=\"git\" ssh-ed25519 "));
    // Running the setup twice does not duplicate the line.
    run(&signing::plan(&setup).unwrap(), &site);
    let again = std::fs::read_to_string(ssh_dir.join("allowed_signers")).unwrap();
    assert_eq!(again.lines().count(), 1);

    std::fs::write(site.join("a.txt"), "hello\n").unwrap();
    assert!(git(&site, &["add", "a.txt"]).0);
    let (ok, out) = git(&site, &["commit", "-q", "-m", "signed"]);
    assert!(ok, "signed commit: {out}");
    let (ok, out) = git(&site, &["verify-commit", "-v", "HEAD"]);
    assert!(ok, "verify-commit: {out}");
    assert!(out.contains("Good \"git\" signature"), "{out}");
    assert!(out.contains("me@home.example"), "{out}");

    let _ = std::fs::remove_dir_all(&root);
}
