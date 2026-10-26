//! Workflow validation: built-in rules over the YAML of each file, plus
//! `actionlint`'s findings when it is installed.
//!
//! serde_yaml values carry no positions, so a finding's line is found by
//! looking for its key (or the offending text) in the file; YAML syntax
//! errors carry their exact line and column.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use serde::Deserialize;
use serde_yaml::{Mapping, Value};

use super::yaml::{Lines, get, get_on, has_on, key_name, mapping, scalar, sequence, str_list};
use crate::devops::proc::ProcSpec;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "Error",
            Severity::Warning => "Warning",
            Severity::Info => "Info",
        }
    }
}

/// Who reported a finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Builtin,
    Actionlint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    /// Stable id, e.g. `needs-unknown`, or actionlint's kind.
    pub rule: String,
    /// Relative path with forward slashes.
    pub file: String,
    /// 1-based; 0 when unknown.
    pub line: u32,
    pub column: u32,
    pub message: String,
    pub hint: String,
    pub source: Source,
}

/// Events a workflow can be triggered by.
pub const EVENTS: &[&str] = &[
    "branch_protection_rule",
    "check_run",
    "check_suite",
    "create",
    "delete",
    "deployment",
    "deployment_status",
    "discussion",
    "discussion_comment",
    "fork",
    "gollum",
    "image_version",
    "issue_comment",
    "issues",
    "label",
    "merge_group",
    "milestone",
    "page_build",
    "project",
    "project_card",
    "project_column",
    "public",
    "pull_request",
    "pull_request_review",
    "pull_request_review_comment",
    "pull_request_target",
    "push",
    "registry_package",
    "release",
    "repository_dispatch",
    "schedule",
    "status",
    "watch",
    "workflow_call",
    "workflow_dispatch",
    "workflow_run",
];

/// Expression contexts.
pub const CONTEXTS: &[&str] = &[
    "github", "env", "vars", "job", "jobs", "steps", "runner", "secrets", "strategy", "matrix",
    "needs", "inputs",
];

/// Expression functions (case-insensitive).
pub const FUNCTIONS: &[&str] = &[
    "contains",
    "startswith",
    "endswith",
    "format",
    "join",
    "tojson",
    "fromjson",
    "hashfiles",
    "success",
    "always",
    "cancelled",
    "failure",
    "case",
];

const LITERALS: &[&str] = &["true", "false", "null", "nan", "infinity"];

/// Deprecated action versions: `(prefix, why)`.
const DEPRECATED_ACTIONS: &[(&str, &str)] = &[
    (
        "actions/checkout@v1",
        "runs on Node 12, which GitHub removed",
    ),
    (
        "actions/checkout@v2",
        "runs on Node 12, which GitHub removed",
    ),
    (
        "actions/setup-node@v1",
        "runs on Node 12, which GitHub removed",
    ),
    (
        "actions/setup-node@v2",
        "runs on Node 12, which GitHub removed",
    ),
    (
        "actions/cache@v1",
        "uses the cache service GitHub shut down",
    ),
    (
        "actions/cache@v2",
        "uses the cache service GitHub shut down",
    ),
    (
        "actions/upload-artifact@v1",
        "is rejected by GitHub since 2024",
    ),
    (
        "actions/upload-artifact@v2",
        "is rejected by GitHub since 2024",
    ),
    (
        "actions/upload-artifact@v3",
        "is rejected by GitHub since January 2025",
    ),
    (
        "actions/download-artifact@v1",
        "is rejected by GitHub since 2024",
    ),
    (
        "actions/download-artifact@v2",
        "is rejected by GitHub since 2024",
    ),
    (
        "actions/download-artifact@v3",
        "is rejected by GitHub since January 2025",
    ),
];

struct Ctx<'a> {
    file: &'a str,
    lines: Lines<'a>,
    lines_text: &'a str,
    out: Vec<Finding>,
}

impl Ctx<'_> {
    fn push(
        &mut self,
        severity: Severity,
        rule: &str,
        line: Option<u32>,
        message: String,
        hint: &str,
    ) {
        let line = line.unwrap_or(0);
        let column = if line > 0 {
            self.lines.indent(line) as u32 + 1
        } else {
            0
        };
        self.out.push(Finding {
            severity,
            rule: rule.to_owned(),
            file: self.file.to_owned(),
            line,
            column,
            message,
            hint: hint.to_owned(),
            source: Source::Builtin,
        });
    }

    /// A finding at the first occurrence of `needle` at or after `from`,
    /// with the column of the needle.
    fn push_at(
        &mut self,
        severity: Severity,
        rule: &str,
        needle: &str,
        from: Option<u32>,
        message: String,
        hint: &str,
    ) {
        let start = from.unwrap_or(1);
        let line = self
            .lines
            .find(needle, start)
            .or_else(|| self.lines.find(needle, 1))
            .or(from);
        let column = line
            .and_then(|l| self.lines.text(l).find(needle))
            .map(|c| c as u32 + 1)
            .unwrap_or_else(|| line.map(|l| self.lines.indent(l) as u32 + 1).unwrap_or(0));
        self.out.push(Finding {
            severity,
            rule: rule.to_owned(),
            file: self.file.to_owned(),
            line: line.unwrap_or(0),
            column,
            message,
            hint: hint.to_owned(),
            source: Source::Builtin,
        });
    }
}

/// Lint one workflow file.
pub fn lint(file: &str, text: &str) -> Vec<Finding> {
    let mut cx = Ctx {
        file,
        lines: Lines::new(text),
        lines_text: text,
        out: Vec::new(),
    };
    if text.trim().is_empty() {
        cx.push(
            Severity::Error,
            "empty-file",
            Some(1),
            "The workflow file is empty".to_owned(),
            "Add `on:` and `jobs:`, or delete the file.",
        );
        return cx.out;
    }
    let root: Value = match serde_yaml::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            let (line, column) = e
                .location()
                .map(|l| (l.line() as u32, l.column() as u32))
                .unwrap_or((0, 0));
            let msg = e.to_string();
            cx.out.push(Finding {
                severity: Severity::Error,
                rule: "yaml-syntax".to_owned(),
                file: file.to_owned(),
                line,
                column,
                message: format!("YAML syntax error: {msg}"),
                hint: "Fix the indentation or quoting at this position; GitHub rejects the whole file."
                    .to_owned(),
                source: Source::Builtin,
            });
            return cx.out;
        }
    };
    let Some(m) = mapping(&root) else {
        cx.push(
            Severity::Error,
            "not-a-mapping",
            Some(1),
            "The top level of a workflow must be a mapping (`on:`, `jobs:`, …)".to_owned(),
            "Start the file with `name:`, `on:` and `jobs:` keys.",
        );
        return cx.out;
    };
    let triggers = check_on(&mut cx, m);
    check_permissions(&mut cx, m);
    let jobs = check_jobs(&mut cx, m, &triggers);
    check_expressions(&mut cx, &root, &jobs);
    cx.out.sort_by(|a, b| {
        a.line
            .cmp(&b.line)
            .then(a.severity.cmp(&b.severity))
            .then(a.rule.cmp(&b.rule))
    });
    cx.out
}

// ---------------------------------------------------------------------------
// on
// ---------------------------------------------------------------------------

fn check_on(cx: &mut Ctx, m: &Mapping) -> Vec<String> {
    let on_line = cx.lines.top("on").or_else(|| cx.lines.top("true"));
    if !has_on(m) {
        cx.push(
            Severity::Error,
            "on-missing",
            Some(1),
            "No `on:` key: this workflow never runs".to_owned(),
            "Add the events that start it, e.g. `on: [push, pull_request]`.",
        );
        return Vec::new();
    }
    let Some(on) = get_on(m) else {
        return Vec::new();
    };
    let triggers = str_list(on);
    let empty = match on {
        Value::Null => true,
        Value::Mapping(x) => x.is_empty(),
        Value::Sequence(x) => x.is_empty(),
        Value::String(s) => s.trim().is_empty(),
        _ => false,
    };
    if empty {
        cx.push(
            Severity::Error,
            "on-empty",
            on_line,
            "`on:` lists no events: this workflow never runs".to_owned(),
            "Add at least one event, e.g. `push` or `workflow_dispatch`.",
        );
        return triggers;
    }
    for t in &triggers {
        if !EVENTS.contains(&t.as_str()) {
            let suggestion = closest(t, EVENTS);
            let hint = match suggestion {
                Some(s) => {
                    format!("Did you mean `{s}`? Event names are lower case with underscores.")
                }
                None => "See the list of events that trigger workflows in GitHub's documentation."
                    .to_owned(),
            };
            let line = on_line.and_then(|l| cx.lines.key_in(t, l).or(Some(l)));
            cx.push(
                Severity::Error,
                "unknown-event",
                line,
                format!("Unknown event `{t}`"),
                &hint,
            );
        }
    }
    if let Some(om) = mapping(on) {
        check_dispatch(cx, om, on_line);
        check_schedule(cx, om, on_line);
    }
    triggers
}

/// The closest known name (edit distance ≤ 3).
fn closest<'a>(word: &str, names: &[&'a str]) -> Option<&'a str> {
    let w = word.to_ascii_lowercase().replace('-', "_");
    if let Some(exact) = names.iter().find(|n| **n == w) {
        return Some(exact);
    }
    names
        .iter()
        .map(|n| (levenshtein(&w, n), *n))
        .filter(|(d, _)| *d <= 3)
        .min_by_key(|(d, _)| *d)
        .map(|(_, n)| n)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

fn check_dispatch(cx: &mut Ctx, on: &Mapping, on_line: Option<u32>) {
    let Some(inputs) = get(on, "workflow_dispatch")
        .and_then(mapping)
        .and_then(|w| get(w, "inputs"))
        .and_then(mapping)
    else {
        return;
    };
    let base = on_line.and_then(|l| cx.lines.key_in("workflow_dispatch", l));
    let inputs_line = base.and_then(|l| cx.lines.key_in("inputs", l));
    for (k, v) in inputs {
        let Some(name) = key_name(k) else { continue };
        let line = inputs_line.and_then(|l| cx.lines.key_in(&name, l));
        let spec = mapping(v);
        let kind = spec
            .and_then(|s| get(s, "type"))
            .and_then(scalar)
            .unwrap_or_else(|| "string".to_owned());
        if !["string", "boolean", "choice", "number", "environment"].contains(&kind.as_str()) {
            cx.push(
                Severity::Error,
                "dispatch-input-type",
                line,
                format!("Input `{name}` has an unknown type `{kind}`"),
                "Use string, boolean, choice, number or environment.",
            );
        }
        if kind == "choice" {
            let options = spec
                .and_then(|s| get(s, "options"))
                .map(str_list)
                .unwrap_or_default();
            if options.is_empty() {
                cx.push(
                    Severity::Error,
                    "dispatch-choice-options",
                    line,
                    format!("Choice input `{name}` has no options"),
                    "Add `options:` with the allowed values.",
                );
            } else if let Some(d) = spec.and_then(|s| get(s, "default")).and_then(scalar)
                && !options.contains(&d)
            {
                let dl = line.and_then(|l| cx.lines.key_in("default", l)).or(line);
                cx.push(
                    Severity::Error,
                    "dispatch-default-option",
                    dl,
                    format!(
                        "The default `{d}` of choice input `{name}` is not one of its options ({})",
                        options.join(", ")
                    ),
                    "Make the default one of the options, or add it to them.",
                );
            }
        }
    }
}

fn check_schedule(cx: &mut Ctx, on: &Mapping, on_line: Option<u32>) {
    let Some(items) = get(on, "schedule").and_then(sequence) else {
        return;
    };
    let base = on_line.and_then(|l| cx.lines.key_in("schedule", l));
    for it in items {
        let Some(c) = mapping(it).and_then(|i| get(i, "cron")).and_then(scalar) else {
            cx.push(
                Severity::Error,
                "cron-invalid",
                base,
                "A schedule entry has no `cron:` expression".to_owned(),
                "Write each entry as `- cron: \"30 5 * * 1\"`.",
            );
            continue;
        };
        match check_cron(&c) {
            Err(e) => cx.push_at(
                Severity::Error,
                "cron-invalid",
                &c,
                base,
                format!("Invalid cron expression `{c}`: {e}"),
                "Five fields: minute (0-59), hour (0-23), day of month (1-31), month (1-12 or JAN-DEC), day of week (0-6 or SUN-SAT).",
            ),
            Ok(true) => cx.push_at(
                Severity::Warning,
                "cron-frequency",
                &c,
                base,
                format!("`{c}` asks for runs more often than every 5 minutes"),
                "GitHub runs scheduled workflows at most every 5 minutes, and delays them under load.",
            ),
            Ok(false) => {}
        }
    }
}

/// Validate a 5-field POSIX cron expression as GitHub accepts it. `Ok(true)`
/// when it asks for runs more often than every 5 minutes.
pub fn check_cron(expr: &str) -> Result<bool, String> {
    const MONTHS: &[&str] = &[
        "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
    ];
    const DAYS: &[&str] = &["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(format!(
            "it needs 5 fields (minute, hour, day of month, month, day of week) and has {}",
            fields.len()
        ));
    }
    let specs: [(&str, u32, u32, &[&str], u32); 5] = [
        ("minute", 0, 59, &[], 0),
        ("hour", 0, 23, &[], 0),
        ("day of month", 1, 31, &[], 0),
        ("month", 1, 12, MONTHS, 1),
        ("day of week", 0, 6, DAYS, 0),
    ];
    for (field, (name, min, max, names, offset)) in fields.iter().zip(specs) {
        let value = |s: &str| -> Result<u32, String> {
            if let Some(i) = names.iter().position(|n| n.eq_ignore_ascii_case(s)) {
                return Ok(i as u32 + offset);
            }
            let v: u32 = s
                .parse()
                .map_err(|_| format!("`{s}` is not a valid {name}"))?;
            if v < min || v > max {
                return Err(format!("{name} `{v}` is outside {min}-{max}"));
            }
            Ok(v)
        };
        for part in field.split(',') {
            if part.is_empty() {
                return Err(format!("empty value in the {name} field"));
            }
            let (range, step) = match part.split_once('/') {
                Some((r, s)) => {
                    let n: u32 = s
                        .parse()
                        .map_err(|_| format!("step `{s}` in the {name} field is not a number"))?;
                    if n == 0 || n > max {
                        return Err(format!("step `{n}` in the {name} field is out of range"));
                    }
                    (r, Some(n))
                }
                None => (part, None),
            };
            if range == "*" {
                continue;
            }
            match range.split_once('-') {
                Some((a, b)) => {
                    let (a, b) = (value(a)?, value(b)?);
                    if a > b {
                        return Err(format!(
                            "range `{range}` in the {name} field runs backwards"
                        ));
                    }
                }
                None => {
                    value(range)?;
                }
            }
            let _ = step;
        }
    }
    let minute = fields[0];
    let frequent = minute == "*"
        || minute
            .strip_prefix("*/")
            .and_then(|n| n.parse::<u32>().ok())
            .is_some_and(|n| n < 5);
    Ok(frequent)
}

// ---------------------------------------------------------------------------
// permissions
// ---------------------------------------------------------------------------

fn check_permissions(cx: &mut Ctx, m: &Mapping) {
    if get(m, "permissions").is_some() {
        return;
    }
    let Some(jobs) = get(m, "jobs").and_then(mapping) else {
        return;
    };
    let all_jobs_set = !jobs.is_empty()
        && jobs
            .values()
            .all(|j| mapping(j).is_some_and(|j| get(j, "permissions").is_some()));
    if !all_jobs_set {
        cx.push(
            Severity::Warning,
            "permissions-missing",
            cx.lines.top("jobs").or(Some(1)),
            "No `permissions:` block: the GITHUB_TOKEN gets the repository default, which can be read and write".to_owned(),
            "Add `permissions: contents: read` at the top and widen it per job where needed.",
        );
    }
}

// ---------------------------------------------------------------------------
// jobs and steps
// ---------------------------------------------------------------------------

/// Job ids with their line, for the expression pass.
type JobLines = Vec<(String, Option<u32>)>;

fn check_jobs(cx: &mut Ctx, m: &Mapping, triggers: &[String]) -> JobLines {
    let mut out = Vec::new();
    let Some(jobs_v) = get(m, "jobs") else {
        cx.push(
            Severity::Error,
            "jobs-missing",
            Some(1),
            "No `jobs:` key: the workflow has nothing to run".to_owned(),
            "Add `jobs:` with at least one job.",
        );
        return out;
    };
    let jobs_line = cx.lines.top("jobs");
    let Some(jobs) = mapping(jobs_v).filter(|j| !j.is_empty()) else {
        cx.push(
            Severity::Error,
            "jobs-missing",
            jobs_line,
            "`jobs:` is empty: the workflow has nothing to run".to_owned(),
            "Add at least one job with `runs-on:` and `steps:`.",
        );
        return out;
    };
    let ids: Vec<String> = jobs.keys().filter_map(key_name).collect();
    let mut needs_graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let prt = triggers.iter().any(|t| t == "pull_request_target");
    for (k, v) in jobs {
        let Some(id) = key_name(k) else { continue };
        let jl = cx.lines.job(&id).or(jobs_line);
        out.push((id.clone(), jl));
        let Some(job) = mapping(v) else {
            cx.push(
                Severity::Error,
                "job-not-a-mapping",
                jl,
                format!("Job `{id}` is not a mapping"),
                "Give the job `runs-on:` and `steps:`.",
            );
            continue;
        };
        let reusable = get(job, "uses").and_then(scalar);
        if reusable.is_none() && get(job, "runs-on").is_none() {
            cx.push(
                Severity::Error,
                "runs-on-missing",
                jl,
                format!("Job `{id}` has no `runs-on:`"),
                "Add `runs-on: ubuntu-latest` (or the runner labels it needs), or call a reusable workflow with `uses:`.",
            );
        }
        if reusable.is_none() && get(job, "steps").is_none() {
            cx.push(
                Severity::Error,
                "steps-missing",
                jl,
                format!("Job `{id}` has no `steps:`"),
                "A job runs its steps; add at least one `run:` or `uses:` step.",
            );
        }
        if reusable.is_none() && get(job, "timeout-minutes").is_none() {
            cx.push(
                Severity::Info,
                "timeout-missing",
                jl,
                format!("Job `{id}` has no `timeout-minutes:` (the default is 360)"),
                "Set `timeout-minutes:` a little above the job's usual duration so a hung job stops early.",
            );
        }
        // needs
        let needs = get(job, "needs").map(str_list).unwrap_or_default();
        for n in &needs {
            if !ids.contains(n) {
                let nl = jl.and_then(|l| cx.lines.key_in("needs", l)).or(jl);
                let hint = match closest(n, &ids.iter().map(String::as_str).collect::<Vec<_>>()) {
                    Some(s) if s != n => format!("Did you mean `{s}`?"),
                    _ => format!("Jobs in this file: {}.", ids.join(", ")),
                };
                cx.push(
                    Severity::Error,
                    "needs-unknown",
                    nl,
                    format!("Job `{id}` needs `{n}`, which is not a job in this workflow"),
                    &hint,
                );
            }
        }
        needs_graph.insert(id.clone(), needs);
        if let Some(strategy) = get(job, "strategy").and_then(mapping)
            && let Some(matrix) = get(strategy, "matrix").and_then(mapping)
        {
            check_matrix(cx, &id, matrix, jl);
        }
        if let Some(steps) = get(job, "steps").and_then(sequence) {
            check_steps(cx, &id, jl, steps, prt);
        }
    }
    for cycle in cycles(&needs_graph) {
        let first = cycle.first().cloned().unwrap_or_default();
        let jl = cx.lines.job(&first).or(jobs_line);
        cx.push(
            Severity::Error,
            "needs-cycle",
            jl,
            format!("Dependency cycle: {}", cycle.join(" → ")),
            "Remove one of the `needs:` entries so the jobs can be ordered.",
        );
    }
    out
}

/// Each dependency cycle once, as a path that returns to its start.
fn cycles(graph: &BTreeMap<String, Vec<String>>) -> Vec<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        New,
        Active,
        Done,
    }
    fn visit(
        n: &str,
        graph: &BTreeMap<String, Vec<String>>,
        marks: &mut HashMap<String, Mark>,
        stack: &mut Vec<String>,
        out: &mut Vec<Vec<String>>,
        seen: &mut BTreeSet<BTreeSet<String>>,
    ) {
        marks.insert(n.to_owned(), Mark::Active);
        stack.push(n.to_owned());
        for m in graph.get(n).into_iter().flatten() {
            if !graph.contains_key(m) {
                continue;
            }
            match marks.get(m).copied().unwrap_or(Mark::New) {
                Mark::New => visit(m, graph, marks, stack, out, seen),
                Mark::Active => {
                    let start = stack.iter().position(|s| s == m).unwrap_or(0);
                    let mut c: Vec<String> = stack[start..].to_vec();
                    let key: BTreeSet<String> = c.iter().cloned().collect();
                    if seen.insert(key) {
                        c.push(m.clone());
                        out.push(c);
                    }
                }
                Mark::Done => {}
            }
        }
        stack.pop();
        marks.insert(n.to_owned(), Mark::Done);
    }
    let mut marks = HashMap::new();
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for n in graph.keys() {
        if marks.get(n).copied().unwrap_or(Mark::New) == Mark::New {
            visit(n, graph, &mut marks, &mut Vec::new(), &mut out, &mut seen);
        }
    }
    out
}

fn check_matrix(cx: &mut Ctx, job: &str, matrix: &Mapping, jl: Option<u32>) {
    let dims: Vec<String> = matrix
        .keys()
        .filter_map(key_name)
        .filter(|k| k != "include" && k != "exclude")
        .collect();
    let ml = jl.and_then(|l| cx.lines.key_in("strategy", l));
    let ml = ml.and_then(|l| cx.lines.key_in("matrix", l)).or(ml).or(jl);
    if let Some(ex) = get(matrix, "exclude").and_then(sequence) {
        let xl = ml.and_then(|l| cx.lines.key_in("exclude", l)).or(ml);
        for e in ex {
            let Some(em) = mapping(e) else { continue };
            for k in em.keys().filter_map(key_name) {
                if !dims.contains(&k) {
                    cx.push_at(
                        Severity::Error,
                        "matrix-unknown-key",
                        &format!("{k}:"),
                        xl,
                        format!(
                            "Matrix exclude of job `{job}` uses `{k}`, which is not a matrix key ({})",
                            if dims.is_empty() { "none".to_owned() } else { dims.join(", ") }
                        ),
                        "An exclude entry can only name keys of the matrix; GitHub fails the workflow otherwise.",
                    );
                }
            }
        }
    }
    if !dims.is_empty()
        && let Some(inc) = get(matrix, "include").and_then(sequence)
    {
        let il = ml.and_then(|l| cx.lines.key_in("include", l)).or(ml);
        for e in inc {
            let Some(em) = mapping(e) else { continue };
            let keys: Vec<String> = em.keys().filter_map(key_name).collect();
            if !keys.is_empty() && keys.iter().all(|k| !dims.contains(k)) {
                cx.push_at(
                    Severity::Info,
                    "matrix-unknown-key",
                    &format!("{}:", keys[0]),
                    il,
                    format!(
                        "Matrix include of job `{job}` names only keys that are not in the matrix ({}): it adds them to every combination",
                        keys.join(", ")
                    ),
                    &format!("Check the key names; the matrix keys are {}.", dims.join(", ")),
                );
            }
        }
    }
}

fn check_steps(cx: &mut Ctx, job: &str, jl: Option<u32>, steps: &[Value], prt: bool) {
    let mut ids: HashMap<String, usize> = HashMap::new();
    for (i, s) in steps.iter().enumerate() {
        let sl = jl.and_then(|l| cx.lines.step(l, i)).or(jl);
        let Some(step) = mapping(s) else {
            cx.push(
                Severity::Error,
                "step-not-a-mapping",
                sl,
                format!("Step {} of job `{job}` is not a mapping", i + 1),
                "Write each step as `- run: …` or `- uses: …`.",
            );
            continue;
        };
        let label = get(step, "name")
            .and_then(scalar)
            .or_else(|| get(step, "id").and_then(scalar))
            .map(|n| format!("`{n}`"))
            .unwrap_or_else(|| format!("{}", i + 1));
        let uses = get(step, "uses").and_then(scalar);
        let run = get(step, "run").and_then(scalar);
        match (&uses, &run) {
            (Some(_), Some(_)) => cx.push(
                Severity::Error,
                "step-uses-and-run",
                sl,
                format!("Step {label} of job `{job}` has both `uses:` and `run:`"),
                "A step either runs an action (`uses:`) or a script (`run:`); split it into two steps.",
            ),
            (None, None) => cx.push(
                Severity::Error,
                "step-no-action",
                sl,
                format!("Step {label} of job `{job}` has neither `uses:` nor `run:`"),
                "Add `run:` with a command or `uses:` with an action.",
            ),
            _ => {}
        }
        if let Some(id) = get(step, "id").and_then(scalar) {
            if let Some(prev) = ids.insert(id.clone(), i) {
                cx.push(
                    Severity::Error,
                    "duplicate-step-id",
                    sl,
                    format!(
                        "Step id `{id}` is used twice in job `{job}` (steps {} and {})",
                        prev + 1,
                        i + 1
                    ),
                    "Step ids must be unique within a job; rename one.",
                );
            }
        }
        if let Some(u) = &uses {
            for (prefix, why) in DEPRECATED_ACTIONS {
                if u == prefix || u.starts_with(&format!("{prefix}.")) {
                    cx.push_at(
                        Severity::Warning,
                        "deprecated-action",
                        u,
                        sl,
                        format!("`{u}` is deprecated: it {why}"),
                        "Move to the current major version (see the action's releases).",
                    );
                }
            }
            if let Some((repo, version)) = third_party(u)
                && !is_sha(version)
            {
                cx.push_at(
                    Severity::Warning,
                    "unpinned-action",
                    u,
                    sl,
                    format!("Third-party action `{repo}` is pinned to `{version}`, a tag or branch that can move"),
                    "Pin it to a full commit SHA (`uses: owner/repo@<40 hex> # v1.2.3`) so the code cannot change under you.",
                );
            }
            if prt && u.starts_with("actions/checkout") {
                let r = get(step, "with")
                    .and_then(mapping)
                    .and_then(|w| get(w, "ref"))
                    .and_then(scalar)
                    .unwrap_or_default();
                let head = [
                    "pull_request.head.sha",
                    "pull_request.head.ref",
                    "github.head_ref",
                    "refs/pull/",
                ]
                .iter()
                .any(|n| r.contains(n));
                if head {
                    cx.push_at(
                        Severity::Error,
                        "prt-checkout",
                        "ref:",
                        sl,
                        format!("Job `{job}` checks out the pull request's head in a `pull_request_target` workflow"),
                        "pull_request_target runs with the base repository's secrets and a write token; untrusted code from the fork then runs with them. Use `pull_request`, or never run the checked-out code.",
                    );
                }
            }
        }
        if let Some(script) = &run {
            for (cmd, sev, hint) in [
                (
                    "::set-output",
                    Severity::Warning,
                    "Write `name=value` to `$GITHUB_OUTPUT` instead.",
                ),
                (
                    "::save-state",
                    Severity::Warning,
                    "Write `name=value` to `$GITHUB_STATE` instead.",
                ),
                (
                    "::set-env",
                    Severity::Error,
                    "Disabled since 2020: write `NAME=value` to `$GITHUB_ENV`.",
                ),
                (
                    "::add-path",
                    Severity::Error,
                    "Disabled since 2020: append the directory to `$GITHUB_PATH`.",
                ),
            ] {
                if script.contains(cmd) {
                    cx.push_at(
                        sev,
                        "deprecated-command",
                        cmd,
                        sl,
                        format!("Step {label} of job `{job}` uses the deprecated `{cmd}` command"),
                        hint,
                    );
                }
            }
        }
    }
}

/// `(owner/repo, version)` of a third-party action reference.
fn third_party(uses: &str) -> Option<(&str, &str)> {
    if uses.starts_with("./") || uses.starts_with("../") || uses.starts_with("docker://") {
        return None;
    }
    let (spec, version) = uses.rsplit_once('@')?;
    let owner = spec.split('/').next()?;
    if owner == "actions" || owner == "github" {
        return None;
    }
    Some((spec, version))
}

fn is_sha(v: &str) -> bool {
    v.len() == 40 && v.bytes().all(|b| b.is_ascii_hexdigit())
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

/// Every string value with the keys leading to it.
fn strings<'a>(v: &'a Value, path: &mut Vec<String>, out: &mut Vec<(Vec<String>, &'a str)>) {
    match super::yaml::untag(v) {
        Value::String(s) => out.push((path.clone(), s.as_str())),
        Value::Mapping(m) => {
            for (k, v) in m {
                path.push(key_name(k).unwrap_or_default());
                strings(v, path, out);
                path.pop();
            }
        }
        Value::Sequence(seq) => {
            for (i, v) in seq.iter().enumerate() {
                path.push(format!("[{i}]"));
                strings(v, path, out);
                path.pop();
            }
        }
        _ => {}
    }
}

/// The bodies of `${{ … }}` in `s`, and whether one is not closed.
pub fn expressions(s: &str) -> (Vec<&str>, bool) {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find("${{") {
        let after = &rest[i + 3..];
        match after.find("}}") {
            Some(j) => {
                out.push(after[..j].trim());
                rest = &after[j + 2..];
            }
            None => return (out, true),
        }
    }
    (out, false)
}

/// Context names (lower case) and function names an expression refers to.
pub fn identifiers(expr: &str) -> (Vec<String>, Vec<String>) {
    let mut contexts = Vec::new();
    let mut functions = Vec::new();
    let chars: Vec<char> = expr.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            // String literal; '' escapes a quote.
            i += 1;
            while i < chars.len() {
                if chars[i] == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '-')
            {
                i += 1;
            }
            let word: String = chars[start..i]
                .iter()
                .collect::<String>()
                .to_ascii_lowercase();
            let prev = chars[..start].iter().rev().find(|c| !c.is_whitespace());
            if prev == Some(&'.') {
                continue;
            }
            let next = chars[i..].iter().find(|c| !c.is_whitespace());
            if next == Some(&'(') {
                functions.push(word);
            } else if !LITERALS.contains(&word.as_str()) {
                contexts.push(word);
            }
            continue;
        }
        if c.is_ascii_digit() {
            // Numbers, including 1e3 and 0xff.
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    (contexts, functions)
}

fn check_expressions(cx: &mut Ctx, root: &Value, jobs: &JobLines) {
    let mut all = Vec::new();
    strings(root, &mut Vec::new(), &mut all);
    // Where to start looking for a value: its step, else its job.
    let lines = Lines::new(cx.lines_text);
    let job_line = |path: &[String]| -> Option<u32> {
        if path.first().map(String::as_str) != Some("jobs") {
            return None;
        }
        let id = path.get(1)?;
        let jl = jobs.iter().find(|(j, _)| j == id).and_then(|(_, l)| *l)?;
        let step = (path.get(2).map(String::as_str) == Some("steps"))
            .then(|| path.get(3))
            .flatten()
            .and_then(|s| {
                s.trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<usize>()
                    .ok()
            });
        Some(step.and_then(|i| lines.step(jl, i)).unwrap_or(jl))
    };
    let mut reported: BTreeSet<(String, String)> = BTreeSet::new();
    for (path, s) in all {
        let key = path.last().map(String::as_str).unwrap_or("");
        let from = job_line(&path);
        let is_if = key == "if";
        let (mut exprs, unclosed) = expressions(s);
        if unclosed {
            let needle = s[s.find("${{").unwrap_or(0)..]
                .lines()
                .next()
                .unwrap_or("${{")
                .chars()
                .take(40)
                .collect::<String>();
            // The line where the expression opens and is not closed.
            let line = (from.unwrap_or(1)..=lines.len() as u32)
                .find(|l| {
                    let t = lines.text(*l);
                    t.find(&needle).is_some_and(|i| !t[i..].contains("}}"))
                })
                .or(from);
            cx.push(
                Severity::Error,
                "expression-unbalanced",
                line,
                format!("`${{{{` without a closing `}}}}` in `{}`", path.join(".")),
                "Close the expression with `}}`.",
            );
        }
        // An `if:` without `${{ }}` is an expression as a whole.
        if is_if && exprs.is_empty() && !s.contains("${{") {
            exprs.push(s.trim());
        }
        for e in exprs {
            let (contexts, functions) = identifiers(e);
            let needle: String = e.chars().take(40).collect();
            for c in &contexts {
                if !CONTEXTS.contains(&c.as_str()) && reported.insert((path.join("."), c.clone())) {
                    let hint = match closest(c, CONTEXTS) {
                        Some(s) => {
                            format!("Did you mean `{s}`? Contexts: {}.", CONTEXTS.join(", "))
                        }
                        None => format!(
                            "Expressions can read {}; quote literal text as 'text'.",
                            CONTEXTS.join(", ")
                        ),
                    };
                    cx.push_at(
                        Severity::Error,
                        "expression-context",
                        &needle,
                        from,
                        format!("Unknown context `{c}` in `{e}`"),
                        &hint,
                    );
                }
            }
            for f in &functions {
                if !FUNCTIONS.contains(&f.as_str()) && reported.insert((path.join("."), f.clone()))
                {
                    cx.push_at(
                        Severity::Error,
                        "expression-function",
                        &needle,
                        from,
                        format!("Unknown function `{f}()` in `{e}`"),
                        "Functions: contains, startsWith, endsWith, format, join, toJSON, fromJSON, hashFiles, success, always, cancelled, failure, case.",
                    );
                }
            }
            if is_if && contexts.iter().any(|c| c == "secrets") {
                cx.push_at(
                    Severity::Error,
                    "secrets-in-if",
                    &needle,
                    from,
                    format!("`secrets` cannot be read in an `if:` condition (`{e}`)"),
                    "Map the secret to an environment variable (`env: HAS_KEY: ${{ secrets.KEY != '' }}`) and test `env.HAS_KEY` instead.",
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// actionlint
// ---------------------------------------------------------------------------

/// `actionlint -format '{{json .}}' FILES…` (the template is one argument;
/// no shell is involved, so it needs no quotes).
pub fn actionlint_spec(workdir: &Path, files: &[String]) -> ProcSpec {
    ProcSpec::new("actionlint", workdir)
        .arg("-format")
        .arg("{{json .}}")
        .args(files.iter().cloned())
}

#[derive(Deserialize)]
struct ActionlintJson {
    #[serde(default)]
    message: String,
    #[serde(default)]
    filepath: String,
    #[serde(default)]
    line: u32,
    #[serde(default)]
    column: u32,
    #[serde(default)]
    kind: String,
}

/// Parse actionlint's JSON output (an array; empty output means no
/// findings).
pub fn parse_actionlint(stdout: &str) -> Result<Vec<Finding>, String> {
    let t = stdout.trim();
    if t.is_empty() || t == "null" {
        return Ok(Vec::new());
    }
    let items: Vec<ActionlintJson> =
        serde_json::from_str(t).map_err(|e| format!("Could not read actionlint's output: {e}"))?;
    Ok(items
        .into_iter()
        .map(|i| Finding {
            severity: if i.kind == "shellcheck" || i.kind == "pyflakes" {
                Severity::Warning
            } else {
                Severity::Error
            },
            rule: if i.kind.is_empty() {
                "actionlint".to_owned()
            } else {
                i.kind
            },
            file: i.filepath.replace('\\', "/"),
            line: i.line,
            column: i.column,
            message: i.message,
            hint: String::new(),
            source: Source::Actionlint,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(text: &str) -> Vec<String> {
        lint(".github/workflows/t.yml", text)
            .into_iter()
            .map(|f| f.rule)
            .collect()
    }

    fn has(text: &str, rule: &str) -> Finding {
        lint(".github/workflows/t.yml", text)
            .into_iter()
            .find(|f| f.rule == rule)
            .unwrap_or_else(|| panic!("no {rule} in {:?}", rules(text)))
    }

    const CLEAN: &str = "name: CI\non: [push]\npermissions:\n  contents: read\njobs:\n  build:\n    runs-on: ubuntu-latest\n    timeout-minutes: 10\n    steps:\n      - uses: actions/checkout@v4\n      - run: cargo test\n";

    #[test]
    fn clean_workflow_has_no_findings() {
        assert_eq!(rules(CLEAN), Vec::<String>::new());
    }

    #[test]
    fn yaml_syntax_error_has_position() {
        let f = has("on: push\njobs:\n  a:\n    runs-on: [x\n", "yaml-syntax");
        assert_eq!(f.severity, Severity::Error);
        assert!(f.line >= 4, "{f:?}");
        assert!(f.column > 0);
        assert_eq!(rules("on: push\njobs:\n  a:\n    runs-on: [x\n").len(), 1);
    }

    #[test]
    fn on_and_jobs_rules() {
        assert_eq!(
            has(
                "jobs:\n  a:\n    runs-on: x\n    steps: [{run: a}]\n",
                "on-missing"
            )
            .line,
            1
        );
        let e = has(
            "on:\njobs:\n  a:\n    runs-on: x\n    steps: [{run: a}]\n",
            "on-empty",
        );
        assert_eq!(e.line, 1);
        let u = has(
            "on:\n  push:\n  pull-request:\npermissions: {}\njobs:\n  a:\n    runs-on: x\n    steps: [{run: a}]\n",
            "unknown-event",
        );
        assert_eq!(u.line, 3);
        assert!(u.hint.contains("pull_request"));
        assert!(rules("on: push\n").contains(&"jobs-missing".to_owned()));
        assert!(rules("on: push\njobs: {}\n").contains(&"jobs-missing".to_owned()));
    }

    #[test]
    fn job_and_step_rules() {
        let text = "on: push\npermissions: {}\njobs:\n  a:\n    timeout-minutes: 5\n    steps:\n      - name: both\n        uses: actions/checkout@v4\n        run: echo\n      - name: neither\n      - id: x\n        run: a\n      - id: x\n        run: b\n  call:\n    uses: ./.github/workflows/r.yml\n";
        let r = rules(text);
        assert!(r.contains(&"runs-on-missing".to_owned()), "{r:?}");
        assert!(r.contains(&"step-uses-and-run".to_owned()));
        assert!(r.contains(&"step-no-action".to_owned()));
        assert!(r.contains(&"duplicate-step-id".to_owned()));
        // A reusable-workflow job needs neither runs-on nor steps.
        assert_eq!(r.iter().filter(|x| *x == "runs-on-missing").count(), 1);
        assert_eq!(has(text, "runs-on-missing").line, 4);
        assert_eq!(has(text, "step-uses-and-run").line, 7);
        assert_eq!(has(text, "step-no-action").line, 10);
        assert_eq!(has(text, "duplicate-step-id").line, 13);
    }

    #[test]
    fn needs_unknown_and_cycles() {
        let text = "on: push\npermissions: {}\njobs:\n  a:\n    runs-on: x\n    timeout-minutes: 1\n    needs: [c]\n    steps: [{run: a}]\n  b:\n    runs-on: x\n    timeout-minutes: 1\n    needs: a\n    steps: [{run: a}]\n  c:\n    runs-on: x\n    timeout-minutes: 1\n    needs: [b, buld]\n    steps: [{run: a}]\n";
        let u = has(text, "needs-unknown");
        assert!(u.message.contains("`buld`"));
        assert_eq!(u.line, 17);
        let c = has(text, "needs-cycle");
        assert!(c.message.contains("a → c → b → a"), "{}", c.message);
        assert_eq!(
            rules(text).iter().filter(|r| *r == "needs-cycle").count(),
            1
        );
    }

    #[test]
    fn expression_rules() {
        let text = "on: push\npermissions: {}\njobs:\n  a:\n    runs-on: ${{ matrix.os }}\n    timeout-minutes: 1\n    if: secrets.TOKEN != ''\n    steps:\n      - run: echo ${{ github.sha }} ${{ githb.ref }}\n      - run: echo ${{ github.sha\n      - if: ${{ toJSON(github) && bogus(1) }}\n        run: echo 'x'\n      - run: echo ${{ 'literal' == format('{0}', env.X) }}\n";
        let r = rules(text);
        assert!(r.contains(&"secrets-in-if".to_owned()), "{r:?}");
        let ctx = has(text, "expression-context");
        assert!(ctx.message.contains("`githb`"));
        assert!(ctx.hint.contains("github"));
        assert_eq!(ctx.line, 9);
        assert_eq!(has(text, "expression-unbalanced").line, 10);
        assert!(has(text, "expression-function").message.contains("bogus"));
        assert_eq!(r.iter().filter(|x| *x == "expression-context").count(), 1);
    }

    #[test]
    fn identifiers_skip_literals_and_properties() {
        let (c, f) = identifiers(
            "github.event.pull_request.head.sha == 'a.b' && contains(needs.x.result, 'fail') || 1e3 > matrix.n",
        );
        assert_eq!(c, ["github", "needs", "matrix"]);
        assert_eq!(f, ["contains"]);
        let (c, _) = identifiers("true && null || 'it''s'");
        assert!(c.is_empty());
        assert_eq!(
            expressions("a ${{ x }} b ${{ y }}"),
            (vec!["x", "y"], false)
        );
        assert_eq!(expressions("a ${{ x"), (vec![], true));
    }

    #[test]
    fn deprecated_unpinned_and_commands() {
        let text = "on: push\npermissions: {}\njobs:\n  a:\n    runs-on: x\n    timeout-minutes: 1\n    steps:\n      - uses: actions/checkout@v2\n      - uses: actions/upload-artifact@v3.1.0\n      - uses: docker/login-action@v3\n      - uses: docker/login-action@0123456789abcdef0123456789abcdef01234567\n      - uses: ./local\n      - run: echo \"::set-output name=x::1\"\n      - run: echo \"::add-path::/x\"\n";
        let fs = lint("t.yml", text);
        let dep: Vec<&Finding> = fs
            .iter()
            .filter(|f| f.rule == "deprecated-action")
            .collect();
        assert_eq!(dep.len(), 2);
        assert_eq!(dep[0].line, 8);
        let pin: Vec<&Finding> = fs.iter().filter(|f| f.rule == "unpinned-action").collect();
        assert_eq!(pin.len(), 1);
        assert_eq!(pin[0].line, 10);
        assert_eq!(pin[0].column, 15);
        let cmd: Vec<&Finding> = fs
            .iter()
            .filter(|f| f.rule == "deprecated-command")
            .collect();
        assert_eq!(cmd.len(), 2);
        assert_eq!(cmd[0].severity, Severity::Warning);
        assert_eq!(cmd[1].severity, Severity::Error);
    }

    #[test]
    fn pull_request_target_checkout() {
        let text = "on: pull_request_target\npermissions: {}\njobs:\n  a:\n    runs-on: x\n    timeout-minutes: 1\n    steps:\n      - uses: actions/checkout@v4\n        with:\n          ref: ${{ github.event.pull_request.head.sha }}\n";
        let f = has(text, "prt-checkout");
        assert_eq!(f.line, 10);
        let safe = text.replace("on: pull_request_target", "on: pull_request");
        assert!(!rules(&safe).contains(&"prt-checkout".to_owned()));
    }

    #[test]
    fn permissions_and_timeout() {
        let text = "on: push\njobs:\n  a:\n    runs-on: x\n    steps: [{run: a}]\n";
        let p = has(text, "permissions-missing");
        assert_eq!(p.severity, Severity::Warning);
        let t = has(text, "timeout-missing");
        assert_eq!((t.severity, t.line), (Severity::Info, 3));
        let per_job = "on: push\njobs:\n  a:\n    runs-on: x\n    permissions: read-all\n    timeout-minutes: 3\n    steps: [{run: a}]\n";
        assert!(rules(per_job).is_empty(), "{:?}", rules(per_job));
    }

    #[test]
    fn dispatch_inputs_rules() {
        let text = "on:\n  workflow_dispatch:\n    inputs:\n      a:\n        type: choice\n      b:\n        type: choice\n        options: [x, y]\n        default: z\n      c:\n        type: text\npermissions: {}\njobs:\n  j:\n    runs-on: x\n    timeout-minutes: 1\n    steps: [{run: a}]\n";
        assert_eq!(has(text, "dispatch-choice-options").line, 4);
        assert_eq!(has(text, "dispatch-default-option").line, 9);
        assert_eq!(has(text, "dispatch-input-type").line, 10);
    }

    #[test]
    fn cron_expressions() {
        assert_eq!(check_cron("0 3 * * 1"), Ok(false));
        assert_eq!(check_cron("*/15 0-6,22 1 JAN-MAR mon-fri"), Ok(false));
        assert_eq!(check_cron("* * * * *"), Ok(true));
        assert_eq!(check_cron("*/2 * * * *"), Ok(true));
        assert!(check_cron("60 * * * *").unwrap_err().contains("0-59"));
        assert!(check_cron("0 3 * *").unwrap_err().contains("5 fields"));
        assert!(check_cron("0 3 0 * *").unwrap_err().contains("1-31"));
        assert!(check_cron("0 3 * 13 *").is_err());
        assert!(check_cron("0 3 * * 7").is_err());
        assert!(check_cron("0 5-2 * * *").unwrap_err().contains("backwards"));
        assert!(check_cron("0 */0 * * *").is_err());
        assert!(check_cron("@daily").is_err());
        let text = "on:\n  schedule:\n    - cron: \"0 25 * * *\"\n    - cron: \"* * * * *\"\npermissions: {}\njobs:\n  j:\n    runs-on: x\n    timeout-minutes: 1\n    steps: [{run: a}]\n";
        let f = has(text, "cron-invalid");
        assert_eq!(f.line, 3);
        assert!(f.message.contains("hour `25`"));
        assert_eq!(has(text, "cron-frequency").line, 4);
    }

    #[test]
    fn matrix_keys() {
        let text = "on: push\npermissions: {}\njobs:\n  j:\n    runs-on: x\n    timeout-minutes: 1\n    strategy:\n      matrix:\n        os: [a, b]\n        node: [1, 2]\n        exclude:\n          - os: a\n            nod: 1\n        include:\n          - experimental: true\n          - os: b\n            extra: 1\n    steps: [{run: a}]\n";
        let fs = lint("t.yml", text);
        let ex: Vec<&Finding> = fs
            .iter()
            .filter(|f| f.rule == "matrix-unknown-key" && f.severity == Severity::Error)
            .collect();
        assert_eq!(ex.len(), 1);
        assert_eq!(ex[0].line, 13);
        assert!(ex[0].message.contains("`nod`"));
        let inc: Vec<&Finding> = fs
            .iter()
            .filter(|f| f.rule == "matrix-unknown-key" && f.severity == Severity::Info)
            .collect();
        assert_eq!(inc.len(), 1);
        assert_eq!(inc[0].line, 15);
    }

    #[test]
    fn actionlint_output() {
        let out = r#"[{"message":"label \"ubuntu-lates\" is unknown","filepath":".github\\workflows\\ci.yml","line":7,"column":14,"kind":"runner-label","snippet":"x","end_column":26},
                     {"message":"SC2086","filepath":".github/workflows/ci.yml","line":9,"column":9,"kind":"shellcheck"}]"#;
        let f = parse_actionlint(out).unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].file, ".github/workflows/ci.yml");
        assert_eq!((f[0].line, f[0].column), (7, 14));
        assert_eq!(f[0].rule, "runner-label");
        assert_eq!(f[0].source, Source::Actionlint);
        assert_eq!(f[1].severity, Severity::Warning);
        assert_eq!(parse_actionlint("").unwrap(), Vec::new());
        assert!(parse_actionlint("not json").is_err());
        let spec = actionlint_spec(Path::new("."), &[".github/workflows/a.yml".into()]);
        assert_eq!(
            spec.args,
            ["-format", "{{json .}}", ".github/workflows/a.yml"]
        );
    }
}
