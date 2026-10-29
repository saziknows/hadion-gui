//! JSON models of the Actions endpoints this area reads, tolerant of
//! missing fields (`#[serde(default)]` everywhere), and conversions into the
//! shared [`crate::github`] types.

use serde::Deserialize;

use crate::github::{Job, Step, WorkflowRun, parse_time};

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct User {
    pub login: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
struct CommitAuthor {
    name: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
struct HeadCommit {
    message: String,
    author: Option<CommitAuthor>,
}

/// A workflow run as the REST API returns it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunJson {
    pub id: u64,
    pub name: Option<String>,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    pub head_branch: Option<String>,
    pub head_sha: String,
    pub html_url: String,
    pub event: String,
    pub created_at: String,
    pub run_started_at: Option<String>,
    pub updated_at: String,
    pub run_number: u64,
    pub run_attempt: u32,
    pub display_title: String,
    actor: Option<User>,
    triggering_actor: Option<User>,
    pub path: String,
    pub workflow_id: u64,
    head_commit: Option<HeadCommit>,
}

impl RunJson {
    pub fn into_run(self) -> WorkflowRun {
        let head = self.head_commit;
        WorkflowRun {
            id: self.id,
            name: self.name.unwrap_or_default(),
            status: self.status.unwrap_or_default(),
            conclusion: self.conclusion,
            branch: self.head_branch.unwrap_or_default(),
            sha: self.head_sha,
            url: self.html_url,
            event: self.event,
            created: parse_time(&self.created_at),
            started: self.run_started_at.as_deref().and_then(parse_time),
            updated: parse_time(&self.updated_at),
            run_number: self.run_number,
            attempt: self.run_attempt.max(1),
            title: self.display_title,
            actor: self.actor.map(|u| u.login).unwrap_or_default(),
            triggering_actor: self.triggering_actor.map(|u| u.login).unwrap_or_default(),
            path: self.path,
            workflow_id: self.workflow_id,
            head_message: head
                .as_ref()
                .map(|h| h.message.lines().next().unwrap_or("").to_owned())
                .unwrap_or_default(),
            head_author: head
                .and_then(|h| h.author)
                .map(|a| a.name)
                .unwrap_or_default(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RunsPage {
    pub total_count: u64,
    pub workflow_runs: Vec<RunJson>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct StepJson {
    number: u32,
    name: String,
    status: String,
    conclusion: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
}

/// A job as the REST API returns it.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct JobJson {
    id: u64,
    run_id: u64,
    name: String,
    status: String,
    conclusion: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
    runner_name: Option<String>,
    runner_group_name: Option<String>,
    labels: Vec<String>,
    html_url: String,
    run_attempt: u32,
    steps: Vec<StepJson>,
}

impl JobJson {
    pub fn into_job(self) -> Job {
        Job {
            id: self.id,
            run_id: self.run_id,
            name: self.name,
            status: self.status,
            conclusion: self.conclusion,
            started: self.started_at.as_deref().and_then(parse_time),
            completed: self.completed_at.as_deref().and_then(parse_time),
            runner: self.runner_name.filter(|s| !s.is_empty()),
            runner_group: self.runner_group_name.filter(|s| !s.is_empty()),
            labels: self.labels,
            url: self.html_url,
            attempt: self.run_attempt.max(1),
            steps: self
                .steps
                .into_iter()
                .map(|s| Step {
                    number: s.number,
                    name: s.name,
                    status: s.status,
                    conclusion: s.conclusion,
                    started: s.started_at.as_deref().and_then(parse_time),
                    completed: s.completed_at.as_deref().and_then(parse_time),
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct JobsPage {
    pub total_count: u64,
    pub jobs: Vec<JobJson>,
}

/// The run an artifact came from.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ArtifactRun {
    pub id: u64,
    pub head_branch: Option<String>,
    pub head_sha: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Artifact {
    pub id: u64,
    pub name: String,
    pub size_in_bytes: u64,
    pub expired: bool,
    pub created_at: Option<String>,
    pub expires_at: Option<String>,
    pub updated_at: Option<String>,
    pub digest: Option<String>,
    pub workflow_run: Option<ArtifactRun>,
}

impl Artifact {
    pub fn created(&self) -> Option<i64> {
        self.created_at.as_deref().and_then(parse_time)
    }

    pub fn expires(&self) -> Option<i64> {
        self.expires_at.as_deref().and_then(parse_time)
    }

    pub fn run_id(&self) -> Option<u64> {
        self.workflow_run
            .as_ref()
            .map(|r| r.id)
            .filter(|id| *id > 0)
    }

    pub fn branch(&self) -> &str {
        self.workflow_run
            .as_ref()
            .and_then(|r| r.head_branch.as_deref())
            .unwrap_or("")
    }

    pub fn sha(&self) -> &str {
        self.workflow_run
            .as_ref()
            .and_then(|r| r.head_sha.as_deref())
            .unwrap_or("")
    }

    /// Expires within `secs` of `now` (and has not yet).
    pub fn expires_within(&self, now: i64, secs: i64) -> bool {
        !self.expired && self.expires().is_some_and(|e| e > now && e - now <= secs)
    }
}

/// `{"total_count": n, "artifacts": [...]}`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ArtifactsPage {
    pub total_count: u64,
    pub artifacts: Vec<Artifact>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Cache {
    pub id: u64,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub key: String,
    pub version: String,
    pub last_accessed_at: Option<String>,
    pub created_at: Option<String>,
    pub size_in_bytes: u64,
}

impl Cache {
    pub fn created(&self) -> Option<i64> {
        self.created_at.as_deref().and_then(parse_time)
    }

    pub fn last_accessed(&self) -> Option<i64> {
        self.last_accessed_at.as_deref().and_then(parse_time)
    }

    /// The pull request number of a `refs/pull/N/merge` (or `/head`) ref.
    pub fn pr_number(&self) -> Option<u64> {
        pr_number(&self.git_ref)
    }
}

/// `refs/pull/123/merge` → 123.
pub fn pr_number(git_ref: &str) -> Option<u64> {
    let rest = git_ref.strip_prefix("refs/pull/")?;
    rest.split('/').next()?.parse().ok()
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CacheUsage {
    pub full_name: String,
    pub active_caches_size_in_bytes: u64,
    pub active_caches_count: u64,
}

/// GitHub's cache storage limit per repository.
pub const CACHE_LIMIT_BYTES: u64 = 10 * 1024 * 1024 * 1024;

/// One cache ref with its entries, for bulk deletion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheRef {
    pub git_ref: String,
    pub count: usize,
    pub bytes: u64,
    pub ids: Vec<u64>,
    pub last_accessed: Option<i64>,
}

/// Group caches by ref, largest first.
pub fn group_by_ref(caches: &[Cache]) -> Vec<CacheRef> {
    let mut map: std::collections::BTreeMap<&str, CacheRef> = Default::default();
    for c in caches {
        let e = map.entry(&c.git_ref).or_insert_with(|| CacheRef {
            git_ref: c.git_ref.clone(),
            count: 0,
            bytes: 0,
            ids: Vec::new(),
            last_accessed: None,
        });
        e.count += 1;
        e.bytes += c.size_in_bytes;
        e.ids.push(c.id);
        e.last_accessed = e.last_accessed.max(c.last_accessed());
    }
    let mut v: Vec<CacheRef> = map.into_values().collect();
    v.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.git_ref.cmp(&b.git_ref)));
    v
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct CompareAuthor {
    name: String,
    date: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct CompareCommitInner {
    message: String,
    author: Option<CompareAuthor>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct CompareCommit {
    sha: String,
    commit: CompareCommitInner,
    author: Option<User>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct CompareFile {
    filename: String,
    status: String,
    additions: u64,
    deletions: u64,
    previous_filename: Option<String>,
}

/// `GET compare/{base}...{head}`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct CompareJson {
    status: String,
    ahead_by: u64,
    behind_by: u64,
    total_commits: u64,
    commits: Vec<CompareCommit>,
    files: Vec<CompareFile>,
}

/// A commit between two runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitLite {
    pub sha: String,
    pub summary: String,
    pub author: String,
    pub time: Option<i64>,
}

/// A file changed between two runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileLite {
    pub path: String,
    /// `added`, `modified`, `removed`, `renamed`, …
    pub status: String,
    pub additions: u64,
    pub deletions: u64,
    pub old_path: Option<String>,
}

/// Where the commit list came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitsSource {
    Local,
    Api,
}

/// Commits and files between the last green run and the failing one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Between {
    pub source: CommitsSource,
    /// Newest first.
    pub commits: Vec<CommitLite>,
    pub files: Vec<FileLite>,
    pub total_commits: u64,
    /// `ahead`, `behind`, `diverged`, `identical` (API only).
    pub status: String,
    pub behind_by: u64,
}

impl CompareJson {
    pub fn into_between(self) -> Between {
        let mut commits: Vec<CommitLite> = self
            .commits
            .into_iter()
            .map(|c| CommitLite {
                summary: c.commit.message.lines().next().unwrap_or("").to_owned(),
                author: c
                    .author
                    .map(|a| a.login)
                    .filter(|l| !l.is_empty())
                    .or_else(|| c.commit.author.as_ref().map(|a| a.name.clone()))
                    .unwrap_or_default(),
                time: c.commit.author.as_ref().and_then(|a| parse_time(&a.date)),
                sha: c.sha,
            })
            .collect();
        // The API lists oldest first.
        commits.reverse();
        Between {
            source: CommitsSource::Api,
            total_commits: self.total_commits.max(self.ahead_by),
            commits,
            files: self
                .files
                .into_iter()
                .map(|f| FileLite {
                    path: f.filename,
                    status: f.status,
                    additions: f.additions,
                    deletions: f.deletions,
                    old_path: f.previous_filename,
                })
                .collect(),
            status: self.status,
            behind_by: self.behind_by,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARTIFACTS: &str = r#"{"total_count":2,"artifacts":[
      {"id":11,"node_id":"x","name":"coverage","size_in_bytes":2048,"url":"u","archive_download_url":"a",
       "expired":false,"created_at":"2026-10-20T10:00:00Z","updated_at":"2026-10-20T10:00:00Z","expires_at":"2026-10-28T10:00:00Z",
       "digest":"sha256:abc","workflow_run":{"id":77,"repository_id":1,"head_repository_id":1,"head_branch":"main","head_sha":"deadbeef"}},
      {"id":12,"name":"old","size_in_bytes":1,"expired":true,"created_at":null,"expires_at":null,"workflow_run":null}]}"#;

    const CACHES: &str = r#"{"total_count":3,"actions_caches":[
      {"id":9541564459,"ref":"refs/pull/40994/merge","key":"opengrep-v1.30.0-Linux","version":"13ab","last_accessed_at":"2026-10-26T18:55:25.549016Z","created_at":"2026-10-26T18:55:25.549016Z","size_in_bytes":46454034},
      {"id":2,"ref":"refs/pull/40994/merge","key":"node","version":"v","last_accessed_at":"2026-10-25T18:55:25Z","created_at":"2026-10-25T18:55:25Z","size_in_bytes":100},
      {"id":3,"ref":"refs/heads/master","key":"node","version":"v","size_in_bytes":5}]}"#;

    #[test]
    fn artifacts_parse() {
        let p: ArtifactsPage = serde_json::from_str(ARTIFACTS).unwrap();
        assert_eq!(p.total_count, 2);
        let a = &p.artifacts[0];
        assert_eq!(a.run_id(), Some(77));
        assert_eq!(a.branch(), "main");
        assert_eq!(a.sha(), "deadbeef");
        let now = parse_time("2026-10-26T10:00:00Z").unwrap();
        assert!(a.expires_within(now, 3 * 86_400));
        assert!(!a.expires_within(now, 86_400));
        let b = &p.artifacts[1];
        assert!(b.expired);
        assert_eq!(b.run_id(), None);
        assert_eq!(b.created(), None);
    }

    #[test]
    fn caches_parse_and_group() {
        let v: serde_json::Value = serde_json::from_str(CACHES).unwrap();
        let caches: Vec<Cache> = serde_json::from_value(v["actions_caches"].clone()).unwrap();
        assert_eq!(caches[0].git_ref, "refs/pull/40994/merge");
        assert_eq!(caches[0].pr_number(), Some(40994));
        assert!(caches[0].last_accessed().is_some());
        assert_eq!(caches[2].pr_number(), None);
        let g = group_by_ref(&caches);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].git_ref, "refs/pull/40994/merge");
        assert_eq!(g[0].count, 2);
        assert_eq!(g[0].bytes, 46454134);
        assert_eq!(g[0].ids, [9541564459, 2]);
        assert_eq!(pr_number("refs/pull/7/head"), Some(7));
        assert_eq!(pr_number("refs/heads/pull/7"), None);
    }

    #[test]
    fn runs_and_jobs_convert() {
        let text = r#"{"total_count":1,"workflow_runs":[{"id":5,"name":"CI","status":"completed","conclusion":"failure",
          "head_branch":"main","head_sha":"abc","event":"push","created_at":"2026-10-26T10:00:00Z",
          "run_started_at":"2026-10-26T10:00:30Z","updated_at":"2026-10-26T10:05:30Z","run_number":12,"run_attempt":2,
          "display_title":"Fix","actor":{"login":"ada"},"path":".github/workflows/ci.yml","workflow_id":9,
          "head_commit":{"message":"Fix it\n\nbody","author":{"name":"Ada"}}}]}"#;
        let p: RunsPage = serde_json::from_str(text).unwrap();
        let r = p.workflow_runs.into_iter().next().unwrap().into_run();
        assert_eq!(r.attempt, 2);
        assert_eq!(r.queue_secs(), Some(30));
        assert_eq!(r.duration_secs(0), Some(300));
        assert_eq!(r.head_message, "Fix it");
        assert_eq!(r.actor, "ada");
        assert!(r.is_failure());
        let jobs: JobsPage = serde_json::from_str(
            r#"{"total_count":1,"jobs":[{"id":1,"run_id":5,"name":"build","status":"completed","conclusion":"success",
            "started_at":"2026-10-26T10:00:30Z","completed_at":"2026-10-26T10:01:30Z","run_attempt":1,
            "steps":[{"number":1,"name":"Set up job","status":"completed","conclusion":"success",
            "started_at":"2026-10-26T10:00:30Z","completed_at":"2026-10-26T10:00:40Z"}]}]}"#,
        )
        .unwrap();
        let j = jobs.jobs.into_iter().next().unwrap().into_job();
        assert_eq!(j.duration_secs(0), Some(60));
        assert_eq!(j.steps[0].duration_secs(0), Some(10));
    }

    #[test]
    fn compare_converts_newest_first() {
        let text = r#"{"status":"ahead","ahead_by":2,"behind_by":0,"total_commits":2,
          "commits":[{"sha":"a1","commit":{"message":"first\nmore","author":{"name":"A","date":"2026-10-20T10:00:00Z"}},"author":{"login":"ada"}},
                     {"sha":"b2","commit":{"message":"second","author":{"name":"B","date":"2026-10-21T10:00:00Z"}},"author":null}],
          "files":[{"filename":"src/x.rs","status":"modified","additions":3,"deletions":1},
                   {"filename":"y","status":"renamed","previous_filename":"z"}]}"#;
        let c: CompareJson = serde_json::from_str(text).unwrap();
        let b = c.into_between();
        assert_eq!(b.commits[0].sha, "b2");
        assert_eq!(b.commits[0].author, "B");
        assert_eq!(b.commits[1].author, "ada");
        assert_eq!(b.commits[1].summary, "first");
        assert_eq!(b.files[1].old_path.as_deref(), Some("z"));
        assert_eq!(b.total_commits, 2);
        assert_eq!(b.source, CommitsSource::Api);
    }
}
