//! Git configuration as git sees it: every entry of
//! `git config --list --show-origin --show-scope -z` with its scope, the
//! file and line it comes from, the effective value of a key (the last one
//! git reads) and which entries a later scope overrides.
//!
//! git does not print line numbers, so [`locate_lines`] reads each origin
//! file with a small parser of the config syntax ([`file_keys`]) and gives
//! the n-th entry of a key in a file the n-th line that sets it there.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::{Scope, git, slash};

/// Where an entry was read from (`--show-origin`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// `file:PATH`, as git printed it (may be relative to where git ran).
    File(String),
    /// `command line:` (`git -c`, `GIT_CONFIG_PARAMETERS`).
    CommandLine,
    /// `standard input:`.
    Stdin,
    /// `blob:OID` and other kinds, with what followed the colon.
    Other(String),
}

impl Origin {
    pub fn parse(s: &str) -> Origin {
        if let Some(p) = s.strip_prefix("file:") {
            Origin::File(p.to_owned())
        } else if s.starts_with("command line:") {
            Origin::CommandLine
        } else if s.starts_with("standard input:") {
            Origin::Stdin
        } else {
            Origin::Other(s.to_owned())
        }
    }

    /// Short text for tables.
    pub fn label(&self) -> String {
        match self {
            Origin::File(p) => p.clone(),
            Origin::CommandLine => "command line".to_owned(),
            Origin::Stdin => "standard input".to_owned(),
            Origin::Other(s) => s.clone(),
        }
    }
}

/// One `key=value` git read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub scope: Scope,
    pub origin: Origin,
    /// The origin file, resolved against the folder git ran in.
    pub file: Option<PathBuf>,
    /// As git prints it: section and name lower-case, subsection as written.
    pub key: String,
    /// `None` for a bare `name` line (a boolean that means true).
    pub value: Option<String>,
    /// 1-based line in `file`, when it could be found.
    pub line: Option<u32>,
}

impl Entry {
    pub fn section(&self) -> &str {
        split_key(&self.key).0
    }

    pub fn subsection(&self) -> Option<&str> {
        split_key(&self.key).1
    }

    pub fn name(&self) -> &str {
        split_key(&self.key).2
    }

    /// The value as text (`true` for a bare boolean).
    pub fn value_text(&self) -> &str {
        self.value.as_deref().unwrap_or("true")
    }

    /// `~/.gitconfig:12` style location.
    pub fn location(&self, home: Option<&Path>) -> String {
        match (&self.file, self.line) {
            (Some(f), Some(l)) => format!("{}:{l}", super::tilde(f, home)),
            (Some(f), None) => super::tilde(f, home),
            (None, _) => self.origin.label(),
        }
    }
}

/// `(section, subsection, name)` of `section.sub.sec.tion.name`: the
/// subsection is everything between the first and the last dot.
pub fn split_key(key: &str) -> (&str, Option<&str>, &str) {
    match (key.find('.'), key.rfind('.')) {
        (Some(a), Some(b)) if a == b => (&key[..a], None, &key[a + 1..]),
        (Some(a), Some(b)) => (&key[..a], Some(&key[a + 1..b]), &key[b + 1..]),
        _ => (key, None, ""),
    }
}

/// The canonical spelling git prints: section and name lower-case, the
/// subsection unchanged (`commit.gpgSign` → `commit.gpgsign`).
pub fn norm_key(key: &str) -> String {
    let (s, sub, n) = split_key(key.trim());
    match sub {
        Some(sub) => format!(
            "{}.{sub}.{}",
            s.to_ascii_lowercase(),
            n.to_ascii_lowercase()
        ),
        None => format!("{}.{}", s.to_ascii_lowercase(), n.to_ascii_lowercase()),
    }
}

/// The arguments of the listing command.
pub const LIST_ARGS: [&str; 5] = ["config", "--list", "--show-origin", "--show-scope", "-z"];

/// Parse `git config --list --show-origin --show-scope -z` output. With
/// `-z` every entry is `scope NUL origin NUL key LF value NUL` (`key NUL`
/// for a bare boolean). Relative file origins are resolved against `base`.
pub fn parse_list(bytes: &[u8], base: &Path) -> Vec<Entry> {
    let text = String::from_utf8_lossy(bytes);
    let mut fields = text.split('\0');
    let mut out = Vec::new();
    while let Some(scope) = fields.next() {
        if scope.is_empty() {
            break;
        }
        let (Some(origin), Some(kv)) = (fields.next(), fields.next()) else {
            break;
        };
        let (key, value) = match kv.split_once('\n') {
            Some((k, v)) => (k.to_owned(), Some(v.to_owned())),
            None => (kv.to_owned(), None),
        };
        let origin = Origin::parse(origin);
        let file = match &origin {
            Origin::File(p) => {
                let pb = PathBuf::from(super::from_msys(p));
                Some(if pb.is_absolute() { pb } else { base.join(pb) })
            }
            _ => None,
        };
        out.push(Entry {
            scope: Scope::parse(scope),
            origin,
            file,
            key,
            value,
            line: None,
        });
    }
    out
}

/// Parse `git config --file F --list -z` (no origin or scope):
/// `key LF value NUL`.
pub fn parse_plain_list(bytes: &[u8]) -> Vec<(String, Option<String>)> {
    String::from_utf8_lossy(bytes)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|kv| match kv.split_once('\n') {
            Some((k, v)) => (k.to_owned(), Some(v.to_owned())),
            None => (kv.to_owned(), None),
        })
        .collect()
}

/// Does the rest of a value line end with a backslash that continues it
/// on the next line (outside quotes and comments)?
fn continues(value: &str) -> bool {
    let mut quoted = false;
    let mut chars = value.chars().peekable();
    let mut last_backslash = false;
    while let Some(c) = chars.next() {
        last_backslash = false;
        match c {
            '\\' => {
                if chars.peek().is_some() {
                    chars.next();
                } else {
                    last_backslash = true;
                }
            }
            '"' => quoted = !quoted,
            '#' | ';' if !quoted => return false,
            _ => {}
        }
    }
    last_backslash
}

/// Parse a section header line starting with `[`; returns the canonical
/// section (`remote.origin`) and the rest of the line after `]`.
fn header(line: &str) -> Option<(String, &str)> {
    let inner = line.strip_prefix('[')?;
    if let Some(q) = inner.find('"') {
        let name = inner[..q].trim().to_ascii_lowercase();
        let mut sub = String::new();
        let mut it = inner[q + 1..].char_indices();
        let mut end = None;
        while let Some((i, c)) = it.next() {
            match c {
                '\\' => {
                    if let Some((_, n)) = it.next() {
                        sub.push(n);
                    }
                }
                '"' => {
                    end = Some(q + 1 + i + 1);
                    break;
                }
                _ => sub.push(c),
            }
        }
        let after = &inner[end?..];
        let close = after.find(']')?;
        Some((format!("{name}.{sub}"), &after[close + 1..]))
    } else {
        let close = inner.find(']')?;
        let body = inner[..close].trim();
        let canon = match body.split_once('.') {
            // Deprecated `[section.sub]`: the subsection is lower-cased.
            Some((s, sub)) => format!("{}.{}", s.to_ascii_lowercase(), sub.to_ascii_lowercase()),
            None => body.to_ascii_lowercase(),
        };
        Some((canon, &inner[close + 1..]))
    }
}

/// Every key a config file sets, canonical (`remote.origin.url`), with its
/// 1-based line, in file order.
pub fn file_keys(text: &str) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    let mut section = String::new();
    let mut continuation = false;
    for (i, raw) in text.lines().enumerate() {
        let line_no = i as u32 + 1;
        let line = raw.trim_end_matches('\r');
        if continuation {
            continuation = continues(line);
            continue;
        }
        let mut s = line.trim_start();
        if s.starts_with('[') {
            match header(s) {
                Some((sec, rest)) => {
                    section = sec;
                    s = rest.trim_start();
                }
                None => continue,
            }
        }
        if s.is_empty() || s.starts_with('#') || s.starts_with(';') {
            continue;
        }
        let name: String = s
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        if name.is_empty() || section.is_empty() {
            continue;
        }
        out.push((format!("{section}.{}", name.to_ascii_lowercase()), line_no));
        if let Some(eq) = s.find('=') {
            continuation = continues(&s[eq + 1..]);
        }
    }
    out
}

fn path_key(p: &Path) -> String {
    let s = slash(p);
    if cfg!(windows) { s.to_lowercase() } else { s }
}

/// Fill in `line` for entries whose file can be read: the n-th entry of a
/// key from a file gets the n-th line that sets the key in that file.
pub fn locate_lines(entries: &mut [Entry]) {
    let mut files: HashMap<String, HashMap<String, Vec<u32>>> = HashMap::new();
    let mut used: HashMap<(String, String), usize> = HashMap::new();
    for e in entries.iter_mut() {
        let Some(f) = &e.file else { continue };
        let fk = path_key(f);
        let index = files.entry(fk.clone()).or_insert_with(|| {
            let mut m: HashMap<String, Vec<u32>> = HashMap::new();
            if let Ok(text) = std::fs::read_to_string(f) {
                for (k, l) in file_keys(&text) {
                    m.entry(k).or_default().push(l);
                }
            }
            m
        });
        // Subsections compare exactly; the parser canonicalises like git.
        let n = used.entry((fk, e.key.clone())).or_insert(0);
        if let Some(l) = index.get(&e.key).and_then(|v| v.get(*n)) {
            e.line = Some(*l);
        }
        *n += 1;
    }
}

/// One config file git read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigFile {
    pub path: PathBuf,
    pub scope: Scope,
    pub entries: usize,
}

/// Everything `git config --list` showed in one folder.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub entries: Vec<Entry>,
    /// Where git ran (the active repository).
    pub dir: PathBuf,
    /// Files with entries, in the order git read them.
    pub files: Vec<ConfigFile>,
    /// The file `git config --global` writes.
    pub global_file: Option<PathBuf>,
    pub elapsed_ms: u64,
}

impl Snapshot {
    pub fn from_entries(entries: Vec<Entry>, dir: PathBuf) -> Snapshot {
        let mut files: Vec<ConfigFile> = Vec::new();
        for e in &entries {
            let Some(f) = &e.file else { continue };
            match files.iter_mut().find(|c| super::same_path(&c.path, f)) {
                Some(c) => c.entries += 1,
                None => files.push(ConfigFile {
                    path: f.clone(),
                    scope: e.scope,
                    entries: 1,
                }),
            }
        }
        Snapshot {
            entries,
            dir,
            files,
            global_file: super::global_config_file(),
            elapsed_ms: 0,
        }
    }

    /// The last entry of `key` (any spelling): the value git uses.
    pub fn effective(&self, key: &str) -> Option<&Entry> {
        effective(&self.entries, key)
    }

    /// Every entry of `key`, in reading order.
    pub fn values(&self, key: &str) -> Vec<&Entry> {
        values(&self.entries, key)
    }

    pub fn count(&self, scope: Scope) -> usize {
        self.entries.iter().filter(|e| e.scope == scope).count()
    }

    /// Keys set in more than one scope.
    pub fn multi_scope_keys(&self) -> usize {
        let mut scopes: HashMap<&str, Vec<Scope>> = HashMap::new();
        for e in &self.entries {
            let v = scopes.entry(&e.key).or_default();
            if !v.contains(&e.scope) {
                v.push(e.scope);
            }
        }
        scopes.values().filter(|v| v.len() > 1).count()
    }
}

/// Run the listing in `dir` and locate every entry's line.
pub fn load(dir: &Path, cmds: &mut Vec<String>) -> Result<Snapshot, String> {
    let t = Instant::now();
    let out = git(dir, &LIST_ARGS, cmds)?;
    let mut entries = parse_list(&out, dir);
    locate_lines(&mut entries);
    let mut snap = Snapshot::from_entries(entries, dir.to_path_buf());
    snap.elapsed_ms = t.elapsed().as_millis() as u64;
    Ok(snap)
}

/// Every entry of `key`, in reading order.
pub fn values<'a>(entries: &'a [Entry], key: &str) -> Vec<&'a Entry> {
    let k = norm_key(key);
    entries.iter().filter(|e| e.key == k).collect()
}

/// The value git uses for `key`: the last one read.
pub fn effective<'a>(entries: &'a [Entry], key: &str) -> Option<&'a Entry> {
    let k = norm_key(key);
    entries.iter().rev().find(|e| e.key == k)
}

/// Keys that hold a list (every value counts) rather than one value (the
/// last wins). Patterns use `*` for the subsection.
const MULTI: [&str; 22] = [
    "remote.*.fetch",
    "remote.*.push",
    "remote.*.url",
    "remote.*.pushurl",
    "include.path",
    "includeif.*.path",
    "url.*.insteadof",
    "url.*.pushinsteadof",
    "credential.helper",
    "credential.*.helper",
    "http.extraheader",
    "http.*.extraheader",
    "branch.*.merge",
    "safe.directory",
    "log.excludedecoration",
    "transfer.hiderefs",
    "uploadpack.hiderefs",
    "receive.hiderefs",
    "notes.displayref",
    "maintenance.repo",
    "push.pushoption",
    "blame.ignorerevsfile",
];

/// Does `key` hold a list of values?
pub fn is_multi(key: &str) -> bool {
    let k = norm_key(key);
    let (s, sub, n) = split_key(&k);
    MULTI.iter().any(|m| {
        let (ms, msub, mn) = split_key(m);
        ms == s
            && mn == n
            && match (msub, sub) {
                (None, None) => true,
                (Some("*"), Some(_)) => true,
                (Some(a), Some(b)) => a == b,
                _ => false,
            }
    })
}

/// The entry that overrides entry `i` (a later entry of the same
/// single-valued key), if any.
pub fn overridden_by(entries: &[Entry], i: usize) -> Option<&Entry> {
    let e = entries.get(i)?;
    if is_multi(&e.key) {
        return None;
    }
    entries[i + 1..].iter().rev().find(|o| o.key == e.key)
}

/// Entries of one section (`remote "origin"`), for the grouped table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// `remote.origin`: what collapsed groups are remembered by.
    pub id: String,
    /// `remote "origin"`, as the section header reads in a file.
    pub title: String,
    /// Indexes into the entries, in reading order.
    pub items: Vec<usize>,
}

/// Does entry `e` match a filter text (key, value, scope, origin)?
pub fn matches(e: &Entry, filter: &str) -> bool {
    let f = filter.trim().to_lowercase();
    if f.is_empty() {
        return true;
    }
    e.key.to_lowercase().contains(&f)
        || e.value_text().to_lowercase().contains(&f)
        || e.scope.label().contains(&f)
        || e.origin.label().to_lowercase().contains(&f)
}

/// Entries grouped by section and subsection, sorted by section name, with
/// the filter applied (groups without matches are left out).
pub fn groups(entries: &[Entry], filter: &str) -> Vec<Group> {
    let mut out: Vec<Group> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        if !matches(e, filter) {
            continue;
        }
        let (s, sub, _) = split_key(&e.key);
        let id = match sub {
            Some(sub) => format!("{s}.{sub}"),
            None => s.to_owned(),
        };
        match out.iter_mut().find(|g| g.id == id) {
            Some(g) => g.items.push(i),
            None => out.push(Group {
                title: match sub {
                    Some(sub) => format!("{s} \"{sub}\""),
                    None => s.to_owned(),
                },
                id,
                items: vec![i],
            }),
        }
    }
    out.sort_by_key(|g| g.id.to_lowercase());
    out
}

/// Check a key typed by the user: `section.name` or
/// `section.subsection.name`, git's character rules.
pub fn validate_key(key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("Enter a key, e.g. core.editor".to_owned());
    }
    let (s, sub, n) = split_key(key);
    if n.is_empty() || s.is_empty() || !key.contains('.') {
        return Err(format!(
            "{key} is not a config key: it needs a section and a name, e.g. core.editor"
        ));
    }
    if !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(format!(
            "The section \"{s}\" may only hold letters, digits and dashes"
        ));
    }
    if !n.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        || !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(format!(
            "The name \"{n}\" must start with a letter and hold only letters, digits and dashes"
        ));
    }
    if sub.is_some_and(|s| s.contains(['\n', '\0'])) {
        return Err("The subsection cannot contain a line break".to_owned());
    }
    Ok(())
}

/// Check a value typed by the user.
pub fn validate_value(v: &str) -> Result<(), String> {
    if v.contains(['\n', '\r', '\0']) {
        return Err("Values cannot contain line breaks here".to_owned());
    }
    Ok(())
}

fn scope_flag(scope: Scope) -> Result<&'static str, String> {
    if !scope.editable() {
        return Err(format!(
            "gitvis does not write {} config; edit it as an administrator",
            scope.label()
        ));
    }
    scope.flag().ok_or_else(|| "No scope".to_owned())
}

/// `git config --global KEY VALUE`, or `--add` for keys that hold a list.
pub fn set_args(scope: Scope, key: &str, value: &str) -> Result<Vec<String>, String> {
    validate_key(key)?;
    validate_value(value)?;
    let mut a = vec!["config".to_owned(), scope_flag(scope)?.to_owned()];
    if is_multi(key) {
        a.push("--add".to_owned());
    }
    a.push(key.trim().to_owned());
    a.push(value.to_owned());
    Ok(a)
}

/// `git config --global --unset KEY`; for a key that holds a list, only the
/// entry with `value` (`--fixed-value --unset KEY VALUE`).
pub fn unset_args(scope: Scope, key: &str, value: Option<&str>) -> Result<Vec<String>, String> {
    validate_key(key)?;
    let mut a = vec!["config".to_owned(), scope_flag(scope)?.to_owned()];
    match value.filter(|_| is_multi(key)) {
        Some(v) => {
            a.push("--fixed-value".to_owned());
            a.push("--unset".to_owned());
            a.push(key.trim().to_owned());
            a.push(v.to_owned());
        }
        None => {
            a.push("--unset".to_owned());
            a.push(key.trim().to_owned());
        }
    }
    Ok(a)
}

/// How a common setting is edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bool,
    /// One of these `(value, what it does)`.
    Choice(&'static [(&'static str, &'static str)]),
    /// Free text with a hint and suggestions.
    Text {
        hint: &'static str,
        suggestions: &'static [&'static str],
    },
}

/// A setting most people want to know about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Common {
    /// As documented (`init.defaultBranch`).
    pub key: &'static str,
    pub title: &'static str,
    pub about: &'static str,
    pub kind: Kind,
    /// The value gitvis suggests, and why.
    pub recommended: Option<(&'static str, &'static str)>,
    /// What can go wrong.
    pub warning: Option<&'static str>,
    /// What git does when the key is not set.
    pub default: &'static str,
    /// Group in the form.
    pub group: &'static str,
}

/// The common settings form, in display order.
pub const COMMON: [Common; 20] = [
    Common {
        key: "user.name",
        title: "Name",
        about: "Your name on the commits you make. It is copied into every commit and cannot be changed later without rewriting history.",
        kind: Kind::Text {
            hint: "Ada Lovelace",
            suggestions: &[],
        },
        recommended: None,
        warning: None,
        default: "not set: git refuses to commit",
        group: "Identity",
    },
    Common {
        key: "user.email",
        title: "E-mail",
        about: "The address on your commits. GitHub links a commit to your account by this address; its noreply address (ID+login@users.noreply.github.com) keeps your mailbox private.",
        kind: Kind::Text {
            hint: "you@example.com",
            suggestions: &[],
        },
        recommended: None,
        warning: None,
        default: "not set: git refuses to commit",
        group: "Identity",
    },
    Common {
        key: "init.defaultBranch",
        title: "First branch of new repositories",
        about: "The branch git init creates. GitHub, GitLab and Azure DevOps name it main.",
        kind: Kind::Text {
            hint: "main",
            suggestions: &["main", "master", "trunk", "develop"],
        },
        recommended: Some(("main", "the default on every major host")),
        warning: None,
        default: "master (with a hint on every git init)",
        group: "Branches",
    },
    Common {
        key: "pull.rebase",
        title: "Pull rebases",
        about: "What git pull does when your branch and its upstream both have new commits.",
        kind: Kind::Choice(&[
            ("false", "merge the upstream into your branch"),
            ("true", "replay your commits on top of the upstream"),
            ("merges", "rebase, keeping your merge commits"),
            ("interactive", "rebase, opening the todo list first"),
        ]),
        recommended: None,
        warning: None,
        default: "false (merge), with a warning when pull.ff is not set either",
        group: "Pull and push",
    },
    Common {
        key: "pull.ff",
        title: "Pull fast-forward",
        about: "Whether git pull may create a merge commit.",
        kind: Kind::Choice(&[
            ("true", "fast-forward when possible, merge otherwise"),
            ("only", "fast-forward or refuse: never a surprise merge"),
            ("false", "always create a merge commit"),
        ]),
        recommended: Some((
            "only",
            "pull never creates a merge you did not ask for; diverged branches stop with a message",
        )),
        warning: None,
        default: "true",
        group: "Pull and push",
    },
    Common {
        key: "push.autoSetupRemote",
        title: "Push sets up the remote branch",
        about: "The first git push of a new branch creates it on the remote and tracks it, without --set-upstream.",
        kind: Kind::Bool,
        recommended: Some(("true", "no more \"has no upstream branch\" errors")),
        warning: None,
        default: "false",
        group: "Pull and push",
    },
    Common {
        key: "push.default",
        title: "What a bare git push sends",
        about: "Which branch git push pushes when you name none.",
        kind: Kind::Choice(&[
            (
                "simple",
                "the current branch to its upstream of the same name",
            ),
            ("current", "the current branch to a branch of the same name"),
            (
                "upstream",
                "the current branch to its upstream, whatever its name",
            ),
            ("matching", "every branch that exists on both sides"),
            ("nothing", "nothing: always name what to push"),
        ]),
        recommended: Some(("simple", "the safe default")),
        warning: None,
        default: "simple",
        group: "Pull and push",
    },
    Common {
        key: "fetch.prune",
        title: "Fetch prunes",
        about: "Every fetch deletes remote-tracking branches whose branch is gone on the remote, so the branch list stays true.",
        kind: Kind::Bool,
        recommended: Some(("true", "deleted branches stop showing up in the graph")),
        warning: None,
        default: "false",
        group: "Pull and push",
    },
    Common {
        key: "rebase.autoStash",
        title: "Rebase stashes first",
        about: "Uncommitted changes are stashed before a rebase starts and restored after it, instead of the rebase refusing to start.",
        kind: Kind::Bool,
        recommended: Some(("true", "rebase and pull --rebase work with a dirty tree")),
        warning: None,
        default: "false",
        group: "Rebase and merge",
    },
    Common {
        key: "rebase.updateRefs",
        title: "Rebase moves stacked branches",
        about: "When you rebase a branch that has other branches on top of it, those branches move with it (git 2.38 or later).",
        kind: Kind::Bool,
        recommended: None,
        warning: None,
        default: "false",
        group: "Rebase and merge",
    },
    Common {
        key: "merge.conflictStyle",
        title: "Conflict markers",
        about: "How conflicts are written into files.",
        kind: Kind::Choice(&[
            ("merge", "ours and theirs"),
            ("diff3", "ours, the common ancestor, theirs"),
            (
                "zdiff3",
                "like diff3, without the lines both sides share (git 2.35+)",
            ),
        ]),
        recommended: Some((
            "zdiff3",
            "showing the common ancestor makes most conflicts obvious; zdiff3 keeps the markers small",
        )),
        warning: None,
        default: "merge",
        group: "Rebase and merge",
    },
    Common {
        key: "diff.algorithm",
        title: "Diff algorithm",
        about: "How git lines up old and new text in diffs (gitvis' own diffs follow it too when they come from git).",
        kind: Kind::Choice(&[
            ("myers", "the classic default"),
            ("minimal", "the smallest diff, slower"),
            ("patience", "matches unique lines first"),
            ("histogram", "patience, faster; usually the most readable"),
        ]),
        recommended: Some(("histogram", "moved and reordered code reads better")),
        warning: None,
        default: "myers",
        group: "Rebase and merge",
    },
    Common {
        key: "core.autocrlf",
        title: "Line endings",
        about: "Whether git converts line endings between the repository and your working tree.",
        kind: Kind::Choice(&[
            ("true", "LF in the repository, CRLF in your files"),
            (
                "input",
                "CRLF becomes LF on commit, files are left alone on checkout",
            ),
            ("false", "no conversion"),
        ]),
        recommended: None,
        warning: Some(
            "On Windows true (the Git for Windows installer's default) rewrites every text file on checkout: files then look changed when an editor or tool writes LF, and shell scripts with CRLF fail in containers and WSL. Many teams set false or input and commit a .gitattributes with `* text=auto eol=lf` instead, which wins over this setting for everyone.",
        ),
        default: "false (Git for Windows sets true in its system config)",
        group: "Files",
    },
    Common {
        key: "core.longpaths",
        title: "Long paths (Windows)",
        about: "Lets Git for Windows handle paths longer than 260 characters (deep node_modules, Java packages).",
        kind: Kind::Bool,
        recommended: Some((
            "true",
            "clones with deep trees no longer fail with \"Filename too long\"",
        )),
        warning: None,
        default: "false",
        group: "Files",
    },
    Common {
        key: "core.editor",
        title: "Editor",
        about: "The program git opens for commit messages and rebase todo lists when you run git in a terminal (gitvis never opens it).",
        kind: Kind::Text {
            hint: "code --wait",
            suggestions: &["code --wait", "notepad", "vim", "nano"],
        },
        recommended: None,
        warning: None,
        default: "vi (Git for Windows asks during setup)",
        group: "Files",
    },
    Common {
        key: "credential.helper",
        title: "Credential helper",
        about: "Where git keeps the passwords and tokens for HTTPS remotes. Several helpers can be listed; an empty value clears the ones before it.",
        kind: Kind::Text {
            hint: "manager",
            suggestions: &["manager", "store", "cache"],
        },
        recommended: Some((
            "manager",
            "Git Credential Manager keeps tokens in the Windows credential store",
        )),
        warning: Some(
            "store writes credentials in plain text to ~/.git-credentials. For github.com, `gh auth setup-git` makes gh answer instead (see GitHub account).",
        ),
        default: "none: git asks every time",
        group: "Signing and credentials",
    },
    Common {
        key: "commit.gpgSign",
        title: "Sign every commit",
        about: "Every commit is signed with user.signingKey in the format gpg.format.",
        kind: Kind::Bool,
        recommended: None,
        warning: None,
        default: "false",
        group: "Signing and credentials",
    },
    Common {
        key: "gpg.format",
        title: "Signature format",
        about: "What kind of key signs commits and tags.",
        kind: Kind::Choice(&[
            ("openpgp", "a GnuPG key"),
            ("ssh", "an SSH key (git 2.34+)"),
            ("x509", "an S/MIME certificate (gpgsm)"),
        ]),
        recommended: None,
        warning: None,
        default: "openpgp",
        group: "Signing and credentials",
    },
    Common {
        key: "user.signingKey",
        title: "Signing key",
        about: "A GPG key id, or for SSH the path to a key file (or the key itself as key::ssh-ed25519 …).",
        kind: Kind::Text {
            hint: "ABCDEF0123456789 or ~/.ssh/id_ed25519.pub",
            suggestions: &[],
        },
        recommended: None,
        warning: None,
        default: "GnuPG picks a key by your committer e-mail",
        group: "Signing and credentials",
    },
    Common {
        key: "tag.gpgSign",
        title: "Sign every tag",
        about: "Every annotated tag is signed (git tag -s).",
        kind: Kind::Bool,
        recommended: None,
        warning: None,
        default: "false",
        group: "Signing and credentials",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut v = Vec::new();
        for (scope, origin, kv) in [
            (
                "system",
                "file:C:/Program Files/Git/etc/gitconfig",
                "core.autocrlf\ntrue",
            ),
            (
                "system",
                "file:C:/Program Files/Git/etc/gitconfig",
                "pull.rebase\nfalse",
            ),
            (
                "global",
                "file:C:/Users/me/.gitconfig",
                "user.name\nAda Lovelace",
            ),
            (
                "global",
                "file:C:/Users/me/.gitconfig",
                "core.autocrlf\ninput",
            ),
            (
                "global",
                "file:C:/Users/me/.gitconfig",
                "credential.helper\nmanager",
            ),
            (
                "global",
                "file:C:/Users/me/.gitconfig",
                "includeif.gitdir/i:C:/work/.path\n~/.gitconfig-work",
            ),
            ("local", "file:.git/config", "core.bare\nfalse"),
            ("local", "file:.git/config", "core.autocrlf\nfalse"),
            ("local", "file:.git/config", "credential.helper\n"),
            (
                "local",
                "file:.git/config",
                "remote.origin.url\nhttps://github.com/o/r.git",
            ),
            ("command", "command line:", "color.ui\nalways"),
            ("local", "file:.git/config", "core.sparse"),
        ] {
            v.extend_from_slice(scope.as_bytes());
            v.push(0);
            v.extend_from_slice(origin.as_bytes());
            v.push(0);
            v.extend_from_slice(kv.as_bytes());
            v.push(0);
        }
        v
    }

    #[test]
    fn parses_the_listing() {
        let base = Path::new("C:/repo");
        let e = parse_list(&sample(), base);
        assert_eq!(e.len(), 12);
        assert_eq!(e[0].scope, Scope::System);
        assert_eq!(e[0].key, "core.autocrlf");
        assert_eq!(e[0].value.as_deref(), Some("true"));
        assert_eq!(
            e[0].file.as_deref(),
            Some(Path::new("C:/Program Files/Git/etc/gitconfig"))
        );
        // Relative origins are resolved against where git ran.
        assert_eq!(
            e[6].file.as_deref(),
            Some(base.join(".git/config").as_path())
        );
        assert_eq!(e[10].scope, Scope::Command);
        assert_eq!(e[10].origin, Origin::CommandLine);
        assert_eq!(e[10].file, None);
        // A bare boolean has no value.
        assert_eq!(e[11].value, None);
        assert_eq!(e[11].value_text(), "true");
        assert_eq!(e[5].subsection(), Some("gitdir/i:C:/work/"));
        assert_eq!(e[5].name(), "path");
        // An empty value is kept as empty.
        assert_eq!(e[8].value.as_deref(), Some(""));
        assert!(parse_list(b"", base).is_empty());
    }

    #[test]
    fn effective_values_and_overrides() {
        let e = parse_list(&sample(), Path::new("C:/repo"));
        let eff = effective(&e, "core.autoCRLF").unwrap();
        assert_eq!(eff.scope, Scope::Local);
        assert_eq!(eff.value_text(), "false");
        assert_eq!(values(&e, "core.autocrlf").len(), 3);
        // system and global core.autocrlf are overridden by the local one.
        assert_eq!(overridden_by(&e, 0).map(|o| o.scope), Some(Scope::Local));
        assert_eq!(overridden_by(&e, 3).map(|o| o.scope), Some(Scope::Local));
        assert!(overridden_by(&e, 7).is_none());
        // Lists are not overridden.
        assert!(is_multi("credential.helper"));
        assert!(is_multi("remote.upstream.fetch"));
        assert!(is_multi("includeIf.gitdir:~/x/.path"));
        assert!(!is_multi("remote.origin.prune"));
        assert!(overridden_by(&e, 4).is_none());
        let snap = Snapshot::from_entries(e, PathBuf::from("C:/repo"));
        assert_eq!(snap.files.len(), 3);
        assert_eq!(snap.files[1].entries, 4);
        assert_eq!(snap.count(Scope::Local), 5);
        // core.autocrlf (3 scopes) and credential.helper (2).
        assert_eq!(snap.multi_scope_keys(), 2);
    }

    #[test]
    fn keys_split_and_normalise() {
        assert_eq!(split_key("core.editor"), ("core", None, "editor"));
        assert_eq!(
            split_key("includeif.gitdir:C:/a.b/.path"),
            ("includeif", Some("gitdir:C:/a.b/"), "path")
        );
        assert_eq!(norm_key("commit.gpgSign"), "commit.gpgsign");
        assert_eq!(norm_key("Remote.Origin.URL"), "remote.Origin.url");
        assert!(validate_key("core.editor").is_ok());
        assert!(validate_key("url.https://x.example/.insteadOf").is_ok());
        assert!(validate_key("editor").is_err());
        assert!(validate_key("co re.editor").is_err());
        assert!(validate_key("core.1x").is_err());
        assert!(validate_value("a\nb").is_err());
    }

    #[test]
    fn edit_commands() {
        assert_eq!(
            set_args(Scope::Global, "pull.rebase", "true").unwrap(),
            ["config", "--global", "pull.rebase", "true"]
        );
        assert_eq!(
            set_args(Scope::Local, "credential.helper", "manager").unwrap(),
            ["config", "--local", "--add", "credential.helper", "manager"]
        );
        assert!(set_args(Scope::System, "a.b", "c").is_err());
        assert_eq!(
            unset_args(Scope::Global, "core.editor", Some("vim")).unwrap(),
            ["config", "--global", "--unset", "core.editor"]
        );
        assert_eq!(
            unset_args(Scope::Global, "credential.helper", Some("store")).unwrap(),
            [
                "config",
                "--global",
                "--fixed-value",
                "--unset",
                "credential.helper",
                "store"
            ]
        );
    }

    #[test]
    fn file_parser_finds_lines() {
        let text = "# comment\n[core]\n\tautocrlf = false\n\teditor = \"code --wait\" # x\n[remote \"origin\"]\n\turl = https://x\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n[alias]\n\tlg = log \\\n\t  --oneline\n\tst = status\n[branch.Main]\n\tremote = origin\n[includeIf \"gitdir/i:C:/w \\\"q\\\"/\"]\n\tpath = ~/.gitconfig-work\n[user] name = Ada\n; done\n";
        let keys = file_keys(text);
        assert_eq!(
            keys,
            vec![
                ("core.autocrlf".to_owned(), 3),
                ("core.editor".to_owned(), 4),
                ("remote.origin.url".to_owned(), 6),
                ("remote.origin.fetch".to_owned(), 7),
                ("alias.lg".to_owned(), 9),
                ("alias.st".to_owned(), 11),
                ("branch.main.remote".to_owned(), 13),
                ("includeif.gitdir/i:C:/w \"q\"/.path".to_owned(), 15),
                ("user.name".to_owned(), 16),
            ]
        );
    }

    #[test]
    fn lines_are_located_in_files() {
        let dir = std::env::temp_dir().join(format!("gitvis-id-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("gitconfig");
        std::fs::write(
            &f,
            "[credential]\n\thelper = \n\thelper = manager\n[core]\n\teditor = vim\n",
        )
        .unwrap();
        let origin = format!("file:{}", slash(&f));
        let mut bytes = Vec::new();
        for kv in [
            "credential.helper\n",
            "credential.helper\nmanager",
            "core.editor\nvim",
        ] {
            bytes.extend_from_slice(b"global\0");
            bytes.extend_from_slice(origin.as_bytes());
            bytes.push(0);
            bytes.extend_from_slice(kv.as_bytes());
            bytes.push(0);
        }
        let mut e = parse_list(&bytes, &dir);
        locate_lines(&mut e);
        assert_eq!(
            e.iter().map(|e| e.line).collect::<Vec<_>>(),
            vec![Some(2), Some(3), Some(5)]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn grouping_and_filtering() {
        let e = parse_list(&sample(), Path::new("C:/repo"));
        let g = groups(&e, "");
        let ids: Vec<&str> = g.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "color",
                "core",
                "credential",
                "includeif.gitdir/i:C:/work/",
                "pull",
                "remote.origin",
                "user"
            ]
        );
        assert_eq!(g[3].title, "includeif \"gitdir/i:C:/work/\"");
        let core = g.iter().find(|g| g.id == "core").unwrap();
        assert_eq!(core.items.len(), 5);
        let f = groups(&e, "github.com");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].id, "remote.origin");
        assert_eq!(groups(&e, "command line").len(), 1);
    }

    #[test]
    fn common_settings_are_well_formed() {
        for c in COMMON {
            assert!(validate_key(c.key).is_ok(), "{}", c.key);
            assert!(!c.about.is_empty() && !c.title.is_empty());
            if let (Kind::Choice(opts), Some((rec, _))) = (c.kind, c.recommended) {
                assert!(opts.iter().any(|(v, _)| *v == rec), "{}", c.key);
            }
        }
        assert_eq!(
            COMMON
                .iter()
                .filter(|c| c.key == "merge.conflictStyle")
                .map(|c| c.recommended.unwrap().0)
                .next(),
            Some("zdiff3")
        );
    }
}
