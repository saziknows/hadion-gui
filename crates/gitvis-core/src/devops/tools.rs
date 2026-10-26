//! Which external tools are installed, where, and their versions.
//!
//! Features that need a tool show what it is, why it is needed and how to
//! install it when it is missing, instead of failing on first use.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use super::proc::{ProcSpec, resolve, run_capture};

/// A tool gitvis can drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tool {
    Git,
    Gh,
    Glab,
    Az,
    Kubectl,
    Helm,
    Docker,
    Terraform,
    Tofu,
    Act,
    Actionlint,
    Trivy,
    Grype,
    Syft,
    Npm,
    Pnpm,
    Yarn,
    Bun,
    Deno,
    Cargo,
    Make,
    Just,
    Task,
    Go,
    Dotnet,
    Python,
    Composer,
    Gradle,
    Maven,
}

impl Tool {
    pub const ALL: [Tool; 29] = [
        Tool::Git,
        Tool::Gh,
        Tool::Glab,
        Tool::Az,
        Tool::Kubectl,
        Tool::Helm,
        Tool::Docker,
        Tool::Terraform,
        Tool::Tofu,
        Tool::Act,
        Tool::Actionlint,
        Tool::Trivy,
        Tool::Grype,
        Tool::Syft,
        Tool::Npm,
        Tool::Pnpm,
        Tool::Yarn,
        Tool::Bun,
        Tool::Deno,
        Tool::Cargo,
        Tool::Make,
        Tool::Just,
        Tool::Task,
        Tool::Go,
        Tool::Dotnet,
        Tool::Python,
        Tool::Composer,
        Tool::Gradle,
        Tool::Maven,
    ];

    /// The executable name looked up on PATH.
    pub fn program(self) -> &'static str {
        match self {
            Tool::Git => "git",
            Tool::Gh => "gh",
            Tool::Glab => "glab",
            Tool::Az => "az",
            Tool::Kubectl => "kubectl",
            Tool::Helm => "helm",
            Tool::Docker => "docker",
            Tool::Terraform => "terraform",
            Tool::Tofu => "tofu",
            Tool::Act => "act",
            Tool::Actionlint => "actionlint",
            Tool::Trivy => "trivy",
            Tool::Grype => "grype",
            Tool::Syft => "syft",
            Tool::Npm => "npm",
            Tool::Pnpm => "pnpm",
            Tool::Yarn => "yarn",
            Tool::Bun => "bun",
            Tool::Deno => "deno",
            Tool::Cargo => "cargo",
            Tool::Make => "make",
            Tool::Just => "just",
            Tool::Task => "task",
            Tool::Go => "go",
            Tool::Dotnet => "dotnet",
            Tool::Python => "python",
            Tool::Composer => "composer",
            Tool::Gradle => "gradle",
            Tool::Maven => "mvn",
        }
    }

    /// Display name.
    pub fn name(self) -> &'static str {
        match self {
            Tool::Git => "Git",
            Tool::Gh => "GitHub CLI",
            Tool::Glab => "GitLab CLI",
            Tool::Az => "Azure CLI",
            Tool::Kubectl => "kubectl",
            Tool::Helm => "Helm",
            Tool::Docker => "Docker",
            Tool::Terraform => "Terraform",
            Tool::Tofu => "OpenTofu",
            Tool::Act => "act",
            Tool::Actionlint => "actionlint",
            Tool::Trivy => "Trivy",
            Tool::Grype => "Grype",
            Tool::Syft => "Syft",
            Tool::Npm => "npm",
            Tool::Pnpm => "pnpm",
            Tool::Yarn => "Yarn",
            Tool::Bun => "Bun",
            Tool::Deno => "Deno",
            Tool::Cargo => "Cargo",
            Tool::Make => "Make",
            Tool::Just => "just",
            Tool::Task => "Task",
            Tool::Go => "Go",
            Tool::Dotnet => ".NET SDK",
            Tool::Python => "Python",
            Tool::Composer => "Composer",
            Tool::Gradle => "Gradle",
            Tool::Maven => "Maven",
        }
    }

    /// Arguments that print the version.
    fn version_args(self) -> &'static [&'static str] {
        match self {
            Tool::Kubectl => &["version", "--client", "-o", "yaml"],
            Tool::Helm => &["version", "--short"],
            Tool::Act | Tool::Trivy | Tool::Grype | Tool::Syft => &["--version"],
            Tool::Go => &["version"],
            Tool::Terraform | Tool::Tofu => &["version"],
            Tool::Task => &["--version"],
            Tool::Az => &["version", "-o", "tsv"],
            _ => &["--version"],
        }
    }

    /// One line on how to get it, shown when it is missing.
    pub fn install_hint(self) -> &'static str {
        match self {
            Tool::Git => "Install Git from https://git-scm.com/downloads",
            Tool::Gh => "winget install GitHub.cli, then run `gh auth login`",
            Tool::Glab => "winget install GLab.GLab, then run `glab auth login`",
            Tool::Az => {
                "winget install Microsoft.AzureCLI, then `az extension add --name azure-devops` and `az login`"
            }
            Tool::Kubectl => "winget install Kubernetes.kubectl (Docker Desktop also ships it)",
            Tool::Helm => "winget install Helm.Helm",
            Tool::Docker => {
                "Install Docker Desktop from https://www.docker.com/products/docker-desktop"
            }
            Tool::Terraform => "winget install Hashicorp.Terraform",
            Tool::Tofu => "winget install OpenTofu.Tofu",
            Tool::Act => "winget install nektos.act (needs Docker running)",
            Tool::Actionlint => "go install github.com/rhysd/actionlint/cmd/actionlint@latest",
            Tool::Trivy => "winget install AquaSecurity.Trivy",
            Tool::Grype => "winget install Anchore.Grype",
            Tool::Syft => "winget install Anchore.Syft",
            Tool::Npm => "Install Node.js from https://nodejs.org",
            Tool::Pnpm => "npm install -g pnpm",
            Tool::Yarn => "npm install -g yarn",
            Tool::Bun => "npm install -g bun",
            Tool::Deno => "winget install DenoLand.Deno",
            Tool::Cargo => "Install Rust from https://rustup.rs",
            Tool::Make => "winget install ezwinports.make (or use Git Bash's make)",
            Tool::Just => "winget install Casey.Just",
            Tool::Task => "winget install Task.Task",
            Tool::Go => "winget install GoLang.Go",
            Tool::Dotnet => "winget install Microsoft.DotNet.SDK.8",
            Tool::Python => "winget install Python.Python.3.12",
            Tool::Composer => "Install Composer from https://getcomposer.org",
            Tool::Gradle => "Use the project's ./gradlew, or winget install Gradle.Gradle",
            Tool::Maven => "winget install Apache.Maven",
        }
    }
}

/// An installed tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub path: PathBuf,
    /// First meaningful line of the version output.
    pub version: String,
}

fn cache() -> &'static Mutex<HashMap<Tool, Option<Installed>>> {
    static CACHE: OnceLock<Mutex<HashMap<Tool, Option<Installed>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Where the tool is, without running it (cheap: a PATH lookup).
pub fn path(tool: Tool) -> Option<PathBuf> {
    resolve(tool.program())
}

/// Whether the tool is on PATH (cheap).
pub fn available(tool: Tool) -> bool {
    path(tool).is_some()
}

/// Path and version, cached for the process (runs `tool --version` once,
/// with a 10 s timeout). Call from a background thread.
pub fn detect(tool: Tool) -> Option<Installed> {
    if let Some(hit) = cache().lock().ok().and_then(|c| c.get(&tool).cloned()) {
        return hit;
    }
    let found = path(tool).map(|path| {
        let spec = ProcSpec::new(path.to_string_lossy().into_owned(), ".")
            .args(tool.version_args().iter().copied());
        let version = run_capture(&spec, Some(Duration::from_secs(10)))
            .ok()
            .map(|c| {
                let text = if c.stdout.is_empty() {
                    c.stderr.clone()
                } else {
                    c.stdout_text()
                };
                version_line(tool, &text)
            })
            .unwrap_or_default();
        Installed { path, version }
    });
    if let Ok(mut c) = cache().lock() {
        c.insert(tool, found.clone());
    }
    found
}

/// Forget cached results (after the user installed something).
pub fn refresh() {
    if let Ok(mut c) = cache().lock() {
        c.clear();
    }
}

fn version_line(tool: Tool, text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if tool == Tool::Kubectl {
        // `clientVersion:\n  gitVersion: v1.30.2`
        if let Some(l) = lines.iter().find(|l| l.starts_with("gitVersion:")) {
            return l.trim_start_matches("gitVersion:").trim().to_owned();
        }
    }
    lines.first().map(|l| (*l).to_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_is_detected_with_a_version() {
        let git = detect(Tool::Git).expect("git on PATH");
        assert!(git.version.starts_with("git version"), "{}", git.version);
        assert!(available(Tool::Git));
    }

    #[test]
    fn kubectl_version_line() {
        let text = "clientVersion:\n  buildDate: x\n  gitVersion: v1.30.2\n";
        assert_eq!(version_line(Tool::Kubectl, text), "v1.30.2");
        assert_eq!(version_line(Tool::Npm, "\n10.8.1\n"), "10.8.1");
    }
}
