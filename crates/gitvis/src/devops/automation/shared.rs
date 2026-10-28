//! State that views outside the Settings page and the status bar need: the
//! Home page's groups view, the Ship publish form's release section and the
//! hooks (commit panel, pull requests, WIP panel) are drawn with their own
//! arguments and cannot reach `Globals.automation`. They read (and edit
//! UI-only fields of) this one process-wide value instead; `apply` /
//! `on_msg` write it.
//!
//! Rules: lock it for one short block at a time, never while it is already
//! locked, and never from a background task (tasks return messages).
//! At integration this can move into `automation::State` once the Home and
//! Ship views receive it (see the area's report).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use gitvis_core::devops::automation::groups::{Batch, RepoResult, RepoStatus};
use gitvis_core::devops::automation::links::Tracker;
use gitvis_core::devops::automation::lookup::IssueInfo;
use gitvis_core::devops::automation::release::Detected;

/// A title lookup.
#[derive(Clone, Debug)]
pub enum Lookup {
    Loading,
    Found(IssueInfo),
    Failed(String),
}

/// A status read of a repository that is not open in gitvis.
#[derive(Clone, Debug)]
pub struct StatusEntry {
    pub result: Result<RepoStatus, String>,
    pub at: Instant,
}

/// A batch operation that is running.
pub struct BatchRun {
    pub group: String,
    pub batch: Batch,
    pub names: Vec<String>,
    pub started: Instant,
    pub progress: Arc<Mutex<Progress>>,
}

#[derive(Default)]
pub struct Progress {
    pub current: Option<String>,
    pub done: Vec<RepoResult>,
}

/// The last finished batch.
#[derive(Clone)]
pub struct BatchDone {
    pub group: String,
    pub batch: Batch,
    pub results: Vec<RepoResult>,
    pub elapsed_ms: u64,
    pub at: i64,
}

/// The Home page's Groups section.
#[derive(Default)]
pub struct GroupsRt {
    /// The group shown (by name).
    pub selected: Option<String>,
    pub new_name: String,
    /// The new-group form is open.
    pub creating: bool,
    /// `(old name, edited name)` while renaming.
    pub rename: Option<(String, String)>,
    /// `(branch, base)` of the open create-branch form.
    pub branch_form: Option<(String, String)>,
    /// Status of repositories that are not open, by path.
    pub status: HashMap<PathBuf, StatusEntry>,
    pub status_loading: bool,
    pub status_at: Option<Instant>,
    pub batch: Option<BatchRun>,
    pub last: Option<BatchDone>,
}

/// What the release section found in a repository.
#[derive(Clone, Debug)]
pub struct Found {
    pub manifests: Vec<Detected>,
    /// `(path, content)` of the root CHANGELOG.md.
    pub changelog: Option<(String, String)>,
    pub at: Instant,
}

/// The Ship publish form's release section, per repository.
#[derive(Default)]
pub struct ReleaseRt {
    pub open: bool,
    pub loading: bool,
    pub found: Option<Result<Found, String>>,
    /// Explicit choice of `(path, field label)`; `None`: the default.
    pub checked: Option<HashSet<(String, String)>>,
    pub changelog: Option<bool>,
    pub create_changelog: bool,
    pub use_unreleased: bool,
    /// The commit message being edited (`None`: from the template).
    pub message: Option<String>,
    /// The message template from the settings (`{version}`, `{tag}`).
    pub template: Option<String>,
    pub preparing: bool,
    pub committing: bool,
    pub error: Option<String>,
    /// `abc1234 chore(release): v1.2.0` after the commit.
    pub committed: Option<String>,
    /// Scroll the section into view on the next frame.
    pub reveal: bool,
}

#[derive(Default)]
pub struct Shared {
    pub groups: GroupsRt,
    pub release: HashMap<PathBuf, ReleaseRt>,
    pub lookups: HashMap<(Tracker, String), Lookup>,
    /// Script: the next automation page drawn scrolls to its end.
    pub scroll_end: bool,
}

/// Take the one-shot "scroll to the end" request.
pub fn take_scroll_end() -> bool {
    std::mem::take(&mut lock().scroll_end)
}

/// The shared value. Recovers from a poisoned lock (a panicking view must
/// not take the area down with it).
pub fn lock() -> MutexGuard<'static, Shared> {
    static S: OnceLock<Mutex<Shared>> = OnceLock::new();
    S.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}
