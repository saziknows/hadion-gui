//! Identities per folder: `includeIf` rules, the files they include, which
//! rule applies where, and the plans that add, edit and remove one.
//!
//! An identity is a small config file (`~/.gitconfig-work` with
//! `user.name`, `user.email`, maybe `user.signingkey`) and a rule in the
//! global config that includes it for repositories under a folder:
//!
//! ```text
//! [includeIf "gitdir/i:C:/work/"]
//!     path = ~/.gitconfig-work
//! ```
//!
//! git's matching rules ([`expand_gitdir`], [`gitdir_matches`]): `~/` is the
//! home folder, `./` the including file's folder, a pattern that is not
//! absolute gets `**/` in front, and a trailing `/` gets `**` after it, so
//! `C:/work/` matches every repository below `C:/work`. The pattern is
//! matched against the repository's `.git` folder with `*` not crossing `/`;
//! `gitdir/i:` ignores case, which on Windows avoids misses on `c:` vs `C:`.

use std::path::{Path, PathBuf};

use super::config::{self, Entry, parse_plain_list, split_key};
use super::{Scope, Step, config_quote, git, git_raw, same_path, slash, tilde};

/// The condition of an `includeIf` section (or an unconditional
/// `include.path`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Condition {
    GitDir {
        pattern: String,
        icase: bool,
    },
    OnBranch(String),
    /// `hasconfig:remote.*.url:PATTERN`.
    RemoteUrl(String),
    /// `[include] path = …`.
    Always,
    Other(String),
}

impl Condition {
    pub fn parse(sub: &str) -> Condition {
        if let Some(p) = sub.strip_prefix("gitdir/i:") {
            Condition::GitDir {
                pattern: p.to_owned(),
                icase: true,
            }
        } else if let Some(p) = sub.strip_prefix("gitdir:") {
            Condition::GitDir {
                pattern: p.to_owned(),
                icase: false,
            }
        } else if let Some(p) = sub.strip_prefix("onbranch:") {
            Condition::OnBranch(p.to_owned())
        } else if let Some(p) = sub.strip_prefix("hasconfig:remote.*.url:") {
            Condition::RemoteUrl(p.to_owned())
        } else {
            Condition::Other(sub.to_owned())
        }
    }

    /// The subsection as written (`gitdir/i:C:/work/`).
    pub fn raw(&self) -> String {
        match self {
            Condition::GitDir { pattern, icase } => {
                format!("{}:{pattern}", if *icase { "gitdir/i" } else { "gitdir" })
            }
            Condition::OnBranch(p) => format!("onbranch:{p}"),
            Condition::RemoteUrl(p) => format!("hasconfig:remote.*.url:{p}"),
            Condition::Always => String::new(),
            Condition::Other(s) => s.clone(),
        }
    }

    /// What kind of rule this is, for a badge.
    pub fn kind(&self) -> &'static str {
        match self {
            Condition::GitDir { icase: true, .. } => "Folder, any case",
            Condition::GitDir { icase: false, .. } => "Folder",
            Condition::OnBranch(_) => "Branch",
            Condition::RemoteUrl(_) => "Remote URL",
            Condition::Always => "Always",
            Condition::Other(_) => "Other",
        }
    }

    /// The pattern alone (`C:/work/`).
    pub fn pattern(&self) -> &str {
        match self {
            Condition::GitDir { pattern, .. }
            | Condition::OnBranch(pattern)
            | Condition::RemoteUrl(pattern)
            | Condition::Other(pattern) => pattern,
            Condition::Always => "",
        }
    }

    /// One sentence on when the rule applies.
    pub fn explain(&self) -> String {
        match self {
            Condition::GitDir { pattern, icase } => format!(
                "Repositories whose .git folder matches {pattern}{}",
                if *icase { ", ignoring case" } else { "" }
            ),
            Condition::OnBranch(p) => {
                format!("Repositories with a branch matching {p} checked out")
            }
            Condition::RemoteUrl(p) => format!("Repositories with a remote whose URL matches {p}"),
            Condition::Always => "Everywhere: an unconditional include".to_owned(),
            Condition::Other(s) => format!("Condition {s} (not known to gitvis)"),
        }
    }
}

/// The identity values of an included file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileIdentity {
    pub name: Option<String>,
    pub email: Option<String>,
    pub signing_key: Option<String>,
    pub format: Option<String>,
    pub gpgsign: Option<String>,
    /// Every key the file sets.
    pub keys: usize,
}

/// One `includeIf` (or `include`) rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub scope: Scope,
    /// The file the rule is written in.
    pub config_file: Option<PathBuf>,
    pub line: Option<u32>,
    pub condition: Condition,
    /// The listed key (`includeif.gitdir/i:C:/work/.path`).
    pub key: String,
    /// The path as written.
    pub path: String,
    /// The path resolved like git does.
    pub target: PathBuf,
    pub exists: bool,
    pub identity: FileIdentity,
    /// Entries of the same section in the same file (1 when the section
    /// holds only this path, so the whole section can be removed).
    pub siblings: usize,
}

impl Rule {
    /// Problems with the rule that make it miss or surprise.
    pub fn warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        if let Condition::GitDir { pattern, icase } = &self.condition {
            if pattern.contains('\\') {
                w.push("The pattern has backslashes: git compares it with forward-slash paths, so it never matches. Write C:/work/ instead of C:\\work\\.".to_owned());
            }
            if !pattern.ends_with('/') && !pattern.ends_with("**") {
                w.push("No trailing slash: the pattern matches only a .git folder at exactly that path. With a / at the end it matches every repository below the folder.".to_owned());
            }
            if cfg!(windows) && !icase {
                w.push("On Windows, gitdir/i: is safer: the same folder can be spelled C:/Work or c:/work, and gitdir: is case-sensitive.".to_owned());
            }
        }
        if !self.exists {
            w.push(format!(
                "{} does not exist: git skips a missing include without a word.",
                slash(&self.target)
            ));
        }
        w
    }
}

/// Resolve an include path like git: `~/` from `home`, relative paths from
/// the including file's folder.
pub fn resolve_include(path: &str, config_file: Option<&Path>, home: Option<&Path>) -> PathBuf {
    let p = path.trim();
    if p.starts_with("~/") || p == "~" {
        return super::expand_tilde(p, home);
    }
    let pb = PathBuf::from(super::from_msys(p));
    if pb.is_absolute() || p.starts_with('/') {
        return pb;
    }
    match config_file.and_then(Path::parent) {
        Some(dir) => dir.join(pb),
        None => pb,
    }
}

/// Every include rule in the listing, in reading order, with targets
/// resolved (but not read: see [`read_targets`]).
pub fn rules(entries: &[Entry], home: Option<&Path>) -> Vec<Rule> {
    let mut out = Vec::new();
    for e in entries {
        let (section, sub, name) = split_key(&e.key);
        if name != "path" {
            continue;
        }
        let condition = match (section, sub) {
            ("includeif", Some(sub)) => Condition::parse(sub),
            ("include", None) => Condition::Always,
            _ => continue,
        };
        let Some(path) = e.value.clone() else {
            continue;
        };
        let target = resolve_include(&path, e.file.as_deref(), home);
        let prefix = match sub {
            Some(sub) => format!("{section}.{sub}."),
            None => format!("{section}."),
        };
        let siblings = entries
            .iter()
            .filter(|o| {
                o.key.starts_with(&prefix)
                    && !o.key[prefix.len()..].contains('.')
                    && match (&o.file, &e.file) {
                        (Some(a), Some(b)) => same_path(a, b),
                        _ => false,
                    }
            })
            .count();
        out.push(Rule {
            scope: e.scope,
            config_file: e.file.clone(),
            line: e.line,
            condition,
            key: e.key.clone(),
            exists: target.is_file(),
            target,
            path,
            identity: FileIdentity::default(),
            siblings,
        });
    }
    out
}

/// Identity values from `git config --file F --list -z` output.
pub fn identity_of(listing: &[(String, Option<String>)]) -> FileIdentity {
    let get = |k: &str| {
        listing
            .iter()
            .rev()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone().unwrap_or_else(|| "true".to_owned()))
    };
    FileIdentity {
        name: get("user.name"),
        email: get("user.email"),
        signing_key: get("user.signingkey"),
        format: get("gpg.format"),
        gpgsign: get("commit.gpgsign"),
        keys: listing.len(),
    }
}

/// Read each existing target's identity values.
pub fn read_targets(rules: &mut [Rule], dir: &Path, cmds: &mut Vec<String>) {
    for r in rules.iter_mut() {
        if !r.exists {
            continue;
        }
        let f = slash(&r.target);
        if let Ok(out) = git(dir, &["config", "--file", f.as_str(), "--list", "-z"], cmds) {
            r.identity = identity_of(&parse_plain_list(&out));
        }
    }
}

/// The pattern git matches, expanded (`~/`, `./`, `**/` in front of
/// relative patterns, `**` after a trailing slash).
pub fn expand_gitdir(pattern: &str, config_file: Option<&Path>, home: Option<&Path>) -> String {
    let mut p = if let Some(rest) = pattern.strip_prefix("~/") {
        match home {
            Some(h) => format!("{}/{rest}", slash(h).trim_end_matches('/')),
            None => pattern.to_owned(),
        }
    } else if let Some(rest) = pattern.strip_prefix("./") {
        match config_file.and_then(Path::parent) {
            Some(d) => format!("{}/{rest}", slash(d).trim_end_matches('/')),
            None => pattern.to_owned(),
        }
    } else {
        pattern.to_owned()
    };
    let absolute = p.starts_with('/') || Path::new(&p).is_absolute() || is_drive(&p);
    if !absolute && !p.starts_with("**/") {
        p.insert_str(0, "**/");
    }
    if p.ends_with('/') {
        p.push_str("**");
    }
    p
}

fn is_drive(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/'
}

fn class_match(p: &[char], c: char, icase: bool) -> Option<(bool, usize)> {
    // p starts after '['; returns (matched, length consumed including ']').
    let mut i = 0;
    let negate = matches!(p.first(), Some('!') | Some('^'));
    if negate {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    let norm = |x: char| if icase { x.to_ascii_lowercase() } else { x };
    while i < p.len() {
        let ch = p[i];
        if ch == ']' && !first {
            return Some((matched != negate, i + 1));
        }
        first = false;
        if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
            let (lo, hi) = (norm(ch), norm(p[i + 2]));
            if lo <= norm(c) && norm(c) <= hi {
                matched = true;
            }
            i += 3;
        } else {
            if norm(ch) == norm(c) {
                matched = true;
            }
            i += 1;
        }
    }
    None
}

/// git's `wildmatch` with `WM_PATHNAME`: `*` and `?` do not match `/`,
/// `**/` matches zero or more folders, a trailing `/**` everything below.
pub fn wildmatch(pattern: &str, text: &str, icase: bool) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    wm(&p, &t, icase)
}

fn eq(a: char, b: char, icase: bool) -> bool {
    if icase {
        a.to_lowercase().eq(b.to_lowercase())
    } else {
        a == b
    }
}

fn wm(p: &[char], t: &[char], icase: bool) -> bool {
    let Some(&c) = p.first() else {
        return t.is_empty();
    };
    match c {
        '*' if p.get(1) == Some(&'*') => {
            let rest = &p[2..];
            if rest.is_empty() {
                return true;
            }
            if rest[0] == '/' {
                let after = &rest[1..];
                if wm(after, t, icase) {
                    return true;
                }
                return (0..t.len()).any(|i| t[i] == '/' && wm(after, &t[i + 1..], icase));
            }
            (0..=t.len()).any(|i| wm(rest, &t[i..], icase))
        }
        '*' => {
            let rest = &p[1..];
            for i in 0..=t.len() {
                if wm(rest, &t[i..], icase) {
                    return true;
                }
                if i < t.len() && t[i] == '/' {
                    break;
                }
            }
            false
        }
        '?' => !t.is_empty() && t[0] != '/' && wm(&p[1..], &t[1..], icase),
        '[' => match (
            t.first(),
            class_match(&p[1..], *t.first().unwrap_or(&'\0'), icase),
        ) {
            (Some(&tc), Some((true, n))) if tc != '/' => wm(&p[1 + n..], &t[1..], icase),
            (Some(_), Some(_)) => false,
            // An unclosed bracket is a literal.
            (Some(&tc), None) => eq('[', tc, icase) && wm(&p[1..], &t[1..], icase),
            (None, _) => false,
        },
        '\\' if p.len() > 1 => {
            !t.is_empty() && eq(p[1], t[0], icase) && wm(&p[2..], &t[1..], icase)
        }
        _ => !t.is_empty() && eq(c, t[0], icase) && wm(&p[1..], &t[1..], icase),
    }
}

/// Does a `gitdir` rule match a repository whose `.git` folder is
/// `gitdir`?
pub fn gitdir_matches(rule: &Rule, gitdir: &Path, home: Option<&Path>) -> bool {
    let Condition::GitDir { pattern, icase } = &rule.condition else {
        return false;
    };
    let pat = expand_gitdir(pattern, rule.config_file.as_deref(), home);
    let text = slash(gitdir);
    wildmatch(&pat, text.trim_end_matches('/'), *icase)
}

// ---------------------------------------------------------------------------
// Which identity applies
// ---------------------------------------------------------------------------

/// The identity git uses in one folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub path: PathBuf,
    /// A repository (git ran there); otherwise the rules were evaluated by
    /// gitvis for a repository that would be created at `path`.
    pub repo: bool,
    pub gitdir: Option<PathBuf>,
    pub name: Option<Entry>,
    pub email: Option<Entry>,
    pub signing_key: Option<Entry>,
    pub gpgsign: Option<Entry>,
    /// Indexes of the rules that apply (their file was included, or their
    /// pattern matches).
    pub matched: Vec<usize>,
    pub error: Option<String>,
}

/// What identity git uses at `path`, given the rules of the global config
/// and the listing of the global and system scopes (`base`, used for a
/// folder that is not a repository yet).
pub fn applied(
    path: &Path,
    rules: &[Rule],
    base: &[Entry],
    home: Option<&Path>,
    cmds: &mut Vec<String>,
) -> Applied {
    let mut a = Applied {
        path: path.to_path_buf(),
        ..Default::default()
    };
    if path.is_file() {
        a.error = Some(format!("{} is a file, not a folder", slash(path)));
        return a;
    }
    let gitdir = git_raw(path, &["rev-parse", "--absolute-git-dir"], cmds)
        .ok()
        .filter(|o| o.success)
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()));
    match gitdir {
        Some(gd) => {
            a.repo = true;
            a.gitdir = Some(gd);
            match git(path, &config::LIST_ARGS, cmds) {
                Ok(out) => {
                    let mut entries = config::parse_list(&out, path);
                    config::locate_lines(&mut entries);
                    let pick = |k: &str| config::effective(&entries, k).cloned();
                    a.name = pick("user.name");
                    a.email = pick("user.email");
                    a.signing_key = pick("user.signingkey");
                    a.gpgsign = pick("commit.gpgsign");
                    a.matched = rules
                        .iter()
                        .enumerate()
                        .filter(|(_, r)| {
                            entries
                                .iter()
                                .any(|e| e.file.as_deref().is_some_and(|f| same_path(f, &r.target)))
                        })
                        .map(|(i, _)| i)
                        .collect();
                }
                Err(e) => a.error = Some(e),
            }
        }
        None => {
            // Not a repository: evaluate folder rules for <path>/.git.
            let gd = path.join(".git");
            a.matched = rules
                .iter()
                .enumerate()
                .filter(|(_, r)| gitdir_matches(r, &gd, home))
                .map(|(i, _)| i)
                .collect();
            a.gitdir = Some(gd);
            let base_pick = |k: &str| {
                config::effective(
                    &base
                        .iter()
                        .filter(|e| matches!(e.scope, Scope::System | Scope::Global))
                        .cloned()
                        .collect::<Vec<_>>(),
                    k,
                )
                .cloned()
            };
            a.name = base_pick("user.name");
            a.email = base_pick("user.email");
            a.signing_key = base_pick("user.signingkey");
            a.gpgsign = base_pick("commit.gpgsign");
            // The last matching rule's file wins for the keys it sets.
            for &i in &a.matched {
                let r = &rules[i];
                let synth = |k: &str, v: &Option<String>| {
                    v.as_ref().map(|v| Entry {
                        scope: r.scope,
                        origin: config::Origin::File(slash(&r.target)),
                        file: Some(r.target.clone()),
                        key: k.to_owned(),
                        value: Some(v.clone()),
                        line: None,
                    })
                };
                if let Some(e) = synth("user.name", &r.identity.name) {
                    a.name = Some(e);
                }
                if let Some(e) = synth("user.email", &r.identity.email) {
                    a.email = Some(e);
                }
                if let Some(e) = synth("user.signingkey", &r.identity.signing_key) {
                    a.signing_key = Some(e);
                }
            }
        }
    }
    a
}

// ---------------------------------------------------------------------------
// Plans
// ---------------------------------------------------------------------------

/// The `gitdir` pattern for a folder: forward slashes, a trailing slash,
/// `~/` kept. Errors for an empty or relative folder.
pub fn folder_pattern(folder: &str) -> Result<String, String> {
    let f = folder.trim().trim_matches('"');
    if f.is_empty() {
        return Err("Choose a folder first".to_owned());
    }
    let mut p = super::from_msys(&f.replace('\\', "/"));
    if !(p.starts_with("~/") || p.starts_with('/') || is_drive(&format!("{p}/"))) {
        return Err(format!(
            "{f} is not an absolute folder: use a full path such as C:/work or ~/work"
        ));
    }
    if !p.ends_with('/') {
        p.push('/');
    }
    Ok(p)
}

/// `work` for `C:/Users/me/work/`: the slug of the folder's last part.
pub fn slug_for(folder: &str) -> String {
    let last = folder
        .replace('\\', "/")
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_owned();
    let s = crate::devops::slug(&last, 40);
    if s.is_empty() {
        "identity".to_owned()
    } else {
        s
    }
}

/// `<dir>/.gitconfig-<slug>`.
pub fn identity_file(dir: &Path, slug: &str) -> PathBuf {
    dir.join(format!(".gitconfig-{slug}"))
}

/// What the form holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewIdentity {
    pub folder: String,
    /// `gitdir/i:` instead of `gitdir:`.
    pub icase: bool,
    pub name: String,
    pub email: String,
    /// Optional.
    pub signing_key: String,
    /// The identity file to write.
    pub file: PathBuf,
}

/// The commands that add or change an identity and what the files will
/// hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityPlan {
    pub condition: Condition,
    /// `includeIf.gitdir/i:C:/work/.path`.
    pub include_key: String,
    /// `~/.gitconfig-work`.
    pub include_value: String,
    pub file: PathBuf,
    pub file_exists: bool,
    pub global_file: PathBuf,
    pub steps: Vec<Step>,
    /// The `[user]` section of the identity file after the change.
    pub file_text: String,
    /// The section added to the global config (empty when it stays).
    pub global_text: String,
}

fn user_section(name: &str, email: &str, key: &str) -> String {
    let mut s = format!(
        "[user]\n\tname = {}\n\temail = {}\n",
        config_quote(name),
        config_quote(email)
    );
    if !key.trim().is_empty() {
        s.push_str(&format!("\tsigningkey = {}\n", config_quote(key.trim())));
    }
    s
}

fn include_section(cond: &str, value: &str) -> String {
    format!(
        "[includeIf \"{}\"]\n\tpath = {}\n",
        cond.replace('\\', "\\\\").replace('"', "\\\""),
        config_quote(value)
    )
}

fn validate(new: &NewIdentity) -> Result<String, String> {
    let pattern = folder_pattern(&new.folder)?;
    if new.name.trim().is_empty() {
        return Err("Enter the name for commits in this folder".to_owned());
    }
    let email = new.email.trim();
    if email.is_empty() || !email.contains('@') || email.contains(char::is_whitespace) {
        return Err("Enter a valid e-mail address".to_owned());
    }
    for v in [&new.name, &new.email, &new.signing_key] {
        config::validate_value(v)?;
    }
    Ok(pattern)
}

fn file_arg(file: &Path) -> String {
    slash(file)
}

fn user_steps(file: &Path, name: &str, email: &str, key: &str, had_key: bool) -> Vec<Step> {
    let f = file_arg(file);
    let set = |k: &str, v: &str| {
        Step::Git(vec![
            "config".into(),
            "--file".into(),
            f.clone(),
            k.into(),
            v.into(),
        ])
    };
    let mut steps = vec![
        set("user.name", name.trim()),
        set("user.email", email.trim()),
    ];
    if !key.trim().is_empty() {
        steps.push(set("user.signingkey", key.trim()));
    } else if had_key {
        steps.push(Step::Git(vec![
            "config".into(),
            "--file".into(),
            f,
            "--unset".into(),
            "user.signingkey".into(),
        ]));
    }
    steps
}

fn add_include_step(include_key: &str, value: &str) -> Step {
    Step::Git(vec![
        "config".into(),
        "--global".into(),
        "--add".into(),
        include_key.into(),
        value.into(),
    ])
}

/// The flag (or `--file F`) that addresses the file a rule is written in.
fn rule_file_args(rule: &Rule, global_file: &Path) -> Vec<String> {
    match &rule.config_file {
        Some(f) if rule.scope == Scope::Global && same_path(f, global_file) => {
            vec!["--global".into()]
        }
        Some(f) => vec!["--file".into(), slash(f)],
        None => vec!["--global".into()],
    }
}

fn remove_rule_step(rule: &Rule, global_file: &Path) -> Step {
    let mut a = vec!["config".to_owned()];
    a.extend(rule_file_args(rule, global_file));
    if rule.siblings <= 1 && rule.condition != Condition::Always {
        a.push("--remove-section".into());
        a.push(format!("includeIf.{}", rule.condition.raw()));
    } else {
        a.push("--fixed-value".into());
        a.push("--unset".into());
        a.push(rule.key.clone());
        a.push(rule.path.clone());
    }
    Step::Git(a)
}

/// Add an identity: write the identity file and append the rule to the
/// global config. `rules` are the existing ones (a rule for the same
/// folder is refused).
pub fn plan_add(
    new: &NewIdentity,
    rules: &[Rule],
    global_file: &Path,
    home: Option<&Path>,
) -> Result<IdentityPlan, String> {
    let pattern = validate(new)?;
    let condition = Condition::GitDir {
        pattern: pattern.clone(),
        icase: new.icase,
    };
    let clash = rules.iter().any(|r| match &r.condition {
        Condition::GitDir { pattern: p, .. } => p
            .trim_end_matches('/')
            .eq_ignore_ascii_case(pattern.trim_end_matches('/')),
        _ => false,
    });
    if clash {
        return Err(format!(
            "A rule for {pattern} exists already: edit that identity instead"
        ));
    }
    if new.file.as_os_str().is_empty() {
        return Err("Choose where the identity file goes".to_owned());
    }
    let include_key = format!("includeIf.{}.path", condition.raw());
    let include_value = tilde(&new.file, home);
    let mut steps = user_steps(&new.file, &new.name, &new.email, &new.signing_key, false);
    steps.push(add_include_step(&include_key, &include_value));
    Ok(IdentityPlan {
        global_text: include_section(&condition.raw(), &include_value),
        file_text: user_section(&new.name, &new.email, &new.signing_key),
        condition,
        include_key,
        include_value,
        file_exists: new.file.exists(),
        file: new.file.clone(),
        global_file: global_file.to_path_buf(),
        steps,
    })
}

/// Change an identity: the values in its file and, when the folder or the
/// case rule changed, the rule itself.
pub fn plan_edit(
    rule: &Rule,
    new: &NewIdentity,
    global_file: &Path,
) -> Result<IdentityPlan, String> {
    let pattern = validate(new)?;
    let condition = Condition::GitDir {
        pattern,
        icase: new.icase,
    };
    let file = rule.target.clone();
    let mut steps = user_steps(
        &file,
        &new.name,
        &new.email,
        &new.signing_key,
        rule.identity.signing_key.is_some(),
    );
    let include_key = format!("includeIf.{}.path", condition.raw());
    let mut global_text = String::new();
    if condition != rule.condition {
        steps.push(remove_rule_step(rule, global_file));
        steps.push(add_include_step(&include_key, &rule.path));
        global_text = include_section(&condition.raw(), &rule.path);
    }
    Ok(IdentityPlan {
        file_text: user_section(&new.name, &new.email, &new.signing_key),
        global_text,
        condition,
        include_key,
        include_value: rule.path.clone(),
        file_exists: rule.exists,
        file,
        global_file: global_file.to_path_buf(),
        steps,
    })
}

/// Remove a rule (the whole section when it holds only this path), and
/// the identity file when asked.
pub fn plan_remove(rule: &Rule, delete_file: bool, global_file: &Path) -> Vec<Step> {
    let mut steps = vec![remove_rule_step(rule, global_file)];
    if delete_file && rule.exists {
        steps.push(Step::RemoveFile(rule.target.clone()));
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::identity::config::Origin;

    /// An absolute path on the platform running the test: `C:/x` on
    /// Windows, `/x` elsewhere (`C:/x` is relative on Linux).
    fn abs(p: &str) -> String {
        if cfg!(windows) {
            format!("C:{p}")
        } else {
            p.to_owned()
        }
    }

    fn entry(scope: Scope, file: &str, key: &str, value: &str) -> Entry {
        Entry {
            scope,
            origin: Origin::File(file.to_owned()),
            file: Some(PathBuf::from(file)),
            key: key.to_owned(),
            value: Some(value.to_owned()),
            line: Some(3),
        }
    }

    #[test]
    fn conditions_parse_and_round_trip() {
        for raw in [
            "gitdir:~/work/",
            "gitdir/i:C:/Work/",
            "onbranch:release/**",
            "hasconfig:remote.*.url:https://github.com/acme/**",
            "weird:thing",
        ] {
            assert_eq!(Condition::parse(raw).raw(), raw);
        }
        assert_eq!(
            Condition::parse("gitdir/i:C:/w/"),
            Condition::GitDir {
                pattern: "C:/w/".into(),
                icase: true
            }
        );
        assert_eq!(Condition::parse("onbranch:x").kind(), "Branch");
    }

    #[test]
    fn rules_from_the_listing() {
        let home = Path::new("C:/Users/me");
        let g = "C:/Users/me/.gitconfig";
        let entries = vec![
            entry(Scope::Global, g, "user.name", "Me"),
            entry(
                Scope::Global,
                g,
                "includeif.gitdir/i:C:/work/.path",
                "~/.gitconfig-work",
            ),
            entry(
                Scope::Global,
                g,
                "includeif.onbranch:oss/**.path",
                "oss.inc",
            ),
            entry(Scope::Global, g, "include.path", &abs("/shared/common.inc")),
        ];
        let r = rules(&entries, Some(home));
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].target, home.join(".gitconfig-work"));
        assert_eq!(r[0].siblings, 1);
        // Relative to the including file.
        assert_eq!(r[1].target, Path::new("C:/Users/me").join("oss.inc"));
        assert_eq!(r[2].condition, Condition::Always);
        assert_eq!(r[2].target, PathBuf::from(abs("/shared/common.inc")));
        assert!(!r[0].exists);
        assert!(r[0].warnings().iter().any(|w| w.contains("does not exist")));
    }

    #[test]
    fn identity_values_from_a_file_listing() {
        let l = parse_plain_list(
            b"user.name\nWork Me\0user.email\nme@corp.example\0user.signingkey\nABC\0commit.gpgsign\0",
        );
        let id = identity_of(&l);
        assert_eq!(id.name.as_deref(), Some("Work Me"));
        assert_eq!(id.email.as_deref(), Some("me@corp.example"));
        assert_eq!(id.signing_key.as_deref(), Some("ABC"));
        assert_eq!(id.gpgsign.as_deref(), Some("true"));
        assert_eq!(id.keys, 4);
    }

    #[test]
    fn gitdir_patterns_expand_and_match() {
        let home = Path::new("C:/Users/me");
        assert_eq!(
            expand_gitdir("~/work/", None, Some(home)),
            "C:/Users/me/work/**"
        );
        assert_eq!(expand_gitdir("work/", None, None), "**/work/**");
        assert_eq!(
            expand_gitdir("./sub/", Some(Path::new("C:/cfg/.gitconfig")), None),
            "C:/cfg/sub/**"
        );
        assert_eq!(expand_gitdir("C:/work/", None, None), "C:/work/**");
        assert_eq!(expand_gitdir("/srv/x/.git", None, None), "/srv/x/.git");

        assert!(wildmatch("C:/work/**", "C:/work/app/.git", false));
        assert!(wildmatch("C:/work/**", "C:/work/a/b/c/.git", false));
        assert!(!wildmatch("C:/work/**", "C:/workshop/a/.git", false));
        assert!(!wildmatch("C:/work/**", "c:/work/app/.git", false));
        assert!(wildmatch("C:/work/**", "c:/Work/app/.git", true));
        assert!(wildmatch("**/work/**", "D:/x/work/app/.git", false));
        assert!(wildmatch("**/work/**", "work/app/.git", false));
        assert!(!wildmatch("C:/work/*/.git", "C:/work/a/b/.git", false));
        assert!(wildmatch("C:/work/*/.git", "C:/work/a/.git", false));
        assert!(wildmatch("C:/w?rk/[a-c]pp/.git", "C:/work/app/.git", false));
        assert!(!wildmatch("C:/w?rk/[!a]pp/.git", "C:/work/app/.git", false));
        // No trailing slash: only the exact folder.
        assert!(!wildmatch("C:/work", "C:/work/app/.git", false));

        let rule = |pattern: &str, icase| Rule {
            scope: Scope::Global,
            config_file: None,
            line: None,
            condition: Condition::GitDir {
                pattern: pattern.into(),
                icase,
            },
            key: String::new(),
            path: String::new(),
            target: PathBuf::new(),
            exists: true,
            identity: FileIdentity::default(),
            siblings: 1,
        };
        assert!(gitdir_matches(
            &rule("~/work/", false),
            Path::new("C:\\Users\\me\\work\\api\\.git"),
            Some(home)
        ));
        assert!(!gitdir_matches(
            &rule("~/work/", false),
            Path::new("C:/Users/me/play/.git"),
            Some(home)
        ));
        let w = rule("C:\\work", false).warnings();
        assert!(w.iter().any(|w| w.contains("backslashes")));
        assert!(w.iter().any(|w| w.contains("No trailing slash")));
    }

    #[test]
    fn folders_become_patterns() {
        assert_eq!(
            folder_pattern("C:\\Users\\me\\work").unwrap(),
            "C:/Users/me/work/"
        );
        assert_eq!(folder_pattern("~/oss/").unwrap(), "~/oss/");
        assert!(folder_pattern("").is_err());
        assert!(folder_pattern("work").is_err());
        assert_eq!(slug_for("C:/Users/me/Acme Corp/"), "acme-corp");
        assert_eq!(slug_for("/"), "identity");
        assert_eq!(
            identity_file(Path::new("C:/Users/me"), "work"),
            Path::new("C:/Users/me").join(".gitconfig-work")
        );
    }

    #[test]
    fn add_plan() {
        let home = Path::new("C:/Users/me");
        let new = NewIdentity {
            folder: "C:\\work".into(),
            icase: true,
            name: "Ada Work".into(),
            email: "ada@corp.example".into(),
            signing_key: String::new(),
            file: home.join(".gitconfig-work"),
        };
        let g = home.join(".gitconfig");
        let p = plan_add(&new, &[], &g, Some(home)).unwrap();
        assert_eq!(p.include_key, "includeIf.gitdir/i:C:/work/.path");
        assert_eq!(p.include_value, "~/.gitconfig-work");
        let lines: Vec<String> = p.steps.iter().map(Step::display).collect();
        let f = slash(&home.join(".gitconfig-work"));
        assert_eq!(
            lines,
            vec![
                format!("git config --file {f} user.name \"Ada Work\""),
                format!("git config --file {f} user.email ada@corp.example"),
                "git config --global --add includeIf.gitdir/i:C:/work/.path ~/.gitconfig-work"
                    .to_owned(),
            ]
        );
        assert_eq!(
            p.global_text,
            "[includeIf \"gitdir/i:C:/work/\"]\n\tpath = ~/.gitconfig-work\n"
        );
        assert_eq!(
            p.file_text,
            "[user]\n\tname = Ada Work\n\temail = ada@corp.example\n"
        );
        // Validation.
        let mut bad = new.clone();
        bad.email = "nope".into();
        assert!(plan_add(&bad, &[], &g, Some(home)).is_err());
        // A second rule for the same folder is refused.
        let existing = rules(
            &[entry(
                Scope::Global,
                "C:/Users/me/.gitconfig",
                "includeif.gitdir:C:/work/.path",
                "x",
            )],
            Some(home),
        );
        assert!(plan_add(&new, &existing, &g, Some(home)).is_err());
    }

    #[test]
    fn edit_and_remove_plans() {
        let home = Path::new("C:/Users/me");
        let g = home.join(".gitconfig");
        let gs = slash(&g);
        let mut r = rules(
            &[entry(
                Scope::Global,
                &gs,
                "includeif.gitdir:C:/work/.path",
                "~/.gitconfig-work",
            )],
            Some(home),
        );
        r[0].identity.signing_key = Some("OLD".into());
        let new = NewIdentity {
            folder: "C:/work/".into(),
            icase: true,
            name: "Ada".into(),
            email: "ada@corp.example".into(),
            signing_key: String::new(),
            file: PathBuf::new(),
        };
        let p = plan_edit(&r[0], &new, &g).unwrap();
        let lines: Vec<String> = p.steps.iter().map(Step::display).collect();
        assert!(lines[2].ends_with("--unset user.signingkey"), "{lines:?}");
        assert_eq!(
            lines[3],
            "git config --global --remove-section includeIf.gitdir:C:/work/"
        );
        assert_eq!(
            lines[4],
            "git config --global --add includeIf.gitdir/i:C:/work/.path ~/.gitconfig-work"
        );
        let rm = plan_remove(&r[0], true, &g);
        assert_eq!(rm.len(), 1, "the file does not exist, nothing to delete");
        // A section with other keys only loses the path.
        r[0].siblings = 2;
        assert_eq!(
            plan_remove(&r[0], false, &g)[0].display(),
            "git config --global --fixed-value --unset includeif.gitdir:C:/work/.path ~/.gitconfig-work"
        );
        // A rule in another file is addressed with --file.
        r[0].config_file = Some(PathBuf::from("C:/other/inc"));
        r[0].siblings = 1;
        assert!(
            plan_remove(&r[0], false, &g)[0]
                .display()
                .starts_with("git config --file C:/other/inc --remove-section")
        );
    }
}
