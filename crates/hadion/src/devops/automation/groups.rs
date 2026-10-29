//! Repository groups on the Home page: named lists of repositories (in
//! `Settings::ext.automation.groups`), a status table (open repositories
//! from their snapshot, the others from a quick `git status --porcelain=v2
//! --branch` in the background), "Open all as tabs", and batch operations
//! confirmed with every repository and command, then run one repository
//! after another with a result each.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Align2, FontId, Rect, RichText, Sense, Ui, pos2, vec2};

use hadion_core::devops::automation::groups::{
    self as core, Batch, Group, Outcome, RepoStatus, same_path,
};

use super::shared::{self, BatchDone, BatchRun, GroupsRt, Progress, StatusEntry};
use super::{Act, Msg};
use crate::devops::ctx::Ctx;
use crate::devops::global::GlobalCx;
use crate::format;
use crate::state::{Action, Page, RepoState, Settings, ToastKind};
use crate::theme::{Tone, medium, pal, semibold, size, space};
use crate::views::form;
use crate::views::icons::{self, Icon, IconButton};
use crate::views::kit;

/// How long one repository's command may take.
const TIMEOUT: Duration = Duration::from_secs(180);
/// Status of repositories that are not open is read again after this.
const STATUS_STALE: Duration = Duration::from_secs(45);

/// The folder name of a path (`C:/src/web/` → `web`).
pub fn dir_name(p: &Path) -> String {
    p.components()
        .rev()
        .find_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .unwrap_or_else(|| p.display().to_string())
}

fn open_index(repos: &[RepoState], dir: &Path) -> Option<usize> {
    repos.iter().position(|r| same_path(&r.workdir, dir))
}

fn group_mut<'a>(g: &'a mut GlobalCx, name: &str) -> Option<&'a mut Group> {
    g.settings
        .ext
        .automation
        .groups
        .iter_mut()
        .find(|x| x.name.eq_ignore_ascii_case(name.trim()))
}

pub fn apply(cx: &mut Ctx, g: &mut GlobalCx, act: Act) {
    match act {
        Act::GroupNew(name) => {
            let name = name.trim().to_owned();
            if let Some(p) = core::name_problem(&g.settings.ext.automation.groups, &name, None) {
                cx.toast(ToastKind::Error, format!("Group \"{name}\": {p}"));
                return;
            }
            g.settings
                .ext
                .automation
                .groups
                .push(Group::new(name.clone()));
            let mut s = shared::lock();
            s.groups.selected = Some(name);
            s.groups.creating = false;
            s.groups.new_name.clear();
        }
        Act::GroupSelect(name) => {
            let s = &mut shared::lock().groups;
            s.selected = Some(name);
            s.rename = None;
            s.branch_form = None;
        }
        Act::GroupRename { from, to } => {
            let to = to.trim().to_owned();
            if let Some(p) = core::name_problem(&g.settings.ext.automation.groups, &to, Some(&from))
            {
                cx.toast(ToastKind::Error, format!("Group \"{to}\": {p}"));
                return;
            }
            if let Some(gr) = group_mut(g, &from) {
                gr.name = to.clone();
            }
            let mut s = shared::lock();
            s.groups.rename = None;
            s.groups.selected = Some(to);
        }
        Act::GroupDelete(name) => {
            let n = core::find(&g.settings.ext.automation.groups, &name)
                .map(|x| x.repos.len())
                .unwrap_or(0);
            cx.confirm(
                format!("Delete the group {name}?"),
                format!(
                    "Removes the group and its list of {}. The repositories themselves are not touched.",
                    format::plural(n, "repository", "repositories")
                ),
                "Delete group",
                true,
                Vec::new(),
                Act::GroupDeleteConfirmed(name).into(),
            );
        }
        Act::GroupDeleteConfirmed(name) => {
            g.settings
                .ext
                .automation
                .groups
                .retain(|x| !x.name.eq_ignore_ascii_case(&name));
            let mut s = shared::lock();
            if s.groups
                .selected
                .as_deref()
                .is_some_and(|x| x.eq_ignore_ascii_case(&name))
            {
                s.groups.selected = None;
            }
        }
        Act::GroupAdd { group, path } => {
            let dir = match hadion_core::repo::locate(&path) {
                Ok((w, _)) => w,
                Err(e) => {
                    let msg = if path.exists() {
                        format!("Not a git repository: {} ({e})", path.display())
                    } else {
                        format!("Folder not found: {}", path.display())
                    };
                    cx.toast(ToastKind::Error, msg);
                    return;
                }
            };
            if core::find(&g.settings.ext.automation.groups, &group).is_none() {
                if let Some(p) = core::name_problem(&g.settings.ext.automation.groups, &group, None)
                {
                    cx.toast(ToastKind::Error, format!("Group \"{group}\": {p}"));
                    return;
                }
                g.settings
                    .ext
                    .automation
                    .groups
                    .push(Group::new(group.trim()));
            }
            let added = group_mut(g, &group).is_some_and(|gr| gr.add(dir.clone()));
            if !added {
                cx.toast(
                    ToastKind::Info,
                    format!("{} is in {group} already", dir_name(&dir)),
                );
            }
            let mut s = shared::lock();
            s.groups.selected = Some(group.trim().to_owned());
            s.groups.status_at = None;
        }
        Act::GroupRemove { group, path } => {
            if let Some(gr) = group_mut(g, &group) {
                gr.repos.retain(|r| !same_path(r, &path));
            }
        }
        Act::GroupPickFolder(group) => {
            let mut d = rfd::FileDialog::new().set_title(format!("Add a repository to {group}"));
            if let Some(start) = g
                .active_repo()
                .and_then(|r| r.workdir.parent().map(Path::to_path_buf))
            {
                d = d.set_directory(start);
            }
            if let Some(path) = d.pick_folder() {
                apply(cx, g, Act::GroupAdd { group, path });
            }
        }
        Act::GroupOpenAll(group) => {
            let Some(gr) = core::find(&g.settings.ext.automation.groups, &group).cloned() else {
                return;
            };
            let mut missing = Vec::new();
            let mut opened = 0;
            for p in &gr.repos {
                if p.exists() {
                    cx.action(Action::OpenRepo(p.clone()));
                    opened += 1;
                } else {
                    missing.push(dir_name(p));
                }
            }
            if opened > 0 {
                cx.action(Action::ShowPage(Page::Home));
            }
            if !missing.is_empty() {
                cx.toast(
                    ToastKind::Error,
                    format!("Not found: {}", missing.join(", ")),
                );
            }
        }
        Act::GroupStatus => read_status(cx, g),
        Act::GroupBatch { group, batch } => {
            let Some((list, skipped)) = batch_list(cx, g, &group, &batch) else {
                return;
            };
            let mut details = vec![format!(
                "In each repository, one after another ({}):",
                if batch.remote() {
                    "talks to the remotes"
                } else {
                    "local only"
                }
            )];
            for c in batch.checks() {
                details.push(format!("  {c}"));
            }
            details.push(String::new());
            for (name, dir) in &list {
                details.push(format!("{name}  ({})", dir.display()));
                details.push(format!("  {}", batch.command_line()));
            }
            if !skipped.is_empty() {
                details.push(String::new());
                details.push(format!("Not included: {}", skipped.join(", ")));
            }
            let (title, body, confirm) = match &batch {
                Batch::Fetch => (
                    format!("Fetch {} in {group}?", format::plural(list.len(), "repository", "repositories")),
                    "Downloads new commits, branches and tags from every remote and removes remote-tracking branches deleted there (--prune). Local branches and working trees are not touched.".to_owned(),
                    "Fetch all",
                ),
                Batch::Pull => (
                    format!("Pull {} in {group}?", format::plural(list.len(), "repository", "repositories")),
                    "Fast-forwards each current branch to its upstream (git pull --ff-only). Repositories with uncommitted changes, without an upstream or diverged from it are skipped and listed as such; nothing is merged or rebased.".to_owned(),
                    "Pull all",
                ),
                Batch::Branch { name, base } => (
                    format!("Create {name} in {}?", format::plural(list.len(), "repository", "repositories")),
                    match base.as_ref().filter(|b| !b.trim().is_empty()) {
                        Some(b) => format!("Creates and switches to {name} from {} in each repository (git switch -c). Repositories where it exists already, where {} is unknown, or with uncommitted changes are skipped.", b.trim(), b.trim()),
                        None => format!("Creates and switches to {name} at the current commit in each repository (git switch -c); uncommitted changes stay with you. Repositories where it exists already are skipped."),
                    },
                    "Create branches",
                ),
            };
            cx.confirm(
                title,
                body,
                confirm,
                false,
                details,
                Act::GroupBatchConfirmed { group, batch, list }.into(),
            );
        }
        Act::GroupBatchConfirmed { group, batch, list } => start(cx, group, batch, list),
        Act::GroupBatchNow { group, batch } => {
            if let Some((list, _)) = batch_list(cx, g, &group, &batch) {
                start(cx, group, batch, list);
            }
        }
        Act::ClearBatch => shared::lock().groups.last = None,
        _ => {}
    }
}

/// `(name, dir)` of each repository to run in, and the ones left out.
type RunList = (Vec<(String, PathBuf)>, Vec<String>);

/// `(name, dir)` to run in and what is left out (busy repositories), or
/// `None` after a toast saying why nothing can run.
fn batch_list(cx: &mut Ctx, g: &GlobalCx, group: &str, batch: &Batch) -> Option<RunList> {
    if shared::lock().groups.batch.is_some() {
        cx.toast(ToastKind::Info, "A group operation is running already");
        return None;
    }
    let Some(gr) = core::find(&g.settings.ext.automation.groups, group) else {
        cx.toast(ToastKind::Error, format!("No group named {group}"));
        return None;
    };
    if let Batch::Branch { name, .. } = batch
        && let Some(p) = core::branch_name_problem(name)
    {
        cx.toast(ToastKind::Error, format!("Branch name: {p}"));
        return None;
    }
    let mut list = Vec::new();
    let mut skipped = Vec::new();
    for p in &gr.repos {
        let open = open_index(g.repos, p).map(|i| &g.repos[i]);
        let name = open.map(|r| r.name.clone()).unwrap_or_else(|| dir_name(p));
        if open.is_some_and(|r| r.is_busy()) {
            skipped.push(format!("{name} (busy in Hadion)"));
        } else {
            list.push((name, p.clone()));
        }
    }
    if list.is_empty() {
        cx.toast(
            ToastKind::Info,
            if gr.repos.is_empty() {
                format!("{group} has no repositories yet")
            } else {
                format!("Nothing to run: {}", skipped.join(", "))
            },
        );
        return None;
    }
    Some((list, skipped))
}

fn start(cx: &mut Ctx, group: String, batch: Batch, list: Vec<(String, PathBuf)>) {
    let progress = Arc::new(Mutex::new(Progress::default()));
    {
        let mut s = shared::lock();
        if s.groups.batch.is_some() {
            return;
        }
        s.groups.batch = Some(BatchRun {
            group: group.clone(),
            batch: batch.clone(),
            names: list.iter().map(|(n, _)| n.clone()).collect(),
            started: Instant::now(),
            progress: progress.clone(),
        });
        s.groups.branch_form = None;
    }
    let label = format!(
        "{} in {} ({})",
        batch.label(),
        group,
        format::plural(list.len(), "repository", "repositories")
    );
    cx.remote_change(label, move |_dir, log| {
        let started = Instant::now();
        let mut results = Vec::new();
        for (name, dir) in &list {
            if let Ok(mut p) = progress.lock() {
                p.current = Some(name.clone());
            }
            let r = core::run_one(&batch, name, dir, TIMEOUT, &mut log.commands);
            if let Ok(mut p) = progress.lock() {
                p.done.push(r.clone());
            }
            results.push(r);
        }
        if let Ok(mut p) = progress.lock() {
            p.current = None;
        }
        let mut text = core::summary(&batch, &results);
        for r in &results {
            text.push_str(&format!(
                "\n{}: {} · {} · {}",
                r.name,
                r.outcome.label(),
                r.outcome.text(),
                format::millis(r.elapsed_ms)
            ));
        }
        let failed = results
            .iter()
            .any(|r| matches!(r.outcome, Outcome::Failed(_)));
        (
            if failed { Err(text) } else { Ok(text) },
            Some(Msg::BatchDone {
                group,
                batch,
                results,
                elapsed_ms: started.elapsed().as_millis() as u64,
            }),
        )
    });
}

/// Read the status of every grouped repository that is not open.
fn read_status(cx: &mut Ctx, g: &GlobalCx) {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for gr in &g.settings.ext.automation.groups {
        for p in &gr.repos {
            if open_index(g.repos, p).is_none() && !dirs.iter().any(|d| same_path(d, p)) {
                dirs.push(p.clone());
            }
        }
    }
    {
        let mut s = shared::lock();
        if s.groups.status_loading {
            return;
        }
        s.groups.status_at = Some(Instant::now());
        if dirs.is_empty() {
            return;
        }
        s.groups.status_loading = true;
    }
    let label = format!(
        "Read the status of {} in groups",
        format::plural(dirs.len(), "repository", "repositories")
    );
    let ran = cx.read(label, move |_wd, log| {
        let out = dirs
            .into_iter()
            .map(|d| {
                let r = core::read_status(&d, &mut log.commands).map_err(|e| e.to_string());
                (d, r)
            })
            .collect();
        Msg::Status(out)
    });
    if !ran {
        shared::lock().groups.status_loading = false;
    }
}

pub fn on_msg(cx: &mut Ctx, _g: &mut GlobalCx, msg: Msg) {
    match msg {
        Msg::Status(v) => {
            let mut s = shared::lock();
            s.groups.status_loading = false;
            s.groups.status_at = Some(Instant::now());
            for (d, r) in v {
                s.groups.status.insert(
                    d,
                    StatusEntry {
                        result: r,
                        at: Instant::now(),
                    },
                );
            }
        }
        Msg::BatchDone {
            group,
            batch,
            results,
            elapsed_ms,
        } => {
            {
                let mut s = shared::lock();
                s.groups.batch = None;
                s.groups.last = Some(BatchDone {
                    group,
                    batch,
                    results,
                    elapsed_ms,
                    at: format::now_secs(),
                });
                s.groups.status_at = None;
            }
            cx.action(Act::GroupStatus);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// The Home page section
// ---------------------------------------------------------------------------

/// One row of the status table.
struct Row {
    name: String,
    path: String,
    open: Option<usize>,
    dir: PathBuf,
    /// `None` while reading; `Err` when unreadable.
    status: Option<Result<RepoStatus, String>>,
    /// "open in Hadion", "read 12 s ago".
    source: String,
}

fn status_of_open(r: &RepoState) -> Option<RepoStatus> {
    let snap = r.snapshot.as_ref()?;
    let b = snap.current_branch();
    let mut s = RepoStatus {
        branch: snap.head.branch().map(str::to_owned),
        head: snap.head.target().map(hadion_core::repo::short_id),
        upstream: b.and_then(|b| b.upstream.clone()),
        ahead: b.map(|b| b.ahead as u32).unwrap_or(0),
        behind: b.map(|b| b.behind as u32).unwrap_or(0),
        ..Default::default()
    };
    for e in &snap.status.staged {
        let _ = e;
        s.staged += 1;
    }
    for e in &snap.status.unstaged {
        if e.kind == hadion_core::ChangeKind::Untracked {
            s.untracked += 1;
        } else {
            s.unstaged += 1;
        }
    }
    s.conflicted = snap.status.conflicted.len();
    s.last_commit = snap
        .head
        .target()
        .and_then(|t| snap.index_of.get(&t))
        .and_then(|i| snap.commits.get(*i))
        .map(|c| c.time);
    Some(s)
}

fn rows(gr: &Group, repos: &[RepoState], rt: &GroupsRt) -> Vec<Row> {
    gr.repos
        .iter()
        .map(|p| {
            let open = open_index(repos, p);
            let (name, status, source) = match open {
                Some(i) => {
                    let r = &repos[i];
                    (
                        r.name.clone(),
                        match status_of_open(r) {
                            Some(s) => Some(Ok(s)),
                            None if r.load_error.is_some() => {
                                Some(Err(r.load_error.clone().unwrap_or_default()))
                            }
                            None => None,
                        },
                        "open in Hadion".to_owned(),
                    )
                }
                None => {
                    let e = rt
                        .status
                        .iter()
                        .find(|(d, _)| same_path(d, p))
                        .map(|(_, e)| e.clone());
                    let source = match &e {
                        Some(e) => format!("read {} ago", short_age(e.at.elapsed())),
                        None => "not open".to_owned(),
                    };
                    (dir_name(p), e.map(|e| e.result), source)
                }
            };
            Row {
                name,
                path: p.display().to_string(),
                open,
                dir: p.clone(),
                status,
                source,
            }
        })
        .collect()
}

fn short_age(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s / 60)
    } else {
        format!("{} h", s / 3600)
    }
}

/// The Groups section of the Home page.
pub fn page(ui: &mut Ui, repos: &[RepoState], settings: &Settings, actions: &mut Vec<Action>) {
    let groups = &settings.ext.automation.groups;
    let mut guard = shared::lock();
    let scroll_end = std::mem::take(&mut guard.scroll_end);
    let rt = &mut guard.groups;
    // Keep the selection valid.
    if rt
        .selected
        .as_deref()
        .is_none_or(|s| core::find(groups, s).is_none())
    {
        rt.selected = groups.first().map(|g| g.name.clone());
    }
    // Status of repositories that are not open: read when missing or stale.
    let needs_status = groups
        .iter()
        .flat_map(|g| g.repos.iter())
        .any(|p| open_index(repos, p).is_none());
    let unread = groups
        .iter()
        .flat_map(|g| g.repos.iter())
        .any(|p| open_index(repos, p).is_none() && !rt.status.keys().any(|d| same_path(d, p)));
    if needs_status
        && !rt.status_loading
        && (unread || rt.status_at.is_none_or(|t| t.elapsed() > STATUS_STALE))
    {
        actions.push(Act::GroupStatus.into());
    }
    let total: usize = groups.iter().map(|g| g.repos.len()).sum();
    // Short: the "What groups do" note under it explains the rest (the old
    // sentence repeated it and ran under the buttons on a narrow window).
    let parts: Vec<String> = if groups.is_empty() {
        vec!["Repositories you work on together".to_owned()]
    } else {
        vec![
            format!(
                "{}, {}",
                format::plural(groups.len(), "group", "groups"),
                format::plural(total, "repository", "repositories")
            ),
            "saved with your settings".to_owned(),
        ]
    };
    // "New group", "Refresh" and their gaps.
    let (subtitle, _) = crate::views::github::fit_subtitle(ui, "Groups", &parts, 230.0);
    kit::page_header(ui, "Groups", &subtitle, |ui| {
        if IconButton::new(Icon::Plus, "New group")
            .primary(groups.is_empty())
            .compact()
            .show(ui)
            .clicked()
        {
            rt.creating = true;
        }
        if rt.status_loading {
            ui.add(egui::Spinner::new().size(14.0));
        } else if needs_status
            && IconButton::new(Icon::Rerun, "Refresh")
                .compact()
                .show(ui)
                .on_hover_text("Read the status of grouped repositories that are not open (git status --porcelain=v2 --branch, git log -1)")
                .clicked()
        {
            actions.push(Act::GroupStatus.into());
        }
    });
    if rt.batch.is_some() {
        ui.ctx().request_repaint_after(Duration::from_millis(250));
    }
    body(ui, scroll_end, |ui| {
        if rt.creating || groups.is_empty() {
            new_group_form(ui, rt, groups, actions);
        }
        if groups.is_empty() {
            ui.add_space(space::L);
            kit::callout(
                ui,
                Tone::Neutral,
                "What groups do",
                "Open every repository of a group as tabs in one click, see their branches, sync state and uncommitted changes in one table, and run fetch, pull or create-a-branch in all of them, each confirmed with the full list of repositories and commands.",
            );
            return;
        }
        // Group chips.
        ui.add_space(space::XS);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for gr in groups {
                let on = rt.selected.as_deref() == Some(gr.name.as_str());
                if kit::chip(ui, &format!("{} · {}", gr.name, gr.repos.len()), on).clicked() && !on
                {
                    actions.push(Act::GroupSelect(gr.name.clone()).into());
                }
            }
        });
        let Some(gr) = rt
            .selected
            .as_deref()
            .and_then(|s| core::find(groups, s))
            .cloned()
        else {
            return;
        };
        ui.add_space(space::M);
        group_view(ui, &gr, repos, settings, rt, actions);
    });
}

fn body(ui: &mut Ui, scroll_end: bool, add: impl FnOnce(&mut Ui)) {
    egui::ScrollArea::vertical()
        .id_salt("automation_groups_body")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::NONE
                .inner_margin(egui::Margin {
                    left: 24,
                    right: 24,
                    top: 16,
                    bottom: 28,
                })
                .show(ui, |ui| {
                    ui.set_max_width(ui.available_width().min(kit::DETAIL_MAX + 240.0));
                    ui.spacing_mut().item_spacing.y = 8.0;
                    add(ui);
                    if scroll_end {
                        ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
                    }
                });
        });
}

fn new_group_form(ui: &mut Ui, rt: &mut GroupsRt, groups: &[Group], actions: &mut Vec<Action>) {
    form::card(ui, Tone::Neutral, |ui| {
        kit::subtitle(ui, "New group");
        ui.add_space(4.0);
        let problem = core::name_problem(groups, &rt.new_name, None);
        let mut submit = false;
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 200.0).max(160.0);
            let resp = form::input(
                ui,
                "automation_group_new",
                &mut rt.new_name,
                form::InputOpts::new("Name, e.g. Work or Client A").width(w),
            );
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                submit = true;
            }
            if form::primary(ui, "Create", problem.is_none()).clicked() {
                submit = true;
            }
            if !groups.is_empty() && form::button(ui, "Cancel", true).clicked() {
                rt.creating = false;
                rt.new_name.clear();
            }
        });
        if !rt.new_name.trim().is_empty()
            && let Some(p) = problem
        {
            form::error(ui, p);
        }
        if submit && problem.is_none() {
            actions.push(Act::GroupNew(rt.new_name.clone()).into());
        }
    });
}

fn group_view(
    ui: &mut Ui,
    gr: &Group,
    repos: &[RepoState],
    settings: &Settings,
    rt: &mut GroupsRt,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let running = rt.batch.is_some();
    let empty = gr.repos.is_empty();
    // Title (or rename) and actions.
    ui.horizontal(
        |ui| match rt.rename.as_mut().filter(|(old, _)| *old == gr.name) {
            Some((old, buf)) => {
                let resp = form::input(
                    ui,
                    "automation_group_rename",
                    buf,
                    form::InputOpts::new("Group name").width(260.0),
                );
                let problem = core::name_problem(&settings.ext.automation.groups, buf, Some(old));
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (form::primary(ui, "Rename", problem.is_none()).clicked() || enter)
                    && problem.is_none()
                {
                    actions.push(
                        Act::GroupRename {
                            from: old.clone(),
                            to: buf.clone(),
                        }
                        .into(),
                    );
                }
                if form::button(ui, "Cancel", true).clicked() {
                    rt.rename = None;
                    return;
                }
                if let Some(pr) = problem {
                    kit::muted(ui, pr);
                }
            }
            None => {
                ui.label(
                    RichText::new(&gr.name)
                        .size(size::TITLE)
                        .family(semibold())
                        .color(p.text_strong),
                );
                ui.label(
                    RichText::new(format::plural(gr.repos.len(), "repository", "repositories"))
                        .size(size::SMALL)
                        .color(p.text_3),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if IconButton::icon(Icon::Trash)
                        .compact()
                        .show(ui)
                        .on_hover_text("Delete this group (the repositories stay)")
                        .clicked()
                    {
                        actions.push(Act::GroupDelete(gr.name.clone()).into());
                    }
                    if IconButton::icon(Icon::Edit)
                        .compact()
                        .show(ui)
                        .on_hover_text("Rename")
                        .clicked()
                    {
                        rt.rename = Some((gr.name.clone(), gr.name.clone()));
                    }
                });
            }
        },
    );
    ui.add_space(space::XS);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        if IconButton::new(Icon::Folder, "Open all as tabs")
            .enabled(!empty)
            .show(ui)
            .on_hover_text("Open every repository of the group as a tab (those already open stay as they are)")
            .clicked()
        {
            actions.push(Act::GroupOpenAll(gr.name.clone()).into());
        }
        if IconButton::new(Icon::Fetch, "Fetch all")
            .enabled(!empty && !running)
            .show(ui)
            .on_hover_text("git fetch --all --prune in each repository (asks first)")
            .clicked()
        {
            actions.push(
                Act::GroupBatch {
                    group: gr.name.clone(),
                    batch: Batch::Fetch,
                }
                .into(),
            );
        }
        if IconButton::new(Icon::Pull, "Pull all")
            .enabled(!empty && !running)
            .show(ui)
            .on_hover_text("git pull --ff-only in each repository; dirty or diverged ones are skipped (asks first)")
            .clicked()
        {
            actions.push(
                Act::GroupBatch {
                    group: gr.name.clone(),
                    batch: Batch::Pull,
                }
                .into(),
            );
        }
        if IconButton::new(Icon::Branch, "Create a branch in all…")
            .enabled(!empty && !running)
            .selected(rt.branch_form.is_some())
            .show(ui)
            .clicked()
        {
            rt.branch_form = match rt.branch_form {
                Some(_) => None,
                None => Some((String::new(), String::new())),
            };
        }
    });
    if let Some((name, base)) = rt.branch_form.as_mut() {
        ui.add_space(space::XS);
        form::card(ui, Tone::Neutral, |ui| {
            let problem = core::branch_name_problem(name);
            ui.horizontal(|ui| {
                let w = ((ui.available_width() - 150.0) / 2.0).max(140.0);
                form::input(
                    ui,
                    "automation_group_branch",
                    name,
                    form::InputOpts::new("New branch, e.g. release/1.4")
                        .width(w)
                        .mono(),
                );
                form::input(
                    ui,
                    "automation_group_branch_base",
                    base,
                    form::InputOpts::new("From (optional): origin/main")
                        .width(w)
                        .mono(),
                );
                if form::primary(ui, "Review…", problem.is_none()).clicked() {
                    actions.push(
                        Act::GroupBatch {
                            group: gr.name.clone(),
                            batch: Batch::Branch {
                                name: name.trim().to_owned(),
                                base: Some(base.trim().to_owned()).filter(|b| !b.is_empty()),
                            },
                        }
                        .into(),
                    );
                }
            });
            match problem {
                Some(pr) if !name.trim().is_empty() => form::error(ui, pr),
                _ => {
                    kit::muted(
                        ui,
                        "Runs git switch -c NAME [FROM] in each repository. Empty FROM: at the current commit, keeping uncommitted changes.",
                    );
                }
            }
        });
    }

    // Running batch or the last result for this group.
    if let Some(b) = rt.batch.as_ref().filter(|b| b.group == gr.name) {
        ui.add_space(space::S);
        batch_progress(ui, b);
    } else if let Some(last) = rt.last.as_ref().filter(|l| l.group == gr.name) {
        ui.add_space(space::S);
        batch_results(ui, last, actions);
    }

    // Status table.
    kit::section(ui, "Status", Some(gr.repos.len()));
    if empty {
        kit::muted(
            ui,
            "No repositories yet. Add the ones open in Hadion or recently opened from the list below, or choose a folder.",
        );
    } else {
        let rows = rows(gr, repos, rt);
        status_table(ui, &gr.name, &rows, rt.status_loading, actions);
    }

    // Add repositories.
    ui.add_space(space::S);
    ui.horizontal(|ui| {
        let mut options: Vec<(String, String)> = vec![(
            String::new(),
            "Add an open or recent repository…".to_owned(),
        )];
        let mut seen: Vec<PathBuf> = Vec::new();
        for r in repos {
            if !gr.repos.iter().any(|p| same_path(p, &r.workdir)) {
                options.push((
                    r.workdir.display().to_string(),
                    format!("{}  (open)  {}", r.name, r.workdir.display()),
                ));
                seen.push(r.workdir.clone());
            }
        }
        for p in &settings.recent {
            if !gr.repos.iter().any(|x| same_path(x, p)) && !seen.iter().any(|x| same_path(x, p)) {
                options.push((
                    p.display().to_string(),
                    format!("{}  (recent)  {}", dir_name(p), p.display()),
                ));
            }
        }
        let mut pick = String::new();
        if options.len() > 1
            && form::select(ui, "automation_group_add", &mut pick, &options, 360.0)
            && !pick.is_empty()
        {
            actions.push(
                Act::GroupAdd {
                    group: gr.name.clone(),
                    path: PathBuf::from(pick),
                }
                .into(),
            );
        }
        if IconButton::new(Icon::Folder, "Choose a folder…")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Act::GroupPickFolder(gr.name.clone()).into());
        }
    });
}

/// Column layout of the status table.
struct Cols {
    repo: f32,
    branch: f32,
    sync: f32,
    changes: f32,
    last: f32,
}

fn cols(w: f32) -> Cols {
    let fixed = 190.0 + 150.0 + 170.0 + 120.0 + 36.0;
    Cols {
        repo: (w - fixed).max(200.0),
        branch: 190.0,
        sync: 150.0,
        changes: 170.0,
        last: 120.0,
    }
}

fn status_table(ui: &mut Ui, group: &str, rows: &[Row], loading: bool, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let w = ui.available_width();
    let c = cols(w);
    let head_h = 24.0;
    let row_h = 48.0;
    let (rect, _) = ui.allocate_exact_size(vec2(w, head_h), Sense::hover());
    let painter = ui.painter().clone();
    let head_font = FontId::new(size::CAPTION, medium());
    let mut x = rect.left() + 8.0;
    for (title, cw) in [
        ("REPOSITORY", c.repo),
        ("BRANCH", c.branch),
        ("UPSTREAM", c.sync),
        ("CHANGES", c.changes),
        ("LAST COMMIT", c.last),
    ] {
        painter.text(
            pos2(x, rect.center().y),
            Align2::LEFT_CENTER,
            title,
            head_font.clone(),
            p.text_3,
        );
        x += cw;
    }
    let now = format::now_secs();
    for r in rows {
        let (rect, resp) = ui.allocate_exact_size(vec2(w, row_h), Sense::click());
        let hovered = ui.rect_contains_pointer(rect);
        kit::row_background(ui, rect, hovered, false);
        painter.hline(rect.x_range(), rect.top(), egui::Stroke::new(1.0, p.border));
        let top = rect.top() + 15.0;
        let bottom = rect.top() + 33.0;
        let mut x = rect.left() + 8.0;
        // Repository.
        let g = crate::views::truncated(
            &painter,
            &r.name,
            FontId::new(size::BODY, medium()),
            p.text,
            c.repo - 16.0,
        );
        painter.galley(pos2(x, top - g.size().y / 2.0), g, p.text);
        let sub = format!("{} · {}", r.source, r.path);
        let g = crate::views::truncated(
            &painter,
            &sub,
            FontId::proportional(size::SMALL),
            p.text_3,
            c.repo - 16.0,
        );
        painter.galley(pos2(x, bottom - g.size().y / 2.0), g, p.text_3);
        x += c.repo;
        match &r.status {
            None => {
                let text = if loading || r.open.is_some() {
                    "reading…"
                } else {
                    "not read yet"
                };
                painter.text(
                    pos2(x, top),
                    Align2::LEFT_CENTER,
                    text,
                    FontId::proportional(size::SMALL),
                    p.text_3,
                );
            }
            Some(Err(e)) => {
                let g = crate::views::truncated(
                    &painter,
                    e,
                    FontId::proportional(size::SMALL),
                    p.danger,
                    c.branch + c.sync + c.changes + c.last - 16.0,
                );
                painter.galley(pos2(x, top - g.size().y / 2.0), g, p.danger);
            }
            Some(Ok(s)) => {
                // Branch.
                let (branch, bc) = match &s.branch {
                    Some(b) => (b.clone(), p.text),
                    None => (
                        format!("detached at {}", s.head.as_deref().unwrap_or("?")),
                        p.warning,
                    ),
                };
                let g = crate::views::truncated(
                    &painter,
                    &branch,
                    FontId::monospace(size::MONO),
                    bc,
                    c.branch - 14.0,
                );
                painter.galley(pos2(x, top - g.size().y / 2.0), g, bc);
                if let Some(h) = &s.head {
                    painter.text(
                        pos2(x, bottom),
                        Align2::LEFT_CENTER,
                        h,
                        FontId::monospace(size::MONO - 1.0),
                        p.text_3,
                    );
                }
                x += c.branch;
                // Upstream.
                match &s.upstream {
                    None => {
                        painter.text(
                            pos2(x, top),
                            Align2::LEFT_CENTER,
                            if s.branch.is_some() {
                                "no upstream"
                            } else {
                                "—"
                            },
                            FontId::proportional(size::SMALL),
                            p.text_3,
                        );
                    }
                    Some(u) => {
                        let mut bx = x;
                        if s.ahead == 0 && s.behind == 0 {
                            paint_badge_left(&painter, &p, dark, bx, top, "in sync", Tone::Success);
                        } else {
                            if s.ahead > 0 {
                                bx = paint_badge_left(
                                    &painter,
                                    &p,
                                    dark,
                                    bx,
                                    top,
                                    &format!("{} ahead", s.ahead),
                                    if s.behind > 0 {
                                        Tone::Warning
                                    } else {
                                        Tone::Accent
                                    },
                                );
                            }
                            if s.behind > 0 {
                                paint_badge_left(
                                    &painter,
                                    &p,
                                    dark,
                                    bx,
                                    top,
                                    &format!("{} behind", s.behind),
                                    Tone::Warning,
                                );
                            }
                        }
                        let g = crate::views::truncated(
                            &painter,
                            u,
                            FontId::proportional(size::SMALL),
                            p.text_3,
                            c.sync - 14.0,
                        );
                        painter.galley(pos2(x, bottom - g.size().y / 2.0), g, p.text_3);
                    }
                }
                x += c.sync;
                // Changes.
                if s.tracked_changes() == 0 && s.untracked == 0 {
                    painter.text(
                        pos2(x, top),
                        Align2::LEFT_CENTER,
                        "clean",
                        FontId::proportional(size::SMALL),
                        p.text_2,
                    );
                } else {
                    let mut bx = x;
                    if s.conflicted > 0 {
                        bx = paint_badge_left(
                            &painter,
                            &p,
                            dark,
                            bx,
                            top,
                            &format!("{} conflicted", s.conflicted),
                            Tone::Danger,
                        );
                    }
                    let changed = s.staged + s.unstaged;
                    if changed > 0 {
                        bx = paint_badge_left(
                            &painter,
                            &p,
                            dark,
                            bx,
                            top,
                            &format!("{changed} changed"),
                            Tone::Warning,
                        );
                    }
                    if s.untracked > 0 && bx < x + c.changes - 40.0 {
                        paint_badge_left(
                            &painter,
                            &p,
                            dark,
                            bx,
                            top,
                            &format!("{} new", s.untracked),
                            Tone::Neutral,
                        );
                    }
                    if s.staged > 0 {
                        painter.text(
                            pos2(x, bottom),
                            Align2::LEFT_CENTER,
                            format!("{} staged", s.staged),
                            FontId::proportional(size::SMALL),
                            p.text_3,
                        );
                    }
                }
                x += c.changes;
                // Last commit.
                match s.last_commit {
                    Some(t) => {
                        painter.text(
                            pos2(x, top),
                            Align2::LEFT_CENTER,
                            format::relative(t, now),
                            FontId::proportional(size::SMALL),
                            p.text_2,
                        );
                        painter.text(
                            pos2(x, bottom),
                            Align2::LEFT_CENTER,
                            format::absolute(t),
                            FontId::proportional(size::SMALL),
                            p.text_3,
                        );
                    }
                    None => {
                        painter.text(
                            pos2(x, top),
                            Align2::LEFT_CENTER,
                            "no commits",
                            FontId::proportional(size::SMALL),
                            p.text_3,
                        );
                    }
                }
            }
        }
        // Remove, on hover (overlay: never moves the layout).
        let mut on_button = false;
        if hovered {
            let br = Rect::from_center_size(
                pos2(rect.right() - 18.0, rect.center().y),
                vec2(26.0, 24.0),
            );
            on_button = ui.rect_contains_pointer(br);
            let b = kit::overlay(ui, br, IconButton::icon(Icon::Close).compact());
            if b.on_hover_text("Remove from the group").clicked() {
                actions.push(
                    Act::GroupRemove {
                        group: group.to_owned(),
                        path: r.dir.clone(),
                    }
                    .into(),
                );
            }
            if !on_button {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
        if resp.clicked() && !on_button {
            match r.open {
                Some(i) => {
                    actions.push(Action::Activate(i));
                    actions.push(Action::ShowPage(Page::Graph));
                }
                None => actions.push(Action::OpenRepo(r.dir.clone())),
            }
        }
    }
    kit::muted(
        ui,
        "Click a row to open its history. Open repositories show their live state; the others were read with git status --porcelain=v2 --branch.",
    );
}

/// A badge whose left edge is at `left`; returns where the next one starts.
fn paint_badge_left(
    painter: &egui::Painter,
    p: &crate::theme::Palette,
    dark: bool,
    left: f32,
    cy: f32,
    text: &str,
    tone: Tone,
) -> f32 {
    let w = kit::badge_width(painter, text);
    kit::paint_badge(painter, p, dark, left + w, cy, text, tone);
    left + w + 4.0
}

fn batch_progress(ui: &mut Ui, b: &BatchRun) {
    let p = *pal(ui);
    let (current, done) = b
        .progress
        .lock()
        .map(|pr| (pr.current.clone(), pr.done.clone()))
        .unwrap_or_default();
    form::card(ui, Tone::Accent, |ui| {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(14.0).color(p.accent));
            ui.label(
                RichText::new(format!(
                    "{} · {} of {} done{}",
                    b.batch.label(),
                    done.len(),
                    b.names.len(),
                    current.map(|c| format!(" · now: {c}")).unwrap_or_default()
                ))
                .size(size::BODY)
                .color(p.text),
            );
            ui.label(
                RichText::new(format::millis(b.started.elapsed().as_millis() as u64))
                    .size(size::SMALL)
                    .color(p.text_3),
            );
        });
        if !done.is_empty() {
            ui.add_space(space::XS);
            result_rows(ui, &done);
        }
    });
}

fn batch_results(ui: &mut Ui, last: &BatchDone, actions: &mut Vec<Action>) {
    let failed = last
        .results
        .iter()
        .any(|r| matches!(r.outcome, Outcome::Failed(_)));
    let skipped = last
        .results
        .iter()
        .any(|r| matches!(r.outcome, Outcome::Skipped(_)));
    let tone = if failed {
        Tone::Danger
    } else if skipped {
        Tone::Warning
    } else {
        Tone::Success
    };
    form::card(ui, tone, |ui| {
        ui.horizontal(|ui| {
            kit::subtitle(ui, &core::summary(&last.batch, &last.results));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if IconButton::text("Dismiss").compact().show(ui).clicked() {
                    actions.push(Act::ClearBatch.into());
                }
                let p = *pal(ui);
                ui.label(
                    RichText::new(format!(
                        "{} · {}",
                        format::clock(last.at),
                        format::millis(last.elapsed_ms)
                    ))
                    .size(size::SMALL)
                    .color(p.text_3),
                );
            });
        });
        kit::muted(
            ui,
            &format!("{} in each repository", last.batch.command_line()),
        );
        ui.add_space(space::XS);
        result_rows(ui, &last.results);
    });
}

fn result_rows(ui: &mut Ui, results: &[core::RepoResult]) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let painter = ui.painter().clone();
    let w = ui.available_width();
    for r in results {
        let (rect, resp) = ui.allocate_exact_size(vec2(w, 26.0), Sense::hover());
        let (label, tone) = match &r.outcome {
            Outcome::Ok(_) => ("ok", Tone::Success),
            Outcome::Failed(_) => ("failed", Tone::Danger),
            Outcome::Skipped(_) => ("skipped", Tone::Warning),
        };
        let cy = rect.center().y;
        let name_w = 200.0_f32.min(w * 0.3);
        let g = crate::views::truncated(
            &painter,
            &r.name,
            FontId::new(size::BODY, medium()),
            p.text,
            name_w - 10.0,
        );
        painter.galley(pos2(rect.left(), cy - g.size().y / 2.0), g, p.text);
        let bl = rect.left() + name_w;
        kit::paint_badge(
            &painter,
            &p,
            dark,
            bl + kit::badge_width(&painter, label),
            cy,
            label,
            tone,
        );
        let tx = bl + 76.0;
        let time = format::millis(r.elapsed_ms);
        let tg = painter.layout_no_wrap(time, FontId::proportional(size::SMALL), p.text_3);
        let tw = tg.size().x;
        painter.galley(
            pos2(rect.right() - tw, cy - tg.size().y / 2.0),
            tg,
            p.text_3,
        );
        let text = r.outcome.text();
        let color = if matches!(r.outcome, Outcome::Failed(_)) {
            p.danger
        } else {
            p.text_2
        };
        let g = crate::views::truncated(
            &painter,
            text,
            FontId::proportional(size::SMALL),
            color,
            (rect.right() - tw - 12.0 - tx).max(40.0),
        );
        let elided = g.elided;
        painter.galley(pos2(tx, cy - g.size().y / 2.0), g, color);
        let full = if r.output.trim().is_empty() || r.output.trim() == text {
            String::new()
        } else {
            r.output.trim().to_owned()
        };
        if elided || !full.is_empty() {
            let tip = if full.is_empty() {
                text.to_owned()
            } else {
                full
            };
            resp.on_hover_text_at_pointer(tip);
        }
    }
    let _ = icons::paint;
}
