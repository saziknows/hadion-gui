//! Worktrees: `git worktree list --porcelain`, the state of each one
//! (`git -C PATH status --porcelain=v2 --branch`), and the command lines
//! for add / remove / lock / unlock / move / prune.

use std::path::{Path, PathBuf};

use super::{git, git_line};

/// One entry of `git worktree list --porcelain`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Worktree {
    pub path: String,
    pub head: Option<String>,
    /// Short branch name (`refs/heads/` stripped).
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    /// `Some(reason)` when locked (the reason may be empty).
    pub locked: Option<String>,
    /// `Some(reason)` when git would prune it (its directory is gone).
    pub prunable: Option<String>,
    /// The first entry: the main working tree.
    pub main: bool,
}

/// Parse `git worktree list --porcelain`.
pub fn parse_porcelain(text: &str) -> Vec<Worktree> {
    let mut out: Vec<Worktree> = Vec::new();
    let mut cur: Option<Worktree> = None;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            if let Some(w) = cur.take() {
                out.push(w);
            }
            continue;
        }
        let (key, val) = match line.split_once(' ') {
            Some((k, v)) => (k, Some(v)),
            None => (line, None),
        };
        if key == "worktree" {
            if let Some(w) = cur.take() {
                out.push(w);
            }
            cur = Some(Worktree {
                path: val.unwrap_or_default().to_owned(),
                ..Default::default()
            });
            continue;
        }
        let Some(w) = cur.as_mut() else { continue };
        match key {
            "HEAD" => w.head = val.map(str::to_owned),
            "branch" => {
                w.branch = val.map(|b| b.strip_prefix("refs/heads/").unwrap_or(b).to_owned())
            }
            "bare" => w.bare = true,
            "detached" => w.detached = true,
            "locked" => w.locked = Some(val.unwrap_or_default().to_owned()),
            "prunable" => w.prunable = Some(val.unwrap_or_default().to_owned()),
            _ => {}
        }
    }
    if let Some(w) = cur.take() {
        out.push(w);
    }
    if let Some(first) = out.first_mut() {
        first.main = true;
    }
    out
}

/// The state of a worktree's files and branch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
    pub conflicted: usize,
    pub upstream: Option<String>,
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    /// Changed paths with their porcelain code (`M.`, `??`, `UU`), capped.
    pub files: Vec<(String, String)>,
    /// Changed paths beyond the cap.
    pub more: usize,
}

impl Status {
    pub fn dirty(&self) -> bool {
        self.staged + self.unstaged + self.untracked + self.conflicted > 0
    }

    pub fn changes(&self) -> usize {
        self.files.len() + self.more
    }
}

/// Changed paths kept per worktree.
pub const MAX_FILES: usize = 200;

/// Parse `git status --porcelain=v2 --branch` (without `-z`).
pub fn parse_status(text: &str) -> Status {
    let mut s = Status::default();
    let push = |s: &mut Status, code: &str, path: &str| {
        if s.files.len() < MAX_FILES {
            s.files.push((path.to_owned(), code.to_owned()));
        } else {
            s.more += 1;
        }
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# branch.upstream ") {
            s.upstream = Some(rest.trim().to_owned());
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            let mut it = rest.split_whitespace();
            s.ahead = it
                .next()
                .and_then(|a| a.trim_start_matches('+').parse().ok());
            s.behind = it
                .next()
                .and_then(|b| b.trim_start_matches('-').parse().ok());
        } else if let Some(rest) = line.strip_prefix("1 ") {
            let xy = &rest[..2.min(rest.len())];
            let path = rest.splitn(8, ' ').nth(7).unwrap_or("");
            count_xy(&mut s, xy);
            push(&mut s, xy, path);
        } else if let Some(rest) = line.strip_prefix("2 ") {
            let xy = &rest[..2.min(rest.len())];
            // `2 XY sub mH mI mW hH hI Rscore path\torig`
            let paths = rest.splitn(9, ' ').nth(8).unwrap_or("");
            let path = paths.split('\t').next().unwrap_or(paths);
            count_xy(&mut s, xy);
            push(&mut s, xy, path);
        } else if let Some(rest) = line.strip_prefix("u ") {
            s.conflicted += 1;
            let path = rest.splitn(10, ' ').nth(9).unwrap_or("");
            push(&mut s, "UU", path);
        } else if let Some(path) = line.strip_prefix("? ") {
            s.untracked += 1;
            push(&mut s, "??", path);
        }
    }
    s
}

fn count_xy(s: &mut Status, xy: &str) {
    let mut c = xy.chars();
    let (x, y) = (c.next().unwrap_or('.'), c.next().unwrap_or('.'));
    if x != '.' {
        s.staged += 1;
    }
    if y != '.' {
        s.unstaged += 1;
    }
}

/// A worktree with its state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub wt: Worktree,
    /// `None` for bare or missing worktrees.
    pub status: Option<Result<Status, String>>,
    /// HEAD's subject.
    pub subject: Option<String>,
    pub head_time: Option<i64>,
}

/// List the worktrees and read each one's state.
pub fn list(dir: &Path, cmds: &mut Vec<String>) -> Result<Vec<Entry>, String> {
    let text = git(dir, &["worktree", "list", "--porcelain"], cmds)?;
    let mut out = Vec::new();
    for wt in parse_porcelain(&text) {
        let path = PathBuf::from(&wt.path);
        let exists = path.is_dir();
        let status = if wt.bare || !exists {
            None
        } else {
            Some(
                git(
                    &path,
                    &[
                        "-c",
                        "core.quotepath=false",
                        "status",
                        "--porcelain=v2",
                        "--branch",
                    ],
                    cmds,
                )
                .map(|t| parse_status(&t)),
            )
        };
        // `git -C` is how it reads; record it that way.
        if let Some(last) = cmds.last_mut()
            && status.is_some()
        {
            *last = last.replacen("git ", &format!("git -C {} ", quote(&wt.path)), 1);
        }
        let (subject, head_time) = match &wt.head {
            Some(h) if !wt.bare => match git(dir, &["log", "-1", "--format=%at%x1f%s", h], cmds) {
                Ok(t) => {
                    let (time, subj) = t.trim().split_once('\u{1f}').unwrap_or(("", ""));
                    (Some(subj.to_owned()), time.parse().ok())
                }
                Err(_) => (None, None),
            },
            _ => (None, None),
        };
        out.push(Entry {
            wt,
            status,
            subject,
            head_time,
        });
    }
    Ok(out)
}

fn quote(s: &str) -> String {
    if s.contains(' ') {
        format!("\"{s}\"")
    } else {
        s.to_owned()
    }
}

/// How a new worktree gets its checkout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddMode {
    /// `-b NAME` from a start point (empty = HEAD).
    NewBranch { name: String, start: String },
    /// An existing local branch (not checked out anywhere else).
    Existing { branch: String },
    /// `--detach` at a commit.
    Detached { commit: String },
}

/// `git worktree add …` arguments.
pub fn add_args(path: &str, mode: &AddMode) -> Vec<String> {
    let mut v = vec!["worktree".to_owned(), "add".to_owned()];
    match mode {
        AddMode::NewBranch { name, start } => {
            v.extend(["-b".to_owned(), name.trim().to_owned(), path.to_owned()]);
            if !start.trim().is_empty() {
                v.push(start.trim().to_owned());
            }
        }
        AddMode::Existing { branch } => {
            v.extend([path.to_owned(), branch.trim().to_owned()]);
        }
        AddMode::Detached { commit } => {
            v.extend(["--detach".to_owned(), path.to_owned()]);
            let c = commit.trim();
            v.push(if c.is_empty() { "HEAD" } else { c }.to_owned());
        }
    }
    v
}

/// `git worktree remove`: `--force` once for a worktree with changes,
/// twice for a locked one (git's rule).
pub fn remove_args(path: &str, dirty: bool, locked: bool) -> Vec<String> {
    let mut v = vec!["worktree".to_owned(), "remove".to_owned()];
    let n = if locked { 2 } else { dirty as usize };
    for _ in 0..n {
        v.push("--force".to_owned());
    }
    v.push(path.to_owned());
    v
}

pub fn lock_args(path: &str, reason: &str) -> Vec<String> {
    let mut v = vec!["worktree".to_owned(), "lock".to_owned()];
    if !reason.trim().is_empty() {
        v.extend(["--reason".to_owned(), reason.trim().to_owned()]);
    }
    v.push(path.to_owned());
    v
}

pub fn unlock_args(path: &str) -> Vec<String> {
    vec!["worktree".to_owned(), "unlock".to_owned(), path.to_owned()]
}

pub fn move_args(path: &str, to: &str) -> Vec<String> {
    vec![
        "worktree".to_owned(),
        "move".to_owned(),
        path.to_owned(),
        to.to_owned(),
    ]
}

pub fn prune_args(dry_run: bool) -> Vec<String> {
    let mut v = vec!["worktree".to_owned(), "prune".to_owned()];
    if dry_run {
        v.push("--dry-run".to_owned());
    }
    v.push("-v".to_owned());
    v
}

/// Command line of a set of arguments, for confirmations.
pub fn line(args: &[String]) -> String {
    git_line(args)
}

/// The default place for a new worktree: beside the main one, named
/// `<repo>-<branch>` (slashes become dashes).
pub fn default_path(main: &Path, branch: &str) -> PathBuf {
    let name = main
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_owned());
    let slug: String = branch
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-');
    let file = if slug.is_empty() {
        format!("{name}-worktree")
    } else {
        format!("{name}-{slug}")
    };
    main.parent().unwrap_or(main).join(file)
}

/// Paths `git worktree prune --dry-run -v` would remove: `Removing
/// worktrees/x: gitdir file points to non-existent location`.
pub fn parse_prune(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("Removing ")?;
            let (what, why) = rest.split_once(": ").unwrap_or((rest, ""));
            Some((what.to_owned(), why.to_owned()))
        })
        .collect()
}

/// Same path, whatever the separators and case (Windows).
pub fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('\\', "/");
        let s = s.trim_end_matches('/').to_owned();
        if cfg!(windows) {
            s.to_ascii_lowercase()
        } else {
            s
        }
    };
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PORCELAIN: &str = "\
worktree C:/Users/me/dev/gitvis
HEAD 77e11462b51748bdbe82e09c88d7d8950f193dc7
branch refs/heads/devops

worktree C:/Users/me/dev/gitvis-wt/ci
HEAD 77e11462b51748bdbe82e09c88d7d8950f193dc7
branch refs/heads/feature/ci-tabs
locked reason with spaces

worktree C:/tmp/gone
HEAD 1111111111111111111111111111111111111111
detached
prunable gitdir file points to non-existent location

worktree C:/repos/bare.git
bare

worktree C:/x/locked-no-reason
HEAD 2222222222222222222222222222222222222222
branch refs/heads/x
locked
";

    #[test]
    fn porcelain() {
        let w = parse_porcelain(PORCELAIN);
        assert_eq!(w.len(), 5);
        assert!(w[0].main && !w[1].main);
        assert_eq!(w[0].branch.as_deref(), Some("devops"));
        assert_eq!(w[1].branch.as_deref(), Some("feature/ci-tabs"));
        assert_eq!(w[1].locked.as_deref(), Some("reason with spaces"));
        assert!(w[2].detached);
        assert_eq!(
            w[2].prunable.as_deref(),
            Some("gitdir file points to non-existent location")
        );
        assert!(w[3].bare);
        assert_eq!(w[3].head, None);
        assert_eq!(w[4].locked.as_deref(), Some(""));
    }

    #[test]
    fn status_v2() {
        let text = "\
# branch.oid 77e11462b51748bdbe82e09c88d7d8950f193dc7
# branch.head main
# branch.upstream origin/main
# branch.ab +2 -1
1 M. N... 100644 100644 100644 aaaa bbbb src/staged.rs
1 .M N... 100644 100644 100644 aaaa bbbb src/with space.rs
1 MM N... 100644 100644 100644 aaaa bbbb both.rs
2 R. N... 100644 100644 100644 aaaa bbbb R100 new name.rs\told.rs
u UU N... 100644 100644 100644 100644 aaaa bbbb cccc conflict.rs
? untracked.txt
";
        let s = parse_status(text);
        assert_eq!(s.staged, 3);
        assert_eq!(s.unstaged, 2);
        assert_eq!(s.untracked, 1);
        assert_eq!(s.conflicted, 1);
        assert_eq!(s.ahead, Some(2));
        assert_eq!(s.behind, Some(1));
        assert_eq!(s.upstream.as_deref(), Some("origin/main"));
        let paths: Vec<&str> = s.files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "src/staged.rs",
                "src/with space.rs",
                "both.rs",
                "new name.rs",
                "conflict.rs",
                "untracked.txt"
            ]
        );
        assert!(s.dirty());
        let clean = parse_status("# branch.oid x\n# branch.head main\n");
        assert!(!clean.dirty());
        assert_eq!(clean.ahead, None);
    }

    #[test]
    fn command_lines() {
        assert_eq!(
            line(&add_args(
                "C:/dev/app-feature",
                &AddMode::NewBranch {
                    name: "feature".into(),
                    start: "origin/main".into()
                }
            )),
            "git worktree add -b feature C:/dev/app-feature origin/main"
        );
        assert_eq!(
            add_args(
                "p",
                &AddMode::Existing {
                    branch: "dev".into()
                }
            ),
            vec!["worktree", "add", "p", "dev"]
        );
        assert_eq!(
            add_args("p", &AddMode::Detached { commit: "".into() }),
            vec!["worktree", "add", "--detach", "p", "HEAD"]
        );
        assert_eq!(
            remove_args("p", true, false),
            vec!["worktree", "remove", "--force", "p"]
        );
        assert_eq!(
            remove_args("p", false, true),
            vec!["worktree", "remove", "--force", "--force", "p"]
        );
        assert_eq!(
            remove_args("p", false, false),
            vec!["worktree", "remove", "p"]
        );
        assert_eq!(
            lock_args("p", " on a USB disk "),
            vec!["worktree", "lock", "--reason", "on a USB disk", "p"]
        );
        assert_eq!(lock_args("p", ""), vec!["worktree", "lock", "p"]);
        assert_eq!(unlock_args("p"), vec!["worktree", "unlock", "p"]);
        assert_eq!(move_args("a", "b"), vec!["worktree", "move", "a", "b"]);
        assert_eq!(
            prune_args(true),
            vec!["worktree", "prune", "--dry-run", "-v"]
        );
    }

    #[test]
    fn default_paths() {
        let main = Path::new("/home/me/dev/gitvis");
        assert_eq!(
            default_path(main, "feature/login page"),
            Path::new("/home/me/dev/gitvis-feature-login-page")
        );
        assert_eq!(
            default_path(main, ""),
            Path::new("/home/me/dev/gitvis-worktree")
        );
    }

    #[test]
    fn prune_output() {
        let p = parse_prune(
            "Removing worktrees/gone: gitdir file points to non-existent location\nRemoving worktrees/x: not a valid directory\n",
        );
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].0, "worktrees/gone");
        assert!(parse_prune("").is_empty());
    }

    #[test]
    fn paths_compare() {
        assert!(same_path(Path::new("C:/a/b/"), Path::new("C:/a/b")));
        if cfg!(windows) {
            assert!(same_path(Path::new(r"C:\Users\X"), Path::new("c:/users/x")));
        }
    }
}
