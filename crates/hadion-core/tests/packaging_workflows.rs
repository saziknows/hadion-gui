//! The repository's own workflows (`.github/workflows/ci.yml` and
//! `release.yml`) pass Hadion's workflow linter with no finding at all, and
//! keep the properties the release depends on.

use std::path::PathBuf;

use hadion_core::devops::ci::lint::lint;

fn workflows_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(".github")
        .join("workflows")
}

fn read(name: &str) -> String {
    let p = workflows_dir().join(name);
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
        .replace("\r\n", "\n")
}

#[test]
fn workflows_lint_clean() {
    let mut all = Vec::new();
    for name in ["ci.yml", "release.yml"] {
        let findings = lint(&format!(".github/workflows/{name}"), &read(name));
        for f in &findings {
            eprintln!(
                "{}:{}:{} {} [{}] {} ({})",
                f.file,
                f.line,
                f.column,
                f.severity.label(),
                f.rule,
                f.message,
                f.hint
            );
        }
        all.extend(findings);
    }
    assert!(
        all.is_empty(),
        "{} lint findings (printed above)",
        all.len()
    );
}

#[test]
fn actions_are_pinned_and_permissions_are_minimal() {
    for name in ["ci.yml", "release.yml"] {
        let text = read(name);
        for line in text.lines().filter(|l| {
            l.trim_start().starts_with("- uses:") || l.trim_start().starts_with("uses:")
        }) {
            let spec = line.split("uses:").nth(1).unwrap().trim();
            let (_, rest) = spec.split_once('@').expect("pinned with @");
            let sha: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
            assert_eq!(sha.len(), 40, "{name}: {line}");
            assert!(sha.chars().all(|c| c.is_ascii_hexdigit()), "{name}: {line}");
            assert!(
                rest.contains("# v"),
                "{name}: the version as a comment: {line}"
            );
        }
        assert!(text.contains("permissions:\n  contents: read"), "{name}");
    }
    let release = read("release.yml");
    assert_eq!(release.matches("contents: write").count(), 1);
    assert!(release.contains("tags: ['v*']"));
    assert!(release.contains("gh release upload"));
    assert!(release.contains("GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}"));
    // No expression is spliced into a script: values go through env.
    for line in release.lines().filter(|l| l.contains("${{")) {
        let t = line.trim_start();
        assert!(
            !t.starts_with("run:") && !t.starts_with("gh ") && !t.starts_with('$'),
            "expression inside a script: {line}"
        );
    }
}
