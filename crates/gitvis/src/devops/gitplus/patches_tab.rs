//! Patches tab: create patch files from commits (`git format-patch -o DIR`
//! for a range or the last N commits), and apply patch files or mailboxes
//! after a preview (`git apply --numstat --summary`, `--check`, the mails),
//! with `git apply --3way` or `git am --3way`. While `git am` is stopped the
//! tab offers Continue / Skip / Abort and shows the patch it stopped on.

use std::path::{Path, PathBuf};
use std::time::Instant;

use egui::{RichText, Ui};
use gitvis_core::Pending;
use gitvis_core::devops::gitplus::patches::{self as core, Applies, Preview, RangeCommit, Source};
use gitvis_core::devops::gitplus::{git_line, git_ok};

use super::{Act, Msg, ui as gui};
use crate::devops::ctx::Ctx;
use crate::format;
use crate::state::{Action, RepoState, ToastKind};
use crate::theme::{pal, size};
use crate::views::form::{self, InputOpts};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Tone};

/// A picked patch file and its preview.
pub struct PatchItem {
    pub path: PathBuf,
    /// `None` while checking.
    pub preview: Option<Result<Preview, String>>,
}

pub struct Patches {
    /// Export a range (else the last N commits).
    pub use_range: bool,
    pub range: String,
    pub last_n: String,
    pub out_dir: String,
    pub creating: bool,
    pub created: Option<Result<(PathBuf, Vec<PathBuf>), String>>,
    pub created_at: Option<Instant>,
    pub files: Vec<PatchItem>,
    pub three_way: bool,
    pub busy: bool,
    pub am_current: Option<Result<String, String>>,
    pub am_loading: bool,
}

impl Default for Patches {
    fn default() -> Self {
        Self {
            use_range: false,
            range: String::new(),
            last_n: "1".to_owned(),
            out_dir: String::new(),
            creating: false,
            created: None,
            created_at: None,
            files: Vec::new(),
            three_way: true,
            busy: false,
            am_current: None,
            am_loading: false,
        }
    }
}

impl Patches {
    fn source(&self) -> Result<Source, String> {
        if self.use_range {
            let r = self.range.trim();
            if r.is_empty() {
                return Err("Enter a range such as main..feature.".into());
            }
            Ok(Source::Range(r.to_owned()))
        } else {
            match self.last_n.trim().parse::<usize>() {
                Ok(n) if (1..=500).contains(&n) => Ok(Source::Last(n)),
                _ => Err("Enter how many commits: 1 to 500.".into()),
            }
        }
    }

    fn all_mailboxes(&self) -> bool {
        !self.files.is_empty()
            && self
                .files
                .iter()
                .all(|f| matches!(&f.preview, Some(Ok(p)) if p.is_mailbox()))
    }

    fn ready(&self) -> bool {
        !self.files.is_empty() && self.files.iter().all(|f| matches!(&f.preview, Some(Ok(_))))
    }
}

/// `<parent>/<repo>-patches`, next to the repository.
pub fn default_out_dir(workdir: &Path) -> PathBuf {
    let name = workdir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    workdir
        .parent()
        .unwrap_or(workdir)
        .join(format!("{name}-patches"))
}

fn display(p: &Path) -> String {
    let s = p.to_string_lossy().into_owned();
    if cfg!(windows) {
        s.replace('/', "\\")
    } else {
        s
    }
}

pub fn shown(cx: &mut Ctx, repo: &mut RepoState) {
    if repo.devops.gitplus.patches.out_dir.is_empty() {
        repo.devops.gitplus.patches.out_dir = display(&default_out_dir(&repo.workdir));
    }
    if repo.snapshot.as_ref().map(|s| s.pending) == Some(Pending::ApplyMailbox) {
        load_am_current(cx, repo);
    }
}

pub fn on_snapshot(cx: &mut Ctx, repo: &mut RepoState) {
    let pending = repo.snapshot.as_ref().map(|s| s.pending);
    let p = &mut repo.devops.gitplus.patches;
    if pending == Some(Pending::ApplyMailbox) {
        if p.am_current.is_none() {
            load_am_current(cx, repo);
        }
    } else {
        p.am_current = None;
    }
}

fn load_am_current(cx: &mut Ctx, repo: &mut RepoState) {
    let p = &mut repo.devops.gitplus.patches;
    if p.am_loading {
        return;
    }
    p.am_loading = true;
    cx.read("Show the patch git am stopped on", |dir, log| {
        let mut cmds = Vec::new();
        let r = git_ok(dir, &core::current_patch_args(), &mut cmds);
        log.commands.extend(cmds);
        Msg::AmCurrent(r)
    });
}

fn preview(cx: &mut Ctx, path: PathBuf) {
    cx.read(
        format!(
            "Check {}",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        ),
        move |dir, log| {
            let mut cmds = Vec::new();
            let r = core::preview(dir, &path, &mut cmds);
            log.commands.extend(cmds);
            Msg::PatchPreview(path, r)
        },
    );
}

fn add_file(cx: &mut Ctx, repo: &mut RepoState, path: PathBuf) {
    let p = &mut repo.devops.gitplus.patches;
    if let Some(item) = p.files.iter_mut().find(|f| f.path == path) {
        item.preview = None;
    } else {
        p.files.push(PatchItem {
            path: path.clone(),
            preview: None,
        });
    }
    preview(cx, path);
}

fn local(cx: &mut Ctx, repo: &mut RepoState, label: String, args: Vec<String>) {
    let p = &mut repo.devops.gitplus.patches;
    if p.busy {
        return;
    }
    p.busy = true;
    cx.local_change(label, move |dir, log| {
        let mut cmds = Vec::new();
        let r = git_ok(dir, &args, &mut cmds).map(|t| {
            let t = t.trim();
            if t.is_empty() {
                "Done".to_owned()
            } else {
                t.lines().last().unwrap_or(t).to_owned()
            }
        });
        log.commands.extend(cmds);
        (r, Some(Msg::PatchesApplied))
    });
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::PickPatchFolder => {
            let start = repo.devops.gitplus.patches.out_dir.clone();
            let mut d = rfd::FileDialog::new().set_title("Folder for the patch files");
            let start = PathBuf::from(start);
            if start.is_dir() {
                d = d.set_directory(&start);
            } else if let Some(parent) = start.parent().filter(|p| p.is_dir()) {
                d = d.set_directory(parent);
            }
            if let Some(dir) = d.pick_folder() {
                repo.devops.gitplus.patches.out_dir = display(&dir);
            }
        }
        Act::CreatePatches => {
            let p = &mut repo.devops.gitplus.patches;
            let source = match p.source() {
                Ok(s) => s,
                Err(e) => {
                    cx.toast(ToastKind::Error, e);
                    return;
                }
            };
            let out = p.out_dir.trim().to_owned();
            if out.is_empty() {
                cx.toast(
                    ToastKind::Error,
                    "Choose a folder for the patch files first.",
                );
                return;
            }
            if p.creating {
                return;
            }
            p.creating = true;
            let out = PathBuf::from(out);
            cx.read("List the commits to export", move |dir, log| {
                let mut cmds = Vec::new();
                let r = core::range_commits(dir, &source, &mut cmds);
                log.commands.extend(cmds);
                Msg::PatchRange(source, out, r)
            });
        }
        Act::CreatePatchesConfirmed { source, out_dir } => {
            let p = &mut repo.devops.gitplus.patches;
            if p.busy {
                return;
            }
            p.busy = true;
            cx.local_change("Create patch files", move |dir, log| {
                let mut cmds = Vec::new();
                let r = core::create(dir, &out_dir, &source, &mut cmds);
                log.commands.extend(cmds);
                let summary = r.as_ref().map(|f| {
                    format!(
                        "Wrote {} to {}",
                        format::plural(f.len(), "patch file", "patch files"),
                        out_dir.display()
                    )
                });
                (
                    summary.map_err(Clone::clone),
                    Some(Msg::PatchesCreated(r.map(|f| (out_dir, f)))),
                )
            });
        }
        Act::PickPatchFiles => {
            let mut d = rfd::FileDialog::new()
                .set_title("Patch files to apply")
                .add_filter(
                    "Patches and mailboxes",
                    &["patch", "diff", "mbox", "eml", "txt"],
                )
                .add_filter("All files", &["*"]);
            if let Some(Ok((dir, _))) = &repo.devops.gitplus.patches.created {
                d = d.set_directory(dir);
            }
            if let Some(files) = d.pick_files() {
                let mut files = files;
                files.sort();
                for f in files {
                    add_file(cx, repo, f);
                }
            }
        }
        Act::PreviewPatch(path) => add_file(cx, repo, path),
        Act::RemovePatchFile(i) => {
            let p = &mut repo.devops.gitplus.patches;
            if i < p.files.len() {
                p.files.remove(i);
            }
        }
        Act::ClearPatchFiles => repo.devops.gitplus.patches.files.clear(),
        Act::RecheckPatches => {
            let paths: Vec<PathBuf> = repo
                .devops
                .gitplus
                .patches
                .files
                .iter()
                .map(|f| f.path.clone())
                .collect();
            for p in paths {
                add_file(cx, repo, p);
            }
        }
        Act::ApplyPatches { am } => {
            let p = &repo.devops.gitplus.patches;
            if !p.ready() {
                cx.toast(
                    ToastKind::Error,
                    "Wait for the checks to finish (or remove the unreadable files).",
                );
                return;
            }
            let files: Vec<PathBuf> = p.files.iter().map(|f| f.path.clone()).collect();
            let three_way = p.three_way;
            let previews: Vec<&Preview> = p
                .files
                .iter()
                .filter_map(|f| f.preview.as_ref().and_then(|r| r.as_ref().ok()))
                .collect();
            let commits: usize = previews.iter().map(|x| x.mails.len()).sum();
            let changed: usize = previews.iter().map(|x| x.files.len()).sum();
            let refused = previews
                .iter()
                .filter(|x| matches!(x.applies, Applies::No(_)))
                .count();
            let args = if am {
                core::am_args(&files, three_way)
            } else {
                core::apply_args(&files, three_way)
            };
            let mut body = if am {
                format!(
                    "Creates {} on the current branch, one per mail, keeping their authors, dates and messages.",
                    format::plural(commits, "commit", "commits")
                )
            } else {
                format!(
                    "Applies the changes ({}) to the working tree{}. Nothing is committed.",
                    format::plural(changed, "file", "files"),
                    if three_way { " and the index" } else { "" }
                )
            };
            if three_way {
                body.push_str(" Where a patch does not apply cleanly, git merges it (--3way) and may leave conflicts to resolve.");
            }
            if refused > 0 {
                body.push_str(&format!(
                    " {} did not pass the check: expect it to fail.",
                    format::plural(refused, "file", "files")
                ));
            }
            if am {
                body.push_str(
                    " If git am stops, Continue, Skip and Abort are on this tab and on the banner.",
                );
            }
            crate::devops::pages::explain::confirm(
                cx,
                if am {
                    "Apply as commits?"
                } else {
                    "Apply the patches?"
                },
                body,
                if am { "Apply with git am" } else { "Apply" },
                false,
                vec![git_line(&args)],
                Act::ApplyPatchesConfirmed {
                    files,
                    am,
                    three_way,
                }
                .into(),
            );
        }
        Act::ApplyPatchesConfirmed {
            files,
            am,
            three_way,
        } => {
            let args = if am {
                core::am_args(&files, three_way)
            } else {
                core::apply_args(&files, three_way)
            };
            local(
                cx,
                repo,
                if am {
                    format!("git am {}", format::plural(files.len(), "file", "files"))
                } else {
                    format!("git apply {}", format::plural(files.len(), "file", "files"))
                },
                args,
            );
        }
        Act::AmStep(step) => {
            let args = core::am_step_args(&step);
            let (title, body, danger) = match step.as_str() {
                "continue" => (
                    "Continue git am?",
                    "Commits the current patch with what is staged (resolve and stage the conflicts first), then applies the next ones.",
                    false,
                ),
                "skip" => (
                    "Skip this patch?",
                    "Drops the patch git am stopped on and goes on with the next one. Its changes are not applied.",
                    true,
                ),
                _ => (
                    "Abort git am?",
                    "Stops applying and puts the branch back where it was before git am started; commits it already made are undone.",
                    true,
                ),
            };
            crate::devops::pages::explain::confirm(
                cx,
                title,
                body,
                match step.as_str() {
                    "continue" => "Continue",
                    "skip" => "Skip patch",
                    _ => "Abort",
                },
                danger,
                vec![git_line(&args)],
                Act::AmStepConfirmed(step).into(),
            );
        }
        Act::AmStepConfirmed(step) => {
            repo.devops.gitplus.patches.am_current = None;
            local(
                cx,
                repo,
                format!("git am --{step}"),
                core::am_step_args(&step),
            );
        }
        Act::LoadAmCurrent => {
            repo.devops.gitplus.patches.am_current = None;
            load_am_current(cx, repo);
        }
        _ => {}
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    let p = &mut repo.devops.gitplus.patches;
    match msg {
        Msg::PatchRange(source, out_dir, r) => {
            p.creating = false;
            match r {
                Err(e) => cx.toast(ToastKind::Error, e),
                Ok(commits) => confirm_create(cx, source, out_dir, &commits),
            }
        }
        Msg::PatchesCreated(r) => {
            p.busy = false;
            p.created_at = Some(Instant::now());
            p.created = Some(r);
        }
        Msg::PatchPreview(path, r) => {
            if let Some(item) = p.files.iter_mut().find(|f| f.path == path) {
                item.preview = Some(r);
            }
        }
        Msg::PatchesApplied => {
            p.busy = false;
            p.am_current = None;
            // The repository changed: check the remaining files again.
            let paths: Vec<PathBuf> = p.files.iter().map(|f| f.path.clone()).collect();
            for path in paths {
                if let Some(item) = p.files.iter_mut().find(|f| f.path == path) {
                    item.preview = None;
                }
                preview(cx, path);
            }
        }
        Msg::AmCurrent(r) => {
            p.am_loading = false;
            p.am_current = Some(r);
        }
        _ => {}
    }
}

fn confirm_create(cx: &mut Ctx, source: Source, out_dir: PathBuf, commits: &[RangeCommit]) {
    let args = core::format_patch_args(&out_dir, &source);
    let mut list: Vec<String> = commits
        .iter()
        .take(40)
        .enumerate()
        .map(|(i, c)| format!("{:04}  {}  {}", i + 1, c.short, c.subject))
        .collect();
    if commits.len() > 40 {
        list.push(format!("… and {} more", commits.len() - 40));
    }
    crate::devops::pages::explain::confirm(
        cx,
        "Create the patch files?",
        format!(
            "Writes {} into {}, one mail per commit ({}), oldest first. Merge commits are left out.",
            format::plural(commits.len(), "patch file", "patch files"),
            display(&out_dir),
            source.describe()
        ),
        "Create patches",
        false,
        vec![git_line(&args), list.join("\n")],
        Act::CreatePatchesConfirmed { source, out_dir }.into(),
    );
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

pub fn show(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) {
    let busy = repo.is_busy() || repo.devops.gitplus.patches.busy;
    let pending = repo.snapshot.as_ref().map(|s| s.pending) == Some(Pending::ApplyMailbox);
    let branch = repo
        .snapshot
        .as_ref()
        .and_then(|s| s.head.branch().map(str::to_owned))
        .unwrap_or_else(|| "HEAD".to_owned());
    let p = &mut repo.devops.gitplus.patches;
    kit::page_header(
        ui,
        "Patches",
        crate::devops::workbench::Tab::Patches.hint(),
        |_| {},
    );
    gui::page(ui, "gp_patches", |ui| {
        if pending {
            am_stopped(ui, p, busy, actions);
        }
        create(ui, p, &branch, busy, actions);
        apply_section(ui, p, busy, pending, actions);
    });
}

fn am_stopped(ui: &mut Ui, p: &mut Patches, busy: bool, actions: &mut Vec<Action>) {
    kit::callout_with(
        ui,
        Tone::Warning,
        "git am stopped",
        "A patch did not apply.\nResolve the conflicts (or apply it by hand) and stage the result, then Continue; or Skip this patch; or Abort to go back to where the branch was.",
        |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if IconButton::new(Icon::Check, "Continue")
                    .primary(true)
                    .enabled(!busy)
                    .show(ui)
                    .clicked()
                {
                    actions.push(Act::AmStep("continue".into()).into());
                }
                if IconButton::text("Skip patch")
                    .enabled(!busy)
                    .show(ui)
                    .clicked()
                {
                    actions.push(Act::AmStep("skip".into()).into());
                }
                if IconButton::text("Abort")
                    .danger()
                    .enabled(!busy)
                    .show(ui)
                    .clicked()
                {
                    actions.push(Act::AmStep("abort".into()).into());
                }
            });
        },
    );
    match &p.am_current {
        None if p.am_loading => kit::loading(ui, "Loading the patch it stopped on…"),
        None => {}
        Some(Ok(t)) => {
            kit::section(ui, "The patch it stopped on", None);
            let text: String = t.lines().take(60).collect::<Vec<_>>().join("\n");
            kit::code_block(ui, &text, actions);
            if t.lines().count() > 60 {
                kit::muted(
                    ui,
                    "Showing the first 60 lines (git am --show-current-patch=diff).",
                );
            }
        }
        Some(Err(e)) => {
            let e = e.clone();
            if gui::error_card(ui, "Could not show the current patch", &e, "Try again") {
                actions.push(Act::LoadAmCurrent.into());
            }
        }
    }
}

fn create(ui: &mut Ui, p: &mut Patches, branch: &str, busy: bool, actions: &mut Vec<Action>) {
    kit::section(ui, "Create patch files", None);
    form::card(ui, Tone::Neutral, |ui| {
        kit::secondary(
            ui,
            "git format-patch writes one mail-formatted file per commit, with its author, date and message.\nSend it, attach it to an issue, or apply it elsewhere with git am.",
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if form::radio(ui, !p.use_range, &format!("The last commits of {branch}")) {
                p.use_range = false;
            }
            ui.add_space(8.0);
            if form::radio(ui, p.use_range, "A range") {
                p.use_range = true;
            }
        });
        if p.use_range {
            form::field(
                ui,
                "Range",
                Some(
                    "A..B is the commits in B that are not in A; one revision (v1.0) means its commits up to HEAD.",
                ),
                |ui| {
                    let iw = crate::devops::pages::layout::input_width(ui, 320.0);
                    form::input(
                        ui,
                        "gp_patch_range",
                        &mut p.range,
                        InputOpts::new("main..feature").mono().width(iw),
                    )
                },
            );
        } else {
            form::field(ui, "How many commits", None, |ui| {
                form::input(
                    ui,
                    "gp_patch_n",
                    &mut p.last_n,
                    InputOpts::new("1").mono().width(100.0),
                )
            });
        }
        form::field(
            ui,
            "Folder",
            Some("Created when missing; existing files with the same names are overwritten."),
            |ui| {
                ui.horizontal(|ui| {
                    form::input(
                        ui,
                        "gp_patch_out",
                        &mut p.out_dir,
                        InputOpts::new("Choose a folder")
                            // Room for the Choose button; the frame adds 22.
                            .width((ui.available_width() - 134.0).max(120.0)),
                    );
                    if form::button(ui, "Choose…", true).clicked() {
                        actions.push(Act::PickPatchFolder.into());
                    }
                });
            },
        );
        if let Ok(source) = p.source()
            && !p.out_dir.trim().is_empty()
        {
            let line = git_line(&core::format_patch_args(
                Path::new(p.out_dir.trim()),
                &source,
            ));
            kit::code_block(ui, &line, actions);
        }
        form::actions_row(ui, |ui| {
            let label = if p.creating {
                "Listing commits…"
            } else {
                "Create patches…"
            };
            if form::primary(ui, label, !busy && !p.creating).clicked() {
                actions.push(Act::CreatePatches.into());
            }
        });
    });
    match &p.created {
        Some(Ok((dir, files))) => {
            let dir = dir.clone();
            let files = files.clone();
            kit::callout_with(
                ui,
                Tone::Success,
                &format!(
                    "Wrote {}",
                    format::plural(files.len(), "patch file", "patch files")
                ),
                &display(&dir),
                |ui| {
                    for f in files.iter().take(20) {
                        ui.label(
                            kit::mono(
                                f.file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default(),
                            )
                            .color(pal(ui).text),
                        );
                    }
                    if files.len() > 20 {
                        kit::muted(ui, &format!("and {} more", files.len() - 20));
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if IconButton::new(Icon::Folder, "Open the folder")
                            .compact()
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Action::OpenPath(dir.clone()));
                        }
                        if IconButton::new(Icon::Copy, "Copy the paths")
                            .compact()
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Action::Copy(
                                files
                                    .iter()
                                    .map(|f| display(f))
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                            ));
                        }
                    });
                },
            );
        }
        Some(Err(e)) => kit::callout(ui, Tone::Danger, "git format-patch failed", e),
        None => {}
    }
}

fn applies_badge(a: &Applies) -> (&'static str, Tone) {
    match a {
        Applies::Cleanly => ("Applies cleanly", Tone::Success),
        Applies::WithThreeWay(_) => ("Needs a 3-way merge", Tone::Warning),
        Applies::No(_) => ("Does not apply", Tone::Danger),
    }
}

fn apply_section(
    ui: &mut Ui,
    p: &mut Patches,
    busy: bool,
    pending: bool,
    actions: &mut Vec<Action>,
) {
    let pal = *pal(ui);
    kit::section_with(ui, "Apply patch files", Some(p.files.len()), |ui| {
        if !p.files.is_empty() {
            if IconButton::text("Clear").compact().show(ui).clicked() {
                actions.push(Act::ClearPatchFiles.into());
            }
            if IconButton::new(Icon::Rerun, "Check again")
                .compact()
                .show(ui)
                .clicked()
            {
                actions.push(Act::RecheckPatches.into());
            }
        }
        if IconButton::new(Icon::Plus, "Choose files…")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Act::PickPatchFiles.into());
        }
    });
    if p.files.is_empty() {
        kit::secondary(
            ui,
            "Choose .patch, .diff or .mbox files.\ngitvis checks each one against this repository before anything is applied.",
        );
        return;
    }
    let mut remove = None;
    for (i, item) in p.files.iter().enumerate() {
        let name = item
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        form::card(ui, Tone::Neutral, |ui| {
            ui.horizontal(|ui| {
                kit::subtitle(ui, &name);
                if let Some(Ok(pr)) = &item.preview {
                    let (text, tone) = applies_badge(&pr.applies);
                    kit::badge(ui, text, tone);
                    kit::badge(
                        ui,
                        if pr.is_mailbox() { "Mailbox" } else { "Diff" },
                        Tone::Neutral,
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if IconButton::icon(Icon::Close)
                        .compact()
                        .show(ui)
                        .on_hover_text("Remove from the list")
                        .clicked()
                    {
                        remove = Some(i);
                    }
                });
            });
            ui.label(
                RichText::new(display(&item.path))
                    .size(size::SMALL)
                    .color(pal.text_2),
            );
            match &item.preview {
                None => kit::loading(ui, "Checking…"),
                Some(Err(e)) => kit::callout(ui, Tone::Danger, "Not a patch git can read", e),
                Some(Ok(pr)) => preview_view(ui, pr),
            }
        });
        ui.add_space(4.0);
    }
    if let Some(i) = remove {
        actions.push(Act::RemovePatchFile(i).into());
    }
    form::checkbox(
        ui,
        &mut p.three_way,
        "3-way merge where a patch does not apply cleanly (--3way)",
    );
    let ready = p.ready() && !busy && !pending;
    form::actions_row(ui, |ui| {
        if form::primary(ui, "Apply as commits (git am)…", ready && p.all_mailboxes())
            .on_hover_text("Needs mailbox files (from git format-patch)")
            .clicked()
        {
            actions.push(Act::ApplyPatches { am: true }.into());
        }
        if form::button(ui, "Apply as changes (git apply)…", ready).clicked() {
            actions.push(Act::ApplyPatches { am: false }.into());
        }
    });
    if pending {
        kit::muted(ui, "Finish or abort the stopped git am first.");
    }
}

fn preview_view(ui: &mut Ui, pr: &Preview) {
    let p = *pal(ui);
    if !pr.mails.is_empty() {
        for m in &pr.mails {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if let Some(n) = &m.number {
                    ui.label(kit::mono(n).color(p.text_3));
                }
                ui.label(RichText::new(&m.subject).color(p.text));
            });
            ui.label(
                RichText::new(format!("{}  ·  {}", m.from, m.date))
                    .size(size::SMALL)
                    .color(p.text_2),
            );
        }
        ui.add_space(4.0);
    }
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!(
                "{}  ·  {}",
                format::plural(pr.files.len(), "file", "files"),
                format::bytes(pr.bytes as usize)
            ))
            .size(size::SMALL)
            .color(p.text_2),
        );
        gui::churn(ui, pr.added(), pr.deleted());
    });
    for f in pr.files.iter().take(30) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(kit::mono(&f.path).color(p.text));
            match (f.added, f.deleted) {
                (Some(a), Some(d)) => ui.label(
                    RichText::new(format!("+{a} −{d}"))
                        .size(size::SMALL)
                        .color(p.text_2),
                ),
                _ => ui.label(RichText::new("binary").size(size::SMALL).color(p.text_2)),
            };
        });
    }
    if pr.files.len() > 30 {
        kit::muted(ui, &format!("and {} more files", pr.files.len() - 30));
    }
    for s in &pr.summary {
        kit::muted(ui, s);
    }
    match &pr.applies {
        Applies::Cleanly => {}
        Applies::WithThreeWay(e) => kit::callout(
            ui,
            Tone::Warning,
            "Does not apply as it is",
            &format!(
                "With --3way git merges it and may leave conflicts. git apply --check said: {}",
                e.trim()
            ),
        ),
        Applies::No(e) => kit::callout(ui, Tone::Danger, "Does not apply here", e.trim()),
    }
}
