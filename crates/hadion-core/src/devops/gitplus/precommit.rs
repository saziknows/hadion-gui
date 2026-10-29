//! Other checkers run from the Hooks tab: the `pre-commit` framework
//! (`.pre-commit-config.yaml`, `pre-commit run`) and `semgrep` (`semgrep
//! scan --config auto --json --output FILE`, parsed into a summary).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The pre-commit configuration file, when the repository has one.
pub fn config_path(dir: &Path) -> Option<PathBuf> {
    [".pre-commit-config.yaml", ".pre-commit-config.yml"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file())
}

/// A repository of hooks in the pre-commit configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigRepo {
    /// `https://github.com/pre-commit/pre-commit-hooks`, `local`, `meta`.
    pub repo: String,
    pub rev: Option<String>,
    pub hooks: Vec<String>,
}

#[derive(Deserialize)]
struct RawConfig {
    #[serde(default)]
    repos: Vec<RawRepo>,
}

#[derive(Deserialize)]
struct RawRepo {
    #[serde(default)]
    repo: String,
    #[serde(default)]
    rev: Option<String>,
    #[serde(default)]
    hooks: Vec<RawHook>,
}

#[derive(Deserialize)]
struct RawHook {
    #[serde(default)]
    id: String,
}

/// The repositories and hook ids of a `.pre-commit-config.yaml`.
pub fn parse_config(text: &str) -> Result<Vec<ConfigRepo>, String> {
    let raw: RawConfig =
        serde_yaml::from_str(text).map_err(|e| format!("Unreadable configuration: {e}"))?;
    Ok(raw
        .repos
        .into_iter()
        .map(|r| ConfigRepo {
            repo: r.repo,
            rev: r.rev,
            hooks: r.hooks.into_iter().map(|h| h.id).collect(),
        })
        .collect())
}

/// `pre-commit run --all-files --color always`.
pub fn run_all_args() -> Vec<String> {
    ["run", "--all-files", "--color", "always"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

/// `pre-commit run --color always --files F…`.
pub fn run_files_args(files: &[String]) -> Vec<String> {
    let mut v: Vec<String> = ["run", "--color", "always", "--files"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    v.extend(files.iter().cloned());
    v
}

/// How one pre-commit hook ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Failed,
    Skipped,
}

/// One result line of `pre-commit run`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookResult {
    pub name: String,
    pub outcome: Outcome,
    /// `(no files to check)`.
    pub note: String,
}

/// The `Name......Passed` lines of `pre-commit run` (escapes stripped).
pub fn parse_run(text: &str) -> Vec<HookResult> {
    let mut out = Vec::new();
    for l in text.lines() {
        let l = l.trim_end();
        let (outcome, rest) = if let Some(r) = l.strip_suffix("Passed") {
            (Outcome::Passed, r)
        } else if let Some(r) = l.strip_suffix("Failed") {
            (Outcome::Failed, r)
        } else if let Some(r) = l.strip_suffix("Skipped") {
            (Outcome::Skipped, r)
        } else {
            continue;
        };
        // `Name....(no files to check)` or `Name.......`
        let (rest, note) = match rest.rfind('(') {
            Some(i) if rest.ends_with(')') => (&rest[..i], rest[i..].to_owned()),
            _ => (rest, String::new()),
        };
        if !rest.ends_with("...") {
            continue;
        }
        let name = rest.trim_end_matches('.').trim().to_owned();
        if name.is_empty() {
            continue;
        }
        out.push(HookResult {
            name,
            outcome,
            note,
        });
    }
    out
}

/// `semgrep scan --config auto --json --output FILE --metrics on`: the
/// rules come from semgrep.dev (network), which `--config auto` needs
/// metrics for.
pub fn semgrep_args(out: &Path) -> Vec<String> {
    vec![
        "scan".into(),
        "--config".into(),
        "auto".into(),
        "--json".into(),
        "--output".into(),
        out.to_string_lossy().into_owned(),
        "--metrics".into(),
        "on".into(),
    ]
}

/// One semgrep result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemgrepFinding {
    pub rule: String,
    pub path: String,
    pub line: u64,
    /// `ERROR`, `WARNING`, `INFO` (or the newer `CRITICAL` … `LOW`).
    pub severity: String,
    pub message: String,
}

/// What a semgrep run found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SemgrepSummary {
    pub findings: Vec<SemgrepFinding>,
    /// Severity and count, most severe first.
    pub by_severity: Vec<(String, usize)>,
    /// Rule and count, most frequent first.
    pub top_rules: Vec<(String, usize)>,
    pub errors: Vec<String>,
    pub scanned: usize,
    pub version: Option<String>,
}

#[derive(Deserialize)]
struct SgOut {
    #[serde(default)]
    results: Vec<SgResult>,
    #[serde(default)]
    errors: Vec<serde_json::Value>,
    #[serde(default)]
    paths: Option<SgPaths>,
    #[serde(default)]
    version: Option<String>,
}

#[derive(Deserialize)]
struct SgPaths {
    #[serde(default)]
    scanned: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct SgResult {
    #[serde(default)]
    check_id: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    start: Option<SgPos>,
    #[serde(default)]
    extra: Option<SgExtra>,
}

#[derive(Deserialize)]
struct SgPos {
    #[serde(default)]
    line: u64,
}

#[derive(Deserialize)]
struct SgExtra {
    #[serde(default)]
    message: String,
    #[serde(default)]
    severity: String,
}

/// Rank of a semgrep severity (higher is worse).
pub fn severity_rank(s: &str) -> u8 {
    match s.to_ascii_uppercase().as_str() {
        "CRITICAL" => 5,
        "ERROR" | "HIGH" => 4,
        "WARNING" | "MEDIUM" => 3,
        "INFO" | "LOW" => 2,
        "INVENTORY" | "EXPERIMENT" => 1,
        _ => 0,
    }
}

/// Parse `semgrep --json` output.
pub fn parse_semgrep(bytes: &[u8]) -> Result<SemgrepSummary, String> {
    let v: SgOut =
        serde_json::from_slice(bytes).map_err(|e| format!("Unreadable semgrep output: {e}"))?;
    let mut findings: Vec<SemgrepFinding> = v
        .results
        .into_iter()
        .map(|r| {
            let extra = r.extra.unwrap_or(SgExtra {
                message: String::new(),
                severity: String::new(),
            });
            SemgrepFinding {
                rule: r.check_id,
                path: r.path.replace('\\', "/"),
                line: r.start.map(|p| p.line).unwrap_or(0),
                severity: if extra.severity.is_empty() {
                    "INFO".to_owned()
                } else {
                    extra.severity.to_ascii_uppercase()
                },
                message: extra.message.trim().to_owned(),
            }
        })
        .collect();
    findings.sort_by(|a, b| {
        severity_rank(&b.severity)
            .cmp(&severity_rank(&a.severity))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
    let mut sev: HashMap<String, usize> = HashMap::new();
    let mut rules: HashMap<String, usize> = HashMap::new();
    for f in &findings {
        *sev.entry(f.severity.clone()).or_default() += 1;
        *rules.entry(f.rule.clone()).or_default() += 1;
    }
    let mut by_severity: Vec<(String, usize)> = sev.into_iter().collect();
    by_severity.sort_by_key(|s| std::cmp::Reverse(severity_rank(&s.0)));
    let mut top_rules: Vec<(String, usize)> = rules.into_iter().collect();
    top_rules.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let errors = v
        .errors
        .iter()
        .map(|e| {
            e.get("message")
                .and_then(|m| m.as_str())
                .map(|m| m.lines().next().unwrap_or(m).to_owned())
                .unwrap_or_else(|| e.to_string().chars().take(200).collect())
        })
        .collect();
    Ok(SemgrepSummary {
        findings,
        by_severity,
        top_rules,
        errors,
        scanned: v.paths.map(|p| p.scanned.len()).unwrap_or(0),
        version: v.version,
    })
}

/// A fresh file for semgrep's JSON report.
pub fn report_path() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "hadion-semgrep-{}-{nanos}.json",
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_config() {
        let yaml = "repos:\n  - repo: https://github.com/pre-commit/pre-commit-hooks\n    rev: v4.6.0\n    hooks:\n      - id: trailing-whitespace\n      - id: end-of-file-fixer\n  - repo: local\n    hooks:\n      - id: cargo-fmt\n        name: cargo fmt\n        entry: cargo fmt --\n        language: system\n";
        let v = parse_config(yaml).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].rev.as_deref(), Some("v4.6.0"));
        assert_eq!(v[0].hooks, vec!["trailing-whitespace", "end-of-file-fixer"]);
        assert_eq!(v[1].repo, "local");
        assert!(parse_config("repos: [").is_err());
    }

    #[test]
    fn reads_run_output() {
        let text = "Trim Trailing Whitespace.................................................Passed\nFix End of Files.........................................................Failed\n- hook id: end-of-file-fixer\n- exit code: 1\nCheck Yaml...........................................(no files to check)Skipped\nrandom line\n";
        let v = parse_run(text);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].name, "Trim Trailing Whitespace");
        assert_eq!(v[1].outcome, Outcome::Failed);
        assert_eq!(v[2].outcome, Outcome::Skipped);
        assert_eq!(v[2].note, "(no files to check)");
        assert_eq!(run_files_args(&["a.rs".into()]).last().unwrap(), "a.rs");
    }

    #[test]
    fn summarises_semgrep_json() {
        let json = br#"{"version":"1.90.0","results":[
          {"check_id":"python.lang.security.eval","path":"src/a.py","start":{"line":3,"col":1},"end":{"line":3,"col":9},"extra":{"message":"Avoid eval","severity":"ERROR"}},
          {"check_id":"generic.secrets.key","path":"src\\b.py","start":{"line":10},"extra":{"message":"Key","severity":"WARNING"}},
          {"check_id":"python.lang.security.eval","path":"src/c.py","start":{"line":1},"extra":{"message":"Avoid eval","severity":"ERROR"}}
        ],"errors":[{"message":"Syntax error in x.js\nmore"}],"paths":{"scanned":["src/a.py","src/b.py","src/c.py"]}}"#;
        let s = parse_semgrep(json).unwrap();
        assert_eq!(s.findings.len(), 3);
        assert_eq!(s.findings[0].severity, "ERROR");
        assert_eq!(
            s.by_severity,
            vec![("ERROR".into(), 2), ("WARNING".into(), 1)]
        );
        assert_eq!(s.top_rules[0], ("python.lang.security.eval".into(), 2));
        assert_eq!(s.findings[2].path, "src/b.py");
        assert_eq!(s.errors, vec!["Syntax error in x.js"]);
        assert_eq!(s.scanned, 3);
        assert_eq!(s.version.as_deref(), Some("1.90.0"));
        assert!(parse_semgrep(b"not json").is_err());
        let args = semgrep_args(Path::new("/t/r.json"));
        assert_eq!(&args[..3], &["scan", "--config", "auto"]);
    }
}
