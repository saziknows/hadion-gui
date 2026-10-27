//! Terminal sessions: a pseudo-terminal (`portable-pty`; ConPTY on Windows
//! asks for the cursor position with `ESC[6n` and waits for the answer
//! before it sends anything else) with a VT100 screen (`vt100`), shells
//! found on this machine, keyboard input encoding.
//!
//! - [`session`]: [`Session`] starts a shell in a pseudo-terminal of a
//!   given size and directory (`TERM=xterm-256color`,
//!   `COLORTERM=truecolor`), reads its output on a thread into a shared
//!   `vt100::Parser`, answers terminal queries, writes input, resizes,
//!   kills, and records the exit code.
//! - [`stream`]: the output filter (queries, `ESC[3J`, dim text).
//! - [`screen`]: styled runs of the visible rows, ANSI palettes, selections
//!   in scrollback coordinates and their text.
//! - [`keys`]: what keys, pastes and mouse events send (pure functions).
//! - [`shells`]: shell discovery and the `cd` command of each shell.
//!
//! No UI here; the panel lives in the app's `devops::terminal`.

pub mod keys;
pub mod screen;
pub mod session;
pub mod shells;
pub mod stream;

pub use session::{Session, SpawnOptions, Status, Term};
pub use shells::{Discovery, Shell, ShellKind};
pub use stream::Colors;
