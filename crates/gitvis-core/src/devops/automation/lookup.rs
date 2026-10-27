//! Optional title and status lookup of issue keys: Jira REST (e-mail + API
//! token, basic auth) and Linear GraphQL (personal API key), through
//! `curl`.
//!
//! Credentials never reach a command line: the whole request (URL, method,
//! headers including `Authorization`, body) is a curl config read from
//! standard input (`curl --config -`), and the base64 of `email:token` is
//! computed here. The command line is always just `curl --config -`.

use std::time::Duration;

use serde_json::{Value, json};

use crate::devops::proc::{ProcSpec, run_capture};
use crate::{Error, Result};

use super::links::{Tracker, jira_base};

/// Keychain entry names (service `gitvis`).
pub const JIRA_TOKEN: &str = "jira-token";
pub const LINEAR_KEY: &str = "linear-api-key";

/// How long one lookup may take.
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// What a lookup found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueInfo {
    pub key: String,
    pub title: String,
    /// `In Progress`, `Done`, … (empty when the tracker did not say).
    pub status: String,
}

/// Standard base64 with padding (RFC 4648).
pub fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// One HTTP request for curl, described without running it.
#[derive(Clone, PartialEq, Eq)]
pub struct CurlRequest {
    pub method: &'static str,
    pub url: String,
    /// Headers that may be shown.
    pub headers: Vec<(String, String)>,
    /// The `Authorization` header value; never shown or logged.
    auth: String,
    pub body: Option<String>,
}

impl std::fmt::Debug for CurlRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CurlRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("auth", &"•••")
            .field("body", &self.body)
            .finish()
    }
}

/// A value for a curl config file: double-quoted, with `\` and `"` escaped
/// and control characters written as escapes.
fn quoted(v: &str) -> String {
    let mut s = String::with_capacity(v.len() + 2);
    s.push('"');
    for c in v.chars() {
        match c {
            '\\' => s.push_str("\\\\"),
            '"' => s.push_str("\\\""),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\t' => s.push_str("\\t"),
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

impl CurlRequest {
    /// The arguments: everything else is on standard input.
    pub fn args() -> [&'static str; 2] {
        ["--config", "-"]
    }

    /// The curl config written to standard input (contains the secret).
    pub fn config(&self) -> String {
        let mut c = String::new();
        c.push_str(&format!("url = {}\n", quoted(&self.url)));
        c.push_str(&format!("request = {}\n", quoted(self.method)));
        for (k, v) in &self.headers {
            c.push_str(&format!("header = {}\n", quoted(&format!("{k}: {v}"))));
        }
        c.push_str(&format!(
            "header = {}\n",
            quoted(&format!("Authorization: {}", self.auth))
        ));
        if let Some(b) = &self.body {
            c.push_str(&format!("data-raw = {}\n", quoted(b)));
        }
        c.push_str("silent\nshow-error\n");
        c.push_str(&format!("max-time = {}\n", TIMEOUT.as_secs()));
        // The status code on a last line of its own.
        c.push_str("write-out = \"\\n%{http_code}\"\n");
        c
    }

    /// The request as the activity log shows it: method, URL and the
    /// header names, never the credential.
    pub fn display(&self) -> String {
        let mut names: Vec<&str> = self.headers.iter().map(|(k, _)| k.as_str()).collect();
        names.push("Authorization");
        format!(
            "curl --config -   # {} {}, headers {} on standard input{}",
            self.method,
            self.url,
            names.join(", "),
            if self.body.is_some() {
                ", JSON body"
            } else {
                ""
            }
        )
    }

    /// Whether `needle` occurs in what would be visible (arguments and
    /// display); for tests.
    pub fn visible(&self) -> String {
        format!("{} {}", Self::args().join(" "), self.display())
    }
}

/// `GET {base}/rest/api/3/issue/{key}?fields=summary,status` with basic
/// auth (Jira Cloud: e-mail and API token).
pub fn jira_request(base: &str, key: &str, email: &str, token: &str) -> CurlRequest {
    let creds = base64(format!("{}:{}", email.trim(), token.trim()).as_bytes());
    CurlRequest {
        method: "GET",
        url: format!(
            "{}/rest/api/3/issue/{}?fields=summary,status",
            jira_base(base),
            crate::devops::gh::encode(key)
        ),
        headers: vec![("Accept".into(), "application/json".into())],
        auth: format!("Basic {creds}"),
        body: None,
    }
}

/// The Linear issue query (identifiers like `ENG-42` are accepted as ids).
pub const LINEAR_QUERY: &str =
    "query Issue($id: String!) { issue(id: $id) { identifier title state { name } } }";

/// `POST https://api.linear.app/graphql` with the personal API key.
pub fn linear_request(key: &str, api_key: &str) -> CurlRequest {
    let body = json!({ "query": LINEAR_QUERY, "variables": { "id": key } });
    CurlRequest {
        method: "POST",
        url: "https://api.linear.app/graphql".into(),
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("Accept".into(), "application/json".into()),
        ],
        auth: api_key.trim().to_owned(),
        body: Some(body.to_string()),
    }
}

/// Split curl's output into the body and the status code of the last line.
pub fn split_status(out: &[u8]) -> (Vec<u8>, Option<u16>) {
    let text = String::from_utf8_lossy(out);
    match text.rsplit_once('\n') {
        Some((body, code)) => (body.as_bytes().to_vec(), code.trim().parse().ok()),
        None => (Vec::new(), text.trim().parse().ok()),
    }
}

fn http_error(tracker: &str, code: u16, body: &[u8]) -> Error {
    let detail = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| {
            v.get("errorMessages")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    v.get("errors")
                        .and_then(Value::as_array)
                        .and_then(|a| a.first())
                        .and_then(|e| e.get("message"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
        })
        .unwrap_or_default();
    let why = match code {
        401 => "the credentials were rejected",
        403 => "access denied",
        404 => "not found (or not visible to this account)",
        429 => "rate limited",
        _ => "request failed",
    };
    let mut msg = format!("{tracker}: {why} (HTTP {code})");
    if !detail.is_empty() {
        msg.push_str(&format!(": {detail}"));
    }
    Error::new(msg)
}

/// Parse a Jira `issue` answer.
pub fn parse_jira(body: &[u8]) -> Result<IssueInfo> {
    let v: Value =
        serde_json::from_slice(body).map_err(|e| Error::new(format!("Jira answer: {e}")))?;
    let fields = v.get("fields").cloned().unwrap_or(Value::Null);
    let title = fields
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if title.is_empty() {
        return Err(Error::new("Jira answer: no summary"));
    }
    Ok(IssueInfo {
        key: v
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        title,
        status: fields
            .pointer("/status/name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
    })
}

/// Parse a Linear GraphQL answer.
pub fn parse_linear(body: &[u8]) -> Result<IssueInfo> {
    let v: Value =
        serde_json::from_slice(body).map_err(|e| Error::new(format!("Linear answer: {e}")))?;
    if let Some(m) = v
        .get("errors")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
    {
        return Err(Error::new(format!("Linear: {m}")));
    }
    let issue = v.pointer("/data/issue").cloned().unwrap_or(Value::Null);
    if issue.is_null() {
        return Err(Error::new("Linear: issue not found"));
    }
    Ok(IssueInfo {
        key: issue
            .get("identifier")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        title: issue
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        status: issue
            .pointer("/state/name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
    })
}

/// The curl executable: `GITVIS_CURL` (a test double) or `curl` on PATH.
pub fn curl_program() -> String {
    std::env::var("GITVIS_CURL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "curl".to_owned())
}

/// Run a request through curl; `commands` gets the displayed form.
pub fn send(
    req: &CurlRequest,
    program: &str,
    dir: &std::path::Path,
    commands: &mut Vec<String>,
) -> Result<(Option<u16>, Vec<u8>)> {
    commands.push(req.display());
    let spec = ProcSpec::new(program, dir)
        .args(CurlRequest::args())
        .stdin(req.config().into_bytes());
    let out = run_capture(&spec, Some(TIMEOUT + Duration::from_secs(5)))?;
    if !out.success() {
        // curl's own errors ("Could not resolve host") never echo the config.
        return Err(Error::new(format!("curl: {}", out.error_text())));
    }
    let (body, code) = split_status(&out.stdout);
    Ok((code, body))
}

/// Look up one key.
pub fn lookup(
    tracker: Tracker,
    req: &CurlRequest,
    program: &str,
    dir: &std::path::Path,
    commands: &mut Vec<String>,
) -> Result<IssueInfo> {
    let name = match tracker {
        Tracker::Jira => "Jira",
        Tracker::Linear => "Linear",
        Tracker::Custom(_) => return Err(Error::new("custom trackers have no lookup")),
    };
    let (code, body) = send(req, program, dir, commands)?;
    match code {
        Some(c) if (200..300).contains(&c) => {}
        Some(c) => return Err(http_error(name, c, &body)),
        None => return Err(Error::new(format!("{name}: no HTTP status from curl"))),
    }
    match tracker {
        Tracker::Jira => parse_jira(&body),
        _ => parse_linear(&body),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc_4648() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
            ("me@example.com:tok+/=", "bWVAZXhhbXBsZS5jb206dG9rKy89"),
        ] {
            assert_eq!(base64(i.as_bytes()), o, "{i}");
        }
    }

    #[test]
    fn jira_token_only_in_the_config() {
        let token = "ATATT3xFfGF0-secret-token";
        let r = jira_request("acme.atlassian.net/", "ABC-1", "me@example.com", token);
        let creds = base64(format!("me@example.com:{token}").as_bytes());
        let cfg = r.config();
        assert!(cfg.contains(&format!("header = \"Authorization: Basic {creds}\"")));
        assert!(cfg.contains(
            "url = \"https://acme.atlassian.net/rest/api/3/issue/ABC-1?fields=summary,status\""
        ));
        assert!(cfg.contains("request = \"GET\""));
        assert!(cfg.contains("write-out = \"\\n%{http_code}\""));
        let visible = r.visible();
        assert!(
            !visible.contains(token) && !visible.contains(&creds),
            "{visible}"
        );
        assert!(visible.starts_with("--config -"));
        assert!(r.display().contains("Authorization on standard input"));
        assert!(!format!("{r:?}").contains(&creds));
        assert_eq!(CurlRequest::args(), ["--config", "-"]);
    }

    #[test]
    fn linear_key_only_in_the_config() {
        let key = "lin_api_abcdefSECRET";
        let r = linear_request("ENG-42", key);
        let cfg = r.config();
        assert!(cfg.contains(&format!("header = \"Authorization: {key}\"")));
        assert!(cfg.contains("request = \"POST\""));
        // The JSON body is escaped for the config syntax.
        assert!(cfg.contains("data-raw = \"{\\\"query\\\":"), "{cfg}");
        assert!(cfg.contains("\\\"ENG-42\\\""));
        assert!(!r.visible().contains(key));
        assert!(!format!("{r:?}").contains(key));
    }

    #[test]
    fn quoting() {
        assert_eq!(quoted(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(quoted("x\ny"), r#""x\ny""#);
    }

    #[test]
    fn parses_answers() {
        let j = br#"{"key":"ABC-1","fields":{"summary":"Login fails","status":{"name":"In Progress"}}}"#;
        let i = parse_jira(j).unwrap();
        assert_eq!(
            (i.key.as_str(), i.title.as_str(), i.status.as_str()),
            ("ABC-1", "Login fails", "In Progress")
        );
        assert!(parse_jira(b"{}").is_err());
        let l = br#"{"data":{"issue":{"identifier":"ENG-42","title":"Speed up","state":{"name":"Todo"}}}}"#;
        let i = parse_linear(l).unwrap();
        assert_eq!((i.key.as_str(), i.status.as_str()), ("ENG-42", "Todo"));
        let e = parse_linear(br#"{"errors":[{"message":"Entity not found"}]}"#).unwrap_err();
        assert!(e.to_string().contains("Entity not found"));
        assert!(parse_linear(br#"{"data":{"issue":null}}"#).is_err());
    }

    #[test]
    fn status_lines_and_errors() {
        assert_eq!(
            split_status(b"{\"a\":1}\n200"),
            (b"{\"a\":1}".to_vec(), Some(200))
        );
        assert_eq!(split_status(b"404"), (Vec::new(), Some(404)));
        let e = http_error("Jira", 401, br#"{"errorMessages":["Login required"]}"#);
        assert_eq!(
            e.to_string(),
            "Jira: the credentials were rejected (HTTP 401): Login required"
        );
        assert_eq!(
            http_error("Linear", 500, b"").to_string(),
            "Linear: request failed (HTTP 500)"
        );
    }
}
