//! Which commit an image was built from.
//!
//! Registries and clusters only know tags (`sha-1a2b3c4`, `v1.4.0`, `main`,
//! `pr-123`, `latest`), digests and sometimes an OCI revision label. This
//! maps them onto the loaded history: a commit id (full, short, or embedded
//! as in `main-1a2b3c4` or git-describe's `-g1a2b3c4`) by prefix, a semver
//! tag to the git tag (with or without the `v`), a branch-name tag to the
//! branch tip (`feature-x` also for `feature/x`, as docker/metadata-action
//! writes it), `pr-N` to a pull request, and the moving tags to "floating".
//!
//! [`ahead_behind`] counts how far a commit is from a branch, walking the
//! loaded history (children come before parents in the snapshot).

use std::collections::HashMap;

use crate::{Commit, Oid, Snapshot};

/// What an image tag or label turned out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MatchKind {
    /// A commit id (`1a2b3c4`, `sha-1a2b3c4`, `main-1a2b3c4`) or an OCI
    /// revision label.
    Commit,
    /// A git tag (`v1.2.3`, or `1.2.3` for the tag `v1.2.3`).
    Tag,
    /// A branch tip (`main`; `feature-x` for the branch `feature/x`). It
    /// moves with each push, so the commit is the tip now, not at build time.
    Branch,
    /// `pr-123`: built from a pull request.
    PullRequest,
    /// `latest`, `edge`, `stable`, `nightly`: moves with every build.
    Floating,
}

impl MatchKind {
    pub fn label(self) -> &'static str {
        match self {
            MatchKind::Commit => "commit",
            MatchKind::Tag => "git tag",
            MatchKind::Branch => "branch",
            MatchKind::PullRequest => "pull request",
            MatchKind::Floating => "floating",
        }
    }
}

/// A tag or label matched to something in the repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefMatch {
    pub kind: MatchKind,
    /// The image tag or label text that matched.
    pub via: String,
    /// What it matched: the git tag or branch name, `#123`, the short id.
    pub name: String,
    /// The commit, when one is known (none for pull requests and floating
    /// tags).
    pub commit: Option<Oid>,
}

/// Tags that move with every build and say nothing about the commit.
pub const FLOATING: [&str; 8] = [
    "latest", "edge", "stable", "nightly", "dev", "canary", "beta", "alpha",
];

/// A branch known to the index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    /// `main`, or `origin/main` for a remote branch.
    pub name: String,
    pub target: Oid,
    pub remote: bool,
}

impl Branch {
    /// The name without the remote: `origin/feature/x` → `feature/x`.
    pub fn short(&self) -> &str {
        if self.remote {
            self.name.split_once('/').map_or(&self.name, |(_, b)| b)
        } else {
            &self.name
        }
    }
}

/// Commits, tags and branches of a snapshot, indexed for matching.
#[derive(Clone, Debug, Default)]
pub struct RefIndex {
    /// Lower-case hex ids, sorted (prefix search).
    ids: Vec<(String, Oid)>,
    /// Git tags, `(name, peeled target)`.
    tags: Vec<(String, Oid)>,
    /// Local branches (`main`) and remote branches (`origin/main`).
    branches: Vec<Branch>,
    /// What HEAD points at.
    pub head: Option<Oid>,
    /// The branch distances are measured against (`origin/main`, …), with
    /// its tip.
    pub base: Option<(String, Oid)>,
    /// The loaded history stops before the root commits (counts are lower
    /// bounds).
    pub truncated: bool,
}

impl RefIndex {
    /// Build from explicit lists (tests, other sources). `remote` names
    /// carry their remote (`origin/main`).
    pub fn new(
        commits: impl IntoIterator<Item = Oid>,
        tags: Vec<(String, Oid)>,
        local: Vec<(String, Oid)>,
        remote: Vec<(String, Oid)>,
        head: Option<Oid>,
        base: Option<(String, Oid)>,
    ) -> Self {
        let mut branches: Vec<Branch> = local
            .into_iter()
            .map(|(name, target)| Branch {
                name,
                target,
                remote: false,
            })
            .collect();
        branches.extend(remote.into_iter().map(|(name, target)| Branch {
            name,
            target,
            remote: true,
        }));
        let mut ids: Vec<(String, Oid)> = commits.into_iter().map(|o| (o.to_string(), o)).collect();
        ids.sort_by(|a, b| a.0.cmp(&b.0));
        ids.dedup_by(|a, b| a.0 == b.0);
        Self {
            ids,
            tags,
            branches,
            head,
            base,
            truncated: false,
        }
    }

    pub fn from_snapshot(s: &Snapshot) -> Self {
        let tags = s.tags.iter().map(|t| (t.name.clone(), t.target)).collect();
        let local = s
            .branches
            .iter()
            .map(|b| (b.name.clone(), b.target))
            .collect();
        let remote = s
            .remotes
            .iter()
            .flat_map(|r| &r.branches)
            .map(|b| (b.name.clone(), b.target))
            .collect();
        let head = s.head.target();
        let base = base_branch(s);
        let mut idx = Self::new(
            s.commits.iter().map(|c| c.id),
            tags,
            local,
            remote,
            head,
            base,
        );
        idx.truncated = s.truncated;
        idx
    }

    pub fn commit_count(&self) -> usize {
        self.ids.len()
    }

    /// The one loaded commit whose id starts with `hex` (at least 7 hex
    /// digits). `None` when there is none or more than one.
    pub fn commit_by_prefix(&self, hex: &str) -> Option<Oid> {
        let hex = hex.trim().to_ascii_lowercase();
        if !(7..=40).contains(&hex.len()) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let start = self.ids.partition_point(|(s, _)| s.as_str() < hex.as_str());
        let mut hits = self.ids[start..]
            .iter()
            .take_while(|(s, _)| s.starts_with(&hex));
        let first = hits.next()?;
        if hits.next().is_some() {
            return None;
        }
        Some(first.1)
    }

    /// A git tag by exact name.
    pub fn tag(&self, name: &str) -> Option<(&str, Oid)> {
        self.tags
            .iter()
            .find(|(n, _)| n == name)
            .map(|(n, o)| (n.as_str(), *o))
    }

    /// A branch by name: the local branch, else `origin/NAME`, else any
    /// remote's `NAME`, else a remote branch given with its remote
    /// (`origin/main`).
    pub fn branch(&self, name: &str) -> Option<(&str, Oid)> {
        self.find_branch(name).map(|b| (b.name.as_str(), b.target))
    }

    fn find_branch(&self, name: &str) -> Option<&Branch> {
        let origin = format!("origin/{name}");
        self.branches
            .iter()
            .find(|b| !b.remote && b.name == name)
            .or_else(|| self.branches.iter().find(|b| b.remote && b.name == origin))
            .or_else(|| self.branches.iter().find(|b| b.remote && b.short() == name))
            .or_else(|| self.branches.iter().find(|b| b.remote && b.name == name))
    }

    /// Match one image tag (or `app.kubernetes.io/version` value).
    pub fn match_tag(&self, tag: &str) -> Option<RefMatch> {
        let t = tag.trim();
        if t.is_empty() {
            return None;
        }
        let lower = t.to_ascii_lowercase();
        let mk = |kind, name: String, commit| {
            Some(RefMatch {
                kind,
                via: t.to_owned(),
                name,
                commit,
            })
        };
        // 1. A commit id, bare or with a conventional prefix.
        for prefix in ["", "sha-", "sha_", "git-", "commit-", "rev-", "g"] {
            if let Some(rest) = lower.strip_prefix(prefix)
                && let Some(id) = self.commit_by_prefix(rest)
            {
                return mk(MatchKind::Commit, short(id), Some(id));
            }
        }
        // 2. A git tag of the same name, or a semver with / without `v`.
        if let Some((n, id)) = self.tag(t) {
            return mk(MatchKind::Tag, n.to_owned(), Some(id));
        }
        if is_semver_like(t) {
            let alt = match t.strip_prefix(['v', 'V']) {
                Some(bare) => bare.to_owned(),
                None => format!("v{t}"),
            };
            if let Some((n, id)) = self.tag(&alt) {
                return mk(MatchKind::Tag, n.to_owned(), Some(id));
            }
            // `1.2` (docker/metadata-action's {{major}}.{{minor}}) floats to
            // the newest `v1.2.x`.
            if let Some((n, id)) = self.newest_tag_with_prefix(t) {
                return mk(MatchKind::Tag, n.to_owned(), Some(id));
            }
        }
        // 3. A branch tip (also `feature-x` for `feature/x`).
        if !FLOATING.contains(&lower.as_str()) {
            if let Some(b) = self.find_branch(t) {
                return mk(MatchKind::Branch, b.short().to_owned(), Some(b.target));
            }
            if let Some(b) = self.sanitized_branch(&lower) {
                return mk(MatchKind::Branch, b.short().to_owned(), Some(b.target));
            }
        }
        // 4. A pull request.
        if let Some(n) = pr_number(&lower) {
            return mk(MatchKind::PullRequest, format!("#{n}"), None);
        }
        // 5. A commit id inside the tag: `main-1a2b3c4`, `20261026-1a2b3c4`,
        //    git describe's `v1.2-3-g1a2b3c4`.
        for tok in lower.split(['-', '_', '.', '+']).rev() {
            let tok = if tok.len() == 8 && tok.starts_with('g') {
                &tok[1..]
            } else {
                tok
            };
            if tok.len() >= 7
                && tok.bytes().any(|b| b.is_ascii_alphabetic())
                && let Some(id) = self.commit_by_prefix(tok)
            {
                return mk(MatchKind::Commit, short(id), Some(id));
            }
        }
        for tok in lower.split(['-', '_', '.', '+']).rev() {
            if tok.len() >= 7
                && let Some(id) = self.commit_by_prefix(tok)
            {
                return mk(MatchKind::Commit, short(id), Some(id));
            }
        }
        if FLOATING.contains(&lower.as_str()) {
            return mk(MatchKind::Floating, t.to_owned(), None);
        }
        None
    }

    /// An OCI `org.opencontainers.image.revision` value: a commit id, or
    /// whatever `match_tag` makes of it.
    pub fn match_revision(&self, rev: &str) -> Option<RefMatch> {
        let r = rev.trim();
        if let Some(id) = self.commit_by_prefix(r) {
            return Some(RefMatch {
                kind: MatchKind::Commit,
                via: r.to_owned(),
                name: short(id),
                commit: Some(id),
            });
        }
        self.match_tag(r)
    }

    /// The most telling match of several tags: a commit beats a git tag,
    /// which beats a branch, a pull request, then a floating tag.
    pub fn best<S: AsRef<str>>(&self, tags: &[S]) -> Option<RefMatch> {
        tags.iter()
            .filter_map(|t| self.match_tag(t.as_ref()))
            .min_by_key(|m| m.kind)
    }

    /// Every tag's match (unknown tags are left out).
    pub fn all<S: AsRef<str>>(&self, tags: &[S]) -> Vec<RefMatch> {
        tags.iter()
            .filter_map(|t| self.match_tag(t.as_ref()))
            .collect()
    }

    fn sanitized_branch(&self, lower_tag: &str) -> Option<&Branch> {
        // Local branches first, then remote ones.
        let mut sorted: Vec<&Branch> = self.branches.iter().collect();
        sorted.sort_by_key(|b| b.remote);
        sorted.into_iter().find(|b| {
            let n = b.short().to_ascii_lowercase();
            n.contains('/') && sanitize(&n) == lower_tag
        })
    }

    fn newest_tag_with_prefix(&self, t: &str) -> Option<(&str, Oid)> {
        let bare = t.trim_start_matches(['v', 'V']);
        if bare.split('.').count() >= 3 {
            return None;
        }
        let want = format!("{bare}.");
        self.tags
            .iter()
            .filter(|(n, _)| n.trim_start_matches(['v', 'V']).starts_with(&want))
            .filter(|(n, _)| !n.contains('-'))
            .max_by(|a, b| version_cmp(&a.0, &b.0))
            .map(|(n, o)| (n.as_str(), *o))
    }
}

/// How docker/metadata-action turns a branch into a tag: `/` and other
/// characters a tag cannot hold become `-`.
pub fn sanitize(branch: &str) -> String {
    branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// `pr-123`, `pr123`, `pr_123`, `pull-123` → 123.
pub fn pr_number(lower: &str) -> Option<u64> {
    for p in ["pr-", "pr_", "pull-", "pr"] {
        if let Some(rest) = lower.strip_prefix(p)
            && !rest.is_empty()
            && rest.bytes().all(|b| b.is_ascii_digit())
        {
            return rest.parse().ok();
        }
    }
    None
}

/// `v1`, `1.2`, `v1.2.3`, `1.2.3-rc.1`, `1.2.3+build`.
pub fn is_semver_like(t: &str) -> bool {
    let bare = t.strip_prefix(['v', 'V']).unwrap_or(t);
    let core = bare.split(['-', '+']).next().unwrap_or("");
    let parts: Vec<&str> = core.split('.').collect();
    !core.is_empty()
        && (1..=4).contains(&parts.len())
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        && (parts.len() > 1 || t.starts_with(['v', 'V']))
}

/// Compare dotted versions numerically (`v1.10.0` > `v1.9.3`).
pub fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let nums = |s: &str| -> Vec<u64> {
        s.trim_start_matches(['v', 'V'])
            .split(['-', '+'])
            .next()
            .unwrap_or("")
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    nums(a).cmp(&nums(b))
}

fn short(id: Oid) -> String {
    let mut s = id.to_string();
    s.truncate(7);
    s
}

/// The branch to measure "behind" against: the remote's default branch
/// (`origin/main`, `origin/master`, …), else HEAD's upstream, else a local
/// `main` / `master`.
pub fn base_branch(s: &Snapshot) -> Option<(String, Oid)> {
    for name in [
        "origin/main",
        "origin/master",
        "origin/trunk",
        "origin/develop",
        "upstream/main",
        "upstream/master",
    ] {
        if let Some(b) = s.remote_branch(name) {
            return Some((b.name.clone(), b.target));
        }
    }
    if let Some(up) = s.current_branch().and_then(|b| b.upstream.clone())
        && let Some(b) = s.remote_branch(&up)
    {
        return Some((b.name.clone(), b.target));
    }
    for name in ["main", "master"] {
        if let Some(b) = s.branch(name) {
            return Some((b.name.clone(), b.target));
        }
    }
    None
}

/// `(only a, only b)`: commits reachable from `a` but not `b`, and from `b`
/// but not `a`, within the loaded history (`git rev-list --left-right
/// --count a...b`). `None` when either commit is not loaded. `commits` must
/// list children before parents, as the snapshot does.
pub fn ahead_behind(
    commits: &[Commit],
    index_of: &HashMap<Oid, usize>,
    a: Oid,
    b: Oid,
) -> Option<(usize, usize)> {
    const A: u8 = 1;
    const B: u8 = 2;
    let ia = *index_of.get(&a)?;
    let ib = *index_of.get(&b)?;
    if ia == ib {
        return Some((0, 0));
    }
    let mut flags = vec![0u8; commits.len()];
    flags[ia] |= A;
    flags[ib] |= B;
    // Unprocessed commits carrying exactly one flag.
    let mut live = 2usize;
    let (mut only_a, mut only_b) = (0usize, 0usize);
    for i in ia.min(ib)..commits.len() {
        let f = flags[i];
        if f == 0 {
            continue;
        }
        if f != A | B {
            live -= 1;
            if f == A {
                only_a += 1;
            } else {
                only_b += 1;
            }
        }
        for p in &commits[i].parent_ids {
            let Some(&pi) = index_of.get(p) else { continue };
            if pi <= i {
                continue; // not topological; ignore rather than loop
            }
            let before = flags[pi];
            let after = before | f;
            if after == before {
                continue;
            }
            let single = |x: u8| x == A || x == B;
            match (single(before), single(after)) {
                (false, true) => live += 1,
                (true, false) => live -= 1,
                _ => {}
            }
            flags[pi] = after;
        }
        if live == 0 {
            break;
        }
    }
    Some((only_a, only_b))
}

/// How a commit relates to HEAD and the base branch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Distance {
    /// Commits HEAD has that the image does not (and the reverse).
    pub head: Option<(usize, usize)>,
    /// `(base name, behind, ahead)`: commits the base branch has that the
    /// image does not, and commits only the image has.
    pub base: Option<(String, usize, usize)>,
}

impl Distance {
    /// `3 behind origin/main`, `on origin/main`, `2 ahead, 5 behind
    /// origin/main`; falls back to HEAD.
    pub fn text(&self) -> String {
        if let Some((name, behind, ahead)) = &self.base {
            return relation(*behind, *ahead, name);
        }
        if let Some((behind, ahead)) = self.head {
            return relation(behind, ahead, "HEAD");
        }
        String::new()
    }

    pub fn behind(&self) -> Option<usize> {
        self.base
            .as_ref()
            .map(|(_, b, _)| *b)
            .or(self.head.map(|(b, _)| b))
    }
}

/// `on main`, `3 behind main`, `2 ahead of main`, `2 ahead, 3 behind main`.
pub fn relation(behind: usize, ahead: usize, name: &str) -> String {
    let n = |x: usize| {
        if x == 1 {
            "1 commit".to_owned()
        } else {
            format!("{x} commits")
        }
    };
    match (behind, ahead) {
        (0, 0) => format!("at the tip of {name}"),
        (b, 0) => format!("{} behind {name}", n(b)),
        (0, a) => format!("{} ahead of {name}", n(a)),
        (b, a) => format!("{} ahead, {} behind {name}", n(a), n(b)),
    }
}

/// Distances of `id` to HEAD and to the base branch in a snapshot.
pub fn distance(s: &Snapshot, idx: &RefIndex, id: Oid) -> Distance {
    let head = idx
        .head
        .and_then(|h| ahead_behind(&s.commits, &s.index_of, h, id));
    let base = idx.base.as_ref().and_then(|(name, tip)| {
        ahead_behind(&s.commits, &s.index_of, *tip, id).map(|(b, a)| (name.clone(), b, a))
    });
    Distance { head, base }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(n: u8) -> Oid {
        let mut b = [0u8; 20];
        b[0] = n;
        b[19] = n;
        Oid::from_bytes(&b).unwrap()
    }

    fn commit(n: u8, parents: &[u8]) -> Commit {
        Commit {
            id: oid(n),
            short: String::new(),
            parent_ids: parents.iter().map(|p| oid(*p)).collect(),
            summary: String::new(),
            author: String::new(),
            email: String::new(),
            time: 0,
        }
    }

    fn hex(n: u8) -> String {
        oid(n).to_string()
    }

    fn index() -> RefIndex {
        // ab..., cd... style ids: oid(0xab) = "ab0000…00ab"
        RefIndex::new(
            [oid(0xab), oid(0xcd), oid(0x12), oid(0xef)],
            vec![
                ("v1.2.3".into(), oid(0xcd)),
                ("v1.2.4".into(), oid(0xef)),
                ("v1.10.0".into(), oid(0x12)),
                ("release-7".into(), oid(0x12)),
            ],
            vec![
                ("main".into(), oid(0xab)),
                ("feature/login".into(), oid(0x12)),
            ],
            vec![
                ("origin/main".into(), oid(0xef)),
                ("origin/hotfix/db".into(), oid(0xcd)),
            ],
            Some(oid(0xab)),
            Some(("origin/main".into(), oid(0xef))),
        )
    }

    #[test]
    fn commit_ids_match_by_prefix() {
        let idx = index();
        let ab = hex(0xab);
        assert_eq!(idx.commit_by_prefix(&ab[..7]), Some(oid(0xab)));
        assert_eq!(idx.commit_by_prefix(&ab), Some(oid(0xab)));
        assert_eq!(
            idx.commit_by_prefix(&ab[..7].to_uppercase()),
            Some(oid(0xab))
        );
        assert_eq!(idx.commit_by_prefix(&ab[..6]), None, "too short");
        assert_eq!(idx.commit_by_prefix("zzzzzzz"), None);
        let m = idx.match_tag(&format!("sha-{}", &ab[..7])).unwrap();
        assert_eq!(m.kind, MatchKind::Commit);
        assert_eq!(m.commit, Some(oid(0xab)));
        assert_eq!(m.name, ab[..7]);
        let m = idx.match_tag(&format!("main-{}", &ab[..8])).unwrap();
        assert_eq!((m.kind, m.commit), (MatchKind::Commit, Some(oid(0xab))));
        let m = idx
            .match_tag(&format!("v1.2-3-g{}", &hex(0xcd)[..7]))
            .unwrap();
        assert_eq!((m.kind, m.commit), (MatchKind::Commit, Some(oid(0xcd))));
    }

    #[test]
    fn ambiguous_prefixes_do_not_match() {
        let mut a = [0u8; 20];
        a[0] = 0xaa;
        let mut b = a;
        b[19] = 1;
        let idx = RefIndex::new(
            [Oid::from_bytes(&a).unwrap(), Oid::from_bytes(&b).unwrap()],
            vec![],
            vec![],
            vec![],
            None,
            None,
        );
        assert_eq!(idx.commit_by_prefix("aa00000"), None);
        assert!(
            idx.commit_by_prefix(&Oid::from_bytes(&b).unwrap().to_string())
                .is_some()
        );
    }

    #[test]
    fn semver_and_git_tags() {
        let idx = index();
        let m = idx.match_tag("v1.2.3").unwrap();
        assert_eq!((m.kind, m.name.as_str()), (MatchKind::Tag, "v1.2.3"));
        let m = idx.match_tag("1.2.3").unwrap();
        assert_eq!((m.kind, m.commit), (MatchKind::Tag, Some(oid(0xcd))));
        // {{major}}.{{minor}} floats to the newest patch.
        let m = idx.match_tag("1.2").unwrap();
        assert_eq!((m.name.as_str(), m.commit), ("v1.2.4", Some(oid(0xef))));
        let m = idx.match_tag("v1").unwrap();
        assert_eq!(m.name, "v1.10.0", "numeric, not lexical, order");
        assert_eq!(idx.match_tag("release-7").unwrap().kind, MatchKind::Tag);
        assert!(idx.match_tag("2.0.0").is_none());
    }

    #[test]
    fn branches_pull_requests_and_floating_tags() {
        let idx = index();
        let m = idx.match_tag("main").unwrap();
        assert_eq!((m.kind, m.commit), (MatchKind::Branch, Some(oid(0xab))));
        let m = idx.match_tag("feature-login").unwrap();
        assert_eq!(
            (m.kind, m.name.as_str()),
            (MatchKind::Branch, "feature/login")
        );
        let m = idx.match_tag("hotfix-db").unwrap();
        assert_eq!((m.kind, m.name.as_str()), (MatchKind::Branch, "hotfix/db"));
        assert_eq!(m.commit, Some(oid(0xcd)));
        let m = idx.match_tag("pr-123").unwrap();
        assert_eq!((m.kind, m.name.as_str()), (MatchKind::PullRequest, "#123"));
        assert_eq!(m.commit, None);
        assert_eq!(idx.match_tag("latest").unwrap().kind, MatchKind::Floating);
        assert!(idx.match_tag("something-else").is_none());
        assert!(idx.match_tag("").is_none());
    }

    #[test]
    fn best_prefers_commits_over_moving_tags() {
        let idx = index();
        let sha = format!("sha-{}", &hex(0xcd)[..7]);
        let tags = ["latest", "main", sha.as_str(), "v1.2.3"];
        let m = idx.best(&tags).unwrap();
        assert_eq!((m.kind, m.commit), (MatchKind::Commit, Some(oid(0xcd))));
        assert_eq!(idx.all(&tags).len(), 4);
        let m = idx.best(&["latest", "main"]).unwrap();
        assert_eq!(m.kind, MatchKind::Branch);
        assert_eq!(idx.best::<&str>(&[]), None);
        let m = idx.match_revision(&hex(0xef)).unwrap();
        assert_eq!((m.kind, m.commit), (MatchKind::Commit, Some(oid(0xef))));
    }

    #[test]
    fn helpers() {
        assert_eq!(sanitize("feature/new login"), "feature-new-login");
        assert_eq!(pr_number("pr-42"), Some(42));
        assert_eq!(pr_number("pr42"), Some(42));
        assert_eq!(pr_number("prod"), None);
        assert!(is_semver_like("v1.2.3-rc.1"));
        assert!(is_semver_like("1.2"));
        assert!(!is_semver_like("1"));
        assert!(!is_semver_like("main"));
        assert_eq!(
            version_cmp("v1.10.0", "v1.9.3"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            relation(3, 0, "origin/main"),
            "3 commits behind origin/main"
        );
        assert_eq!(
            relation(1, 2, "main"),
            "2 commits ahead, 1 commit behind main"
        );
        assert_eq!(relation(0, 0, "main"), "at the tip of main");
    }

    /// ```text
    /// 1 (main)   2 (feature)
    /// |          |
    /// 3 -------- 4
    /// |
    /// 5 (merge base region)
    /// |
    /// 6
    /// ```
    #[test]
    fn ahead_behind_walks_to_the_merge_base() {
        // Topological: children first.
        let commits = vec![
            commit(1, &[3]),
            commit(2, &[4]),
            commit(3, &[5]),
            commit(4, &[5]),
            commit(5, &[6]),
            commit(6, &[]),
        ];
        let index_of: HashMap<Oid, usize> =
            commits.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
        // main (1) vs feature (2): main has 1, 3; feature has 2, 4.
        assert_eq!(
            ahead_behind(&commits, &index_of, oid(1), oid(2)),
            Some((2, 2))
        );
        // main vs an ancestor: 1 and 3 are ahead of 5.
        assert_eq!(
            ahead_behind(&commits, &index_of, oid(1), oid(5)),
            Some((2, 0))
        );
        assert_eq!(
            ahead_behind(&commits, &index_of, oid(6), oid(1)),
            Some((0, 3))
        );
        assert_eq!(
            ahead_behind(&commits, &index_of, oid(4), oid(4)),
            Some((0, 0))
        );
        assert_eq!(ahead_behind(&commits, &index_of, oid(1), oid(99)), None);
    }

    #[test]
    fn ahead_behind_with_a_merge() {
        // 1 merges 2 and 3; 2 and 3 fork from 4.
        let commits = vec![
            commit(1, &[2, 3]),
            commit(2, &[4]),
            commit(3, &[4]),
            commit(4, &[]),
        ];
        let index_of: HashMap<Oid, usize> =
            commits.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
        assert_eq!(
            ahead_behind(&commits, &index_of, oid(1), oid(3)),
            Some((2, 0))
        );
        let d = Distance {
            head: Some((2, 0)),
            base: None,
        };
        assert_eq!(d.text(), "2 commits behind HEAD");
        assert_eq!(d.behind(), Some(2));
    }
}
