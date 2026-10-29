//! package.json scripts (with the package manager the lockfile names and
//! workspaces), composer.json scripts and deno.json(c) tasks.

use std::path::Path;

use serde_json::Value;

use super::{DirInfo, Found, Source, Task, expand_globs, join_rel, line_of};
use crate::devops::tools::Tool;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PackageManager {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

impl PackageManager {
    pub fn program(self) -> &'static str {
        match self {
            PackageManager::Npm => "npm",
            PackageManager::Pnpm => "pnpm",
            PackageManager::Yarn => "yarn",
            PackageManager::Bun => "bun",
        }
    }

    pub fn tool(self) -> Tool {
        match self {
            PackageManager::Npm => Tool::Npm,
            PackageManager::Pnpm => Tool::Pnpm,
            PackageManager::Yarn => Tool::Yarn,
            PackageManager::Bun => Tool::Bun,
        }
    }

    fn from_name(s: &str) -> Option<Self> {
        let name = s.split('@').next().unwrap_or(s).trim();
        Some(match name {
            "npm" => PackageManager::Npm,
            "pnpm" => PackageManager::Pnpm,
            "yarn" => PackageManager::Yarn,
            "bun" => PackageManager::Bun,
            _ => return None,
        })
    }
}

const LOCKFILES: [(&str, PackageManager); 6] = [
    ("pnpm-lock.yaml", PackageManager::Pnpm),
    ("yarn.lock", PackageManager::Yarn),
    ("bun.lockb", PackageManager::Bun),
    ("bun.lock", PackageManager::Bun),
    ("package-lock.json", PackageManager::Npm),
    ("npm-shrinkwrap.json", PackageManager::Npm),
];

/// The package manager for a package in `dir` (relative to `root`): the
/// lockfile in the directory or the nearest one above it (workspace members
/// share the root's), then the `packageManager` field, else npm. Returns it
/// with the reason (`pnpm-lock.yaml`, `packageManager: pnpm@9.1.0`, …).
pub fn package_manager(
    root: &Path,
    dir: &str,
    package_manager_field: Option<&str>,
) -> (PackageManager, String) {
    let mut d = dir.to_owned();
    loop {
        let abs = if d.is_empty() {
            root.to_path_buf()
        } else {
            root.join(&d)
        };
        for (file, pm) in LOCKFILES {
            if abs.join(file).is_file() {
                return (pm, join_rel(&d, file));
            }
        }
        if d.is_empty() {
            break;
        }
        d = match d.rfind('/') {
            Some(i) => d[..i].to_owned(),
            None => String::new(),
        };
    }
    if let Some(f) = package_manager_field
        && let Some(pm) = PackageManager::from_name(f)
    {
        return (pm, format!("packageManager: {f}"));
    }
    (PackageManager::Npm, "no lockfile".to_owned())
}

/// Keys of the object at top-level member `member`, with their lines, in
/// source order (serde_json sorts keys). Works on JSON with comments too.
pub fn json_member_lines(text: &str, member: &str) -> Vec<(String, u32)> {
    #[derive(Clone)]
    struct Level {
        object: bool,
        expect_key: bool,
        last_key: Option<String>,
        key: Option<String>,
    }
    let b = text.as_bytes();
    let mut stack: Vec<Level> = Vec::new();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let start = i;
                i += 1;
                let mut s = Vec::new();
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        i += 1;
                        s.push(match b[i] {
                            b'n' => b'\n',
                            b't' => b'\t',
                            c => c,
                        });
                    } else {
                        s.push(b[i]);
                    }
                    i += 1;
                }
                let s = String::from_utf8_lossy(&s).into_owned();
                if let Some(top) = stack.last_mut()
                    && top.object
                    && top.expect_key
                {
                    top.expect_key = false;
                    top.last_key = Some(s.clone());
                    if stack.len() == 2 && stack[1].key.as_deref() == Some(member) {
                        out.push((s, line_of(text, start)));
                    }
                }
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            c @ (b'{' | b'[') => {
                let key = stack.last().and_then(|l| l.last_key.clone());
                stack.push(Level {
                    object: c == b'{',
                    expect_key: c == b'{',
                    last_key: None,
                    key,
                });
            }
            b'}' | b']' => {
                stack.pop();
            }
            b',' => {
                if let Some(top) = stack.last_mut()
                    && top.object
                {
                    top.expect_key = true;
                    top.last_key = None;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// JSON with comments and trailing commas → JSON, keeping every newline
/// (so line numbers still match).
pub(crate) fn strip_jsonc(text: &str) -> String {
    let b = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                out.push(b'"');
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        out.push(b[i]);
                        i += 1;
                    }
                    out.push(b[i]);
                    i += 1;
                }
                if i < b.len() {
                    out.push(b'"');
                }
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    out.push(b' ');
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                out.extend_from_slice(b"  ");
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    out.push(if b[i] == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                }
                out.extend_from_slice(b"  ");
                i += 1;
            }
            b',' => {
                // A trailing comma: the next thing that is neither blank nor
                // a comment is a closer.
                let mut j = i + 1;
                loop {
                    while j < b.len() && b[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if b.get(j) == Some(&b'/') && b.get(j + 1) == Some(&b'/') {
                        while j < b.len() && b[j] != b'\n' {
                            j += 1;
                        }
                    } else if b.get(j) == Some(&b'/') && b.get(j + 1) == Some(&b'*') {
                        j += 2;
                        while j + 1 < b.len() && !(b[j] == b'*' && b[j + 1] == b'/') {
                            j += 1;
                        }
                        j += 2;
                    } else {
                        break;
                    }
                }
                out.push(if matches!(b.get(j), Some(b'}' | b']')) {
                    b' '
                } else {
                    b','
                });
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What a package.json declares.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageInfo {
    pub name: Option<String>,
    pub package_manager: Option<String>,
    /// Workspace globs (`workspaces` as an array or `{ packages }`).
    pub workspaces: Vec<String>,
}

/// Scripts of a package.json as tasks for `pm`, in source order, with the
/// package info.
pub fn package_scripts(
    dir: &str,
    file: &str,
    text: &str,
    pm: PackageManager,
    pm_reason: &str,
) -> Result<(Vec<Task>, PackageInfo), String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("Not valid JSON: {e}"))?;
    let info = PackageInfo {
        name: v.get("name").and_then(Value::as_str).map(str::to_owned),
        package_manager: v
            .get("packageManager")
            .and_then(Value::as_str)
            .map(str::to_owned),
        workspaces: match v.get("workspaces") {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            Some(Value::Object(o)) => o
                .get("packages")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        },
    };
    let Some(scripts) = v.get("scripts").and_then(Value::as_object) else {
        return Ok((Vec::new(), info));
    };
    let lines = json_member_lines(text, "scripts");
    let mut ordered: Vec<(String, Option<u32>)> =
        lines.iter().map(|(k, l)| (k.clone(), Some(*l))).collect();
    for k in scripts.keys() {
        if !ordered.iter().any(|(o, _)| o == k) {
            ordered.push((k.clone(), None));
        }
    }
    let mut tasks = Vec::new();
    for (name, line) in ordered {
        let Some(body) = scripts.get(&name).and_then(Value::as_str) else {
            continue;
        };
        let mut desc = format!("Script run by {} ({pm_reason})", pm.program());
        if let Some(target) = name
            .strip_prefix("pre")
            .filter(|t| scripts.contains_key(*t))
        {
            desc = format!("Runs before \"{target}\" when that script runs");
        } else if let Some(target) = name
            .strip_prefix("post")
            .filter(|t| scripts.contains_key(*t))
        {
            desc = format!("Runs after \"{target}\" when that script runs");
        } else if matches!(
            name.as_str(),
            "preinstall" | "install" | "postinstall" | "prepare" | "prepublishOnly"
        ) {
            desc = format!(
                "Lifecycle script: {} runs it on install or publish",
                pm.program()
            );
        }
        let mut t = Task::new(
            Source::Npm,
            dir,
            &name,
            pm.program(),
            &["run", &name],
            file,
            line,
            desc,
        )
        .tool(pm.tool())
        .body(body);
        t.args_separator = pm == PackageManager::Npm;
        tasks.push(t);
    }
    Ok((tasks, info))
}

/// `packages:` of a pnpm-workspace.yaml.
fn pnpm_workspace_globs(text: &str) -> Vec<String> {
    let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(text) else {
        return Vec::new();
    };
    v.get("packages")
        .and_then(|p| p.as_sequence())
        .map(|s| {
            s.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn scan_package(root: &Path, dir: &str, found: &mut Found) -> Option<PackageInfo> {
    if !found.done.insert(format!("npm:{dir}")) {
        return None;
    }
    let file = join_rel(dir, "package.json");
    let text = found.read(root, &file)?;
    // The packageManager field is only a fallback, so read it first.
    let field = serde_json::from_str::<Value>(&text).ok().and_then(|v| {
        v.get("packageManager")
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    let (pm, reason) = package_manager(root, dir, field.as_deref());
    match package_scripts(dir, &file, &text, pm, &reason) {
        Ok((tasks, info)) => {
            let p = found.project(dir);
            if let Some(n) = &info.name {
                p.name = n.clone();
            }
            p.package_manager = Some((pm, reason));
            if !tasks.is_empty() && !p.sources.contains(&Source::Npm) {
                p.sources.push(Source::Npm);
            }
            found.add(tasks);
            Some(info)
        }
        Err(e) => {
            found.problem(&file, e);
            None
        }
    }
}

pub(crate) fn scan(root: &Path, info: &DirInfo, found: &mut Found) {
    if info.has("package.json")
        && let Some(pkg) = scan_package(root, &info.rel, found)
    {
        let mut globs = pkg.workspaces.clone();
        if info.has("pnpm-workspace.yaml")
            && let Some(text) = found.read(root, &info.rel_file("pnpm-workspace.yaml"))
        {
            globs.extend(pnpm_workspace_globs(&text));
        }
        if !globs.is_empty() {
            found.project(&info.rel).workspace = true;
            for member in expand_globs(root, &info.rel, &globs) {
                if member == info.rel || !root.join(&member).join("package.json").is_file() {
                    continue;
                }
                found.project(&member).member_of = Some(info.rel.clone());
                scan_package(root, &member, found);
            }
        }
    }
    if info.has("composer.json")
        && let Some(text) = found.read(root, &info.rel_file("composer.json"))
    {
        match composer(&info.rel, &info.rel_file("composer.json"), &text) {
            Ok(t) => found.add(t),
            Err(e) => found.problem(&info.rel_file("composer.json"), e),
        }
    }
    if let Some(name) = info.first(&["deno.json", "deno.jsonc"])
        && let Some(text) = found.read(root, &info.rel_file(&name))
    {
        match deno(&info.rel, &info.rel_file(&name), &text) {
            Ok(t) => found.add(t),
            Err(e) => found.problem(&info.rel_file(&name), e),
        }
    }
}

/// composer.json `scripts` (string or list of commands), with
/// `scripts-descriptions`.
pub fn composer(dir: &str, file: &str, text: &str) -> Result<Vec<Task>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("Not valid JSON: {e}"))?;
    let Some(scripts) = v.get("scripts").and_then(Value::as_object) else {
        return Ok(Vec::new());
    };
    let descs = v.get("scripts-descriptions").and_then(Value::as_object);
    let lines = json_member_lines(text, "scripts");
    let mut out = Vec::new();
    for (name, line) in &lines {
        let Some(val) = scripts.get(name) else {
            continue;
        };
        let body = match val {
            Value::String(s) => s.clone(),
            Value::Array(a) => a
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n"),
            _ => continue,
        };
        let desc = descs
            .and_then(|d| d.get(name))
            .and_then(Value::as_str)
            .unwrap_or("Composer script")
            .to_owned();
        let mut t = Task::new(
            Source::Composer,
            dir,
            name,
            "composer",
            &["run-script", name],
            file,
            Some(*line),
            desc,
        )
        .tool(Tool::Composer)
        .body(body);
        t.args_separator = true;
        out.push(t);
    }
    Ok(out)
}

/// deno.json / deno.jsonc `tasks` (a command string or
/// `{ command, description }`).
pub fn deno(dir: &str, file: &str, text: &str) -> Result<Vec<Task>, String> {
    let clean = strip_jsonc(text);
    let v: Value = serde_json::from_str(&clean).map_err(|e| format!("Not valid JSON: {e}"))?;
    let Some(tasks) = v.get("tasks").and_then(Value::as_object) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (name, line) in json_member_lines(&clean, "tasks") {
        let Some(val) = tasks.get(&name) else {
            continue;
        };
        let (body, desc) = match val {
            Value::String(s) => (s.clone(), "Deno task".to_owned()),
            Value::Object(o) => (
                o.get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                o.get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("Deno task")
                    .to_owned(),
            ),
            _ => continue,
        };
        out.push(
            Task::new(
                Source::Deno,
                dir,
                &name,
                "deno",
                &["task", &name],
                file,
                Some(line),
                desc,
            )
            .tool(Tool::Deno)
            .body(body),
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::testutil::Tree;
    use super::*;

    const PKG: &str = r#"{
  "name": "@acme/web",
  "packageManager": "yarn@4.1.0",
  "scripts": {
    "dev": "vite --port 3000",
    "prebuild": "rimraf dist",
    "build": "tsc -b && vite build",
    "test": "vitest run",
    "postinstall": "husky"
  },
  "dependencies": { "scripts": "1.0.0" }
}"#;

    #[test]
    fn scripts_in_source_order_with_lines() {
        let (tasks, info) = package_scripts(
            "web",
            "web/package.json",
            PKG,
            PackageManager::Pnpm,
            "pnpm-lock.yaml",
        )
        .unwrap();
        let names: Vec<&str> = tasks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["dev", "prebuild", "build", "test", "postinstall"]
        );
        assert_eq!(tasks[0].line, Some(5));
        assert_eq!(tasks[2].body.as_deref(), Some("tsc -b && vite build"));
        assert_eq!(tasks[2].command_line(&[]), "pnpm run build");
        assert!(
            !tasks[2].args_separator,
            "pnpm forwards arguments as they are"
        );
        assert!(tasks[1].description.contains("before \"build\""));
        assert!(tasks[4].description.starts_with("Lifecycle"));
        assert_eq!(info.name.as_deref(), Some("@acme/web"));
        assert_eq!(info.package_manager.as_deref(), Some("yarn@4.1.0"));
        let (npm, _) = package_scripts(
            "",
            "package.json",
            PKG,
            PackageManager::Npm,
            "package-lock.json",
        )
        .unwrap();
        assert_eq!(
            npm[0].command_line(&["--host".into()]),
            "npm run dev -- --host"
        );
        assert!(package_scripts("", "package.json", "{ nope", PackageManager::Npm, "").is_err());
    }

    #[test]
    fn workspaces_forms() {
        let (_, a) = package_scripts(
            "",
            "package.json",
            r#"{"workspaces": ["packages/*", "apps/web"]}"#,
            PackageManager::Npm,
            "",
        )
        .unwrap();
        assert_eq!(a.workspaces, vec!["packages/*", "apps/web"]);
        let (_, b) = package_scripts(
            "",
            "package.json",
            r#"{"workspaces": {"packages": ["libs/*"]}}"#,
            PackageManager::Npm,
            "",
        )
        .unwrap();
        assert_eq!(b.workspaces, vec!["libs/*"]);
        assert_eq!(
            pnpm_workspace_globs("packages:\n  - 'apps/*'\n  - packages/*\n"),
            vec!["apps/*", "packages/*"]
        );
    }

    #[test]
    fn package_manager_detection() {
        let t = Tree::new("pm");
        t.file("package.json", "{}")
            .file("yarn.lock", "")
            .file("sub/package.json", "{}")
            .file("other/package.json", "{}")
            .file("other/bun.lockb", "");
        let root = t.path();
        assert_eq!(
            package_manager(root, "", None),
            (PackageManager::Yarn, "yarn.lock".into())
        );
        assert_eq!(
            package_manager(root, "sub", None),
            (PackageManager::Yarn, "yarn.lock".into()),
            "members inherit the root lockfile"
        );
        assert_eq!(
            package_manager(root, "other", None),
            (PackageManager::Bun, "other/bun.lockb".into())
        );
        let bare = Tree::new("pm-bare");
        bare.file("package.json", "{}");
        assert_eq!(
            package_manager(bare.path(), "", Some("pnpm@9.1.0")).0,
            PackageManager::Pnpm
        );
        assert_eq!(
            package_manager(bare.path(), "", None),
            (PackageManager::Npm, "no lockfile".into())
        );
        for (file, pm) in [
            ("pnpm-lock.yaml", PackageManager::Pnpm),
            ("bun.lock", PackageManager::Bun),
            ("package-lock.json", PackageManager::Npm),
        ] {
            let x = Tree::new("pm-one");
            x.file(file, "");
            assert_eq!(package_manager(x.path(), "", None).0, pm, "{file}");
        }
    }

    #[test]
    fn member_lines_ignore_nested_and_values() {
        let text = "{\n  \"a\": {\"scripts\": {\"x\": 1}},\n  \"scripts\": {\n    \"one\": \"echo \\\"scripts\\\"\",\n    \"two\": \"{}\"\n  }\n}";
        assert_eq!(
            json_member_lines(text, "scripts"),
            vec![("one".to_owned(), 4), ("two".to_owned(), 5)]
        );
    }

    #[test]
    fn composer_and_deno() {
        let c = composer(
            "",
            "composer.json",
            r#"{
  "scripts": {
    "test": "phpunit",
    "lint": ["phpcs src", "phpstan analyse"]
  },
  "scripts-descriptions": { "test": "Run the unit tests" }
}"#,
        )
        .unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].command_line(&[]), "composer run-script test");
        assert_eq!(c[0].description, "Run the unit tests");
        assert_eq!(c[1].body.as_deref(), Some("phpcs src\nphpstan analyse"));
        assert_eq!(c[1].line, Some(4));
        let d = deno(
            "",
            "deno.jsonc",
            "{\n  // tasks\n  \"tasks\": {\n    \"dev\": \"deno run -A main.ts\",\n    \"check\": { \"command\": \"deno check\", \"description\": \"Type-check\" }, /* x */\n  },\n}\n",
        )
        .unwrap();
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].command_line(&[]), "deno task dev");
        assert_eq!(d[0].line, Some(4));
        assert_eq!(d[1].description, "Type-check");
        assert_eq!(d[1].body.as_deref(), Some("deno check"));
    }

    #[test]
    fn jsonc_keeps_lines() {
        let s = strip_jsonc("{\n/* a\nb */\"k\": [1,2,],\n}");
        assert_eq!(s.matches('\n').count(), 3);
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["k"].as_array().unwrap().len(), 2);
    }
}
