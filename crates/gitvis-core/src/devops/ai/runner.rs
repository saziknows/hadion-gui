//! Sending a request with `curl` and following the stream.
//!
//! The request body goes to a temporary file (deleted when the run ends,
//! whatever the outcome); the key and the headers go to curl's standard
//! input. The answer is folded into a shared [`Live`] as lines arrive, so
//! the UI can show the text while it streams.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::cli::{CliError, CliRun};
use super::model::{Effort, Model};
use super::request::{self, Prompt};
use super::sse::{self, Answer, ApiError, SseParser};
use crate::devops::keychain;
use crate::devops::proc::{self, ProcSpec, Stream};

/// The keychain entry of the key.
pub const KEY_NAME: &str = "anthropic-api-key";
/// The environment variable used when no key is stored.
pub const ENV_KEY: &str = "ANTHROPIC_API_KEY";
/// Overrides [`KEY_NAME`] for tests; only names starting with
/// `gitvis-test-` are accepted, so a test never touches the real entry.
pub const KEY_NAME_OVERRIDE: &str = "GITVIS_AI_KEYCHAIN_NAME";

/// The keychain entry name in use.
pub fn key_name() -> String {
    match std::env::var(KEY_NAME_OVERRIDE) {
        Ok(n) if n.starts_with("gitvis-test-") => n,
        _ => KEY_NAME.to_owned(),
    }
}

/// Where the key came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    Keychain,
    Env,
}

impl KeySource {
    pub fn label(self) -> &'static str {
        match self {
            KeySource::Keychain => "the OS keychain",
            KeySource::Env => "the ANTHROPIC_API_KEY environment variable",
        }
    }
}

/// What is known about the key, without the key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyStatus {
    /// `…abcd` of the stored key.
    pub keychain: Option<String>,
    /// `…abcd` of `ANTHROPIC_API_KEY`.
    pub env: Option<String>,
    /// The keychain could not be read.
    pub keychain_error: Option<String>,
}

impl KeyStatus {
    /// The stored key wins over the environment.
    pub fn source(&self) -> Option<KeySource> {
        if self.keychain.is_some() {
            Some(KeySource::Keychain)
        } else if self.env.is_some() {
            Some(KeySource::Env)
        } else {
            None
        }
    }

    pub fn hint(&self) -> Option<&str> {
        self.keychain.as_deref().or(self.env.as_deref())
    }
}

fn env_key() -> Option<String> {
    std::env::var(ENV_KEY)
        .ok()
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty())
}

/// Look up the key (keychain first, then the environment). Call from a
/// background task.
pub fn key_status() -> KeyStatus {
    key_status_in(&key_name())
}

fn key_status_in(name: &str) -> KeyStatus {
    let (keychain, keychain_error) = match keychain::get(name) {
        Ok(Some(k)) if !k.trim().is_empty() => (Some(keychain::hint(k.trim())), None),
        Ok(_) => (None, None),
        Err(e) => (None, Some(e.to_string())),
    };
    KeyStatus {
        keychain,
        env: env_key().map(|k| keychain::hint(&k)),
        keychain_error,
    }
}

/// The key itself, for the request about to be sent.
fn load_key() -> Option<(String, KeySource)> {
    if let Ok(Some(k)) = keychain::get(&key_name()) {
        let k = k.trim().to_owned();
        if !k.is_empty() {
            return Some((k, KeySource::Keychain));
        }
    }
    env_key().map(|k| (k, KeySource::Env))
}

/// A pasted key, trimmed; refused when empty or with inner whitespace.
pub fn clean_key(key: &str) -> Result<&str, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("The key is empty".to_owned());
    }
    if key.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("The key contains spaces or line breaks; paste only the key".to_owned());
    }
    Ok(key)
}

/// Store a key (trimmed). Call from a background task.
pub fn store_key(key: &str) -> Result<KeyStatus, String> {
    store_key_in(&key_name(), key)
}

fn store_key_in(name: &str, key: &str) -> Result<KeyStatus, String> {
    let key = clean_key(key)?;
    keychain::set(name, key).map_err(|e| e.to_string())?;
    Ok(key_status_in(name))
}

/// Delete the stored key. Call from a background task.
pub fn remove_key() -> Result<KeyStatus, String> {
    remove_key_in(&key_name())
}

fn remove_key_in(name: &str) -> Result<KeyStatus, String> {
    keychain::delete(name).map_err(|e| e.to_string())?;
    Ok(key_status_in(name))
}

/// Is curl on PATH?
pub fn curl_path() -> Option<PathBuf> {
    proc::resolve("curl")
}

pub const CURL_HINT: &str = "curl ships with Windows 10 and 11 (C:\\Windows\\System32\\curl.exe) and with Git for Windows; otherwise: winget install cURL.cURL";

/// The answer as it streams, shared with the UI.
#[derive(Debug)]
pub struct Live {
    pub answer: Answer,
    pub started: Instant,
    pub first_text: Option<Instant>,
    pub bytes: usize,
    /// Response lines that were not SSE (an HTTP error body).
    pub other: Vec<String>,
    /// What is happening besides the text: Claude Code retrying after a
    /// rate limit, for instance.
    pub note: Option<String>,
}

impl Default for Live {
    fn default() -> Self {
        Self {
            answer: Answer::default(),
            started: Instant::now(),
            first_text: None,
            bytes: 0,
            other: Vec::new(),
            note: None,
        }
    }
}

pub type Shared = Arc<Mutex<Live>>;

/// Why a request produced no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// No stored key and no `ANTHROPIC_API_KEY`.
    NoKey,
    /// curl is not on PATH.
    NoCurl,
    /// Stopped by the user.
    Cancelled,
    /// Gathering the input failed (a git command, the GitHub API).
    Prepare(String),
    /// curl failed before an HTTP answer (DNS, TLS, timeout).
    Transport(String),
    /// The API answered with an error.
    Api(ApiError),
    /// The stream ended before `message_stop`.
    Incomplete(String),
    /// Claude Code was asked for but is not installed.
    NoClaudeCode,
    /// Claude Code answered with an error (not signed in, a plan limit, …).
    ClaudeCode(CliError),
    /// "Auto": neither Claude Code with a plan nor an API key is ready.
    NoProvider(String),
}

impl Failure {
    pub fn message(&self) -> String {
        match self {
            Failure::NoKey => "No Claude API key: store one in Settings → AI assist, or set ANTHROPIC_API_KEY before starting gitvis.".to_owned(),
            Failure::NoCurl => format!("curl was not found on PATH. {CURL_HINT}"),
            Failure::Cancelled => "Stopped.".to_owned(),
            Failure::Prepare(e) => e.clone(),
            Failure::Transport(e) => format!("curl: {e}"),
            Failure::Api(e) => {
                let mut s = e.summary();
                if let Some(id) = &e.request_id {
                    s.push_str(&format!(" (request {id})"));
                }
                s
            }
            Failure::Incomplete(e) => e.clone(),
            Failure::NoClaudeCode => format!(
                "Claude Code is not installed (or not on PATH). Install it with one of: {}",
                super::cli::INSTALL_HINTS
                    .iter()
                    .map(|(_, c)| *c)
                    .collect::<Vec<_>>()
                    .join("  ·  ")
            ),
            Failure::ClaudeCode(e) => e.summary(),
            Failure::NoProvider(why) => why.clone(),
        }
    }

    /// Nothing was sent (a missing key or tool, a failed git read).
    pub fn before_sending(&self) -> bool {
        matches!(
            self,
            Failure::NoKey
                | Failure::NoCurl
                | Failure::Prepare(_)
                | Failure::NoClaudeCode
                | Failure::NoProvider(_)
        )
    }
}

/// A request body file that is removed when dropped.
pub struct TempBody {
    pub path: PathBuf,
}

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

impl TempBody {
    pub fn create(dir: &Path, body: &[u8]) -> std::io::Result<Self> {
        sweep_stale(
            dir,
            std::time::Duration::from_secs(u64::from(request::MAX_TIME_SECS) + 60),
        );
        let n = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(
            "gitvis-ai-{}-{}-{n}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or_default()
        ));
        std::fs::write(&path, body)?;
        Ok(Self { path })
    }
}

impl Drop for TempBody {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Remove request files older than `age`: left behind when gitvis quit
/// while a request was running (no request lives longer than curl's
/// `max-time`). Returns how many were removed.
pub fn sweep_stale(dir: &Path, age: std::time::Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !(name.starts_with("gitvis-ai-") && name.ends_with(".json")) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|el| el > age);
        if old && std::fs::remove_file(e.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Which way a request went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// The Claude API with a key from here.
    Api(KeySource),
    /// The owner's Claude Code (their subscription, unless it is signed in
    /// with an API key).
    ClaudeCode,
}

impl Via {
    pub fn is_api(self) -> bool {
        matches!(self, Via::Api(_))
    }
}

/// A finished request.
#[derive(Clone, Debug)]
pub struct Done {
    pub answer: Answer,
    pub elapsed_ms: u64,
    pub via: Via,
    pub request_bytes: usize,
    /// What Claude Code reported besides the answer.
    pub cli: Option<CliRun>,
}

/// Send `prompt` and follow the stream into `live`. Blocks until the
/// answer is complete, the request failed or `cancel` was set.
pub fn send(
    dir: &Path,
    model: Model,
    effort: Effort,
    prompt: &Prompt,
    cancel: &AtomicBool,
    live: &Shared,
) -> Result<Done, Failure> {
    if curl_path().is_none() {
        return Err(Failure::NoCurl);
    }
    let Some((key, source)) = load_key() else {
        return Err(Failure::NoKey);
    };
    let call = Call {
        program: "curl",
        dir,
        key: &key,
        source,
        model,
        effort,
        temp_dir: None,
    };
    send_with(&call, prompt, cancel, live)
}

/// One request: which curl, where, with which key and model.
pub struct Call<'a> {
    /// `curl`, or a test double.
    pub program: &'a str,
    pub dir: &'a Path,
    pub key: &'a str,
    pub source: KeySource,
    pub model: Model,
    pub effort: Effort,
    /// Where the request file goes (the system temp dir when `None`).
    pub temp_dir: Option<&'a Path>,
}

/// [`send`] with the program and key given.
pub fn send_with(
    call: &Call,
    prompt: &Prompt,
    cancel: &AtomicBool,
    live: &Shared,
) -> Result<Done, Failure> {
    let started = Instant::now();
    let (dir, model, effort, source) = (call.dir, call.model, call.effort, call.source);
    if proc::resolve(call.program).is_none() {
        return Err(Failure::NoCurl);
    }
    let key = call.key;
    let body = serde_json::to_vec(&request::body(model, effort, prompt))
        .map_err(|e| Failure::Prepare(format!("Could not encode the request: {e}")))?;
    let temp = call
        .temp_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let file = TempBody::create(&temp, &body)
        .map_err(|e| Failure::Prepare(format!("Could not write the request file: {e}")))?;
    let config = request::curl_config(
        key,
        &file.path.to_string_lossy(),
        model.supports_fallbacks(),
    )
    .map_err(Failure::Prepare)?;
    let spec = ProcSpec::new(call.program, dir)
        .args(request::curl_args())
        .stdin(config.into_bytes());
    let mut parser = SseParser::default();
    let mut stderr = String::new();
    let exit = proc::run_streaming(
        &spec,
        cancel,
        |_| {},
        |lines| {
            let mut l = live.lock().unwrap_or_else(|e| e.into_inner());
            for line in lines {
                match line.stream {
                    Stream::Stderr => {
                        stderr.push_str(&line.text);
                        stderr.push('\n');
                    }
                    Stream::Stdout => {
                        l.bytes += line.text.len() + 1;
                        let is_sse = line.text.is_empty()
                            || line.text.starts_with("event:")
                            || line.text.starts_with("data:")
                            || line.text.starts_with(':');
                        if !is_sse {
                            l.other.push(line.text.clone());
                        }
                        if let Some(ev) = parser.push_line(&line.text) {
                            let before = l.answer.text.len();
                            l.answer.apply(&ev);
                            if l.first_text.is_none() && l.answer.text.len() > before {
                                l.first_text = Some(Instant::now());
                            }
                        }
                    }
                }
            }
        },
    );
    drop(file); // removed in every outcome
    let mut l = live.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(ev) = parser.finish() {
        l.answer.apply(&ev);
    }
    let exit = exit.map_err(|e| Failure::Transport(e.to_string()))?;
    if exit.cancelled || cancel.load(Ordering::Relaxed) {
        return Err(Failure::Cancelled);
    }
    let meta = sse::parse_http_meta(&stderr, request::STATUS_MARKER);
    let status = meta.status.unwrap_or(0);
    if status >= 400 || (status == 0 && !l.answer.complete) {
        let body = l.other.join("\n");
        if let Some(mut e) = sse::parse_error_body(&body) {
            e.status = meta.status;
            e.retry_after = meta.retry_after;
            if e.request_id.is_none() {
                e.request_id = meta.request_id.clone();
            }
            return Err(Failure::Api(e));
        }
        if status >= 400 {
            return Err(Failure::Api(ApiError {
                kind: String::new(),
                message: first_line(&body)
                    .unwrap_or_else(|| format!("HTTP {status} without an error body")),
                request_id: meta.request_id,
                status: meta.status,
                retry_after: meta.retry_after,
            }));
        }
        let curl_error = stderr
            .lines()
            .filter(|l| !l.contains(request::STATUS_MARKER))
            .collect::<Vec<_>>()
            .join(" ");
        return Err(Failure::Transport(if curl_error.trim().is_empty() {
            format!("exited with {:?} before an answer arrived", exit.code)
        } else {
            curl_error.trim().to_owned()
        }));
    }
    if let Some(mut e) = l.answer.error.clone() {
        if e.request_id.is_none() {
            e.request_id = meta.request_id.clone();
        }
        return Err(Failure::Api(e));
    }
    if !l.answer.complete {
        return Err(Failure::Incomplete(format!(
            "The stream ended before the answer was complete ({} received).",
            super::super::plural_bytes(l.bytes)
        )));
    }
    Ok(Done {
        answer: l.answer.clone(),
        elapsed_ms: started.elapsed().as_millis() as u64,
        via: Via::Api(source),
        request_bytes: body.len(),
        cli: None,
    })
}

fn first_line(s: &str) -> Option<String> {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(300).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_files_are_removed_when_dropped() {
        let dir = std::env::temp_dir();
        let t = TempBody::create(&dir, b"{}").unwrap();
        let p = t.path.clone();
        assert!(p.exists());
        assert!(
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("gitvis-ai-")
        );
        drop(t);
        assert!(!p.exists());
    }

    #[test]
    fn stale_request_files_are_swept() {
        let dir = std::env::temp_dir().join(format!("gitvis-ai-sweep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("gitvis-ai-1-2-3.json"), "{}").unwrap();
        std::fs::write(dir.join("other.json"), "{}").unwrap();
        // Nothing is old enough yet.
        assert_eq!(sweep_stale(&dir, std::time::Duration::from_secs(3600)), 0);
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(sweep_stale(&dir, std::time::Duration::from_millis(1)), 1);
        assert!(!dir.join("gitvis-ai-1-2-3.json").exists());
        assert!(dir.join("other.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn key_status_prefers_the_keychain() {
        let s = KeyStatus {
            keychain: Some("…abcd".into()),
            env: Some("…wxyz".into()),
            keychain_error: None,
        };
        assert_eq!(s.source(), Some(KeySource::Keychain));
        assert_eq!(s.hint(), Some("…abcd"));
        let s = KeyStatus {
            keychain: None,
            env: Some("…wxyz".into()),
            keychain_error: None,
        };
        assert_eq!(s.source(), Some(KeySource::Env));
        assert_eq!(KeyStatus::default().source(), None);
    }

    #[test]
    fn pasted_keys_are_cleaned() {
        assert_eq!(
            clean_key(
                "  sk-ant-x 
"
            )
            .unwrap(),
            "sk-ant-x"
        );
        assert!(clean_key("   ").is_err());
        let e = clean_key("sk-ant one").unwrap_err();
        assert!(!e.contains("sk-ant"), "{e}");
    }

    #[test]
    #[ignore = "writes the OS keychain (a gitvis-test- entry, removed again); run by hand"]
    fn keychain_round_trip() {
        let name = "gitvis-test-anthropic-api-key";
        let s = store_key_in(name, " sk-ant-test-0000wxyz ").unwrap();
        assert_eq!(s.keychain.as_deref(), Some("…wxyz"));
        assert_eq!(s.source(), Some(KeySource::Keychain));
        assert_eq!(
            keychain::get(name).unwrap().as_deref(),
            Some("sk-ant-test-0000wxyz")
        );
        let s = remove_key_in(name).unwrap();
        assert_eq!(s.keychain, None);
        assert_eq!(keychain::get(name).unwrap(), None);
    }

    #[test]
    fn test_key_names_only() {
        // Not set in the test environment: the real name.
        if std::env::var(KEY_NAME_OVERRIDE).is_err() {
            assert_eq!(key_name(), KEY_NAME);
        }
    }

    #[test]
    fn failure_messages_never_carry_a_key() {
        let e = Failure::Api(ApiError {
            kind: "authentication_error".into(),
            message: "invalid x-api-key".into(),
            request_id: Some("req_1".into()),
            status: Some(401),
            retry_after: None,
        });
        assert_eq!(
            e.message(),
            "authentication_error (HTTP 401): invalid x-api-key (request req_1)"
        );
        assert!(Failure::NoKey.message().contains("ANTHROPIC_API_KEY"));
        assert!(
            Failure::NoCurl
                .message()
                .contains("winget install cURL.cURL")
        );
    }
}
