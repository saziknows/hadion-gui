//! One call to a forge CLI (`glab api …`, `az …`) or, for public GitLab
//! projects without a sign-in, `curl` — described without running it so
//! writes can be unit tested, then run with every call recorded like a
//! GitHub API call (command, bytes, latency, error, the body it sent).

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::devops::proc::{ProcSpec, render_args, run_capture};
use crate::github::ApiCall;
use crate::{Error, Result};

/// Which program runs the call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
    Glab,
    Az,
    Curl,
}

impl Program {
    pub fn name(self) -> &'static str {
        match self {
            Program::Glab => "glab",
            Program::Az => "az",
            Program::Curl => "curl",
        }
    }
}

/// A described call.
#[derive(Clone, PartialEq)]
pub struct Call {
    pub program: Program,
    pub args: Vec<String>,
    /// JSON body sent on standard input (never on the command line).
    pub body: Option<Value>,
    /// What confirmations and the activity log show of the body: the body
    /// itself, or a copy with secret values masked.
    pub shown: Option<Value>,
    /// Changes something on the forge.
    pub write: bool,
}

/// Debug output shows the masked body only, so a secret never reaches a
/// debug print.
impl std::fmt::Debug for Call {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Call")
            .field("command", &self.display())
            .field("body", &self.shown)
            .field("write", &self.write)
            .finish()
    }
}

impl Call {
    pub fn new(program: Program, args: Vec<String>) -> Self {
        Self {
            program,
            args,
            body: None,
            shown: None,
            write: false,
        }
    }

    pub fn body(mut self, body: Value) -> Self {
        self.shown = Some(body.clone());
        self.body = Some(body);
        self
    }

    /// Show `shown` instead of the body (secret values masked).
    pub fn shown(mut self, shown: Value) -> Self {
        self.shown = Some(shown);
        self
    }

    pub fn write(mut self) -> Self {
        self.write = true;
        self
    }

    /// The command line as typed, with a note when a body goes on
    /// standard input.
    pub fn display(&self) -> String {
        let args: Vec<&str> = self.args.iter().map(String::as_str).collect();
        let mut s = format!("{} {}", self.program.name(), render_args(&args));
        if self.body.is_some() {
            s.push_str(if self.shown != self.body {
                " (JSON body on standard input, secret values masked)"
            } else {
                " (JSON body on standard input)"
            });
        }
        s
    }

    /// The body as pretty JSON for confirmations and the log.
    pub fn body_preview(&self) -> Option<String> {
        self.shown
            .as_ref()
            .and_then(|v| serde_json::to_string_pretty(v).ok())
    }

    /// `display()` then the body, for a confirmation's detail block.
    pub fn details(&self) -> Vec<String> {
        let mut out = vec![self.display()];
        if let Some(b) = self.body_preview() {
            out.push(b);
        }
        out
    }
}

/// Mask a value for display.
pub const MASK: &str = "•••";

/// The first useful line of a failed call: stderr, else stdout.
fn error_text(
    stderr: &str,
    stdout: &[u8],
    code: Option<i32>,
    timed_out: bool,
    elapsed: u64,
) -> String {
    if timed_out {
        return format!("Timed out after {elapsed} ms");
    }
    let e = stderr.trim();
    let o = String::from_utf8_lossy(stdout).trim().to_owned();
    // glab prints the API's JSON error to stdout and a summary to stderr.
    let msg = match (e.is_empty(), o.is_empty()) {
        (false, false) if o.starts_with('{') => format!("{e}\n{}", api_message(&o).unwrap_or(o)),
        (false, _) => e.to_owned(),
        (true, false) => api_message(&o).unwrap_or(o),
        _ => format!("exited with {code:?}"),
    };
    msg
}

/// `message` / `error` of a JSON error body.
pub fn api_message(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    let m = v.get("message").or_else(|| v.get("error"))?;
    Some(match m {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

/// Run a call in `dir` and return its standard output. Recorded in `calls`
/// with the shown body as its input.
pub fn run(
    call: &Call,
    dir: &Path,
    timeout: Duration,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<u8>> {
    let mut spec = ProcSpec::new(call.program.name(), dir).args(call.args.iter().cloned());
    if call.program == Program::Az {
        // Keep az from asking questions or printing upgrade notes.
        spec = spec
            .env("AZURE_CORE_ONLY_SHOW_ERRORS", "true")
            .env("AZURE_CORE_NO_COLOR", "true")
            .env("AZURE_EXTENSION_USE_DYNAMIC_INSTALL", "no");
    }
    if let Some(b) = &call.body {
        let bytes = serde_json::to_vec(b).map_err(|e| Error::new(e.to_string()))?;
        spec = spec.stdin(bytes);
    }
    let command = call.display();
    let input = call.body_preview();
    match run_capture(&spec, Some(timeout)) {
        Ok(c) if c.success() => {
            calls.push(ApiCall {
                command,
                elapsed_ms: c.elapsed_ms,
                bytes: c.stdout.len(),
                rate: None,
                error: None,
                input,
            });
            Ok(c.stdout)
        }
        Ok(c) => {
            let msg = error_text(&c.stderr, &c.stdout, c.code, c.timed_out, c.elapsed_ms);
            calls.push(ApiCall {
                command,
                elapsed_ms: c.elapsed_ms,
                bytes: c.stdout.len(),
                rate: None,
                error: Some(msg.clone()),
                input,
            });
            Err(Error::new(msg))
        }
        Err(e) => {
            let msg = e.to_string();
            calls.push(ApiCall {
                command,
                elapsed_ms: 0,
                bytes: 0,
                rate: None,
                error: Some(msg.clone()),
                input,
            });
            Err(Error::new(msg))
        }
    }
}

/// An unauthenticated GET through curl. The HTTP status is appended by
/// curl (`-w`) and checked here.
pub fn curl_get(url: &str) -> Call {
    Call::new(
        Program::Curl,
        vec![
            "-sS".into(),
            "-L".into(),
            "--max-time".into(),
            "40".into(),
            "-H".into(),
            "Accept: application/json".into(),
            "-w".into(),
            "\n%{http_code}".into(),
            url.into(),
        ],
    )
}

/// Run a [`curl_get`] call: the body on HTTP 2xx, the API's message
/// otherwise.
pub fn run_curl(call: &Call, dir: &Path, calls: &mut Vec<ApiCall>) -> Result<Vec<u8>> {
    let mut out = run(call, dir, Duration::from_secs(50), calls)?;
    let cut = out.iter().rposition(|b| *b == b'\n').unwrap_or(0);
    let code: u32 = String::from_utf8_lossy(&out[cut..])
        .trim()
        .parse()
        .unwrap_or(0);
    out.truncate(cut);
    if let Some(last) = calls.last_mut() {
        last.bytes = out.len();
    }
    if (200..300).contains(&code) {
        return Ok(out);
    }
    let body = String::from_utf8_lossy(&out).trim().to_owned();
    let msg = format!(
        "{} (HTTP {code})",
        api_message(&body).unwrap_or_else(|| if body.is_empty() {
            "no answer".to_owned()
        } else {
            body.chars().take(300).collect()
        })
    );
    if let Some(last) = calls.last_mut() {
        last.error = Some(msg.clone());
    }
    Err(Error::new(msg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn display_and_preview() {
        let c = Call::new(Program::Glab, vec!["api".into(), "user".into()]);
        assert_eq!(c.display(), "glab api user");
        assert_eq!(c.body_preview(), None);
        assert!(!c.write);
        let w = Call::new(
            Program::Glab,
            vec!["api".into(), "--method".into(), "POST".into(), "x".into()],
        )
        .body(json!({"a": 1}))
        .write();
        assert!(w.display().ends_with("(JSON body on standard input)"));
        assert!(w.body_preview().unwrap().contains("\"a\": 1"));
        assert_eq!(w.details().len(), 2);
        let m = w.clone().shown(json!({"a": MASK}));
        assert!(m.display().contains("secret values masked"));
        assert!(!m.body_preview().unwrap().contains('1'));
        assert!(!format!("{m:?}").contains(": 1"), "{m:?}");
        let a = Call::new(
            Program::Az,
            vec![
                "repos".into(),
                "pr".into(),
                "list".into(),
                "--org".into(),
                "https://dev.azure.com/acme".into(),
                "--project".into(),
                "Web Shop".into(),
            ],
        );
        assert_eq!(
            a.display(),
            "az repos pr list --org https://dev.azure.com/acme --project \"Web Shop\""
        );
    }

    #[test]
    fn errors() {
        assert_eq!(
            api_message(r#"{"message":"401 Unauthorized"}"#).as_deref(),
            Some("401 Unauthorized")
        );
        assert_eq!(
            api_message(r#"{"message":{"title":["is too short"]}}"#).as_deref(),
            Some(r#"{"title":["is too short"]}"#)
        );
        assert_eq!(
            error_text(
                "glab: 404 Not Found (HTTP 404)",
                br#"{"message":"404 Project Not Found"}"#,
                Some(1),
                false,
                5
            ),
            "glab: 404 Not Found (HTTP 404)\n404 Project Not Found"
        );
        assert_eq!(
            error_text("", b"", Some(2), false, 5),
            "exited with Some(2)"
        );
        assert_eq!(
            error_text("x", b"", None, true, 900),
            "Timed out after 900 ms"
        );
    }

    #[test]
    fn curl_call() {
        let c = curl_get("https://gitlab.com/api/v4/projects/a%2Fb/pipelines");
        assert_eq!(c.program, Program::Curl);
        assert_eq!(
            c.args.last().unwrap(),
            "https://gitlab.com/api/v4/projects/a%2Fb/pipelines"
        );
        assert!(c.args.contains(&"\n%{http_code}".to_owned()));
        assert!(!c.write);
    }

    #[test]
    fn a_missing_program_is_recorded() {
        let mut calls = Vec::new();
        let c = Call::new(Program::Glab, vec!["--version".into()]);
        // glab is usually not installed on the test machine; either way the
        // call is recorded.
        let _ = run(&c, Path::new("."), Duration::from_secs(20), &mut calls);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].command, "glab --version");
    }
}
