//! Shells installed on this machine, and the command that changes a
//! shell's directory.
//!
//! Windows: PowerShell 7 (`pwsh` on `PATH` or under Program Files),
//! Windows PowerShell, cmd (`%ComSpec%`), Git Bash (`bin\bash.exe` of the
//! git installation that `git` on `PATH` belongs to, else the usual install
//! folders; started with `--login -i`) and WSL (`wsl.exe`, one entry per
//! distribution `wsl -l -q` lists). Elsewhere: `$SHELL`, then bash, zsh,
//! fish and sh from `PATH`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::devops::proc::{self, ProcSpec};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShellKind {
    Pwsh,
    WindowsPowerShell,
    Cmd,
    GitBash,
    Wsl,
    /// `$SHELL`, bash, zsh, fish, sh (not on Windows).
    Posix,
}

impl ShellKind {
    /// Short neutral mark for tabs (`PS`, `C:`, `$`).
    pub fn mark(self) -> &'static str {
        match self {
            ShellKind::Pwsh | ShellKind::WindowsPowerShell => "PS",
            ShellKind::Cmd => "C:",
            ShellKind::GitBash | ShellKind::Posix => "$",
            ShellKind::Wsl => "λ",
        }
    }

    /// Whether Ctrl+L redraws the prompt on a clean screen.
    pub fn clears_with_ctrl_l(self) -> bool {
        !matches!(self, ShellKind::Cmd)
    }
}

/// A shell that can be started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shell {
    /// Stable id for preferences and scripts: `pwsh`, `powershell`, `cmd`,
    /// `bash`, `wsl`, `wsl:Ubuntu`, `zsh`.
    pub id: String,
    /// "PowerShell 7", "Git Bash", "WSL · Ubuntu".
    pub name: String,
    pub kind: ShellKind,
    pub program: PathBuf,
    pub args: Vec<String>,
    /// How it was found ("PATH", "Program Files", "next to git", …).
    pub found_via: String,
}

impl Shell {
    /// The command line, for the Settings page and tooltips.
    pub fn command_line(&self) -> String {
        let mut s = quote(&self.program.display().to_string());
        for a in &self.args {
            s.push(' ');
            s.push_str(&quote(a));
        }
        s
    }

    /// Whether `name` (from a script or preference) means this shell.
    pub fn matches(&self, name: &str) -> bool {
        let n = name.trim().to_ascii_lowercase();
        if n.is_empty() {
            return false;
        }
        if self.id.eq_ignore_ascii_case(&n) {
            return true;
        }
        match n.as_str() {
            "powershell7" | "ps7" => self.kind == ShellKind::Pwsh,
            "windows-powershell" | "ps" => self.kind == ShellKind::WindowsPowerShell,
            "git-bash" | "gitbash" => self.kind == ShellKind::GitBash,
            "wsl" => self.kind == ShellKind::Wsl,
            "bash" => matches!(self.kind, ShellKind::GitBash) || self.id == "bash",
            _ => false,
        }
    }
}

fn quote(s: &str) -> String {
    if s.contains(' ') {
        format!("\"{s}\"")
    } else {
        s.to_owned()
    }
}

/// What discovery found and what it looked for without success.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Discovery {
    pub shells: Vec<Shell>,
    /// Shells looked for and not found, with where gitvis looked.
    pub missing: Vec<(String, String)>,
    /// Commands discovery ran (`wsl.exe -l -q`).
    pub commands: Vec<String>,
}

impl Discovery {
    /// The shell for a preference or script name; the first one for an
    /// empty or unknown name.
    pub fn pick(&self, name: &str) -> Option<&Shell> {
        self.find(name).or_else(|| self.shells.first())
    }

    /// The shell for a name, if there is one.
    pub fn find(&self, name: &str) -> Option<&Shell> {
        self.shells
            .iter()
            .find(|s| s.id.eq_ignore_ascii_case(name.trim()))
            .or_else(|| self.shells.iter().find(|s| s.matches(name)))
    }
}

/// Find the shells on this machine. Runs `wsl.exe -l -q` (up to 5 s), so
/// call it off the UI thread.
pub fn discover() -> Discovery {
    if cfg!(windows) {
        discover_windows()
    } else {
        discover_unix()
    }
}

fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn discover_windows() -> Discovery {
    let mut d = Discovery::default();
    let system_root = env_path("SystemRoot").unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let program_files =
        env_path("ProgramFiles").unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));

    // PowerShell 7.
    let pwsh = proc::resolve("pwsh.exe").map(|p| (p, "PATH")).or_else(|| {
        ["7", "7-preview"]
            .iter()
            .map(|v| program_files.join("PowerShell").join(v).join("pwsh.exe"))
            .find(|p| p.is_file())
            .map(|p| (p, "Program Files"))
    });
    match pwsh {
        Some((program, via)) => d.shells.push(Shell {
            id: "pwsh".into(),
            name: "PowerShell 7".into(),
            kind: ShellKind::Pwsh,
            program,
            args: vec!["-NoLogo".into()],
            found_via: via.into(),
        }),
        None => d.missing.push((
            "PowerShell 7".into(),
            format!(
                "pwsh.exe on PATH and under {}",
                program_files.join("PowerShell").display()
            ),
        )),
    }

    // Windows PowerShell.
    let ps = system_root
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    if ps.is_file() {
        d.shells.push(Shell {
            id: "powershell".into(),
            name: "Windows PowerShell".into(),
            kind: ShellKind::WindowsPowerShell,
            program: ps,
            args: vec!["-NoLogo".into()],
            found_via: "Windows".into(),
        });
    } else {
        d.missing
            .push(("Windows PowerShell".into(), ps.display().to_string()));
    }

    // cmd.
    let cmd = env_path("ComSpec")
        .filter(|p| p.is_file())
        .map(|p| (p, "%ComSpec%"))
        .or_else(|| {
            let p = system_root.join("System32").join("cmd.exe");
            p.is_file().then_some((p, "Windows"))
        });
    match cmd {
        Some((program, via)) => d.shells.push(Shell {
            id: "cmd".into(),
            name: "Command Prompt".into(),
            kind: ShellKind::Cmd,
            program,
            args: Vec::new(),
            found_via: via.into(),
        }),
        None => d
            .missing
            .push(("Command Prompt".into(), "%ComSpec%".into())),
    }

    // Git Bash.
    let mut looked = Vec::new();
    let from_git = proc::resolve("git.exe").and_then(|git| {
        looked.push(format!("next to {}", git.display()));
        git_bash_near(&git)
    });
    let bash = from_git.map(|p| (p, "next to git on PATH")).or_else(|| {
        let mut roots = vec![program_files.join("Git")];
        if let Some(x86) = env_path("ProgramFiles(x86)") {
            roots.push(x86.join("Git"));
        }
        if let Some(local) = env_path("LOCALAPPDATA") {
            roots.push(local.join("Programs").join("Git"));
        }
        roots
            .into_iter()
            .map(|r| {
                let p = r.join("bin").join("bash.exe");
                looked.push(p.display().to_string());
                p
            })
            .find(|p| p.is_file())
            .map(|p| (p, "install folder"))
    });
    match bash {
        Some((program, via)) => d.shells.push(Shell {
            id: "bash".into(),
            name: "Git Bash".into(),
            kind: ShellKind::GitBash,
            program,
            args: vec!["--login".into(), "-i".into()],
            found_via: via.into(),
        }),
        None => d.missing.push(("Git Bash".into(), looked.join(", "))),
    }

    // WSL.
    let wsl = system_root.join("System32").join("wsl.exe");
    if wsl.is_file() {
        let spec = ProcSpec::new(wsl.display().to_string(), system_root.clone()).args(["-l", "-q"]);
        d.commands.push("wsl.exe -l -q".into());
        let distros = proc::run_capture(&spec, Some(Duration::from_secs(5)))
            .ok()
            .filter(|c| c.success())
            .map(|c| parse_wsl_list(&c.stdout))
            .unwrap_or_default();
        if distros.is_empty() {
            d.shells.push(Shell {
                id: "wsl".into(),
                name: "WSL".into(),
                kind: ShellKind::Wsl,
                program: wsl,
                args: Vec::new(),
                found_via: "Windows".into(),
            });
        } else {
            for (i, name) in distros.iter().enumerate() {
                d.shells.push(Shell {
                    // The first (default) distribution is also plain `wsl`.
                    id: if i == 0 {
                        "wsl".into()
                    } else {
                        format!("wsl:{name}")
                    },
                    name: format!("WSL · {name}"),
                    kind: ShellKind::Wsl,
                    program: wsl.clone(),
                    args: vec!["-d".into(), name.clone()],
                    found_via: "wsl -l".into(),
                });
            }
        }
    } else {
        d.missing.push(("WSL".into(), wsl.display().to_string()));
    }
    d
}

/// `bin\bash.exe` of the git installation `git` belongs to
/// (`<root>\cmd\git.exe`, `<root>\bin\git.exe` or
/// `<root>\mingw64\bin\git.exe`).
pub fn git_bash_near(git: &Path) -> Option<PathBuf> {
    let mut dir = git.parent()?;
    for _ in 0..3 {
        let candidate = dir.join("bin").join("bash.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    None
}

/// Distribution names from `wsl -l -q` (UTF-16LE on Windows; plain text
/// elsewhere). Docker Desktop's internal distributions are left out.
pub fn parse_wsl_list(raw: &[u8]) -> Vec<String> {
    let utf16 = raw.starts_with(&[0xff, 0xfe]) || (raw.len() >= 2 && raw[1] == 0);
    let text = if utf16 && raw.len().is_multiple_of(2) {
        let units: Vec<u16> = raw
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(raw).into_owned()
    };
    text.trim_start_matches('\u{feff}')
        .lines()
        .map(|l| l.trim().trim_matches('\0').trim().to_owned())
        .filter(|l| !l.is_empty())
        .filter(|l| {
            !l.eq_ignore_ascii_case("docker-desktop")
                && !l.eq_ignore_ascii_case("docker-desktop-data")
        })
        .collect()
}

fn discover_unix() -> Discovery {
    let mut d = Discovery::default();
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut add = |d: &mut Discovery, program: PathBuf, via: &str| {
        if seen.contains(&program) {
            return;
        }
        seen.push(program.clone());
        let name = program
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "sh".into());
        let args = if matches!(name.as_str(), "bash" | "zsh" | "fish" | "sh") {
            vec!["-l".into()]
        } else {
            Vec::new()
        };
        d.shells.push(Shell {
            id: name.clone(),
            name,
            kind: ShellKind::Posix,
            program,
            args,
            found_via: via.into(),
        });
    };
    if let Some(sh) = env_path("SHELL").filter(|p| p.is_file()) {
        add(&mut d, sh, "$SHELL");
    }
    for name in ["bash", "zsh", "fish", "sh"] {
        if let Some(p) = proc::resolve(name) {
            add(&mut d, p, "PATH");
        }
    }
    if d.shells.is_empty() {
        d.missing.push(("A shell".into(), "$SHELL and PATH".into()));
    }
    d
}

// ---------------------------------------------------------------------------
// cd
// ---------------------------------------------------------------------------

/// `C:\Users\me\repo` → `/c/Users/me/repo` (Git Bash, MSYS paths).
pub fn msys_path(path: &str) -> String {
    let p = path.replace('\\', "/");
    let b = p.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        format!("/{}{}", (b[0] as char).to_ascii_lowercase(), &p[2..])
    } else {
        p
    }
}

/// `C:\Users\me\repo` → `/mnt/c/Users/me/repo` (WSL's default mount).
pub fn wsl_path(path: &str) -> String {
    let m = msys_path(path);
    if path.as_bytes().get(1) == Some(&b':') {
        format!("/mnt{m}")
    } else {
        m
    }
}

fn single_quoted_posix(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The command (without Enter) that moves `kind` to `dir`.
pub fn cd_command(kind: ShellKind, dir: &Path) -> String {
    let d = dir.display().to_string();
    match kind {
        ShellKind::Pwsh | ShellKind::WindowsPowerShell => {
            format!("Set-Location -LiteralPath '{}'", d.replace('\'', "''"))
        }
        ShellKind::Cmd => format!("cd /d \"{d}\""),
        ShellKind::GitBash => format!("cd {}", single_quoted_posix(&msys_path(&d))),
        ShellKind::Wsl => format!("cd {}", single_quoted_posix(&wsl_path(&d))),
        ShellKind::Posix => format!("cd {}", single_quoted_posix(&d)),
    }
}

/// What clears a half-typed command line before gitvis types one: Escape
/// in PowerShell and cmd, Ctrl+E Ctrl+U in readline shells.
pub fn clear_line(kind: ShellKind) -> &'static [u8] {
    match kind {
        ShellKind::Pwsh | ShellKind::WindowsPowerShell | ShellKind::Cmd => b"\x1b",
        ShellKind::GitBash | ShellKind::Wsl | ShellKind::Posix => b"\x05\x15",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cd_per_shell() {
        let dir = Path::new(r"C:\Users\me\it's here");
        assert_eq!(
            cd_command(ShellKind::Pwsh, dir),
            r"Set-Location -LiteralPath 'C:\Users\me\it''s here'"
        );
        assert_eq!(
            cd_command(ShellKind::Cmd, dir),
            r#"cd /d "C:\Users\me\it's here""#
        );
        assert_eq!(
            cd_command(ShellKind::GitBash, dir),
            r"cd '/c/Users/me/it'\''s here'"
        );
        assert_eq!(
            cd_command(ShellKind::Wsl, dir),
            r"cd '/mnt/c/Users/me/it'\''s here'"
        );
        assert_eq!(
            cd_command(ShellKind::Posix, Path::new("/home/me/repo")),
            "cd '/home/me/repo'"
        );
    }

    #[test]
    fn paths() {
        assert_eq!(msys_path(r"D:\src\x"), "/d/src/x");
        assert_eq!(msys_path("/already/posix"), "/already/posix");
        assert_eq!(wsl_path(r"C:\a"), "/mnt/c/a");
        assert_eq!(wsl_path("/home/x"), "/home/x");
    }

    #[test]
    fn wsl_list_utf16() {
        let text = "\u{feff}Ubuntu\r\ndocker-desktop\r\nDebian\r\n";
        let raw: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(parse_wsl_list(&raw), vec!["Ubuntu", "Debian"]);
        assert_eq!(
            parse_wsl_list(b"Ubuntu\nAlpine\n"),
            vec!["Ubuntu", "Alpine"]
        );
        assert!(parse_wsl_list(b"").is_empty());
    }

    fn shell(id: &str, kind: ShellKind) -> Shell {
        Shell {
            id: id.into(),
            name: id.into(),
            kind,
            program: PathBuf::from(format!("{id}.exe")),
            args: Vec::new(),
            found_via: "test".into(),
        }
    }

    #[test]
    fn picking_shells() {
        let d = Discovery {
            shells: vec![
                shell("powershell", ShellKind::WindowsPowerShell),
                shell("cmd", ShellKind::Cmd),
                shell("bash", ShellKind::GitBash),
                shell("wsl", ShellKind::Wsl),
                shell("wsl:Debian", ShellKind::Wsl),
            ],
            ..Default::default()
        };
        assert_eq!(d.pick("cmd").unwrap().id, "cmd");
        assert_eq!(d.pick("CMD").unwrap().id, "cmd");
        assert_eq!(d.pick("git-bash").unwrap().id, "bash");
        assert_eq!(d.pick("wsl:debian").unwrap().id, "wsl:Debian");
        assert_eq!(d.pick("wsl").unwrap().id, "wsl");
        // Unknown or empty: the first shell.
        assert_eq!(d.pick("").unwrap().id, "powershell");
        assert_eq!(d.pick("pwsh").unwrap().id, "powershell");
        assert!(d.find("pwsh").is_none());
        assert_eq!(shell("bash", ShellKind::GitBash).command_line(), "bash.exe");
    }

    #[test]
    fn git_bash_next_to_git() {
        let root = std::env::temp_dir().join(format!("gitvis-term-shells-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("cmd")).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::create_dir_all(root.join("mingw64").join("bin")).unwrap();
        std::fs::write(root.join("bin").join("bash.exe"), b"").unwrap();
        let expected = Some(root.join("bin").join("bash.exe"));
        assert_eq!(git_bash_near(&root.join("cmd").join("git.exe")), expected);
        assert_eq!(
            git_bash_near(&root.join("mingw64").join("bin").join("git.exe")),
            expected
        );
        assert_eq!(git_bash_near(&root.join("bin").join("git.exe")), expected);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore = "prints the shells found on this machine"]
    fn print_discovery() {
        let d = discover();
        for s in &d.shells {
            println!(
                "{:<12} {:<22} {} ({})",
                s.id,
                s.name,
                s.command_line(),
                s.found_via
            );
        }
        for (name, looked) in &d.missing {
            println!("missing: {name} (looked at {looked})");
        }
    }
}
