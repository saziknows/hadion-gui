//! Makefile targets, justfile recipes and Taskfile tasks.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use super::{DirInfo, Found, Source, Task};
use crate::devops::proc::{ProcSpec, run_capture};
use crate::devops::tools::{self, Tool};

/// Lines of a recipe kept as its body.
const BODY_LINES: usize = 8;

/// Targets of a Makefile: `^target:` lines (several per line allowed),
/// without special targets (`.PHONY`), pattern rules (`%.o`), variable
/// references or assignments. The description comes from a `## text`
/// after the prerequisites or on the line above. File targets
/// (`out/Makefile`, `config.gypi`) are left out unless they are phony or
/// documented.
pub fn makefile(dir: &str, file: &str, text: &str) -> Vec<Task> {
    let lines: Vec<&str> = text.lines().collect();
    // `.PHONY` lines anywhere in the file, before the targets are read.
    let phony: HashSet<String> = lines
        .iter()
        .filter_map(|l| l.strip_prefix(".PHONY"))
        .filter_map(|l| l.trim_start().strip_prefix(':'))
        .flat_map(|l| l.split('#').next().unwrap_or("").split_whitespace())
        .map(str::to_owned)
        .collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    let mut in_define = 0usize;
    let mut i = 0;
    while i < lines.len() {
        let raw = lines[i];
        let line_no = i as u32 + 1;
        i += 1;
        let trimmed = raw.trim_start();
        if trimmed.starts_with("define ") || trimmed == "define" {
            in_define += 1;
            continue;
        }
        if in_define > 0 {
            if trimmed.starts_with("endef") {
                in_define -= 1;
            }
            continue;
        }
        if raw.starts_with('\t') || raw.starts_with('#') || raw.trim().is_empty() {
            continue;
        }
        // Join continuation lines for the prerequisites.
        let mut full = raw.to_owned();
        while full.ends_with('\\') && i < lines.len() {
            full.pop();
            full.push(' ');
            full.push_str(lines[i].trim());
            i += 1;
        }
        let Some((targets, rest)) = split_rule(&full) else {
            continue;
        };
        let (prereqs, inline_desc) = match rest.find("##") {
            Some(p) => (&rest[..p], Some(rest[p + 2..].trim().to_owned())),
            None => (rest, None),
        };
        let prereqs = prereqs.split(';').next().unwrap_or("").trim();
        if targets.trim() == ".PHONY" {
            continue;
        }
        let desc = inline_desc.filter(|d| !d.is_empty()).or_else(|| {
            let prev = lines.get((line_no as usize).checked_sub(2)?)?.trim();
            prev.strip_prefix("##").map(|d| d.trim().to_owned())
        });
        // The recipe: tab-indented lines that follow.
        let mut body = Vec::new();
        let mut j = i;
        while j < lines.len() && lines[j].starts_with('\t') {
            if body.len() < BODY_LINES {
                body.push(lines[j].trim_start_matches('\t'));
            }
            j += 1;
        }
        for target in targets.split_whitespace() {
            if target.starts_with('.')
                || target.contains('%')
                || target.contains('$')
                || target.contains('(')
                // `a b &: deps` (grouped targets).
                || target == "&"
                || ((target.contains('/') || target.contains('.'))
                    && !phony.contains(target)
                    && desc.is_none())
                || !seen.insert(target.to_owned())
            {
                continue;
            }
            let mut description = desc.clone().unwrap_or_default();
            if description.is_empty() && !prereqs.is_empty() {
                description = format!("Depends on {prereqs}");
            }
            out.push(
                Task::new(
                    Source::Make,
                    dir,
                    target,
                    "make",
                    &[target],
                    file,
                    Some(line_no),
                    description,
                )
                .tool(Tool::Make)
                .body(body.join("\n")),
            );
        }
    }
    out
}

/// `targets : rest` of a rule line, or `None` for assignments and
/// directives.
fn split_rule(line: &str) -> Option<(&str, &str)> {
    let first = line.split_whitespace().next()?;
    if matches!(
        first,
        "include"
            | "-include"
            | "sinclude"
            | "export"
            | "unexport"
            | "override"
            | "ifeq"
            | "ifneq"
            | "ifdef"
            | "ifndef"
            | "else"
            | "endif"
            | "vpath"
    ) {
        return None;
    }
    let colon = line.find(':')?;
    let before = &line[..colon];
    // `X = a:b`, `X := …`, `X ?= …`: an assignment, not a rule.
    if before.contains('=') {
        return None;
    }
    let after = &line[colon + 1..];
    if after.starts_with('=') || after.starts_with(":=") || after.starts_with(":::=") {
        return None;
    }
    // `a::` (double-colon rule).
    let after = after.strip_prefix(':').unwrap_or(after);
    if before.trim().is_empty() {
        return None;
    }
    Some((before, after))
}

/// Recipes of a justfile: `name params: deps` at the start of a line,
/// the `# comment` above as the description, `[doc("…")]` too. Private
/// recipes (`_name`, `[private]`) are left out.
pub fn justfile(dir: &str, file: &str, text: &str) -> Vec<Task> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut comment: Option<String> = None;
    let mut doc: Option<String> = None;
    let mut private = false;
    let mut i = 0;
    while i < lines.len() {
        let raw = lines[i];
        let line_no = i as u32 + 1;
        i += 1;
        if raw.trim().is_empty() {
            comment = None;
            doc = None;
            private = false;
            continue;
        }
        if raw.starts_with(' ') || raw.starts_with('\t') {
            continue;
        }
        if let Some(c) = raw.strip_prefix('#') {
            if !c.starts_with('!') {
                comment = Some(c.trim().to_owned());
            }
            continue;
        }
        if raw.starts_with('[') {
            let attr = raw.trim();
            if attr.contains("private") {
                private = true;
            }
            if let Some(p) = attr.find("doc(") {
                let inner = &attr[p + 4..];
                let inner = inner.trim_end_matches([']', ')']);
                doc = Some(inner.trim_matches(['"', '\'']).to_owned());
            }
            continue;
        }
        let first = raw.split_whitespace().next().unwrap_or("");
        if matches!(
            first,
            "set" | "alias" | "export" | "import" | "mod" | "import?" | "mod?"
        ) || raw.contains(":=")
        {
            comment = None;
            continue;
        }
        let Some(colon) = raw.find(':') else {
            continue;
        };
        let head = raw[..colon].trim().trim_start_matches('@');
        let mut parts = head.split_whitespace();
        let Some(name) = parts.next() else { continue };
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            continue;
        }
        let params: Vec<String> = parts
            .map(|p| p.split('=').next().unwrap_or(p).to_owned())
            .collect();
        let deps = raw[colon + 1..].trim();
        let mut body = Vec::new();
        let mut j = i;
        while j < lines.len()
            && (lines[j].starts_with(' ') || lines[j].starts_with('\t') || lines[j].is_empty())
        {
            if !lines[j].trim().is_empty() && body.len() < BODY_LINES {
                body.push(lines[j].trim());
            }
            if lines[j].is_empty() && j + 1 < lines.len() && !lines[j + 1].starts_with([' ', '\t'])
            {
                break;
            }
            j += 1;
        }
        let skip = private || name.starts_with('_');
        let description = doc.take().or_else(|| comment.take()).unwrap_or_else(|| {
            if deps.is_empty() {
                String::new()
            } else {
                format!("Depends on {deps}")
            }
        });
        private = false;
        if skip {
            continue;
        }
        let mut t = Task::new(
            Source::Just,
            dir,
            name,
            "just",
            &[name],
            file,
            Some(line_no),
            description,
        )
        .tool(Tool::Just)
        .body(body.join("\n"));
        t.params = params
            .into_iter()
            .map(|p| p.trim_start_matches(['+', '*', '$']).to_owned())
            .filter(|p| !p.is_empty())
            .collect();
        out.push(t);
    }
    out
}

/// Recipe names from `just --summary` (when just is installed), to catch
/// recipes from imported files and modules.
fn just_summary(dir: &Path, file: &Path) -> Option<Vec<String>> {
    if !tools::available(Tool::Just) {
        return None;
    }
    let spec = ProcSpec::new("just", dir)
        .arg("--summary")
        .arg("--justfile")
        .arg(file.to_string_lossy().into_owned());
    let cap = run_capture(&spec, Some(Duration::from_secs(5))).ok()?;
    cap.success().then(|| {
        cap.stdout_text()
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    })
}

/// Tasks of a Taskfile (`tasks:` mapping; internal tasks left out).
pub fn taskfile(dir: &str, file: &str, text: &str) -> Result<Vec<Task>, String> {
    let v: serde_yaml::Value =
        serde_yaml::from_str(text).map_err(|e| format!("Not valid YAML: {e}"))?;
    let Some(tasks) = v.get("tasks").and_then(|t| t.as_mapping()) else {
        return Ok(Vec::new());
    };
    // Line of each task key: the first indented key under `tasks:`.
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim_end() == "tasks:")
        .unwrap_or(0);
    let indent = lines
        .iter()
        .skip(start + 1)
        .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|l| l.len() - l.trim_start().len())
        .unwrap_or(2);
    let line_of_task = |name: &str| -> Option<u32> {
        lines.iter().enumerate().skip(start + 1).find_map(|(i, l)| {
            let lead = l.len() - l.trim_start().len();
            let key = l.trim_start();
            let k = key
                .strip_prefix(name)
                .or_else(|| key.strip_prefix(&format!("\"{name}\"")))
                .or_else(|| key.strip_prefix(&format!("'{name}'")))?;
            (lead == indent && k.starts_with(':')).then_some(i as u32 + 1)
        })
    };
    let mut out = Vec::new();
    for (k, val) in tasks {
        let Some(name) = k.as_str() else { continue };
        let mut desc = String::new();
        let mut cmds: Vec<String> = Vec::new();
        match val {
            serde_yaml::Value::String(s) => cmds.push(s.clone()),
            serde_yaml::Value::Sequence(s) => {
                cmds.extend(s.iter().filter_map(|c| c.as_str().map(str::to_owned)))
            }
            serde_yaml::Value::Mapping(m) => {
                if m.get("internal").and_then(|x| x.as_bool()) == Some(true) {
                    continue;
                }
                desc = m
                    .get("desc")
                    .or_else(|| m.get("summary"))
                    .and_then(|d| d.as_str())
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                if let Some(seq) = m.get("cmds").and_then(|c| c.as_sequence()) {
                    for c in seq {
                        if let Some(s) = c.as_str() {
                            cmds.push(s.to_owned());
                        } else if let Some(s) = c.get("cmd").and_then(|x| x.as_str()) {
                            cmds.push(s.to_owned());
                        } else if let Some(s) = c.get("task").and_then(|x| x.as_str()) {
                            cmds.push(format!("task: {s}"));
                        }
                    }
                } else if let Some(s) = m.get("cmd").and_then(|c| c.as_str()) {
                    cmds.push(s.to_owned());
                }
                if desc.is_empty()
                    && let Some(deps) = m.get("deps").and_then(|d| d.as_sequence())
                {
                    let names: Vec<&str> = deps.iter().filter_map(|d| d.as_str()).collect();
                    if !names.is_empty() {
                        desc = format!("Depends on {}", names.join(", "));
                    }
                }
            }
            _ => {}
        }
        cmds.truncate(BODY_LINES);
        out.push(
            Task::new(
                Source::Taskfile,
                dir,
                name,
                "task",
                &[name],
                file,
                line_of_task(name),
                desc,
            )
            .tool(Tool::Task)
            .body(cmds.join("\n")),
        );
    }
    Ok(out)
}

pub(crate) fn scan(root: &Path, info: &DirInfo, found: &mut Found) {
    if let Some(name) = info.first(&["GNUmakefile", "Makefile", "makefile"])
        && let Some(text) = found.read(root, &info.rel_file(&name))
    {
        found.add(makefile(&info.rel, &info.rel_file(&name), &text));
    }
    if let Some(name) = info.first(&["justfile", "Justfile", ".justfile"]) {
        let file = info.rel_file(&name);
        if let Some(text) = found.read(root, &file) {
            let mut tasks = justfile(&info.rel, &file, &text);
            if let Some(names) = just_summary(&info.abs, &info.abs.join(&name)) {
                for n in names {
                    if !tasks.iter().any(|t| t.name == n) {
                        tasks.push(
                            Task::new(
                                Source::Just,
                                &info.rel,
                                &n,
                                "just",
                                &[&n],
                                &file,
                                None,
                                "From an imported justfile or module (just --summary)",
                            )
                            .tool(Tool::Just),
                        );
                    }
                }
            }
            found.add(tasks);
        }
    }
    if let Some(name) = info.first(&[
        "Taskfile.yml",
        "Taskfile.yaml",
        "taskfile.yml",
        "taskfile.yaml",
        "Taskfile.dist.yml",
    ]) && let Some(text) = found.read(root, &info.rel_file(&name))
    {
        match taskfile(&info.rel, &info.rel_file(&name), &text) {
            Ok(t) => found.add(t),
            Err(e) => found.problem(&info.rel_file(&name), e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAKEFILE: &str = "\
# Variables
CC := gcc
FLAGS = -O2 -Wall
PREFIX ?= /usr/local
.PHONY: all test clean help
.DEFAULT_GOAL := all

## Build everything
all: build docs

build: src/main.c ## Compile the binary
\t$(CC) $(FLAGS) -o app src/main.c
\t@echo built

test: build
\t./app --self-test

%.o: %.c
\t$(CC) -c $<

clean distclean:
\trm -rf build

$(OUT)/x: y
\ttouch $@

out/Makefile: config.gypi
\tpython configure

docs/api.html: ## Build the API page
\tmake-docs

define HELP_TEXT
not: a target
endef

install:: build
\tcp app $(PREFIX)/bin
";

    #[test]
    fn makefile_targets() {
        let t = makefile("", "Makefile", MAKEFILE);
        let names: Vec<&str> = t.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "all",
                "build",
                "test",
                "clean",
                "distclean",
                "docs/api.html",
                "install"
            ]
        );
        assert_eq!(t[0].description, "Build everything");
        assert_eq!(t[0].line, Some(9));
        assert_eq!(t[1].description, "Compile the binary");
        assert_eq!(
            t[1].body.as_deref(),
            Some("$(CC) $(FLAGS) -o app src/main.c\n@echo built")
        );
        assert_eq!(t[2].description, "Depends on build");
        assert_eq!(t[3].command_line(&[]), "make clean");
        assert_eq!(t[3].line, t[4].line);
        assert_eq!(t[6].line, Some(37));
    }

    #[test]
    fn makefile_continuations_and_assignments() {
        let text = "SRC = a.c \\\n  b.c\nURL = http://x\nweb: \\\n  a b ## Serve\n\tserve\n";
        let t = makefile("sub", "sub/Makefile", text);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].name, "web");
        assert_eq!(t[0].description, "Serve");
        assert_eq!(t[0].id, "make:sub:web");
    }

    const JUSTFILE: &str = "\
set shell := [\"bash\", \"-c\"]
version := \"1.0\"
alias b := build

# Build the project
build:
    cargo build

# Run with arguments
run target='debug' +args='': build
    cargo run --profile {{target}} -- {{args}}

[private]
helper:
    echo private

_hidden:
    echo hidden

[doc('Deploy to an environment')]
@deploy env:
    ./deploy.sh {{env}}
";

    #[test]
    fn justfile_recipes() {
        let t = justfile("", "justfile", JUSTFILE);
        let names: Vec<&str> = t.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["build", "run", "deploy"]);
        assert_eq!(t[0].description, "Build the project");
        assert_eq!(t[0].line, Some(6));
        assert_eq!(t[0].body.as_deref(), Some("cargo build"));
        assert_eq!(t[1].params, vec!["target", "args"]);
        assert_eq!(t[2].description, "Deploy to an environment");
        assert_eq!(t[2].params, vec!["env"]);
        assert_eq!(t[2].command_line(&["prod".into()]), "just deploy prod");
    }

    const TASKFILE: &str = "\
version: '3'

vars:
  NAME: app

tasks:
  build:
    desc: Build the app
    cmds:
      - go build -o {{.NAME}} .
      - task: lint
  lint: golangci-lint run
  test:
    deps: [build]
    cmds:
      - cmd: go test ./...
  secret-step:
    internal: true
    cmds: [echo hi]
";

    #[test]
    fn taskfile_tasks() {
        let t = taskfile("", "Taskfile.yml", TASKFILE).unwrap();
        let names: Vec<&str> = t.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["build", "lint", "test"]);
        assert_eq!(t[0].description, "Build the app");
        assert_eq!(t[0].line, Some(7));
        assert_eq!(
            t[0].body.as_deref(),
            Some("go build -o {{.NAME}} .\ntask: lint")
        );
        assert_eq!(t[1].body.as_deref(), Some("golangci-lint run"));
        assert_eq!(t[2].description, "Depends on build");
        assert_eq!(t[2].line, Some(13));
        assert_eq!(t[2].command_line(&[]), "task test");
        assert!(taskfile("", "Taskfile.yml", "tasks: [").is_err());
    }
}
