//! Server-Sent Events from the Messages API, and the answer they build.
//!
//! [`SseParser`] turns lines (or arbitrary chunks, with `\n`, `\r\n` or
//! `\r` endings) into events; [`Answer::apply`] folds the events into the
//! text, the model that served it, fallback switches, usage and the stop
//! reason. Thinking blocks and their deltas are ignored.

use serde_json::Value;

use super::model::Usage;

/// One event: its `event:` name and the joined `data:` lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

/// Incremental SSE parser.
#[derive(Default, Debug)]
pub struct SseParser {
    event: String,
    data: Vec<String>,
    /// A chunk's unfinished last line.
    partial: String,
    /// The previous chunk ended with `\r`: a following `\n` belongs to it.
    after_cr: bool,
}

impl SseParser {
    /// Feed one complete line (without its line break).
    pub fn push_line(&mut self, line: &str) -> Option<SseEvent> {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None; // comment
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = value.to_owned(),
            "data" => self.data.push(value.to_owned()),
            _ => {} // id, retry: not used
        }
        None
    }

    /// Feed a chunk of the stream; lines may be split across chunks.
    pub fn feed(&mut self, chunk: &str) -> Vec<SseEvent> {
        let mut out = Vec::new();
        for c in chunk.chars() {
            if self.after_cr {
                self.after_cr = false;
                if c == '\n' {
                    continue;
                }
            }
            match c {
                '\n' | '\r' => {
                    self.after_cr = c == '\r';
                    let line = std::mem::take(&mut self.partial);
                    if let Some(e) = self.push_line(&line) {
                        out.push(e);
                    }
                }
                _ => self.partial.push(c),
            }
        }
        out
    }

    /// The stream ended: an event without its blank line still counts.
    pub fn finish(&mut self) -> Option<SseEvent> {
        if !self.partial.is_empty() {
            let line = std::mem::take(&mut self.partial);
            if let Some(e) = self.push_line(&line) {
                return Some(e);
            }
        }
        self.dispatch()
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        if self.data.is_empty() && self.event.is_empty() {
            return None;
        }
        let data = std::mem::take(&mut self.data).join("\n");
        let event = std::mem::take(&mut self.event);
        Some(SseEvent { event, data })
    }
}

/// An error the API reported (HTTP error body or an `error` event).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApiError {
    /// `authentication_error`, `rate_limit_error`, `overloaded_error`, …
    pub kind: String,
    pub message: String,
    pub request_id: Option<String>,
    /// HTTP status (200 for an error event inside a stream).
    pub status: Option<u16>,
    /// Seconds to wait before retrying (`retry-after`).
    pub retry_after: Option<u64>,
}

impl ApiError {
    /// 429, 500 and 529 (and their error types) can be retried.
    pub fn retryable(&self) -> bool {
        matches!(self.status, Some(429 | 500 | 529))
            || matches!(
                self.kind.as_str(),
                "rate_limit_error" | "api_error" | "overloaded_error"
            )
    }

    /// One line: `authentication_error (HTTP 401): invalid x-api-key`.
    pub fn summary(&self) -> String {
        let mut s = if self.kind.is_empty() {
            "API error".to_owned()
        } else {
            self.kind.clone()
        };
        if let Some(st) = self.status.filter(|s| *s != 200) {
            s.push_str(&format!(" (HTTP {st})"));
        }
        if !self.message.is_empty() {
            s.push_str(": ");
            s.push_str(&self.message);
        }
        s
    }

    /// What the user can do about it.
    pub fn hint(&self) -> &'static str {
        match self.kind.as_str() {
            "authentication_error" => {
                "The API key was not accepted. Check it, or store a new one in Settings → AI assist."
            }
            "permission_error" => "This key may not use this model or feature.",
            "not_found_error" => "The model or endpoint was not found. Pick another model.",
            "rate_limit_error" => {
                "Too many requests or tokens per minute for this key. Wait, then retry."
            }
            "overloaded_error" => "The API is overloaded. Retry in a moment.",
            "api_error" => "The API had an internal error. Retry in a moment.",
            "billing_error" => "The account has a billing problem (credit or payment).",
            "request_too_large" => {
                "The request is too large. Lower the size budget in Settings → AI assist."
            }
            "invalid_request_error" => "The API rejected the request.",
            _ => "",
        }
    }
}

/// Parse `{"type":"error","error":{"type","message"},"request_id"}`.
pub fn parse_error_body(text: &str) -> Option<ApiError> {
    let v: Value = serde_json::from_str(text.trim()).ok()?;
    let e = v.get("error")?;
    Some(ApiError {
        kind: e.get("type")?.as_str()?.to_owned(),
        message: e
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        request_id: v
            .get("request_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        status: None,
        retry_after: None,
    })
}

/// The status line curl's `write-out` printed on standard error.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HttpMeta {
    pub status: Option<u16>,
    pub retry_after: Option<u64>,
    pub request_id: Option<String>,
}

/// Find `gitvis-http: 429 retry-after=12 request-id=req_1` in `stderr`.
pub fn parse_http_meta(stderr: &str, marker: &str) -> HttpMeta {
    let Some(line) = stderr.lines().rev().find(|l| l.contains(marker)) else {
        return HttpMeta::default();
    };
    let rest = &line[line.find(marker).unwrap_or(0) + marker.len()..];
    let mut meta = HttpMeta::default();
    for (i, part) in rest.split_whitespace().enumerate() {
        if i == 0 {
            meta.status = part.parse().ok().filter(|s| *s != 0);
        } else if let Some(v) = part.strip_prefix("retry-after=") {
            meta.retry_after = v.trim().parse().ok();
        } else if let Some(v) = part.strip_prefix("request-id=") {
            meta.request_id = (!v.is_empty()).then(|| v.to_owned());
        }
    }
    meta
}

/// A server-side fallback: `from` declined, `to` continued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fallback {
    pub from: String,
    pub to: String,
}

/// The answer as it streams in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Answer {
    pub message_id: String,
    /// The model that produced the message (a fallback model after a
    /// switch before any output).
    pub model: String,
    /// The text blocks, in order.
    pub text: String,
    pub fallbacks: Vec<Fallback>,
    pub usage: Usage,
    pub stop_reason: Option<String>,
    /// `stop_details.category` of a refusal.
    pub refusal_category: Option<String>,
    pub refusal_explanation: Option<String>,
    /// An `error` event arrived.
    pub error: Option<ApiError>,
    /// `message_stop` arrived.
    pub complete: bool,
    /// Type of each content block by index (`text`, `thinking`, …).
    pub blocks: Vec<String>,
    /// Thinking blocks seen (their content is not shown).
    pub thinking_blocks: usize,
    pub events: usize,
}

impl Answer {
    /// Is the model thinking right now (a thinking block is open)?
    pub fn thinking_now(&self) -> bool {
        self.blocks.last().is_some_and(|b| b == "thinking") && self.text.is_empty()
    }

    pub fn refused(&self) -> bool {
        self.stop_reason.as_deref() == Some("refusal")
    }

    pub fn hit_max_tokens(&self) -> bool {
        self.stop_reason.as_deref() == Some("max_tokens")
    }

    /// Fold one event in. Unknown events and malformed data are ignored.
    pub fn apply(&mut self, ev: &SseEvent) {
        self.events += 1;
        let Ok(v) = serde_json::from_str::<Value>(&ev.data) else {
            return;
        };
        let kind = if ev.event.is_empty() {
            v.get("type").and_then(Value::as_str).unwrap_or_default()
        } else {
            ev.event.as_str()
        };
        match kind {
            "message_start" => {
                let m = &v["message"];
                if let Some(id) = m.get("id").and_then(Value::as_str) {
                    self.message_id = id.to_owned();
                }
                if let Some(model) = m.get("model").and_then(Value::as_str) {
                    self.model = model.to_owned();
                }
                merge_usage(&mut self.usage, &m["usage"]);
            }
            "content_block_start" => {
                let b = &v["content_block"];
                let t = b
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let index = v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if self.blocks.len() <= index {
                    self.blocks.resize(index + 1, String::new());
                }
                match t.as_str() {
                    "text" => {
                        if let Some(s) = b.get("text").and_then(Value::as_str) {
                            self.text.push_str(s);
                        }
                    }
                    "thinking" | "redacted_thinking" => self.thinking_blocks += 1,
                    "fallback" => {
                        let model = |k: &str| {
                            b[k].get("model")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned()
                        };
                        let to = model("to");
                        self.fallbacks.push(Fallback {
                            from: model("from"),
                            to: to.clone(),
                        });
                        if !to.is_empty() {
                            self.model = to;
                        }
                    }
                    _ => {}
                }
                self.blocks[index] = t;
            }
            "content_block_delta" => {
                let d = &v["delta"];
                if d.get("type").and_then(Value::as_str) == Some("text_delta")
                    && let Some(s) = d.get("text").and_then(Value::as_str)
                {
                    self.text.push_str(s);
                }
                // thinking_delta, signature_delta: ignored.
            }
            "message_delta" => {
                let d = &v["delta"];
                if let Some(r) = d.get("stop_reason").and_then(Value::as_str) {
                    self.stop_reason = Some(r.to_owned());
                }
                let details = d
                    .get("stop_details")
                    .filter(|s| !s.is_null())
                    .or_else(|| v.get("stop_details").filter(|s| !s.is_null()));
                if let Some(sd) = details {
                    self.refusal_category = sd
                        .get("category")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    self.refusal_explanation = sd
                        .get("explanation")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
                merge_usage(&mut self.usage, &v["usage"]);
            }
            "message_stop" => self.complete = true,
            "error" => {
                let e = &v["error"];
                self.error = Some(ApiError {
                    kind: e
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("error")
                        .to_owned(),
                    message: e
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    request_id: v
                        .get("request_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    status: Some(200),
                    retry_after: None,
                });
            }
            // content_block_stop, ping: nothing to do.
            _ => {}
        }
    }
}

/// Usage fields present in `u` replace the ones in `into` (the API sends
/// cumulative counts).
fn merge_usage(into: &mut Usage, u: &Value) {
    let get = |k: &str| u.get(k).and_then(Value::as_u64);
    if let Some(n) = get("input_tokens") {
        into.input_tokens = n;
    }
    if let Some(n) = get("output_tokens") {
        into.output_tokens = n;
    }
    if let Some(n) = get("cache_creation_input_tokens") {
        into.cache_creation_input_tokens = n;
    }
    if let Some(n) = get("cache_read_input_tokens") {
        into.cache_read_input_tokens = n;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NORMAL: &str = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":1200,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":800,\"output_tokens\":1}}}\n",
        "\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n",
        "\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"secret plans\"}}\n",
        "\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"abc\"}}\n",
        "\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
        "\n",
        "event: ping\n",
        "data: {\"type\": \"ping\"}\n",
        "\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
        "\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Add the \"}}\n",
        "\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"parser\"}}\n",
        "\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n",
        "\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":42}}\n",
        "\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n",
        "\n",
    );

    fn run(text: &str) -> Answer {
        let mut p = SseParser::default();
        let mut a = Answer::default();
        for e in p.feed(text) {
            a.apply(&e);
        }
        if let Some(e) = p.finish() {
            a.apply(&e);
        }
        a
    }

    #[test]
    fn normal_answer() {
        let a = run(NORMAL);
        assert_eq!(a.text, "Add the parser");
        assert_eq!(a.model, "claude-opus-5-5");
        assert_eq!(a.message_id, "msg_1");
        assert_eq!(a.usage.input_tokens, 1200);
        assert_eq!(a.usage.cache_read_input_tokens, 800);
        assert_eq!(a.usage.output_tokens, 42);
        assert_eq!(a.stop_reason.as_deref(), Some("end_turn"));
        assert!(a.complete);
        assert!(!a.refused());
        assert_eq!(a.thinking_blocks, 1);
        assert!(!a.text.contains("secret plans"));
        assert_eq!(a.blocks, vec!["thinking", "text"]);
    }

    #[test]
    fn crlf_and_split_chunks_give_the_same_answer() {
        let crlf = NORMAL.replace('\n', "\r\n");
        assert_eq!(run(&crlf).text, "Add the parser");
        let cr = NORMAL.replace('\n', "\r");
        assert_eq!(run(&cr).text, "Add the parser");
        // Every possible split point of the CRLF stream, in two chunks.
        for cut in (0..crlf.len()).step_by(7) {
            if !crlf.is_char_boundary(cut) {
                continue;
            }
            let mut p = SseParser::default();
            let mut a = Answer::default();
            for e in p.feed(&crlf[..cut]).into_iter().chain(p.feed(&crlf[cut..])) {
                a.apply(&e);
            }
            assert_eq!(a.text, "Add the parser", "cut at {cut}");
            assert!(a.complete);
        }
        // Line by line, as the process runner delivers it.
        let mut p = SseParser::default();
        let mut a = Answer::default();
        for line in crlf.split('\n') {
            if let Some(e) = p.push_line(line) {
                a.apply(&e);
            }
        }
        assert_eq!(a.usage.output_tokens, 42);
    }

    #[test]
    fn multi_line_data_and_comments() {
        let mut p = SseParser::default();
        assert!(p.push_line(": keep-alive").is_none());
        assert!(p.push_line("event: x").is_none());
        assert!(p.push_line("data: a").is_none());
        assert!(p.push_line("data:b").is_none());
        let e = p.push_line("").unwrap();
        assert_eq!(e.event, "x");
        assert_eq!(e.data, "a\nb");
        assert!(p.push_line("").is_none());
        // An unterminated last event is delivered by finish().
        p.push_line("event: message_stop");
        p.push_line("data: {\"type\":\"message_stop\"}");
        assert_eq!(p.finish().unwrap().event, "message_stop");
    }

    #[test]
    fn refusal_with_category() {
        let text = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_2\",\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":50,\"output_tokens\":0}}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\",\"stop_details\":{\"type\":\"refusal\",\"category\":\"cyber\",\"explanation\":\"Exploit development\"}},\"usage\":{\"output_tokens\":0}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let a = run(text);
        assert!(a.refused());
        assert_eq!(a.refusal_category.as_deref(), Some("cyber"));
        assert_eq!(
            a.refusal_explanation.as_deref(),
            Some("Exploit development")
        );
        assert!(a.text.is_empty());
    }

    #[test]
    fn fallback_block_switches_the_model() {
        let text = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_3\",\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":10}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"Partial \"}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"fallback\",\"from\":{\"model\":\"claude-opus-5-5\"},\"to\":{\"model\":\"claude-opus-5\"}}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"text_delta\",\"text\":\"continued\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let a = run(text);
        assert_eq!(a.text, "Partial continued");
        assert_eq!(
            a.fallbacks,
            vec![Fallback {
                from: "claude-opus-5-5".into(),
                to: "claude-opus-5".into()
            }]
        );
        assert_eq!(a.model, "claude-opus-5");
    }

    #[test]
    fn error_event_and_max_tokens() {
        let text = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":10}}}\n\n",
            "event: error\n",
            "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n",
        );
        let a = run(text);
        let e = a.error.clone().unwrap();
        assert_eq!(e.kind, "overloaded_error");
        assert_eq!(e.message, "Overloaded");
        assert!(e.retryable());
        assert!(!a.complete);
        let text = "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{\"output_tokens\":16000}}\n\n";
        assert!(run(text).hit_max_tokens());
    }

    #[test]
    fn events_without_names_use_the_data_type() {
        let a = run(
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n\n",
        );
        assert_eq!(a.text, "x");
        // Garbage is ignored.
        let a = run("event: content_block_delta\ndata: {not json\n\n");
        assert!(a.text.is_empty());
    }

    #[test]
    fn error_bodies() {
        let body = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"},"request_id":"req_011"}"#;
        let mut e = parse_error_body(body).unwrap();
        assert_eq!(e.kind, "authentication_error");
        assert_eq!(e.message, "invalid x-api-key");
        assert_eq!(e.request_id.as_deref(), Some("req_011"));
        e.status = Some(401);
        assert!(!e.retryable());
        assert_eq!(
            e.summary(),
            "authentication_error (HTTP 401): invalid x-api-key"
        );
        assert!(e.hint().contains("API key"));
        for (kind, status, retry) in [
            ("rate_limit_error", 429, true),
            ("api_error", 500, true),
            ("overloaded_error", 529, true),
            ("invalid_request_error", 400, false),
            ("permission_error", 403, false),
            ("not_found_error", 404, false),
            ("billing_error", 402, false),
            ("request_too_large", 413, false),
        ] {
            let e = ApiError {
                kind: kind.into(),
                status: Some(status),
                ..ApiError::default()
            };
            assert_eq!(e.retryable(), retry, "{kind}");
            assert!(!e.hint().is_empty(), "{kind}");
        }
        assert!(parse_error_body("not json").is_none());
        assert!(parse_error_body(r#"{"type":"message"}"#).is_none());
    }

    #[test]
    fn http_meta_from_stderr() {
        let m = parse_http_meta(
            "curl: (22) The requested URL returned error: 429\ngitvis-http: 429 retry-after=12 request-id=req_9\n",
            "gitvis-http:",
        );
        assert_eq!(m.status, Some(429));
        assert_eq!(m.retry_after, Some(12));
        assert_eq!(m.request_id.as_deref(), Some("req_9"));
        let m = parse_http_meta(
            "gitvis-http: 200 retry-after= request-id=\n",
            "gitvis-http:",
        );
        assert_eq!(m.status, Some(200));
        assert_eq!(m.retry_after, None);
        assert_eq!(m.request_id, None);
        // No connection at all: http_code 000.
        let m = parse_http_meta("gitvis-http: 000 retry-after= request-id=", "gitvis-http:");
        assert_eq!(m.status, None);
        assert_eq!(parse_http_meta("", "gitvis-http:"), HttpMeta::default());
    }
}
