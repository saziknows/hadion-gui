//! "Where is my commit": the checkpoints a commit passes on its way to
//! production. Local history answers most of it (dates, branches, tags,
//! whether each environment's deployed commit contains it); GitHub adds
//! the pull request that introduced it.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use super::local::{self, Tally, US};
use crate::devops::gh::Gh;
use crate::github::{ApiCall, parse_time};
use crate::{Error, Result};

/// Tags kept in the answer (the total is reported).
pub const MAX_TAGS: usize = 200;
/// Deployed commits tested per environment for "since when".
pub const MAX_DEPLOYED: usize = 60;

/// One checkpoint's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Done,
    NotYet,
    Unknown,
}

/// What the caller knows about one environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvDeploys {
    pub env: String,
    /// The commit deployed there now.
    pub current: Option<String>,
    /// Successful deployments, oldest first: `(id, sha, time)`.
    pub successes: Vec<(u64, String, i64)>,
}

/// Whether an environment has the commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvHit {
    pub env: String,
    pub mark: Mark,
    pub current: Option<String>,
    /// The first successful deployment that contained the commit.
    pub since: Option<(i64, u64, String)>,
    /// Why the answer is unknown, or how it was decided.
    pub note: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Located {
    pub sha: String,
    pub summary: String,
    pub author: String,
    pub author_email: String,
    pub author_time: i64,
    pub committer: String,
    pub commit_time: i64,
    pub local_branches: Vec<String>,
    /// `origin/main` style.
    pub remote_branches: Vec<String>,
    /// Tags containing the commit, oldest first (by creator date), capped.
    pub tags: Vec<(String, i64)>,
    pub tags_total: usize,
    pub envs: Vec<EnvHit>,
}

impl Located {
    pub fn first_tag(&self) -> Option<&(String, i64)> {
        self.tags.first()
    }

    pub fn short(&self) -> &str {
        &self.sha[..self.sha.len().min(10)]
    }
}

/// Answer every local checkpoint for `rev`.
pub fn locate(
    dir: &Path,
    rev: &str,
    envs: &[EnvDeploys],
    commands: &mut Vec<String>,
) -> Result<Located> {
    let rev = rev.trim();
    if rev.is_empty() || rev.starts_with('-') {
        return Err(Error::new("Type a commit id, branch or tag."));
    }
    let sha = local::resolve(dir, rev, commands).ok_or_else(|| local::not_found(rev))?;
    let show = local::git(
        dir,
        &[
            "show",
            "-s",
            "--format=%H%x1f%an%x1f%ae%x1f%at%x1f%cn%x1f%ct%x1f%s",
            &sha,
        ],
        commands,
    )?;
    let f: Vec<&str> = show.trim_end().splitn(7, US).collect();
    let field = |i: usize| f.get(i).copied().unwrap_or("").to_owned();
    let num = |i: usize| f.get(i).and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let mut out = Located {
        sha: sha.clone(),
        author: field(1),
        author_email: field(2),
        author_time: num(3),
        committer: field(4),
        commit_time: num(5),
        summary: field(6),
        ..Default::default()
    };
    let branches = local::git(
        dir,
        &["branch", "-a", "--contains", &sha, "--format=%(refname)"],
        commands,
    )
    .unwrap_or_default();
    for b in branches.lines().map(str::trim).filter(|b| !b.is_empty()) {
        if let Some(l) = b.strip_prefix("refs/heads/") {
            out.local_branches.push(l.to_owned());
        } else if let Some(r) = b.strip_prefix("refs/remotes/")
            && !r.ends_with("/HEAD")
        {
            out.remote_branches.push(r.to_owned());
        }
    }
    let tags = local::git(
        dir,
        &[
            "tag",
            "--contains",
            &sha,
            "--sort=creatordate",
            "--format=%(refname:short)%1f%(creatordate:unix)",
        ],
        commands,
    )
    .unwrap_or_default();
    for t in tags.lines().filter(|t| !t.trim().is_empty()) {
        out.tags_total += 1;
        if out.tags.len() < MAX_TAGS {
            let (name, time) = t.split_once(US).unwrap_or((t, "0"));
            out.tags
                .push((name.to_owned(), time.trim().parse().unwrap_or(0)));
        }
    }
    out.envs = env_hits(dir, &sha, envs, commands);
    Ok(out)
}

/// For each environment: is the commit an ancestor of what is deployed
/// there, and since which deployment.
pub fn env_hits(
    dir: &Path,
    sha: &str,
    envs: &[EnvDeploys],
    commands: &mut Vec<String>,
) -> Vec<EnvHit> {
    let mut tally = Tally::new("git merge-base --is-ancestor");
    let mut cache: HashMap<String, Option<bool>> = HashMap::new();
    let mut contains = |target: &str, tally: &mut Tally, commands: &mut Vec<String>| {
        if let Some(c) = cache.get(target) {
            return *c;
        }
        let c = local::is_ancestor(dir, sha, target, tally, commands);
        cache.insert(target.to_owned(), c);
        c
    };
    let mut out = Vec::new();
    for e in envs {
        let Some(cur) = e.current.clone() else {
            out.push(EnvHit {
                env: e.env.clone(),
                mark: Mark::Unknown,
                current: None,
                since: None,
                note: "No successful deployment known for this environment.".to_owned(),
            });
            continue;
        };
        let (mark, mut note) = match contains(&cur, &mut tally, commands) {
            Some(true) => (Mark::Done, String::new()),
            Some(false) => (Mark::NotYet, String::new()),
            None => (
                Mark::Unknown,
                format!(
                    "The deployed commit {} is not in the local history: fetch to compare.",
                    &cur[..cur.len().min(8)]
                ),
            ),
        };
        let mut since = None;
        if mark == Mark::Done {
            let mut tested = 0;
            let mut seen: Vec<&str> = Vec::new();
            for (id, dsha, time) in &e.successes {
                if seen.contains(&dsha.as_str()) {
                    continue;
                }
                if tested >= MAX_DEPLOYED {
                    note = format!(
                        "Only the oldest {MAX_DEPLOYED} deployed commits were tested for the first deployment."
                    );
                    break;
                }
                seen.push(dsha);
                tested += 1;
                if contains(dsha, &mut tally, commands) == Some(true) {
                    since = Some((*time, *id, dsha.clone()));
                    break;
                }
            }
        }
        out.push(EnvHit {
            env: e.env.clone(),
            mark,
            current: Some(cur),
            since,
            note,
        });
    }
    tally.finish(commands);
    out
}

// ---------------------------------------------------------------------------
// The pull request
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitPull {
    pub number: u64,
    pub title: String,
    /// `open` / `closed`.
    pub state: String,
    pub html_url: String,
    pub user: String,
    pub merged_at: Option<i64>,
    /// From the single pull request (the list does not carry it).
    pub merged_by: String,
    pub base: String,
    pub head: String,
    pub merge_commit_sha: String,
    pub draft: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawRef {
    #[serde(rename = "ref")]
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawUser {
    login: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawPull {
    number: u64,
    title: String,
    state: String,
    html_url: String,
    user: Option<RawUser>,
    merged_at: Option<String>,
    merged_by: Option<RawUser>,
    base: Option<RawRef>,
    head: Option<RawRef>,
    merge_commit_sha: Option<String>,
    draft: bool,
}

fn pull_of(p: RawPull) -> CommitPull {
    CommitPull {
        number: p.number,
        title: p.title,
        state: p.state,
        html_url: p.html_url,
        user: p.user.map(|u| u.login).unwrap_or_default(),
        merged_at: p.merged_at.as_deref().and_then(parse_time),
        merged_by: p.merged_by.map(|u| u.login).unwrap_or_default(),
        base: p.base.map(|r| r.name).unwrap_or_default(),
        head: p.head.map(|r| r.name).unwrap_or_default(),
        merge_commit_sha: p.merge_commit_sha.unwrap_or_default(),
        draft: p.draft,
    }
}

/// Parse `GET commits/{sha}/pulls` (or one `GET pulls/{n}`).
pub fn parse_pulls(v: &Value) -> Vec<CommitPull> {
    match v {
        Value::Array(a) => a
            .iter()
            .filter_map(|p| serde_json::from_value::<RawPull>(p.clone()).ok())
            .map(pull_of)
            .collect(),
        Value::Object(_) => serde_json::from_value::<RawPull>(v.clone())
            .ok()
            .map(pull_of)
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

/// `commits/{sha}/pulls`.
pub fn pulls_endpoint(gh: &Gh, sha: &str) -> String {
    gh.path(&format!("commits/{sha}/pulls"))
}

/// The pull request that brought the commit in: the merged one whose
/// merge commit is this commit, else the first merged, else the first.
pub fn introducing<'a>(pulls: &'a [CommitPull], sha: &str) -> Option<&'a CommitPull> {
    pulls
        .iter()
        .find(|p| p.merged_at.is_some() && p.merge_commit_sha == sha)
        .or_else(|| pulls.iter().find(|p| p.merged_at.is_some()))
        .or_else(|| pulls.first())
}

/// Pull requests associated with the commit, the introducing one first
/// and completed with who merged it.
pub fn fetch_pulls(gh: &Gh, sha: &str, calls: &mut Vec<ApiCall>) -> Result<Vec<CommitPull>> {
    let v: Value = gh.get_json(
        &pulls_endpoint(gh, sha),
        "pull requests of the commit",
        calls,
    )?;
    let mut pulls = parse_pulls(&v);
    let Some(first) = introducing(&pulls, sha).map(|p| p.number) else {
        return Ok(pulls);
    };
    pulls.sort_by_key(|p| p.number != first);
    if pulls[0].merged_at.is_some()
        && let Ok(one) =
            gh.get_json::<Value>(&gh.path(&format!("pulls/{first}")), "pull request", calls)
        && let Some(full) = parse_pulls(&one).into_iter().next()
    {
        pulls[0].merged_by = full.merged_by;
    }
    Ok(pulls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_introducing_pull_request() {
        let v = serde_json::json!([
            {"number": 5, "title": "Backport", "state": "open", "merged_at": null,
             "base": {"ref": "release"}, "head": {"ref": "bp"}},
            {"number": 3, "title": "Add x", "state": "closed", "merged_at": "2026-10-01T10:00:00Z",
             "merge_commit_sha": "abc", "user": {"login": "ada"},
             "base": {"ref": "main"}, "head": {"ref": "feature/x"}}
        ]);
        let p = parse_pulls(&v);
        assert_eq!(p.len(), 2);
        assert_eq!(introducing(&p, "abc").unwrap().number, 3);
        assert_eq!(introducing(&p, "zzz").unwrap().number, 3);
        assert_eq!(p[1].base, "main");
        assert_eq!(p[1].user, "ada");
        let one = parse_pulls(&serde_json::json!({"number": 3, "merged_by": {"login": "bob"}}));
        assert_eq!(one[0].merged_by, "bob");
        assert!(introducing(&[], "x").is_none());
        let gh = Gh::new(".", "o", "r");
        assert_eq!(pulls_endpoint(&gh, "abc"), "repos/o/r/commits/abc/pulls");
    }
}
