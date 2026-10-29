//! Packaging: update checks against GitHub releases, downloads verified
//! with SHA-256 and swapped in on restart, crash reports (a panic hook
//! writing a report file), installer and release-build helpers.
//!
//! - [`version`]: semver precedence for tags and the running version.
//! - [`buildinfo`]: what the app's `build.rs` embeds (commit, dirty flag,
//!   date, target, profile), with fallbacks.
//! - [`release`]: GitHub release JSON, the asset contract
//!   (`hadion-<version>-<os>-<arch>.zip` or `.exe`, plus `SHA256SUMS`),
//!   channels, and which releases are newer.
//! - [`checksum`]: `SHA256SUMS` parsing and SHA-256 of files.
//! - [`zip`]: taking the executable out of a release zip.
//! - [`update`]: the download → verify → extract → stage pipeline (through
//!   `gh release download`, or a local test folder) and the update log.
//! - [`swap`]: replacing the running executable (`.new` / `.old`) with
//!   rollback.
//! - [`crash`]: crash report text, redaction through the exposure scanner,
//!   files on disk.
//! - [`issue`]: prefilled GitHub "new issue" links, cut to a safe length.
//! - [`paths`]: the data, crash and download folders (with test overrides).
//! - [`migrate`]: settings and data carried over from gitvis, the earlier name.
//! - [`time`]: UTC timestamps without a date library.
//!
//! Nothing here goes online by itself: checks and downloads run only when
//! the app asks (a button, or "check on start" which is off by default).

pub mod buildinfo;
pub mod checksum;
pub mod crash;
pub mod issue;
pub mod migrate;
pub mod paths;
pub mod release;
pub mod swap;
pub mod time;
pub mod update;
pub mod version;
pub mod zip;
