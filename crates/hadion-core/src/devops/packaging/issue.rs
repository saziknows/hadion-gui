//! Prefilled "new issue" links: `https://github.com/<o>/<r>/issues/new?
//! title=…&body=…`. Nothing is sent by Hadion; the browser opens the form
//! and the user decides whether to submit it.
//!
//! Browsers and GitHub refuse very long URLs, so the body is cut until its
//! percent-encoded form fits [`MAX_BODY_ENCODED`] bytes (about 6 KB),
//! preferably at a line break, with a note saying so.

use super::release::valid_repo;

/// Longest percent-encoded body.
pub const MAX_BODY_ENCODED: usize = 6 * 1024;

/// What goes into the URL, decoded, plus the URL itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueLink {
    pub url: String,
    pub repo: String,
    pub title: String,
    /// The body exactly as it goes into the URL (after any cut).
    pub body: String,
    pub truncated: bool,
}

/// Percent-encode a query value (RFC 3986 unreserved characters kept,
/// everything else as UTF-8 `%XX`).
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
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

fn encoded_len(s: &str) -> usize {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                1
            } else {
                3
            }
        })
        .sum()
}

/// Cut `body` so that it, plus `note`, encodes to at most `max` bytes.
/// Returns the (possibly) shortened body and whether it was cut.
pub fn fit(body: &str, note: &str, max: usize) -> (String, bool) {
    if encoded_len(body) <= max {
        return (body.to_owned(), false);
    }
    let budget = max.saturating_sub(encoded_len(note));
    let mut used = 0usize;
    let mut end = 0usize;
    let mut last_newline = None;
    for (i, c) in body.char_indices() {
        let mut buf = [0u8; 4];
        let n = encoded_len(c.encode_utf8(&mut buf));
        if used + n > budget {
            break;
        }
        used += n;
        end = i + c.len_utf8();
        if c == '\n' {
            last_newline = Some(end);
        }
    }
    // Prefer a whole line unless that would throw away most of the text.
    let cut = match last_newline {
        Some(nl) if nl * 4 >= end * 3 => nl,
        _ => end,
    };
    let mut out = body.get(..cut).unwrap_or("").to_owned();
    out.push_str(note);
    (out, true)
}

/// Build the link. `note` is appended when the body had to be cut.
pub fn new_issue(
    repo: &str,
    title: &str,
    body: &str,
    note: &str,
    max_body: usize,
) -> Result<IssueLink, String> {
    let repo = repo.trim();
    if !valid_repo(repo) {
        return Err(format!(
            "\"{repo}\" is not an owner/repository name; set the update source in Settings → Updates"
        ));
    }
    let (body, truncated) = fit(body, note, max_body);
    let url = format!(
        "https://github.com/{repo}/issues/new?title={}&body={}",
        encode(title),
        encode(&body)
    );
    Ok(IssueLink {
        url,
        repo: repo.to_owned(),
        title: title.to_owned(),
        body,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_query_values() {
        assert_eq!(encode("a b&c=d/é\n"), "a%20b%26c%3Dd%2F%C3%A9%0A");
        assert_eq!(encode("Crash: x.rs:1"), "Crash%3A%20x.rs%3A1");
        assert_eq!(encoded_len("a b/é"), encode("a b/é").len());
    }

    #[test]
    fn short_bodies_are_kept_whole() {
        let l = new_issue("o/r", "Crash: boom", "line 1\nline 2", "\n(cut)", 6144).unwrap();
        assert_eq!(
            l.url,
            "https://github.com/o/r/issues/new?title=Crash%3A%20boom&body=line%201%0Aline%202"
        );
        assert!(!l.truncated);
        assert_eq!(l.body, "line 1\nline 2");
        assert!(new_issue("not a repo", "t", "b", "", 100).is_err());
        assert!(new_issue("", "t", "b", "", 100).is_err());
    }

    #[test]
    fn long_bodies_are_cut_at_a_line_within_the_budget() {
        let body: String = (0..2_000)
            .map(|i| format!("   {i}: hadion::frame::function_{i} at src/x.rs:{i}\n"))
            .collect();
        let note = "\n… (cut to fit a link; the full report is on your machine)";
        let l = new_issue("o/r", "Crash", &body, note, MAX_BODY_ENCODED).unwrap();
        assert!(l.truncated);
        assert!(l.body.ends_with(note));
        let kept = l.body.strip_suffix(note).unwrap();
        assert!(kept.ends_with('\n'), "cut at a line break");
        assert!(body.starts_with(kept));
        let encoded = encode(&l.body).len();
        assert!(encoded <= MAX_BODY_ENCODED, "{encoded}");
        assert!(
            encoded > MAX_BODY_ENCODED - 200,
            "uses the budget: {encoded}"
        );
        // Multi-byte characters are never split.
        let (s, cut) = fit(&"é".repeat(5_000), "", 600);
        assert!(cut && s.chars().all(|c| c == 'é') && encode(&s).len() <= 600);
        // One huge line: cut inside it.
        let (s, cut) = fit(&"x".repeat(10_000), "!", 100);
        assert!(cut);
        assert_eq!(s.len(), 98);
        assert!(s.ends_with('!'));
    }
}
