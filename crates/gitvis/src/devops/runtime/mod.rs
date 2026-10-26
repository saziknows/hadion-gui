//! What is running: GHCR packages, Kubernetes, Terraform / OpenTofu.
//! Its tabs of the Ship page (`devops/pages.rs`): Packages, Kubernetes,
//! Terraform.
//!
//! - Packages: the owner's container packages (`users|orgs/{owner}/packages`),
//!   those linked to this repository first, versions with their tags matched
//!   to commits, git tags and branch tips; `docker pull` lines; deleting a
//!   version behind a confirmation. A token without `read:packages` gets an
//!   explanation and the exact `gh auth refresh` line to copy.
//! - Kubernetes: contexts and namespaces from the kubeconfig (never
//!   changed), workloads with rollout state, images matched to commits, pods,
//!   events, rollout history, logs in the console; restart / undo / scale
//!   behind confirmations.
//! - Terraform: roots of the repository, which ones the current changes
//!   touch, `fmt -check`, `init -backend=false`, `validate -json`, `plan`
//!   with a parsed summary and `show -json`, drift through `plan
//!   -refresh-only`.
//!
//! Every network call happens when a tab opens (once, then cached with its
//! time) or on an explicit click.

mod common;
mod kube_view;
mod packages_view;
mod tf_view;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use gitvis_core::devops::runtime::kube::{
    self, DetailLoad, Event, KubeConfig, KubeWrite, LogOpts, NamespaceLoad, Pod, Target, Workload,
    WorkloadKind,
};
use gitvis_core::devops::runtime::matching::{self, Distance, RefIndex};
use gitvis_core::devops::runtime::packages::{self, Listing, OwnerKind, Package, PackageVersion};
use gitvis_core::devops::runtime::terraform::{
    self, Binary, Command, DriftOutcome, PlanJson, PlanOutcome, PlanText, Scan, Validation,
};
use gitvis_core::devops::tools::Installed;
use gitvis_core::{Oid, Snapshot};

use crate::devops::console::{Console, ConsoleRun};
use crate::devops::ctx::Ctx;
use crate::devops::pages::ShipTab;
use crate::state::{Action, RepoState, Settings, ToastKind};
use crate::theme::Tone;

/// Reload a tab's data when it is opened and the last fetch is older.
const STALE_SECS: i64 = 120;

fn now() -> i64 {
    crate::format::now_secs()
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct State {
    pub pkg: PkgState,
    pub kube: KubeState,
    pub tf: TfState,
    /// Commits, tags and branches for matching, rebuilt when the history
    /// reloads.
    pub refs: RefCache,
}

/// [`RefIndex`] of the loaded snapshot and the distances computed so far.
#[derive(Default)]
pub struct RefCache {
    key: Option<Instant>,
    pub idx: Option<RefIndex>,
    dist: HashMap<Oid, Distance>,
}

impl RefCache {
    /// Rebuild when the history was reloaded (`loaded_at` changed).
    pub fn refresh(&mut self, snap: Option<&Snapshot>, loaded_at: Option<Instant>) {
        if self.idx.is_some() && self.key == loaded_at {
            return;
        }
        self.key = loaded_at;
        self.dist.clear();
        self.idx = snap.map(RefIndex::from_snapshot);
    }

    /// How far `id` is from HEAD and the base branch (cached).
    pub fn distance(&mut self, snap: Option<&Snapshot>, id: Oid) -> Distance {
        if let Some(d) = self.dist.get(&id) {
            return d.clone();
        }
        let (Some(s), Some(idx)) = (snap, &self.idx) else {
            return Distance::default();
        };
        let d = matching::distance(s, idx, id);
        self.dist.insert(id, d.clone());
        d
    }
}

/// The Packages tab.
#[derive(Default)]
pub struct PkgState {
    pub loading: bool,
    /// When the listing arrived (seconds since the epoch).
    pub fetched: Option<i64>,
    /// Whose packages were asked for.
    pub owner: Option<(String, OwnerKind)>,
    pub listing: Option<Result<Listing, String>>,
    /// Show every package of the owner, not only those linked here.
    pub show_all: bool,
    pub filter: String,
    /// Selected package (name).
    pub selected: Option<String>,
    pub versions: HashMap<String, Versions>,
    /// Selected version id of the selected package.
    pub version: Option<u64>,
    /// The view asked for the first load (when the tab opened before the
    /// GitHub remote was known).
    pub auto_requested: bool,
}

/// Versions of one package.
#[derive(Default)]
pub struct Versions {
    pub loading: bool,
    pub result: Option<Result<Vec<PackageVersion>, String>>,
    pub fetched: Option<i64>,
}

impl PkgState {
    /// Packages in list order: those linked to the repository (all of the
    /// owner's with "show all", linked ones first), filtered by name.
    pub fn visible(&self) -> Vec<&Package> {
        let Some(Ok(l)) = &self.listing else {
            return Vec::new();
        };
        let needle = self.filter.trim().to_lowercase();
        let mut v: Vec<&Package> = l
            .packages
            .iter()
            .filter(|p| self.show_all || p.linked_to(&l.repo))
            .filter(|p| needle.is_empty() || p.name.to_lowercase().contains(&needle))
            .collect();
        v.sort_by(|a, b| {
            b.linked_to(&l.repo)
                .cmp(&a.linked_to(&l.repo))
                .then_with(|| b.updated_at.cmp(&a.updated_at))
        });
        v
    }

    pub fn package(&self, name: &str) -> Option<&Package> {
        match &self.listing {
            Some(Ok(l)) => l.packages.iter().find(|p| p.name == name),
            _ => None,
        }
    }

    /// The loaded versions of a package, newest first.
    pub fn versions_of(&self, name: &str) -> Option<&[PackageVersion]> {
        match self.versions.get(name).and_then(|v| v.result.as_ref()) {
            Some(Ok(v)) => Some(v),
            _ => None,
        }
    }

    /// The loaded package version with this digest (for images running in
    /// the cluster), looking in `prefer` (the image's package) first.
    pub fn tags_for_digest(
        &self,
        digest: &str,
        prefer: Option<&str>,
    ) -> Option<(&str, &PackageVersion)> {
        fn find<'a>(v: &'a Versions, digest: &str) -> Option<&'a PackageVersion> {
            match &v.result {
                Some(Ok(list)) => list.iter().find(|x| x.digest() == digest),
                _ => None,
            }
        }
        if let Some(p) = prefer
            && let Some((name, v)) = self
                .versions
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(p))
            && let Some(x) = find(v, digest)
        {
            return Some((name.as_str(), x));
        }
        let mut names: Vec<&String> = self.versions.keys().collect();
        names.sort();
        names
            .into_iter()
            .find_map(|n| find(&self.versions[n], digest).map(|x| (n.as_str(), x)))
    }
}

/// A context's reachability check.
#[derive(Clone, Debug)]
pub enum Probe {
    Running,
    Done {
        result: Result<usize, String>,
        ms: u64,
    },
}

/// The detail of the selected workload.
pub struct Detail {
    pub key: (WorkloadKind, String),
    pub target: Target,
    pub loading: bool,
    pub data: Option<DetailLoad>,
    pub fetched: Option<i64>,
}

#[derive(Default)]
pub struct PodEvents {
    pub loading: bool,
    pub result: Option<Result<Vec<Event>, String>>,
}

/// Options of the next log request.
#[derive(Clone, Copy, Default)]
pub struct LogForm {
    pub follow: bool,
    pub timestamps: bool,
}

/// The Kubernetes tab.
#[derive(Default)]
pub struct KubeState {
    /// kubectl is on PATH (`None` until the tab was opened).
    pub tool: Option<bool>,
    pub version: Option<String>,
    pub loading: bool,
    pub config: Option<Result<KubeConfig, String>>,
    /// The chosen context (persisted); the kubeconfig's current one when
    /// `None`.
    pub context: Option<String>,
    /// Chosen namespace per context (persisted).
    pub namespaces: BTreeMap<String, String>,
    pub load: Option<NamespaceLoad>,
    pub fetched: Option<i64>,
    pub load_ms: u64,
    pub selected: Option<(WorkloadKind, String)>,
    pub detail: Option<Detail>,
    pub pod: Option<String>,
    pub pod_events: HashMap<String, PodEvents>,
    /// The console run shown in the log panel.
    pub log_run: Option<u64>,
    pub log: LogForm,
    pub probes: BTreeMap<String, Probe>,
    pub filter: String,
    /// The replica count typed for "Scale".
    pub scale_to: String,
}

impl KubeState {
    /// The context and namespace in use.
    pub fn target(&self) -> Option<Target> {
        if let Some(l) = &self.load {
            return Some(l.target.clone());
        }
        let Some(Ok(cfg)) = &self.config else {
            return None;
        };
        let ctx = cfg.choose(self.context.as_deref())?;
        let ns = cfg.namespace_for(&ctx, self.namespaces.get(&ctx).map(String::as_str));
        Some(Target::new(ctx, ns))
    }

    pub fn workloads(&self) -> &[Workload] {
        match self.load.as_ref().and_then(|l| l.workloads.as_ref()) {
            Some(Ok(w)) => w,
            _ => &[],
        }
    }

    pub fn pods(&self) -> &[Pod] {
        match self.load.as_ref().and_then(|l| l.pods.as_ref()) {
            Some(Ok(p)) => p,
            _ => &[],
        }
    }

    /// Workloads in list order (failed first, then by kind and name),
    /// filtered by name or image.
    pub fn visible(&self) -> Vec<&Workload> {
        let needle = self.filter.trim().to_lowercase();
        let mut v: Vec<&Workload> = self
            .workloads()
            .iter()
            .filter(|w| {
                needle.is_empty()
                    || w.name.to_lowercase().contains(&needle)
                    || w.containers
                        .iter()
                        .any(|c| c.image.to_lowercase().contains(&needle))
            })
            .collect();
        v.sort_by_key(|w| (kube::rollout(w).state, w.kind, w.name.clone()));
        v
    }

    pub fn workload(&self, key: &(WorkloadKind, String)) -> Option<&Workload> {
        self.workloads()
            .iter()
            .find(|w| w.kind == key.0 && w.name == key.1)
    }

    /// Pods of a workload: the ones fetched with its selector when the
    /// detail is loaded, else the namespace's pods it selects.
    pub fn pods_of(&self, w: &Workload) -> Vec<&Pod> {
        if let Some(d) = &self.detail
            && d.key == (w.kind, w.name.clone())
            && let Some(data) = &d.data
            && let Ok(p) = &data.pods
        {
            return p.iter().collect();
        }
        self.pods()
            .iter()
            .filter(|p| w.selector.matches(&p.labels))
            .collect()
    }

    /// Pods of the last fetch that are failing, with the reason.
    pub fn failing(&self) -> Vec<(&Pod, String)> {
        self.pods()
            .iter()
            .filter_map(|p| p.failing().map(|r| (p, r)))
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct FmtResult {
    pub files: Vec<String>,
    pub ok: bool,
    /// The output tail when it failed for another reason than formatting.
    pub error: Option<String>,
    pub at: i64,
}

#[derive(Clone, Debug)]
pub struct InitResult {
    pub ok: bool,
    pub cause: Option<String>,
    pub tail: String,
    pub at: i64,
}

#[derive(Clone, Debug)]
pub struct ValidationResult {
    pub result: Result<Validation, String>,
    pub cause: Option<String>,
    pub at: i64,
}

#[derive(Clone, Debug)]
pub struct PlanResult {
    pub outcome: PlanOutcome,
    pub text: PlanText,
    pub json: Option<Result<PlanJson, String>>,
    pub json_loading: bool,
    pub cause: Option<String>,
    pub tail: String,
    pub at: i64,
    pub run: u64,
    pub ms: u64,
}

#[derive(Clone, Debug)]
pub struct DriftResult {
    pub outcome: DriftOutcome,
    pub text: PlanText,
    pub cause: Option<String>,
    pub tail: String,
    pub at: i64,
    pub ms: u64,
}

/// Results of the commands run in one root.
#[derive(Default)]
pub struct RootRuns {
    /// The latest console run per command.
    pub last: HashMap<Command, u64>,
    pub fmt: Option<FmtResult>,
    pub init: Option<InitResult>,
    pub validation: Option<ValidationResult>,
    pub plan: Option<PlanResult>,
    pub drift: Option<DriftResult>,
}

/// The Terraform tab.
#[derive(Default)]
pub struct TfState {
    pub checking: bool,
    /// `(terraform, tofu)` once checked.
    pub tools: Option<(Option<Installed>, Option<Installed>)>,
    /// The binary chosen when both are installed (persisted).
    pub binary: Option<Binary>,
    pub scanning: bool,
    pub scan: Option<Result<Scan, String>>,
    pub scanned: Option<i64>,
    pub scan_ms: u64,
    /// `(upstream, files changed by the commits not pushed)`.
    pub unpushed: Option<(String, Result<Vec<String>, String>)>,
    pub selected: Option<String>,
    pub roots: HashMap<String, RootRuns>,
    /// Console run → `(root, command, binary)`.
    pub run_index: HashMap<u64, (String, Command, Binary)>,
    /// Plan files by console run.
    pub plan_files: HashMap<u64, PathBuf>,
    /// The console run shown in the output panel.
    pub shown_run: Option<u64>,
}

impl TfState {
    pub fn installed(&self, b: Binary) -> Option<&Installed> {
        let (tf, tofu) = self.tools.as_ref()?;
        match b {
            Binary::Terraform => tf.as_ref(),
            Binary::Tofu => tofu.as_ref(),
        }
    }

    /// The binary to run: the chosen one when installed, else the only one
    /// installed, else OpenTofu when the configuration mentions it.
    pub fn binary(&self) -> Option<Binary> {
        let tf = self.installed(Binary::Terraform).is_some();
        let tofu = self.installed(Binary::Tofu).is_some();
        match (tf, tofu) {
            (false, false) => None,
            (true, false) => Some(Binary::Terraform),
            (false, true) => Some(Binary::Tofu),
            (true, true) => Some(self.binary.unwrap_or_else(|| {
                if self.mentions_tofu() {
                    Binary::Tofu
                } else {
                    Binary::Terraform
                }
            })),
        }
    }

    fn mentions_tofu(&self) -> bool {
        matches!(&self.scan, Some(Ok(s)) if s.roots.iter().any(|r| r.info.mentions_tofu))
    }

    pub fn roots(&self) -> &[terraform::TfRoot] {
        match &self.scan {
            Some(Ok(s)) => &s.roots,
            _ => &[],
        }
    }

    /// Roots with drift found by their last drift check.
    pub fn drifting(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .roots
            .iter()
            .filter(|(_, r)| {
                r.drift
                    .as_ref()
                    .is_some_and(|d| d.outcome == DriftOutcome::Drift)
            })
            .map(|(k, _)| k.as_str())
            .collect();
        v.sort();
        v
    }
}

// ---------------------------------------------------------------------------
// Actions and messages
// ---------------------------------------------------------------------------

/// A write the view asks to confirm, resolved against the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteKind {
    Restart,
    Undo(Option<u32>),
    Scale(u32),
}

#[derive(Clone, Debug)]
pub enum Act {
    // Packages
    PkgRefresh,
    PkgSelect(Option<String>),
    PkgSelectAt(usize),
    PkgShowAll(bool),
    PkgVersion(Option<u64>),
    PkgVersionAt(usize),
    PkgLoadVersions(String),
    /// Ask to delete a version (confirmation).
    PkgAskDelete {
        package: String,
        version: u64,
    },
    /// Ask to delete the selected version (script).
    PkgAskDeleteSelected,
    PkgDelete {
        package: String,
        version: u64,
        owner: String,
        kind: OwnerKind,
    },
    // Kubernetes
    KubeCheckTool,
    KubeRefresh,
    KubeContext(String),
    KubeNamespace(String),
    KubeSelect(Option<(WorkloadKind, String)>),
    KubeSelectAt(usize),
    KubePod(Option<String>),
    KubePodAt(usize),
    KubeLogs {
        pod: String,
        container: Option<String>,
        previous: bool,
    },
    /// Logs of the N-th pod of the selected workload (script).
    KubeLogsAt(usize),
    KubeHideLogs,
    /// Log options (follow, timestamps).
    KubeLogForm(bool, bool),
    /// The replica count typed for Scale.
    KubeScaleText(String),
    KubeProbeAll,
    /// Confirm a write on the selected workload.
    KubeAsk(WriteKind),
    KubeWrite {
        target: Target,
        write: KubeWrite,
    },
    // Terraform
    TfCheckTools,
    TfRescan,
    TfSelect(Option<String>),
    TfSelectAt(usize),
    TfBinary(Binary),
    /// Run a command in a root (Init asks first through [`Act::TfAskInit`]).
    TfRun {
        root: String,
        cmd: Command,
    },
    /// The same for the selected root (scripts).
    TfRunSelected(Command),
    TfAskInit(String),
    TfShowRun(Option<u64>),
}

pub enum Msg {
    Packages {
        owner: Option<(String, OwnerKind)>,
        result: Result<Listing, String>,
    },
    Versions {
        package: String,
        result: Result<Vec<PackageVersion>, String>,
    },
    PkgDeleted {
        package: String,
    },
    Kube {
        config: Result<KubeConfig, String>,
        load: Option<NamespaceLoad>,
        version: Option<String>,
        ms: u64,
    },
    KubeDetail {
        key: (WorkloadKind, String),
        target: Target,
        data: DetailLoad,
    },
    PodEvents {
        pod: String,
        result: Result<Vec<Event>, String>,
    },
    Probe {
        context: String,
        result: Result<usize, String>,
        ms: u64,
    },
    KubeWritten,
    TfTools {
        terraform: Option<Installed>,
        tofu: Option<Installed>,
    },
    TfScanned {
        scan: Result<Scan, String>,
        unpushed: Option<(String, Result<Vec<String>, String>)>,
        ms: u64,
    },
    TfShown {
        root: String,
        run: u64,
        result: Result<PlanJson, String>,
    },
}

fn n_of(n: usize, one: &str, many: &str) -> String {
    crate::format::plural(n, one, many)
}

impl Msg {
    /// One line for the activity log (`Err` when the work failed).
    pub fn summary(&self) -> Result<String, String> {
        match self {
            Msg::Packages { result, .. } => match result {
                Ok(l) => Ok(format!(
                    "{} of {} ({} linked to {})",
                    n_of(l.packages.len(), "container package", "container packages"),
                    l.owner,
                    l.packages.iter().filter(|p| p.linked_to(&l.repo)).count(),
                    l.repo
                )),
                Err(e) => Err(e.clone()),
            },
            Msg::Versions { package, result } => match result {
                Ok(v) => Ok(format!(
                    "{} of {package}",
                    n_of(v.len(), "version", "versions")
                )),
                Err(e) => Err(e.clone()),
            },
            Msg::PkgDeleted { package } => Ok(format!("Reloading the versions of {package}")),
            Msg::Kube {
                config, load, ms, ..
            } => match (config, load) {
                (Err(e), _) => Err(e.clone()),
                (Ok(cfg), None) => Ok(format!(
                    "{} in the kubeconfig, none chosen",
                    n_of(cfg.contexts.len(), "context", "contexts")
                )),
                (Ok(_), Some(l)) => match l.fatal() {
                    Some(e) => Err(format!("{}: {e}", l.target.context)),
                    None => {
                        let w = l.workloads.as_ref().and_then(|w| w.as_ref().ok());
                        let p = l.pods.as_ref().and_then(|p| p.as_ref().ok());
                        let failing =
                            p.map_or(0, |p| p.iter().filter(|x| x.failing().is_some()).count());
                        Ok(format!(
                            "{} / {}: {}, {} ({failing} failing) in {}",
                            l.target.context,
                            l.target.namespace,
                            n_of(w.map_or(0, Vec::len), "workload", "workloads"),
                            n_of(p.map_or(0, Vec::len), "pod", "pods"),
                            crate::format::millis(*ms)
                        ))
                    }
                },
            },
            Msg::KubeDetail { key, data, .. } => Ok(format!(
                "{}/{}: {}, {}, {}",
                key.0.resource(),
                key.1,
                match &data.pods {
                    Ok(p) => n_of(p.len(), "pod", "pods"),
                    Err(_) => "pods failed".into(),
                },
                match &data.events {
                    Ok(e) => n_of(e.len(), "event", "events"),
                    Err(_) => "events failed".into(),
                },
                match &data.history {
                    Ok(h) => n_of(h.len(), "revision", "revisions"),
                    Err(_) => "history failed".into(),
                }
            )),
            Msg::PodEvents { pod, result } => match result {
                Ok(e) => Ok(format!("{} for {pod}", n_of(e.len(), "event", "events"))),
                Err(e) => Err(e.clone()),
            },
            Msg::Probe {
                context,
                result,
                ms,
            } => match result {
                Ok(n) => Ok(format!(
                    "{context} answers ({}, {})",
                    n_of(*n, "namespace", "namespaces"),
                    crate::format::millis(*ms)
                )),
                Err(e) => Err(format!("{context}: {e}")),
            },
            Msg::KubeWritten => Ok("Reloading the workloads".into()),
            Msg::TfTools { terraform, tofu } => Ok(format!(
                "Terraform: {}; OpenTofu: {}",
                terraform
                    .as_ref()
                    .map_or("not installed".to_owned(), |t| t.version.clone()),
                tofu.as_ref()
                    .map_or("not installed".to_owned(), |t| t.version.clone())
            )),
            Msg::TfScanned { scan, ms, .. } => match scan {
                Ok(s) => Ok(format!(
                    "{}, {} from {} in {}",
                    n_of(s.roots.len(), "root", "roots"),
                    n_of(s.modules.len(), "module directory", "module directories"),
                    n_of(s.files_read, "file", "files"),
                    crate::format::millis(*ms)
                )),
                Err(e) => Err(e.clone()),
            },
            Msg::TfShown { root, result, .. } => match result {
                Ok(p) => {
                    let c = p.counts();
                    Ok(format!(
                        "{}: {} to add, {} to change, {} to destroy",
                        if root.is_empty() { "." } else { root },
                        c.add,
                        c.change,
                        c.destroy
                    ))
                }
                Err(e) => Err(e.clone()),
            },
        }
    }
}

/// Persisted per repository.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub show_all_packages: bool,
    pub kube_context: Option<String>,
    pub kube_namespaces: BTreeMap<String, String>,
    pub tf_binary: Option<Binary>,
}

impl State {
    pub fn prefs(&self) -> Prefs {
        Prefs {
            show_all_packages: self.pkg.show_all,
            kube_context: self.kube.context.clone(),
            kube_namespaces: self.kube.namespaces.clone(),
            tf_binary: self.tf.binary,
        }
    }

    pub fn apply_prefs(&mut self, p: &Prefs) {
        self.pkg.show_all = p.show_all_packages;
        self.kube.context = p.kube_context.clone();
        self.kube.namespaces = p.kube_namespaces.clone();
        self.tf.binary = p.tf_binary;
    }
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

fn stale(at: Option<i64>) -> bool {
    at.is_none_or(|t| now() - t > STALE_SECS)
}

fn load_packages(cx: &mut Ctx, st: &mut PkgState) {
    if st.loading {
        return;
    }
    st.loading = cx.gh_read("Container packages", |gh, log| {
        let (owner, result) = packages::load(gh, &mut log.calls);
        Msg::Packages {
            owner,
            result: result.map_err(|e| e.to_string()),
        }
    });
}

fn load_versions(cx: &mut Ctx, st: &mut PkgState, name: &str) {
    let Some(Ok(l)) = &st.listing else {
        return;
    };
    let (owner, kind) = (l.owner.clone(), l.kind);
    let entry = st.versions.entry(name.to_owned()).or_default();
    if entry.loading {
        return;
    }
    let package = name.to_owned();
    entry.loading = cx.gh_read(format!("Versions of {name}"), move |gh, log| {
        let result = packages::load_versions(gh, kind, &owner, &package, &mut log.calls)
            .map_err(|e| e.to_string());
        Msg::Versions { package, result }
    });
}

fn select_package(cx: &mut Ctx, st: &mut PkgState, name: Option<String>) {
    if st.selected != name {
        st.version = None;
    }
    st.selected = name.clone();
    if let Some(n) = name {
        let have = st
            .versions
            .get(&n)
            .is_some_and(|v| v.result.is_some() || v.loading);
        if !have {
            load_versions(cx, st, &n);
        }
    }
}

fn load_kube(cx: &mut Ctx, k: &mut KubeState) {
    if k.loading {
        return;
    }
    let want = k.context.clone();
    let ns_prefs = k.namespaces.clone();
    k.loading = cx.read(
        "Kubernetes: kubeconfig, namespaces, workloads, pods",
        move |dir, log| {
            let started = Instant::now();
            log.command(format!("{} version --client", kube::program()));
            let version = kube::detect().map(|i| i.version).filter(|v| !v.is_empty());
            let config = kube::load_config(dir, &mut log.commands);
            let load = config.as_ref().ok().and_then(|cfg| {
                let ctx = cfg.choose(want.as_deref())?;
                let ns = cfg.namespace_for(&ctx, ns_prefs.get(&ctx).map(String::as_str));
                Some(kube::load_namespace(
                    dir,
                    &Target::new(ctx, ns),
                    &mut log.commands,
                ))
            });
            Msg::Kube {
                config,
                load,
                version,
                ms: started.elapsed().as_millis() as u64,
            }
        },
    );
}

fn load_detail(cx: &mut Ctx, k: &mut KubeState, key: (WorkloadKind, String)) {
    let Some(target) = k.target() else { return };
    let Some(w) = k.workload(&key) else { return };
    let selector = w.selector.clone();
    let label = format!("Kubernetes: {} pods, events, history", w.resource());
    let keep = k
        .detail
        .take()
        .filter(|d| d.key == key && d.target == target);
    k.detail = Some(Detail {
        key: key.clone(),
        target: target.clone(),
        loading: true,
        data: keep.as_ref().and_then(|d| d.data.clone()),
        fetched: keep.and_then(|d| d.fetched),
    });
    let started = cx.read(label, move |dir, log| {
        let data = kube::load_detail(dir, &target, key.0, &key.1, &selector, &mut log.commands);
        Msg::KubeDetail { key, target, data }
    });
    if !started && let Some(d) = &mut k.detail {
        d.loading = false;
    }
}

fn load_pod_events(cx: &mut Ctx, k: &mut KubeState, pod: &str) {
    let Some(target) = k.target() else { return };
    let e = k.pod_events.entry(pod.to_owned()).or_default();
    if e.loading {
        return;
    }
    let name = pod.to_owned();
    e.loading = cx.read(
        format!("Kubernetes: events of pod {pod}"),
        move |dir, log| {
            let result = kube::load_pod_events(dir, &target, &name, &mut log.commands);
            Msg::PodEvents { pod: name, result }
        },
    );
}

fn probe_all(cx: &mut Ctx, k: &mut KubeState) {
    let Some(Ok(cfg)) = &k.config else { return };
    for c in &cfg.contexts {
        if matches!(k.probes.get(&c.name), Some(Probe::Running)) {
            continue;
        }
        let context = c.name.clone();
        let label = format!("Kubernetes: is {context} reachable?");
        if cx.read(label, {
            let context = context.clone();
            move |dir, log| {
                let started = Instant::now();
                let result = kube::probe(dir, &context, &mut log.commands);
                Msg::Probe {
                    context,
                    result,
                    ms: started.elapsed().as_millis() as u64,
                }
            }
        }) {
            k.probes.insert(context, Probe::Running);
        }
    }
}

fn check_tf_tools(cx: &mut Ctx, tf: &mut TfState) {
    if tf.checking {
        return;
    }
    tf.checking = cx.read("Terraform / OpenTofu: installed?", |_, log| {
        log.command("terraform version");
        log.command("tofu version");
        Msg::TfTools {
            terraform: Binary::Terraform.detect(),
            tofu: Binary::Tofu.detect(),
        }
    });
}

fn scan_tf(cx: &mut Ctx, repo: &mut RepoState) {
    let tf = &mut repo.devops.runtime.tf;
    if tf.scanning {
        return;
    }
    let upstream = repo
        .snapshot
        .as_ref()
        .and_then(|s| s.current_branch())
        .filter(|b| !b.upstream_gone)
        .and_then(|b| b.upstream.clone());
    tf.scanning = cx.read("Terraform: find roots", move |dir, log| {
        let started = Instant::now();
        log.command(
            "git ls-files -z --cached --others --exclude-standard -- *.tf *.tofu *.tf.json *.tfvars *.tfvars.json *.terraform.lock.hcl",
        );
        let scan = terraform::find_roots(dir);
        let has_roots = scan.as_ref().is_ok_and(|s| !s.roots.is_empty());
        let unpushed = upstream.filter(|_| has_roots).map(|u| {
            log.command("git diff --name-only -z @{u}...HEAD");
            (u, terraform::unpushed_changes(dir))
        });
        Msg::TfScanned {
            scan,
            unpushed,
            ms: started.elapsed().as_millis() as u64,
        }
    });
}

fn root_display(root: &str) -> &str {
    if root.is_empty() { "." } else { root }
}

fn run_tf(cx: &mut Ctx, repo: &mut RepoState, root: String, cmd: Command) {
    let workdir = repo.workdir.clone();
    let tf = &mut repo.devops.runtime.tf;
    let Some(binary) = tf.binary() else {
        cx.toast(
            ToastKind::Error,
            "Neither Terraform nor OpenTofu is installed",
        );
        return;
    };
    let dir = if root.is_empty() {
        workdir
    } else {
        workdir.join(&root)
    };
    let plan_out = (cmd == Command::Plan).then(|| terraform::plan_file(&root));
    let spec = terraform::spec(binary, &dir, cmd, plan_out.as_deref());
    let title = format!(
        "{} {} · {}",
        binary.word(),
        match cmd {
            Command::Fmt => "fmt -check",
            Command::Init => "init",
            Command::Validate => "validate",
            Command::Plan => "plan",
            Command::Drift => "plan -refresh-only",
        },
        root_display(&root)
    );
    let id = cx.console(title, spec);
    tf.run_index.insert(id, (root.clone(), cmd, binary));
    if let Some(p) = plan_out {
        tf.plan_files.insert(id, p);
    }
    tf.roots.entry(root).or_default().last.insert(cmd, id);
    // Validate prints JSON that the tab shows parsed; its console output
    // stays one click away ("Show output") instead of covering the result.
    if cmd != Command::Validate {
        tf.shown_run = Some(id);
    } else if tf.shown_run.is_some_and(|r| {
        tf.run_index
            .get(&r)
            .is_some_and(|(_, c, _)| *c == Command::Validate)
    }) {
        tf.shown_run = None;
    }
}

fn selected_write(k: &KubeState, kind: WriteKind) -> Option<(Target, KubeWrite, String)> {
    let target = k.target()?;
    let key = k.selected.clone()?;
    let (wk, name) = key;
    let write = match kind {
        WriteKind::Restart => KubeWrite::Restart { kind: wk, name },
        WriteKind::Undo(to) => KubeWrite::Undo {
            kind: wk,
            name,
            to_revision: to,
        },
        WriteKind::Scale(n) => {
            if !wk.scalable() {
                return None;
            }
            KubeWrite::Scale {
                kind: wk,
                name,
                replicas: n,
            }
        }
    };
    let confirm = match kind {
        WriteKind::Restart => "Restart",
        WriteKind::Undo(_) => "Roll back",
        WriteKind::Scale(_) => "Scale",
    }
    .to_owned();
    Some((target, write, confirm))
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        // ---- Packages
        Act::PkgRefresh => {
            let st = &mut repo.devops.runtime.pkg;
            st.versions.retain(|_, v| v.loading);
            load_packages(cx, st);
        }
        Act::PkgSelect(name) => select_package(cx, &mut repo.devops.runtime.pkg, name),
        Act::PkgSelectAt(i) => {
            let st = &mut repo.devops.runtime.pkg;
            let name = st.visible().get(i).map(|p| p.name.clone());
            if name.is_some() {
                select_package(cx, st, name);
            }
        }
        Act::PkgShowAll(on) => repo.devops.runtime.pkg.show_all = on,
        Act::PkgVersion(v) => repo.devops.runtime.pkg.version = v,
        Act::PkgVersionAt(i) => {
            let st = &mut repo.devops.runtime.pkg;
            let id = st
                .selected
                .as_deref()
                .and_then(|n| st.versions_of(n))
                .and_then(|v| v.get(i))
                .map(|v| v.id);
            if id.is_some() {
                st.version = id;
            }
        }
        Act::PkgLoadVersions(name) => {
            let st = &mut repo.devops.runtime.pkg;
            if let Some(v) = st.versions.get_mut(&name)
                && !v.loading
            {
                v.result = None;
            }
            load_versions(cx, st, &name);
        }
        Act::PkgAskDeleteSelected => {
            let st = &repo.devops.runtime.pkg;
            let pick = st.selected.clone().and_then(|n| {
                let versions = st.versions_of(&n)?;
                let id = st.version.or_else(|| versions.first().map(|v| v.id))?;
                Some((n, id))
            });
            if let Some((package, version)) = pick {
                apply(cx, repo, Act::PkgAskDelete { package, version });
            }
        }
        Act::PkgAskDelete { package, version } => {
            let st = &repo.devops.runtime.pkg;
            let Some(Ok(l)) = &st.listing else { return };
            let Some(v) = st
                .versions_of(&package)
                .and_then(|vs| vs.iter().find(|v| v.id == version))
            else {
                return;
            };
            let req = packages::delete_version(l.kind, &l.owner, &package, version);
            let tags = if v.tags().is_empty() {
                "It has no tags.".to_owned()
            } else {
                format!(
                    "Its tags go with it: {}. Anything pulling {} by one of these tags will fail until it is pushed again.",
                    v.tags().join(", "),
                    packages::image(&l.owner, &package)
                )
            };
            let body = format!(
                "Version {} ({}) of {} is removed from the GitHub Container Registry. {tags} This cannot be undone from gitvis (GitHub keeps deleted versions restorable for 30 days on its website). It needs the delete:packages scope and admin rights on the package.",
                version,
                v.short_digest(),
                package
            );
            cx.confirm(
                format!("Delete {} of {package}?", v.short_digest()),
                body,
                "Delete version",
                true,
                vec![req.display(), format!("  digest {}", v.digest())],
                Act::PkgDelete {
                    package,
                    version,
                    owner: l.owner.clone(),
                    kind: l.kind,
                }
                .into(),
            );
        }
        Act::PkgDelete {
            package,
            version,
            owner,
            kind,
        } => {
            let req = packages::delete_version(kind, &owner, &package, version);
            cx.gh_change(
                format!("Delete version {version} of {package}"),
                move |gh, log| {
                    let r = gh
                        .send(&req, &mut log.calls)
                        .map(|_| format!("Deleted version {version} of {package}"))
                        .map_err(|e| {
                            let e = e.to_string();
                            match packages::problem(&e).fix() {
                                Some(fix) => format!("{e}\nFix: {fix}"),
                                None => e,
                            }
                        });
                    (r, Some(Msg::PkgDeleted { package }))
                },
            );
        }

        // ---- Kubernetes
        Act::KubeCheckTool => {
            let k = &mut repo.devops.runtime.kube;
            k.tool = Some(kube::available());
            if k.tool == Some(true) {
                load_kube(cx, k);
            }
        }
        Act::KubeRefresh => {
            let k = &mut repo.devops.runtime.kube;
            k.tool = Some(kube::available());
            k.pod_events.clear();
            load_kube(cx, k);
            if let Some(key) = k.selected.clone() {
                load_detail(cx, k, key);
            }
        }
        Act::KubeContext(c) => {
            let k = &mut repo.devops.runtime.kube;
            if k.load.as_ref().is_some_and(|l| l.target.context == c) {
                return;
            }
            k.context = Some(c);
            k.selected = None;
            k.detail = None;
            k.pod = None;
            k.pod_events.clear();
            k.load = None;
            load_kube(cx, k);
        }
        Act::KubeNamespace(ns) => {
            let k = &mut repo.devops.runtime.kube;
            let Some(t) = k.target() else { return };
            if t.namespace == ns {
                return;
            }
            k.namespaces.insert(t.context.clone(), ns);
            k.context = Some(t.context);
            k.selected = None;
            k.detail = None;
            k.pod = None;
            k.pod_events.clear();
            k.load = None;
            load_kube(cx, k);
        }
        Act::KubeSelect(key) => {
            let k = &mut repo.devops.runtime.kube;
            if k.selected != key {
                k.pod = None;
            }
            k.selected = key.clone();
            if let Some(key) = key {
                let fresh = k
                    .detail
                    .as_ref()
                    .is_some_and(|d| d.key == key && (d.loading || !stale(d.fetched)));
                if !fresh {
                    load_detail(cx, k, key);
                }
            }
        }
        Act::KubeSelectAt(i) => {
            let key = repo
                .devops
                .runtime
                .kube
                .visible()
                .get(i)
                .map(|w| (w.kind, w.name.clone()));
            if key.is_some() {
                apply(cx, repo, Act::KubeSelect(key));
            }
        }
        Act::KubePod(pod) => {
            let k = &mut repo.devops.runtime.kube;
            k.pod = pod.clone();
            if let Some(p) = pod
                && !k.pod_events.contains_key(&p)
            {
                load_pod_events(cx, k, &p);
            }
        }
        Act::KubePodAt(i) => {
            let k = &repo.devops.runtime.kube;
            let name = k
                .selected
                .as_ref()
                .and_then(|key| k.workload(key))
                .and_then(|w| k.pods_of(w).get(i).map(|p| p.name.clone()));
            if name.is_some() {
                apply(cx, repo, Act::KubePod(name));
            }
        }
        Act::KubeLogs {
            pod,
            container,
            previous,
        } => {
            let workdir = repo.workdir.clone();
            let k = &mut repo.devops.runtime.kube;
            let Some(t) = k.target() else { return };
            let opts = LogOpts {
                container: container.clone(),
                previous,
                follow: k.log.follow && !previous,
                timestamps: k.log.timestamps,
                ..LogOpts::default()
            };
            let mut title = format!("Logs · {pod}");
            if let Some(c) = &container {
                title.push_str(&format!(" · {c}"));
            }
            if previous {
                title.push_str(" · previous instance");
            }
            if opts.follow {
                title.push_str(" · following");
            }
            let id = cx.console(
                title,
                kube::spec(&workdir, kube::logs_args(&t, &pod, &opts)),
            );
            k.log_run = Some(id);
            k.pod = Some(pod);
        }
        Act::KubeLogsAt(i) => {
            let k = &repo.devops.runtime.kube;
            let pick = k
                .selected
                .as_ref()
                .and_then(|key| k.workload(key))
                .and_then(|w| {
                    let pods = k.pods_of(w);
                    let p = pods.get(i)?;
                    let c = p
                        .containers_in_order()
                        .first()
                        .map(|c| c.name.clone())
                        .or_else(|| p.container_names.first().cloned());
                    Some((p.name.clone(), c))
                });
            if let Some((pod, container)) = pick {
                apply(
                    cx,
                    repo,
                    Act::KubeLogs {
                        pod,
                        container,
                        previous: false,
                    },
                );
            }
        }
        Act::KubeHideLogs => repo.devops.runtime.kube.log_run = None,
        Act::KubeLogForm(follow, timestamps) => {
            repo.devops.runtime.kube.log = LogForm { follow, timestamps }
        }
        Act::KubeScaleText(t) => repo.devops.runtime.kube.scale_to = t,
        Act::KubeProbeAll => probe_all(cx, &mut repo.devops.runtime.kube),
        Act::KubeAsk(kind) => {
            let k = &repo.devops.runtime.kube;
            let Some((target, write, confirm)) = selected_write(k, kind) else {
                return;
            };
            let args = write.args(&target);
            let ctx_line = k
                .config
                .as_ref()
                .and_then(|c| c.as_ref().ok())
                .and_then(|c| c.context(&target.context))
                .and_then(|c| c.server.clone())
                .map(|s| format!(" ({s})"))
                .unwrap_or_default();
            let body = format!(
                "{}\n\nContext {}{ctx_line}, namespace {}. This changes the cluster; gitvis runs exactly the command below.",
                write.explain(),
                target.context,
                target.namespace
            );
            cx.confirm(
                format!("{}?", write.title()),
                body,
                confirm,
                true,
                vec![kube::display(&args)],
                Act::KubeWrite { target, write }.into(),
            );
        }
        Act::KubeWrite { target, write } => {
            let label = write.title();
            cx.remote_change(label, move |dir, log| {
                let r = kube::run_write(dir, &target, &write, &mut log.commands);
                (r, Some(Msg::KubeWritten))
            });
        }

        // ---- Terraform
        Act::TfCheckTools => {
            let tf = &mut repo.devops.runtime.tf;
            gitvis_core::devops::tools::refresh();
            tf.tools = None;
            check_tf_tools(cx, tf);
        }
        Act::TfRescan => scan_tf(cx, repo),
        Act::TfSelect(root) => repo.devops.runtime.tf.selected = root,
        Act::TfSelectAt(i) => {
            let tf = &mut repo.devops.runtime.tf;
            if let Some(r) = tf.roots().get(i) {
                tf.selected = Some(r.info.dir.clone());
            }
        }
        Act::TfBinary(b) => repo.devops.runtime.tf.binary = Some(b),
        Act::TfRun { root, cmd } => run_tf(cx, repo, root, cmd),
        Act::TfRunSelected(cmd) => {
            let tf = &repo.devops.runtime.tf;
            let Some(root) = tf
                .selected
                .clone()
                .or_else(|| tf.roots().first().map(|r| r.info.dir.clone()))
            else {
                return;
            };
            if cmd == Command::Init {
                apply(cx, repo, Act::TfAskInit(root));
            } else {
                run_tf(cx, repo, root, cmd);
            }
        }
        Act::TfAskInit(root) => {
            let tf = &repo.devops.runtime.tf;
            let Some(binary) = tf.binary() else {
                cx.toast(
                    ToastKind::Error,
                    "Neither Terraform nor OpenTofu is installed",
                );
                return;
            };
            let dir = repo.workdir.join(&root);
            let line = format!("{} {}", binary.word(), Command::Init.args(None).join(" "));
            let body = format!(
                "{} downloads the providers and modules this configuration needs from the registry (network) into {}/.terraform/. It also creates or updates .terraform.lock.hcl in that directory, a file you normally commit. The backend is skipped (-backend=false): no state is read or written and no credentials are used.",
                binary.name(),
                root_display(&root)
            );
            cx.confirm(
                format!("Run {} init in {}?", binary.word(), root_display(&root)),
                body,
                "Run init",
                false,
                vec![line, format!("  in {}", dir.display())],
                Act::TfRun {
                    root,
                    cmd: Command::Init,
                }
                .into(),
            );
        }
        Act::TfShowRun(id) => repo.devops.runtime.tf.shown_run = id,
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    let rt = &mut repo.devops.runtime;
    match msg {
        Msg::Packages { owner, result } => {
            let st = &mut rt.pkg;
            st.loading = false;
            st.fetched = Some(now());
            st.owner = owner;
            st.listing = Some(result);
            // Keep the selection when it still exists, else pick the first.
            let names: Vec<String> = st.visible().iter().map(|p| p.name.clone()).collect();
            let keep = st.selected.as_ref().is_some_and(|s| names.contains(s));
            if !keep {
                st.selected = None;
                st.version = None;
            }
            let pick = st.selected.clone().or_else(|| names.first().cloned());
            if let Some(n) = pick {
                st.selected = Some(n.clone());
                load_versions(cx, st, &n);
            }
        }
        Msg::Versions { package, result } => {
            let v = rt.pkg.versions.entry(package).or_default();
            v.loading = false;
            v.fetched = Some(now());
            v.result = Some(result);
        }
        Msg::PkgDeleted { package } => {
            rt.pkg.version = None;
            if let Some(v) = rt.pkg.versions.get_mut(&package) {
                v.result = None;
            }
            load_versions(cx, &mut rt.pkg, &package);
        }
        Msg::Kube {
            config,
            load,
            version,
            ms,
        } => {
            let k = &mut rt.kube;
            k.loading = false;
            k.fetched = Some(now());
            k.load_ms = ms;
            if version.is_some() {
                k.version = version;
            }
            k.config = Some(config);
            if let (Some(sel), Some(l)) = (&k.selected, &load)
                && !matches!(&l.workloads, Some(Ok(w)) if w.iter().any(|w| (w.kind, &w.name) == (sel.0, &sel.1)))
            {
                k.selected = None;
                k.detail = None;
                k.pod = None;
            }
            k.load = load;
        }
        Msg::KubeDetail { key, target, data } => {
            let k = &mut rt.kube;
            if k.target().as_ref() != Some(&target) {
                return;
            }
            k.detail = Some(Detail {
                key,
                target,
                loading: false,
                data: Some(data),
                fetched: Some(now()),
            });
            if let Some(p) = &k.pod
                && !k.pods().iter().any(|x| &x.name == p)
                && !k
                    .detail
                    .as_ref()
                    .and_then(|d| d.data.as_ref())
                    .is_some_and(|d| matches!(&d.pods, Ok(v) if v.iter().any(|x| &x.name == p)))
            {
                k.pod = None;
            }
        }
        Msg::PodEvents { pod, result } => {
            let e = rt.kube.pod_events.entry(pod).or_default();
            e.loading = false;
            e.result = Some(result);
        }
        Msg::Probe {
            context,
            result,
            ms,
        } => {
            rt.kube.probes.insert(context, Probe::Done { result, ms });
        }
        Msg::KubeWritten => {
            let k = &mut rt.kube;
            load_kube(cx, k);
            if let Some(key) = k.selected.clone() {
                load_detail(cx, k, key);
            }
        }
        Msg::TfTools { terraform, tofu } => {
            rt.tf.checking = false;
            rt.tf.tools = Some((terraform, tofu));
        }
        Msg::TfScanned { scan, unpushed, ms } => {
            let tf = &mut rt.tf;
            tf.scanning = false;
            tf.scanned = Some(now());
            tf.scan_ms = ms;
            tf.unpushed = unpushed;
            tf.scan = Some(scan);
            let dirs: Vec<String> = tf.roots().iter().map(|r| r.info.dir.clone()).collect();
            if tf.selected.as_ref().is_none_or(|s| !dirs.contains(s)) {
                tf.selected = dirs.first().cloned();
            }
        }
        Msg::TfShown { root, run, result } => {
            if let Some(p) = rt.tf.roots.get_mut(&root).and_then(|r| r.plan.as_mut())
                && p.run == run
            {
                p.json_loading = false;
                p.json = Some(result);
            }
        }
    }
}

/// A console run started by this area (or any other) ended.
pub fn on_console_done(cx: &mut Ctx, repo: &mut RepoState, run: &ConsoleRun) {
    let tf = &mut repo.devops.runtime.tf;
    let Some((root, cmd, binary)) = tf.run_index.get(&run.id).cloned() else {
        return;
    };
    let text = run.text();
    let tail = run.tail(30);
    let at = now();
    let ms = run.elapsed_ms();
    let code = if run.cancelled() || matches!(run.exit, Some(Err(_))) {
        None
    } else {
        run.exit_code()
    };
    let start_error = match &run.exit {
        Some(Err(e)) => Some(e.clone()),
        _ => None,
    };
    let cause = |t: &str| {
        start_error
            .clone()
            .or_else(|| terraform::likely_cause(t, binary))
    };
    let plan_file = tf.plan_files.get(&run.id).cloned();
    let mut initialized = false;
    let entry = tf.roots.entry(root.clone()).or_default();
    match cmd {
        Command::Fmt => {
            let files = terraform::parse_fmt(&text);
            let ok = run.succeeded();
            entry.fmt = Some(FmtResult {
                error: (!ok && files.is_empty())
                    .then(|| start_error.clone().unwrap_or_else(|| tail.clone())),
                files,
                ok,
                at,
            });
        }
        Command::Init => {
            let ok = run.succeeded();
            initialized = ok;
            entry.init = Some(InitResult {
                ok,
                cause: if ok { None } else { cause(&text) },
                tail,
                at,
            });
        }
        Command::Validate => {
            let result = match &start_error {
                Some(e) => Err(e.clone()),
                None => terraform::parse_validate(&text).map_err(|e| {
                    if text.trim().is_empty() {
                        e
                    } else {
                        format!("{e}\n{tail}")
                    }
                }),
            };
            let detail_text = match &result {
                Ok(v) => v
                    .diagnostics
                    .iter()
                    .map(|d| format!("{} {}", d.summary, d.detail))
                    .collect::<Vec<_>>()
                    .join("\n"),
                Err(e) => e.clone(),
            };
            entry.validation = Some(ValidationResult {
                // "The configuration has errors" says nothing a validation
                // result does not show already.
                cause: if result.as_ref().is_ok_and(|v| v.valid) {
                    None
                } else {
                    cause(&detail_text).filter(|c| !c.starts_with("The configuration has errors"))
                },
                result,
                at,
            });
        }
        Command::Plan => {
            let outcome = if run.cancelled() {
                PlanOutcome::Stopped
            } else if start_error.is_some() {
                PlanOutcome::Failed
            } else {
                terraform::plan_outcome(code)
            };
            let parsed = terraform::parse_plan_text(&text);
            let want_json = matches!(outcome, PlanOutcome::Changes | PlanOutcome::NoChanges)
                && plan_file.as_ref().is_some_and(|p| p.exists());
            entry.plan = Some(PlanResult {
                outcome,
                cause: if outcome == PlanOutcome::Failed {
                    cause(&text)
                } else {
                    None
                },
                text: parsed,
                json: None,
                json_loading: want_json,
                tail,
                at,
                run: run.id,
                ms,
            });
            if let Some(plan) = plan_file {
                if want_json {
                    let root2 = root.clone();
                    let id = run.id;
                    let started = cx.read(
                        format!("{} show -json · {}", binary.word(), root_display(&root)),
                        move |dir, log| {
                            let root_dir = if root2.is_empty() {
                                dir.to_path_buf()
                            } else {
                                dir.join(&root2)
                            };
                            let result =
                                terraform::show_plan(binary, &root_dir, &plan, &mut log.commands);
                            Msg::TfShown {
                                root: root2,
                                run: id,
                                result,
                            }
                        },
                    );
                    if !started && let Some(p) = &mut entry.plan {
                        p.json_loading = false;
                    }
                } else {
                    let _ = std::fs::remove_file(&plan);
                }
            }
        }
        Command::Drift => {
            let outcome = if run.cancelled() {
                DriftOutcome::Stopped
            } else if start_error.is_some() {
                DriftOutcome::Failed
            } else {
                terraform::drift_outcome(code)
            };
            entry.drift = Some(DriftResult {
                outcome,
                cause: if outcome == DriftOutcome::Failed {
                    cause(&text)
                } else {
                    None
                },
                text: terraform::parse_plan_text(&text),
                tail,
                at,
                ms,
            });
        }
    }
    if initialized
        && let Some(Ok(scan)) = &mut tf.scan
        && let Some(r) = scan.roots.iter_mut().find(|r| r.info.dir == root)
    {
        r.info.initialized = true;
    }
}

/// One of this area's tabs was opened.
pub fn on_tab_shown(cx: &mut Ctx, repo: &mut RepoState, tab: ShipTab) {
    match tab {
        ShipTab::Packages => {
            let st = &mut repo.devops.runtime.pkg;
            if st.listing.is_none() || stale(st.fetched) {
                load_packages(cx, st);
            }
        }
        ShipTab::Kubernetes => {
            let k = &mut repo.devops.runtime.kube;
            if k.tool.is_none() {
                k.tool = Some(kube::available());
            }
            if k.tool == Some(true) && (k.config.is_none() || stale(k.fetched)) {
                load_kube(cx, k);
            }
        }
        ShipTab::Terraform => {
            let tf = &mut repo.devops.runtime.tf;
            if tf.tools.is_none() {
                check_tf_tools(cx, tf);
            }
            if tf.scan.is_none() || stale(tf.scanned) {
                scan_tf(cx, repo);
            }
        }
        _ => {}
    }
}

/// `GITVIS_SCRIPT` commands this area understands.
pub fn script(cmd: &str, arg: &str) -> Option<Vec<Action>> {
    let arg = arg.trim();
    let n = arg.parse::<usize>().ok();
    let act = match cmd {
        "pkg" => Act::PkgSelectAt(n?),
        "pkg-all" => Act::PkgShowAll(!matches!(arg, "off" | "0" | "false")),
        "pkg-version" => Act::PkgVersionAt(n?),
        "pkg-delete" => Act::PkgAskDeleteSelected,
        "pkg-refresh" => Act::PkgRefresh,
        "k8s-context" => Act::KubeContext(arg.to_owned()),
        "k8s-ns" => Act::KubeNamespace(arg.to_owned()),
        "k8s-refresh" => Act::KubeRefresh,
        "k8s-probe" => Act::KubeProbeAll,
        "k8s-restart" => Act::KubeAsk(WriteKind::Restart),
        "k8s-undo" => Act::KubeAsk(WriteKind::Undo(arg.parse().ok())),
        "k8s-scale" => Act::KubeAsk(WriteKind::Scale(arg.parse().ok()?)),
        "workload" => Act::KubeSelectAt(n?),
        "pod" => Act::KubePodAt(n?),
        "pod-logs" => Act::KubeLogsAt(n?),
        "tf-root" => Act::TfSelectAt(n?),
        "tf-validate" => Act::TfRunSelected(Command::Validate),
        "tf-fmt" => Act::TfRunSelected(Command::Fmt),
        "tf-init" => Act::TfRunSelected(Command::Init),
        "tf-plan" => Act::TfRunSelected(Command::Plan),
        "tf-drift" => Act::TfRunSelected(Command::Drift),
        "tf-rescan" => Act::TfRescan,
        "tf-console" => Act::TfShowRun(None),
        "k8s-hide-logs" => Act::KubeHideLogs,
        _ => return None,
    };
    Some(vec![act.into()])
}

pub fn show_tab(
    ui: &mut egui::Ui,
    repo: &mut RepoState,
    tab: ShipTab,
    console: &Console,
    _settings: &Settings,
    actions: &mut Vec<Action>,
) {
    let RepoState {
        snapshot,
        loaded_at,
        devops,
        github_remote,
        workdir,
        ..
    } = repo;
    let rt = &mut devops.runtime;
    rt.refs.refresh(snapshot.as_ref(), *loaded_at);
    let cx = common::Cx {
        snap: snapshot.as_ref(),
        now: now(),
        github: github_remote.is_some(),
        workdir: workdir.clone(),
    };
    match tab {
        ShipTab::Packages => packages_view::show(ui, &cx, rt, actions),
        ShipTab::Kubernetes => kube_view::show(ui, &cx, rt, console, actions),
        ShipTab::Terraform => tf_view::show(ui, &cx, rt, console, actions),
        _ => {}
    }
}

/// A dot on the rail entry and a line for its tooltip, when this area
/// needs attention: failing pods in the last Kubernetes fetch, drift found
/// by a drift check.
pub fn rail_badge(repo: &RepoState, _console: &Console) -> Option<(Tone, String)> {
    let rt = &repo.devops.runtime;
    let failing = rt.kube.failing();
    if !failing.is_empty() {
        let ns = rt
            .kube
            .load
            .as_ref()
            .map(|l| format!(" in {}", l.target.namespace))
            .unwrap_or_default();
        return Some((
            Tone::Danger,
            format!(
                "{} failing{ns}",
                crate::format::plural(failing.len(), "pod", "pods")
            ),
        ));
    }
    let drift = rt.tf.drifting();
    if let Some(first) = drift.first() {
        let more = if drift.len() > 1 {
            format!(" (and {} more)", drift.len() - 1)
        } else {
            String::new()
        };
        return Some((
            Tone::Warning,
            format!("terraform drift in {}{more}", root_display(first)),
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitvis_core::devops::proc::{Exit, Line, Stream};
    use gitvis_core::devops::tools::Installed;

    const PODS: &str =
        include_str!("../../../../gitvis-core/src/devops/runtime/fixtures/pods.json");
    const WORKLOADS: &str =
        include_str!("../../../../gitvis-core/src/devops/runtime/fixtures/workloads.json");

    fn repo() -> RepoState {
        RepoState::new(1, PathBuf::from("."))
    }

    fn cx() -> Ctx {
        Ctx::new(Some(1), Some(PathBuf::from(".")), None, None, true, false)
    }

    fn installed(name: &str) -> Installed {
        Installed {
            path: PathBuf::from(name),
            version: format!("{name} v1"),
        }
    }

    #[test]
    fn script_commands_map_to_actions() {
        for (cmd, arg) in [
            ("workload", "2"),
            ("pod", "0"),
            ("pod-logs", "1"),
            ("tf-root", "0"),
            ("tf-validate", ""),
            ("tf-fmt", ""),
            ("pkg", "3"),
            ("k8s-context", "k3d-shop"),
            ("k8s-ns", "shop"),
            ("k8s-scale", "2"),
            ("k8s-restart", ""),
        ] {
            let a = script(cmd, arg).unwrap_or_else(|| panic!("{cmd} {arg}"));
            assert_eq!(a.len(), 1, "{cmd}");
        }
        assert!(script("workload", "x").is_none());
        assert!(script("k8s-scale", "").is_none());
        assert!(script("nope", "").is_none());
    }

    #[test]
    fn failing_pods_raise_the_rail_badge() {
        let mut r = repo();
        assert!(rail_badge(&r, &Console::default()).is_none());
        let pods = kube::parse_pods(PODS.as_bytes()).unwrap();
        let workloads = kube::parse_workloads(WORKLOADS.as_bytes()).unwrap();
        r.devops.runtime.kube.load = Some(NamespaceLoad {
            target: Target::new("k3d-shop", "shop"),
            namespaces: Ok(Vec::new()),
            workloads: Some(Ok(workloads)),
            pods: Some(Ok(pods)),
        });
        let (tone, text) = rail_badge(&r, &Console::default()).unwrap();
        assert_eq!(tone, Tone::Danger);
        assert_eq!(text, "3 pods failing in shop");
        // Failed first in the list, then by kind and name.
        let k = &r.devops.runtime.kube;
        assert_eq!(k.visible()[0].name, "temporal-worker");
        let web = k
            .workload(&(WorkloadKind::Deployment, "web".into()))
            .unwrap();
        assert_eq!(k.pods_of(web).len(), 3);
        r.devops.runtime.kube.filter = "envoy".into();
        assert_eq!(r.devops.runtime.kube.visible().len(), 1, "filter by image");
    }

    #[test]
    fn drift_raises_a_warning_badge() {
        let mut r = repo();
        r.devops.runtime.tf.roots.insert(
            "infra/live".into(),
            RootRuns {
                drift: Some(DriftResult {
                    outcome: DriftOutcome::Drift,
                    text: PlanText::default(),
                    cause: None,
                    tail: String::new(),
                    at: 0,
                    ms: 0,
                }),
                ..RootRuns::default()
            },
        );
        let (tone, text) = rail_badge(&r, &Console::default()).unwrap();
        assert_eq!(tone, Tone::Warning);
        assert_eq!(text, "terraform drift in infra/live");
    }

    #[test]
    fn binary_choice() {
        let mut tf = TfState::default();
        assert_eq!(tf.binary(), None);
        tf.tools = Some((None, Some(installed("tofu"))));
        assert_eq!(tf.binary(), Some(Binary::Tofu));
        tf.tools = Some((Some(installed("terraform")), Some(installed("tofu"))));
        assert_eq!(tf.binary(), Some(Binary::Terraform));
        tf.binary = Some(Binary::Tofu);
        assert_eq!(tf.binary(), Some(Binary::Tofu));
        tf.tools = Some((Some(installed("terraform")), None));
        assert_eq!(
            tf.binary(),
            Some(Binary::Terraform),
            "the choice needs the tool"
        );
    }

    #[test]
    fn packages_linked_first() {
        let packages: Vec<Package> = serde_json::from_str(
            r#"[{"name":"other","updated_at":"2026-10-26T00:00:00Z"},
                {"name":"shop/web","updated_at":"2026-10-01T00:00:00Z","repository":{"full_name":"acme/shop"}},
                {"name":"shop/api","updated_at":"2026-10-20T00:00:00Z","repository":{"full_name":"acme/shop"}}]"#,
        )
        .unwrap();
        let mut st = PkgState {
            listing: Some(Ok(Listing {
                owner: "acme".into(),
                kind: OwnerKind::User,
                repo: "acme/shop".into(),
                private: false,
                packages,
            })),
            ..PkgState::default()
        };
        let names = |st: &PkgState| -> Vec<String> {
            st.visible().iter().map(|p| p.name.clone()).collect()
        };
        assert_eq!(names(&st), ["shop/api", "shop/web"]);
        st.show_all = true;
        assert_eq!(names(&st), ["shop/api", "shop/web", "other"]);
        st.filter = "WEB".into();
        assert_eq!(names(&st), ["shop/web"]);
    }

    fn finish(console: &mut Console, id: u64, title: &str, out: &str, code: i32) {
        console.add(
            id,
            Some(1),
            title.into(),
            gitvis_core::devops::proc::ProcSpec::new("terraform", "."),
        );
        console.output(
            id,
            out.lines()
                .map(|l| Line {
                    stream: Stream::Stdout,
                    text: l.to_owned(),
                })
                .collect(),
        );
        console.finished(
            id,
            Ok(Exit {
                code: Some(code),
                cancelled: false,
                elapsed_ms: 10,
            }),
        );
    }

    #[test]
    fn console_runs_are_parsed_into_results() {
        let mut r = repo();
        let mut console = Console::default();
        let tf = &mut r.devops.runtime.tf;
        tf.run_index
            .insert(901, ("infra".into(), Command::Validate, Binary::Terraform));
        tf.run_index
            .insert(902, ("infra".into(), Command::Drift, Binary::Terraform));
        tf.run_index
            .insert(903, ("infra".into(), Command::Plan, Binary::Terraform));
        finish(
            &mut console,
            901,
            "validate",
            r#"{"format_version":"1.0","valid":false,"error_count":1,"warning_count":0,"diagnostics":[{"severity":"error","summary":"Missing required provider","detail":"This configuration requires provider registry.terraform.io/hashicorp/aws, but that provider isn't available."}]}"#,
            1,
        );
        finish(
            &mut console,
            902,
            "drift",
            "Note: Objects have changed outside of Terraform\n  # aws_instance.web has changed\n",
            2,
        );
        finish(
            &mut console,
            903,
            "plan",
            "\u{2577}\n\u{2502} Error: No valid credential sources found\n\u{2502} \n\u{2502}   on main.tf line 3, in provider \"aws\":\n\u{2575}\n",
            1,
        );
        let mut c = cx();
        for id in [901, 902, 903] {
            on_console_done(&mut c, &mut r, console.get(id).unwrap());
        }
        let runs = &r.devops.runtime.tf.roots["infra"];
        let v = runs.validation.as_ref().unwrap();
        assert_eq!(v.result.as_ref().unwrap().errors, 1);
        assert!(v.cause.as_deref().unwrap().contains("run Init first"));
        let d = runs.drift.as_ref().unwrap();
        assert_eq!(d.outcome, DriftOutcome::Drift);
        assert_eq!(d.text.drift.len(), 1);
        let p = runs.plan.as_ref().unwrap();
        assert_eq!(p.outcome, PlanOutcome::Failed);
        assert_eq!(
            p.text.errors[0].summary,
            "No valid credential sources found"
        );
        assert!(p.cause.as_deref().unwrap().contains("credentials"));
        assert!(!p.json_loading, "no plan file, no show -json");
        assert_eq!(
            rail_badge(&r, &console).unwrap().1,
            "terraform drift in infra"
        );
    }

    #[test]
    fn digests_resolve_in_the_image_package_first() {
        let v = |id: u64| -> PackageVersion {
            serde_json::from_value(serde_json::json!({
                "id": id, "name": "sha256:abc",
                "metadata": {"container": {"tags": [format!("t{id}")]}}
            }))
            .unwrap()
        };
        let mut st = PkgState::default();
        for (name, id) in [("a/one", 1), ("b/two", 2)] {
            st.versions.insert(
                name.into(),
                Versions {
                    loading: false,
                    result: Some(Ok(vec![v(id)])),
                    fetched: None,
                },
            );
        }
        assert_eq!(
            st.tags_for_digest("sha256:abc", Some("b/two")).unwrap().0,
            "b/two"
        );
        assert_eq!(st.tags_for_digest("sha256:abc", None).unwrap().0, "a/one");
        assert!(st.tags_for_digest("sha256:zzz", None).is_none());
    }
}
