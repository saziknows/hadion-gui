//! AI assist, through the owner's Claude Code CLI (their subscription) or
//! the Claude API (a key): "Write the message" in the WIP panel, "Write the
//! description" in the new pull request form, "Explain this failure" in the
//! Actions log viewer, "Summarise" on a pull request, "Explain this commit"
//! in the commit panel and menu, and the AI section of the Settings page.
//! Per-repository state (requests in flight, results); configuration in
//! `Settings::ext.ai`; Claude Code's status, the key's status and the
//! session's usage in [`session`] (app-wide).
//!
//! A request goes through four phases: choosing the way to Claude
//! ([`Route`]) and gathering and redacting the material in a background
//! read ([`Phase::Preparing`]), the confirmation that shows what is sent
//! and how ([`Phase::Confirming`], skipped when "Ask before sending" is
//! off), the streamed run ([`Phase::Running`], the text shows as it
//! arrives) and the result ([`Phase::Done`]).

mod control;
pub mod session;
mod settings;
mod view;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use hadion_core::Commit;
use hadion_core::devops::ai::cli::{self, Install};
use hadion_core::devops::ai::redact::Redactions;
use hadion_core::devops::ai::runner::{Done, KeySource};
use hadion_core::devops::ai::{
    CliModel, Effort, Failure, Feature, KeyStatus, Model, Prompt, Provider, Shared,
};
use serde::{Deserialize, Serialize};

use crate::devops::console::ConsoleRun;
use crate::devops::ctx::Ctx;
use crate::devops::hooks::HookCx;
use crate::state::{Action, Page, RepoState, Settings};

/// What a request is about (one request per target at a time).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// The commit message of the working tree.
    Wip,
    /// The new pull request form.
    NewPr,
    /// A pull request's summary.
    Pr(u64),
    /// A failed Actions job.
    Job { run: u64, job: u64 },
    /// One commit (full hash).
    Commit(String),
}

impl Target {
    pub fn feature(&self) -> Feature {
        match self {
            Target::Wip => Feature::CommitMessage,
            Target::NewPr => Feature::PrDescription,
            Target::Pr(_) => Feature::SummarisePr,
            Target::Job { .. } => Feature::ExplainFailure,
            Target::Commit(_) => Feature::ExplainCommit,
        }
    }
}

/// The way a request reaches Claude.
#[derive(Clone, Debug)]
pub enum Route {
    /// The Claude API, with the key from here.
    Api(KeySource),
    /// The owner's Claude Code.
    ClaudeCode(Box<CliRoute>),
}

/// The Claude Code a request runs.
#[derive(Clone, Debug)]
pub struct CliRoute {
    pub install: Install,
    pub version: Option<cli::Version>,
    /// `Claude Max plan`, `API key`.
    pub account: String,
    /// Signed in with an API key: requests bill the API, not the plan.
    pub bills_api: bool,
    /// `your Claude Max plan`, `an API key`, for sentences.
    pub phrase: String,
}

impl Route {
    pub fn is_cli(&self) -> bool {
        matches!(self, Route::ClaudeCode(_))
    }
}

/// What was gathered for a request, after redaction and fitting.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub prompt: Prompt,
    /// `(what, detail)` lines for the confirmation: "Staged diff" → "12 of
    /// 48 files, 30 KB of 210 KB; …".
    pub sources: Vec<(String, String)>,
    /// Files (or log ranges) included.
    pub files: Vec<String>,
    pub redactions: Redactions,
    pub route: Route,
}

/// An answer, ready to show.
#[derive(Clone, Debug)]
pub struct Reply {
    pub done: Box<Done>,
    /// Estimated API cost; 0 through Claude Code (the plan covers it, see
    /// `done.cli` for Claude Code's own estimate).
    pub cost: f64,
}

pub enum Phase {
    Preparing,
    Confirming,
    Running {
        live: Shared,
        cancel: Arc<AtomicBool>,
    },
    Done(Result<Reply, Failure>),
}

/// One request and what became of it.
pub struct Ask {
    pub id: u64,
    pub target: Target,
    pub provider: Provider,
    /// API model and effort.
    pub model: Model,
    pub effort: Effort,
    /// Claude Code model and effort.
    pub cli_model: CliModel,
    pub cli_effort: Effort,
    pub ask_first: bool,
    pub phase: Phase,
    pub prepared: Option<Prepared>,
    pub started: Instant,
    /// The answer was filled in (commit message, pull request form).
    pub applied: bool,
    /// The answer waits for "Replace your message?".
    pub unapplied: bool,
    /// The route picked before the material was read (`Some(true)` Claude
    /// Code), kept when reading the material fails.
    pub chosen_cli: Option<bool>,
}

impl Ask {
    pub fn feature(&self) -> Feature {
        self.target.feature()
    }

    pub fn running(&self) -> bool {
        matches!(self.phase, Phase::Preparing | Phase::Running { .. })
    }

    pub fn reply(&self) -> Option<&Reply> {
        match &self.phase {
            Phase::Done(Ok(r)) => Some(r),
            _ => None,
        }
    }

    /// Through Claude Code: known once the material is ready (a reply
    /// tells, too); before that, from the provider asked for.
    pub fn via_cli(&self) -> bool {
        if let Some(r) = self.reply() {
            return !r.done.via.is_api();
        }
        match &self.prepared {
            Some(p) => p.route.is_cli(),
            None => self
                .chosen_cli
                .unwrap_or(self.provider == Provider::ClaudeCode),
        }
    }

    /// Settled once the route is chosen.
    pub fn route_known(&self) -> bool {
        self.prepared.is_some()
            || self.reply().is_some()
            || self.chosen_cli.is_some()
            || self.provider != Provider::Auto
    }

    /// `Claude Code · Opus`, `Opus 5.5`, `Auto`.
    pub fn engine(&self) -> String {
        if !self.route_known() {
            return "Auto".to_owned();
        }
        if self.via_cli() {
            match self.cli_model {
                CliModel::Default => "Claude Code".to_owned(),
                m => format!("Claude Code · {}", m.label()),
            }
        } else {
            self.model.short().to_owned()
        }
    }
}

#[derive(Default)]
pub struct State {
    pub asks: HashMap<Target, Ask>,
    pub next_id: u64,
    /// The request waiting at its confirmation (for `ai-send`).
    pub last_pending: Option<Target>,
    /// Result cards folded away.
    pub collapsed: HashSet<Target>,
    /// Jobs seen in the log viewer: did they fail? (`None`: unknown.)
    pub job_failed: HashMap<u64, Option<bool>>,
}

/// A secret value that never shows in `Debug` output (actions are
/// cloneable and debuggable).
#[derive(Clone)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(•••)")
    }
}

#[derive(Clone, Debug)]
pub enum Act {
    /// Gather the material for a target (then confirm or send).
    Start(Target),
    /// The confirmation was accepted.
    Send {
        target: Target,
        id: u64,
    },
    /// `ai-send`: send the request waiting at its confirmation.
    SendPending,
    /// Show the confirmation again (it was closed without an answer).
    Reconfirm(Target),
    /// Stop a running request, or drop one waiting for confirmation.
    Cancel(Target),
    /// Forget a result.
    Dismiss(Target),
    /// Fill in an answer that waited for "Replace your message?".
    Use {
        target: Target,
        id: u64,
    },
    ToggleCollapsed(Target),
    /// The log viewer shows this job: learn whether it failed.
    SeenJob {
        run: u64,
        job: u64,
    },
    /// The job shown in the log viewer (`ai-explain` without arguments).
    ExplainShownJob,
    /// The selected pull request (`ai-summary` without a number).
    SummariseSelected,
    /// `ai-commit-explain head`.
    ExplainHead,
    CheckKey,
    SaveKey(Secret),
    RemoveKey,
    RemoveKeyConfirmed,
    /// Find Claude Code and read how it is signed in (again).
    CheckCli,
    /// Script-only session overrides (`ai-model`, `ai-ask`, `ai-provider`,
    /// `ai-cli-model`).
    SetModelOverride(Option<Model>),
    SetAskOverride(Option<bool>),
    SetProviderOverride(Option<Provider>),
    SetCliModelOverride(Option<CliModel>),
}

/// Which keychain operation a [`Msg::Key`] answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyOp {
    Check,
    Save,
    Remove,
}

pub enum Msg {
    Prepared {
        target: Target,
        id: u64,
        /// The route chosen (`Some(true)` Claude Code), `None` when none
        /// could be.
        chosen: Option<bool>,
        result: Result<Prepared, Failure>,
        /// Claude Code as the route choice found it.
        cli: Option<Box<cli::Status>>,
    },
    Finished {
        target: Target,
        id: u64,
        feature: Feature,
        result: Result<Box<Done>, Failure>,
    },
    Key {
        op: KeyOp,
        result: Result<KeyStatus, String>,
        curl: Option<std::path::PathBuf>,
    },
    Cli(Box<cli::Status>),
}

impl Msg {
    /// One line for the activity log (`Err` when the work failed).
    pub fn summary(&self) -> Result<String, String> {
        match self {
            Msg::Prepared { result, .. } => match result {
                Ok(p) => {
                    let chars = p.prompt.chars();
                    let mut s = format!(
                        "{} characters, about {} tokens, {}",
                        format_count(chars),
                        format_count(hadion_core::devops::ai::estimate_tokens(chars) as usize),
                        match &p.route {
                            Route::Api(_) => "through the Claude API".to_owned(),
                            Route::ClaudeCode(c) => format!("through Claude Code ({})", c.account),
                        }
                    );
                    if !p.redactions.is_empty() {
                        s.push_str(&format!("; redacted {}", p.redactions.summary()));
                    }
                    for (what, detail) in &p.sources {
                        s.push_str(&format!("\n{what}: {detail}"));
                    }
                    Ok(s)
                }
                Err(f) => Err(f.message()),
            },
            Msg::Finished {
                result, feature, ..
            } => match result {
                Ok(d) => {
                    let a = &d.answer;
                    let cost = match &d.cli {
                        Some(run) if run.bills_api => format!(
                            "billed to the API account Claude Code is signed in with (Claude Code estimates {})",
                            run.cost_estimate
                                .map(hadion_core::devops::ai::dollars)
                                .unwrap_or_else(|| "nothing".to_owned())
                        ),
                        Some(run) => format!(
                            "covered by the plan (Claude Code estimates {})",
                            run.cost_estimate
                                .map(hadion_core::devops::ai::dollars)
                                .unwrap_or_else(|| "nothing".to_owned())
                        ),
                        None => hadion_core::devops::ai::dollars(
                            Model::parse(&a.model)
                                .map(|m| hadion_core::devops::ai::cost(m, &a.usage))
                                .unwrap_or(0.0),
                        ),
                    };
                    let mut s = format!(
                        "{} via {}: {} · {} in · {} out · cache write {} / read {} · {} · {}",
                        feature.label(),
                        if d.via.is_api() {
                            "the Claude API"
                        } else {
                            "Claude Code"
                        },
                        a.model,
                        format_count(a.usage.input_tokens as usize),
                        format_count(a.usage.output_tokens as usize),
                        format_count(a.usage.cache_creation_input_tokens as usize),
                        format_count(a.usage.cache_read_input_tokens as usize),
                        cost,
                        a.stop_reason.as_deref().unwrap_or("no stop reason"),
                    );
                    for f in &a.fallbacks {
                        s.push_str(&format!("\n{} declined; continued on {}", f.from, f.to));
                    }
                    if a.refused() {
                        Err(format!(
                            "{s}\nClaude declined ({})",
                            a.refusal_category.as_deref().unwrap_or("no category")
                        ))
                    } else {
                        Ok(s)
                    }
                }
                Err(f) => Err(f.message()),
            },
            Msg::Key { op, result, .. } => match result {
                Ok(k) => Ok(match (op, k.source()) {
                    (KeyOp::Remove, _) => "Removed the stored key".to_owned(),
                    (KeyOp::Save, _) => "Stored the key in the keychain".to_owned(),
                    (KeyOp::Check, Some(s)) => format!("Key found in {}", s.label()),
                    (KeyOp::Check, None) => "No key stored or in the environment".to_owned(),
                }),
                Err(e) => Err(e.clone()),
            },
            Msg::Cli(st) => Ok(st.summary()),
        }
    }
}

fn format_count(n: usize) -> String {
    crate::format::count(n)
}

/// Size budgets offered in the settings, in KB.
pub const BUDGETS_KB: [u32; 5] = [16, 32, 64, 128, 256];

/// App-wide configuration (`Settings::ext.ai`): the provider, the model and
/// effort of each provider, what is sent. The API key lives only in the OS
/// keychain; Claude Code keeps its own login.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Claude Code, the API, or Auto (Claude Code when it is signed in
    /// with a plan, else the API key).
    pub provider: Provider,
    /// API model and effort per feature.
    pub model: Model,
    pub effort_commit: Effort,
    pub effort_pr: Effort,
    pub effort_ci: Effort,
    pub effort_summary: Effort,
    pub effort_commit_explain: Effort,
    /// Claude Code model and effort per feature.
    pub cli_model: CliModel,
    pub cli_effort_commit: Effort,
    pub cli_effort_pr: Effort,
    pub cli_effort_ci: Effort,
    pub cli_effort_summary: Effort,
    pub cli_effort_commit_explain: Effort,
    /// Show what is sent and ask before every request.
    pub ask: bool,
    /// Most of a diff sent, in KB.
    pub diff_kb: u32,
    /// Most of a CI log sent, in KB.
    pub log_kb: u32,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            provider: Provider::Auto,
            model: Model::default(),
            effort_commit: Feature::CommitMessage.default_effort(),
            effort_pr: Feature::PrDescription.default_effort(),
            effort_ci: Feature::ExplainFailure.default_effort(),
            effort_summary: Feature::SummarisePr.default_effort(),
            effort_commit_explain: Feature::ExplainCommit.default_effort(),
            cli_model: CliModel::Default,
            cli_effort_commit: Feature::CommitMessage.default_effort(),
            cli_effort_pr: Feature::PrDescription.default_effort(),
            cli_effort_ci: Feature::ExplainFailure.default_effort(),
            cli_effort_summary: Feature::SummarisePr.default_effort(),
            cli_effort_commit_explain: Feature::ExplainCommit.default_effort(),
            ask: true,
            diff_kb: 64,
            log_kb: 48,
        }
    }
}

impl Prefs {
    /// The API effort of a feature.
    pub fn effort(&self, f: Feature) -> Effort {
        match f {
            Feature::CommitMessage => self.effort_commit,
            Feature::PrDescription => self.effort_pr,
            Feature::ExplainFailure => self.effort_ci,
            Feature::SummarisePr => self.effort_summary,
            Feature::ExplainCommit => self.effort_commit_explain,
        }
    }

    pub fn effort_mut(&mut self, f: Feature) -> &mut Effort {
        match f {
            Feature::CommitMessage => &mut self.effort_commit,
            Feature::PrDescription => &mut self.effort_pr,
            Feature::ExplainFailure => &mut self.effort_ci,
            Feature::SummarisePr => &mut self.effort_summary,
            Feature::ExplainCommit => &mut self.effort_commit_explain,
        }
    }

    /// The Claude Code effort of a feature.
    pub fn cli_effort(&self, f: Feature) -> Effort {
        match f {
            Feature::CommitMessage => self.cli_effort_commit,
            Feature::PrDescription => self.cli_effort_pr,
            Feature::ExplainFailure => self.cli_effort_ci,
            Feature::SummarisePr => self.cli_effort_summary,
            Feature::ExplainCommit => self.cli_effort_commit_explain,
        }
    }

    pub fn cli_effort_mut(&mut self, f: Feature) -> &mut Effort {
        match f {
            Feature::CommitMessage => &mut self.cli_effort_commit,
            Feature::PrDescription => &mut self.cli_effort_pr,
            Feature::ExplainFailure => &mut self.cli_effort_ci,
            Feature::SummarisePr => &mut self.cli_effort_summary,
            Feature::ExplainCommit => &mut self.cli_effort_commit_explain,
        }
    }

    /// The provider in use (a script override wins).
    pub fn provider_now(&self) -> Provider {
        session::with(|s| s.provider_override).unwrap_or(self.provider)
    }

    /// The API model in use (a script override wins).
    pub fn model_now(&self) -> Model {
        session::with(|s| s.model_override).unwrap_or(self.model)
    }

    /// The Claude Code model in use (a script override wins).
    pub fn cli_model_now(&self) -> CliModel {
        session::with(|s| s.cli_model_override).unwrap_or(self.cli_model)
    }

    pub fn ask_now(&self) -> bool {
        session::with(|s| s.ask_override).unwrap_or(self.ask)
    }

    pub fn diff_budget(&self) -> usize {
        self.diff_kb.clamp(4, 1024) as usize * 1024
    }

    pub fn log_budget(&self) -> usize {
        self.log_kb.clamp(4, 1024) as usize * 1024
    }
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    control::apply(cx, repo, act);
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    control::on_msg(cx, repo, msg);
}

/// A console run of this repository ended (requests do not use the
/// console: their text streams into the area's own state).
pub fn on_console_done(_cx: &mut Ctx, _repo: &mut RepoState, _run: &ConsoleRun) {}

/// `HADION_SCRIPT` commands this area understands.
///
/// - `ai-commit`: write the commit message (stops at the confirmation when
///   "Ask before sending" is on); `ai-send` sends the request waiting
///   there (the dialog stays on screen: scripts cannot close it).
/// - `ai-pr` (new pull request form), `ai-explain [RUN JOB]` (the job in
///   the log viewer without ids), `ai-summary [N]` (the selected pull
///   request without a number), `ai-commit-explain SHA|head`.
/// - `ai-provider auto|cli|api`, `ai-model NAME` (API), `ai-cli-model
///   default|opus|sonnet|haiku|fable`, `ai-ask on|off`: session-only
///   overrides for tests.
/// - `ai-stop`, `ai-dismiss`, `ai-settings [usage|cli|api]`,
///   `ai-check-key`, `ai-cli-check`.
pub fn script(cmd: &str, arg: &str) -> Option<Vec<Action>> {
    let arg = arg.trim();
    let mut nums = arg.split_whitespace().map(|s| s.parse::<u64>().ok());
    Some(match cmd {
        "ai-commit" => vec![Act::Start(Target::Wip).into()],
        "ai-send" => vec![Act::SendPending.into()],
        "ai-pr" => vec![Act::Start(Target::NewPr).into()],
        "ai-explain" => match (nums.next().flatten(), nums.next().flatten()) {
            (Some(run), Some(job)) => vec![Act::Start(Target::Job { run, job }).into()],
            _ => vec![Act::ExplainShownJob.into()],
        },
        "ai-summary" => match nums.next().flatten() {
            Some(n) => vec![Act::Start(Target::Pr(n)).into()],
            None => vec![Act::SummariseSelected.into()],
        },
        "ai-commit-explain" => {
            if arg.is_empty() || arg.eq_ignore_ascii_case("head") {
                vec![Act::ExplainHead.into()]
            } else {
                vec![Act::Start(Target::Commit(arg.to_owned())).into()]
            }
        }
        "ai-model" => vec![Act::SetModelOverride(Some(Model::parse(arg)?)).into()],
        "ai-provider" => vec![Act::SetProviderOverride(Some(Provider::parse(arg)?)).into()],
        "ai-cli-model" => vec![Act::SetCliModelOverride(Some(CliModel::parse(arg)?)).into()],
        "ai-cli-check" => vec![Act::CheckCli.into()],
        "ai-ask" => vec![
            Act::SetAskOverride(match arg {
                "on" => Some(true),
                "off" => Some(false),
                _ => None,
            })
            .into(),
        ],
        "ai-stop" => vec![
            Act::Cancel(Target::Wip).into(),
            Act::Cancel(Target::NewPr).into(),
        ],
        "ai-dismiss" => vec![Act::Dismiss(Target::Wip).into()],
        "ai-check-key" => vec![Act::CheckKey.into()],
        "ai-settings" => {
            if !arg.is_empty() {
                session::with(|s| s.scroll_to = Some((arg.to_owned(), Instant::now())));
            }
            vec![
                Action::DevOps(crate::devops::DevOpsAction::SetSettingsSection(
                    crate::devops::settings_page::Section::Ai,
                )),
                Action::ShowPage(Page::Settings),
            ]
        }
        _ => return None,
    })
}

/// The AI section of the Settings page.
pub fn settings_section(
    ui: &mut egui::Ui,
    settings: &mut Settings,
    repo: Option<&RepoState>,
    actions: &mut Vec<Action>,
) {
    settings::show(ui, settings, repo, actions);
}

pub fn hook_commit_menu(ui: &mut egui::Ui, h: &HookCx, c: &Commit, actions: &mut Vec<Action>) {
    view::commit_menu(ui, h, c, actions);
}

pub fn hook_commit_panel(ui: &mut egui::Ui, h: &HookCx, c: &Commit, actions: &mut Vec<Action>) {
    view::commit_panel(ui, h, c, actions);
}

pub fn hook_wip_message_tools(ui: &mut egui::Ui, h: &HookCx, actions: &mut Vec<Action>) {
    view::wip_tools(ui, h, actions);
}

pub fn hook_log_viewer_tools(
    ui: &mut egui::Ui,
    h: &crate::devops::hooks::LogHookCx,
    run: u64,
    job: u64,
    actions: &mut Vec<Action>,
) {
    view::log_tools(ui, h, run, job, actions);
}

pub fn hook_pr_tools(ui: &mut egui::Ui, h: &HookCx, number: u64, actions: &mut Vec<Action>) {
    view::pr_tools(ui, h, number, actions);
}

pub fn hook_new_pr_tools(ui: &mut egui::Ui, h: &HookCx, actions: &mut Vec<Action>) {
    view::new_pr_tools(ui, h, actions);
}

/// Keychain status for the views (checked on first need).
pub(crate) fn key_status() -> session::KeyState {
    session::with(|s| s.key.clone())
}

/// The check the Settings section asks for when it first shows.
pub(crate) fn key_unknown() -> bool {
    matches!(key_status(), session::KeyState::Unknown)
}

/// Claude Code's status has not been read yet.
pub(crate) fn cli_unknown() -> bool {
    session::with(|s| matches!(s.cli, session::CliState::Unknown))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefs_defaults_and_old_settings() {
        let p = Prefs::default();
        assert_eq!(p.model, Model::Opus55);
        assert!(p.ask);
        assert_eq!(p.effort(Feature::CommitMessage), Effort::Low);
        assert_eq!(p.effort(Feature::PrDescription), Effort::Medium);
        assert_eq!(p.effort(Feature::ExplainFailure), Effort::Medium);
        assert_eq!(p.effort(Feature::SummarisePr), Effort::High);
        assert_eq!(p.provider, Provider::Auto);
        assert_eq!(p.cli_model, CliModel::Default);
        assert_eq!(p.cli_effort(Feature::SummarisePr), Effort::High);
        // Settings saved before this area existed, or by a newer version.
        let p: Prefs = serde_json::from_str("{}").unwrap();
        assert!(p.ask);
        assert_eq!(p.diff_kb, 64);
        let p: Prefs = serde_json::from_str(r#"{"model":"claude-x","ask":false}"#).unwrap();
        assert_eq!(p.model, Model::Opus55);
        assert!(!p.ask);
        // Nothing secret is serialised.
        let s = serde_json::to_string(&Prefs::default()).unwrap();
        assert!(!s.contains("key"), "{s}");
    }

    #[test]
    fn secrets_do_not_debug_print() {
        let a = Act::SaveKey(Secret("sk-ant-api03-real".into()));
        let d = format!("{a:?}");
        assert!(!d.contains("sk-ant"), "{d}");
    }

    #[test]
    fn script_commands() {
        assert!(script("ai-commit", "").is_some());
        assert!(script("ai-model", "sonnet").is_some());
        assert!(script("ai-model", "gpt").is_none());
        assert!(script("ai-provider", "cli").is_some());
        assert!(script("ai-provider", "pigeon").is_none());
        assert!(script("ai-cli-model", "haiku").is_some());
        assert!(script("ai-cli-check", "").is_some());
        assert!(script("ai-explain", "12 34").is_some());
        assert!(script("ai-summary", "").is_some());
        assert!(script("ai-commit-explain", "head").is_some());
        assert!(script("nope", "").is_none());
    }
}
