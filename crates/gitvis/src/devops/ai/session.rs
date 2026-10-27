//! What the AI area knows for the whole session, whichever repository is
//! active: the key's status (never the key), where curl is, the usage and
//! estimated cost of every request, and the script-only overrides. Updated
//! from the controller on the UI thread; read by the views.

use std::path::PathBuf;
use std::sync::Mutex;

use gitvis_core::devops::ai::{Feature, KeyStatus, Model, Usage, cost};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum KeyState {
    #[default]
    Unknown,
    Checking,
    Known(KeyStatus),
}

/// Requests of one model for one feature.
#[derive(Clone, Debug)]
pub struct UsageRow {
    /// The model that answered (a fallback model after a switch).
    pub model: String,
    pub feature: Feature,
    pub requests: u32,
    pub failed: u32,
    pub usage: Usage,
    pub cost: f64,
}

#[derive(Debug, Default)]
pub struct Session {
    pub key: KeyState,
    /// `Some("Saving…")` while the keychain is written.
    pub key_busy: Option<&'static str>,
    pub key_error: Option<String>,
    /// `Some(None)`: checked, not found.
    pub curl: Option<Option<PathBuf>>,
    pub usage: Vec<UsageRow>,
    /// Set by the `ai-model` script command; never saved.
    pub model_override: Option<Model>,
    /// Set by the `ai-ask` script command; never saved.
    pub ask_override: Option<bool>,
}

static SESSION: Mutex<Session> = Mutex::new(Session {
    key: KeyState::Unknown,
    key_busy: None,
    key_error: None,
    curl: None,
    usage: Vec::new(),
    model_override: None,
    ask_override: None,
});

/// Read or change the session.
pub fn with<R>(f: impl FnOnce(&mut Session) -> R) -> R {
    let mut s = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut s)
}

/// Count one request.
pub fn record(model_id: &str, requested: Model, feature: Feature, usage: &Usage, ok: bool) {
    let priced = Model::parse(model_id).unwrap_or(requested);
    let c = cost(priced, usage);
    let model = if model_id.is_empty() {
        requested.id().to_owned()
    } else {
        model_id.to_owned()
    };
    with(|s| {
        let row = match s
            .usage
            .iter_mut()
            .find(|r| r.model == model && r.feature == feature)
        {
            Some(r) => r,
            None => {
                s.usage.push(UsageRow {
                    model: model.clone(),
                    feature,
                    requests: 0,
                    failed: 0,
                    usage: Usage::default(),
                    cost: 0.0,
                });
                s.usage.last_mut().expect("pushed")
            }
        };
        row.requests += 1;
        if !ok {
            row.failed += 1;
        }
        row.usage.add(usage);
        row.cost += c;
    });
}

/// Totals over every row: requests, failed, usage, cost.
pub fn totals() -> (u32, u32, Usage, f64) {
    with(|s| {
        let mut u = Usage::default();
        let (mut n, mut f, mut c) = (0, 0, 0.0);
        for r in &s.usage {
            n += r.requests;
            f += r.failed;
            u.add(&r.usage);
            c += r.cost;
        }
        (n, f, u, c)
    })
}
