//! Hooks tab: the hooks in `core.hooksPath` or `.git/hooks` (enabled,
//! executable, size, first line), viewing and editing one, enabling and
//! disabling by renaming `.sample`, the gitvis secret check as the
//! `pre-commit` hook, and the `pre-commit` framework and `semgrep` run in
//! the console.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{RichText, Ui};
use gitvis_core::devops::gitplus::githooks::{self as core, HookFile};
use gitvis_core::devops::gitplus::precommit::{
    self, ConfigRepo, HookResult, Outcome, SemgrepSummary,
};
use gitvis_core::devops::gitplus::secret_hook;
use gitvis_core::devops::proc::{ProcSpec, resolve, run_capture};
use gitvis_core::devops::workbench::shell::sh_path;

use super::{Act, Msg, ui as gui};
use crate::devops::console::{Console, ConsoleRun};
use crate::devops::ctx::Ctx;
use crate::format;
use crate::state::{Action, RepoState, ToastKind};
use crate::theme::{pal, size};
use crate::views::form::{self};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Row, Stat, Tone};

pub const PRECOMMIT_HINT: &str = "pip install pre-commit (or pipx install pre-commit), then pre-commit install in the repository";
pub const SEMGREP_HINT: &str =
    "pip install semgrep (or pipx install semgrep; brew install semgrep on macOS)";

/// What the folder holds, read in the background.
#[derive(Clone, Debug)]
pub struct Listing {
    pub dir: PathBuf,
    /// `core.hooksPath` when set.
    pub custom: Option<String>,
    pub files: Vec<HookFile>,
    pub common: Option<PathBuf>,
    /// Fingerprints in the hook's dismissed-findings file.
    pub dismissed_in_file: Option<HashSet<String>>,
    /// The exe the installed secret check calls.
    pub gitvis_exe: Option<String>,
    pub precommit: Option<(PathBuf, Result<Vec<ConfigRepo>, String>)>,
}

impl Listing {
    fn gitvis(&self) -> Option<&HookFile> {
        self.files
            .iter()
            .find(|f| f.gitvis && f.file == "pre-commit")
    }
}

/// Installed checkers: path and version.
#[derive(Clone, Debug, Default)]
pub struct ToolCheck {
    pub precommit: Option<(PathBuf, String)>,
    pub semgrep: Option<(PathBuf, String)>,
}

pub struct SemgrepRun {
    pub id: u64,
    pub report: PathBuf,
    pub summary: Option<Result<SemgrepSummary, String>>,
}

pub struct PreCommitRun {
    pub id: u64,
    pub all: bool,
    pub results: Option<Vec<HookResult>>,
}

#[derive(Default)]
pub struct Hooks {
    pub listing: Option<Result<Listing, String>>,
    pub loading: bool,
    pub at: Option<Instant>,
    /// File name of the selected hook.
    pub selected: Option<String>,
    /// A hook name asked for before the listing arrived (scripts).
    pub pending_select: Option<String>,
    /// Content of `content_for` as read from disk.
    pub content: Option<Result<String, String>>,
    pub content_for: Option<String>,
    pub editor: String,
    pub new_hook: String,
    pub busy: bool,
    pub tools: Option<ToolCheck>,
    pub tools_loading: bool,
    pub semgrep: Option<SemgrepRun>,
    pub precommit_run: Option<PreCommitRun>,
}

impl Hooks {
    fn listing(&self) -> Option<&Listing> {
        self.listing.as_ref().and_then(|l| l.as_ref().ok())
    }
}

/// The running gitvis, as the hook script would call it.
pub fn current_exe() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

pub fn load(cx: &mut Ctx, repo: &mut RepoState) {
    let h = &mut repo.devops.gitplus.hooks;
    if h.loading {
        return;
    }
    h.loading = true;
    cx.read("List git hooks", |dir, log| {
        let mut cmds = Vec::new();
        let r = core::hooks_dir(dir, &mut cmds).and_then(|(hooks, custom)| {
            log.command(format!("ls {}", sh_path(&hooks)));
            let files = core::list(&hooks)?;
            let common = secret_hook::common_dir(dir);
            cmds.push("git rev-parse --git-common-dir".into());
            let gitvis_exe = files
                .iter()
                .find(|f| f.gitvis && f.file == "pre-commit")
                .and_then(|f| std::fs::read_to_string(&f.path).ok())
                .and_then(|t| core::installed_exe(&t));
            let dismissed_in_file = common.as_ref().and_then(|c| {
                let p = c.join(secret_hook::DISMISSED_FILE);
                p.is_file().then(|| secret_hook::read_dismissed(c))
            });
            let precommit = precommit::config_path(dir).map(|p| {
                let r = std::fs::read_to_string(&p)
                    .map_err(|e| e.to_string())
                    .and_then(|t| precommit::parse_config(&t));
                (p, r)
            });
            Ok(Listing {
                dir: hooks,
                custom: custom.filter(|c| !c.is_empty()),
                files,
                common,
                dismissed_in_file,
                gitvis_exe,
                precommit,
            })
        });
        log.commands.extend(cmds);
        Msg::Hooks(r)
    });
}

fn check_tools(cx: &mut Ctx, repo: &mut RepoState) {
    let h = &mut repo.devops.gitplus.hooks;
    if h.tools_loading {
        return;
    }
    h.tools_loading = true;
    cx.read("Look for pre-commit and semgrep", |dir, log| {
        let mut probe = |program: &str| -> Option<(PathBuf, String)> {
            let path = resolve(program)?;
            log.command(format!("{program} --version"));
            let spec = ProcSpec::new(path.to_string_lossy().into_owned(), dir).arg("--version");
            let version = run_capture(&spec, Some(Duration::from_secs(20)))
                .ok()
                .map(|c| {
                    let t = if c.stdout.is_empty() {
                        c.stderr.clone()
                    } else {
                        c.stdout_text()
                    };
                    t.lines()
                        .map(str::trim)
                        .find(|l| !l.is_empty())
                        .unwrap_or("")
                        .to_owned()
                })
                .unwrap_or_default();
            Some((path, version))
        };
        let tools = ToolCheck {
            precommit: probe("pre-commit"),
            semgrep: probe("semgrep"),
        };
        Msg::Tools(tools)
    });
}

fn load_content(cx: &mut Ctx, repo: &mut RepoState, file: &str) {
    let Some(path) = repo
        .devops
        .gitplus
        .hooks
        .listing()
        .and_then(|l| l.files.iter().find(|f| f.file == file))
        .map(|f| f.path.clone())
    else {
        return;
    };
    let h = &mut repo.devops.gitplus.hooks;
    h.content = None;
    h.content_for = Some(file.to_owned());
    let file = file.to_owned();
    cx.read(format!("Read the {file} hook"), move |_, log| {
        log.command(format!("cat {}", sh_path(&path)));
        let r = std::fs::read(&path)
            .map_err(|e| format!("Could not read {}: {e}", path.display()))
            .and_then(|b| {
                if b.iter().take(8000).any(|&c| c == 0) {
                    Err("A binary file: it cannot be edited here.".to_owned())
                } else {
                    Ok(String::from_utf8_lossy(&b).replace("\r\n", "\n"))
                }
            });
        Msg::HookContent(file, r)
    });
}

pub fn shown(cx: &mut Ctx, repo: &mut RepoState) {
    load(cx, repo);
    if repo.devops.gitplus.hooks.tools.is_none() {
        check_tools(cx, repo);
    }
}

fn change(
    cx: &mut Ctx,
    repo: &mut RepoState,
    label: String,
    run: impl FnOnce(&Path, &mut Vec<String>) -> Result<String, String> + Send + 'static,
) {
    let h = &mut repo.devops.gitplus.hooks;
    if h.busy {
        return;
    }
    h.busy = true;
    cx.local_change(label, move |dir, log| {
        let mut cmds = Vec::new();
        let r = run(dir, &mut cmds);
        log.commands.extend(cmds);
        (r, Some(Msg::HookChanged))
    });
}

fn dismissed_of(repo: &RepoState) -> Vec<String> {
    let mut v: Vec<String> = repo.dismissed.iter().cloned().collect();
    v.sort();
    v
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::LoadHooks => load(cx, repo),
        Act::CheckTools => {
            gitvis_core::devops::tools::refresh();
            repo.devops.gitplus.hooks.tools = None;
            check_tools(cx, repo);
        }
        Act::SelectHook(file) => {
            let h = &mut repo.devops.gitplus.hooks;
            h.selected = file.clone();
            h.editor.clear();
            h.content = None;
            h.content_for = None;
            if let Some(f) = file {
                load_content(cx, repo, &f);
            }
        }
        Act::SelectHookName(name) => {
            let h = &mut repo.devops.gitplus.hooks;
            match h.listing() {
                Some(l) => {
                    let file = l
                        .files
                        .iter()
                        .find(|f| f.file == name)
                        .or_else(|| l.files.iter().find(|f| f.hook == name))
                        .map(|f| f.file.clone());
                    apply(cx, repo, Act::SelectHook(file));
                }
                None => h.pending_select = Some(name),
            }
        }
        Act::RevertHookEdit => {
            let h = &mut repo.devops.gitplus.hooks;
            if let Some(Ok(t)) = &h.content {
                h.editor = t.clone();
            }
        }
        Act::SaveHook => {
            let h = &repo.devops.gitplus.hooks;
            let (Some(file), Some(l)) = (h.selected.clone(), h.listing()) else {
                return;
            };
            let Some(f) = l.files.iter().find(|x| x.file == file) else {
                return;
            };
            let content = h.editor.clone();
            let before = match &h.content {
                Some(Ok(t)) => t.lines().count(),
                _ => 0,
            };
            let mut details = vec![format!(
                "write {} ({} lines, was {before})",
                sh_path(&f.path),
                content.lines().count()
            )];
            if cfg!(unix) {
                details.push(format!("chmod +x {}", sh_path(&f.path)));
            }
            let mut body = format!("Writes your edits to {}.", f.file);
            if cfg!(windows) && !content.starts_with("#!") {
                body.push_str(" It does not start with #!, so Git for Windows will not run it.");
            }
            if f.gitvis {
                body.push_str(" This is the gitvis secret check: reinstalling it later overwrites your edits.");
            }
            cx.confirm(
                "Save the hook?",
                body,
                "Save",
                false,
                details,
                Act::SaveHookConfirmed { file, content }.into(),
            );
        }
        Act::SaveHookConfirmed { file, content } => {
            let Some(path) = repo
                .devops
                .gitplus
                .hooks
                .listing()
                .and_then(|l| l.files.iter().find(|f| f.file == file))
                .map(|f| f.path.clone())
            else {
                return;
            };
            change(cx, repo, format!("Save the {file} hook"), move |_, cmds| {
                cmds.push(format!("write {}", sh_path(&path)));
                core::save(&path, &content)
            });
        }
        Act::EnableHook(hook) => {
            let Some(l) = repo.devops.gitplus.hooks.listing() else {
                return;
            };
            let from = l.dir.join(format!("{hook}.sample"));
            let to = l.dir.join(&hook);
            cx.confirm(
                "Enable the hook?",
                format!(
                    "Renames {hook}.sample to {hook}, so git runs it (on {}).",
                    core::describe(&hook).unwrap_or("its event")
                ),
                "Enable",
                false,
                vec![core::rename_command(&from, &to)],
                Act::EnableHookConfirmed(hook).into(),
            );
        }
        Act::EnableHookConfirmed(hook) => {
            let Some(dir) = repo.devops.gitplus.hooks.listing().map(|l| l.dir.clone()) else {
                return;
            };
            repo.devops.gitplus.hooks.selected = Some(hook.clone());
            change(
                cx,
                repo,
                format!("Enable the {hook} hook"),
                move |_, cmds| {
                    cmds.push(core::rename_command(
                        &dir.join(format!("{hook}.sample")),
                        &dir.join(&hook),
                    ));
                    core::enable(&dir, &hook)
                },
            );
        }
        Act::DisableHook(hook) => {
            let Some(l) = repo.devops.gitplus.hooks.listing() else {
                return;
            };
            let from = l.dir.join(&hook);
            let to = l.dir.join(format!("{hook}.sample"));
            let replaces = to.exists();
            let gitvis = l.files.iter().any(|f| f.file == hook && f.gitvis);
            let mut body = format!(
                "Renames {hook} to {hook}.sample: git stops running it, and Enable brings it back."
            );
            if replaces {
                body.push_str(&format!(
                    " The existing {hook}.sample (git's example) is replaced."
                ));
            }
            if gitvis {
                body.push_str(" This turns the gitvis secret check off.");
            }
            cx.confirm(
                "Disable the hook?",
                body,
                "Disable",
                false,
                vec![core::rename_command(&from, &to)],
                Act::DisableHookConfirmed(hook).into(),
            );
        }
        Act::DisableHookConfirmed(hook) => {
            let Some(dir) = repo.devops.gitplus.hooks.listing().map(|l| l.dir.clone()) else {
                return;
            };
            repo.devops.gitplus.hooks.selected = Some(format!("{hook}.sample"));
            change(
                cx,
                repo,
                format!("Disable the {hook} hook"),
                move |_, cmds| {
                    cmds.push(core::rename_command(
                        &dir.join(&hook),
                        &dir.join(format!("{hook}.sample")),
                    ));
                    core::disable(&dir, &hook)
                },
            );
        }
        Act::NewHook => {
            let name = repo.devops.gitplus.hooks.new_hook.clone();
            let Some(l) = repo.devops.gitplus.hooks.listing() else {
                return;
            };
            if name.is_empty() || core::describe(&name).is_none() {
                cx.toast(ToastKind::Error, "Pick a hook first.");
                return;
            }
            let path = l.dir.join(&name);
            if path.exists() {
                cx.toast(ToastKind::Error, format!("{name} exists already."));
                return;
            }
            cx.confirm(
                "Create the hook?",
                format!(
                    "Writes a {name} script that does nothing yet (exit 0), for you to edit. Git runs it on {}.",
                    core::describe(&name).unwrap_or("")
                ),
                "Create",
                false,
                vec![format!("write {}", sh_path(&path)), core::template(&name)],
                Act::NewHookConfirmed(name).into(),
            );
        }
        Act::NewHookConfirmed(name) => {
            let Some(dir) = repo.devops.gitplus.hooks.listing().map(|l| l.dir.clone()) else {
                return;
            };
            let h = &mut repo.devops.gitplus.hooks;
            h.selected = Some(name.clone());
            h.new_hook.clear();
            change(
                cx,
                repo,
                format!("Create the {name} hook"),
                move |_, cmds| {
                    let path = dir.join(&name);
                    cmds.push(format!("write {}", sh_path(&path)));
                    core::save(&path, &core::template(&name))
                },
            );
        }
        Act::InstallSecretHook => {
            let Some(l) = repo.devops.gitplus.hooks.listing() else {
                return;
            };
            let Some(exe) = current_exe() else {
                cx.toast(
                    ToastKind::Error,
                    "Could not find the path of the running gitvis.",
                );
                return;
            };
            let plan = match core::plan_install(&l.dir, &exe) {
                Ok(p) => p,
                Err(e) => {
                    cx.toast(ToastKind::Error, e);
                    return;
                }
            };
            let dismissed = repo.dismissed.len();
            let mut details = plan.commands();
            if let Some(c) = &l.common {
                details.push(format!(
                    "write {} ({})",
                    sh_path(&c.join(secret_hook::DISMISSED_FILE)),
                    format::plural(dismissed, "dismissed fingerprint", "dismissed fingerprints")
                ));
            }
            details.push(plan.script.clone());
            let mut body = "Before each commit git runs gitvis, which scans the added lines of the staged changes with the Exposure page's rules. A high or critical secret stops the commit and is printed redacted, never in full; git commit --no-verify skips the check once.".to_owned();
            if plan.reinstall {
                body.push_str(
                    " The check is installed already: this updates it to the running gitvis.",
                );
            } else if plan.backup_from.is_some() {
                body.push_str(&format!(
                    " The existing pre-commit hook is kept as {} and still runs first.",
                    core::BACKUP
                ));
            }
            body.push_str(&format!(
                " The script calls {}: if gitvis moves, reinstall the check.",
                exe.display()
            ));
            cx.confirm(
                if plan.reinstall {
                    "Update the gitvis secret check?"
                } else {
                    "Install the gitvis secret check?"
                },
                body,
                if plan.reinstall { "Update" } else { "Install" },
                false,
                details,
                Act::InstallSecretHookConfirmed.into(),
            );
        }
        Act::InstallSecretHookConfirmed => {
            let Some(l) = repo.devops.gitplus.hooks.listing().cloned() else {
                return;
            };
            let Some(exe) = current_exe() else {
                return;
            };
            let dismissed = dismissed_of(repo);
            repo.devops.gitplus.hooks.selected = Some("pre-commit".into());
            change(
                cx,
                repo,
                "Install the gitvis secret check".into(),
                move |_, cmds| {
                    let plan = core::plan_install(&l.dir, &exe)?;
                    cmds.extend(plan.commands());
                    let msg = core::install(&plan)?;
                    if let Some(c) = &l.common {
                        let p = secret_hook::write_dismissed(c, &dismissed)?;
                        cmds.push(format!("write {}", sh_path(&p)));
                    }
                    Ok(msg)
                },
            );
        }
        Act::RemoveSecretHook => {
            let Some(l) = repo.devops.gitplus.hooks.listing() else {
                return;
            };
            let target = l.dir.join("pre-commit");
            let backup = l.dir.join(core::BACKUP);
            let mut details = vec![format!("rm {}", sh_path(&target))];
            let body = if backup.exists() {
                details.push(format!("mv {} {}", sh_path(&backup), sh_path(&target)));
                "Removes the gitvis secret check and puts back the pre-commit hook it had kept."
            } else {
                "Removes the gitvis secret check: commits are no longer scanned for secrets."
            };
            cx.confirm(
                "Remove the gitvis secret check?",
                body,
                "Remove",
                true,
                details,
                Act::RemoveSecretHookConfirmed.into(),
            );
        }
        Act::RemoveSecretHookConfirmed => {
            let Some(dir) = repo.devops.gitplus.hooks.listing().map(|l| l.dir.clone()) else {
                return;
            };
            repo.devops.gitplus.hooks.selected = None;
            change(
                cx,
                repo,
                "Remove the gitvis secret check".into(),
                move |_, cmds| {
                    cmds.push(format!("rm {}", sh_path(&dir.join("pre-commit"))));
                    core::uninstall(&dir)
                },
            );
        }
        Act::SyncDismissed => {
            let Some(c) = repo
                .devops
                .gitplus
                .hooks
                .listing()
                .and_then(|l| l.common.clone())
            else {
                return;
            };
            let n = repo.dismissed.len();
            cx.confirm(
                "Update the hook's dismissed findings?",
                format!(
                    "Writes the fingerprints of the {} you dismissed on the Exposure page, so the pre-commit check does not stop commits for them. Fingerprints only: no secret is written.",
                    format::plural(n, "finding", "findings")
                ),
                "Update",
                false,
                vec![format!("write {}", sh_path(&c.join(secret_hook::DISMISSED_FILE)))],
                Act::SyncDismissedConfirmed.into(),
            );
        }
        Act::SyncDismissedConfirmed => {
            let Some(c) = repo
                .devops
                .gitplus
                .hooks
                .listing()
                .and_then(|l| l.common.clone())
            else {
                return;
            };
            let dismissed = dismissed_of(repo);
            change(
                cx,
                repo,
                "Update the hook's dismissed findings".into(),
                move |_, cmds| {
                    let p = secret_hook::write_dismissed(&c, &dismissed)?;
                    cmds.push(format!("write {}", sh_path(&p)));
                    Ok(format!(
                        "The pre-commit check skips {}",
                        format::plural(dismissed.len(), "dismissed finding", "dismissed findings")
                    ))
                },
            );
        }
        Act::RunPreCommit { all } => {
            let Some((path, _)) = repo
                .devops
                .gitplus
                .hooks
                .tools
                .as_ref()
                .and_then(|t| t.precommit.clone())
            else {
                cx.toast(ToastKind::Error, "pre-commit is not installed.");
                return;
            };
            let staged: Vec<String> = repo
                .snapshot
                .as_ref()
                .map(|s| s.status.staged.iter().map(|e| e.path.clone()).collect())
                .unwrap_or_default();
            if !all && staged.is_empty() {
                cx.toast(ToastKind::Error, "Nothing is staged.");
                return;
            }
            let args = if all {
                precommit::run_all_args()
            } else {
                precommit::run_files_args(&staged)
            };
            let spec =
                ProcSpec::new(path.to_string_lossy().into_owned(), repo.workdir.clone()).args(args);
            cx.confirm(
                "Run pre-commit?",
                if all {
                    "Runs every hook of .pre-commit-config.yaml on every tracked file, in the console. Hooks that fix files (formatters, whitespace) change them. The first run downloads and installs the hook repositories (network)."
                } else {
                    "Runs every hook of .pre-commit-config.yaml on the staged files, in the console. Hooks that fix files (formatters, whitespace) change them. The first run downloads and installs the hook repositories (network)."
                },
                "Run",
                false,
                vec![spec.display()],
                Act::RunPreCommitConfirmed { all }.into(),
            );
        }
        Act::RunPreCommitConfirmed { all } => {
            let Some((path, _)) = repo
                .devops
                .gitplus
                .hooks
                .tools
                .as_ref()
                .and_then(|t| t.precommit.clone())
            else {
                return;
            };
            let staged: Vec<String> = repo
                .snapshot
                .as_ref()
                .map(|s| s.status.staged.iter().map(|e| e.path.clone()).collect())
                .unwrap_or_default();
            let args = if all {
                precommit::run_all_args()
            } else {
                precommit::run_files_args(&staged)
            };
            let spec =
                ProcSpec::new(path.to_string_lossy().into_owned(), repo.workdir.clone()).args(args);
            let id = cx.console(
                if all {
                    "pre-commit on all files"
                } else {
                    "pre-commit on staged files"
                },
                spec,
            );
            repo.devops.gitplus.hooks.precommit_run = Some(PreCommitRun {
                id,
                all,
                results: None,
            });
        }
        Act::RunSemgrep => {
            let Some((path, _)) = repo
                .devops
                .gitplus
                .hooks
                .tools
                .as_ref()
                .and_then(|t| t.semgrep.clone())
            else {
                cx.toast(ToastKind::Error, "semgrep is not installed.");
                return;
            };
            let report = precommit::report_path();
            let spec = ProcSpec::new(path.to_string_lossy().into_owned(), repo.workdir.clone())
                .args(precommit::semgrep_args(&report));
            cx.confirm(
                "Scan with semgrep?",
                "semgrep --config auto downloads its rules for the languages it finds from semgrep.dev (network) and sends anonymous usage metrics, which that mode requires. The code itself is scanned on this machine. It runs in the console; the summary appears here when it ends.",
                "Scan",
                false,
                vec![spec.display()],
                Act::RunSemgrepConfirmed.into(),
            );
        }
        Act::RunSemgrepConfirmed => {
            let Some((path, _)) = repo
                .devops
                .gitplus
                .hooks
                .tools
                .as_ref()
                .and_then(|t| t.semgrep.clone())
            else {
                return;
            };
            let report = precommit::report_path();
            let spec = ProcSpec::new(path.to_string_lossy().into_owned(), repo.workdir.clone())
                .args(precommit::semgrep_args(&report));
            let id = cx.console("semgrep scan --config auto", spec);
            repo.devops.gitplus.hooks.semgrep = Some(SemgrepRun {
                id,
                report,
                summary: None,
            });
        }
        Act::ShowConsoleRun(id) => {
            cx.action(crate::devops::workbench::Act::SetTab(
                crate::devops::workbench::Tab::Console,
            ));
            cx.action(crate::devops::workbench::Act::SelectRun(id));
        }
        _ => {}
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    let h = &mut repo.devops.gitplus.hooks;
    match msg {
        Msg::Hooks(r) => {
            h.loading = false;
            h.at = Some(Instant::now());
            let pending = h.pending_select.take();
            if let Ok(l) = &r
                && let Some(sel) = &h.selected
                && !l.files.iter().any(|f| &f.file == sel)
            {
                h.selected = None;
                h.content = None;
                h.content_for = None;
            }
            h.listing = Some(r);
            if let Some(name) = pending {
                apply(cx, repo, Act::SelectHookName(name));
            } else if let Some(sel) = repo.devops.gitplus.hooks.selected.clone()
                && repo.devops.gitplus.hooks.content_for.as_deref() != Some(sel.as_str())
            {
                load_content(cx, repo, &sel);
            }
        }
        Msg::HookContent(file, r) => {
            if h.content_for.as_deref() == Some(file.as_str()) {
                if let Ok(t) = &r {
                    h.editor = t.clone();
                }
                h.content = Some(r);
            }
        }
        Msg::HookChanged => {
            h.busy = false;
            h.content_for = None;
            load(cx, repo);
        }
        Msg::Tools(t) => {
            h.tools_loading = false;
            h.tools = Some(t);
        }
        Msg::SemgrepParsed(id, r) => {
            if let Some(s) = h.semgrep.as_mut()
                && s.id == id
            {
                s.summary = Some(r);
            }
        }
        _ => {}
    }
}

pub fn on_console_done(cx: &mut Ctx, repo: &mut RepoState, run: &ConsoleRun) {
    let h = &mut repo.devops.gitplus.hooks;
    if let Some(p) = h.precommit_run.as_mut()
        && p.id == run.id
    {
        p.results = Some(precommit::parse_run(&run.text()));
    }
    if let Some(s) = h.semgrep.as_mut()
        && s.id == run.id
    {
        if run.cancelled() {
            s.summary = Some(Err("Stopped.".into()));
            let _ = std::fs::remove_file(&s.report);
            return;
        }
        let (id, report) = (s.id, s.report.clone());
        let tail = run.tail(8);
        cx.read("Read the semgrep report", move |_, log| {
            log.command(format!("read {}", report.display()));
            let r = std::fs::read(&report)
                .map_err(|_| {
                    if tail.trim().is_empty() {
                        "semgrep wrote no report.".to_owned()
                    } else {
                        format!("semgrep wrote no report:\n{tail}")
                    }
                })
                .and_then(|b| precommit::parse_semgrep(&b));
            let _ = std::fs::remove_file(&report);
            Msg::SemgrepParsed(id, r)
        });
    }
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

fn badges(f: &HookFile) -> Vec<(String, Tone)> {
    let mut b = Vec::new();
    if f.gitvis {
        b.push(("gitvis secret check".to_owned(), Tone::Accent));
    }
    if f.backup {
        b.push(("Kept by gitvis".to_owned(), Tone::Neutral));
    }
    if f.enabled {
        b.push(("Enabled".to_owned(), Tone::Success));
    } else if f.sample {
        b.push(("Sample".to_owned(), Tone::Neutral));
    } else if f.known && !f.executable && !f.backup {
        b.push(("Not executable".to_owned(), Tone::Warning));
    } else if !f.known && !f.backup {
        b.push(("Not a hook".to_owned(), Tone::Neutral));
    }
    b
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, console: &Console, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let busy = repo.is_busy() || repo.devops.gitplus.hooks.busy;
    let dismissed = repo.dismissed.clone();
    let staged = repo
        .snapshot
        .as_ref()
        .map(|s| s.status.staged.len())
        .unwrap_or(0);
    let h = &mut repo.devops.gitplus.hooks;
    kit::page_header(
        ui,
        "Hooks",
        crate::devops::workbench::Tab::Hooks.hint(),
        |ui| {
            if IconButton::new(Icon::Rerun, "Refresh")
                .compact()
                .enabled(!h.loading)
                .show(ui)
                .on_hover_text("git rev-parse --git-path hooks, then list the folder")
                .clicked()
            {
                actions.push(Act::LoadHooks.into());
            }
            if let (Some(Ok(l)), Some(at)) = (&h.listing, h.at) {
                let enabled = l.files.iter().filter(|f| f.enabled).count();
                gui::header_note(
                    ui,
                    &format!(
                        "{} enabled  ·  {}",
                        format::plural(enabled, "hook", "hooks"),
                        gui::loaded_ago(at)
                    ),
                );
            }
        },
    );
    let l = match &h.listing {
        None => {
            gui::empty_pane(ui, |ui| kit::loading(ui, "Listing hooks…"));
            return;
        }
        Some(Err(e)) => {
            let e = e.clone();
            gui::empty_pane(ui, |ui| {
                if gui::error_card(ui, "Could not list the hooks", &e, "Try again") {
                    actions.push(Act::LoadHooks.into());
                }
            });
            return;
        }
        Some(Ok(l)) => l.clone(),
    };
    let selected = h.selected.clone();
    gui::list_pane(ui, "gp_hooks_list", 380.0, |ui| {
        kit::list_header(ui, "Hooks", Some(l.files.len()), |ui| {
            if IconButton::text("Overview").compact().show(ui).clicked() {
                actions.push(Act::SelectHook(None).into());
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new(l.dir.to_string_lossy())
                    .size(size::SMALL)
                    .color(p.text_2),
            );
            if l.custom.is_some() {
                kit::badge(ui, "core.hooksPath", Tone::Accent);
            }
        });
        ui.add_space(6.0);
        if l.files.is_empty() {
            kit::muted(ui, "The folder is empty: git runs no hooks here.");
        }
        let files = l.files.clone();
        kit::list(
            ui,
            "gp_hooks_rows",
            files.len(),
            kit::ROW_H,
            |i| selected.as_deref() == Some(files[i].file.as_str()),
            |ui, rect, resp, i| {
                let f = &files[i];
                let sub = if f.known {
                    core::describe(&f.hook).unwrap_or("").to_owned()
                } else if f.first_line.is_empty() {
                    format::bytes(f.size as usize)
                } else {
                    f.first_line.clone()
                };
                let b = badges(f);
                let dot = if f.enabled {
                    p.success
                } else if f.known && !f.sample && !f.backup {
                    p.warning
                } else {
                    p.text_3
                };
                kit::paint_row(
                    ui,
                    rect,
                    &Row::new(&f.file)
                        .leading(kit::Leading::Dot(dot))
                        .subtitle(&sub)
                        .meta(&format::bytes(f.size as usize))
                        .badges(&b)
                        .mono()
                        .dimmed(f.sample),
                    selected.as_deref() == Some(f.file.as_str()),
                );
                if resp.clicked() {
                    actions.push(Act::SelectHook(Some(f.file.clone())).into());
                }
            },
        );
    });
    let h = &mut repo.devops.gitplus.hooks;
    match selected.and_then(|s| l.files.iter().find(|f| f.file == s).cloned()) {
        Some(f) => gui::detail_pane(ui, ("gp_hook", &f.file), |ui| {
            detail(ui, h, &f, busy, actions);
        }),
        None => gui::detail_pane(ui, "gp_hooks_overview", |ui| {
            overview(ui, h, &l, &dismissed, staged, console, busy, actions);
        }),
    }
}

fn detail(ui: &mut Ui, h: &mut Hooks, f: &HookFile, busy: bool, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    crate::views::insights::detail_title(ui, &f.file, &badges(f));
    ui.add_space(4.0);
    kit::kv(ui, 120.0, |kv| {
        kv.row("File", |ui| {
            gui::copyable(ui, &f.path.to_string_lossy(), actions)
        });
        match core::describe(&f.hook) {
            Some(d) => kv.text("Runs on", d),
            None => kv.muted("Runs on", "Nothing: not a hook name git knows"),
        }
        kv.text("Size", format::bytes(f.size as usize));
        if !f.first_line.is_empty() {
            kv.text("First line", kit::mono(&f.first_line));
        }
        kv.row("State", |ui| {
            let (text, c) = if f.enabled {
                ("Enabled: git runs it", p.success)
            } else if f.sample {
                ("A sample: git ignores files ending in .sample", p.text_2)
            } else if f.backup {
                ("Kept by the gitvis check, which runs it first", p.text_2)
            } else if !f.executable {
                if cfg!(windows) {
                    (
                        "Not runnable: it does not start with #! (Git for Windows needs that)",
                        p.warning,
                    )
                } else {
                    ("Not executable: git skips it (chmod +x)", p.warning)
                }
            } else {
                ("Not run: git does not know this name", p.text_2)
            };
            ui.label(RichText::new(text).color(c));
        });
    });
    gui::buttons(ui, |ui| {
        if f.sample
            && IconButton::new(Icon::Check, "Enable…")
                .enabled(!busy)
                .show(ui)
                .clicked()
        {
            actions.push(Act::EnableHook(f.hook.clone()).into());
        }
        if !f.sample
            && f.known
            && !f.backup
            && IconButton::text("Disable…")
                .enabled(!busy)
                .show(ui)
                .clicked()
        {
            actions.push(Act::DisableHook(f.hook.clone()).into());
        }
        if f.gitvis
            && IconButton::new(Icon::Trash, "Remove the gitvis check…")
                .danger()
                .enabled(!busy)
                .show(ui)
                .clicked()
        {
            actions.push(Act::RemoveSecretHook.into());
        }
    });
    let changed = matches!(&h.content, Some(Ok(t)) if *t != h.editor);
    kit::section_with(ui, "Content", None, |ui| {
        if matches!(&h.content, Some(Ok(_))) {
            if form::primary(ui, "Save…", changed && !busy).clicked() {
                actions.push(Act::SaveHook.into());
            }
            if form::button(ui, "Revert", changed).clicked() {
                actions.push(Act::RevertHookEdit.into());
            }
            if changed {
                ui.label(RichText::new("Edited").size(size::SMALL).color(p.warning));
            }
        }
    });
    match &h.content {
        None => kit::loading(ui, "Reading the file…"),
        Some(Err(e)) => kit::callout(ui, Tone::Danger, "Could not read it", e),
        Some(Ok(_)) => {
            let rows = h.editor.lines().count().clamp(8, 40);
            form::code(ui, ("gp_hook_editor", f.file.as_str()), &mut h.editor, rows);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn overview(
    ui: &mut Ui,
    h: &mut Hooks,
    l: &Listing,
    dismissed: &HashSet<String>,
    staged: usize,
    console: &Console,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    kit::title(ui, "Hooks");
    let enabled = l.files.iter().filter(|f| f.enabled).count();
    let samples = l.files.iter().filter(|f| f.sample).count();
    kit::stats(
        ui,
        &[
            Stat::new("Enabled hooks", format::count(enabled)),
            Stat::new("Samples", format::count(samples)),
            Stat::new(
                "Secret check",
                if l.gitvis().is_some_and(|f| f.enabled) {
                    "On"
                } else {
                    "Off"
                },
            )
            .tone(if l.gitvis().is_some_and(|f| f.enabled) {
                Tone::Success
            } else {
                Tone::Neutral
            }),
        ],
    );
    if let Some(c) = &l.custom {
        kit::callout(
            ui,
            Tone::Neutral,
            "core.hooksPath is set",
            &format!(
                "Git runs the hooks in {c} instead of .git/hooks; this tab shows that folder."
            ),
        );
    }
    secret_check(ui, l, dismissed, busy, actions);
    precommit_card(ui, h, l, staged, console, busy, actions);
    semgrep_card(ui, h, console, actions);
    new_hook(ui, h, l, busy, actions);
}

fn secret_check(
    ui: &mut Ui,
    l: &Listing,
    dismissed: &HashSet<String>,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    kit::section(ui, "gitvis secret check", None);
    let installed = l.gitvis();
    let exe = current_exe().map(|e| core::script_exe_path(&e));
    form::card(
        ui,
        if installed.is_some() {
            Tone::Success
        } else {
            Tone::Neutral
        },
        |ui| {
            kit::body(
                ui,
                "Install the Exposure page's secret scan as the pre-commit hook: before each commit, the added lines of the staged changes are checked, and a high or critical secret (API keys, tokens, private keys, passwords) stops the commit. Findings are printed redacted with file:line and rule; git commit --no-verify skips the check once.",
            );
            ui.add_space(4.0);
            match installed {
                Some(f) => {
                    kit::kv(ui, 150.0, |kv| {
                        kv.text(
                            "State",
                            if f.enabled {
                                "Installed and enabled"
                            } else {
                                "Installed, not runnable"
                            },
                        );
                        match (&l.gitvis_exe, &exe) {
                            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => {
                                kv.text("Runs", kit::mono(a))
                            }
                            (Some(a), _) => kv.text(
                                "Runs",
                                RichText::new(format!(
                                    "{a}  (not this gitvis: Update to point it here)"
                                ))
                                .color(p.warning),
                            ),
                            (None, _) => kv.muted("Runs", "unknown (edited script)"),
                        }
                        let known = l.dismissed_in_file.as_ref().map(|s| s.len()).unwrap_or(0);
                        let same = l.dismissed_in_file.as_ref().is_some_and(|s| s == dismissed);
                        kv.text(
                            "Dismissed findings",
                            if same {
                                format!(
                                    "{} skipped by the hook (same as the Exposure page)",
                                    format::count(known)
                                )
                            } else {
                                format!(
                                    "the hook knows {}, the Exposure page has {}: Update to match",
                                    format::count(known),
                                    format::count(dismissed.len())
                                )
                            },
                        );
                        if l.files.iter().any(|x| x.backup) {
                            kv.text("Chained", format!("{} runs first", core::BACKUP));
                        }
                    });
                    gui::buttons(ui, |ui| {
                        if IconButton::new(Icon::Rerun, "Update the hook…")
                            .enabled(!busy)
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Act::InstallSecretHook.into());
                        }
                        let same = l.dismissed_in_file.as_ref().is_some_and(|s| s == dismissed);
                        if !same
                            && IconButton::text("Update dismissed findings…")
                                .enabled(!busy)
                                .show(ui)
                                .clicked()
                        {
                            actions.push(Act::SyncDismissed.into());
                        }
                        if IconButton::text("Show the script").show(ui).clicked() {
                            actions.push(Act::SelectHook(Some("pre-commit".into())).into());
                        }
                        if IconButton::new(Icon::Trash, "Remove…")
                            .danger()
                            .enabled(!busy)
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Act::RemoveSecretHook.into());
                        }
                    });
                }
                None => {
                    let existing = l.files.iter().any(|f| f.file == "pre-commit");
                    if existing {
                        kit::muted(
                            ui,
                            &format!(
                                "A pre-commit hook exists: it is kept as {} and runs before the check.",
                                core::BACKUP
                            ),
                        );
                    }
                    form::actions_row(ui, |ui| {
                        if form::primary(ui, "Install as the pre-commit hook…", !busy).clicked() {
                            actions.push(Act::InstallSecretHook.into());
                        }
                    });
                }
            }
        },
    );
}

fn precommit_card(
    ui: &mut Ui,
    h: &mut Hooks,
    l: &Listing,
    staged: usize,
    console: &Console,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    kit::section(ui, "pre-commit framework", None);
    form::card(ui, Tone::Neutral, |ui| {
        match &l.precommit {
            None => {
                kit::secondary(
                    ui,
                    "No .pre-commit-config.yaml in this repository. The pre-commit framework runs linters and formatters from many languages as git hooks; add the file to use it.",
                );
            }
            Some((path, Err(e))) => kit::callout(
                ui,
                Tone::Danger,
                &format!(
                    "{} is unreadable",
                    path.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                ),
                e,
            ),
            Some((path, Ok(repos))) => {
                kit::body(
                    ui,
                    &format!(
                        "{}: {} from {}.",
                        path.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        format::plural(repos.iter().map(|r| r.hooks.len()).sum(), "hook", "hooks"),
                        format::plural(repos.len(), "repository", "repositories")
                    ),
                );
                for r in repos.iter().take(12) {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(kit::mono(&r.repo).color(p.text));
                        if let Some(rev) = &r.rev {
                            ui.label(RichText::new(rev).size(size::SMALL).color(p.text_3));
                        }
                        ui.label(
                            RichText::new(r.hooks.join(", "))
                                .size(size::SMALL)
                                .color(p.text_2),
                        );
                    });
                }
            }
        }
        if l.precommit.is_none() {
            return;
        }
        match &h.tools {
            None => kit::loading(ui, "Looking for pre-commit…"),
            Some(t) => match &t.precommit {
                None => {
                    if gui::missing_tool(
                        ui,
                        "pre-commit",
                        "pre-commit",
                        "The pre-commit framework installs and runs the hooks listed in .pre-commit-config.yaml.",
                        PRECOMMIT_HINT,
                    ) {
                        actions.push(Act::CheckTools.into());
                    }
                }
                Some((_, version)) => {
                    kit::muted(ui, version);
                    gui::buttons(ui, |ui| {
                        if IconButton::new(Icon::Play, "Run on all files…")
                            .enabled(!busy)
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Act::RunPreCommit { all: true }.into());
                        }
                        if IconButton::new(Icon::Play, "Run on staged files…")
                            .enabled(!busy && staged > 0)
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Act::RunPreCommit { all: false }.into());
                        }
                    });
                }
            },
        }
        if let Some(run) = &h.precommit_run {
            let state = console.get(run.id);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(if run.all {
                        "Last run, all files:"
                    } else {
                        "Last run, staged files:"
                    })
                    .size(size::SMALL)
                    .color(p.text_2),
                );
                if let Some(r) = state {
                    kit::badge(ui, &r.status_text(), crate::views::console::status_tone(r));
                }
                if IconButton::text("Show in the console")
                    .compact()
                    .show(ui)
                    .clicked()
                {
                    actions.push(Act::ShowConsoleRun(run.id).into());
                }
            });
            if let Some(results) = &run.results {
                if results.is_empty() {
                    kit::muted(ui, "No hook results in the output.");
                }
                for r in results {
                    ui.horizontal(|ui| {
                        let (t, tone) = match r.outcome {
                            Outcome::Passed => ("Passed", Tone::Success),
                            Outcome::Failed => ("Failed", Tone::Danger),
                            Outcome::Skipped => ("Skipped", Tone::Neutral),
                        };
                        kit::badge(ui, t, tone);
                        ui.label(RichText::new(&r.name).color(p.text));
                        if !r.note.is_empty() {
                            ui.label(RichText::new(&r.note).size(size::SMALL).color(p.text_3));
                        }
                    });
                }
            }
        }
    });
}

fn semgrep_card(ui: &mut Ui, h: &mut Hooks, console: &Console, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    kit::section(ui, "Semgrep", None);
    form::card(ui, Tone::Neutral, |ui| {
        kit::secondary(
            ui,
            "Semgrep looks for bugs and insecure patterns (injection, unsafe deserialisation, hard-coded credentials) with community rules for the languages it finds.",
        );
        match &h.tools {
            None => kit::loading(ui, "Looking for semgrep…"),
            Some(t) => match &t.semgrep {
                None => {
                    if gui::missing_tool(
                        ui,
                        "Semgrep",
                        "semgrep",
                        "The scan runs the semgrep command line.",
                        SEMGREP_HINT,
                    ) {
                        actions.push(Act::CheckTools.into());
                    }
                }
                Some((_, version)) => {
                    kit::muted(ui, &format!("semgrep {version}"));
                    let running = h
                        .semgrep
                        .as_ref()
                        .and_then(|s| console.get(s.id))
                        .is_some_and(|r| r.running());
                    if form::button(ui, "Scan with --config auto…", !running).clicked() {
                        actions.push(Act::RunSemgrep.into());
                    }
                }
            },
        }
        let Some(run) = &h.semgrep else {
            return;
        };
        ui.horizontal(|ui| {
            if let Some(r) = console.get(run.id) {
                kit::badge(ui, &r.status_text(), crate::views::console::status_tone(r));
            }
            if IconButton::text("Show in the console")
                .compact()
                .show(ui)
                .clicked()
            {
                actions.push(Act::ShowConsoleRun(run.id).into());
            }
        });
        match &run.summary {
            None => {}
            Some(Err(e)) => kit::callout(ui, Tone::Danger, "No summary", e),
            Some(Ok(s)) => {
                let mut tiles = vec![
                    Stat::new("Findings", format::count(s.findings.len()))
                        .tone_if(!s.findings.is_empty(), Tone::Warning),
                ];
                for (sev, n) in &s.by_severity {
                    tiles.push(Stat::new(sev.as_str(), format::count(*n)));
                }
                tiles.push(Stat::new("Files scanned", format::count(s.scanned)));
                kit::stats(ui, &tiles);
                if !s.top_rules.is_empty() {
                    kit::subtitle(ui, "Top rules");
                    let items: Vec<(String, u32)> = s
                        .top_rules
                        .iter()
                        .take(8)
                        .map(|(r, n)| (r.clone(), *n as u32))
                        .collect();
                    kit::count_bars(ui, &items, p.accent, 8);
                }
                if !s.findings.is_empty() {
                    kit::subtitle(ui, "Findings");
                    for f in s.findings.iter().take(60) {
                        ui.horizontal(|ui| {
                            let tone = match precommit::severity_rank(&f.severity) {
                                4..=5 => Tone::Danger,
                                3 => Tone::Warning,
                                _ => Tone::Neutral,
                            };
                            kit::badge(ui, &f.severity, tone);
                            ui.label(kit::mono(format!("{}:{}", f.path, f.line)).color(p.text));
                        });
                        ui.label(
                            RichText::new(format!("{}  ·  {}", f.rule, f.message))
                                .size(size::SMALL)
                                .color(p.text_2),
                        );
                    }
                    if s.findings.len() > 60 {
                        kit::muted(
                            ui,
                            &format!("and {} more in the console output", s.findings.len() - 60),
                        );
                    }
                }
                for e in s.errors.iter().take(5) {
                    kit::muted(ui, &format!("semgrep error: {e}"));
                }
            }
        }
    });
}

fn new_hook(ui: &mut Ui, h: &mut Hooks, l: &Listing, busy: bool, actions: &mut Vec<Action>) {
    kit::section(ui, "New hook", None);
    let free: Vec<(String, String)> = core::KNOWN
        .iter()
        .filter(|(n, _)| !l.files.iter().any(|f| f.file == *n))
        .filter(|(n, _)| {
            !matches!(
                *n,
                "pre-receive" | "update" | "proc-receive" | "post-receive" | "post-update"
            ) && !n.starts_with("p4-")
        })
        .map(|(n, _)| ((*n).to_owned(), (*n).to_owned()))
        .collect();
    if free.is_empty() {
        kit::muted(ui, "Every client-side hook exists already.");
        return;
    }
    let mut options = vec![(String::new(), "Pick a hook".to_owned())];
    options.extend(free);
    ui.horizontal(|ui| {
        form::select(ui, "gp_new_hook", &mut h.new_hook, &options, 260.0);
        if form::button(ui, "Create…", !busy && !h.new_hook.is_empty()).clicked() {
            actions.push(Act::NewHook.into());
        }
    });
    if let Some(d) = core::describe(&h.new_hook) {
        kit::muted(ui, &format!("Runs on: {d}."));
    }
}
