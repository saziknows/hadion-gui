//! Models, effort levels, features, token usage and what it costs.

use serde::{Deserialize, Serialize};

/// A Claude model gitvis can ask. Stored in the settings by its API id; an
/// unknown id (a model that was renamed or removed) reads as the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Model {
    #[default]
    Opus55,
    Sonnet55,
    Haiku55,
    Fable51,
}

/// Prices in US dollars per million tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
}

/// Cache writes (5-minute lifetime) cost 1.25 times the input price.
pub const CACHE_WRITE_FACTOR: f64 = 1.25;

/// Claude Haiku 5.5 charges more for prompts above this many tokens.
pub const HAIKU_LONG_PROMPT: u64 = 100_000;

impl Model {
    pub const ALL: [Model; 4] = [
        Model::Opus55,
        Model::Sonnet55,
        Model::Haiku55,
        Model::Fable51,
    ];

    /// The id the API takes (no date suffix).
    pub fn id(self) -> &'static str {
        match self {
            Model::Opus55 => "claude-opus-5-5",
            Model::Sonnet55 => "claude-sonnet-5-5",
            Model::Haiku55 => "claude-haiku-5-5",
            Model::Fable51 => "claude-fable-5-1",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Model::Opus55 => "Claude Opus 5.5",
            Model::Sonnet55 => "Claude Sonnet 5.5",
            Model::Haiku55 => "Claude Haiku 5.5",
            Model::Fable51 => "Claude Fable 5.1",
        }
    }

    /// `Opus 5.5`: the label without "Claude", for tight places.
    pub fn short(self) -> &'static str {
        self.label().trim_start_matches("Claude ")
    }

    /// One line for the model picker.
    pub fn note(self) -> &'static str {
        match self {
            Model::Opus55 => "The default: strong on code, $4 / $20 per million tokens",
            Model::Sonnet55 => "Faster and half the price of Opus: $2 / $10",
            Model::Haiku55 => {
                "Cheapest and quickest: $0.10 / $0.50 up to 100K-token prompts, $0.50 / $2.50 beyond; no refusal fallback"
            }
            Model::Fable51 => "Most capable and slowest: $10 / $50",
        }
    }

    /// From an API id, a label or a short name (`opus`, `sonnet 5.5`).
    pub fn parse(s: &str) -> Option<Model> {
        let s = s.trim().to_ascii_lowercase();
        if s.is_empty() {
            return None;
        }
        Model::ALL.into_iter().find(|m| {
            m.id() == s
                || m.label().to_ascii_lowercase() == s
                || m.short().to_ascii_lowercase() == s
                || m.short().to_ascii_lowercase().split(' ').next() == Some(s.as_str())
        })
    }

    /// The label of an API id. Ids gitvis does not list (a fallback model)
    /// are spelled out: `claude-opus-4-8` → `Claude Opus 4.8`.
    pub fn label_of(id: &str) -> String {
        if let Some(m) = Model::ALL.into_iter().find(|m| m.id() == id) {
            return m.label().to_owned();
        }
        let Some(rest) = id.strip_prefix("claude-") else {
            return id.to_owned();
        };
        let parts: Vec<&str> = rest.split('-').collect();
        let split = parts
            .iter()
            .position(|p| p.chars().all(|c| c.is_ascii_digit()))
            .unwrap_or(parts.len());
        let (words, numbers) = parts.split_at(split);
        if words.is_empty() || numbers.iter().any(|n| n.len() > 2) {
            return id.to_owned(); // dated snapshot ids stay as they are
        }
        let mut s = String::from("Claude");
        for w in words {
            let mut c = w.chars();
            if let Some(f) = c.next() {
                s.push(' ');
                s.push(f.to_ascii_uppercase());
                s.push_str(c.as_str());
            }
        }
        if !numbers.is_empty() {
            s.push(' ');
            s.push_str(&numbers.join("."));
        }
        s
    }

    /// Prices for a request whose prompt (input plus cache reads and
    /// writes) has `prompt_tokens` tokens.
    pub fn price(self, prompt_tokens: u64) -> Price {
        match self {
            Model::Opus55 => Price {
                input: 4.0,
                output: 20.0,
                cache_read: 0.20,
            },
            Model::Sonnet55 => Price {
                input: 2.0,
                output: 10.0,
                cache_read: 0.20,
            },
            Model::Haiku55 if prompt_tokens > HAIKU_LONG_PROMPT => Price {
                input: 0.50,
                output: 2.50,
                cache_read: 0.05,
            },
            Model::Haiku55 => Price {
                input: 0.10,
                output: 0.50,
                cache_read: 0.01,
            },
            Model::Fable51 => Price {
                input: 10.0,
                output: 50.0,
                cache_read: 0.25,
            },
        }
    }

    /// Server-side refusal fallbacks exist for every model but Haiku.
    pub fn supports_fallbacks(self) -> bool {
        self != Model::Haiku55
    }
}

impl From<String> for Model {
    fn from(s: String) -> Self {
        Model::parse(&s).unwrap_or_default()
    }
}

impl From<Model> for String {
    fn from(m: Model) -> Self {
        m.id().to_owned()
    }
}

/// How hard the model thinks (`output_config.effort` for the API,
/// `--effort` for Claude Code).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Effort {
    Low,
    #[default]
    Medium,
    High,
    /// Claude Code only (`--effort xhigh`).
    XHigh,
    /// Claude Code only (`--effort max`).
    Max,
}

impl Effort {
    /// The levels offered for API requests.
    pub const ALL: [Effort; 3] = [Effort::Low, Effort::Medium, Effort::High];
    /// The levels Claude Code's `--effort` takes.
    pub const CLI: [Effort; 5] = [
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::XHigh,
        Effort::Max,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Effort::Low => "Low",
            Effort::Medium => "Medium",
            Effort::High => "High",
            Effort::XHigh => "Extra high",
            Effort::Max => "Max",
        }
    }

    /// The short label for segmented controls (`X-high`).
    pub fn short(self) -> &'static str {
        match self {
            Effort::XHigh => "X-high",
            e => e.label(),
        }
    }

    /// The level sent to the API, which gitvis offers up to `high`.
    pub fn for_api(self) -> Effort {
        match self {
            Effort::XHigh | Effort::Max => Effort::High,
            e => e,
        }
    }

    pub fn parse(s: &str) -> Option<Effort> {
        Effort::CLI
            .into_iter()
            .find(|e| e.as_str() == s.trim().to_ascii_lowercase())
    }
}

impl From<String> for Effort {
    fn from(s: String) -> Self {
        Effort::parse(&s).unwrap_or_default()
    }
}

impl From<Effort> for String {
    fn from(e: Effort) -> Self {
        e.as_str().to_owned()
    }
}

/// How gitvis reaches Claude.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Provider {
    /// Claude Code when it is signed in with a Claude plan, otherwise the
    /// API key.
    #[default]
    Auto,
    /// The owner's Claude Code CLI (their subscription).
    ClaudeCode,
    /// The Claude API with an API key (billed per token).
    Api,
}

impl Provider {
    pub const ALL: [Provider; 3] = [Provider::Auto, Provider::ClaudeCode, Provider::Api];

    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Auto => "auto",
            Provider::ClaudeCode => "claude-code",
            Provider::Api => "api",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Provider::Auto => "Auto",
            Provider::ClaudeCode => "Claude Code",
            Provider::Api => "Claude API",
        }
    }

    /// `auto`, `claude-code` / `cli` / `code`, `api`.
    pub fn parse(s: &str) -> Option<Provider> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Provider::Auto),
            "claude-code" | "claude code" | "cli" | "code" | "claudecode" => {
                Some(Provider::ClaudeCode)
            }
            "api" | "claude-api" | "key" => Some(Provider::Api),
            _ => None,
        }
    }
}

impl From<String> for Provider {
    fn from(s: String) -> Self {
        Provider::parse(&s).unwrap_or_default()
    }
}

impl From<Provider> for String {
    fn from(p: Provider) -> Self {
        p.as_str().to_owned()
    }
}

/// The model Claude Code is asked for (`--model`). Names, not versions:
/// Claude Code maps each to the model it currently means.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum CliModel {
    /// No `--model`: whatever the owner's Claude Code uses.
    #[default]
    Default,
    Opus,
    Sonnet,
    Haiku,
    Fable,
}

impl CliModel {
    pub const ALL: [CliModel; 5] = [
        CliModel::Default,
        CliModel::Opus,
        CliModel::Sonnet,
        CliModel::Haiku,
        CliModel::Fable,
    ];

    /// The `--model` value, `None` for Claude Code's default.
    pub fn arg(self) -> Option<&'static str> {
        match self {
            CliModel::Default => None,
            CliModel::Opus => Some("opus"),
            CliModel::Sonnet => Some("sonnet"),
            CliModel::Haiku => Some("haiku"),
            CliModel::Fable => Some("fable"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CliModel::Default => "Default",
            CliModel::Opus => "Opus",
            CliModel::Sonnet => "Sonnet",
            CliModel::Haiku => "Haiku",
            CliModel::Fable => "Fable",
        }
    }

    fn as_str(self) -> &'static str {
        self.arg().unwrap_or("default")
    }

    pub fn parse(s: &str) -> Option<CliModel> {
        let s = s.trim().to_ascii_lowercase();
        CliModel::ALL.into_iter().find(|m| m.as_str() == s)
    }
}

impl From<String> for CliModel {
    fn from(s: String) -> Self {
        CliModel::parse(&s).unwrap_or_default()
    }
}

impl From<CliModel> for String {
    fn from(m: CliModel) -> Self {
        m.as_str().to_owned()
    }
}

/// What gitvis asks Claude for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Feature {
    CommitMessage,
    PrDescription,
    ExplainFailure,
    SummarisePr,
    ExplainCommit,
}

impl Feature {
    pub const ALL: [Feature; 5] = [
        Feature::CommitMessage,
        Feature::PrDescription,
        Feature::ExplainFailure,
        Feature::SummarisePr,
        Feature::ExplainCommit,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Feature::CommitMessage => "Commit message",
            Feature::PrDescription => "Pull request description",
            Feature::ExplainFailure => "CI failure explanation",
            Feature::SummarisePr => "Pull request summary",
            Feature::ExplainCommit => "Commit explanation",
        }
    }

    /// A short label for tables: `PR description`.
    pub fn short(self) -> &'static str {
        match self {
            Feature::CommitMessage => "Commit message",
            Feature::PrDescription => "PR description",
            Feature::ExplainFailure => "CI failure",
            Feature::SummarisePr => "PR summary",
            Feature::ExplainCommit => "Commit explanation",
        }
    }

    /// The button / log label: "Write the commit message".
    pub fn action(self) -> &'static str {
        match self {
            Feature::CommitMessage => "Write the commit message",
            Feature::PrDescription => "Write the pull request description",
            Feature::ExplainFailure => "Explain this failure",
            Feature::SummarisePr => "Summarise this pull request",
            Feature::ExplainCommit => "Explain this commit",
        }
    }

    pub fn default_effort(self) -> Effort {
        match self {
            Feature::CommitMessage | Feature::ExplainCommit => Effort::Low,
            Feature::PrDescription | Feature::ExplainFailure => Effort::Medium,
            Feature::SummarisePr => Effort::High,
        }
    }
}

/// Token counts of one answer (or a sum of answers).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

impl Usage {
    /// Input plus cache writes and reads: the prompt's size.
    pub fn prompt_tokens(&self) -> u64 {
        self.input_tokens + self.cache_creation_input_tokens + self.cache_read_input_tokens
    }

    pub fn add(&mut self, o: &Usage) {
        self.input_tokens += o.input_tokens;
        self.output_tokens += o.output_tokens;
        self.cache_creation_input_tokens += o.cache_creation_input_tokens;
        self.cache_read_input_tokens += o.cache_read_input_tokens;
    }

    pub fn is_empty(&self) -> bool {
        *self == Usage::default()
    }
}

/// Estimated cost in US dollars of `usage` on `model` (list prices; cache
/// writes at [`CACHE_WRITE_FACTOR`] times the input price).
pub fn cost(model: Model, usage: &Usage) -> f64 {
    let p = model.price(usage.prompt_tokens());
    (usage.input_tokens as f64 * p.input
        + usage.cache_creation_input_tokens as f64 * p.input * CACHE_WRITE_FACTOR
        + usage.cache_read_input_tokens as f64 * p.cache_read
        + usage.output_tokens as f64 * p.output)
        / 1_000_000.0
}

/// Rough token count of `chars` characters of code and prose (about 3.5
/// characters per token), for the "about N tokens" shown before sending.
pub fn estimate_tokens(chars: usize) -> u64 {
    (chars as f64 / 3.5).ceil() as u64
}

/// `$0.0123`, `$1.24`: four decimals under a dollar, two above.
pub fn dollars(v: f64) -> String {
    if v == 0.0 {
        "$0".to_owned()
    } else if v < 0.0001 {
        "< $0.0001".to_owned()
    } else if v < 1.0 {
        format!("${v:.4}")
    } else {
        format!("${v:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_no_date_suffix_and_parse_back() {
        for m in Model::ALL {
            assert!(m.id().starts_with("claude-"));
            // No `-20261001`-style snapshot suffix.
            assert!(
                !m.id()
                    .split('-')
                    .any(|p| p.len() == 8 && p.chars().all(|c| c.is_ascii_digit()))
            );
            assert_eq!(Model::parse(m.id()), Some(m));
            assert_eq!(Model::parse(m.label()), Some(m));
        }
        assert_eq!(Model::parse("opus"), Some(Model::Opus55));
        assert_eq!(Model::parse("Sonnet"), Some(Model::Sonnet55));
        assert_eq!(Model::parse("haiku 5.5"), Some(Model::Haiku55));
        assert_eq!(Model::parse("fable"), Some(Model::Fable51));
        assert_eq!(Model::parse("gpt"), None);
        assert_eq!(Model::default().id(), "claude-opus-5-5");
        assert_eq!(Model::label_of("claude-opus-5-5"), "Claude Opus 5.5");
        assert_eq!(Model::label_of("claude-opus-5"), "Claude Opus 5");
        assert_eq!(Model::label_of("claude-opus-4-8"), "Claude Opus 4.8");
        assert_eq!(Model::label_of("claude-x-20250101"), "claude-x-20250101");
        assert_eq!(Model::label_of("gpt"), "gpt");
    }

    #[test]
    fn unknown_ids_read_as_the_default() {
        #[derive(Deserialize)]
        struct P {
            m: Model,
            e: Effort,
        }
        let p: P = serde_json::from_str(r#"{"m":"claude-opus-9","e":"extreme"}"#).unwrap();
        assert_eq!(p.m, Model::Opus55);
        assert_eq!(p.e, Effort::Medium);
        let p: P = serde_json::from_str(r#"{"m":"claude-haiku-5-5","e":"high"}"#).unwrap();
        assert_eq!(p.m, Model::Haiku55);
        assert_eq!(p.e, Effort::High);
        assert_eq!(
            serde_json::to_string(&Model::Sonnet55).unwrap(),
            r#""claude-sonnet-5-5""#
        );
    }

    #[test]
    fn providers_and_cli_models() {
        assert_eq!(Provider::default(), Provider::Auto);
        for p in Provider::ALL {
            assert_eq!(Provider::parse(p.as_str()), Some(p));
            let json = serde_json::to_string(&p).unwrap();
            assert_eq!(serde_json::from_str::<Provider>(&json).unwrap(), p);
        }
        assert_eq!(Provider::parse("cli"), Some(Provider::ClaudeCode));
        assert_eq!(
            serde_json::from_str::<Provider>("\"carrier-pigeon\"").unwrap(),
            Provider::Auto
        );
        assert_eq!(CliModel::Default.arg(), None);
        assert_eq!(CliModel::Opus.arg(), Some("opus"));
        for m in CliModel::ALL {
            let json = serde_json::to_string(&m).unwrap();
            assert_eq!(serde_json::from_str::<CliModel>(&json).unwrap(), m);
        }
        assert_eq!(
            serde_json::from_str::<CliModel>("\"gpt\"").unwrap(),
            CliModel::Default
        );
        assert_eq!(Effort::parse("xhigh"), Some(Effort::XHigh));
        assert_eq!(Effort::Max.for_api(), Effort::High);
        assert_eq!(Effort::Low.for_api(), Effort::Low);
        assert_eq!(Effort::ALL.len(), 3);
        assert_eq!(Effort::CLI.len(), 5);
    }

    #[test]
    fn feature_efforts() {
        assert_eq!(Feature::CommitMessage.default_effort(), Effort::Low);
        assert_eq!(Feature::PrDescription.default_effort(), Effort::Medium);
        assert_eq!(Feature::ExplainFailure.default_effort(), Effort::Medium);
        assert_eq!(Feature::SummarisePr.default_effort(), Effort::High);
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn cost_math() {
        let u = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        };
        // Haiku: 2M-token prompt is beyond 100K, so the long-prompt prices.
        assert!(close(cost(Model::Opus55, &u), 24.0));
        assert!(close(cost(Model::Sonnet55, &u), 12.0));
        assert!(close(cost(Model::Fable51, &u), 60.0));
        assert!(close(cost(Model::Haiku55, &u), 3.0));
        let small = Usage {
            input_tokens: 10_000,
            output_tokens: 2_000,
            cache_creation_input_tokens: 4_000,
            cache_read_input_tokens: 20_000,
        };
        // Opus: 10k × 4 + 4k × 5 + 20k × 0.2 + 2k × 20 = 40k + 20k + 4k + 40k = 104k µ$.
        assert!(close(cost(Model::Opus55, &small), 0.104));
        // Haiku under 100K: 10k × 0.1 + 4k × 0.125 + 20k × 0.01 + 2k × 0.5.
        assert!(close(cost(Model::Haiku55, &small), 0.0027));
        // Exactly at the threshold is still the short-prompt price.
        let edge = Usage {
            input_tokens: HAIKU_LONG_PROMPT,
            ..Usage::default()
        };
        assert!(close(cost(Model::Haiku55, &edge), 0.01));
        let over = Usage {
            input_tokens: HAIKU_LONG_PROMPT + 1,
            ..Usage::default()
        };
        assert!(close(cost(Model::Haiku55, &over), 0.0500005));
        assert_eq!(cost(Model::Opus55, &Usage::default()), 0.0);
    }

    #[test]
    fn usage_sums() {
        let mut a = Usage {
            input_tokens: 1,
            output_tokens: 2,
            cache_creation_input_tokens: 3,
            cache_read_input_tokens: 4,
        };
        a.add(&a.clone());
        assert_eq!(a.prompt_tokens(), 16);
        assert_eq!(a.output_tokens, 4);
        assert!(!a.is_empty());
    }

    #[test]
    fn money_and_token_estimates() {
        assert_eq!(dollars(0.0), "$0");
        assert_eq!(dollars(0.00001), "< $0.0001");
        assert_eq!(dollars(0.0123), "$0.0123");
        assert_eq!(dollars(1.239), "$1.24");
        assert_eq!(estimate_tokens(0), 0);
        assert_eq!(estimate_tokens(7), 2);
        assert_eq!(estimate_tokens(3500), 1000);
    }

    #[test]
    fn fallbacks_except_haiku() {
        assert!(Model::Opus55.supports_fallbacks());
        assert!(Model::Fable51.supports_fallbacks());
        assert!(!Model::Haiku55.supports_fallbacks());
    }
}
