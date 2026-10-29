//! The Govern page's local reads against a repository built with the real
//! git binary: tracked files, the lockfile SBOM, Dockerfile lint, CODEOWNERS
//! coverage and the workflow secret references.

use std::path::{Path, PathBuf};
use std::process::Command;

use hadion_core::devops::govern::{codeowners, config, docker, sbom, tracked_files};

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new() -> Repo {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("hadion-govern-it-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let r = Repo { root };
        r.git(&["init", "-q", "-b", "main"]);
        for (k, v) in [
            ("user.name", "Test User"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.autocrlf", "false"),
        ] {
            r.git(&["config", k, v]);
        }
        r
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn local_reads_on_a_real_repository() {
    let r = Repo::new();
    r.write(
        ".github/CODEOWNERS",
        "* @acme/core\n/web/ @acme/web\n/legacy/ @acme/core\n!secret.txt @x\n",
    );
    r.write(
        "web/package-lock.json",
        r#"{"lockfileVersion":3,"packages":{"":{},"node_modules/@scope/pkg":{"version":"1.2.3"},"node_modules/lodash":{"version":"4.17.21"}}}"#,
    );
    r.write("Cargo.lock", "[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n");
    r.write(
        "Dockerfile",
        "FROM node:20\nENV API_TOKEN=x\nCOPY . .\nRUN npm ci\n",
    );
    r.write(
        "docker-compose.yml",
        "services:\n  db:\n    image: postgres\n",
    );
    r.write(
        ".github/workflows/ci.yml",
        "jobs:\n  a:\n    steps:\n      - run: echo ${{ secrets.NPM_TOKEN }}\n",
    );
    r.write("web/index.js", "x\n");
    r.write("untracked-later.txt", "x\n");
    r.git(&["add", "-A"]);
    r.git(&["rm", "-q", "--cached", "untracked-later.txt"]);
    r.git(&["commit", "-qm", "init"]);

    let mut commands = Vec::new();
    let tracked = tracked_files(r.path(), &mut commands).unwrap();
    assert_eq!(commands, vec!["git ls-files -z"]);
    assert!(tracked.contains(&"web/package-lock.json".to_string()));
    assert!(!tracked.contains(&"untracked-later.txt".to_string()));

    // SBOM: both lockfiles, a scoped npm package with its purl.
    let s = sbom::local_sbom(r.path(), &tracked);
    assert_eq!(s.lockfiles.len(), 2);
    assert_eq!(s.unique().len(), 3);
    let scoped = s.packages.iter().find(|p| p.name == "@scope/pkg").unwrap();
    assert_eq!(sbom::purl(scoped), "pkg:npm/%40scope/pkg@1.2.3");
    let doc = sbom::cyclonedx("demo", &s.packages, "u", "2026-10-26T00:00:00Z", "0");
    assert_eq!(doc["components"].as_array().unwrap().len(), 3);

    // Images: the Dockerfile and the compose service.
    let images = docker::scan_tree(r.path(), &tracked);
    assert_eq!(images.dockerfiles.len(), 1);
    let rules: Vec<&str> = images.dockerfiles[0].1.iter().map(|f| f.rule).collect();
    for rule in [
        "secret-variable",
        "copy-before-install",
        "root-user",
        "no-healthcheck",
    ] {
        assert!(rules.contains(&rule), "{rule} missing from {rules:?}");
    }
    assert_eq!(images.compose.len(), 1);
    assert_eq!(images.images(), vec!["node:20", "postgres"]);

    // CODEOWNERS: found where GitHub looks first; coverage over the index.
    assert_eq!(codeowners::locate(r.path()), vec![".github/CODEOWNERS"]);
    let text = std::fs::read_to_string(r.path().join(".github/CODEOWNERS")).unwrap();
    let file = codeowners::parse(".github/CODEOWNERS", &text);
    assert_eq!(file.errors(), 1); // the negation
    let a = codeowners::analyze(&file, &tracked).unwrap();
    assert_eq!(a.files, tracked.len());
    assert_eq!(a.owned, tracked.len());
    assert!(a.issues.iter().any(|i| i.kind == "no-match" && i.line == 3));

    // Workflow references.
    let refs = config::workflow_secret_refs(r.path());
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].0, "NPM_TOKEN");
}
