//! Submodules: `git submodule status --recursive` joined with
//! `.gitmodules` (`git config -f .gitmodules --list -z`), the commit the
//! superproject records (`git ls-files -s`), and per checked-out submodule
//! its own status, HEAD and ahead / behind its tracked branch. Plus the
//! command lines of init, update, sync, add and remove.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{git_line, git_ok, git_probe, git_raw, resolve_in, unquote};

/// The state letter `git submodule status` prints first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SubState {
    /// ` `: checked out at the recorded commit.
    Current,
    /// `-`: not initialised (no checkout).
    NotInitialised,
    /// `+`: the checked-out commit differs from the recorded one.
    Modified,
    /// `U`: merge conflicts on the gitlink.
    Conflict,
}

impl SubState {
    pub fn from_char(c: char) -> Option<SubState> {
        Some(match c {
            ' ' => SubState::Current,
            '-' => SubState::NotInitialised,
            '+' => SubState::Modified,
            'U' => SubState::Conflict,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            SubState::Current => "Up to date",
            SubState::NotInitialised => "Not initialised",
            SubState::Modified => "Different commit",
            SubState::Conflict => "Conflict",
        }
    }

    pub fn explain(self) -> &'static str {
        match self {
            SubState::Current => "Checked out at the commit this repository records.",
            SubState::NotInitialised => {
                "Listed in .gitmodules but not checked out: Init, then Update clones it."
            }
            SubState::Modified => {
                "Checked out at another commit than the one recorded: commit the new pointer here, or Update to go back to the recorded commit."
            }
            SubState::Conflict => {
                "A merge left two versions of the submodule pointer: pick one and stage it."
            }
        }
    }
}

/// One line of `git submodule status --recursive`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusLine {
    pub state: SubState,
    /// The checked-out commit (the recorded one when not initialised).
    pub sha: String,
    /// Relative to the top of this repository.
    pub path: String,
    /// `git describe` of the checkout (`heads/main`, `v1.2-3-gabc`).
    pub describe: Option<String>,
}

/// Parse `git submodule status --recursive`.
pub fn parse_status(text: &str) -> Vec<StatusLine> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut chars = line.chars();
        let Some(state) = chars.next().and_then(SubState::from_char) else {
            continue;
        };
        let rest = chars.as_str();
        let Some((sha, tail)) = rest.split_once(' ') else {
            continue;
        };
        if sha.len() < 7 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        let (path, describe) = match tail.rfind(" (") {
            Some(i) if tail.ends_with(')') => (
                tail[..i].to_owned(),
                Some(tail[i + 2..tail.len() - 1].to_owned()),
            ),
            _ => (tail.to_owned(), None),
        };
        out.push(StatusLine {
            state,
            sha: sha.to_owned(),
            path: unquote(&path),
            describe,
        });
    }
    out
}

/// One `[submodule "name"]` section of `.gitmodules`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModuleConfig {
    pub name: String,
    pub path: String,
    pub url: String,
    /// `branch = main` (`.` means the superproject's current branch).
    pub branch: Option<String>,
    /// `update = rebase | merge | checkout | none`.
    pub update: Option<String>,
    /// Other keys (`shallow`, `ignore`, `fetchRecurseSubmodules`).
    pub other: Vec<(String, String)>,
}

/// Split `submodule.<name>.<var>` (names may contain dots).
fn split_key(key: &str) -> Option<(&str, &str)> {
    let rest = key.strip_prefix("submodule.")?;
    let dot = rest.rfind('.')?;
    Some((&rest[..dot], &rest[dot + 1..]))
}

fn collect(pairs: impl Iterator<Item = (String, String)>) -> Vec<ModuleConfig> {
    let mut order: Vec<String> = Vec::new();
    let mut map: HashMap<String, ModuleConfig> = HashMap::new();
    for (key, value) in pairs {
        let Some((name, var)) = split_key(&key) else {
            continue;
        };
        let m = map.entry(name.to_owned()).or_insert_with(|| {
            order.push(name.to_owned());
            ModuleConfig {
                name: name.to_owned(),
                ..Default::default()
            }
        });
        match var.to_ascii_lowercase().as_str() {
            "path" => m.path = value,
            "url" => m.url = value,
            "branch" => m.branch = Some(value),
            "update" => m.update = Some(value),
            _ => m.other.push((var.to_owned(), value)),
        }
    }
    order.into_iter().filter_map(|n| map.remove(&n)).collect()
}

/// Parse `git config -f .gitmodules --list -z` (`key\nvalue\0` records).
pub fn parse_config_z(bytes: &[u8]) -> Vec<ModuleConfig> {
    let text = String::from_utf8_lossy(bytes);
    collect(
        text.split('\0')
            .filter(|r| !r.is_empty())
            .map(|r| match r.split_once('\n') {
                Some((k, v)) => (k.to_owned(), v.to_owned()),
                None => (r.to_owned(), "true".to_owned()),
            }),
    )
}

/// Parse `git config -f .gitmodules --list` (`key=value` lines).
pub fn parse_config(text: &str) -> Vec<ModuleConfig> {
    collect(text.lines().filter_map(|l| {
        let (k, v) = l.split_once('=')?;
        Some((k.to_owned(), v.to_owned()))
    }))
}

/// Everything shown about one submodule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submodule {
    pub name: String,
    pub path: String,
    pub url: String,
    /// The URL this repository's `.git/config` uses (after init); differs
    /// from `url` when `.gitmodules` changed and `sync` was not run.
    pub configured_url: Option<String>,
    pub branch: Option<String>,
    pub update: Option<String>,
    pub state: SubState,
    /// Inside another submodule (from `--recursive`).
    pub nested: bool,
    /// The commit this repository records for it.
    pub recorded: Option<String>,
    /// The commit checked out in it.
    pub checked_out: Option<String>,
    pub describe: Option<String>,
    pub head_subject: Option<String>,
    /// The branch checked out inside (`None` when detached).
    pub head_branch: Option<String>,
    /// `git status --porcelain` lines inside it.
    pub dirty: Vec<String>,
    pub status_error: Option<String>,
    /// The ref compared against, and (ahead, behind).
    pub tracking: Option<String>,
    pub ahead_behind: Option<(usize, usize)>,
}

impl Submodule {
    pub fn initialised(&self) -> bool {
        self.state != SubState::NotInitialised
    }

    pub fn url_out_of_sync(&self) -> bool {
        self.configured_url
            .as_deref()
            .is_some_and(|c| !self.url.is_empty() && c != self.url)
    }
}

/// The whole tab.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overview {
    /// `.gitmodules` exists at the top of the repository.
    pub has_gitmodules: bool,
    pub modules: Vec<Submodule>,
    /// Sections of `.gitmodules` with no matching status line (a path
    /// that is not a gitlink in the index).
    pub orphans: Vec<ModuleConfig>,
    pub elapsed_ms: u64,
}

fn git_in(root: &Path, rel: &str, args: &[&str], cmds: &mut Vec<String>) -> Result<String, String> {
    let mut shown = vec!["-C", rel];
    shown.extend_from_slice(args);
    cmds.push(git_line(&shown));
    crate::cli::run(&root.join(rel), args, None)
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .map_err(|e| e.to_string())
}

fn config_of(root: &Path, rel: &str, cmds: &mut Vec<String>) -> Vec<ModuleConfig> {
    let dir = root.join(rel);
    if !dir.join(".gitmodules").is_file() {
        return Vec::new();
    }
    let args = ["config", "-f", ".gitmodules", "--list", "-z"];
    let mut shown: Vec<&str> = Vec::new();
    if !rel.is_empty() {
        shown.extend(["-C", rel]);
    }
    shown.extend_from_slice(&args);
    cmds.push(git_line(&shown));
    match crate::cli::run(&dir, &args, None) {
        Ok(o) => parse_config_z(&o.stdout),
        Err(_) => Vec::new(),
    }
}

/// Recorded gitlinks of `paths` (relative to `repo_rel`) from its index.
fn recorded_in(
    root: &Path,
    repo_rel: &str,
    paths: &[String],
    cmds: &mut Vec<String>,
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if paths.is_empty() {
        return out;
    }
    let mut args: Vec<String> = vec![
        "-c".into(),
        "core.quotepath=false".into(),
        "ls-files".into(),
        "-s".into(),
        "--".into(),
    ];
    args.extend(paths.iter().cloned());
    let mut shown: Vec<String> = Vec::new();
    if !repo_rel.is_empty() {
        shown.extend(["-C".to_owned(), repo_rel.to_owned()]);
    }
    shown.extend(args.iter().cloned());
    cmds.push(git_line(&shown));
    let Ok(o) = crate::cli::run(&root.join(repo_rel), &args, None) else {
        return out;
    };
    for line in String::from_utf8_lossy(&o.stdout).lines() {
        // `160000 <sha> <stage>\t<path>`
        let Some((meta, path)) = line.split_once('\t') else {
            continue;
        };
        let f: Vec<&str> = meta.split_whitespace().collect();
        if f.len() == 3 && f[0] == "160000" && (f[2] == "0" || f[2] == "2") {
            out.insert(unquote(path), f[1].to_owned());
        }
    }
    out
}

/// The closest submodule path that contains `path` (for nested ones).
fn parent_of<'a>(path: &str, all: &'a [StatusLine]) -> Option<&'a str> {
    all.iter()
        .map(|s| s.path.as_str())
        .filter(|p| *p != path && path.starts_with(&format!("{p}/")))
        .max_by_key(|p| p.len())
}

/// Read every submodule (status, config, recorded commit, inner state).
pub fn load(dir: &Path, cmds: &mut Vec<String>) -> Result<Overview, String> {
    let started = std::time::Instant::now();
    let has_gitmodules = dir.join(".gitmodules").is_file();
    // Raw stdout: the first line starts with a meaningful space.
    let status_text = super::git(dir, &["submodule", "status", "--recursive"], cmds)?;
    let lines = parse_status(&status_text);
    // Config and recorded commits per containing repository ("" = this one).
    let mut configs: HashMap<String, Vec<ModuleConfig>> = HashMap::new();
    let mut by_parent: HashMap<String, Vec<String>> = HashMap::new();
    for l in &lines {
        let parent = parent_of(&l.path, &lines).unwrap_or("").to_owned();
        let rel = if parent.is_empty() {
            l.path.clone()
        } else {
            l.path[parent.len() + 1..].to_owned()
        };
        by_parent.entry(parent).or_default().push(rel);
    }
    let mut recorded: HashMap<String, String> = HashMap::new();
    for (parent, rels) in &by_parent {
        if !configs.contains_key(parent) {
            configs.insert(parent.clone(), config_of(dir, parent, cmds));
        }
        for (rel, sha) in recorded_in(dir, parent, rels, cmds) {
            let full = if parent.is_empty() {
                rel
            } else {
                format!("{parent}/{rel}")
            };
            recorded.insert(full, sha);
        }
    }
    if !configs.contains_key("") {
        configs.insert(String::new(), config_of(dir, "", cmds));
    }
    let superproject_branch = git_probe(dir, &["symbolic-ref", "--short", "-q", "HEAD"], cmds);
    let mut used: Vec<String> = Vec::new();
    let mut modules = Vec::new();
    for l in &lines {
        let parent = parent_of(&l.path, &lines).unwrap_or("").to_owned();
        let rel = if parent.is_empty() {
            l.path.clone()
        } else {
            l.path[parent.len() + 1..].to_owned()
        };
        let cfg = configs
            .get(&parent)
            .and_then(|c| c.iter().find(|m| m.path == rel))
            .cloned()
            .unwrap_or_else(|| ModuleConfig {
                name: rel.clone(),
                path: rel.clone(),
                ..Default::default()
            });
        if parent.is_empty() {
            used.push(cfg.name.clone());
        }
        let configured_url = if parent.is_empty() {
            git_probe(
                dir,
                &["config", "--get", &format!("submodule.{}.url", cfg.name)],
                cmds,
            )
        } else {
            None
        };
        let mut m = Submodule {
            name: cfg.name.clone(),
            path: l.path.clone(),
            url: cfg.url.clone(),
            configured_url,
            branch: cfg.branch.clone(),
            update: cfg.update.clone(),
            state: l.state,
            nested: !parent.is_empty(),
            recorded: recorded.get(&l.path).cloned(),
            checked_out: None,
            describe: l.describe.clone(),
            head_subject: None,
            head_branch: None,
            dirty: Vec::new(),
            status_error: None,
            tracking: None,
            ahead_behind: None,
        };
        let checked_out = dir.join(&l.path).join(".git").exists();
        if m.initialised() && checked_out {
            m.checked_out = Some(l.sha.clone());
            if m.recorded.is_none() && l.state == SubState::Current {
                m.recorded = Some(l.sha.clone());
            }
            match git_in(dir, &l.path, &["status", "--porcelain"], cmds) {
                Ok(t) => {
                    m.dirty = t
                        .lines()
                        .filter(|x| !x.trim().is_empty())
                        .map(str::to_owned)
                        .collect()
                }
                Err(e) => m.status_error = Some(e),
            }
            m.head_subject = git_in(dir, &l.path, &["log", "-1", "--format=%s"], cmds)
                .ok()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty());
            m.head_branch = git_in(
                dir,
                &l.path,
                &["symbolic-ref", "--short", "-q", "HEAD"],
                cmds,
            )
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
            let tracking = match m.branch.as_deref() {
                Some(".") => superproject_branch.clone().map(|b| format!("origin/{b}")),
                Some(b) if !b.is_empty() => Some(format!("origin/{b}")),
                _ => m.head_branch.as_ref().and_then(|_| {
                    git_in(
                        dir,
                        &l.path,
                        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
                        cmds,
                    )
                    .ok()
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                }),
            };
            if let Some(t) = tracking {
                let range = format!("HEAD...{t}");
                if let Ok(text) = git_in(
                    dir,
                    &l.path,
                    &["rev-list", "--left-right", "--count", &range],
                    cmds,
                ) && let Some(ab) = parse_left_right(&text)
                {
                    m.ahead_behind = Some(ab);
                }
                m.tracking = Some(t);
            }
        }
        modules.push(m);
    }
    let orphans = configs
        .get("")
        .map(|c| {
            c.iter()
                .filter(|m| !used.contains(&m.name))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    Ok(Overview {
        has_gitmodules,
        modules,
        orphans,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// `3\t1` of `rev-list --left-right --count` → (3, 1).
pub fn parse_left_right(text: &str) -> Option<(usize, usize)> {
    let mut it = text.split_whitespace();
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

fn with_paths(mut v: Vec<String>, paths: &[String]) -> Vec<String> {
    if !paths.is_empty() {
        v.push("--".into());
        v.extend(paths.iter().cloned());
    }
    v
}

/// `git submodule init [-- paths]`: copy URLs from `.gitmodules` into
/// `.git/config`. Local only.
pub fn init_args(paths: &[String]) -> Vec<String> {
    with_paths(vec!["submodule".into(), "init".into()], paths)
}

/// `git submodule update --init --recursive [--remote] [-- paths]`: clone
/// missing submodules and check out the recorded commits (with `--remote`,
/// the newest commit of each tracked branch instead). Fetches.
pub fn update_args(paths: &[String], remote: bool) -> Vec<String> {
    let mut v: Vec<String> = vec![
        "submodule".into(),
        "update".into(),
        "--init".into(),
        "--recursive".into(),
    ];
    if remote {
        v.push("--remote".into());
    }
    with_paths(v, paths)
}

/// `git submodule sync --recursive [-- paths]`: copy changed URLs from
/// `.gitmodules` into the configuration. Local only.
pub fn sync_args(paths: &[String]) -> Vec<String> {
    with_paths(
        vec!["submodule".into(), "sync".into(), "--recursive".into()],
        paths,
    )
}

/// `git submodule add [-b BRANCH] -- URL PATH`. Clones.
pub fn add_args(url: &str, path: &str, branch: &str) -> Vec<String> {
    let mut v: Vec<String> = vec!["submodule".into(), "add".into()];
    let branch = branch.trim();
    if !branch.is_empty() {
        v.push("-b".into());
        v.push(branch.to_owned());
    }
    v.push("--".into());
    v.push(url.trim().to_owned());
    v.push(path.trim().trim_end_matches(['/', '\\']).replace('\\', "/"));
    v
}

/// The commands that remove a submodule completely, as typed.
pub fn remove_commands(name: &str, path: &str, modules_dir: &Path) -> Vec<String> {
    vec![
        git_line(&["submodule", "deinit", "-f", "--", path]),
        git_line(&["rm", "-f", "--", path]),
        format!(
            "rm -rf {}",
            crate::devops::workbench::shell::sh_path(&modules_dir.join(name))
        ),
    ]
}

/// `<common git dir>/modules`.
pub fn modules_dir(dir: &Path, cmds: &mut Vec<String>) -> Result<PathBuf, String> {
    let common = git_probe(dir, &["rev-parse", "--git-common-dir"], cmds)
        .ok_or_else(|| "Not a git repository.".to_owned())?;
    Ok(resolve_in(dir, &common).join("modules"))
}

/// Remove a submodule: deinit, `git rm`, then its clone under
/// `.git/modules/NAME` (so the same name can be added again).
pub fn remove(
    dir: &Path,
    name: &str,
    path: &str,
    cmds: &mut Vec<String>,
) -> Result<String, String> {
    if name.is_empty()
        || name.starts_with(['/', '\\'])
        || name.contains(':')
        || name.split(['/', '\\']).any(|c| c == ".." || c.is_empty())
    {
        return Err(format!("Refusing to remove a submodule named \"{name}\"."));
    }
    let modules = modules_dir(dir, cmds)?;
    git_ok(dir, &["submodule", "deinit", "-f", "--", path], cmds)?;
    git_ok(dir, &["rm", "-f", "--", path], cmds)?;
    let clone = modules.join(name);
    cmds.push(format!(
        "rm -rf {}",
        crate::devops::workbench::shell::sh_path(&clone)
    ));
    if clone.starts_with(&modules) && clone.is_dir() {
        std::fs::remove_dir_all(&clone).map_err(|e| {
            format!(
                "Removed from the index, but could not delete {}: {e}",
                clone.display()
            )
        })?;
    }
    Ok(format!(
        "Removed the submodule {path}. Commit the change to .gitmodules and the removed path."
    ))
}

/// Run a submodule command; the summary is git's output or "Done".
pub fn run(dir: &Path, args: &[String], cmds: &mut Vec<String>) -> Result<String, String> {
    let out = git_raw(dir, args, &[], cmds)?;
    let text = super::output_text(&out);
    if out.success {
        Ok(if text.trim().is_empty() {
            "Done".to_owned()
        } else {
            text
        })
    } else {
        Err(if text.is_empty() {
            format!("git {} failed", args.join(" "))
        } else {
            text
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA_A: &str = "21afcf5e2952b813f6325134dd204b1f8fb5ae86";
    const SHA_B: &str = "585a15e8b312ca4196f2ebb90846e9ec007d39f9";

    #[test]
    fn parses_status_lines() {
        let text = format!(
            " {SHA_A} libs/sub (heads/main)\n+{SHA_B} other dir/two (heads/main)\n-{SHA_A} vendor/x\nU{SHA_B} conflicted (v1.2-3-gabc)\ngarbage\n"
        );
        let v = parse_status(&text);
        assert_eq!(v.len(), 4);
        assert_eq!(v[0].state, SubState::Current);
        assert_eq!(v[0].path, "libs/sub");
        assert_eq!(v[0].describe.as_deref(), Some("heads/main"));
        assert_eq!(v[1].state, SubState::Modified);
        assert_eq!(v[1].path, "other dir/two");
        assert_eq!(v[1].sha, SHA_B);
        assert_eq!(v[2].state, SubState::NotInitialised);
        assert_eq!(v[2].describe, None);
        assert_eq!(v[3].state, SubState::Conflict);
    }

    #[test]
    fn parses_gitmodules_config() {
        let z = b"submodule.libs/sub.path\nlibs/sub\0submodule.libs/sub.url\n../sub-bare\0submodule.libs/sub.branch\nmain\0submodule.a.b.c.path\nweird/dots\0submodule.a.b.c.url\nhttps://x/y.git\0submodule.a.b.c.shallow\ntrue\0";
        let v = parse_config_z(z);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "libs/sub");
        assert_eq!(v[0].url, "../sub-bare");
        assert_eq!(v[0].branch.as_deref(), Some("main"));
        assert_eq!(v[1].name, "a.b.c");
        assert_eq!(v[1].path, "weird/dots");
        assert_eq!(v[1].other, vec![("shallow".to_owned(), "true".to_owned())]);
        let plain = parse_config(
            "submodule.libs/sub.path=libs/sub\nsubmodule.libs/sub.url=../sub-bare\nsubmodule.other dir/two.path=other dir/two\n",
        );
        assert_eq!(plain.len(), 2);
        assert_eq!(plain[1].name, "other dir/two");
    }

    #[test]
    fn nested_parent() {
        let lines = parse_status(&format!(
            " {SHA_A} a (x)\n {SHA_A} a/b (x)\n {SHA_A} a/b/c (x)\n {SHA_A} ab (x)\n"
        ));
        assert_eq!(parent_of("a/b/c", &lines), Some("a/b"));
        assert_eq!(parent_of("a/b", &lines), Some("a"));
        assert_eq!(parent_of("ab", &lines), None);
        assert_eq!(parse_left_right("3\t1\n"), Some((3, 1)));
        assert_eq!(parse_left_right(""), None);
    }

    #[test]
    fn command_lines() {
        assert_eq!(init_args(&[]), vec!["submodule", "init"]);
        assert_eq!(
            update_args(&["libs/sub".into()], true),
            vec![
                "submodule",
                "update",
                "--init",
                "--recursive",
                "--remote",
                "--",
                "libs/sub"
            ]
        );
        assert_eq!(sync_args(&[]), vec!["submodule", "sync", "--recursive"]);
        assert_eq!(
            add_args(" https://x/lib.git ", "vendor\\lib\\", "main"),
            vec![
                "submodule",
                "add",
                "-b",
                "main",
                "--",
                "https://x/lib.git",
                "vendor/lib"
            ]
        );
        let cmds = remove_commands("libs/sub", "libs/sub", Path::new("/r/.git/modules"));
        assert_eq!(cmds[0], "git submodule deinit -f -- libs/sub");
        assert_eq!(cmds[1], "git rm -f -- libs/sub");
        assert_eq!(cmds[2], "rm -rf '/r/.git/modules/libs/sub'");
    }
}
