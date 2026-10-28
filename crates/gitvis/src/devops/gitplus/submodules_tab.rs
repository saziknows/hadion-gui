//! Submodules tab: each submodule with its state (not initialised,
//! different commit, conflict), the recorded commit against the checked-out
//! one, changes inside it and ahead / behind its tracked branch; init,
//! update, sync, update to the remote, open as a tab, add and remove.

use std::time::Instant;

use egui::{RichText, Ui};
use gitvis_core::devops::gitplus::submodules::{self as core, Overview, SubState, Submodule};
use gitvis_core::devops::gitplus::{git_line, short};

use super::{Act, Msg, ui as gui};
use crate::devops::ctx::Ctx;
use crate::format;
use crate::state::{Action, RepoState, ToastKind};
use crate::theme::pal;
use crate::views::form::{self, InputOpts};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Row, Stat, Tone};

/// A submodule command run on some (or all) submodules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubCmd {
    Init,
    Update,
    UpdateRemote,
    Sync,
}

impl SubCmd {
    pub fn args(self, paths: &[String]) -> Vec<String> {
        match self {
            SubCmd::Init => core::init_args(paths),
            SubCmd::Update => core::update_args(paths, false),
            SubCmd::UpdateRemote => core::update_args(paths, true),
            SubCmd::Sync => core::sync_args(paths),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SubCmd::Init => "Init",
            SubCmd::Update => "Update",
            SubCmd::UpdateRemote => "Update to remote",
            SubCmd::Sync => "Sync URLs",
        }
    }

    fn explain(self) -> &'static str {
        match self {
            SubCmd::Init => {
                "Copies the URLs from .gitmodules into this repository's configuration, so Update knows where to clone from. Nothing is downloaded."
            }
            SubCmd::Update => {
                "Checks out the commit this repository records in each submodule, cloning the ones that are not there yet (network) and recursing into nested ones. Local commits inside stay reachable but are no longer checked out."
            }
            SubCmd::UpdateRemote => {
                "Fetches each submodule's tracked branch from its remote (network) and checks out its newest commit. This repository then shows the submodules as changed: commit the new pointers to keep them."
            }
            SubCmd::Sync => {
                "Copies changed URLs from .gitmodules into the configuration of this repository and of each submodule's remote. Nothing is downloaded."
            }
        }
    }

    fn network(self) -> bool {
        matches!(self, SubCmd::Update | SubCmd::UpdateRemote)
    }
}

#[derive(Default)]
pub struct Submodules {
    pub overview: Option<Result<Overview, String>>,
    pub loading: bool,
    pub at: Option<Instant>,
    /// Path of the selected submodule.
    pub selected: Option<String>,
    pub pending_select: Option<usize>,
    pub add_url: String,
    pub add_path: String,
    pub add_branch: String,
    pub busy: bool,
}

impl Submodules {
    fn modules(&self) -> &[Submodule] {
        match &self.overview {
            Some(Ok(o)) => &o.modules,
            _ => &[],
        }
    }
}

pub fn load(cx: &mut Ctx, repo: &mut RepoState) {
    let s = &mut repo.devops.gitplus.submodules;
    if s.loading {
        return;
    }
    s.loading = true;
    cx.read("List submodules", |dir, log| {
        let mut cmds = Vec::new();
        let r = core::load(dir, &mut cmds);
        log.commands.extend(cmds);
        Msg::Submodules(r)
    });
}

pub fn shown(cx: &mut Ctx, repo: &mut RepoState) {
    load(cx, repo);
}

fn change(
    cx: &mut Ctx,
    repo: &mut RepoState,
    label: String,
    run: impl FnOnce(&std::path::Path, &mut Vec<String>) -> Result<String, String> + Send + 'static,
) {
    let s = &mut repo.devops.gitplus.submodules;
    if s.busy {
        return;
    }
    s.busy = true;
    cx.local_change(label, move |dir, log| {
        let mut cmds = Vec::new();
        let r = run(dir, &mut cmds);
        log.commands.extend(cmds);
        (r, Some(Msg::SubmoduleChanged))
    });
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::LoadSubmodules => load(cx, repo),
        Act::SelectSubmodule(p) => repo.devops.gitplus.submodules.selected = p,
        Act::SelectSubmoduleIndex(n) => {
            let s = &mut repo.devops.gitplus.submodules;
            match &s.overview {
                Some(Ok(o)) => s.selected = o.modules.get(n).map(|m| m.path.clone()),
                _ => s.pending_select = Some(n),
            }
        }
        Act::SubmoduleCmd { kind, paths } => {
            let args = kind.args(&paths);
            let which = if paths.is_empty() {
                "every submodule".to_owned()
            } else {
                paths.join(", ")
            };
            let body = format!(
                "{} Applies to {which}.{}",
                kind.explain(),
                if kind.network() {
                    " This talks to the submodules' remotes."
                } else {
                    ""
                }
            );
            cx.confirm(
                format!(
                    "{} {}?",
                    kind.label(),
                    if paths.len() == 1 {
                        "the submodule"
                    } else {
                        "submodules"
                    }
                ),
                body,
                kind.label(),
                false,
                vec![git_line(&args)],
                Act::SubmoduleCmdConfirmed { kind, paths }.into(),
            );
        }
        Act::SubmoduleCmdConfirmed { kind, paths } => {
            let args = kind.args(&paths);
            change(
                cx,
                repo,
                format!("Submodules: {}", kind.label()),
                move |dir, cmds| core::run(dir, &args, cmds),
            );
        }
        Act::AddSubmodule => {
            let s = &repo.devops.gitplus.submodules;
            let (url, path, branch) = (
                s.add_url.trim().to_owned(),
                s.add_path.trim().replace('\\', "/"),
                s.add_branch.trim().to_owned(),
            );
            if url.is_empty() || path.is_empty() {
                cx.toast(ToastKind::Error, "Enter the URL and the folder first.");
                return;
            }
            let target = repo.workdir.join(&path);
            if target.exists()
                && std::fs::read_dir(&target)
                    .map(|mut d| d.next().is_some())
                    .unwrap_or(true)
            {
                cx.toast(
                    ToastKind::Error,
                    format!("{path} exists and is not empty: pick another folder."),
                );
                return;
            }
            let args = core::add_args(&url, &path, &branch);
            cx.confirm(
                "Add the submodule?",
                format!(
                    "Clones {url} into {path} (network), records its current commit{} and adds it to .gitmodules. Commit the change afterwards; others then get it with Update.",
                    if branch.is_empty() {
                        String::new()
                    } else {
                        format!(" and tracks the branch {branch}")
                    }
                ),
                "Add submodule",
                false,
                vec![git_line(&args)],
                Act::AddSubmoduleConfirmed { url, path, branch }.into(),
            );
        }
        Act::AddSubmoduleConfirmed { url, path, branch } => {
            repo.devops.gitplus.submodules.selected = Some(path.clone());
            let args = core::add_args(&url, &path, &branch);
            change(
                cx,
                repo,
                format!("Add submodule {path}"),
                move |dir, cmds| {
                    core::run(dir, &args, cmds).map(|_| format!("Added the submodule {path}"))
                },
            );
        }
        Act::RemoveSubmodule { name, path } => {
            // `.git/modules` lives in the common dir (a linked worktree's
            // git dir is `.git/worktrees/NAME`).
            let modules = repo
                .snapshot
                .as_ref()
                .map(|s| {
                    let gd = s.git_dir.as_path();
                    match gd.parent() {
                        Some(parent) if parent.file_name().is_some_and(|n| n == "worktrees") => {
                            parent.parent().unwrap_or(gd).join("modules")
                        }
                        _ => gd.join("modules"),
                    }
                })
                .unwrap_or_else(|| repo.workdir.join(".git").join("modules"));
            let dirty = repo
                .devops
                .gitplus
                .submodules
                .modules()
                .iter()
                .find(|m| m.path == path)
                .map(|m| m.dirty.len())
                .unwrap_or(0);
            let mut body = format!(
                "Removes the submodule {path}: its checkout, its entry in .gitmodules and the index, and its clone under .git/modules. Commit the change afterwards."
            );
            if dirty > 0 {
                body.push_str(&format!(
                    " It has {} inside that will be lost.",
                    format::plural(dirty, "uncommitted change", "uncommitted changes")
                ));
            }
            body.push_str(" Commits that exist only in it are lost too unless they were pushed.");
            cx.confirm(
                "Remove the submodule?",
                body,
                "Deinit and remove",
                true,
                core::remove_commands(&name, &path, &modules),
                Act::RemoveSubmoduleConfirmed { name, path }.into(),
            );
        }
        Act::RemoveSubmoduleConfirmed { name, path } => {
            let s = &mut repo.devops.gitplus.submodules;
            if s.selected.as_deref() == Some(path.as_str()) {
                s.selected = None;
            }
            change(
                cx,
                repo,
                format!("Remove submodule {path}"),
                move |dir, cmds| core::remove(dir, &name, &path, cmds),
            );
        }
        _ => {}
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    match msg {
        Msg::Submodules(r) => {
            let s = &mut repo.devops.gitplus.submodules;
            s.loading = false;
            s.at = Some(Instant::now());
            if let Ok(o) = &r {
                if let Some(n) = s.pending_select.take() {
                    s.selected = o.modules.get(n).map(|m| m.path.clone());
                }
                if let Some(sel) = &s.selected
                    && !o.modules.iter().any(|m| &m.path == sel)
                {
                    s.selected = None;
                }
            }
            s.overview = Some(r);
        }
        Msg::SubmoduleChanged => {
            let s = &mut repo.devops.gitplus.submodules;
            s.busy = false;
            s.add_url.clear();
            s.add_path.clear();
            s.add_branch.clear();
            load(cx, repo);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

fn state_tone(s: SubState) -> Tone {
    match s {
        SubState::Current => Tone::Success,
        SubState::NotInitialised | SubState::Modified => Tone::Warning,
        SubState::Conflict => Tone::Danger,
    }
}

fn badges(m: &Submodule) -> Vec<(String, Tone)> {
    let mut b = Vec::new();
    if m.state != SubState::Current {
        b.push((m.state.label().to_owned(), state_tone(m.state)));
    }
    if !m.dirty.is_empty() {
        b.push((
            format::plural(m.dirty.len(), "change", "changes"),
            Tone::Warning,
        ));
    }
    if let Some((a, bh)) = m.ahead_behind
        && (a > 0 || bh > 0)
    {
        b.push((format!("↑{a} ↓{bh}"), Tone::Neutral));
    }
    if m.url_out_of_sync() {
        b.push(("URL changed".to_owned(), Tone::Warning));
    }
    if m.nested {
        b.push(("Nested".to_owned(), Tone::Neutral));
    }
    b
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let busy = repo.is_busy() || repo.devops.gitplus.submodules.busy;
    let workdir = repo.workdir.clone();
    let s = &mut repo.devops.gitplus.submodules;
    kit::page_header(
        ui,
        "Submodules",
        crate::devops::workbench::Tab::Submodules.hint(),
        |ui| {
            if IconButton::new(Icon::Rerun, "Refresh")
                .compact()
                .enabled(!s.loading)
                .show(ui)
                .on_hover_text(
                    "git submodule status --recursive, .gitmodules, and git status in each",
                )
                .clicked()
            {
                actions.push(Act::LoadSubmodules.into());
            }
            if IconButton::new(Icon::Plus, "Add")
                .compact()
                .show(ui)
                .on_hover_text("Add another repository as a submodule")
                .clicked()
            {
                actions.push(Act::SelectSubmodule(None).into());
            }
            if let (Some(Ok(o)), Some(at)) = (&s.overview, s.at) {
                gui::header_note(
                    ui,
                    &format!(
                        "{}  ·  {} ms  ·  {}",
                        format::plural(o.modules.len(), "submodule", "submodules"),
                        o.elapsed_ms,
                        gui::loaded_ago(at)
                    ),
                );
            }
        },
    );
    let o = match &s.overview {
        None => {
            gui::empty_pane(ui, |ui| kit::loading(ui, "Reading submodules…"));
            return;
        }
        Some(Err(e)) => {
            let e = e.clone();
            gui::empty_pane(ui, |ui| {
                if gui::error_card(ui, "Could not read the submodules", &e, "Try again") {
                    actions.push(Act::LoadSubmodules.into());
                }
            });
            return;
        }
        Some(Ok(o)) => o.clone(),
    };
    if o.modules.is_empty() && o.orphans.is_empty() {
        gui::page(ui, "gp_sub_empty", |ui| {
            kit::callout(
                ui,
                Tone::Neutral,
                "No submodules",
                "A submodule is another git repository checked out inside this one at a fixed commit: a library, a theme, shared configuration. This repository records which commit; Update checks that commit out, Update to remote moves it forward. This repository has none.",
            );
            add_form(ui, s, busy, actions);
        });
        return;
    }
    let selected = s.selected.clone();
    let modules = o.modules.clone();
    gui::list_pane(ui, "gp_sub_list", 400.0, |ui| {
        kit::list_header(ui, "Submodules", Some(modules.len()), |_| {});
        ui.add_space(6.0);
        kit::list(
            ui,
            "gp_sub_rows",
            modules.len(),
            kit::ROW_H,
            |i| selected.as_deref() == Some(modules[i].path.as_str()),
            |ui, rect, resp, i| {
                let m = &modules[i];
                let mut sub = if m.url.is_empty() {
                    "no URL in .gitmodules".to_owned()
                } else {
                    m.url.clone()
                };
                if let Some(b) = &m.branch {
                    sub.push_str(&format!("  ·  {b}"));
                }
                let meta = m
                    .checked_out
                    .as_deref()
                    .or(m.recorded.as_deref())
                    .map(|x| x[..7.min(x.len())].to_owned())
                    .unwrap_or_default();
                let b = badges(m);
                kit::paint_row(
                    ui,
                    rect,
                    &Row::new(&m.path)
                        .leading(kit::Leading::Dot(state_tone(m.state).color(&p)))
                        .subtitle(&sub)
                        .meta(&meta)
                        .badges(&b),
                    selected.as_deref() == Some(m.path.as_str()),
                );
                if resp.clicked() {
                    actions.push(Act::SelectSubmodule(Some(m.path.clone())).into());
                }
                if resp.double_clicked() && m.initialised() {
                    actions.push(Action::OpenRepo(workdir.join(&m.path)));
                }
                resp.context_menu(|ui| {
                    if m.initialised() && ui.button("Open as a tab").clicked() {
                        actions.push(Action::OpenRepo(workdir.join(&m.path)));
                        ui.close();
                    }
                    if ui.button("Open the folder").clicked() {
                        actions.push(Action::OpenPath(workdir.join(&m.path)));
                        ui.close();
                    }
                    if ui.button("Copy path").clicked() {
                        actions.push(Action::Copy(m.path.clone()));
                        ui.close();
                    }
                });
            },
        );
    });
    match selected.and_then(|sel| modules.iter().find(|m| m.path == sel).cloned()) {
        Some(m) => gui::detail_pane(ui, ("gp_sub", &m.path), |ui| {
            detail(ui, &m, &workdir, busy, actions);
        }),
        None => gui::detail_pane(ui, "gp_sub_overview", |ui| {
            overview(ui, s, &o, busy, actions);
        }),
    }
}

fn overview(ui: &mut Ui, s: &mut Submodules, o: &Overview, busy: bool, actions: &mut Vec<Action>) {
    kit::title(ui, "Submodules");
    let not_init = o
        .modules
        .iter()
        .filter(|m| m.state == SubState::NotInitialised)
        .count();
    let moved = o
        .modules
        .iter()
        .filter(|m| m.state == SubState::Modified)
        .count();
    let dirty = o.modules.iter().filter(|m| !m.dirty.is_empty()).count();
    let behind = o
        .modules
        .iter()
        .filter(|m| m.ahead_behind.is_some_and(|(_, b)| b > 0))
        .count();
    kit::stats(
        ui,
        &[
            Stat::new("Submodules", format::count(o.modules.len())),
            Stat::new("Not initialised", format::count(not_init))
                .tone_if(not_init > 0, Tone::Warning),
            Stat::new("Different commit", format::count(moved)).tone_if(moved > 0, Tone::Warning),
            Stat::new("With changes inside", format::count(dirty))
                .tone_if(dirty > 0, Tone::Warning),
            Stat::new("Behind their branch", format::count(behind)),
        ],
    );
    if not_init > 0 {
        kit::callout(
            ui,
            Tone::Warning,
            &format!(
                "{} not checked out",
                format::plural(not_init, "submodule is", "submodules are")
            ),
            "Update (with --init) clones and checks them out at the recorded commits.",
        );
    }
    kit::section(ui, "All submodules", None);
    gui::buttons(ui, |ui| {
        for (kind, icon) in [
            (SubCmd::Update, Icon::Download),
            (SubCmd::Init, Icon::Check),
            (SubCmd::Sync, Icon::Link),
            (SubCmd::UpdateRemote, Icon::Fetch),
        ] {
            if IconButton::new(icon, kind.label())
                .enabled(!busy)
                .show(ui)
                .on_hover_text(git_line(&kind.args(&[])))
                .clicked()
            {
                actions.push(
                    Act::SubmoduleCmd {
                        kind,
                        paths: Vec::new(),
                    }
                    .into(),
                );
            }
        }
    });
    kit::muted(
        ui,
        "Update checks out the recorded commits (cloning missing ones); Update to remote moves each one to the newest commit of its branch. Both use the network.",
    );
    if !o.orphans.is_empty() {
        kit::section(
            ui,
            ".gitmodules entries without a submodule",
            Some(o.orphans.len()),
        );
        for c in &o.orphans {
            kit::body(ui, &format!("{} → {} ({})", c.name, c.path, c.url));
        }
        kit::muted(
            ui,
            "These paths are not gitlinks in the index: the submodule was removed without editing .gitmodules, or was never committed.",
        );
    }
    add_form(ui, s, busy, actions);
}

fn add_form(ui: &mut Ui, s: &mut Submodules, busy: bool, actions: &mut Vec<Action>) {
    kit::section(ui, "Add a submodule", None);
    form::card(ui, Tone::Neutral, |ui| {
        form::field(
            ui,
            "Repository URL",
            Some("HTTPS or SSH URL, or a path relative to this repository's remote (../lib.git)."),
            |ui| {
                form::input(
                    ui,
                    "gp_sub_url",
                    &mut s.add_url,
                    InputOpts::new("https://github.com/owner/lib.git").mono(),
                )
            },
        );
        form::field(
            ui,
            "Folder",
            Some("Where it is checked out, relative to the top of this repository."),
            |ui| {
                form::input(
                    ui,
                    "gp_sub_path",
                    &mut s.add_path,
                    InputOpts::new("vendor/lib").mono(),
                )
            },
        );
        form::field(
            ui,
            "Branch to track (optional)",
            Some("Recorded in .gitmodules for Update to remote."),
            |ui| {
                form::input(
                    ui,
                    "gp_sub_branch",
                    &mut s.add_branch,
                    InputOpts::new("main").mono().width(240.0),
                )
            },
        );
        if !s.add_url.trim().is_empty() && !s.add_path.trim().is_empty() {
            let line = git_line(&core::add_args(&s.add_url, &s.add_path, &s.add_branch));
            kit::code_block(ui, &line, actions);
        }
        form::actions_row(ui, |ui| {
            let ready = !s.add_url.trim().is_empty() && !s.add_path.trim().is_empty();
            if form::primary(ui, "Add submodule…", ready && !busy).clicked() {
                actions.push(Act::AddSubmodule.into());
            }
        });
    });
}

fn detail(
    ui: &mut Ui,
    m: &Submodule,
    workdir: &std::path::Path,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    crate::views::insights::detail_title(ui, &m.path, &badges(m));
    kit::callout(ui, state_tone(m.state), m.state.label(), m.state.explain());
    ui.add_space(4.0);
    kit::kv(ui, 140.0, |kv| {
        kv.text("Name", kit::mono(&m.name));
        if m.url.is_empty() {
            kv.muted("URL", "None in .gitmodules");
        } else {
            kv.row("URL", |ui| gui::copyable(ui, &m.url, actions));
        }
        if let Some(c) = &m.configured_url
            && m.url_out_of_sync()
        {
            kv.text(
                "URL in use",
                RichText::new(format!("{c}  (differs from .gitmodules: run Sync URLs)"))
                    .color(p.warning),
            );
        }
        match &m.branch {
            Some(b) if b == "." => {
                kv.text("Tracks", "the branch of the same name as this repository's")
            }
            Some(b) => kv.text("Tracks", kit::mono(b)),
            None => kv.muted(
                "Tracks",
                "No branch set (Update to remote uses the remote's default)",
            ),
        }
        match &m.recorded {
            Some(r) => kv.row("Recorded commit", |ui| gui::copyable(ui, r, actions)),
            None => kv.muted("Recorded commit", "Not in the index"),
        }
        match &m.checked_out {
            Some(c) => {
                let same = m.recorded.as_deref() == Some(c.as_str());
                kv.row("Checked out", |ui| {
                    ui.horizontal(|ui| {
                        gui::copyable(ui, c, actions);
                        if same {
                            kit::badge(ui, "Same", Tone::Success);
                        } else {
                            kit::badge(ui, "Different", Tone::Warning);
                        }
                    });
                });
                if let Some(subj) = &m.head_subject {
                    kv.text("Its HEAD", subj.as_str());
                }
                match &m.head_branch {
                    Some(b) => kv.text("Branch inside", kit::mono(b)),
                    None => kv.muted("Branch inside", "Detached HEAD (normal after Update)"),
                }
                if let Some(d) = &m.describe {
                    kv.text("Describe", kit::mono(d));
                }
            }
            None => kv.muted("Checked out", "Nothing: not initialised"),
        }
        if let (Some(t), Some((a, b))) = (&m.tracking, m.ahead_behind) {
            kv.text(
                "Against its branch",
                format!(
                    "{} ahead, {} behind {t}",
                    format::count(a),
                    format::count(b)
                ),
            );
        } else if let Some(t) = &m.tracking {
            kv.muted("Against its branch", &format!("{t} is not fetched here"));
        }
        if let Some(e) = &m.status_error {
            kv.text("Status", RichText::new(e.as_str()).color(p.danger));
        }
    });
    gui::buttons(ui, |ui| {
        let path = vec![m.path.clone()];
        if m.initialised()
            && IconButton::new(Icon::External, "Open as a tab")
                .show(ui)
                .clicked()
        {
            actions.push(Action::OpenRepo(workdir.join(&m.path)));
        }
        if IconButton::new(Icon::Folder, "Open the folder")
            .show(ui)
            .clicked()
        {
            actions.push(Action::OpenPath(workdir.join(&m.path)));
        }
        for kind in [
            SubCmd::Init,
            SubCmd::Update,
            SubCmd::Sync,
            SubCmd::UpdateRemote,
        ] {
            if kind == SubCmd::Init && m.configured_url.is_some() {
                continue;
            }
            if IconButton::text(kind.label())
                .enabled(!busy && !m.nested)
                .show(ui)
                .on_hover_text(git_line(&kind.args(&path)))
                .clicked()
            {
                actions.push(
                    Act::SubmoduleCmd {
                        kind,
                        paths: path.clone(),
                    }
                    .into(),
                );
            }
        }
        if !m.nested
            && IconButton::new(Icon::Trash, "Deinit and remove…")
                .danger()
                .enabled(!busy)
                .show(ui)
                .clicked()
        {
            actions.push(
                Act::RemoveSubmodule {
                    name: m.name.clone(),
                    path: m.path.clone(),
                }
                .into(),
            );
        }
    });
    if m.nested {
        kit::muted(
            ui,
            "A nested submodule: run commands from the submodule that contains it (open it as a tab).",
        );
    }
    if !m.dirty.is_empty() {
        kit::section(ui, "Changes inside", Some(m.dirty.len()));
        let text = m
            .dirty
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        kit::code_block(ui, &text, actions);
        if m.dirty.len() > 40 {
            kit::muted(ui, &format!("and {} more", m.dirty.len() - 40));
        }
        kit::muted(ui, "Open it as a tab to commit or discard them.");
    }
    if let (Some(r), Some(c)) = (&m.recorded, &m.checked_out)
        && r != c
    {
        kit::section(ui, "What to do", None);
        kit::body(
            ui,
            &format!(
                "This repository records {} but {} is checked out. Stage the submodule in the WIP panel and commit to record the new commit, or Update to check the recorded one out again.",
                short(r),
                short(c)
            ),
        );
    }
}
