//! Releases and deployments on GitHub: releases (list with assets and
//! downloads, publish from a tag with generated or local changelog, upload
//! assets, draft / pre-release / latest), environments and deployments
//! (what is deployed where, deployment statuses, pending approvals,
//! approve / reject, promote, roll back by redeploying), "where is my
//! commit" (branches, pull request, tags and releases, environments, CI)
//! and DORA metrics (deployment frequency, lead time for changes, change
//! failure rate, time to restore) computed from deployments, releases or
//! tags plus local history.
//!
//! - [`release`]: the release model, reads, and the writes as builders
//!   (`gh release create` arguments, PATCH / DELETE requests).
//! - [`notes`]: next-version suggestion and grouped release notes from
//!   local conventional commits.
//! - [`deploy`]: environments and deployments (one GraphQL query, REST
//!   fallback), approvals, re-run and create-deployment builders,
//!   pipeline order, local facts per environment.
//! - [`whereis`]: the checkpoints of one commit.
//! - [`dora`]: the four DORA metrics, pure, with their bands.

pub mod deploy;
pub mod dora;
pub mod local;
pub mod notes;
pub mod release;
pub mod whereis;
