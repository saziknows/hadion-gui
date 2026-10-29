//! Updates: read the source's releases (only when asked), download, verify,
//! extract and stage the newest usable one next to the running executable,
//! then swap it in on restart, with rollback.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use egui::{Align, CornerRadius, Layout, RichText, Sense, Ui, vec2};
use hadion_core::devops::packaging::release::{
    self, Channel, Evaluation, Platform, Release, releases_endpoint,
};
use hadion_core::devops::packaging::swap::{self, Paths};
use hadion_core::devops::packaging::update::{
    self, Failure, Job, Progress, Source, Staged, Step, StepDone,
};
use hadion_core::devops::packaging::{paths, time};
use hadion_core::devops::tools::Tool;
use hadion_core::github::ApiCall;

use super::{Act, Msg, Prefs, info};
use crate::devops::ctx::Ctx;
use crate::devops::global::GlobalCx;
use crate::devops::settings_page::Section;
use crate::format;
use crate::state::{Action, Settings, ToastKind};
use crate::theme::{Tone, medium, pal, size, space};
use crate::views::form;
use crate::views::icons::{self, Icon, IconButton};
use crate::views::kit;

/// The release check.
#[derive(Default)]
pub enum Check {
    #[default]
    Never,
    Running {
        since: Instant,
        command: String,
        source: String,
    },
    Done {
        at: i64,
        source: String,
        command: String,
        result: Result<Vec<Release>, String>,
        ms: u64,
        /// `gh` is not installed.
        gh_missing: bool,
    },
}

/// A download in progress.
pub struct Running {
    pub version: String,
    pub progress: Arc<Mutex<Progress>>,
    pub cancel: Arc<AtomicBool>,
    pub started: Instant,
}

/// A verified executable at `<exe>.new`.
pub struct StagedInfo {
    pub tag: String,
    pub version: String,
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
    pub steps: Vec<StepDone>,
    /// Found at start: staged in an earlier session.
    pub earlier: bool,
}

/// What the last restart attempt did (only kept when it failed).
pub struct RestartResult {
    pub steps: Vec<String>,
    pub error: String,
    pub rolled_back: bool,
}

#[derive(Default)]
pub struct State {
    pub check: Check,
    pub job: Option<Running>,
    pub staged: Option<StagedInfo>,
    pub failure: Option<Failure>,
    pub restart: Option<RestartResult>,
    /// The update log's most recent attempt.
    pub last_log: Vec<String>,
    /// Update source suggestions: `(owner/repo, why)`.
    pub suggestions: Vec<(String, String)>,
}

/// Set by [`after_relaunch`]: this process was started by an update.
struct Relaunched {
    from: String,
    cleanup: String,
}

static RELAUNCHED: OnceLock<Relaunched> = OnceLock::new();

/// First thing in `main`: when an update started this process, wait for
/// the previous one to exit (its `<exe>.old` can only be deleted then, on
/// Windows) and delete it. At most ~8 s.
pub fn after_relaunch() {
    let (Some(from), Some(old)) = (
        std::env::var_os("HADION_UPDATED_FROM"),
        std::env::var_os("HADION_UPDATE_OLD"),
    ) else {
        return;
    };
    // SAFETY: called first thing in `main`, before any other thread starts.
    unsafe {
        std::env::remove_var("HADION_UPDATED_FROM");
        std::env::remove_var("HADION_UPDATE_OLD");
    }
    let Some(exe) = info::exe() else {
        return;
    };
    let p = Paths::for_exe(&exe);
    // Only ever our own `.old`, whatever the variable says.
    let cleanup = if *old != *p.old.as_os_str() {
        format!("ignored HADION_UPDATE_OLD={}", PathBuf::from(old).display())
    } else if swap::cleanup_old(&p, 40, std::time::Duration::from_millis(200)) {
        format!("deleted {}", p.old.display())
    } else {
        format!(
            "could not delete {} yet (the previous Hadion is still running); it is retried at the next start",
            p.old.display()
        )
    };
    let _ = RELAUNCHED.set(Relaunched {
        from: from.to_string_lossy().into_owned(),
        cleanup,
    });
}

fn log(event: &str, detail: &str) {
    if let Some(p) = info::update_log() {
        update::log(&p, event, detail);
    }
}

fn read_last_log() -> Vec<String> {
    info::update_log()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| update::last_attempt(&t))
        .unwrap_or_default()
}

/// Where releases come from: the test folder, else the configured repository.
pub fn source(prefs: &Prefs) -> Option<Source> {
    if let Some(d) = paths::test_dir() {
        return Some(Source::Folder(d));
    }
    let r = prefs.source.trim();
    release::valid_repo(r).then(|| Source::GitHub(r.to_owned()))
}

fn current_exe_paths() -> Option<Paths> {
    info::exe().map(|e| Paths::for_exe(&e))
}

/// A `.new` staged in an earlier session (its version from the log).
fn detect_staged(st: &mut State) {
    if st.staged.is_some() || st.job.is_some() {
        return;
    }
    let Some(p) = current_exe_paths() else {
        return;
    };
    if !p.staged() {
        return;
    }
    let text = info::update_log()
        .and_then(|l| std::fs::read_to_string(l).ok())
        .unwrap_or_default();
    // `… stage     v0.2.0 sha256=<hex> C:\…\hadion.exe.new`
    let line = text
        .lines()
        .rev()
        .find(|l| l.split_whitespace().nth(1) == Some("stage"))
        .unwrap_or("");
    let tag = line.split_whitespace().nth(2).unwrap_or("").to_owned();
    let sha = line
        .split_whitespace()
        .find_map(|w| w.strip_prefix("sha256="))
        .unwrap_or("")
        .to_owned();
    st.staged = Some(StagedInfo {
        version: tag.trim_start_matches('v').to_owned(),
        tag,
        bytes: std::fs::metadata(&p.new).map(|m| m.len()).unwrap_or(0),
        path: p.new,
        sha256: sha,
        steps: Vec::new(),
        earlier: true,
    });
}

/// Once per session.
pub fn start(cx: &mut Ctx, st: &mut State, prefs: &Prefs) {
    if let Some(r) = RELAUNCHED.get() {
        let b = info::build();
        log(
            "started",
            &format!(
                "Hadion {} ({}) after updating from {}; {}",
                b.version,
                b.commit_label(),
                r.from,
                r.cleanup
            ),
        );
        cx.toast(
            ToastKind::Success,
            format!("Updated to Hadion {} (from {})", b.version, r.from),
        );
    } else if let Some(p) = current_exe_paths()
        && p.old.exists()
    {
        // A leftover from an update whose cleanup could not finish.
        if swap::cleanup_old(&p, 1, std::time::Duration::ZERO) {
            log("cleanup", &format!("deleted {}", p.old.display()));
        }
    }
    st.last_log = read_last_log();
    detect_staged(st);
    if prefs.check_on_start && source(prefs).is_some() {
        check(cx, st, prefs);
    }
}

/// The Updates section became visible.
pub fn on_shown(st: &mut State, g: &GlobalCx) {
    st.last_log = read_last_log();
    detect_staged(st);
    let mut out: Vec<(String, String)> = Vec::new();
    for r in g.repos {
        let is_hadion = r
            .workdir
            .join("crates")
            .join("hadion")
            .join("Cargo.toml")
            .is_file()
            && r.workdir.join("crates").join("hadion-core").is_dir();
        if let (true, Some((o, n))) = (is_hadion, r.github_remote.as_ref()) {
            out.push((
                format!("{o}/{n}"),
                format!(
                    "the GitHub remote of the Hadion checkout open in tab \"{}\"",
                    r.name
                ),
            ));
        }
    }
    let b = info::build();
    if release::valid_repo(&b.remote) {
        out.push((
            b.remote.clone(),
            "the repository this build was made from".to_owned(),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|(r, _)| seen.insert(r.to_ascii_lowercase()));
    st.suggestions = out;
}

/// The evaluation of the last successful check under the current channel.
pub fn evaluation(st: &State, prefs: &Prefs) -> Option<Evaluation> {
    let Check::Done {
        result: Ok(list), ..
    } = &st.check
    else {
        return None;
    };
    Some(release::evaluate(
        list,
        &info::build().semver(),
        prefs.channel,
        &Platform::current(),
    ))
}

pub fn check(cx: &mut Ctx, st: &mut State, prefs: &Prefs) {
    if matches!(st.check, Check::Running { .. }) {
        return;
    }
    let Some(src) = source(prefs) else {
        cx.toast(
            ToastKind::Error,
            "Set the source repository (owner/name) in Settings → Updates first",
        );
        return;
    };
    let command = match &src {
        Source::GitHub(r) => format!("gh api {}", releases_endpoint(r)),
        Source::Folder(d) => format!("read {}", d.join("releases.json").display()),
    };
    let label = src.label();
    if matches!(src, Source::GitHub(_))
        && hadion_core::devops::proc::resolve(&hadion_core::github::gh_program()).is_none()
    {
        st.check = Check::Done {
            at: time::now_secs(),
            source: label,
            command,
            result: Err("gh is not installed (not found on PATH)".into()),
            ms: 0,
            gh_missing: true,
        };
        return;
    }
    log("check", &command);
    st.check = Check::Running {
        since: Instant::now(),
        command: command.clone(),
        source: label.clone(),
    };
    cx.user_read("Check for updates", move |gh, tlog| {
        let t = Instant::now();
        let result = match &src {
            Source::GitHub(repo) => gh
                .get(&releases_endpoint(repo), &mut tlog.calls)
                .map_err(|e| e.to_string())
                .and_then(|b| release::parse_releases(&b)),
            Source::Folder(dir) => {
                let p = dir.join("releases.json");
                let read =
                    std::fs::read(&p).map_err(|e| format!("Could not read {}: {e}", p.display()));
                tlog.calls.push(ApiCall {
                    command: format!("read {} (HADION_UPDATE_DIR test mode)", p.display()),
                    elapsed_ms: t.elapsed().as_millis() as u64,
                    bytes: read.as_ref().map(Vec::len).unwrap_or(0),
                    rate: None,
                    error: read.as_ref().err().cloned(),
                    input: None,
                });
                read.and_then(|b| release::parse_releases(&b))
            }
        };
        Msg::Checked {
            source: label,
            result,
            ms: t.elapsed().as_millis() as u64,
        }
    });
}

pub fn on_checked(
    _cx: &mut Ctx,
    st: &mut State,
    prefs: &Prefs,
    source: String,
    result: Result<Vec<Release>, String>,
    ms: u64,
) {
    let command = match &st.check {
        Check::Running { command, .. } => command.clone(),
        _ => String::new(),
    };
    st.check = Check::Done {
        at: time::now_secs(),
        source: source.clone(),
        command,
        result,
        ms,
        gh_missing: false,
    };
    let line = match (&st.check, evaluation(st, prefs)) {
        (Check::Done { result: Err(e), .. }, _) => format!("{source}: failed: {e}"),
        (_, Some(e)) => match e.best() {
            Some(c) => format!(
                "{source}: {} newer, newest {}",
                e.newer.len(),
                c.release.tag_name
            ),
            None => format!("{source}: up to date ({} releases)", e.seen),
        },
        _ => source.clone(),
    };
    log("result", &line);
}

pub fn download(cx: &mut Ctx, st: &mut State, prefs: &Prefs) {
    if st.job.is_some() {
        return;
    }
    let platform = Platform::current();
    let Some(eval) = evaluation(st, prefs) else {
        cx.toast(ToastKind::Error, "Check for updates first");
        return;
    };
    let Some(c) = eval.best() else {
        cx.toast(ToastKind::Info, "There is no newer release to download");
        return;
    };
    if let Some(p) = c.problem(&platform) {
        cx.toast(ToastKind::Error, p);
        return;
    }
    let (Some(asset), Some(sums), Some(exe), Some(src)) =
        (c.asset.clone(), c.sums.clone(), info::exe(), source(prefs))
    else {
        return;
    };
    if let Some(why) = swap::dev_build_reason(&exe) {
        cx.toast(ToastKind::Error, why);
        return;
    }
    let tag = c.release.tag_name.clone();
    let throttle_kib = if matches!(src, Source::Folder(_)) {
        std::env::var("HADION_UPDATE_THROTTLE_KIB")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    } else {
        0
    };
    let job = Job {
        source: src,
        tag: tag.clone(),
        version: c.version.plain(),
        asset,
        sums,
        dir: paths::download_dir(&tag),
        exe,
        platform,
        throttle_kib,
    };
    let progress = Arc::new(Mutex::new(Progress::default()));
    let cancel = Arc::new(AtomicBool::new(false));
    st.job = Some(Running {
        version: job.version.clone(),
        progress: progress.clone(),
        cancel: cancel.clone(),
        started: Instant::now(),
    });
    st.failure = None;
    st.staged = None;
    st.restart = None;
    log(
        "download",
        &format!("{tag} {} from {}", job.asset.name, job.source.label()),
    );
    cx.user_read(format!("Download Hadion {tag}"), move |_gh, tlog| {
        let t = Instant::now();
        let r = update::run(&job, &progress, &cancel, &mut tlog.commands);
        tlog.calls.push(ApiCall {
            command: update::download_command(&job),
            elapsed_ms: t.elapsed().as_millis() as u64,
            bytes: job.asset.size as usize,
            rate: None,
            error: r.as_ref().err().map(|f| f.error.clone()),
            input: None,
        });
        let steps = match &r {
            Ok(s) => &s.steps,
            Err(f) => &f.steps,
        };
        for s in steps {
            let event = match s.step {
                Step::Download => "fetched",
                Step::Verify => "verify",
                Step::Extract => "extract",
                Step::Stage => "stage",
            };
            let detail = if s.step == Step::Stage {
                // Tag and hash first: a later session reads them back and
                // checks the staged file before swapping it in.
                let sha = r.as_ref().map(|s| s.sha256.as_str()).unwrap_or("");
                format!("{} sha256={sha} {}", job.tag, s.detail)
            } else {
                s.detail.clone()
            };
            log(event, &detail);
        }
        if let Err(f) = &r {
            log("failed", &format!("{}: {}", f.step.label(), f.error));
        }
        Msg::Downloaded(Box::new(r))
    });
}

pub fn on_downloaded(cx: &mut Ctx, st: &mut State, r: Result<Staged, Failure>) {
    st.job = None;
    match r {
        Ok(s) => {
            cx.toast(
                ToastKind::Success,
                format!(
                    "Hadion {} is verified and ready: restart to update",
                    s.version
                ),
            );
            st.staged = Some(StagedInfo {
                tag: s.tag,
                version: s.version,
                path: s.new_exe,
                sha256: s.sha256,
                bytes: s.bytes,
                steps: s.steps,
                earlier: false,
            });
        }
        Err(f) => st.failure = Some(f),
    }
    st.last_log = read_last_log();
}

pub fn cancel(st: &State) {
    if let Some(j) = &st.job {
        j.cancel.store(true, Ordering::Relaxed);
    }
}

fn args() -> Vec<OsString> {
    std::env::args_os().skip(1).collect()
}

pub fn confirm_restart(cx: &mut Ctx, st: &State) {
    let (Some(s), Some(p)) = (&st.staged, current_exe_paths()) else {
        return;
    };
    let shown: Vec<String> = args()
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    cx.confirm(
        if s.version.is_empty() {
            "Restart into the staged update?".to_owned()
        } else {
            format!("Restart into Hadion {}?", s.version)
        },
        "Hadion closes and starts again with the same repositories. If a step fails, the files are put back and this version keeps running.",
        "Restart to update",
        false,
        p.plan(&shown),
        Act::RestartNow.into(),
    );
}

/// Start the new executable with this process's arguments. The script of
/// a scripted run is not passed on (it would update again):
/// `HADION_SCRIPT_AFTER_UPDATE` names the new process's script, and a
/// scripted run without one gets `wait 5; quit`, so a test never turns
/// into a normal session that saves settings on exit.
fn relaunch(exe: &Path, args: &[OsString], old: &Path, from: &str) -> std::io::Result<u32> {
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args)
        .env("HADION_UPDATED_FROM", from)
        .env("HADION_UPDATE_OLD", old)
        .env_remove("HADION_SCRIPT")
        .env_remove("HADION_SCRIPT_AFTER_UPDATE");
    if let Ok(cwd) = std::env::current_dir() {
        cmd.current_dir(cwd);
    }
    match std::env::var("HADION_SCRIPT_AFTER_UPDATE") {
        Ok(s) => {
            cmd.env("HADION_SCRIPT", s);
        }
        Err(_) if std::env::var_os("HADION_SCRIPT").is_some() => {
            cmd.env("HADION_SCRIPT", "wait 5; quit");
        }
        Err(_) => {}
    }
    cmd.spawn().map(|c| c.id())
}

pub fn restart(cx: &mut Ctx, st: &mut State, g: &mut GlobalCx) {
    let Some(p) = current_exe_paths() else {
        cx.toast(ToastKind::Error, "Cannot tell where this executable is");
        return;
    };
    if let Some(why) = swap::dev_build_reason(&p.exe) {
        cx.toast(ToastKind::Error, why);
        return;
    }
    if st.job.is_some() {
        cx.toast(
            ToastKind::Error,
            "A download is still running: restart once it is verified and staged",
        );
        return;
    }
    // The staged file must still be the one that was verified.
    if let Some(s) = st.staged.as_ref().filter(|s| !s.sha256.is_empty())
        && p.new.is_file()
    {
        match hadion_core::devops::packaging::checksum::sha256_file(&p.new) {
            Ok(h) if h.eq_ignore_ascii_case(&s.sha256) => {}
            Ok(h) => {
                let e = format!(
                    "{} changed since it was verified (SHA-256 {h}, expected {}); download the update again",
                    p.new.display(),
                    s.sha256
                );
                log("failed", &format!("restart: {e}"));
                cx.toast(ToastKind::Error, e.clone());
                st.restart = Some(RestartResult {
                    steps: Vec::new(),
                    error: e,
                    rolled_back: false,
                });
                return;
            }
            Err(e) => {
                cx.toast(
                    ToastKind::Error,
                    format!("Could not read {}: {e}", p.new.display()),
                );
                return;
            }
        }
    }
    let b = info::build();
    let from = format!("{} ({})", b.version, b.short_commit());
    let a = args();
    let o = swap::install(&p, |exe| relaunch(exe, &a, &p.old, &from));
    for s in &o.steps {
        log(
            if s.starts_with("rolled back") {
                "rollback"
            } else {
                "swap"
            },
            s,
        );
    }
    match o.error {
        Some(e) => {
            log("failed", &format!("restart: {e}"));
            cx.toast(ToastKind::Error, format!("Update not installed: {e}"));
            st.restart = Some(RestartResult {
                steps: o.steps,
                error: e,
                rolled_back: o.rolled_back,
            });
            st.last_log = read_last_log();
        }
        None => {
            log(
                "relaunch",
                &format!(
                    "closing Hadion {from}; the new process is {}",
                    o.pid.unwrap_or(0)
                ),
            );
            g.egui.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

pub fn discard(cx: &mut Ctx, st: &mut State) {
    let Some(p) = current_exe_paths() else {
        return;
    };
    match swap::discard(&p) {
        Ok(()) => {
            log("discard", &p.new.display().to_string());
            st.staged = None;
            st.restart = None;
            cx.toast(ToastKind::Info, "The staged update was deleted");
        }
        Err(e) => cx.toast(ToastKind::Error, e),
    }
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

fn percent(p: &Progress) -> Option<u32> {
    (p.total > 0).then(|| ((p.bytes.min(p.total) * 100) / p.total) as u32)
}

pub fn status_item(ui: &mut Ui, st: &State, prefs: &Prefs) -> Option<Section> {
    let resp = if let Some(j) = &st.job {
        let pr = j.progress.lock().map(|p| p.clone()).unwrap_or_default();
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(300));
        let mut text = format!("Downloading Hadion {}", j.version);
        if let Some(n) = percent(&pr) {
            text.push_str(&format!(" · {n}%"));
        }
        let r = kit::badge(ui, &text, Tone::Accent);
        ui.add(egui::Spinner::new().size(11.0).color(pal(ui).accent));
        r
    } else if let Some(s) = &st.staged {
        kit::badge(
            ui,
            &if s.version.is_empty() {
                "Restart to update".to_owned()
            } else {
                format!("Restart to update to {}", s.version)
            },
            Tone::Success,
        )
    } else {
        // Only a release that can actually be installed.
        let c = evaluation(st, prefs)
            .and_then(|e| e.best().cloned())
            .filter(|c| c.problem(&Platform::current()).is_none())?;
        kit::badge(
            ui,
            &format!("Update to {} available", c.release.tag_name),
            Tone::Accent,
        )
    };
    ui.add_space(6.0);
    resp.interact(Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
        .then_some(Section::Updates)
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

pub fn show(ui: &mut Ui, st: &mut State, settings: &mut Settings, actions: &mut Vec<Action>) {
    let prefs = &mut settings.ext.packaging;
    let src = source(prefs);
    let checking = matches!(st.check, Check::Running { .. });
    kit::page_header(
        ui,
        "Updates",
        "New versions from GitHub releases, verified with SHA-256 before they replace this one",
        |ui| {
            if form::primary(
                ui,
                if checking { "Checking…" } else { "Check now" },
                src.is_some() && !checking,
            )
            .clicked()
            {
                actions.push(Act::CheckNow.into());
            }
        },
    );
    let b = info::build();
    let exe = info::exe();
    let dev = exe.as_deref().and_then(swap::dev_build_reason);
    kit::scroll_page(ui, "packaging_updates", |ui| {
        ui.set_max_width(ui.available_width().min(super::WIDTH));
        if let Some(d) = paths::test_dir() {
            kit::callout(
                ui,
                Tone::Warning,
                "Test mode",
                &format!(
                    "HADION_UPDATE_DIR is set: releases come from {} instead of GitHub, and downloads are copies from that folder. The source repository below is not used.",
                    d.join("releases.json").display()
                ),
            );
        }
        if let Some(r) = RELAUNCHED.get() {
            kit::callout(
                ui,
                Tone::Success,
                &format!("Updated to Hadion {}", b.version),
                &format!(
                    "This session started after an update from {}. Clean-up: {}.",
                    r.from, r.cleanup
                ),
            );
        }
        if let Some(why) = &dev {
            kit::callout(ui, Tone::Neutral, "Development build", why);
        }

        kit::section(ui, "This version", None);
        crate::views::insights::stats(
            ui,
            &[
                kit::Stat::new("Version", b.version.clone()),
                kit::Stat::new("Commit", b.commit_label()).tone_if(b.dirty, Tone::Warning),
                kit::Stat::new(
                    "Built",
                    time::parse_utc(&b.date)
                        .map(|t| {
                            let (y, m, d, ..) = time::utc_parts(t);
                            format!("{y:04}-{m:02}-{d:02}")
                        })
                        .unwrap_or_else(|| "unknown".into()),
                ),
                kit::Stat::new("Platform", Platform::current().label()),
            ],
        );

        // What happens now first; the settings after it.
        kit::section(ui, "Status", None);
        status_block(ui, st, prefs, src.as_ref(), dev.is_some(), actions);

        if st.job.is_some() || st.staged.is_some() || st.failure.is_some() || st.restart.is_some() {
            kit::section(ui, "Install", None);
            install_card(ui, st, dev.is_some(), actions);
        }

        if let Some(e) = evaluation(st, prefs)
            && !e.newer.is_empty()
        {
            kit::section(ui, "What's new", Some(e.newer.len()));
            notes(ui, &e, actions);
        }

        kit::section(ui, "Source", None);
        source_card(ui, st, prefs, actions);

        kit::section_with(ui, "Last update", None, |ui| {
            if let Some(p) = info::update_log()
                && p.exists()
                && IconButton::new(Icon::External, "Open log")
                    .compact()
                    .show(ui)
                    .clicked()
            {
                actions.push(Action::OpenPath(p));
            }
        });
        if st.last_log.is_empty() {
            kit::muted(
                ui,
                "Nothing yet. Every check, download, verification and restart is written to the update log, so a failed update can be followed afterwards.",
            );
        } else {
            kit::code_block(ui, &st.last_log.join("\n"), actions);
        }
    });
}

fn source_card(ui: &mut Ui, st: &State, prefs: &mut Prefs, actions: &mut Vec<Action>) {
    form::card(ui, Tone::Neutral, |ui| {
        let invalid = !prefs.source.trim().is_empty() && !release::valid_repo(&prefs.source);
        form::field(
            ui,
            "Repository",
            Some(
                "owner/name of the GitHub repository whose releases are Hadion updates. Read with your gh sign-in; crash reports are filed there too.",
            ),
            |ui| {
                form::input(
                    ui,
                    "packaging_source",
                    &mut prefs.source,
                    form::InputOpts::new("owner/name").invalid(invalid),
                );
            },
        );
        if invalid {
            form::error(ui, "Use the form owner/name, e.g. saziknows/hadion-gui.");
        }
        for (repo, why) in &st.suggestions {
            if prefs.source.trim().eq_ignore_ascii_case(repo) {
                continue;
            }
            ui.horizontal_wrapped(|ui| {
                if IconButton::new(Icon::Link, &format!("Use {repo}"))
                    .compact()
                    .show(ui)
                    .clicked()
                {
                    actions.push(Act::SetSource(repo.clone()).into());
                }
                kit::secondary(ui, why);
            });
        }
        ui.add_space(space::S);
        form::field(ui, "Channel", None, |ui| {
            for c in [Channel::Stable, Channel::PreRelease] {
                if form::radio(ui, prefs.channel == c, c.label()) {
                    prefs.channel = c;
                }
            }
        });
        form::checkbox(
            ui,
            &mut prefs.check_on_start,
            "Check for updates when Hadion starts",
        );
        kit::secondary(
            ui,
            "Off by default: Hadion only goes online when you press Check now. Nothing is ever installed without your click.",
        );
    });
}

fn status_block(
    ui: &mut Ui,
    st: &State,
    prefs: &Prefs,
    src: Option<&Source>,
    dev: bool,
    actions: &mut Vec<Action>,
) {
    let b = info::build();
    match &st.check {
        Check::Never => {
            form::card(ui, Tone::Neutral, |ui| {
                kit::subtitle(ui, "Not checked yet");
                match src {
                    Some(s) => {
                        kit::secondary(
                            ui,
                            &format!(
                                "Check now reads the newest ten releases of {} and compares them with {} ({}). Nothing is downloaded until you choose to.",
                                s.label(),
                                b.version,
                                b.commit_label()
                            ),
                        );
                        if let Source::GitHub(r) = s {
                            kit::code_block(
                                ui,
                                &format!("gh api {}", releases_endpoint(r)),
                                actions,
                            );
                        }
                    }
                    None => {
                        kit::secondary(
                            ui,
                            "Enter the source repository under Source below first. Hadion does not guess one and never goes online on its own.",
                        );
                    }
                }
            });
        }
        Check::Running {
            since,
            command,
            source,
        } => {
            form::card(ui, Tone::Neutral, |ui| {
                kit::loading(
                    ui,
                    &format!(
                        "Reading the releases of {source}… {}",
                        format::millis(since.elapsed().as_millis() as u64)
                    ),
                );
                kit::code_block(ui, command, actions);
            });
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(200));
        }
        Check::Done {
            gh_missing: true, ..
        } => {
            let mut again = false;
            let program = hadion_core::github::gh_program();
            let why = if program == "gh" {
                "Hadion reads releases with the GitHub CLI (gh) and your gh sign-in; gh was not found on PATH.".to_owned()
            } else {
                format!(
                    "Hadion reads releases with the GitHub CLI and your gh sign-in; HADION_GH names {program}, which does not exist."
                )
            };
            kit::callout_with(ui, Tone::Warning, "gh is not installed", &why, |ui| {
                ui.label(
                    RichText::new("Install it with")
                        .size(size::SMALL)
                        .color(pal(ui).text_2),
                );
                ui.label(kit::mono(Tool::Gh.install_hint()).color(pal(ui).text));
                again = IconButton::new(Icon::Fetch, "Check again")
                    .compact()
                    .show(ui)
                    .clicked();
            });
            if again {
                hadion_core::devops::tools::refresh();
                actions.push(Act::CheckNow.into());
            }
        }
        Check::Done {
            result: Err(e),
            command,
            source,
            ..
        } => {
            let mut retry = false;
            kit::callout_with(
                ui,
                Tone::Danger,
                &format!("Could not read the releases of {source}"),
                e,
                |ui| {
                    ui.label(kit::mono(command).color(pal(ui).text_2));
                    retry = IconButton::new(Icon::Rerun, "Try again")
                        .compact()
                        .show(ui)
                        .clicked();
                },
            );
            if retry {
                actions.push(Act::CheckNow.into());
            }
        }
        Check::Done { at, source, ms, .. } => {
            let Some(e) = evaluation(st, prefs) else {
                return;
            };
            let checked = format!(
                "Checked {} at {} in {}.",
                source,
                format::clock(*at),
                format::millis(*ms)
            );
            match e.best() {
                None => {
                    let (tone, title, body) = match &e.latest {
                        None if e.seen == 0 => (
                            Tone::Neutral,
                            format!("{source} has no releases yet"),
                            format!(
                                "There is nothing to update to. Releases need the assets hadion-<version>-{}.zip and SHA256SUMS. {checked}",
                                Platform::current().label()
                            ),
                        ),
                        None => (
                            Tone::Neutral,
                            "No release in this channel".to_owned(),
                            format!(
                                "{} looked at, none usable: {}. {checked}",
                                format::plural(e.seen, "release", "releases"),
                                e.skipped.join(", ")
                            ),
                        ),
                        Some((r, v)) if *v < e.current => (
                            Tone::Success,
                            format!(
                                "This build is newer than the latest release ({})",
                                r.tag_name
                            ),
                            format!(
                                "Hadion {} ({}) is ahead of every release. {checked}",
                                b.version,
                                b.commit_label()
                            ),
                        ),
                        Some((r, _)) => (
                            Tone::Success,
                            format!("Hadion {} is up to date", b.version),
                            format!(
                                "The newest {} is {}{}. {checked}",
                                if prefs.channel == Channel::Stable {
                                    "stable release"
                                } else {
                                    "release"
                                },
                                r.tag_name,
                                r.published_at
                                    .as_deref()
                                    .and_then(time::parse_utc)
                                    .map(|t| format!(", published {}", time::human(t)))
                                    .unwrap_or_default()
                            ),
                        ),
                    };
                    kit::callout(ui, tone, &title, &body);
                }
                Some(c) => {
                    let platform = Platform::current();
                    let problem = c.problem(&platform);
                    let ready = st
                        .staged
                        .as_ref()
                        .is_some_and(|s| s.tag == c.release.tag_name);
                    form::card(
                        ui,
                        if problem.is_some() {
                            Tone::Warning
                        } else if ready {
                            Tone::Success
                        } else {
                            Tone::Accent
                        },
                        |ui| {
                            ui.horizontal(|ui| {
                                kit::title(
                                    ui,
                                    &if ready {
                                        format!(
                                            "Hadion {} is verified and ready to install",
                                            c.version.plain()
                                        )
                                    } else {
                                        format!("Hadion {} is available", c.version.plain())
                                    },
                                );
                                if c.release.is_prerelease() {
                                    kit::badge(ui, "Pre-release", Tone::Warning);
                                }
                            });
                            kit::secondary(
                                ui,
                                &format!(
                                    "You have {} ({}). {} {checked}",
                                    b.version,
                                    b.commit_label(),
                                    if e.newer.len() > 1 {
                                        format!(
                                            "{} newer releases; the newest is offered.",
                                            e.newer.len()
                                        )
                                    } else {
                                        "One newer release.".to_owned()
                                    }
                                ),
                            );
                            ui.add_space(space::XS);
                            kit::kv(ui, 110.0, |kv| {
                                kv.text(
                                    "Release",
                                    format!("{} · {}", c.release.tag_name, c.release.title()),
                                );
                                if let Some(t) =
                                    c.release.published_at.as_deref().and_then(time::parse_utc)
                                {
                                    kv.text("Published", time::human(t));
                                }
                                match &c.asset {
                                    Some(a) => kv.text(
                                        "Asset",
                                        kit::mono(format!(
                                            "{} · {}",
                                            a.name,
                                            format::bytes(a.size as usize)
                                        )),
                                    ),
                                    None => kv.muted("Asset", "none for this platform"),
                                }
                                match &c.sums {
                                    Some(_) => kv.text(
                                        "Checksums",
                                        if c.asset
                                            .as_ref()
                                            .and_then(|a| a.digest.as_ref())
                                            .is_some()
                                        {
                                            "SHA256SUMS, and GitHub's asset digest"
                                        } else {
                                            "SHA256SUMS"
                                        },
                                    ),
                                    None => kv.muted("Checksums", "no SHA256SUMS file"),
                                }
                            });
                            if let Some(p) = &problem {
                                ui.add_space(space::XS);
                                form::error(ui, p);
                            }
                            let busy = st.job.is_some();
                            let staged_this = st
                                .staged
                                .as_ref()
                                .is_some_and(|s| s.tag == c.release.tag_name);
                            form::actions_row(ui, |ui| {
                                if staged_this {
                                    // Verified and staged: the next step is here too.
                                    if form::primary(ui, "Restart to update", !dev).clicked() {
                                        actions.push(Act::Restart.into());
                                    }
                                } else if form::primary(
                                    ui,
                                    if busy {
                                        "Downloading…"
                                    } else {
                                        "Download and verify"
                                    },
                                    problem.is_none() && !busy && !dev,
                                )
                                .clicked()
                                {
                                    actions.push(Act::Download.into());
                                }
                                if !c.release.html_url.is_empty()
                                    && IconButton::new(Icon::External, "Open on GitHub")
                                        .show(ui)
                                        .clicked()
                                {
                                    actions.push(Action::OpenUrl(c.release.html_url.clone()));
                                }
                            });
                            if dev {
                                kit::muted(
                                    ui,
                                    "Downloads are off for development builds (see above).",
                                );
                            }
                        },
                    );
                }
            }
            if !e.skipped.is_empty() && e.best().is_some() {
                kit::muted(ui, &format!("Not considered: {}.", e.skipped.join(", ")));
            }
        }
    }
}

/// The state of each install step.
enum StepState {
    Pending,
    Running,
    Done(String, u64),
    Failed(String),
}

fn step_row(ui: &mut Ui, label: &str, state: &StepState, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    ui.horizontal_top(|ui| {
        if matches!(state, StepState::Running) {
            ui.add_sized(
                vec2(20.0, 20.0),
                egui::Spinner::new().size(14.0).color(p.accent),
            );
        } else {
            let (rect, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
            let c = rect.center();
            match state {
                StepState::Done(..) => icons::paint(ui.painter(), Icon::Check, c, 14.0, p.success),
                StepState::Failed(_) => icons::paint(ui.painter(), Icon::Error, c, 14.0, p.danger),
                _ => {
                    ui.painter()
                        .circle_stroke(c, 5.0, egui::Stroke::new(1.2, p.border_strong));
                }
            }
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.horizontal(|ui| {
                let color = match state {
                    StepState::Pending => p.text_2,
                    _ => p.text_strong,
                };
                ui.label(
                    RichText::new(label)
                        .size(size::BODY)
                        .family(medium())
                        .color(color),
                );
                if let StepState::Done(_, ms) = state
                    && *ms > 0
                {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format::millis(*ms))
                                .size(size::SMALL)
                                .color(p.text_3),
                        );
                    });
                }
            });
            match state {
                StepState::Done(detail, _) if !detail.is_empty() => {
                    ui.add(
                        egui::Label::new(kit::mono(detail).size(size::MONO - 0.5).color(p.text_2))
                            .wrap(),
                    );
                }
                StepState::Failed(e) => form::error(ui, e),
                _ => {}
            }
            add(ui);
        });
    });
    ui.add_space(space::S);
}

fn install_card(ui: &mut Ui, st: &State, dev: bool, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let progress = st
        .job
        .as_ref()
        .map(|j| j.progress.lock().map(|g| g.clone()).unwrap_or_default());
    let done: Vec<StepDone> = if let Some(pr) = &progress {
        pr.done.clone()
    } else if let Some(s) = &st.staged {
        s.steps.clone()
    } else if let Some(f) = &st.failure {
        f.steps.clone()
    } else {
        Vec::new()
    };
    if progress.is_some() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(200));
    }
    form::card(ui, Tone::Neutral, |ui| {
        if let Some(j) = &st.job {
            ui.horizontal(|ui| {
                kit::subtitle(ui, &format!("Installing Hadion {}", j.version));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if IconButton::new(Icon::Stop, "Cancel")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Act::CancelDownload.into());
                    }
                    ui.label(
                        RichText::new(format::millis(j.started.elapsed().as_millis() as u64))
                            .size(size::SMALL)
                            .color(p.text_3),
                    );
                });
            });
        } else if let Some(s) = &st.staged {
            kit::subtitle(
                ui,
                &if s.version.is_empty() {
                    "An update is staged".to_owned()
                } else {
                    format!("Hadion {} is ready to install", s.version)
                },
            );
        } else if let Some(f) = &st.failure {
            kit::subtitle(
                ui,
                &format!("The update stopped at {}", f.step.label().to_lowercase()),
            );
        }
        ui.add_space(space::S);
        let earlier = st.staged.as_ref().is_some_and(|s| s.earlier);
        for step in Step::ALL {
            let state = if let Some(d) = done.iter().find(|d| d.step == step) {
                StepState::Done(d.detail.clone(), d.ms)
            } else if progress.as_ref().and_then(|p| p.step) == Some(step) {
                StepState::Running
            } else if let Some(f) = st.failure.as_ref().filter(|f| f.step == step) {
                StepState::Failed(f.error.clone())
            } else if earlier {
                StepState::Done(
                    if step == Step::Stage {
                        st.staged
                            .as_ref()
                            .map(|s| format!("{} (staged in an earlier session)", s.path.display()))
                            .unwrap_or_default()
                    } else {
                        "in an earlier session".to_owned()
                    },
                    0,
                )
            } else {
                StepState::Pending
            };
            let running_download = step == Step::Download && matches!(state, StepState::Running);
            let failed_here = matches!(state, StepState::Failed(_));
            step_row(ui, step.label(), &state, |ui| {
                if running_download && let Some(pr) = &progress {
                    download_progress(ui, pr, actions);
                }
                if failed_here
                    && IconButton::new(Icon::Rerun, "Try again")
                        .compact()
                        .enabled(!dev)
                        .show(ui)
                        .clicked()
                {
                    actions.push(Act::Download.into());
                }
            });
        }
        // Restart: the last step.
        let restart_state = match &st.restart {
            Some(r) => StepState::Failed(format!(
                "{}{}",
                r.error,
                if r.rolled_back {
                    " Everything was put back; this version keeps running."
                } else {
                    ""
                }
            )),
            None => StepState::Pending,
        };
        step_row(ui, "Restart", &restart_state, |ui| {
            if let Some(r) = &st.restart
                && !r.steps.is_empty()
            {
                ui.add(
                    egui::Label::new(
                        kit::mono(r.steps.join("\n"))
                            .size(size::MONO - 0.5)
                            .color(p.text_2),
                    )
                    .wrap(),
                );
            }
            if let Some(s) = &st.staged {
                kit::secondary(
                    ui,
                    &format!(
                        "Swaps {} in for this executable, starts it with the same repositories and closes this window. The previous version is kept as .old until the new one runs.",
                        s.path.file_name().unwrap_or_default().to_string_lossy()
                    ),
                );
                ui.horizontal(|ui| {
                    if form::primary(ui, "Restart to update", !dev).clicked() {
                        actions.push(Act::Restart.into());
                    }
                    if form::button(ui, "Discard", true).clicked() {
                        actions.push(Act::DiscardStaged.into());
                    }
                });
                if !s.sha256.is_empty() {
                    kit::muted(
                        ui,
                        &format!("{} · SHA-256 {}", format::bytes(s.bytes as usize), s.sha256),
                    );
                }
            }
        });
    });
}

fn download_progress(ui: &mut Ui, pr: &Progress, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let w = ui.available_width().min(520.0);
    let (rect, _) = ui.allocate_exact_size(vec2(w, 6.0), Sense::hover());
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(3),
        p.raised.lerp_to_gamma(p.border, 0.5),
    );
    if let Some(n) = percent(pr) {
        let mut fill = rect;
        fill.set_width(rect.width() * n as f32 / 100.0);
        ui.painter()
            .rect_filled(fill, CornerRadius::same(3), p.accent);
    }
    ui.label(
        RichText::new(match percent(pr) {
            Some(n) => format!(
                "{} of {} · {n}%",
                format::bytes(pr.bytes as usize),
                format::bytes(pr.total as usize)
            ),
            None => format::bytes(pr.bytes as usize),
        })
        .size(size::SMALL)
        .color(p.text_2),
    );
    if !pr.command.is_empty() {
        let mut text = pr.command.clone();
        for l in pr.lines.iter().rev().take(6).rev() {
            text.push('\n');
            text.push_str(l);
        }
        kit::code_block(ui, &text, actions);
    }
}

fn notes(ui: &mut Ui, e: &Evaluation, actions: &mut Vec<Action>) {
    for (i, c) in e.newer.iter().enumerate() {
        if i > 0 {
            ui.add_space(space::S);
            kit::hairline(ui);
            ui.add_space(space::S);
        }
        ui.horizontal(|ui| {
            kit::subtitle(
                ui,
                &format!("{} · {}", c.release.tag_name, c.release.title()),
            );
            if i == 0 {
                kit::badge(ui, "Newest", Tone::Accent);
            }
            if c.release.is_prerelease() {
                kit::badge(ui, "Pre-release", Tone::Warning);
            }
        });
        if let Some(t) = c.release.published_at.as_deref().and_then(time::parse_utc) {
            kit::muted(ui, &format!("Published {}", time::human(t)));
        }
        let text = hadion_core::devops::review::local::plain_text(c.release.notes());
        if text.trim().is_empty() {
            kit::muted(ui, "No release notes.");
        } else {
            let shown: String = text.chars().take(6_000).collect();
            kit::body(ui, shown.trim_end());
            if text.chars().count() > 6_000 {
                kit::muted(ui, "Cut at 6,000 characters.");
            }
        }
        if !c.release.html_url.is_empty() && kit::link(ui, "Read on GitHub").clicked() {
            actions.push(Action::OpenUrl(c.release.html_url.clone()));
        }
    }
}
