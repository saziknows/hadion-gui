//! Go, .NET, Gradle, Maven, Python and docker compose.

use std::path::Path;

use super::{DirInfo, Found, Source, Task, join_rel};
use crate::devops::tools::Tool;
use crate::devops::workbench::toml;

/// `go build / test / vet ./...` for a module.
pub fn go(dir: &str, file: &str, text: &str) -> Vec<Task> {
    let module = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("module "))
        .map(|m| m.trim().trim_matches('"').to_owned())
        .unwrap_or_default();
    let line = text
        .lines()
        .position(|l| l.trim().starts_with("module "))
        .map(|i| i as u32 + 1);
    let of = if module.is_empty() {
        String::new()
    } else {
        format!(" of {module}")
    };
    [
        ("build", "Compile every package"),
        ("test", "Run the tests of every package"),
        ("vet", "Report suspicious constructs in every package"),
    ]
    .into_iter()
    .map(|(cmd, what)| {
        Task::new(
            Source::Go,
            dir,
            cmd,
            "go",
            &[cmd, "./..."],
            file,
            line,
            format!("{what}{of}"),
        )
        .tool(Tool::Go)
    })
    .collect()
}

/// `dotnet build` / `dotnet test` per solution, else per project (`test`
/// only for test projects).
pub fn dotnet(dir: &str, files: &[(String, String)]) -> Vec<Task> {
    let solutions: Vec<&(String, String)> = files
        .iter()
        .filter(|(n, _)| n.ends_with(".sln") || n.ends_with(".slnx"))
        .collect();
    let mut out = Vec::new();
    let one = |name: &str, cmd: &str, what: &str| {
        Task::new(
            Source::Dotnet,
            dir,
            &format!("{cmd} {name}"),
            "dotnet",
            &[cmd, name],
            &join_rel(dir, name),
            None,
            what.to_owned(),
        )
        .tool(Tool::Dotnet)
    };
    if !solutions.is_empty() {
        for (name, _) in solutions {
            out.push(one(name, "build", "Build every project of the solution"));
            out.push(one(name, "test", "Run the test projects of the solution"));
        }
        return out;
    }
    for (name, text) in files {
        if !(name.ends_with(".csproj") || name.ends_with(".fsproj") || name.ends_with(".vbproj")) {
            continue;
        }
        out.push(one(name, "build", "Build the project"));
        let is_test = text.contains("Microsoft.NET.Test.Sdk")
            || text.contains("<IsTestProject>true")
            || name.to_ascii_lowercase().contains("test");
        if is_test {
            out.push(one(name, "test", "Run the tests of the project"));
        }
        if text.contains("<OutputType>Exe") || text.contains("Sdk=\"Microsoft.NET.Sdk.Web\"") {
            out.push(
                Task::new(
                    Source::Dotnet,
                    dir,
                    &format!("run {name}"),
                    "dotnet",
                    &["run", "--project", name],
                    &join_rel(dir, name),
                    None,
                    "Build and run the project",
                )
                .tool(Tool::Dotnet),
            );
        }
    }
    out
}

/// Gradle: the wrapper when it exists (`gradlew.bat` on Windows), else
/// `gradle`.
pub fn gradle(dir: &str, info_files: &[String]) -> Vec<Task> {
    let wrapper = if cfg!(windows) {
        "gradlew.bat"
    } else {
        "gradlew"
    };
    let has_wrapper = info_files.iter().any(|f| f == wrapper);
    let build_file = [
        "build.gradle.kts",
        "build.gradle",
        "settings.gradle.kts",
        "settings.gradle",
    ]
    .into_iter()
    .find(|f| info_files.iter().any(|x| x == f));
    if !has_wrapper && build_file.is_none() {
        return Vec::new();
    }
    let file = if has_wrapper {
        join_rel(dir, wrapper)
    } else {
        join_rel(dir, build_file.unwrap_or("build.gradle"))
    };
    [
        ("build", "Assemble and test the project"),
        ("test", "Run the tests"),
        ("clean", "Delete the build directory"),
    ]
    .into_iter()
    .map(|(cmd, what)| {
        let t = Task::new(
            Source::Gradle,
            dir,
            cmd,
            if has_wrapper { wrapper } else { "gradle" },
            &[cmd],
            &file,
            None,
            if has_wrapper {
                format!("{what} (Gradle wrapper)")
            } else {
                what.to_owned()
            },
        )
        .tool(Tool::Gradle);
        if has_wrapper { t.local() } else { t }
    })
    .collect()
}

/// Maven: `mvnw` when present, else `mvn`; package / test / verify.
pub fn maven(dir: &str, info_files: &[String], pom: &str) -> Vec<Task> {
    let wrapper = if cfg!(windows) { "mvnw.cmd" } else { "mvnw" };
    let has_wrapper = info_files.iter().any(|f| f == wrapper);
    let artifact = pom
        .split("<artifactId>")
        .nth(1)
        .and_then(|s| s.split("</artifactId>").next())
        .map(str::trim)
        .unwrap_or_default()
        .to_owned();
    let of = if artifact.is_empty() {
        String::new()
    } else {
        format!(" ({artifact})")
    };
    [
        ("package", "Compile, test and package"),
        ("test", "Run the tests"),
        ("verify", "Run the integration tests and checks"),
    ]
    .into_iter()
    .map(|(goal, what)| {
        let t = Task::new(
            Source::Maven,
            dir,
            goal,
            if has_wrapper { wrapper } else { "mvn" },
            &[goal],
            &join_rel(dir, "pom.xml"),
            None,
            format!("{what}{of}"),
        )
        .tool(Tool::Maven);
        if has_wrapper { t.local() } else { t }
    })
    .collect()
}

/// Python: `[project.scripts]` / `[tool.poetry.scripts]` entry points,
/// poe tasks, pytest (when `tests/` exists or pytest is configured) and
/// tox.
pub fn python(
    dir: &str,
    pyproject: Option<&str>,
    has_tests_dir: bool,
    has_tox_ini: bool,
) -> Result<Vec<Task>, String> {
    let file = join_rel(dir, "pyproject.toml");
    let mut out = Vec::new();
    let mut pytest_configured = false;
    let mut tox_configured = has_tox_ini;
    if let Some(text) = pyproject {
        let t = toml::parse(text).map_err(|e| format!("Not valid TOML: {e}"))?;
        for path in [
            &["project", "scripts"][..],
            &["tool", "poetry", "scripts"][..],
        ] {
            if let Some(scripts) = t.table(path) {
                for e in &scripts.entries {
                    let Some(target) = e.value.as_str() else {
                        continue;
                    };
                    out.push(
                        Task::new(
                            Source::Python,
                            dir,
                            &e.key,
                            &e.key,
                            &[],
                            &file,
                            Some(e.line),
                            format!(
                                "Entry point {target}; on PATH once the package is installed (pip install -e .)"
                            ),
                        )
                        .tool(Tool::Python),
                    );
                }
            }
        }
        if let Some(tasks) = t.table(&["tool", "poe", "tasks"]) {
            for e in &tasks.entries {
                let (body, help) = match &e.value {
                    toml::Value::Str(s) => (s.clone(), String::new()),
                    toml::Value::Array(_) => (
                        format!("sequence: {}", e.value.strings().join(", ")),
                        String::new(),
                    ),
                    toml::Value::Table(tab) => (
                        ["cmd", "script", "shell", "ref"]
                            .iter()
                            .find_map(|k| tab.str_at(&[k]).map(|s| format!("{k}: {s}")))
                            .or_else(|| {
                                tab.get("sequence")
                                    .map(|s| format!("sequence: {}", s.strings().join(", ")))
                            })
                            .unwrap_or_default(),
                        tab.str_at(&["help"]).unwrap_or_default().to_owned(),
                    ),
                    _ => continue,
                };
                let desc = if help.is_empty() {
                    "Poe the Poet task".to_owned()
                } else {
                    help
                };
                let mut task = Task::new(
                    Source::Python,
                    dir,
                    &format!("poe {}", e.key),
                    "poe",
                    &[e.key.as_str()],
                    &file,
                    Some(e.line),
                    desc,
                )
                .body(body);
                task.id = format!("python:{dir}:poe {}", e.key);
                out.push(task);
            }
        }
        pytest_configured = t.get_path(&["tool", "pytest"]).is_some();
        tox_configured |= t.get_path(&["tool", "tox"]).is_some();
    }
    if has_tests_dir || pytest_configured {
        out.push(
            Task::new(
                Source::Python,
                dir,
                "pytest",
                "python",
                &["-m", "pytest"],
                &if pytest_configured {
                    file.clone()
                } else {
                    join_rel(dir, "tests")
                },
                None,
                "Run the test suite with pytest",
            )
            .tool(Tool::Python),
        );
    }
    if tox_configured {
        out.push(
            Task::new(
                Source::Python,
                dir,
                "tox",
                "tox",
                &[],
                &if has_tox_ini {
                    join_rel(dir, "tox.ini")
                } else {
                    file.clone()
                },
                None,
                "Run every tox environment",
            )
            .tool(Tool::Python),
        );
    }
    Ok(out)
}

/// docker compose: up (detached), down, build, ps and the recent logs.
pub fn compose(dir: &str, file_name: &str, text: &str) -> Result<Vec<Task>, String> {
    let v: serde_yaml::Value =
        serde_yaml::from_str(text).map_err(|e| format!("Not valid YAML: {e}"))?;
    let services: Vec<String> = v
        .get("services")
        .and_then(|s| s.as_mapping())
        .map(|m| {
            m.keys()
                .filter_map(|k| k.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let list = if services.is_empty() {
        "no services".to_owned()
    } else {
        format!("services: {}", services.join(", "))
    };
    let file = join_rel(dir, file_name);
    let mut out = Vec::new();
    for (name, args, what) in [
        (
            "up",
            &["up", "-d"][..],
            "Start the containers in the background",
        ),
        ("down", &["down"][..], "Stop and remove the containers"),
        ("build", &["build"][..], "Build the images"),
        ("ps", &["ps"][..], "List the containers"),
        (
            "logs",
            &["logs", "--tail", "200"][..],
            "The last 200 log lines of every service",
        ),
    ] {
        let mut a: Vec<&str> = vec!["compose", "-f", file_name];
        a.extend_from_slice(args);
        let mut t = Task::new(
            Source::Compose,
            dir,
            name,
            "docker",
            &a,
            &file,
            None,
            format!("{what} ({list})"),
        )
        .tool(Tool::Docker);
        t.destructive = name == "down";
        out.push(t);
    }
    Ok(out)
}

pub(crate) fn scan(root: &Path, info: &DirInfo, found: &mut Found) {
    if info.has("go.mod")
        && let Some(text) = found.read(root, &info.rel_file("go.mod"))
    {
        found.add(go(&info.rel, &info.rel_file("go.mod"), &text));
    }
    let dotnet_files: Vec<String> = info
        .files
        .iter()
        .filter(|f| {
            [".sln", ".slnx", ".csproj", ".fsproj", ".vbproj"]
                .iter()
                .any(|e| f.ends_with(e))
        })
        .cloned()
        .collect();
    if !dotnet_files.is_empty() {
        let mut with_text = Vec::new();
        for f in dotnet_files {
            let text = if f.ends_with("proj") {
                found.read(root, &info.rel_file(&f)).unwrap_or_default()
            } else {
                String::new()
            };
            with_text.push((f, text));
        }
        found.add(dotnet(&info.rel, &with_text));
    }
    found.add(gradle(&info.rel, &info.files));
    if info.has("pom.xml")
        && let Some(text) = found.read(root, &info.rel_file("pom.xml"))
    {
        found.add(maven(&info.rel, &info.files, &text));
    }
    let pyproject = if info.has("pyproject.toml") {
        found.read(root, &info.rel_file("pyproject.toml"))
    } else {
        None
    };
    let pythonish =
        pyproject.is_some() || info.has("setup.py") || info.has("tox.ini") || info.has("setup.cfg");
    if pythonish {
        let tests = info.has_dir("tests") || info.has_dir("test");
        match python(&info.rel, pyproject.as_deref(), tests, info.has("tox.ini")) {
            Ok(t) => found.add(t),
            Err(e) => found.problem(&info.rel_file("pyproject.toml"), e),
        }
    }
    if let Some(name) = info.first(&[
        "compose.yaml",
        "compose.yml",
        "docker-compose.yml",
        "docker-compose.yaml",
    ]) && let Some(text) = found.read(root, &info.rel_file(&name))
    {
        match compose(&info.rel, &name, &text) {
            Ok(t) => found.add(t),
            Err(e) => found.problem(&info.rel_file(&name), e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_module() {
        let t = go(
            "svc",
            "svc/go.mod",
            "// c\nmodule example.com/svc\n\ngo 1.22\n",
        );
        assert_eq!(t.len(), 3);
        assert_eq!(t[1].command_line(&[]), "go test ./...");
        assert_eq!(t[1].dir, "svc");
        assert_eq!(t[1].line, Some(2));
        assert!(t[0].description.ends_with("of example.com/svc"));
    }

    #[test]
    fn dotnet_solutions_then_projects() {
        let sln = dotnet(
            "",
            &[
                ("App.sln".into(), String::new()),
                ("App.csproj".into(), String::new()),
            ],
        );
        let cmds: Vec<String> = sln.iter().map(|t| t.command_line(&[])).collect();
        assert_eq!(cmds, vec!["dotnet build App.sln", "dotnet test App.sln"]);
        let projects = dotnet(
            "src",
            &[
                (
                    "Api.csproj".into(),
                    "<Project Sdk=\"Microsoft.NET.Sdk.Web\"></Project>".into(),
                ),
                (
                    "Api.Tests.csproj".into(),
                    "<PackageReference Include=\"Microsoft.NET.Test.Sdk\" />".into(),
                ),
            ],
        );
        let cmds: Vec<String> = projects.iter().map(|t| t.command_line(&[])).collect();
        assert_eq!(
            cmds,
            vec![
                "dotnet build Api.csproj",
                "dotnet run --project Api.csproj",
                "dotnet build Api.Tests.csproj",
                "dotnet test Api.Tests.csproj"
            ]
        );
    }

    #[test]
    fn gradle_and_maven_wrappers() {
        let wrapper = if cfg!(windows) {
            "gradlew.bat"
        } else {
            "gradlew"
        };
        let g = gradle("", &[wrapper.to_owned(), "build.gradle.kts".to_owned()]);
        assert_eq!(g.len(), 3);
        assert!(g[0].local_program);
        assert!(
            g[0].command_line(&[])
                .ends_with(&format!("{wrapper} build"))
        );
        let plain = gradle("", &["build.gradle".to_owned()]);
        assert_eq!(plain[1].command_line(&[]), "gradle test");
        assert!(gradle("", &["README.md".to_owned()]).is_empty());
        let m = maven(
            "",
            &["pom.xml".to_owned()],
            "<project><artifactId> demo </artifactId></project>",
        );
        assert_eq!(m[0].command_line(&[]), "mvn package");
        assert!(m[0].description.ends_with("(demo)"));
    }

    #[test]
    fn python_project() {
        let text = r#"
[project]
name = "demo"

[project.scripts]
demo = "demo.cli:main"

[tool.poe.tasks]
lint = "ruff check ."
serve = { cmd = "uvicorn app:app", help = "Development server" }
ci = ["lint", "test"]

[tool.pytest.ini_options]
addopts = "-q"
"#;
        let t = python("", Some(text), false, true).unwrap();
        let names: Vec<&str> = t.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["demo", "poe lint", "poe serve", "poe ci", "pytest", "tox"]
        );
        assert_eq!(t[0].line, Some(6));
        assert_eq!(t[0].command_line(&[]), "demo");
        assert_eq!(t[2].description, "Development server");
        assert_eq!(t[2].body.as_deref(), Some("cmd: uvicorn app:app"));
        assert_eq!(t[3].body.as_deref(), Some("sequence: lint, test"));
        assert_eq!(t[4].command_line(&[]), "python -m pytest");
        assert_eq!(t[5].file, "tox.ini");
        let bare = python("lib", None, true, false).unwrap();
        assert_eq!(bare.len(), 1);
        assert_eq!(bare[0].file, "lib/tests");
    }

    #[test]
    fn compose_commands() {
        let t = compose(
            "deploy",
            "compose.yaml",
            "services:\n  web:\n    image: nginx\n  db:\n    image: postgres\n",
        )
        .unwrap();
        let cmds: Vec<String> = t.iter().map(|t| t.command_line(&[])).collect();
        assert_eq!(
            cmds,
            vec![
                "docker compose -f compose.yaml up -d",
                "docker compose -f compose.yaml down",
                "docker compose -f compose.yaml build",
                "docker compose -f compose.yaml ps",
                "docker compose -f compose.yaml logs --tail 200"
            ]
        );
        assert!(t[1].destructive);
        assert!(t[0].description.contains("services: web, db"));
        assert_eq!(t[0].file, "deploy/compose.yaml");
    }
}
