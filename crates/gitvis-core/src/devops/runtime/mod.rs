//! What is running: container packages on GHCR mapped to commits and tags,
//! Kubernetes through `kubectl` (contexts, namespaces, deployments with
//! images matched to commits / branches / tags, rollout status, pods, logs,
//! rollout restart and undo) and Terraform / OpenTofu (roots in the
//! repository, which roots a change touches, `validate`, `fmt -check`,
//! `plan` with a parsed summary, drift through `plan -refresh-only`).
