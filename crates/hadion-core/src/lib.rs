//! Repository model, graph layout and git operations for Hadion.
//!
//! Reads go through libgit2 (fast, in-process); writes go through the git
//! executable so behaviour matches the user's command line exactly.

pub mod cli;
pub mod devops;
pub mod diff;
pub mod github;
pub mod graph;
pub mod insights;
pub mod joblog;
pub mod model;
pub mod ops;
pub mod osint;
pub mod reflog;
pub mod repo;
pub mod secrets;

mod diff_source;

pub use diff_source::{DiffOptions, DiffTarget, diff_args, load_diff};
pub use model::*;

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<git2::Error> for Error {
    fn from(e: git2::Error) -> Self {
        Self(e.message().to_owned())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
