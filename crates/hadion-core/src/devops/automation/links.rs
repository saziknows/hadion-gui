//! Issue-tracker keys in commit messages, branch names and pull requests:
//! Jira (`ABC-123` → `https://acme.atlassian.net/browse/ABC-123`), Linear
//! (`ENG-42` → `https://linear.app/<workspace>/issue/ENG-42`) and custom
//! patterns (a regular expression and a URL template).
//!
//! Detection is deliberately strict:
//! - With project / team keys configured, only those prefixes match
//!   (case-insensitively in branch names, where `abc-123-login` is common).
//! - Without keys, an upper-case `KEY-123` matches unless its prefix is a
//!   well-known technical acronym (`UTF-8`, `SHA-256`, `ISO-8601`, `RFC-2616`,
//!   `CVE-2024`, …), and branch names only match upper-case keys.

use std::collections::{HashMap, HashSet};

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Jira Cloud or Server.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct JiraPrefs {
    /// `https://acme.atlassian.net`.
    pub base_url: String,
    /// Project keys (`ABC`); empty: any key that is not an acronym.
    pub keys: Vec<String>,
    /// Account e-mail for the title lookup (the API token is in the keychain).
    pub email: String,
    /// Look up titles and status (off by default; needs the e-mail and token).
    pub lookup: bool,
}

/// Linear.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LinearPrefs {
    /// The workspace slug in `https://linear.app/<slug>/…`.
    pub workspace: String,
    /// Team keys (`ENG`); empty: any key that is not an acronym.
    pub keys: Vec<String>,
    /// Look up titles and status (off by default; the API key is in the keychain).
    pub lookup: bool,
}

/// A custom tracker: a regular expression and a URL template. The template
/// may use `{0}` (the whole match), `{1}`…`{9}` (groups) and `{key}` (group
/// 1, or the whole match without groups).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomRule {
    pub name: String,
    pub pattern: String,
    pub url: String,
}

/// Where "Add ABC-123" puts the key in the commit message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Placement {
    /// `ABC-123: Fix the login page`.
    #[default]
    Prefix,
    /// A last line `ABC-123`.
    Suffix,
}

/// Every issue-tracker setting.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LinkPrefs {
    pub jira: JiraPrefs,
    pub linear: LinearPrefs,
    pub custom: Vec<CustomRule>,
    pub placement: Placement,
}

impl LinkPrefs {
    /// Any tracker set up at all.
    pub fn any(&self) -> bool {
        self.jira_on() || self.linear_on() || self.custom.iter().any(|c| c.usable())
    }

    pub fn jira_on(&self) -> bool {
        !self.jira.base_url.trim().is_empty()
    }

    pub fn linear_on(&self) -> bool {
        !linear_slug(&self.linear.workspace).is_empty()
    }
}

impl CustomRule {
    /// Both a pattern and a URL are filled in.
    pub fn usable(&self) -> bool {
        !self.pattern.trim().is_empty() && !self.url.trim().is_empty()
    }
}

/// Which tracker a link goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tracker {
    Jira,
    Linear,
    /// Index into [`LinkPrefs::custom`].
    Custom(usize),
}

impl Tracker {
    pub fn label(self, prefs: &LinkPrefs) -> String {
        match self {
            Tracker::Jira => "Jira".to_owned(),
            Tracker::Linear => "Linear".to_owned(),
            Tracker::Custom(i) => prefs
                .custom
                .get(i)
                .map(|c| c.name.trim())
                .filter(|n| !n.is_empty())
                .unwrap_or("Custom")
                .to_owned(),
        }
    }

    /// Can titles be looked up for it?
    pub fn lookup_supported(self) -> bool {
        matches!(self, Tracker::Jira | Tracker::Linear)
    }
}

/// One key found in a text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// `ABC-123` (upper-case for Jira and Linear), or the matched text for
    /// custom rules.
    pub key: String,
    /// What a chip shows: the key, or for custom rules with a group
    /// `NAME #GROUP` (`Bugzilla #42`).
    pub label: String,
    pub tracker: Tracker,
    pub url: String,
    /// Byte range of the match in the text.
    pub start: usize,
    pub end: usize,
}

/// What kind of text is searched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Commit messages, pull request titles and bodies.
    Text,
    /// Branch names (`feature/abc-123-login`): lower-case keys count when
    /// the prefix is configured.
    Branch,
}

/// Prefixes that look like keys but are standards, algorithms or formats.
/// Only consulted when no project keys are configured.
pub const ACRONYMS: &[&str] = &[
    "UTF", "UCS", "SHA", "MD", "ISO", "IEC", "RFC", "CVE", "CWE", "GHSA", "HTTP", "HTTPS", "TLS",
    "SSL", "AES", "RSA", "DES", "ECDSA", "ED", "PEP", "ES", "ECMA", "IEEE", "ANSI", "ASCII", "UTC",
    "GMT", "CET", "PST", "EST", "IPV", "IP", "WIN", "ARM", "AMD", "X86", "GPL", "LGPL", "AGPL",
    "MPL", "BSD", "CC", "APACHE", "LTS", "RC", "VS", "TC", "COVID", "USB", "HDMI", "DDR", "PCI",
    "MP", "H", "AV", "CP", "WINDOWS", "LATIN", "BASE", "FIPS", "NIST", "OWASP", "OAUTH", "SAML",
    "JSON", "YAML", "TOML", "XML", "CSS", "HTML", "PHP", "PY", "NODE", "GO", "JS", "TS", "EC", "P",
    "TOP", "PART", "STEP", "PHASE", "VERSION", "V", "API", "SDK", "OS", "IOS", "MACOS", "EN", "DE",
    "FR", "ZH", "JA", "UPDATE",
];

fn key_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b([A-Za-z][A-Za-z0-9]{1,9})-([0-9]{1,7})\b").unwrap())
}

/// The workspace slug from what the user typed (`acme`, `linear.app/acme`,
/// `https://linear.app/acme/`).
pub fn linear_slug(input: &str) -> String {
    let s = input.trim().trim_end_matches('/');
    let s = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let s = s.strip_prefix("linear.app/").unwrap_or(s);
    s.split('/').next().unwrap_or("").trim().to_owned()
}

/// `https://acme.atlassian.net` from `acme.atlassian.net/`,
/// `https://acme.atlassian.net/browse/` and the like.
pub fn jira_base(input: &str) -> String {
    let mut s = input.trim().trim_end_matches('/').to_owned();
    for tail in ["/browse", "/jira"] {
        if let Some(t) = s.strip_suffix(tail) {
            s = t.to_owned();
        }
    }
    if s.is_empty() {
        return s;
    }
    if !s.starts_with("http://") && !s.starts_with("https://") {
        s = format!("https://{s}");
    }
    s
}

/// `{base}/browse/{KEY}`.
pub fn jira_url(base: &str, key: &str) -> String {
    format!("{}/browse/{}", jira_base(base), key)
}

/// `https://linear.app/{workspace}/issue/{KEY}`.
pub fn linear_url(workspace: &str, key: &str) -> String {
    format!(
        "https://linear.app/{}/issue/{}",
        linear_slug(workspace),
        key
    )
}

/// Fill a custom URL template from a regex match.
pub fn fill_template(template: &str, caps: &regex::Captures) -> String {
    let mut out = template.to_owned();
    let whole = caps.get(0).map(|m| m.as_str()).unwrap_or("");
    let key = caps.get(1).map(|m| m.as_str()).unwrap_or(whole);
    out = out.replace("{key}", key);
    for i in (0..10).rev() {
        let v = caps.get(i).map(|m| m.as_str()).unwrap_or("");
        out = out.replace(&format!("{{{i}}}"), v);
    }
    out
}

fn upper_set(keys: &[String]) -> HashSet<String> {
    keys.iter()
        .map(|k| k.trim().to_ascii_uppercase())
        .filter(|k| !k.is_empty())
        .collect()
}

/// Is `prefix` upper-case letters and digits (`ABC`, `X86`)?
fn is_upper_key(prefix: &str) -> bool {
    prefix
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// Custom rules that compiled: `(index, regex)`.
pub type Compiled = Vec<(usize, Regex)>;
/// Custom rules that did not: `(index, message)`.
pub type RuleErrors = Vec<(usize, String)>;

/// Compile the custom rules once; invalid ones come back as errors (index,
/// message) for the settings page.
pub fn compile_custom(rules: &[CustomRule]) -> (Compiled, RuleErrors) {
    let mut ok = Vec::new();
    let mut bad = Vec::new();
    for (i, r) in rules.iter().enumerate() {
        if !r.usable() {
            continue;
        }
        match Regex::new(r.pattern.trim()) {
            Ok(re) => ok.push((i, re)),
            Err(e) => bad.push((i, e.to_string())),
        }
    }
    (ok, bad)
}

/// Every key in `text`, in order of appearance, each key once.
pub fn find(text: &str, prefs: &LinkPrefs, source: Source) -> Vec<Link> {
    let mut out: Vec<Link> = Vec::new();
    let jira = prefs.jira_on();
    let linear = prefs.linear_on();
    if jira || linear {
        let jira_keys = upper_set(&prefs.jira.keys);
        let linear_keys = upper_set(&prefs.linear.keys);
        let configured = !jira_keys.is_empty() || !linear_keys.is_empty();
        for caps in key_regex().captures_iter(text) {
            let (Some(m), Some(pre), Some(num)) = (caps.get(0), caps.get(1), caps.get(2)) else {
                continue;
            };
            let prefix_raw = pre.as_str();
            let prefix = prefix_raw.to_ascii_uppercase();
            let upper = is_upper_key(prefix_raw);
            let tracker = if configured {
                // Only configured prefixes; lower-case only in branch names.
                if !upper && source != Source::Branch {
                    continue;
                }
                if jira && jira_keys.contains(&prefix) {
                    Tracker::Jira
                } else if linear && linear_keys.contains(&prefix) {
                    Tracker::Linear
                } else if jira && jira_keys.is_empty() && !linear_keys.contains(&prefix) && upper {
                    // Linear has keys, Jira takes any other upper-case key.
                    if ACRONYMS.contains(&prefix.as_str()) {
                        continue;
                    }
                    Tracker::Jira
                } else if linear && linear_keys.is_empty() && upper {
                    if ACRONYMS.contains(&prefix.as_str()) {
                        continue;
                    }
                    Tracker::Linear
                } else {
                    continue;
                }
            } else {
                if !upper || ACRONYMS.contains(&prefix.as_str()) {
                    continue;
                }
                if jira { Tracker::Jira } else { Tracker::Linear }
            };
            let key = format!("{prefix}-{}", num.as_str());
            if out.iter().any(|l| l.key == key) {
                continue;
            }
            let url = match tracker {
                Tracker::Jira => jira_url(&prefs.jira.base_url, &key),
                _ => linear_url(&prefs.linear.workspace, &key),
            };
            out.push(Link {
                label: key.clone(),
                key,
                tracker,
                url,
                start: m.start(),
                end: m.end(),
            });
        }
    }
    let (rules, _) = compile_custom(&prefs.custom);
    for (i, re) in rules {
        for caps in re.captures_iter(text) {
            let Some(m) = caps.get(0) else { continue };
            if m.as_str().is_empty() {
                continue;
            }
            // A custom rule never repeats a Jira / Linear key.
            if out
                .iter()
                .any(|l| l.start < m.end() && m.start() < l.end && l.tracker != Tracker::Custom(i))
            {
                continue;
            }
            let key = m.as_str().to_owned();
            if out
                .iter()
                .any(|l| l.tracker == Tracker::Custom(i) && l.key == key)
            {
                continue;
            }
            let label = match caps.get(1).map(|g| g.as_str()).filter(|g| !g.is_empty()) {
                Some(g) => {
                    let name = Tracker::Custom(i).label(prefs);
                    if g.chars().all(|c| c.is_ascii_digit()) {
                        format!("{name} #{g}")
                    } else {
                        format!("{name} {g}")
                    }
                }
                None => key.clone(),
            };
            out.push(Link {
                label,
                key,
                tracker: Tracker::Custom(i),
                url: fill_template(prefs.custom[i].url.trim(), &caps),
                start: m.start(),
                end: m.end(),
            });
        }
    }
    out.sort_by_key(|l| l.start);
    out
}

/// Keys in several texts (title, body, branch), each once, in order.
pub fn find_all(texts: &[(&str, Source)], prefs: &LinkPrefs) -> Vec<Link> {
    let mut out: Vec<Link> = Vec::new();
    for (t, src) in texts {
        for l in find(t, prefs, *src) {
            if !out.iter().any(|o| o.key == l.key && o.tracker == l.tracker) {
                out.push(l);
            }
        }
    }
    out
}

/// The text `AddToCommitMessage` gets for a key: `ABC-123: ` at the start,
/// or `ABC-123` on a last line.
pub fn message_insert(key: &str, placement: Placement) -> (String, bool) {
    match placement {
        Placement::Prefix => (format!("{key}: "), true),
        Placement::Suffix => (key.to_owned(), false),
    }
}

/// Does a commit message already mention `key` (case-insensitively, as a
/// whole key)?
pub fn mentions(message: &str, key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    key_regex().captures_iter(message).any(|c| {
        let (Some(p), Some(n)) = (c.get(1), c.get(2)) else {
            return false;
        };
        format!("{}-{}", p.as_str().to_ascii_uppercase(), n.as_str()) == k
    })
}

/// Project keys that look like issue keys in these texts (recent commit
/// summaries, branch names), most frequent first: a prefix needs at least
/// two distinct issue numbers and must not be an acronym. `known` are left
/// out.
pub fn suggest_keys<'a>(
    texts: impl IntoIterator<Item = &'a str>,
    known: &[String],
    max: usize,
) -> Vec<(String, usize)> {
    let known = upper_set(known);
    let mut numbers: HashMap<String, HashSet<String>> = HashMap::new();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for t in texts {
        for c in key_regex().captures_iter(t) {
            let (Some(p), Some(n)) = (c.get(1), c.get(2)) else {
                continue;
            };
            let prefix = p.as_str();
            if !is_upper_key(prefix)
                || prefix.chars().all(|c| c.is_ascii_digit())
                || ACRONYMS.contains(&prefix)
                || known.contains(prefix)
            {
                continue;
            }
            numbers
                .entry(prefix.to_owned())
                .or_default()
                .insert(n.as_str().to_owned());
            *counts.entry(prefix.to_owned()).or_default() += 1;
        }
    }
    let mut out: Vec<(String, usize)> = counts
        .into_iter()
        .filter(|(p, _)| numbers.get(p).is_some_and(|s| s.len() >= 2))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out.truncate(max);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jira(keys: &[&str]) -> LinkPrefs {
        LinkPrefs {
            jira: JiraPrefs {
                base_url: "acme.atlassian.net/".into(),
                keys: keys.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn keys(links: &[Link]) -> Vec<&str> {
        links.iter().map(|l| l.key.as_str()).collect()
    }

    #[test]
    fn finds_jira_keys_without_acronyms() {
        let p = jira(&[]);
        let text = "ABC-123: use UTF-8 and SHA-256 per ISO-8601 (RFC-2616, CVE-2024-1234) for XY-9";
        let got = find(text, &p, Source::Text);
        assert_eq!(keys(&got), vec!["ABC-123", "XY-9"]);
        assert_eq!(got[0].url, "https://acme.atlassian.net/browse/ABC-123");
        assert_eq!(got[0].tracker, Tracker::Jira);
        assert_eq!(&text[got[0].start..got[0].end], "ABC-123");
    }

    #[test]
    fn no_false_positives_in_prose() {
        let p = jira(&[]);
        for t in [
            "Encode as UTF-8",
            "hash with SHA-256 or SHA-1",
            "dates in ISO-8601",
            "see RFC-2616",
            "fixes CVE-2024-3094",
            "x86-64 and X86-64 builds",
            "abc-123 lower case in prose",
            "foo_ABC-123 inside a word",
            "ABC-123x glued to letters",
            "A-1 single letter",
            "PEP-8, ES-2015, TLS-13, AES-128, MD-5",
            "COVID-19",
        ] {
            assert!(find(t, &p, Source::Text).is_empty(), "{t}");
        }
    }

    #[test]
    fn configured_keys_are_exact() {
        let p = jira(&["abc", "WEB"]);
        let got = find("ABC-1 DEF-2 web-3 WEB-4 UTF-8", &p, Source::Text);
        assert_eq!(
            keys(&got),
            vec!["ABC-1", "WEB-4"],
            "lower-case only in branches"
        );
        let b = find("feature/abc-77-login-page", &p, Source::Branch);
        assert_eq!(keys(&b), vec!["ABC-77"]);
        assert!(find("feature/fix-2-bugs", &p, Source::Branch).is_empty());
        // Without configured keys, branch names need upper case.
        let open = jira(&[]);
        assert!(find("feature/abc-77-login", &open, Source::Branch).is_empty());
        assert_eq!(
            keys(&find("feature/ABC-77-login", &open, Source::Branch)),
            vec!["ABC-77"]
        );
    }

    #[test]
    fn linear_and_jira_together() {
        let mut p = jira(&["ABC"]);
        p.linear = LinearPrefs {
            workspace: "https://linear.app/acme/".into(),
            keys: vec!["ENG".into()],
            lookup: false,
        };
        let got = find("ENG-42 and ABC-7, ZZZ-1", &p, Source::Text);
        assert_eq!(keys(&got), vec!["ENG-42", "ABC-7"]);
        assert_eq!(got[0].tracker, Tracker::Linear);
        assert_eq!(got[0].url, "https://linear.app/acme/issue/ENG-42");
        // Linear only, no keys: any non-acronym upper-case key.
        let only = LinkPrefs {
            linear: LinearPrefs {
                workspace: "acme".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let got = find("ENG-1 UTF-8", &only, Source::Text);
        assert_eq!(keys(&got), vec!["ENG-1"]);
        assert_eq!(got[0].url, "https://linear.app/acme/issue/ENG-1");
        // Jira takes the rest when Linear has keys and Jira has none.
        let mut mixed = jira(&[]);
        mixed.linear = LinearPrefs {
            workspace: "acme".into(),
            keys: vec!["ENG".into()],
            lookup: false,
        };
        let got = find("ENG-1 OPS-2 SHA-1", &mixed, Source::Text);
        assert_eq!(got[0].tracker, Tracker::Linear);
        assert_eq!(got[1].tracker, Tracker::Jira);
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn nothing_without_a_tracker() {
        assert!(find("ABC-123", &LinkPrefs::default(), Source::Text).is_empty());
        assert!(!LinkPrefs::default().any());
    }

    #[test]
    fn custom_rules_and_templates() {
        let p = LinkPrefs {
            custom: vec![
                CustomRule {
                    name: "Bugzilla".into(),
                    pattern: r"bug #?(\d+)".into(),
                    url: "https://bugs.example.com/show_bug.cgi?id={1}".into(),
                },
                CustomRule {
                    name: "Broken".into(),
                    pattern: r"(".into(),
                    url: "x".into(),
                },
            ],
            ..Default::default()
        };
        let got = find("Fixes bug #42 and bug 7; bug #42 again", &p, Source::Text);
        assert_eq!(keys(&got), vec!["bug #42", "bug 7"]);
        assert_eq!(got[0].url, "https://bugs.example.com/show_bug.cgi?id=42");
        assert_eq!(got[0].label, "Bugzilla #42");
        assert_eq!(got[0].tracker, Tracker::Custom(0));
        assert_eq!(Tracker::Custom(0).label(&p), "Bugzilla");
        let (_, bad) = compile_custom(&p.custom);
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].0, 1);
        let re = Regex::new(r"([A-Z]+)-(\d+)").unwrap();
        let caps = re.captures("OPS-9").unwrap();
        assert_eq!(
            fill_template("{0}|{key}|{1}|{2}|{3}", &caps),
            "OPS-9|OPS|OPS|9|"
        );
    }

    #[test]
    fn urls_and_slugs() {
        assert_eq!(
            jira_base("acme.atlassian.net"),
            "https://acme.atlassian.net"
        );
        assert_eq!(
            jira_base("https://jira.example.com/browse/"),
            "https://jira.example.com"
        );
        assert_eq!(jira_base("http://jira.local/"), "http://jira.local");
        assert_eq!(linear_slug("https://linear.app/acme/team/ENG"), "acme");
        assert_eq!(linear_slug("linear.app/acme"), "acme");
        assert_eq!(linear_slug(" acme "), "acme");
        assert_eq!(
            linear_url("acme", "ENG-1"),
            "https://linear.app/acme/issue/ENG-1"
        );
    }

    #[test]
    fn many_texts_dedupe() {
        let p = jira(&["ABC"]);
        let got = find_all(
            &[
                ("ABC-1 title", Source::Text),
                ("body ABC-1 and ABC-2", Source::Text),
                ("abc-3-branch", Source::Branch),
            ],
            &p,
        );
        assert_eq!(keys(&got), vec!["ABC-1", "ABC-2", "ABC-3"]);
    }

    #[test]
    fn message_helpers() {
        assert_eq!(
            message_insert("ABC-1", Placement::Prefix),
            ("ABC-1: ".to_owned(), true)
        );
        assert_eq!(
            message_insert("ABC-1", Placement::Suffix),
            ("ABC-1".to_owned(), false)
        );
        assert!(mentions("abc-1: fix", "ABC-1"));
        assert!(mentions("Fix\n\nRefs ABC-1", "ABC-1"));
        assert!(!mentions("ABC-12", "ABC-1"));
        assert!(!mentions("XABC-1", "ABC-1"));
    }

    #[test]
    fn suggestions_need_two_issues() {
        let texts = [
            "ABC-1 fix",
            "ABC-2 more",
            "ABC-2 again",
            "OPS-7 once",
            "UTF-8 and UTF-16",
            "WEB-1 a",
            "WEB-3 b",
            "KNOWN-1 KNOWN-2",
        ];
        let got = suggest_keys(texts.iter().copied(), &["known".into()], 5);
        assert_eq!(got, vec![("ABC".to_owned(), 3), ("WEB".to_owned(), 2)]);
    }

    #[test]
    fn prefs_serde_defaults() {
        let p: LinkPrefs = serde_json::from_str(r#"{"jira":{"base_url":"x"}}"#).unwrap();
        assert_eq!(p.jira.base_url, "x");
        assert!(p.jira.keys.is_empty() && !p.jira.lookup);
        assert_eq!(p.placement, Placement::Prefix);
        let back: LinkPrefs = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
    }
}
