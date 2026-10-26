//! Worktrees: every checkout of this repository with its branch, state
//! and ahead / behind; open one as a tab, add, lock / unlock, move, remove
//! and prune.

use std::path::{Path, PathBuf};
use std::time::Instant;

use egui::{RichText, Ui, vec2};
use gitvis_core::devops::workbench::worktrees::{
    self as core, AddMode, Entry, default_path, same_path,
};
use gitvis_core::devops::workbench::{git, git_raw, output_text};

use super::{Act, Msg, ui as wui};
use crate::devops::ctx::Ctx;
use crate::format;
use crate::state::{Action, RepoState};
use crate::theme::{pal, size};
use crate::views::form::{self, InputOpts};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Row, Stat, Tone};

/// How a new worktree gets its checkout (form state).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    NewBranch,
    Existing,
    Detached,
}

#[derive(Default)]
pub struct Worktrees {
    pub list: Option<Result<Vec<Entry>, String>>,
    pub loading: bool,
    pub at: Option<Instant>,
    /// Path of the selected worktree.
    pub selected: Option<String>,
    pub pending_select: Option<usize>,
    pub prune: Option<Result<String, String>>,
    pub prune_loading: bool,
    pub busy: bool,
    pub mode: Mode,
    pub new_branch: String,
    pub start: String,
    pub existing: String,
    pub commit: String,
    pub path: String,
    /// The path last filled in automatically (typing over it stops that).
    pub path_auto: String,
    pub lock_reason: String,
    pub move_to: String,
}

impl Worktrees {
    fn entries(&self) -> &[Entry] {
        match &self.list {
            Some(Ok(l)) => l,
            _ => &[],
        }
    }

    fn entry(&self, path: &str) -> Option<&Entry> {
        self.entries().iter().find(|e| e.wt.path == path)
    }
}

fn load(cx: &mut Ctx, repo: &mut RepoState) {
    let w = &mut repo.devops.workbench.worktrees;
    if w.loading {
        return;
    }
    w.loading = true;
    cx.read("List worktrees", |dir, log| {
        let mut cmds = Vec::new();
        let r = core::list(dir, &mut cmds);
        log.commands.extend(cmds);
        Msg::Worktrees(r)
    });
}

pub fn shown(cx: &mut Ctx, repo: &mut RepoState) {
    load(cx, repo);
}

fn change(cx: &mut Ctx, repo: &mut RepoState, label: &str, args: Vec<String>) {
    let w = &mut repo.devops.workbench.worktrees;
    if w.busy {
        return;
    }
    w.busy = true;
    cx.local_change(label.to_owned(), move |dir, log| {
        let mut cmds = Vec::new();
        let r = git_raw(dir, &args, &[], &mut cmds);
        log.commands.extend(cmds);
        let res = match r {
            Ok(out) if out.success => {
                let t = output_text(&out);
                Ok(if t.is_empty() { "Done".to_owned() } else { t })
            }
            Ok(out) => Err(output_text(&out)),
            Err(e) => Err(e),
        };
        (res, Some(Msg::WorktreeChanged))
    });
}

fn main_path(repo: &RepoState) -> PathBuf {
    repo.devops
        .workbench
        .worktrees
        .entries()
        .iter()
        .find(|e| e.wt.main)
        .map(|e| PathBuf::from(&e.wt.path))
        .unwrap_or_else(|| repo.workdir.clone())
}

fn display(p: &str) -> String {
    if cfg!(windows) {
        p.replace('/', "\\")
    } else {
        p.to_owned()
    }
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::LoadWorktrees => load(cx, repo),
        Act::SelectWorktree(p) => {
            let w = &mut repo.devops.workbench.worktrees;
            w.selected = p;
            w.lock_reason.clear();
            w.move_to.clear();
        }
        Act::SelectWorktreeIndex(n) => {
            let w = &mut repo.devops.workbench.worktrees;
            match &w.list {
                Some(Ok(l)) => w.selected = l.get(n).map(|e| e.wt.path.clone()),
                _ => w.pending_select = Some(n),
            }
        }
        Act::SetWorktreeBranch(name) => {
            let main = main_path(repo);
            let w = &mut repo.devops.workbench.worktrees;
            let path = display(&default_path(&main, &name).to_string_lossy());
            w.mode = Mode::NewBranch;
            w.new_branch = name;
            w.path = path.clone();
            w.path_auto = path;
        }
        Act::AddWorktree => {
            let w = &repo.devops.workbench.worktrees;
            let path = w.path.trim().to_owned();
            let mode = match w.mode {
                Mode::NewBranch => AddMode::NewBranch {
                    name: w.new_branch.trim().to_owned(),
                    start: w.start.trim().to_owned(),
                },
                Mode::Existing => AddMode::Existing {
                    branch: w.existing.trim().to_owned(),
                },
                Mode::Detached => AddMode::Detached {
                    commit: w.commit.trim().to_owned(),
                },
            };
            let missing = match &mode {
                AddMode::NewBranch { name, .. } if name.is_empty() => Some("a branch name"),
                AddMode::Existing { branch } if branch.is_empty() => Some("a branch"),
                _ if path.is_empty() => Some("a folder"),
                _ => None,
            };
            if let Some(m) = missing {
                cx.toast(crate::state::ToastKind::Error, format!("Enter {m} first."));
                return;
            }
            if Path::new(&path).exists() {
                cx.toast(
                    crate::state::ToastKind::Error,
                    format!("{path} exists already: pick a new folder."),
                );
                return;
            }
            let args = core::add_args(&path, &mode);
            let what = match &mode {
                AddMode::NewBranch { name, start } => format!(
                    "a new branch {name} from {}",
                    if start.is_empty() { "HEAD" } else { start }
                ),
                AddMode::Existing { branch } => format!("the branch {branch}"),
                AddMode::Detached { commit } => format!(
                    "{} (detached)",
                    if commit.is_empty() { "HEAD" } else { commit }
                ),
            };
            cx.confirm(
                "Add a worktree?",
                format!("Creates {} with {what} checked out. It shares this repository's history, branches and stashes.", display(&path)),
                "Add worktree",
                false,
                vec![core::line(&args)],
                Act::AddWorktreeConfirmed(path, mode).into(),
            );
        }
        Act::AddWorktreeConfirmed(path, mode) => {
            let w = &mut repo.devops.workbench.worktrees;
            w.selected = Some(path.replace('\\', "/"));
            change(cx, repo, "Add worktree", core::add_args(&path, &mode));
        }
        Act::RemoveWorktree(path) => {
            let Some(e) = repo.devops.workbench.worktrees.entry(&path).cloned() else {
                return;
            };
            if e.wt.main || same_path(Path::new(&path), &repo.workdir) {
                cx.toast(
                    crate::state::ToastKind::Error,
                    "The main worktree and the open one cannot be removed here.",
                );
                return;
            }
            let status = e.status.clone().and_then(|s| s.ok());
            let dirty = status.as_ref().is_some_and(|s| s.dirty());
            let locked = e.wt.locked.is_some();
            let args = core::remove_args(&path, dirty, locked);
            let mut details = vec![core::line(&args)];
            let mut body = format!("Deletes the folder {}", display(&path));
            if let Some(b) = &e.wt.branch {
                body.push_str(&format!("; the branch {b} stays."));
            } else {
                body.push('.');
            }
            if let Some(s) = &status
                && s.dirty()
            {
                body.push_str(&format!(
                    " It has {} uncommitted change{} that will be lost (--force):",
                    s.changes(),
                    if s.changes() == 1 { "" } else { "s" }
                ));
                let mut files: Vec<String> = s
                    .files
                    .iter()
                    .map(|(p, code)| format!("{code}  {p}"))
                    .collect();
                if s.more > 0 {
                    files.push(format!("… and {} more", s.more));
                }
                details.push(files.join("\n"));
            }
            if locked {
                body.push_str(" It is locked: --force twice removes it anyway.");
            }
            cx.confirm(
                "Remove the worktree?",
                body,
                if dirty {
                    "Remove and lose changes"
                } else {
                    "Remove"
                },
                true,
                details,
                Act::RemoveWorktreeConfirmed(path, dirty, locked).into(),
            );
        }
        Act::RemoveWorktreeConfirmed(path, dirty, locked) => {
            let w = &mut repo.devops.workbench.worktrees;
            if w.selected.as_deref() == Some(path.as_str()) {
                w.selected = None;
            }
            change(
                cx,
                repo,
                "Remove worktree",
                core::remove_args(&path, dirty, locked),
            );
        }
        Act::LockWorktree(path) => {
            let reason = repo
                .devops
                .workbench
                .worktrees
                .lock_reason
                .trim()
                .to_owned();
            let args = core::lock_args(&path, &reason);
            cx.confirm(
                "Lock the worktree?",
                "A locked worktree is never pruned, moved or removed by git until it is unlocked: useful for one on a removable disk or a network share.",
                "Lock",
                false,
                vec![core::line(&args)],
                Act::LockWorktreeConfirmed(path, reason).into(),
            );
        }
        Act::LockWorktreeConfirmed(path, reason) => {
            change(cx, repo, "Lock worktree", core::lock_args(&path, &reason))
        }
        Act::UnlockWorktree(path) => {
            let args = core::unlock_args(&path);
            cx.confirm(
                "Unlock the worktree?",
                "git may then prune it when its folder is missing.",
                "Unlock",
                false,
                vec![core::line(&args)],
                Act::UnlockWorktreeConfirmed(path).into(),
            );
        }
        Act::UnlockWorktreeConfirmed(path) => {
            change(cx, repo, "Unlock worktree", core::unlock_args(&path))
        }
        Act::MoveWorktree(path) => {
            let to = repo.devops.workbench.worktrees.move_to.trim().to_owned();
            if to.is_empty() {
                cx.toast(
                    crate::state::ToastKind::Error,
                    "Enter the new folder first.",
                );
                return;
            }
            let args = core::move_args(&path, &to);
            cx.confirm(
                "Move the worktree?",
                format!(
                    "Moves {} to {} and updates git's records. Programs with files open in it may lose them.",
                    display(&path),
                    display(&to)
                ),
                "Move",
                false,
                vec![core::line(&args)],
                Act::MoveWorktreeConfirmed(path, to).into(),
            );
        }
        Act::MoveWorktreeConfirmed(path, to) => {
            repo.devops.workbench.worktrees.selected = Some(to.replace('\\', "/"));
            change(cx, repo, "Move worktree", core::move_args(&path, &to));
        }
        Act::PrunePreview => {
            let w = &mut repo.devops.workbench.worktrees;
            if w.prune_loading {
                return;
            }
            w.prune_loading = true;
            cx.read("Preview worktree prune", |dir, log| {
                let mut cmds = Vec::new();
                let r = git(dir, &core::prune_args(true), &mut cmds);
                log.commands.extend(cmds);
                Msg::PrunePreview(r)
            });
        }
        Act::Prune => {
            let preview = match &repo.devops.workbench.worktrees.prune {
                Some(Ok(t)) => core::parse_prune(t),
                _ => Vec::new(),
            };
            let mut details = vec![core::line(&core::prune_args(false))];
            if !preview.is_empty() {
                details.push(
                    preview
                        .iter()
                        .map(|(what, why)| format!("{what}: {why}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
            cx.confirm(
                "Prune worktree records?",
                format!(
                    "Removes git's records of {} worktree{} whose folders are gone. No files are deleted.",
                    preview.len(),
                    if preview.len() == 1 { "" } else { "s" }
                ),
                "Prune",
                false,
                details,
                Act::PruneConfirmed.into(),
            );
        }
        Act::PruneConfirmed => change(cx, repo, "Prune worktrees", core::prune_args(false)),
        _ => {}
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    let w = &mut repo.devops.workbench.worktrees;
    match msg {
        Msg::Worktrees(r) => {
            w.loading = false;
            w.at = Some(Instant::now());
            if let Ok(l) = &r {
                if let Some(n) = w.pending_select.take() {
                    w.selected = l.get(n).map(|e| e.wt.path.clone());
                }
                if let Some(sel) = &w.selected
                    && !l
                        .iter()
                        .any(|e| same_path(Path::new(&e.wt.path), Path::new(sel)))
                {
                    w.selected = None;
                } else if let Some(sel) = w.selected.clone() {
                    // Normalise to the listed spelling.
                    w.selected = l
                        .iter()
                        .find(|e| same_path(Path::new(&e.wt.path), Path::new(&sel)))
                        .map(|e| e.wt.path.clone());
                }
            }
            w.list = Some(r);
        }
        Msg::PrunePreview(r) => {
            w.prune_loading = false;
            w.prune = Some(r);
        }
        Msg::WorktreeChanged => {
            w.busy = false;
            w.prune = None;
            w.lock_reason.clear();
            w.move_to.clear();
            load(cx, repo);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

fn folder_name(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

fn badges(e: &Entry, current: bool) -> Vec<(String, Tone)> {
    let mut b = Vec::new();
    if current {
        b.push(("Open here".to_owned(), Tone::Accent));
    }
    if e.wt.main {
        b.push(("Main".to_owned(), Tone::Neutral));
    }
    if e.wt.bare {
        b.push(("Bare".to_owned(), Tone::Neutral));
    }
    if e.wt.detached {
        b.push(("Detached".to_owned(), Tone::Neutral));
    }
    if e.wt.locked.is_some() {
        b.push(("Locked".to_owned(), Tone::Warning));
    }
    if e.wt.prunable.is_some() {
        b.push(("Prunable".to_owned(), Tone::Danger));
    }
    if let Some(Ok(s)) = &e.status {
        if s.conflicted > 0 {
            b.push((
                format::plural(s.conflicted, "conflict", "conflicts"),
                Tone::Danger,
            ));
        } else if s.dirty() {
            b.push((
                format::plural(s.changes(), "change", "changes"),
                Tone::Warning,
            ));
        }
        match (s.ahead, s.behind) {
            (Some(a), Some(bh)) if a > 0 || bh > 0 => {
                b.push((format!("↑{a} ↓{bh}"), Tone::Neutral));
            }
            _ => {}
        }
    }
    b
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let now = format::now_secs();
    let workdir = repo.workdir.clone();
    let main = main_path(repo);
    let branches: Vec<String> = repo
        .snapshot
        .as_ref()
        .map(|s| s.branches.iter().map(|b| b.name.clone()).collect())
        .unwrap_or_default();
    let busy = repo.is_busy();
    let w = &mut repo.devops.workbench.worktrees;
    kit::page_header(ui, "Worktrees", super::Tab::Worktrees.hint(), |ui| {
        if IconButton::new(Icon::Rerun, "Refresh")
            .compact()
            .enabled(!w.loading)
            .show(ui)
            .on_hover_text("git worktree list --porcelain, then git status in each")
            .clicked()
        {
            actions.push(Act::LoadWorktrees.into());
        }
        if IconButton::new(Icon::Plus, "Add")
            .compact()
            .show(ui)
            .on_hover_text("Check out a branch or commit in another folder")
            .clicked()
        {
            actions.push(Act::SelectWorktree(None).into());
        }
        if let (Some(Ok(l)), Some(at)) = (&w.list, w.at) {
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!(
                    "{}  ·  {}",
                    format::plural(l.len(), "worktree", "worktrees"),
                    format::relative(now - at.elapsed().as_secs() as i64, now)
                ))
                .size(size::SMALL)
                .color(p.text_2),
            );
        }
    });
    let list = match &w.list {
        None => {
            wui::empty_pane(ui, |ui| kit::loading(ui, "Listing worktrees…"));
            return;
        }
        Some(Err(e)) => {
            let e = e.clone();
            wui::empty_pane(ui, |ui| {
                if wui::error_card(ui, "Could not list the worktrees", &e, "Try again") {
                    actions.push(Act::LoadWorktrees.into());
                }
            });
            return;
        }
        Some(Ok(l)) => l.clone(),
    };
    let selected = w.selected.clone();
    wui::list_pane(ui, "wb_wt_list", 380.0, |ui| {
        kit::list_header(ui, "Worktrees", Some(list.len()), |_| {});
        ui.add_space(6.0);
        kit::list(
            ui,
            "wb_wt_rows",
            list.len(),
            kit::ROW_H,
            |i| selected.as_deref() == Some(list[i].wt.path.as_str()),
            |ui, rect, resp, i| {
                let e = &list[i];
                let current = same_path(Path::new(&e.wt.path), &workdir);
                let b = badges(e, current);
                let branch = match (&e.wt.branch, &e.wt.head) {
                    (Some(br), _) => br.clone(),
                    (None, Some(h)) => format!("detached at {}", &h[..10.min(h.len())]),
                    (None, None) => "bare".to_owned(),
                };
                let sub = format!("{branch}  ·  {}", display(&e.wt.path));
                let meta = match &e.status {
                    Some(Ok(s)) if !s.dirty() => "clean".to_owned(),
                    Some(Ok(_)) => String::new(),
                    Some(Err(_)) => "unreadable".to_owned(),
                    None => String::new(),
                };
                let dot = if e.wt.prunable.is_some() {
                    p.danger
                } else if current {
                    p.accent
                } else {
                    p.text_3
                };
                kit::paint_row(
                    ui,
                    rect,
                    &Row::new(folder_name(&e.wt.path))
                        .leading(kit::Leading::Dot(dot))
                        .subtitle(&sub)
                        .meta(&meta)
                        .badges(&b),
                    selected.as_deref() == Some(e.wt.path.as_str()),
                );
                if resp.clicked() {
                    actions.push(Act::SelectWorktree(Some(e.wt.path.clone())).into());
                }
                if resp.double_clicked() && !current && !e.wt.bare {
                    actions.push(Action::OpenRepo(PathBuf::from(&e.wt.path)));
                }
                resp.context_menu(|ui| {
                    if !current && !e.wt.bare && ui.button("Open as a tab").clicked() {
                        actions.push(Action::OpenRepo(PathBuf::from(&e.wt.path)));
                        ui.close();
                    }
                    if ui.button("Open the folder").clicked() {
                        actions.push(Action::OpenPath(PathBuf::from(&e.wt.path)));
                        ui.close();
                    }
                    if ui.button("Copy path").clicked() {
                        actions.push(Action::Copy(display(&e.wt.path)));
                        ui.close();
                    }
                });
            },
        );
    });
    let w = &mut repo.devops.workbench.worktrees;
    match selected.and_then(|s| list.iter().find(|e| e.wt.path == s).cloned()) {
        Some(e) => wui::detail_pane(ui, ("wb_wt", &e.wt.path), |ui| {
            let current = same_path(Path::new(&e.wt.path), &workdir);
            detail(ui, w, &e, current, busy, now, actions);
        }),
        None => wui::detail_pane(ui, "wb_wt_overview", |ui| {
            overview(ui, w, &list, &main, &branches, busy, actions);
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn detail(
    ui: &mut Ui,
    w: &mut Worktrees,
    e: &Entry,
    current: bool,
    busy: bool,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    crate::views::insights::detail_title(ui, folder_name(&e.wt.path), &badges(e, current));
    ui.add_space(4.0);
    let path = display(&e.wt.path);
    kit::kv(ui, 120.0, |kv| {
        kv.row("Folder", |ui| wui::copyable(ui, &path, actions));
        match (&e.wt.branch, e.wt.bare) {
            (_, true) => kv.muted("Branch", "Bare repository: no working tree"),
            (Some(b), _) => kv.text("Branch", kit::mono(b)),
            (None, _) => kv.text("Branch", "Detached HEAD"),
        }
        if let Some(h) = &e.wt.head {
            let mut text = h[..12.min(h.len())].to_owned();
            if let Some(s) = &e.subject {
                text.push_str(&format!("  {s}"));
            }
            kv.text("HEAD", text);
            if let Some(t) = e.head_time {
                kv.text(
                    "Last commit",
                    format!("{}  ·  {}", format::absolute(t), format::relative(t, now)),
                );
            }
        }
        if let Some(Ok(s)) = &e.status {
            match &s.upstream {
                Some(u) => kv.text(
                    "Upstream",
                    format!(
                        "{u}  ·  {} ahead, {} behind",
                        s.ahead.unwrap_or(0),
                        s.behind.unwrap_or(0)
                    ),
                ),
                None => kv.muted("Upstream", "None"),
            }
            kv.text(
                "Changes",
                if s.dirty() {
                    format!(
                        "{} staged  ·  {} unstaged  ·  {} untracked  ·  {} conflicted",
                        s.staged, s.unstaged, s.untracked, s.conflicted
                    )
                } else {
                    "Clean".to_owned()
                },
            );
        }
        if let Some(Err(err)) = &e.status {
            kv.text("Status", RichText::new(err.as_str()).color(p.danger));
        }
        if let Some(reason) = &e.wt.locked {
            kv.text(
                "Locked",
                if reason.is_empty() {
                    "Yes (no reason given)".to_owned()
                } else {
                    reason.clone()
                },
            );
        }
        if let Some(reason) = &e.wt.prunable {
            kv.text(
                "Prunable",
                RichText::new(if reason.is_empty() {
                    "Its folder is gone".to_owned()
                } else {
                    reason.clone()
                })
                .color(p.danger),
            );
        }
    });
    wui::buttons(ui, |ui| {
        if !current
            && !e.wt.bare
            && e.wt.prunable.is_none()
            && IconButton::new(Icon::Plus, "Open as a tab")
                .primary(true)
                .show(ui)
                .clicked()
        {
            actions.push(Action::OpenRepo(PathBuf::from(&e.wt.path)));
        }
        if e.wt.prunable.is_none()
            && IconButton::new(Icon::Folder, "Open the folder")
                .show(ui)
                .clicked()
        {
            actions.push(Action::OpenPath(PathBuf::from(&e.wt.path)));
        }
        if IconButton::new(Icon::Copy, "Copy path").show(ui).clicked() {
            actions.push(Action::Copy(path.clone()));
        }
    });
    if let Some(Ok(s)) = &e.status
        && s.dirty()
    {
        kit::section(ui, "Uncommitted changes", Some(s.changes()));
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            for (path, code) in s.files.iter().take(60) {
                ui.horizontal(|ui| {
                    ui.label(kit::mono(code).color(p.text_3));
                    ui.label(kit::mono(path).color(p.text));
                });
            }
        });
        let more = s.changes().saturating_sub(60);
        if more > 0 {
            kit::muted(ui, &format!("and {more} more"));
        }
    }
    if e.wt.bare {
        return;
    }
    let enabled = !w.busy && !busy;
    kit::section(ui, "Lock", None);
    match &e.wt.locked {
        Some(_) => {
            kit::secondary(
                ui,
                "git will not prune, move or remove it until it is unlocked.",
            );
            if form::button(ui, "Unlock", enabled).clicked() {
                actions.push(Act::UnlockWorktree(e.wt.path.clone()).into());
            }
        }
        None => {
            kit::secondary(
                ui,
                "Keep git from pruning it while its folder is unavailable (a removable disk, a network share).",
            );
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                form::input(
                    ui,
                    ("wb_wt_lock", &e.wt.path),
                    &mut w.lock_reason,
                    InputOpts::new("Reason (optional)").width(320.0),
                );
                if form::button(ui, "Lock", enabled).clicked() {
                    actions.push(Act::LockWorktree(e.wt.path.clone()).into());
                }
            });
        }
    }
    if e.wt.main {
        ui.add_space(4.0);
        kit::muted(
            ui,
            "This is the main worktree: git cannot move or remove it.",
        );
        return;
    }
    kit::section(ui, "Move", None);
    if e.wt.locked.is_some() {
        kit::secondary(ui, "Unlock it first: git does not move a locked worktree.");
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let w_in = (ui.available_width() - 90.0).max(240.0);
        form::input(
            ui,
            ("wb_wt_move", &e.wt.path),
            &mut w.move_to,
            InputOpts::new("New folder").mono().width(w_in),
        );
        if form::button(
            ui,
            "Move",
            enabled && e.wt.locked.is_none() && !w.move_to.trim().is_empty() && !current,
        )
        .clicked()
        {
            actions.push(Act::MoveWorktree(e.wt.path.clone()).into());
        }
    });
    if current {
        kit::muted(
            ui,
            "Open in this tab: switch to another worktree to move or remove this one.",
        );
        return;
    }
    kit::section(ui, "Remove", None);
    kit::secondary(
        ui,
        "Deletes the folder and git's record of it; the branch stays. Uncommitted changes are listed in the confirmation.",
    );
    if form::danger(ui, "Remove worktree…", enabled).clicked() {
        actions.push(Act::RemoveWorktree(e.wt.path.clone()).into());
    }
}

fn overview(
    ui: &mut Ui,
    w: &mut Worktrees,
    list: &[Entry],
    main: &Path,
    branches: &[String],
    busy: bool,
    actions: &mut Vec<Action>,
) {
    let dirty = list
        .iter()
        .filter(|e| matches!(&e.status, Some(Ok(s)) if s.dirty()))
        .count();
    let locked = list.iter().filter(|e| e.wt.locked.is_some()).count();
    let prunable = list.iter().filter(|e| e.wt.prunable.is_some()).count();
    let detached = list.iter().filter(|e| e.wt.detached).count();
    kit::title(ui, "Worktrees of this repository");
    kit::secondary(
        ui,
        "Each worktree is a folder with its own checkout; they share one history, so a branch can only be checked out in one of them at a time. Select one for its details and actions, double-click to open it as a tab.",
    );
    ui.add_space(4.0);
    crate::views::insights::stats(
        ui,
        &[
            Stat::new("Worktrees", format::count(list.len())),
            Stat::new("With changes", format::count(dirty)).tone_if(dirty > 0, Tone::Warning),
            Stat::new("Locked", format::count(locked)),
            Stat::new("Prunable", format::count(prunable)).tone_if(prunable > 0, Tone::Danger),
            Stat::new("Detached", format::count(detached)),
        ],
    );

    kit::section(ui, "Add a worktree", None);
    let used: Vec<&str> = list.iter().filter_map(|e| e.wt.branch.as_deref()).collect();
    let free: Vec<String> = branches
        .iter()
        .filter(|b| !used.contains(&b.as_str()))
        .cloned()
        .collect();
    form::card(ui, Tone::Neutral, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            for (m, label) in [
                (Mode::NewBranch, "New branch"),
                (Mode::Existing, "Existing branch"),
                (Mode::Detached, "Detached at a commit"),
            ] {
                if form::radio(ui, w.mode == m, label) {
                    w.mode = m;
                }
            }
        });
        ui.add_space(6.0);
        let branch_for_path = match w.mode {
            Mode::NewBranch => {
                form::field(ui, "Branch name", None, |ui| {
                    form::input(
                        ui,
                        "wb_wt_new_branch",
                        &mut w.new_branch,
                        InputOpts::new("feature/login").mono().width(360.0),
                    );
                });
                form::field(
                    ui,
                    "Start point",
                    Some("A branch, tag or commit. Default: HEAD."),
                    |ui| {
                        form::input(
                            ui,
                            "wb_wt_start",
                            &mut w.start,
                            InputOpts::new("HEAD").mono().width(360.0),
                        );
                    },
                );
                w.new_branch.clone()
            }
            Mode::Existing => {
                form::field(
                    ui,
                    "Branch",
                    Some("Local branches that no worktree has checked out."),
                    |ui| {
                        if free.is_empty() {
                            kit::muted(ui, "Every local branch is checked out somewhere already.");
                        } else {
                            if !free.contains(&w.existing) {
                                w.existing = free[0].clone();
                            }
                            let opts: Vec<(String, String)> =
                                free.iter().map(|b| (b.clone(), b.clone())).collect();
                            form::select(ui, "wb_wt_existing", &mut w.existing, &opts, 360.0);
                        }
                    },
                );
                w.existing.clone()
            }
            Mode::Detached => {
                form::field(
                    ui,
                    "Commit",
                    Some("A commit, tag or branch. Default: HEAD."),
                    |ui| {
                        form::input(
                            ui,
                            "wb_wt_commit",
                            &mut w.commit,
                            InputOpts::new("HEAD or v1.2.0").mono().width(360.0),
                        );
                    },
                );
                let c = w.commit.trim();
                if c.is_empty() {
                    "detached".to_owned()
                } else {
                    c.to_owned()
                }
            }
        };
        // Follow the branch name until the path is typed over.
        let auto = display(&default_path(main, &branch_for_path).to_string_lossy());
        if w.path.trim().is_empty() || w.path == w.path_auto {
            w.path = auto.clone();
            w.path_auto = auto;
        }
        form::field(
            ui,
            "Folder",
            Some("Default: next to the main worktree, named <repository>-<branch>."),
            |ui| {
                form::input(
                    ui,
                    "wb_wt_path",
                    &mut w.path,
                    InputOpts::new("Folder").mono(),
                );
            },
        );
        form::actions_row(ui, |ui| {
            if form::primary(ui, "Add worktree", !w.busy && !busy).clicked() {
                actions.push(Act::AddWorktree.into());
            }
        });
    });

    kit::section(ui, "Prune", None);
    kit::secondary(
        ui,
        "git keeps a record of every worktree; when a folder is deleted by hand the record stays. Prune removes those records (never files).",
    );
    match &w.prune {
        None => {
            if w.prune_loading {
                kit::loading(ui, "git worktree prune --dry-run -v");
            } else if form::button(ui, "Preview", true).clicked() {
                actions.push(Act::PrunePreview.into());
            }
        }
        Some(Err(e)) => {
            let e = e.clone();
            if wui::error_card(ui, "The preview failed", &e, "Try again") {
                actions.push(Act::PrunePreview.into());
            }
        }
        Some(Ok(text)) => {
            let items = core::parse_prune(text);
            if items.is_empty() {
                kit::callout(
                    ui,
                    Tone::Success,
                    "Nothing to prune",
                    "Every recorded worktree still has its folder.",
                );
                if form::button(ui, "Preview again", true).clicked() {
                    actions.push(Act::PrunePreview.into());
                }
            } else {
                for (what, why) in &items {
                    ui.horizontal(|ui| {
                        ui.label(kit::mono(what).color(pal(ui).text));
                        kit::muted(ui, why);
                    });
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
                    if form::primary(ui, &format!("Prune {}", items.len()), !w.busy && !busy)
                        .clicked()
                    {
                        actions.push(Act::Prune.into());
                    }
                    if form::button(ui, "Preview again", true).clicked() {
                        actions.push(Act::PrunePreview.into());
                    }
                });
            }
        }
    }
}
