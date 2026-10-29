//! Asking Claude through the owner's own Claude Code CLI, so requests use
//! their Claude subscription instead of an API key.
//!
//! - Where it is: `HADION_CLAUDE` (a test double), the native installer's
//!   `~/.local/bin/claude.exe`, a `claude.exe` on PATH, or the program an
//!   npm `claude.cmd` shim starts (the shim itself is never run: batch files
//!   cannot carry a multi-line system prompt safely).
//! - How it is signed in: `claude auth status` (JSON; exit 0 signed in, 1
//!   not). `claude.ai` and `oauth_token` use the plan; an API key (in Claude
//!   Code's own login, an `apiKeyHelper` or `ANTHROPIC_API_KEY`) bills the
//!   API instead.
//! - How it runs: `claude -p` with stream-json output, every tool, MCP
//!   server, skill, hook, plugin and CLAUDE.md switched off, one turn, no
//!   saved session, from an empty folder Hadion owns. The material goes on
//!   standard input; the command line carries only flags and the feature's
//!   system prompt. Every inherited `ANTHROPIC_*` and `CLAUDE*` variable
//!   is removed (except `CLAUDE_CONFIG_DIR`, where the login lives), so an
//!   `ANTHROPIC_API_KEY` meant for Hadion's API route never bills through
//!   Claude Code, and a parent Claude Code session's variables never leak in.
//! - The stream: `system/init` (abort if any tool is enabled),
//!   `system/api_retry` (shown as a note), `stream_event` (the Messages API
//!   events, folded by [`Answer::apply`]), `rate_limit_event` (the plan's
//!   usage) and the final `result`. Hadion never reads or stores Claude
//!   Code's credentials.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::model::{CliModel, Effort, Usage};
use super::request::Prompt;
use super::runner::{Done, Failure, Shared, Via};
use super::sse::SseEvent;

/// Replaces the `claude` executable (a test double).
pub const OVERRIDE: &str = "HADION_CLAUDE";
/// The first version with `CLAUDE_CODE_STARTUP_FAILURE_RESULTS` (sign-in
/// problems arrive as a result instead of a bare exit code).
pub const MIN_VERSION: Version = Version(2, 1, 274);
/// Claude Code reads at most 10 MB from a pipe.
pub const MAX_STDIN: usize = 9_500_000;
/// The longest a request may run.
pub const MAX_TIME: Duration = Duration::from_secs(900);
/// `--mcp-config` with no servers (with `--strict-mcp-config`).
pub const NO_MCP: &str = r#"{"mcpServers":{}}"#;
/// The folder every request runs in (empty: Claude Code reads CLAUDE.md
/// and settings from its working directory and every parent).
pub const WORKDIR: &str = "hadion-claude-code";
/// How to install Claude Code: `(label, command)`.
pub const INSTALL_HINTS: [(&str, &str); 3] = [
    (
        "PowerShell (native installer)",
        "irm https://claude.ai/install.ps1 | iex",
    ),
    ("npm", "npm install -g @anthropic-ai/claude-code"),
    ("winget", "winget install Anthropic.ClaudeCode"),
];
/// How to sign in (in a terminal; Hadion never runs it).
pub const SIGN_IN: &str = "claude auth login";

/// Variables set for every Claude Code process Hadion starts.
pub const SET_ENV: [(&str, &str); 7] = [
    ("DISABLE_AUTOUPDATER", "1"),
    ("DISABLE_TELEMETRY", "1"),
    ("DISABLE_ERROR_REPORTING", "1"),
    ("CLAUDE_CODE_SKIP_PROMPT_HISTORY", "1"),
    ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1"),
    ("CLAUDE_CODE_DISABLE_CLAUDE_MDS", "1"),
    ("CLAUDE_CODE_STARTUP_FAILURE_RESULTS", "1"),
];

// ---------------------------------------------------------------------------
// Finding it
// ---------------------------------------------------------------------------

/// A Claude Code installation Hadion can start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Install {
    /// The program started (by full path, never through a shell).
    pub program: PathBuf,
    /// Arguments before Claude Code's own (`cli.js` when Node.js runs an
    /// older npm install).
    pub prefix: Vec<String>,
    /// How it was found, for the Settings page.
    pub how: &'static str,
    /// The npm shim on PATH that led to `program`.
    pub shim: Option<PathBuf>,
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    }
}

/// Find Claude Code (cheap: file checks only).
pub fn find() -> Option<Install> {
    let home = std::env::var_os("USERPROFILE")
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    find_with(std::env::var_os(OVERRIDE), home, std::env::var_os("PATH"))
}

/// [`find`] with the inputs given (tests).
pub fn find_with(
    over: Option<OsString>,
    home: Option<PathBuf>,
    path: Option<OsString>,
) -> Option<Install> {
    if let Some(o) = over.filter(|o| !o.is_empty()) {
        let p = PathBuf::from(o);
        return p.is_file().then_some(Install {
            program: p,
            prefix: Vec::new(),
            how: "HADION_CLAUDE",
            shim: None,
        });
    }
    if let Some(h) = home {
        let p = h.join(".local").join("bin").join(exe_name());
        if p.is_file() {
            return Some(Install {
                program: p,
                prefix: Vec::new(),
                how: "the native installer (~/.local/bin)",
                shim: None,
            });
        }
    }
    let dirs: Vec<PathBuf> = path
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for d in &dirs {
        let p = d.join(exe_name());
        if p.is_file() {
            return Some(Install {
                program: p,
                prefix: Vec::new(),
                how: "PATH",
                shim: None,
            });
        }
    }
    // npm's shims (`claude.cmd`, `claude.ps1`, `claude`): the program they
    // start, from the package next to them.
    for d in &dirs {
        for shim in ["claude.cmd", "claude.ps1", "claude"] {
            let shim = d.join(shim);
            if shim.is_file()
                && let Some(i) = from_npm_shim(d, &shim)
            {
                return Some(i);
            }
        }
    }
    None
}

fn from_npm_shim(dir: &Path, shim: &Path) -> Option<Install> {
    let pkg = dir
        .join("node_modules")
        .join("@anthropic-ai")
        .join("claude-code");
    let exe = pkg.join("bin").join(exe_name());
    if exe.is_file() {
        return Some(Install {
            program: exe,
            prefix: Vec::new(),
            how: "npm (global install)",
            shim: Some(shim.to_path_buf()),
        });
    }
    let js = pkg.join("cli.js");
    if js.is_file() {
        let node = [dir.join(if cfg!(windows) { "node.exe" } else { "node" })]
            .into_iter()
            .find(|p| p.is_file())
            .or_else(|| crate::devops::proc::resolve("node"))?;
        return Some(Install {
            program: node,
            prefix: vec![js.to_string_lossy().into_owned()],
            how: "npm (global install, run by Node.js)",
            shim: Some(shim.to_path_buf()),
        });
    }
    None
}

// ---------------------------------------------------------------------------
// Version and sign-in
// ---------------------------------------------------------------------------

/// `2.1.281`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// From `claude --version` (`2.1.281 (Claude Code)`).
pub fn parse_version(text: &str) -> Option<Version> {
    let word = text.split_whitespace().next()?;
    let mut parts = word.trim_start_matches('v').split('.');
    let n = |p: Option<&str>| -> Option<u32> {
        let digits: String = p?.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().ok()
    };
    Some(Version(
        n(parts.next())?,
        n(parts.next())?,
        n(parts.next()).unwrap_or(0),
    ))
}

/// From `claude auth status` (JSON).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Auth {
    pub logged_in: bool,
    /// `none`, `claude.ai`, `oauth_token`, `api_key`, `api_key_helper`,
    /// `third_party`.
    pub method: String,
    /// Set when an API key would be used (`ANTHROPIC_API_KEY`, a helper).
    pub api_key_source: Option<String>,
    /// `firstParty`, or Bedrock / Vertex / Foundry.
    pub provider: Option<String>,
    pub email: Option<String>,
    /// `max`, `pro`, `team`, `enterprise`.
    pub subscription: Option<String>,
    pub org: Option<String>,
}

impl Auth {
    /// Signed in with a Claude account: requests use the plan.
    pub fn plan(&self) -> bool {
        self.logged_in
            && matches!(self.method.as_str(), "claude.ai" | "oauth_token")
            && self.api_key_source.is_none()
            && self.provider.as_deref().is_none_or(|p| p == "firstParty")
    }

    /// Signed in, but requests bill an API account (or a cloud provider).
    pub fn bills_api(&self) -> bool {
        self.logged_in && !self.plan()
    }

    /// `Claude Max plan`, `API key`, `Not signed in`.
    pub fn label(&self) -> String {
        if !self.logged_in {
            return "Not signed in".to_owned();
        }
        if self.plan() {
            return match self.subscription.as_deref().filter(|s| !s.is_empty()) {
                Some(s) => format!("Claude {} plan", title_case(s)),
                None => "Claude account".to_owned(),
            };
        }
        if self.api_key_source.is_some() {
            return format!(
                "API key ({})",
                self.api_key_source.as_deref().unwrap_or_default()
            );
        }
        match self.method.as_str() {
            "api_key" => "API key".to_owned(),
            "api_key_helper" => "API key helper".to_owned(),
            "third_party" => format!(
                "Cloud provider ({})",
                self.provider.as_deref().unwrap_or("not first-party")
            ),
            m => m.to_owned(),
        }
    }
}

impl Auth {
    /// The sign-in for a sentence ("signed in with …"): `your Claude Max
    /// plan`, `an API key`, `an API key (ANTHROPIC_API_KEY)`.
    pub fn phrase(&self) -> String {
        if !self.logged_in {
            return "no account".to_owned();
        }
        if self.plan() {
            return format!("your {}", self.label());
        }
        if let Some(src) = &self.api_key_source {
            return format!("an API key ({src})");
        }
        match self.method.as_str() {
            "api_key" => "an API key".to_owned(),
            "api_key_helper" => "an API key helper".to_owned(),
            "third_party" => format!(
                "a cloud provider ({})",
                self.provider.as_deref().unwrap_or("not first-party")
            ),
            m => format!("`{m}`"),
        }
    }
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Parse `claude auth status` output.
pub fn parse_auth(text: &str) -> Option<Auth> {
    let start = text.find('{')?;
    let v: Value = serde_json::from_str(text[start..].trim()).ok()?;
    let s = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && *s != "none")
            .map(str::to_owned)
    };
    Some(Auth {
        logged_in: v.get("loggedIn").and_then(Value::as_bool).unwrap_or(false),
        method: v
            .get("authMethod")
            .and_then(Value::as_str)
            .unwrap_or("none")
            .to_owned(),
        api_key_source: s("apiKeySource"),
        provider: s("apiProvider"),
        email: s("email"),
        subscription: s("subscriptionType"),
        org: s("orgName"),
    })
}

/// What Hadion knows about Claude Code on this machine.
#[derive(Clone, Debug)]
pub struct Status {
    pub install: Option<Install>,
    pub version: Option<Version>,
    /// `claude --version` as printed.
    pub version_text: String,
    pub auth: Option<Auth>,
    /// Why the version or the sign-in could not be read.
    pub error: Option<String>,
    pub checked: Instant,
}

impl Status {
    pub fn installed(&self) -> bool {
        self.install.is_some()
    }

    pub fn signed_in(&self) -> bool {
        self.auth.as_ref().is_some_and(|a| a.logged_in)
    }

    /// Installed and signed in with a Claude plan.
    pub fn plan_ready(&self) -> bool {
        self.installed() && self.auth.as_ref().is_some_and(Auth::plan)
    }

    /// Older than [`MIN_VERSION`].
    pub fn old(&self) -> bool {
        self.version.is_some_and(|v| v < MIN_VERSION)
    }

    /// `Claude Code 2.1.281 · Claude Max plan`.
    pub fn summary(&self) -> String {
        match (&self.install, &self.auth) {
            (None, _) => "Claude Code is not installed".to_owned(),
            (Some(_), None) => format!(
                "Claude Code {}: the sign-in could not be read",
                self.version.map(|v| v.to_string()).unwrap_or_default()
            ),
            (Some(_), Some(a)) => format!(
                "Claude Code {} · {}",
                self.version
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "(version unknown)".to_owned()),
                a.label()
            ),
        }
    }
}

fn cache() -> &'static Mutex<Option<Status>> {
    static CACHE: OnceLock<Mutex<Option<Status>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// How long a check is reused.
const FRESH: Duration = Duration::from_secs(300);

/// Find Claude Code, its version and how it is signed in (runs `claude
/// --version` and `claude auth status`; reused for five minutes unless
/// `force`). Call from a background task.
pub fn detect(force: bool) -> Status {
    if !force
        && let Some(s) = cache()
            .lock()
            .ok()
            .and_then(|c| c.clone())
            .filter(|s| s.checked.elapsed() < FRESH)
    {
        return s;
    }
    let status = match workdir() {
        Ok(cwd) => detect_in(find(), &cwd),
        Err(e) => Status {
            install: find(),
            version: None,
            version_text: String::new(),
            auth: None,
            error: Some(format!("Could not create the working folder: {e}")),
            checked: Instant::now(),
        },
    };
    if let Ok(mut c) = cache().lock() {
        *c = Some(status.clone());
    }
    status
}

/// Forget the cached check (after a sign-in, an install).
pub fn forget() {
    if let Ok(mut c) = cache().lock() {
        *c = None;
    }
}

/// Check `install` (no cache), running it in `cwd`.
pub fn detect_in(install: Option<Install>, cwd: &Path) -> Status {
    let mut status = Status {
        install: install.clone(),
        version: None,
        version_text: String::new(),
        auth: None,
        error: None,
        checked: Instant::now(),
    };
    let Some(install) = install else {
        return status;
    };
    match capture(&install, &["--version"], cwd, Duration::from_secs(20)) {
        Ok((_, out, err)) => {
            let text = if out.trim().is_empty() { err } else { out };
            status.version_text = text.trim().lines().next().unwrap_or("").to_owned();
            status.version = parse_version(&status.version_text);
        }
        Err(e) => status.error = Some(e),
    }
    match capture(&install, &["auth", "status"], cwd, Duration::from_secs(30)) {
        Ok((_, out, err)) => match parse_auth(&out) {
            Some(a) => status.auth = Some(a),
            None => {
                let text = if err.trim().is_empty() { out } else { err };
                status.error = Some(format!(
                    "`claude auth status` printed something Hadion could not read: {}",
                    text.trim().chars().take(300).collect::<String>()
                ));
            }
        },
        Err(e) => status.error = Some(e),
    }
    status
}

// ---------------------------------------------------------------------------
// The command
// ---------------------------------------------------------------------------

/// Claude Code's arguments for one request. The material is not among
/// them: it goes on standard input.
pub fn args(model: CliModel, effort: Effort, system: &str) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--safe-mode",
        "--tools",
        "",
        "--disallowedTools",
        "*",
        "--disable-slash-commands",
        "--strict-mcp-config",
        "--mcp-config",
        NO_MCP,
        "--system-prompt",
        system,
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    if let Some(m) = model.arg() {
        a.push("--model".to_owned());
        a.push(m.to_owned());
    }
    for s in [
        "--effort",
        effort.as_str(),
        "--max-turns",
        "1",
        "--no-session-persistence",
    ] {
        a.push(s.to_owned());
    }
    a
}

/// The flags of [`args`] that take a value.
const TAKES_VALUE: [&str; 8] = [
    "--output-format",
    "--tools",
    "--disallowedTools",
    "--mcp-config",
    "--system-prompt",
    "--model",
    "--effort",
    "--max-turns",
];

/// The command as shown in the confirmation and the log: the program, then
/// one flag per line with its value, the system prompt replaced by its size.
pub fn display(install: &Install, args: &[String]) -> Vec<String> {
    let mut out = vec![install.program.display().to_string()];
    out.extend(install.prefix.iter().map(|p| format!("  {}", quote(p))));
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if !TAKES_VALUE.contains(&a.as_str()) {
            out.push(format!("  {}", quote(a)));
            continue;
        }
        let v = it.next().map(String::as_str).unwrap_or_default();
        let shown = if a == "--system-prompt" {
            format!(
                "\"<the system prompt below: {} characters>\"",
                v.chars().count()
            )
        } else {
            quote(v)
        };
        out.push(format!("  {a} {shown}"));
    }
    out
}

fn quote(a: &str) -> String {
    if a.is_empty() || a.contains(|c: char| c.is_whitespace() || c == '"' || c == '*') {
        format!("\"{}\"", a.replace('"', "\\\""))
    } else {
        a.to_owned()
    }
}

/// Environment changes for a Claude Code process.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvPlan {
    /// Inherited variables removed.
    pub remove: Vec<String>,
    pub set: Vec<(String, String)>,
}

/// Remove every inherited `ANTHROPIC_*` and `CLAUDE*` variable except
/// `CLAUDE_CONFIG_DIR`, then set [`SET_ENV`]. `names` are the current
/// environment's variable names.
pub fn env_plan<I, S>(names: I) -> EnvPlan
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut remove: Vec<String> = names
        .into_iter()
        .map(|n| n.as_ref().to_owned())
        .filter(|n| {
            let u = n.to_ascii_uppercase();
            (u.starts_with("ANTHROPIC_") || u.starts_with("CLAUDE")) && u != "CLAUDE_CONFIG_DIR"
        })
        .filter(|n| !SET_ENV.iter().any(|(k, _)| k.eq_ignore_ascii_case(n)))
        .collect();
    remove.sort();
    remove.dedup();
    EnvPlan {
        remove,
        set: SET_ENV
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
    }
}

/// The empty folder requests run in (created when missing).
pub fn workdir() -> std::io::Result<PathBuf> {
    let d = std::env::temp_dir().join(WORKDIR);
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

fn command(install: &Install, args: &[String], plan: &EnvPlan, cwd: &Path) -> Command {
    let mut cmd = Command::new(&install.program);
    cmd.args(&install.prefix).args(args).current_dir(cwd);
    for k in &plan.remove {
        cmd.env_remove(k);
    }
    for (k, v) in &plan.set {
        cmd.env(k, v);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd
}

fn current_names() -> Vec<String> {
    std::env::vars_os()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect()
}

/// Run a short Claude Code command (`--version`, `auth status`) and
/// capture its output.
fn capture(
    install: &Install,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
) -> Result<(Option<i32>, String, String), String> {
    let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    let mut child = command(install, &args, &env_plan(current_names()), cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not start {} ({e})", install.program.display()))?;
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let out_h = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(p) = out.as_mut() {
            let _ = p.read_to_string(&mut s);
        }
        s
    });
    let err_h = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(p) = err.as_mut() {
            let _ = p.read_to_string(&mut s);
        }
        s
    });
    let started = Instant::now();
    let code = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.code(),
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }
        if started.elapsed() > timeout {
            crate::devops::proc::kill_tree(child.id());
            let _ = child.kill();
            return Err(format!(
                "`claude {}` did not finish within {} s",
                args.join(" "),
                timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Ok((
        code,
        out_h.join().unwrap_or_default(),
        err_h.join().unwrap_or_default(),
    ))
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// What went wrong, as far as Claude Code says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliErrorKind {
    NotSignedIn,
    LoginExpired,
    /// Claude Code's API key was refused.
    InvalidApiKey,
    /// The plan's five-hour limit is used up.
    SessionLimit,
    WeeklyLimit,
    /// An API login without credit.
    CreditTooLow,
    /// "Server is temporarily limiting requests".
    Throttled,
    RateLimited,
    Overloaded,
    ModelNotFound,
    OrgNotAllowed,
    AccountOnHold,
    Billing,
    ServerError,
    /// Too old for the flags Hadion passes.
    TooOld,
    /// Claude Code started with tools enabled; Hadion stopped it.
    ToolsEnabled,
    /// Ran longer than [`MAX_TIME`].
    Timeout,
    Other,
}

impl CliErrorKind {
    pub fn title(self) -> &'static str {
        match self {
            CliErrorKind::NotSignedIn => "Claude Code is not signed in",
            CliErrorKind::LoginExpired => "Claude Code's sign-in has expired",
            CliErrorKind::InvalidApiKey => "Claude Code's API key was refused",
            CliErrorKind::SessionLimit => "Your plan's session limit is reached",
            CliErrorKind::WeeklyLimit => "Your plan's weekly limit is reached",
            CliErrorKind::CreditTooLow => "The API credit balance is too low",
            CliErrorKind::Throttled => "Claude is limiting requests right now",
            CliErrorKind::RateLimited => "Rate limited",
            CliErrorKind::Overloaded => "Claude is overloaded",
            CliErrorKind::ModelNotFound => "Claude Code cannot use that model",
            CliErrorKind::OrgNotAllowed => "Your organisation does not allow this sign-in",
            CliErrorKind::AccountOnHold => "The account is on hold",
            CliErrorKind::Billing => "A billing problem stopped the request",
            CliErrorKind::ServerError => "Claude had a server error",
            CliErrorKind::TooOld => "Claude Code is too old for Hadion",
            CliErrorKind::ToolsEnabled => "Claude Code started with tools enabled",
            CliErrorKind::Timeout => "Claude Code took too long",
            CliErrorKind::Other => "Claude Code reported an error",
        }
    }

    /// What to do about it.
    pub fn next_step(self) -> &'static str {
        match self {
            CliErrorKind::NotSignedIn | CliErrorKind::LoginExpired => {
                "Sign in: run `claude auth login` in a terminal, then try again."
            }
            CliErrorKind::InvalidApiKey => {
                "Sign in again with your Claude account (`claude auth login` in a terminal), or use the API provider in Settings → AI assist."
            }
            CliErrorKind::SessionLimit | CliErrorKind::WeeklyLimit => {
                "Wait until the limit resets (Claude Code's message says when), or switch to the Claude API in Settings → AI assist."
            }
            CliErrorKind::CreditTooLow => {
                "Claude Code is signed in with an API account. Add credit, or sign in with your Claude account (`claude auth login`)."
            }
            CliErrorKind::Throttled
            | CliErrorKind::RateLimited
            | CliErrorKind::Overloaded
            | CliErrorKind::ServerError => "Try again in a moment.",
            CliErrorKind::ModelNotFound => {
                "Pick another Claude Code model in Settings → AI assist (Default uses Claude Code's own choice)."
            }
            CliErrorKind::OrgNotAllowed | CliErrorKind::AccountOnHold | CliErrorKind::Billing => {
                "Check your account on claude.ai, or use the API provider in Settings → AI assist."
            }
            CliErrorKind::TooOld => {
                "Update Claude Code (`claude update` in a terminal), then use Check again in Settings → AI assist."
            }
            CliErrorKind::ToolsEnabled => {
                "Hadion only runs Claude Code with every tool switched off. Update Claude Code, or use the API provider."
            }
            CliErrorKind::Timeout => "Try again, or lower the effort in Settings → AI assist.",
            CliErrorKind::Other => "",
        }
    }

    pub fn retryable(self) -> bool {
        matches!(
            self,
            CliErrorKind::Throttled
                | CliErrorKind::RateLimited
                | CliErrorKind::Overloaded
                | CliErrorKind::ServerError
                | CliErrorKind::Timeout
                | CliErrorKind::Other
        )
    }

    /// Fixed by signing in again.
    pub fn needs_sign_in(self) -> bool {
        matches!(
            self,
            CliErrorKind::NotSignedIn | CliErrorKind::LoginExpired | CliErrorKind::InvalidApiKey
        )
    }
}

/// A Claude Code error with its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliError {
    pub kind: CliErrorKind,
    /// Claude Code's own message.
    pub message: String,
    /// The `error` code of its error message (`authentication_failed`).
    pub code: Option<String>,
    /// `api_error_status`.
    pub status: Option<u16>,
    pub request_id: Option<String>,
}

impl CliError {
    pub fn new(kind: CliErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            code: None,
            status: None,
            request_id: None,
        }
    }

    /// `Not logged in · Please run /login (authentication_failed)`.
    pub fn summary(&self) -> String {
        let mut s = if self.message.is_empty() {
            self.kind.title().to_owned()
        } else {
            self.message.clone()
        };
        let mut extra = Vec::new();
        if let Some(c) = &self.code {
            extra.push(c.clone());
        }
        if let Some(st) = self.status {
            extra.push(format!("HTTP {st}"));
        }
        if let Some(r) = &self.request_id {
            extra.push(format!("request {r}"));
        }
        if !extra.is_empty() {
            s.push_str(&format!(" ({})", extra.join(", ")));
        }
        s
    }
}

/// What kind of error a Claude Code message (and its error code) is.
pub fn classify(code: Option<&str>, text: &str) -> CliErrorKind {
    let t = text.to_ascii_lowercase();
    let by_text = [
        ("not logged in", CliErrorKind::NotSignedIn),
        ("login expired", CliErrorKind::LoginExpired),
        ("invalid api key", CliErrorKind::InvalidApiKey),
        ("session limit", CliErrorKind::SessionLimit),
        ("weekly limit", CliErrorKind::WeeklyLimit),
        ("credit balance is too low", CliErrorKind::CreditTooLow),
        ("temporarily limiting requests", CliErrorKind::Throttled),
        ("unknown option", CliErrorKind::TooOld),
        ("issue with the selected model", CliErrorKind::ModelNotFound),
    ];
    if let Some((_, k)) = by_text.iter().find(|(needle, _)| t.contains(needle)) {
        return *k;
    }
    match code.unwrap_or("") {
        "authentication_failed" => CliErrorKind::NotSignedIn,
        "oauth_org_not_allowed" => CliErrorKind::OrgNotAllowed,
        "account_on_hold" => CliErrorKind::AccountOnHold,
        "billing_error" => CliErrorKind::Billing,
        "rate_limit" => CliErrorKind::RateLimited,
        "overloaded" => CliErrorKind::Overloaded,
        "model_not_found" => CliErrorKind::ModelNotFound,
        "server_error" => CliErrorKind::ServerError,
        _ => CliErrorKind::Other,
    }
}

// ---------------------------------------------------------------------------
// The stream
// ---------------------------------------------------------------------------

/// `system/init`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Init {
    pub version: String,
    pub model: String,
    pub tools: Vec<String>,
    pub mcp_servers: Vec<String>,
    pub permission_mode: String,
    pub api_key_source: String,
}

/// `system/api_retry`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Retry {
    pub attempt: u64,
    pub max: u64,
    pub delay_ms: u64,
    pub status: Option<u64>,
    pub error: String,
}

impl Retry {
    /// `Retrying after a rate limit (HTTP 429): attempt 2 of 10, in 5 s`.
    pub fn note(&self) -> String {
        let what = match self.error.as_str() {
            "rate_limit" => "a rate limit",
            "overloaded" => "an overload",
            "server_error" => "a server error",
            "" | "unknown" => "an error",
            e => e,
        };
        let status = self
            .status
            .map(|s| format!(" (HTTP {s})"))
            .unwrap_or_default();
        format!(
            "Retrying after {what}{status}: attempt {} of {}, in {} s",
            self.attempt,
            self.max,
            self.delay_ms.div_ceil(1000)
        )
    }
}

/// `rate_limit_event`: how much of the plan's limits is used.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanUsage {
    /// `allowed`, `allowed_warning`, `rejected`.
    pub status: String,
    /// The limit the status is about (`five_hour`, `seven_day`).
    pub kind: String,
    /// Fractions 0..1 and reset times (Unix seconds).
    pub five_hour: Option<(f64, Option<i64>)>,
    pub seven_day: Option<(f64, Option<i64>)>,
    pub overage: bool,
}

/// The final `result`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    pub subtype: String,
    pub is_error: bool,
    pub text: String,
    pub stop_reason: Option<String>,
    pub usage: Option<Usage>,
    /// Claude Code's client-side estimate at list prices.
    pub cost_usd: Option<f64>,
    pub duration_ms: Option<u64>,
    pub duration_api_ms: Option<u64>,
    pub num_turns: Option<u64>,
    pub api_error_status: Option<u16>,
    pub terminal_reason: Option<String>,
    pub errors: Vec<String>,
}

/// What a line asks the runner to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Continue,
    /// `system/init` listed tools: stop the process.
    ToolsEnabled(Vec<String>),
}

/// Everything in a Claude Code stream besides the answer itself.
#[derive(Clone, Debug, Default)]
pub struct Stream {
    pub init: Option<Init>,
    pub retry: Option<Retry>,
    pub plan: Option<PlanUsage>,
    /// An error message (`is_api_error_message`): code, text, request id.
    pub api_error: Option<(Option<String>, String, Option<String>)>,
    pub outcome: Option<Outcome>,
    pub lines: usize,
    pub unreadable: usize,
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|x| match x {
                    Value::String(s) => s.clone(),
                    o => o
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| o.to_string()),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn usage_of(u: &Value) -> Option<Usage> {
    if !u.is_object() {
        return None;
    }
    let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    Some(Usage {
        input_tokens: n("input_tokens"),
        output_tokens: n("output_tokens"),
        cache_creation_input_tokens: n("cache_creation_input_tokens"),
        cache_read_input_tokens: n("cache_read_input_tokens"),
    })
}

impl Stream {
    /// Fold one stdout line in; text and usage go to `answer`.
    pub fn line(&mut self, answer: &mut super::sse::Answer, text: &str) -> Step {
        let text = text.trim();
        if text.is_empty() {
            return Step::Continue;
        }
        self.lines += 1;
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            self.unreadable += 1;
            return Step::Continue;
        };
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
        let sub = v.get("subtype").and_then(Value::as_str).unwrap_or("");
        match (kind, sub) {
            ("system", "init") => {
                let s = |k: &str| {
                    v.get(k)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                let init = Init {
                    version: s("claude_code_version"),
                    model: s("model"),
                    tools: strings(&v["tools"]),
                    mcp_servers: strings(&v["mcp_servers"]),
                    permission_mode: s("permissionMode"),
                    api_key_source: s("apiKeySource"),
                };
                if answer.model.is_empty() {
                    answer.model = init.model.clone();
                }
                let tools = init.tools.clone();
                self.init = Some(init);
                if !tools.is_empty() {
                    return Step::ToolsEnabled(tools);
                }
            }
            ("system", "api_retry") => {
                let n = |k: &str| v.get(k).and_then(Value::as_u64);
                self.retry = Some(Retry {
                    attempt: n("attempt").unwrap_or(0),
                    max: n("max_retries").unwrap_or(0),
                    delay_ms: n("retry_delay_ms").unwrap_or(0),
                    status: n("error_status"),
                    error: v
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                });
            }
            ("stream_event", _) => {
                let ev = &v["event"];
                if let Some(name) = ev.get("type").and_then(Value::as_str) {
                    // A real token arrived: the retry is over.
                    if name == "message_start" {
                        self.retry = None;
                    }
                    answer.apply(&SseEvent {
                        event: name.to_owned(),
                        data: ev.to_string(),
                    });
                }
            }
            ("assistant", _) => {
                // Its text repeats the stream events; only errors matter.
                if v.get("is_api_error_message").and_then(Value::as_bool) == Some(true)
                    || v.get("error").is_some_and(Value::is_string)
                {
                    let content: String = v["message"]["content"]
                        .as_array()
                        .map(|blocks| {
                            blocks
                                .iter()
                                .filter_map(|b| b.get("text").and_then(Value::as_str))
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default();
                    self.api_error = Some((
                        v.get("error").and_then(Value::as_str).map(str::to_owned),
                        content,
                        v.get("request_id")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    ));
                }
            }
            ("rate_limit_event", _) => {
                let info = &v["rate_limit_info"];
                let window = |k: &str| {
                    let w = &info["unifiedWindows"][k];
                    w.get("utilization")
                        .and_then(Value::as_f64)
                        .map(|u| (u, w.get("resetsAt").and_then(Value::as_i64)))
                };
                self.plan = Some(PlanUsage {
                    status: info
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    kind: info
                        .get("rateLimitType")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    five_hour: window("five_hour"),
                    seven_day: window("seven_day"),
                    overage: info
                        .get("isUsingOverage")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                });
            }
            ("result", _) => {
                let n = |k: &str| v.get(k).and_then(Value::as_u64);
                let o = Outcome {
                    subtype: sub.to_owned(),
                    is_error: v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                    text: v
                        .get("result")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    stop_reason: v
                        .get("stop_reason")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    usage: usage_of(&v["usage"]),
                    cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
                    duration_ms: n("duration_ms"),
                    duration_api_ms: n("duration_api_ms"),
                    num_turns: n("num_turns"),
                    api_error_status: n("api_error_status").and_then(|s| u16::try_from(s).ok()),
                    terminal_reason: v
                        .get("terminal_reason")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    errors: strings(&v["errors"]),
                };
                if !o.is_error && o.subtype == "success" {
                    if answer.text.is_empty() && !o.text.is_empty() {
                        answer.text = o.text.clone();
                    }
                    if let Some(u) = o.usage {
                        answer.usage = u;
                    }
                    if answer.stop_reason.is_none() {
                        answer.stop_reason = o.stop_reason.clone();
                    }
                    answer.complete = true;
                }
                self.outcome = Some(o);
            }
            // system/status, user, and newer event types: nothing to do.
            _ => {}
        }
        Step::Continue
    }

    /// The error a finished stream reports, if any.
    pub fn failure(&self) -> Option<CliError> {
        let o = self.outcome.as_ref()?;
        if !o.is_error && o.subtype == "success" {
            return None;
        }
        let (code, api_text, request_id) = self.api_error.clone().unwrap_or_default();
        let mut message = if !o.text.is_empty() {
            o.text.clone()
        } else if !api_text.is_empty() {
            api_text
        } else if !o.errors.is_empty() {
            o.errors.join("; ")
        } else {
            match o.subtype.as_str() {
                "error_max_turns" => "Claude Code stopped after its one turn".to_owned(),
                "error_during_execution" => "Claude Code failed while answering".to_owned(),
                s => format!("Claude Code ended with {s}"),
            }
        };
        if message.len() > 600 {
            message = message.chars().take(600).collect::<String>() + "…";
        }
        Some(CliError {
            kind: classify(code.as_deref(), &message),
            message,
            code,
            status: o.api_error_status,
            request_id,
        })
    }
}

// ---------------------------------------------------------------------------
// Running a request
// ---------------------------------------------------------------------------

/// What Claude Code reported besides the answer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CliRun {
    pub version: String,
    /// Claude Code's client-side estimate at list prices: covered by a plan,
    /// or billed to the API account it is signed in with (`bills_api`).
    pub cost_estimate: Option<f64>,
    pub duration_api_ms: Option<u64>,
    pub plan: Option<PlanUsage>,
    /// `Claude Max plan`.
    pub account: String,
    /// Signed in with an API key or token rather than a Claude plan.
    pub bills_api: bool,
}

/// One Claude Code request.
pub struct CliCall<'a> {
    pub install: &'a Install,
    pub model: CliModel,
    pub effort: Effort,
    /// `Claude Max plan`, for the result.
    pub account: String,
    /// Signed in with an API key or token: the result says it was billed.
    pub bills_api: bool,
    /// Where it runs ([`workdir`] when `None`).
    pub cwd: Option<&'a Path>,
}

/// The material, cut to what Claude Code reads from a pipe.
pub fn stdin_payload(user: &str) -> Vec<u8> {
    if user.len() <= MAX_STDIN {
        return user.as_bytes().to_vec();
    }
    let mut end = MAX_STDIN - 200;
    while !user.is_char_boundary(end) {
        end -= 1;
    }
    let mut s = user[..end].to_owned();
    s.push_str("\n[… the rest was not sent: Claude Code reads at most 10 MB]\n");
    s.into_bytes()
}

/// Ask Claude Code; the answer streams into `live`. Blocks until it ends,
/// fails or `cancel` is set.
pub fn send(
    call: &CliCall,
    prompt: &Prompt,
    cancel: &AtomicBool,
    live: &Shared,
) -> Result<Done, Failure> {
    let started = Instant::now();
    let cwd = match call.cwd {
        Some(c) => c.to_path_buf(),
        None => workdir().map_err(|e| {
            Failure::Prepare(format!(
                "Could not create Claude Code's working folder: {e}"
            ))
        })?,
    };
    let args = args(call.model, call.effort, &prompt.system);
    let plan = env_plan(current_names());
    let input = stdin_payload(&prompt.user);
    let request_bytes = input.len();
    let mut child = command(call.install, &args, &plan, &cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            Failure::ClaudeCode(CliError::new(
                CliErrorKind::Other,
                format!("Could not start {} ({e})", call.install.program.display()),
            ))
        })?;
    if let Some(mut pipe) = child.stdin.take() {
        std::thread::spawn(move || {
            let _ = pipe.write_all(&input);
            // Dropping the pipe closes it: Claude Code starts on EOF.
        });
    }
    let (tx, rx) = mpsc::channel::<String>();
    if let Some(out) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut r = BufReader::new(out);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match r.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if tx.send(String::from_utf8_lossy(&buf).into_owned()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
    }
    let err_h = child.stderr.take().map(|mut e| {
        std::thread::spawn(move || {
            let mut s = Vec::new();
            let _ = e.by_ref().take(64 * 1024).read_to_end(&mut s);
            String::from_utf8_lossy(&s).into_owned()
        })
    });
    let mut stream = Stream::default();
    let mut stop: Option<Failure> = None;
    let mut exited_at: Option<Instant> = None;
    let mut code = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(30)) {
            Ok(line) => {
                let mut l = live.lock().unwrap_or_else(|e| e.into_inner());
                l.bytes += line.len();
                let before = l.answer.text.len();
                let step = stream.line(&mut l.answer, &line);
                if l.first_text.is_none() && l.answer.text.len() > before {
                    l.first_text = Some(Instant::now());
                }
                l.note = stream.retry.as_ref().map(Retry::note);
                if let Step::ToolsEnabled(tools) = step {
                    stop = Some(Failure::ClaudeCode(CliError::new(
                        CliErrorKind::ToolsEnabled,
                        format!(
                            "Claude Code listed these tools although Hadion asked for none: {}. Hadion stopped it before it answered.",
                            tools.join(", ")
                        ),
                    )));
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if exited_at.is_some() {
                    break;
                }
                // Output closed but the process has not exited yet.
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        if stop.is_none() && cancel.load(Ordering::Relaxed) {
            stop = Some(Failure::Cancelled);
        }
        if stop.is_none() && started.elapsed() > MAX_TIME {
            stop = Some(Failure::ClaudeCode(CliError::new(
                CliErrorKind::Timeout,
                format!(
                    "No answer after {} minutes; Hadion stopped Claude Code.",
                    MAX_TIME.as_secs() / 60
                ),
            )));
        }
        if let Some(f) = stop.take() {
            crate::devops::proc::kill_tree(child.id());
            let _ = child.kill();
            let _ = child.wait();
            return Err(f);
        }
        if exited_at.is_none()
            && let Ok(Some(st)) = child.try_wait()
        {
            exited_at = Some(Instant::now());
            code = st.code();
        }
        // Grandchildren may hold the pipe open: stop reading soon after exit.
        if exited_at.is_some_and(|t| t.elapsed() > Duration::from_millis(1500)) {
            break;
        }
    }
    if exited_at.is_none() {
        code = child.wait().ok().and_then(|s| s.code());
    }
    let stderr = err_h.and_then(|h| h.join().ok()).unwrap_or_default();
    let l = live.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(e) = stream.failure() {
        return Err(Failure::ClaudeCode(e));
    }
    if !l.answer.complete {
        let err = stderr
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.starts_with("[claude-code:"))
            .collect::<Vec<_>>()
            .join(" ");
        if code == Some(143) {
            return Err(Failure::Cancelled);
        }
        if !err.is_empty() {
            let kind = classify(None, &err);
            return Err(Failure::ClaudeCode(CliError {
                kind,
                message: err.chars().take(600).collect(),
                code: None,
                status: None,
                request_id: None,
            }));
        }
        return Err(Failure::Incomplete(format!(
            "Claude Code stopped before it finished answering (exit {}, {} lines received).",
            code.map(|c| c.to_string())
                .unwrap_or_else(|| "unknown".to_owned()),
            stream.lines
        )));
    }
    let o = stream.outcome.clone().unwrap_or_default();
    Ok(Done {
        answer: l.answer.clone(),
        elapsed_ms: started.elapsed().as_millis() as u64,
        via: Via::ClaudeCode,
        request_bytes,
        cli: Some(CliRun {
            version: stream
                .init
                .as_ref()
                .map(|i| i.version.clone())
                .unwrap_or_default(),
            cost_estimate: o.cost_usd,
            duration_api_ms: o.duration_api_ms,
            plan: stream.plan.clone(),
            account: call.account.clone(),
            bills_api: call.bills_api,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::ai::sse::Answer;

    #[test]
    fn versions() {
        assert_eq!(
            parse_version("2.1.281 (Claude Code)"),
            Some(Version(2, 1, 281))
        );
        assert_eq!(parse_version("v2.0.5"), Some(Version(2, 0, 5)));
        assert_eq!(parse_version("3.1"), Some(Version(3, 1, 0)));
        assert_eq!(parse_version("Claude Code"), None);
        assert!(Version(2, 1, 273) < MIN_VERSION);
        assert!(Version(2, 1, 274) >= MIN_VERSION);
        assert!(Version(2, 2, 0) > MIN_VERSION);
        assert_eq!(Version(2, 1, 281).to_string(), "2.1.281");
    }

    // Recorded from `claude auth status` 2.1.281 (personal fields replaced).
    const SIGNED_IN: &str = r#"{
  "loggedIn": true,
  "authMethod": "claude.ai",
  "apiProvider": "firstParty",
  "analyticsDisabled": false,
  "email": "someone@example.com",
  "orgId": "00000000-0000-0000-0000-000000000000",
  "orgName": "someone@example.com's Organization",
  "subscriptionType": "max"
}"#;
    const ENV_KEY: &str = r#"{"loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty", "apiKeySource": "ANTHROPIC_API_KEY", "email": null, "orgId": null, "orgName": null, "subscriptionType": null}"#;
    const SIGNED_OUT: &str =
        r#"{"loggedIn": false, "authMethod": "none", "apiProvider": "firstParty"}"#;
    const API_LOGIN: &str =
        r#"{"loggedIn": true, "authMethod": "api_key", "apiProvider": "firstParty"}"#;

    #[test]
    fn sign_in_states() {
        let a = parse_auth(SIGNED_IN).unwrap();
        assert!(a.plan() && !a.bills_api());
        assert_eq!(a.label(), "Claude Max plan");
        assert_eq!(a.email.as_deref(), Some("someone@example.com"));
        let a = parse_auth(ENV_KEY).unwrap();
        assert!(!a.plan() && a.bills_api());
        assert_eq!(a.label(), "API key (ANTHROPIC_API_KEY)");
        let a = parse_auth(SIGNED_OUT).unwrap();
        assert!(!a.logged_in && !a.plan() && !a.bills_api());
        assert_eq!(a.label(), "Not signed in");
        assert!(parse_auth(API_LOGIN).unwrap().bills_api());
        let oauth = r#"{"loggedIn": true, "authMethod": "oauth_token"}"#;
        assert!(parse_auth(oauth).unwrap().plan());
        let bedrock =
            r#"{"loggedIn": true, "authMethod": "third_party", "apiProvider": "bedrock"}"#;
        let a = parse_auth(bedrock).unwrap();
        assert!(!a.plan() && a.bills_api());
        assert_eq!(a.label(), "Cloud provider (bedrock)");
        assert_eq!(a.phrase(), "a cloud provider (bedrock)");
        // Sentences: "signed in with …".
        assert_eq!(
            parse_auth(SIGNED_IN).unwrap().phrase(),
            "your Claude Max plan"
        );
        assert_eq!(
            parse_auth(ENV_KEY).unwrap().phrase(),
            "an API key (ANTHROPIC_API_KEY)"
        );
        assert_eq!(parse_auth(API_LOGIN).unwrap().phrase(), "an API key");
        // Noise before the JSON (a warning line) is skipped.
        assert!(parse_auth(&format!("warning: x\n{SIGNED_OUT}")).is_some());
        assert!(parse_auth("not json").is_none());
    }

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
    }

    #[test]
    fn finding_claude_code() {
        let root = std::env::temp_dir().join(format!("hadion-cli-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let npm = root.join("npm");
        let bin = root.join("bin");
        let path = std::env::join_paths([bin.clone(), npm.clone()]).unwrap();
        // Nothing installed.
        assert_eq!(
            find_with(None, Some(home.clone()), Some(path.clone())),
            None
        );
        // An npm shim: the exe it starts, not the shim.
        touch(&npm.join("claude.cmd"));
        let exe = npm
            .join("node_modules")
            .join("@anthropic-ai")
            .join("claude-code")
            .join("bin")
            .join(exe_name());
        touch(&exe);
        let i = find_with(None, Some(home.clone()), Some(path.clone())).unwrap();
        assert_eq!(i.program, exe);
        assert_eq!(i.how, "npm (global install)");
        assert_eq!(i.shim, Some(npm.join("claude.cmd")));
        // A claude.exe on PATH wins over the shim.
        touch(&bin.join(exe_name()));
        let i = find_with(None, Some(home.clone()), Some(path.clone())).unwrap();
        assert_eq!(i.program, bin.join(exe_name()));
        assert_eq!(i.how, "PATH");
        // The native installer wins over PATH.
        touch(&home.join(".local").join("bin").join(exe_name()));
        let i = find_with(None, Some(home.clone()), Some(path.clone())).unwrap();
        assert_eq!(i.how, "the native installer (~/.local/bin)");
        // HADION_CLAUDE wins over everything; a missing one means "not installed".
        let fake = root.join("fake").join(exe_name());
        touch(&fake);
        let i = find_with(
            Some(fake.clone().into()),
            Some(home.clone()),
            Some(path.clone()),
        )
        .unwrap();
        assert_eq!(i.program, fake);
        assert_eq!(
            find_with(Some(root.join("missing").into()), Some(home), Some(path)),
            None
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn arguments_never_carry_the_material() {
        let a = args(
            CliModel::Opus,
            Effort::XHigh,
            "You write commit messages.\nTwo lines.",
        );
        let joined = a.join(" ");
        for flag in [
            "-p",
            "--safe-mode",
            "--disable-slash-commands",
            "--strict-mcp-config",
            "--no-session-persistence",
            "--include-partial-messages",
            "--verbose",
        ] {
            assert!(a.iter().any(|x| x == flag), "{flag}");
        }
        let after = |f: &str| a[a.iter().position(|x| x == f).unwrap() + 1].clone();
        assert_eq!(after("--output-format"), "stream-json");
        assert_eq!(after("--tools"), "");
        assert_eq!(after("--disallowedTools"), "*");
        assert_eq!(after("--mcp-config"), NO_MCP);
        assert_eq!(after("--model"), "opus");
        assert_eq!(after("--effort"), "xhigh");
        assert_eq!(after("--max-turns"), "1");
        assert_eq!(
            after("--system-prompt"),
            "You write commit messages.\nTwo lines."
        );
        assert!(!a.iter().any(|x| x == "--bare"));
        assert!(!joined.contains("diff --git"));
        // Claude Code's default model: no --model at all.
        let a = args(CliModel::Default, Effort::Low, "s");
        assert!(!a.iter().any(|x| x == "--model"));
    }

    #[test]
    fn display_hides_the_system_prompt() {
        let install = Install {
            program: PathBuf::from("C:/x/claude.exe"),
            prefix: Vec::new(),
            how: "PATH",
            shim: None,
        };
        let d = display(
            &install,
            &args(CliModel::Sonnet, Effort::Low, "secret\nprompt"),
        );
        assert_eq!(d[0], "C:/x/claude.exe");
        assert!(
            d.iter()
                .any(|l| l.contains("<the system prompt below: 13 characters>"))
        );
        assert!(!d.iter().any(|l| l.contains("secret")));
        assert!(d.iter().any(|l| l == "  --tools \"\""));
        assert!(d.iter().any(|l| l == "  --disallowedTools \"*\""));
        assert!(d.iter().any(|l| l == "  --model sonnet"));
        assert!(d.iter().any(|l| l == "  --effort low"));
        assert!(d.iter().any(|l| l == "  -p"));
    }

    #[test]
    fn environment_is_cleaned() {
        let p = env_plan([
            "PATH",
            "USERPROFILE",
            "HOME",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_BASE_URL",
            "anthropic_auth_token",
            "CLAUDE_CONFIG_DIR",
            "CLAUDECODE",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_EFFORT",
            "CLAUDE_CODE_DISABLE_CLAUDE_MDS",
        ]);
        for k in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_BASE_URL",
            "anthropic_auth_token",
            "CLAUDECODE",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_EFFORT",
        ] {
            assert!(p.remove.iter().any(|r| r == k), "{k} must be removed");
        }
        for k in ["PATH", "USERPROFILE", "HOME", "CLAUDE_CONFIG_DIR"] {
            assert!(!p.remove.iter().any(|r| r == k), "{k} must stay");
        }
        // Set again rather than removed.
        assert!(
            !p.remove
                .iter()
                .any(|r| r == "CLAUDE_CODE_DISABLE_CLAUDE_MDS")
        );
        for (k, v) in SET_ENV {
            assert!(p.set.iter().any(|(a, b)| a == k && b == v));
        }
    }

    #[test]
    fn stdin_is_capped() {
        assert_eq!(stdin_payload("abc"), b"abc");
        let big = "é".repeat(MAX_STDIN / 2 + 10);
        let out = stdin_payload(&big);
        assert!(out.len() <= MAX_STDIN);
        let s = String::from_utf8(out).unwrap();
        assert!(s.ends_with("reads at most 10 MB]\n"));
    }

    // Recorded from Claude Code 2.1.281 (`-p --output-format stream-json
    // --verbose --include-partial-messages`, prompt "Reply with the single
    // word OK."), long fields shortened.
    const OK_STREAM: &str = r#"{"type":"system","subtype":"init","cwd":"C:\\Temp\\hadion-claude-code","session_id":"s1","tools":[],"mcp_servers":[],"model":"claude-haiku-4-5-20251001","permissionMode":"default","slash_commands":[],"apiKeySource":"none","claude_code_version":"2.1.281","output_style":"default","skills":[]}
{"type":"system","subtype":"status","status":"requesting","session_id":"s1"}
{"type":"stream_event","event":{"type":"message_start","message":{"model":"claude-haiku-4-5-20251001","id":"msg_1","type":"message","role":"assistant","content":[],"stop_reason":null,"usage":{"input_tokens":425,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":3}}},"session_id":"s1"}
{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}},"session_id":"s1"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc"}},"session_id":"s1"}
{"type":"assistant","message":{"model":"claude-haiku-4-5-20251001","id":"msg_1","content":[{"type":"thinking","thinking":""}]},"session_id":"s1"}
{"type":"stream_event","event":{"type":"content_block_stop","index":0},"session_id":"s1"}
{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}},"session_id":"s1"}
{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"OK"}},"session_id":"s1"}
{"type":"assistant","message":{"model":"claude-haiku-4-5-20251001","id":"msg_1","content":[{"type":"text","text":"OK"}]},"session_id":"s1","request_id":"req_1"}
{"type":"stream_event","event":{"type":"content_block_stop","index":1},"session_id":"s1"}
{"type":"stream_event","event":{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"input_tokens":425,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":56}},"session_id":"s1"}
{"type":"stream_event","event":{"type":"message_stop"},"session_id":"s1"}
{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","resetsAt":1793772000,"rateLimitType":"seven_day","utilization":0.27,"isUsingOverage":false,"unifiedWindows":{"five_hour":{"utilization":0.17,"resetsAt":1793235000},"seven_day":{"utilization":0.27,"resetsAt":1793772000}}},"session_id":"s1"}
{"type":"result","subtype":"success","is_error":false,"duration_ms":1365,"duration_api_ms":1308,"num_turns":1,"result":"OK","stop_reason":"end_turn","session_id":"s1","total_cost_usd":0.000705,"usage":{"input_tokens":425,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":56},"terminal_reason":"completed","api_error_status":null}
"#;

    fn run(text: &str) -> (Stream, Answer, Vec<Step>) {
        let mut s = Stream::default();
        let mut a = Answer::default();
        let steps = text.lines().map(|l| s.line(&mut a, l)).collect();
        (s, a, steps)
    }

    #[test]
    fn a_recorded_answer() {
        let (s, a, steps) = run(OK_STREAM);
        assert!(steps.iter().all(|x| *x == Step::Continue));
        // Text from the stream events only: the assistant message repeats it.
        assert_eq!(a.text, "OK");
        assert!(a.complete);
        assert_eq!(a.model, "claude-haiku-4-5-20251001");
        assert_eq!(a.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(a.usage.input_tokens, 425);
        assert_eq!(a.usage.output_tokens, 56);
        let init = s.init.clone().unwrap();
        assert!(init.tools.is_empty() && init.mcp_servers.is_empty());
        assert_eq!(init.version, "2.1.281");
        assert_eq!(init.api_key_source, "none");
        let o = s.outcome.clone().unwrap();
        assert_eq!(o.cost_usd, Some(0.000705));
        assert_eq!(o.duration_api_ms, Some(1308));
        let plan = s.plan.clone().unwrap();
        assert_eq!(plan.five_hour.map(|w| w.0), Some(0.17));
        assert_eq!(plan.seven_day, Some((0.27, Some(1793772000))));
        assert_eq!(plan.kind, "seven_day");
        assert!(s.failure().is_none());
        assert!(s.api_error.is_none());
    }

    // Recorded with an empty CLAUDE_CONFIG_DIR (signed out).
    const SIGNED_OUT_STREAM: &str = r#"{"type":"system","subtype":"init","session_id":"s2","tools":[],"mcp_servers":[],"model":"claude-haiku-4-5-20251001","apiKeySource":"none","claude_code_version":"2.1.281"}
{"type":"system","subtype":"status","status":"requesting","session_id":"s2"}
{"type":"assistant","message":{"id":"m","model":"<synthetic>","role":"assistant","stop_reason":"stop_sequence","content":[{"type":"text","text":"Not logged in · Please run /login"}]},"session_id":"s2","error":"authentication_failed","is_api_error_message":true}
{"type":"result","subtype":"success","is_error":true,"duration_ms":31,"duration_api_ms":0,"num_turns":1,"result":"Not logged in · Please run /login","stop_reason":"stop_sequence","total_cost_usd":0,"terminal_reason":"api_error","api_error_status":null}
"#;

    // Recorded with `--model claude-hadion-no-such-model`.
    const BAD_MODEL_STREAM: &str = r#"{"type":"system","subtype":"init","tools":[],"mcp_servers":[],"model":"claude-hadion-no-such-model","claude_code_version":"2.1.281"}
{"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"There's an issue with the selected model (claude-hadion-no-such-model). It may not exist or you may not have access to it. Run --model to pick a different model."}]},"error":"model_not_found","request_id":"req_9","is_api_error_message":true}
{"type":"result","subtype":"success","is_error":true,"result":"There's an issue with the selected model (claude-hadion-no-such-model). It may not exist or you may not have access to it. Run --model to pick a different model.","stop_reason":"stop_sequence","total_cost_usd":0,"terminal_reason":"api_error","api_error_status":404}
"#;

    #[test]
    fn recorded_errors() {
        let (s, a, _) = run(SIGNED_OUT_STREAM);
        // The synthetic message's text never becomes an answer.
        assert!(a.text.is_empty());
        assert!(!a.complete);
        let e = s.failure().unwrap();
        assert_eq!(e.kind, CliErrorKind::NotSignedIn);
        assert_eq!(e.code.as_deref(), Some("authentication_failed"));
        assert!(e.kind.needs_sign_in());
        assert!(e.summary().starts_with("Not logged in · Please run /login"));
        let (s, _, _) = run(BAD_MODEL_STREAM);
        let e = s.failure().unwrap();
        assert_eq!(e.kind, CliErrorKind::ModelNotFound);
        assert_eq!(e.status, Some(404));
        assert_eq!(e.request_id.as_deref(), Some("req_9"));
        assert!(
            e.summary()
                .contains("model_not_found, HTTP 404, request req_9")
        );
    }

    #[test]
    fn tools_retries_refusals_and_turn_limits() {
        let (_, _, steps) =
            run(r#"{"type":"system","subtype":"init","tools":["Bash","Read"],"mcp_servers":[]}"#);
        assert_eq!(
            steps,
            vec![Step::ToolsEnabled(vec!["Bash".into(), "Read".into()])]
        );
        let (s, _, _) = run(
            r#"{"type":"system","subtype":"api_retry","attempt":2,"max_retries":10,"retry_delay_ms":4200,"error_status":429,"error":"rate_limit"}"#,
        );
        assert_eq!(
            s.retry.clone().unwrap().note(),
            "Retrying after a rate limit (HTTP 429): attempt 2 of 10, in 5 s"
        );
        // A refusal is an answer with stop_reason "refusal".
        let refusal = r#"{"type":"stream_event","event":{"type":"message_start","message":{"model":"claude-opus-5-5","usage":{"input_tokens":9}}}}
{"type":"stream_event","event":{"type":"message_delta","delta":{"stop_reason":"refusal","stop_details":{"category":"cyber"}},"usage":{"output_tokens":0}}}
{"type":"result","subtype":"success","is_error":false,"result":"","stop_reason":"refusal"}"#;
        let (s, a, _) = run(refusal);
        assert!(a.refused() && a.complete);
        assert_eq!(a.refusal_category.as_deref(), Some("cyber"));
        assert!(s.failure().is_none());
        let max = r#"{"type":"result","subtype":"success","is_error":false,"result":"partial","stop_reason":"max_tokens"}"#;
        let (_, a, _) = run(max);
        assert!(a.hit_max_tokens());
        assert_eq!(a.text, "partial");
        let turns = r#"{"type":"result","subtype":"error_max_turns","is_error":true,"errors":["Reached maximum number of turns (1)"]}"#;
        let e = run(turns).0.failure().unwrap();
        assert_eq!(e.message, "Reached maximum number of turns (1)");
        // Unreadable lines are counted, not fatal.
        let (s, _, _) = run("not json\n{\"type\":\"user\"}");
        assert_eq!((s.lines, s.unreadable), (2, 1));
    }

    #[test]
    fn known_error_texts() {
        for (text, kind) in [
            (
                "Not logged in · Please run /login",
                CliErrorKind::NotSignedIn,
            ),
            (
                "Login expired · Please run /login",
                CliErrorKind::LoginExpired,
            ),
            (
                "Invalid API key · Fix external API key",
                CliErrorKind::InvalidApiKey,
            ),
            (
                "You've hit your session limit · resets 5pm",
                CliErrorKind::SessionLimit,
            ),
            (
                "You've hit your weekly limit · resets Mon",
                CliErrorKind::WeeklyLimit,
            ),
            ("Credit balance is too low", CliErrorKind::CreditTooLow),
            (
                "Server is temporarily limiting requests (not your usage limit)",
                CliErrorKind::Throttled,
            ),
            ("error: unknown option '--safe-mode'", CliErrorKind::TooOld),
        ] {
            assert_eq!(classify(None, text), kind, "{text}");
            assert!(!kind.title().is_empty());
        }
        assert_eq!(classify(Some("overloaded"), "x"), CliErrorKind::Overloaded);
        assert_eq!(classify(Some("rate_limit"), "x"), CliErrorKind::RateLimited);
        assert_eq!(classify(Some("billing_error"), "x"), CliErrorKind::Billing);
        assert_eq!(classify(None, "something else"), CliErrorKind::Other);
        assert!(CliErrorKind::Overloaded.retryable());
        assert!(!CliErrorKind::NotSignedIn.retryable());
        assert!(
            CliErrorKind::SessionLimit
                .next_step()
                .contains("switch to the Claude API")
        );
    }
}
