//! CODEOWNERS: where GitHub looks for it, the parser, gitignore-style
//! pattern matching with GitHub's exceptions, validation, coverage over the
//! tracked files, and GitHub's own error list mapped to lines.
//!
//! Matching follows GitHub's documented behaviour: the last matching rule
//! wins; a pattern with a slash at the start or in the middle is anchored
//! to the repository root, otherwise it matches at any depth; a trailing
//! slash (or a last segment that is a name or a glob) also matches
//! everything below; a last segment of `*` matches only direct children
//! (`docs/*` does not own `docs/a/b.md`); `**` spans directories. `!`
//! negation, `[ ]` ranges and `\#` escapes do not work in CODEOWNERS.

use std::collections::HashMap;
use std::path::Path;

use regex::RegexSet;
use serde::Deserialize;

use crate::devops::gh::{Gh, encode};
use crate::github::ApiCall;
use crate::{Error, Result};

/// Where GitHub looks, in order; the first one found is used.
pub const LOCATIONS: [&str; 3] = [".github/CODEOWNERS", "CODEOWNERS", "docs/CODEOWNERS"];

/// GitHub ignores a CODEOWNERS file over this size.
pub const MAX_BYTES: usize = 3 * 1024 * 1024;

/// Which of [`LOCATIONS`] exist in the working tree, in GitHub's order.
pub fn locate(workdir: &Path) -> Vec<String> {
    LOCATIONS
        .iter()
        .filter(|p| workdir.join(p).is_file())
        .map(|p| (*p).to_owned())
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Info,
    Warning,
    Error,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::Error => "Error",
            Level::Warning => "Warning",
            Level::Info => "Info",
        }
    }
}

/// A problem on one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    /// 1-based; 0 for the whole file.
    pub line: usize,
    pub level: Level,
    /// Short machine name: `negation`, `range`, `escape`, `owner`,
    /// `no-match`, `shadowed`, `duplicate`, `no-owners`, `size`,
    /// `github` (from GitHub's error list).
    pub kind: &'static str,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerKind {
    User,
    Team,
    Email,
    Invalid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub text: String,
    pub kind: OwnerKind,
}

/// One rule line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    /// 1-based line number.
    pub line: usize,
    pub pattern: String,
    pub owners: Vec<Owner>,
    /// Text after an inline `#`.
    pub comment: Option<String>,
    /// The regex the pattern compiles to; `None` when GitHub would not use
    /// the line (unsupported syntax).
    pub regex: Option<String>,
}

impl Rule {
    pub fn owner_list(&self) -> String {
        self.owners
            .iter()
            .map(|o| o.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// A parsed CODEOWNERS file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeOwners {
    pub path: String,
    pub rules: Vec<Rule>,
    pub lines: usize,
    pub comment_lines: usize,
    pub blank_lines: usize,
    pub bytes: usize,
    /// Syntax and owner problems found without looking at files.
    pub issues: Vec<Issue>,
}

impl CodeOwners {
    /// Distinct owners with the number of rules naming them, most first.
    pub fn owners(&self) -> Vec<(String, usize)> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for r in &self.rules {
            for o in &r.owners {
                *counts.entry(o.text.as_str()).or_insert(0) += 1;
            }
        }
        let mut v: Vec<(String, usize)> =
            counts.into_iter().map(|(k, n)| (k.to_owned(), n)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }

    pub fn rule_at_line(&self, line: usize) -> Option<usize> {
        self.rules.iter().position(|r| r.line == line)
    }

    pub fn errors(&self) -> usize {
        self.issues
            .iter()
            .filter(|i| i.level == Level::Error)
            .count()
    }
}

fn valid_login(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 39
        && !s.starts_with('-')
        && !s.ends_with('-')
        && !s.contains("--")
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Classify an owner token.
pub fn owner_kind(token: &str) -> OwnerKind {
    if let Some(rest) = token.strip_prefix('@') {
        if let Some((org, team)) = rest.split_once('/') {
            let team_ok = !team.is_empty()
                && team
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
            if valid_login(org) && team_ok {
                return OwnerKind::Team;
            }
            return OwnerKind::Invalid;
        }
        // Bots end in `[bot]` and are not valid code owners either.
        return if valid_login(rest) {
            OwnerKind::User
        } else {
            OwnerKind::Invalid
        };
    }
    let mut parts = token.split('@');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(local), Some(domain), None)
            if !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.') =>
        {
            OwnerKind::Email
        }
        _ => OwnerKind::Invalid,
    }
}

/// Parse a CODEOWNERS file.
pub fn parse(path: &str, text: &str) -> CodeOwners {
    let mut out = CodeOwners {
        path: path.to_owned(),
        bytes: text.len(),
        ..Default::default()
    };
    if text.len() > MAX_BYTES {
        out.issues.push(Issue {
            line: 0,
            level: Level::Error,
            kind: "size",
            message: format!(
                "The file is {} bytes; GitHub ignores CODEOWNERS files of 3 MB or more.",
                text.len()
            ),
        });
    }
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        out.lines = line;
        let t = raw.trim();
        if t.is_empty() {
            out.blank_lines += 1;
            continue;
        }
        if t.starts_with('#') {
            out.comment_lines += 1;
            continue;
        }
        let mut tokens = t.split_whitespace();
        let pattern = tokens.next().unwrap_or_default().to_owned();
        let mut owners = Vec::new();
        let mut comment: Option<String> = None;
        let rest: Vec<&str> = tokens.collect();
        for (k, tok) in rest.iter().enumerate() {
            if tok.starts_with('#') {
                comment = Some(
                    rest[k..]
                        .join(" ")
                        .trim_start_matches('#')
                        .trim()
                        .to_owned(),
                );
                break;
            }
            let kind = owner_kind(tok);
            if kind == OwnerKind::Invalid {
                out.issues.push(Issue {
                    line,
                    level: Level::Error,
                    kind: "owner",
                    message: format!(
                        "`{tok}` is not a valid owner: use @user, @org/team or an email address."
                    ),
                });
            }
            owners.push(Owner {
                text: (*tok).to_owned(),
                kind,
            });
        }
        let mut usable = true;
        if pattern.starts_with('!') {
            usable = false;
            out.issues.push(Issue {
                line,
                level: Level::Error,
                kind: "negation",
                message: format!(
                    "`{pattern}`: negation with ! does not work in CODEOWNERS, so this line has no effect. To leave paths unowned, add a later rule for them with no owners."
                ),
            });
        } else if pattern.starts_with("\\#") {
            usable = false;
            out.issues.push(Issue {
                line,
                level: Level::Error,
                kind: "escape",
                message: format!(
                    "`{pattern}`: escaping # with a backslash does not work in CODEOWNERS, so this line has no effect."
                ),
            });
        } else if pattern.contains('[') || pattern.contains(']') {
            usable = false;
            out.issues.push(Issue {
                line,
                level: Level::Error,
                kind: "range",
                message: format!(
                    "`{pattern}`: character ranges with [ ] do not work in CODEOWNERS, so this line has no effect. List the alternatives as separate rules."
                ),
            });
        }
        if owners.is_empty() && usable {
            out.issues.push(Issue {
                line,
                level: Level::Info,
                kind: "no-owners",
                message: format!(
                    "`{pattern}` has no owners: matching files have no code owner (an explicit exception to earlier rules)."
                ),
            });
        }
        if let Some(prev) = seen.insert(pattern.clone(), line) {
            out.issues.push(Issue {
                line: prev,
                level: Level::Warning,
                kind: "duplicate",
                message: format!(
                    "`{pattern}` appears again on line {line}; the later line wins, so this one never applies."
                ),
            });
        }
        out.rules.push(Rule {
            line,
            pattern: pattern.clone(),
            owners,
            comment,
            regex: usable.then(|| pattern_regex(&pattern)),
        });
    }
    out.issues
        .sort_by(|a, b| a.line.cmp(&b.line).then(b.level.cmp(&a.level)));
    out
}

/// The regex (anchored, for paths relative to the root with `/`
/// separators) a CODEOWNERS pattern compiles to.
pub fn pattern_regex(pattern: &str) -> String {
    let mut re = String::from("^");
    let mut segs: Vec<&str> = pattern.split('/').collect();
    if segs.first() == Some(&"") {
        // Leading slash: relative to the root.
        segs.remove(0);
    } else if (segs.len() == 1 || (segs.len() == 2 && segs[1].is_empty())) && segs[0] != "**" {
        // A single name matches at any depth, like a leading `**/`.
        segs.insert(0, "**");
    }
    if segs.len() > 1 && segs.last() == Some(&"") {
        // Trailing slash: everything below.
        let n = segs.len();
        segs[n - 1] = "**";
    }
    let last = segs.len().saturating_sub(1);
    let mut need_slash = false;
    for (i, seg) in segs.iter().enumerate() {
        match *seg {
            "**" => {
                if i == 0 && i == last {
                    re.push_str(".+");
                } else if i == 0 {
                    re.push_str("(?:.+/)?");
                    need_slash = false;
                } else if i == last {
                    re.push_str("/.*");
                } else {
                    re.push_str("(?:/.+)?");
                    need_slash = true;
                }
            }
            "*" => {
                if need_slash {
                    re.push('/');
                }
                re.push_str("[^/]+");
                need_slash = true;
            }
            _ => {
                if need_slash {
                    re.push('/');
                }
                let mut escape = false;
                for ch in seg.chars() {
                    if escape {
                        escape = false;
                        re.push_str(&regex::escape(&ch.to_string()));
                        continue;
                    }
                    match ch {
                        '\\' => escape = true,
                        '*' => re.push_str("[^/]*"),
                        '?' => re.push_str("[^/]"),
                        c => re.push_str(&regex::escape(&c.to_string())),
                    }
                }
                if i == last {
                    // A name also owns what is below it.
                    re.push_str("(?:/.*)?");
                }
                need_slash = true;
            }
        }
    }
    re.push('$');
    re
}

/// All usable rules compiled into one set.
pub struct Matcher {
    set: RegexSet,
    /// Index into `CodeOwners::rules` of each pattern in the set.
    rules: Vec<usize>,
}

impl Matcher {
    pub fn new(file: &CodeOwners) -> Result<Self> {
        let mut pats = Vec::new();
        let mut rules = Vec::new();
        for (i, r) in file.rules.iter().enumerate() {
            if let Some(re) = &r.regex {
                pats.push(re.clone());
                rules.push(i);
            }
        }
        let set =
            RegexSet::new(&pats).map_err(|e| Error::new(format!("CODEOWNERS patterns: {e}")))?;
        Ok(Self { set, rules })
    }

    /// Indices (into `rules`) of every rule matching `path`, in file order.
    pub fn matching(&self, path: &str) -> Vec<usize> {
        let path = path.trim_start_matches('/');
        self.set
            .matches(path)
            .into_iter()
            .map(|k| self.rules[k])
            .collect()
    }

    /// The rule that decides the owners of `path`: the last match.
    pub fn owner_rule(&self, path: &str) -> Option<usize> {
        self.matching(path).last().copied()
    }
}

/// Who owns a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lookup {
    pub path: String,
    /// The path is a directory (it ends with `/` or has tracked files
    /// below it): the rules are those of a new file directly inside it.
    pub is_dir: bool,
    /// Every matching rule (indices into `rules`), in file order; the last
    /// one wins.
    pub matching: Vec<usize>,
    pub winner: Option<usize>,
    /// For a directory with tracked files: `(rule or None, files)` of the
    /// rules that win for the files below it, most files first.
    pub below: Vec<(Option<usize>, usize)>,
    pub files_below: usize,
}

/// The name of the imaginary new file a directory lookup probes with
/// (no extension, so extension rules do not match it).
pub const PROBE: &str = "new-file";

/// Look up `path` (a file or a directory) against the rules.
pub fn lookup(matcher: &Matcher, path: &str, tracked: &[String]) -> Lookup {
    let path = path.trim().trim_start_matches("./").trim_start_matches('/');
    let dir = path.trim_end_matches('/');
    let prefix = format!("{dir}/");
    let mut below: HashMap<Option<usize>, usize> = HashMap::new();
    let mut files_below = 0;
    if !dir.is_empty() {
        for f in tracked.iter().filter(|f| f.starts_with(&prefix)) {
            files_below += 1;
            *below.entry(matcher.owner_rule(f)).or_insert(0) += 1;
        }
    }
    let is_dir = path.ends_with('/') || files_below > 0;
    let matching = if is_dir {
        matcher.matching(&format!("{dir}/{PROBE}"))
    } else {
        matcher.matching(dir)
    };
    let winner = matching.last().copied();
    let mut below: Vec<(Option<usize>, usize)> = below.into_iter().collect();
    below.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    Lookup {
        path: path.to_owned(),
        is_dir,
        matching,
        winner,
        below,
        files_below,
    }
}

/// Per-rule numbers over the tracked files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuleStat {
    /// Files the pattern matches.
    pub matched: usize,
    /// Files whose owners this rule decides (it is the last match).
    pub won: usize,
    /// Rules (indices) that took this rule's files, with counts.
    pub lost_to: Vec<(usize, usize)>,
}

/// Coverage and the file-based checks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Analysis {
    pub files: usize,
    /// Files with a code owner.
    pub owned: usize,
    /// Files no rule matches.
    pub unmatched: usize,
    /// Files whose deciding rule names no owners.
    pub explicitly_unowned: usize,
    /// Directories holding the most unowned files: `(dir, files)`.
    pub unowned_dirs: Vec<(String, usize)>,
    /// A few unowned files as examples.
    pub unowned_examples: Vec<String>,
    /// One per rule, in rule order.
    pub stats: Vec<RuleStat>,
    /// `no-match` and `shadowed` findings.
    pub issues: Vec<Issue>,
    pub elapsed_ms: u64,
}

/// Owners per tracked file: coverage, rules that match nothing, rules
/// fully shadowed by later ones.
pub fn analyze(file: &CodeOwners, tracked: &[String]) -> Result<Analysis> {
    let started = std::time::Instant::now();
    let matcher = Matcher::new(file)?;
    let mut stats: Vec<RuleStat> = vec![RuleStat::default(); file.rules.len()];
    let mut lost: Vec<HashMap<usize, usize>> = vec![HashMap::new(); file.rules.len()];
    let mut out = Analysis {
        files: tracked.len(),
        ..Default::default()
    };
    let mut dirs: HashMap<String, usize> = HashMap::new();
    for f in tracked {
        let m = matcher.matching(f);
        let winner = m.last().copied();
        for &r in &m {
            stats[r].matched += 1;
            match winner {
                Some(w) if w == r => stats[r].won += 1,
                Some(w) => *lost[r].entry(w).or_insert(0) += 1,
                None => {}
            }
        }
        let owned = winner.is_some_and(|w| !file.rules[w].owners.is_empty());
        if owned {
            out.owned += 1;
        } else {
            if winner.is_none() {
                out.unmatched += 1;
            } else {
                out.explicitly_unowned += 1;
            }
            let dir = f.rfind('/').map(|i| &f[..i]).unwrap_or("(root)");
            // Group by the first two levels so the list stays short.
            let top: String = dir.split('/').take(2).collect::<Vec<_>>().join("/");
            *dirs.entry(top).or_insert(0) += 1;
            if out.unowned_examples.len() < 12 {
                out.unowned_examples.push(f.clone());
            }
        }
    }
    for (i, l) in lost.into_iter().enumerate() {
        let mut v: Vec<(usize, usize)> = l.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        stats[i].lost_to = v;
    }
    for (i, r) in file.rules.iter().enumerate() {
        if r.regex.is_none() {
            continue;
        }
        let s = &stats[i];
        if s.matched == 0 {
            out.issues.push(Issue {
                line: r.line,
                level: Level::Warning,
                kind: "no-match",
                message: format!(
                    "`{}` matches no tracked file (a typo, a moved directory, or files not committed yet).",
                    r.pattern
                ),
            });
        } else if s.won == 0 {
            let by: Vec<String> = s
                .lost_to
                .iter()
                .take(3)
                .map(|(w, n)| {
                    format!(
                        "line {} `{}` ({})",
                        file.rules[*w].line,
                        file.rules[*w].pattern,
                        plural(*n, "file")
                    )
                })
                .collect();
            out.issues.push(Issue {
                line: r.line,
                level: Level::Warning,
                kind: "shadowed",
                message: format!(
                    "`{}` matches {} but later rules take every one of them: {}. The last match wins, so this rule never decides an owner.",
                    r.pattern,
                    plural(s.matched, "file"),
                    by.join(", ")
                ),
            });
        }
    }
    let mut dirs: Vec<(String, usize)> = dirs.into_iter().collect();
    dirs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    dirs.truncate(15);
    out.unowned_dirs = dirs;
    out.stats = stats;
    out.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(out)
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// One entry of GitHub's `codeowners/errors`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct GhError {
    pub line: usize,
    pub column: usize,
    pub kind: String,
    pub source: String,
    pub suggestion: Option<String>,
    pub message: String,
    pub path: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawErrors {
    errors: Vec<GhError>,
}

pub fn parse_errors(body: &[u8]) -> Result<Vec<GhError>> {
    let raw: RawErrors = crate::devops::gh::decode(body, "CODEOWNERS errors")?;
    Ok(raw.errors)
}

/// `GET codeowners/errors` (for `git_ref`, else the default branch).
pub fn errors_endpoint(gh: &Gh, git_ref: Option<&str>) -> String {
    match git_ref {
        Some(r) if !r.is_empty() => gh.path(&format!("codeowners/errors?ref={}", encode(r))),
        _ => gh.path("codeowners/errors"),
    }
}

pub fn fetch_errors(
    gh: &Gh,
    git_ref: Option<&str>,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<GhError>> {
    parse_errors(&gh.get(&errors_endpoint(gh, git_ref), calls)?)
}

/// GitHub's errors as issues on their lines.
pub fn github_issues(errors: &[GhError]) -> Vec<Issue> {
    errors
        .iter()
        .map(|e| Issue {
            line: e.line,
            level: Level::Error,
            kind: "github",
            message: {
                let mut m = if e.message.is_empty() {
                    format!("{}: {}", e.kind, e.source)
                } else {
                    e.message.trim().to_owned()
                };
                if let Some(s) = e.suggestion.as_deref().filter(|s| !s.is_empty()) {
                    m.push_str(&format!(" Suggestion: {s}"));
                }
                m
            },
        })
        .collect()
}

/// A line diff for the save confirmation: `@@ -a,b +c,d @@` headers and
/// `+` / `-` / ` ` lines with one line of context, at most `max` lines.
pub fn line_diff(old: &str, new: &str, max: usize) -> Vec<String> {
    let diff = similar::TextDiff::from_lines(old, new);
    let mut out = Vec::new();
    for group in diff.grouped_ops(1) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        let (o0, o1) = (first.old_range().start, last.old_range().end);
        let (n0, n1) = (first.new_range().start, last.new_range().end);
        out.push(format!(
            "@@ -{},{} +{},{} @@",
            o0 + 1,
            o1 - o0,
            n0 + 1,
            n1 - n0
        ));
        for op in &group {
            for change in diff.iter_changes(op) {
                let sign = match change.tag() {
                    similar::ChangeTag::Delete => '-',
                    similar::ChangeTag::Insert => '+',
                    similar::ChangeTag::Equal => ' ',
                };
                out.push(format!(
                    "{sign} {}",
                    change.value().trim_end_matches(['\n', '\r'])
                ));
                if out.len() >= max {
                    out.push("… (diff cut)".into());
                    return out;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owns(pattern: &str, path: &str) -> bool {
        regex::Regex::new(&pattern_regex(pattern))
            .unwrap()
            .is_match(path)
    }

    #[test]
    fn gitignore_style_semantics() {
        // Unanchored names match at any depth, and what is below them.
        assert!(owns("*.js", "a.js"));
        assert!(owns("*.js", "src/deep/a.js"));
        assert!(!owns("*.js", "a.jsx"));
        assert!(owns("apps/", "apps/x.txt"));
        assert!(owns("apps/", "lib/apps/deep/x.txt"));
        assert!(!owns("apps/", "apps"));
        // Anchored with a leading slash.
        assert!(owns("/docs/", "docs/a/b.md"));
        assert!(!owns("/docs/", "src/docs/a.md"));
        assert!(owns("/README.md", "README.md"));
        assert!(!owns("/README.md", "sub/README.md"));
        // A middle slash anchors too.
        assert!(owns("build/logs/", "build/logs/x/y.log"));
        assert!(!owns("build/logs/", "a/build/logs/y.log"));
        // `docs/*` only owns direct children.
        assert!(owns("docs/*", "docs/getting-started.md"));
        assert!(!owns("docs/*", "docs/build-app/troubleshooting.md"));
        // `*` vs `**`.
        assert!(owns("docs/**", "docs/a/b/c.md"));
        assert!(owns("**/logs", "build/logs/x.log"));
        assert!(owns("**/logs", "logs/x.log"));
        assert!(owns("**/logs", "deeply/nested/logs/x"));
        assert!(owns("src/**/test.js", "src/test.js"));
        assert!(owns("src/**/test.js", "src/a/b/test.js"));
        assert!(!owns("src/*/test.js", "src/a/b/test.js"));
        assert!(owns("src/*/test.js", "src/a/test.js"));
        // A name without a trailing slash owns a directory and its contents.
        assert!(owns("/apps/github", "apps/github/x/y.rb"));
        assert!(owns("/apps/github", "apps/github"));
        assert!(!owns("/apps/github", "apps/githubx/y.rb"));
        assert!(owns("*", "anything/at/all.txt"));
        assert!(owns("**/*.rs", "a/b.rs"));
        assert!(owns("package*.json", "web/package-lock.json"));
        assert!(owns("a?c", "abc"));
        assert!(!owns("a?c", "a/c"));
        // Case-sensitive.
        assert!(!owns("*.JS", "a.js"));
    }

    const FILE: &str = "\
# Default owners
*       @global-owner1 @global-owner2

*.js    @js-owner #This is an inline comment.
*.go docs@example.com
/docs/ @doctocat
docs/old/ @old-team
!secret.txt @x
[Ss]rc/ @y
\\#hash @z
/apps/ @octocat
/apps/github
/nowhere/ @acme/ghosts
bad-owner @not/valid/owner user@
*.js @js-owner
";

    #[test]
    fn parse_and_validate() {
        let f = parse(".github/CODEOWNERS", FILE);
        assert_eq!(f.rules.len(), 13);
        assert_eq!(f.comment_lines, 1);
        assert_eq!(f.blank_lines, 1);
        let js = &f.rules[1];
        assert_eq!(js.line, 4);
        assert_eq!(js.owner_list(), "@js-owner");
        assert_eq!(js.comment.as_deref(), Some("This is an inline comment."));
        assert_eq!(f.rules[2].owners[0].kind, OwnerKind::Email);
        let kinds = |k: &str| {
            f.issues
                .iter()
                .filter(|i| i.kind == k)
                .map(|i| i.line)
                .collect::<Vec<_>>()
        };
        assert_eq!(kinds("negation"), vec![8]);
        assert_eq!(kinds("range"), vec![9]);
        assert_eq!(kinds("escape"), vec![10]);
        assert_eq!(kinds("no-owners"), vec![12]);
        assert_eq!(kinds("owner"), vec![14, 14]);
        assert_eq!(kinds("duplicate"), vec![4]);
        assert!(f.rules[6].regex.is_none());
        assert_eq!(owner_kind("@acme/ghosts"), OwnerKind::Team);
        assert_eq!(owner_kind("@a-b"), OwnerKind::User);
        assert_eq!(owner_kind("@-ab"), OwnerKind::Invalid);
        assert_eq!(owner_kind("@a--b"), OwnerKind::Invalid);
        assert_eq!(owner_kind("x@y.io"), OwnerKind::Email);
        assert_eq!(owner_kind("plainname"), OwnerKind::Invalid);
        assert_eq!(f.owners()[0], ("@js-owner".into(), 2));
    }

    #[test]
    fn matching_and_coverage() {
        let f = parse("CODEOWNERS", FILE);
        let tracked: Vec<String> = [
            "README.md",
            "src/app.js",
            "src/main.go",
            "docs/intro.md",
            "docs/old/a.md",
            "apps/web/x.rb",
            "apps/github/y.rb",
            "secret.txt",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let m = Matcher::new(&f).unwrap();
        // Last match wins: the second `*.js` (line 15) beats line 4 and `*`.
        let w = m.owner_rule("src/app.js").unwrap();
        assert_eq!(f.rules[w].line, 15);
        assert_eq!(f.rules[m.owner_rule("apps/web/x.rb").unwrap()].line, 11);
        // Explicitly unowned subdirectory.
        assert!(
            f.rules[m.owner_rule("apps/github/y.rb").unwrap()]
                .owners
                .is_empty()
        );
        // `docs/old/` (line 7) comes after `/docs/` (line 6).
        assert_eq!(f.rules[m.owner_rule("docs/old/a.md").unwrap()].line, 7);
        let a = analyze(&f, &tracked).unwrap();
        assert_eq!(a.files, 8);
        assert_eq!(a.explicitly_unowned, 1);
        assert_eq!(a.unmatched, 0);
        assert_eq!(a.owned, 7);
        assert_eq!(a.unowned_dirs, vec![("apps/github".to_string(), 1)]);
        let issue = |k: &str| {
            a.issues
                .iter()
                .filter(|i| i.kind == k)
                .map(|i| i.line)
                .collect::<Vec<_>>()
        };
        // `/nowhere/` and `bad-owner` match nothing.
        assert_eq!(issue("no-match"), vec![13, 14]);
        // The first `*.js` is fully shadowed by the second.
        assert_eq!(issue("shadowed"), vec![4]);
        let line4 = f.rule_at_line(4).unwrap();
        assert_eq!(a.stats[line4].matched, 1);
        assert_eq!(a.stats[line4].won, 0);
        // Lookup of a directory.
        let l = lookup(&m, "apps", &tracked);
        assert!(l.is_dir);
        assert_eq!(l.files_below, 2);
        assert_eq!(l.below.len(), 2);
        // A new file directly in apps/ falls under `/apps/` (line 11).
        assert_eq!(f.rules[l.winner.unwrap()].line, 11);
        let l = lookup(&m, "docs/", &tracked);
        assert!(l.is_dir);
        assert_eq!(f.rules[l.winner.unwrap()].line, 6);
        let l = lookup(&m, "/src/app.js", &tracked);
        assert!(!l.is_dir);
        assert_eq!(l.matching.len(), 3);
        assert_eq!(f.rules[l.winner.unwrap()].line, 15);
    }

    #[test]
    fn coverage_without_default_rule() {
        let f = parse("CODEOWNERS", "/src/ @a\n");
        let tracked: Vec<String> = ["src/a.rs", "lib/x/y/z.rs", "lib/x/q.rs", "top.txt"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let a = analyze(&f, &tracked).unwrap();
        assert_eq!(a.unmatched, 3);
        assert_eq!(a.owned, 1);
        assert_eq!(a.unowned_dirs[0], ("lib/x".to_string(), 2));
        assert_eq!(a.unowned_dirs[1], ("(root)".to_string(), 1));
    }

    #[test]
    fn github_errors_and_diff() {
        let body = br#"{"errors":[{"line":3,"column":1,"kind":"Unknown owner","source":"* @nobody","suggestion":"make sure @nobody exists","message":"Unknown owner on line 3: make sure @nobody exists and has write access to the repository\n\n  * @nobody\n    ^","path":".github/CODEOWNERS"}]}"#;
        let e = parse_errors(body).unwrap();
        assert_eq!(e[0].line, 3);
        let issues = github_issues(&e);
        assert_eq!(issues[0].kind, "github");
        assert!(issues[0].message.starts_with("Unknown owner on line 3"));
        let gh = Gh::new(".", "o", "r");
        assert_eq!(errors_endpoint(&gh, None), "repos/o/r/codeowners/errors");
        assert_eq!(
            errors_endpoint(&gh, Some("feature/x")),
            "repos/o/r/codeowners/errors?ref=feature%2Fx"
        );
        let d = line_diff("a\nb\nc\n", "a\nB\nc\nd\n", 50);
        assert_eq!(d[0], "@@ -1,3 +1,4 @@");
        assert!(d.contains(&"- b".to_string()));
        assert!(d.contains(&"+ B".to_string()));
        assert!(d.contains(&"+ d".to_string()));
        assert!(line_diff("same\n", "same\n", 10).is_empty());
    }
}
