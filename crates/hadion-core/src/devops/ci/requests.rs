//! Every Actions write as a pure [`Request`] (method, endpoint, body), plus
//! the read endpoints this area uses. Nothing here touches the network.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::devops::gh::{Gh, Method, Request, encode};

fn debug_body(debug: bool) -> Option<Value> {
    debug.then(|| json!({ "enable_debug_logging": true }))
}

fn post(gh: &Gh, rest: &str, body: Option<Value>) -> Request {
    let r = Request::new(Method::Post, gh.path(rest));
    match body {
        Some(b) => r.body(b),
        None => r,
    }
}

/// Re-run every job of a run (`gh run rerun`), optionally with runner and
/// step debug logging.
pub fn rerun(gh: &Gh, run: u64, debug: bool) -> Request {
    post(gh, &format!("actions/runs/{run}/rerun"), debug_body(debug))
}

/// Re-run the failed jobs of a run and the jobs that depend on them.
pub fn rerun_failed(gh: &Gh, run: u64, debug: bool) -> Request {
    post(
        gh,
        &format!("actions/runs/{run}/rerun-failed-jobs"),
        debug_body(debug),
    )
}

/// Re-run one job (and the jobs that depend on it).
pub fn rerun_job(gh: &Gh, job: u64, debug: bool) -> Request {
    post(gh, &format!("actions/jobs/{job}/rerun"), debug_body(debug))
}

/// Cancel a run (running jobs stop, queued jobs are skipped).
pub fn cancel(gh: &Gh, run: u64) -> Request {
    post(gh, &format!("actions/runs/{run}/cancel"), None)
}

/// Cancel a run that does not respond to a normal cancel (ignores
/// `always()` conditions).
pub fn force_cancel(gh: &Gh, run: u64) -> Request {
    post(gh, &format!("actions/runs/{run}/force-cancel"), None)
}

/// Delete every log of a run.
pub fn delete_logs(gh: &Gh, run: u64) -> Request {
    Request::delete(gh.path(&format!("actions/runs/{run}/logs")))
}

/// Delete a run (its logs, artifacts and check suite entry).
pub fn delete_run(gh: &Gh, run: u64) -> Request {
    Request::delete(gh.path(&format!("actions/runs/{run}")))
}

/// Start a `workflow_dispatch` run. `workflow` is the numeric id or the
/// file name (`deploy.yml`); `git_ref` a branch or tag name.
pub fn dispatch(
    gh: &Gh,
    workflow: &str,
    git_ref: &str,
    inputs: &BTreeMap<String, String>,
) -> Request {
    let inputs: serde_json::Map<String, Value> = inputs
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    Request::post(
        gh.path(&format!(
            "actions/workflows/{}/dispatches",
            encode(workflow)
        )),
        json!({ "ref": git_ref, "inputs": inputs }),
    )
}

/// Enable a disabled workflow.
pub fn enable_workflow(gh: &Gh, workflow: u64) -> Request {
    Request::new(
        Method::Put,
        gh.path(&format!("actions/workflows/{workflow}/enable")),
    )
}

/// Disable a workflow (no new runs start until it is enabled again).
pub fn disable_workflow(gh: &Gh, workflow: u64) -> Request {
    Request::new(
        Method::Put,
        gh.path(&format!("actions/workflows/{workflow}/disable")),
    )
}

/// Delete one artifact.
pub fn delete_artifact(gh: &Gh, artifact: u64) -> Request {
    Request::delete(gh.path(&format!("actions/artifacts/{artifact}")))
}

/// Delete one cache entry by id.
pub fn delete_cache(gh: &Gh, cache: u64) -> Request {
    Request::delete(gh.path(&format!("actions/caches/{cache}")))
}

/// Delete the caches with a key on one ref (every version of that key).
pub fn delete_caches_by_key(gh: &Gh, key: &str, git_ref: &str) -> Request {
    Request::delete(gh.path(&format!(
        "actions/caches?key={}&ref={}",
        encode(key),
        encode(git_ref)
    )))
}

// ---------------------------------------------------------------------------
// Read endpoints
// ---------------------------------------------------------------------------

/// The newest `per_page` runs of a workflow, optionally on one branch.
pub fn workflow_runs(gh: &Gh, workflow: u64, branch: Option<&str>, per_page: u32) -> String {
    let mut e = gh.path(&format!(
        "actions/workflows/{workflow}/runs?per_page={per_page}"
    ));
    if let Some(b) = branch.filter(|b| !b.trim().is_empty()) {
        e.push_str(&format!("&branch={}", encode(b.trim())));
    }
    e
}

/// The newest successful runs of a workflow on a branch created before
/// `before` (RFC 3339), for "compare with the last green run".
pub fn last_green(gh: &Gh, workflow: u64, branch: &str, before: &str) -> String {
    let mut e = gh.path(&format!(
        "actions/workflows/{workflow}/runs?status=success&per_page=5"
    ));
    if !branch.is_empty() {
        e.push_str(&format!("&branch={}", encode(branch)));
    }
    if !before.is_empty() {
        e.push_str(&format!("&created={}", encode(&format!("<{before}"))));
    }
    e
}

/// Artifacts of one run.
pub fn run_artifacts(gh: &Gh, run: u64) -> String {
    gh.path(&format!("actions/runs/{run}/artifacts?per_page=100"))
}

/// Every artifact of the repository (paged by the caller).
pub fn artifacts(gh: &Gh) -> String {
    gh.path("actions/artifacts")
}

pub fn cache_usage(gh: &Gh) -> String {
    gh.path("actions/cache/usage")
}

/// Cache entries, sorted by `last_accessed_at`, `size_in_bytes` or
/// `created_at`, newest / largest first (paged by the caller).
pub fn caches(gh: &Gh, sort: &str) -> String {
    gh.path(&format!("actions/caches?sort={sort}&direction=desc"))
}

/// Jobs of one attempt of a run.
pub fn attempt_jobs(gh: &Gh, run: u64, attempt: u32) -> String {
    gh.path(&format!(
        "actions/runs/{run}/attempts/{attempt}/jobs?per_page=100"
    ))
}

/// Commits and files between two commits (`base...head`).
pub fn compare(gh: &Gh, base: &str, head: &str) -> String {
    gh.path(&format!(
        "compare/{}...{}?per_page=100",
        encode(base),
        encode(head)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gh() -> Gh {
        Gh::new(".", "o", "r")
    }

    #[test]
    fn reruns_with_and_without_debug() {
        let r = rerun(&gh(), 42, false);
        assert_eq!(r.method, Method::Post);
        assert_eq!(r.endpoint, "repos/o/r/actions/runs/42/rerun");
        assert_eq!(r.body, None);
        assert_eq!(
            r.display(),
            "gh api --method POST repos/o/r/actions/runs/42/rerun"
        );
        let d = rerun(&gh(), 42, true);
        assert_eq!(d.body, Some(json!({"enable_debug_logging": true})));
        assert!(d.display().ends_with("--input - (JSON body)"));
        assert!(
            d.body_preview()
                .unwrap()
                .contains("\"enable_debug_logging\": true")
        );

        let f = rerun_failed(&gh(), 7, false);
        assert_eq!(f.endpoint, "repos/o/r/actions/runs/7/rerun-failed-jobs");
        assert_eq!(f.body, None);
        let fd = rerun_failed(&gh(), 7, true);
        assert_eq!(fd.method, Method::Post);
        assert_eq!(fd.body, Some(json!({"enable_debug_logging": true})));

        let j = rerun_job(&gh(), 99, false);
        assert_eq!(j.endpoint, "repos/o/r/actions/jobs/99/rerun");
        assert_eq!(j.method, Method::Post);
        assert_eq!(j.body, None);
        assert_eq!(
            rerun_job(&gh(), 99, true).body,
            Some(json!({"enable_debug_logging": true}))
        );
    }

    #[test]
    fn cancel_force_cancel_and_deletes() {
        let c = cancel(&gh(), 5);
        assert_eq!(
            (c.method, c.endpoint.as_str()),
            (Method::Post, "repos/o/r/actions/runs/5/cancel")
        );
        assert_eq!(c.body, None);
        let f = force_cancel(&gh(), 5);
        assert_eq!(
            (f.method, f.endpoint.as_str()),
            (Method::Post, "repos/o/r/actions/runs/5/force-cancel")
        );
        let l = delete_logs(&gh(), 5);
        assert_eq!(
            (l.method, l.endpoint.as_str()),
            (Method::Delete, "repos/o/r/actions/runs/5/logs")
        );
        assert_eq!(l.body, None);
        let d = delete_run(&gh(), 5);
        assert_eq!(
            (d.method, d.endpoint.as_str()),
            (Method::Delete, "repos/o/r/actions/runs/5")
        );
        assert_eq!(
            d.display(),
            "gh api --method DELETE repos/o/r/actions/runs/5"
        );
    }

    #[test]
    fn dispatch_body() {
        let mut inputs = BTreeMap::new();
        inputs.insert("level".to_owned(), "debug".to_owned());
        inputs.insert("dry_run".to_owned(), "true".to_owned());
        let r = dispatch(&gh(), "1234", "feature/x", &inputs);
        assert_eq!(r.method, Method::Post);
        assert_eq!(r.endpoint, "repos/o/r/actions/workflows/1234/dispatches");
        assert_eq!(
            r.body,
            Some(json!({"ref": "feature/x", "inputs": {"dry_run": "true", "level": "debug"}}))
        );
        let by_file = dispatch(&gh(), "deploy.yml", "main", &BTreeMap::new());
        assert_eq!(
            by_file.endpoint,
            "repos/o/r/actions/workflows/deploy.yml/dispatches"
        );
        assert_eq!(by_file.body, Some(json!({"ref": "main", "inputs": {}})));
    }

    #[test]
    fn enable_disable_workflow() {
        let e = enable_workflow(&gh(), 3);
        assert_eq!(
            (e.method, e.endpoint.as_str()),
            (Method::Put, "repos/o/r/actions/workflows/3/enable")
        );
        assert_eq!(e.body, None);
        let d = disable_workflow(&gh(), 3);
        assert_eq!(
            (d.method, d.endpoint.as_str()),
            (Method::Put, "repos/o/r/actions/workflows/3/disable")
        );
        assert_eq!(
            d.display(),
            "gh api --method PUT repos/o/r/actions/workflows/3/disable"
        );
    }

    #[test]
    fn artifact_and_cache_deletes() {
        let a = delete_artifact(&gh(), 11);
        assert_eq!(
            (a.method, a.endpoint.as_str()),
            (Method::Delete, "repos/o/r/actions/artifacts/11")
        );
        let c = delete_cache(&gh(), 12);
        assert_eq!(
            (c.method, c.endpoint.as_str()),
            (Method::Delete, "repos/o/r/actions/caches/12")
        );
        let k = delete_caches_by_key(&gh(), "node-cache-Linux-x64", "refs/pull/40994/merge");
        assert_eq!(k.method, Method::Delete);
        assert_eq!(
            k.endpoint,
            "repos/o/r/actions/caches?key=node-cache-Linux-x64&ref=refs%2Fpull%2F40994%2Fmerge"
        );
        assert_eq!(k.body, None);
    }

    #[test]
    fn read_endpoints() {
        assert_eq!(
            workflow_runs(&gh(), 9, None, 100),
            "repos/o/r/actions/workflows/9/runs?per_page=100"
        );
        assert_eq!(
            workflow_runs(&gh(), 9, Some("feature/a b"), 100),
            "repos/o/r/actions/workflows/9/runs?per_page=100&branch=feature%2Fa%20b"
        );
        assert_eq!(
            last_green(&gh(), 9, "main", "2026-10-26T19:05:21Z"),
            "repos/o/r/actions/workflows/9/runs?status=success&per_page=5&branch=main&created=%3C2026-10-26T19%3A05%3A21Z"
        );
        assert_eq!(
            attempt_jobs(&gh(), 5, 2),
            "repos/o/r/actions/runs/5/attempts/2/jobs?per_page=100"
        );
        assert_eq!(
            compare(&gh(), "abc", "def"),
            "repos/o/r/compare/abc...def?per_page=100"
        );
        assert_eq!(
            caches(&gh(), "size_in_bytes"),
            "repos/o/r/actions/caches?sort=size_in_bytes&direction=desc"
        );
        assert_eq!(
            run_artifacts(&gh(), 4),
            "repos/o/r/actions/runs/4/artifacts?per_page=100"
        );
    }
}
