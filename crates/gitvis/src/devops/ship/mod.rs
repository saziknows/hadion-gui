//! Releases and deployments: publish releases, environments and approvals,
//! where a commit is, DORA metrics.
//! Its tabs of the Ship page (`devops/pages.rs`): Releases, Environments, WhereIsMyCommit, Dora.
//!
//! The controller lives here (state, actions, replies); each tab's view
//! has its own file. Everything that touches GitHub or git goes through
//! [`Ctx`]; the computations are in `gitvis_core::devops::ship`.

#![warn(dead_code, unused_variables)]

mod dora_tab;
mod envs;
mod releases;
mod ui;
mod where_tab;

use std::collections::HashMap;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use gitvis_core::Oid;
use gitvis_core::devops::proc::{ProcSpec, render_args};
use gitvis_core::devops::ship::deploy::{self, Approval, Deployment, EnvData, EnvLocal};
use gitvis_core::devops::ship::dora::{self, Event, LocalHistory, Report};
use gitvis_core::devops::ship::local as ship_local;
use gitvis_core::devops::ship::notes::{self, Prepared};
use gitvis_core::devops::ship::release::{self, NewRelease, Release, ReleaseEdit};
use gitvis_core::devops::ship::whereis::{self, CommitPull, EnvDeploys, Located};
use gitvis_core::devops::tools::{self, Tool};

use crate::devops::console::{Console, ConsoleRun};
use crate::devops::ctx::Ctx;
use crate::devops::pages::ShipTab;
use crate::format;
use crate::state::{Action, RepoState, Selection, Settings, ToastKind};

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Something loaded in the background.
pub struct Load<T> {
    pub loading: bool,
    pub at: Option<Instant>,
    pub result: Option<Result<T, String>>,
}

impl<T> Default for Load<T> {
    fn default() -> Self {
        Self {
            loading: false,
            at: None,
            result: None,
        }
    }
}

impl<T> Load<T> {
    pub fn ok(&self) -> Option<&T> {
        self.result.as_ref().and_then(|r| r.as_ref().ok())
    }

    pub fn err(&self) -> Option<&str> {
        self.result
            .as_ref()
            .and_then(|r| r.as_ref().err())
            .map(String::as_str)
    }

    /// Never loaded and not loading.
    pub fn idle(&self) -> bool {
        !self.loading && self.result.is_none()
    }

    pub fn finish(&mut self, r: Result<T, String>) {
        self.loading = false;
        self.at = Some(Instant::now());
        self.result = Some(r);
    }

    fn started() -> Self {
        Self {
            loading: true,
            ..Default::default()
        }
    }
}

/// Where the notes in the publish form came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NotesFrom {
    #[default]
    Typed,
    GitHub,
    Local,
}

/// The publish form.
#[derive(Default)]
pub struct ReleaseForm {
    pub open: bool,
    pub tag: String,
    pub target: String,
    /// Previous tag the notes and suggestion start from (empty: the latest
    /// version tag).
    pub previous: String,
    pub title: String,
    pub notes: String,
    pub notes_from: NotesFrom,
    pub draft: bool,
    pub prerelease: bool,
    pub latest: bool,
    /// `(path, size in bytes)`.
    pub files: Vec<(String, u64)>,
    pub prepared: Load<Prepared>,
    /// `(target, previous)` the prepared data is for.
    pub prepared_for: (String, String),
    /// Put the local notes into the form when they arrive.
    pub fill_local: bool,
    pub generating: bool,
    pub gen_error: Option<String>,
    pub publishing: bool,
    pub error: Option<String>,
}

/// Editing a published release.
#[derive(Clone, Debug, Default)]
pub struct EditForm {
    pub id: u64,
    pub tag: String,
    pub name: String,
    pub body: String,
    pub draft: bool,
    pub prerelease: bool,
    /// 0 = leave as is, 1 = make latest, 2 = not latest.
    pub latest: u8,
    pub saving: bool,
}

/// What the DORA metrics count as a deployment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DoraSource {
    Environment(String),
    Releases,
    Tags(String),
    /// `(workflow id, name)`: successful runs on the default branch.
    Workflow(u64, String),
}

impl DoraSource {
    pub fn key(&self) -> String {
        match self {
            DoraSource::Environment(e) => format!("env:{e}"),
            DoraSource::Releases => "releases".to_owned(),
            DoraSource::Tags(p) => format!("tags:{p}"),
            DoraSource::Workflow(id, _) => format!("workflow:{id}"),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            DoraSource::Environment(e) => format!("deployments to {e}"),
            DoraSource::Releases => "published releases".to_owned(),
            DoraSource::Tags(p) => format!("tags matching {p}"),
            DoraSource::Workflow(_, n) => format!("runs of {n} on the default branch"),
        }
    }
}

#[derive(Default)]
pub struct DoraState {
    /// `None` until chosen (or until the default is resolved).
    pub source: Option<DoraSource>,
    pub period: u32,
    /// Source key the events are for.
    pub key: String,
    /// Prepared (sorted, folded) events.
    pub events: Load<Vec<Event>>,
    pub folded: usize,
    pub local: Load<LocalHistory>,
    pub report: Option<Report>,
    /// Text of the tag pattern input.
    pub tag_pattern: String,
    /// Whether the user picked the source (else it was a default).
    pub chosen: bool,
}

/// Per-repository state of the Ship tabs this area owns.
#[derive(Default)]
pub struct State {
    /// `gh` found on PATH (checked when a tab opens).
    pub gh_ok: Option<bool>,

    // Releases
    pub releases: Load<Vec<Release>>,
    pub latest_id: Option<u64>,
    pub pages: u32,
    pub more: bool,
    pub loading_more: bool,
    /// tag → (local commit, commits since the previous release).
    pub facts: notes::ReleaseFacts,
    pub facts_loading: bool,
    pub selected: Option<u64>,
    pub filter: String,
    pub form: ReleaseForm,
    pub edit: Option<EditForm>,
    /// Console runs of asset downloads.
    pub downloads: Vec<u64>,

    // Environments
    pub envs: Load<EnvData>,
    pub env_local: HashMap<String, EnvLocal>,
    pub env_local_loading: bool,
    /// The remote default branch as `origin/main` (from the local facts).
    pub default_branch: Option<String>,
    pub selected_env: Option<String>,
    pub history: HashMap<String, Load<Vec<Deployment>>>,
    pub approvals: Load<Vec<Approval>>,
    pub approval_comment: String,
    /// User-chosen pipeline order (persisted).
    pub order: Vec<String>,
    /// Promote target chosen in the environment detail.
    pub promote_to: String,

    // Where is my commit
    pub where_input: String,
    pub where_rev: Option<String>,
    pub where_local: Load<Located>,
    pub where_pulls: Load<Vec<CommitPull>>,
    /// The commit whose checks were asked for.
    pub where_checks: Option<Oid>,

    // DORA
    pub dora: DoraState,
}

/// Persisted per repository.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub env_order: Vec<String>,
    pub dora_source: Option<DoraSource>,
    pub dora_period: u32,
}

impl State {
    pub fn prefs(&self) -> Prefs {
        Prefs {
            env_order: self.order.clone(),
            dora_source: self.dora.chosen.then(|| self.dora.source.clone()).flatten(),
            dora_period: self.dora.period,
        }
    }

    pub fn apply_prefs(&mut self, p: &Prefs) {
        self.order = p.env_order.clone();
        if let Some(s) = &p.dora_source {
            self.dora.source = Some(s.clone());
            self.dora.chosen = true;
            if let DoraSource::Tags(t) = s {
                self.dora.tag_pattern = t.clone();
            }
        }
        self.dora.period = match p.dora_period {
            30 | 90 | 180 => p.dora_period,
            _ => 90,
        };
    }

    /// Releases as listed (filter applied), newest first.
    pub fn visible_releases(&self) -> Vec<&Release> {
        let needle = self.filter.trim().to_lowercase();
        self.releases
            .ok()
            .map(|list| {
                list.iter()
                    .filter(|r| {
                        needle.is_empty()
                            || r.tag_name.to_lowercase().contains(&needle)
                            || r.title().to_lowercase().contains(&needle)
                            || r.author_login().to_lowercase().contains(&needle)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn release(&self, id: u64) -> Option<&Release> {
        self.releases.ok()?.iter().find(|r| r.id == id)
    }

    /// Environment names in pipeline order.
    pub fn env_order(&self) -> Vec<String> {
        let Some(d) = self.envs.ok() else {
            return Vec::new();
        };
        let names: Vec<String> = d.environments.iter().map(|e| e.name.clone()).collect();
        deploy::order(&names, &self.order)
    }

    /// Deployments of one environment (list query plus loaded history),
    /// newest first.
    pub fn deployments_of(&self, env: &str) -> Vec<&Deployment> {
        self.envs
            .ok()
            .map(|d| {
                d.deployments
                    .iter()
                    .filter(|x| x.environment == env)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// What every environment runs, for the local checks.
    pub fn env_deploys(&self) -> Vec<EnvDeploys> {
        self.env_order()
            .into_iter()
            .map(|env| {
                let list = self.deployments_of(&env);
                let current = deploy::current(&list).map(|d| d.sha.clone());
                let successes = list
                    .iter()
                    .rev()
                    .filter(|d| d.effective().succeeded() && !d.sha.is_empty())
                    .map(|d| (d.id, d.sha.clone(), d.created))
                    .collect();
                EnvDeploys {
                    env,
                    current,
                    successes,
                }
            })
            .collect()
    }

    /// Pending approvals the user can give.
    pub fn approvable(&self) -> usize {
        self.approvals
            .ok()
            .map(|a| a.iter().map(|x| x.approvable().len()).sum())
            .unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// Actions and replies
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Act {
    /// Look for `gh` on PATH again.
    CheckTools,

    RefreshReleases,
    LoadMoreReleases,
    SelectRelease(Option<u64>),
    /// Select the N-th listed release (script).
    SelectReleaseIndex(usize),
    NewRelease,
    CloseForm,
    /// Re-read local history for the suggestion and notes.
    Prepare,
    /// Prepare (when needed), then put the local notes into the form.
    LocalNotes,
    GitHubNotes,
    SetFormTag(String),
    AddAssets,
    RemoveAsset(usize),
    Publish,
    PublishConfirmed(NewRelease),
    EditRelease(u64),
    /// Edit the selected release (script).
    EditSelected,
    CancelEdit,
    SaveEdit,
    SaveEditConfirmed(u64, ReleaseEdit),
    DeleteRelease {
        id: u64,
        with_tag: bool,
    },
    DeleteConfirmed {
        id: u64,
        tag: String,
        with_tag: bool,
    },
    /// Delete the selected release (script).
    DeleteSelected,
    Download {
        tag: String,
        asset: String,
    },
    /// Download the N-th asset of the selected release into `dir` without
    /// asking for a folder (script).
    DownloadTo {
        index: usize,
        dir: String,
    },
    JumpToTag(String),
    JumpTo(String),

    RefreshEnvs,
    SelectEnv(Option<String>),
    SelectEnvIndex(usize),
    MoveEnv(String, i32),
    ResetOrder,
    RefreshApprovals,
    Review {
        run: u64,
        envs: Vec<(u64, String)>,
        approve: bool,
    },
    ReviewConfirmed {
        run: u64,
        env_ids: Vec<u64>,
        approve: bool,
        comment: String,
    },
    /// Re-run the Actions run that produced a deployment.
    Rerun {
        run: u64,
        title: String,
        body: String,
    },
    RerunConfirmed {
        run: u64,
        label: String,
    },
    /// Ask for a deployment of `sha` to `env`.
    Deploy {
        sha: String,
        env: String,
        description: String,
        title: String,
    },
    DeployConfirmed {
        sha: String,
        env: String,
        description: String,
    },
    /// Roll the selected environment back (script): `rerun` picks the
    /// re-run mechanism, else a new deployment.
    RollbackSelected {
        rerun: bool,
    },
    /// Promote the selected environment's commit to `env` (script).
    PromoteSelected(String),
    /// Approve or reject what the user can approve on the N-th waiting run
    /// (script).
    ReviewIndex(usize, bool),

    /// Show where a commit is (commit context menu).
    WhereIs(Oid),
    WhereRev(String),
    WhereHead,
    WhereSelected,

    DoraSource(DoraSource),
    /// A workflow chosen by name (script).
    DoraWorkflowNamed(String),
    DoraPeriod(u32),
    DoraRefresh,
    /// Scroll the Where / DORA page to an offset in points (script).
    ScrollTo(f32),
}

pub enum Msg {
    Releases {
        page: u32,
        result: Result<(Vec<Release>, Option<u64>), String>,
    },
    Facts(notes::ReleaseFacts),
    Prepared {
        key: (String, String),
        result: Box<Result<Prepared, String>>,
    },
    Generated(Result<(String, String), String>),
    Published(Result<String, String>),
    /// A release was edited or deleted.
    ReleaseChanged(Result<String, String>),
    Envs(Box<Result<EnvData, String>>),
    EnvLocal {
        facts: HashMap<String, EnvLocal>,
        default: Option<String>,
    },
    History {
        env: String,
        result: Result<Vec<Deployment>, String>,
    },
    Approvals(Result<Vec<Approval>, String>),
    /// An approval, re-run or deployment request finished.
    EnvChanged(Result<String, String>),
    WhereLocal {
        rev: String,
        result: Box<Result<Located, String>>,
    },
    WherePulls {
        sha: String,
        result: Result<Vec<CommitPull>, String>,
    },
    DoraEvents {
        key: String,
        result: Result<Vec<Event>, String>,
    },
    DoraLocal {
        key: String,
        local: Box<LocalHistory>,
    },
}

pub(crate) fn count(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", format::count(n), if n == 1 { one } else { many })
}

impl Msg {
    /// One line for the activity log (`Err` when the work failed).
    pub fn summary(&self) -> Result<String, String> {
        match self {
            Msg::Releases { page, result } => result
                .as_ref()
                .map(|(r, _)| format!("{} (page {page})", count(r.len(), "release", "releases")))
                .map_err(Clone::clone),
            Msg::Facts(f) => Ok(format!(
                "{} found locally",
                count(
                    f.values().filter(|v| v.0.is_some()).count(),
                    "release tag",
                    "release tags"
                )
            )),
            Msg::Prepared { result, .. } => result
                .as_ref()
                .as_ref()
                .map(|p| {
                    format!(
                        "Suggested {} from {}",
                        p.suggestion.next,
                        count(p.suggestion.commits, "commit", "commits")
                    )
                })
                .map_err(Clone::clone),
            Msg::Generated(r) => r
                .as_ref()
                .map(|(_, b)| format!("GitHub wrote {} of notes", format::bytes(b.len())))
                .map_err(Clone::clone),
            Msg::Published(r) | Msg::ReleaseChanged(r) | Msg::EnvChanged(r) => r.clone(),
            Msg::Envs(r) => r
                .as_ref()
                .as_ref()
                .map(|d| {
                    format!(
                        "{}, {} of {}",
                        count(d.environments.len(), "environment", "environments"),
                        count(d.deployments.len(), "deployment", "deployments"),
                        format::count(d.total_deployments as usize)
                    )
                })
                .map_err(Clone::clone),
            Msg::EnvLocal { facts, .. } => Ok(format!(
                "Local facts for {}",
                count(facts.len(), "environment", "environments")
            )),
            Msg::History { env, result } => result
                .as_ref()
                .map(|h| format!("{} to {env}", count(h.len(), "deployment", "deployments")))
                .map_err(Clone::clone),
            Msg::Approvals(r) => r
                .as_ref()
                .map(|a| count(a.len(), "run waiting for review", "runs waiting for review"))
                .map_err(Clone::clone),
            Msg::WhereLocal { rev, result } => result
                .as_ref()
                .as_ref()
                .map(|l| {
                    format!(
                        "{rev}: {} branches, {} tags, {} environments checked",
                        l.local_branches.len() + l.remote_branches.len(),
                        l.tags_total,
                        l.envs.len()
                    )
                })
                .map_err(Clone::clone),
            Msg::WherePulls { result, .. } => result
                .as_ref()
                .map(|p| count(p.len(), "pull request", "pull requests"))
                .map_err(Clone::clone),
            Msg::DoraEvents { key, result } => result
                .as_ref()
                .map(|e| format!("{} for {key}", count(e.len(), "event", "events")))
                .map_err(Clone::clone),
            Msg::DoraLocal { local, .. } => Ok(format!(
                "New commits read for {}",
                count(local.new_commits.len(), "deployment", "deployments")
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

/// Days of deployments read for the DORA metrics: the longest period
/// (180 days) plus a margin, so the deployment before a period's start is
/// there for its lead time.
const DORA_WINDOW_DAYS: i64 = 180 + 60;
/// Events kept for the DORA metrics (newest).
const DORA_MAX_EVENTS: usize = 1_500;

/// Deployment-review comment used when none was typed.
const DEFAULT_COMMENT: &str = "Reviewed in gitvis";

fn gh_name(repo: &RepoState) -> Option<String> {
    repo.github_remote.as_ref().map(|(o, r)| format!("{o}/{r}"))
}

fn check_gh(st: &mut State) -> bool {
    *st.gh_ok.get_or_insert_with(|| tools::available(Tool::Gh))
}

/// The current branch, else HEAD's commit.
fn current_target(repo: &RepoState) -> String {
    let Some(snap) = repo.snapshot.as_ref() else {
        return "HEAD".to_owned();
    };
    match &snap.head {
        gitvis_core::Head::Branch { name, .. } | gitvis_core::Head::Unborn(name) => name.clone(),
        gitvis_core::Head::Detached(id) => id.to_string(),
    }
}

fn local_tags(repo: &RepoState) -> Vec<String> {
    repo.snapshot
        .as_ref()
        .map(|s| s.tags.iter().map(|t| t.name.clone()).collect())
        .unwrap_or_default()
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::CheckTools => {
            tools::refresh();
            repo.devops.ship.gh_ok = None;
            let tab = repo.devops.ship_tab;
            on_tab_shown(cx, repo, tab);
        }
        Act::RefreshReleases => fetch_releases(cx, repo, 1),
        Act::LoadMoreReleases => {
            let st = &repo.devops.ship;
            if st.more && !st.loading_more && !st.releases.loading {
                let next = st.pages + 1;
                fetch_releases(cx, repo, next);
            }
        }
        Act::SelectRelease(id) => {
            let st = &mut repo.devops.ship;
            st.selected = id;
            st.form.open = false;
            st.edit = None;
        }
        Act::SelectReleaseIndex(n) => {
            let id = repo.devops.ship.visible_releases().get(n).map(|r| r.id);
            if let Some(id) = id {
                apply(cx, repo, Act::SelectRelease(Some(id)));
            }
        }
        Act::NewRelease => {
            let target = current_target(repo);
            let st = &mut repo.devops.ship;
            st.edit = None;
            if !st.form.open {
                st.form = ReleaseForm {
                    open: true,
                    target,
                    latest: true,
                    ..Default::default()
                };
            }
            if st.form.prepared.idle() {
                prepare(cx, repo, false);
            }
            if repo.github_remote.is_some() && repo.devops.ship.releases.idle() {
                fetch_releases(cx, repo, 1);
            }
        }
        Act::CloseForm => repo.devops.ship.form.open = false,
        Act::Prepare => prepare(cx, repo, false),
        Act::LocalNotes => {
            let f = &repo.devops.ship.form;
            let fresh =
                !f.prepared.loading && f.prepared_for == (f.target.clone(), f.previous.clone());
            match f.prepared.ok().filter(|_| fresh).map(|p| p.notes.clone()) {
                Some(notes) => {
                    let f = &mut repo.devops.ship.form;
                    f.notes = notes;
                    f.notes_from = NotesFrom::Local;
                }
                None => prepare(cx, repo, true),
            }
        }
        Act::GitHubNotes => generate_notes(cx, repo),
        Act::SetFormTag(t) => {
            let f = &mut repo.devops.ship.form;
            let old = std::mem::replace(&mut f.tag, t.trim().to_owned());
            if f.title.trim().is_empty() || f.title == old {
                f.title = f.tag.clone();
            }
        }
        Act::AddAssets => {
            if let Some(files) = rfd::FileDialog::new()
                .set_title("Add release assets")
                .set_directory(&repo.workdir)
                .pick_files()
            {
                let f = &mut repo.devops.ship.form;
                for p in files {
                    let path = p.to_string_lossy().into_owned();
                    if f.files.iter().any(|(x, _)| *x == path) {
                        continue;
                    }
                    let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                    f.files.push((path, size));
                }
            }
        }
        Act::RemoveAsset(i) => {
            let f = &mut repo.devops.ship.form;
            if i < f.files.len() {
                f.files.remove(i);
            }
        }
        Act::Publish => confirm_publish(cx, repo),
        Act::PublishConfirmed(new) => publish(cx, repo, new),
        Act::EditRelease(id) => {
            if let Some(r) = repo.devops.ship.release(id) {
                let e = EditForm {
                    id,
                    tag: r.tag_name.clone(),
                    name: r.name.clone().unwrap_or_default(),
                    body: r.body.clone().unwrap_or_default(),
                    draft: r.draft,
                    prerelease: r.prerelease,
                    latest: 0,
                    saving: false,
                };
                let st = &mut repo.devops.ship;
                st.selected = Some(id);
                st.form.open = false;
                st.edit = Some(e);
            }
        }
        Act::EditSelected => {
            if let Some(id) = repo.devops.ship.selected {
                apply(cx, repo, Act::EditRelease(id));
            }
        }
        Act::CancelEdit => repo.devops.ship.edit = None,
        Act::SaveEdit => confirm_edit(cx, repo),
        Act::SaveEditConfirmed(id, edit) => save_edit(cx, repo, id, edit),
        Act::DeleteRelease { id, with_tag } => confirm_delete(cx, repo, id, with_tag),
        Act::DeleteSelected => {
            if let Some(id) = repo.devops.ship.selected {
                confirm_delete(cx, repo, id, false);
            }
        }
        Act::DeleteConfirmed { id, tag, with_tag } => delete_release(cx, repo, id, tag, with_tag),
        Act::Download { tag, asset } => {
            if let Some(dir) = rfd::FileDialog::new()
                .set_title(format!("Download {asset} to"))
                .pick_folder()
            {
                download(cx, repo, tag, asset, dir.to_string_lossy().into_owned());
            }
        }
        Act::DownloadTo { index, dir } => {
            let target = repo
                .devops
                .ship
                .selected
                .and_then(|id| repo.devops.ship.release(id))
                .and_then(|r| {
                    r.assets
                        .get(index)
                        .map(|a| (r.tag_name.clone(), a.name.clone()))
                });
            if let Some((tag, asset)) = target {
                download(cx, repo, tag, asset, dir);
            }
        }
        Act::JumpToTag(tag) => {
            let oid = repo
                .snapshot
                .as_ref()
                .and_then(|s| s.tags.iter().find(|t| t.name == tag).map(|t| t.target));
            match oid {
                Some(o) => cx.action(Action::JumpTo(o)),
                None => cx.toast(
                    ToastKind::Info,
                    format!("Tag {tag} is not in this repository: fetch tags to get it"),
                ),
            }
        }
        Act::JumpTo(sha) => match Oid::from_str(&sha) {
            Ok(o) => cx.action(Action::JumpTo(o)),
            Err(_) => cx.toast(ToastKind::Info, format!("{sha} is not a commit id")),
        },

        Act::RefreshEnvs => {
            repo.devops.ship.history.clear();
            fetch_envs(cx, repo);
            fetch_approvals(cx, repo);
        }
        Act::SelectEnv(env) => {
            repo.devops.ship.selected_env = env.clone();
            if let Some(e) = env {
                ensure_history(cx, repo, &e);
                let order = repo.devops.ship.env_order();
                let st = &mut repo.devops.ship;
                if st.promote_to.is_empty() || st.promote_to == e {
                    let pos = order.iter().position(|x| *x == e);
                    st.promote_to = pos
                        .and_then(|p| order.iter().skip(p + 1).find(|x| !deploy::is_side(x)))
                        .or_else(|| order.iter().find(|x| **x != e && !deploy::is_side(x)))
                        .cloned()
                        .unwrap_or_default();
                }
            }
        }
        Act::SelectEnvIndex(n) => {
            let env = repo.devops.ship.env_order().get(n).cloned();
            if env.is_some() {
                apply(cx, repo, Act::SelectEnv(env));
            }
        }
        Act::MoveEnv(env, delta) => {
            let mut order = repo.devops.ship.env_order();
            if let Some(i) = order.iter().position(|e| *e == env) {
                let j = (i as i64 + delta as i64).clamp(0, order.len() as i64 - 1) as usize;
                let e = order.remove(i);
                order.insert(j, e);
                repo.devops.ship.order = order;
                start_env_local(cx, repo);
            }
        }
        Act::ResetOrder => {
            repo.devops.ship.order.clear();
            start_env_local(cx, repo);
        }
        Act::RefreshApprovals => fetch_approvals(cx, repo),
        Act::Review { run, envs, approve } => confirm_review(cx, repo, run, envs, approve),
        Act::ReviewConfirmed {
            run,
            env_ids,
            approve,
            comment,
        } => {
            let label = format!(
                "{} deployment of run {run}",
                if approve { "Approve" } else { "Reject" }
            );
            cx.gh_change(label, move |gh, log| {
                let req = deploy::review_request(gh, run, &env_ids, approve, &comment);
                let r = gh
                    .send(&req, &mut log.calls)
                    .map(|_| {
                        format!(
                            "{} {} of run {run}",
                            if approve { "Approved" } else { "Rejected" },
                            count(env_ids.len(), "environment", "environments")
                        )
                    })
                    .map_err(|e| e.to_string());
                (r.clone(), Some(Msg::EnvChanged(r)))
            });
            repo.devops.ship.approval_comment.clear();
        }
        Act::Rerun { run, title, body } => {
            let Some(gh) = cx.gh.clone() else { return };
            let req = deploy::rerun_request(&gh, run);
            let danger = title.contains("Roll ");
            cx.confirm(
                title,
                body,
                "Re-run",
                danger,
                vec![req.display()],
                Act::RerunConfirmed {
                    run,
                    label: format!("Re-run run {run}"),
                }
                .into(),
            );
        }
        Act::RerunConfirmed { run, label } => {
            cx.gh_change(label, move |gh, log| {
                let r = gh
                    .send(&deploy::rerun_request(gh, run), &mut log.calls)
                    .map(|_| format!("Run {run} started again"))
                    .map_err(|e| e.to_string());
                (r.clone(), Some(Msg::EnvChanged(r)))
            });
        }
        Act::Deploy {
            sha,
            env,
            description,
            title,
        } => {
            let Some(gh) = cx.gh.clone() else { return };
            let req = deploy::create_deployment_request(&gh, &sha, &env, &description);
            let mut details = vec![req.display()];
            details.extend(req.body_preview());
            cx.confirm(
                title,
                format!(
                    "GitHub records a deployment of {} to {env} and sends the `deployment` event. Only a workflow with `on: deployment` (or an external deployer) acts on it; without one it stays pending.",
                    short(&sha)
                ),
                "Create deployment",
                deploy::is_production(&env),
                details,
                Act::DeployConfirmed {
                    sha,
                    env,
                    description,
                }
                .into(),
            );
        }
        Act::DeployConfirmed {
            sha,
            env,
            description,
        } => {
            let label = format!("Deploy {} to {env}", short(&sha));
            cx.gh_change(label, move |gh, log| {
                let req = deploy::create_deployment_request(gh, &sha, &env, &description);
                let r = gh
                    .send_json::<serde_json::Value>(&req, "deployment", &mut log.calls)
                    .map(|v| {
                        format!(
                            "Deployment {} created for {env}",
                            v.get("id").and_then(serde_json::Value::as_u64).unwrap_or(0)
                        )
                    })
                    .map_err(|e| e.to_string());
                (r.clone(), Some(Msg::EnvChanged(r)))
            });
        }
        Act::ReviewIndex(n, approve) => {
            let target = repo
                .devops
                .ship
                .approvals
                .ok()
                .and_then(|a| a.get(n))
                .map(|a| {
                    (
                        a.run.id,
                        a.approvable()
                            .iter()
                            .map(|p| (p.environment.id, p.environment.name.clone()))
                            .collect::<Vec<_>>(),
                    )
                });
            if let Some((run, envs)) = target.filter(|(_, e)| !e.is_empty()) {
                confirm_review(cx, repo, run, envs, approve);
            }
        }
        Act::RollbackSelected { rerun } => match envs::rollback_action(&repo.devops.ship, rerun) {
            Some(a) => apply(cx, repo, a),
            None => cx.toast(
                ToastKind::Info,
                "No earlier successful deployment to roll back to",
            ),
        },
        Act::PromoteSelected(to) => match envs::promote_action(&repo.devops.ship, &to, false) {
            Some(a) => apply(cx, repo, a),
            None => cx.toast(ToastKind::Info, "Nothing to promote: no current deployment"),
        },

        Act::WhereIs(oid) => {
            repo.devops.ship_tab = ShipTab::WhereIsMyCommit;
            where_start(cx, repo, oid.to_string());
        }
        Act::WhereRev(rev) => where_start(cx, repo, rev),
        Act::WhereHead => where_start(cx, repo, "HEAD".to_owned()),
        Act::WhereSelected => {
            let rev = match repo.selection {
                Selection::Commit(id) => id.to_string(),
                _ => "HEAD".to_owned(),
            };
            where_start(cx, repo, rev);
        }

        Act::DoraSource(s) => {
            let d = &mut repo.devops.ship.dora;
            if let DoraSource::Tags(p) = &s {
                d.tag_pattern = p.clone();
            }
            d.source = Some(s);
            d.chosen = true;
            dora_start(cx, repo, false);
        }
        Act::DoraWorkflowNamed(name) => {
            let found = repo.github.as_ref().and_then(|g| {
                let n = name.to_lowercase();
                g.workflows
                    .iter()
                    .find(|w| w.name.to_lowercase() == n || w.path.to_lowercase().ends_with(&n))
                    .or_else(|| {
                        g.workflows
                            .iter()
                            .find(|w| w.name.to_lowercase().contains(&n))
                    })
                    .map(|w| (w.id, w.name.clone()))
            });
            match found {
                Some((id, n)) => apply(cx, repo, Act::DoraSource(DoraSource::Workflow(id, n))),
                None => cx.toast(ToastKind::Info, format!("No workflow named {name}")),
            }
        }
        Act::DoraPeriod(p) => {
            repo.devops.ship.dora.period = p;
            recompute_dora(&mut repo.devops.ship);
            dora_start(cx, repo, false);
        }
        Act::ScrollTo(y) => crate::devops::pages::layout::scroll_next(y),
        Act::DoraRefresh => {
            if matches!(
                repo.devops.ship.dora.source,
                Some(DoraSource::Environment(_)) | None
            ) && !repo.devops.ship.dora.chosen
            {
                // Re-resolve the default (environments may have loaded).
                repo.devops.ship.dora.source = None;
            }
            dora_start(cx, repo, true);
        }
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    let tab = repo.devops.ship_tab;
    match msg {
        Msg::Releases { page, result } => {
            let st = &mut repo.devops.ship;
            st.loading_more = false;
            match result {
                Ok((list, latest)) => {
                    st.more = list.len() as u32 >= release::PAGE;
                    if page <= 1 {
                        st.latest_id = latest;
                        st.pages = 1;
                        st.releases.finish(Ok(list));
                    } else {
                        st.pages = page;
                        if let Some(Ok(all)) = st.releases.result.as_mut() {
                            for r in list {
                                if !all.iter().any(|x| x.id == r.id) {
                                    all.push(r);
                                }
                            }
                        }
                    }
                    start_facts(cx, repo);
                }
                Err(e) => {
                    if page <= 1 {
                        st.releases.finish(Err(e));
                    } else {
                        cx.toast(
                            ToastKind::Error,
                            format!("Could not load more releases: {e}"),
                        );
                    }
                }
            }
            if tab == ShipTab::Dora {
                dora_start(cx, repo, false);
            }
        }
        Msg::Facts(f) => {
            let st = &mut repo.devops.ship;
            st.facts_loading = false;
            st.facts.extend(f);
            // Releases loaded meanwhile (more pages) get their facts too.
            start_facts(cx, repo);
        }
        Msg::Prepared { key, result } => {
            let result = *result;
            let f = &mut repo.devops.ship.form;
            f.prepared_for = key;
            let fill = std::mem::take(&mut f.fill_local);
            if let Ok(p) = &result {
                if f.tag.trim().is_empty() {
                    f.tag = p.suggestion.next.clone();
                }
                if f.title.trim().is_empty() {
                    f.title = f.tag.clone();
                }
                if fill || (f.notes.trim().is_empty() && f.notes_from == NotesFrom::Typed) {
                    f.notes = p.notes.clone();
                    f.notes_from = NotesFrom::Local;
                }
            }
            f.prepared.finish(result);
        }
        Msg::Generated(r) => {
            let f = &mut repo.devops.ship.form;
            f.generating = false;
            match r {
                Ok((name, body)) => {
                    f.notes = body;
                    f.notes_from = NotesFrom::GitHub;
                    f.gen_error = None;
                    if (f.title.trim().is_empty() || f.title == f.tag) && !name.trim().is_empty() {
                        f.title = name;
                    }
                }
                Err(e) => f.gen_error = Some(e),
            }
        }
        Msg::Published(r) => {
            let f = &mut repo.devops.ship.form;
            f.publishing = false;
            match r {
                Ok(_) => {
                    f.open = false;
                    f.error = None;
                    fetch_releases(cx, repo, 1);
                }
                Err(e) => f.error = Some(e),
            }
        }
        Msg::ReleaseChanged(r) => {
            let st = &mut repo.devops.ship;
            if let Some(e) = st.edit.as_mut() {
                e.saving = false;
            }
            if let Ok(m) = &r {
                st.edit = None;
                if m.starts_with("Deleted")
                    && let Some(id) = st.selected.take()
                    && let Some(Ok(list)) = st.releases.result.as_mut()
                {
                    list.retain(|x| x.id != id);
                }
                fetch_releases(cx, repo, 1);
            }
        }
        Msg::Envs(r) => {
            let r = *r;
            let st = &mut repo.devops.ship;
            if let Ok(d) = &r
                && let Some(name) = &st.selected_env
                && !d.environments.iter().any(|e| &e.name == name)
            {
                st.selected_env = None;
            }
            // Keep loaded histories merged in.
            let r = r.map(|mut d| {
                for h in st.history.values() {
                    if let Some(list) = h.ok() {
                        d.merge(list.clone());
                    }
                }
                d
            });
            st.envs.finish(r);
            start_env_local(cx, repo);
            if let Some(e) = repo.devops.ship.selected_env.clone() {
                ensure_history(cx, repo, &e);
            }
            if tab == ShipTab::Dora {
                dora_start(cx, repo, false);
            }
            if repo.devops.ship.where_local.result.is_some()
                && let Some(rev) = repo.devops.ship.where_rev.clone()
            {
                where_local(cx, repo, rev);
            }
        }
        Msg::EnvLocal { facts, default } => {
            let st = &mut repo.devops.ship;
            st.env_local_loading = false;
            st.env_local = facts;
            st.default_branch = default;
        }
        Msg::History { env, result } => {
            let st = &mut repo.devops.ship;
            if let (Ok(list), Some(Ok(d))) = (&result, st.envs.result.as_mut()) {
                d.merge(list.clone());
            }
            st.history.entry(env).or_default().finish(result);
            start_env_local(cx, repo);
        }
        Msg::Approvals(r) => repo.devops.ship.approvals.finish(r),
        Msg::EnvChanged(r) => {
            if r.is_ok() {
                repo.devops.ship.history.clear();
                fetch_envs(cx, repo);
                fetch_approvals(cx, repo);
            }
        }
        Msg::WhereLocal { rev, result } => {
            let result = *result;
            let st = &mut repo.devops.ship;
            if st.where_rev.as_deref() != Some(rev.as_str()) {
                return;
            }
            let sha = result.as_ref().ok().map(|l| l.sha.clone());
            st.where_local.finish(result);
            if let Some(sha) = sha
                && st.where_checks.map(|o| o.to_string()) != Some(sha.clone())
            {
                where_remote(cx, repo, sha);
            }
        }
        Msg::WherePulls { sha, result } => {
            let st = &mut repo.devops.ship;
            if st.where_checks.map(|o| o.to_string()) == Some(sha) {
                st.where_pulls.finish(result);
            }
        }
        Msg::DoraEvents { key, result } => {
            let d = &mut repo.devops.ship.dora;
            if d.key != key {
                return;
            }
            match result {
                Ok(mut raw) => {
                    // Only the window the periods need (tags go back years).
                    let since = format::now_secs() - DORA_WINDOW_DAYS * 86_400;
                    raw.retain(|e| e.time >= since);
                    let n = raw.len();
                    let mut events = dora::prepare(raw);
                    d.folded = n - events.len();
                    if events.len() > DORA_MAX_EVENTS {
                        events.drain(..events.len() - DORA_MAX_EVENTS);
                    }
                    d.events.finish(Ok(events.clone()));
                    d.local = Load::started();
                    d.report = None;
                    let k = key.clone();
                    if !cx.read("Read deployed commits for DORA metrics", move |dir, log| {
                        let local = dora::collect_local(dir, &events, &mut log.commands);
                        Msg::DoraLocal {
                            key: k,
                            local: Box::new(local),
                        }
                    }) {
                        repo.devops.ship.dora.local = Load::default();
                    }
                }
                Err(e) => {
                    d.events.finish(Err(e));
                    d.report = None;
                }
            }
        }
        Msg::DoraLocal { key, local } => {
            let d = &mut repo.devops.ship.dora;
            if d.key != key {
                return;
            }
            d.local.finish(Ok(*local));
            recompute_dora(&mut repo.devops.ship);
        }
    }
}

/// A console run started by this area (or any other) ended.
pub fn on_console_done(cx: &mut Ctx, repo: &mut RepoState, run: &ConsoleRun) {
    if !repo.devops.ship.downloads.contains(&run.id) || !run.succeeded() {
        return;
    }
    let dir = run
        .spec
        .args
        .iter()
        .skip_while(|a| *a != "-D")
        .nth(1)
        .cloned()
        .unwrap_or_default();
    let what = run.title.trim_start_matches("Download ").to_owned();
    cx.toast(ToastKind::Success, format!("{what} saved to {dir}"));
}

/// One of this area's tabs was opened.
pub fn on_tab_shown(cx: &mut Ctx, repo: &mut RepoState, tab: ShipTab) {
    let has_gh = repo.github_remote.is_some() && check_gh(&mut repo.devops.ship);
    match tab {
        ShipTab::Releases => {
            if has_gh && repo.devops.ship.releases.idle() {
                fetch_releases(cx, repo, 1);
            }
        }
        ShipTab::Environments => {
            if has_gh && repo.devops.ship.envs.idle() {
                fetch_envs(cx, repo);
            }
            if has_gh && repo.devops.ship.approvals.idle() {
                fetch_approvals(cx, repo);
            }
        }
        ShipTab::WhereIsMyCommit => {
            if has_gh && repo.devops.ship.envs.idle() {
                fetch_envs(cx, repo);
            }
            if has_gh && repo.devops.ship.releases.idle() {
                fetch_releases(cx, repo, 1);
            }
            if repo.devops.ship.where_rev.is_none() {
                apply(cx, repo, Act::WhereSelected);
            }
        }
        ShipTab::Dora => {
            if has_gh && repo.devops.ship.envs.idle() {
                fetch_envs(cx, repo);
            }
            if has_gh && repo.devops.ship.releases.idle() {
                fetch_releases(cx, repo, 1);
            }
            dora_start(cx, repo, false);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Releases
// ---------------------------------------------------------------------------

fn fetch_releases(cx: &mut Ctx, repo: &mut RepoState, page: u32) {
    let st = &mut repo.devops.ship;
    if page <= 1 {
        if st.releases.loading {
            return;
        }
        st.releases.loading = true;
    } else {
        if st.loading_more {
            return;
        }
        st.loading_more = true;
    }
    let started = cx.gh_read(format!("Releases (page {page})"), move |gh, log| {
        let result = release::fetch_page(gh, page, &mut log.calls)
            .map(|list| {
                let latest = (page <= 1)
                    .then(|| release::fetch_latest_id(gh, &mut log.calls))
                    .flatten();
                (list, latest)
            })
            .map_err(|e| e.to_string());
        Msg::Releases { page, result }
    });
    if !started {
        let st = &mut repo.devops.ship;
        st.releases.loading = false;
        st.loading_more = false;
    }
}

/// Local facts for the listed releases: tag present, commits since the
/// previous release.
fn start_facts(cx: &mut Ctx, repo: &mut RepoState) {
    let st = &mut repo.devops.ship;
    if st.facts_loading {
        return;
    }
    let Some(list) = st.releases.ok() else { return };
    let pairs: Vec<(String, Option<String>)> = list
        .iter()
        .enumerate()
        .filter(|(_, r)| !st.facts.contains_key(&r.tag_name))
        .map(|(i, r)| {
            (
                r.tag_name.clone(),
                release::previous_of(list, i).map(|p| p.tag_name.clone()),
            )
        })
        .collect();
    if pairs.is_empty() {
        return;
    }
    st.facts_loading = true;
    if !cx.read("Release tags in local history", move |dir, log| {
        Msg::Facts(notes::release_facts(dir, &pairs, &mut log.commands))
    }) {
        repo.devops.ship.facts_loading = false;
    }
}

fn prepare(cx: &mut Ctx, repo: &mut RepoState, fill: bool) {
    let tags = local_tags(repo);
    let gh = repo.github_remote.clone();
    let target_default = current_target(repo);
    let f = &mut repo.devops.ship.form;
    if f.target.trim().is_empty() {
        f.target = target_default;
    }
    f.fill_local = f.fill_local || fill;
    f.prepared.loading = true;
    let key = (f.target.clone(), f.previous.clone());
    let (target, previous) = key.clone();
    cx.read("Suggest the next version and notes", move |dir, log| {
        let compare = gh.map(|(o, r)| {
            move |a: &str, b: &str| format!("https://github.com/{o}/{r}/compare/{a}...{b}")
        });
        let result = notes::prepare(
            dir,
            &tags,
            Some(previous.as_str()).filter(|p| !p.trim().is_empty()),
            &target,
            compare.as_ref().map(|f| f as &dyn Fn(&str, &str) -> String),
            &mut log.commands,
        )
        .map_err(|e| e.to_string());
        Msg::Prepared {
            key,
            result: Box::new(result),
        }
    });
}

fn generate_notes(cx: &mut Ctx, repo: &mut RepoState) {
    let f = &mut repo.devops.ship.form;
    if f.tag.trim().is_empty() {
        f.gen_error = Some("Choose a tag first: GitHub writes the notes for that tag.".into());
        return;
    }
    if f.generating {
        return;
    }
    let tag = f.tag.trim().to_owned();
    let target = f.target.trim().to_owned();
    let previous = if f.previous.trim().is_empty() {
        f.prepared.ok().and_then(|p| p.suggestion.base_tag.clone())
    } else {
        Some(f.previous.trim().to_owned())
    };
    f.generating = true;
    f.gen_error = None;
    let started = cx.gh_read(format!("Generate notes for {tag}"), move |gh, log| {
        let req = release::generate_notes_request(gh, &tag, &target, previous.as_deref());
        Msg::Generated(release::generate_notes(gh, &req, &mut log.calls).map_err(|e| e.to_string()))
    });
    if !started {
        let f = &mut repo.devops.ship.form;
        f.generating = false;
        f.gen_error = Some("This repository has no GitHub remote.".into());
    }
}

pub(crate) fn new_release(f: &ReleaseForm) -> NewRelease {
    NewRelease {
        tag: f.tag.trim().to_owned(),
        target: f.target.trim().to_owned(),
        title: f.title.trim().to_owned(),
        notes: f.notes.clone(),
        draft: f.draft,
        prerelease: f.prerelease,
        latest: f.latest && !f.draft && !f.prerelease,
        files: f.files.iter().map(|(p, _)| p.clone()).collect(),
    }
}

fn confirm_publish(cx: &mut Ctx, repo: &mut RepoState) {
    let Some(full) = gh_name(repo) else {
        cx.toast(ToastKind::Info, "This repository has no GitHub remote");
        return;
    };
    let tags = local_tags(repo);
    let f = &mut repo.devops.ship.form;
    let new = new_release(f);
    if let Some(p) = new.problem() {
        f.error = Some(p.to_owned());
        return;
    }
    f.error = None;
    let args = release::create_args(&full, &new);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut details = vec![format!("gh {}", render_args(&refs))];
    let lines: Vec<&str> = new.notes.lines().collect();
    details.push(format!(
        "# Release notes on standard input ({}, {}):",
        format::bytes(new.notes.len()),
        count(lines.len(), "line", "lines")
    ));
    for line in lines.iter().take(40) {
        details.push(format!("    {line}"));
    }
    if lines.len() > 40 {
        details.push(format!(
            "    … {} more",
            count(lines.len() - 40, "line", "lines")
        ));
    }
    let size: u64 = f.files.iter().map(|(_, s)| *s).sum();
    let exists = tags.contains(&new.tag);
    let body = format!(
        "{}{}{}",
        if exists {
            format!(
                "Tag {} exists: the release is published on it as it is. ",
                new.tag
            )
        } else {
            format!(
                "GitHub creates tag {} from {} and publishes the release. ",
                new.tag,
                if new.target.is_empty() {
                    "the default branch"
                } else {
                    &new.target
                }
            )
        },
        if new.draft {
            "It stays a draft, visible to collaborators only. "
        } else {
            "It is public at once. "
        },
        if new.files.is_empty() {
            String::new()
        } else {
            format!(
                "{} ({}) are uploaded.",
                count(new.files.len(), "asset", "assets"),
                format::bytes(size as usize)
            )
        }
    );
    cx.confirm(
        format!("Publish release {}?", new.tag),
        body,
        if new.draft { "Save draft" } else { "Publish" },
        false,
        details,
        Act::PublishConfirmed(new).into(),
    );
}

fn publish(cx: &mut Ctx, repo: &mut RepoState, new: NewRelease) {
    let f = &mut repo.devops.ship.form;
    if f.publishing {
        return;
    }
    f.publishing = true;
    let label = format!("Publish release {}", new.tag);
    let started = cx.gh_change(label, move |gh, log| {
        let r = release::create(gh, &new, &mut log.calls)
            .map(|url| {
                format!(
                    "{} {}{}",
                    if new.draft {
                        "Saved draft"
                    } else {
                        "Published"
                    },
                    new.tag,
                    if url.is_empty() {
                        String::new()
                    } else {
                        format!(": {url}")
                    }
                )
            })
            .map_err(|e| e.to_string());
        (r.clone(), Some(Msg::Published(r)))
    });
    if !started {
        repo.devops.ship.form.publishing = false;
    }
}

fn edit_of(e: &EditForm) -> ReleaseEdit {
    ReleaseEdit {
        name: e.name.trim().to_owned(),
        body: e.body.clone(),
        draft: e.draft,
        prerelease: e.prerelease,
        make_latest: match e.latest {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        },
    }
}

fn confirm_edit(cx: &mut Ctx, repo: &mut RepoState) {
    let (Some(gh), Some(e)) = (cx.gh.clone(), repo.devops.ship.edit.as_ref()) else {
        return;
    };
    let edit = edit_of(e);
    let req = release::edit_request(&gh, e.id, &edit);
    let mut details = vec![req.display()];
    details.extend(req.body_preview());
    cx.confirm(
        format!("Save release {}?", e.tag),
        "GitHub updates the release's title, notes and flags. The tag and the assets stay as they are.",
        "Save",
        false,
        details,
        Act::SaveEditConfirmed(e.id, edit).into(),
    );
}

fn save_edit(cx: &mut Ctx, repo: &mut RepoState, id: u64, edit: ReleaseEdit) {
    let tag = repo
        .devops
        .ship
        .release(id)
        .map(|r| r.tag_name.clone())
        .unwrap_or_default();
    if let Some(e) = repo.devops.ship.edit.as_mut() {
        e.saving = true;
    }
    cx.gh_change(format!("Edit release {tag}"), move |gh, log| {
        let r = gh
            .send(&release::edit_request(gh, id, &edit), &mut log.calls)
            .map(|_| format!("Saved release {tag}"))
            .map_err(|e| e.to_string());
        (r.clone(), Some(Msg::ReleaseChanged(r)))
    });
}

fn confirm_delete(cx: &mut Ctx, repo: &mut RepoState, id: u64, with_tag: bool) {
    let (Some(gh), Some(r)) = (cx.gh.clone(), repo.devops.ship.release(id)) else {
        return;
    };
    let tag = r.tag_name.clone();
    let mut details = vec![release::delete_request(&gh, id).display()];
    if with_tag {
        details.push(release::delete_tag_request(&gh, &tag).display());
    }
    cx.confirm(
        format!(
            "Delete release {}{}?",
            r.title(),
            if with_tag { " and its tag" } else { "" }
        ),
        format!(
            "{} The release page, its notes and {} ({} downloads so far) go away for good. This cannot be undone.",
            if with_tag {
                format!("Tag {tag} is deleted on GitHub too; your local tag stays.")
            } else {
                format!("Tag {tag} stays on GitHub.")
            },
            count(r.assets.len(), "asset", "assets"),
            format::count(r.downloads() as usize)
        ),
        if with_tag {
            "Delete release and tag"
        } else {
            "Delete release"
        },
        true,
        details,
        Act::DeleteConfirmed { id, tag, with_tag }.into(),
    );
}

fn delete_release(cx: &mut Ctx, repo: &mut RepoState, id: u64, tag: String, with_tag: bool) {
    repo.devops.ship.selected = Some(id);
    cx.gh_change(format!("Delete release {tag}"), move |gh, log| {
        let mut r = gh
            .send(&release::delete_request(gh, id), &mut log.calls)
            .map(|_| format!("Deleted release {tag}"))
            .map_err(|e| e.to_string());
        if with_tag && r.is_ok() {
            r = gh
                .send(&release::delete_tag_request(gh, &tag), &mut log.calls)
                .map(|_| format!("Deleted release {tag} and its tag"))
                .map_err(|e| format!("Deleted the release, but not the tag: {e}"));
        }
        (r.clone(), Some(Msg::ReleaseChanged(r)))
    });
}

fn download(cx: &mut Ctx, repo: &mut RepoState, tag: String, asset: String, dir: String) {
    let Some(full) = gh_name(repo) else { return };
    let spec = ProcSpec::new("gh", repo.workdir.clone())
        .args(release::download_args(&full, &tag, &asset, &dir));
    let id = cx.console(format!("Download {asset}"), spec);
    let st = &mut repo.devops.ship;
    st.downloads.push(id);
    if st.downloads.len() > 20 {
        st.downloads.remove(0);
    }
}

// ---------------------------------------------------------------------------
// Environments
// ---------------------------------------------------------------------------

fn fetch_envs(cx: &mut Ctx, repo: &mut RepoState) {
    let st = &mut repo.devops.ship;
    if st.envs.loading {
        return;
    }
    st.envs.loading = true;
    if !cx.gh_read("Environments and deployments", |gh, log| {
        Msg::Envs(Box::new(
            deploy::fetch(gh, &mut log.calls).map_err(|e| e.to_string()),
        ))
    }) {
        repo.devops.ship.envs.loading = false;
    }
}

fn fetch_approvals(cx: &mut Ctx, repo: &mut RepoState) {
    let st = &mut repo.devops.ship;
    if st.approvals.loading {
        return;
    }
    st.approvals.loading = true;
    if !cx.gh_read("Deployments waiting for review", |gh, log| {
        Msg::Approvals(deploy::fetch_approvals(gh, &mut log.calls).map_err(|e| e.to_string()))
    }) {
        repo.devops.ship.approvals.loading = false;
    }
}

fn ensure_history(cx: &mut Ctx, repo: &mut RepoState, env: &str) {
    let st = &mut repo.devops.ship;
    if st
        .envs
        .ok()
        .is_none_or(|d| d.source == deploy::Source::Rest)
    {
        return;
    }
    let h = st.history.entry(env.to_owned()).or_default();
    if !h.idle() {
        return;
    }
    h.loading = true;
    let e = env.to_owned();
    if !cx.gh_read(format!("Deployments to {env}"), move |gh, log| {
        let result =
            deploy::fetch_history(gh, &e, deploy::HISTORY_DEPLOYMENTS, 1, None, &mut log.calls)
                .map_err(|x| x.to_string());
        Msg::History { env: e, result }
    }) {
        repo.devops.ship.history.remove(env);
    }
}

/// Behind-default counts and differences between consecutive environments.
fn start_env_local(cx: &mut Ctx, repo: &mut RepoState) {
    let st = &mut repo.devops.ship;
    let Some(data) = st.envs.ok() else { return };
    let hint = data.default_branch.clone();
    let ordered: Vec<(String, String)> = st
        .env_deploys()
        .into_iter()
        .filter_map(|e| e.current.map(|c| (e.env, c)))
        .collect();
    st.env_local_loading = true;
    cx.read(
        "Compare deployed commits with local history",
        move |dir, log| {
            let default = ship_local::default_branch(dir, hint.as_deref(), &mut log.commands);
            let facts = deploy::local_facts(dir, &ordered, default.as_deref(), &mut log.commands);
            Msg::EnvLocal { facts, default }
        },
    );
}

fn confirm_review(
    cx: &mut Ctx,
    repo: &mut RepoState,
    run: u64,
    envs: Vec<(u64, String)>,
    approve: bool,
) {
    let Some(gh) = cx.gh.clone() else { return };
    let comment = {
        let c = repo.devops.ship.approval_comment.trim();
        if c.is_empty() {
            DEFAULT_COMMENT.to_owned()
        } else {
            c.to_owned()
        }
    };
    let ids: Vec<u64> = envs.iter().map(|(id, _)| *id).collect();
    let names: Vec<String> = envs.iter().map(|(_, n)| n.clone()).collect();
    let req = deploy::review_request(&gh, run, &ids, approve, &comment);
    let mut details = vec![req.display()];
    details.extend(req.body_preview());
    let run_name = repo
        .devops
        .ship
        .approvals
        .ok()
        .and_then(|a| a.iter().find(|x| x.run.id == run))
        .map(|a| format!("{} #{}", a.run.name, a.run.run_number))
        .unwrap_or_else(|| format!("Run {run}"));
    cx.confirm(
        format!(
            "{} the deployment to {}?",
            if approve { "Approve" } else { "Reject" },
            names.join(", ")
        ),
        if approve {
            format!("{run_name} continues and deploys to {}.", names.join(", "))
        } else {
            format!(
                "{run_name}'s jobs for {} fail and nothing is deployed there.",
                names.join(", ")
            )
        },
        if approve {
            "Approve and deploy"
        } else {
            "Reject"
        },
        !approve,
        details,
        Act::ReviewConfirmed {
            run,
            env_ids: ids,
            approve,
            comment,
        }
        .into(),
    );
}

// ---------------------------------------------------------------------------
// Where is my commit
// ---------------------------------------------------------------------------

fn where_start(cx: &mut Ctx, repo: &mut RepoState, rev: String) {
    let rev = rev.trim().to_owned();
    if rev.is_empty() {
        return;
    }
    let st = &mut repo.devops.ship;
    st.where_input = if rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit()) {
        rev[..10].to_owned()
    } else {
        rev.clone()
    };
    st.where_rev = Some(rev.clone());
    st.where_pulls = Load::default();
    st.where_checks = None;
    where_local(cx, repo, rev);
}

fn where_local(cx: &mut Ctx, repo: &mut RepoState, rev: String) {
    let envs: Vec<EnvDeploys> = repo
        .devops
        .ship
        .env_deploys()
        .into_iter()
        .filter(|e| !deploy::is_side(&e.env))
        .collect();
    repo.devops.ship.where_local.loading = true;
    cx.read(format!("Where is {rev}"), move |dir, log| {
        let result =
            whereis::locate(dir, &rev, &envs, &mut log.commands).map_err(|e| e.to_string());
        Msg::WhereLocal {
            rev,
            result: Box::new(result),
        }
    });
}

fn where_remote(cx: &mut Ctx, repo: &mut RepoState, sha: String) {
    let oid = Oid::from_str(&sha).ok();
    repo.devops.ship.where_checks = oid;
    if repo.github_remote.is_none() {
        return;
    }
    repo.devops.ship.where_pulls.loading = true;
    let s = sha.clone();
    if !cx.gh_read("Pull request of the commit", move |gh, log| {
        let result = whereis::fetch_pulls(gh, &s, &mut log.calls).map_err(|e| e.to_string());
        Msg::WherePulls { sha: s, result }
    }) {
        repo.devops.ship.where_pulls.loading = false;
    }
    if let Some(o) = oid {
        cx.action(Action::RefreshChecks(o));
    }
}

// ---------------------------------------------------------------------------
// DORA
// ---------------------------------------------------------------------------

/// The default source: a production-like environment, else published
/// releases, else version tags. `None` while what it depends on loads.
fn default_source(repo: &RepoState) -> Option<DoraSource> {
    let st = &repo.devops.ship;
    let tags = || {
        let has_v = repo.snapshot.as_ref().is_some_and(|s| {
            s.tags.iter().any(|t| {
                t.name.starts_with('v') && t.name[1..].starts_with(|c: char| c.is_ascii_digit())
            })
        });
        DoraSource::Tags(if has_v { "v*" } else { "*" }.to_owned())
    };
    if repo.github_remote.is_none() || st.gh_ok == Some(false) {
        return Some(tags());
    }
    if st.envs.loading || st.releases.loading {
        return None;
    }
    if let Some(d) = st.envs.ok() {
        let names: Vec<String> = d.environments.iter().map(|e| e.name.clone()).collect();
        // Production by name; else a production-like stage (`publish`,
        // `release`) that has deployments.
        let prod = names.iter().find(|n| deploy::is_production(n)).or_else(|| {
            names.iter().find(|n| {
                !deploy::is_side(n) && deploy::stage_rank(n) == 6 && d.of(n).next().is_some()
            })
        });
        if let Some(p) = prod {
            return Some(DoraSource::Environment(p.clone()));
        }
    }
    if st.releases.ok().is_some_and(|r| !r.is_empty()) {
        return Some(DoraSource::Releases);
    }
    Some(tags())
}

fn dora_start(cx: &mut Ctx, repo: &mut RepoState, force: bool) {
    if !matches!(repo.devops.ship.dora.period, 30 | 90 | 180) {
        repo.devops.ship.dora.period = 90;
    }
    let source = match repo.devops.ship.dora.source.clone() {
        Some(s) => s,
        None => match default_source(repo) {
            Some(s) => {
                let d = &mut repo.devops.ship.dora;
                if let DoraSource::Tags(p) = &s {
                    d.tag_pattern = p.clone();
                }
                d.source = Some(s.clone());
                s
            }
            None => return,
        },
    };
    let key = source.key();
    let d = &mut repo.devops.ship.dora;
    if !force && d.key == key && (d.events.loading || d.events.result.is_some()) {
        return;
    }
    d.key = key.clone();
    d.events = Load::started();
    d.local = Load::default();
    d.report = None;
    // Read back to the longest period plus a margin, so the deployment
    // before the period start is there (lead time needs it).
    let since = format::now_secs() - DORA_WINDOW_DAYS * 86_400;
    let branch = repo
        .devops
        .ship
        .envs
        .ok()
        .and_then(|d| d.default_branch.clone())
        .or_else(|| {
            repo.devops
                .ship
                .default_branch
                .as_ref()
                .map(|b| b.trim_start_matches("origin/").to_owned())
        })
        .unwrap_or_else(|| "main".to_owned());
    let started = match source {
        DoraSource::Environment(env) => {
            cx.gh_read(format!("Deployments to {env} for DORA"), move |gh, log| {
                let result = deploy::fetch_history(gh, &env, 100, 6, Some(since), &mut log.calls)
                    .map(|list| dora::events_from_deployments(&list))
                    .map_err(|e| e.to_string());
                Msg::DoraEvents { key, result }
            })
        }
        DoraSource::Releases => cx.gh_read("Releases for DORA", move |gh, log| {
            let result = release::fetch_many(gh, 3, &mut log.calls)
                .map(|list| dora::events_from_releases(&list, false))
                .map_err(|e| e.to_string());
            Msg::DoraEvents { key, result }
        }),
        DoraSource::Tags(pattern) => cx.read(
            format!("Tags matching {pattern} for DORA"),
            move |dir, log| {
                let result = dora::events_from_tags(dir, &pattern, &mut log.commands)
                    .map_err(|e| e.to_string());
                Msg::DoraEvents { key, result }
            },
        ),
        DoraSource::Workflow(id, name) => cx.gh_read(
            format!("Runs of {name} on {branch} for DORA"),
            move |gh, log| {
                let result = dora::fetch_run_events(gh, id, &branch, since, &mut log.calls)
                    .map_err(|e| e.to_string());
                Msg::DoraEvents { key, result }
            },
        ),
    };
    if !started {
        repo.devops.ship.dora.events.finish(Err(
            "This repository has no GitHub remote: count tags instead.".into(),
        ));
    }
}

fn recompute_dora(st: &mut State) {
    let d = &mut st.dora;
    let (Some(events), Some(local)) = (d.events.ok(), d.local.ok()) else {
        return;
    };
    d.report = Some(dora::compute(
        events,
        local,
        d.period.max(1),
        format::now_secs(),
        d.folded,
    ));
}

// ---------------------------------------------------------------------------
// Script, badge, views
// ---------------------------------------------------------------------------

/// `GITVIS_SCRIPT` commands this area understands: `release N`,
/// `release-more`, `new-release`, `release-tag TAG`, `notes local|github`, `publish`,
/// `edit-release`, `delete-release`, `download N DIR` (N-th asset of the
/// selected release), `where SHA|head|selected`, `env N`,
/// `rollback [rerun]`, `promote ENV`, `approve N`, `reject N`,
/// `dora-source env NAME|releases|tags
/// PATTERN|workflow NAME`, `dora-period 30|90|180`, `ship-scroll Y` (Where
/// and DORA pages), `explain open|closed` (every folded explanation of the
/// Ship, Govern and Tools pages drawn open). Writes stop at their
/// confirmation dialog.
pub fn script(cmd: &str, arg: &str) -> Option<Vec<Action>> {
    let arg = arg.trim();
    let num = || arg.parse::<usize>().ok();
    if cmd == "explain" {
        // `explain open|closed`: draw every folded explanation of the Ship,
        // Govern and Tools pages open, for screenshots.
        crate::devops::pages::explain::set_open_all(arg != "closed");
        return Some(Vec::new());
    }
    let (tab, a) = match cmd {
        "release" => (ShipTab::Releases, Act::SelectReleaseIndex(num()?)),
        "new-release" => (ShipTab::Releases, Act::NewRelease),
        "release-more" => (ShipTab::Releases, Act::LoadMoreReleases),
        "release-tag" => (ShipTab::Releases, Act::SetFormTag(arg.to_owned())),
        "notes" => (
            ShipTab::Releases,
            if arg == "github" {
                Act::GitHubNotes
            } else {
                Act::LocalNotes
            },
        ),
        "publish" => (ShipTab::Releases, Act::Publish),
        "edit-release" => (ShipTab::Releases, Act::EditSelected),
        "delete-release" => (ShipTab::Releases, Act::DeleteSelected),
        "download" => {
            let (n, dir) = arg.split_once(' ')?;
            (
                ShipTab::Releases,
                Act::DownloadTo {
                    index: n.trim().parse().ok()?,
                    dir: dir.trim().to_owned(),
                },
            )
        }
        "where" => (
            ShipTab::WhereIsMyCommit,
            match arg {
                "" | "selected" => Act::WhereSelected,
                "head" | "HEAD" => Act::WhereHead,
                rev => Act::WhereRev(rev.to_owned()),
            },
        ),
        "env" => (ShipTab::Environments, Act::SelectEnvIndex(num()?)),
        "rollback" => (
            ShipTab::Environments,
            Act::RollbackSelected {
                rerun: arg == "rerun",
            },
        ),
        "promote" => (ShipTab::Environments, Act::PromoteSelected(arg.to_owned())),
        "approve" => (ShipTab::Environments, Act::ReviewIndex(num()?, true)),
        "reject" => (ShipTab::Environments, Act::ReviewIndex(num()?, false)),
        "dora-source" => {
            let (kind, rest) = arg.split_once(' ').unwrap_or((arg, ""));
            let rest = rest.trim();
            let a = match kind {
                "env" | "environment" => Act::DoraSource(DoraSource::Environment(rest.to_owned())),
                "releases" => Act::DoraSource(DoraSource::Releases),
                "tags" => Act::DoraSource(DoraSource::Tags(
                    if rest.is_empty() { "*" } else { rest }.to_owned(),
                )),
                "workflow" => Act::DoraWorkflowNamed(rest.to_owned()),
                _ => return None,
            };
            (ShipTab::Dora, a)
        }
        "ship-scroll" => {
            let y: f32 = arg.parse().ok()?;
            return Some(vec![Act::ScrollTo(y).into()]);
        }
        "dora-period" => match num()? {
            n @ (30 | 90 | 180) => (ShipTab::Dora, Act::DoraPeriod(n as u32)),
            _ => return None,
        },
        _ => return None,
    };
    // The act first (so opening the tab does not start a default lookup
    // it would replace), then the tab, then the page.
    Some(vec![
        a.into(),
        Action::DevOps(crate::devops::DevOpsAction::SetShipTab(tab)),
        Action::ShowPage(crate::state::Page::Ship),
    ])
}

pub fn show_tab(
    ui: &mut egui::Ui,
    repo: &mut RepoState,
    tab: ShipTab,
    console: &Console,
    settings: &Settings,
    actions: &mut Vec<Action>,
) {
    match tab {
        ShipTab::Releases => releases::show(ui, repo, console, settings, actions),
        ShipTab::Environments => envs::show(ui, repo, settings, actions),
        ShipTab::WhereIsMyCommit => where_tab::show(ui, repo, settings, actions),
        ShipTab::Dora => dora_tab::show(ui, repo, settings, actions),
        _ => {}
    }
}

/// A dot on the rail entry and a line for its tooltip, when this area
/// needs attention (e.g. pending approvals, critical alerts, a running
/// task). `None` when nothing does.
pub fn rail_badge(repo: &RepoState, _console: &Console) -> Option<(crate::theme::Tone, String)> {
    let n = repo.devops.ship.approvable();
    (n > 0).then(|| {
        (
            crate::theme::Tone::Warning,
            format!(
                "{} waiting for your approval",
                count(n, "deployment", "deployments")
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::DevOpsAction;

    fn ship_act(actions: &[Action]) -> Option<&Act> {
        actions.iter().find_map(|a| match a {
            Action::DevOps(DevOpsAction::Ship(x)) => Some(x),
            _ => None,
        })
    }

    #[test]
    fn script_commands() {
        let a = script("release", "2").unwrap();
        assert!(matches!(ship_act(&a), Some(Act::SelectReleaseIndex(2))));
        assert!(
            a.iter()
                .any(|x| matches!(x, Action::ShowPage(crate::state::Page::Ship)))
        );
        assert!(script("release", "x").is_none());
        assert!(script("dora-period", "45").is_none());
        let a = script("dora-period", "180").unwrap();
        assert!(matches!(ship_act(&a), Some(Act::DoraPeriod(180))));
        let a = script("dora-source", "tags v*").unwrap();
        assert!(matches!(ship_act(&a), Some(Act::DoraSource(DoraSource::Tags(p))) if p == "v*"));
        let a = script("dora-source", "env production").unwrap();
        assert!(
            matches!(ship_act(&a), Some(Act::DoraSource(DoraSource::Environment(e))) if e == "production")
        );
        let a = script("dora-source", "workflow Deploy app").unwrap();
        assert!(matches!(ship_act(&a), Some(Act::DoraWorkflowNamed(n)) if n == "Deploy app"));
        assert!(script("dora-source", "nonsense").is_none());
        let a = script("where", "head").unwrap();
        assert!(matches!(ship_act(&a), Some(Act::WhereHead)));
        let a = script("where", "abc123").unwrap();
        assert!(matches!(ship_act(&a), Some(Act::WhereRev(r)) if r == "abc123"));
        let a = script("download", "1 C:/tmp/out dir").unwrap();
        assert!(
            matches!(ship_act(&a), Some(Act::DownloadTo { index: 1, dir }) if dir == "C:/tmp/out dir")
        );
        let a = script("approve", "0").unwrap();
        assert!(matches!(ship_act(&a), Some(Act::ReviewIndex(0, true))));
        let a = script("rollback", "rerun").unwrap();
        assert!(matches!(
            ship_act(&a),
            Some(Act::RollbackSelected { rerun: true })
        ));
        assert!(script("unknown-command", "").is_none());
    }

    #[test]
    fn prefs_round_trip() {
        let mut st = State {
            order: vec!["staging".into(), "production".into()],
            dora: DoraState {
                source: Some(DoraSource::Workflow(7, "Deploy".into())),
                chosen: true,
                period: 30,
                ..Default::default()
            },
            ..Default::default()
        };
        let json = serde_json::to_string(&st.prefs()).unwrap();
        let p: Prefs = serde_json::from_str(&json).unwrap();
        let mut back = State::default();
        back.apply_prefs(&p);
        assert_eq!(back.order, st.order);
        assert_eq!(back.dora.source, st.dora.source);
        assert_eq!(back.dora.period, 30);
        // A default (not chosen) source is not persisted; odd periods reset.
        st.dora.chosen = false;
        assert!(st.prefs().dora_source.is_none());
        let p: Prefs = serde_json::from_str(r#"{"dora_period": 7}"#).unwrap();
        back.apply_prefs(&p);
        assert_eq!(back.dora.period, 90);
    }

    #[test]
    fn load_states_and_source_keys() {
        let mut l: Load<u32> = Load::default();
        assert!(l.idle());
        l.loading = true;
        assert!(!l.idle());
        l.finish(Err("boom".into()));
        assert_eq!(l.err(), Some("boom"));
        assert!(l.ok().is_none());
        l.finish(Ok(3));
        assert_eq!(l.ok(), Some(&3));
        assert_ne!(
            DoraSource::Tags("v*".into()).key(),
            DoraSource::Tags("*".into()).key()
        );
        assert_eq!(DoraSource::Releases.describe(), "published releases");
    }
}
