//! A typed helper over `gh api` for one repository.
//!
//! Every request is recorded as an [`ApiCall`] (command, bytes, latency,
//! rate limit, error, and the body that was sent unless it is withheld).
//! Bodies go to `gh api --input -` on standard input, so request content
//! never appears on a command line.
//!
//! Writes are described by a [`Request`] built by pure functions in each
//! area, so tests can check method, endpoint and body without a network.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::github::{ApiCall, Body, api_request};
use crate::{Error, Result};

/// HTTP method of a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
        }
    }
}

/// One REST call, described without running it.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub method: Method,
    /// Relative endpoint, e.g. `repos/o/r/pulls` (no leading slash).
    pub endpoint: String,
    pub body: Option<Value>,
    /// The body holds something that must not be logged.
    pub withheld: bool,
}

impl Request {
    pub fn new(method: Method, endpoint: impl Into<String>) -> Self {
        Self {
            method,
            endpoint: endpoint.into(),
            body: None,
            withheld: false,
        }
    }

    pub fn get(endpoint: impl Into<String>) -> Self {
        Self::new(Method::Get, endpoint)
    }

    pub fn post(endpoint: impl Into<String>, body: Value) -> Self {
        Self::new(Method::Post, endpoint).body(body)
    }

    pub fn put(endpoint: impl Into<String>, body: Value) -> Self {
        Self::new(Method::Put, endpoint).body(body)
    }

    pub fn patch(endpoint: impl Into<String>, body: Value) -> Self {
        Self::new(Method::Patch, endpoint).body(body)
    }

    pub fn delete(endpoint: impl Into<String>) -> Self {
        Self::new(Method::Delete, endpoint)
    }

    pub fn body(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }

    pub fn withheld(mut self) -> Self {
        self.withheld = true;
        self
    }

    /// The command as the activity log shows it, e.g.
    /// `gh api --method POST repos/o/r/pulls --input - (JSON body)`.
    pub fn display(&self) -> String {
        let mut s = String::from("gh api");
        if self.method != Method::Get {
            s.push_str(&format!(" --method {}", self.method.as_str()));
        }
        s.push(' ');
        s.push_str(&self.endpoint);
        if self.body.is_some() {
            s.push_str(if self.withheld {
                " --input - (body withheld)"
            } else {
                " --input - (JSON body)"
            });
        }
        s
    }

    /// The body as pretty JSON for confirmation dialogs (`None` when there
    /// is no body or it is withheld).
    pub fn body_preview(&self) -> Option<String> {
        if self.withheld {
            return None;
        }
        self.body
            .as_ref()
            .and_then(|b| serde_json::to_string_pretty(b).ok())
    }
}

/// `gh api` bound to one repository (or to the user, with empty owner/repo).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gh {
    /// Where `gh` runs (any directory works for API calls).
    pub dir: PathBuf,
    pub owner: String,
    pub repo: String,
}

impl Gh {
    pub fn new(dir: impl Into<PathBuf>, owner: impl Into<String>, repo: impl Into<String>) -> Self {
        Self {
            dir: dir.into(),
            owner: owner.into(),
            repo: repo.into(),
        }
    }

    /// Not bound to a repository: for `notifications`, `search/issues`, `user`.
    pub fn user(dir: impl Into<PathBuf>) -> Self {
        Self::new(dir, "", "")
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `owner/repo`.
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

    /// `repos/{owner}/{repo}/{rest}` (or `repos/{owner}/{repo}` for "").
    pub fn path(&self, rest: &str) -> String {
        let rest = rest.trim_start_matches('/');
        if rest.is_empty() {
            format!("repos/{}/{}", self.owner, self.repo)
        } else {
            format!("repos/{}/{}/{rest}", self.owner, self.repo)
        }
    }

    /// `https://github.com/{owner}/{repo}{suffix}`.
    pub fn web_url(&self, suffix: &str) -> String {
        format!("https://github.com/{}/{}{suffix}", self.owner, self.repo)
    }

    /// GET an endpoint, raw bytes.
    pub fn get(&self, endpoint: &str, calls: &mut Vec<ApiCall>) -> Result<Vec<u8>> {
        api_request(&self.dir, endpoint, &[], None, calls)
    }

    /// GET with extra `gh api` flags (e.g. `--allow-escape-sequences`, or
    /// `-H Accept: application/vnd.github.raw`).
    pub fn get_with(
        &self,
        endpoint: &str,
        flags: &[&str],
        calls: &mut Vec<ApiCall>,
    ) -> Result<Vec<u8>> {
        api_request(&self.dir, endpoint, flags, None, calls)
    }

    /// GET and decode JSON; `what` names the resource in errors.
    pub fn get_json<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        what: &str,
        calls: &mut Vec<ApiCall>,
    ) -> Result<T> {
        decode(&self.get(endpoint, calls)?, what)
    }

    /// GET a list endpoint page by page (`per_page=100&page=N` appended)
    /// until a short page or `max_pages`. For endpoints answering a bare
    /// JSON array.
    pub fn get_pages<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        what: &str,
        max_pages: usize,
        calls: &mut Vec<ApiCall>,
    ) -> Result<Vec<T>> {
        let sep = if endpoint.contains('?') { '&' } else { '?' };
        let mut all = Vec::new();
        for page in 1..=max_pages.max(1) {
            let items: Vec<T> = self.get_json(
                &format!("{endpoint}{sep}per_page=100&page={page}"),
                what,
                calls,
            )?;
            let n = items.len();
            all.extend(items);
            if n < 100 {
                break;
            }
        }
        Ok(all)
    }

    /// GET a list endpoint whose answer wraps the array in an object, e.g.
    /// `{"total_count": 3, "artifacts": [...]}` with `key = "artifacts"`.
    /// Returns the items and the reported total.
    pub fn get_wrapped_pages<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        key: &str,
        what: &str,
        max_pages: usize,
        calls: &mut Vec<ApiCall>,
    ) -> Result<(Vec<T>, u64)> {
        let sep = if endpoint.contains('?') { '&' } else { '?' };
        let mut all = Vec::new();
        let mut total = 0u64;
        for page in 1..=max_pages.max(1) {
            let v: Value = self.get_json(
                &format!("{endpoint}{sep}per_page=100&page={page}"),
                what,
                calls,
            )?;
            total = v.get("total_count").and_then(Value::as_u64).unwrap_or(0);
            let items = v.get(key).cloned().unwrap_or(Value::Array(Vec::new()));
            let items: Vec<T> = serde_json::from_value(items)
                .map_err(|e| Error::new(format!("GitHub {what}: {e}")))?;
            let n = items.len();
            all.extend(items);
            if n < 100 || all.len() as u64 >= total {
                break;
            }
        }
        Ok((all, total))
    }

    /// Run a described request. The body (if any) goes to standard input;
    /// a withheld body is never logged.
    pub fn send(&self, req: &Request, calls: &mut Vec<ApiCall>) -> Result<Vec<u8>> {
        let method = req.method.as_str();
        let flags: Vec<&str> = if req.method == Method::Get {
            Vec::new()
        } else {
            vec!["--method", method]
        };
        match &req.body {
            None => api_request(&self.dir, &req.endpoint, &flags, None, calls),
            Some(v) => {
                let bytes = serde_json::to_vec(v).map_err(|e| Error::new(e.to_string()))?;
                let body = if req.withheld {
                    Body::Withheld(&bytes)
                } else {
                    Body::Logged(&bytes)
                };
                api_request(&self.dir, &req.endpoint, &flags, Some(body), calls)
            }
        }
    }

    /// Run a request and decode its JSON answer (`Value::Null` for an
    /// empty body, e.g. `204 No Content`).
    pub fn send_json<T: DeserializeOwned>(
        &self,
        req: &Request,
        what: &str,
        calls: &mut Vec<ApiCall>,
    ) -> Result<T> {
        let body = self.send(req, calls)?;
        if body.iter().all(|b| b.is_ascii_whitespace()) {
            return serde_json::from_value(Value::Null)
                .map_err(|e| Error::new(format!("GitHub {what}: empty answer ({e})")));
        }
        decode(&body, what)
    }

    /// A request whose body must not be logged (shorthand for
    /// `send(&req.withheld())`).
    pub fn send_withheld(&self, req: Request, calls: &mut Vec<ApiCall>) -> Result<Vec<u8>> {
        self.send(&req.withheld(), calls)
    }

    /// A GraphQL query or mutation. Returns `data`; GraphQL `errors` become
    /// an error with every message.
    pub fn graphql(
        &self,
        query: &str,
        variables: Value,
        calls: &mut Vec<ApiCall>,
    ) -> Result<Value> {
        let body = serde_json::json!({ "query": query, "variables": variables });
        let bytes = serde_json::to_vec(&body).map_err(|e| Error::new(e.to_string()))?;
        let raw = api_request(
            &self.dir,
            "graphql",
            &["--method", "POST"],
            Some(Body::Logged(&bytes)),
            calls,
        )?;
        let v: Value = decode(&raw, "GraphQL answer")?;
        if let Some(errs) = v.get("errors").and_then(Value::as_array)
            && !errs.is_empty()
        {
            let msgs: Vec<String> = errs
                .iter()
                .filter_map(|e| e.get("message").and_then(Value::as_str))
                .map(str::to_owned)
                .collect();
            let msg = if msgs.is_empty() {
                "GraphQL request failed".to_owned()
            } else {
                msgs.join("; ")
            };
            if let Some(last) = calls.last_mut() {
                last.error = Some(msg.clone());
            }
            return Err(Error::new(msg));
        }
        Ok(v.get("data").cloned().unwrap_or(Value::Null))
    }

    /// Run another `gh` subcommand (e.g. `secret set NAME`) in the
    /// repository, with optional standard input that is never logged.
    /// Recorded as an [`ApiCall`] so it shows up with the API calls.
    pub fn cli(
        &self,
        args: &[&str],
        stdin: Option<&[u8]>,
        calls: &mut Vec<ApiCall>,
    ) -> Result<String> {
        let program = crate::github::gh_program();
        let mut command = format!("gh {}", crate::devops::proc::render_args(args));
        if let Some(b) = stdin {
            command.push_str(&format!(
                " (standard input: {}, not logged)",
                crate::devops::plural_bytes(b.len())
            ));
        }
        let started = std::time::Instant::now();
        let out = crate::cli::run_raw(&program, &self.dir, args, stdin, &[]);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match out {
            Ok(o) if o.success => {
                let text = String::from_utf8_lossy(&o.stdout).trim().to_owned();
                let text = if text.is_empty() { o.stderr } else { text };
                calls.push(ApiCall {
                    command,
                    elapsed_ms,
                    bytes: o.stdout.len(),
                    rate: None,
                    error: None,
                    input: None,
                });
                Ok(text)
            }
            Ok(o) => {
                let mut msg = o.stderr.trim().to_owned();
                if msg.is_empty() {
                    msg = String::from_utf8_lossy(&o.stdout).trim().to_owned();
                }
                if msg.is_empty() {
                    msg = format!("gh exited with {:?}", o.code);
                }
                calls.push(ApiCall {
                    command,
                    elapsed_ms,
                    bytes: o.stdout.len(),
                    rate: None,
                    error: Some(msg.clone()),
                    input: None,
                });
                Err(Error::new(msg))
            }
            Err(e) => {
                let msg = e.to_string();
                calls.push(ApiCall {
                    command,
                    elapsed_ms,
                    bytes: 0,
                    rate: None,
                    error: Some(msg.clone()),
                    input: None,
                });
                Err(Error::new(msg))
            }
        }
    }

    /// The login of the authenticated user (`gh api user`).
    pub fn login(&self, calls: &mut Vec<ApiCall>) -> Result<String> {
        #[derive(serde::Deserialize)]
        struct User {
            login: String,
        }
        let u: User = self.get_json("user", "user", calls)?;
        Ok(u.login)
    }
}

/// Decode a JSON body, naming the resource in the error.
pub fn decode<T: DeserializeOwned>(body: &[u8], what: &str) -> Result<T> {
    serde_json::from_slice(body).map_err(|e| Error::new(format!("GitHub {what}: {e}")))
}

/// Percent-encode a value for a query string or a path segment
/// (`feature/x` → `feature%2Fx`).
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_display_and_preview() {
        let g = Gh::new(".", "o", "r");
        let r = Request::post(g.path("pulls"), json!({"title": "x"}));
        assert_eq!(r.endpoint, "repos/o/r/pulls");
        assert_eq!(
            r.display(),
            "gh api --method POST repos/o/r/pulls --input - (JSON body)"
        );
        assert!(r.body_preview().unwrap().contains("\"title\": \"x\""));
        let s = Request::put(g.path("actions/variables/X"), json!({"value": "v"})).withheld();
        assert!(s.display().ends_with("(body withheld)"));
        assert_eq!(s.body_preview(), None);
        assert_eq!(Request::get("user").display(), "gh api user");
        assert_eq!(g.path(""), "repos/o/r");
        assert_eq!(g.web_url("/pull/3"), "https://github.com/o/r/pull/3");
    }

    #[test]
    fn percent_encoding() {
        assert_eq!(encode("feature/x y"), "feature%2Fx%20y");
        assert_eq!(encode("v1.2.3-rc_1~"), "v1.2.3-rc_1~");
    }
}
