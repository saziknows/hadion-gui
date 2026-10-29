//! The merge conflict editor: base / ours / theirs side by side with the
//! result, per-hunk accept (ours, theirs, both, base), manual editing, save
//! and mark resolved. Opens from a conflicted file in the WIP panel and
//! replaces the graph on the History page while it is open.
//!
//! - [`session`]: one open file (load, resolutions, unsaved edits, the
//!   aligned rows the panes draw).
//! - [`view`]: header, file switcher, toolbar and the states (loading,
//!   error, resolved, all resolved); [`panes`] the aligned code panes;
//!   [`cards`] the choice card for binary and delete / modify conflicts.
//!
//! Reads (`git status`, the operation files, the stage blobs, `git
//! merge-file`) run in one background task after the editor opens and
//! after every repository load. Saving writes the file through
//! `Ctx::local_change`; marking resolved (`git add`), whole-file choices,
//! recreating the markers and continuing the operation are confirmed first
//! with their exact commands. Unsaved edits live in [`State::sessions`],
//! so they survive reloads and a trip to the raw diff; closing the editor
//! with unsaved edits asks first.

mod cards;
mod panes;
pub mod session;
mod view;

use std::collections::HashMap;

use hadion_core::devops::conflict::actions::{self, Choice};
use hadion_core::devops::conflict::labels::SideLabel;
use hadion_core::devops::conflict::load::{self, FileLoad};
use hadion_core::devops::conflict::resolve::{self, Resolution};
use hadion_core::devops::conflict::{self as core, Overview};
use hadion_core::devops::workbench::git_line;
use hadion_core::ops::GitOp;
use hadion_core::{DiffTarget, Pending};
use serde::{Deserialize, Serialize};

use crate::devops::console::{Console, ConsoleRun};
use crate::devops::ctx::Ctx;
use crate::devops::hooks::{HookCx, WipList};
use crate::state::{Action, Page, RepoState, Settings, ToastKind};

pub use session::Session;

#[derive(Default)]
pub struct State {
    /// The conflicted file being edited.
    pub open: Option<String>,
    /// Conflicted files and the operation, from the last read.
    pub overview: Option<Result<Overview, String>>,
    /// A read is running; `reread` asks for another when it ends.
    pub reading: bool,
    reread: bool,
    /// Replies of reads older than this are dropped (a save or a change
    /// happened since they started).
    read_gen: u64,
    /// Open files, by path. Unsaved edits live here.
    pub sessions: HashMap<String, Session>,
    /// Why a file could not be read, by path.
    pub errors: HashMap<String, String>,
    /// Every file seen conflicted since the editor opened, in order (the
    /// switcher keeps resolved ones, marked so).
    pub seen: Vec<String>,
    /// Conflicts in each file when it was first read (progress).
    pub first_counts: HashMap<String, usize>,
    /// Show the common ancestor as a fourth pane.
    pub show_base: bool,
    /// Whole-file text edit of the open file.
    pub whole: Option<String>,
    /// A change of this editor is running (its label).
    pub busy: Option<String>,
    /// The last change that failed.
    pub last_error: Option<String>,
    /// A note for the header (edits dropped because a file was resolved
    /// elsewhere).
    pub notice: Option<String>,
    /// Alt+↓ / Alt+↑ held last frame (the graph consumes the key events,
    /// so presses are found from the key state).
    pub keys: (bool, bool),
}

#[derive(Clone, Debug)]
pub enum Act {
    /// Open the editor for a conflicted file.
    Open(String),
    /// Close (asks first when edits are unsaved).
    Close,
    CloseConfirmed,
    /// Read again (retry).
    Refresh,
    /// Resolve hunk `k` of the open file.
    Resolve(usize, Resolution),
    /// Every hunk of the open file the same way.
    ResolveAll(Resolution),
    Focus(usize),
    Next,
    Prev,
    /// The next conflicted file.
    NextFile,
    SetBase(bool),
    /// Edit hunk `k` as text; apply or cancel.
    EditHunk(usize),
    /// Replace the hunk editor's text (scripts).
    EditText(String),
    EditApply,
    EditCancel,
    /// Edit the whole result as text.
    WholeEdit,
    WholeApply,
    WholeCancel,
    Save,
    /// Ask, then `git add` (saving first when edits are unsaved).
    MarkResolved,
    MarkResolvedConfirmed(String),
    /// Mark the open file resolved without asking (scripts, scratch
    /// repositories only).
    MarkResolvedNow,
    /// A whole-file choice (binary, delete / modify): ask, then run.
    Choose(Choice),
    ChooseConfirmed(String, Choice),
    ChooseNow(Choice),
    /// `git checkout --conflict=<style>`: ask, then run.
    Recreate(String),
    RecreateConfirmed(String, String),
    /// The file changed on disk under unsaved edits: take the disk version
    /// or keep the edits.
    ReloadFromDisk,
    KeepEdits,
    /// Leave the editor for the raw combined diff (edits are kept).
    OpenRawDiff,
    /// Ask, then continue the operation in progress.
    Continue,
}

pub enum Msg {
    Read {
        rev: u64,
        overview: Result<Box<Overview>, String>,
        file: Option<(String, Result<Box<FileLoad>, String>)>,
    },
    Saved {
        path: String,
        hash: u64,
        ok: bool,
    },
    Changed {
        path: String,
        what: Change,
        ok: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Resolved,
    Chosen,
    Recreated,
}

impl Msg {
    /// One line for the activity log (`Err` when the work failed).
    pub fn summary(&self) -> Result<String, String> {
        match self {
            Msg::Read { overview, file, .. } => {
                let ov = overview.as_ref().map_err(Clone::clone)?;
                let mut s = format!(
                    "{} · {}",
                    crate::format::plural(ov.files.len(), "conflicted file", "conflicted files"),
                    ov.op.summary()
                );
                match file {
                    Some((p, Ok(f))) => {
                        let hunks = f.doc.as_ref().map(|d| d.hunk_count()).unwrap_or(0);
                        s.push_str(&format!(
                            " · {p}: {} ({}), {}, {} ms",
                            f.entry.kind.label(),
                            f.entry.kind.code(),
                            crate::format::plural(hunks, "conflict", "conflicts"),
                            f.elapsed_ms
                        ));
                    }
                    Some((p, Err(e))) => return Err(format!("{p}: {e}")),
                    None => {}
                }
                Ok(s)
            }
            Msg::Saved { path, ok, .. } | Msg::Changed { path, ok, .. } => {
                if *ok {
                    Ok(path.clone())
                } else {
                    Err(format!("{path} was not changed"))
                }
            }
        }
    }
}

/// Persisted per repository.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub show_base: bool,
}

impl State {
    pub fn prefs(&self) -> Prefs {
        Prefs {
            show_base: self.show_base,
        }
    }

    pub fn apply_prefs(&mut self, p: &Prefs) {
        self.show_base = p.show_base;
    }

    /// The open file's session.
    pub fn current(&self) -> Option<&Session> {
        self.sessions.get(self.open.as_deref()?)
    }

    pub fn current_mut(&mut self) -> Option<&mut Session> {
        let p = self.open.clone()?;
        self.sessions.get_mut(&p)
    }

    /// Conflicted files from the last read.
    pub fn files(&self) -> &[core::ConflictEntry] {
        match &self.overview {
            Some(Ok(o)) => &o.files,
            _ => &[],
        }
    }

    pub fn is_conflicted(&self, path: &str) -> bool {
        self.files().iter().any(|e| e.path == path)
    }

    /// Paths with unsaved edits.
    pub fn dirty_paths(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .sessions
            .values()
            .filter(|s| s.dirty)
            .map(|s| s.path().to_owned())
            .collect();
        v.sort();
        v
    }

    /// `(resolved, total)` conflicts of a file: the total is the count
    /// when it was first read, so hunks resolved and saved still count.
    pub fn progress(&self, s: &Session) -> (usize, usize) {
        let left = s.unresolved();
        let total = self
            .first_counts
            .get(s.path())
            .copied()
            .unwrap_or(0)
            .max(s.hunk_count());
        (total.saturating_sub(left), total)
    }

    /// What the two sides are called (operation labels, else the markers').
    pub fn sides(&self, s: Option<&Session>) -> (SideLabel, SideLabel) {
        let first = s.and_then(|s| s.hunk(0));
        let ours_marker = first.map(|h| h.ours_label.as_str()).unwrap_or("");
        let theirs_marker = first.map(|h| h.theirs_label.as_str()).unwrap_or("");
        match &self.overview {
            Some(Ok(o)) => (o.op.ours(ours_marker), o.op.theirs(theirs_marker)),
            _ => (
                SideLabel {
                    role: "Ours",
                    tag: "",
                    name: ours_marker.to_owned(),
                    detail: String::new(),
                },
                SideLabel {
                    role: "Theirs",
                    tag: "",
                    name: theirs_marker.to_owned(),
                    detail: String::new(),
                },
            ),
        }
    }

    /// The conflicted file after the open one (wrapping), if any other.
    pub fn next_file(&self) -> Option<String> {
        let files = self.files();
        if files.is_empty() {
            return None;
        }
        let at = self
            .open
            .as_deref()
            .and_then(|p| files.iter().position(|e| e.path == p));
        match at {
            Some(i) => (1..files.len())
                .map(|d| &files[(i + d) % files.len()])
                .map(|e| e.path.clone())
                .next(),
            // The open file is resolved: the first one in path order after
            // it, else the first.
            None => {
                let open = self.open.clone().unwrap_or_default();
                files
                    .iter()
                    .find(|e| e.path > open)
                    .or_else(|| files.first())
                    .map(|e| e.path.clone())
            }
        }
    }
}

/// Start a read of the conflicts (and the open file), or queue one.
fn request_read(cx: &mut Ctx, st: &mut State) {
    if st.reading {
        st.reread = true;
        return;
    }
    st.reading = true;
    let rev = st.read_gen;
    let path = st.open.clone();
    cx.read("Read conflicts", move |dir, log| {
        let mut cmds = Vec::new();
        let overview = core::overview(dir, &mut cmds).map(Box::new);
        let file = match (&overview, path) {
            (Ok(o), Some(p)) => o.files.iter().find(|e| e.path == p).map(|e| {
                let r = load::load_file(dir, e, &mut cmds).map(Box::new);
                (p.clone(), r)
            }),
            _ => None,
        };
        log.commands.extend(cmds);
        Msg::Read {
            rev,
            overview,
            file,
        }
    });
}

/// Replies of reads already running are stale after a change.
fn invalidate_reads(st: &mut State) {
    st.read_gen += 1;
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    let pending = repo
        .snapshot
        .as_ref()
        .map(|s| s.pending)
        .unwrap_or(Pending::None);
    let page = repo.page;
    let st = &mut repo.devops.conflict;
    match act {
        Act::Open(path) => {
            if page != Page::Graph {
                cx.action(Action::ShowPage(Page::Graph));
            }
            if !st.seen.contains(&path) {
                st.seen.push(path.clone());
            }
            if st.open.as_deref() != Some(path.as_str()) {
                st.whole = None;
            }
            st.open = Some(path);
            st.last_error = None;
            if let Some(s) = st.current_mut()
                && let Some(k) = s.first_unresolved()
            {
                s.focus = k;
                s.scroll_to = Some(k);
            }
            request_read(cx, st);
        }
        Act::Close => {
            let dirty = st.dirty_paths();
            if dirty.is_empty() {
                close(st);
            } else {
                cx.confirm(
                    "Close the conflict editor?",
                    format!(
                        "Unsaved edits to {} will be lost. The files on disk keep what was last saved, conflict markers included.",
                        names(&dirty)
                    ),
                    "Discard edits and close",
                    true,
                    Vec::new(),
                    Act::CloseConfirmed.into(),
                );
            }
        }
        Act::CloseConfirmed => close(st),
        Act::Refresh => {
            if let Some(p) = st.open.clone() {
                st.errors.remove(&p);
            }
            request_read(cx, st);
        }
        Act::Resolve(k, r) => {
            if let Some(s) = st.current_mut() {
                if s.editing.as_ref().is_some_and(|(e, _)| *e == k) {
                    s.editing = None;
                }
                if s.set(k, r) {
                    s.focus = k;
                }
            }
        }
        Act::ResolveAll(r) => {
            if let Some(s) = st.current_mut() {
                s.editing = None;
                for k in 0..s.hunk_count() {
                    s.set(k, r.clone());
                }
            }
        }
        Act::Focus(k) => {
            if let Some(s) = st.current_mut() {
                s.focus = k.min(s.hunk_count().saturating_sub(1));
            }
        }
        Act::Next | Act::Prev => {
            if let Some(s) = st.current_mut()
                && let Some(k) = s.step(matches!(act, Act::Next))
            {
                s.focus = k;
                s.scroll_to = Some(k);
            }
        }
        Act::NextFile => {
            if let Some(p) = st.next_file() {
                apply(cx, repo, Act::Open(p));
            }
        }
        Act::SetBase(on) => st.show_base = on,
        Act::EditHunk(k) => {
            if let Some(s) = st.current_mut() {
                if !s.load.utf8 {
                    cx.toast(
                        ToastKind::Error,
                        "This file is not UTF-8 text: resolve its conflicts by choosing a side, or edit it in another editor.",
                    );
                    return;
                }
                if let (Some(h), Some(r)) = (s.hunk(k), s.res.get(k)) {
                    let text = resolve::edit_text(&resolve::hunk_bytes(h, r));
                    s.editing = Some((k, text));
                    s.focus = k;
                    s.rev += 1;
                }
            }
        }
        Act::EditText(t) => {
            if let Some(s) = st.current_mut()
                && let Some((_, buf)) = s.editing.as_mut()
            {
                *buf = t;
                s.rev += 1;
            }
        }
        Act::EditApply => {
            if let Some(s) = st.current_mut()
                && let Some((k, text)) = s.editing.take()
            {
                let last = s.doc.ends_with_conflict() && k + 1 == s.hunk_count();
                if let Some(h) = s.hunk(k) {
                    let bytes = resolve::custom_bytes(&text, h.eol, last);
                    if !s.set(k, Resolution::Custom(bytes)) {
                        s.rev += 1;
                    }
                }
            }
        }
        Act::EditCancel => {
            if let Some(s) = st.current_mut() {
                s.editing = None;
                s.rev += 1;
            }
        }
        Act::WholeEdit => {
            let text = st
                .current()
                .filter(|s| s.load.utf8 && s.is_text())
                .map(|s| resolve::edit_text(&s.result()));
            if let Some(t) = text {
                st.whole = Some(t);
            }
        }
        Act::WholeApply => {
            if let Some(text) = st.whole.take()
                && let Some(s) = st.current_mut()
            {
                let bytes = resolve::whole_file_bytes(&text, s.doc.eol);
                if bytes != s.result() {
                    s.replace_text(&bytes);
                    s.focus = s.first_unresolved().unwrap_or(0);
                }
            }
        }
        Act::WholeCancel => st.whole = None,
        Act::Save => save(cx, st),
        Act::MarkResolved => mark_resolved(cx, st),
        Act::MarkResolvedConfirmed(path) => run_mark_resolved(cx, st, &path),
        Act::MarkResolvedNow => {
            if let Some(p) = st.open.clone() {
                run_mark_resolved(cx, st, &p);
            }
        }
        Act::Choose(choice) => choose(cx, st, choice),
        Act::ChooseConfirmed(path, choice) => run_choice(cx, st, &path, choice),
        Act::ChooseNow(choice) => {
            if let Some(p) = st.open.clone() {
                run_choice(cx, st, &p, choice);
            }
        }
        Act::Recreate(style) => {
            let Some(path) = st.open.clone() else { return };
            let dirty = st.current().is_some_and(|s| s.dirty);
            cx.confirm(
                format!("Recreate the conflict markers in {style} style?"),
                format!(
                    "Writes {path} again from the three versions in the index, with fresh conflict markers{}. Edits to this file, saved or not, are replaced.{}",
                    if style == "merge" {
                        ""
                    } else {
                        " and the common ancestor in every conflict"
                    },
                    if dirty { " You have unsaved edits here." } else { "" }
                ),
                "Recreate the markers",
                true,
                actions::render(&actions::recreate(&path, &style)),
                Act::RecreateConfirmed(path, style).into(),
            );
        }
        Act::RecreateConfirmed(path, style) => {
            invalidate_reads(st);
            st.busy = Some("Recreating the conflict".into());
            let cmds_v = actions::recreate(&path, &style);
            let label = format!("Recreate the conflict in {path} ({style} style)");
            cx.local_change(label, move |dir, log| {
                let mut cmds = Vec::new();
                let r = actions::run(dir, &cmds_v, &mut cmds);
                log.commands.extend(cmds);
                let ok = r.is_ok();
                (
                    r.map(|_| format!("{path}: conflict markers written again ({style} style)")),
                    Some(Msg::Changed {
                        path,
                        what: Change::Recreated,
                        ok,
                    }),
                )
            });
        }
        Act::ReloadFromDisk => {
            if let Some(s) = st.current_mut() {
                if let Some(newer) = s.external.take() {
                    *s = Session::new(*newer);
                } else {
                    let p = s.path().to_owned();
                    st.sessions.remove(&p);
                    request_read(cx, st);
                }
            }
        }
        Act::KeepEdits => {
            if let Some(s) = st.current_mut()
                && let Some(newer) = s.external.take()
            {
                // Saving will overwrite the disk version on purpose.
                s.disk_hash = newer.hash;
            }
        }
        Act::OpenRawDiff => {
            let Some(path) = st.open.take() else { return };
            st.whole = None;
            if st.sessions.get(&path).is_some_and(|s| s.dirty) {
                cx.toast(
                    ToastKind::Info,
                    format!(
                        "Unsaved edits to {path} are kept: open it in the conflict editor again to continue."
                    ),
                );
            }
            cx.action(Action::OpenFile(DiffTarget::Conflicted { path }));
        }
        Act::Continue => {
            if pending == Pending::None {
                return;
            }
            let op = GitOp::Continue(pending);
            let lines: Vec<String> = op.commands().iter().map(|c| git_line(c)).collect();
            cx.confirm(
                format!("Continue the {}?", pending.label().to_lowercase()),
                "Every conflict is resolved and staged. This records the result and moves the operation on; a rebase or cherry-pick may stop again at the next conflict.",
                format!("Continue {}", pending.label().to_lowercase()),
                false,
                lines,
                Action::Run(op),
            );
        }
    }
}

fn close(st: &mut State) {
    let show_base = st.show_base;
    *st = State {
        show_base,
        ..Default::default()
    };
}

/// `a.rs`, `a.rs and b.rs`, `a.rs, b.rs and 3 more`.
fn names(paths: &[String]) -> String {
    match paths {
        [] => String::new(),
        [a] => a.clone(),
        [a, b] => format!("{a} and {b}"),
        [a, b, rest @ ..] => format!("{a}, {b} and {} more", rest.len()),
    }
}

fn save(cx: &mut Ctx, st: &mut State) {
    let Some(s) = st.current() else { return };
    if !s.dirty || !s.is_text() {
        return;
    }
    let path = s.path().to_owned();
    let bytes = s.result();
    let hash = load::hash(&bytes);
    let (done, total) = st.progress(s);
    invalidate_reads(st);
    st.busy = Some("Saving".into());
    st.last_error = None;
    cx.local_change(format!("Save {path}"), move |dir, log| {
        let mut cmds = Vec::new();
        let r = actions::write(dir, &path, &bytes, &mut cmds);
        log.commands.extend(cmds);
        let ok = r.is_ok();
        let summary = r.map(|_| {
            format!(
                "{path}: {done} of {} resolved, {}",
                crate::format::plural(total, "conflict", "conflicts"),
                crate::format::bytes(bytes.len())
            )
        });
        (summary, Some(Msg::Saved { path, hash, ok }))
    });
}

fn mark_resolved(cx: &mut Ctx, st: &mut State) {
    let Some(s) = st.current() else { return };
    let path = s.path().to_owned();
    if s.is_text() {
        let (left, _) = resolve::remaining(&s.result(), s.doc.marker_size);
        if !left.is_empty() {
            cx.toast(
                ToastKind::Error,
                format!(
                    "{} of {path} still {} conflict markers.",
                    resolve::lines_phrase(&left),
                    if left.len() == 1 { "has" } else { "have" }
                ),
            );
            return;
        }
    }
    let mut details = Vec::new();
    if s.dirty {
        details.push(actions::write_line(&path, s.result().len()));
    }
    details.extend(actions::render(&actions::mark_resolved(&path)));
    let others = st.files().iter().filter(|e| e.path != path).count();
    cx.confirm(
        format!("Mark {path} resolved?"),
        format!(
            "{}Stages the file as it is on disk; git then treats its conflict as resolved. {}",
            if s.dirty {
                "Saves your edits, then "
            } else {
                ""
            },
            if others == 0 {
                "No other file is conflicted.".to_owned()
            } else {
                format!(
                    "{} still conflicted.",
                    crate::format::plural(others, "other file is", "other files are")
                )
            }
        ),
        "Mark resolved",
        false,
        details,
        Act::MarkResolvedConfirmed(path).into(),
    );
}

fn run_mark_resolved(cx: &mut Ctx, st: &mut State, path: &str) {
    let bytes = st
        .sessions
        .get(path)
        .filter(|s| s.dirty && s.is_text())
        .map(|s| s.result());
    invalidate_reads(st);
    st.busy = Some("Marking resolved".into());
    st.last_error = None;
    let path = path.to_owned();
    cx.local_change(format!("Mark {path} resolved"), move |dir, log| {
        let mut cmds = Vec::new();
        let r = (|| {
            if let Some(b) = &bytes {
                actions::write(dir, &path, b, &mut cmds)?;
            }
            actions::run(dir, &actions::mark_resolved(&path), &mut cmds)
        })();
        log.commands.extend(cmds);
        let ok = r.is_ok();
        (
            r.map(|_| format!("{path} marked resolved")),
            Some(Msg::Changed {
                path,
                what: Change::Resolved,
                ok,
            }),
        )
    });
}

fn choose(cx: &mut Ctx, st: &mut State, choice: Choice) {
    let Some(path) = st.open.clone() else { return };
    let Some(e) = st.files().iter().find(|e| e.path == path).cloned() else {
        return;
    };
    let (ours, theirs) = st.sides(st.current());
    let Some(spec) = actions::choice_specs(e.kind, &path, &ours.name, &theirs.name)
        .into_iter()
        .find(|s| s.choice == choice)
    else {
        return;
    };
    cx.confirm(
        format!("{}: {path}?", spec.title),
        format!("{} The file is then resolved.", spec.explain),
        spec.title.clone(),
        spec.danger,
        actions::render(&spec.commands),
        Act::ChooseConfirmed(path, choice).into(),
    );
}

fn run_choice(cx: &mut Ctx, st: &mut State, path: &str, choice: Choice) {
    let Some(e) = st.files().iter().find(|e| e.path == path).cloned() else {
        return;
    };
    let (ours, theirs) = st.sides(st.sessions.get(path));
    let Some(spec) = actions::choice_specs(e.kind, path, &ours.name, &theirs.name)
        .into_iter()
        .find(|s| s.choice == choice)
    else {
        return;
    };
    invalidate_reads(st);
    st.busy = Some(spec.title.clone());
    st.last_error = None;
    let path = path.to_owned();
    let title = spec.title.clone();
    cx.local_change(format!("{title}: {path}"), move |dir, log| {
        let mut cmds = Vec::new();
        let r = actions::run(dir, &spec.commands, &mut cmds);
        log.commands.extend(cmds);
        let ok = r.is_ok();
        (
            r.map(|_| format!("{path}: {}", title.to_lowercase())),
            Some(Msg::Changed {
                path,
                what: Change::Chosen,
                ok,
            }),
        )
    });
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    let st = &mut repo.devops.conflict;
    match msg {
        Msg::Read {
            rev,
            overview,
            file,
        } => {
            st.reading = false;
            let stale = rev != st.read_gen;
            if stale || st.reread {
                st.reread = false;
                if st.open.is_some() {
                    request_read(cx, st);
                }
                if stale {
                    return;
                }
            }
            take_read(st, overview, file);
        }
        Msg::Saved { path, hash, ok } => {
            st.busy = None;
            if ok {
                if let Some(s) = st.sessions.get_mut(&path) {
                    s.dirty = false;
                    s.disk_hash = hash;
                    s.external = None;
                }
            } else {
                st.last_error = Some(format!("{path} was not saved; see the activity log."));
            }
        }
        Msg::Changed { path, what, ok } => {
            st.busy = None;
            if ok {
                // Resolved files leave the conflicted list on the reload;
                // recreated ones are read again from disk.
                st.sessions.remove(&path);
                if what == Change::Recreated {
                    st.first_counts.remove(&path);
                }
            } else {
                st.last_error = Some(format!("{path} was not changed; see the activity log."));
            }
        }
    }
}

/// Apply a read's results.
fn take_read(
    st: &mut State,
    overview: Result<Box<Overview>, String>,
    file: Option<(String, Result<Box<FileLoad>, String>)>,
) {
    if let Ok(o) = &overview {
        for e in &o.files {
            if st.open.is_some() && !st.seen.contains(&e.path) {
                st.seen.push(e.path.clone());
            }
        }
        // Files resolved elsewhere: their sessions go.
        let gone: Vec<String> = st
            .sessions
            .keys()
            .filter(|p| !o.files.iter().any(|e| &e.path == *p))
            .cloned()
            .collect();
        for p in gone {
            if st.sessions.remove(&p).is_some_and(|s| s.dirty) {
                st.notice = Some(format!(
                    "Unsaved edits to {p} were dropped: the file is no longer conflicted."
                ));
            }
            st.first_counts.remove(&p);
        }
    }
    st.overview = Some(overview.map(|o| *o));
    let Some((path, r)) = file else { return };
    match r {
        Err(e) => {
            st.errors.insert(path, e);
        }
        Ok(load) => {
            st.errors.remove(&path);
            let hunks = load.doc.as_ref().map(|d| d.hunk_count()).unwrap_or(0);
            st.first_counts.entry(path.clone()).or_insert(hunks);
            let opened = st.open.as_deref() == Some(path.as_str());
            match st.sessions.get_mut(&path) {
                Some(s) if load.hash == s.disk_hash => {
                    // Unchanged on disk (or just saved): keep the session
                    // and its resolutions; take the fresh stage data.
                    s.load.entry = load.entry.clone();
                    s.external = None;
                }
                Some(s) if s.dirty => s.external = Some(load),
                _ => {
                    let mut s = Session::new(*load);
                    if opened && let Some(k) = s.first_unresolved() {
                        s.focus = k;
                        s.scroll_to = Some(k);
                    }
                    st.sessions.insert(path, s);
                }
            }
        }
    }
}

/// A console run of this repository ended.
pub fn on_console_done(_cx: &mut Ctx, _repo: &mut RepoState, _run: &ConsoleRun) {}

/// `HADION_SCRIPT` commands this area understands:
///
/// - `conflict PATH`: open the editor on a conflicted file.
/// - `conflict-next` / `conflict-prev`: the next / previous conflict
///   (unresolved first); `conflict-next-file`; `conflict-focus N`.
/// - `conflict-hunk N ours|theirs|both|both-theirs|base|reset` (N from 0).
/// - `conflict-all ours|theirs`.
/// - `conflict-base on|off`: the base pane.
/// - `conflict-edit N`, `conflict-type TEXT` (`\n` for new lines),
///   `conflict-apply`, `conflict-cancel`; `conflict-whole` (whole-file edit).
/// - `conflict-save`: write the file.
/// - `conflict-resolve`: `git add` the file without asking (scratch
///   repositories only); `conflict-resolve-ask` opens the confirmation.
/// - `conflict-choice ours|theirs|delete` opens the confirmation;
///   `conflict-choice-now …` runs it (scratch repositories only).
/// - `conflict-recreate merge|diff3|zdiff3` (confirmation).
/// - `conflict-raw`: the raw combined diff; `conflict-continue`
///   (confirmation); `conflict-close` (asks when edits are unsaved).
pub fn script(cmd: &str, arg: &str) -> Option<Vec<Action>> {
    let arg = arg.trim();
    let num = arg
        .split_whitespace()
        .next()
        .and_then(|n| n.parse::<usize>().ok());
    let one = |a: Act| Some(vec![a.into()]);
    match cmd {
        "conflict" if !arg.is_empty() => Some(vec![
            Action::ShowPage(Page::Graph),
            Act::Open(arg.to_owned()).into(),
        ]),
        "conflict-next" => one(Act::Next),
        "conflict-prev" => one(Act::Prev),
        "conflict-next-file" => one(Act::NextFile),
        "conflict-focus" => one(Act::Focus(num?)),
        "conflict-hunk" => {
            let mut it = arg.split_whitespace();
            let k = it.next()?.parse().ok()?;
            let r = Resolution::from_name(it.next()?)?;
            one(Act::Resolve(k, r))
        }
        "conflict-all" => match arg {
            "ours" => one(Act::ResolveAll(Resolution::Ours)),
            "theirs" => one(Act::ResolveAll(Resolution::Theirs)),
            _ => None,
        },
        "conflict-base" => one(Act::SetBase(arg != "off")),
        "conflict-edit" => one(Act::EditHunk(num?)),
        "conflict-type" => one(Act::EditText(arg.replace("\\n", "\n"))),
        "conflict-apply" => one(Act::EditApply),
        "conflict-cancel" => one(Act::EditCancel),
        "conflict-whole" => one(Act::WholeEdit),
        "conflict-save" => one(Act::Save),
        "conflict-resolve" => one(Act::MarkResolvedNow),
        "conflict-resolve-ask" => one(Act::MarkResolved),
        "conflict-choice" => one(Act::Choose(Choice::from_name(arg)?)),
        "conflict-choice-now" => one(Act::ChooseNow(Choice::from_name(arg)?)),
        "conflict-recreate" if actions::STYLES.contains(&arg) => one(Act::Recreate(arg.to_owned())),
        "conflict-raw" => one(Act::OpenRawDiff),
        "conflict-continue" => one(Act::Continue),
        "conflict-close" => one(Act::Close),
        "conflict-refresh" => one(Act::Refresh),
        _ => None,
    }
}

/// After every load: read the conflicts again while the editor is open
/// (a file resolved elsewhere then shows as resolved).
pub fn on_snapshot(cx: &mut Ctx, repo: &mut RepoState) {
    let st = &mut repo.devops.conflict;
    if st.open.is_some() {
        request_read(cx, st);
    }
}

/// Does the editor replace the graph?
pub fn is_open(repo: &RepoState) -> bool {
    repo.devops.conflict.open.is_some()
}

/// The editor, drawn in place of the graph.
pub fn show_editor(
    ui: &mut egui::Ui,
    repo: &mut RepoState,
    _console: &Console,
    settings: &Settings,
    actions: &mut Vec<Action>,
) {
    view::show(ui, repo, settings, actions);
}

/// "Resolve in the conflict editor" on conflicted files.
pub fn hook_wip_file_menu(
    ui: &mut egui::Ui,
    _h: &HookCx,
    path: &str,
    list: WipList,
    actions: &mut Vec<Action>,
) {
    if list != WipList::Conflicted {
        return;
    }
    if ui.button("Resolve in the conflict editor").clicked() {
        actions.push(Act::Open(path.to_owned()).into());
        ui.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::DevOpsAction;

    fn acts(cmd: &str, arg: &str) -> Vec<Act> {
        script(cmd, arg)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|a| match a {
                Action::DevOps(DevOpsAction::Conflict(a)) => Some(a),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn script_commands() {
        assert!(matches!(
            acts("conflict", "src/a.rs").as_slice(),
            [Act::Open(p)] if p == "src/a.rs"
        ));
        assert_eq!(script("conflict", "").map(|v| v.len()), None);
        assert!(matches!(
            acts("conflict-hunk", "2 theirs").as_slice(),
            [Act::Resolve(2, Resolution::Theirs)]
        ));
        assert!(matches!(
            acts("conflict-hunk", "0 both").as_slice(),
            [Act::Resolve(0, Resolution::OursThenTheirs)]
        ));
        assert!(script("conflict-hunk", "x ours").is_none());
        assert!(script("conflict-hunk", "1 maybe").is_none());
        assert!(matches!(
            acts("conflict-all", "ours").as_slice(),
            [Act::ResolveAll(Resolution::Ours)]
        ));
        assert!(matches!(
            acts("conflict-base", "off").as_slice(),
            [Act::SetBase(false)]
        ));
        assert!(matches!(
            acts("conflict-type", "a\\nb").as_slice(),
            [Act::EditText(t)] if t == "a\nb"
        ));
        assert!(matches!(
            acts("conflict-choice", "delete").as_slice(),
            [Act::Choose(Choice::Delete)]
        ));
        assert!(matches!(
            acts("conflict-recreate", "zdiff3").as_slice(),
            [Act::Recreate(s)] if s == "zdiff3"
        ));
        assert!(script("conflict-recreate", "fancy").is_none());
        for c in [
            "conflict-next",
            "conflict-prev",
            "conflict-save",
            "conflict-resolve",
            "conflict-close",
            "conflict-raw",
            "conflict-whole",
        ] {
            assert_eq!(acts(c, "").len(), 1, "{c}");
        }
        assert!(script("not-a-conflict-command", "").is_none());
    }

    #[test]
    fn names_and_next_file() {
        assert_eq!(names(&["a".into()]), "a");
        assert_eq!(names(&["a".into(), "b".into()]), "a and b");
        assert_eq!(
            names(&["a".into(), "b".into(), "c".into(), "d".into()]),
            "a, b and 2 more"
        );
        let entry = |p: &str| core::ConflictEntry {
            path: p.into(),
            kind: core::ConflictKind::BothModified,
            stages: [None, None, None],
            worktree_mode: "100644".into(),
        };
        let mut st = State {
            overview: Some(Ok(Overview {
                files: vec![entry("a"), entry("c"), entry("e")],
                op: Default::default(),
            })),
            open: Some("c".into()),
            ..Default::default()
        };
        assert_eq!(st.next_file().as_deref(), Some("e"));
        st.open = Some("e".into());
        assert_eq!(st.next_file().as_deref(), Some("a"));
        st.open = Some("d".into());
        assert_eq!(st.next_file().as_deref(), Some("e"), "after a resolved one");
        st.open = Some("z".into());
        assert_eq!(st.next_file().as_deref(), Some("a"));
    }

    #[test]
    fn prefs_round_trip() {
        let st = State {
            show_base: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&st.prefs()).unwrap();
        let mut back = State::default();
        back.apply_prefs(&serde_json::from_str(&json).unwrap());
        assert!(back.show_base);
        let empty: Prefs = serde_json::from_str("{}").unwrap();
        assert!(!empty.show_base);
    }
}
