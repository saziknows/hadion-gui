//! What an area controller can ask the app to do.
//!
//! [`Ctx`] is a command buffer: `apply` / `on_msg` push effects (background
//! reads, changes, console processes, app actions, toasts) and the app runs
//! them after the call returns. Areas never borrow the worker or the app.
//!
//! Logging and feedback are uniform:
//! - [`Ctx::gh_read`] / [`Ctx::user_read`]: GitHub API activity (verbose log).
//! - [`Ctx::read`]: a background read (verbose log) with the commands it ran.
//! - [`Ctx::gh_change`]: a change on GitHub, always logged as an operation
//!   with every request (and its JSON body unless withheld), success or
//!   error toast.
//! - [`Ctx::local_change`]: a change to the repository; marks it busy, logs
//!   an operation, toasts, then reloads the repository.
//! - [`Ctx::console`]: a process streamed into the console.

use std::path::{Path, PathBuf};

use gitvis_core::devops::forge::ForgeRemote;
use gitvis_core::devops::gh::Gh;
use gitvis_core::devops::proc::ProcSpec;
use gitvis_core::github::ApiCall;
use gitvis_core::ops::GitOp;

use super::DevOpsMsg;
use crate::state::{Action, Dialog, RepoId, ToastKind};

/// What a background task recorded while it ran.
#[derive(Default, Debug)]
pub struct TaskLog {
    /// GitHub API calls (`gh api …` and other `gh` subcommands).
    pub calls: Vec<ApiCall>,
    /// Other commands, rendered as typed (`kubectl get pods -o json`).
    pub commands: Vec<String>,
}

impl TaskLog {
    /// Record a command line.
    pub fn command(&mut self, c: impl Into<String>) {
        self.commands.push(c.into());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskKind {
    GhRead,
    LocalRead,
    GhChange,
    LocalChange,
}

impl TaskKind {
    pub fn is_change(self) -> bool {
        matches!(self, TaskKind::GhChange | TaskKind::LocalChange)
    }
}

/// What a task produced.
pub struct TaskDone {
    pub log: TaskLog,
    /// Set for changes: the summary on success, the error otherwise.
    pub result: Option<Result<String, String>>,
    pub msg: Option<DevOpsMsg>,
}

/// Background work queued by an area.
pub struct Task {
    pub repo: Option<RepoId>,
    pub label: String,
    pub kind: TaskKind,
    pub run: Box<dyn FnOnce() -> TaskDone + Send + 'static>,
}

pub enum Effect {
    Action(Action),
    Task(Task),
    Console {
        id: u64,
        repo: Option<RepoId>,
        title: String,
        spec: ProcSpec,
    },
}

/// The context an area's `apply` / `on_msg` gets.
pub struct Ctx {
    pub repo: Option<RepoId>,
    pub workdir: Option<PathBuf>,
    /// `gh api` bound to the repository's GitHub remote, if it has one.
    pub gh: Option<Gh>,
    /// The GitLab / Azure DevOps remote, if any.
    pub forge: Option<ForgeRemote>,
    pub dark: bool,
    pub verbose: bool,
    effects: Vec<Effect>,
}

impl Ctx {
    pub fn new(
        repo: Option<RepoId>,
        workdir: Option<PathBuf>,
        gh: Option<Gh>,
        forge: Option<ForgeRemote>,
        dark: bool,
        verbose: bool,
    ) -> Self {
        Self {
            repo,
            workdir,
            gh,
            forge,
            dark,
            verbose,
            effects: Vec::new(),
        }
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// Run an app action after this call (show a page, open a URL, …).
    pub fn action(&mut self, a: impl Into<Action>) {
        self.effects.push(Effect::Action(a.into()));
    }

    pub fn toast(&mut self, kind: ToastKind, msg: impl Into<String>) {
        self.action(Action::Toast(kind, msg.into()));
    }

    /// Run git operations through the normal operation pipeline (busy
    /// indicator, activity log, refresh, queueing).
    pub fn ops(&mut self, ops: Vec<GitOp>) {
        if !ops.is_empty() {
            self.action(Action::RunSeq(ops));
        }
    }

    /// Ask for confirmation, then run `action`. `details` are shown in a
    /// monospace block (the exact commands or request bodies).
    pub fn confirm(
        &mut self,
        title: impl Into<String>,
        body: impl Into<String>,
        confirm: impl Into<String>,
        danger: bool,
        details: Vec<String>,
        action: Action,
    ) {
        self.action(Action::Dialog(Dialog::ConfirmAction {
            title: title.into(),
            body: body.into(),
            confirm: confirm.into(),
            danger,
            details,
            action: Box::new(action),
        }));
    }

    /// A read through `gh api` in the background; `f`'s message comes back
    /// to the area's `on_msg`. Returns false (and does nothing) without a
    /// GitHub remote.
    pub fn gh_read<M: Into<DevOpsMsg>>(
        &mut self,
        label: impl Into<String>,
        f: impl FnOnce(&Gh, &mut TaskLog) -> M + Send + 'static,
    ) -> bool {
        let Some(gh) = self.gh.clone() else {
            return false;
        };
        self.effects.push(Effect::Task(Task {
            repo: self.repo,
            label: label.into(),
            kind: TaskKind::GhRead,
            run: Box::new(move || {
                let mut log = TaskLog::default();
                let msg = f(&gh, &mut log).into();
                TaskDone {
                    log,
                    result: None,
                    msg: Some(msg),
                }
            }),
        }));
        true
    }

    /// A read through `gh api` that is not about this repository
    /// (notifications, search, the user). Works without a GitHub remote.
    pub fn user_read<M: Into<DevOpsMsg>>(
        &mut self,
        label: impl Into<String>,
        f: impl FnOnce(&Gh, &mut TaskLog) -> M + Send + 'static,
    ) {
        let dir = self
            .workdir
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let gh = Gh::user(dir);
        self.effects.push(Effect::Task(Task {
            repo: self.repo,
            label: label.into(),
            kind: TaskKind::GhRead,
            run: Box::new(move || {
                let mut log = TaskLog::default();
                let msg = f(&gh, &mut log).into();
                TaskDone {
                    log,
                    result: None,
                    msg: Some(msg),
                }
            }),
        }));
    }

    /// A local read in the background (git, kubectl, terraform, files).
    /// `f` gets the working directory. Returns false without a repository.
    pub fn read<M: Into<DevOpsMsg>>(
        &mut self,
        label: impl Into<String>,
        f: impl FnOnce(&Path, &mut TaskLog) -> M + Send + 'static,
    ) -> bool {
        let Some(dir) = self.workdir.clone() else {
            return false;
        };
        self.effects.push(Effect::Task(Task {
            repo: self.repo,
            label: label.into(),
            kind: TaskKind::LocalRead,
            run: Box::new(move || {
                let mut log = TaskLog::default();
                let msg = f(&dir, &mut log).into();
                TaskDone {
                    log,
                    result: None,
                    msg: Some(msg),
                }
            }),
        }));
        true
    }

    /// A change on GitHub. `f` returns the outcome (a one-line summary on
    /// success, the error otherwise) and an optional message for the area
    /// (sent in both cases, e.g. to refresh or to clear a "submitting"
    /// state). Always logged as an operation; toasts the outcome.
    pub fn gh_change<M: Into<DevOpsMsg>>(
        &mut self,
        label: impl Into<String>,
        f: impl FnOnce(&Gh, &mut TaskLog) -> (Result<String, String>, Option<M>) + Send + 'static,
    ) -> bool {
        let Some(gh) = self.gh.clone() else {
            return false;
        };
        self.effects.push(Effect::Task(Task {
            repo: self.repo,
            label: label.into(),
            kind: TaskKind::GhChange,
            run: Box::new(move || {
                let mut log = TaskLog::default();
                let (result, msg) = f(&gh, &mut log);
                TaskDone {
                    log,
                    result: Some(result),
                    msg: msg.map(Into::into),
                }
            }),
        }));
        true
    }

    /// A change that is not about this repository's GitHub remote (marking
    /// notifications read, GitLab / Azure DevOps writes). Logged and toasted
    /// like [`Ctx::gh_change`]; `f` gets the working directory.
    pub fn remote_change<M: Into<DevOpsMsg>>(
        &mut self,
        label: impl Into<String>,
        f: impl FnOnce(&Path, &mut TaskLog) -> (Result<String, String>, Option<M>) + Send + 'static,
    ) {
        let dir = self
            .workdir
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        self.effects.push(Effect::Task(Task {
            repo: self.repo,
            label: label.into(),
            kind: TaskKind::GhChange,
            run: Box::new(move || {
                let mut log = TaskLog::default();
                let (result, msg) = f(&dir, &mut log);
                TaskDone {
                    log,
                    result: Some(result),
                    msg: msg.map(Into::into),
                }
            }),
        }));
    }

    /// A change to the repository (worktrees, bisect, rebase, files). The
    /// repository shows as busy while it runs and is reloaded afterwards.
    pub fn local_change<M: Into<DevOpsMsg>>(
        &mut self,
        label: impl Into<String>,
        f: impl FnOnce(&Path, &mut TaskLog) -> (Result<String, String>, Option<M>) + Send + 'static,
    ) -> bool {
        let Some(dir) = self.workdir.clone() else {
            return false;
        };
        self.effects.push(Effect::Task(Task {
            repo: self.repo,
            label: label.into(),
            kind: TaskKind::LocalChange,
            run: Box::new(move || {
                let mut log = TaskLog::default();
                let (result, msg) = f(&dir, &mut log);
                TaskDone {
                    log,
                    result: Some(result),
                    msg: msg.map(Into::into),
                }
            }),
        }));
        true
    }

    /// Stream a process into the console. Returns the run id; the area's
    /// `on_console_done` is called with the run when it ends.
    pub fn console(&mut self, title: impl Into<String>, spec: ProcSpec) -> u64 {
        let id = super::console::next_id();
        self.effects.push(Effect::Console {
            id,
            repo: self.repo,
            title: title.into(),
            spec,
        });
        id
    }
}
