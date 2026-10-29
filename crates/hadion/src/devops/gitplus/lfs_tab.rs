//! Git LFS tab: whether LFS is installed and set up here, the tracked
//! patterns (track / untrack edit `.gitattributes`), the LFS files with
//! size and whether the content is downloaded, `git lfs status`, locks
//! (loaded on request: a network call), pull / fetch / prune, lock / unlock.

use std::time::Instant;

use egui::{Align2, FontId, RichText, Sense, Ui, pos2, vec2};
use hadion_core::devops::gitplus::lfs::{self as core, Lock, Overview};
use hadion_core::devops::gitplus::{git_line, git_ok};

use super::{Act, Msg, ui as gui};
use crate::devops::ctx::Ctx;
use crate::format;
use crate::state::{Action, RepoState, Settings, ToastKind};
use crate::theme::{pal, size};
use crate::views::form::{self, InputOpts};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Stat, Tone};

/// Install hint shown when `git lfs` is missing.
pub const INSTALL_HINT: &str = "Git for Windows ships Git LFS (re-run its installer with \"Git LFS\" ticked), or winget install GitHub.GitLFS; then run git lfs install once.";

/// LFS commands with a confirmation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LfsCmd {
    InstallLocal,
    Pull,
    Fetch,
    Prune,
}

impl LfsCmd {
    pub fn args(self) -> Vec<String> {
        match self {
            LfsCmd::InstallLocal => core::install_local_args(),
            LfsCmd::Pull => core::pull_args(),
            LfsCmd::Fetch => core::fetch_args(),
            LfsCmd::Prune => core::prune_args(false),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LfsCmd::InstallLocal => "Set up LFS here",
            LfsCmd::Pull => "Pull",
            LfsCmd::Fetch => "Fetch",
            LfsCmd::Prune => "Prune",
        }
    }

    fn explain(self) -> &'static str {
        match self {
            LfsCmd::InstallLocal => {
                "Adds the LFS filter and the pre-push, post-checkout, post-commit and post-merge hooks to this repository only (--local); your global git configuration is not touched."
            }
            LfsCmd::Pull => {
                "Downloads the LFS content of the current checkout from the LFS server (network) and replaces the pointer files in the working tree with it."
            }
            LfsCmd::Fetch => {
                "Downloads the LFS content of the current checkout into the local LFS store (network) without touching the working tree."
            }
            LfsCmd::Prune => {
                "Deletes local copies of LFS content that no recent commit, unpushed commit or stash needs. They can be downloaded again later."
            }
        }
    }
}

#[derive(Default)]
pub struct Lfs {
    pub overview: Option<Box<Overview>>,
    pub loading: bool,
    pub at: Option<Instant>,
    pub pattern: String,
    pub lockable: bool,
    pub filter: String,
    pub locks: Option<Result<Vec<Lock>, String>>,
    pub locks_loading: bool,
    pub locks_at: Option<Instant>,
    pub prune: Option<Result<String, String>>,
    pub prune_loading: bool,
    pub busy: bool,
}

pub fn load(cx: &mut Ctx, repo: &mut RepoState) {
    let s = &mut repo.devops.gitplus.lfs;
    if s.loading {
        return;
    }
    s.loading = true;
    cx.read("Read Git LFS state", |dir, log| {
        let mut cmds = Vec::new();
        let o = core::load(dir, &mut cmds);
        log.commands.extend(cmds);
        Msg::Lfs(Box::new(o))
    });
}

pub fn shown(cx: &mut Ctx, repo: &mut RepoState) {
    load(cx, repo);
}

fn local(cx: &mut Ctx, repo: &mut RepoState, label: String, args: Vec<String>) {
    let s = &mut repo.devops.gitplus.lfs;
    if s.busy {
        return;
    }
    s.busy = true;
    cx.local_change(label, move |dir, log| {
        let mut cmds = Vec::new();
        let r = git_ok(dir, &args, &mut cmds).map(|t| {
            if t.trim().is_empty() {
                "Done".to_owned()
            } else {
                t
            }
        });
        log.commands.extend(cmds);
        (r, Some(Msg::LfsChanged))
    });
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::LoadLfs => load(cx, repo),
        Act::LfsTrack => {
            let s = &repo.devops.gitplus.lfs;
            let pattern = s.pattern.trim().to_owned();
            if pattern.is_empty() {
                cx.toast(ToastKind::Error, "Enter a pattern such as *.psd first.");
                return;
            }
            let args = core::track_args(&pattern, s.lockable);
            crate::devops::pages::explain::confirm(
                cx,
                "Track the pattern with LFS?",
                format!(
                    "Adds \"{pattern}\" to .gitattributes, so matching files added from now on are stored in LFS{}. Files already committed stay as they are until they are added again (git lfs migrate rewrites history; Hadion does not). Commit .gitattributes afterwards.",
                    if s.lockable {
                        " and are read-only until you lock them"
                    } else {
                        ""
                    }
                ),
                "Track",
                false,
                vec![git_line(&args)],
                Act::LfsTrackConfirmed {
                    pattern,
                    lockable: s.lockable,
                }
                .into(),
            );
        }
        Act::LfsTrackConfirmed { pattern, lockable } => {
            repo.devops.gitplus.lfs.pattern.clear();
            local(
                cx,
                repo,
                format!("Track {pattern} with LFS"),
                core::track_args(&pattern, lockable),
            );
        }
        Act::LfsUntrack(pattern) => {
            let args = core::untrack_args(&pattern);
            crate::devops::pages::explain::confirm(
                cx,
                "Stop tracking the pattern?",
                format!(
                    "Removes \"{pattern}\" from .gitattributes. Files already in LFS stay pointers in history; new versions are stored in git directly. Commit .gitattributes afterwards."
                ),
                "Untrack",
                false,
                vec![git_line(&args)],
                Act::LfsUntrackConfirmed(pattern).into(),
            );
        }
        Act::LfsUntrackConfirmed(pattern) => local(
            cx,
            repo,
            format!("Untrack {pattern}"),
            core::untrack_args(&pattern),
        ),
        Act::LfsCmd(kind) => {
            let mut details = vec![git_line(&kind.args())];
            if kind == LfsCmd::Prune
                && let Some(Ok(t)) = &repo.devops.gitplus.lfs.prune
            {
                let summary = core::prune_summary(t);
                if !summary.is_empty() {
                    details.push(summary);
                }
            }
            crate::devops::pages::explain::confirm(
                cx,
                format!("{}?", kind.label()),
                kind.explain(),
                kind.label(),
                kind == LfsCmd::Prune,
                details,
                Act::LfsCmdConfirmed(kind).into(),
            );
        }
        Act::LfsCmdConfirmed(kind) => {
            if kind == LfsCmd::Prune {
                repo.devops.gitplus.lfs.prune = None;
            }
            local(cx, repo, format!("Git LFS: {}", kind.label()), kind.args())
        }
        Act::LfsPrunePreview => {
            let s = &mut repo.devops.gitplus.lfs;
            if s.prune_loading {
                return;
            }
            s.prune_loading = true;
            cx.read("Preview git lfs prune", |dir, log| {
                let mut cmds = Vec::new();
                let r = git_ok(dir, &core::prune_args(true), &mut cmds);
                log.commands.extend(cmds);
                Msg::LfsPrune(r)
            });
        }
        Act::LoadLocks => {
            let s = &mut repo.devops.gitplus.lfs;
            if s.locks_loading {
                return;
            }
            s.locks_loading = true;
            cx.read("List Git LFS locks", |dir, log| {
                let mut cmds = Vec::new();
                let args = core::locks_args();
                let r = hadion_core::devops::gitplus::git_raw(dir, &args, &[], &mut cmds).and_then(
                    |out| {
                        if out.success {
                            core::parse_locks_json(&out.stdout)
                        } else {
                            Err(hadion_core::devops::gitplus::output_text(&out))
                        }
                    },
                );
                log.commands.extend(cmds);
                Msg::Locks(r)
            });
        }
        Act::LfsLock(path) => {
            let args = core::lock_args(&path);
            crate::devops::pages::explain::confirm(
                cx,
                "Lock the file?",
                format!(
                    "Asks the LFS server to lock {path} for you: others cannot push changes to it until you unlock it. This is a change on the server."
                ),
                "Lock",
                false,
                vec![git_line(&args)],
                Act::LfsLockConfirmed(path).into(),
            );
        }
        Act::LfsLockConfirmed(path) => {
            let args = core::lock_args(&path);
            cx.remote_change(format!("Lock {path}"), move |dir, log| {
                let mut cmds = Vec::new();
                let r = git_ok(dir, &args, &mut cmds).map(|_| format!("Locked {path}"));
                log.commands.extend(cmds);
                (r, Some(Msg::LocksChanged))
            });
        }
        Act::LfsUnlock { path, force } => {
            let args = core::unlock_args(&path, force);
            crate::devops::pages::explain::confirm(
                cx,
                if force {
                    "Force the unlock?"
                } else {
                    "Unlock the file?"
                },
                if force {
                    format!(
                        "Removes the lock on {path} although someone else holds it (needs admin rights on the server). They are not told."
                    )
                } else {
                    format!("Releases your lock on {path} on the LFS server.")
                },
                if force { "Force unlock" } else { "Unlock" },
                force,
                vec![git_line(&args)],
                Act::LfsUnlockConfirmed { path, force }.into(),
            );
        }
        Act::LfsUnlockConfirmed { path, force } => {
            let args = core::unlock_args(&path, force);
            cx.remote_change(format!("Unlock {path}"), move |dir, log| {
                let mut cmds = Vec::new();
                let r = git_ok(dir, &args, &mut cmds).map(|_| format!("Unlocked {path}"));
                log.commands.extend(cmds);
                (r, Some(Msg::LocksChanged))
            });
        }
        _ => {}
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    let s = &mut repo.devops.gitplus.lfs;
    match msg {
        Msg::Lfs(o) => {
            s.loading = false;
            s.at = Some(Instant::now());
            s.overview = Some(o);
        }
        Msg::LfsChanged => {
            s.busy = false;
            load(cx, repo);
        }
        Msg::Locks(r) => {
            s.locks_loading = false;
            s.locks_at = Some(Instant::now());
            s.locks = Some(r);
        }
        Msg::LocksChanged => apply(cx, repo, Act::LoadLocks),
        Msg::LfsPrune(r) => {
            s.prune_loading = false;
            s.prune = Some(r);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

/// Files listed before the rest is summarised.
const MAX_FILES: usize = 300;

pub fn show(ui: &mut Ui, repo: &mut RepoState, settings: &Settings, actions: &mut Vec<Action>) {
    let busy = repo.is_busy() || repo.devops.gitplus.lfs.busy;
    let s = &mut repo.devops.gitplus.lfs;
    // Pull and fetch only where LFS is in use.
    let installed = s
        .overview
        .as_ref()
        .is_some_and(|o| o.installed() && !o.unused());
    kit::page_header(
        ui,
        "Git LFS",
        crate::devops::workbench::Tab::Lfs.hint(),
        |ui| {
            if IconButton::new(Icon::Rerun, "Refresh")
                .compact()
                .enabled(!s.loading)
                .show(ui)
                .on_hover_text("git lfs version, track, ls-files, status")
                .clicked()
            {
                actions.push(Act::LoadLfs.into());
            }
            if installed {
                for kind in [LfsCmd::Fetch, LfsCmd::Pull] {
                    if IconButton::new(
                        if kind == LfsCmd::Pull {
                            Icon::Pull
                        } else {
                            Icon::Download
                        },
                        kind.label(),
                    )
                    .compact()
                    .enabled(!busy)
                    .show(ui)
                    .on_hover_text(git_line(&kind.args()))
                    .clicked()
                    {
                        actions.push(Act::LfsCmd(kind).into());
                    }
                }
            }
            if let (Some(o), Some(at)) = (&s.overview, s.at) {
                gui::header_note(
                    ui,
                    &format!("{} ms  ·  {}", o.elapsed_ms, gui::loaded_ago(at)),
                );
            }
        },
    );
    let Some(o) = s.overview.clone() else {
        gui::empty_pane(ui, |ui| kit::loading(ui, "Reading Git LFS state…"));
        return;
    };
    gui::page(ui, "gp_lfs", |ui| {
        if !o.installed() {
            if gui::missing_tool(
                ui,
                "Git LFS",
                "git lfs",
                "Git LFS stores large files (images, videos, datasets, builds) outside the repository and keeps small pointer files in git.",
                INSTALL_HINT,
            ) {
                actions.push(Act::LoadLfs.into());
            }
            if !o.missing_reason.is_empty() {
                kit::muted(ui, &o.missing_reason);
            }
            if !o.patterns.is_empty() {
                kit::callout(
                    ui,
                    Tone::Warning,
                    "This repository uses LFS",
                    &format!(
                        ".gitattributes sends {} through LFS: without it you only see pointer files.",
                        format::plural(o.patterns.len(), "pattern", "patterns")
                    ),
                );
            }
            return;
        }
        if o.unused() {
            kit::callout(
                ui,
                Tone::Neutral,
                "This repository does not use Git LFS",
                "Git LFS keeps large files (images, videos, datasets, builds) out of the repository: git stores a small pointer and the content lives on the LFS server.\nTo start, track a pattern below and commit .gitattributes.\nSet up LFS adds the hooks that upload the content on push.",
            );
        }
        setup(ui, &o, busy, actions);
        crate::views::insights::stats(
            ui,
            &[
                Stat::new("Tracked patterns", format::count(o.patterns.len())),
                Stat::new("LFS files", format::count(o.files.len())),
                Stat::new("Their size", format::bytes(o.total_size() as usize)),
                Stat::new("Pointer only", format::count(o.pointers_only()))
                    .tone_if(o.pointers_only() > 0, Tone::Warning)
                    .tip("Files whose content is not in the working tree: Pull downloads them"),
            ],
        );
        patterns(ui, s, &o, busy, actions);
        files(ui, s, &o, actions);
        status(ui, &o);
        locks(ui, s, settings, busy, actions);
        prune(ui, s, busy, actions);
    });
}

fn setup(ui: &mut Ui, o: &Overview, busy: bool, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    kit::kv(ui, 150.0, |kv| {
        kv.text("Version", o.version.clone().unwrap_or_default());
        kv.row("Filter", |ui| {
            if o.filter_configured {
                ui.label(
                    RichText::new("Configured: files matching a pattern go through LFS")
                        .color(p.text),
                );
            } else {
                ui.label(
                    RichText::new("Not configured: git stores LFS files as they are")
                        .color(p.warning),
                );
            }
        });
        kv.row("Hooks here", |ui| {
            if o.hooks_installed {
                ui.label(RichText::new("Installed (pre-push uploads LFS content)").color(p.text));
            } else if o.unused() {
                ui.label(
                    RichText::new("Not installed (needed once you track a pattern)")
                        .color(p.text_2),
                );
            } else {
                ui.label(
                    RichText::new("Missing: a push would not upload LFS content").color(p.warning),
                );
            }
        });
    });
    if (!o.filter_configured || !o.hooks_installed)
        && IconButton::new(Icon::Check, "Set up LFS for this repository…")
            .enabled(!busy)
            .show(ui)
            .on_hover_text(git_line(&LfsCmd::InstallLocal.args()))
            .clicked()
    {
        actions.push(Act::LfsCmd(LfsCmd::InstallLocal).into());
    }
}

fn patterns(ui: &mut Ui, s: &mut Lfs, o: &Overview, busy: bool, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    kit::section(ui, "Tracked patterns", Some(o.patterns.len()));
    if o.patterns.is_empty() {
        kit::secondary(
            ui,
            "No pattern sends files through LFS yet.\nTrack one below, for example *.psd or assets/**.",
        );
    }
    for pat in &o.patterns {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(kit::mono(&pat.pattern).color(p.text));
            let at = match pat.line {
                Some(l) => format!("{}:{l}", pat.source),
                None => pat.source.clone(),
            };
            ui.label(RichText::new(at).size(size::SMALL).color(p.text_2));
            if pat.lockable {
                kit::badge(ui, "Lockable", Tone::Accent);
            }
            if IconButton::text("Untrack")
                .compact()
                .enabled(!busy)
                .show(ui)
                .clicked()
            {
                actions.push(Act::LfsUntrack(pat.pattern.clone()).into());
            }
        });
    }
    if !o.excluded.is_empty() {
        kit::muted(
            ui,
            &format!(
                "Excluded: {}",
                o.excluded
                    .iter()
                    .map(|(pt, src)| format!("{pt} ({src})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        let iw = crate::devops::pages::layout::input_width(ui, 220.0);
        form::input(
            ui,
            "gp_lfs_pattern",
            &mut s.pattern,
            InputOpts::new("*.psd").width(iw).mono(),
        );
        form::checkbox(ui, &mut s.lockable, "Lockable");
        if form::button(ui, "Track…", !busy && !s.pattern.trim().is_empty()).clicked() {
            actions.push(Act::LfsTrack.into());
        }
    });
}

fn files(ui: &mut Ui, s: &mut Lfs, o: &Overview, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    kit::section(ui, "Files", Some(o.files.len()));
    if let Some(e) = &o.files_error {
        kit::callout(ui, Tone::Danger, "Could not list the LFS files", e);
        return;
    }
    if o.files.is_empty() {
        kit::muted(ui, "No file in the index is stored in LFS.");
        return;
    }
    if o.files.len() > 12 {
        gui_search(ui, &mut s.filter);
    }
    let needle = s.filter.trim().to_lowercase();
    let locks: Vec<&Lock> = match &s.locks {
        Some(Ok(l)) => l.iter().collect(),
        _ => Vec::new(),
    };
    let shown: Vec<_> = o
        .files
        .iter()
        .filter(|f| needle.is_empty() || f.path.to_lowercase().contains(&needle))
        .collect();
    for f in shown.iter().take(MAX_FILES) {
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), kit::ROW_H_ONE), Sense::click());
        if !ui.is_rect_visible(rect) {
            continue;
        }
        kit::row_background(ui, rect, resp.hovered(), false);
        let painter = ui.painter();
        let mid = rect.center().y;
        let dark = ui.visuals().dark_mode;
        let mut right = rect.right() - 10.0;
        let (badge, tone) = if f.checkout {
            ("Downloaded", Tone::Success)
        } else if f.downloaded {
            ("In the store", Tone::Neutral)
        } else {
            ("Pointer only", Tone::Warning)
        };
        right = kit::paint_badge(painter, &p, dark, right, mid, badge, tone) - 8.0;
        if let Some(l) = locks.iter().find(|l| l.path == f.path) {
            right = kit::paint_badge(
                painter,
                &p,
                dark,
                right,
                mid,
                &format!("Locked by {}", l.owner),
                Tone::Accent,
            ) - 8.0;
        }
        let meta = format!(
            "{}  ·  {}",
            f.size
                .map(|b| format::bytes(b as usize))
                .unwrap_or_default(),
            &f.oid[..f.oid.len().min(10)]
        );
        let r = painter.text(
            pos2(right, mid),
            Align2::RIGHT_CENTER,
            meta,
            FontId::proportional(size::SMALL),
            p.text_2,
        );
        let g = crate::views::truncated(
            painter,
            &f.path,
            FontId::monospace(size::MONO),
            p.text,
            (r.left() - rect.left() - 26.0).max(30.0),
        );
        painter.galley(pos2(rect.left() + 12.0, mid - g.size().y / 2.0), g, p.text);
        resp.context_menu(|ui| {
            if ui.button("Lock this file…").clicked() {
                actions.push(Act::LfsLock(f.path.clone()).into());
                ui.close();
            }
            if ui.button("Copy path").clicked() {
                actions.push(Action::Copy(f.path.clone()));
                ui.close();
            }
            if ui.button("Copy object id").clicked() {
                actions.push(Action::Copy(f.oid.clone()));
                ui.close();
            }
        });
    }
    if shown.len() > MAX_FILES {
        kit::muted(
            ui,
            &format!(
                "and {} more: type in the filter to narrow the list.",
                format::count(shown.len() - MAX_FILES)
            ),
        );
    } else if shown.is_empty() {
        kit::muted(ui, "No file matches the filter.");
    }
    kit::secondary(
        ui,
        "Right-click a file to lock it or copy its path or object id.",
    );
}

fn gui_search(ui: &mut Ui, text: &mut String) {
    ui.horizontal(|ui| {
        crate::devops::pages::layout::cap_width(ui, 420.0);
        kit::search_box(ui, "gp_lfs_filter", text, "Filter files");
    });
    ui.add_space(4.0);
}

fn status(ui: &mut Ui, o: &Overview) {
    let p = *pal(ui);
    kit::section(ui, "git lfs status", None);
    if let Some(e) = &o.status_error {
        kit::callout(ui, Tone::Danger, "Could not read the status", e);
        return;
    }
    let sections: Vec<_> = o.status.iter().filter(|s| !s.entries.is_empty()).collect();
    if sections.is_empty() {
        kit::muted(ui, "No LFS objects waiting to be committed or pushed.");
        return;
    }
    for sec in sections {
        kit::subtitle(ui, &sec.title);
        for (path, detail) in &sec.entries {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.label(kit::mono(path).color(p.text));
                ui.label(RichText::new(detail).size(size::SMALL).color(p.text_2));
            });
        }
        ui.add_space(4.0);
    }
}

fn locks(ui: &mut Ui, s: &mut Lfs, settings: &Settings, busy: bool, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    kit::section_with(
        ui,
        "Locks",
        s.locks.as_ref().and_then(|l| l.as_ref().ok()).map(Vec::len),
        |ui| {
            let label = if s.locks.is_some() {
                "Reload locks"
            } else {
                "Load locks"
            };
            if IconButton::new(Icon::Lock, label)
                .compact()
                .enabled(!s.locks_loading)
                .show(ui)
                .on_hover_text("git lfs locks --json: asks the LFS server")
                .clicked()
            {
                actions.push(Act::LoadLocks.into());
            }
        },
    );
    if settings.safe_mode {
        kit::secondary(
            ui,
            "Safe mode is on: lock and unlock are refused (they change the server).",
        );
    }
    if s.locks_loading {
        kit::loading(ui, "Asking the LFS server…");
        return;
    }
    match &s.locks {
        None => {
            kit::secondary(
                ui,
                "Locks live on the LFS server: Load locks asks it (network).\nLockable files are read-only until you lock them.",
            );
        }
        Some(Err(e)) => {
            let e = e.clone();
            if gui::error_card(ui, "Could not list the locks", &e, "Try again") {
                actions.push(Act::LoadLocks.into());
            }
        }
        Some(Ok(l)) if l.is_empty() => {
            kit::muted(ui, "Nobody holds a lock.");
        }
        Some(Ok(l)) => {
            for lock in l.clone() {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.label(kit::mono(&lock.path).color(p.text));
                    ui.label(
                        RichText::new(format!(
                            "{}  ·  id {}  ·  {}",
                            lock.owner, lock.id, lock.locked_at
                        ))
                        .size(size::SMALL)
                        .color(p.text_2),
                    );
                    if IconButton::text("Unlock")
                        .compact()
                        .enabled(!busy && !settings.safe_mode)
                        .show(ui)
                        .clicked()
                    {
                        actions.push(
                            Act::LfsUnlock {
                                path: lock.path.clone(),
                                force: false,
                            }
                            .into(),
                        );
                    }
                    if IconButton::text("Force unlock")
                        .compact()
                        .danger()
                        .enabled(!busy && !settings.safe_mode)
                        .show(ui)
                        .clicked()
                    {
                        actions.push(
                            Act::LfsUnlock {
                                path: lock.path.clone(),
                                force: true,
                            }
                            .into(),
                        );
                    }
                });
            }
            if let Some(at) = s.locks_at {
                kit::muted(ui, &gui::loaded_ago(at));
            }
        }
    }
}

fn prune(ui: &mut Ui, s: &mut Lfs, busy: bool, actions: &mut Vec<Action>) {
    kit::section(ui, "Free disk space", None);
    kit::secondary(ui, LfsCmd::Prune.explain());
    gui::buttons(ui, |ui| {
        if IconButton::new(Icon::Search, "Preview prune")
            .enabled(!s.prune_loading)
            .show(ui)
            .on_hover_text(git_line(&core::prune_args(true)))
            .clicked()
        {
            actions.push(Act::LfsPrunePreview.into());
        }
        if IconButton::new(Icon::Trash, "Prune…")
            .danger()
            .enabled(!busy)
            .show(ui)
            .clicked()
        {
            actions.push(Act::LfsCmd(LfsCmd::Prune).into());
        }
    });
    if s.prune_loading {
        kit::loading(ui, "git lfs prune --dry-run…");
    }
    match &s.prune {
        Some(Ok(t)) => {
            let text = if t.trim().is_empty() {
                "Nothing to prune.".to_owned()
            } else {
                t.lines().take(40).collect::<Vec<_>>().join("\n")
            };
            kit::code_block(ui, &text, actions);
        }
        Some(Err(e)) => kit::callout(ui, Tone::Danger, "The preview failed", e),
        None => {}
    }
}
