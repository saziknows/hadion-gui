//! Workflow files in the working tree: name, triggers, jobs and the
//! `workflow_dispatch` inputs (for the dispatch form and for `act`).
//!
//! `on` can be a string (`on: push`), a list (`on: [push, workflow_dispatch]`)
//! or a mapping (`on: {workflow_dispatch: {inputs: …}}`); under YAML 1.1 the
//! key itself reads as the boolean `true`. All forms are handled.

use std::collections::BTreeMap;
use std::path::Path;

use serde_yaml::Value;

use super::yaml::{get, get_on, is_true, key_name, mapping, scalar, str_list};

/// Type of a `workflow_dispatch` input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    String,
    Boolean,
    Choice,
    Number,
    Environment,
}

impl InputKind {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "boolean" => InputKind::Boolean,
            "choice" => InputKind::Choice,
            "number" => InputKind::Number,
            "environment" => InputKind::Environment,
            _ => InputKind::String,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            InputKind::String => "string",
            InputKind::Boolean => "boolean",
            InputKind::Choice => "choice",
            InputKind::Number => "number",
            InputKind::Environment => "environment",
        }
    }
}

/// One `workflow_dispatch` input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchInput {
    pub name: String,
    pub kind: InputKind,
    pub description: String,
    pub required: bool,
    pub default: Option<String>,
    /// `choice` options.
    pub options: Vec<String>,
}

/// A job as declared in the file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalJob {
    pub id: String,
    /// `name:` or the id.
    pub name: String,
    pub runs_on: Vec<String>,
    pub needs: Vec<String>,
    /// A reusable workflow called with `uses:` at job level.
    pub uses: Option<String>,
    pub steps: usize,
}

/// A parsed workflow file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkflowFile {
    /// Relative to the repository, with forward slashes.
    pub path: String,
    /// `name:` or the file name.
    pub name: String,
    pub triggers: Vec<String>,
    pub schedules: Vec<String>,
    /// `Some` when the workflow can be dispatched (possibly without inputs).
    pub dispatch: Option<Vec<DispatchInput>>,
    pub jobs: Vec<LocalJob>,
    pub parse_error: Option<String>,
    pub bytes: usize,
}

impl WorkflowFile {
    pub fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// Triggers of an `on` value: a string, a list, or the keys of a mapping.
pub fn triggers(on: &Value) -> Vec<String> {
    str_list(on)
}

/// The `workflow_dispatch` inputs of an `on` value: `None` when the
/// workflow has no `workflow_dispatch` trigger.
pub fn dispatch_inputs(on: &Value) -> Option<Vec<DispatchInput>> {
    if !triggers(on).iter().any(|t| t == "workflow_dispatch") {
        return None;
    }
    let Some(m) = mapping(on) else {
        // `on: workflow_dispatch` or `on: [push, workflow_dispatch]`.
        return Some(Vec::new());
    };
    let Some(wd) = get(m, "workflow_dispatch").and_then(mapping) else {
        return Some(Vec::new());
    };
    let Some(inputs) = get(wd, "inputs").and_then(mapping) else {
        return Some(Vec::new());
    };
    let mut out = Vec::new();
    for (k, v) in inputs {
        let Some(name) = key_name(k) else { continue };
        let spec = mapping(v);
        let field = |key: &str| spec.and_then(|s| get(s, key));
        let kind = field("type")
            .and_then(scalar)
            .map(|t| InputKind::parse(&t))
            .unwrap_or(InputKind::String);
        out.push(DispatchInput {
            name,
            kind,
            description: field("description").and_then(scalar).unwrap_or_default(),
            required: field("required").is_some_and(is_true),
            default: field("default").and_then(scalar),
            options: field("options").map(str_list).unwrap_or_default(),
        });
    }
    Some(out)
}

/// Parse a workflow file's text.
pub fn parse(path: &str, text: &str) -> WorkflowFile {
    let stem = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".yml")
        .trim_end_matches(".yaml")
        .to_owned();
    let mut wf = WorkflowFile {
        path: path.to_owned(),
        name: stem,
        bytes: text.len(),
        ..Default::default()
    };
    let root: Value = match serde_yaml::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            wf.parse_error = Some(e.to_string());
            return wf;
        }
    };
    let Some(m) = mapping(&root) else {
        wf.parse_error = Some("The file is not a YAML mapping".to_owned());
        return wf;
    };
    if let Some(n) = get(m, "name").and_then(scalar) {
        wf.name = n;
    }
    if let Some(on) = get_on(m) {
        wf.triggers = triggers(on);
        wf.dispatch = dispatch_inputs(on);
        if let Some(items) = mapping(on)
            .and_then(|o| get(o, "schedule"))
            .and_then(super::yaml::sequence)
        {
            for it in items {
                if let Some(c) = mapping(it).and_then(|i| get(i, "cron")).and_then(scalar) {
                    wf.schedules.push(c);
                }
            }
        }
    }
    if let Some(jobs) = get(m, "jobs").and_then(mapping) {
        for (k, v) in jobs {
            let Some(id) = key_name(k) else { continue };
            let jm = mapping(v);
            let field = |key: &str| jm.and_then(|j| get(j, key));
            wf.jobs.push(LocalJob {
                name: field("name").and_then(scalar).unwrap_or_else(|| id.clone()),
                runs_on: field("runs-on").map(str_list).unwrap_or_default(),
                needs: field("needs").map(str_list).unwrap_or_default(),
                uses: field("uses").and_then(scalar),
                steps: field("steps")
                    .and_then(super::yaml::sequence)
                    .map(Vec::len)
                    .unwrap_or(0),
                id,
            });
        }
    }
    wf
}

/// Every `.github/workflows/*.yml|yaml` file under `workdir`, sorted by
/// path, with its text.
pub fn read_dir(workdir: &Path) -> Vec<(String, String)> {
    let dir = workdir.join(".github").join("workflows");
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        let is_yaml = p
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| x.eq_ignore_ascii_case("yml") || x.eq_ignore_ascii_case("yaml"));
        if !is_yaml || !p.is_file() {
            continue;
        }
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let text = std::fs::read(&p)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        out.push((format!(".github/workflows/{name}"), text));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Starting values for the dispatch form: each input's default, booleans
/// as `true` / `false`, choices fall back to their first option.
pub fn default_values(inputs: &[DispatchInput]) -> BTreeMap<String, String> {
    inputs
        .iter()
        .map(|i| {
            let v = match i.kind {
                InputKind::Boolean => match i.default.as_deref() {
                    Some(d) if d.eq_ignore_ascii_case("true") => "true".to_owned(),
                    _ => "false".to_owned(),
                },
                InputKind::Choice => i
                    .default
                    .clone()
                    .or_else(|| i.options.first().cloned())
                    .unwrap_or_default(),
                _ => i.default.clone().unwrap_or_default(),
            };
            (i.name.clone(), v)
        })
        .collect()
}

/// Problems with the values of a dispatch form, per input name.
pub fn validate_values(
    inputs: &[DispatchInput],
    values: &BTreeMap<String, String>,
) -> Vec<(String, String)> {
    let mut errs = Vec::new();
    for i in inputs {
        let v = values.get(&i.name).map(|s| s.trim()).unwrap_or("");
        if v.is_empty() {
            if i.required && i.kind != InputKind::Boolean {
                errs.push((i.name.clone(), format!("{} is required", i.name)));
            }
            continue;
        }
        match i.kind {
            InputKind::Choice => {
                if i.options.is_empty() {
                    errs.push((
                        i.name.clone(),
                        format!(
                            "{} is a choice without options in the workflow file",
                            i.name
                        ),
                    ));
                } else if !i.options.iter().any(|o| o == v) {
                    errs.push((
                        i.name.clone(),
                        format!(
                            "{} must be one of {}",
                            i.name,
                            i.options
                                .iter()
                                .map(|o| format!("`{o}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    ));
                }
            }
            InputKind::Number => {
                if v.parse::<f64>().is_err() {
                    errs.push((i.name.clone(), format!("{} must be a number", i.name)));
                }
            }
            InputKind::Boolean if v != "true" && v != "false" => {
                errs.push((i.name.clone(), format!("{} must be true or false", i.name)));
            }
            _ => {}
        }
    }
    let total = inputs.len();
    if total > 25 {
        errs.push((
            String::new(),
            format!("GitHub accepts at most 25 inputs; this workflow declares {total}"),
        ));
    }
    errs
}

/// The `inputs` object of a dispatch: every boolean, every non-empty value
/// (an empty optional input is left out so the workflow's default applies).
pub fn request_inputs(
    inputs: &[DispatchInput],
    values: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for i in inputs {
        let v = values.get(&i.name).map(|s| s.trim()).unwrap_or("");
        if i.kind == InputKind::Boolean {
            out.insert(
                i.name.clone(),
                if v == "true" { "true" } else { "false" }.to_owned(),
            );
        } else if !v.is_empty() {
            out.insert(i.name.clone(), v.to_owned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISPATCH: &str = r#"
name: Deploy
on:
  push:
    branches: [main]
  workflow_dispatch:
    inputs:
      environment:
        description: Where to deploy
        type: environment
        required: true
      level:
        description: Log level
        type: choice
        options: [info, warning, debug]
        default: warning
      dry_run:
        type: boolean
        default: true
      count:
        type: number
        default: 3
      note:
        description: Free text
  schedule:
    - cron: "0 3 * * 1"
jobs:
  deploy:
    name: Deploy it
    runs-on: [self-hosted, linux]
    needs: build
    steps:
      - run: echo hi
  build:
    runs-on: ubuntu-latest
    steps: []
  call:
    uses: ./.github/workflows/reusable.yml
"#;

    #[test]
    fn parses_inputs_of_every_kind() {
        let wf = parse(".github/workflows/deploy.yml", DISPATCH);
        assert_eq!(wf.parse_error, None);
        assert_eq!(wf.name, "Deploy");
        assert_eq!(wf.triggers, ["push", "workflow_dispatch", "schedule"]);
        assert_eq!(wf.schedules, ["0 3 * * 1"]);
        let inputs = wf.dispatch.expect("dispatchable");
        assert_eq!(inputs.len(), 5);
        assert_eq!(inputs[0].name, "environment");
        assert_eq!(inputs[0].kind, InputKind::Environment);
        assert!(inputs[0].required);
        assert_eq!(inputs[1].kind, InputKind::Choice);
        assert_eq!(inputs[1].options, ["info", "warning", "debug"]);
        assert_eq!(inputs[1].default.as_deref(), Some("warning"));
        assert_eq!(inputs[2].kind, InputKind::Boolean);
        assert_eq!(inputs[2].default.as_deref(), Some("true"));
        assert_eq!(inputs[3].kind, InputKind::Number);
        assert_eq!(inputs[3].default.as_deref(), Some("3"));
        assert_eq!(inputs[4].kind, InputKind::String);
        assert_eq!(inputs[4].description, "Free text");
        assert!(!inputs[4].required);
        assert_eq!(wf.jobs.len(), 3);
        assert_eq!(wf.jobs[0].name, "Deploy it");
        assert_eq!(wf.jobs[0].runs_on, ["self-hosted", "linux"]);
        assert_eq!(wf.jobs[0].needs, ["build"]);
        assert_eq!(wf.jobs[0].steps, 1);
        assert_eq!(
            wf.jobs[2].uses.as_deref(),
            Some("./.github/workflows/reusable.yml")
        );
    }

    #[test]
    fn on_as_string_list_map_and_true() {
        let s = parse("a.yml", "on: workflow_dispatch\njobs: {}\n");
        assert_eq!(s.dispatch, Some(Vec::new()));
        assert_eq!(s.triggers, ["workflow_dispatch"]);
        let l = parse("a.yml", "on: [push, workflow_dispatch]\njobs: {}\n");
        assert_eq!(l.dispatch, Some(Vec::new()));
        assert_eq!(l.triggers, ["push", "workflow_dispatch"]);
        let m = parse("a.yml", "on:\n  workflow_dispatch:\njobs: {}\n");
        assert_eq!(m.dispatch, Some(Vec::new()));
        let none = parse("a.yml", "on: push\njobs: {}\n");
        assert_eq!(none.dispatch, None);
        // YAML 1.1: the key `on` is the boolean true.
        let mut root = serde_yaml::Mapping::new();
        let mut on = serde_yaml::Mapping::new();
        let mut wd = serde_yaml::Mapping::new();
        let mut inputs = serde_yaml::Mapping::new();
        let mut x = serde_yaml::Mapping::new();
        x.insert("type".into(), "boolean".into());
        inputs.insert("flag".into(), Value::Mapping(x));
        wd.insert("inputs".into(), Value::Mapping(inputs));
        on.insert("workflow_dispatch".into(), Value::Mapping(wd));
        root.insert(Value::Bool(true), Value::Mapping(on));
        let got = dispatch_inputs(get_on(&root).unwrap()).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, InputKind::Boolean);
        let text = serde_yaml::to_string(&Value::Mapping(root)).unwrap();
        assert!(text.starts_with("true:"), "{text}");
        let t = parse("t.yml", &text);
        assert_eq!(t.dispatch.map(|d| d.len()), Some(1));
        assert_eq!(t.triggers, ["workflow_dispatch"]);
    }

    #[test]
    fn values_validation_and_body() {
        let wf = parse("d.yml", DISPATCH);
        let inputs = wf.dispatch.unwrap();
        let mut v = default_values(&inputs);
        assert_eq!(v["level"], "warning");
        assert_eq!(v["dry_run"], "true");
        assert_eq!(v["count"], "3");
        assert_eq!(v["environment"], "");
        let errs = validate_values(&inputs, &v);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].0, "environment");
        v.insert("environment".into(), "prod".into());
        v.insert("level".into(), "trace".into());
        v.insert("count".into(), "three".into());
        let errs = validate_values(&inputs, &v);
        assert_eq!(errs.len(), 2, "{errs:?}");
        assert!(errs[0].1.contains("`info`, `warning`, `debug`"));
        assert!(errs[1].1.contains("number"));
        v.insert("level".into(), "debug".into());
        v.insert("count".into(), "4".into());
        assert!(validate_values(&inputs, &v).is_empty());
        let body = request_inputs(&inputs, &v);
        assert_eq!(body.get("note"), None);
        assert_eq!(body["dry_run"], "true");
        assert_eq!(body["environment"], "prod");
        assert_eq!(body.len(), 4);
    }

    #[test]
    fn choice_without_options_is_reported() {
        let text = "on:\n  workflow_dispatch:\n    inputs:\n      x:\n        type: choice\n";
        let wf = parse("c.yml", text);
        let inputs = wf.dispatch.unwrap();
        let mut v = default_values(&inputs);
        v.insert("x".into(), "a".into());
        let errs = validate_values(&inputs, &v);
        assert!(errs[0].1.contains("without options"));
    }

    #[test]
    fn syntax_error_is_kept() {
        let wf = parse("bad.yml", "on: [push\njobs:\n");
        assert!(wf.parse_error.is_some());
    }
}
