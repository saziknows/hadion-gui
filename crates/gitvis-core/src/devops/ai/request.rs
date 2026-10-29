//! The Messages API request and the `curl` invocation that sends it.
//!
//! The key never reaches a command line: curl reads its headers and the
//! path of the request body file from a config on standard input
//! (`--config -`), and the arguments are the same for every request.

use serde_json::{Value, json};

use super::model::{Effort, Model};

pub const API_URL: &str = "https://api.anthropic.com/v1/messages";
pub const API_VERSION: &str = "2026-10-21";
/// Beta header for `"fallbacks": "default"` (server-side refusal fallbacks).
pub const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Plenty for a commit message or an explanation; thinking counts too.
pub const MAX_TOKENS: u32 = 16_000;
/// The whole request may take this long (thinking at high effort is slow).
pub const MAX_TIME_SECS: u32 = 900;
/// Marker of the line curl's `write-out` prints on standard error.
pub const STATUS_MARKER: &str = "gitvis-http:";

/// What is sent: stable instructions (cached) and the changing material.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prompt {
    pub system: String,
    pub user: String,
}

impl Prompt {
    pub fn chars(&self) -> usize {
        self.system.chars().count() + self.user.chars().count()
    }
}

/// The JSON body of a streamed Messages request.
///
/// - No `temperature` / `top_p` / `top_k` (rejected on this API version).
/// - No `thinking` field: Opus 5.5 thinks adaptively and cannot turn it off;
///   depth is `output_config.effort`, always set explicitly.
/// - The system prompt is one text block marked for prompt caching; the
///   diff or log goes in the user message.
/// - `"fallbacks": "default"` when the model has server-side fallbacks.
/// - No assistant prefill: the last message is the user's.
pub fn body(model: Model, effort: Effort, prompt: &Prompt) -> Value {
    let mut v = json!({
        "model": model.id(),
        "max_tokens": MAX_TOKENS,
        "stream": true,
        "system": [{
            "type": "text",
            "text": prompt.system,
            "cache_control": {"type": "ephemeral"},
        }],
        "messages": [{"role": "user", "content": prompt.user}],
        "output_config": {"effort": effort.for_api().as_str()},
    });
    if model.supports_fallbacks() {
        v["fallbacks"] = json!("default");
    }
    v
}

/// curl's arguments: the same for every request, nothing secret in them.
pub fn curl_args() -> Vec<String> {
    ["-sS", "-N", "--fail-with-body", "--config", "-", API_URL]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

/// A value for a quoted curl config string: backslashes and quotes
/// escaped; control characters (a newline would start a new option) are
/// refused.
pub fn config_quote(value: &str) -> Result<String, String> {
    if value.chars().any(|c| c.is_control()) {
        return Err("contains a control character".to_owned());
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

/// The config curl reads from standard input: the headers (key included),
/// the body file and the status line written to standard error. This text
/// is never shown or logged.
pub fn curl_config(key: &str, body_path: &str, fallbacks: bool) -> Result<String, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("The API key is empty".to_owned());
    }
    let key_header = config_quote(&format!("x-api-key: {key}"))
        .map_err(|_| "The API key contains a line break or control character".to_owned())?;
    let body =
        config_quote(&format!("@{body_path}")).map_err(|e| format!("The request file path {e}"))?;
    let mut s = String::new();
    s.push_str(&format!("header = {key_header}\n"));
    s.push_str(&format!("header = \"anthropic-version: {API_VERSION}\"\n"));
    s.push_str("header = \"content-type: application/json\"\n");
    if fallbacks {
        s.push_str(&format!("header = \"anthropic-beta: {FALLBACK_BETA}\"\n"));
    }
    s.push_str(&format!("data-binary = {body}\n"));
    s.push_str(&format!("max-time = {MAX_TIME_SECS}\n"));
    s.push_str("connect-timeout = 20\n");
    s.push_str(&format!(
        "write-out = \"%{{stderr}}{STATUS_MARKER} %{{http_code}} retry-after=%header{{retry-after}} request-id=%header{{request-id}}\\n\"\n"
    ));
    Ok(s)
}

/// The command as shown in logs and confirmations.
pub fn curl_display() -> String {
    format!(
        "curl {}  (headers and the request file on standard input, not logged)",
        curl_args().join(" ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt() -> Prompt {
        Prompt {
            system: "You write commit messages.".to_owned(),
            user: "diff --git a/x b/x".to_owned(),
        }
    }

    #[test]
    fn body_has_effort_cache_and_fallbacks_but_no_sampling() {
        let v = body(Model::Opus55, Effort::Low, &prompt());
        assert_eq!(v["model"], "claude-opus-5-5");
        assert_eq!(v["max_tokens"], 16000);
        assert_eq!(v["stream"], true);
        assert_eq!(v["output_config"]["effort"], "low");
        assert_eq!(v["fallbacks"], "default");
        for k in ["temperature", "top_p", "top_k", "thinking"] {
            assert!(v.get(k).is_none(), "{k} must not be sent");
        }
        // cache_control on the system block, nowhere in the messages.
        assert_eq!(v["system"][0]["type"], "text");
        assert_eq!(v["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(v["system"][0]["text"], "You write commit messages.");
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "diff --git a/x b/x");
        assert!(!v["messages"].to_string().contains("cache_control"));
        // No assistant prefill.
        assert_eq!(msgs.last().unwrap()["role"], "user");
    }

    #[test]
    fn haiku_gets_no_fallbacks() {
        let v = body(Model::Haiku55, Effort::High, &prompt());
        assert!(v.get("fallbacks").is_none());
        assert_eq!(v["output_config"]["effort"], "high");
        let v = body(Model::Fable51, Effort::Medium, &prompt());
        assert_eq!(v["fallbacks"], "default");
        assert_eq!(v["output_config"]["effort"], "medium");
    }

    #[test]
    fn args_never_carry_the_key() {
        let a = curl_args();
        assert_eq!(
            a,
            vec![
                "-sS",
                "-N",
                "--fail-with-body",
                "--config",
                "-",
                "https://api.anthropic.com/v1/messages"
            ]
        );
        assert!(curl_display().starts_with("curl -sS -N --fail-with-body --config - https://"));
    }

    #[test]
    fn config_text() {
        let c = curl_config("sk-ant-test-123", r"C:\Temp\gitvis ai\b.json", true).unwrap();
        let lines: Vec<&str> = c.lines().collect();
        assert_eq!(lines[0], r#"header = "x-api-key: sk-ant-test-123""#);
        assert_eq!(lines[1], r#"header = "anthropic-version: 2026-10-21""#);
        assert_eq!(lines[2], r#"header = "content-type: application/json""#);
        assert_eq!(
            lines[3],
            r#"header = "anthropic-beta: server-side-fallback-2026-07-01""#
        );
        // Backslashes escaped inside the quoted value.
        assert_eq!(lines[4], r#"data-binary = "@C:\\Temp\\gitvis ai\\b.json""#);
        assert!(c.contains("max-time = 900"));
        assert!(c.contains(
            r#"write-out = "%{stderr}gitvis-http: %{http_code} retry-after=%header{retry-after} request-id=%header{request-id}\n""#
        ));
        // Without fallbacks there is no beta header.
        let c = curl_config("k-123456789", "/tmp/b.json", false).unwrap();
        assert!(!c.contains("anthropic-beta"));
    }

    #[test]
    fn config_refuses_bad_keys() {
        assert!(curl_config("", "/tmp/b", false).is_err());
        assert!(curl_config("   ", "/tmp/b", false).is_err());
        let e = curl_config("abc\nheader = \"x: y\"", "/tmp/b", false).unwrap_err();
        assert!(!e.contains("abc"), "the error must not echo the key: {e}");
        assert_eq!(config_quote(r#"a"b\c"#).unwrap(), r#""a\"b\\c""#);
    }
}
