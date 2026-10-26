//! Cargo: build / check / test / clippy / fmt / doc for a package or a
//! whole workspace and per member, `run` for every binary, examples, and
//! the aliases of `.cargo/config.toml`.

use std::path::Path;

use super::{DirInfo, Found, Source, Task, expand_globs, join_rel};
use crate::devops::tools::Tool;
use crate::devops::workbench::toml::{self, Table};

/// The commands offered for a package or a workspace.
const COMMANDS: [(&str, &str); 6] = [
    ("build", "Compile"),
    ("check", "Type-check without producing binaries"),
    ("test", "Build and run the tests"),
    ("clippy", "Run the Clippy lints"),
    ("doc", "Build the documentation"),
    ("fmt", "Format the code with rustfmt"),
];

fn cargo_task(
    dir: &str,
    name: &str,
    args: &[&str],
    file: &str,
    line: Option<u32>,
    desc: String,
) -> Task {
    Task::new(Source::Cargo, dir, name, "cargo", args, file, line, desc).tool(Tool::Cargo)
}

/// A package: its name, binaries and examples.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub line: Option<u32>,
    /// `(name, line in Cargo.toml or None, file)`.
    pub bins: Vec<(String, Option<u32>, String)>,
    pub examples: Vec<(String, Option<u32>, String)>,
}

fn targets(
    t: &Table,
    key: &str,
    pkg_dir: &Path,
    rel: &str,
    implicit: &[(&str, bool)],
) -> Vec<(String, Option<u32>, String)> {
    let mut out: Vec<(String, Option<u32>, String)> = Vec::new();
    if let Some(items) = t.get(key).and_then(|v| v.as_array()) {
        for it in items {
            if let Some(tab) = it.as_table()
                && let Some(e) = tab.entry("name")
                && let Some(n) = e.value.as_str()
            {
                out.push((n.to_owned(), Some(e.line), join_rel(rel, "Cargo.toml")));
            }
        }
    }
    // Implicit targets in a directory: `dir/*.rs` and `dir/*/main.rs`.
    for (sub, enabled) in implicit {
        if !enabled {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(pkg_dir.join(sub)) else {
            continue;
        };
        let mut found: Vec<(String, String)> = Vec::new();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let path = e.path();
            if path.is_file() && name.ends_with(".rs") {
                found.push((
                    name.trim_end_matches(".rs").to_owned(),
                    join_rel(rel, &format!("{sub}/{name}")),
                ));
            } else if path.join("main.rs").is_file() {
                found.push((
                    name.clone(),
                    join_rel(rel, &format!("{sub}/{name}/main.rs")),
                ));
            }
        }
        found.sort();
        for (n, f) in found {
            if !out.iter().any(|(o, _, _)| *o == n) {
                out.push((n, None, f));
            }
        }
    }
    out
}

/// Read a package manifest (already parsed).
pub fn package(t: &Table, pkg_dir: &Path, rel: &str) -> Option<Package> {
    let pkg = t.table(&["package"])?;
    let name = pkg.str_at(&["name"])?.to_owned();
    let line = t.entry("package").map(|e| e.line);
    let autobins = pkg.get("autobins").and_then(|v| v.as_bool()) != Some(false);
    let autoexamples = pkg.get("autoexamples").and_then(|v| v.as_bool()) != Some(false);
    let mut bins = targets(t, "bin", pkg_dir, rel, &[("src/bin", autobins)]);
    if autobins && pkg_dir.join("src/main.rs").is_file() && !bins.iter().any(|(n, _, _)| *n == name)
    {
        bins.insert(0, (name.clone(), line, join_rel(rel, "src/main.rs")));
    }
    let examples = targets(t, "example", pkg_dir, rel, &[("examples", autoexamples)]);
    Some(Package {
        name,
        line,
        bins,
        examples,
    })
}

/// Tasks of one package. With `in_workspace`, commands select it with
/// `-p NAME`.
fn package_tasks(dir: &str, pkg: &Package, in_workspace: bool) -> Vec<Task> {
    let file = join_rel(dir, "Cargo.toml");
    let mut out = Vec::new();
    for (cmd, what) in COMMANDS {
        if cmd == "fmt" && in_workspace {
            // rustfmt formats a whole workspace at once (see the root).
            continue;
        }
        let mut args: Vec<&str> = vec![cmd];
        if in_workspace {
            args.extend(["-p", pkg.name.as_str()]);
        }
        if cmd == "doc" {
            args.push("--no-deps");
        }
        out.push(cargo_task(
            dir,
            cmd,
            &args,
            &file,
            pkg.line,
            format!("{what}: package {}", pkg.name),
        ));
    }
    for (bin, line, f) in &pkg.bins {
        let mut args: Vec<&str> = vec!["run"];
        if in_workspace {
            args.extend(["-p", pkg.name.as_str()]);
        }
        args.extend(["--bin", bin.as_str()]);
        let mut t = cargo_task(
            dir,
            &format!("run {bin}"),
            &args,
            if line.is_some() { &file } else { f },
            *line,
            format!("Build and run the {bin} binary ({f})"),
        );
        t.args_separator = true;
        out.push(t);
    }
    for (ex, line, f) in &pkg.examples {
        let mut args: Vec<&str> = vec!["run"];
        if in_workspace {
            args.extend(["-p", pkg.name.as_str()]);
        }
        args.extend(["--example", ex.as_str()]);
        let mut t = cargo_task(
            dir,
            &format!("example {ex}"),
            &args,
            if line.is_some() { &file } else { f },
            *line,
            format!("Build and run the {ex} example ({f})"),
        );
        t.args_separator = true;
        out.push(t);
    }
    out
}

/// Tasks of a Cargo.toml in `dir` (relative to `root`) plus its workspace
/// members, and the directories they cover.
pub fn cargo_tasks(
    root: &Path,
    dir: &str,
    text: &str,
) -> Result<(Vec<Task>, Vec<(String, String)>, bool), String> {
    let t = toml::parse(text).map_err(|e| format!("Not valid TOML: {e}"))?;
    let file = join_rel(dir, "Cargo.toml");
    let abs = if dir.is_empty() {
        root.to_path_buf()
    } else {
        root.join(dir)
    };
    let mut tasks = Vec::new();
    // (member dir, package name)
    let mut members: Vec<(String, String)> = Vec::new();
    let workspace = t.table(&["workspace"]);
    if let Some(ws) = workspace {
        let globs = ws.get("members").map(|m| m.strings()).unwrap_or_default();
        let exclude = ws.get("exclude").map(|m| m.strings()).unwrap_or_default();
        let mut dirs = expand_globs(root, dir, &globs);
        let excluded = expand_globs(root, dir, &exclude);
        dirs.retain(|d| !excluded.contains(d) && root.join(d).join("Cargo.toml").is_file());
        let line = t.entry("workspace").map(|e| e.line);
        let has_root_pkg = t.table(&["package"]).is_some();
        let count = dirs.len() + has_root_pkg as usize;
        for (cmd, what) in COMMANDS {
            let mut args: Vec<&str> = vec![cmd];
            match cmd {
                "fmt" => args.push("--all"),
                "doc" => args.extend(["--workspace", "--no-deps"]),
                _ => args.push("--workspace"),
            }
            tasks.push(cargo_task(
                dir,
                cmd,
                &args,
                &file,
                line,
                format!(
                    "{what}: the whole workspace ({} member{})",
                    count,
                    if count == 1 { "" } else { "s" }
                ),
            ));
        }
        for m in &dirs {
            let mfile = join_rel(m, "Cargo.toml");
            let Ok(mtext) = std::fs::read_to_string(root.join(&mfile)) else {
                continue;
            };
            let Ok(mt) = toml::parse(&mtext) else {
                continue;
            };
            if let Some(pkg) = package(&mt, &root.join(m), m) {
                members.push((m.clone(), pkg.name.clone()));
                tasks.extend(package_tasks(m, &pkg, true));
            }
        }
        if has_root_pkg && let Some(pkg) = package(&t, &abs, dir) {
            // The root package's own commands, selected by name.
            let mut own = package_tasks(dir, &pkg, true);
            for task in &mut own {
                task.id = format!("cargo:{dir}:{} ({})", task.name, pkg.name);
                task.name = format!("{} ({})", task.name, pkg.name);
            }
            tasks.extend(own);
        }
    } else if let Some(pkg) = package(&t, &abs, dir) {
        tasks.extend(package_tasks(dir, &pkg, false));
    }
    Ok((tasks, members, workspace.is_some()))
}

/// `[alias]` of a `.cargo/config.toml`: `cargo NAME` running the
/// expansion.
pub fn cargo_aliases(dir: &str, file: &str, text: &str) -> Result<Vec<Task>, String> {
    let t = toml::parse(text).map_err(|e| format!("Not valid TOML: {e}"))?;
    let Some(alias) = t.table(&["alias"]) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for e in &alias.entries {
        let expansion = match &e.value {
            toml::Value::Str(s) => s.clone(),
            v @ toml::Value::Array(_) => v.strings().join(" "),
            _ => continue,
        };
        let mut task = cargo_task(
            dir,
            &e.key,
            &[e.key.as_str()],
            file,
            Some(e.line),
            format!("Alias for cargo {expansion}"),
        )
        .body(format!("cargo {expansion}"));
        task.id = format!("cargo:{dir}:alias {}", e.key);
        out.push(task);
    }
    Ok(out)
}

/// Scan a directory; returns the directories now covered (a workspace
/// root covers its members).
pub(crate) fn scan(root: &Path, info: &DirInfo, found: &mut Found) -> Vec<String> {
    let mut covered = Vec::new();
    if info.has("Cargo.toml") {
        let file = info.rel_file("Cargo.toml");
        if let Some(text) = found.read(root, &file) {
            match cargo_tasks(root, &info.rel, &text) {
                Ok((tasks, members, workspace)) => {
                    covered.push(info.rel.clone());
                    if workspace {
                        found.project(&info.rel).workspace = true;
                    }
                    if let Ok(t) = toml::parse(&text)
                        && let Some(n) = t.str_at(&["package", "name"])
                        && found.project(&info.rel).name
                            == info.rel.rsplit('/').next().unwrap_or("")
                    {
                        found.project(&info.rel).name = n.to_owned();
                    }
                    for (m, name) in &members {
                        let p = found.project(m);
                        p.member_of = Some(info.rel.clone());
                        p.name = name.clone();
                        covered.push(m.clone());
                    }
                    found.add(tasks);
                }
                Err(e) => found.problem(&file, e),
            }
        }
    }
    if info.has_dir(".cargo") {
        for name in ["config.toml", "config"] {
            let rel = info.rel_file(&format!(".cargo/{name}"));
            if root.join(&rel).is_file() {
                if let Some(text) = found.read(root, &rel) {
                    match cargo_aliases(&info.rel, &rel, &text) {
                        Ok(t) => found.add(t),
                        Err(e) => found.problem(&rel, e),
                    }
                }
                break;
            }
        }
    }
    covered
}

#[cfg(test)]
mod tests {
    use super::super::testutil::Tree;
    use super::*;

    #[test]
    fn workspace_members_bins_and_examples() {
        let t = Tree::new("cargo-ws");
        t.file(
            "Cargo.toml",
            "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\nexclude = [\"crates/skip\"]\n",
        )
        .file(
            "crates/core/Cargo.toml",
            "[package]\nname = \"demo-core\"\n\n[[example]]\nname = \"declared\"\npath = \"ex/declared.rs\"\n",
        )
        .file("crates/core/src/lib.rs", "")
        .file("crates/core/examples/bench.rs", "fn main() {}")
        .file("crates/app/Cargo.toml", "[package]\nname = \"demo\"\n")
        .file("crates/app/src/main.rs", "fn main() {}")
        .file("crates/app/src/bin/tool.rs", "fn main() {}")
        .file("crates/skip/Cargo.toml", "[package]\nname = \"skip\"\n")
        .file(".cargo/config.toml", "[alias]\nxt = \"run -p xtask --\"\nb = [\"build\", \"--release\"]\n");
        let text = std::fs::read_to_string(t.path().join("Cargo.toml")).unwrap();
        let (tasks, members, ws) = cargo_tasks(t.path(), "", &text).unwrap();
        assert!(ws);
        assert_eq!(
            members,
            vec![
                ("crates/app".to_owned(), "demo".to_owned()),
                ("crates/core".to_owned(), "demo-core".to_owned())
            ]
        );
        let lines: Vec<String> = tasks
            .iter()
            .map(|t| format!("{} @{}", t.command_line(&[]), t.dir))
            .collect();
        for want in [
            "cargo build --workspace @",
            "cargo fmt --all @",
            "cargo doc --workspace --no-deps @",
            "cargo test -p demo @crates/app",
            "cargo run -p demo --bin demo @crates/app",
            "cargo run -p demo --bin tool @crates/app",
            "cargo run -p demo-core --example declared @crates/core",
            "cargo run -p demo-core --example bench @crates/core",
        ] {
            assert!(lines.iter().any(|l| l == want), "{want} not in {lines:#?}");
        }
        assert!(!lines.iter().any(|l| l.contains("skip")), "excluded member");
        assert!(
            !lines.iter().any(|l| l.contains("fmt -p")),
            "fmt only at the workspace level"
        );
        let run = tasks.iter().find(|t| t.name == "run tool").unwrap();
        assert_eq!(run.file, "crates/app/src/bin/tool.rs");
        assert!(run.args_separator);
        assert_eq!(
            run.command_line(&["--help".into()]),
            "cargo run -p demo --bin tool -- --help"
        );
        let declared = tasks.iter().find(|t| t.name == "example declared").unwrap();
        assert_eq!(declared.line, Some(5));

        let config = std::fs::read_to_string(t.path().join(".cargo/config.toml")).unwrap();
        let aliases = cargo_aliases("", ".cargo/config.toml", &config).unwrap();
        assert_eq!(aliases.len(), 2);
        assert_eq!(aliases[0].command_line(&[]), "cargo xt");
        assert_eq!(aliases[0].description, "Alias for cargo run -p xtask --");
        assert_eq!(aliases[1].body.as_deref(), Some("cargo build --release"));
        assert_eq!(aliases[1].line, Some(3));
    }

    #[test]
    fn single_package() {
        let t = Tree::new("cargo-one");
        t.file(
            "Cargo.toml",
            "[package]\nname = \"solo\"\nversion = \"0.1.0\"\n",
        )
        .file("src/main.rs", "fn main() {}");
        let text = std::fs::read_to_string(t.path().join("Cargo.toml")).unwrap();
        let (tasks, members, ws) = cargo_tasks(t.path(), "", &text).unwrap();
        assert!(!ws);
        assert!(members.is_empty());
        let cmds: Vec<String> = tasks.iter().map(|t| t.command_line(&[])).collect();
        assert_eq!(
            cmds,
            vec![
                "cargo build",
                "cargo check",
                "cargo test",
                "cargo clippy",
                "cargo doc --no-deps",
                "cargo fmt",
                "cargo run --bin solo"
            ]
        );
        assert_eq!(tasks[0].line, Some(1));
        assert!(cargo_tasks(t.path(), "", "[package\nname=").is_err());
    }
}
