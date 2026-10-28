//! More of git: Tools page tabs (Stashes, Submodules, Git LFS, Patches,
//! Hooks), "Stash this file…" in the WIP panel, "Split this commit…" in the
//! commit menu, and `gitvis hook pre-commit` (the secret check as a real
//! git hook).
//!
//! One file per tab: each holds the tab's state, its controller (`apply` /
//! `on_msg`) and its view; this file owns the contract the dispatcher calls.

mod githooks_tab;
mod lfs_tab;
mod patches_tab;
mod split;
mod stashes;
mod submodules_tab;
mod ui;

use std::path::{Path, PathBuf};

use gitvis_core::devops::gitplus::lfs::{Lock, Overview as LfsOverview};
use gitvis_core::devops::gitplus::patches::{Preview, RangeCommit, Source};
use gitvis_core::devops::gitplus::precommit::SemgrepSummary;
use gitvis_core::devops::gitplus::split::SplitPlan;
use gitvis_core::devops::gitplus::stash::{StashAction, StashEntry};
use gitvis_core::devops::gitplus::submodules::Overview as SubOverview;
use serde::{Deserialize, Serialize};

use crate::devops::console::{Console, ConsoleRun};
use crate::devops::ctx::Ctx;
use crate::state::{Action, Page, RepoState, Settings};
use crate::theme::Tone;
use gitvis_core::Commit;

use crate::devops::hooks::{HookCx, WipList};

pub use lfs_tab::LfsCmd;
pub use submodules_tab::SubCmd;

/// Tools page tabs drawn here (see `workbench::Tab::gitplus`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Tab {
    #[default]
    Submodules,
    Lfs,
    Patches,
    Hooks,
    Stashes,
}

impl Tab {
    fn workbench(self) -> crate::devops::workbench::Tab {
        use crate::devops::workbench::Tab as W;
        match self {
            Tab::Submodules => W::Submodules,
            Tab::Lfs => W::Lfs,
            Tab::Patches => W::Patches,
            Tab::Hooks => W::Hooks,
            Tab::Stashes => W::Stashes,
        }
    }
}

#[derive(Default)]
pub struct State {
    pub stashes: stashes::Stashes,
    pub submodules: submodules_tab::Submodules,
    pub lfs: lfs_tab::Lfs,
    pub patches: patches_tab::Patches,
    pub hooks: githooks_tab::Hooks,
    pub split: split::Split,
    /// HEAD and working-tree signature of the last snapshot, so a visible
    /// tab refreshes only when something it shows may have changed.
    last_sig: Option<(Option<String>, usize, usize)>,
}

#[derive(Clone, Debug)]
pub enum Act {
    // Stashes
    LoadStashes,
    SelectStash(Option<String>),
    SelectStashIndex(usize),
    SelectStashFile(usize),
    StashAction {
        index: usize,
        sha: String,
        action: StashAction,
    },
    StashActionConfirmed {
        index: usize,
        sha: String,
        action: StashAction,
    },
    StashBranch {
        index: usize,
        sha: String,
    },
    StashBranchConfirmed {
        index: usize,
        sha: String,
        name: String,
    },
    /// Open the "Stash selected files" form with these files ticked.
    OpenStashForm {
        paths: Vec<String>,
        message: Option<String>,
    },
    CloseStashForm,
    StashFiles,
    StashFilesConfirmed {
        message: String,
        keep_index: bool,
        include_untracked: bool,
        paths: Vec<String>,
    },
    // Submodules
    LoadSubmodules,
    SelectSubmodule(Option<String>),
    SelectSubmoduleIndex(usize),
    SubmoduleCmd {
        kind: SubCmd,
        paths: Vec<String>,
    },
    SubmoduleCmdConfirmed {
        kind: SubCmd,
        paths: Vec<String>,
    },
    AddSubmodule,
    AddSubmoduleConfirmed {
        url: String,
        path: String,
        branch: String,
    },
    RemoveSubmodule {
        name: String,
        path: String,
    },
    RemoveSubmoduleConfirmed {
        name: String,
        path: String,
    },
    // Git LFS
    LoadLfs,
    LfsTrack,
    LfsTrackConfirmed {
        pattern: String,
        lockable: bool,
    },
    LfsUntrack(String),
    LfsUntrackConfirmed(String),
    LfsCmd(LfsCmd),
    LfsCmdConfirmed(LfsCmd),
    LfsPrunePreview,
    LoadLocks,
    LfsLock(String),
    LfsLockConfirmed(String),
    LfsUnlock {
        path: String,
        force: bool,
    },
    LfsUnlockConfirmed {
        path: String,
        force: bool,
    },
    // Patches
    PickPatchFolder,
    CreatePatches,
    CreatePatchesConfirmed {
        source: Source,
        out_dir: PathBuf,
    },
    PickPatchFiles,
    PreviewPatch(PathBuf),
    RemovePatchFile(usize),
    ClearPatchFiles,
    RecheckPatches,
    ApplyPatches {
        am: bool,
    },
    ApplyPatchesConfirmed {
        files: Vec<PathBuf>,
        am: bool,
        three_way: bool,
    },
    /// `continue`, `skip` or `abort` of a stopped `git am`.
    AmStep(String),
    AmStepConfirmed(String),
    LoadAmCurrent,
    // Hooks
    LoadHooks,
    CheckTools,
    SelectHook(Option<String>),
    /// Select by file or hook name (scripts).
    SelectHookName(String),
    RevertHookEdit,
    SaveHook,
    SaveHookConfirmed {
        file: String,
        content: String,
    },
    EnableHook(String),
    EnableHookConfirmed(String),
    DisableHook(String),
    DisableHookConfirmed(String),
    NewHook,
    NewHookConfirmed(String),
    InstallSecretHook,
    InstallSecretHookConfirmed,
    RemoveSecretHook,
    RemoveSecretHookConfirmed,
    SyncDismissed,
    SyncDismissedConfirmed,
    RunPreCommit {
        all: bool,
    },
    RunPreCommitConfirmed {
        all: bool,
    },
    RunSemgrep,
    RunSemgrepConfirmed,
    /// Show a console run on the Tools page's Console tab.
    ShowConsoleRun(u64),
    // Split
    /// A sha, or `head`.
    Split(String),
    SplitConfirmed(Box<SplitPlan>),
    /// Switch the Tools page to one of these tabs.
    #[allow(dead_code)] // for other areas (palette entries)
    ShowTab(Tab),
}

pub enum Msg {
    Stashes(Result<Vec<StashEntry>, String>),
    StashPatch(String, Result<Vec<stashes::StashDiff>, String>),
    StashChanged { ok: bool, close_form: bool },
    Submodules(Result<SubOverview, String>),
    SubmoduleChanged,
    Lfs(Box<LfsOverview>),
    LfsChanged,
    Locks(Result<Vec<Lock>, String>),
    LocksChanged,
    LfsPrune(Result<String, String>),
    PatchRange(Source, PathBuf, Result<Vec<RangeCommit>, String>),
    PatchesCreated(Result<(PathBuf, Vec<PathBuf>), String>),
    PatchPreview(PathBuf, Result<Preview, String>),
    PatchesApplied,
    AmCurrent(Result<String, String>),
    Hooks(Result<githooks_tab::Listing, String>),
    HookContent(String, Result<String, String>),
    HookChanged,
    Tools(githooks_tab::ToolCheck),
    SemgrepParsed(u64, Result<SemgrepSummary, String>),
    SplitPlanned(Result<Box<SplitPlan>, String>),
    SplitDone(Option<Box<SplitPlan>>),
}

impl Msg {
    /// One line for the activity log (`Err` when the work failed).
    pub fn summary(&self) -> Result<String, String> {
        use crate::format::plural;
        use gitvis_core::devops::gitplus::patches::Applies;
        match self {
            Msg::Stashes(Ok(l)) => Ok(plural(l.len(), "stash", "stashes")),
            Msg::Stashes(Err(e)) => Err(e.clone()),
            Msg::StashPatch(_, Ok(f)) => {
                Ok(format!("Stash diff: {}", plural(f.len(), "file", "files")))
            }
            Msg::StashPatch(_, Err(e)) => Err(e.clone()),
            Msg::Submodules(Ok(o)) => Ok(format!(
                "{} · {} ms",
                plural(o.modules.len(), "submodule", "submodules"),
                o.elapsed_ms
            )),
            Msg::Submodules(Err(e)) => Err(e.clone()),
            Msg::Lfs(o) => match &o.version {
                Some(v) => Ok(format!(
                    "Git LFS {v}: {}, {} · {} ms",
                    plural(o.patterns.len(), "pattern", "patterns"),
                    plural(o.files.len(), "file", "files"),
                    o.elapsed_ms
                )),
                None => Ok("Git LFS is not installed".to_owned()),
            },
            Msg::Locks(Ok(l)) => Ok(plural(l.len(), "LFS lock", "LFS locks")),
            Msg::Locks(Err(e)) => Err(e.clone()),
            Msg::LfsPrune(r) => r.clone().map(|t| {
                t.lines()
                    .last()
                    .unwrap_or("git lfs prune --dry-run")
                    .to_owned()
            }),
            Msg::PatchRange(s, _, Ok(c)) => Ok(format!(
                "{} in {}",
                plural(c.len(), "commit", "commits"),
                s.describe()
            )),
            Msg::PatchRange(_, _, Err(e)) => Err(e.clone()),
            Msg::PatchPreview(p, Ok(pr)) => Ok(format!(
                "{}: {}, {}",
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                plural(pr.files.len(), "file", "files"),
                match pr.applies {
                    Applies::Cleanly => "applies cleanly",
                    Applies::WithThreeWay(_) => "needs a 3-way merge",
                    Applies::No(_) => "does not apply",
                }
            )),
            Msg::PatchPreview(_, Err(e)) => Err(e.clone()),
            Msg::AmCurrent(r) => r.clone().map(|_| "The patch git am stopped on".to_owned()),
            Msg::Hooks(Ok(l)) => Ok(format!(
                "{} in {}",
                plural(l.files.len(), "hook file", "hook files"),
                l.dir.display()
            )),
            Msg::Hooks(Err(e)) => Err(e.clone()),
            Msg::HookContent(f, r) => r
                .as_ref()
                .map(|t| format!("{f}: {}", plural(t.lines().count(), "line", "lines")))
                .map_err(Clone::clone),
            Msg::Tools(t) => Ok(format!(
                "pre-commit {}, semgrep {}",
                if t.precommit.is_some() {
                    "found"
                } else {
                    "not installed"
                },
                if t.semgrep.is_some() {
                    "found"
                } else {
                    "not installed"
                }
            )),
            Msg::SemgrepParsed(_, Ok(s)) => Ok(format!(
                "semgrep: {}",
                plural(s.findings.len(), "finding", "findings")
            )),
            Msg::SemgrepParsed(_, Err(e)) => Err(e.clone()),
            Msg::SplitPlanned(Ok(p)) => Ok(format!("Split plan for \"{}\"", p.subject)),
            Msg::SplitPlanned(Err(e)) => Err(e.clone()),
            // Changes are logged by `Ctx::local_change` itself.
            Msg::StashChanged { .. }
            | Msg::SubmoduleChanged
            | Msg::LfsChanged
            | Msg::LocksChanged
            | Msg::PatchesCreated(_)
            | Msg::PatchesApplied
            | Msg::HookChanged
            | Msg::SplitDone(_) => Ok(String::new()),
        }
    }
}

/// Persisted per repository.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Folder for created patch files.
    pub patch_dir: String,
    /// 3-way merge when applying patches.
    pub patch_three_way: Option<bool>,
}

impl State {
    pub fn prefs(&self) -> Prefs {
        Prefs {
            patch_dir: self.patches.out_dir.clone(),
            patch_three_way: Some(self.patches.three_way),
        }
    }

    pub fn apply_prefs(&mut self, p: &Prefs) {
        if !p.patch_dir.is_empty() {
            self.patches.out_dir = p.patch_dir.clone();
        }
        if let Some(t) = p.patch_three_way {
            self.patches.three_way = t;
        }
    }
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::LoadStashes
        | Act::SelectStash(_)
        | Act::SelectStashIndex(_)
        | Act::SelectStashFile(_)
        | Act::StashAction { .. }
        | Act::StashActionConfirmed { .. }
        | Act::StashBranch { .. }
        | Act::StashBranchConfirmed { .. }
        | Act::OpenStashForm { .. }
        | Act::CloseStashForm
        | Act::StashFiles
        | Act::StashFilesConfirmed { .. } => stashes::apply(cx, repo, act),
        Act::LoadSubmodules
        | Act::SelectSubmodule(_)
        | Act::SelectSubmoduleIndex(_)
        | Act::SubmoduleCmd { .. }
        | Act::SubmoduleCmdConfirmed { .. }
        | Act::AddSubmodule
        | Act::AddSubmoduleConfirmed { .. }
        | Act::RemoveSubmodule { .. }
        | Act::RemoveSubmoduleConfirmed { .. } => submodules_tab::apply(cx, repo, act),
        Act::LoadLfs
        | Act::LfsTrack
        | Act::LfsTrackConfirmed { .. }
        | Act::LfsUntrack(_)
        | Act::LfsUntrackConfirmed(_)
        | Act::LfsCmd(_)
        | Act::LfsCmdConfirmed(_)
        | Act::LfsPrunePreview
        | Act::LoadLocks
        | Act::LfsLock(_)
        | Act::LfsLockConfirmed(_)
        | Act::LfsUnlock { .. }
        | Act::LfsUnlockConfirmed { .. } => lfs_tab::apply(cx, repo, act),
        Act::PickPatchFolder
        | Act::CreatePatches
        | Act::CreatePatchesConfirmed { .. }
        | Act::PickPatchFiles
        | Act::PreviewPatch(_)
        | Act::RemovePatchFile(_)
        | Act::ClearPatchFiles
        | Act::RecheckPatches
        | Act::ApplyPatches { .. }
        | Act::ApplyPatchesConfirmed { .. }
        | Act::AmStep(_)
        | Act::AmStepConfirmed(_)
        | Act::LoadAmCurrent => patches_tab::apply(cx, repo, act),
        Act::LoadHooks
        | Act::CheckTools
        | Act::SelectHook(_)
        | Act::SelectHookName(_)
        | Act::RevertHookEdit
        | Act::SaveHook
        | Act::SaveHookConfirmed { .. }
        | Act::EnableHook(_)
        | Act::EnableHookConfirmed(_)
        | Act::DisableHook(_)
        | Act::DisableHookConfirmed(_)
        | Act::NewHook
        | Act::NewHookConfirmed(_)
        | Act::InstallSecretHook
        | Act::InstallSecretHookConfirmed
        | Act::RemoveSecretHook
        | Act::RemoveSecretHookConfirmed
        | Act::SyncDismissed
        | Act::SyncDismissedConfirmed
        | Act::RunPreCommit { .. }
        | Act::RunPreCommitConfirmed { .. }
        | Act::RunSemgrep
        | Act::RunSemgrepConfirmed
        | Act::ShowConsoleRun(_) => githooks_tab::apply(cx, repo, act),
        Act::Split(_) | Act::SplitConfirmed(_) => split::apply(cx, repo, act),
        Act::ShowTab(t) => {
            if repo.page != Page::Workbench {
                cx.action(Action::ShowPage(Page::Workbench));
            }
            cx.action(crate::devops::workbench::Act::SetTab(t.workbench()));
        }
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    match msg {
        Msg::Stashes(_) | Msg::StashPatch(..) | Msg::StashChanged { .. } => {
            stashes::on_msg(cx, repo, msg)
        }
        Msg::Submodules(_) | Msg::SubmoduleChanged => submodules_tab::on_msg(cx, repo, msg),
        Msg::Lfs(_) | Msg::LfsChanged | Msg::Locks(_) | Msg::LocksChanged | Msg::LfsPrune(_) => {
            lfs_tab::on_msg(cx, repo, msg)
        }
        Msg::PatchRange(..)
        | Msg::PatchesCreated(_)
        | Msg::PatchPreview(..)
        | Msg::PatchesApplied
        | Msg::AmCurrent(_) => patches_tab::on_msg(cx, repo, msg),
        Msg::Hooks(_)
        | Msg::HookContent(..)
        | Msg::HookChanged
        | Msg::Tools(_)
        | Msg::SemgrepParsed(..) => githooks_tab::on_msg(cx, repo, msg),
        Msg::SplitPlanned(_) | Msg::SplitDone(_) => split::on_msg(cx, repo, msg),
    }
}

/// A console run of this repository ended.
pub fn on_console_done(cx: &mut Ctx, repo: &mut RepoState, run: &ConsoleRun) {
    githooks_tab::on_console_done(cx, repo, run);
}

/// The tab of this area the Tools page shows, if any.
fn visible_tab(repo: &RepoState) -> Option<Tab> {
    (repo.page == Page::Workbench)
        .then(|| repo.devops.workbench.tab.gitplus())
        .flatten()
}

/// After every load.
pub fn on_snapshot(cx: &mut Ctx, repo: &mut RepoState) {
    let Some(snap) = repo.snapshot.as_ref() else {
        return;
    };
    let sig = (
        snap.head.target().map(|o| o.to_string()),
        snap.status.change_count(),
        snap.stashes.len(),
    );
    let changed = repo.devops.gitplus.last_sig.as_ref() != Some(&sig);
    repo.devops.gitplus.last_sig = Some(sig);
    stashes::on_snapshot(cx, repo);
    patches_tab::on_snapshot(cx, repo);
    if !changed {
        return;
    }
    match visible_tab(repo) {
        Some(Tab::Submodules) if repo.devops.gitplus.submodules.overview.is_some() => {
            submodules_tab::load(cx, repo)
        }
        Some(Tab::Lfs) if repo.devops.gitplus.lfs.overview.is_some() => lfs_tab::load(cx, repo),
        _ => {}
    }
}

/// One of this area's Tools tabs became visible.
pub fn on_tab_shown(cx: &mut Ctx, repo: &mut RepoState, tab: Tab) {
    match tab {
        Tab::Stashes => stashes::shown(cx, repo),
        Tab::Submodules => submodules_tab::shown(cx, repo),
        Tab::Lfs => lfs_tab::shown(cx, repo),
        Tab::Patches => patches_tab::shown(cx, repo),
        Tab::Hooks => githooks_tab::shown(cx, repo),
    }
}

pub fn show_tab(
    ui: &mut egui::Ui,
    repo: &mut RepoState,
    console: &Console,
    settings: &Settings,
    tab: Tab,
    actions: &mut Vec<Action>,
) {
    match tab {
        Tab::Stashes => stashes::show(ui, repo, actions),
        Tab::Submodules => submodules_tab::show(ui, repo, actions),
        Tab::Lfs => lfs_tab::show(ui, repo, settings, actions),
        Tab::Patches => patches_tab::show(ui, repo, actions),
        Tab::Hooks => githooks_tab::show(ui, repo, console, actions),
    }
}

/// A count for the Tools page navigation (`workbench::nav_counts` can show
/// it): stashes, submodules that need attention, LFS files, a stopped
/// `git am`, enabled hooks.
#[allow(dead_code)] // for the workbench navigation (shared-file request)
pub fn nav_count(repo: &RepoState, tab: Tab) -> Option<(usize, Tone)> {
    use gitvis_core::devops::gitplus::submodules::SubState;
    let g = &repo.devops.gitplus;
    match tab {
        Tab::Stashes => repo
            .snapshot
            .as_ref()
            .map(|s| s.stashes.len())
            .filter(|n| *n > 0)
            .map(|n| (n, Tone::Neutral)),
        Tab::Submodules => match &g.submodules.overview {
            Some(Ok(o)) => {
                let attention = o
                    .modules
                    .iter()
                    .filter(|m| m.state != SubState::Current || !m.dirty.is_empty())
                    .count();
                if attention > 0 {
                    Some((attention, Tone::Warning))
                } else {
                    (!o.modules.is_empty()).then_some((o.modules.len(), Tone::Neutral))
                }
            }
            _ => None,
        },
        Tab::Lfs => g
            .lfs
            .overview
            .as_ref()
            .filter(|o| !o.files.is_empty())
            .map(|o| (o.files.len(), Tone::Neutral)),
        Tab::Patches => (repo.snapshot.as_ref().map(|s| s.pending)
            == Some(gitvis_core::Pending::ApplyMailbox))
        .then_some((1, Tone::Warning)),
        Tab::Hooks => match &g.hooks.listing {
            Some(Ok(l)) => {
                let n = l.files.iter().filter(|f| f.enabled).count();
                (n > 0).then_some((n, Tone::Neutral))
            }
            _ => None,
        },
    }
}

fn tab_actions(tab: Tab) -> Vec<Action> {
    vec![
        Action::ShowPage(Page::Workbench),
        crate::devops::workbench::Act::SetTab(tab.workbench()).into(),
    ]
}

/// `GITVIS_SCRIPT` commands this area understands:
/// `stashes`, `stash N`, `stash-files PATH1,PATH2 MESSAGE` (opens the
/// form), `stash-form`, `submodules`, `submodule N`, `lfs`, `lfs-locks`
/// (network), `patches`, `patch-preview PATH`, `hooks`, `hook-view NAME`,
/// `install-secret-hook` (stops at the confirmation), `split-commit
/// SHA|head` (stops at the confirmation; the built-in `split` is the diff
/// view's).
pub fn script(cmd: &str, arg: &str) -> Option<Vec<Action>> {
    let arg = arg.trim();
    let num = arg.parse::<usize>().ok();
    Some(match cmd {
        "stashes" => tab_actions(Tab::Stashes),
        "stash" => {
            let mut v = tab_actions(Tab::Stashes);
            v.push(Act::SelectStashIndex(num?).into());
            v
        }
        "stash-form" => vec![
            Act::OpenStashForm {
                paths: Vec::new(),
                message: None,
            }
            .into(),
        ],
        "stash-files" => {
            let (paths, message) = match arg.split_once(' ') {
                Some((p, m)) => (p, Some(m.trim().to_owned()).filter(|m| !m.is_empty())),
                None => (arg, None),
            };
            let paths: Vec<String> = paths
                .split(',')
                .map(|p| p.trim().replace('\\', "/"))
                .filter(|p| !p.is_empty())
                .collect();
            vec![Act::OpenStashForm { paths, message }.into()]
        }
        "submodules" => tab_actions(Tab::Submodules),
        "submodule" => {
            let mut v = tab_actions(Tab::Submodules);
            v.push(Act::SelectSubmoduleIndex(num?).into());
            v
        }
        "lfs" => tab_actions(Tab::Lfs),
        "lfs-locks" => vec![Act::LoadLocks.into()],
        "patches" => tab_actions(Tab::Patches),
        "patch-preview" if !arg.is_empty() => {
            let mut v = tab_actions(Tab::Patches);
            v.push(Act::PreviewPatch(PathBuf::from(arg)).into());
            v
        }
        "hooks" => tab_actions(Tab::Hooks),
        "hook-view" if !arg.is_empty() => {
            let mut v = tab_actions(Tab::Hooks);
            v.push(Act::SelectHookName(arg.to_owned()).into());
            v
        }
        "install-secret-hook" => {
            let mut v = tab_actions(Tab::Hooks);
            v.push(Act::SelectHook(None).into());
            v.push(Act::InstallSecretHook.into());
            v
        }
        "split-commit" if !arg.is_empty() => vec![Act::Split(arg.to_owned()).into()],
        _ => return None,
    })
}

pub fn hook_commit_menu(ui: &mut egui::Ui, h: &HookCx, c: &Commit, actions: &mut Vec<Action>) {
    // Merges and the first commit cannot be split; the rest is checked
    // when the item is picked (it must be on the current branch).
    if c.parent_ids.len() != 1 {
        return;
    }
    let head = h.snap.head.target() == Some(c.id);
    let label = if head {
        "Split the last commit…"
    } else {
        "Split this commit…"
    };
    // While an operation runs the item stays: picking it explains why it
    // has to wait.
    crate::views::menus::action(ui, label, actions, Act::Split(c.id.to_string()).into());
}

pub fn hook_wip_file_menu(
    ui: &mut egui::Ui,
    _h: &HookCx,
    path: &str,
    list: WipList,
    actions: &mut Vec<Action>,
) {
    if list == WipList::Conflicted {
        return;
    }
    if ui.button("Stash this file…").clicked() {
        actions.push(
            Act::OpenStashForm {
                paths: vec![path.to_owned()],
                message: None,
            }
            .into(),
        );
        ui.close();
    }
}

/// Run a gitvis hook in `dir`: the text for standard error and the exit
/// code.
pub fn run_hook(name: &str, dir: &Path) -> (String, i32) {
    match name {
        "pre-commit" => gitvis_core::devops::gitplus::secret_hook::run(dir),
        "" => (
            "usage: gitvis hook pre-commit\nRuns the gitvis secret check on the staged changes (installed from Tools > Hooks).\n".to_owned(),
            2,
        ),
        other => (
            format!("gitvis: no hook named \"{other}\"; the one gitvis provides is pre-commit.\n"),
            2,
        ),
    }
}

/// `gitvis hook <name> [args]` run by git as a hook (before the UI starts).
/// Returns the exit code, or `None` when the arguments are not a hook call.
pub fn cli_main(args: &[std::ffi::OsString]) -> Option<i32> {
    if args.first().and_then(|a| a.to_str()) != Some("hook") {
        return None;
    }
    let name = args.get(1).and_then(|a| a.to_str()).unwrap_or("");
    let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (text, code) = run_hook(name, &dir);
    eprint!("{text}");
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::DevOpsAction;
    use std::ffi::OsString;
    use std::process::Command;

    fn acts(cmd: &str, arg: &str) -> Vec<Act> {
        script(cmd, arg)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|a| match a {
                Action::DevOps(DevOpsAction::Gitplus(a)) => Some(a),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn script_commands() {
        assert!(matches!(
            acts("stash-files", "a.txt,dir\\b c.txt Some message").as_slice(),
            [Act::OpenStashForm { paths, message: Some(m) }]
                if paths == &["a.txt", "dir/b"] && m == "c.txt Some message"
        ));
        assert!(matches!(
            acts("stash-files", "a.txt").as_slice(),
            [Act::OpenStashForm { paths, message: None }] if paths == &["a.txt"]
        ));
        assert!(matches!(
            acts("stash", "2").as_slice(),
            [Act::SelectStashIndex(2)]
        ));
        assert!(script("stash", "x").is_none());
        assert!(matches!(
            acts("split-commit", "head").as_slice(),
            [Act::Split(r)] if r == "head"
        ));
        assert!(script("split-commit", "").is_none());
        assert!(matches!(
            acts("patch-preview", "C:/p/0001.patch").as_slice(),
            [Act::PreviewPatch(p)] if p == Path::new("C:/p/0001.patch")
        ));
        assert!(matches!(
            acts("hook-view", "pre-commit").as_slice(),
            [Act::SelectHookName(n)] if n == "pre-commit"
        ));
        assert!(matches!(
            acts("install-secret-hook", "").as_slice(),
            [Act::SelectHook(None), Act::InstallSecretHook]
        ));
        for c in ["stashes", "submodules", "lfs", "patches", "hooks"] {
            let v = script(c, "").unwrap();
            assert!(matches!(v[0], Action::ShowPage(Page::Workbench)), "{c}");
        }
        assert!(script("not-ours", "").is_none());
    }

    #[test]
    fn cli_main_only_takes_hook_calls() {
        assert_eq!(cli_main(&[]), None);
        assert_eq!(cli_main(&[OsString::from("C:/repo")]), None);
        let (text, code) = run_hook("", Path::new("."));
        assert_eq!(code, 2);
        assert!(text.contains("usage"));
        assert_eq!(run_hook("pre-push", Path::new(".")).1, 2);
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn pre_commit_hook_exit_codes() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("gitvis-cli-hook-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["config", "user.name", "T"]);
        git(&dir, &["config", "user.email", "t@example.com"]);
        std::fs::write(dir.join("notes.md"), "plain text\n").unwrap();
        git(&dir, &["add", "notes.md"]);
        let (text, code) = run_hook("pre-commit", &dir);
        assert_eq!(code, 0, "{text}");
        // A FAKE key, assembled at run time.
        let token = format!("AKIA{}", "J6ZQMPTX3V7UBNRQ");
        std::fs::write(
            dir.join("deploy.yml"),
            format!("aws_access_key_id: {token}\n"),
        )
        .unwrap();
        git(&dir, &["add", "deploy.yml"]);
        let (text, code) = run_hook("pre-commit", &dir);
        assert_eq!(code, 1, "{text}");
        assert!(text.contains("deploy.yml:1"), "{text}");
        assert!(!text.contains(&token));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn short_paths() {
        assert_eq!(
            ui::short_path(
                Path::new("C:/Work/Repo/.git/hooks"),
                Path::new(r"c:\work\repo")
            ),
            ".git/hooks"
        );
        assert_eq!(
            ui::short_path(Path::new("/srv/hooks"), Path::new("/home/me/repo")),
            ui::native_path(Path::new("/srv/hooks"))
        );
    }

    #[test]
    fn nav_counts_need_data() {
        let repo = RepoState::new(1, PathBuf::from("."));
        for t in [
            Tab::Stashes,
            Tab::Submodules,
            Tab::Lfs,
            Tab::Patches,
            Tab::Hooks,
        ] {
            assert!(nav_count(&repo, t).is_none());
        }
    }

    #[test]
    fn prefs_round_trip() {
        let mut s = State::default();
        s.patches.out_dir = "C:/p".into();
        s.patches.three_way = false;
        let json = serde_json::to_string(&s.prefs()).unwrap();
        let back: Prefs = serde_json::from_str(&json).unwrap();
        let mut t = State::default();
        t.apply_prefs(&back);
        assert_eq!(t.patches.out_dir, "C:/p");
        assert!(!t.patches.three_way);
        let empty: Prefs = serde_json::from_str("{}").unwrap();
        let mut u = State::default();
        u.apply_prefs(&empty);
        assert!(u.patches.three_way);
    }
}
