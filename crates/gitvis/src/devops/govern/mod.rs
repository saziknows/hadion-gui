//! Security and administration: alerts, SBOM, image scans, branch
//! protection, CODEOWNERS, environments, secrets and variables.
//!
//! The page is a section navigation in three groups (security, supply
//! chain, access and rules). Each section fetches what it needs the first
//! time it is shown and caches it with a timestamp; a refresh button reads
//! again. Every change goes through a confirmation that lists the exact
//! request and its JSON body (secret values excepted: they travel on the
//! standard input of `gh secret set` and are never shown).
//!
//! - `control.rs`: `apply` / `on_msg` (reads, changes, confirmations).
//! - `alerts_view.rs`: Dependabot and code scanning.
//! - `supply_view.rs`: SBOM and container images.
//! - `access_view.rs`: branch protection and CODEOWNERS.
//! - `config_view.rs`: environments, secrets and variables.
//! - `widgets.rs`: loading / error / refusal states shared by the views.

mod access_view;
mod alerts_view;
mod config_view;
mod control;
mod supply_view;
mod widgets;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use gitvis_core::devops::gh::Request;
use gitvis_core::devops::govern::alerts::{
    CodeAlert, CodeReason, DependabotAlert, DependabotReason, Severity, Snippet, StateFilter,
};
use gitvis_core::devops::govern::codeowners::{Analysis, CodeOwners, GhError, Lookup};
use gitvis_core::devops::govern::config::{Scope, Secret, Variable};
use gitvis_core::devops::govern::docker::{ImagesReport, ScanSummary, Scanner};
use gitvis_core::devops::govern::envs::{
    BranchPolicy, EnvForm, Environment, Reviewer, ReviewerKind,
};
use gitvis_core::devops::govern::protection::{EffectiveRule, ProtectionForm, Ruleset};
use gitvis_core::devops::govern::sbom::{GhSbom, LocalSbom};
use gitvis_core::devops::govern::{Access, codeowners};
use gitvis_core::osint::Package;

use crate::devops::console::Console;
use crate::devops::ctx::Ctx;
use crate::state::{Action, Page, RepoState, Settings};
use crate::theme::Tone;
use crate::views::kit::{self, NavGroup, NavItem};

pub use control::{apply, on_console_done, on_msg};

/// The sections of the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
pub enum Tab {
    #[default]
    Dependabot,
    CodeScanning,
    Sbom,
    Images,
    Protection,
    Codeowners,
    Environments,
    Secrets,
    Variables,
}

const NAV: [(&str, &[Tab]); 3] = [
    ("Security", &[Tab::Dependabot, Tab::CodeScanning]),
    ("Supply chain", &[Tab::Sbom, Tab::Images]),
    (
        "Access and rules",
        &[
            Tab::Protection,
            Tab::Codeowners,
            Tab::Environments,
            Tab::Secrets,
            Tab::Variables,
        ],
    ),
];

impl Tab {
    pub const ALL: [Tab; 9] = [
        Tab::Dependabot,
        Tab::CodeScanning,
        Tab::Sbom,
        Tab::Images,
        Tab::Protection,
        Tab::Codeowners,
        Tab::Environments,
        Tab::Secrets,
        Tab::Variables,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Dependabot => "Dependabot alerts",
            Tab::CodeScanning => "Code scanning",
            Tab::Sbom => "SBOM",
            Tab::Images => "Container images",
            Tab::Protection => "Branch protection",
            Tab::Codeowners => "CODEOWNERS",
            Tab::Environments => "Environments",
            Tab::Secrets => "Secrets",
            Tab::Variables => "Variables",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Tab::Dependabot => {
                "Vulnerable dependencies GitHub found, with the fix and the Dependabot pull request"
            }
            Tab::CodeScanning => "CodeQL and other SARIF results, with the code they point at",
            Tab::Sbom => {
                "Packages from the lockfiles; export CycloneDX or SPDX; compare with GitHub's dependency graph"
            }
            Tab::Images => {
                "Dockerfile lint, images in compose files, image scans with Trivy, Grype or Docker Scout"
            }
            Tab::Protection => "Classic branch protection rules and the rulesets in effect",
            Tab::Codeowners => "Who owns which path, validation, coverage, and an editor",
            Tab::Environments => "Deployment environments: reviewers, wait timers, branch policies",
            Tab::Secrets => {
                "Actions secrets at repository, environment and organisation level (names only)"
            }
            Tab::Variables => {
                "Actions configuration variables at repository, environment and organisation level"
            }
        }
    }

    /// Names accepted by `gov-tab`.
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "dependabot" | "alerts" => Tab::Dependabot,
            "code-scanning" | "code" | "codeql" => Tab::CodeScanning,
            "sbom" => Tab::Sbom,
            "images" | "docker" | "containers" => Tab::Images,
            "protection" | "branches" => Tab::Protection,
            "codeowners" | "owners" => Tab::Codeowners,
            "environments" | "envs" => Tab::Environments,
            "secrets" => Tab::Secrets,
            "variables" | "vars" => Tab::Variables,
            _ => return None,
        })
    }

    /// Needs the GitHub remote (everything except the local sections).
    pub fn needs_github(self) -> bool {
        !matches!(self, Tab::Sbom | Tab::Images | Tab::Codeowners)
    }
}

/// Something read in the background: the data, the last error, whether a
/// read is in flight and when the last one finished.
pub struct Fetch<T> {
    pub data: Option<T>,
    pub error: Option<String>,
    pub loading: bool,
    pub at: Option<Instant>,
}

impl<T> Default for Fetch<T> {
    fn default() -> Self {
        Self {
            data: None,
            error: None,
            loading: false,
            at: None,
        }
    }
}

impl<T> Fetch<T> {
    /// Mark a read as started; false when one is already running.
    pub fn begin(&mut self) -> bool {
        if self.loading {
            return false;
        }
        self.loading = true;
        true
    }

    pub fn finish(&mut self, r: Result<T, String>) {
        self.loading = false;
        self.at = Some(Instant::now());
        match r {
            Ok(d) => {
                self.data = Some(d);
                self.error = None;
            }
            Err(e) => {
                self.data = None;
                self.error = Some(e);
            }
        }
    }

    /// Never read yet (and not reading).
    pub fn untouched(&self) -> bool {
        self.data.is_none() && self.error.is_none() && !self.loading
    }

    pub fn ok(&self) -> Option<&T> {
        self.data.as_ref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertKind {
    Dependabot,
    Code,
}

/// The dismiss form of an alert.
pub struct DismissForm {
    pub kind: AlertKind,
    pub number: u64,
    pub dep_reason: DependabotReason,
    pub code_reason: CodeReason,
    pub comment: String,
}

#[derive(Clone, Debug)]
pub enum ScanFailure {
    /// Docker Scout without a Docker Hub login.
    NeedsLogin(String),
    Failed(String),
}

/// An image scan started from the page.
pub struct ScanRun {
    /// Console run id.
    pub id: u64,
    pub scanner: Scanner,
    pub image: String,
    pub report: PathBuf,
    pub started: Instant,
    pub result: Option<Result<ScanSummary, ScanFailure>>,
}

/// An installed (or missing) scanner.
#[derive(Clone, Debug)]
pub struct ScannerStatus {
    pub scanner: Scanner,
    /// `None` when not installed.
    pub version: Option<String>,
    pub path: Option<String>,
    /// Why it cannot be used even though something is installed.
    pub note: Option<String>,
}

/// The CODEOWNERS file as found in the working tree.
#[derive(Clone, Debug, Default)]
pub struct CoFile {
    /// The file GitHub uses (`None` when there is none).
    pub path: Option<String>,
    /// Other CODEOWNERS files GitHub ignores because an earlier one exists.
    pub ignored: Vec<String>,
    pub text: String,
}

/// CODEOWNERS section state.
#[derive(Default)]
pub struct CoState {
    pub file: Fetch<CoFile>,
    pub tracked: Option<Arc<Vec<String>>>,
    /// The parsed saved file (or the editor text while editing).
    pub parsed: Option<CodeOwners>,
    pub analysis: Fetch<Analysis>,
    /// Generation of the text the analysis is for.
    pub analysis_gen: u64,
    pub generation: u64,
    pub errors: Fetch<Vec<GhError>>,
    pub editing: bool,
    /// Editor contents.
    pub text: String,
    /// When the editor text last changed (re-analysis is debounced).
    pub edited_at: Option<Instant>,
    pub test_path: String,
    pub lookup: Option<Lookup>,
    /// "Edit" was asked for before the file was read: open the editor
    /// when it arrives.
    pub edit_when_loaded: bool,
}

impl CoState {
    /// The text the page shows: the editor's while editing.
    pub fn current_text(&self) -> &str {
        if self.editing {
            &self.text
        } else {
            self.file.ok().map(|f| f.text.as_str()).unwrap_or("")
        }
    }

    /// The path GitHub reads (or the one a new file would get).
    pub fn path(&self) -> String {
        self.file
            .ok()
            .and_then(|f| f.path.clone())
            .unwrap_or_else(|| codeowners::LOCATIONS[0].to_owned())
    }

    /// Re-parse after the text changed.
    pub fn reparse(&mut self) {
        let text = self.current_text().to_owned();
        self.parsed = Some(codeowners::parse(&self.path(), &text));
        self.generation += 1;
    }
}

/// The create / edit form of an environment.
pub struct EnvEdit {
    pub form: EnvForm,
    /// The environment being edited (`None` when creating).
    pub editing: Option<String>,
    pub reviewer_buf: String,
    pub wait_text: String,
    pub resolving: bool,
    pub error: String,
    pub known: Vec<Reviewer>,
}

/// The set-secret form. Not `Debug`: the value must never be printed.
pub struct SecretForm {
    pub scope: Scope,
    pub name: String,
    pub value: String,
    /// Updating an existing secret.
    pub existing: bool,
    /// Just opened: the view drops any editor state left in egui memory.
    pub fresh: bool,
    /// Sent or cancelled (the value is gone): the view drops the masked
    /// input's editor state (cursor, undo history) and closes the form.
    pub closing: bool,
}

pub struct VariableForm {
    pub scope: Scope,
    /// The variable being edited (`None` when creating).
    pub original: Option<String>,
    pub name: String,
    pub value: String,
}

#[derive(Default)]
pub struct State {
    pub tab: Tab,
    pub access: Fetch<Access>,

    pub dependabot: Fetch<Vec<DependabotAlert>>,
    pub dep_filter: StateFilter,
    pub dep_selected: Option<u64>,
    pub dep_search: String,
    /// Locked packages, for the installed version in the fix guidance.
    pub dep_packages: Option<Vec<Package>>,
    pub code: Fetch<Vec<CodeAlert>>,
    pub code_filter: StateFilter,
    pub code_selected: Option<u64>,
    pub code_search: String,
    pub snippets: HashMap<u64, Option<Snippet>>,
    pub dismiss: Option<DismissForm>,

    pub sbom: Fetch<LocalSbom>,
    pub gh_sbom: Fetch<GhSbom>,
    pub sbom_search: String,
    pub sbom_eco: Option<String>,
    /// `ecosystem|name|version` of the selected package.
    pub sbom_selected: Option<String>,

    pub images: Fetch<ImagesReport>,
    /// Path of the selected Dockerfile.
    pub image_selected: Option<String>,
    pub scanners: Fetch<Vec<ScannerStatus>>,
    pub scan_image: String,
    pub scan_with: Option<Scanner>,
    pub scans: Vec<ScanRun>,

    pub protected: Fetch<Vec<String>>,
    pub branch: String,
    /// `None` inside: the branch is not protected.
    pub protection: Fetch<Option<ProtectionForm>>,
    pub prot_branch: String,
    /// The edited copy of the protection.
    pub prot_edit: Option<ProtectionForm>,
    pub context_buf: String,
    pub rules: Fetch<Vec<EffectiveRule>>,
    pub rulesets: Fetch<Vec<Ruleset>>,

    pub co: CoState,

    pub envs: Fetch<Vec<Environment>>,
    pub env_selected: Option<String>,
    pub policies: HashMap<String, Fetch<Vec<BranchPolicy>>>,
    pub env_edit: Option<EnvEdit>,
    pub policy_buf: String,
    pub policy_tag: bool,

    pub secret_scope: Scope,
    pub secrets: HashMap<Scope, Fetch<Vec<Secret>>>,
    pub secret_form: Option<SecretForm>,
    /// `secrets.NAME` references in workflows: `(name, files)`.
    pub workflow_secrets: Fetch<Vec<(String, Vec<String>)>>,

    pub var_scope: Scope,
    pub variables: HashMap<Scope, Fetch<Vec<Variable>>>,
    pub variable_form: Option<VariableForm>,
}

/// What the views and the script driver can ask for.
#[derive(Clone, Debug)]
pub enum Act {
    SetTab(Tab),
    /// Read the current section again.
    Refresh,

    SetAlertFilter(AlertKind, StateFilter),
    SelectAlert(AlertKind, Option<u64>),
    OpenDismiss(AlertKind, u64),
    CloseDismiss,
    /// Show the confirmation for the dismiss form.
    ReviewDismiss,
    ReviewReopen(AlertKind, u64),
    /// Send an alert PATCH (after the confirmation).
    PatchAlert {
        kind: AlertKind,
        label: String,
        request: Request,
    },

    LoadSbom,
    FetchGhSbom,
    /// Export through a save dialog: `cyclonedx`, `spdx` or `github`.
    Export(String),
    /// Export without a dialog (script driver).
    ExportTo(String, PathBuf),
    SelectPackage(Option<String>),

    LoadImages,
    SelectDockerfile(Option<String>),
    DetectScanners,
    SetScanner(Scanner),
    Scan(Scanner, String),

    SelectBranch(String),
    /// Start editing protection for an unprotected branch.
    StartProtecting,
    ResetProtection,
    ReviewProtection,
    SaveProtection {
        branch: String,
        request: Request,
    },
    ReviewRemoveProtection,
    RemoveProtection {
        branch: String,
        request: Request,
    },

    LoadCodeowners,
    AnalyzeCodeowners,
    TestPath(String),
    EditCodeowners(bool),
    ReviewCodeownersSave,
    SaveCodeowners {
        path: String,
        text: String,
    },

    SelectEnv(Option<String>),
    NewEnv,
    EditEnv(String),
    CancelEnv,
    ReviewEnv,
    SaveEnv {
        name: String,
        request: Request,
    },
    ReviewDeleteEnv(String),
    DeleteEnv {
        name: String,
        request: Request,
    },
    ReviewAddPolicy(String),
    ReviewDeletePolicy {
        env: String,
        id: u64,
        name: String,
    },
    PolicyChange {
        env: String,
        label: String,
        request: Request,
    },

    SetSecretScope(Scope),
    NewSecret(Scope, Option<String>),
    CancelSecret,
    ReviewSecret,
    /// Send the form's value (it is taken out of the form, never carried here).
    SetSecretConfirmed,
    ReviewDeleteSecret(Scope, String),
    DeleteSecret(Scope, String),

    SetVariableScope(Scope),
    NewVariable(Scope),
    EditVariable(Scope, String),
    CancelVariable,
    ReviewVariable,
    VariableChange {
        scope: Scope,
        label: String,
        request: Request,
    },
    ReviewDeleteVariable(Scope, String),

    /// Script driver: select the N-th item of the current list.
    ScriptItem(usize),
    /// Script driver: open the dismiss form of the selected alert.
    ScriptDismiss,
    /// Script driver: open a form prefilled with harmless example values.
    ScriptSecretForm,
    ScriptVariableForm,
    ScriptEnvForm,
    /// Script driver: append a line to the CODEOWNERS editor.
    ScriptCodeownersAppend(String),
    /// Script driver: the N-th Dockerfile.
    ScriptDockerfile(usize),
}

/// A list to read again after a change.
#[derive(Clone, Debug)]
pub enum Changed {
    Dependabot,
    Code,
    Protection(String),
    Codeowners,
    Envs,
    Policies(String),
    Secrets(Scope),
    Variables(Scope),
}

/// The CODEOWNERS file and the tracked files it is checked against.
pub type CoRead = (CoFile, Arc<Vec<String>>);

/// Replies of background work.
pub enum Msg {
    Access(Result<Box<Access>, String>),
    Dependabot(Result<Vec<DependabotAlert>, String>),
    DepPackages(Vec<Package>),
    Code(Result<Vec<CodeAlert>, String>),
    Snippet(u64, Option<Snippet>),
    Sbom(Result<Box<LocalSbom>, String>),
    GhSbom(Result<Box<GhSbom>, String>),
    Images(Result<Box<ImagesReport>, String>),
    Scanners(Vec<ScannerStatus>),
    ScanParsed(u64, Result<Box<ScanSummary>, String>),
    Protected(Result<Vec<String>, String>),
    Protection(String, Result<Option<Box<ProtectionForm>>, String>),
    Rules(String, Result<Vec<EffectiveRule>, String>),
    Rulesets(Result<Vec<Ruleset>, String>),
    CoFile(Result<Box<CoRead>, String>),
    CoAnalysis(u64, Result<Box<Analysis>, String>),
    CoErrors(Result<Vec<GhError>, String>),
    Envs(Result<Vec<Environment>, String>),
    Policies(String, Result<Vec<BranchPolicy>, String>),
    Resolved(Result<Vec<(ReviewerKind, u64, String)>, String>),
    Secrets(Scope, Result<Vec<Secret>, String>),
    WorkflowSecrets(Vec<(String, Vec<String>)>),
    Variables(Scope, Result<Vec<Variable>, String>),
    Changed(Changed),
    /// A change failed; nothing to reload, but forms stop waiting.
    ChangeFailed,
}

fn count(n: usize, one: &str, many: &str) -> String {
    crate::format::plural(n, one, many)
}

fn sev_summary(items: impl Iterator<Item = Severity>) -> String {
    let c = gitvis_core::devops::govern::alerts::severity_counts(items);
    let parts: Vec<String> = Severity::ALL
        .iter()
        .zip(c.iter())
        .filter(|(_, n)| **n > 0)
        .map(|(s, n)| format!("{n} {}", s.label().to_lowercase()))
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!(": {}", parts.join(", "))
    }
}

impl Msg {
    /// One line for the activity log (`Err` when the work failed).
    pub fn summary(&self) -> Result<String, String> {
        let err = |e: &String| Err(e.clone());
        match self {
            Msg::Access(Ok(a)) => Ok(format!(
                "{}: role {}, default branch {}, {}",
                a.full_name,
                a.role(),
                a.default_branch,
                a.visibility
            )),
            Msg::Access(Err(e)) => err(e),
            Msg::Dependabot(Ok(v)) => Ok(format!(
                "{}{}",
                count(v.len(), "Dependabot alert", "Dependabot alerts"),
                sev_summary(v.iter().map(|a| a.severity))
            )),
            Msg::Dependabot(Err(e)) => err(e),
            Msg::DepPackages(p) => Ok(count(p.len(), "locked package", "locked packages")),
            Msg::Code(Ok(v)) => Ok(format!(
                "{}{}",
                count(v.len(), "code scanning alert", "code scanning alerts"),
                sev_summary(v.iter().map(|a| a.severity()))
            )),
            Msg::Code(Err(e)) => err(e),
            Msg::Snippet(n, s) => Ok(format!(
                "Code of alert #{n}: {}",
                match s {
                    Some(s) => count(s.lines.len(), "line", "lines"),
                    None => "file not in the working tree".into(),
                }
            )),
            Msg::Sbom(Err(e)) => err(e),
            Msg::Sbom(Ok(s)) => Ok(format!(
                "{} from {} in {}",
                count(s.unique().len(), "package", "packages"),
                count(s.lockfiles.len(), "lockfile", "lockfiles"),
                crate::format::millis(s.elapsed_ms)
            )),
            Msg::GhSbom(Ok(s)) => Ok(format!(
                "GitHub SBOM: {}",
                count(s.packages.len(), "package", "packages")
            )),
            Msg::GhSbom(Err(e)) => err(e),
            Msg::Images(Err(e)) => err(e),
            Msg::Images(Ok(r)) => Ok(format!(
                "{}, {} ({} high or worse), {}",
                count(r.dockerfiles.len(), "Dockerfile", "Dockerfiles"),
                count(
                    r.finding_count(Severity::Unknown),
                    "lint finding",
                    "lint findings"
                ),
                r.finding_count(Severity::High),
                count(r.compose.len(), "compose service", "compose services")
            )),
            Msg::Scanners(v) => Ok(format!(
                "Scanners: {}",
                v.iter()
                    .map(|s| format!(
                        "{} {}",
                        s.scanner.label(),
                        s.version.as_deref().unwrap_or("missing")
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Msg::ScanParsed(_, Ok(s)) => Ok(format!(
                "{} scan of {}: {} ({} fixable)",
                s.scanner,
                s.image,
                count(s.total, "vulnerability", "vulnerabilities"),
                s.fixable
            )),
            Msg::ScanParsed(_, Err(e)) => err(e),
            Msg::Protected(Ok(v)) => Ok(count(v.len(), "protected branch", "protected branches")),
            Msg::Protected(Err(e)) => err(e),
            Msg::Protection(b, Ok(Some(_))) => Ok(format!("Protection of {b}")),
            Msg::Protection(b, Ok(None)) => Ok(format!("{b} is not protected")),
            Msg::Protection(_, Err(e)) => err(e),
            Msg::Rules(b, Ok(v)) => Ok(format!(
                "{} in effect on {b}",
                count(v.len(), "rule", "rules")
            )),
            Msg::Rules(_, Err(e)) => err(e),
            Msg::Rulesets(Ok(v)) => Ok(count(v.len(), "ruleset", "rulesets")),
            Msg::Rulesets(Err(e)) => err(e),
            Msg::CoFile(Ok(b)) => Ok(match &b.0.path {
                Some(p) => format!(
                    "{p}: {} bytes, {} tracked files",
                    b.0.text.len(),
                    crate::format::count(b.1.len())
                ),
                None => "No CODEOWNERS file".into(),
            }),
            Msg::CoFile(Err(e)) => err(e),
            Msg::CoAnalysis(_, Ok(a)) => Ok(format!(
                "CODEOWNERS coverage: {} of {} files owned, {} in {}",
                crate::format::count(a.owned),
                crate::format::count(a.files),
                count(a.issues.len(), "finding", "findings"),
                crate::format::millis(a.elapsed_ms)
            )),
            Msg::CoAnalysis(_, Err(e)) => err(e),
            Msg::CoErrors(Ok(v)) => Ok(format!(
                "GitHub reports {} in CODEOWNERS",
                count(v.len(), "error", "errors")
            )),
            Msg::CoErrors(Err(e)) => err(e),
            Msg::Envs(Ok(v)) => Ok(count(v.len(), "environment", "environments")),
            Msg::Envs(Err(e)) => err(e),
            Msg::Policies(env, Ok(v)) => Ok(format!(
                "{} for {env}",
                count(v.len(), "branch policy", "branch policies")
            )),
            Msg::Policies(_, Err(e)) => err(e),
            Msg::Resolved(Ok(v)) => Ok(format!(
                "Resolved {}",
                count(v.len(), "reviewer", "reviewers")
            )),
            Msg::Resolved(Err(e)) => err(e),
            Msg::Secrets(s, Ok(v)) => Ok(format!(
                "{} ({})",
                count(v.len(), "secret", "secrets"),
                s.label()
            )),
            Msg::Secrets(_, Err(e)) => err(e),
            Msg::WorkflowSecrets(v) => Ok(format!(
                "{} referenced in workflows",
                count(v.len(), "secret", "secrets")
            )),
            Msg::Variables(s, Ok(v)) => Ok(format!(
                "{} ({})",
                count(v.len(), "variable", "variables"),
                s.label()
            )),
            Msg::Variables(_, Err(e)) => err(e),
            Msg::Changed(_) | Msg::ChangeFailed => Ok(String::new()),
        }
    }
}

/// Persisted per repository.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub tab: Tab,
    pub dep_filter: StateFilter,
    pub code_filter: StateFilter,
}

impl State {
    pub fn prefs(&self) -> Prefs {
        Prefs {
            tab: self.tab,
            dep_filter: self.dep_filter,
            code_filter: self.code_filter,
        }
    }

    pub fn apply_prefs(&mut self, p: &Prefs) {
        self.tab = p.tab;
        self.dep_filter = p.dep_filter;
        self.code_filter = p.code_filter;
    }

    /// Open alerts at high severity or worse, from the last fetches.
    pub fn serious_alerts(&self) -> usize {
        let dep = self
            .dependabot
            .ok()
            .map(|v| {
                v.iter()
                    .filter(|a| a.is_open() && a.severity >= Severity::High)
                    .count()
            })
            .unwrap_or(0);
        let code = self
            .code
            .ok()
            .map(|v| {
                v.iter()
                    .filter(|a| a.is_open() && a.severity() >= Severity::High)
                    .count()
            })
            .unwrap_or(0);
        dep + code
    }

    /// CODEOWNERS errors: local syntax errors or GitHub's, whichever is more.
    pub fn codeowners_errors(&self) -> usize {
        let local = self.co.parsed.as_ref().map(|p| p.errors()).unwrap_or(0);
        let gh = self.co.errors.ok().map(|e| e.len()).unwrap_or(0);
        local.max(gh)
    }
}

/// The page was opened (fetch what it needs the first time).
pub fn on_page_shown(cx: &mut Ctx, repo: &mut RepoState) {
    control::page_shown(cx, repo);
}

/// `GITVIS_SCRIPT` commands this area understands.
pub fn script(cmd: &str, arg: &str) -> Option<Vec<Action>> {
    let page = Action::ShowPage(Page::Govern);
    let a = |act: Act| -> Action { act.into() };
    let arg = arg.trim();
    Some(match cmd {
        "govern" => vec![page],
        "gov-tab" => vec![a(Act::SetTab(Tab::from_name(arg)?)), page],
        "gov-item" => vec![a(Act::ScriptItem(arg.parse().ok()?))],
        "gov-refresh" => vec![a(Act::Refresh)],
        "gov-filter" => {
            let f = match arg {
                "open" => StateFilter::Open,
                "dismissed" => StateFilter::Dismissed,
                "fixed" => StateFilter::Fixed,
                "all" => StateFilter::All,
                _ => return None,
            };
            vec![
                a(Act::SetAlertFilter(AlertKind::Dependabot, f)),
                a(Act::SetAlertFilter(AlertKind::Code, f)),
            ]
        }
        "protection" => vec![
            page,
            a(Act::SetTab(Tab::Protection)),
            a(Act::SelectBranch(arg.to_owned())),
        ],
        "protection-edit" => vec![a(Act::StartProtecting)],
        "protection-review" => vec![a(Act::ReviewProtection)],
        "codeowners-test" => vec![
            page,
            a(Act::SetTab(Tab::Codeowners)),
            a(Act::TestPath(arg.to_owned())),
        ],
        "codeowners-edit" => vec![
            page,
            a(Act::SetTab(Tab::Codeowners)),
            a(Act::EditCodeowners(true)),
        ],
        "codeowners-review" => vec![a(Act::ReviewCodeownersSave)],
        "secret-form" => vec![page, a(Act::SetTab(Tab::Secrets)), a(Act::ScriptSecretForm)],
        "secret-review" => vec![a(Act::ReviewSecret)],
        "variable-form" => vec![
            page,
            a(Act::SetTab(Tab::Variables)),
            a(Act::ScriptVariableForm),
        ],
        "variable-review" => vec![a(Act::ReviewVariable)],
        "env-new" => vec![a(Act::SetTab(Tab::Environments)), page, a(Act::NewEnv)],
        "env-form" => vec![
            a(Act::SetTab(Tab::Environments)),
            page,
            a(Act::ScriptEnvForm),
        ],
        "codeowners-append" => vec![a(Act::ScriptCodeownersAppend(arg.to_owned()))],
        "env-review" => vec![a(Act::ReviewEnv)],
        "dismiss-form" => vec![a(Act::ScriptDismiss)],
        "dismiss-review" => vec![a(Act::ReviewDismiss)],
        "dockerfile" => vec![
            page,
            a(Act::SetTab(Tab::Images)),
            a(Act::ScriptDockerfile(arg.parse().ok()?)),
        ],
        "gov-scan" => {
            let (scanner, image) = arg.split_once(' ')?;
            let s = match scanner {
                "trivy" => Scanner::Trivy,
                "grype" => Scanner::Grype,
                "scout" => Scanner::Scout,
                _ => return None,
            };
            vec![
                page,
                a(Act::SetTab(Tab::Images)),
                a(Act::Scan(s, image.trim().to_owned())),
            ]
        }
        "gov-scanner" => {
            let s = match arg {
                "trivy" => Scanner::Trivy,
                "grype" => Scanner::Grype,
                "scout" => Scanner::Scout,
                _ => return None,
            };
            vec![a(Act::SetTab(Tab::Images)), page, a(Act::SetScanner(s))]
        }
        "gh-sbom" => vec![a(Act::FetchGhSbom)],
        "sbom-save" => {
            let (format, path) = arg.split_once(' ')?;
            vec![a(Act::ExportTo(
                format.to_owned(),
                PathBuf::from(path.trim()),
            ))]
        }
        _ => return None,
    })
}

pub fn show(
    ui: &mut egui::Ui,
    repo: &mut RepoState,
    console: &Console,
    _settings: &Settings,
    actions: &mut Vec<Action>,
) {
    let tab = repo.devops.govern.tab;
    let order: Vec<Tab> = NAV.iter().flat_map(|(_, t)| t.iter().copied()).collect();
    let mut clicked: Option<Tab> = None;
    let counts: Vec<(Tab, Option<(usize, Tone)>)> = Tab::ALL
        .iter()
        .map(|t| (*t, nav_count(&repo.devops.govern, *t)))
        .collect();
    kit::page_with_nav(
        ui,
        "govern_nav",
        |ui| {
            let groups: Vec<NavGroup> = NAV
                .iter()
                .map(|(title, tabs)| NavGroup {
                    title,
                    items: tabs
                        .iter()
                        .map(|t| {
                            let item = NavItem::new(t.label()).hint(t.hint());
                            match counts.iter().find(|(k, _)| k == t).and_then(|(_, c)| *c) {
                                Some((n, tone)) => item.count(n, tone),
                                None => item,
                            }
                        })
                        .collect(),
                })
                .collect();
            let selected = order.iter().position(|t| *t == tab).unwrap_or(0);
            if let Some(i) = kit::section_nav(ui, &groups, selected) {
                clicked = order.get(i).copied();
            }
        },
        |ui| content(ui, repo, console, actions),
    );
    if let Some(t) = clicked {
        actions.push(Act::SetTab(t).into());
    }
}

/// A count on a navigation item: problems only.
fn nav_count(st: &State, t: Tab) -> Option<(usize, Tone)> {
    match t {
        Tab::Dependabot => {
            let v = st.dependabot.ok()?;
            let open: Vec<&DependabotAlert> = v.iter().filter(|a| a.is_open()).collect();
            let tone = if open.iter().any(|a| a.severity >= Severity::High) {
                Tone::Danger
            } else {
                Tone::Warning
            };
            Some((open.len(), tone))
        }
        Tab::CodeScanning => {
            let v = st.code.ok()?;
            let open: Vec<&CodeAlert> = v.iter().filter(|a| a.is_open()).collect();
            let tone = if open.iter().any(|a| a.severity() >= Severity::High) {
                Tone::Danger
            } else {
                Tone::Warning
            };
            Some((open.len(), tone))
        }
        Tab::Images => {
            let r = st.images.ok()?;
            let high = r.finding_count(Severity::High);
            let medium = r.finding_count(Severity::Medium);
            Some((
                medium,
                if high > 0 {
                    Tone::Danger
                } else {
                    Tone::Warning
                },
            ))
        }
        Tab::Codeowners => Some((st.codeowners_errors(), Tone::Danger)),
        _ => None,
    }
}

fn content(ui: &mut egui::Ui, repo: &mut RepoState, console: &Console, actions: &mut Vec<Action>) {
    let tab = repo.devops.govern.tab;
    if tab.needs_github() && repo.github_remote.is_none() {
        kit::page_header(ui, tab.label(), tab.hint(), |_| {});
        kit::scroll_page(ui, "gov_no_remote", |ui| {
            widgets::no_github(ui, repo, tab);
        });
        return;
    }
    match tab {
        Tab::Dependabot => alerts_view::dependabot(ui, repo, actions),
        Tab::CodeScanning => alerts_view::code(ui, repo, actions),
        Tab::Sbom => supply_view::sbom(ui, repo, actions),
        Tab::Images => supply_view::images(ui, repo, console, actions),
        Tab::Protection => access_view::protection(ui, repo, actions),
        Tab::Codeowners => access_view::codeowners(ui, repo, actions),
        Tab::Environments => config_view::environments(ui, repo, actions),
        Tab::Secrets => config_view::secrets(ui, repo, actions),
        Tab::Variables => config_view::variables(ui, repo, actions),
    }
}

/// A dot on the rail entry and a line for its tooltip, when this area
/// needs attention (e.g. pending approvals, critical alerts, a running
/// task). `None` when nothing does.
pub fn rail_badge(repo: &RepoState, _console: &Console) -> Option<(Tone, String)> {
    let st = &repo.devops.govern;
    let serious = st.serious_alerts();
    if serious > 0 {
        return Some((
            Tone::Danger,
            format!(
                "{} critical/high {}",
                serious,
                if serious == 1 { "alert" } else { "alerts" }
            ),
        ));
    }
    let co = st.codeowners_errors();
    if co > 0 {
        return Some((
            Tone::Warning,
            format!(
                "CODEOWNERS has {co} {}",
                if co == 1 { "error" } else { "errors" }
            ),
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_round_trip_names() {
        for t in Tab::ALL {
            assert!(!t.label().is_empty());
            assert!(!t.hint().is_empty());
        }
        assert_eq!(Tab::from_name("code-scanning"), Some(Tab::CodeScanning));
        assert_eq!(Tab::from_name("Variables"), Some(Tab::Variables));
        assert_eq!(Tab::from_name("nope"), None);
        let order: Vec<Tab> = NAV.iter().flat_map(|(_, t)| t.iter().copied()).collect();
        assert_eq!(order.len(), Tab::ALL.len());
    }

    #[test]
    fn script_commands() {
        assert_eq!(script("govern", "").unwrap().len(), 1);
        assert_eq!(script("gov-tab", "secrets").unwrap().len(), 2);
        assert!(script("gov-tab", "bogus").is_none());
        assert_eq!(script("gov-item", "3").unwrap().len(), 1);
        assert_eq!(script("protection", "main").unwrap().len(), 3);
        assert_eq!(script("codeowners-test", "src/a.rs").unwrap().len(), 3);
        assert_eq!(script("secret-form", "").unwrap().len(), 3);
        assert_eq!(script("variable-form", "").unwrap().len(), 3);
        assert_eq!(script("dockerfile", "0").unwrap().len(), 3);
        assert!(script("gov-scan", "scout alpine:3.20").is_some());
        assert!(script("gov-scan", "nmap x").is_none());
        assert!(script("unknown-thing", "").is_none());
    }

    #[test]
    fn messages_stay_small() {
        // Routed through `DevOpsMsg` with every other area's messages.
        assert!(
            std::mem::size_of::<Msg>() <= 96,
            "{}",
            std::mem::size_of::<Msg>()
        );
    }

    #[test]
    fn fetch_lifecycle() {
        let mut f: Fetch<u32> = Fetch::default();
        assert!(f.untouched());
        assert!(f.begin());
        assert!(!f.begin());
        f.finish(Ok(3));
        assert_eq!(f.ok(), Some(&3));
        assert!(f.begin());
        f.finish(Err("x".into()));
        assert!(f.ok().is_none());
        assert_eq!(f.error.as_deref(), Some("x"));
    }
}
