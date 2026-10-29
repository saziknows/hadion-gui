//! Read-only repository access through libgit2.
//!
//! Everything here is safe to run on a background thread: each call opens
//! its own `git2::Repository`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use git2::{
    BranchType, Delta, DiffFindOptions, DiffFlags, DiffOptions, ErrorCode, Patch, Repository,
    RepositoryState, Sort, Status, StatusOptions,
};

use crate::graph;
use crate::model::*;
use crate::{Error, Result, cli};

/// Commits touching more files than this skip per-file line stats.
const STATS_FILE_LIMIT: usize = 2_000;

pub fn open(path: &Path) -> Result<Repository> {
    let repo = Repository::discover(path)?;
    if repo.is_bare() {
        return Err(Error::new("Bare repositories are not supported"));
    }
    Ok(repo)
}

/// Resolve any path inside a work tree to `(workdir, git_dir)`.
pub fn locate(path: &Path) -> Result<(PathBuf, PathBuf)> {
    let repo = open(path)?;
    let workdir = repo
        .workdir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| path.to_path_buf());
    Ok((workdir, repo.path().to_path_buf()))
}

/// Phase timings are printed to stderr when `HADION_TIMING` is set.
fn timing(label: &str, start: std::time::Instant) {
    if std::env::var_os("HADION_TIMING").is_some() {
        eprintln!("[timing] {label}: {:?}", start.elapsed());
    }
}

pub fn load(path: &Path, opts: &LoadOptions) -> Result<Snapshot> {
    let mut repo = open(path)?;
    let workdir = repo
        .workdir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| path.to_path_buf());
    let git_dir = repo.path().to_path_buf();
    let name = workdir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| workdir.display().to_string());

    let head = read_head(&repo);
    let pending = pending_of(repo.state());
    let t = std::time::Instant::now();
    let (branches, remotes, tags) = read_refs(&repo, &head)?;
    timing("refs", t);
    let stashes = read_stashes(&mut repo);
    let t = std::time::Instant::now();
    let status = read_status(&repo)?;
    timing("status", t);
    let t = std::time::Instant::now();
    let (commits, truncated) = walk(&repo, &head, &branches, &remotes, &tags, opts)?;
    timing("walk", t);
    let t = std::time::Instant::now();

    let index_of: HashMap<Oid, usize> =
        commits.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
    let head_index = head.target().and_then(|t| index_of.get(&t).copied());
    let wip_row = !status.is_clean();
    let offset = wip_row as u32;

    let mut parents: Vec<Vec<u32>> = Vec::with_capacity(commits.len() + 1);
    if wip_row {
        parents.push(head_index.map(|i| i as u32 + offset).into_iter().collect());
    }
    for c in &commits {
        parents.push(
            c.parent_ids
                .iter()
                .filter_map(|p| index_of.get(p).map(|&i| i as u32 + offset))
                .collect(),
        );
    }
    let graph = graph::layout(&parents, wip_row.then_some(0));

    let mut refs: HashMap<Oid, Vec<RefLabel>> = HashMap::new();
    let mut add = |target: Oid, label: RefLabel| refs.entry(target).or_default().push(label);
    for b in &branches {
        add(
            b.target,
            RefLabel {
                name: b.name.clone(),
                full: b.full.clone(),
                kind: RefKind::Local,
                is_head: b.is_head,
            },
        );
    }
    for rb in remotes.iter().flat_map(|r| &r.branches) {
        add(
            rb.target,
            RefLabel {
                name: rb.name.clone(),
                full: rb.full.clone(),
                kind: RefKind::Remote,
                is_head: false,
            },
        );
    }
    for t in &tags {
        add(
            t.target,
            RefLabel {
                name: t.name.clone(),
                full: t.full.clone(),
                kind: RefKind::Tag,
                is_head: false,
            },
        );
    }
    for labels in refs.values_mut() {
        labels.sort_by_key(|l| (!l.is_head, l.kind as u8));
    }
    timing("graph+index", t);

    Ok(Snapshot {
        workdir,
        git_dir,
        name,
        head,
        pending,
        commits,
        graph,
        wip_row,
        refs,
        index_of,
        branches,
        remotes,
        tags,
        stashes,
        status,
        truncated,
    })
}

/// Cheap refresh of just the working tree and in-progress operation.
pub fn load_status(path: &Path) -> Result<(WorkingTree, Pending)> {
    let repo = open(path)?;
    Ok((read_status(&repo)?, pending_of(repo.state())))
}

fn read_head(repo: &Repository) -> Head {
    match repo.head() {
        Ok(r) => {
            let target = r.target().unwrap_or(Oid::ZERO_SHA1);
            if repo.head_detached().unwrap_or(false) {
                Head::Detached(target)
            } else {
                let name = String::from_utf8_lossy(r.shorthand_bytes()).into_owned();
                Head::Branch { name, target }
            }
        }
        Err(e) if e.code() == ErrorCode::UnbornBranch || e.code() == ErrorCode::NotFound => {
            let name = repo
                .find_reference("HEAD")
                .ok()
                .and_then(|h| {
                    h.symbolic_target_bytes()
                        .map(|b| String::from_utf8_lossy(b).into_owned())
                })
                .map(|t| t.trim_start_matches("refs/heads/").to_owned())
                .unwrap_or_else(|| "main".to_owned());
            Head::Unborn(name)
        }
        Err(_) => Head::Unborn("HEAD".to_owned()),
    }
}

fn pending_of(state: RepositoryState) -> Pending {
    use RepositoryState as S;
    match state {
        S::Clean => Pending::None,
        S::Merge => Pending::Merge,
        S::Revert | S::RevertSequence => Pending::Revert,
        S::CherryPick | S::CherryPickSequence => Pending::CherryPick,
        S::Bisect => Pending::Bisect,
        S::Rebase | S::RebaseInteractive | S::RebaseMerge | S::ApplyMailboxOrRebase => {
            Pending::Rebase
        }
        S::ApplyMailbox => Pending::ApplyMailbox,
    }
}

fn read_refs(
    repo: &Repository,
    head: &Head,
) -> Result<(Vec<BranchInfo>, Vec<Remote>, Vec<TagInfo>)> {
    let mut branches = Vec::new();
    for item in repo.branches(Some(BranchType::Local))? {
        let Ok((branch, _)) = item else { continue };
        let reference = branch.get();
        let Some(target) = reference.target() else {
            continue;
        };
        let full = String::from_utf8_lossy(reference.name_bytes()).into_owned();
        let name = full.trim_start_matches("refs/heads/").to_owned();
        let is_head = matches!(head, Head::Branch { name: h, .. } if *h == name);

        let mut info = BranchInfo {
            name,
            full: full.clone(),
            target,
            is_head,
            upstream: None,
            upstream_gone: false,
            ahead: 0,
            behind: 0,
        };
        if let Ok(up_full) = repo.branch_upstream_name(&full) {
            let up_full = String::from_utf8_lossy(&up_full).into_owned();
            info.upstream = Some(short_ref(&up_full));
            match repo.find_reference(&up_full).ok().and_then(|r| r.target()) {
                Some(up) => {
                    if let Ok((a, b)) = repo.graph_ahead_behind(target, up) {
                        info.ahead = a;
                        info.behind = b;
                    }
                }
                None => info.upstream_gone = true,
            }
        }
        branches.push(info);
    }
    branches.sort_by_key(|a| a.name.to_lowercase());

    let remote_names: Vec<String> = repo
        .remotes()?
        .iter_bytes()
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .collect();
    let mut remotes: Vec<Remote> = remote_names
        .iter()
        .map(|name| Remote {
            name: name.clone(),
            url: repo
                .find_remote(name)
                .map(|r| String::from_utf8_lossy(r.url_bytes()).into_owned())
                .unwrap_or_default(),
            branches: Vec::new(),
        })
        .collect();
    for item in repo.branches(Some(BranchType::Remote))? {
        let Ok((branch, _)) = item else { continue };
        let reference = branch.get();
        // `origin/HEAD` is a symbolic pointer, not a branch.
        let Some(target) = reference.target() else {
            continue;
        };
        let full = String::from_utf8_lossy(reference.name_bytes()).into_owned();
        let short = full.trim_start_matches("refs/remotes/").to_owned();
        // Longest remote name that prefixes the ref (remote names may contain '/').
        let Some(remote) = remotes
            .iter_mut()
            .filter(|r| short.starts_with(&format!("{}/", r.name)))
            .max_by_key(|r| r.name.len())
        else {
            continue;
        };
        let branch_name = short[remote.name.len() + 1..].to_owned();
        if branch_name == "HEAD" {
            continue;
        }
        remote.branches.push(RemoteBranch {
            remote: remote.name.clone(),
            branch: branch_name,
            name: short,
            full,
            target,
        });
    }
    for r in &mut remotes {
        r.branches.sort_by_key(|a| a.branch.to_lowercase());
    }

    let mut tags = Vec::new();
    for reference in repo.references_glob("refs/tags/*")? {
        let Ok(reference) = reference else { continue };
        let Ok(commit) = reference.peel_to_commit() else {
            continue;
        };
        let full = String::from_utf8_lossy(reference.name_bytes()).into_owned();
        tags.push(TagInfo {
            name: full.trim_start_matches("refs/tags/").to_owned(),
            full,
            target: commit.id(),
        });
    }
    tags.sort_by(|a, b| natural_cmp(&b.name, &a.name));

    Ok((branches, remotes, tags))
}

fn short_ref(full: &str) -> String {
    for prefix in ["refs/heads/", "refs/remotes/", "refs/tags/"] {
        if let Some(s) = full.strip_prefix(prefix) {
            return s.to_owned();
        }
    }
    full.to_owned()
}

/// Order `v1.10` after `v1.9`.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    fn chunks(s: &str) -> Vec<(bool, String)> {
        let mut out: Vec<(bool, String)> = Vec::new();
        for ch in s.chars() {
            let digit = ch.is_ascii_digit();
            match out.last_mut() {
                Some((d, buf)) if *d == digit => buf.push(ch),
                _ => out.push((digit, ch.to_string())),
            }
        }
        out
    }
    let (ca, cb) = (chunks(a), chunks(b));
    for ((da, sa), (db, sb)) in ca.iter().zip(cb.iter()) {
        let ord = if *da && *db {
            sa.len().cmp(&sb.len()).then_with(|| sa.cmp(sb))
        } else {
            sa.to_lowercase().cmp(&sb.to_lowercase())
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    ca.len().cmp(&cb.len())
}

fn read_stashes(repo: &mut Repository) -> Vec<StashInfo> {
    let mut out = Vec::new();
    let _ = repo.stash_foreach(|index, message, id| {
        out.push(StashInfo {
            index,
            message: message.to_owned(),
            id: *id,
        });
        true
    });
    out
}

/// Working tree status. `git status` is much faster than libgit2 on large
/// trees (untracked cache, fsmonitor, parallel stat), so it is tried first.
fn read_status(repo: &Repository) -> Result<WorkingTree> {
    if let Some(workdir) = repo.workdir() {
        let args = [
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ];
        if let Ok(out) = cli::run(workdir, &args, None) {
            return Ok(parse_porcelain_v2(&out.stdout));
        }
    }
    read_status_libgit2(repo)
}

/// Parse `git status --porcelain=v2 -z` output.
fn parse_porcelain_v2(bytes: &[u8]) -> WorkingTree {
    let mut tree = WorkingTree::default();
    let mut fields = bytes.split(|&b| b == 0);
    while let Some(entry) = fields.next() {
        let Some(&kind) = entry.first() else { continue };
        let text = String::from_utf8_lossy(entry);
        match kind {
            b'?' => {
                if let Some(path) = text.strip_prefix("? ") {
                    tree.unstaged.push(FileEntry {
                        path: path.to_owned(),
                        old_path: None,
                        kind: ChangeKind::Untracked,
                    });
                }
            }
            b'1' | b'2' | b'u' => {
                // 1 XY sub mH mI mW hH hI <path>
                // 2 XY sub mH mI mW hH hI Xscore <path> NUL <orig path>
                // u XY sub m1 m2 m3 mW h1 h2 h3 <path>
                let pieces = match kind {
                    b'1' => 9,
                    b'2' => 10,
                    _ => 11,
                };
                let mut parts = text.splitn(pieces, ' ');
                let _tag = parts.next();
                let xy: Vec<char> = parts.next().unwrap_or("..").chars().collect();
                let Some(path) = parts.last() else { continue };
                let path = path.to_owned();
                let old_path = if kind == b'2' {
                    fields
                        .next()
                        .map(|p| String::from_utf8_lossy(p).into_owned())
                        .filter(|p| *p != path)
                } else {
                    None
                };
                if kind == b'u' {
                    tree.conflicted.push(FileEntry {
                        path,
                        old_path: None,
                        kind: ChangeKind::Conflicted,
                    });
                    continue;
                }
                let (x, y) = (
                    xy.first().copied().unwrap_or('.'),
                    xy.get(1).copied().unwrap_or('.'),
                );
                if let Some(kind) = status_kind(x) {
                    let old_path = old_path
                        .clone()
                        .filter(|_| matches!(kind, ChangeKind::Renamed | ChangeKind::Copied));
                    tree.staged.push(FileEntry {
                        path: path.clone(),
                        old_path,
                        kind,
                    });
                }
                if let Some(kind) = status_kind(y) {
                    tree.unstaged.push(FileEntry {
                        path,
                        old_path: None,
                        kind,
                    });
                }
            }
            _ => {}
        }
    }
    for list in [&mut tree.staged, &mut tree.unstaged, &mut tree.conflicted] {
        list.sort_by(|a, b| a.path.cmp(&b.path));
    }
    tree
}

fn status_kind(c: char) -> Option<ChangeKind> {
    Some(match c {
        'A' => ChangeKind::Added,
        'M' => ChangeKind::Modified,
        'D' => ChangeKind::Deleted,
        'R' => ChangeKind::Renamed,
        'C' => ChangeKind::Copied,
        'T' => ChangeKind::TypeChange,
        _ => return None,
    })
}

fn read_status_libgit2(repo: &Repository) -> Result<WorkingTree> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true)
        .include_ignored(false);
    let statuses = repo.statuses(Some(&mut opts))?;

    let mut tree = WorkingTree::default();
    for entry in statuses.iter() {
        let s = entry.status();
        let fallback = String::from_utf8_lossy(entry.path_bytes()).into_owned();
        if s.contains(Status::CONFLICTED) {
            tree.conflicted.push(FileEntry {
                path: fallback,
                old_path: None,
                kind: ChangeKind::Conflicted,
            });
            continue;
        }

        let index_kind = if s.contains(Status::INDEX_NEW) {
            Some(ChangeKind::Added)
        } else if s.contains(Status::INDEX_RENAMED) {
            Some(ChangeKind::Renamed)
        } else if s.contains(Status::INDEX_DELETED) {
            Some(ChangeKind::Deleted)
        } else if s.contains(Status::INDEX_TYPECHANGE) {
            Some(ChangeKind::TypeChange)
        } else if s.contains(Status::INDEX_MODIFIED) {
            Some(ChangeKind::Modified)
        } else {
            None
        };
        if let Some(kind) = index_kind {
            let delta = entry.head_to_index();
            let new = delta
                .as_ref()
                .and_then(|d| d.new_file().path().map(path_string));
            let old = delta
                .as_ref()
                .and_then(|d| d.old_file().path().map(path_string));
            let path = new.unwrap_or_else(|| fallback.clone());
            let old_path = old.filter(|o| kind == ChangeKind::Renamed && *o != path);
            tree.staged.push(FileEntry {
                path,
                old_path,
                kind,
            });
        }

        let wt_kind = if s.contains(Status::WT_NEW) {
            Some(ChangeKind::Untracked)
        } else if s.contains(Status::WT_DELETED) {
            Some(ChangeKind::Deleted)
        } else if s.contains(Status::WT_RENAMED) {
            Some(ChangeKind::Renamed)
        } else if s.contains(Status::WT_TYPECHANGE) {
            Some(ChangeKind::TypeChange)
        } else if s.contains(Status::WT_MODIFIED) {
            Some(ChangeKind::Modified)
        } else {
            None
        };
        if let Some(kind) = wt_kind {
            let delta = entry.index_to_workdir();
            let path = delta
                .as_ref()
                .and_then(|d| d.new_file().path().map(path_string))
                .unwrap_or(fallback);
            tree.unstaged.push(FileEntry {
                path,
                old_path: None,
                kind,
            });
        }
    }
    for list in [&mut tree.staged, &mut tree.unstaged, &mut tree.conflicted] {
        list.sort_by(|a, b| a.path.cmp(&b.path));
    }
    Ok(tree)
}

fn path_string(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn walk(
    repo: &Repository,
    head: &Head,
    branches: &[BranchInfo],
    remotes: &[Remote],
    tags: &[TagInfo],
    opts: &LoadOptions,
) -> Result<(Vec<Commit>, bool)> {
    let filter = &opts.filter;
    let visible = |full: &str| match &filter.solo {
        Some(solo) => full == solo,
        None => !filter.hidden.iter().any(|h| h == full),
    };

    let mut walk = repo.revwalk()?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
    let mut pushed = 0;
    let mut push = |oid: Oid| {
        if walk.push(oid).is_ok() {
            pushed += 1;
        }
    };
    for b in branches.iter().filter(|b| visible(&b.full)) {
        push(b.target);
    }
    if !filter.hide_remotes {
        for rb in remotes
            .iter()
            .flat_map(|r| &r.branches)
            .filter(|b| visible(&b.full))
        {
            push(rb.target);
        }
    }
    if !filter.hide_tags {
        for t in tags.iter().filter(|t| visible(&t.full)) {
            push(t.target);
        }
    }
    if let Some(t) = head.target() {
        push(t);
    }
    if pushed == 0 {
        return Ok((Vec::new(), false));
    }

    let mut commits = Vec::new();
    let mut truncated = false;
    for oid in walk {
        let Ok(oid) = oid else { continue };
        if commits.len() >= opts.max_commits {
            truncated = true;
            break;
        }
        let Ok(c) = repo.find_commit(oid) else {
            continue;
        };
        let author = c.author();
        commits.push(Commit {
            id: oid,
            short: short_id(oid),
            parent_ids: c.parent_ids().collect(),
            summary: c
                .summary_bytes()
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default(),
            author: String::from_utf8_lossy(author.name_bytes()).into_owned(),
            email: String::from_utf8_lossy(author.email_bytes()).into_owned(),
            time: author.when().seconds(),
        });
    }
    Ok((commits, truncated))
}

pub fn short_id(oid: Oid) -> String {
    let mut s = oid.to_string();
    s.truncate(7);
    s
}

pub fn commit_details(path: &Path, id: Oid) -> Result<CommitDetails> {
    let repo = open(path)?;
    let commit = repo.find_commit(id)?;
    let tree = commit.tree()?;
    let parent_tree = match commit.parent(0) {
        Ok(p) => Some(p.tree()?),
        Err(_) => None,
    };

    let mut dopts = DiffOptions::new();
    dopts.include_typechange(true);
    let mut diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut dopts))?;
    let mut fopts = DiffFindOptions::new();
    fopts.renames(true);
    diff.find_similar(Some(&mut fopts))?;

    let count = diff.deltas().len();
    let stats_skipped = count > STATS_FILE_LIMIT;
    let mut files = Vec::with_capacity(count);
    for (i, delta) in diff.deltas().enumerate() {
        let kind = match delta.status() {
            Delta::Added => ChangeKind::Added,
            Delta::Deleted => ChangeKind::Deleted,
            Delta::Renamed => ChangeKind::Renamed,
            Delta::Copied => ChangeKind::Copied,
            Delta::Typechange => ChangeKind::TypeChange,
            _ => ChangeKind::Modified,
        };
        let new = delta.new_file().path().map(path_string);
        let old = delta.old_file().path().map(path_string);
        let path = match kind {
            ChangeKind::Deleted => old.clone().or(new.clone()),
            _ => new.clone().or(old.clone()),
        }
        .unwrap_or_default();
        let old_path =
            old.filter(|o| matches!(kind, ChangeKind::Renamed | ChangeKind::Copied) && *o != path);

        let (mut additions, mut deletions) = (0, 0);
        let mut binary = delta.flags().contains(DiffFlags::BINARY);
        if !stats_skipped && let Ok(Some(patch)) = Patch::from_diff(&diff, i) {
            binary |= patch.delta().flags().contains(DiffFlags::BINARY);
            if let Ok((_, a, d)) = patch.line_stats() {
                additions = a;
                deletions = d;
            }
        }
        files.push(FileChange {
            path,
            old_path,
            kind,
            additions,
            deletions,
            binary,
        });
    }

    let author = commit.author();
    let committer = commit.committer();
    let signature = repo.extract_signature(&id, None).ok().map(|(sig, _)| {
        let head = String::from_utf8_lossy(&sig[..sig.len().min(40)]).into_owned();
        if head.contains("PGP") {
            "GPG".to_owned()
        } else if head.contains("SSH") {
            "SSH".to_owned()
        } else if head.contains("SIGNED MESSAGE") || head.contains("PKCS7") {
            "X.509".to_owned()
        } else {
            "signed".to_owned()
        }
    });
    Ok(CommitDetails {
        id,
        tree: tree.id(),
        parents: commit.parent_ids().collect(),
        signature,
        author: String::from_utf8_lossy(author.name_bytes()).into_owned(),
        author_email: String::from_utf8_lossy(author.email_bytes()).into_owned(),
        author_time: author.when().seconds(),
        committer: String::from_utf8_lossy(committer.name_bytes()).into_owned(),
        committer_email: String::from_utf8_lossy(committer.email_bytes()).into_owned(),
        commit_time: committer.when().seconds(),
        message: String::from_utf8_lossy(commit.message_bytes())
            .trim_end()
            .to_owned(),
        files,
        stats_skipped,
    })
}

/// Local and remote branches whose history includes `id`, via
/// `git branch -a --contains` (libgit2 has no fast equivalent).
pub fn branches_containing(workdir: &Path, id: Oid) -> Result<Vec<String>> {
    let out = cli::run(
        workdir,
        &[
            "branch",
            "-a",
            "--contains",
            &id.to_string(),
            "--format=%(refname:short)",
        ],
        None,
    )?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.ends_with("/HEAD"))
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn natural_ordering_of_versions() {
        assert_eq!(natural_cmp("v1.10", "v1.9"), Ordering::Greater);
        assert_eq!(natural_cmp("v2", "v10"), Ordering::Less);
        assert_eq!(natural_cmp("abc", "ABC"), Ordering::Equal);
    }

    #[test]
    fn porcelain_v2_parsing() {
        let out = [
            "1 .M N... 100644 100644 100644 abc def src/main.rs",
            "1 A. N... 000000 100644 100644 000 111 new.txt",
            "1 MM N... 100644 100644 100644 abc def both.rs",
            "2 R. N... 100644 100644 100644 abc def R100 dir/new name.rs",
            "old name.rs",
            "u UU N... 100644 100644 100644 100644 1 2 3 conflict.txt",
            "? untracked/file.md",
            "! ignored.log",
        ]
        .join("\0");
        let tree = parse_porcelain_v2(out.as_bytes());
        let names = |v: &[FileEntry]| {
            v.iter()
                .map(|e| (e.path.clone(), e.kind))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(&tree.staged),
            vec![
                ("both.rs".to_owned(), ChangeKind::Modified),
                ("dir/new name.rs".to_owned(), ChangeKind::Renamed),
                ("new.txt".to_owned(), ChangeKind::Added),
            ]
        );
        assert_eq!(tree.staged[1].old_path.as_deref(), Some("old name.rs"));
        assert_eq!(
            names(&tree.unstaged),
            vec![
                ("both.rs".to_owned(), ChangeKind::Modified),
                ("src/main.rs".to_owned(), ChangeKind::Modified),
                ("untracked/file.md".to_owned(), ChangeKind::Untracked),
            ]
        );
        assert_eq!(
            names(&tree.conflicted),
            vec![("conflict.txt".to_owned(), ChangeKind::Conflicted)]
        );
    }
}
