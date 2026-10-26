//! Release preparation from local history: the next version suggested by
//! the conventional commits since the latest version tag, release notes
//! grouped by kind, and per-release facts (commits since the previous one).

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use super::local::{self, LogLine};
use crate::Result;
use crate::insights::refs::parse_version;

/// Commits read for a suggestion or notes (newest first).
pub const MAX_COMMITS: usize = 2_000;

/// A commit seen through the conventional-commit lens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub sha: String,
    /// `feat`, `fix`, … lower-cased; `None` for other messages.
    pub kind: Option<String>,
    pub scope: Option<String>,
    pub breaking: bool,
    /// The message without the `type(scope)!:` prefix and PR suffix.
    pub text: String,
    pub pr: Option<u64>,
    pub author: String,
    pub time: i64,
}

fn conventional_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(\w+)(?:\(([^)]*)\))?(!)?:\s*(.*)$").expect("valid regex"))
}

fn merge_pr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^Merge pull request #(\d+) from \S+").expect("valid regex"))
}

fn pr_suffix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s*\(#(\d+)\)\s*$").expect("valid regex"))
}

/// Read a commit: pull-request merges use the pull request's title (the
/// first body line), squash merges keep their `(#123)`. Merges of branches
/// (`Merge branch 'main' into x`) give `None`: they are noise in notes.
pub fn classify(c: &LogLine) -> Option<Change> {
    let mut pr = None;
    let mut message = c.subject.clone();
    if let Some(caps) = merge_pr_re().captures(&c.subject) {
        pr = caps[1].parse().ok();
        message = c
            .body
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or(&c.subject)
            .to_owned();
    } else if c.parents > 1
        || c.subject.starts_with("Merge branch ")
        || c.subject.starts_with("Merge remote-tracking branch ")
    {
        return None;
    }
    if let Some(caps) = pr_suffix_re().captures(&message) {
        pr = pr.or_else(|| caps[1].parse().ok());
        let end = caps.get(0).map(|m| m.start()).unwrap_or(message.len());
        message.truncate(end);
    }
    let breaking_body = c.body.contains("BREAKING CHANGE") || c.body.contains("BREAKING-CHANGE");
    let (kind, scope, bang, text) = match conventional_re().captures(&message) {
        Some(caps) => (
            Some(caps[1].to_lowercase()),
            caps.get(2)
                .map(|m| m.as_str().trim().to_owned())
                .filter(|s| !s.is_empty()),
            caps.get(3).is_some(),
            caps[4].trim().to_owned(),
        ),
        None => (None, None, false, message.trim().to_owned()),
    };
    Some(Change {
        sha: c.sha.clone(),
        kind,
        scope,
        breaking: bang || breaking_body,
        text,
        pr,
        author: c.author.clone(),
        time: c.commit_time,
    })
}

/// A heading of the generated notes, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    Breaking,
    Features,
    Fixes,
    Performance,
    Documentation,
    Refactoring,
    Other,
}

impl Group {
    pub const ALL: [Group; 7] = [
        Group::Breaking,
        Group::Features,
        Group::Fixes,
        Group::Performance,
        Group::Documentation,
        Group::Refactoring,
        Group::Other,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Group::Breaking => "Breaking changes",
            Group::Features => "Features",
            Group::Fixes => "Fixes",
            Group::Performance => "Performance",
            Group::Documentation => "Documentation",
            Group::Refactoring => "Refactoring",
            Group::Other => "Other",
        }
    }

    pub fn of(c: &Change) -> Group {
        if c.breaking {
            return Group::Breaking;
        }
        match c.kind.as_deref() {
            Some("feat" | "feature") => Group::Features,
            Some("fix" | "bugfix" | "hotfix") => Group::Fixes,
            Some("perf") => Group::Performance,
            Some("docs" | "doc") => Group::Documentation,
            Some("refactor") => Group::Refactoring,
            _ => Group::Other,
        }
    }
}

/// Changes grouped by heading (empty groups left out), each oldest first.
pub fn group(changes: &[Change]) -> Vec<(Group, Vec<Change>)> {
    let mut by: HashMap<Group, Vec<Change>> = HashMap::new();
    // `changes` is newest first (git log order).
    for c in changes.iter().rev() {
        by.entry(Group::of(c)).or_default().push(c.clone());
    }
    Group::ALL
        .iter()
        .filter_map(|g| by.remove(g).map(|v| (*g, v)))
        .collect()
}

/// Markdown release notes from grouped changes. `compare` is an optional
/// "Full changelog" link.
pub fn render(groups: &[(Group, Vec<Change>)], compare: Option<&str>) -> String {
    let mut s = String::new();
    for (g, items) in groups {
        s.push_str(&format!("## {}\n\n", g.title()));
        for c in items {
            s.push_str("- ");
            if let Some(scope) = &c.scope {
                s.push_str(&format!("**{scope}:** "));
            }
            s.push_str(&c.text);
            if let Some(n) = c.pr {
                s.push_str(&format!(" (#{n})"));
            }
            if !c.author.is_empty() {
                s.push_str(&format!(" by {}", c.author));
            }
            s.push('\n');
        }
        s.push('\n');
    }
    if let Some(url) = compare {
        s.push_str(&format!("**Full changelog**: {url}\n"));
    }
    if s.is_empty() {
        s.push_str("No changes since the previous release.\n");
    }
    s
}

/// How the version changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bump {
    Major,
    Minor,
    Patch,
    /// No version tag yet.
    Initial,
}

impl Bump {
    pub fn label(self) -> &'static str {
        match self {
            Bump::Major => "major",
            Bump::Minor => "minor",
            Bump::Patch => "patch",
            Bump::Initial => "first version",
        }
    }
}

/// The suggested next version and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// Latest version tag the suggestion starts from.
    pub base_tag: Option<String>,
    pub next: String,
    pub bump: Bump,
    /// One line per reason, most important first.
    pub reasons: Vec<String>,
    pub commits: usize,
    pub breaking: usize,
    pub features: usize,
    pub fixes: usize,
    pub other: usize,
}

/// Is `name` a pre-release (`1.2.0-rc1`, `2.0b2`)?
pub fn is_prerelease_tag(name: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?i)\d(?:[-_.+]?(?:a|b|rc|alpha|beta|pre|dev|preview|snapshot)\d*)(?:$|[-_.+])",
        )
        .expect("valid regex")
    });
    re.is_match(name)
}

/// Calendar versions (`2024.03.1`) are not bumped by commit type.
fn is_calver(v: (u64, u64, u64)) -> bool {
    (1990..=2100).contains(&v.0)
}

/// The latest version tag: the highest semantic version among `tags`,
/// ignoring pre-releases (unless there is nothing else) and calendar
/// versions.
pub fn latest_version_tag(tags: &[String]) -> Option<String> {
    let versions: Vec<(&String, (u64, u64, u64), bool)> = tags
        .iter()
        .filter_map(|t| parse_version(t).map(|v| (t, v, is_prerelease_tag(t))))
        .filter(|(_, v, _)| !is_calver(*v))
        .collect();
    let finals: Vec<&(&String, (u64, u64, u64), bool)> =
        versions.iter().filter(|(_, _, pre)| !pre).collect();
    let pool: Vec<&(&String, (u64, u64, u64), bool)> = if finals.is_empty() {
        versions.iter().collect()
    } else {
        finals
    };
    pool.iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.len().cmp(&a.0.len())))
        .map(|(t, _, _)| (*t).clone())
}

/// Suggest the next version: a breaking change (`!` or `BREAKING CHANGE`)
/// bumps the major, a `feat` the minor, anything else the patch. The tag
/// prefix (`v`, `release-`) is kept.
pub fn suggest(base_tag: Option<&str>, changes: &[Change]) -> Suggestion {
    let breaking: Vec<&Change> = changes.iter().filter(|c| c.breaking).collect();
    let features = changes
        .iter()
        .filter(|c| !c.breaking && matches!(c.kind.as_deref(), Some("feat" | "feature")))
        .count();
    let fixes = changes
        .iter()
        .filter(|c| !c.breaking && matches!(c.kind.as_deref(), Some("fix" | "bugfix" | "hotfix")))
        .count();
    let other = changes.len() - breaking.len() - features - fixes;
    let plural = |n: usize, one: &str, many: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {many}")
        }
    };
    let base = base_tag.and_then(|t| parse_version(t).map(|v| (t, v)));
    let mut reasons = Vec::new();
    let Some((tag, (major, minor, patch))) = base else {
        let mut reasons = vec!["No version tag yet, so this starts at 0.1.0.".to_owned()];
        if !changes.is_empty() {
            reasons.push(format!(
                "{} in the history so far.",
                plural(changes.len(), "commit", "commits")
            ));
        }
        return Suggestion {
            base_tag: None,
            next: "v0.1.0".to_owned(),
            bump: Bump::Initial,
            reasons,
            commits: changes.len(),
            breaking: breaking.len(),
            features,
            fixes,
            other,
        };
    };
    let prefix = &tag[..tag.find(|c: char| c.is_ascii_digit()).unwrap_or(0)];
    let (bump, next) = if !breaking.is_empty() {
        let first = breaking[0];
        reasons.push(format!(
            "{} → major: \"{}\"{}.",
            plural(breaking.len(), "breaking change", "breaking changes"),
            short(&first.text),
            if breaking.len() > 1 { " and more" } else { "" }
        ));
        (Bump::Major, (major + 1, 0, 0))
    } else if features > 0 {
        reasons.push(format!(
            "{} (feat) and no breaking change → minor.",
            plural(features, "feature", "features")
        ));
        (Bump::Minor, (major, minor + 1, 0))
    } else {
        reasons.push(if fixes > 0 {
            format!(
                "{} and no feature or breaking change → patch.",
                plural(fixes, "fix", "fixes")
            )
        } else {
            "No feature or breaking change → patch.".to_owned()
        });
        (Bump::Patch, (major, minor, patch + 1))
    };
    reasons.push(format!(
        "{} since {tag}: {} breaking, {} feat, {} fix, {} other.",
        plural(changes.len(), "commit", "commits"),
        breaking.len(),
        features,
        fixes,
        other
    ));
    if changes.is_empty() {
        reasons.push(format!(
            "Nothing changed since {tag}: a release would repeat it."
        ));
    }
    Suggestion {
        base_tag: Some(tag.to_owned()),
        next: format!("{prefix}{}.{}.{}", next.0, next.1, next.2),
        bump,
        reasons,
        commits: changes.len(),
        breaking: breaking.len(),
        features,
        fixes,
        other,
    }
}

fn short(s: &str) -> String {
    let mut out: String = s.chars().take(70).collect();
    if s.chars().count() > 70 {
        out.push('…');
    }
    out
}

/// What the publish form needs from local history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub target: String,
    pub suggestion: Suggestion,
    /// Notes grouped from the first-parent history since the base tag.
    pub notes: String,
    pub groups: Vec<(Group, usize)>,
    /// The commit limit cut the range.
    pub truncated: bool,
}

/// Read the commits between the latest version tag (or `previous`, when
/// given) and `target`, suggest a version and write grouped notes.
pub fn prepare(
    dir: &Path,
    tags: &[String],
    previous: Option<&str>,
    target: &str,
    compare_url: Option<&dyn Fn(&str, &str) -> String>,
    commands: &mut Vec<String>,
) -> Result<Prepared> {
    let target = if target.trim().is_empty() {
        "HEAD"
    } else {
        target.trim()
    };
    if local::resolve(dir, target, commands).is_none() {
        return Err(local::not_found(target));
    }
    let base = previous
        .map(str::to_owned)
        .filter(|p| !p.trim().is_empty())
        .or_else(|| latest_version_tag(tags));
    let range = match &base {
        Some(b) => format!("{b}..{target}"),
        None => target.to_owned(),
    };
    // All commits for the version (a breaking change in any commit
    // counts), the mainline for the notes (one line per pull request).
    let all = local::log(dir, &range, MAX_COMMITS, false, true, commands)?;
    let mainline = local::log(dir, &range, MAX_COMMITS, true, true, commands)?;
    let truncated = all.len() >= MAX_COMMITS;
    let changes: Vec<Change> = all.iter().filter_map(classify).collect();
    let suggestion = suggest(base.as_deref(), &changes);
    let main_changes: Vec<Change> = mainline.iter().filter_map(classify).collect();
    let grouped = group(&main_changes);
    let compare = match (&base, compare_url) {
        (Some(b), Some(f)) => Some(f(b, &suggestion.next)),
        _ => None,
    };
    let notes = render(&grouped, compare.as_deref());
    Ok(Prepared {
        target: target.to_owned(),
        suggestion,
        notes,
        groups: grouped.iter().map(|(g, v)| (*g, v.len())).collect(),
        truncated,
    })
}

/// Local facts about the listed releases: for each `(tag, previous tag)`
/// pair, the commit the tag points at locally (`None` when the tag is not
/// here) and `git rev-list --count previous..tag` when both are.
pub fn release_facts(
    dir: &Path,
    pairs: &[(String, Option<String>)],
    commands: &mut Vec<String>,
) -> HashMap<String, (Option<String>, Option<u32>)> {
    let mut names: Vec<String> = pairs
        .iter()
        .map(|(t, _)| format!("refs/tags/{t}"))
        .collect();
    names.extend(
        pairs
            .iter()
            .filter_map(|(_, p)| p.as_ref().map(|p| format!("refs/tags/{p}"))),
    );
    names.sort();
    names.dedup();
    let resolved: HashMap<String, Option<String>> = names
        .iter()
        .cloned()
        .zip(local::resolve_many(dir, &names, commands))
        .collect();
    let mut tally = local::Tally::new("git rev-list --count");
    let mut out = HashMap::new();
    for (tag, prev) in pairs {
        let here = resolved.get(&format!("refs/tags/{tag}")).cloned().flatten();
        let prev_here = prev
            .as_ref()
            .and_then(|p| resolved.get(&format!("refs/tags/{p}")).cloned().flatten());
        let count = match (&here, &prev_here) {
            (Some(a), Some(b)) => {
                let range = format!("{b}..{a}");
                tally.record(format!("git rev-list --count {range}"), commands);
                crate::cli::run(dir, &["rev-list", "--count", &range], None)
                    .ok()
                    .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
            }
            _ => None,
        };
        out.insert(tag.clone(), (here, count));
    }
    tally.finish(commands);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(sha: &str, subject: &str, body: &str, parents: usize) -> LogLine {
        LogLine {
            sha: sha.into(),
            author: "Ada".into(),
            author_time: 1,
            commit_time: 2,
            subject: subject.into(),
            body: body.into(),
            parents,
        }
    }

    #[test]
    fn classifies_conventional_merges_and_squashes() {
        let c = classify(&line("a", "feat(ui)!: new layout (#12)", "", 1)).unwrap();
        assert_eq!(c.kind.as_deref(), Some("feat"));
        assert_eq!(c.scope.as_deref(), Some("ui"));
        assert!(c.breaking);
        assert_eq!(c.text, "new layout");
        assert_eq!(c.pr, Some(12));
        let m = classify(&line(
            "b",
            "Merge pull request #34 from me/fix-crash",
            "fix: crash on start\n\nlong text",
            2,
        ))
        .unwrap();
        assert_eq!(m.pr, Some(34));
        assert_eq!(m.kind.as_deref(), Some("fix"));
        assert_eq!(m.text, "crash on start");
        assert!(classify(&line("c", "Merge branch 'main' into x", "", 2)).is_none());
        let plain = classify(&line("d", "Update README", "BREAKING CHANGE: api", 1)).unwrap();
        assert_eq!(plain.kind, None);
        assert!(plain.breaking);
    }

    fn change(kind: Option<&str>, breaking: bool, text: &str) -> Change {
        Change {
            sha: text.into(),
            kind: kind.map(str::to_owned),
            scope: None,
            breaking,
            text: text.into(),
            pr: None,
            author: "Ada".into(),
            time: 0,
        }
    }

    #[test]
    fn next_version_rules() {
        let feat = change(Some("feat"), false, "add x");
        let fix = change(Some("fix"), false, "fix y");
        let chore = change(Some("chore"), false, "bump");
        let brk = change(Some("refactor"), true, "drop old api");
        let s = suggest(Some("v1.2.3"), &[fix.clone(), chore.clone()]);
        assert_eq!((s.next.as_str(), s.bump), ("v1.2.4", Bump::Patch));
        assert!(s.reasons[0].contains("1 fix"), "{:?}", s.reasons);
        let s = suggest(Some("v1.2.3"), &[fix.clone(), feat.clone()]);
        assert_eq!((s.next.as_str(), s.bump), ("v1.3.0", Bump::Minor));
        let s = suggest(Some("release-1.2"), &[feat, brk.clone(), fix]);
        assert_eq!((s.next.as_str(), s.bump), ("release-2.0.0", Bump::Major));
        assert!(s.reasons[0].contains("drop old api"));
        assert_eq!((s.breaking, s.features, s.fixes, s.other), (1, 1, 1, 0));
        let s = suggest(None, &[chore]);
        assert_eq!((s.next.as_str(), s.bump), ("v0.1.0", Bump::Initial));
        let s = suggest(Some("2.0.0"), &[]);
        assert_eq!(s.next, "2.0.1");
        assert!(s.reasons.iter().any(|r| r.contains("Nothing changed")));
    }

    #[test]
    fn latest_tag_ignores_prereleases_and_calver() {
        let tags: Vec<String> = [
            "v1.9.0",
            "v1.10.0-rc1",
            "v1.10.0-rc2",
            "2024.01.1",
            "v1.2.0",
            "foo",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(latest_version_tag(&tags).as_deref(), Some("v1.9.0"));
        let pre: Vec<String> = vec!["v2.0.0-beta1".into(), "v2.0.0-beta2".into()];
        assert!(latest_version_tag(&pre).is_some());
        assert_eq!(latest_version_tag(&[]), None);
        assert!(is_prerelease_tag("1.0.0rc1"));
        assert!(is_prerelease_tag("v2.0.0-beta.3"));
        assert!(!is_prerelease_tag("v2.0.0"));
        assert!(!is_prerelease_tag("v2.0.10"));
    }

    #[test]
    fn groups_in_order_and_renders_markdown() {
        let mut c = vec![
            change(Some("docs"), false, "explain"),
            change(Some("fix"), false, "crash"),
            change(None, false, "tidy"),
            change(Some("feat"), true, "new api"),
            change(Some("perf"), false, "faster"),
            change(Some("refactor"), false, "split"),
            change(Some("feat"), false, "button"),
        ];
        c[1].pr = Some(9);
        c[6].scope = Some("ui".into());
        let g = group(&c);
        let order: Vec<Group> = g.iter().map(|(g, _)| *g).collect();
        assert_eq!(
            order,
            vec![
                Group::Breaking,
                Group::Features,
                Group::Fixes,
                Group::Performance,
                Group::Documentation,
                Group::Refactoring,
                Group::Other
            ]
        );
        let md = render(&g, Some("https://github.com/o/r/compare/v1...v2"));
        assert!(md.starts_with("## Breaking changes\n\n- new api by Ada\n"));
        assert!(md.contains("## Fixes\n\n- crash (#9) by Ada\n"));
        assert!(md.contains("- **ui:** button by Ada"));
        assert!(md.ends_with("**Full changelog**: https://github.com/o/r/compare/v1...v2\n"));
        assert_eq!(
            render(&[], None),
            "No changes since the previous release.\n"
        );
    }
}
