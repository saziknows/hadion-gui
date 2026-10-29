//! Terraform root detection and "which roots do my changes touch" against
//! repositories built with the real git binary.

use std::path::{Path, PathBuf};
use std::process::Command;

use hadion_core::devops::runtime::terraform::{self, find_roots, touched, unpushed_changes};

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp(tag: &str) -> Dir {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "hadion-runtime-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    Dir(root)
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
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

fn init(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    for (k, v) in [
        ("user.name", "Test User"),
        ("user.email", "test@example.com"),
        ("commit.gpgsign", "false"),
        ("core.autocrlf", "false"),
    ] {
        git(dir, &["config", k, v]);
    }
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

const LIVE_MAIN: &str = "module \"vpc\" {\n  source = \"../../modules/vpc\"\n}\n";
const LIVE_BACKEND: &str = "terraform {\n  required_version = \">= 1.6\"\n  backend \"s3\" {\n    bucket = \"state\"\n  }\n}\n";
const VPC: &str = "resource \"aws_vpc\" \"main\" {\n  cidr_block = \"10.0.0.0/16\"\n}\n";

#[test]
fn roots_come_from_tracked_and_untracked_files_but_not_ignored_ones() {
    let d = temp("roots");
    let r = &d.0;
    init(r);
    write(r, "infra/live/main.tf", LIVE_MAIN);
    write(r, "infra/live/backend.tf", LIVE_BACKEND);
    write(r, "modules/vpc/main.tf", VPC);
    write(r, ".gitignore", "scratch/\n.terraform/\n");
    write(r, "README.md", "demo\n");
    git(r, &["add", "-A"]);
    git(r, &["commit", "-q", "-m", "infra"]);
    // Untracked (not ignored): found. Ignored and .terraform/: not.
    write(
        r,
        "envs/dev/main.tf",
        "terraform {\n  backend \"local\" {}\n}\n",
    );
    write(r, "scratch/main.tf", "resource \"x\" \"y\" {}\n");
    write(r, "infra/live/.terraform/modules/vpc/main.tf", VPC);
    let scan = find_roots(r).unwrap();
    let roots: Vec<&str> = scan.roots.iter().map(|x| x.info.dir.as_str()).collect();
    assert_eq!(roots, ["envs/dev", "infra/live"]);
    let modules: Vec<&str> = scan.modules.iter().map(|m| m.dir.as_str()).collect();
    assert_eq!(modules, ["modules/vpc"]);
    let live = &scan.roots[1];
    assert_eq!(live.info.backend.as_deref(), Some("s3"));
    assert_eq!(live.info.required_version.as_deref(), Some(">= 1.6"));
    assert_eq!(live.module_dirs, ["modules/vpc"]);
    assert!(
        live.info.initialized,
        "the .terraform/ directory exists on disk"
    );
    assert_eq!(scan.files_read, 4);
}

#[test]
fn unpushed_commits_and_the_working_tree_touch_roots() {
    let d = temp("touch");
    let source = d.0.join("source");
    std::fs::create_dir_all(&source).unwrap();
    init(&source);
    write(&source, "infra/live/main.tf", LIVE_MAIN);
    write(&source, "infra/live/backend.tf", LIVE_BACKEND);
    write(&source, "modules/vpc/main.tf", VPC);
    write(&source, "app/main.go", "package main\n");
    git(&source, &["add", "-A"]);
    git(&source, &["commit", "-q", "-m", "infra"]);
    // A clone has an upstream (origin/main) without pushing anywhere.
    git(&d.0, &["clone", "-q", "source", "work"]);
    let work = d.0.join("work");
    for (k, v) in [
        ("user.name", "Test User"),
        ("user.email", "test@example.com"),
        ("commit.gpgsign", "false"),
    ] {
        git(&work, &["config", k, v]);
    }
    assert!(unpushed_changes(&work).unwrap().is_empty());
    write(&work, "modules/vpc/main.tf", &format!("{VPC}# tags\n"));
    write(&work, "app/main.go", "package main // changed\n");
    git(&work, &["commit", "-qam", "Change the module and the app"]);
    let mut files = unpushed_changes(&work).unwrap();
    files.sort();
    assert_eq!(files, ["app/main.go", "modules/vpc/main.tf"]);
    let scan = find_roots(&work).unwrap();
    let t = touched(&scan.roots, &files);
    assert_eq!(t.len(), 1);
    assert_eq!(scan.roots[t[0].0].info.dir, "infra/live");
    assert_eq!(t[0].1, ["modules/vpc/main.tf"]);
    // Without an upstream the question has no answer.
    assert!(unpushed_changes(&source).is_err());
}

#[test]
fn plan_files_live_in_the_temp_directory() {
    let p = terraform::plan_file("infra/live");
    assert!(p.starts_with(std::env::temp_dir()));
    assert_ne!(p, terraform::plan_file("infra/live"), "fresh per run");
}
