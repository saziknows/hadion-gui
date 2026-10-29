//! Branch intelligence and the release view, from `for-each-ref`,
//! `rev-list`, `cherry` and `diff --name-only`.
//!
//! Every per-branch command names the branch by its full ref
//! (`refs/heads/test`): a short name can be ambiguous with a path.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

use super::{InsightsOptions, LogRequest, days_between, git, git_ok, git_stdin, git_z, stream_log};
use crate::Oid;

// ---------------------------------------------------------------------------
// Branches
// ---------------------------------------------------------------------------

/// A commit on a branch whose patch already exists upstream / on HEAD
/// (`git cherry` `-` lines).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Duplicate {
    pub id: Oid,
    pub summary: String,
    /// What it was compared against (`HEAD`, `origin/main`).
    pub equivalent_in: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchNote {
    /// Short name (`feature`, `origin/feature`).
    pub name: String,
    pub full: String,
    pub is_remote: bool,
    pub is_head: bool,
    pub tip: Oid,
    pub tip_time: i64,
    pub author: String,
    pub summary: String,
    /// Tip reachable from HEAD.
    pub merged: bool,
    pub upstream: Option<String>,
    /// Upstream configured but its ref is gone (`[gone]`).
    pub upstream_gone: bool,
    /// Ahead/behind the upstream, when configured.
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    /// Commits not reachable from HEAD (`rev-list --count branch ^HEAD`);
    /// computed for local branches and the newest remote ones.
    pub unique: Option<u32>,
    /// Older than `stale_days` and not merged.
    pub stale: bool,
    /// Files changed on both sides since the merge base with HEAD and
    /// still different between the tips: a merge would likely conflict
    /// here (an identical change on both sides merges cleanly). Local
    /// branches only.
    pub conflicts: Vec<String>,
    /// Files changed only on the branch since the merge base.
    pub changed_files: u32,
    pub duplicates: Vec<Duplicate>,
    /// `conflicts`/`duplicates` were computed.
    pub analysed: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Branches {
    pub local: Vec<BranchNote>,
    /// Remote-tracking branches, newest first (all of them; the expensive
    /// per-branch counts only for the newest 50).
    pub remote: Vec<BranchNote>,
    pub stale_days: u32,
    pub merged: usize,
    pub gone: usize,
    pub stale: usize,
    pub unmerged: usize,
    /// Branches whose names look like personal or throwaway work
    /// (`tmp`, `test`, `wip`, `old`, a user name prefix), not merged.
    pub throwaway: usize,
    /// `git remote prune` would delete this many remote-tracking refs
    /// (`git remote prune --dry-run`, parsed; None when offline/not run).
    pub prunable: Option<usize>,
    /// Current branch name, if any.
    pub head: Option<String>,
}

// ---------------------------------------------------------------------------
// Releases
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagNote {
    pub name: String,
    pub target: Oid,
    pub annotated: bool,
    pub signed: bool,
    /// Tagger date for annotated tags, else the commit date.
    pub time: i64,
    pub tagger: String,
    pub message: String,
    /// Target reachable from some branch.
    pub on_branch: bool,
    /// Commits between the previous tag (by time) and this one.
    pub commits_since_previous: Option<u32>,
    /// Parsed version, when the name is a version (`v1.2.3` → (1,2,3)).
    pub version: Option<(u64, u64, u64)>,
    pub prerelease: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bump {
    pub from: String,
    pub to: String,
    /// `major`, `minor`, `patch`, `other`.
    pub kind: &'static str,
    pub time: i64,
    pub days: u32,
    pub commits: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct Releases {
    /// Newest first.
    pub tags: Vec<TagNote>,
    pub annotated: usize,
    pub lightweight: usize,
    pub signed: usize,
    pub orphan: usize,
    /// Latest tag reachable from HEAD (`git describe --tags --abbrev=0`).
    pub last_tag: Option<String>,
    pub last_tag_time: Option<i64>,
    /// Commits on HEAD since `last_tag` (newest first, top 400):
    /// `(id, summary, author, time, conventional type)`.
    pub unreleased: Vec<(Oid, String, String, i64, Option<String>)>,
    pub unreleased_total: u32,
    /// `(type or "other", count)` desc.
    pub unreleased_by_type: Vec<(String, u32)>,
    /// Mean days between the newest 10 version tags.
    pub cadence_days: Option<f32>,
    /// Days since the last tag.
    pub since_last_days: Option<u32>,
    /// `semver`, `calver`, `mixed`, `none`.
    pub scheme: &'static str,
    /// Newest first, between consecutive version tags.
    pub bumps: Vec<Bump>,
    /// Suggested next version from the unreleased conventional types
    /// (`feat` → minor, `!`/BREAKING → major, else patch).
    pub suggested_next: Option<String>,
}

impl Default for Releases {
    fn default() -> Self {
        Self {
            tags: Vec::new(),
            annotated: 0,
            lightweight: 0,
            signed: 0,
            orphan: 0,
            last_tag: None,
            last_tag_time: None,
            unreleased: Vec::new(),
            unreleased_total: 0,
            unreleased_by_type: Vec::new(),
            cadence_days: None,
            since_last_days: None,
            scheme: "none",
            bumps: Vec::new(),
            suggested_next: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct Refs {
    pub branches: Branches,
    pub releases: Releases,
}

// ---------------------------------------------------------------------------
// Caps (brave-core: 1.8k remote branches, 12k tags, must stay well under
// 15 s; every per-ref command costs a process spawn)
// ---------------------------------------------------------------------------

/// Remote branches (newest first) that get a `rev-list --count`.
const REMOTE_UNIQUE_CAP: usize = 50;
/// Local branches (newest first) analysed for conflicts and duplicates.
const ANALYSED_CAP: usize = 30;
/// `git cherry` computes a patch id per commit on both sides: a branch
/// further away than this is no feature branch and would take seconds.
const CHERRY_MAX_UNIQUE: u32 = 1_000;
/// Duplicates kept per branch.
const DUPLICATES_CAP: usize = 50;
/// Above this many tags the branch-reachability set is not built and
/// every tag counts as on a branch.
const ON_BRANCH_MAX_TAGS: usize = 20_000;
/// Newest tags that get a `rev-list --count prev..tag`.
const SINCE_PREVIOUS_CAP: usize = 30;
/// Unreleased commits kept (the total is counted separately).
const UNRELEASED_CAP: usize = 400;
/// Version tags the cadence is averaged over.
const CADENCE_TAGS: usize = 10;
/// Version bumps kept (newest first).
const BUMPS_CAP: usize = 200;
/// Annotated tags inspected with `cat-file -p` when `for-each-ref` lacks
/// `%(contents:signature)`.
const SIGNATURE_FALLBACK_CAP: usize = 50;

const BRANCH_FORMAT: &str = "--format=%(refname)%1f%(refname:short)%1f%(objectname)%1f%(committerdate:unix)%1f%(authorname)%1f%(subject)%1f%(upstream)%1f%(upstream:short)%1f%(upstream:track)%1f%(HEAD)%1e";
const TAG_FORMAT: &str = "--format=%(refname)%1f%(refname:short)%1f%(objecttype)%1f%(objectname)%1f%(*objectname)%1f%(creatordate:unix)%1f%(taggername)%1f%(authorname)%1f%(contents:subject)%1f%(contents:signature)%1e";
const TAG_FORMAT_NO_SIGNATURE: &str = "--format=%(refname)%1f%(refname:short)%1f%(objecttype)%1f%(objectname)%1f%(*objectname)%1f%(creatordate:unix)%1f%(taggername)%1f%(authorname)%1f%(contents:subject)%1e";

/// Branch and tag intelligence. Never fails; missing data stays empty.
/// Checks `cancel` between per-branch commands.
pub fn analyze(
    workdir: &Path,
    opts: &InsightsOptions,
    cancel: &AtomicBool,
    commands: &mut Vec<String>,
) -> Refs {
    let now = now_epoch();
    let branches = analyze_branches(workdir, opts, now, cancel, commands);
    let releases = if cancel.load(Ordering::Relaxed) {
        Releases::default()
    } else {
        analyze_releases(workdir, now, cancel, commands)
    };
    Refs { branches, releases }
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Branches
// ---------------------------------------------------------------------------

/// A parsed `for-each-ref` record plus the upstream's full ref name, which
/// the commands use (the short name is only for display).
struct BranchRecord {
    note: BranchNote,
    upstream_full: Option<String>,
}

fn parse_branch(record: &str) -> Option<BranchRecord> {
    let record = record.trim_start_matches(['\n', '\r']);
    let f: Vec<&str> = record.split('\x1f').collect();
    if f.len() < 10 {
        return None;
    }
    let full = f[0].trim().to_owned();
    if full.is_empty() {
        return None;
    }
    let tip = Oid::from_str(f[2].trim()).ok()?;
    let upstream = (!f[7].is_empty()).then(|| f[7].to_owned());
    let (ahead, behind, upstream_gone) = parse_track(f[8], upstream.is_some());
    Some(BranchRecord {
        upstream_full: (!f[6].is_empty()).then(|| f[6].to_owned()),
        note: BranchNote {
            name: f[1].to_owned(),
            is_remote: full.starts_with("refs/remotes/"),
            is_head: f[9].trim() == "*",
            full,
            tip,
            tip_time: f[3].trim().parse().unwrap_or(0),
            author: f[4].to_owned(),
            summary: f[5].to_owned(),
            merged: false,
            upstream,
            upstream_gone,
            ahead,
            behind,
            unique: None,
            stale: false,
            conflicts: Vec::new(),
            changed_files: 0,
            duplicates: Vec::new(),
            analysed: false,
        },
    })
}

/// `%(upstream:track)`: `[ahead 2, behind 3]`, `[ahead 1]`, `[gone]` or
/// empty (in sync, or no upstream) → `(ahead, behind, gone)`.
fn parse_track(track: &str, has_upstream: bool) -> (Option<u32>, Option<u32>, bool) {
    let inner = track
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim();
    if inner == "gone" {
        return (None, None, true);
    }
    if !has_upstream {
        return (None, None, false);
    }
    let (mut ahead, mut behind) = (0u32, 0u32);
    for part in inner.split(',') {
        let mut words = part.split_whitespace();
        match (
            words.next(),
            words.next().and_then(|n| n.parse::<u32>().ok()),
        ) {
            (Some("ahead"), Some(n)) => ahead = n,
            (Some("behind"), Some(n)) => behind = n,
            _ => {}
        }
    }
    (Some(ahead), Some(behind), false)
}

/// `tmp`, `test-2`, `wip/foo`, `feature-old`, `origin/junk`: the prefix rule
/// on every path segment (a `wip/` folder counts), the suffix rule on the
/// last, lower-cased; `testimonials` is not a throwaway.
fn is_throwaway(name: &str) -> bool {
    static RES: OnceLock<(Regex, Regex)> = OnceLock::new();
    let (prefix, suffix) = RES.get_or_init(|| {
        (
            Regex::new(
                r"^(tmp|temp|test|testing|wip|old|backup|bak|scratch|junk|foo|bar|experiment|try|hack|delete-?me)(?:[-_.0-9]|$)",
            )
            .expect("valid regex"),
            Regex::new(r"[-_](tmp|old|bak|backup|wip)$").expect("valid regex"),
        )
    });
    let lower = name.to_lowercase();
    let last = lower.rsplit('/').next().unwrap_or(&lower);
    suffix.is_match(last) || lower.split('/').any(|segment| prefix.is_match(segment))
}

fn analyze_branches(
    workdir: &Path,
    opts: &InsightsOptions,
    now: i64,
    cancel: &AtomicBool,
    commands: &mut Vec<String>,
) -> Branches {
    let mut out = Branches {
        stale_days: opts.stale_days,
        prunable: None,
        ..Default::default()
    };
    out.head = git(
        workdir,
        &["symbolic-ref", "--short", "-q", "HEAD"],
        commands,
    )
    .ok()
    .map(|s| s.trim().to_owned())
    .filter(|s| !s.is_empty());

    let listing = git_ok(
        workdir,
        &["for-each-ref", BRANCH_FORMAT, "refs/heads", "refs/remotes"],
        commands,
    );
    let merged: HashSet<String> = git_ok(
        workdir,
        &[
            "for-each-ref",
            "--merged",
            "HEAD",
            "--format=%(refname)",
            "refs/heads",
            "refs/remotes",
        ],
        commands,
    )
    .lines()
    .map(|l| l.trim().to_owned())
    .filter(|l| !l.is_empty())
    .collect();
    let stale_before = now - i64::from(opts.stale_days) * 86_400;

    let mut local: Vec<BranchRecord> = Vec::new();
    let mut remote: Vec<BranchNote> = Vec::new();
    for record in listing.split('\x1e') {
        let Some(mut rec) = parse_branch(record) else {
            continue;
        };
        // `origin/HEAD` is a symbolic ref, not a branch.
        if rec.note.is_remote && rec.note.name.ends_with("/HEAD") {
            continue;
        }
        rec.note.merged = merged.contains(&rec.note.full);
        rec.note.stale = !rec.note.merged && rec.note.tip_time < stale_before;
        if rec.note.is_remote {
            remote.push(rec.note);
        } else {
            local.push(rec);
        }
    }
    local.sort_by(|a, b| {
        b.note
            .is_head
            .cmp(&a.note.is_head)
            .then(b.note.tip_time.cmp(&a.note.tip_time))
    });
    remote.sort_by_key(|n| Reverse(n.tip_time));

    // Commits not in HEAD: local branches and the newest remote ones.
    // Merged branches (and HEAD) have none by definition: no command.
    for note in local
        .iter_mut()
        .map(|r| &mut r.note)
        .chain(remote.iter_mut().take(REMOTE_UNIQUE_CAP))
    {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if note.is_head || note.merged {
            note.unique = Some(0);
            continue;
        }
        note.unique = git(
            workdir,
            &["rev-list", "--count", &note.full, "^HEAD"],
            commands,
        )
        .ok()
        .and_then(|s| s.trim().parse().ok());
    }

    // Conflict prediction and duplicate patches for local branches.
    let mut analysed = 0usize;
    for rec in local.iter_mut() {
        if analysed >= ANALYSED_CAP || cancel.load(Ordering::Relaxed) {
            break;
        }
        let note = &mut rec.note;
        if note.is_head {
            continue;
        }
        analysed += 1;
        note.analysed = true;
        if note.merged {
            // Nothing on the branch that HEAD lacks: no conflicts, and
            // every commit would be a "duplicate".
            continue;
        }
        if let Ok(base) = git(workdir, &["merge-base", "HEAD", &note.full], commands) {
            let base = base.trim().to_owned();
            if !base.is_empty() {
                let tip = note.tip.to_string();
                let ours: HashSet<String> = name_only(workdir, &base, "HEAD", commands)
                    .into_iter()
                    .collect();
                let theirs = name_only(workdir, &base, &tip, commands);
                note.changed_files = theirs.len() as u32;
                // Changed on both sides and still different between the
                // tips: an identical change (cherry-pick) merges cleanly.
                let differing: HashSet<String> = name_only(workdir, "HEAD", &tip, commands)
                    .into_iter()
                    .collect();
                let mut conflicts: Vec<String> = theirs
                    .into_iter()
                    .filter(|p| ours.contains(p) && differing.contains(p))
                    .collect();
                conflicts.sort();
                conflicts.dedup();
                note.conflicts = conflicts;
            }
        }
        if note.unique.is_none_or(|u| u <= CHERRY_MAX_UNIQUE) {
            cherry(
                workdir,
                "HEAD",
                &note.full,
                "HEAD",
                &mut note.duplicates,
                commands,
            );
            if let (Some(up_full), Some(up)) = (&rec.upstream_full, &note.upstream)
                && !note.upstream_gone
            {
                cherry(
                    workdir,
                    up_full,
                    &note.full,
                    up,
                    &mut note.duplicates,
                    commands,
                );
            }
        }
    }
    // One `git log` for every duplicate's summary.
    let wanted: Vec<String> = local
        .iter()
        .flat_map(|r| r.note.duplicates.iter().map(|d| d.id.to_string()))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if !wanted.is_empty() {
        let summaries = commit_summaries(workdir, &wanted, commands);
        for d in local.iter_mut().flat_map(|r| r.note.duplicates.iter_mut()) {
            if let Some(s) = summaries.get(&d.id) {
                d.summary = s.clone();
            }
        }
    }

    out.local = local.into_iter().map(|r| r.note).collect();
    out.remote = remote;
    let all = || out.local.iter().chain(out.remote.iter());
    out.merged = all().filter(|n| n.merged && !n.is_head).count();
    out.gone = all().filter(|n| n.upstream_gone).count();
    out.stale = all().filter(|n| n.stale).count();
    out.unmerged = all().filter(|n| !n.merged && !n.is_head).count();
    out.throwaway = all()
        .filter(|n| !n.merged && !n.is_head && is_throwaway(&n.name))
        .count();
    out
}

/// `git diff --name-only -z <from> <to> --`.
fn name_only(workdir: &Path, from: &str, to: &str, commands: &mut Vec<String>) -> Vec<String> {
    git_z(
        workdir,
        &["diff", "--name-only", "-z", from, to, "--"],
        commands,
    )
    .unwrap_or_default()
}

/// `git cherry <upstream> <branch>`: `-` lines are commits of the branch
/// whose patch already exists in `upstream`.
fn cherry(
    workdir: &Path,
    upstream_ref: &str,
    branch_ref: &str,
    label: &str,
    out: &mut Vec<Duplicate>,
    commands: &mut Vec<String>,
) {
    let text = git_ok(workdir, &["cherry", upstream_ref, branch_ref], commands);
    for line in text.lines() {
        if out.len() >= DUPLICATES_CAP {
            break;
        }
        let Some(sha) = line.strip_prefix("- ") else {
            continue;
        };
        let Ok(id) = Oid::from_str(sha.trim()) else {
            continue;
        };
        if out.iter().any(|d| d.id == id) {
            continue;
        }
        out.push(Duplicate {
            id,
            summary: String::new(),
            equivalent_in: label.to_owned(),
        });
    }
}

/// Summaries of the given commits through one `git log --no-walk --stdin`.
fn commit_summaries(
    workdir: &Path,
    shas: &[String],
    commands: &mut Vec<String>,
) -> HashMap<Oid, String> {
    let stdin = shas.join("\n") + "\n";
    let Ok(out) = git_stdin(
        workdir,
        &["log", "--no-walk=unsorted", "--stdin", "--format=%H%x1f%s"],
        stdin.as_bytes(),
        &format!("{} sha(s) on stdin", shas.len()),
        commands,
    ) else {
        return HashMap::new();
    };
    String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| {
            let (h, s) = l.split_once('\x1f')?;
            Some((Oid::from_str(h.trim()).ok()?, s.trim().to_owned()))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Releases
// ---------------------------------------------------------------------------

/// A parsed tag record plus its full ref name (for commands).
fn parse_tag(record: &str) -> Option<(TagNote, String)> {
    let record = record.trim_start_matches(['\n', '\r']);
    let f: Vec<&str> = record.split('\x1f').collect();
    if f.len() < 9 {
        return None;
    }
    let full = f[0].trim().to_owned();
    if full.is_empty() {
        return None;
    }
    let annotated = f[2].trim() == "tag";
    let target = Oid::from_str(if annotated { f[4] } else { f[3] }.trim()).ok()?;
    let name = f[1].to_owned();
    let tagger = if annotated { f[6] } else { f[7] };
    Some((
        TagNote {
            target,
            annotated,
            signed: f.get(9).is_some_and(|s| !s.trim().is_empty()),
            time: f[5].trim().parse().unwrap_or(0),
            tagger: tagger.to_owned(),
            message: f[8].trim().to_owned(),
            on_branch: true,
            commits_since_previous: None,
            version: parse_version(&name),
            prerelease: is_prerelease(&name),
            name,
        },
        full,
    ))
}

/// `1.2.3-rc1`, `v2.0.0-beta.2`, `1.0a1`, `3.1.0-nightly` …
fn is_prerelease(name: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?i)(?:[-._](?:alpha|beta|rc|pre|preview|dev|nightly|snapshot)|\d(?:a|b|rc)\d+)",
        )
        .expect("valid regex")
    });
    re.is_match(name)
}

/// `feat(ui)!: thing` → `feat`.
fn conventional_type(summary: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^(\w+)(\([^)]*\))?!?:").expect("valid regex"));
    re.captures(summary).map(|c| c[1].to_lowercase())
}

/// `feat!: …` / `fix(core)!: …`, or `BREAKING CHANGE:` in the body.
fn is_breaking(summary: &str, body: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^\w+(\([^)]*\))?!:").expect("valid regex"));
    re.is_match(summary) || body.contains("BREAKING CHANGE") || body.contains("BREAKING-CHANGE")
}

/// `major`, `minor`, `patch` for a forward step in that component (lower
/// components reset or not), else `other` (downgrade, same version).
fn bump_kind(from: (u64, u64, u64), to: (u64, u64, u64)) -> &'static str {
    if to.0 > from.0 {
        "major"
    } else if to.0 == from.0 && to.1 > from.1 {
        "minor"
    } else if to.0 == from.0 && to.1 == from.1 && to.2 > from.2 {
        "patch"
    } else {
        "other"
    }
}

/// Calendar versions: `2024.03.1`, `2024-10-26`.
fn is_calver(version: (u64, u64, u64)) -> bool {
    version.0 >= 1990 && version.0 <= 2100
}

fn analyze_releases(
    workdir: &Path,
    now: i64,
    cancel: &AtomicBool,
    commands: &mut Vec<String>,
) -> Releases {
    let mut out = Releases::default();

    // Tags, newest first.
    let (listing, with_signature) = match git(
        workdir,
        &[
            "for-each-ref",
            "--sort=-creatordate",
            TAG_FORMAT,
            "refs/tags",
        ],
        commands,
    ) {
        Ok(text) => (text, true),
        Err(_) => (
            git_ok(
                workdir,
                &[
                    "for-each-ref",
                    "--sort=-creatordate",
                    TAG_FORMAT_NO_SIGNATURE,
                    "refs/tags",
                ],
                commands,
            ),
            false,
        ),
    };
    // Newest first; `for-each-ref` breaks creatordate ties by name, which
    // puts `v1.10.0` before `v1.9.0` when a release script made both in
    // the same second: break ties by version instead.
    let mut pairs: Vec<(TagNote, String)> = listing.split('\x1e').filter_map(parse_tag).collect();
    pairs.sort_by(|a, b| {
        b.0.time
            .cmp(&a.0.time)
            .then_with(|| b.0.version.cmp(&a.0.version))
    });
    let (mut tags, fulls): (Vec<TagNote>, Vec<String>) = pairs.into_iter().unzip();
    if !with_signature {
        for (tag, full) in tags
            .iter_mut()
            .zip(&fulls)
            .filter(|(t, _)| t.annotated)
            .take(SIGNATURE_FALLBACK_CAP)
        {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            tag.signed =
                git_ok(workdir, &["cat-file", "-p", full], commands).contains("-----BEGIN");
        }
    }

    // Reachability from branches.
    if !tags.is_empty()
        && tags.len() <= ON_BRANCH_MAX_TAGS
        && !cancel.load(Ordering::Relaxed)
        && let Ok(text) = git(workdir, &["rev-list", "--branches", "--remotes"], commands)
    {
        let reachable: HashSet<Oid> = text
            .lines()
            .filter_map(|l| Oid::from_str(l.trim()).ok())
            .collect();
        for t in &mut tags {
            t.on_branch = reachable.contains(&t.target);
        }
    }

    // Commits between consecutive tags, newest 30.
    for i in 0..tags.len().min(SINCE_PREVIOUS_CAP) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Some(prev) = fulls.get(i + 1) else {
            break;
        };
        let range = format!("{prev}..{}", fulls[i]);
        tags[i].commits_since_previous = git(workdir, &["rev-list", "--count", &range], commands)
            .ok()
            .and_then(|s| s.trim().parse().ok());
    }

    out.annotated = tags.iter().filter(|t| t.annotated).count();
    out.lightweight = tags.len() - out.annotated;
    out.signed = tags.iter().filter(|t| t.signed).count();
    out.orphan = tags.iter().filter(|t| !t.on_branch).count();

    // The last tag on HEAD's line and what came after it.
    let mut breaking = false;
    let mut has_feat = false;
    let last_tag = git(workdir, &["describe", "--tags", "--abbrev=0"], commands)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    if let Some(name) = &last_tag
        && !cancel.load(Ordering::Relaxed)
    {
        let full = format!("refs/tags/{name}");
        out.last_tag_time = tags
            .iter()
            .zip(&fulls)
            .find(|(t, f)| **f == full || t.name == *name)
            .map(|(t, _)| t.time)
            .or_else(|| {
                git_ok(
                    workdir,
                    &["log", "-1", "--format=%ct", name, "--"],
                    commands,
                )
                .trim()
                .parse()
                .ok()
            });
        out.since_last_days = out.last_tag_time.map(|t| days_between(t, now));
        let range = format!("{name}..HEAD");
        let req = LogRequest {
            max_commits: UNRELEASED_CAP,
            revisions: vec![range.clone()],
            ..Default::default()
        };
        let mut by_type: HashMap<String, u32> = HashMap::new();
        let _ = stream_log(workdir, &req, cancel, commands, &mut |c| {
            let ty = conventional_type(&c.summary);
            if is_breaking(&c.summary, &c.body) {
                breaking = true;
            }
            if ty.as_deref() == Some("feat") {
                has_feat = true;
            }
            *by_type
                .entry(ty.clone().unwrap_or_else(|| "other".to_owned()))
                .or_insert(0) += 1;
            out.unreleased.push((c.id, c.summary, c.author, c.time, ty));
            true
        });
        out.unreleased_total = git(workdir, &["rev-list", "--count", &range], commands)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(out.unreleased.len() as u32);
        let mut by_type: Vec<(String, u32)> = by_type.into_iter().collect();
        by_type.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out.unreleased_by_type = by_type;
    }
    out.last_tag = last_tag;

    // Version tags: cadence, scheme, bumps, suggestion.
    let versions: Vec<usize> = (0..tags.len())
        .filter(|&i| tags[i].version.is_some())
        .collect();
    let times: Vec<i64> = versions
        .iter()
        .take(CADENCE_TAGS)
        .map(|&i| tags[i].time)
        .collect();
    if times.len() >= 2 {
        let span = (times[0] - times[times.len() - 1]).max(0) as f32;
        out.cadence_days = Some(span / 86_400.0 / (times.len() - 1) as f32);
    }
    let calver = versions
        .iter()
        .filter(|&&i| tags[i].version.is_some_and(is_calver))
        .count();
    out.scheme = match (versions.len(), calver) {
        (0, _) => "none",
        (n, c) if c == n => "calver",
        (_, 0) => "semver",
        _ => "mixed",
    };
    for pair in versions.windows(2).take(BUMPS_CAP) {
        let (to, from) = (&tags[pair[0]], &tags[pair[1]]);
        let (Some(vt), Some(vf)) = (to.version, from.version) else {
            continue;
        };
        out.bumps.push(Bump {
            from: from.name.clone(),
            to: to.name.clone(),
            kind: bump_kind(vf, vt),
            time: to.time,
            days: days_between(from.time, to.time),
            // The count is against the previous tag by time: only valid
            // when that is `from`.
            commits: (pair[1] == pair[0] + 1)
                .then_some(to.commits_since_previous)
                .flatten(),
        });
    }
    if out.unreleased_total > 0 && out.scheme != "calver" {
        let base = out
            .last_tag
            .as_deref()
            .and_then(parse_version)
            .or_else(|| versions.first().and_then(|&i| tags[i].version))
            .filter(|v| !is_calver(*v));
        if let Some((major, minor, patch)) = base {
            let next = if breaking {
                (major + 1, 0, 0)
            } else if has_feat {
                (major, minor + 1, 0)
            } else {
                (major, minor, patch + 1)
            };
            out.suggested_next = Some(format!("{}.{}.{}", next.0, next.1, next.2));
        }
    }

    out.tags = tags;
    out
}

/// Parse `v1.2.3`, `1.2`, `release-2.0.1`, `2024.03.1` style names into
/// `(major, minor, patch)`; `None` when not a version.
pub fn parse_version(name: &str) -> Option<(u64, u64, u64)> {
    static RES: OnceLock<(Regex, Regex)> = OnceLock::new();
    let (version, suffix) = RES.get_or_init(|| {
        (
            Regex::new(r"^(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:\.\d+)?").expect("valid regex"),
            // What may follow the numbers: nothing, a separator, or a
            // glued pre-release marker (`1.2.3rc1`, `1.0b2`).
            Regex::new(r"^(?:$|[-_+.]|(?i:a|b|rc|alpha|beta|pre|dev|post)\d+(?:$|[-_+.]))")
                .expect("valid regex"),
        )
    });
    // Optional prefix: `v`, `release-`, `version-`, anything up to the
    // first digit.
    let start = name.find(|c: char| c.is_ascii_digit())?;
    let rest = &name[start..];
    let caps = version.captures(rest)?;
    let major = caps.get(1)?;
    if major.len() > 6 || !suffix.is_match(&rest[caps.get(0)?.end()..]) {
        return None;
    }
    let part = |i: usize| {
        caps.get(i)
            .and_then(|m| m.as_str().parse::<u64>().ok())
            .unwrap_or(0)
    };
    Some((major.as_str().parse().ok()?, part(2), part(3)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("V1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("release-2.0"), Some((2, 0, 0)));
        assert_eq!(parse_version("version-3"), Some((3, 0, 0)));
        assert_eq!(parse_version("2024.03.1"), Some((2024, 3, 1)));
        assert_eq!(parse_version("1.2.3-rc1"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2.3rc1"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2.3.4"), Some((1, 2, 3)));
        assert_eq!(parse_version("v1.2.3+build.7"), Some((1, 2, 3)));
        assert_eq!(parse_version("foo"), None);
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("build-20240101"), None, "more than 6 digits");
        assert_eq!(parse_version("sha-1a2b3c"), None, "hex, not a version");
        assert!(is_prerelease("1.2.3-rc1"));
        assert!(is_prerelease("v2.0.0-beta.2"));
        assert!(is_prerelease("1.0b2"));
        assert!(is_prerelease("3.1.0-nightly"));
        assert!(!is_prerelease("v1.2.3"));
        assert!(!is_prerelease("2024.03.1"));
    }

    #[test]
    fn bumps_and_schemes() {
        assert_eq!(bump_kind((1, 2, 3), (2, 0, 0)), "major");
        assert_eq!(bump_kind((1, 2, 3), (1, 3, 0)), "minor");
        assert_eq!(bump_kind((1, 2, 3), (1, 2, 4)), "patch");
        assert_eq!(bump_kind((1, 2, 3), (1, 2, 3)), "other");
        assert_eq!(bump_kind((2, 0, 0), (1, 9, 9)), "other", "downgrade");
        assert!(is_calver((2024, 3, 1)));
        assert!(!is_calver((1, 2, 3)));
        assert!(!is_calver((101, 0, 0)));
    }

    #[test]
    fn throwaway_names() {
        for name in [
            "tmp",
            "tmp-fix",
            "test",
            "test2",
            "wip/thing",
            "origin/junk",
            "feature-old",
            "main-bak",
            "experiment_x",
            "deleteme",
            "delete-me",
            "TEMP",
        ] {
            assert!(is_throwaway(name), "{name}");
        }
        for name in [
            "main",
            "testimonials",
            "feature/login",
            "oldham-fix",
            "release/1.2",
            "bartender",
        ] {
            assert!(!is_throwaway(name), "{name}");
        }
    }

    #[test]
    fn track_parsing() {
        assert_eq!(
            parse_track("[ahead 2, behind 3]", true),
            (Some(2), Some(3), false)
        );
        assert_eq!(parse_track("[ahead 1]", true), (Some(1), Some(0), false));
        assert_eq!(parse_track("[behind 7]", true), (Some(0), Some(7), false));
        assert_eq!(parse_track("[gone]", true), (None, None, true));
        assert_eq!(parse_track("", true), (Some(0), Some(0), false), "in sync");
        assert_eq!(parse_track("", false), (None, None, false), "no upstream");
    }

    #[test]
    fn conventional_commits() {
        assert_eq!(
            conventional_type("feat(ui): add thing").as_deref(),
            Some("feat")
        );
        assert_eq!(conventional_type("Fix: typo").as_deref(), Some("fix"));
        assert_eq!(
            conventional_type("feat!: drop api").as_deref(),
            Some("feat")
        );
        assert_eq!(conventional_type("Merge branch 'x'"), None);
        assert_eq!(conventional_type("WIP"), None);
        assert!(is_breaking("feat!: drop api", ""));
        assert!(is_breaking("refactor(core)!: rename", ""));
        assert!(is_breaking("fix: x", "BREAKING CHANGE: the flag is gone"));
        assert!(!is_breaking("feat: x", "nothing special"));
    }

    #[test]
    fn parses_ref_records() {
        let rec = "refs/heads/feature\x1ffeature\x1f0123456789abcdef0123456789abcdef01234567\x1f1700000000\x1fAda\x1fAdd thing\x1frefs/remotes/origin/feature\x1forigin/feature\x1f[ahead 2, behind 1]\x1f ";
        let r = parse_branch(rec).expect("parses");
        assert_eq!(r.note.name, "feature");
        assert!(!r.note.is_remote);
        assert!(!r.note.is_head);
        assert_eq!(r.note.tip_time, 1_700_000_000);
        assert_eq!(r.note.upstream.as_deref(), Some("origin/feature"));
        assert_eq!(
            r.upstream_full.as_deref(),
            Some("refs/remotes/origin/feature")
        );
        assert_eq!((r.note.ahead, r.note.behind), (Some(2), Some(1)));
        let head = "\nrefs/heads/main\x1fmain\x1f0123456789abcdef0123456789abcdef01234567\x1f1\x1fAda\x1fRoot\x1f\x1f\x1f\x1f*";
        let r = parse_branch(head).expect("parses");
        assert!(r.note.is_head);
        assert!(r.note.upstream.is_none());
        assert_eq!(r.note.ahead, None);
        assert!(parse_branch("").is_none());

        let tag = "refs/tags/v1.2.3\x1fv1.2.3\x1ftag\x1faaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\x1f0123456789abcdef0123456789abcdef01234567\x1f1700000000\x1fBob\x1fAda\x1fRelease 1.2.3\x1f-----BEGIN PGP SIGNATURE-----\nabc\n-----END PGP SIGNATURE-----\n";
        let (t, full) = parse_tag(tag).expect("parses");
        assert_eq!(full, "refs/tags/v1.2.3");
        assert!(t.annotated);
        assert!(t.signed);
        assert_eq!(
            t.target.to_string(),
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(t.tagger, "Bob");
        assert_eq!(t.message, "Release 1.2.3");
        assert_eq!(t.version, Some((1, 2, 3)));
        let light = "refs/tags/2024.03.1\x1f2024.03.1\x1fcommit\x1f0123456789abcdef0123456789abcdef01234567\x1f\x1f1700000000\x1f\x1fAda\x1fSome commit\x1f";
        let (t, _) = parse_tag(light).expect("parses");
        assert!(!t.annotated);
        assert!(!t.signed);
        assert_eq!(t.tagger, "Ada");
        assert_eq!(t.version, Some((2024, 3, 1)));
    }
}
