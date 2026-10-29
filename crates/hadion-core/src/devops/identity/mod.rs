//! Git configuration (system, global, repository; `includeIf`
//! identities), SSH and GPG keys (list, test, signing setup), and the GitHub
//! CLI account (`gh auth status`, token scopes).
//!
//! - [`config`]: `git config --list --show-origin --show-scope -z`, parsed,
//!   with the line of every entry in its file, the effective value of a key
//!   and what overrides what; the common settings with explanations; edit
//!   and unset command builders.
//! - [`includes`]: `includeIf` rules and the files they include, `gitdir`
//!   pattern matching, plans to add, edit and remove an identity per folder.
//! - [`ssh`]: public keys in `~/.ssh` (`ssh-keygen -lf`), the agent
//!   (`ssh-add -l`), `ssh -T git@github.com`, key generation, the
//!   allowed-signers line.
//! - [`gpg`]: `gpg --list-secret-keys --with-colons`, where gpg is.
//! - [`signing`]: commit signing setup (format, key, `commit.gpgsign`,
//!   allowed signers) as a plan of commands.
//! - [`account`]: `gh auth status` (JSON and two text formats), the user,
//!   rate limits, the feature → token scope analysis, `gh` commands.
//!
//! Every change is a list of [`Step`]s built by pure functions, shown in
//! full in a confirmation and run by [`run_steps`]. The global config is
//! written through `git config --global`, so `GIT_CONFIG_GLOBAL` is
//! honoured; files Hadion writes itself (identity files sit next to the
//! global config, the allowed-signers file in the SSH folder) follow the
//! same environment.

pub mod account;
pub mod config;
pub mod gpg;
pub mod includes;
pub mod signing;
pub mod ssh;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::cli;
use crate::devops::gh::Gh;
use crate::devops::proc::{self, ProcSpec};
use crate::github::ApiCall;

/// Where a configuration value comes from (`git config --show-scope`).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
pub enum Scope {
    System,
    #[default]
    Global,
    Local,
    Worktree,
    /// `git -c`, `GIT_CONFIG_PARAMETERS`, `GIT_CONFIG_COUNT`.
    Command,
    Unknown,
}

impl Scope {
    pub fn parse(s: &str) -> Scope {
        match s.trim() {
            "system" => Scope::System,
            "global" => Scope::Global,
            "local" => Scope::Local,
            "worktree" => Scope::Worktree,
            "command" => Scope::Command,
            _ => Scope::Unknown,
        }
    }

    /// Lower-case name as git prints it (`command line` for `command`).
    pub fn label(self) -> &'static str {
        match self {
            Scope::System => "system",
            Scope::Global => "global",
            Scope::Local => "local",
            Scope::Worktree => "worktree",
            Scope::Command => "command line",
            Scope::Unknown => "unknown",
        }
    }

    /// Sentence-case name for badges.
    pub fn title(self) -> &'static str {
        match self {
            Scope::System => "System",
            Scope::Global => "Global",
            Scope::Local => "Local",
            Scope::Worktree => "Worktree",
            Scope::Command => "Command line",
            Scope::Unknown => "Unknown",
        }
    }

    /// The `git config` flag that selects this scope.
    pub fn flag(self) -> Option<&'static str> {
        match self {
            Scope::System => Some("--system"),
            Scope::Global => Some("--global"),
            Scope::Local => Some("--local"),
            Scope::Worktree => Some("--worktree"),
            Scope::Command | Scope::Unknown => None,
        }
    }

    /// Scopes Hadion writes to (system needs administrator rights and is
    /// shown read-only).
    pub fn editable(self) -> bool {
        matches!(self, Scope::Global | Scope::Local)
    }
}

/// Convert a Git Bash style path (`/c/Users/x`) to a Windows one
/// (`C:/Users/x`); anything else is returned unchanged.
pub fn from_msys(p: &str) -> String {
    let b = p.as_bytes();
    if cfg!(windows)
        && b.len() >= 2
        && b[0] == b'/'
        && b[1].is_ascii_alphabetic()
        && (b.len() == 2 || b[2] == b'/')
    {
        format!("{}:{}", (b[1] as char).to_ascii_uppercase(), &p[2..])
    } else {
        p.to_owned()
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .map(|v| PathBuf::from(from_msys(&v)))
}

/// The home folder git expands `~` to: `HOME`, else `HOMEDRIVE` +
/// `HOMEPATH`, else `USERPROFILE` (Git for Windows' order).
pub fn git_home() -> Option<PathBuf> {
    if let Some(h) = env_path("HOME") {
        return Some(h);
    }
    if let (Ok(d), Ok(p)) = (std::env::var("HOMEDRIVE"), std::env::var("HOMEPATH")) {
        let h = PathBuf::from(format!("{d}{p}"));
        if h.is_dir() {
            return Some(h);
        }
    }
    env_path("USERPROFILE")
}

/// The folder whose `.ssh` holds the user's keys: `USERPROFILE` on Windows
/// (the profile folder Windows OpenSSH and Git's ssh use), `HOME` elsewhere.
pub fn ssh_home() -> Option<PathBuf> {
    if cfg!(windows) {
        env_path("USERPROFILE").or_else(|| env_path("HOME"))
    } else {
        env_path("HOME")
    }
}

/// `~/.ssh`.
pub fn ssh_dir() -> Option<PathBuf> {
    ssh_home().map(|h| h.join(".ssh"))
}

/// The global config file git writes with `--global`: `GIT_CONFIG_GLOBAL`
/// when set, else `~/.gitconfig` (or the XDG file when only that exists).
pub fn global_config_file() -> Option<PathBuf> {
    if let Some(p) = env_path("GIT_CONFIG_GLOBAL") {
        return Some(p);
    }
    let home = git_home()?;
    let classic = home.join(".gitconfig");
    if classic.exists() {
        return Some(classic);
    }
    let xdg = env_path("XDG_CONFIG_HOME")
        .unwrap_or_else(|| home.join(".config"))
        .join("git")
        .join("config");
    if xdg.exists() {
        return Some(xdg);
    }
    Some(classic)
}

/// A path with forward slashes, as git prints and accepts it.
pub fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Do two paths name the same file (case-insensitive on Windows)?
pub fn same_path(a: &Path, b: &Path) -> bool {
    let n = |p: &Path| {
        let s = slash(p).trim_end_matches('/').to_owned();
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    n(a) == n(b)
}

/// `~/…` when `p` is inside `home`, else the path with forward slashes.
pub fn tilde(p: &Path, home: Option<&Path>) -> String {
    let s = slash(p);
    if let Some(h) = home {
        let hs = slash(h).trim_end_matches('/').to_owned();
        let (a, b) = if cfg!(windows) {
            (s.to_lowercase(), hs.to_lowercase())
        } else {
            (s.clone(), hs.clone())
        };
        if a == b {
            return "~".to_owned();
        }
        if a.starts_with(&format!("{b}/")) {
            return format!("~{}", &s[hs.len()..]);
        }
    }
    s
}

/// Expand a leading `~/` with `home`.
pub fn expand_tilde(p: &str, home: Option<&Path>) -> PathBuf {
    match (p.strip_prefix("~/"), home) {
        (Some(rest), Some(h)) => h.join(rest),
        _ if p == "~" && home.is_some() => home.map(Path::to_path_buf).unwrap_or_default(),
        _ => PathBuf::from(from_msys(p)),
    }
}

/// `git a b "c d"` as shown in logs and confirmations.
pub fn git_line<S: AsRef<str>>(args: &[S]) -> String {
    cmd_line("git", args)
}

/// `program a b "c d"`.
pub fn cmd_line<S: AsRef<str>>(program: &str, args: &[S]) -> String {
    let a: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
    if a.is_empty() {
        program.to_owned()
    } else {
        format!("{program} {}", proc::render_args(&a))
    }
}

/// Run git in `dir`, record the command, return stdout.
pub fn git<S: AsRef<str>>(
    dir: &Path,
    args: &[S],
    cmds: &mut Vec<String>,
) -> Result<Vec<u8>, String> {
    cmds.push(git_line(args));
    cli::run(dir, args, None)
        .map(|o| o.stdout)
        .map_err(|e| e.to_string())
}

/// Run git and return its output whatever the exit code (`git config
/// --get` exits 1 when a key is not set).
pub fn git_raw<S: AsRef<str>>(
    dir: &Path,
    args: &[S],
    cmds: &mut Vec<String>,
) -> Result<cli::RawOutput, String> {
    cmds.push(git_line(args));
    cli::run_raw("git", dir, args, None, &[]).map_err(|e| e.to_string())
}

/// Run another tool (ssh-keygen, ssh-add, ssh, gpg) to the end with a
/// timeout, recording the command.
pub fn tool(
    program: &str,
    args: &[String],
    dir: &Path,
    timeout: Duration,
    cmds: &mut Vec<String>,
) -> Result<proc::Captured, String> {
    cmds.push(cmd_line(program, args));
    let spec = ProcSpec::new(program, dir).args(args.iter().cloned());
    proc::run_capture(&spec, Some(timeout)).map_err(|e| e.to_string())
}

/// One thing a change does. Built by pure functions, shown in the
/// confirmation exactly as [`Step::display`] renders it, run by
/// [`run_steps`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// `git <args>` in the active repository (local scope) or anywhere.
    Git(Vec<String>),
    /// Create a folder and its parents.
    MakeDir(PathBuf),
    /// Append a line to a file (created when missing) unless the file has
    /// that line already.
    AppendLine { path: PathBuf, line: String },
    /// Delete a file.
    RemoveFile(PathBuf),
    /// Another local program (`ssh-keygen`).
    Run { program: String, args: Vec<String> },
    /// A `gh` subcommand that changes something on GitHub or in gh's own
    /// configuration (`gh ssh-key add`, `gh auth switch`).
    Gh(Vec<String>),
    /// `gpg --armor --export KEY | gh gpg-key add - --title TITLE`.
    GpgUpload {
        gpg: String,
        key: String,
        title: String,
    },
}

impl Step {
    pub fn display(&self) -> String {
        match self {
            Step::Git(a) => git_line(a),
            Step::MakeDir(p) => format!("create folder {}", slash(p)),
            Step::AppendLine { path, line } => {
                format!("append to {}:\n    {line}", slash(path))
            }
            Step::RemoveFile(p) => format!("delete {}", slash(p)),
            Step::Run { program, args } => cmd_line(&program_name(program), args),
            Step::Gh(a) => cmd_line("gh", a),
            Step::GpgUpload { gpg, key, title } => format!(
                "{} | gh gpg-key add - --title {}",
                cmd_line(&program_name(gpg), &["--armor", "--export", key.as_str()]),
                proc::render_args(&[title.as_str()])
            ),
        }
    }

    /// Does this step talk to GitHub?
    pub fn remote(&self) -> bool {
        matches!(self, Step::Gh(_) | Step::GpgUpload { .. })
    }
}

/// The name a program is typed as (`ssh-keygen` for
/// `C:/Program Files/Git/usr/bin/ssh-keygen.exe`): steps run the resolved
/// path, confirmations show what the user would type.
pub fn program_name(program: &str) -> String {
    let base = Path::new(program)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(program);
    base.strip_suffix(".exe")
        .or_else(|| base.strip_suffix(".EXE"))
        .unwrap_or(base)
        .to_owned()
}

/// Every step's display, for a confirmation.
pub fn display_steps(steps: &[Step]) -> Vec<String> {
    steps.iter().map(Step::display).collect()
}

fn append_line(path: &Path, line: &str) -> Result<bool, String> {
    let existing = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("Could not read {}: {e}", slash(path))),
    };
    if existing.lines().any(|l| l.trim() == line.trim()) {
        return Ok(false);
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(line);
    text.push('\n');
    std::fs::write(path, text).map_err(|e| format!("Could not write {}: {e}", slash(path)))?;
    Ok(true)
}

/// Run the steps in order in `dir`, stopping at the first failure. Returns
/// a one-line summary.
pub fn run_steps(
    steps: &[Step],
    dir: &Path,
    cmds: &mut Vec<String>,
    calls: &mut Vec<ApiCall>,
) -> Result<String, String> {
    let mut done = 0usize;
    let mut notes: Vec<String> = Vec::new();
    for s in steps {
        let r: Result<(), String> = match s {
            Step::Git(args) => git(dir, args, cmds).map(|_| ()),
            Step::MakeDir(p) => {
                cmds.push(s.display());
                std::fs::create_dir_all(p)
                    .map_err(|e| format!("Could not create {}: {e}", slash(p)))
            }
            Step::AppendLine { path, line } => {
                cmds.push(format!("append a line to {}", slash(path)));
                append_line(path, line).map(|added| {
                    if !added {
                        notes.push(format!("{} had the line already", slash(path)));
                    }
                })
            }
            Step::RemoveFile(p) => {
                cmds.push(s.display());
                match std::fs::remove_file(p) {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(format!("Could not delete {}: {e}", slash(p))),
                }
            }
            Step::Run { program, args } => tool(program, args, dir, Duration::from_secs(60), cmds)
                .and_then(|c| {
                    if c.success() {
                        Ok(())
                    } else {
                        Err(c.error_text())
                    }
                }),
            Step::Gh(args) => {
                let a: Vec<&str> = args.iter().map(String::as_str).collect();
                Gh::user(dir)
                    .cli(&a, None, calls)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
            Step::GpgUpload { gpg, key, title } => {
                let args: Vec<String> = ["--armor", "--export", key.as_str()]
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect();
                tool(gpg, &args, dir, Duration::from_secs(30), cmds).and_then(|c| {
                    if !c.success() || c.stdout.is_empty() {
                        return Err(if c.success() {
                            format!("gpg exported nothing for {key}")
                        } else {
                            c.error_text()
                        });
                    }
                    Gh::user(dir)
                        .cli(
                            &["gpg-key", "add", "-", "--title", title.as_str()],
                            Some(&c.stdout),
                            calls,
                        )
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                })
            }
        };
        if let Err(e) = r {
            return Err(if done == 0 {
                e
            } else {
                format!(
                    "{e} (after {done} of {} steps had run; this one failed: {})",
                    steps.len(),
                    s.display().lines().next().unwrap_or("")
                )
            });
        }
        done += 1;
    }
    let mut summary = format!("{} step{} done", done, if done == 1 { "" } else { "s" });
    if !notes.is_empty() {
        summary.push_str(&format!(" ({})", notes.join("; ")));
    }
    Ok(summary)
}

/// Quote a value for a git config file the way `git config` writes it.
pub fn config_quote(v: &str) -> String {
    let needs = v.starts_with(' ')
        || v.ends_with(' ')
        || v.contains(['#', ';', '"', '\\'])
        || v.contains('\t');
    let esc = v
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\t', "\\t");
    if needs { format!("\"{esc}\"") } else { esc }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes() {
        for s in ["system", "global", "local", "worktree", "command"] {
            assert_ne!(Scope::parse(s), Scope::Unknown);
        }
        assert_eq!(Scope::parse("x"), Scope::Unknown);
        assert_eq!(Scope::Global.flag(), Some("--global"));
        assert!(Scope::Local.editable() && !Scope::System.editable());
        assert!(Scope::System < Scope::Global && Scope::Global < Scope::Local);
    }

    #[test]
    fn paths() {
        if cfg!(windows) {
            assert_eq!(from_msys("/c/Users/x"), "C:/Users/x");
            assert_eq!(from_msys("/d"), "D:");
            assert!(same_path(
                Path::new("C:\\Users\\X"),
                Path::new("c:/users/x/")
            ));
        }
        assert_eq!(from_msys("/usr/bin"), "/usr/bin");
        let home = Path::new("/home/me");
        assert_eq!(
            tilde(Path::new("/home/me/.ssh/id"), Some(home)),
            "~/.ssh/id"
        );
        assert_eq!(tilde(Path::new("/home/me"), Some(home)), "~");
        assert_eq!(tilde(Path::new("/home/meow/x"), Some(home)), "/home/meow/x");
        assert_eq!(
            expand_tilde("~/.gitconfig-work", Some(home)),
            home.join(".gitconfig-work")
        );
    }

    #[test]
    fn step_display() {
        let s = Step::Git(vec![
            "config".into(),
            "--global".into(),
            "user.name".into(),
            "Ada Lovelace".into(),
        ]);
        assert_eq!(
            s.display(),
            "git config --global user.name \"Ada Lovelace\""
        );
        assert!(!s.remote());
        let u = Step::GpgUpload {
            gpg: "gpg".into(),
            key: "ABCD".into(),
            title: "My laptop".into(),
        };
        assert_eq!(
            u.display(),
            "gpg --armor --export ABCD | gh gpg-key add - --title \"My laptop\""
        );
        assert!(u.remote());
        assert_eq!(cmd_line::<&str>("ssh-add", &[]), "ssh-add");
        assert_eq!(
            program_name("C:/Program Files/Git/usr/bin/ssh-keygen.exe"),
            "ssh-keygen"
        );
        assert_eq!(program_name("gpg"), "gpg");
    }

    #[test]
    fn appending_lines_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("hadion-id-append-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("allowed_signers");
        let _ = std::fs::remove_file(&f);
        let steps = vec![
            Step::AppendLine {
                path: f.clone(),
                line: "a@b ssh-ed25519 AAAA".into(),
            },
            Step::AppendLine {
                path: f.clone(),
                line: "a@b ssh-ed25519 AAAA".into(),
            },
        ];
        let (mut cmds, mut calls) = (Vec::new(), Vec::new());
        let s = run_steps(&steps, &dir, &mut cmds, &mut calls).unwrap();
        assert!(s.contains("had the line already"), "{s}");
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            "a@b ssh-ed25519 AAAA\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quoting_config_values() {
        assert_eq!(config_quote("Ada Lovelace"), "Ada Lovelace");
        assert_eq!(config_quote("a#b"), "\"a#b\"");
        assert_eq!(config_quote("C:\\x"), "\"C:\\\\x\"");
        assert_eq!(config_quote(" lead"), "\" lead\"");
    }
}
