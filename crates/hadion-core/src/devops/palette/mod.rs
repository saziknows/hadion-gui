//! The command palette's search engine (no UI).
//!
//! - [`fuzzy`]: case-insensitive subsequence matching with scoring (word
//!   starts, camelCase, runs, prefix) and the matched character indices.
//! - [`frecency`]: recently and frequently used items, with decay.
//! - [`scope`]: the query prefixes (`>` commands, `@` branches, `#` pull
//!   requests, `/` files, `:` commits, `!` runs, `~` repositories, `?`).
//! - [`item`]: what a row shows and stands for ([`item::Target`]).
//! - [`sources`]: builders from plain data (refs, files, commits, …).
//! - [`rank`]: one search over many sources with frecency, narrowing the
//!   previous matches while the query only grows.
//! - [`files`]: `git ls-files` for the file scope.

pub mod files;
pub mod frecency;
pub mod fuzzy;
pub mod item;
pub mod rank;
pub mod scope;
pub mod sources;

pub use frecency::Use;
pub use fuzzy::{Haystack, Matcher, Query};
pub use item::{Item, Kind, Target};
pub use rank::{Hit, Ranked, Ranker};
pub use scope::Scope;
