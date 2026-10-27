//! Automation: desktop notifications (opt-in polling of runs and review
//! requests), repository groups and batch operations, issue-tracker links
//! (Jira / Linear keys in branches, commits and pull requests), release
//! helpers (version bumps in manifests, CHANGELOG.md).
//!
//! - [`notify`]: the poll (notifications, the inbox search, runs per open
//!   repository), the diff between two polls, quiet hours, delivery through
//!   `notify-rust` (`GITVIS_NOTIFY=log` writes a file instead).
//! - [`groups`]: groups of repositories, their status
//!   (`git status --porcelain=v2 --branch`), fetch / pull / branch in each.
//! - [`links`]: key detection without false positives (`UTF-8`, `SHA-256`),
//!   tracker URLs, custom patterns, key suggestions from history.
//! - [`lookup`]: optional title / status lookup through `curl --config -`
//!   (credentials only on standard input).
//! - [`release`]: version manifests (detect, bump keeping the formatting),
//!   Keep a Changelog sections, the release commit.

pub mod groups;
pub mod links;
pub mod lookup;
pub mod notify;
pub mod release;
