//! AI assist: commit messages from the staged diff, pull request
//! descriptions, explaining a failing CI log, summarising a pull request,
//! explaining a commit. Two ways to reach Claude:
//!
//! - the owner's own Claude Code CLI ([`cli`]), which uses their Claude
//!   subscription: `claude -p` with every tool switched off, the material
//!   on standard input;
//! - the Claude API ([`runner`]) through `curl`, the key on standard input,
//!   never on a command line.
//!
//! Both feed the same streaming [`Live`] state, and both get the same
//! redacted, budgeted material.
//!
//! - [`model`]: providers, models, effort, features, usage and cost.
//! - [`request`]: the Messages API body and the curl config (headers and
//!   the body file path on standard input; the arguments never change).
//! - [`sse`]: the event stream parser and the answer it builds; API error
//!   bodies and curl's status line.
//! - [`cli`]: finding Claude Code, its version and sign-in (`claude auth
//!   status`), the arguments and environment of a request, the stream-json
//!   parser and the runner.
//! - [`redact`]: every value the secret scanner finds becomes
//!   `[REDACTED:<rule>]`; private-key blocks and sensitive files go entirely.
//! - [`budget`]: diffs fitted file by file with a per-file cap, CI logs as
//!   error windows.
//! - [`inputs`]: the git commands that gather each request's material.
//! - [`prompts`]: one stable (cached) system prompt per feature; the
//!   material goes in the user message.
//! - [`output`]: commit message, pull request title and body, sections.
//! - [`runner`]: the key (OS keychain, else `ANTHROPIC_API_KEY`), the temp
//!   body file (always deleted) and the streamed curl run.

pub mod budget;
pub mod cli;
pub mod inputs;
pub mod model;
pub mod output;
pub mod prompts;
pub mod redact;
pub mod request;
pub mod runner;
pub mod sse;

pub use model::{
    CliModel, Effort, Feature, Model, Provider, Usage, cost, dollars, estimate_tokens,
};
pub use request::Prompt;
pub use runner::{Failure, KeySource, KeyStatus, Live, Shared, Via};
pub use sse::{Answer, ApiError, Fallback};
