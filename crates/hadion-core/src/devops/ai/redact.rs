//! Remove secrets from anything sent to the API, with Hadion's own
//! scanner ([`crate::secrets`]): every value a rule finds becomes
//! `[REDACTED:<rule>]`, whole private-key blocks are replaced, and files
//! with sensitive names (`.env`, `id_rsa`, …) are withheld from diffs.

use std::collections::BTreeMap;

use crate::secrets;

/// Lines are cut to this many characters before scanning: the generic
/// rules skip longer lines, and minified lines tell the model nothing.
pub const MAX_LINE_CHARS: usize = 1_500;

/// What was removed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Redactions {
    /// Rule id → values replaced.
    pub by_rule: BTreeMap<String, usize>,
    /// Diff sections left out because of the file name: `(path, rule)`.
    pub withheld: Vec<(String, String)>,
    /// Lines cut to [`MAX_LINE_CHARS`].
    pub cut_lines: usize,
}

impl Redactions {
    pub fn values(&self) -> usize {
        self.by_rule.values().sum()
    }

    pub fn is_empty(&self) -> bool {
        self.by_rule.is_empty() && self.withheld.is_empty()
    }

    pub fn merge(&mut self, o: &Redactions) {
        for (k, v) in &o.by_rule {
            *self.by_rule.entry(k.clone()).or_insert(0) += v;
        }
        self.withheld.extend(o.withheld.iter().cloned());
        self.cut_lines += o.cut_lines;
    }

    /// `3 values (aws-access-key ×2, github-pat)`, plus withheld files.
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "nothing".to_owned();
        }
        let mut parts = Vec::new();
        let n = self.values();
        if n > 0 {
            let rules: Vec<String> = self
                .by_rule
                .iter()
                .map(|(r, c)| {
                    if *c > 1 {
                        format!("{r} ×{c}")
                    } else {
                        r.clone()
                    }
                })
                .collect();
            parts.push(format!(
                "{n} {} ({})",
                if n == 1 { "value" } else { "values" },
                rules.join(", ")
            ));
        }
        if !self.withheld.is_empty() {
            let files: Vec<&str> = self.withheld.iter().map(|(p, _)| p.as_str()).collect();
            parts.push(format!("file contents withheld: {}", files.join(", ")));
        }
        parts.join("; ")
    }
}

fn is_pem_begin(line: &str) -> bool {
    line.contains("-----BEGIN") && line.contains("PRIVATE KEY")
}

fn is_pem_end(line: &str) -> bool {
    line.contains("-----END") && line.contains("PRIVATE KEY")
}

/// Cut a line to [`MAX_LINE_CHARS`] characters.
fn cut(line: &str, r: &mut Redactions) -> String {
    match line.char_indices().nth(MAX_LINE_CHARS) {
        Some((i, _)) => {
            r.cut_lines += 1;
            let rest = line[i..].chars().count();
            format!("{} [… {rest} more characters not sent]", &line[..i])
        }
        None => line.to_owned(),
    }
}

/// Redact one line of `path`.
fn line(text: &str, path: &str, r: &mut Redactions) -> String {
    let rules = secrets::rules();
    let mut hits = secrets::scan_line(rules, path, text);
    if hits.is_empty() {
        return text.to_owned();
    }
    hits.sort_by_key(|h| std::cmp::Reverse(h.start));
    let mut out = text.to_owned();
    for h in hits {
        if h.end > out.len() || !out.is_char_boundary(h.start) || !out.is_char_boundary(h.end) {
            continue;
        }
        let id = rules.rules[h.rule].id;
        out.replace_range(h.start..h.end, &format!("[REDACTED:{id}]"));
        *r.by_rule.entry(id.to_owned()).or_insert(0) += 1;
    }
    out
}

/// Redact free text (a log, a pull request body); `path` gives the rules
/// that depend on the file name a context (`ci.log`, `pr-body.md`).
pub fn text(input: &str, path: &str) -> (String, Redactions) {
    let mut r = Redactions::default();
    let mut out = String::with_capacity(input.len());
    let mut in_pem = false;
    for raw in input.split_inclusive('\n') {
        let (body, nl) = match raw.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (raw, ""),
        };
        let body = cut(body, &mut r);
        if in_pem {
            if is_pem_end(&body) {
                in_pem = false;
            }
            continue;
        }
        if is_pem_begin(&body) {
            *r.by_rule.entry("private-key".to_owned()).or_insert(0) += 1;
            out.push_str("[REDACTED:private-key]");
            out.push_str(nl);
            in_pem = !is_pem_end(&body);
            continue;
        }
        out.push_str(&line(&body, path, &mut r));
        out.push_str(nl);
    }
    (out, r)
}

/// The path a `diff --git a/x b/y` header names (the new side).
fn diff_header_path(l: &str) -> Option<String> {
    let rest = l.strip_prefix("diff --git ")?;
    // `a/path b/path`, possibly quoted; take the part after " b/".
    if let Some(i) = rest.rfind(" b/") {
        return Some(rest[i + 3..].trim_matches('"').to_owned());
    }
    rest.split_whitespace()
        .last()
        .map(|s| s.trim_matches('"').trim_start_matches("b/").to_owned())
}

/// Redact a unified diff: each file section is scanned with its own path;
/// sections of files with sensitive names are replaced by one line.
pub fn diff(input: &str) -> (String, Redactions) {
    let mut r = Redactions::default();
    let mut out = String::with_capacity(input.len());
    let mut path = String::new();
    let mut withhold = false;
    let mut in_pem = false;
    for raw in input.split_inclusive('\n') {
        let (body, nl) = match raw.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (raw, ""),
        };
        if let Some(p) = diff_header_path(body) {
            path = p;
            in_pem = false;
            withhold = false;
            if let Some(rule) = secrets::check_path(&path) {
                withhold = true;
                r.withheld.push((path.clone(), rule.id.to_owned()));
                out.push_str(body);
                out.push_str(nl);
                out.push_str(&format!(
                    "[REDACTED FILE: {path} matches the sensitive file rule \"{}\"; its changes were not sent]\n",
                    rule.name
                ));
            } else {
                out.push_str(body);
                out.push_str(nl);
            }
            continue;
        }
        if withhold {
            continue;
        }
        let body = cut(body, &mut r);
        if in_pem {
            if is_pem_end(&body) {
                in_pem = false;
            }
            // Keep the diff marker so the hunk stays readable.
            continue;
        }
        if is_pem_begin(&body) {
            *r.by_rule.entry("private-key".to_owned()).or_insert(0) += 1;
            let marker = body.chars().next().filter(|c| matches!(c, '+' | '-' | ' '));
            if let Some(m) = marker {
                out.push(m);
            }
            out.push_str("[REDACTED:private-key]");
            out.push_str(nl);
            in_pem = !is_pem_end(&body);
            continue;
        }
        let scan_path = if path.is_empty() { "diff" } else { &path };
        out.push_str(&line(&body, scan_path, &mut r));
        out.push_str(nl);
    }
    (out, r)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Built at run time so this file holds no scanner-matching literal.
    fn aws() -> String {
        format!("AKIA{}", "Q7ZK4WJ2MX9PLR3T")
    }

    fn ghp() -> String {
        format!("ghp_{}", "x8Kq2Lm9Vb7Nc4Zr1Td6Yh3Wp5Fs0Gj8Ue2A")
    }

    #[test]
    fn values_are_replaced_with_the_rule() {
        let input = format!(
            "fn main() {{}}\nlet key = \"{}\";\ntoken: {}\nplain line\n",
            aws(),
            ghp()
        );
        let (out, r) = text(&input, "src/main.rs");
        assert!(!out.contains(&aws()), "{out}");
        assert!(!out.contains(&ghp()), "{out}");
        assert!(out.contains("[REDACTED:"), "{out}");
        assert!(out.contains("plain line\n"));
        assert_eq!(r.values(), 2, "{:?}", r.by_rule);
        assert!(r.summary().starts_with("2 values ("), "{}", r.summary());
        // Line structure is kept.
        assert_eq!(out.lines().count(), input.lines().count());
    }

    #[test]
    fn private_key_blocks_go_entirely() {
        let input = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA\nabcdefgh\n-----END RSA PRIVATE KEY-----\nafter\n";
        let (out, r) = text(input, "notes.txt");
        assert_eq!(out, "before\n[REDACTED:private-key]\nafter\n");
        assert_eq!(r.by_rule.get("private-key"), Some(&1));
        // In a diff the + marker stays.
        let d = "diff --git a/k.txt b/k.txt\n+++ b/k.txt\n@@ -0,0 +1,3 @@\n+-----BEGIN OPENSSH PRIVATE KEY-----\n+b3BlbnNzaC1rZXk\n+-----END OPENSSH PRIVATE KEY-----\n+ok\n";
        let (out, _) = diff(d);
        assert!(out.contains("+[REDACTED:private-key]\n+ok\n"), "{out}");
        assert!(!out.contains("b3BlbnNzaC1rZXk"));
    }

    #[test]
    fn sensitive_files_are_withheld_from_diffs() {
        let d = format!(
            "diff --git a/.env b/.env\nnew file mode 100644\n--- /dev/null\n+++ b/.env\n@@ -0,0 +1 @@\n+DB_URL=postgres://u:hunter@db/x\ndiff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+let k = \"{}\";\n",
            aws()
        );
        let (out, r) = diff(&d);
        assert!(!out.contains("postgres://"), "{out}");
        assert!(out.contains("[REDACTED FILE: .env"), "{out}");
        assert!(out.contains("diff --git a/src/a.rs b/src/a.rs"));
        assert!(out.contains("-old\n"));
        assert!(!out.contains(&aws()));
        assert_eq!(r.withheld.len(), 1);
        assert_eq!(r.withheld[0].0, ".env");
        assert!(r.summary().contains("withheld: .env"), "{}", r.summary());
    }

    #[test]
    fn long_lines_are_cut_before_scanning() {
        let long = format!("{}{}", "x".repeat(3000), aws());
        let (out, r) = text(&long, "bundle.js");
        assert!(!out.contains(&aws()));
        assert!(out.contains("more characters not sent"));
        assert_eq!(r.cut_lines, 1);
    }

    #[test]
    fn nothing_to_redact() {
        let (out, r) = text("hello\nworld", "a.txt");
        assert_eq!(out, "hello\nworld");
        assert!(r.is_empty());
        assert_eq!(r.summary(), "nothing");
        let mut a = Redactions::default();
        a.by_rule.insert("x".into(), 2);
        let mut b = Redactions::default();
        b.by_rule.insert("x".into(), 1);
        b.cut_lines = 3;
        a.merge(&b);
        assert_eq!(a.values(), 3);
        assert_eq!(a.cut_lines, 3);
    }

    #[test]
    fn header_paths() {
        assert_eq!(
            diff_header_path("diff --git a/src/x y.rs b/src/x y.rs").as_deref(),
            Some("src/x y.rs")
        );
        assert_eq!(diff_header_path("+++ b/x"), None);
    }
}
