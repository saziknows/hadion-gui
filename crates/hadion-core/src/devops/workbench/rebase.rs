//! Interactive rebase without an editor: the plan (one step per commit,
//! in the new order) becomes a todo file that `GIT_SEQUENCE_EDITOR` copies
//! over git's own, and new messages (reword, squash) are applied by
//! `exec git commit --amend -F FILE` lines, so no editor ever opens.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::shell::sh_path;
use super::{git, git_probe, git_raw, output_text};

/// What happens to one commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Step {
    #[default]
    Pick,
    Reword,
    Edit,
    Squash,
    Fixup,
    Drop,
}

impl Step {
    pub const ALL: [Step; 6] = [
        Step::Pick,
        Step::Reword,
        Step::Edit,
        Step::Squash,
        Step::Fixup,
        Step::Drop,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Step::Pick => "Pick",
            Step::Reword => "Reword",
            Step::Edit => "Edit",
            Step::Squash => "Squash",
            Step::Fixup => "Fixup",
            Step::Drop => "Drop",
        }
    }

    /// The keyboard shortcut (and git's one-letter command).
    pub fn key(self) -> char {
        match self {
            Step::Pick => 'p',
            Step::Reword => 'r',
            Step::Edit => 'e',
            Step::Squash => 's',
            Step::Fixup => 'f',
            Step::Drop => 'd',
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Step::Pick => "Keep the commit as it is",
            Step::Reword => "Keep the changes, change the message",
            Step::Edit => "Stop after this commit so you can amend it",
            Step::Squash => "Fold into the commit above and combine the messages",
            Step::Fixup => "Fold into the commit above and keep its message",
            Step::Drop => "Remove the commit and its changes",
        }
    }

    pub fn from_name(s: &str) -> Option<Step> {
        let s = s.trim().to_ascii_lowercase();
        Step::ALL
            .into_iter()
            .find(|st| s == st.label().to_ascii_lowercase() || s == st.key().to_string())
    }

    /// Folds into the previous commit.
    pub fn folds(self) -> bool {
        matches!(self, Step::Squash | Step::Fixup)
    }
}

/// A commit in the rebased range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebaseCommit {
    pub sha: String,
    pub short: String,
    pub subject: String,
    /// Full message, trailing whitespace trimmed.
    pub message: String,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub files: usize,
    pub added: u64,
    pub deleted: u64,
    /// On a remote-tracking branch already.
    pub pushed: bool,
}

/// One row of the plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub commit: RebaseCommit,
    pub step: Step,
    /// The new message of a reword (starts as the original).
    pub message: String,
    /// Position in the original order (oldest = 0).
    pub orig: usize,
}

/// A head commit (pick / reword / edit) and the commits folded into it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub head: usize,
    pub members: Vec<usize>,
}

/// The rebase plan, oldest commit first (the order of a todo file).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub rows: Vec<Row>,
    /// Combined messages of squash groups by head sha: the text and the
    /// default it started from (an untouched text follows the default when
    /// the group changes).
    pub squash_messages: HashMap<String, (String, String)>,
}

impl Plan {
    pub fn new(commits: Vec<RebaseCommit>) -> Plan {
        Plan {
            rows: commits
                .into_iter()
                .enumerate()
                .map(|(i, c)| Row {
                    message: c.message.clone(),
                    commit: c,
                    step: Step::Pick,
                    orig: i,
                })
                .collect(),
            squash_messages: HashMap::new(),
        }
    }

    pub fn set_step(&mut self, row: usize, step: Step) {
        if let Some(r) = self.rows.get_mut(row) {
            r.step = step;
        }
        self.sync_messages();
    }

    /// Move a row to another position (indices in the current order).
    pub fn move_row(&mut self, from: usize, to: usize) {
        if from >= self.rows.len() || from == to {
            return;
        }
        let r = self.rows.remove(from);
        let to = to.min(self.rows.len());
        self.rows.insert(to, r);
        self.sync_messages();
    }

    /// Every row picked in its original order.
    pub fn is_noop(&self) -> bool {
        self.rows
            .iter()
            .enumerate()
            .all(|(i, r)| r.step == Step::Pick && r.orig == i)
    }

    /// Groups of kept rows in order. A folding row with nothing before it
    /// forms its own (invalid) group.
    pub fn groups(&self) -> Vec<Group> {
        let mut out: Vec<Group> = Vec::new();
        for (i, r) in self.rows.iter().enumerate() {
            match r.step {
                Step::Drop => {}
                s if s.folds() && !out.is_empty() => {
                    out.last_mut().expect("non-empty").members.push(i)
                }
                _ => out.push(Group {
                    head: i,
                    members: Vec::new(),
                }),
            }
        }
        out
    }

    /// The group whose head is `row`.
    pub fn group_of_head(&self, row: usize) -> Option<Group> {
        self.groups().into_iter().find(|g| g.head == row)
    }

    fn head_message(&self, g: &Group) -> String {
        let h = &self.rows[g.head];
        if h.step == Step::Reword {
            h.message.clone()
        } else {
            h.commit.message.clone()
        }
    }

    /// A group with a squash member: its messages are combined.
    pub fn has_squash(&self, g: &Group) -> bool {
        g.members.iter().any(|&m| self.rows[m].step == Step::Squash)
    }

    /// The combined message git would offer: the head's message, then each
    /// squashed commit's (fixups contribute nothing).
    pub fn default_squash_message(&self, g: &Group) -> String {
        let mut s = self.head_message(g).trim_end().to_owned();
        for &m in &g.members {
            let r = &self.rows[m];
            if r.step == Step::Squash {
                s.push_str("\n\n");
                s.push_str(r.commit.message.trim_end());
            }
        }
        s
    }

    /// The message a group ends with, when Hadion has to set one.
    pub fn group_message(&self, g: &Group) -> Option<String> {
        if self.has_squash(g) {
            let sha = &self.rows[g.head].commit.sha;
            Some(match self.squash_messages.get(sha) {
                Some((text, base)) if text != base => text.clone(),
                _ => self.default_squash_message(g),
            })
        } else if self.rows[g.head].step == Step::Reword {
            Some(self.rows[g.head].message.clone())
        } else {
            None
        }
    }

    /// Keep untouched combined messages in step with their groups.
    pub fn sync_messages(&mut self) {
        let groups = self.groups();
        let mut live = HashSet::new();
        for g in &groups {
            if !self.has_squash(g) {
                continue;
            }
            let sha = self.rows[g.head].commit.sha.clone();
            let default = self.default_squash_message(g);
            live.insert(sha.clone());
            let e = self
                .squash_messages
                .entry(sha)
                .or_insert_with(|| (default.clone(), default.clone()));
            if e.0 == e.1 {
                *e = (default.clone(), default);
            }
        }
        self.squash_messages.retain(|k, _| live.contains(k));
    }

    /// The combined-message buffer of a squash group (for an editor).
    pub fn squash_buffer(&mut self, head: usize) -> Option<&mut String> {
        self.sync_messages();
        let sha = self.rows.get(head)?.commit.sha.clone();
        self.squash_messages.get_mut(&sha).map(|(t, _)| t)
    }

    /// Index of the first row whose commit gets a new id (everything
    /// before it is fast-forwarded by git and stays the same).
    pub fn first_rewritten(&self) -> Option<usize> {
        let i = self
            .rows
            .iter()
            .enumerate()
            .position(|(i, r)| r.step != Step::Pick || r.orig != i)?;
        // A commit folded into the one above changes that one too (the
        // rows above `i` are unchanged picks).
        Some(if self.rows[i].step.folds() && i > 0 {
            i - 1
        } else {
            i
        })
    }

    /// Commits whose id changes or which disappear.
    pub fn rewritten(&self) -> Vec<&RebaseCommit> {
        match self.first_rewritten() {
            Some(i) => self.rows[i..].iter().map(|r| &r.commit).collect(),
            None => Vec::new(),
        }
    }
}

/// What the repository is in the middle of, for validation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RepoCheck<'a> {
    /// `Some("merge")`, `Some("rebase")`, … when an operation is pending.
    pub pending: Option<&'a str>,
    pub conflicts: usize,
}

/// Why the plan cannot run (empty when it can).
pub fn validate(plan: &Plan, check: RepoCheck) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(p) = check.pending {
        out.push(format!(
            "A {p} is in progress. Finish or abort it before starting a rebase."
        ));
    }
    if check.conflicts > 0 {
        out.push(if check.conflicts == 1 {
            "1 file has conflicts. Resolve them first.".to_owned()
        } else {
            format!(
                "{} files have conflicts. Resolve them first.",
                check.conflicts
            )
        });
    }
    if plan.rows.is_empty() {
        out.push("There is nothing to rebase: no commits between the base and HEAD.".to_owned());
        return out;
    }
    if plan.rows.iter().all(|r| r.step == Step::Drop) {
        out.push("The plan drops every commit. Reset the branch to the base instead.".to_owned());
        return out;
    }
    if let Some(first) = plan.rows.iter().find(|r| r.step != Step::Drop)
        && first.step.folds()
    {
        out.push(format!(
            "The first commit ({}) cannot be a {}: there is no earlier commit in the plan to fold it into.",
            first.commit.short,
            first.step.label().to_lowercase()
        ));
    }
    for g in plan.groups() {
        let head = &plan.rows[g.head];
        if head.step.folds() {
            continue;
        }
        if let Some(m) = plan.group_message(&g)
            && m.trim().is_empty()
        {
            out.push(if plan.has_squash(&g) {
                format!(
                    "The combined message of the squash into {} is empty.",
                    head.commit.short
                )
            } else {
                format!("The new message of {} is empty.", head.commit.short)
            });
        }
    }
    if plan.is_noop() {
        out.push(
            "The plan changes nothing: every commit is picked in its original order.".to_owned(),
        );
    }
    out
}

/// The todo file and the message files it refers to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Todo {
    pub text: String,
    /// `(file name, content)` written next to the todo.
    pub messages: Vec<(String, String)>,
}

/// The amend line that sets a message from a file.
pub fn amend_line(msg_file: &Path) -> String {
    format!(
        "exec git commit --amend --allow-empty --no-verify -F {}",
        sh_path(msg_file)
    )
}

/// Build the todo for `plan`, with message files in `dir`. Squash rows
/// become `fixup` lines followed by an amend with the combined message,
/// rewords a `pick` followed by an amend, so git never opens an editor.
pub fn todo(plan: &Plan, dir: &Path) -> Todo {
    let groups = plan.groups();
    // Row index of the last row of each group → the group.
    let mut ends: HashMap<usize, &Group> = HashMap::new();
    for g in &groups {
        ends.insert(*g.members.last().unwrap_or(&g.head), g);
    }
    let mut text = String::new();
    let mut messages = Vec::new();
    for (i, r) in plan.rows.iter().enumerate() {
        let verb = match r.step {
            Step::Pick | Step::Reword => "pick",
            Step::Edit => "edit",
            Step::Squash | Step::Fixup => "fixup",
            Step::Drop => "drop",
        };
        text.push_str(&format!("{verb} {} {}\n", r.commit.short, r.commit.subject));
        if let Some(g) = ends.get(&i)
            && let Some(msg) = plan.group_message(g)
        {
            let name = format!("message-{}.txt", messages.len() + 1);
            text.push_str(&amend_line(&dir.join(&name)));
            text.push('\n');
            let mut content = msg.trim_end().to_owned();
            content.push('\n');
            messages.push((name, content));
        }
    }
    Todo { text, messages }
}

/// One commit of the resulting history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewCommit {
    pub subject: String,
    /// Short ids of the commits it is made of (head first).
    pub sources: Vec<String>,
    pub reworded: bool,
    /// Commits folded in.
    pub folded: usize,
    /// The rebase stops here for editing.
    pub stops: bool,
    /// Gets a new id.
    pub rewritten: bool,
}

/// The history after the rebase, oldest first.
pub fn preview(plan: &Plan) -> Vec<PreviewCommit> {
    let first = plan.first_rewritten().unwrap_or(usize::MAX);
    plan.groups()
        .into_iter()
        .map(|g| {
            let head = &plan.rows[g.head];
            let msg = plan
                .group_message(&g)
                .unwrap_or_else(|| head.commit.message.clone());
            let subject = msg.lines().next().unwrap_or("").trim().to_owned();
            let mut sources = vec![head.commit.short.clone()];
            sources.extend(g.members.iter().map(|&m| plan.rows[m].commit.short.clone()));
            PreviewCommit {
                subject,
                sources,
                reworded: plan.group_message(&g).is_some(),
                folded: g.members.len(),
                stops: head.step == Step::Edit,
                rewritten: g.head >= first,
            }
        })
        .collect()
}

/// A base the user can pick, with why it is offered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaseChoice {
    /// What goes into the base input (a ref name or a sha).
    pub rev: String,
    pub label: String,
    pub why: String,
}

fn count(dir: &Path, range: &str, cmds: &mut Vec<String>) -> Option<usize> {
    git_probe(dir, &["rev-list", "--count", range], cmds)?
        .parse()
        .ok()
}

/// Bases to offer, the best first: the upstream when HEAD has unpushed
/// commits, the merge base with the default branch, the latest tag,
/// `HEAD~5`.
pub fn suggestions(dir: &Path, cmds: &mut Vec<String>) -> Vec<BaseChoice> {
    let mut out: Vec<BaseChoice> = Vec::new();
    let head = git_probe(dir, &["rev-parse", "HEAD"], cmds);
    if let Some(up) = git_probe(
        dir,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
        cmds,
    ) && let Some(n) = count(dir, "@{u}..HEAD", cmds)
        && n > 0
    {
        out.push(BaseChoice {
            rev: up.clone(),
            label: up,
            why: format!(
                "Upstream: {n} commit{} not pushed yet",
                if n == 1 { "" } else { "s" }
            ),
        });
    }
    let current = git_probe(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"], cmds);
    let default = git_probe(
        dir,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
        cmds,
    )
    .or_else(|| {
        ["main", "master", "develop", "trunk"]
            .into_iter()
            .find(|b| {
                git_probe(
                    dir,
                    &[
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("refs/heads/{b}"),
                    ],
                    cmds,
                )
                .is_some()
            })
            .map(str::to_owned)
    });
    if let Some(def) = default
        && current.as_deref() != Some(def.as_str())
        && current.as_deref() != def.strip_prefix("origin/")
        && let Some(mb) = git_probe(dir, &["merge-base", "HEAD", &def], cmds)
        && Some(&mb) != head.as_ref()
        && let Some(n) = count(dir, &format!("{mb}..HEAD"), cmds)
        && n > 0
    {
        out.push(BaseChoice {
            rev: mb.clone(),
            label: format!("Merge base with {def}"),
            why: format!(
                "{n} commit{} since this branch left {def} ({})",
                if n == 1 { "" } else { "s" },
                &mb[..mb.len().min(10)]
            ),
        });
    }
    if let Some(tag) = git_probe(dir, &["describe", "--tags", "--abbrev=0"], cmds)
        && let Some(n) = count(dir, &format!("{tag}..HEAD"), cmds)
        && n > 0
    {
        out.push(BaseChoice {
            rev: tag.clone(),
            label: format!("Latest tag {tag}"),
            why: format!("{n} commit{} since {tag}", if n == 1 { "" } else { "s" }),
        });
    }
    if count(dir, "HEAD", cmds).is_some_and(|n| n > 5) {
        out.push(BaseChoice {
            rev: "HEAD~5".to_owned(),
            label: "HEAD~5".to_owned(),
            why: "The last 5 commits".to_owned(),
        });
    }
    out
}

/// The range to edit, read from the repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    /// The base as typed.
    pub base: String,
    pub base_sha: String,
    pub base_subject: String,
    pub head_sha: String,
    /// Oldest first; merges are left out (git flattens them too).
    pub commits: Vec<RebaseCommit>,
    pub merges_skipped: usize,
    /// Remote branches that contain the oldest commit of the range.
    pub remote_branches: Vec<String>,
    /// The base is not an ancestor of HEAD: the commits are replayed onto
    /// it (a plain rebase onto another line).
    pub not_ancestor: bool,
}

/// Most commits the editor accepts.
pub const MAX_COMMITS: usize = 400;

/// Parse `log --format=%x1e%H%x1f%h%x1f%an%x1f%ae%x1f%at%x1f%B%x1d
/// --numstat` output.
pub fn parse_log(text: &str) -> Vec<RebaseCommit> {
    let mut out = Vec::new();
    for rec in text.split('\u{1e}') {
        if rec.trim().is_empty() {
            continue;
        }
        let (head, stats) = match rec.find('\u{1d}') {
            Some(p) => (&rec[..p], &rec[p + 1..]),
            None => (rec, ""),
        };
        let f: Vec<&str> = head.splitn(6, '\u{1f}').collect();
        if f.len() < 6 {
            continue;
        }
        let message = f[5].trim_end().to_owned();
        let (mut files, mut added, mut deleted) = (0usize, 0u64, 0u64);
        for l in stats.lines() {
            let mut p = l.split('\t');
            let (Some(a), Some(d), Some(_)) = (p.next(), p.next(), p.next()) else {
                continue;
            };
            files += 1;
            added += a.parse::<u64>().unwrap_or(0);
            deleted += d.parse::<u64>().unwrap_or(0);
        }
        out.push(RebaseCommit {
            sha: f[0].trim().to_owned(),
            short: f[1].to_owned(),
            subject: message.lines().next().unwrap_or("").to_owned(),
            message,
            author: f[2].to_owned(),
            email: f[3].to_owned(),
            time: f[4].parse().unwrap_or(0),
            files,
            added,
            deleted,
            pushed: false,
        });
    }
    out
}

/// Read `base..HEAD` for the editor.
pub fn load(dir: &Path, base: &str, cmds: &mut Vec<String>) -> Result<Loaded, String> {
    let base = base.trim();
    if base.is_empty() {
        return Err("Enter a base: a branch, tag or commit.".to_owned());
    }
    let base_sha = git_probe(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{base}^{{commit}}"),
        ],
        cmds,
    )
    .ok_or_else(|| format!("\"{base}\" is not a commit, branch or tag of this repository."))?;
    let head_sha = git_probe(dir, &["rev-parse", "--verify", "HEAD"], cmds)
        .ok_or_else(|| "HEAD has no commit yet.".to_owned())?;
    let base_subject =
        git_probe(dir, &["log", "-1", "--format=%s", &base_sha], cmds).unwrap_or_default();
    let not_ancestor = !git_raw(
        dir,
        &["merge-base", "--is-ancestor", &base_sha, &head_sha],
        &[],
        cmds,
    )?
    .success;
    let range = format!("{base_sha}..{head_sha}");
    let n: usize = git_probe(dir, &["rev-list", "--count", "--no-merges", &range], cmds)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if n > MAX_COMMITS {
        return Err(format!(
            "{n} commits between {base} and HEAD: too many to edit one by one (at most {MAX_COMMITS}). Pick a closer base."
        ));
    }
    let text = git(
        dir,
        &[
            "-c",
            "core.quotepath=false",
            "log",
            "--reverse",
            "--no-merges",
            "--numstat",
            "--format=%x1e%H%x1f%h%x1f%an%x1f%ae%x1f%at%x1f%B%x1d",
            &range,
        ],
        cmds,
    )?;
    let mut commits = parse_log(&text);
    let merges_skipped = git_probe(dir, &["rev-list", "--count", "--merges", &range], cmds)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let has_remotes = git_probe(dir, &["remote"], cmds).is_some();
    let mut remote_branches = Vec::new();
    if has_remotes && !commits.is_empty() {
        let unpushed: HashSet<String> = git(dir, &["rev-list", &range, "--not", "--remotes"], cmds)
            .unwrap_or_default()
            .lines()
            .map(|l| l.trim().to_owned())
            .collect();
        for c in &mut commits {
            c.pushed = !unpushed.contains(&c.sha);
        }
        if commits[0].pushed {
            remote_branches = git(dir, &["branch", "-r", "--contains", &commits[0].sha], cmds)
                .unwrap_or_default()
                .lines()
                .map(|l| l.trim().to_owned())
                .filter(|l| !l.is_empty() && !l.contains(" -> "))
                .collect();
        }
    }
    Ok(Loaded {
        base: base.to_owned(),
        base_sha,
        base_subject,
        head_sha,
        commits,
        merges_skipped,
        remote_branches,
        not_ancestor,
    })
}

/// The command line Hadion runs, for the confirmation.
pub fn command(base_sha: &str, todo_path: &Path) -> Vec<String> {
    vec![
        format!("GIT_SEQUENCE_EDITOR=\"cp {}\"", sh_path(todo_path)),
        format!("git rebase -i --autostash --no-update-refs {base_sha}"),
    ]
}

/// A fresh directory for the todo and message files.
pub fn work_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("hadion-rebase-{}-{nanos}", std::process::id()))
}

/// How a rebase run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Done; the summary line.
    Done(String),
    /// Stopped at an `edit` (or a failing exec): the repository is
    /// mid-rebase, continue or abort from the banner.
    Stopped(String),
    /// Stopped on conflicts.
    Conflict(String),
}

/// Write the todo and messages into `work`, then run the rebase. Checks
/// that HEAD is still `expected_head` (the plan was made for it).
pub fn execute(
    dir: &Path,
    base_sha: &str,
    expected_head: &str,
    plan: &Plan,
    work: &Path,
    cmds: &mut Vec<String>,
) -> Result<Outcome, String> {
    let head = git_probe(dir, &["rev-parse", "HEAD"], cmds).unwrap_or_default();
    if head != expected_head {
        return Err(format!(
            "HEAD moved since the plan was loaded ({} → {}). Load the commits again.",
            &expected_head[..expected_head.len().min(10)],
            &head[..head.len().min(10)]
        ));
    }
    let todo = todo(plan, work);
    std::fs::create_dir_all(work)
        .map_err(|e| format!("Could not create {}: {e}", work.display()))?;
    let todo_path = work.join("git-rebase-todo");
    std::fs::write(&todo_path, &todo.text)
        .map_err(|e| format!("Could not write {}: {e}", todo_path.display()))?;
    for (name, content) in &todo.messages {
        std::fs::write(work.join(name), content)
            .map_err(|e| format!("Could not write {name}: {e}"))?;
    }
    let editor = format!("cp {}", sh_path(&todo_path));
    cmds.push(format!("GIT_SEQUENCE_EDITOR=\"{editor}\""));
    let out = git_raw(
        dir,
        &["rebase", "-i", "--autostash", "--no-update-refs", base_sha],
        &[
            ("GIT_SEQUENCE_EDITOR", editor.as_str()),
            ("GIT_EDITOR", "true"),
        ],
        cmds,
    )?;
    let text = output_text(&out);
    let state = git_probe(dir, &["rev-parse", "--git-path", "rebase-merge"], cmds).map(|p| {
        let p = PathBuf::from(p);
        if p.is_absolute() { p } else { dir.join(p) }
    });
    let in_progress = state.as_ref().is_some_and(|p| p.is_dir());
    if !in_progress {
        if out.success {
            let _ = std::fs::remove_dir_all(work);
            let kept = plan.groups().len();
            return Ok(Outcome::Done(format!(
                "Rebased onto {}: {} commit{} now ({} before)",
                &base_sha[..base_sha.len().min(10)],
                kept,
                if kept == 1 { "" } else { "s" },
                plan.rows.len()
            )));
        }
        return Err(if text.is_empty() {
            "git rebase failed".to_owned()
        } else {
            text
        });
    }
    let conflicts = git(dir, &["diff", "--name-only", "--diff-filter=U"], cmds)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    if conflicts > 0 {
        Ok(Outcome::Conflict(format!(
            "The rebase stopped on conflicts in {conflicts} file{}. Resolve them, then Continue, or Abort to go back.\n{text}",
            if conflicts == 1 { "" } else { "s" }
        )))
    } else {
        Ok(Outcome::Stopped(format!(
            "The rebase stopped so you can edit the commit. Amend it, then Continue.\n{text}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(n: usize) -> RebaseCommit {
        RebaseCommit {
            sha: format!("{n:0>40}"),
            short: format!("c{n}"),
            subject: format!("Commit {n}"),
            message: format!("Commit {n}\n\nBody {n}"),
            author: "A".into(),
            email: "a@x".into(),
            time: 0,
            files: 1,
            added: 1,
            deleted: 0,
            pushed: false,
        }
    }

    fn plan(n: usize) -> Plan {
        Plan::new((1..=n).map(commit).collect())
    }

    #[test]
    fn todo_for_a_mixed_plan() {
        let mut p = plan(5);
        // c1 pick, c3 moved above c2, c2 squashed into c3, c4 reworded, c5 dropped.
        p.move_row(2, 1);
        p.set_step(2, Step::Squash);
        p.set_step(3, Step::Reword);
        p.rows[3].message = "New four".into();
        p.set_step(4, Step::Drop);
        let dir = Path::new("/tmp/w");
        let t = todo(&p, dir);
        let lines: Vec<&str> = t.text.lines().collect();
        assert_eq!(
            lines,
            vec![
                "pick c1 Commit 1",
                "pick c3 Commit 3",
                "fixup c2 Commit 2",
                "exec git commit --amend --allow-empty --no-verify -F '/tmp/w/message-1.txt'",
                "pick c4 Commit 4",
                "exec git commit --amend --allow-empty --no-verify -F '/tmp/w/message-2.txt'",
                "drop c5 Commit 5",
            ]
        );
        assert_eq!(
            t.messages,
            vec![
                (
                    "message-1.txt".to_owned(),
                    "Commit 3\n\nBody 3\n\nCommit 2\n\nBody 2\n".to_owned()
                ),
                ("message-2.txt".to_owned(), "New four\n".to_owned()),
            ]
        );
    }

    #[test]
    fn fixups_need_no_message_and_edits_stop() {
        let mut p = plan(3);
        p.set_step(1, Step::Fixup);
        p.set_step(2, Step::Edit);
        let t = todo(&p, Path::new("/w"));
        assert_eq!(
            t.text,
            "pick c1 Commit 1\nfixup c2 Commit 2\nedit c3 Commit 3\n"
        );
        assert!(t.messages.is_empty());
        let pv = preview(&p);
        assert_eq!(pv.len(), 2);
        assert_eq!(pv[0].sources, vec!["c1", "c2"]);
        assert_eq!(pv[0].folded, 1);
        assert!(!pv[0].reworded);
        assert!(pv[1].stops);
        assert!(pv[0].rewritten, "c1 absorbs c2, so it gets a new id");
    }

    #[test]
    fn squash_message_follows_the_group_until_edited() {
        let mut p = plan(3);
        p.set_step(1, Step::Squash);
        let g = p.group_of_head(0).unwrap();
        assert_eq!(
            p.group_message(&g).unwrap(),
            "Commit 1\n\nBody 1\n\nCommit 2\n\nBody 2"
        );
        p.set_step(2, Step::Squash);
        let g = p.group_of_head(0).unwrap();
        assert!(p.group_message(&g).unwrap().ends_with("Commit 3\n\nBody 3"));
        *p.squash_buffer(0).unwrap() = "Hand written".into();
        p.set_step(2, Step::Pick);
        let g = p.group_of_head(0).unwrap();
        assert_eq!(p.group_message(&g).unwrap(), "Hand written");
        p.set_step(1, Step::Pick);
        assert!(p.squash_messages.is_empty(), "no squash group left");
    }

    #[test]
    fn validation() {
        let ok = RepoCheck::default();
        let mut p = plan(3);
        assert_eq!(
            validate(&p, ok),
            vec!["The plan changes nothing: every commit is picked in its original order."]
        );
        p.set_step(0, Step::Fixup);
        assert!(validate(&p, ok)[0].starts_with("The first commit (c1) cannot be a fixup"));
        p.set_step(0, Step::Drop);
        p.set_step(1, Step::Squash);
        assert!(
            validate(&p, ok)[0].starts_with("The first commit (c2) cannot be a squash"),
            "a drop above does not count"
        );
        for i in 0..3 {
            p.set_step(i, Step::Drop);
        }
        assert!(validate(&p, ok)[0].contains("drops every commit"));
        let mut r = plan(2);
        r.set_step(1, Step::Reword);
        r.rows[1].message = "  \n".into();
        assert_eq!(validate(&r, ok), vec!["The new message of c2 is empty."]);
        r.rows[1].message = "Fine".into();
        assert!(validate(&r, ok).is_empty());
        let busy = RepoCheck {
            pending: Some("merge"),
            conflicts: 2,
        };
        let v = validate(&r, busy);
        assert!(v[0].starts_with("A merge is in progress"));
        assert_eq!(v[1], "2 files have conflicts. Resolve them first.");
        assert!(validate(&Plan::default(), ok)[0].starts_with("There is nothing to rebase"));
    }

    #[test]
    fn rewritten_starts_at_the_first_change() {
        let mut p = plan(4);
        assert!(p.rewritten().is_empty());
        p.set_step(2, Step::Reword);
        let shas: Vec<&str> = p.rewritten().iter().map(|c| c.short.as_str()).collect();
        assert_eq!(shas, vec!["c3", "c4"]);
        p.set_step(2, Step::Pick);
        p.move_row(3, 1);
        assert_eq!(p.first_rewritten(), Some(1));
    }

    #[test]
    fn steps_by_name_and_key() {
        assert_eq!(Step::from_name("squash"), Some(Step::Squash));
        assert_eq!(Step::from_name("f"), Some(Step::Fixup));
        assert_eq!(Step::from_name("D"), Some(Step::Drop));
        assert_eq!(Step::from_name("x"), None);
    }

    #[test]
    fn parses_the_log_format() {
        let text = "\u{1e}aaaa\u{1f}aa\u{1f}Ann\u{1f}ann@x\u{1f}100\u{1f}Subject one\n\nBody\n\u{1d}\n3\t1\tsrc/a.rs\n-\t-\tbin.png\n\u{1e}bbbb\u{1f}bb\u{1f}Bob\u{1f}bob@x\u{1f}200\u{1f}Two\n\u{1d}\n";
        let c = parse_log(text);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].subject, "Subject one");
        assert_eq!(c[0].message, "Subject one\n\nBody");
        assert_eq!((c[0].files, c[0].added, c[0].deleted), (2, 3, 1));
        assert_eq!(c[1].files, 0);
        assert_eq!(c[1].time, 200);
    }
}
