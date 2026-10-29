//! Task discovery: what can be run in a working directory and its
//! subprojects (two levels down), with the exact command line, the file
//! and line it comes from and a description.
//!
//! Sources: package.json scripts (with the package manager the lockfile
//! names), Makefile targets, justfile recipes, Taskfile tasks, Cargo
//! commands per workspace member / binary / example plus `.cargo` aliases,
//! composer and deno scripts, Go, .NET, Gradle, Maven, Python (entry
//! points, poe, pytest, tox) and docker compose.

mod cargo;
mod make;
mod node;
mod other;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::devops::proc::{ProcSpec, render_args};
use crate::devops::tools::Tool;

pub use cargo::{CargoScan, cargo_aliases, cargo_tasks};
pub use make::{justfile, makefile, taskfile};
pub use node::{
    PackageManager, composer, deno, json_member_lines, package_manager, package_scripts,
};
pub use other::{compose, dotnet, go, gradle, maven, python};

/// Where a task comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    Npm,
    Make,
    Just,
    Taskfile,
    Cargo,
    Composer,
    Deno,
    Go,
    Dotnet,
    Gradle,
    Maven,
    Python,
    Compose,
}

impl Source {
    pub const ALL: [Source; 13] = [
        Source::Npm,
        Source::Make,
        Source::Just,
        Source::Taskfile,
        Source::Cargo,
        Source::Composer,
        Source::Deno,
        Source::Go,
        Source::Dotnet,
        Source::Gradle,
        Source::Maven,
        Source::Python,
        Source::Compose,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Source::Npm => "package.json",
            Source::Make => "Makefile",
            Source::Just => "justfile",
            Source::Taskfile => "Taskfile",
            Source::Cargo => "Cargo",
            Source::Composer => "Composer",
            Source::Deno => "Deno",
            Source::Go => "Go",
            Source::Dotnet => ".NET",
            Source::Gradle => "Gradle",
            Source::Maven => "Maven",
            Source::Python => "Python",
            Source::Compose => "Compose",
        }
    }

    /// Short stable key used in task ids.
    pub fn key(self) -> &'static str {
        match self {
            Source::Npm => "npm",
            Source::Make => "make",
            Source::Just => "just",
            Source::Taskfile => "task",
            Source::Cargo => "cargo",
            Source::Composer => "composer",
            Source::Deno => "deno",
            Source::Go => "go",
            Source::Dotnet => "dotnet",
            Source::Gradle => "gradle",
            Source::Maven => "maven",
            Source::Python => "python",
            Source::Compose => "compose",
        }
    }
}

/// One runnable task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    /// Stable across sessions: `npm:packages/web:build`.
    pub id: String,
    pub source: Source,
    pub name: String,
    /// Directory it runs in, relative to the root (`""` for the root).
    pub dir: String,
    pub program: String,
    /// `program` is a file in `dir` (`gradlew.bat`, `mvnw.cmd`).
    pub local_program: bool,
    pub args: Vec<String>,
    /// File the task was found in, relative to the root.
    pub file: String,
    /// 1-based line in `file`, when the task is written there.
    pub line: Option<u32>,
    pub description: String,
    /// What it runs: the script body, the recipe lines.
    pub body: Option<String>,
    /// The tool it needs, for the install hint when it is missing.
    pub tool: Option<Tool>,
    /// npm passes arguments to the script only after `--`.
    pub args_separator: bool,
    /// Parameters the recipe takes (`just deploy env`).
    pub params: Vec<String>,
    /// Stops or deletes something (`docker compose down`): confirm first.
    pub destructive: bool,
}

impl Task {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source: Source,
        dir: &str,
        name: &str,
        program: &str,
        args: &[&str],
        file: &str,
        line: Option<u32>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            id: format!("{}:{}:{}", source.key(), dir, name),
            source,
            name: name.to_owned(),
            dir: dir.to_owned(),
            program: program.to_owned(),
            local_program: false,
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            file: file.to_owned(),
            line,
            description: description.into(),
            body: None,
            tool: None,
            args_separator: false,
            params: Vec::new(),
            destructive: false,
        }
    }

    pub fn tool(mut self, t: Tool) -> Self {
        self.tool = Some(t);
        self
    }

    pub fn body(mut self, b: impl Into<String>) -> Self {
        let b = b.into();
        if !b.trim().is_empty() {
            self.body = Some(b);
        }
        self
    }

    pub fn local(mut self) -> Self {
        self.local_program = true;
        self
    }

    /// Program and arguments, with `extra` appended (after `--` for npm).
    pub fn argv(&self, extra: &[String]) -> Vec<String> {
        let mut v = vec![self.program_display()];
        v.extend(self.args.iter().cloned());
        if !extra.is_empty() {
            if self.args_separator {
                v.push("--".to_owned());
            }
            v.extend(extra.iter().cloned());
        }
        v
    }

    fn program_display(&self) -> String {
        if self.local_program && !self.program.contains(['/', '\\']) {
            if cfg!(windows) {
                format!(".\\{}", self.program)
            } else {
                format!("./{}", self.program)
            }
        } else {
            self.program.clone()
        }
    }

    /// The command as the user would type it: `pnpm run build -- --watch`.
    pub fn command_line(&self, extra: &[String]) -> String {
        let argv = self.argv(extra);
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        render_args(&refs)
    }

    /// Where it runs.
    pub fn cwd(&self, root: &Path) -> PathBuf {
        if self.dir.is_empty() {
            root.to_path_buf()
        } else {
            root.join(&self.dir)
        }
    }

    /// The process to start (colour forced, as in a terminal).
    pub fn spec(&self, root: &Path, extra: &[String]) -> ProcSpec {
        let cwd = self.cwd(root);
        let program = if self.local_program {
            cwd.join(&self.program).to_string_lossy().into_owned()
        } else {
            self.program.clone()
        };
        let mut args = self.args.clone();
        if !extra.is_empty() {
            if self.args_separator {
                args.push("--".to_owned());
            }
            args.extend(extra.iter().cloned());
        }
        ProcSpec::new(program, cwd).args(args).with_color()
    }

    /// The program is installed (a PATH lookup, or the wrapper file exists).
    pub fn available(&self, root: &Path) -> bool {
        if self.local_program {
            self.cwd(root).join(&self.program).is_file()
        } else {
            crate::devops::proc::resolve(&self.program).is_some()
        }
    }
}

/// A directory with something runnable in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    /// Relative to the root (`""` for the root).
    pub dir: String,
    /// Package name when one is declared, else the directory name.
    pub name: String,
    pub sources: Vec<Source>,
    /// Package manager and the file that decided it.
    pub package_manager: Option<(PackageManager, String)>,
    /// Directory of the npm / Cargo workspace this project is a member of.
    pub member_of: Option<String>,
    /// Declares a workspace.
    pub workspace: bool,
}

/// A file that could not be read or parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub file: String,
    pub message: String,
}

/// Everything found under a root.
#[derive(Clone, Debug, Default)]
pub struct Discovery {
    pub root: PathBuf,
    pub tasks: Vec<Task>,
    pub projects: Vec<Project>,
    pub problems: Vec<Problem>,
    pub dirs_scanned: usize,
    pub files_read: usize,
    pub elapsed_ms: u64,
}

impl Discovery {
    pub fn task(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn project(&self, dir: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.dir == dir)
    }

    /// Distinct tools the tasks need.
    pub fn tools(&self) -> Vec<Tool> {
        let mut seen = HashSet::new();
        self.tasks
            .iter()
            .filter_map(|t| t.tool)
            .filter(|t| seen.insert(*t))
            .collect()
    }
}

/// Directories never descended into.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "vendor",
    "venv",
    "__pycache__",
    "bin",
    "obj",
    "coverage",
];

/// What one directory holds: file names and sub-directory names.
pub(crate) struct DirInfo {
    pub rel: String,
    pub abs: PathBuf,
    pub files: Vec<String>,
    pub dirs: Vec<String>,
}

impl DirInfo {
    pub fn has(&self, name: &str) -> bool {
        self.files.iter().any(|f| f == name)
    }

    pub fn has_dir(&self, name: &str) -> bool {
        self.dirs.iter().any(|d| d == name)
    }

    /// The first of `names` present.
    pub fn first(&self, names: &[&str]) -> Option<String> {
        names.iter().find(|n| self.has(n)).map(|n| (*n).to_owned())
    }

    /// Relative path of a file in this directory.
    pub fn rel_file(&self, name: &str) -> String {
        join_rel(&self.rel, name)
    }
}

pub(crate) fn join_rel(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

fn read_dir_info(root: &Path, rel: &str) -> Option<DirInfo> {
    let abs = if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    let rd = std::fs::read_dir(&abs).ok()?;
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        match e.file_type() {
            Ok(t) if t.is_dir() => dirs.push(name),
            Ok(t) if t.is_file() || t.is_symlink() => files.push(name),
            _ => {}
        }
    }
    files.sort();
    dirs.sort();
    Some(DirInfo {
        rel: rel.to_owned(),
        abs,
        files,
        dirs,
    })
}

fn skip_dir(name: &str) -> bool {
    name.starts_with('.') || SKIP_DIRS.contains(&name)
}

/// The root and its sub-directories down to `depth` levels.
pub(crate) fn walk(root: &Path, depth: usize) -> Vec<DirInfo> {
    let mut out = Vec::new();
    let Some(top) = read_dir_info(root, "") else {
        return out;
    };
    let mut frontier = vec![(top, 0usize)];
    while let Some((info, d)) = frontier.pop() {
        if d < depth {
            for name in info.dirs.iter().rev() {
                if skip_dir(name) {
                    continue;
                }
                let rel = join_rel(&info.rel, name);
                if let Some(child) = read_dir_info(root, &rel) {
                    frontier.push((child, d + 1));
                }
            }
        }
        out.push(info);
    }
    // The root first, then in tree order.
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

/// Expand simple workspace globs (`packages/*`, `crates/a`, `apps/**`)
/// into existing directories, relative to `base` (itself relative to the
/// root). `**` matches one or two levels.
pub(crate) fn expand_globs(root: &Path, base: &str, patterns: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut excluded: Vec<String> = Vec::new();
    for pat in patterns {
        let (negate, pat) = match pat.strip_prefix('!') {
            Some(p) => (true, p),
            None => (false, pat.as_str()),
        };
        let pat = pat.trim_start_matches("./").trim_end_matches('/');
        let mut found: Vec<String> = vec![base.to_owned()];
        for seg in pat.split('/') {
            let mut next = Vec::new();
            for dir in &found {
                let abs = if dir.is_empty() {
                    root.to_path_buf()
                } else {
                    root.join(dir)
                };
                if seg == "**" {
                    // One or two levels below.
                    for child in subdirs(&abs) {
                        let c = join_rel(dir, &child);
                        for grand in subdirs(&root.join(&c)) {
                            next.push(join_rel(&c, &grand));
                        }
                        next.push(c);
                    }
                } else if seg.contains('*') {
                    for child in subdirs(&abs) {
                        if glob_match(seg, &child) {
                            next.push(join_rel(dir, &child));
                        }
                    }
                } else if abs.join(seg).is_dir() {
                    next.push(join_rel(dir, seg));
                }
            }
            found = next;
        }
        if negate {
            excluded.extend(found);
        } else {
            out.extend(found);
        }
    }
    out.retain(|d| !excluded.contains(d));
    out.sort();
    out.dedup();
    out
}

fn subdirs(abs: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(abs) else {
        return Vec::new();
    };
    let mut v: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !skip_dir(n))
        .collect();
    v.sort();
    v
}

/// `*` and `?` matching for one path segment.
pub(crate) fn glob_match(pat: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some('*') => (0..=n.len()).any(|i| go(&p[1..], &n[i..])),
            Some('?') => !n.is_empty() && go(&p[1..], &n[1..]),
            Some(c) => n.first() == Some(c) && go(&p[1..], &n[1..]),
        }
    }
    let p: Vec<char> = pat.chars().collect();
    let n: Vec<char> = name.chars().collect();
    go(&p, &n)
}

/// 1-based line of byte offset `pos`.
pub(crate) fn line_of(text: &str, pos: usize) -> u32 {
    text.as_bytes()[..pos.min(text.len())]
        .iter()
        .filter(|b| **b == b'\n')
        .count() as u32
        + 1
}

/// Collects tasks, projects and problems while walking.
pub(crate) struct Found {
    pub tasks: Vec<Task>,
    pub projects: BTreeMap<String, Project>,
    pub problems: Vec<Problem>,
    pub files_read: usize,
    /// `source:dir` pairs already scanned (workspace members found by
    /// their root are not scanned again).
    pub done: HashSet<String>,
}

impl Found {
    pub fn read(&mut self, root: &Path, rel_file: &str) -> Option<String> {
        self.files_read += 1;
        match std::fs::read(root.join(rel_file)) {
            Ok(bytes) => Some(
                String::from_utf8_lossy(&bytes)
                    .trim_start_matches('\u{feff}')
                    .to_owned(),
            ),
            Err(e) => {
                self.problem(rel_file, format!("Could not read it: {e}"));
                None
            }
        }
    }

    pub fn problem(&mut self, file: &str, message: impl Into<String>) {
        self.problems.push(Problem {
            file: file.to_owned(),
            message: message.into(),
        });
    }

    pub fn project(&mut self, dir: &str) -> &mut Project {
        self.projects
            .entry(dir.to_owned())
            .or_insert_with(|| Project {
                dir: dir.to_owned(),
                name: dir.rsplit('/').next().unwrap_or(dir).to_owned(),
                sources: Vec::new(),
                package_manager: None,
                member_of: None,
                workspace: false,
            })
    }

    pub fn add(&mut self, tasks: Vec<Task>) {
        for t in tasks {
            let p = self.project(&t.dir);
            if !p.sources.contains(&t.source) {
                p.sources.push(t.source);
            }
            if !self.tasks.iter().any(|x| x.id == t.id) {
                self.tasks.push(t);
            }
        }
    }
}

/// Find every task under `root` (the root and two levels of
/// sub-directories; Cargo workspace members wherever they are).
pub fn discover(root: &Path) -> Discovery {
    let started = Instant::now();
    let dirs = walk(root, 2);
    let mut found = Found {
        tasks: Vec::new(),
        projects: BTreeMap::new(),
        problems: Vec::new(),
        files_read: 0,
        done: HashSet::new(),
    };
    let mut cargo_done: HashSet<String> = HashSet::new();
    for info in &dirs {
        node::scan(root, info, &mut found);
        make::scan(root, info, &mut found);
        if !cargo_done.contains(&info.rel) {
            for d in cargo::scan(root, info, &mut found) {
                cargo_done.insert(d);
            }
        }
        other::scan(root, info, &mut found);
    }
    // Root project first, then by path.
    let projects: Vec<Project> = found
        .projects
        .into_values()
        .filter(|p| !p.sources.is_empty())
        .collect();
    Discovery {
        root: root.to_path_buf(),
        tasks: found.tasks,
        projects,
        problems: found.problems,
        dirs_scanned: dirs.len(),
        files_read: found.files_read,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};

    /// A throwaway directory tree.
    pub struct Tree {
        pub root: PathBuf,
    }

    impl Tree {
        pub fn new(name: &str) -> Tree {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "hadion-tasks-{name}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&root).unwrap();
            Tree { root }
        }

        pub fn file(&self, rel: &str, text: &str) -> &Self {
            let p = self.root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
            self
        }

        pub fn path(&self) -> &Path {
            &self.root
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::Tree;
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("*", "web"));
        assert!(glob_match("app-*", "app-web"));
        assert!(!glob_match("app-*", "lib-web"));
        assert!(glob_match("v?", "v1"));
        let t = Tree::new("globs");
        t.file("packages/a/package.json", "{}")
            .file("packages/b/package.json", "{}")
            .file("apps/web/x/y.txt", "")
            .file("node_modules/z/package.json", "{}");
        assert_eq!(
            expand_globs(t.path(), "", &["packages/*".into(), "!packages/b".into()]),
            vec!["packages/a"]
        );
        assert_eq!(
            expand_globs(t.path(), "", &["apps/**".into()]),
            vec!["apps/web", "apps/web/x"]
        );
    }

    #[test]
    fn walks_two_levels_and_skips_heavy_dirs() {
        let t = Tree::new("walk");
        t.file("a/b/c/deep.txt", "")
            .file("node_modules/x/package.json", "{}")
            .file(".git/HEAD", "")
            .file("top.txt", "");
        let rels: Vec<String> = walk(t.path(), 2).into_iter().map(|d| d.rel).collect();
        assert_eq!(rels, vec!["", "a", "a/b"]);
    }

    #[test]
    fn command_lines_and_specs() {
        let mut t = Task::new(
            Source::Npm,
            "web",
            "dev",
            "pnpm",
            &["run", "dev"],
            "web/package.json",
            Some(4),
            "",
        );
        t.args_separator = true;
        let extra = vec!["--port".to_owned(), "3000 x".to_owned()];
        assert_eq!(t.command_line(&extra), "pnpm run dev -- --port \"3000 x\"");
        assert_eq!(t.command_line(&[]), "pnpm run dev");
        let spec = t.spec(Path::new("/r"), &extra);
        assert_eq!(spec.program, "pnpm");
        assert_eq!(spec.args, vec!["run", "dev", "--", "--port", "3000 x"]);
        assert_eq!(spec.cwd, Path::new("/r").join("web"));
        assert!(spec.env.iter().any(|(k, _)| k == "FORCE_COLOR"));
        assert_eq!(t.id, "npm:web:dev");
        let w = Task::new(
            Source::Gradle,
            "",
            "build",
            "gradlew.bat",
            &["build"],
            "gradlew.bat",
            None,
            "",
        )
        .local();
        assert!(w.command_line(&[]).ends_with("gradlew.bat build"));
        assert!(
            w.spec(Path::new("/r"), &[])
                .program
                .replace('\\', "/")
                .ends_with("/r/gradlew.bat")
        );
    }

    #[test]
    fn discovers_a_mixed_tree() {
        let t = Tree::new("mixed");
        t.file(
            "package.json",
            r#"{ "name": "root", "workspaces": ["packages/*"], "scripts": { "build": "tsc -b", "test": "vitest" } }"#,
        )
        .file("pnpm-lock.yaml", "")
        .file(
            "packages/web/package.json",
            r#"{ "name": "@x/web", "scripts": { "dev": "vite" } }"#,
        )
        .file("Makefile", "## Build everything\nall: build\n\t@echo hi\n")
        .file("go.mod", "module example.com/x\n")
        .file("docker-compose.yml", "services:\n  web:\n    image: nginx\n");
        let d = discover(t.path());
        let ids: Vec<&str> = d.tasks.iter().map(|t| t.id.as_str()).collect();
        for want in [
            "npm::build",
            "npm::test",
            "npm:packages/web:dev",
            "make::all",
            "go::test",
            "compose::up",
        ] {
            assert!(ids.contains(&want), "{want} missing from {ids:?}");
        }
        let web = d.task("npm:packages/web:dev").unwrap();
        assert_eq!(web.program, "pnpm", "the workspace root lockfile decides");
        assert_eq!(
            d.project("packages/web").unwrap().member_of.as_deref(),
            Some("")
        );
        assert!(d.project("").unwrap().workspace);
        assert!(d.problems.is_empty(), "{:?}", d.problems);
        assert!(d.dirs_scanned >= 3);
    }
}

/// `HADION_DISCOVER=<dir> cargo test -p hadion-core --lib print_discovery
/// -- --ignored --nocapture` prints what the Tasks page would list.
#[cfg(test)]
#[test]
#[ignore]
fn print_discovery() {
    let Some(dir) = std::env::var_os("HADION_DISCOVER") else {
        return;
    };
    let d = discover(Path::new(&dir));
    println!(
        "{} tasks, {} projects, {} dirs, {} files, {} ms",
        d.tasks.len(),
        d.projects.len(),
        d.dirs_scanned,
        d.files_read,
        d.elapsed_ms
    );
    for p in &d.problems {
        println!("problem {}: {}", p.file, p.message);
    }
    for t in &d.tasks {
        println!(
            "{:<40} {:<45} {}:{} {}",
            t.id,
            t.command_line(&[]),
            t.file,
            t.line.map(|l| l.to_string()).unwrap_or_default(),
            t.description
        );
    }
}
