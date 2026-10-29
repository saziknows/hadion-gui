use std::collections::HashMap;
use std::path::PathBuf;

pub use git2::Oid;

use crate::graph::GraphLayout;

#[derive(Clone, Debug)]
pub struct Commit {
    pub id: Oid,
    pub short: String,
    pub parent_ids: Vec<Oid>,
    pub summary: String,
    pub author: String,
    pub email: String,
    /// Author time, seconds since the epoch.
    pub time: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RefKind {
    Local,
    Remote,
    Tag,
}

#[derive(Clone, Debug)]
pub struct RefLabel {
    /// Short name as displayed, e.g. `main`, `origin/main`, `v1.0`.
    pub name: String,
    /// Full ref name, e.g. `refs/heads/main`.
    pub full: String,
    pub kind: RefKind,
    /// The branch HEAD points at.
    pub is_head: bool,
}

#[derive(Clone, Debug)]
pub struct BranchInfo {
    pub name: String,
    pub full: String,
    pub target: Oid,
    pub is_head: bool,
    /// Short upstream name, e.g. `origin/main`.
    pub upstream: Option<String>,
    /// An upstream is configured but its ref no longer exists.
    pub upstream_gone: bool,
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Clone, Debug)]
pub struct RemoteBranch {
    pub remote: String,
    /// Branch name without the remote prefix.
    pub branch: String,
    /// `origin/feature`.
    pub name: String,
    pub full: String,
    pub target: Oid,
}

#[derive(Clone, Debug)]
pub struct Remote {
    pub name: String,
    pub url: String,
    pub branches: Vec<RemoteBranch>,
}

#[derive(Clone, Debug)]
pub struct TagInfo {
    pub name: String,
    pub full: String,
    /// Commit the tag points at (annotated tags are peeled).
    pub target: Oid,
}

#[derive(Clone, Debug)]
pub struct StashInfo {
    pub index: usize,
    pub message: String,
    pub id: Oid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Head {
    Branch {
        name: String,
        target: Oid,
    },
    Detached(Oid),
    /// A branch with no commits yet.
    Unborn(String),
}

impl Head {
    pub fn target(&self) -> Option<Oid> {
        match self {
            Head::Branch { target, .. } | Head::Detached(target) => Some(*target),
            Head::Unborn(_) => None,
        }
    }

    pub fn branch(&self) -> Option<&str> {
        match self {
            Head::Branch { name, .. } | Head::Unborn(name) => Some(name),
            Head::Detached(_) => None,
        }
    }
}

/// A multi-step operation the repository is in the middle of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pending {
    None,
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
    ApplyMailbox,
}

impl Pending {
    pub fn label(self) -> &'static str {
        match self {
            Pending::None => "",
            Pending::Merge => "Merge",
            Pending::Rebase => "Rebase",
            Pending::CherryPick => "Cherry-pick",
            Pending::Revert => "Revert",
            Pending::Bisect => "Bisect",
            Pending::ApplyMailbox => "Apply mailbox",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChange,
    Untracked,
    Conflicted,
}

impl ChangeKind {
    pub fn letter(self) -> &'static str {
        match self {
            ChangeKind::Added => "A",
            ChangeKind::Modified => "M",
            ChangeKind::Deleted => "D",
            ChangeKind::Renamed => "R",
            ChangeKind::Copied => "C",
            ChangeKind::TypeChange => "T",
            ChangeKind::Untracked => "U",
            ChangeKind::Conflicted => "!",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    /// Previous path for renames and copies.
    pub old_path: Option<String>,
    pub kind: ChangeKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkingTree {
    pub staged: Vec<FileEntry>,
    pub unstaged: Vec<FileEntry>,
    pub conflicted: Vec<FileEntry>,
}

impl WorkingTree {
    pub fn is_clean(&self) -> bool {
        self.staged.is_empty() && self.unstaged.is_empty() && self.conflicted.is_empty()
    }

    pub fn change_count(&self) -> usize {
        self.staged.len() + self.unstaged.len() + self.conflicted.len()
    }
}

#[derive(Clone, Debug)]
pub struct FileChange {
    pub path: String,
    pub old_path: Option<String>,
    pub kind: ChangeKind,
    pub additions: usize,
    pub deletions: usize,
    pub binary: bool,
}

#[derive(Clone, Debug)]
pub struct CommitDetails {
    pub id: Oid,
    pub tree: Oid,
    pub parents: Vec<Oid>,
    /// `GPG`, `SSH`, `X.509` or `signed` when the commit carries a signature.
    pub signature: Option<String>,
    pub author: String,
    pub author_email: String,
    pub author_time: i64,
    pub committer: String,
    pub committer_email: String,
    pub commit_time: i64,
    pub message: String,
    pub files: Vec<FileChange>,
    /// Stats were skipped because the commit is very large.
    pub stats_skipped: bool,
}

/// Which refs feed the graph.
#[derive(Clone, Debug, Default)]
pub struct GraphFilter {
    /// Full ref names to leave out.
    pub hidden: Vec<String>,
    /// Only draw history reachable from this full ref name (and HEAD).
    pub solo: Option<String>,
    pub hide_remotes: bool,
    pub hide_tags: bool,
}

#[derive(Clone, Debug)]
pub struct LoadOptions {
    pub max_commits: usize,
    pub filter: GraphFilter,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            max_commits: 50_000,
            filter: GraphFilter::default(),
        }
    }
}

/// Everything the UI needs to draw one repository.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub workdir: PathBuf,
    pub git_dir: PathBuf,
    pub name: String,
    pub head: Head,
    pub pending: Pending,
    pub commits: Vec<Commit>,
    /// Graph rows. When `wip_row` is true, row 0 is the uncommitted-changes
    /// row and commit `i` is drawn on row `i + 1`.
    pub graph: GraphLayout,
    pub wip_row: bool,
    pub refs: HashMap<Oid, Vec<RefLabel>>,
    pub index_of: HashMap<Oid, usize>,
    pub branches: Vec<BranchInfo>,
    pub remotes: Vec<Remote>,
    pub tags: Vec<TagInfo>,
    pub stashes: Vec<StashInfo>,
    pub status: WorkingTree,
    /// The commit limit was hit; older history is not shown.
    pub truncated: bool,
}

impl Snapshot {
    /// Graph row of a commit index.
    pub fn row_of_commit(&self, index: usize) -> usize {
        index + self.wip_row as usize
    }

    /// Commit index of a graph row, `None` for the WIP row.
    pub fn commit_of_row(&self, row: usize) -> Option<usize> {
        if self.wip_row {
            row.checked_sub(1)
        } else {
            Some(row)
        }
    }

    pub fn row_count(&self) -> usize {
        self.commits.len() + self.wip_row as usize
    }

    pub fn branch(&self, name: &str) -> Option<&BranchInfo> {
        self.branches.iter().find(|b| b.name == name)
    }

    pub fn current_branch(&self) -> Option<&BranchInfo> {
        self.branches.iter().find(|b| b.is_head)
    }

    pub fn remote_branch(&self, name: &str) -> Option<&RemoteBranch> {
        self.remotes
            .iter()
            .flat_map(|r| &r.branches)
            .find(|b| b.name == name)
    }
}
