//! GitHub Actions secrets (names and dates only) and variables, at
//! repository and environment level, plus the organisation secrets and
//! variables shared with the repository (read-only).
//!
//! A secret value only ever travels on the standard input of
//! `gh secret set NAME`: [`secret_set`] returns the arguments and the
//! standard input separately, and its `Debug` never prints the value.

use serde::Deserialize;

use crate::Result;
use crate::devops::gh::{Gh, Request, encode};
use crate::github::{ApiCall, parse_time};

/// Where a secret or variable lives.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum Scope {
    #[default]
    Repository,
    Environment(String),
    /// Shared with the repository by its organisation (read-only here).
    Organization,
}

impl Scope {
    pub fn label(&self) -> String {
        match self {
            Scope::Repository => "Repository".into(),
            Scope::Environment(e) => format!("Environment {e}"),
            Scope::Organization => "Organisation".into(),
        }
    }

    pub fn env(&self) -> Option<&str> {
        match self {
            Scope::Environment(e) => Some(e),
            _ => None,
        }
    }
}

/// A secret's metadata (GitHub never returns the value).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Secret {
    pub name: String,
    pub scope: Scope,
    pub created: Option<i64>,
    pub updated: Option<i64>,
    /// Organisation secrets: `all`, `private` or `selected`.
    pub visibility: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub scope: Scope,
    pub created: Option<i64>,
    pub updated: Option<i64>,
    pub visibility: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSecret {
    name: String,
    created_at: Option<String>,
    updated_at: Option<String>,
    visibility: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawVariable {
    name: String,
    value: String,
    created_at: Option<String>,
    updated_at: Option<String>,
    visibility: Option<String>,
}

fn t(s: &Option<String>) -> Option<i64> {
    s.as_deref().and_then(parse_time)
}

/// The endpoint listing secrets of a scope.
pub fn secrets_endpoint(gh: &Gh, scope: &Scope) -> String {
    match scope {
        Scope::Repository => gh.path("actions/secrets"),
        Scope::Environment(e) => gh.path(&format!("environments/{}/secrets", encode(e))),
        Scope::Organization => gh.path("actions/organization-secrets"),
    }
}

/// The endpoint listing variables of a scope.
pub fn variables_endpoint(gh: &Gh, scope: &Scope) -> String {
    match scope {
        Scope::Repository => gh.path("actions/variables"),
        Scope::Environment(e) => gh.path(&format!("environments/{}/variables", encode(e))),
        Scope::Organization => gh.path("actions/organization-variables"),
    }
}

/// GET a wrapped list (`{"total_count": n, key: [...]}`) page by page.
fn pages<T: serde::de::DeserializeOwned>(
    gh: &Gh,
    endpoint: &str,
    key: &str,
    per_page: usize,
    max_pages: usize,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<T>> {
    let sep = if endpoint.contains('?') { '&' } else { '?' };
    let mut all = Vec::new();
    for page in 1..=max_pages {
        let v: serde_json::Value = gh.get_json(
            &format!("{endpoint}{sep}per_page={per_page}&page={page}"),
            key,
            calls,
        )?;
        let total = v.get("total_count").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
        let items: Vec<T> = serde_json::from_value(v.get(key).cloned().unwrap_or_default())
            .map_err(|e| crate::Error::new(format!("GitHub {key}: {e}")))?;
        let n = items.len();
        all.extend(items);
        if n < per_page || all.len() >= total {
            break;
        }
    }
    Ok(all)
}

/// Secrets of one scope (up to 300).
pub fn fetch_secrets(gh: &Gh, scope: &Scope, calls: &mut Vec<ApiCall>) -> Result<Vec<Secret>> {
    let raw: Vec<RawSecret> = pages(gh, &secrets_endpoint(gh, scope), "secrets", 100, 3, calls)?;
    let mut v: Vec<Secret> = raw
        .into_iter()
        .map(|s| Secret {
            created: t(&s.created_at),
            updated: t(&s.updated_at),
            name: s.name,
            scope: scope.clone(),
            visibility: s.visibility,
        })
        .collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(v)
}

/// Variables of one scope (up to 300).
pub fn fetch_variables(gh: &Gh, scope: &Scope, calls: &mut Vec<ApiCall>) -> Result<Vec<Variable>> {
    // The variables endpoints serve at most 30 per page.
    let raw: Vec<RawVariable> = pages(
        gh,
        &variables_endpoint(gh, scope),
        "variables",
        30,
        10,
        calls,
    )?;
    let mut v: Vec<Variable> = raw
        .into_iter()
        .map(|s| Variable {
            created: t(&s.created_at),
            updated: t(&s.updated_at),
            name: s.name,
            value: s.value,
            scope: scope.clone(),
            visibility: s.visibility,
        })
        .collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(v)
}

/// Why a secret or variable name is not accepted, or `None` when it is.
/// GitHub's rules: letters, digits and underscores only, not starting with
/// a digit or with `GITHUB_`. Names are stored upper-case.
pub fn name_problem(name: &str) -> Option<String> {
    let n = name.trim();
    if n.is_empty() {
        return Some("Enter a name.".into());
    }
    if let Some(c) = n
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '_'))
    {
        return Some(format!(
            "`{c}` is not allowed: use letters A–Z, digits and underscores."
        ));
    }
    if n.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return Some("A name cannot start with a digit.".into());
    }
    if n.to_ascii_uppercase().starts_with("GITHUB_") {
        return Some("Names starting with GITHUB_ are reserved by GitHub.".into());
    }
    if n.len() > 255 {
        return Some("A name can be at most 255 characters.".into());
    }
    None
}

/// The name as GitHub stores it (upper-case).
pub fn normalize_name(name: &str) -> String {
    name.trim().to_ascii_uppercase()
}

/// `gh secret set` for one secret: the arguments and, separately, the
/// value for standard input.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretSet {
    pub args: Vec<String>,
    pub stdin: Vec<u8>,
}

impl std::fmt::Debug for SecretSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretSet")
            .field("args", &self.args)
            .field(
                "stdin",
                &format_args!("<{} bytes withheld>", self.stdin.len()),
            )
            .finish()
    }
}

impl SecretSet {
    /// The command as shown in confirmations: never the value.
    pub fn display(&self) -> String {
        secret_set_preview(
            &self.args,
            String::from_utf8_lossy(&self.stdin).chars().count(),
        )
    }

    /// Overwrite the value in memory.
    pub fn wipe(&mut self) {
        self.stdin.fill(0);
        self.stdin.clear();
    }
}

/// `gh secret set NAME --repo o/r   (value from standard input, N characters, not shown)`.
pub fn secret_set_preview(args: &[String], chars: usize) -> String {
    format!(
        "gh {}   (value from standard input, {chars} characters, not shown)",
        crate::devops::proc::render_args(&args.iter().map(String::as_str).collect::<Vec<_>>())
    )
}

/// The arguments of [`secret_set`] alone (for previews before the value is
/// taken out of the form).
pub fn secret_set_args(full_name: &str, name: &str, env: Option<&str>) -> Vec<String> {
    secret_set(full_name, name, env, Vec::new()).args
}

/// `secrets.NAME` references in `.github/workflows/*.yml`: `(name, files)`,
/// sorted by name. `GITHUB_TOKEN` is left out (GitHub provides it).
pub fn workflow_secret_refs(workdir: &std::path::Path) -> Vec<(String, Vec<String>)> {
    use std::collections::BTreeMap;
    let re = regex::Regex::new(r"secrets\.([A-Za-z_][A-Za-z0-9_]*)").expect("valid regex");
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let dir = workdir.join(".github").join("workflows");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".yml") || n.ends_with(".yaml"))
        .collect();
    files.sort();
    for f in files {
        let Ok(text) = std::fs::read_to_string(dir.join(&f)) else {
            continue;
        };
        let rel = format!(".github/workflows/{f}");
        for c in re.captures_iter(&text) {
            let name = c[1].to_ascii_uppercase();
            if name == "GITHUB_TOKEN" {
                continue;
            }
            let v = map.entry(name).or_default();
            if !v.contains(&rel) {
                v.push(rel.clone());
            }
        }
    }
    map.into_iter().collect()
}

/// `gh secret set NAME --repo o/r [--env E]` with the value on standard
/// input (gh reads it from there when standard input is not a terminal).
pub fn secret_set(full_name: &str, name: &str, env: Option<&str>, value: Vec<u8>) -> SecretSet {
    let mut args = vec![
        "secret".to_owned(),
        "set".to_owned(),
        normalize_name(name),
        "--repo".to_owned(),
        full_name.to_owned(),
    ];
    if let Some(e) = env.filter(|e| !e.is_empty()) {
        args.push("--env".into());
        args.push(e.to_owned());
    }
    SecretSet { args, stdin: value }
}

/// `gh secret delete NAME --repo o/r [--env E]`.
pub fn secret_delete_args(full_name: &str, name: &str, env: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "secret".to_owned(),
        "delete".to_owned(),
        name.to_owned(),
        "--repo".to_owned(),
        full_name.to_owned(),
    ];
    if let Some(e) = env.filter(|e| !e.is_empty()) {
        args.push("--env".into());
        args.push(e.to_owned());
    }
    args
}

/// `POST …/variables {name, value}`.
pub fn variable_create(gh: &Gh, scope: &Scope, name: &str, value: &str) -> Request {
    Request::post(
        variables_endpoint(gh, scope),
        serde_json::json!({ "name": normalize_name(name), "value": value }),
    )
}

/// `PATCH …/variables/{name} {name, value}`; a new name renames it.
pub fn variable_update(gh: &Gh, scope: &Scope, name: &str, new_name: &str, value: &str) -> Request {
    Request::patch(
        format!("{}/{}", variables_endpoint(gh, scope), encode(name)),
        serde_json::json!({ "name": normalize_name(new_name), "value": value }),
    )
}

/// `DELETE …/variables/{name}`.
pub fn variable_delete(gh: &Gh, scope: &Scope, name: &str) -> Request {
    Request::delete(format!(
        "{}/{}",
        variables_endpoint(gh, scope),
        encode(name)
    ))
}

/// Secret names a workflow references but no scope defines.
pub fn missing_secrets(referenced: &[String], defined: &[Secret]) -> Vec<String> {
    referenced
        .iter()
        .filter(|r| r.as_str() != "GITHUB_TOKEN")
        .filter(|r| !defined.iter().any(|s| s.name.eq_ignore_ascii_case(r)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::gh::Method;

    #[test]
    fn secret_set_keeps_the_value_out_of_argv() {
        let value = b"s3cr3t-value-123".to_vec();
        let s = secret_set("o/r", "deploy_key", Some("production"), value.clone());
        assert_eq!(
            s.args,
            vec![
                "secret",
                "set",
                "DEPLOY_KEY",
                "--repo",
                "o/r",
                "--env",
                "production"
            ]
        );
        assert!(!s.args.iter().any(|a| a.contains("s3cr3t")));
        assert_eq!(s.stdin, value);
        let shown = s.display();
        assert!(!shown.contains("s3cr3t"), "{shown}");
        assert!(shown.contains("16 characters, not shown"), "{shown}");
        let dbg = format!("{s:?}");
        assert!(!dbg.contains("s3cr3t"), "{dbg}");
        assert!(dbg.contains("16 bytes withheld"));
        let mut s = secret_set("o/r", "X", None, b"abc".to_vec());
        assert_eq!(s.args, vec!["secret", "set", "X", "--repo", "o/r"]);
        s.wipe();
        assert!(s.stdin.is_empty());
        assert_eq!(
            secret_delete_args("o/r", "X", Some("staging")),
            vec!["secret", "delete", "X", "--repo", "o/r", "--env", "staging"]
        );
        assert_eq!(
            secret_delete_args("o/r", "X", None),
            vec!["secret", "delete", "X", "--repo", "o/r"]
        );
    }

    #[test]
    fn workflow_refs() {
        let dir = std::env::temp_dir().join(format!("gitvis-govern-wf-{}", std::process::id()));
        let wf = dir.join(".github/workflows");
        std::fs::create_dir_all(&wf).unwrap();
        std::fs::write(
            wf.join("ci.yml"),
            "env:\n  A: ${{ secrets.NPM_TOKEN }}\n  B: ${{ secrets.GITHUB_TOKEN }}\n  C: ${{secrets.deploy_key}}\n",
        )
        .unwrap();
        std::fs::write(wf.join("release.yaml"), "x: ${{ secrets.NPM_TOKEN }}\n").unwrap();
        std::fs::write(wf.join("notes.md"), "secrets.IGNORED").unwrap();
        let refs = workflow_secret_refs(&dir);
        assert_eq!(
            refs,
            vec![
                (
                    "DEPLOY_KEY".to_string(),
                    vec![".github/workflows/ci.yml".to_string()]
                ),
                (
                    "NPM_TOKEN".to_string(),
                    vec![
                        ".github/workflows/ci.yml".to_string(),
                        ".github/workflows/release.yaml".to_string()
                    ]
                ),
            ]
        );
        assert_eq!(
            secret_set_preview(&secret_set_args("o/r", "x", None), 5),
            "gh secret set X --repo o/r   (value from standard input, 5 characters, not shown)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names() {
        assert_eq!(name_problem("API_TOKEN"), None);
        assert_eq!(name_problem("api_token_2"), None);
        assert!(name_problem("").is_some());
        assert!(name_problem("1ST").unwrap().contains("digit"));
        assert!(name_problem("GITHUB_TOKEN").unwrap().contains("reserved"));
        assert!(name_problem("github_x").unwrap().contains("reserved"));
        assert!(name_problem("MY-SECRET").unwrap().contains("`-`"));
        assert!(name_problem("MY SECRET").is_some());
        assert!(name_problem("ÄPI").is_some());
        assert_eq!(normalize_name(" api_key "), "API_KEY");
    }

    #[test]
    fn variable_requests() {
        let gh = Gh::new(".", "o", "r");
        let r = variable_create(&gh, &Scope::Repository, "region", "eu-west-1");
        assert_eq!(r.method, Method::Post);
        assert_eq!(r.endpoint, "repos/o/r/actions/variables");
        assert_eq!(
            r.body.unwrap(),
            serde_json::json!({"name": "REGION", "value": "eu-west-1"})
        );
        let env = Scope::Environment("prod eu".into());
        let r = variable_update(&gh, &env, "REGION", "REGION", "eu-central-1");
        assert_eq!(r.method, Method::Patch);
        assert_eq!(
            r.endpoint,
            "repos/o/r/environments/prod%20eu/variables/REGION"
        );
        assert_eq!(
            r.body.unwrap(),
            serde_json::json!({"name": "REGION", "value": "eu-central-1"})
        );
        let r = variable_delete(&gh, &Scope::Repository, "REGION");
        assert_eq!(r.method, Method::Delete);
        assert_eq!(r.endpoint, "repos/o/r/actions/variables/REGION");
        assert!(r.body.is_none());
        assert_eq!(
            secrets_endpoint(&gh, &Scope::Organization),
            "repos/o/r/actions/organization-secrets"
        );
        assert_eq!(
            secrets_endpoint(&gh, &Scope::Environment("prod".into())),
            "repos/o/r/environments/prod/secrets"
        );
        assert_eq!(
            variables_endpoint(&gh, &Scope::Organization),
            "repos/o/r/actions/organization-variables"
        );
    }

    #[test]
    fn missing() {
        let defined = vec![Secret {
            name: "NPM_TOKEN".into(),
            scope: Scope::Repository,
            created: None,
            updated: None,
            visibility: None,
        }];
        let refs = vec!["NPM_TOKEN".into(), "GITHUB_TOKEN".into(), "AWS_KEY".into()];
        assert_eq!(
            missing_secrets(&refs, &defined),
            vec!["AWS_KEY".to_string()]
        );
    }
}
