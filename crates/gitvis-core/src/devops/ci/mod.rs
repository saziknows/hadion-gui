//! CI control for GitHub Actions: `workflow_dispatch` inputs parsed from
//! the workflow file and dispatching a run, re-runs with debug logging,
//! bulk cancel, artifacts (list, download, delete), caches (list, usage,
//! delete), per-workflow history (duration trend, success rate, queue time,
//! flaky jobs), comparing a failed run with the last green one (commits in
//! between, normalised log diff), workflow YAML validation (built-in rules
//! plus `actionlint` when installed) and local runs through `act`.
