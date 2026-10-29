//! Kubernetes through `kubectl`.
//!
//! Rules:
//! - Every command that talks to a cluster gets an explicit `--context`
//!   and `--request-timeout` ([`REQUEST_TIMEOUT`]); Hadion never runs
//!   `kubectl config use-context` or edits the kubeconfig. Reading the
//!   kubeconfig (`config get-contexts`, `config current-context`, `config
//!   view`, which redacts credentials) is local.
//! - Following logs (`logs -f`) streams for as long as the user wants, so
//!   it gets `--request-timeout=0` (no client timeout; Stop ends it): a 10 s
//!   client timeout would cut the stream after 10 s.
//! - Writes ([`KubeWrite`]: `rollout restart`, `rollout undo`, `scale`) are
//!   built here and run by the app only after a confirmation.
//!
//! Everything a cluster answers is parsed with `#[serde(default)]` models
//! that tolerate missing and `null` fields.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Deserializer};

use super::matching::{RefIndex, RefMatch};
use crate::devops::proc::{ProcSpec, render_args, run_capture};
use crate::github::parse_time;

/// The client timeout for every call that talks to a cluster.
pub const REQUEST_TIMEOUT: &str = "--request-timeout=10s";
/// Reachability probes give up sooner.
pub const PROBE_TIMEOUT: &str = "--request-timeout=5s";
/// Following logs: no client timeout (see the module docs).
pub const FOLLOW_TIMEOUT: &str = "--request-timeout=0";
/// The process is killed after this, whatever kubectl does (discovery
/// retries can outlast the request timeout).
pub const PROCESS_TIMEOUT: Duration = Duration::from_secs(40);
/// Overrides the kubectl executable (a recording double for tests).
pub const PROGRAM_ENV: &str = "HADION_KUBECTL";
/// The `--tail` of a log request.
pub const LOG_TAIL: u32 = 500;
/// Annotations dropped when parsing (huge and useless here).
const NOISY_ANNOTATIONS: [&str; 1] = ["kubectl.kubernetes.io/last-applied-configuration"];
/// Where images say which commit they were built from.
pub const REVISION_KEYS: [&str; 3] = ["org.opencontainers.image.revision", "vcs-ref", "git-commit"];

pub fn program() -> String {
    super::program_override(PROGRAM_ENV, "kubectl")
}

/// kubectl's path and client version (honours [`PROGRAM_ENV`]).
pub fn detect() -> Option<crate::devops::tools::Installed> {
    super::detect(
        crate::devops::tools::Tool::Kubectl,
        PROGRAM_ENV,
        &["version", "--client"],
    )
}

/// Whether kubectl can be started (a PATH lookup).
pub fn available() -> bool {
    super::program_available(&program())
}

/// A context and the namespace to look at in it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct Target {
    pub context: String,
    pub namespace: String,
}

impl Target {
    pub fn new(context: impl Into<String>, namespace: impl Into<String>) -> Self {
        Self {
            context: context.into(),
            namespace: namespace.into(),
        }
    }
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_owned()).collect()
}

/// `--context C --request-timeout=10s`.
pub fn base(context: &str) -> Vec<String> {
    vec![
        "--context".into(),
        context.to_owned(),
        REQUEST_TIMEOUT.into(),
    ]
}

fn ns_base(t: &Target) -> Vec<String> {
    let mut a = base(&t.context);
    a.extend(["-n".into(), t.namespace.clone()]);
    a
}

// ---------------------------------------------------------------------------
// Argument builders (reads)
// ---------------------------------------------------------------------------

/// `config get-contexts -o name` (local).
pub fn contexts_args() -> Vec<String> {
    s(&["config", "get-contexts", "-o", "name"])
}

/// `config current-context` (local).
pub fn current_context_args() -> Vec<String> {
    s(&["config", "current-context"])
}

/// `config view -o json` (local; certificates and tokens are redacted).
pub fn config_view_args() -> Vec<String> {
    s(&["config", "view", "-o", "json"])
}

pub fn namespaces_args(context: &str) -> Vec<String> {
    let mut a = base(context);
    a.extend(s(&["get", "namespaces", "-o", "json"]));
    a
}

/// A short reachability check of one context.
pub fn probe_args(context: &str) -> Vec<String> {
    vec![
        "--context".into(),
        context.to_owned(),
        PROBE_TIMEOUT.into(),
        "get".into(),
        "namespaces".into(),
        "-o".into(),
        "name".into(),
    ]
}

pub fn workloads_args(t: &Target) -> Vec<String> {
    let mut a = ns_base(t);
    a.extend(s(&["get", "deploy,sts,ds", "-o", "json"]));
    a
}

/// Pods of the namespace, or of one workload with its label selector.
pub fn pods_args(t: &Target, selector: Option<&str>) -> Vec<String> {
    let mut a = ns_base(t);
    a.extend(s(&["get", "pods", "-o", "json"]));
    if let Some(sel) = selector.filter(|s| !s.is_empty()) {
        a.extend(["-l".into(), sel.to_owned()]);
    }
    a
}

/// Events about one object (a workload or a pod).
pub fn events_args(t: &Target, name: &str) -> Vec<String> {
    let mut a = ns_base(t);
    a.extend([
        "get".into(),
        "events".into(),
        "--field-selector".into(),
        format!("involvedObject.name={name}"),
        "-o".into(),
        "json".into(),
    ]);
    a
}

pub fn rollout_history_args(t: &Target, kind: WorkloadKind, name: &str) -> Vec<String> {
    let mut a = ns_base(t);
    a.extend([
        "rollout".into(),
        "history".into(),
        format!("{}/{name}", kind.resource()),
    ]);
    a
}

/// Options of a log request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogOpts {
    pub container: Option<String>,
    pub tail: u32,
    /// The previous (crashed) instance of the container.
    pub previous: bool,
    pub follow: bool,
    pub timestamps: bool,
}

impl Default for LogOpts {
    fn default() -> Self {
        Self {
            container: None,
            tail: LOG_TAIL,
            previous: false,
            follow: false,
            timestamps: false,
        }
    }
}

/// `--context C --request-timeout=… -n NS logs POD -c CONTAINER --tail=500
/// [--previous] [--timestamps] [-f]`.
pub fn logs_args(t: &Target, pod: &str, o: &LogOpts) -> Vec<String> {
    let mut a = vec![
        "--context".into(),
        t.context.clone(),
        if o.follow {
            FOLLOW_TIMEOUT.into()
        } else {
            REQUEST_TIMEOUT.into()
        },
        "-n".into(),
        t.namespace.clone(),
        "logs".into(),
        pod.to_owned(),
    ];
    if let Some(c) = o.container.as_deref().filter(|c| !c.is_empty()) {
        a.extend(["-c".into(), c.to_owned()]);
    }
    a.push(format!("--tail={}", o.tail));
    if o.previous {
        a.push("--previous".into());
    }
    if o.timestamps {
        a.push("--timestamps".into());
    }
    if o.follow {
        a.push("-f".into());
    }
    a
}

/// The command line as shown: `kubectl --context … get pods -o json`.
pub fn display(args: &[String]) -> String {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    format!("kubectl {}", render_args(&refs))
}

/// A process spec for the console (logs).
pub fn spec(dir: &Path, args: Vec<String>) -> ProcSpec {
    ProcSpec::new(program(), dir).args(args)
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// A change to a workload. Built here, run only after a confirmation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KubeWrite {
    /// `rollout restart`: new pods with the same spec.
    Restart { kind: WorkloadKind, name: String },
    /// `rollout undo [--to-revision=N]`: back to the previous (or the
    /// given) revision.
    Undo {
        kind: WorkloadKind,
        name: String,
        to_revision: Option<u32>,
    },
    /// `scale --replicas=N` (deployments and stateful sets).
    Scale {
        kind: WorkloadKind,
        name: String,
        replicas: u32,
    },
}

impl KubeWrite {
    pub fn args(&self, t: &Target) -> Vec<String> {
        let mut a = ns_base(t);
        match self {
            KubeWrite::Restart { kind, name } => a.extend([
                "rollout".into(),
                "restart".into(),
                format!("{}/{name}", kind.resource()),
            ]),
            KubeWrite::Undo {
                kind,
                name,
                to_revision,
            } => {
                a.extend([
                    "rollout".into(),
                    "undo".into(),
                    format!("{}/{name}", kind.resource()),
                ]);
                if let Some(r) = to_revision {
                    a.push(format!("--to-revision={r}"));
                }
            }
            KubeWrite::Scale {
                kind,
                name,
                replicas,
            } => a.extend([
                "scale".into(),
                format!("{}/{name}", kind.resource()),
                format!("--replicas={replicas}"),
            ]),
        }
        a
    }

    pub fn name(&self) -> &str {
        match self {
            KubeWrite::Restart { name, .. }
            | KubeWrite::Undo { name, .. }
            | KubeWrite::Scale { name, .. } => name,
        }
    }

    /// `Restart deployment api-gateway`, for titles and the activity log.
    pub fn title(&self) -> String {
        match self {
            KubeWrite::Restart { kind, name } => format!("Restart {} {name}", kind.noun()),
            KubeWrite::Undo {
                kind,
                name,
                to_revision: Some(r),
            } => format!("Roll {} {name} back to revision {r}", kind.noun()),
            KubeWrite::Undo { kind, name, .. } => {
                format!("Roll {} {name} back to the previous revision", kind.noun())
            }
            KubeWrite::Scale {
                kind,
                name,
                replicas,
            } => format!(
                "Scale {} {name} to {replicas} {}",
                kind.noun(),
                if *replicas == 1 {
                    "replica"
                } else {
                    "replicas"
                }
            ),
        }
    }

    /// What it does, for the confirmation.
    pub fn explain(&self) -> &'static str {
        match self {
            KubeWrite::Restart { .. } => {
                "Kubernetes replaces every pod with a new one running the same spec (a rolling restart, following the update strategy). Use it to pick up changed config maps or secrets, or to clear a stuck process."
            }
            KubeWrite::Undo { .. } => {
                "The pod template goes back to an earlier revision and Kubernetes rolls the pods over to it. The images and settings of that revision come back; anything changed since is replaced."
            }
            KubeWrite::Scale { .. } => {
                "The number of pods changes right away. Scaling to 0 stops every pod of the workload. A horizontal pod autoscaler, if any, may change it again."
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

/// `null` → the default value (Kubernetes writes `null` for empty fields).
fn nullable<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Meta {
    name: String,
    namespace: String,
    #[serde(deserialize_with = "nullable")]
    labels: BTreeMap<String, String>,
    #[serde(deserialize_with = "nullable")]
    annotations: BTreeMap<String, String>,
    creation_timestamp: Option<String>,
    deletion_timestamp: Option<String>,
    generation: i64,
    #[serde(deserialize_with = "nullable")]
    owner_references: Vec<OwnerRef>,
}

impl Meta {
    fn clean_annotations(&self) -> BTreeMap<String, String> {
        self.annotations
            .iter()
            .filter(|(k, _)| !NOISY_ANNOTATIONS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct OwnerRef {
    kind: String,
    name: String,
}

/// Decode a `kubectl get -o json` answer (a `List`, or a single object).
fn list<T: for<'de> Deserialize<'de> + Default>(
    bytes: &[u8],
    what: &str,
) -> Result<Vec<T>, String> {
    let v: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| format!("kubectl {what}: not JSON ({e})"))?;
    if let Some(items) = v.get("items") {
        if items.is_null() {
            return Ok(Vec::new());
        }
        serde_json::from_value(items.clone()).map_err(|e| format!("kubectl {what}: {e}"))
    } else {
        let one: T = serde_json::from_value(v).map_err(|e| format!("kubectl {what}: {e}"))?;
        Ok(vec![one])
    }
}

/// Which controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum WorkloadKind {
    #[default]
    Deployment,
    StatefulSet,
    DaemonSet,
}

impl WorkloadKind {
    pub fn from_kind(k: &str) -> Option<Self> {
        Some(match k {
            "Deployment" => WorkloadKind::Deployment,
            "StatefulSet" => WorkloadKind::StatefulSet,
            "DaemonSet" => WorkloadKind::DaemonSet,
            _ => return None,
        })
    }

    /// The short resource name kubectl takes: `deploy`, `sts`, `ds`.
    pub fn resource(self) -> &'static str {
        match self {
            WorkloadKind::Deployment => "deploy",
            WorkloadKind::StatefulSet => "sts",
            WorkloadKind::DaemonSet => "ds",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WorkloadKind::Deployment => "Deployment",
            WorkloadKind::StatefulSet => "StatefulSet",
            WorkloadKind::DaemonSet => "DaemonSet",
        }
    }

    /// Lower-case noun for sentences.
    pub fn noun(self) -> &'static str {
        match self {
            WorkloadKind::Deployment => "deployment",
            WorkloadKind::StatefulSet => "stateful set",
            WorkloadKind::DaemonSet => "daemon set",
        }
    }

    /// Daemon sets run one pod per node and cannot be scaled.
    pub fn scalable(self) -> bool {
        !matches!(self, WorkloadKind::DaemonSet)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawExpr {
    key: String,
    operator: String,
    #[serde(deserialize_with = "nullable")]
    values: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawSelector {
    #[serde(deserialize_with = "nullable")]
    match_labels: BTreeMap<String, String>,
    #[serde(deserialize_with = "nullable")]
    match_expressions: Vec<RawExpr>,
}

/// One `matchExpressions` entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Expr {
    pub key: String,
    /// `In`, `NotIn`, `Exists`, `DoesNotExist`.
    pub op: String,
    pub values: Vec<String>,
}

/// A label selector.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selector {
    pub labels: BTreeMap<String, String>,
    pub exprs: Vec<Expr>,
}

impl Selector {
    fn from_raw(r: Option<RawSelector>) -> Self {
        let r = r.unwrap_or_default();
        Self {
            labels: r.match_labels,
            exprs: r
                .match_expressions
                .into_iter()
                .map(|e| Expr {
                    key: e.key,
                    op: e.operator,
                    values: e.values,
                })
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.labels.is_empty() && self.exprs.is_empty()
    }

    /// The `-l` argument: `app=web,tier in (edge,frontend),!legacy`.
    pub fn to_arg(&self) -> String {
        let mut parts: Vec<String> = self
            .labels
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        for e in &self.exprs {
            parts.push(match e.op.as_str() {
                "In" => format!("{} in ({})", e.key, e.values.join(",")),
                "NotIn" => format!("{} notin ({})", e.key, e.values.join(",")),
                "Exists" => e.key.clone(),
                "DoesNotExist" => format!("!{}", e.key),
                other => format!("{} {other} ({})", e.key, e.values.join(",")),
            });
        }
        parts.join(",")
    }

    /// Does a pod with `labels` match? (An empty selector matches nothing,
    /// as for workloads.)
    pub fn matches(&self, labels: &BTreeMap<String, String>) -> bool {
        if self.is_empty() {
            return false;
        }
        self.labels.iter().all(|(k, v)| labels.get(k) == Some(v))
            && self.exprs.iter().all(|e| {
                let have = labels.get(&e.key);
                match e.op.as_str() {
                    "In" => have.is_some_and(|v| e.values.contains(v)),
                    "NotIn" => have.is_none_or(|v| !e.values.contains(v)),
                    "Exists" => have.is_some(),
                    "DoesNotExist" => have.is_none(),
                    _ => false,
                }
            })
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawContainer {
    name: String,
    image: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawPodSpec {
    #[serde(deserialize_with = "nullable")]
    containers: Vec<RawContainer>,
    #[serde(deserialize_with = "nullable")]
    init_containers: Vec<RawContainer>,
    node_name: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawTemplate {
    metadata: Meta,
    spec: RawPodSpec,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawRolling {
    partition: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawStrategy {
    #[serde(rename = "type")]
    kind: String,
    rolling_update: Option<RawRolling>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawWorkloadSpec {
    replicas: Option<i64>,
    selector: Option<RawSelector>,
    template: RawTemplate,
    paused: bool,
    strategy: Option<RawStrategy>,
    update_strategy: Option<RawStrategy>,
    progress_deadline_seconds: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawCondition {
    #[serde(rename = "type")]
    kind: String,
    status: String,
    reason: String,
    message: String,
    last_update_time: Option<String>,
    last_transition_time: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawWorkloadStatus {
    replicas: i64,
    ready_replicas: i64,
    updated_replicas: i64,
    available_replicas: i64,
    current_replicas: i64,
    observed_generation: i64,
    #[serde(deserialize_with = "nullable")]
    conditions: Vec<RawCondition>,
    current_revision: String,
    update_revision: String,
    desired_number_scheduled: i64,
    current_number_scheduled: i64,
    number_ready: i64,
    updated_number_scheduled: i64,
    number_available: i64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawWorkload {
    kind: String,
    metadata: Meta,
    spec: RawWorkloadSpec,
    status: RawWorkloadStatus,
}

/// A status condition (`Available`, `Progressing`, `ReplicaFailure`, …).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Condition {
    pub kind: String,
    pub status: String,
    pub reason: String,
    pub message: String,
    pub updated: Option<i64>,
    pub changed: Option<i64>,
}

impl Condition {
    pub fn is_true(&self) -> bool {
        self.status == "True"
    }
}

/// A container of a pod template.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Container {
    pub name: String,
    pub image: String,
}

/// A deployment, stateful set or daemon set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Workload {
    pub kind: WorkloadKind,
    pub name: String,
    pub namespace: String,
    pub created: Option<i64>,
    pub generation: i64,
    pub observed_generation: i64,
    pub labels: BTreeMap<String, String>,
    pub annotations: BTreeMap<String, String>,
    pub pod_labels: BTreeMap<String, String>,
    pub pod_annotations: BTreeMap<String, String>,
    pub selector: Selector,
    pub containers: Vec<Container>,
    pub init_containers: Vec<Container>,
    /// Pods wanted (spec.replicas; nodes for a daemon set).
    pub desired: i64,
    /// Pods that exist, old and new.
    pub current: i64,
    pub ready: i64,
    /// Pods on the newest template.
    pub updated: i64,
    pub available: i64,
    pub conditions: Vec<Condition>,
    pub paused: bool,
    /// `RollingUpdate`, `Recreate`, `OnDelete`.
    pub strategy: String,
    /// A stateful set's rolling-update partition.
    pub partition: Option<i64>,
    pub current_revision: String,
    pub update_revision: String,
    pub progress_deadline: Option<i64>,
}

impl Workload {
    fn from_raw(r: RawWorkload) -> Option<Self> {
        let kind = WorkloadKind::from_kind(&r.kind)?;
        let st = &r.status;
        let (desired, current, ready, updated, available) = match kind {
            WorkloadKind::DaemonSet => (
                st.desired_number_scheduled,
                st.current_number_scheduled,
                st.number_ready,
                st.updated_number_scheduled,
                st.number_available,
            ),
            _ => (
                r.spec.replicas.unwrap_or(1),
                st.replicas,
                st.ready_replicas,
                st.updated_replicas,
                st.available_replicas,
            ),
        };
        let strategy = match kind {
            WorkloadKind::Deployment => r.spec.strategy.as_ref(),
            _ => r.spec.update_strategy.as_ref(),
        };
        Some(Self {
            kind,
            name: r.metadata.name.clone(),
            namespace: r.metadata.namespace.clone(),
            created: r
                .metadata
                .creation_timestamp
                .as_deref()
                .and_then(parse_time),
            generation: r.metadata.generation,
            observed_generation: st.observed_generation,
            labels: r.metadata.labels.clone(),
            annotations: r.metadata.clean_annotations(),
            pod_labels: r.spec.template.metadata.labels.clone(),
            pod_annotations: r.spec.template.metadata.clean_annotations(),
            selector: Selector::from_raw(r.spec.selector.clone()),
            containers: r
                .spec
                .template
                .spec
                .containers
                .iter()
                .map(|c| Container {
                    name: c.name.clone(),
                    image: c.image.clone(),
                })
                .collect(),
            init_containers: r
                .spec
                .template
                .spec
                .init_containers
                .iter()
                .map(|c| Container {
                    name: c.name.clone(),
                    image: c.image.clone(),
                })
                .collect(),
            desired,
            current,
            ready,
            updated,
            available,
            conditions: st
                .conditions
                .iter()
                .map(|c| Condition {
                    kind: c.kind.clone(),
                    status: c.status.clone(),
                    reason: c.reason.clone(),
                    message: c.message.clone(),
                    updated: c.last_update_time.as_deref().and_then(parse_time),
                    changed: c.last_transition_time.as_deref().and_then(parse_time),
                })
                .collect(),
            paused: r.spec.paused,
            strategy: strategy
                .map(|s| s.kind.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "RollingUpdate".into()),
            partition: strategy
                .and_then(|s| s.rolling_update.as_ref())
                .and_then(|r| r.partition),
            current_revision: st.current_revision.clone(),
            update_revision: st.update_revision.clone(),
            progress_deadline: r.spec.progress_deadline_seconds,
        })
    }

    pub fn condition(&self, kind: &str) -> Option<&Condition> {
        self.conditions.iter().find(|c| c.kind == kind)
    }

    /// `deployment.kubernetes.io/revision`.
    pub fn revision(&self) -> Option<&str> {
        self.annotations
            .get("deployment.kubernetes.io/revision")
            .map(String::as_str)
    }

    /// The commit the workload says it runs: an OCI revision (or similar)
    /// annotation or label on the pod template, else on the workload.
    pub fn revision_hint(&self) -> Option<(&'static str, &str)> {
        for key in REVISION_KEYS {
            for map in [
                &self.pod_annotations,
                &self.pod_labels,
                &self.annotations,
                &self.labels,
            ] {
                if let Some(v) = map.get(key).filter(|v| !v.trim().is_empty()) {
                    return Some((key, v.as_str()));
                }
            }
        }
        None
    }

    /// `app.kubernetes.io/version`, when set.
    pub fn version_label(&self) -> Option<&str> {
        self.pod_labels
            .get("app.kubernetes.io/version")
            .or_else(|| self.labels.get("app.kubernetes.io/version"))
            .map(String::as_str)
    }

    /// `deploy/name`.
    pub fn resource(&self) -> String {
        format!("{}/{}", self.kind.resource(), self.name)
    }
}

pub fn parse_workloads(bytes: &[u8]) -> Result<Vec<Workload>, String> {
    let raw: Vec<RawWorkload> = list(bytes, "workloads")?;
    let mut out: Vec<Workload> = raw.into_iter().filter_map(Workload::from_raw).collect();
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

/// Where a rollout stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RolloutState {
    Failed,
    Stalled,
    Progressing,
    Complete,
    ScaledToZero,
}

impl RolloutState {
    pub fn label(self) -> &'static str {
        match self {
            RolloutState::Failed => "Failed",
            RolloutState::Stalled => "Stalled",
            RolloutState::Progressing => "Progressing",
            RolloutState::Complete => "Complete",
            RolloutState::ScaledToZero => "Scaled to zero",
        }
    }
}

/// A rollout state with its reason (short) and Kubernetes' own message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rollout {
    pub state: RolloutState,
    pub reason: String,
    pub message: String,
}

impl Rollout {
    fn new(state: RolloutState, reason: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            state,
            reason: reason.into(),
            message: message.into(),
        }
    }
}

/// Compute the rollout state from status and conditions, the way
/// `kubectl rollout status` does, plus what it leaves implicit:
/// - failed: the progress deadline passed, or a replica could not be
///   created;
/// - progressing: the controller has not seen the new spec yet, pods are
///   still being replaced, or new pods are not available yet while the
///   rollout is in progress;
/// - stalled: nothing is rolling but not every pod is ready / available
///   (crash loops after a finished rollout), the rollout is paused, or an
///   `OnDelete` stateful set waits for pods to be deleted;
/// - complete; scaled to zero.
pub fn rollout(w: &Workload) -> Rollout {
    use RolloutState::*;
    if w.observed_generation < w.generation {
        return Rollout::new(
            Progressing,
            "Waiting for the controller",
            format!(
                "Generation {} is not observed yet (observed {})",
                w.generation, w.observed_generation
            ),
        );
    }
    match w.kind {
        WorkloadKind::Deployment => {
            if let Some(c) = w.condition("Progressing")
                && c.reason == "ProgressDeadlineExceeded"
            {
                return Rollout::new(Failed, "Progress deadline exceeded", &c.message);
            }
            if let Some(c) = w.condition("ReplicaFailure")
                && c.is_true()
            {
                let reason = if c.reason.is_empty() {
                    "Replica failure".to_owned()
                } else {
                    c.reason.clone()
                };
                return Rollout::new(Failed, reason, &c.message);
            }
            if w.paused {
                return Rollout::new(
                    Stalled,
                    "Paused",
                    "The rollout is paused; `kubectl rollout resume` continues it",
                );
            }
            if w.desired == 0 {
                return if w.current == 0 {
                    Rollout::new(ScaledToZero, "Scaled to zero", "spec.replicas is 0")
                } else {
                    Rollout::new(
                        Progressing,
                        format!("{} pods terminating", w.current),
                        "Scaling down to 0",
                    )
                };
            }
            let progressing = w.condition("Progressing");
            let msg = progressing.map(|c| c.message.clone()).unwrap_or_default();
            if w.updated < w.desired {
                return Rollout::new(
                    Progressing,
                    format!("{} of {} pods updated", w.updated, w.desired),
                    msg,
                );
            }
            if w.current > w.updated {
                let old = w.current - w.updated;
                return Rollout::new(
                    Progressing,
                    format!(
                        "{old} old {} pending termination",
                        if old == 1 { "pod" } else { "pods" }
                    ),
                    msg,
                );
            }
            if w.available < w.updated {
                let done = progressing.is_some_and(|c| c.reason == "NewReplicaSetAvailable");
                let reason = format!("{} of {} pods available", w.available, w.updated);
                let message = w
                    .condition("Available")
                    .filter(|c| !c.is_true())
                    .map(|c| c.message.clone())
                    .unwrap_or(msg);
                return Rollout::new(if done { Stalled } else { Progressing }, reason, message);
            }
            Rollout::new(Complete, "Rolled out", msg)
        }
        WorkloadKind::StatefulSet => {
            if w.desired == 0 && w.current == 0 {
                return Rollout::new(ScaledToZero, "Scaled to zero", "spec.replicas is 0");
            }
            let pending_revision =
                !w.update_revision.is_empty() && w.update_revision != w.current_revision;
            if w.strategy == "OnDelete" {
                return if pending_revision && w.updated < w.desired {
                    Rollout::new(
                        Stalled,
                        format!("{} of {} pods updated (OnDelete)", w.updated, w.desired),
                        "With the OnDelete strategy, pods change only when they are deleted",
                    )
                } else if w.ready < w.desired {
                    Rollout::new(
                        Stalled,
                        format!("{} of {} pods ready", w.ready, w.desired),
                        "",
                    )
                } else {
                    Rollout::new(Complete, "All pods ready", "")
                };
            }
            let expected = w.desired - w.partition.unwrap_or(0).clamp(0, w.desired);
            if pending_revision && w.updated < expected {
                return Rollout::new(
                    Progressing,
                    format!("{} of {} pods updated", w.updated, expected),
                    format!("Rolling to revision {}", w.update_revision),
                );
            }
            if w.ready < w.desired {
                return Rollout::new(
                    if pending_revision {
                        Progressing
                    } else {
                        Stalled
                    },
                    format!("{} of {} pods ready", w.ready, w.desired),
                    "",
                );
            }
            if let Some(p) = w.partition.filter(|p| *p > 0) {
                return Rollout::new(
                    Complete,
                    format!("Partitioned: {expected} of {} pods updated", w.desired),
                    format!("Pods with an ordinal below {p} keep the old revision"),
                );
            }
            Rollout::new(Complete, "All pods ready", "")
        }
        WorkloadKind::DaemonSet => {
            if w.desired == 0 {
                return Rollout::new(
                    ScaledToZero,
                    "No nodes match",
                    "No node matches the daemon set's node selector",
                );
            }
            if w.updated < w.desired {
                return Rollout::new(
                    Progressing,
                    format!("{} of {} nodes updated", w.updated, w.desired),
                    "",
                );
            }
            if w.available < w.desired {
                return Rollout::new(
                    Stalled,
                    format!("{} of {} pods available", w.available, w.desired),
                    "",
                );
            }
            Rollout::new(Complete, "Every node runs the new pod", "")
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawWaiting {
    reason: String,
    message: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawRunning {
    started_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawTerminated {
    reason: String,
    message: String,
    exit_code: i32,
    signal: i32,
    started_at: Option<String>,
    finished_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawState {
    waiting: Option<RawWaiting>,
    running: Option<RawRunning>,
    terminated: Option<RawTerminated>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawContainerStatus {
    name: String,
    image: String,
    #[serde(rename = "imageID")]
    image_id: String,
    ready: bool,
    restart_count: i64,
    state: RawState,
    last_state: RawState,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawPodStatus {
    phase: String,
    reason: String,
    message: String,
    #[serde(rename = "podIP")]
    pod_ip: String,
    start_time: Option<String>,
    #[serde(deserialize_with = "nullable")]
    conditions: Vec<RawCondition>,
    #[serde(deserialize_with = "nullable")]
    container_statuses: Vec<RawContainerStatus>,
    #[serde(deserialize_with = "nullable")]
    init_container_statuses: Vec<RawContainerStatus>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawPod {
    metadata: Meta,
    spec: RawPodSpec,
    status: RawPodStatus,
}

/// How a container ended.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Terminated {
    /// `OOMKilled`, `Error`, `Completed`, …
    pub reason: String,
    pub message: String,
    pub exit_code: i32,
    pub signal: i32,
    pub started: Option<i64>,
    pub finished: Option<i64>,
}

impl Terminated {
    fn from_raw(t: &RawTerminated) -> Self {
        Self {
            reason: t.reason.clone(),
            message: t.message.clone(),
            exit_code: t.exit_code,
            signal: t.signal,
            started: t.started_at.as_deref().and_then(parse_time),
            finished: t.finished_at.as_deref().and_then(parse_time),
        }
    }

    /// `OOMKilled (exit 137)`, `Error (exit 1)`, `exit 2`.
    pub fn text(&self) -> String {
        let code = if self.signal != 0 {
            format!("signal {}", self.signal)
        } else {
            format!("exit {}", self.exit_code)
        };
        if self.reason.is_empty() {
            code
        } else {
            format!("{} ({code})", self.reason)
        }
    }
}

/// What a container is doing now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContainerState {
    Waiting { reason: String, message: String },
    Running { since: Option<i64> },
    Terminated(Terminated),
    Unknown,
}

impl ContainerState {
    fn from_raw(s: &RawState) -> Self {
        if let Some(w) = &s.waiting {
            ContainerState::Waiting {
                reason: w.reason.clone(),
                message: w.message.clone(),
            }
        } else if let Some(r) = &s.running {
            ContainerState::Running {
                since: r.started_at.as_deref().and_then(parse_time),
            }
        } else if let Some(t) = &s.terminated {
            ContainerState::Terminated(Terminated::from_raw(t))
        } else {
            ContainerState::Unknown
        }
    }

    /// `Running`, `CrashLoopBackOff`, `Completed`, `Waiting`.
    pub fn text(&self) -> String {
        match self {
            ContainerState::Waiting { reason, .. } if !reason.is_empty() => reason.clone(),
            ContainerState::Waiting { .. } => "Waiting".into(),
            ContainerState::Running { .. } => "Running".into(),
            ContainerState::Terminated(t) => t.text(),
            ContainerState::Unknown => "Unknown".into(),
        }
    }
}

/// A container of a pod, as the kubelet reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerStatus {
    pub name: String,
    pub image: String,
    /// `ghcr.io/o/n@sha256:…` (what was actually pulled).
    pub image_id: String,
    pub ready: bool,
    pub restarts: i64,
    pub state: ContainerState,
    /// The previous instance, when it ended (crash, OOM).
    pub last: Option<Terminated>,
}

impl ContainerStatus {
    fn from_raw(c: &RawContainerStatus) -> Self {
        Self {
            name: c.name.clone(),
            image: c.image.clone(),
            image_id: c.image_id.clone(),
            ready: c.ready,
            restarts: c.restart_count,
            state: ContainerState::from_raw(&c.state),
            last: c.last_state.terminated.as_ref().map(Terminated::from_raw),
        }
    }

    /// The pulled digest, `sha256:…`.
    pub fn digest(&self) -> Option<&str> {
        digest_of(&self.image_id)
    }
}

/// `…@sha256:abc` → `sha256:abc`.
pub fn digest_of(image: &str) -> Option<&str> {
    image
        .rsplit_once('@')
        .map(|(_, d)| d)
        .filter(|d| d.contains(':'))
}

/// A pod.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pod {
    pub name: String,
    pub namespace: String,
    pub labels: BTreeMap<String, String>,
    pub created: Option<i64>,
    pub started: Option<i64>,
    pub node: String,
    pub ip: String,
    pub phase: String,
    /// `Evicted`, `NodeLost`, …
    pub reason: String,
    pub message: String,
    pub deleting: bool,
    /// `(kind, name)` of the controller (`ReplicaSet`, `Job`, …).
    pub owner: Option<(String, String)>,
    /// Container names from the spec, in order.
    pub container_names: Vec<String>,
    pub init_count: usize,
    pub containers: Vec<ContainerStatus>,
    pub init_containers: Vec<ContainerStatus>,
    pub ready_condition: bool,
}

impl Pod {
    fn from_raw(r: RawPod) -> Self {
        Self {
            name: r.metadata.name.clone(),
            namespace: r.metadata.namespace.clone(),
            labels: r.metadata.labels.clone(),
            created: r
                .metadata
                .creation_timestamp
                .as_deref()
                .and_then(parse_time),
            started: r.status.start_time.as_deref().and_then(parse_time),
            node: r.spec.node_name.clone(),
            ip: r.status.pod_ip.clone(),
            phase: r.status.phase.clone(),
            reason: r.status.reason.clone(),
            message: r.status.message.clone(),
            deleting: r.metadata.deletion_timestamp.is_some(),
            owner: r
                .metadata
                .owner_references
                .first()
                .map(|o| (o.kind.clone(), o.name.clone())),
            container_names: r.spec.containers.iter().map(|c| c.name.clone()).collect(),
            init_count: r.spec.init_containers.len(),
            containers: r
                .status
                .container_statuses
                .iter()
                .map(ContainerStatus::from_raw)
                .collect(),
            init_containers: r
                .status
                .init_container_statuses
                .iter()
                .map(ContainerStatus::from_raw)
                .collect(),
            ready_condition: r
                .status
                .conditions
                .iter()
                .any(|c| c.kind == "Ready" && c.status == "True"),
        }
    }

    /// Containers in spec order (statuses come back sorted by name).
    pub fn containers_in_order(&self) -> Vec<&ContainerStatus> {
        let mut v: Vec<&ContainerStatus> = self.containers.iter().collect();
        v.sort_by_key(|c| {
            self.container_names
                .iter()
                .position(|n| n == &c.name)
                .unwrap_or(usize::MAX)
        });
        v
    }

    /// `(ready, total)` containers.
    pub fn ready(&self) -> (usize, usize) {
        let total = self.container_names.len().max(self.containers.len());
        (self.containers.iter().filter(|c| c.ready).count(), total)
    }

    pub fn restarts(&self) -> i64 {
        self.containers.iter().map(|c| c.restarts).sum()
    }

    /// The STATUS column of `kubectl get pods`.
    pub fn status_text(&self) -> String {
        let mut reason = if self.reason.is_empty() {
            self.phase.clone()
        } else {
            self.reason.clone()
        };
        let mut initializing = false;
        for (i, c) in self.init_containers.iter().enumerate() {
            match &c.state {
                ContainerState::Terminated(t) if t.exit_code == 0 && t.signal == 0 => continue,
                ContainerState::Terminated(t) => {
                    reason = if !t.reason.is_empty() {
                        format!("Init:{}", t.reason)
                    } else if t.signal != 0 {
                        format!("Init:Signal:{}", t.signal)
                    } else {
                        format!("Init:ExitCode:{}", t.exit_code)
                    };
                }
                ContainerState::Waiting { reason: r, .. }
                    if !r.is_empty() && r != "PodInitializing" =>
                {
                    reason = format!("Init:{r}");
                }
                _ => {
                    reason = format!(
                        "Init:{i}/{}",
                        self.init_count.max(self.init_containers.len())
                    );
                }
            }
            initializing = true;
            break;
        }
        if !initializing {
            let mut running = false;
            for c in self.containers.iter().rev() {
                match &c.state {
                    ContainerState::Waiting { reason: r, .. } if !r.is_empty() => {
                        reason = r.clone()
                    }
                    ContainerState::Terminated(t) if !t.reason.is_empty() => {
                        reason = t.reason.clone()
                    }
                    ContainerState::Terminated(t) if t.signal != 0 => {
                        reason = format!("Signal:{}", t.signal)
                    }
                    ContainerState::Terminated(t) => reason = format!("ExitCode:{}", t.exit_code),
                    ContainerState::Running { .. } if c.ready => running = true,
                    _ => {}
                }
            }
            if reason == "Completed" && running {
                reason = if self.ready_condition {
                    "Running".into()
                } else {
                    "NotReady".into()
                };
            }
        }
        if self.deleting {
            reason = if self.reason == "NodeLost" {
                "Unknown".into()
            } else {
                "Terminating".into()
            };
        }
        if reason.is_empty() {
            "Unknown".into()
        } else {
            reason
        }
    }

    /// Why the pod is failing, when it is: a container in
    /// `CrashLoopBackOff`, `ImagePullBackOff` / `ErrImagePull`, a config
    /// error, or terminated with `Error` / `OOMKilled` (and not done).
    pub fn failing(&self) -> Option<String> {
        if self.phase == "Succeeded" {
            return None;
        }
        for c in self.init_containers.iter().chain(&self.containers) {
            match &c.state {
                ContainerState::Waiting { reason, .. } if is_failure_reason(reason) => {
                    return Some(reason.clone());
                }
                ContainerState::Terminated(t)
                    if matches!(
                        t.reason.as_str(),
                        "Error" | "OOMKilled" | "ContainerCannotRun"
                    ) =>
                {
                    return Some(t.reason.clone());
                }
                _ => {}
            }
        }
        if self.phase == "Failed" {
            return Some(if self.reason.is_empty() {
                "Failed".into()
            } else {
                self.reason.clone()
            });
        }
        None
    }

    /// The most recent termination of any container: `(container, how)`.
    pub fn last_termination(&self) -> Option<(&str, &Terminated)> {
        self.containers
            .iter()
            .filter_map(|c| c.last.as_ref().map(|t| (c.name.as_str(), t)))
            .max_by_key(|(_, t)| t.finished.unwrap_or(0))
    }
}

/// Waiting reasons that mean the pod will not start without help.
pub fn is_failure_reason(r: &str) -> bool {
    matches!(
        r,
        "CrashLoopBackOff"
            | "ImagePullBackOff"
            | "ErrImagePull"
            | "InvalidImageName"
            | "CreateContainerConfigError"
            | "CreateContainerError"
            | "RunContainerError"
            | "ErrImageNeverPull"
    )
}

pub fn parse_pods(bytes: &[u8]) -> Result<Vec<Pod>, String> {
    let raw: Vec<RawPod> = list(bytes, "pods")?;
    let mut out: Vec<Pod> = raw.into_iter().map(Pod::from_raw).collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawObjRef {
    kind: String,
    name: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawSource {
    component: String,
    host: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawSeries {
    count: i64,
    last_observed_time: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawEvent {
    metadata: Meta,
    #[serde(rename = "type")]
    kind: String,
    reason: String,
    message: String,
    count: Option<i64>,
    first_timestamp: Option<String>,
    last_timestamp: Option<String>,
    event_time: Option<String>,
    involved_object: RawObjRef,
    #[serde(deserialize_with = "nullable")]
    source: RawSource,
    reporting_component: String,
    series: Option<RawSeries>,
}

/// A Kubernetes event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Event {
    /// `Normal` or `Warning`.
    pub kind: String,
    pub reason: String,
    pub message: String,
    pub count: i64,
    pub first: Option<i64>,
    pub last: Option<i64>,
    /// `Pod/web-7c9d8f6b5-qwert`.
    pub object: String,
    /// `kubelet on k3d-shop-agent-0`, `deployment-controller`.
    pub source: String,
}

impl Event {
    pub fn warning(&self) -> bool {
        self.kind == "Warning"
    }
}

pub fn parse_events(bytes: &[u8]) -> Result<Vec<Event>, String> {
    let raw: Vec<RawEvent> = list(bytes, "events")?;
    let mut out: Vec<Event> = raw
        .into_iter()
        .map(|e| {
            let created = e
                .metadata
                .creation_timestamp
                .as_deref()
                .and_then(parse_time);
            let event_time = e.event_time.as_deref().and_then(parse_time);
            let series_last = e
                .series
                .as_ref()
                .and_then(|s| s.last_observed_time.as_deref())
                .and_then(parse_time);
            let first = e
                .first_timestamp
                .as_deref()
                .and_then(parse_time)
                .or(event_time)
                .or(created);
            let last = e
                .last_timestamp
                .as_deref()
                .and_then(parse_time)
                .or(series_last)
                .or(event_time)
                .or(created);
            let count = e
                .count
                .filter(|c| *c > 0)
                .or(e.series.as_ref().map(|s| s.count).filter(|c| *c > 0))
                .unwrap_or(1);
            let component = if e.source.component.is_empty() {
                e.reporting_component.clone()
            } else {
                e.source.component.clone()
            };
            let source = if e.source.host.is_empty() {
                component
            } else {
                format!("{component} on {}", e.source.host)
            };
            Event {
                kind: e.kind,
                reason: e.reason,
                message: e.message.trim().to_owned(),
                count,
                first,
                last,
                object: format!("{}/{}", e.involved_object.kind, e.involved_object.name),
                source,
            }
        })
        .collect();
    out.sort_by_key(|e| std::cmp::Reverse(e.last));
    Ok(out)
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawNsStatus {
    phase: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawNamespace {
    metadata: Meta,
    status: RawNsStatus,
}

/// A namespace.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Namespace {
    pub name: String,
    /// `Active` or `Terminating`.
    pub phase: String,
    pub created: Option<i64>,
}

pub fn parse_namespaces(bytes: &[u8]) -> Result<Vec<Namespace>, String> {
    let raw: Vec<RawNamespace> = list(bytes, "namespaces")?;
    let mut out: Vec<Namespace> = raw
        .into_iter()
        .map(|n| Namespace {
            name: n.metadata.name,
            phase: n.status.phase,
            created: n
                .metadata
                .creation_timestamp
                .as_deref()
                .and_then(parse_time),
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawCtxInner {
    cluster: String,
    user: String,
    namespace: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawNamedCtx {
    name: String,
    context: RawCtxInner,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawClusterInner {
    server: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawNamedCluster {
    name: String,
    cluster: RawClusterInner,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawConfig {
    #[serde(rename = "current-context")]
    current_context: String,
    #[serde(deserialize_with = "nullable")]
    contexts: Vec<RawNamedCtx>,
    #[serde(deserialize_with = "nullable")]
    clusters: Vec<RawNamedCluster>,
}

/// A kubeconfig context.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KubeContext {
    pub name: String,
    pub cluster: String,
    pub user: String,
    /// The context's default namespace.
    pub namespace: Option<String>,
    /// The API server URL.
    pub server: Option<String>,
}

/// The contexts of the kubeconfig and which one is current.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KubeConfig {
    pub contexts: Vec<KubeContext>,
    pub current: Option<String>,
}

impl KubeConfig {
    pub fn context(&self, name: &str) -> Option<&KubeContext> {
        self.contexts.iter().find(|c| c.name == name)
    }

    /// The context to use: `wanted` when it exists, else the current one,
    /// else the first.
    pub fn choose(&self, wanted: Option<&str>) -> Option<String> {
        wanted
            .filter(|w| self.context(w).is_some())
            .map(str::to_owned)
            .or_else(|| self.current.clone().filter(|c| self.context(c).is_some()))
            .or_else(|| self.contexts.first().map(|c| c.name.clone()))
    }

    /// The namespace to use in a context: `wanted`, else the context's
    /// default, else `default`.
    pub fn namespace_for(&self, context: &str, wanted: Option<&str>) -> String {
        wanted
            .filter(|w| !w.is_empty())
            .map(str::to_owned)
            .or_else(|| self.context(context).and_then(|c| c.namespace.clone()))
            .unwrap_or_else(|| "default".into())
    }
}

/// Merge `config get-contexts -o name`, `config current-context` and
/// `config view -o json` (any of the last two may be missing).
pub fn parse_config(names: &str, current: Option<&str>, view: Option<&[u8]>) -> KubeConfig {
    let raw: RawConfig = view
        .and_then(|v| serde_json::from_slice(v).ok())
        .unwrap_or_default();
    let mut names: Vec<String> = names
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    if names.is_empty() {
        names = raw.contexts.iter().map(|c| c.name.clone()).collect();
    }
    let contexts = names
        .into_iter()
        .map(|name| {
            let c = raw.contexts.iter().find(|c| c.name == name);
            let cluster = c.map(|c| c.context.cluster.clone()).unwrap_or_default();
            let server = raw
                .clusters
                .iter()
                .find(|k| k.name == cluster)
                .map(|k| k.cluster.server.clone())
                .filter(|s| !s.is_empty());
            KubeContext {
                user: c.map(|c| c.context.user.clone()).unwrap_or_default(),
                namespace: c
                    .map(|c| c.context.namespace.clone())
                    .filter(|n| !n.is_empty()),
                cluster,
                server,
                name,
            }
        })
        .collect();
    let current = current
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_owned)
        .or_else(|| Some(raw.current_context).filter(|c| !c.is_empty()));
    KubeConfig { contexts, current }
}

/// One line of `kubectl rollout history`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revision {
    pub number: u32,
    /// `CHANGE-CAUSE` (`None` for `<none>`).
    pub cause: Option<String>,
}

/// ```text
/// deployment.apps/api-gateway
/// REVISION  CHANGE-CAUSE
/// 6         <none>
/// 7         kubectl set image deployment/api-gateway api-gateway=…
/// ```
pub fn parse_rollout_history(text: &str) -> Vec<Revision> {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        let mut parts = t.splitn(2, char::is_whitespace);
        let Some(n) = parts.next().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let cause = parts
            .next()
            .map(str::trim)
            .filter(|c| !c.is_empty() && *c != "<none>")
            .map(str::to_owned);
        out.push(Revision { number: n, cause });
    }
    out.sort_by_key(|r| std::cmp::Reverse(r.number));
    out
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

/// `registry/repository:tag@digest`, split.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageRef {
    /// `ghcr.io`, `docker.io`, `quay.io`, `localhost:5000`.
    pub registry: String,
    /// `acme/shop/api-gateway`, `library/postgres`.
    pub repository: String,
    pub tag: Option<String>,
    pub digest: Option<String>,
}

impl ImageRef {
    /// The last path segment: `api-gateway`.
    pub fn name(&self) -> &str {
        self.repository
            .rsplit('/')
            .next()
            .unwrap_or(&self.repository)
    }

    /// The repository without the owner on GHCR (`shop/api-gateway` for
    /// `ghcr.io/acme/shop/api-gateway`), which is the package name.
    pub fn ghcr_package(&self) -> Option<(&str, &str)> {
        if self.registry != "ghcr.io" {
            return None;
        }
        self.repository.split_once('/')
    }
}

pub fn parse_image(image: &str) -> ImageRef {
    let image = image.trim();
    let (rest, digest) = match image.split_once('@') {
        Some((r, d)) => (r, Some(d.to_owned())),
        None => (image, None),
    };
    let last_slash = rest.rfind('/').map_or(0, |i| i + 1);
    let (name_part, tag) = match rest[last_slash..].rfind(':') {
        Some(i) => (
            &rest[..last_slash + i],
            Some(rest[last_slash + i + 1..].to_owned()),
        ),
        None => (rest, None),
    };
    let (registry, repository) = match name_part.split_once('/') {
        Some((first, more))
            if first.contains('.') || first.contains(':') || first == "localhost" =>
        {
            (first.to_owned(), more.to_owned())
        }
        Some(_) => ("docker.io".to_owned(), name_part.to_owned()),
        None => ("docker.io".to_owned(), format!("library/{name_part}")),
    };
    ImageRef {
        registry,
        repository,
        tag: tag.filter(|t| !t.is_empty()),
        digest,
    }
}

/// Which commit a container image is, best evidence first: the OCI
/// revision (annotation or label), the image tag, then the
/// `app.kubernetes.io/version` label.
pub fn image_match(
    idx: &RefIndex,
    image: &ImageRef,
    revision: Option<&str>,
    version: Option<&str>,
) -> Option<RefMatch> {
    let from_rev = revision.and_then(|r| idx.match_revision(r));
    if let Some(m) = &from_rev
        && m.commit.is_some()
    {
        return from_rev;
    }
    let from_tag = image.tag.as_deref().and_then(|t| idx.match_tag(t));
    let from_version = version.and_then(|v| idx.match_tag(v));
    [from_tag, from_version, from_rev]
        .into_iter()
        .flatten()
        .min_by_key(|m| (m.commit.is_none(), m.kind))
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// What kind of failure a kubectl message is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The API server does not answer (stopped cluster, VPN, wrong port).
    Unreachable,
    Timeout,
    /// Credentials expired or rejected.
    Unauthorized,
    /// RBAC: the user may not do this.
    Forbidden,
    NotFound,
    /// The context is not in the kubeconfig.
    NoContext,
    /// kubectl itself is missing.
    NotInstalled,
    Other,
}

pub fn classify(err: &str) -> ErrorKind {
    let l = err.to_ascii_lowercase();
    if l.contains("not installed") || l.contains("not found on path") {
        ErrorKind::NotInstalled
    } else if l.contains("context was not found") || l.contains("no context exists") {
        ErrorKind::NoContext
    } else if l.contains("forbidden") {
        ErrorKind::Forbidden
    } else if l.contains("unauthorized") || l.contains("must be logged in") {
        ErrorKind::Unauthorized
    } else if l.contains("timed out") || l.contains("timeout") || l.contains("deadline exceeded") {
        ErrorKind::Timeout
    } else if l.contains("unable to connect")
        || l.contains("connection refused")
        || l.contains("actively refused")
        || l.contains("no such host")
        || l.contains("connectex")
        || l.contains("dial tcp")
        || l.contains("eof")
    {
        ErrorKind::Unreachable
    } else if l.contains("notfound") || l.contains("not found") {
        ErrorKind::NotFound
    } else {
        ErrorKind::Other
    }
}

impl ErrorKind {
    /// The cluster could not be reached at all (no point in more calls).
    pub fn is_unreachable(self) -> bool {
        matches!(self, ErrorKind::Unreachable | ErrorKind::Timeout)
    }

    pub fn title(self) -> &'static str {
        match self {
            ErrorKind::Unreachable => "The cluster is not reachable",
            ErrorKind::Timeout => "The cluster did not answer in time",
            ErrorKind::Unauthorized => "The cluster rejected the credentials",
            ErrorKind::Forbidden => "Not allowed",
            ErrorKind::NotFound => "Not found",
            ErrorKind::NoContext => "The context is not in the kubeconfig",
            ErrorKind::NotInstalled => "kubectl is not installed",
            ErrorKind::Other => "kubectl failed",
        }
    }
}

/// A likely way out for a context that cannot be reached, as advice and
/// an optional command the user can copy (never run by Hadion).
pub fn hint(kind: ErrorKind, context: &str) -> (String, Option<String>) {
    match kind {
        ErrorKind::Unreachable | ErrorKind::Timeout => {
            if let Some(name) = context.strip_prefix("k3d-") {
                (
                    "This is a k3d cluster: it runs in Docker and stops with Docker Desktop. Start Docker, then the cluster.".to_owned(),
                    Some(format!("k3d cluster start {name}")),
                )
            } else if let Some(name) = context.strip_prefix("kind-") {
                (
                    "This is a kind cluster: its node is a Docker container that stops with Docker Desktop. Start Docker, then the node container.".to_owned(),
                    Some(format!("docker start {name}-control-plane")),
                )
            } else if context == "docker-desktop" {
                (
                    "Docker Desktop's Kubernetes is off or Docker is not running: enable it under Settings → Kubernetes.".to_owned(),
                    None,
                )
            } else if context.starts_with("minikube") {
                ("The minikube VM is stopped.".to_owned(), Some("minikube start".to_owned()))
            } else if context.starts_with("arn:aws:eks") || context.contains("eks") {
                (
                    "An EKS cluster: check the VPN or network, and that the API endpoint is reachable from here.".to_owned(),
                    None,
                )
            } else {
                (
                    "Check that the cluster is running and reachable from this machine (VPN, network, API server address).".to_owned(),
                    None,
                )
            }
        }
        ErrorKind::Unauthorized => (
            "The credentials in the kubeconfig expired or were revoked. Log in again with the tool that created the context (aws eks update-kubeconfig, gcloud, az aks get-credentials, …).".to_owned(),
            None,
        ),
        ErrorKind::Forbidden => (
            "Your user may not read this resource. Pick a namespace you have access to, or ask for a role binding.".to_owned(),
            None,
        ),
        ErrorKind::NoContext => (
            "The kubeconfig changed since it was read. Refresh to read it again.".to_owned(),
            None,
        ),
        ErrorKind::NotInstalled => (
            crate::devops::tools::Tool::Kubectl.install_hint().to_owned(),
            None,
        ),
        ErrorKind::NotFound | ErrorKind::Other => (String::new(), None),
    }
}

/// kubectl's stderr without the klog noise (`E1026 21:05:21.449 … memcache.go:265] …`
/// lines repeated for every discovery retry): the lines a person needs.
pub fn clean_error(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let plain: Vec<&str> = lines.iter().copied().filter(|l| !is_klog(l)).collect();
    let mut out: Vec<&str> = Vec::new();
    for l in if plain.is_empty() { &lines } else { &plain } {
        if !out.contains(l) {
            out.push(l);
        }
    }
    out.join("\n")
}

/// `E1026 21:05:21.449547   72660 memcache.go:265] …`.
fn is_klog(l: &str) -> bool {
    let b = l.as_bytes();
    b.len() > 22
        && matches!(b[0], b'E' | b'W' | b'I' | b'F')
        && b[1..5].iter().all(u8::is_ascii_digit)
        && b[5] == b' '
        && l.contains("] ")
}

// ---------------------------------------------------------------------------
// Runners
// ---------------------------------------------------------------------------

/// Run kubectl with `args` (recorded in `commands`), returning stdout. The
/// error is the cleaned stderr.
pub fn run(dir: &Path, args: &[String], commands: &mut Vec<String>) -> Result<Vec<u8>, String> {
    commands.push(display(args));
    let spec = ProcSpec::new(program(), dir).args(args.iter().cloned());
    let out = run_capture(&spec, Some(PROCESS_TIMEOUT)).map_err(|e| e.to_string())?;
    if out.success() {
        return Ok(out.stdout);
    }
    if out.timed_out {
        return Err(format!(
            "kubectl did not finish within {} s and was stopped (timed out).",
            PROCESS_TIMEOUT.as_secs()
        ));
    }
    let msg = clean_error(&out.stderr);
    Err(if msg.is_empty() {
        out.error_text()
    } else {
        msg
    })
}

/// The kubeconfig: contexts, the current one, servers and namespaces.
pub fn load_config(dir: &Path, commands: &mut Vec<String>) -> Result<KubeConfig, String> {
    let names = run(dir, &contexts_args(), commands)?;
    // No current context is not an error worth stopping for.
    let current = run(dir, &current_context_args(), commands).ok();
    let view = run(dir, &config_view_args(), commands).ok();
    let cfg = parse_config(
        &String::from_utf8_lossy(&names),
        current
            .as_ref()
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .as_deref(),
        view.as_deref(),
    );
    Ok(cfg)
}

/// What the Kubernetes tab loads for one context and namespace.
#[derive(Clone, Debug)]
pub struct NamespaceLoad {
    pub target: Target,
    pub namespaces: Result<Vec<Namespace>, String>,
    /// `None` when skipped (the cluster could not be reached).
    pub workloads: Option<Result<Vec<Workload>, String>>,
    pub pods: Option<Result<Vec<Pod>, String>>,
}

impl NamespaceLoad {
    /// The error that stops everything, if the cluster was unreachable.
    pub fn fatal(&self) -> Option<&str> {
        match (&self.namespaces, &self.workloads) {
            (Err(e), None) => Some(e),
            (_, Some(Err(e))) if classify(e).is_unreachable() => Some(e),
            _ => None,
        }
    }
}

/// Namespaces, workloads and every pod of the namespace. When the cluster
/// cannot be reached the later calls are skipped; when only listing
/// namespaces is forbidden, the namespace is still read.
pub fn load_namespace(dir: &Path, target: &Target, commands: &mut Vec<String>) -> NamespaceLoad {
    let namespaces =
        run(dir, &namespaces_args(&target.context), commands).and_then(|b| parse_namespaces(&b));
    if let Err(e) = &namespaces
        && (classify(e).is_unreachable()
            || matches!(classify(e), ErrorKind::Unauthorized | ErrorKind::NoContext))
    {
        return NamespaceLoad {
            target: target.clone(),
            namespaces,
            workloads: None,
            pods: None,
        };
    }
    let workloads = run(dir, &workloads_args(target), commands).and_then(|b| parse_workloads(&b));
    let pods = run(dir, &pods_args(target, None), commands).and_then(|b| parse_pods(&b));
    NamespaceLoad {
        target: target.clone(),
        namespaces,
        workloads: Some(workloads),
        pods: Some(pods),
    }
}

/// What the detail of a workload loads when it is selected.
#[derive(Clone, Debug)]
pub struct DetailLoad {
    pub pods: Result<Vec<Pod>, String>,
    pub events: Result<Vec<Event>, String>,
    pub history: Result<Vec<Revision>, String>,
}

pub fn load_detail(
    dir: &Path,
    target: &Target,
    kind: WorkloadKind,
    name: &str,
    selector: &Selector,
    commands: &mut Vec<String>,
) -> DetailLoad {
    let pods = if selector.is_empty() {
        Ok(Vec::new())
    } else {
        run(dir, &pods_args(target, Some(&selector.to_arg())), commands)
            .and_then(|b| parse_pods(&b))
    };
    let events = run(dir, &events_args(target, name), commands).and_then(|b| parse_events(&b));
    let history = run(dir, &rollout_history_args(target, kind, name), commands)
        .map(|b| parse_rollout_history(&String::from_utf8_lossy(&b)));
    DetailLoad {
        pods,
        events,
        history,
    }
}

/// Events about one pod.
pub fn load_pod_events(
    dir: &Path,
    target: &Target,
    pod: &str,
    commands: &mut Vec<String>,
) -> Result<Vec<Event>, String> {
    run(dir, &events_args(target, pod), commands).and_then(|b| parse_events(&b))
}

/// Whether a context answers: the number of namespaces it lists.
pub fn probe(dir: &Path, context: &str, commands: &mut Vec<String>) -> Result<usize, String> {
    run(dir, &probe_args(context), commands).map(|b| {
        String::from_utf8_lossy(&b)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count()
    })
}

/// Run a confirmed write; kubectl's answer (`deployment.apps/x restarted`).
pub fn run_write(
    dir: &Path,
    target: &Target,
    write: &KubeWrite,
    commands: &mut Vec<String>,
) -> Result<String, String> {
    run(dir, &write.args(target), commands).map(|b| String::from_utf8_lossy(&b).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Oid;

    const WORKLOADS: &str = include_str!("fixtures/workloads.json");
    const PODS: &str = include_str!("fixtures/pods.json");
    const EVENTS: &str = include_str!("fixtures/events.json");
    const CONFIG: &str = include_str!("fixtures/config_view.json");
    const NAMESPACES: &str = include_str!("fixtures/namespaces.json");

    fn t() -> Target {
        Target::new("k3d-shop", "shop")
    }

    fn has_guards(a: &[String], ctx: &str) {
        let i = a.iter().position(|x| x == "--context").expect("--context");
        assert_eq!(a[i + 1], ctx);
        assert!(
            a.iter().any(|x| x.starts_with("--request-timeout=")),
            "{a:?}"
        );
    }

    #[test]
    fn every_cluster_command_has_context_and_timeout() {
        let target = t();
        let sel = Selector {
            labels: [("app".to_owned(), "web".to_owned())].into(),
            exprs: vec![],
        };
        let all = vec![
            namespaces_args("k3d-shop"),
            probe_args("k3d-shop"),
            workloads_args(&target),
            pods_args(&target, None),
            pods_args(&target, Some(&sel.to_arg())),
            events_args(&target, "web"),
            rollout_history_args(&target, WorkloadKind::Deployment, "web"),
            logs_args(&target, "web-1", &LogOpts::default()),
            logs_args(
                &target,
                "web-1",
                &LogOpts {
                    follow: true,
                    ..LogOpts::default()
                },
            ),
            KubeWrite::Restart {
                kind: WorkloadKind::Deployment,
                name: "web".into(),
            }
            .args(&target),
            KubeWrite::Undo {
                kind: WorkloadKind::StatefulSet,
                name: "db".into(),
                to_revision: Some(3),
            }
            .args(&target),
            KubeWrite::Scale {
                kind: WorkloadKind::Deployment,
                name: "web".into(),
                replicas: 0,
            }
            .args(&target),
        ];
        for a in &all {
            has_guards(a, "k3d-shop");
            assert!(!a.iter().any(|x| x == "use-context"));
        }
    }

    #[test]
    fn read_arguments() {
        assert_eq!(
            display(&workloads_args(&t())),
            "kubectl --context k3d-shop --request-timeout=10s -n shop get deploy,sts,ds -o json"
        );
        assert_eq!(
            events_args(&t(), "web")[5..],
            [
                "get",
                "events",
                "--field-selector",
                "involvedObject.name=web",
                "-o",
                "json"
            ]
        );
        let p = pods_args(&t(), Some("app=web,tier in (edge,frontend)"));
        assert_eq!(p[p.len() - 2..], ["-l", "app=web,tier in (edge,frontend)"]);
        assert_eq!(
            display(&rollout_history_args(&t(), WorkloadKind::DaemonSet, "x")),
            "kubectl --context k3d-shop --request-timeout=10s -n shop rollout history ds/x"
        );
        assert_eq!(probe_args("c")[2], PROBE_TIMEOUT);
    }

    #[test]
    fn log_arguments() {
        let o = LogOpts {
            container: Some("worker".into()),
            previous: true,
            ..LogOpts::default()
        };
        assert_eq!(
            display(&logs_args(&t(), "w-1", &o)),
            "kubectl --context k3d-shop --request-timeout=10s -n shop logs w-1 -c worker --tail=500 --previous"
        );
        let f = LogOpts {
            follow: true,
            timestamps: true,
            ..LogOpts::default()
        };
        let a = logs_args(&t(), "w-1", &f);
        assert_eq!(a[2], FOLLOW_TIMEOUT, "follow is not cut after 10 s");
        assert_eq!(a.last().unwrap(), "-f");
        assert!(a.contains(&"--timestamps".to_owned()));
    }

    #[test]
    fn write_arguments() {
        let target = t();
        let r = KubeWrite::Restart {
            kind: WorkloadKind::Deployment,
            name: "api-gateway".into(),
        };
        assert_eq!(
            display(&r.args(&target)),
            "kubectl --context k3d-shop --request-timeout=10s -n shop rollout restart deploy/api-gateway"
        );
        assert_eq!(r.title(), "Restart deployment api-gateway");
        let u = KubeWrite::Undo {
            kind: WorkloadKind::Deployment,
            name: "web".into(),
            to_revision: None,
        };
        assert!(display(&u.args(&target)).ends_with("rollout undo deploy/web"));
        let u2 = KubeWrite::Undo {
            kind: WorkloadKind::Deployment,
            name: "web".into(),
            to_revision: Some(11),
        };
        assert!(display(&u2.args(&target)).ends_with("rollout undo deploy/web --to-revision=11"));
        assert_eq!(u2.title(), "Roll deployment web back to revision 11");
        let s = KubeWrite::Scale {
            kind: WorkloadKind::StatefulSet,
            name: "postgres".into(),
            replicas: 2,
        };
        assert!(display(&s.args(&target)).ends_with("scale sts/postgres --replicas=2"));
        assert_eq!(s.title(), "Scale stateful set postgres to 2 replicas");
        assert!(!WorkloadKind::DaemonSet.scalable());
    }

    #[test]
    fn workloads_parse() {
        let w = parse_workloads(WORKLOADS.as_bytes()).unwrap();
        let names: Vec<&str> = w.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "api-gateway",
                "docs",
                "temporal-worker",
                "web",
                "postgres",
                "node-exporter"
            ]
        );
        let api = &w[0];
        assert_eq!(api.kind, WorkloadKind::Deployment);
        assert_eq!(
            (api.desired, api.ready, api.updated, api.available),
            (3, 3, 3, 3)
        );
        assert_eq!(api.revision(), Some("7"));
        assert!(
            !api.annotations
                .contains_key("kubectl.kubernetes.io/last-applied-configuration"),
            "noise dropped"
        );
        assert_eq!(
            api.revision_hint(),
            Some((
                "org.opencontainers.image.revision",
                "9a9d91b5c9cac39b1eda670a0f07c16aec2149ba"
            ))
        );
        assert_eq!(api.version_label(), Some("0.9.0"));
        assert_eq!(
            api.containers[0].image,
            "ghcr.io/acme/shop/api-gateway:sha-9a9d91b"
        );
        assert_eq!(api.created, parse_time("2026-09-19T08:14:02Z"));
        let web = &w[3];
        assert_eq!(web.containers.len(), 2);
        assert_eq!(web.selector.to_arg(), "app=web,tier in (frontend,edge)");
        let tw = &w[2];
        assert_eq!(tw.init_containers[0].name, "migrate");
        assert_eq!((tw.ready, tw.available), (0, 0));
        let pg = &w[4];
        assert_eq!(pg.kind, WorkloadKind::StatefulSet);
        assert_eq!(pg.partition, Some(0));
        let ds = &w[5];
        assert_eq!(ds.kind, WorkloadKind::DaemonSet);
        assert_eq!(
            (ds.desired, ds.ready, ds.updated, ds.available),
            (3, 2, 3, 2)
        );
        // A single object (not a List) parses too.
        let one = serde_json::to_vec(
            &serde_json::from_str::<serde_json::Value>(WORKLOADS).unwrap()["items"][0],
        )
        .unwrap();
        assert_eq!(parse_workloads(&one).unwrap().len(), 1);
        assert!(parse_workloads(b"not json").is_err());
    }

    #[test]
    fn rollout_states_from_fixtures() {
        let w = parse_workloads(WORKLOADS.as_bytes()).unwrap();
        let state = |name: &str| rollout(w.iter().find(|x| x.name == name).unwrap());
        assert_eq!(state("api-gateway").state, RolloutState::Complete);
        let web = state("web");
        assert_eq!(web.state, RolloutState::Progressing);
        assert_eq!(web.reason, "1 of 3 pods updated");
        let tw = state("temporal-worker");
        assert_eq!(tw.state, RolloutState::Failed);
        assert_eq!(tw.reason, "Progress deadline exceeded");
        assert!(tw.message.contains("timed out progressing"));
        assert_eq!(state("docs").state, RolloutState::ScaledToZero);
        assert_eq!(state("postgres").state, RolloutState::Complete);
        let ds = state("node-exporter");
        assert_eq!(ds.state, RolloutState::Stalled);
        assert_eq!(ds.reason, "2 of 3 pods available");
    }

    fn deployment() -> Workload {
        Workload {
            kind: WorkloadKind::Deployment,
            name: "d".into(),
            generation: 2,
            observed_generation: 2,
            desired: 3,
            current: 3,
            ready: 3,
            updated: 3,
            available: 3,
            ..Workload::default()
        }
    }

    fn cond(kind: &str, status: &str, reason: &str) -> Condition {
        Condition {
            kind: kind.into(),
            status: status.into(),
            reason: reason.into(),
            message: format!("{reason} message"),
            ..Condition::default()
        }
    }

    #[test]
    fn rollout_state_rules() {
        let mut d = deployment();
        assert_eq!(rollout(&d).state, RolloutState::Complete);
        d.generation = 3;
        let r = rollout(&d);
        assert_eq!(
            (r.state, r.reason.as_str()),
            (RolloutState::Progressing, "Waiting for the controller")
        );
        let mut d = deployment();
        d.current = 5;
        assert_eq!(rollout(&d).reason, "2 old pods pending termination");
        let mut d = deployment();
        d.available = 1;
        d.conditions = vec![cond("Progressing", "True", "ReplicaSetUpdated")];
        assert_eq!(rollout(&d).state, RolloutState::Progressing);
        d.conditions = vec![
            cond("Progressing", "True", "NewReplicaSetAvailable"),
            cond("Available", "False", "MinimumReplicasUnavailable"),
        ];
        let r = rollout(&d);
        assert_eq!(
            r.state,
            RolloutState::Stalled,
            "finished rollout, pods not available"
        );
        assert_eq!(r.message, "MinimumReplicasUnavailable message");
        let mut d = deployment();
        d.conditions = vec![cond("ReplicaFailure", "True", "FailedCreate")];
        let r = rollout(&d);
        assert_eq!(
            (r.state, r.reason.as_str()),
            (RolloutState::Failed, "FailedCreate")
        );
        let mut d = deployment();
        d.paused = true;
        assert_eq!(rollout(&d).state, RolloutState::Stalled);
        let mut d = deployment();
        d.desired = 0;
        assert_eq!(
            rollout(&d).state,
            RolloutState::Progressing,
            "pods still terminating"
        );
        d.current = 0;
        assert_eq!(rollout(&d).state, RolloutState::ScaledToZero);
    }

    #[test]
    fn stateful_set_rules() {
        let mut s = Workload {
            kind: WorkloadKind::StatefulSet,
            desired: 3,
            current: 3,
            ready: 3,
            updated: 1,
            available: 3,
            current_revision: "db-1".into(),
            update_revision: "db-2".into(),
            strategy: "RollingUpdate".into(),
            ..Workload::default()
        };
        assert_eq!(rollout(&s).reason, "1 of 3 pods updated");
        s.partition = Some(2);
        let r = rollout(&s);
        assert_eq!(r.state, RolloutState::Complete);
        assert!(r.reason.starts_with("Partitioned"));
        s.partition = None;
        s.strategy = "OnDelete".into();
        assert_eq!(rollout(&s).state, RolloutState::Stalled);
        s.strategy = "RollingUpdate".into();
        s.updated = 3;
        s.current_revision = "db-2".into();
        s.ready = 2;
        assert_eq!(rollout(&s).state, RolloutState::Stalled);
    }

    #[test]
    fn pods_parse_with_kubectl_status() {
        let pods = parse_pods(PODS.as_bytes()).unwrap();
        let by = |n: &str| pods.iter().find(|p| p.name == n).unwrap();
        let ok = by("api-gateway-6d4cf56db6-2xk8p");
        assert_eq!(ok.status_text(), "Running");
        assert_eq!(ok.ready(), (1, 1));
        assert_eq!(ok.node, "k3d-shop-agent-0");
        assert_eq!(
            ok.containers[0].digest(),
            Some("sha256:9f2c1d0b7a4e5f60718293a4b5c6d7e8f9011223344556677889900aabbccdd")
        );
        assert_eq!(
            ok.owner,
            Some(("ReplicaSet".into(), "api-gateway-6d4cf56db6".into()))
        );
        assert_eq!(ok.failing(), None);
        let oom = by("api-gateway-6d4cf56db6-9qtrv");
        let (c, last) = oom.last_termination().unwrap();
        assert_eq!(c, "api-gateway");
        assert_eq!(last.text(), "OOMKilled (exit 137)");
        assert_eq!(oom.restarts(), 1);
        assert_eq!(oom.failing(), None, "running again");
        let crash = by("temporal-worker-84b7c9d5f-h2j4k");
        assert_eq!(crash.status_text(), "CrashLoopBackOff");
        assert_eq!(crash.failing().as_deref(), Some("CrashLoopBackOff"));
        assert_eq!(crash.restarts(), 14);
        assert_eq!(crash.last_termination().unwrap().1.text(), "Error (exit 1)");
        assert_eq!(
            crash.last_termination().unwrap().1.message,
            "panic: TEMPORAL_HOST is not set"
        );
        let err = by("temporal-worker-84b7c9d5f-p9x7m");
        assert_eq!(err.status_text(), "Error");
        assert_eq!(err.failing().as_deref(), Some("Error"));
        let pull = by("web-7c9d8f6b5-qwert");
        assert_eq!(pull.status_text(), "ImagePullBackOff");
        assert_eq!(pull.ready(), (1, 2));
        assert_eq!(pull.failing().as_deref(), Some("ImagePullBackOff"));
        let order: Vec<&str> = pull
            .containers_in_order()
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(order, ["web", "envoy"]);
        assert_eq!(by("web-5f7b9c8d4-fghij").status_text(), "Terminating");
        assert_eq!(by("node-exporter-x7k2d").status_text(), "ContainerCreating");
        let done = by("nightly-report-29384-t6v2c");
        assert_eq!(done.status_text(), "Completed");
        assert_eq!(done.failing(), None);
        assert_eq!(pods.iter().filter(|p| p.failing().is_some()).count(), 3);
    }

    #[test]
    fn init_container_status() {
        let mut p = Pod {
            phase: "Pending".into(),
            init_count: 2,
            container_names: vec!["app".into()],
            init_containers: vec![ContainerStatus {
                name: "migrate".into(),
                image: String::new(),
                image_id: String::new(),
                ready: false,
                restarts: 3,
                state: ContainerState::Waiting {
                    reason: "CrashLoopBackOff".into(),
                    message: String::new(),
                },
                last: None,
            }],
            ..Pod::default()
        };
        assert_eq!(p.status_text(), "Init:CrashLoopBackOff");
        assert_eq!(p.failing().as_deref(), Some("CrashLoopBackOff"));
        p.init_containers[0].state = ContainerState::Running { since: None };
        assert_eq!(p.status_text(), "Init:0/2");
    }

    #[test]
    fn selectors_match_like_kubernetes() {
        let w = parse_workloads(WORKLOADS.as_bytes()).unwrap();
        let pods = parse_pods(PODS.as_bytes()).unwrap();
        let web = w.iter().find(|x| x.name == "web").unwrap();
        let of_web: Vec<&str> = pods
            .iter()
            .filter(|p| web.selector.matches(&p.labels))
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(
            of_web,
            [
                "web-5f7b9c8d4-abcde",
                "web-5f7b9c8d4-fghij",
                "web-7c9d8f6b5-qwert"
            ]
        );
        let s = Selector {
            labels: BTreeMap::new(),
            exprs: vec![
                Expr {
                    key: "legacy".into(),
                    op: "DoesNotExist".into(),
                    values: vec![],
                },
                Expr {
                    key: "env".into(),
                    op: "NotIn".into(),
                    values: vec!["prod".into()],
                },
                Expr {
                    key: "app".into(),
                    op: "Exists".into(),
                    values: vec![],
                },
            ],
        };
        assert_eq!(s.to_arg(), "!legacy,env notin (prod),app");
        let labels: BTreeMap<String, String> = [("app".to_owned(), "x".to_owned())].into();
        assert!(s.matches(&labels));
        assert!(
            !Selector::default().matches(&labels),
            "empty selects nothing"
        );
    }

    #[test]
    fn events_parse_and_sort() {
        let e = parse_events(EVENTS.as_bytes()).unwrap();
        assert_eq!(e.len(), 4);
        assert_eq!(e[0].reason, "BackOff", "newest first");
        assert_eq!(e[0].count, 61);
        assert_eq!(e[0].source, "kubelet on k3d-shop-agent-0");
        assert!(e[0].warning());
        assert_eq!(e[0].object, "Pod/temporal-worker-84b7c9d5f-h2j4k");
        // New-style event: series count and lastObservedTime.
        let series = e
            .iter()
            .find(|x| x.reason == "MinimumReplicasUnavailable")
            .unwrap();
        assert_eq!(series.count, 4);
        assert_eq!(series.last, parse_time("2026-10-26T18:55:02.000000Z"));
        assert_eq!(series.source, "deployment-controller");
        // Nulls everywhere: falls back to the creation time.
        let nulls = e
            .iter()
            .find(|x| x.message.starts_with("Scaled down"))
            .unwrap();
        assert_eq!(nulls.count, 1);
        assert_eq!(nulls.last, parse_time("2026-10-26T17:42:30Z"));
    }

    #[test]
    fn namespaces_and_config_parse() {
        let ns = parse_namespaces(NAMESPACES.as_bytes()).unwrap();
        assert_eq!(ns.len(), 4);
        assert_eq!(ns[3].name, "shop");
        assert_eq!(ns[2].phase, "Terminating");
        let names = "k3d-shop\nk3d-shopdb\nkind-dev\nkind-local\n";
        let cfg = parse_config(names, Some("k3d-shopdb\n"), Some(CONFIG.as_bytes()));
        assert_eq!(cfg.contexts.len(), 4);
        assert_eq!(cfg.current.as_deref(), Some("k3d-shopdb"));
        let shop = cfg.context("k3d-shop").unwrap();
        assert_eq!(shop.server.as_deref(), Some("https://0.0.0.0:55271"));
        assert_eq!(shop.namespace.as_deref(), Some("shop"));
        assert_eq!(shop.user, "admin@k3d-shop");
        assert_eq!(cfg.context("kind-local").unwrap().server, None);
        assert_eq!(cfg.choose(None).as_deref(), Some("k3d-shopdb"));
        assert_eq!(cfg.choose(Some("kind-dev")).as_deref(), Some("kind-dev"));
        assert_eq!(cfg.choose(Some("gone")).as_deref(), Some("k3d-shopdb"));
        assert_eq!(cfg.namespace_for("k3d-shop", None), "shop");
        assert_eq!(cfg.namespace_for("k3d-shopdb", None), "default");
        assert_eq!(
            cfg.namespace_for("k3d-shop", Some("kube-system")),
            "kube-system"
        );
        // Without `config view` the names still come through.
        let bare = parse_config(names, None, None);
        assert_eq!(bare.contexts.len(), 4);
        assert_eq!(bare.current, None);
        assert_eq!(bare.choose(None).as_deref(), Some("k3d-shop"));
    }

    #[test]
    fn rollout_history_parses() {
        let text = "deployment.apps/api-gateway \nREVISION  CHANGE-CAUSE\n5         <none>\n6         <none>\n7         kubectl set image deployment/api-gateway api-gateway=ghcr.io/acme/shop/api-gateway:sha-9a9d91b\n\n";
        let h = parse_rollout_history(text);
        assert_eq!(h.len(), 3);
        assert_eq!(h[0].number, 7);
        assert!(
            h[0].cause
                .as_deref()
                .unwrap()
                .starts_with("kubectl set image")
        );
        assert_eq!(h[2].cause, None);
        assert!(parse_rollout_history("No rollout history found.").is_empty());
    }

    #[test]
    fn images_split() {
        let i = parse_image("ghcr.io/acme/shop/api-gateway:sha-9a9d91b");
        assert_eq!(i.registry, "ghcr.io");
        assert_eq!(i.repository, "acme/shop/api-gateway");
        assert_eq!(i.tag.as_deref(), Some("sha-9a9d91b"));
        assert_eq!(i.ghcr_package(), Some(("acme", "shop/api-gateway")));
        assert_eq!(i.name(), "api-gateway");
        let p = parse_image("postgres:16.4");
        assert_eq!(
            (p.registry.as_str(), p.repository.as_str()),
            ("docker.io", "library/postgres")
        );
        let l = parse_image("localhost:5000/team/app@sha256:abcd");
        assert_eq!(l.registry, "localhost:5000");
        assert_eq!(l.tag, None);
        assert_eq!(l.digest.as_deref(), Some("sha256:abcd"));
        let both = parse_image("ghcr.io/o/n:sha-33df645@sha256:4e1d");
        assert_eq!(both.tag.as_deref(), Some("sha-33df645"));
        assert_eq!(both.digest.as_deref(), Some("sha256:4e1d"));
        assert_eq!(
            parse_image("envoyproxy/envoy").repository,
            "envoyproxy/envoy"
        );
        assert_eq!(digest_of("docker-pullable://x@sha256:1"), Some("sha256:1"));
        assert_eq!(digest_of("x:1"), None);
    }

    #[test]
    fn images_match_commits() {
        let full = Oid::from_str("9a9d91b5c9cac39b1eda670a0f07c16aec2149ba").unwrap();
        let other = Oid::from_str("33df645b28e8fc8ca22c34b073a5637665ec7da3").unwrap();
        let idx = RefIndex::new(
            [full, other],
            vec![("v0.9.0".into(), other)],
            vec![("master".into(), other)],
            vec![],
            Some(other),
            None,
        );
        let img = parse_image("ghcr.io/acme/shop/api-gateway:latest");
        // The revision label wins over a floating tag.
        let m = image_match(&idx, &img, Some(&full.to_string()), None).unwrap();
        assert_eq!(m.commit, Some(full));
        // Tag, then the version label.
        let img = parse_image("ghcr.io/o/web:sha-33df645");
        assert_eq!(
            image_match(&idx, &img, None, None).unwrap().commit,
            Some(other)
        );
        let img = parse_image("ghcr.io/o/web:latest");
        let m = image_match(&idx, &img, None, Some("0.9.0")).unwrap();
        assert_eq!(m.name, "v0.9.0");
        let m = image_match(&idx, &img, None, None).unwrap();
        assert_eq!(m.commit, None, "latest says nothing");
        assert!(image_match(&idx, &parse_image("redis:7"), None, None).is_none());
    }

    #[test]
    fn errors_are_triaged() {
        let raw = "E1026 21:05:21.449547   72660 memcache.go:265] \"Unhandled Error\" err=\"couldn't get current server API group list: Get \\\"https://0.0.0.0:55379/api?timeout=10s\\\": dial tcp 0.0.0.0:55379: connectex: No connection could be made because the target machine actively refused it.\"\nE1026 21:05:21.450072   72660 memcache.go:265] \"Unhandled Error\" err=\"same\"\nUnable to connect to the server: dial tcp 0.0.0.0:55379: connectex: No connection could be made because the target machine actively refused it.\n";
        let clean = clean_error(raw);
        assert_eq!(
            clean,
            "Unable to connect to the server: dial tcp 0.0.0.0:55379: connectex: No connection could be made because the target machine actively refused it."
        );
        assert_eq!(classify(&clean), ErrorKind::Unreachable);
        assert!(classify(&clean).is_unreachable());
        let (text, cmd) = hint(ErrorKind::Unreachable, "k3d-shopdb");
        assert!(text.contains("k3d"));
        assert_eq!(cmd.as_deref(), Some("k3d cluster start shopdb"));
        assert_eq!(
            hint(ErrorKind::Unreachable, "kind-dev").1.as_deref(),
            Some("docker start dev-control-plane")
        );
        assert_eq!(
            classify("Error in configuration: context was not found for specified context: nope"),
            ErrorKind::NoContext
        );
        assert_eq!(
            classify(
                "Error from server (Forbidden): namespaces is forbidden: User \"dev\" cannot list resource"
            ),
            ErrorKind::Forbidden
        );
        assert_eq!(
            classify("error: You must be logged in to the server (Unauthorized)"),
            ErrorKind::Unauthorized
        );
        assert_eq!(
            classify("Error from server (NotFound): deployments.apps \"x\" not found"),
            ErrorKind::NotFound
        );
        assert_eq!(
            classify("kubectl did not finish within 40 s and was stopped (timed out)."),
            ErrorKind::Timeout
        );
        // Only klog lines: keep them (deduplicated) rather than nothing.
        let only = "E1026 21:05:21.449547   72660 memcache.go:265] boom\nE1026 21:05:21.449547   72660 memcache.go:265] boom";
        assert_eq!(clean_error(only).lines().count(), 1);
    }

    #[test]
    fn namespace_load_reports_the_fatal_error() {
        let l = NamespaceLoad {
            target: t(),
            namespaces: Err("Unable to connect to the server: dial tcp".into()),
            workloads: None,
            pods: None,
        };
        assert!(l.fatal().is_some());
        let ok = NamespaceLoad {
            target: t(),
            namespaces: Err("namespaces is forbidden".into()),
            workloads: Some(Ok(vec![])),
            pods: Some(Ok(vec![])),
        };
        assert!(ok.fatal().is_none());
    }
}
