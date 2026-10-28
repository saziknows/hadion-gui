//! Palette items: what a row shows, what it is matched on, and what it
//! stands for ([`Target`]; the app turns a target and an action into app
//! actions).

use std::path::PathBuf;

use super::fuzzy::Haystack;
use super::scope::Scope;
use crate::RefKind;

/// What kind of thing an item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Command,
    Task,
    Branch,
    Remote,
    Tag,
    Pr,
    Issue,
    File,
    Commit,
    Run,
    Repo,
    /// A prefix of the help list.
    Prefix,
}

impl Kind {
    pub const ALL: [Kind; 12] = [
        Kind::Command,
        Kind::Task,
        Kind::Branch,
        Kind::Remote,
        Kind::Tag,
        Kind::Pr,
        Kind::Issue,
        Kind::File,
        Kind::Commit,
        Kind::Run,
        Kind::Repo,
        Kind::Prefix,
    ];

    /// The scope whose prefix searches this kind.
    pub fn scope(self) -> Scope {
        match self {
            Kind::Command | Kind::Task => Scope::Commands,
            Kind::Branch | Kind::Remote | Kind::Tag => Scope::Refs,
            Kind::Pr | Kind::Issue => Scope::Prs,
            Kind::File => Scope::Files,
            Kind::Commit => Scope::Commits,
            Kind::Run => Scope::Runs,
            Kind::Repo => Scope::Repos,
            Kind::Prefix => Scope::Help,
        }
    }

    /// Searched by `scope`.
    pub fn in_scope(self, scope: Scope) -> bool {
        match scope {
            Scope::All => self != Kind::Prefix,
            s => self.scope() == s,
        }
    }

    /// Category shown on the right of a row when nothing more specific is.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Command => "Command",
            Kind::Task => "Task",
            Kind::Branch => "Branch",
            Kind::Remote => "Remote branch",
            Kind::Tag => "Tag",
            Kind::Pr => "Pull request",
            Kind::Issue => "Issue",
            Kind::File => "File",
            Kind::Commit => "Commit",
            Kind::Run => "Run",
            Kind::Repo => "Repository",
            Kind::Prefix => "Prefix",
        }
    }

    /// Added to every match of this kind: on equal text, commands and local
    /// branches come before remote branches, files and commits.
    pub fn weight(self) -> i32 {
        match self {
            Kind::Command => 14,
            Kind::Branch => 12,
            Kind::Repo => 10,
            Kind::Task => 8,
            Kind::Pr => 8,
            Kind::Issue => 6,
            Kind::Remote => 5,
            Kind::Tag => 5,
            Kind::Run => 3,
            Kind::File => 0,
            Kind::Commit => -6,
            Kind::Prefix => 0,
        }
    }
}

/// What an item stands for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// A command of the app's catalogue, by id.
    Command(String),
    Ref {
        /// Short name: `main`, `origin/main`, `v1.0`.
        name: String,
        /// Full name: `refs/heads/main`.
        full: String,
        kind: RefKind,
    },
    /// Path relative to the repository root.
    File(String),
    /// Full SHA.
    Commit(String),
    Pr(u64),
    Issue(u64),
    Run(u64),
    /// A workbench task id.
    Task(String),
    Repo(PathBuf),
    Prefix(Scope),
}

impl Target {
    pub fn kind(&self) -> Kind {
        match self {
            Target::Command(_) => Kind::Command,
            Target::Ref { kind, .. } => match kind {
                RefKind::Local => Kind::Branch,
                RefKind::Remote => Kind::Remote,
                RefKind::Tag => Kind::Tag,
            },
            Target::File(_) => Kind::File,
            Target::Commit(_) => Kind::Commit,
            Target::Pr(_) => Kind::Pr,
            Target::Issue(_) => Kind::Issue,
            Target::Run(_) => Kind::Run,
            Target::Task(_) => Kind::Task,
            Target::Repo(_) => Kind::Repo,
            Target::Prefix(_) => Kind::Prefix,
        }
    }

    /// A stable key (frecency, selection): `ref:refs/heads/main`.
    pub fn key(&self) -> String {
        match self {
            Target::Command(id) => format!("cmd:{id}"),
            Target::Ref { full, .. } => format!("ref:{full}"),
            Target::File(p) => format!("file:{p}"),
            Target::Commit(s) => format!("commit:{s}"),
            Target::Pr(n) => format!("pr:{n}"),
            Target::Issue(n) => format!("issue:{n}"),
            Target::Run(n) => format!("run:{n}"),
            Target::Task(id) => format!("task:{id}"),
            Target::Repo(p) => format!("repo:{}", p.display()),
            Target::Prefix(s) => format!("prefix:{}", s.prefix().unwrap_or(' ')),
        }
    }

    /// The target a key stands for (an action suffix is ignored).
    pub fn from_key(key: &str) -> Option<Target> {
        let key = super::frecency::entity(key);
        let (kind, rest) = key.split_once(':')?;
        if rest.is_empty() {
            return None;
        }
        Some(match kind {
            "cmd" => Target::Command(rest.to_owned()),
            "ref" => {
                let (kind, name) = if let Some(n) = rest.strip_prefix("refs/heads/") {
                    (RefKind::Local, n)
                } else if let Some(n) = rest.strip_prefix("refs/remotes/") {
                    (RefKind::Remote, n)
                } else if let Some(n) = rest.strip_prefix("refs/tags/") {
                    (RefKind::Tag, n)
                } else {
                    return None;
                };
                Target::Ref {
                    name: name.to_owned(),
                    full: rest.to_owned(),
                    kind,
                }
            }
            "file" => Target::File(rest.to_owned()),
            "commit" => Target::Commit(rest.to_owned()),
            "pr" => Target::Pr(rest.parse().ok()?),
            "issue" => Target::Issue(rest.parse().ok()?),
            "run" => Target::Run(rest.parse().ok()?),
            "task" => Target::Task(rest.to_owned()),
            "repo" => Target::Repo(PathBuf::from(rest)),
            "prefix" => {
                let c = rest.chars().next()?;
                Target::Prefix(super::scope::PREFIXES.iter().find(|p| p.ch == c)?.scope)
            }
            _ => return None,
        })
    }

    /// Belongs to one repository (refs, files, commits, pull requests,
    /// runs, tasks) rather than to the app.
    pub fn per_repo(&self) -> bool {
        !matches!(
            self,
            Target::Command(_) | Target::Repo(_) | Target::Prefix(_)
        )
    }
}

/// One row of the palette.
#[derive(Clone, Debug)]
pub struct Item {
    pub target: Target,
    pub key: String,
    /// Matched and highlighted.
    pub title: String,
    /// Category, path, branch: shown after the title.
    pub subtitle: String,
    /// Right-aligned: a shortcut (`Ctrl+K`) or a category; empty shows the
    /// kind's label.
    pub badge: String,
    /// The badge is a keyboard shortcut.
    pub shortcut: bool,
    pub hay: Haystack,
    /// Other words that find the item (aliases, author, branch), matched
    /// with `extra_adjust` added and never highlighted.
    pub extra: Option<Haystack>,
    /// Added to a token matched in `extra` (a penalty for keywords, a bonus
    /// for a file name).
    pub extra_adjust: i32,
    /// Matched by prefix only (lower-case SHA, a number).
    pub exact: Option<String>,
    /// Shortest token that counts as a prefix of `exact`.
    pub exact_min: usize,
    /// Added to the match score (kind weight and the source's own boost).
    pub weight: i32,
    /// Position in its source: the order for an empty query.
    pub order: u32,
    /// Equal scores put the shorter first: the title's length in
    /// characters, or the file name's for a path.
    pub sort_len: u32,
    /// When it happened (unix seconds), shown relative to now after the
    /// subtitle when the row is drawn.
    pub time: Option<i64>,
}

impl Item {
    pub fn new(target: Target, title: impl Into<String>, subtitle: impl Into<String>) -> Self {
        let title = title.into();
        let key = target.key();
        let weight = target.kind().weight();
        let hay = Haystack::new(&title);
        Self {
            sort_len: hay.len() as u32,
            hay,
            target,
            key,
            title,
            subtitle: subtitle.into(),
            badge: String::new(),
            shortcut: false,
            extra: None,
            extra_adjust: -super::fuzzy::KEYWORD_PENALTY,
            exact: None,
            exact_min: 1,
            weight,
            order: 0,
            time: None,
        }
    }

    pub fn kind(&self) -> Kind {
        self.target.kind()
    }

    /// A category badge.
    pub fn badge(mut self, b: impl Into<String>) -> Self {
        self.badge = b.into();
        self.shortcut = false;
        self
    }

    /// A keyboard shortcut badge (ignored when empty).
    pub fn shortcut(mut self, s: &str) -> Self {
        if !s.is_empty() {
            self.badge = s.to_owned();
            self.shortcut = true;
        }
        self
    }

    pub fn keywords(mut self, k: &str) -> Self {
        let k = k.trim();
        self.extra = (!k.is_empty()).then(|| Haystack::new(k));
        self
    }

    /// Also found by a token of at least `min` characters that starts `e`.
    pub fn exact(mut self, e: &str, min: usize) -> Self {
        self.exact = (!e.is_empty()).then(|| e.to_lowercase());
        self.exact_min = min.max(1);
        self
    }

    pub fn boost(mut self, w: i32) -> Self {
        self.weight += w;
        self
    }

    /// The text on the right: the badge, or the kind's label.
    pub fn badge_text(&self) -> &str {
        if self.badge.is_empty() {
            self.kind().label()
        } else {
            &self.badge
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip() {
        let targets = [
            Target::Command("fetch".into()),
            Target::Ref {
                name: "main".into(),
                full: "refs/heads/main".into(),
                kind: RefKind::Local,
            },
            Target::Ref {
                name: "origin/feature/x".into(),
                full: "refs/remotes/origin/feature/x".into(),
                kind: RefKind::Remote,
            },
            Target::Ref {
                name: "v1.0".into(),
                full: "refs/tags/v1.0".into(),
                kind: RefKind::Tag,
            },
            Target::File("src/a b#1.rs".into()),
            Target::Commit("0123abcd".into()),
            Target::Pr(12),
            Target::Issue(7),
            Target::Run(99),
            Target::Task("npm:web:build".into()),
            Target::Repo(PathBuf::from("C:/dev/x")),
            Target::Prefix(Scope::Files),
        ];
        for t in targets {
            let k = t.key();
            assert_eq!(Target::from_key(&k).as_ref(), Some(&t), "{k}");
            let with = super::super::frecency::with_verb(&k, "merge");
            assert_eq!(Target::from_key(&with).as_ref(), Some(&t));
        }
        assert_eq!(Target::from_key("nope"), None);
        assert_eq!(Target::from_key("pr:x"), None);
        assert_eq!(Target::from_key("ref:HEAD"), None);
        assert_eq!(Target::from_key("cmd:"), None);
    }

    #[test]
    fn kinds_and_scopes() {
        for k in Kind::ALL {
            assert!(!k.label().is_empty());
            assert_eq!(k.in_scope(Scope::All), k != Kind::Prefix);
            assert!(k.in_scope(k.scope()));
        }
        assert!(Kind::Task.in_scope(Scope::Commands));
        assert!(!Kind::File.in_scope(Scope::Refs));
        assert!(Kind::Command.weight() > Kind::Commit.weight());
    }

    #[test]
    fn builder() {
        let i = Item::new(Target::Command("x".into()), "Fetch", "Git")
            .shortcut("Ctrl+F")
            .keywords("download remote")
            .boost(3);
        assert_eq!(i.badge_text(), "Ctrl+F");
        assert!(i.shortcut);
        assert!(i.extra.is_some());
        assert_eq!(i.weight, Kind::Command.weight() + 3);
        let j = Item::new(Target::Pr(1), "Fix", "").shortcut("");
        assert_eq!(j.badge_text(), "Pull request");
        assert!(!Target::Command("x".into()).per_repo());
        assert!(Target::Pr(1).per_repo());
    }
}
