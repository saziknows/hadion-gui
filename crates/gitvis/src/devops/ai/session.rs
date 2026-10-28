//! What the AI area knows for the whole session, whichever repository is
//! active: Claude Code's status (where it is, its version, how it is signed
//! in), the API key's status (never the key), where curl is, the usage of
//! every request (and the estimated cost of API requests), the plan's usage
//! as Claude Code last reported it, and the script-only overrides. Updated
//! from the controller on the UI thread; read by the views. Lock briefly,
//! never inside a background task.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use gitvis_core::devops::ai::cli::{self, PlanUsage};
use gitvis_core::devops::ai::{CliModel, Feature, KeyStatus, Model, Provider, Usage, cost};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum KeyState {
    #[default]
    Unknown,
    Checking,
    Known(KeyStatus),
}

/// What gitvis knows about Claude Code.
#[derive(Clone, Debug, Default)]
pub enum CliState {
    #[default]
    Unknown,
    Checking,
    Known(Box<cli::Status>),
}

impl CliState {
    pub fn status(&self) -> Option<&cli::Status> {
        match self {
            CliState::Known(s) => Some(s),
            _ => None,
        }
    }
}

/// Requests of one model for one feature, through one route.
#[derive(Clone, Debug)]
pub struct UsageRow {
    /// The model that answered (a fallback model after a switch).
    pub model: String,
    pub feature: Feature,
    /// Through Claude Code (the plan), not the API.
    pub cli: bool,
    pub requests: u32,
    pub failed: u32,
    pub usage: Usage,
    /// Estimated API cost (0 for Claude Code).
    pub cost: f64,
    /// Claude Code's own client-side estimate (covered by the plan).
    pub cli_estimate: f64,
}

#[derive(Debug, Default)]
pub struct Session {
    pub key: KeyState,
    /// `Some("Saving…")` while the keychain is written.
    pub key_busy: Option<&'static str>,
    pub key_error: Option<String>,
    /// `Some(None)`: checked, not found.
    pub curl: Option<Option<PathBuf>>,
    pub cli: CliState,
    /// The plan's usage from the last Claude Code answer, and when.
    pub plan: Option<(PlanUsage, Instant)>,
    pub usage: Vec<UsageRow>,
    /// Set by script commands; never saved.
    pub provider_override: Option<Provider>,
    pub model_override: Option<Model>,
    pub cli_model_override: Option<CliModel>,
    pub ask_override: Option<bool>,
    /// Scroll the Settings section to a part (`usage`, `cli`, `api`) once.
    pub scroll_to: Option<String>,
}

static SESSION: Mutex<Session> = Mutex::new(Session {
    key: KeyState::Unknown,
    key_busy: None,
    key_error: None,
    curl: None,
    cli: CliState::Unknown,
    plan: None,
    usage: Vec::new(),
    provider_override: None,
    model_override: None,
    cli_model_override: None,
    ask_override: None,
    scroll_to: None,
});

/// Read or change the session.
pub fn with<R>(f: impl FnOnce(&mut Session) -> R) -> R {
    let mut s = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut s)
}

/// One finished (or failed) request.
pub struct Record<'a> {
    /// The model that answered (empty when none did).
    pub model_id: &'a str,
    /// The API model asked for (prices an unknown model).
    pub requested: Model,
    pub feature: Feature,
    pub usage: &'a Usage,
    pub ok: bool,
    /// Through Claude Code, with its own estimate.
    pub cli: Option<f64>,
}

/// Count one request.
pub fn record(r: Record) {
    let via_cli = r.cli.is_some();
    let c = if via_cli {
        0.0
    } else {
        cost(Model::parse(r.model_id).unwrap_or(r.requested), r.usage)
    };
    let model = if !r.model_id.is_empty() {
        r.model_id.to_owned()
    } else if via_cli {
        "(no answer)".to_owned()
    } else {
        r.requested.id().to_owned()
    };
    with(|s| {
        let row = match s
            .usage
            .iter_mut()
            .find(|x| x.model == model && x.feature == r.feature && x.cli == via_cli)
        {
            Some(x) => x,
            None => {
                s.usage.push(UsageRow {
                    model: model.clone(),
                    feature: r.feature,
                    cli: via_cli,
                    requests: 0,
                    failed: 0,
                    usage: Usage::default(),
                    cost: 0.0,
                    cli_estimate: 0.0,
                });
                s.usage.last_mut().expect("pushed")
            }
        };
        row.requests += 1;
        if !r.ok {
            row.failed += 1;
        }
        row.usage.add(r.usage);
        row.cost += c;
        row.cli_estimate += r.cli.unwrap_or(0.0);
    });
}

/// Totals over every row.
#[derive(Clone, Copy, Debug, Default)]
pub struct Totals {
    pub requests: u32,
    pub failed: u32,
    pub usage: Usage,
    /// Estimated API cost.
    pub cost: f64,
    /// Requests through Claude Code, and its own estimate for them.
    pub cli_requests: u32,
    pub cli_estimate: f64,
}

pub fn totals() -> Totals {
    with(|s| {
        let mut t = Totals::default();
        for r in &s.usage {
            t.requests += r.requests;
            t.failed += r.failed;
            t.usage.add(&r.usage);
            t.cost += r.cost;
            if r.cli {
                t.cli_requests += r.requests;
                t.cli_estimate += r.cli_estimate;
            }
        }
        t
    })
}
