//! Turning answers into what gitvis fills in or shows: a commit message, a
//! pull request title and body, Markdown sections, the flaky verdict.

/// Remove a wrapping code fence and surrounding quotes the model may add.
fn unwrap(text: &str) -> String {
    let t = text.trim();
    let t = if let Some(rest) = t.strip_prefix("```") {
        let rest = match rest.find('\n') {
            Some(i) => &rest[i + 1..],
            None => rest,
        };
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else {
        t
    };
    let t = t
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(t);
    t.trim().to_owned()
}

/// A commit message as it is filled in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitMessage {
    pub message: String,
    pub subject_chars: usize,
    pub warnings: Vec<String>,
}

pub const SUBJECT_MAX: usize = 72;

/// Normalise an answer into `subject\n\nbody`.
pub fn commit_message(answer: &str) -> CommitMessage {
    let text = unwrap(answer).replace("\r\n", "\n");
    let mut lines = text.lines();
    let subject = lines
        .by_ref()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .trim_start_matches("Subject:")
        .trim()
        .to_owned();
    let body: Vec<&str> = lines.collect();
    let body = body.join("\n").trim().to_owned();
    let mut message = subject.clone();
    if !body.is_empty() {
        message.push_str("\n\n");
        message.push_str(&body);
    }
    let subject_chars = subject.chars().count();
    let mut warnings = Vec::new();
    if subject.is_empty() {
        warnings.push("The answer was empty.".to_owned());
    } else if subject_chars > SUBJECT_MAX {
        warnings.push(format!(
            "The subject is {subject_chars} characters; git tools cut it after {SUBJECT_MAX}."
        ));
    }
    CommitMessage {
        message,
        subject_chars,
        warnings,
    }
}

/// `Title: …` then the body. Without the label the first line is the title.
pub fn pr_description(answer: &str) -> (String, String) {
    let text = unwrap(answer).replace("\r\n", "\n");
    let mut title = String::new();
    let mut rest: Vec<&str> = Vec::new();
    let mut found = false;
    for line in text.lines() {
        if !found {
            let l = line.trim();
            if l.is_empty() {
                continue;
            }
            let l = l.trim_start_matches('#').trim();
            title = l
                .strip_prefix("Title:")
                .or_else(|| l.strip_prefix("**Title:**"))
                .unwrap_or(l)
                .trim()
                .trim_matches('*')
                .trim()
                .to_owned();
            found = true;
            continue;
        }
        rest.push(line);
    }
    (title, rest.join("\n").trim().to_owned())
}

/// A `## Heading` and the text under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub heading: String,
    pub body: String,
}

/// Split Markdown at `#`/`##`/`###` headings; text before the first heading
/// is a section with an empty heading.
pub fn sections(text: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let mut fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fence = !fence;
        }
        let t = line.trim();
        if !fence && t.starts_with('#') {
            let heading = t.trim_start_matches('#').trim();
            if !heading.is_empty() {
                out.push(Section {
                    heading: heading.to_owned(),
                    body: String::new(),
                });
                continue;
            }
        }
        if out.is_empty() {
            out.push(Section {
                heading: String::new(),
                body: String::new(),
            });
        }
        let s = out.last_mut().expect("pushed above");
        s.body.push_str(line);
        s.body.push('\n');
    }
    for s in &mut out {
        s.body = s.body.trim().to_owned();
    }
    out.retain(|s| !(s.heading.is_empty() && s.body.is_empty()));
    out
}

/// The answer to "Flaky?".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flaky {
    Yes,
    No,
    Maybe,
}

impl Flaky {
    pub fn label(self) -> &'static str {
        match self {
            Flaky::Yes => "Looks flaky",
            Flaky::No => "Real failure",
            Flaky::Maybe => "Maybe flaky",
        }
    }
}

/// The first word of the "Flaky?" section.
pub fn flaky(text: &str) -> Option<Flaky> {
    let s = sections(text)
        .into_iter()
        .find(|s| s.heading.to_ascii_lowercase().starts_with("flaky"))?;
    let word: String = s
        .body
        .trim_start_matches(['*', '_', ' '])
        .chars()
        .take_while(|c| c.is_alphabetic())
        .collect();
    match word.to_ascii_lowercase().as_str() {
        "yes" | "likely" | "probably" => Some(Flaky::Yes),
        "no" | "unlikely" => Some(Flaky::No),
        "maybe" | "possibly" | "unclear" => Some(Flaky::Maybe),
        _ => None,
    }
}

/// The first word of the "Risk" section (`Low`, `Medium`, `High`).
pub fn risk(text: &str) -> Option<String> {
    let s = sections(text)
        .into_iter()
        .find(|s| s.heading.to_ascii_lowercase().starts_with("risk"))?;
    let word: String = s
        .body
        .trim_start_matches(['*', '_', ' '])
        .chars()
        .take_while(|c| c.is_alphabetic())
        .collect();
    matches!(
        word.to_ascii_lowercase().as_str(),
        "low" | "medium" | "high"
    )
    .then_some(word)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_messages() {
        let m = commit_message("Add the parser\n\nIt reads SSE.\n");
        assert_eq!(m.message, "Add the parser\n\nIt reads SSE.");
        assert_eq!(m.subject_chars, 14);
        assert!(m.warnings.is_empty());
        // Fences, quotes, a missing blank line, CRLF.
        let m = commit_message("```text\r\nfix: handle CRLF\r\nBody line\r\n```");
        assert_eq!(m.message, "fix: handle CRLF\n\nBody line");
        let m = commit_message("\"Subject only\"");
        assert_eq!(m.message, "Subject only");
        let long = "x".repeat(80);
        let m = commit_message(&long);
        assert_eq!(m.subject_chars, 80);
        assert!(m.warnings[0].contains("80 characters"));
        assert_eq!(commit_message("  ").warnings, vec!["The answer was empty."]);
    }

    #[test]
    fn pr_descriptions() {
        let (t, b) = pr_description("Title: Add login\n\n## Why\nUsers asked.\n");
        assert_eq!(t, "Add login");
        assert_eq!(b, "## Why\nUsers asked.");
        let (t, b) = pr_description("**Title:** Fix it\nBody");
        assert_eq!(t, "Fix it");
        assert_eq!(b, "Body");
        let (t, b) = pr_description("Plain first line\n\nrest");
        assert_eq!(t, "Plain first line");
        assert_eq!(b, "rest");
        assert_eq!(pr_description(""), (String::new(), String::new()));
    }

    #[test]
    fn markdown_sections_and_verdicts() {
        let text = "Intro\n## Likely cause\nThe test timed out.\n## Evidence\n> 12 | boom\n```\n# not a heading\n```\n## Flaky?\n**Yes** — a network timeout.\n";
        let s = sections(text);
        assert_eq!(s.len(), 4);
        assert_eq!(s[0].heading, "");
        assert_eq!(s[0].body, "Intro");
        assert_eq!(s[1].heading, "Likely cause");
        assert!(s[2].body.contains("# not a heading"));
        assert_eq!(flaky(text), Some(Flaky::Yes));
        assert_eq!(flaky("## Flaky?\nNo, a real bug."), Some(Flaky::No));
        assert_eq!(flaky("## Flaky?\nMaybe."), Some(Flaky::Maybe));
        assert_eq!(flaky("## Cause\nx"), None);
        assert_eq!(
            risk("## Risk\nMedium — touches parsing."),
            Some("Medium".into())
        );
        assert_eq!(risk("## Risk\nunknown"), None);
    }
}
