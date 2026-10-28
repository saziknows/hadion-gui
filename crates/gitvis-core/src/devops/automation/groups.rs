//! Repository groups: named lists of repositories (kept in the settings),
//! their status (branch, ahead / behind, changes, last commit) and batch
//! operations run one repository after another with a result each:
//! fetch (`git fetch --all --prune`), pull (`git pull --ff-only`, skipping
//! dirty or diverged repositories), create a branch (`git switch -c NAME
//! [BASE]`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::devops::home::fetch;
use crate::devops::proc::{ProcSpec, render_args, run_capture};
use crate::{Error, Result};

/// A named list of repositories.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Group {
    pub name: String,
    /// Working directories.
    pub repos: Vec<PathBuf>,
}

impl Group {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            repos: Vec::new(),
        }
    }

    /// Add a repository unless it is there already (same path, compared
    /// case-insensitively on Windows). Returns true when added.
    pub fn add(&mut self, dir: PathBuf) -> bool {
        if self.repos.iter().any(|r| same_path(r, &dir)) {
            return false;
        }
        self.repos.push(dir);
        true
    }
}

/// Two paths name the same folder (case and separators ignored on Windows,
/// a trailing separator ignored everywhere).
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

/// The group with this name (case-insensitive).
pub fn find<'a>(groups: &'a [Group], name: &str) -> Option<&'a Group> {
    groups
        .iter()
        .find(|g| g.name.trim().eq_ignore_ascii_case(name.trim()))
}

/// Why a group name cannot be used (`None`: it can).
pub fn name_problem(groups: &[Group], name: &str, except: Option<&str>) -> Option<&'static str> {
    let n = name.trim();
    if n.is_empty() {
        return Some("Type a name");
    }
    if n.chars().count() > 60 {
        return Some("At most 60 characters");
    }
    let taken = groups.iter().any(|g| {
        g.name.trim().eq_ignore_ascii_case(n)
            && except.is_none_or(|e| !g.name.eq_ignore_ascii_case(e))
    });
    taken.then_some("A group with this name exists")
}

/// What `git status --porcelain=v2 --branch` (and the last commit's time)
/// says about a repository.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoStatus {
    /// `None` when detached.
    pub branch: Option<String>,
    /// The commit HEAD is at (abbreviated), `None` before the first commit.
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
    pub conflicted: usize,
    /// Committer time of HEAD (epoch seconds).
    pub last_commit: Option<i64>,
}

impl RepoStatus {
    /// Tracked changes (staged, unstaged, conflicted).
    pub fn tracked_changes(&self) -> usize {
        self.staged + self.unstaged + self.conflicted
    }

    pub fn dirty(&self) -> bool {
        self.tracked_changes() > 0
    }

    pub fn diverged(&self) -> bool {
        self.ahead > 0 && self.behind > 0
    }
}

/// Parse `git status --porcelain=v2 --branch` output.
pub fn parse_status(out: &str) -> RepoStatus {
    let mut s = RepoStatus::default();
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("# branch.oid ") {
            let oid = rest.trim();
            if oid != "(initial)" {
                s.head = Some(oid.chars().take(10).collect());
            }
        } else if let Some(rest) = line.strip_prefix("# branch.head ") {
            let b = rest.trim();
            if b != "(detached)" {
                s.branch = Some(b.to_owned());
            }
        } else if let Some(rest) = line.strip_prefix("# branch.upstream ") {
            s.upstream = Some(rest.trim().to_owned());
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            for part in rest.split_whitespace() {
                if let Some(a) = part.strip_prefix('+') {
                    s.ahead = a.parse().unwrap_or(0);
                } else if let Some(b) = part.strip_prefix('-') {
                    s.behind = b.parse().unwrap_or(0);
                }
            }
        } else if line.starts_with("1 ") || line.starts_with("2 ") {
            // `1 XY …`: X = index, Y = worktree.
            let xy: Vec<char> = line[2..].chars().take(2).collect();
            if xy.first().is_some_and(|c| *c != '.') {
                s.staged += 1;
            }
            if xy.get(1).is_some_and(|c| *c != '.') {
                s.unstaged += 1;
            }
        } else if line.starts_with("u ") {
            s.conflicted += 1;
        } else if line.starts_with("? ") {
            s.untracked += 1;
        }
    }
    s
}

fn git_capture(dir: &Path, args: &[&str], commands: &mut Vec<String>) -> Result<String> {
    commands.push(format!(
        "git {}    (in {})",
        render_args(args),
        dir.display()
    ));
    let spec = ProcSpec::new("git", dir).args(args.iter().copied());
    let out = run_capture(&spec, Some(Duration::from_secs(60)))?;
    if !out.success() {
        return Err(Error::new(out.error_text()));
    }
    Ok(out.stdout_text())
}

/// Read a repository's status (two quick local commands).
pub fn read_status(dir: &Path, commands: &mut Vec<String>) -> Result<RepoStatus> {
    if !dir.exists() {
        return Err(Error::new("folder not found"));
    }
    let out = git_capture(
        dir,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=normal",
        ],
        commands,
    )?;
    let mut s = parse_status(&out);
    if s.head.is_some() {
        s.last_commit = git_capture(dir, &["log", "-1", "--format=%ct"], commands)
            .ok()
            .and_then(|t| t.trim().parse().ok());
    }
    Ok(s)
}

/// A batch operation over a group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Batch {
    Fetch,
    Pull,
    Branch { name: String, base: Option<String> },
}

impl Batch {
    /// The git arguments run in each repository.
    pub fn args(&self) -> Vec<String> {
        match self {
            Batch::Fetch => fetch::ARGS.iter().map(|s| s.to_string()).collect(),
            Batch::Pull => vec!["pull".into(), "--ff-only".into()],
            Batch::Branch { name, base } => {
                let mut v = vec!["switch".to_owned(), "-c".to_owned(), name.clone()];
                if let Some(b) = base.as_ref().filter(|b| !b.trim().is_empty()) {
                    v.push(b.trim().to_owned());
                }
                v
            }
        }
    }

    /// `git pull --ff-only`.
    pub fn command_line(&self) -> String {
        let a = self.args();
        let refs: Vec<&str> = a.iter().map(String::as_str).collect();
        format!("git {}", render_args(&refs))
    }

    /// `Fetch`, `Pull`, `Create branch x`.
    pub fn label(&self) -> String {
        match self {
            Batch::Fetch => "Fetch".to_owned(),
            Batch::Pull => "Pull".to_owned(),
            Batch::Branch { name, .. } => format!("Create branch {name}"),
        }
    }

    /// Whether it talks to a remote.
    pub fn remote(&self) -> bool {
        !matches!(self, Batch::Branch { .. })
    }

    /// What happens before the command in each repository (for the
    /// confirmation).
    pub fn checks(&self) -> Vec<String> {
        match self {
            Batch::Fetch => vec!["Skipped: folders that are missing or not a repository".into()],
            Batch::Pull => vec![
                "First: git status --porcelain=v2 --branch".into(),
                "Skipped: uncommitted changes to tracked files, no upstream, detached HEAD, diverged from the upstream (ahead and behind)".into(),
            ],
            Batch::Branch { name, base } => {
                let mut v = vec![
                    "First: git status --porcelain=v2 --branch".into(),
                    format!("git rev-parse --verify --quiet refs/heads/{name}"),
                ];
                if let Some(b) = base.as_ref().filter(|b| !b.trim().is_empty()) {
                    v.push(format!("git rev-parse --verify --quiet {}^{{commit}}", b.trim()));
                    v.push(format!(
                        "Skipped: the branch exists already, {} is unknown there, or uncommitted changes (switching to another base could carry or clash with them)",
                        b.trim()
                    ));
                } else {
                    v.push("Skipped: the branch exists already (the new branch starts at HEAD and keeps uncommitted changes)".into());
                }
                v
            }
        }
    }
}

/// `Err` with the reason a branch name is not allowed (a subset of
/// `git check-ref-format --branch`, which runs too).
pub fn branch_name_problem(name: &str) -> Option<&'static str> {
    let n = name.trim();
    if n.is_empty() {
        return Some("Type a branch name");
    }
    if n.starts_with('-') {
        return Some("Cannot start with -");
    }
    if n.contains(char::is_whitespace) {
        return Some("No spaces");
    }
    if n.contains("..") || n.contains("@{") || n.contains("//") {
        return Some("No .., @{ or //");
    }
    if n.chars()
        .any(|c| matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\') || c.is_control())
    {
        return Some("No ~ ^ : ? * [ or backslash");
    }
    if n.ends_with('/') || n.ends_with('.') || n.ends_with(".lock") || n.starts_with('/') {
        return Some("Cannot end with /, . or .lock");
    }
    if n == "@" || n == "HEAD" {
        return Some("Reserved name");
    }
    None
}

/// What happened in one repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Done; a short summary (`2 new, 1 updated`, `fast-forwarded 3 commits`).
    Ok(String),
    Failed(String),
    /// Not run; why.
    Skipped(String),
}

impl Outcome {
    pub fn label(&self) -> &'static str {
        match self {
            Outcome::Ok(_) => "ok",
            Outcome::Failed(_) => "failed",
            Outcome::Skipped(_) => "skipped",
        }
    }

    pub fn text(&self) -> &str {
        match self {
            Outcome::Ok(s) | Outcome::Failed(s) | Outcome::Skipped(s) => s,
        }
    }
}

/// One repository's result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoResult {
    pub name: String,
    pub dir: PathBuf,
    pub outcome: Outcome,
    pub elapsed_ms: u64,
    /// git's output (trimmed), for the detail view.
    pub output: String,
}

/// Why a repository is skipped for a batch, from its fresh status
/// (`exists` is whether the branch exists, `base_ok` whether BASE resolves).
pub fn skip_reason(batch: &Batch, s: &RepoStatus, exists: bool, base_ok: bool) -> Option<String> {
    let changes = |s: &RepoStatus| {
        let mut parts = Vec::new();
        if s.staged > 0 {
            parts.push(format!("{} staged", s.staged));
        }
        if s.unstaged > 0 {
            parts.push(format!("{} changed", s.unstaged));
        }
        if s.conflicted > 0 {
            parts.push(format!("{} conflicted", s.conflicted));
        }
        parts.join(", ")
    };
    match batch {
        Batch::Fetch => None,
        Batch::Pull => {
            if s.dirty() {
                Some(format!("uncommitted changes ({})", changes(s)))
            } else if s.branch.is_none() {
                Some("detached HEAD".into())
            } else if s.upstream.is_none() {
                Some(format!(
                    "{} has no upstream",
                    s.branch.as_deref().unwrap_or("the branch")
                ))
            } else if s.diverged() {
                Some(format!(
                    "diverged from {}: {} ahead, {} behind",
                    s.upstream.as_deref().unwrap_or("the upstream"),
                    s.ahead,
                    s.behind
                ))
            } else {
                None
            }
        }
        Batch::Branch { name, base } => {
            let based = base.as_ref().is_some_and(|b| !b.trim().is_empty());
            if exists {
                Some(format!("branch {name} exists already"))
            } else if based && !base_ok {
                Some(format!(
                    "{} is not known here",
                    base.as_deref().unwrap_or("").trim()
                ))
            } else if based && s.dirty() {
                Some(format!("uncommitted changes ({})", changes(s)))
            } else {
                None
            }
        }
    }
}

/// The line of git's output that says what went wrong: the `fatal:` or
/// `error:` line (without the prefix), else the last non-empty line.
fn error_line(s: &str) -> String {
    let lines: Vec<&str> = s.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    for l in &lines {
        for p in ["fatal:", "error:"] {
            if let Some(rest) = l.strip_prefix(p) {
                return rest.trim().to_owned();
            }
        }
    }
    lines.last().map(|l| (*l).to_owned()).unwrap_or_default()
}

/// `Fast-forward` / `Already up to date.` / `Updating a..b` from pull output.
fn pull_summary(stdout: &str) -> String {
    if stdout.contains("Already up to date") || stdout.contains("Already up-to-date") {
        return "already up to date".to_owned();
    }
    let files = stdout
        .lines()
        .rev()
        .find(|l| l.contains("changed") && (l.contains("insertion") || l.contains("deletion")))
        .map(|l| l.trim().to_owned());
    match files {
        Some(f) => format!("fast-forwarded · {f}"),
        None => "fast-forwarded".to_owned(),
    }
}

/// Run a batch in one repository: checks, then the command.
pub fn run_one(
    batch: &Batch,
    name: &str,
    dir: &Path,
    timeout: Duration,
    commands: &mut Vec<String>,
) -> RepoResult {
    let started = Instant::now();
    let done = |outcome: Outcome, output: String| RepoResult {
        name: name.to_owned(),
        dir: dir.to_owned(),
        outcome,
        elapsed_ms: started.elapsed().as_millis() as u64,
        output,
    };
    if !dir.exists() {
        return done(Outcome::Skipped("folder not found".into()), String::new());
    }
    if !dir.join(".git").exists() {
        return done(
            Outcome::Skipped("not a git repository (no .git)".into()),
            String::new(),
        );
    }
    if *batch == Batch::Fetch {
        commands.push(format!(
            "{}    (in {})",
            fetch::command_line(),
            dir.display()
        ));
        let o = fetch::run(name, dir, timeout);
        let outcome = if o.ok {
            Outcome::Ok(o.changes.summary())
        } else {
            Outcome::Failed(error_line(&o.output))
        };
        let mut r = done(outcome, o.output);
        r.elapsed_ms = o.elapsed_ms;
        return r;
    }
    let status = match read_status(dir, commands) {
        Ok(s) => s,
        Err(e) => return done(Outcome::Failed(format!("status: {e}")), e.to_string()),
    };
    let (mut exists, mut base_ok) = (false, true);
    if let Batch::Branch { name: b, base } = batch {
        if let Some(p) = branch_name_problem(b) {
            return done(Outcome::Skipped(p.to_owned()), String::new());
        }
        exists = git_capture(
            dir,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{b}"),
            ],
            commands,
        )
        .is_ok();
        if let Some(base) = base.as_ref().filter(|x| !x.trim().is_empty()) {
            base_ok = git_capture(
                dir,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{}^{{commit}}", base.trim()),
                ],
                commands,
            )
            .is_ok();
        }
    }
    if let Some(why) = skip_reason(batch, &status, exists, base_ok) {
        return done(Outcome::Skipped(why), String::new());
    }
    let args = batch.args();
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    commands.push(format!(
        "git {}    (in {})",
        render_args(&refs),
        dir.display()
    ));
    let spec = ProcSpec::new("git", dir).args(refs.iter().copied());
    match run_capture(&spec, Some(timeout)) {
        Ok(c) if c.success() => {
            let out = c.stdout_text();
            let text = format!("{}\n{}", out.trim(), c.stderr.trim())
                .trim()
                .to_owned();
            let summary = match batch {
                Batch::Pull => pull_summary(&out),
                Batch::Branch { name, base } => {
                    match base.as_ref().filter(|b| !b.trim().is_empty()) {
                        Some(b) => format!("switched to new branch {name} from {}", b.trim()),
                        None => format!(
                            "switched to new branch {name} at {}",
                            status.head.as_deref().unwrap_or("HEAD")
                        ),
                    }
                }
                Batch::Fetch => String::new(),
            };
            done(Outcome::Ok(summary), text)
        }
        Ok(c) => {
            let e = c.error_text();
            done(Outcome::Failed(error_line(&e)), e)
        }
        Err(e) => done(Outcome::Failed(e.to_string()), e.to_string()),
    }
}

/// `Pull: 3 ok, 1 skipped, 1 failed`.
pub fn summary(batch: &Batch, results: &[RepoResult]) -> String {
    let n = |f: fn(&Outcome) -> bool| results.iter().filter(|r| f(&r.outcome)).count();
    let ok = n(|o| matches!(o, Outcome::Ok(_)));
    let skipped = n(|o| matches!(o, Outcome::Skipped(_)));
    let failed = n(|o| matches!(o, Outcome::Failed(_)));
    let mut parts = vec![format!("{ok} ok")];
    if skipped > 0 {
        parts.push(format!("{skipped} skipped"));
    }
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    format!(
        "{} in {} repositories: {}",
        batch.label(),
        results.len(),
        parts.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_serde_round_trip() {
        let g = vec![
            Group {
                name: "Work".into(),
                repos: vec![PathBuf::from("C:/src/a"), PathBuf::from("/home/me/b")],
            },
            Group::new("Empty"),
        ];
        let json = serde_json::to_string(&g).unwrap();
        let back: Vec<Group> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, g);
        // Unknown and missing fields are fine.
        let g: Group = serde_json::from_str(r#"{"name":"X","colour":"red"}"#).unwrap();
        assert_eq!(g, Group::new("X"));
    }

    #[test]
    fn add_and_names() {
        let mut g = Group::new("Work");
        assert!(g.add(PathBuf::from("C:/src/a")));
        assert!(!g.add(PathBuf::from("C:/src/a/")));
        if cfg!(windows) {
            assert!(!g.add(PathBuf::from("c:\\SRC\\a")));
        }
        assert_eq!(g.repos.len(), 1);
        let groups = vec![g];
        assert_eq!(name_problem(&groups, " ", None), Some("Type a name"));
        assert_eq!(
            name_problem(&groups, "work", None),
            Some("A group with this name exists")
        );
        assert_eq!(name_problem(&groups, "work", Some("Work")), None);
        assert_eq!(name_problem(&groups, "Play", None), None);
        assert!(find(&groups, " WORK ").is_some());
    }

    #[test]
    fn parses_porcelain_v2() {
        let out = "# branch.oid 1234567890abcdef1234567890abcdef12345678\n\
# branch.head feature/x\n\
# branch.upstream origin/feature/x\n\
# branch.ab +2 -3\n\
1 M. N... 100644 100644 100644 a b src/a.rs\n\
1 .M N... 100644 100644 100644 a b src/b.rs\n\
1 MM N... 100644 100644 100644 a b src/c.rs\n\
2 R. N... 100644 100644 100644 a b R100 new.rs\told.rs\n\
u UU N... 100644 100644 100644 100644 a b c conflict.rs\n\
? notes.txt\n\
! ignored.log\n";
        let s = parse_status(out);
        assert_eq!(s.branch.as_deref(), Some("feature/x"));
        assert_eq!(s.head.as_deref(), Some("1234567890"));
        assert_eq!(s.upstream.as_deref(), Some("origin/feature/x"));
        assert_eq!((s.ahead, s.behind), (2, 3));
        assert_eq!(
            (s.staged, s.unstaged, s.conflicted, s.untracked),
            (3, 2, 1, 1)
        );
        assert!(s.dirty() && s.diverged());
        let fresh = parse_status("# branch.oid (initial)\n# branch.head main\n");
        assert_eq!(fresh.head, None);
        assert_eq!(fresh.branch.as_deref(), Some("main"));
        let detached = parse_status("# branch.oid abc\n# branch.head (detached)\n");
        assert_eq!(detached.branch, None);
    }

    #[test]
    fn batch_commands() {
        assert_eq!(Batch::Fetch.command_line(), "git fetch --all --prune");
        assert_eq!(Batch::Pull.command_line(), "git pull --ff-only");
        let b = Batch::Branch {
            name: "release/1.2".into(),
            base: Some(" origin/main ".into()),
        };
        assert_eq!(b.command_line(), "git switch -c release/1.2 origin/main");
        let plain = Batch::Branch {
            name: "x".into(),
            base: Some("".into()),
        };
        assert_eq!(plain.command_line(), "git switch -c x");
        assert!(!plain.remote() && Batch::Pull.remote());
        assert_eq!(b.label(), "Create branch release/1.2");
        assert!(Batch::Pull.checks().iter().any(|c| c.contains("diverged")));
    }

    #[test]
    fn skip_rules() {
        let clean = RepoStatus {
            branch: Some("main".into()),
            upstream: Some("origin/main".into()),
            behind: 2,
            ..Default::default()
        };
        assert_eq!(skip_reason(&Batch::Pull, &clean, false, true), None);
        let dirty = RepoStatus {
            unstaged: 2,
            staged: 1,
            ..clean.clone()
        };
        assert_eq!(
            skip_reason(&Batch::Pull, &dirty, false, true).unwrap(),
            "uncommitted changes (1 staged, 2 changed)"
        );
        // Untracked files do not block a fast-forward.
        let untracked = RepoStatus {
            untracked: 4,
            ..clean.clone()
        };
        assert_eq!(skip_reason(&Batch::Pull, &untracked, false, true), None);
        let diverged = RepoStatus {
            ahead: 1,
            ..clean.clone()
        };
        assert_eq!(
            skip_reason(&Batch::Pull, &diverged, false, true).unwrap(),
            "diverged from origin/main: 1 ahead, 2 behind"
        );
        let no_up = RepoStatus {
            upstream: None,
            ..clean.clone()
        };
        assert_eq!(
            skip_reason(&Batch::Pull, &no_up, false, true).unwrap(),
            "main has no upstream"
        );
        let detached = RepoStatus {
            branch: None,
            ..clean.clone()
        };
        assert_eq!(
            skip_reason(&Batch::Pull, &detached, false, true).unwrap(),
            "detached HEAD"
        );
        let br = Batch::Branch {
            name: "x".into(),
            base: None,
        };
        assert!(
            skip_reason(&br, &dirty, false, true).is_none(),
            "keeps changes at HEAD"
        );
        assert!(
            skip_reason(&br, &clean, true, true)
                .unwrap()
                .contains("exists")
        );
        let based = Batch::Branch {
            name: "x".into(),
            base: Some("v1".into()),
        };
        assert_eq!(
            skip_reason(&based, &clean, false, false).unwrap(),
            "v1 is not known here"
        );
        assert!(skip_reason(&based, &dirty, false, true).is_some());
        assert_eq!(skip_reason(&Batch::Fetch, &dirty, true, false), None);
    }

    #[test]
    fn branch_names() {
        for ok in ["feature/x", "release-1.2", "ABC-123-login"] {
            assert_eq!(branch_name_problem(ok), None, "{ok}");
        }
        for bad in [
            "", "-x", "a b", "a..b", "a~1", "x.lock", "x/", "HEAD", "a:b", "a\\b",
        ] {
            assert!(branch_name_problem(bad).is_some(), "{bad}");
        }
    }

    #[test]
    fn summaries() {
        let r = |o: Outcome| RepoResult {
            name: "a".into(),
            dir: PathBuf::from("a"),
            outcome: o,
            elapsed_ms: 1,
            output: String::new(),
        };
        let results = vec![
            r(Outcome::Ok("x".into())),
            r(Outcome::Skipped("y".into())),
            r(Outcome::Failed("z".into())),
            r(Outcome::Ok("x".into())),
        ];
        assert_eq!(
            summary(&Batch::Pull, &results),
            "Pull in 4 repositories: 2 ok, 1 skipped, 1 failed"
        );
        assert_eq!(pull_summary("Already up to date.\n"), "already up to date");
        assert_eq!(
            error_line(
                "From /tmp/remote\n * branch main -> FETCH_HEAD\nhint: x\nfatal: Not possible to fast-forward, aborting.\n"
            ),
            "Not possible to fast-forward, aborting."
        );
        assert_eq!(error_line("a\nlast words\n"), "last words");
        assert_eq!(
            pull_summary(
                "Updating a..b\nFast-forward\n a.txt | 1 +\n 1 file changed, 1 insertion(+)\n"
            ),
            "fast-forwarded · 1 file changed, 1 insertion(+)"
        );
    }
}
