//! Local helpers of the review loop: pull request and issue templates,
//! commits between two branches, titles and bodies generated from them,
//! branch names for issues, `#N` references in commit messages, and
//! readable plain text from GitHub bodies.

use std::path::Path;

use crate::Result;
use crate::cli;

// ---------------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------------

/// A pull request template found in the working tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
    /// Relative path with forward slashes.
    pub path: String,
    /// The name shown in a chooser (file stem).
    pub name: String,
    pub body: String,
}

/// Files in `dir` whose name matches `name` ignoring case.
fn find_named(root: &Path, dir: &str, name: &str) -> Option<(String, String)> {
    let full = if dir.is_empty() {
        root.to_path_buf()
    } else {
        root.join(dir)
    };
    let entries = std::fs::read_dir(&full).ok()?;
    let mut found: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.eq_ignore_ascii_case(name))
        .collect();
    found.sort();
    let file = found.into_iter().next()?;
    let body = std::fs::read_to_string(full.join(&file)).ok()?;
    let rel = if dir.is_empty() {
        file
    } else {
        format!("{dir}/{file}")
    };
    Some((rel, body))
}

/// Markdown files of a template directory (any case of the directory name).
fn templates_in(root: &Path, parent: &str, dir_name: &str) -> Vec<Template> {
    let base = if parent.is_empty() {
        root.to_path_buf()
    } else {
        root.join(parent)
    };
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    let Some(dir) = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.eq_ignore_ascii_case(dir_name))
    else {
        return Vec::new();
    };
    let rel_dir = if parent.is_empty() {
        dir.clone()
    } else {
        format!("{parent}/{dir}")
    };
    let Ok(files) = std::fs::read_dir(base.join(&dir)) else {
        return Vec::new();
    };
    let mut out: Vec<Template> = files
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let lower = name.to_ascii_lowercase();
            if !lower.ends_with(".md") {
                return None;
            }
            let body = std::fs::read_to_string(e.path()).ok()?;
            Some(Template {
                path: format!("{rel_dir}/{name}"),
                name: name[..name.len() - 3].to_owned(),
                body,
            })
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Pull request templates, in GitHub's order: a single template in
/// `.github/`, `docs/` or the root (`pull_request_template.md`, any case),
/// then every `*.md` of a `PULL_REQUEST_TEMPLATE/` directory in those
/// places.
pub fn pr_templates(root: &Path) -> Vec<Template> {
    let mut out = Vec::new();
    for dir in [".github", "docs", ""] {
        if let Some((path, body)) = find_named(root, dir, "pull_request_template.md") {
            out.push(Template {
                name: "Default".to_owned(),
                path,
                body,
            });
            break;
        }
    }
    for dir in [".github", "docs", ""] {
        out.extend(templates_in(root, dir, "PULL_REQUEST_TEMPLATE"));
    }
    out
}

/// An issue template (`.github/ISSUE_TEMPLATE/*.md` with front matter).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct IssueTemplate {
    pub path: String,
    pub name: String,
    pub about: String,
    pub title: String,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub body: String,
}

/// A YAML value that is a list or a comma-separated string.
fn yaml_list(v: Option<&serde_yaml::Value>) -> Vec<String> {
    match v {
        Some(serde_yaml::Value::Sequence(items)) => items
            .iter()
            .filter_map(|i| i.as_str())
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect(),
        Some(serde_yaml::Value::String(s)) => s
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// Split `---\nfront matter\n---\nbody`.
pub fn front_matter(text: &str) -> (Option<&str>, &str) {
    let t = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = t
        .strip_prefix("---\n")
        .or_else(|| t.strip_prefix("---\r\n"))
    else {
        return (None, t);
    };
    for marker in ["\n---\n", "\n---\r\n", "\r\n---\r\n"] {
        if let Some(end) = rest.find(marker) {
            return (Some(&rest[..end]), &rest[end + marker.len()..]);
        }
    }
    if let Some(stripped) = rest.strip_suffix("\n---") {
        return (Some(stripped), "");
    }
    (None, t)
}

/// Parse one Markdown issue template.
pub fn parse_issue_template(path: &str, text: &str) -> IssueTemplate {
    let (fm, body) = front_matter(text);
    let stem = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".md")
        .to_owned();
    let mut t = IssueTemplate {
        path: path.to_owned(),
        name: stem,
        body: body.trim_start_matches(['\n', '\r']).to_owned(),
        ..Default::default()
    };
    if let Some(yaml) = fm.and_then(|f| serde_yaml::from_str::<serde_yaml::Value>(f).ok()) {
        let get = |k: &str| yaml.get(k).and_then(|v| v.as_str()).map(str::to_owned);
        if let Some(n) = get("name").filter(|n| !n.trim().is_empty()) {
            t.name = n;
        }
        t.about = get("about").unwrap_or_default();
        t.title = get("title").unwrap_or_default();
        t.labels = yaml_list(yaml.get("labels"));
        t.assignees = yaml_list(yaml.get("assignees"));
    }
    t
}

/// Markdown issue templates of `.github/ISSUE_TEMPLATE/` (issue forms in
/// YAML are not supported and skipped).
pub fn issue_templates(root: &Path) -> Vec<IssueTemplate> {
    templates_in(root, ".github", "ISSUE_TEMPLATE")
        .into_iter()
        .map(|t| parse_issue_template(&t.path, &t.body))
        .collect()
}

// ---------------------------------------------------------------------------
// Commits between branches
// ---------------------------------------------------------------------------

/// Whether `name` resolves to a commit.
pub fn resolves(dir: &Path, name: &str) -> bool {
    cli::run(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{name}^{{commit}}"),
        ],
        None,
    )
    .is_ok()
}

/// The first of `candidates` that resolves.
pub fn first_resolving(dir: &Path, candidates: &[String]) -> Option<String> {
    candidates.iter().find(|c| resolves(dir, c)).cloned()
}

/// `(sha, subject)` of the commits in `head` but not in `base`, newest
/// first, merges left out, at most `max`.
pub fn commits_between(
    dir: &Path,
    base: &str,
    head: &str,
    max: usize,
) -> Result<Vec<(String, String)>> {
    let range = format!("{base}..{head}");
    let out = cli::run(
        dir,
        &[
            "log",
            "--no-merges",
            "--format=%H%x1f%s",
            &format!("--max-count={max}"),
            &range,
            "--",
        ],
        None,
    )?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(parse_log(&text))
}

/// The arguments [`commits_between`] runs, for the activity log.
pub fn commits_between_command(base: &str, head: &str, max: usize) -> String {
    format!("git log --no-merges --format=%H%x1f%s --max-count={max} {base}..{head} --")
}

/// `%H%x1f%s` lines.
pub fn parse_log(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let (sha, subject) = l.split_once('\x1f')?;
            Some((sha.trim().to_owned(), subject.trim().to_owned()))
        })
        .filter(|(sha, _)| !sha.is_empty())
        .collect()
}

/// A body listing the commits, oldest first: `- Subject`.
pub fn body_from_commits(commits: &[(String, String)]) -> String {
    let mut out = String::new();
    for (_, subject) in commits.iter().rev() {
        out.push_str("- ");
        out.push_str(subject);
        out.push('\n');
    }
    out
}

/// `feature/add-login_page` → `Add login page`.
pub fn humanize_branch(branch: &str) -> String {
    let last = branch.rsplit('/').next().unwrap_or(branch);
    let words: Vec<&str> = last
        .split(['-', '_', '.'])
        .filter(|w| !w.is_empty())
        .collect();
    let mut s = words.join(" ");
    if let Some(first) = s.chars().next() {
        let upper: String = first.to_uppercase().collect();
        s.replace_range(..first.len_utf8(), &upper);
    }
    s
}

/// The default title: the only commit's subject, else the branch name in
/// words.
pub fn default_title(commits: &[(String, String)], branch: &str) -> String {
    match commits {
        [(_, subject)] if !subject.trim().is_empty() => subject.trim().to_owned(),
        _ => humanize_branch(branch),
    }
}

// ---------------------------------------------------------------------------
// Issues and references
// ---------------------------------------------------------------------------

/// `123-fix-the-login-page` for issue 123 titled "Fix the login page!".
pub fn issue_branch_name(number: u64, title: &str) -> String {
    let slug = crate::devops::slug(title, 48);
    if slug.is_empty() {
        format!("issue-{number}")
    } else {
        format!("{number}-{slug}")
    }
}

/// Whether `text` mentions `#n` as a whole token: not `#123abc`, not
/// inside a URL (`…/page#12`), not glued to a word (`PR#12`, `o/r#12`).
pub fn mentions(text: &str, n: u64) -> bool {
    let needle = format!("#{n}");
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(i) = text[from..].find(&needle) {
        let start = from + i;
        let end = start + needle.len();
        from = start + 1;
        let before_ok = start == 0 || {
            let c = bytes[start - 1];
            c.is_ascii_whitespace() || matches!(c, b'(' | b'[' | b'{' | b',' | b';' | b'"' | b'\'')
        };
        let after_ok = end == bytes.len() || {
            let c = bytes[end];
            !(c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'/')
        };
        if !before_ok || !after_ok {
            continue;
        }
        // The whitespace-delimited token around it must not be a URL.
        let tok_start = text[..start]
            .rfind(char::is_whitespace)
            .map(|p| p + 1)
            .unwrap_or(0);
        let tok_end = text[end..]
            .find(char::is_whitespace)
            .map(|p| end + p)
            .unwrap_or(text.len());
        if text[tok_start..tok_end].contains("://") {
            continue;
        }
        return true;
    }
    false
}

/// The pull request number of a squash or merge subject: `Fix crash (#123)`.
pub fn trailing_pr_number(subject: &str) -> Option<u64> {
    let s = subject.trim_end();
    let inner = s.strip_suffix(')')?;
    let open = inner.rfind("(#")?;
    let digits = &inner[open + 2..];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// A merge commit subject GitHub writes: `Merge pull request #12 from o/branch`.
pub fn merge_subject_pr_number(subject: &str) -> Option<u64> {
    let rest = subject.strip_prefix("Merge pull request #")?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Markdown and HTML markup that reads as noise in plain text.
fn markup() -> &'static [(regex::Regex, &'static str)] {
    use std::sync::OnceLock;
    static RES: OnceLock<Vec<(regex::Regex, &'static str)>> = OnceLock::new();
    RES.get_or_init(|| {
        [
            // Images: `![alt](url)` becomes `[image: alt]`.
            (r"!\[([^\]]*)\]\([^)\s]*\)", "[image: $1]"),
            // Links: `[text](url)` becomes `text`.
            (r"\[([^\]]+)\]\([^)\s]*\)", "$1"),
            // Bold.
            (r"\*\*([^*\n]+)\*\*", "$1"),
            (r"__([^_\n]+)__", "$1"),
            // Simple HTML tags.
            (
                r"(?i)</?(details|summary|br|p|b|i|em|strong|sub|sup|code|kbd|img|div|span)\b[^>]*>",
                "",
            ),
            // Heading marks at the start of a line.
            (r"(?m)^#{1,6}[ \t]+", ""),
        ]
        .into_iter()
        .filter_map(|(re, to)| regex::Regex::new(re).ok().map(|r| (r, to)))
        .collect()
    })
}

/// A GitHub body as readable plain text: HTML comments and common
/// Markdown markup (links, images, bold, headings, simple HTML tags)
/// removed, runs of blank lines collapsed, CRLF normalised, trailing space
/// trimmed. Code spans and fenced blocks keep their text.
pub fn plain_text(body: &str) -> String {
    let mut s = body.replace("\r\n", "\n");
    while let Some(start) = s.find("<!--") {
        match s[start..].find("-->") {
            Some(end) => s.replace_range(start..start + end + 3, ""),
            None => {
                s.truncate(start);
                break;
            }
        }
    }
    for (re, to) in markup() {
        if re.is_match(&s) {
            s = re.replace_all(&s, *to).into_owned();
        }
    }
    let mut out = String::with_capacity(s.len());
    let mut blank = 0;
    for line in s.lines() {
        let l = line.trim_end();
        if l.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(l);
        out.push('\n');
    }
    out.trim().to_owned()
}

/// What the repository's defaults would make the merge commit title.
pub fn default_merge_title(
    method: super::model::MergeMethod,
    meta: &super::model::RepoMeta,
    title: &str,
    number: u64,
    head_owner: &str,
    head: &str,
) -> String {
    use super::model::MergeMethod;
    match method {
        MergeMethod::Squash => format!("{title} (#{number})"),
        MergeMethod::Merge if meta.merge_title == "PR_TITLE" => format!("{title} (#{number})"),
        MergeMethod::Merge => format!("Merge pull request #{number} from {head_owner}/{head}"),
        MergeMethod::Rebase => String::new(),
    }
}

/// What GitHub puts in the merge commit message when none is given.
pub fn default_merge_message_hint(
    method: super::model::MergeMethod,
    meta: &super::model::RepoMeta,
) -> &'static str {
    use super::model::MergeMethod;
    match method {
        MergeMethod::Squash => match meta.squash_message.as_str() {
            "PR_BODY" => "Leave empty for the pull request description",
            "BLANK" => "Leave empty for no message",
            _ => "Leave empty for the list of commit messages",
        },
        MergeMethod::Merge => match meta.merge_message.as_str() {
            "PR_TITLE" => "Leave empty for the pull request title",
            "BLANK" => "Leave empty for no message",
            _ => "Leave empty for the pull request description",
        },
        MergeMethod::Rebase => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!(
            "hadion-review-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn single_pr_template_in_github_dir() {
        let d = temp_dir("tpl");
        std::fs::create_dir_all(d.join(".github")).unwrap();
        std::fs::create_dir_all(d.join("docs")).unwrap();
        std::fs::write(d.join(".github/PULL_REQUEST_TEMPLATE.md"), "## What\n").unwrap();
        std::fs::write(d.join("docs/pull_request_template.md"), "docs one").unwrap();
        let t = pr_templates(&d);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].path, ".github/PULL_REQUEST_TEMPLATE.md");
        assert_eq!(t[0].body, "## What\n");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn template_chooser_and_fallback_locations() {
        let d = temp_dir("tpls");
        std::fs::create_dir_all(d.join(".github/PULL_REQUEST_TEMPLATE")).unwrap();
        std::fs::write(d.join(".github/PULL_REQUEST_TEMPLATE/feature.md"), "F").unwrap();
        std::fs::write(d.join(".github/PULL_REQUEST_TEMPLATE/bugfix.md"), "B").unwrap();
        std::fs::write(d.join(".github/PULL_REQUEST_TEMPLATE/notes.txt"), "x").unwrap();
        std::fs::write(d.join("pull_request_template.md"), "root").unwrap();
        let t = pr_templates(&d);
        let paths: Vec<&str> = t.iter().map(|t| t.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "pull_request_template.md",
                ".github/PULL_REQUEST_TEMPLATE/bugfix.md",
                ".github/PULL_REQUEST_TEMPLATE/feature.md"
            ]
        );
        assert_eq!(t[1].name, "bugfix");
        let empty = temp_dir("none");
        assert!(pr_templates(&empty).is_empty());
        std::fs::remove_dir_all(&d).ok();
        std::fs::remove_dir_all(&empty).ok();
    }

    #[test]
    fn issue_template_front_matter() {
        let t = parse_issue_template(
            ".github/ISSUE_TEMPLATE/bug.md",
            "---\nname: Bug report\nabout: Something broke\ntitle: '[Bug] '\nlabels: bug, triage\nassignees:\n  - ann\n---\n\n## Steps\n",
        );
        assert_eq!(t.name, "Bug report");
        assert_eq!(t.about, "Something broke");
        assert_eq!(t.title, "[Bug] ");
        assert_eq!(t.labels, vec!["bug", "triage"]);
        assert_eq!(t.assignees, vec!["ann"]);
        assert_eq!(t.body, "## Steps\n");
        let plain = parse_issue_template("a/feature.md", "Just text");
        assert_eq!(plain.name, "feature");
        assert_eq!(plain.body, "Just text");
        let d = temp_dir("issues");
        std::fs::create_dir_all(d.join(".github/ISSUE_TEMPLATE")).unwrap();
        std::fs::write(
            d.join(".github/ISSUE_TEMPLATE/bug.md"),
            "---\nname: Bug\n---\nB",
        )
        .unwrap();
        std::fs::write(
            d.join(".github/ISSUE_TEMPLATE/config.yml"),
            "blank_issues_enabled: false",
        )
        .unwrap();
        let all = issue_templates(&d);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "Bug");
        std::fs::remove_dir_all(&d).ok();
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn commit_body_generation() {
        let d = temp_dir("log");
        git(&d, &["init", "-q", "-b", "main"]);
        for (k, v) in [
            ("user.name", "T"),
            ("user.email", "t@example.com"),
            ("commit.gpgsign", "false"),
        ] {
            git(&d, &["config", k, v]);
        }
        std::fs::write(d.join("a"), "1").unwrap();
        git(&d, &["add", "a"]);
        git(&d, &["commit", "-q", "-m", "Base"]);
        git(&d, &["switch", "-q", "-c", "feature/add-login"]);
        for (i, msg) in ["Add the form", "Wire the endpoint"].iter().enumerate() {
            std::fs::write(d.join("a"), format!("{}", i + 2)).unwrap();
            git(&d, &["commit", "-q", "-am", msg]);
        }
        assert!(resolves(&d, "main"));
        assert!(!resolves(&d, "origin/main"));
        assert_eq!(
            first_resolving(&d, &["origin/main".into(), "main".into()]).as_deref(),
            Some("main")
        );
        let commits = commits_between(&d, "main", "feature/add-login", 50).unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].1, "Wire the endpoint", "newest first");
        assert_eq!(
            body_from_commits(&commits),
            "- Add the form\n- Wire the endpoint\n"
        );
        assert_eq!(default_title(&commits, "feature/add-login"), "Add login");
        assert_eq!(default_title(&commits[..1], "x"), "Wire the endpoint");
        assert_eq!(
            commits_between_command("main", "x", 50),
            "git log --no-merges --format=%H%x1f%s --max-count=50 main..x --"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn titles_and_branch_names() {
        assert_eq!(humanize_branch("feature/add-login_page"), "Add login page");
        assert_eq!(humanize_branch("fix.crash"), "Fix crash");
        assert_eq!(humanize_branch("ümlaut-x"), "Ümlaut x");
        assert_eq!(
            issue_branch_name(123, "Fix the login page!"),
            "123-fix-the-login-page"
        );
        assert_eq!(issue_branch_name(7, "!!!"), "issue-7");
        assert!(issue_branch_name(9, &"word ".repeat(40)).len() <= 2 + 48);
    }

    #[test]
    fn issue_references() {
        assert!(mentions("Fix crash (#12)", 12));
        assert!(mentions("#12 at the start", 12));
        assert!(mentions("fixes #12.", 12));
        assert!(mentions("see #12, #13", 13));
        assert!(!mentions("bump #123abc", 123));
        assert!(!mentions("#120", 12));
        assert!(!mentions("PR#12x", 12));
        assert!(!mentions("PR#12", 12));
        assert!(!mentions("o/r#12", 12));
        assert!(!mentions("https://example.com/page#12", 12));
        assert!(!mentions("see https://x.com/#12 for details", 12));
        assert!(mentions("https://x.com/#12 and #12", 12));
        assert!(!mentions("issue-#12-x", 12));
    }

    #[test]
    fn pr_numbers_in_subjects() {
        assert_eq!(trailing_pr_number("Fix crash (#40847)"), Some(40847));
        assert_eq!(trailing_pr_number("Fix (#12) later"), None);
        assert_eq!(trailing_pr_number("Fix (#abc)"), None);
        assert_eq!(
            merge_subject_pr_number("Merge pull request #9 from o/x"),
            Some(9)
        );
        assert_eq!(merge_subject_pr_number("Merge branch 'x'"), None);
    }

    #[test]
    fn plain_text_cleanup() {
        let body = "<!-- security-action:summary -->\r\n## Summary\r\n\r\n\r\n\r\nText   \r\n<!-- unclosed";
        assert_eq!(plain_text(body), "Summary\n\nText");
        assert_eq!(
            plain_text("**19** files: [`a.rs`](https://x/y#diff) and ![shot](https://i/p.png)"),
            "19 files: `a.rs` and [image: shot]"
        );
        assert_eq!(
            plain_text("<details><summary>More</summary>\nBody<br/></details>"),
            "More\nBody"
        );
        assert_eq!(plain_text("x # not a heading"), "x # not a heading");
    }

    #[test]
    fn merge_defaults() {
        use super::super::model::{MergeMethod, RepoMeta};
        let meta = RepoMeta {
            merge_title: "MERGE_MESSAGE".into(),
            squash_message: "COMMIT_MESSAGES".into(),
            ..Default::default()
        };
        assert_eq!(
            default_merge_title(MergeMethod::Squash, &meta, "Add x", 5, "o", "b"),
            "Add x (#5)"
        );
        assert_eq!(
            default_merge_title(MergeMethod::Merge, &meta, "Add x", 5, "o", "b"),
            "Merge pull request #5 from o/b"
        );
        assert!(default_merge_message_hint(MergeMethod::Squash, &meta).contains("commit messages"));
    }
}
