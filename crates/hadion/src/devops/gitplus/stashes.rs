//! Stashes tab: every stash with branch, age, files and ± (`git stash
//! list` + `git stash show --numstat --include-untracked`), its diff file by
//! file, apply / pop / drop / branch, and the "Stash selected files" form
//! (`git stash push -m MSG [--keep-index] [--include-untracked] -- paths…`).

use std::collections::{BTreeSet, HashMap};
use std::time::Instant;

use egui::{RichText, Ui};
use hadion_core::ChangeKind;
use hadion_core::devops::gitplus::stash::{self as core, StashAction, StashEntry};
use hadion_core::devops::gitplus::{FileStatus, git_line, git_ok, short};

use super::{Act, Msg, ui as gui};
use crate::devops::ctx::Ctx;
use crate::format;
use crate::state::{Action, DiffDoc, Page, RepoState, ToastKind};
use crate::theme::pal;
use crate::views::form::{self, InputOpts};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Row, Stat, Tone};

/// One file of a stash's patch, ready to draw.
pub struct StashDiff {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub added: u64,
    pub deleted: u64,
    pub doc: DiffDoc,
}

/// The "Stash selected files" form.
#[derive(Clone, Debug, Default)]
pub struct Form {
    pub ticked: BTreeSet<String>,
    pub message: String,
    pub keep_index: bool,
    pub include_untracked: bool,
}

#[derive(Default)]
pub struct Stashes {
    pub list: Option<Result<Vec<StashEntry>, String>>,
    pub loading: bool,
    pub at: Option<Instant>,
    /// Sha of the selected stash.
    pub selected: Option<String>,
    pub pending_select: Option<usize>,
    /// Selected file in the stash's patch.
    pub file: usize,
    /// Patches by stash sha; `None` while loading.
    pub patches: HashMap<String, Option<Result<Vec<StashDiff>, String>>>,
    pub form: Option<Form>,
    pub branch_name: String,
    pub restore_index: bool,
    pub busy: bool,
}

impl Stashes {
    fn entries(&self) -> &[StashEntry] {
        match &self.list {
            Some(Ok(l)) => l,
            _ => &[],
        }
    }

    fn entry(&self, sha: &str) -> Option<&StashEntry> {
        self.entries().iter().find(|e| e.sha == sha)
    }
}

/// A changed file the form can tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changed {
    pub path: String,
    pub staged: bool,
    pub unstaged: bool,
    pub untracked: bool,
    pub letter: &'static str,
}

/// Changed files of the working tree (conflicted ones cannot be stashed).
pub fn changed_files(repo: &RepoState) -> (Vec<Changed>, usize) {
    let Some(snap) = repo.snapshot.as_ref() else {
        return (Vec::new(), 0);
    };
    let st = &snap.status;
    let mut out: Vec<Changed> = Vec::new();
    for e in &st.staged {
        out.push(Changed {
            path: e.path.clone(),
            staged: true,
            unstaged: false,
            untracked: false,
            letter: e.kind.letter(),
        });
    }
    for e in &st.unstaged {
        let untracked = e.kind == ChangeKind::Untracked;
        match out.iter_mut().find(|c| c.path == e.path) {
            Some(c) => c.unstaged = true,
            None => out.push(Changed {
                path: e.path.clone(),
                staged: false,
                unstaged: !untracked,
                untracked,
                letter: if untracked { "?" } else { e.kind.letter() },
            }),
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    (out, st.conflicted.len())
}

pub fn load(cx: &mut Ctx, repo: &mut RepoState) {
    let s = &mut repo.devops.gitplus.stashes;
    if s.loading {
        return;
    }
    s.loading = true;
    cx.read("List stashes", |dir, log| {
        let mut cmds = Vec::new();
        let r = core::load(dir, &mut cmds);
        log.commands.extend(cmds);
        Msg::Stashes(r)
    });
}

fn load_patch(cx: &mut Ctx, repo: &mut RepoState, sha: &str) {
    let s = &mut repo.devops.gitplus.stashes;
    let Some(e) = s.entry(sha).cloned() else {
        return;
    };
    if s.patches.contains_key(sha) {
        return;
    }
    s.patches.insert(sha.to_owned(), None);
    let dark = cx.dark;
    cx.read(format!("Diff of {}", e.reference()), move |dir, log| {
        let mut cmds = Vec::new();
        let r = core::patch(dir, e.index, &mut cmds).map(|files| {
            files
                .into_iter()
                .map(|f| StashDiff {
                    doc: gui::diff_doc(&f.raw, &f.path, dark),
                    path: f.path,
                    old_path: f.old_path,
                    status: f.status,
                    added: f.added,
                    deleted: f.deleted,
                })
                .collect()
        });
        log.commands.extend(cmds);
        Msg::StashPatch(e.sha.clone(), r)
    });
}

pub fn shown(cx: &mut Ctx, repo: &mut RepoState) {
    load(cx, repo);
}

/// After a reload: refresh when the stash list changed under us.
pub fn on_snapshot(cx: &mut Ctx, repo: &mut RepoState) {
    let Some(snap) = repo.snapshot.as_ref() else {
        return;
    };
    let s = &repo.devops.gitplus.stashes;
    let Some(Ok(list)) = &s.list else {
        return;
    };
    let ours: Vec<&str> = list.iter().map(|e| e.sha.as_str()).collect();
    let theirs: Vec<String> = snap.stashes.iter().map(|x| x.id.to_string()).collect();
    if ours != theirs.iter().map(String::as_str).collect::<Vec<_>>() {
        load(cx, repo);
    }
}

fn change(
    cx: &mut Ctx,
    label: String,
    run: impl FnOnce(&std::path::Path, &mut Vec<String>) -> Result<String, String> + Send + 'static,
    close_form: bool,
) {
    cx.local_change(label, move |dir, log| {
        let mut cmds = Vec::new();
        let r = run(dir, &mut cmds);
        log.commands.extend(cmds);
        let ok = r.is_ok();
        (r, Some(Msg::StashChanged { ok, close_form }))
    });
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::LoadStashes => load(cx, repo),
        Act::SelectStash(sha) => {
            let s = &mut repo.devops.gitplus.stashes;
            s.selected = sha.clone();
            s.file = 0;
            s.form = None;
            s.branch_name.clear();
            if let Some(sha) = sha {
                load_patch(cx, repo, &sha);
            }
        }
        Act::SelectStashIndex(n) => {
            let s = &mut repo.devops.gitplus.stashes;
            match &s.list {
                Some(Ok(l)) => {
                    let sha = l.get(n).map(|e| e.sha.clone());
                    apply(cx, repo, Act::SelectStash(sha));
                }
                _ => s.pending_select = Some(n),
            }
        }
        Act::SelectStashFile(i) => repo.devops.gitplus.stashes.file = i,
        Act::StashAction { index, sha, action } => {
            let Some(e) = repo.devops.gitplus.stashes.entry(&sha).cloned() else {
                return;
            };
            let args = action.args(index);
            let (title, body, confirm, danger) = match action {
                StashAction::Apply { restore_index } => (
                    "Apply the stash?",
                    format!(
                        "Puts the changes of {} back into the working tree{} and keeps the stash. Files changed since may conflict.",
                        e.reference(),
                        if restore_index {
                            ", restoring what was staged"
                        } else {
                            ""
                        }
                    ),
                    "Apply",
                    false,
                ),
                StashAction::Pop { restore_index } => (
                    "Pop the stash?",
                    format!(
                        "Puts the changes of {} back into the working tree{} and drops the stash when that worked. On a conflict the stash is kept.",
                        e.reference(),
                        if restore_index {
                            ", restoring what was staged"
                        } else {
                            ""
                        }
                    ),
                    "Pop",
                    false,
                ),
                StashAction::Drop => (
                    "Drop the stash?",
                    format!(
                        "Deletes {} (\"{}\", {}). It can be brought back from Insights > Recover until git prunes it.",
                        e.reference(),
                        e.message,
                        format::plural(
                            e.files.as_ref().map(Vec::len).unwrap_or(0),
                            "file",
                            "files"
                        )
                    ),
                    "Drop stash",
                    true,
                ),
            };
            crate::devops::pages::explain::confirm(
                cx,
                title,
                body,
                confirm,
                danger,
                vec![git_line(&args)],
                Act::StashActionConfirmed { index, sha, action }.into(),
            );
        }
        Act::StashActionConfirmed { index, sha, action } => {
            let s = &mut repo.devops.gitplus.stashes;
            if s.busy {
                return;
            }
            s.busy = true;
            if matches!(action, StashAction::Drop | StashAction::Pop { .. })
                && s.selected.as_deref() == Some(sha.as_str())
            {
                s.selected = None;
            }
            change(
                cx,
                format!("{} stash@{{{index}}}", action.label()),
                move |dir, cmds| core::run_checked(dir, index, &sha, &action.args(index), cmds),
                false,
            );
        }
        Act::StashBranch { index, sha } => {
            let name = repo.devops.gitplus.stashes.branch_name.trim().to_owned();
            if name.is_empty() || name.contains(char::is_whitespace) {
                cx.toast(
                    ToastKind::Error,
                    "Enter a branch name without spaces first.",
                );
                return;
            }
            if repo
                .snapshot
                .as_ref()
                .is_some_and(|s| s.branches.iter().any(|b| b.name == name))
            {
                cx.toast(
                    ToastKind::Error,
                    format!("The branch {name} exists already."),
                );
                return;
            }
            let base = repo
                .devops
                .gitplus
                .stashes
                .entry(&sha)
                .and_then(|e| e.base.clone())
                .unwrap_or_default();
            crate::devops::pages::explain::confirm(
                cx,
                "Create a branch from the stash?",
                format!(
                    "Creates the branch {name} at the commit the stash was made on ({}), switches to it, applies stash@{{{index}}} there and drops it when that worked. Your current branch is not touched.",
                    short(&base)
                ),
                "Create branch",
                false,
                vec![git_line(&core::branch_args(&name, index))],
                Act::StashBranchConfirmed { index, sha, name }.into(),
            );
        }
        Act::StashBranchConfirmed { index, sha, name } => {
            let s = &mut repo.devops.gitplus.stashes;
            if s.busy {
                return;
            }
            s.busy = true;
            s.selected = None;
            change(
                cx,
                format!("Branch {name} from stash@{{{index}}}"),
                move |dir, cmds| {
                    core::valid_branch_name(dir, &name, cmds)?;
                    core::run_checked(dir, index, &sha, &core::branch_args(&name, index), cmds)
                },
                false,
            );
        }
        Act::OpenStashForm { paths, message } => {
            let (changed, _) = changed_files(repo);
            let s = &mut repo.devops.gitplus.stashes;
            let ticked: BTreeSet<String> = paths
                .into_iter()
                .filter(|p| changed.iter().any(|c| &c.path == p))
                .collect();
            let include_untracked = changed
                .iter()
                .any(|c| c.untracked && ticked.contains(&c.path));
            s.form = Some(Form {
                ticked,
                message: message.unwrap_or_default(),
                keep_index: false,
                include_untracked,
            });
            s.selected = None;
            if repo.page != Page::Workbench
                || repo.devops.workbench.tab != crate::devops::workbench::Tab::Stashes
            {
                cx.action(Action::ShowPage(Page::Workbench));
                cx.action(crate::devops::workbench::Act::SetTab(
                    crate::devops::workbench::Tab::Stashes,
                ));
            }
        }
        Act::CloseStashForm => repo.devops.gitplus.stashes.form = None,
        Act::StashFiles => {
            let (changed, _) = changed_files(repo);
            let Some(f) = repo.devops.gitplus.stashes.form.clone() else {
                return;
            };
            let paths: Vec<String> = changed
                .iter()
                .filter(|c| f.ticked.contains(&c.path))
                .map(|c| c.path.clone())
                .collect();
            if paths.is_empty() {
                cx.toast(ToastKind::Error, "Tick at least one file to stash.");
                return;
            }
            let untracked = changed
                .iter()
                .any(|c| c.untracked && f.ticked.contains(&c.path));
            let include_untracked = f.include_untracked || untracked;
            let args = core::push_args(&f.message, f.keep_index, include_untracked, &paths);
            let left = changed.len() - paths.len();
            crate::devops::pages::explain::confirm(
                cx,
                "Stash the selected files?",
                format!(
                    "Moves the changes of {} into a new stash and puts those files back as they are in HEAD.{}{}",
                    format::plural(paths.len(), "file", "files"),
                    if left > 0 {
                        format!(
                            " The other {} stay as they are.",
                            format::plural(left, "changed file", "changed files")
                        )
                    } else {
                        String::new()
                    },
                    if f.keep_index {
                        " What is staged stays staged (--keep-index)."
                    } else {
                        ""
                    }
                ),
                "Stash",
                false,
                vec![git_line(&args)],
                Act::StashFilesConfirmed {
                    message: f.message.trim().to_owned(),
                    keep_index: f.keep_index,
                    include_untracked,
                    paths,
                }
                .into(),
            );
        }
        Act::StashFilesConfirmed {
            message,
            keep_index,
            include_untracked,
            paths,
        } => {
            let s = &mut repo.devops.gitplus.stashes;
            if s.busy {
                return;
            }
            s.busy = true;
            let n = paths.len();
            change(
                cx,
                format!("Stash {}", format::plural(n, "file", "files")),
                move |dir, cmds| {
                    git_ok(
                        dir,
                        &core::push_args(&message, keep_index, include_untracked, &paths),
                        cmds,
                    )
                    .map(|t| {
                        t.lines()
                            .find(|l| l.starts_with("Saved"))
                            .unwrap_or("Stashed")
                            .to_owned()
                    })
                },
                true,
            );
        }
        _ => {}
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    match msg {
        Msg::Stashes(r) => {
            let s = &mut repo.devops.gitplus.stashes;
            s.loading = false;
            s.at = Some(Instant::now());
            if let Ok(l) = &r {
                // Patches are keyed by sha: drop the ones of gone stashes.
                s.patches.retain(|sha, _| l.iter().any(|e| &e.sha == sha));
                if let Some(sel) = &s.selected
                    && !l.iter().any(|e| &e.sha == sel)
                {
                    s.selected = None;
                }
            }
            let pending = s.pending_select.take();
            s.list = Some(r);
            if let Some(n) = pending {
                apply(cx, repo, Act::SelectStashIndex(n));
            }
        }
        Msg::StashPatch(sha, r) => {
            repo.devops.gitplus.stashes.patches.insert(sha, Some(r));
        }
        Msg::StashChanged { ok, close_form } => {
            let s = &mut repo.devops.gitplus.stashes;
            s.busy = false;
            if ok && close_form {
                s.form = None;
            }
            if ok {
                s.branch_name.clear();
            }
            load(cx, repo);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

fn title_of(e: &StashEntry) -> String {
    if e.wip {
        format!("WIP: {}", e.message)
    } else if e.message.is_empty() {
        e.reference()
    } else {
        e.message.clone()
    }
}

fn subtitle_of(e: &StashEntry) -> String {
    let mut parts = vec![e.reference()];
    match &e.branch {
        Some(b) => parts.push(format!("on {b}")),
        None => parts.push("detached HEAD".to_owned()),
    }
    match &e.files {
        Some(f) => {
            parts.push(format::plural(f.len(), "file", "files"));
            parts.push(format!(
                "+{} −{}",
                format::count(e.added() as usize),
                format::count(e.deleted() as usize)
            ));
        }
        None if e.files_error.is_some() => parts.push("files unreadable".to_owned()),
        None => {}
    }
    parts.join("  ·  ")
}

fn badges_of(e: &StashEntry) -> Vec<(String, Tone)> {
    let mut b = Vec::new();
    let u = e.untracked_count();
    if u > 0 {
        b.push((format::plural(u, "untracked", "untracked"), Tone::Neutral));
    }
    b
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let now = format::now_secs();
    let busy = repo.is_busy() || repo.devops.gitplus.stashes.busy;
    let (changed, conflicted) = changed_files(repo);
    let s = &repo.devops.gitplus.stashes;
    kit::page_header(
        ui,
        "Stashes",
        crate::devops::workbench::Tab::Stashes.hint(),
        |ui| {
            if IconButton::new(Icon::Rerun, "Refresh")
                .compact()
                .enabled(!s.loading)
                .show(ui)
                .on_hover_text("git stash list, then git stash show --numstat for each")
                .clicked()
            {
                actions.push(Act::LoadStashes.into());
            }
            if IconButton::new(Icon::Stash, "Stash selected files…")
                .compact()
                .enabled(!changed.is_empty())
                .show(ui)
                .clicked()
            {
                actions.push(
                    Act::OpenStashForm {
                        paths: Vec::new(),
                        message: None,
                    }
                    .into(),
                );
            }
            if let (Some(Ok(l)), Some(at)) = (&s.list, s.at) {
                gui::header_note(
                    ui,
                    &format!(
                        "{}  ·  {}",
                        format::plural(l.len(), "stash", "stashes"),
                        gui::loaded_ago(at)
                    ),
                );
            }
        },
    );
    let list: Vec<StashEntry> = match &s.list {
        None => {
            gui::empty_pane(ui, |ui| kit::loading(ui, "Listing stashes…"));
            return;
        }
        Some(Err(e)) => {
            let e = e.clone();
            gui::empty_pane(ui, |ui| {
                if gui::error_card(ui, "Could not list the stashes", &e, "Try again") {
                    actions.push(Act::LoadStashes.into());
                }
            });
            return;
        }
        Some(Ok(l)) => l.clone(),
    };
    let selected = s.selected.clone();
    let form_open = s.form.is_some();
    if list.is_empty() && !form_open {
        gui::empty_pane(ui, |ui| {
            let body = if changed.is_empty() {
                "A stash keeps uncommitted changes aside so you can switch branches or pull, then brings them back.\nThe working tree is clean, so there is nothing to stash right now."
            } else {
                "A stash keeps uncommitted changes aside so you can switch branches or pull, then brings them back.\nStash some or all of your changed files to start."
            };
            let action = (!changed.is_empty()).then_some((Icon::Stash, "Stash selected files…"));
            if kit::empty_state(ui, Icon::Stash, "No stashes", body, action) {
                actions.push(
                    Act::OpenStashForm {
                        paths: Vec::new(),
                        message: None,
                    }
                    .into(),
                );
            }
        });
        return;
    }
    gui::list_pane(ui, "gp_stash_list", 400.0, |ui| {
        kit::list_header(ui, "Stashes", Some(list.len()), |_| {});
        ui.add_space(6.0);
        if list.is_empty() {
            kit::muted(ui, "No stashes yet.");
        }
        kit::list(
            ui,
            "gp_stash_rows",
            list.len(),
            kit::ROW_H,
            |i| !form_open && selected.as_deref() == Some(list[i].sha.as_str()),
            |ui, rect, resp, i| {
                let e = &list[i];
                let title = title_of(e);
                let sub = subtitle_of(e);
                let b = badges_of(e);
                let meta = format::relative(e.time, now);
                kit::paint_row(
                    ui,
                    rect,
                    &Row::new(&title)
                        .leading(kit::Leading::Icon(Icon::Stash, p.text_3))
                        .subtitle(&sub)
                        .meta(&meta)
                        .badges(&crate::devops::pages::layout::fit_badges(
                            ui,
                            rect.width(),
                            &b,
                        )),
                    !form_open && selected.as_deref() == Some(e.sha.as_str()),
                );
                if resp.clicked() {
                    actions.push(Act::SelectStash(Some(e.sha.clone())).into());
                }
            },
        );
    });
    let s = &mut repo.devops.gitplus.stashes;
    if s.form.is_some() {
        gui::detail_pane(ui, "gp_stash_form", |ui| {
            form_view(ui, s, &changed, conflicted, busy, actions);
        });
        return;
    }
    match selected.and_then(|sha| list.iter().find(|e| e.sha == sha).cloned()) {
        Some(e) => gui::detail_pane(ui, ("gp_stash", &e.sha), |ui| {
            detail(ui, s, &e, busy, now, actions);
        }),
        None => gui::detail_pane(ui, "gp_stash_overview", |ui| {
            overview(ui, &list, now);
        }),
    }
}

fn overview(ui: &mut Ui, list: &[StashEntry], now: i64) {
    kit::title(ui, "Stashes");
    let files: usize = list
        .iter()
        .map(|e| e.files.as_ref().map(Vec::len).unwrap_or(0))
        .sum();
    let oldest = list.iter().map(|e| e.time).min().unwrap_or(now);
    let branches: BTreeSet<&str> = list.iter().filter_map(|e| e.branch.as_deref()).collect();
    crate::views::insights::stats(
        ui,
        &[
            Stat::new("Stashes", format::count(list.len())),
            Stat::new("Files in them", format::count(files)),
            Stat::new("Branches", format::count(branches.len())),
            Stat::new("Oldest", format::relative(oldest, now))
                .tone_if(now - oldest > 30 * 86_400, Tone::Warning),
        ],
    );
    kit::secondary(ui, "Select a stash to see its changes, file by file.");
    kit::section(ui, "Apply, pop or branch", None);
    crate::devops::pages::explain::terms(
        ui,
        90.0,
        &[
            (
                "Apply",
                "Puts the changes back and keeps the stash, so you can apply it elsewhere too.",
            ),
            ("Pop", "Applies and drops it."),
            (
                "Branch",
                "Creates a branch at the commit the stash was made on and applies it there.\nThe safe choice when the code has moved on and applying would conflict.",
            ),
        ],
    );
    if now - oldest > 30 * 86_400 {
        kit::callout(
            ui,
            Tone::Warning,
            "Old stashes",
            "Stashes older than a month are easy to forget: turn the ones you still need into branches and drop the rest.",
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn detail(
    ui: &mut Ui,
    s: &mut Stashes,
    e: &StashEntry,
    busy: bool,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    crate::views::insights::detail_title(ui, &title_of(e), &badges_of(e));
    ui.add_space(4.0);
    kit::kv(ui, 120.0, |kv| {
        kv.text("Reference", kit::mono(e.reference()));
        kv.row("Commit", |ui| gui::copyable(ui, &e.sha, actions));
        kv.text(
            "Made on",
            match (&e.branch, &e.base) {
                (Some(b), Some(base)) => format!("{b} at {}", short(base)),
                (None, Some(base)) => format!("detached HEAD at {}", short(base)),
                (Some(b), None) => b.clone(),
                (None, None) => "unknown".to_owned(),
            },
        );
        kv.text("Date", gui::when(e.time, now));
        match (&e.files, &e.files_error) {
            (Some(f), _) => {
                let u = e.untracked_count();
                kv.text(
                    "Files",
                    if u > 0 {
                        format!(
                            "{}, {} of them untracked",
                            format::plural(f.len(), "file", "files"),
                            format::count(u)
                        )
                    } else {
                        format::plural(f.len(), "file", "files")
                    },
                );
                kv.row("Changes", |ui| gui::churn(ui, e.added(), e.deleted()));
            }
            (None, Some(err)) => kv.text("Files", RichText::new(err.as_str()).color(p.danger)),
            (None, None) => kv.muted("Files", "Not listed (only the newest stashes are)"),
        }
    });
    ui.add_space(4.0);
    form::checkbox(
        ui,
        &mut s.restore_index,
        "Restore what was staged too (--index)",
    );
    gui::buttons(ui, |ui| {
        let index = e.index;
        let sha = e.sha.clone();
        let restore_index = s.restore_index;
        if IconButton::new(Icon::Pop, "Apply")
            .enabled(!busy)
            .show(ui)
            .clicked()
        {
            actions.push(
                Act::StashAction {
                    index,
                    sha: sha.clone(),
                    action: StashAction::Apply { restore_index },
                }
                .into(),
            );
        }
        if IconButton::new(Icon::Pop, "Pop")
            .enabled(!busy)
            .show(ui)
            .clicked()
        {
            actions.push(
                Act::StashAction {
                    index,
                    sha: sha.clone(),
                    action: StashAction::Pop { restore_index },
                }
                .into(),
            );
        }
        if IconButton::new(Icon::Trash, "Drop…")
            .danger()
            .enabled(!busy)
            .show(ui)
            .clicked()
        {
            actions.push(
                Act::StashAction {
                    index,
                    sha,
                    action: StashAction::Drop,
                }
                .into(),
            );
        }
    });
    kit::section(ui, "Branch from it", None);
    ui.horizontal_wrapped(|ui| {
        let iw = crate::devops::pages::layout::input_width(ui, 260.0);
        form::input(
            ui,
            ("gp_stash_branch", e.sha.as_str()),
            &mut s.branch_name,
            InputOpts::new("new-branch-name").width(iw).mono(),
        );
        if form::button(
            ui,
            "Create branch",
            !busy && !s.branch_name.trim().is_empty(),
        )
        .clicked()
        {
            actions.push(
                Act::StashBranch {
                    index: e.index,
                    sha: e.sha.clone(),
                }
                .into(),
            );
        }
    });
    kit::secondary(
        ui,
        "git stash branch: a new branch at the commit the stash was made on, with the stash applied and dropped.",
    );
    kit::section(ui, "Changes", e.files.as_ref().map(Vec::len));
    match s.patches.get(&e.sha) {
        None | Some(None) => kit::loading(ui, "Loading the diff…"),
        Some(Some(Err(err))) => {
            if gui::error_card(ui, "Could not load the diff", err, "Try again") {
                s.patches.remove(&e.sha);
                actions.push(Act::SelectStash(Some(e.sha.clone())).into());
            }
        }
        Some(Some(Ok(files))) if files.is_empty() => {
            kit::muted(ui, "This stash records no file changes.");
        }
        Some(Some(Ok(files))) => {
            let untracked: BTreeSet<&str> = e
                .files
                .iter()
                .flatten()
                .filter(|f| f.untracked)
                .map(|f| f.path.as_str())
                .collect();
            let sel = s.file.min(files.len() - 1);
            for (i, f) in files.iter().enumerate().take(300) {
                let (letter, color) = match f.status {
                    FileStatus::Added if untracked.contains(f.path.as_str()) => ("?", p.text_2),
                    FileStatus::Added => ("A", p.success),
                    FileStatus::Deleted => ("D", p.danger),
                    FileStatus::Renamed => ("R", p.accent),
                    FileStatus::Modified => ("M", p.warning),
                };
                let meta = format!("+{} −{}", f.added, f.deleted);
                let title = match &f.old_path {
                    Some(old) => format!("{old} → {}", f.path),
                    None => f.path.clone(),
                };
                if gui::simple_row(ui, Some((letter, color)), &title, &meta, i == sel, true)
                    .clicked()
                {
                    actions.push(Act::SelectStashFile(i).into());
                }
            }
            if files.len() > 300 {
                kit::muted(
                    ui,
                    &format!("and {} more files", format::count(files.len() - 300)),
                );
            }
            let f = &files[sel];
            kit::section(ui, &format!("Diff of {}", f.path), None);
            if untracked.contains(f.path.as_str()) {
                kit::muted(ui, "Untracked when it was stashed (--include-untracked).");
            }
            gui::diff_view(ui, &format!("{}:{}", e.sha, f.path), &f.doc);
        }
    }
}

fn form_view(
    ui: &mut Ui,
    s: &mut Stashes,
    changed: &[Changed],
    conflicted: usize,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    let Some(f) = s.form.as_mut() else {
        return;
    };
    kit::title(ui, "Stash selected files");
    kit::secondary(
        ui,
        "Moves the changes of the ticked files into a new stash and puts those files back as they are in HEAD. The other files keep their changes.",
    );
    if changed.is_empty() {
        kit::callout(
            ui,
            Tone::Neutral,
            "Nothing to stash",
            "The working tree has no changes.",
        );
        form::actions_row(ui, |ui| {
            if form::button(ui, "Close", true).clicked() {
                actions.push(Act::CloseStashForm.into());
            }
        });
        return;
    }
    kit::section_with(ui, "Files", Some(changed.len()), |ui| {
        if IconButton::text("None").compact().show(ui).clicked() {
            f.ticked.clear();
        }
        if IconButton::text("All").compact().show(ui).clicked() {
            f.ticked = changed.iter().map(|c| c.path.clone()).collect();
        }
    });
    for c in changed {
        let mut on = f.ticked.contains(&c.path);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if form::checkbox(ui, &mut on, &c.path).changed() {
                if on {
                    f.ticked.insert(c.path.clone());
                    if c.untracked {
                        f.include_untracked = true;
                    }
                } else {
                    f.ticked.remove(&c.path);
                }
            }
            if c.staged {
                kit::badge(ui, "Staged", Tone::Accent);
            }
            if c.unstaged {
                kit::badge(ui, "Unstaged", Tone::Neutral);
            }
            if c.untracked {
                kit::badge(ui, "Untracked", Tone::Neutral);
            }
        });
    }
    if conflicted > 0 {
        kit::muted(
            ui,
            &format!(
                "{} not listed: resolve conflicts before stashing them.",
                format::plural(conflicted, "conflicted file is", "conflicted files are")
            ),
        );
    }
    kit::section(ui, "Options", None);
    form::field(
        ui,
        "Message",
        Some("Shown in the stash list; empty gives git's \"WIP on branch\" text."),
        |ui| form::text(ui, "gp_stash_msg", &mut f.message, "What these changes are"),
    );
    form::checkbox(
        ui,
        &mut f.keep_index,
        "Keep the index (--keep-index): staged changes stay staged",
    );
    let needs_untracked = changed
        .iter()
        .any(|c| c.untracked && f.ticked.contains(&c.path));
    if needs_untracked {
        f.include_untracked = true;
    }
    ui.add_enabled_ui(!needs_untracked, |ui| {
        form::checkbox(
            ui,
            &mut f.include_untracked,
            "Include untracked files (--include-untracked)",
        );
    });
    if needs_untracked {
        kit::muted(ui, "On because an untracked file is ticked.");
    }
    let paths: Vec<String> = changed
        .iter()
        .filter(|c| f.ticked.contains(&c.path))
        .map(|c| c.path.clone())
        .collect();
    kit::section(ui, "Command", None);
    if paths.is_empty() {
        kit::muted(ui, "Tick at least one file.");
    } else {
        let line = git_line(&core::push_args(
            &f.message,
            f.keep_index,
            f.include_untracked,
            &paths,
        ));
        kit::code_block(ui, &line, actions);
    }
    let n = paths.len();
    let label = if n == 0 {
        "Stash".to_owned()
    } else {
        format!("Stash {}", format::plural(n, "file", "files"))
    };
    form::actions_row(ui, |ui| {
        if form::primary(ui, &label, n > 0 && !busy).clicked() {
            actions.push(Act::StashFiles.into());
        }
        if form::button(ui, "Cancel", true).clicked() {
            actions.push(Act::CloseStashForm.into());
        }
    });
}
