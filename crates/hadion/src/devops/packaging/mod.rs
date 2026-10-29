//! Packaging: update checks and installs (GitHub releases, SHA-256
//! verified, swapped in on restart), crash reports (a panic hook writing a
//! report; offered on the next start), and the Updates / Crash reports /
//! About sections of the Settings page.
//!
//! - [`crash`]: the panic hook and the Crash reports section.
//! - [`updates`]: checking, downloading, staging and restarting, and the
//!   Updates section.
//! - [`about`]: the About section.
//! - [`info`]: build info (`build.rs`), OS version, well-known paths.
//!
//! Test switches (environment): `HADION_UPDATE_DIR` (releases from a local
//! folder), `HADION_UPDATE_THROTTLE_KIB` (slow test copies),
//! `HADION_SCRIPT_AFTER_UPDATE` (the script the relaunched copy runs),
//! `HADION_DATA_DIR` / `HADION_CRASH_DIR` (where files go),
//! `HADION_ALLOW_CRASH_TEST=1` (the `crash-test` script command).

mod about;
mod crash;
mod info;
mod updates;

use hadion_core::devops::packaging::release::{Channel, Release};
use hadion_core::devops::packaging::update::{Failure, Staged};
use serde::{Deserialize, Serialize};

use crate::devops::DevOpsAction;
use crate::devops::ctx::Ctx;
use crate::devops::global::{GlobalCx, GlobalView};
use crate::devops::settings_page::Section;
use crate::state::{Action, Page, Settings};

/// Content width of the three sections (prose stays capped at `kit::PROSE`).
const WIDTH: f32 = crate::views::kit::PROSE + 120.0;

#[derive(Default)]
pub struct State {
    /// The once-per-session start work has run.
    started: bool,
    updates: updates::State,
    crashes: crash::State,
    about: about::State,
}

#[derive(Clone, Debug)]
pub enum Act {
    /// Read the newest releases of the source (or the test folder).
    CheckNow,
    /// Download, verify and stage the newest usable release.
    Download,
    CancelDownload,
    /// Ask before swapping the executable and restarting.
    Restart,
    /// Swap and restart (after the confirmation).
    RestartNow,
    /// `update-restart`: swap and restart without asking (test mode only).
    ScriptRestart,
    /// Delete a staged `<exe>.new`.
    DiscardStaged,
    SetSource(String),
    SetChannel(Channel),
    ReloadCrashes,
    /// Show a report's text (`None` closes it).
    ViewCrash(Option<String>),
    /// Show the N-th listed report (script).
    ViewCrashAt(usize),
    CopyCrash(String),
    DeleteCrash(String),
    DeleteCrashNow(String),
    DeleteAllCrashes,
    DeleteAllCrashesNow,
    /// Ask before opening a prefilled GitHub issue for a report.
    ReportCrash(String),
    /// The same for the N-th listed report (script).
    ReportCrashAt(usize),
    /// Create the crash folder if needed, then open it.
    OpenCrashFolder,
    /// Open a folder (created when missing).
    OpenFolder(std::path::PathBuf),
    /// Panic on purpose (scripted runs with `HADION_ALLOW_CRASH_TEST=1`).
    CrashTest,
}

pub enum Msg {
    Checked {
        source: String,
        result: Result<Vec<Release>, String>,
        ms: u64,
    },
    Downloaded(Box<Result<Staged, Failure>>),
}

impl Msg {
    /// One line for the activity log (`Err` when the work failed).
    pub fn summary(&self) -> Result<String, String> {
        match self {
            Msg::Checked { source, result, .. } => match result {
                Ok(list) => Ok(format!(
                    "{} of {source}{}",
                    crate::format::plural(list.len(), "release", "releases"),
                    list.first()
                        .map(|r| format!(", newest {}", r.tag_name))
                        .unwrap_or_default()
                )),
                Err(e) => Err(format!("Update check ({source}): {e}")),
            },
            Msg::Downloaded(r) => match r.as_ref() {
                Ok(s) => Ok(format!(
                    "Hadion {} verified and staged at {}",
                    s.version,
                    s.new_exe.display()
                )),
                Err(f) => Err(format!("Update stopped at {}: {}", f.step.label(), f.error)),
            },
        }
    }
}

/// App-wide preferences (`Settings::ext`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// `owner/repo` whose GitHub releases are the updates (empty: none).
    pub source: String,
    pub channel: Channel,
    /// Check when Hadion starts (off by default: no network unless asked).
    pub check_on_start: bool,
    /// Newest crash report already looked at in the Crash reports section.
    pub crash_seen: String,
}

pub fn apply(cx: &mut Ctx, st: &mut State, g: &mut GlobalCx, act: Act) {
    match act {
        Act::CheckNow => updates::check(cx, &mut st.updates, &g.settings.ext.packaging),
        Act::Download => updates::download(cx, &mut st.updates, &g.settings.ext.packaging),
        Act::CancelDownload => updates::cancel(&st.updates),
        Act::Restart => updates::confirm_restart(cx, &st.updates),
        Act::RestartNow => updates::restart(cx, &mut st.updates, g),
        Act::ScriptRestart => {
            if hadion_core::devops::packaging::paths::test_dir().is_some() {
                updates::restart(cx, &mut st.updates, g);
            } else {
                cx.toast(
                    crate::state::ToastKind::Error,
                    "update-restart only works in test mode (HADION_UPDATE_DIR)",
                );
            }
        }
        Act::DiscardStaged => updates::discard(cx, &mut st.updates),
        Act::SetSource(s) => g.settings.ext.packaging.source = s.trim().to_owned(),
        Act::SetChannel(c) => g.settings.ext.packaging.channel = c,
        Act::ReloadCrashes => crash::reload(&mut st.crashes, &g.settings.ext.packaging),
        Act::ViewCrash(name) => crash::view(&mut st.crashes, name),
        Act::ViewCrashAt(n) => {
            let name = st.crashes.list.get(n).map(|l| l.name.clone());
            crash::view(&mut st.crashes, name);
        }
        Act::CopyCrash(name) => crash::copy(cx, &st.crashes, &name),
        Act::DeleteCrash(name) => crash::confirm_delete(cx, &st.crashes, Some(name)),
        Act::DeleteAllCrashes => crash::confirm_delete(cx, &st.crashes, None),
        Act::DeleteCrashNow(name) => {
            crash::delete(cx, &mut st.crashes, Some(&name), &g.settings.ext.packaging)
        }
        Act::DeleteAllCrashesNow => {
            crash::delete(cx, &mut st.crashes, None, &g.settings.ext.packaging)
        }
        Act::ReportCrash(name) => {
            crash::confirm_report(cx, &st.crashes, &name, &g.settings.ext.packaging)
        }
        Act::ReportCrashAt(n) => {
            if let Some(name) = st.crashes.list.get(n).map(|l| l.name.clone()) {
                crash::confirm_report(cx, &st.crashes, &name, &g.settings.ext.packaging);
            }
        }
        Act::OpenCrashFolder => {
            if let Some(d) = info::crash_dir() {
                open_folder(cx, d);
            }
        }
        Act::OpenFolder(d) => open_folder(cx, d),
        Act::CrashTest => crash::crash_test(),
    }
}

fn open_folder(cx: &mut Ctx, dir: std::path::PathBuf) {
    if let Err(e) = std::fs::create_dir_all(&dir) {
        cx.toast(
            crate::state::ToastKind::Error,
            format!("Could not create {}: {e}", dir.display()),
        );
        return;
    }
    cx.action(Action::OpenPath(dir));
}

pub fn on_msg(cx: &mut Ctx, st: &mut State, g: &mut GlobalCx, msg: Msg) {
    match msg {
        Msg::Checked { source, result, ms } => updates::on_checked(
            cx,
            &mut st.updates,
            &g.settings.ext.packaging,
            source,
            result,
            ms,
        ),
        Msg::Downloaded(r) => updates::on_downloaded(cx, &mut st.updates, *r),
    }
}

/// Every frame; keep it cheap.
pub fn tick(cx: &mut Ctx, st: &mut State, g: &mut GlobalCx) {
    if !st.started {
        st.started = true;
        info::probe_os();
        crash::start(&mut st.crashes, &g.settings.ext.packaging);
        updates::start(cx, &mut st.updates, &g.settings.ext.packaging);
        st.about.refresh();
    }
    // A background thread panicked during this session.
    crash::tick(&mut st.crashes, &g.settings.ext.packaging);
}

/// `HADION_SCRIPT` commands this area understands.
pub fn script(cmd: &str, arg: &str) -> Option<Vec<Action>> {
    let arg = arg.trim();
    let n = || arg.parse::<usize>().unwrap_or(0);
    let act = match cmd {
        "update-check" => Act::CheckNow,
        "update-download" => Act::Download,
        "update-cancel" => Act::CancelDownload,
        "update-restart" => Act::ScriptRestart,
        "update-confirm-restart" => Act::Restart,
        "update-source" => Act::SetSource(arg.to_owned()),
        "update-channel" => Act::SetChannel(if arg.starts_with("pre") {
            Channel::PreRelease
        } else {
            Channel::Stable
        }),
        "crash-test" => {
            // Both switches, so a stray script line cannot crash a session.
            if std::env::var_os("HADION_SCRIPT").is_none()
                || std::env::var("HADION_ALLOW_CRASH_TEST").as_deref() != Ok("1")
            {
                eprintln!("[script] crash-test needs HADION_ALLOW_CRASH_TEST=1");
                return Some(Vec::new());
            }
            Act::CrashTest
        }
        "crash-open" => Act::ViewCrashAt(n()),
        "crash-report" => Act::ReportCrashAt(n()),
        "crash-reload" => Act::ReloadCrashes,
        _ => return None,
    };
    Some(vec![act.into()])
}

/// A Settings section became visible (check `section`; most are not yours).
pub fn on_section_shown(_cx: &mut Ctx, st: &mut State, g: &mut GlobalCx, section: Section) {
    match section {
        Section::Updates => updates::on_shown(&mut st.updates, g),
        Section::CrashReports => crash::on_shown(&mut st.crashes, &mut g.settings.ext.packaging),
        Section::About => st.about.refresh(),
        _ => {}
    }
}

/// Updates, Crash reports and About sections.
pub fn settings_section(
    ui: &mut egui::Ui,
    st: &mut State,
    settings: &mut Settings,
    section: Section,
    actions: &mut Vec<Action>,
) {
    match section {
        Section::Updates => updates::show(ui, &mut st.updates, settings, actions),
        Section::CrashReports => crash::show(ui, &mut st.crashes, settings, actions),
        _ => about::show(ui, &mut st.about, &st.updates, settings, actions),
    }
}

/// Open a Settings section (from the status bar).
fn open_section(actions: &mut Vec<Action>, s: Section) {
    actions.push(Action::DevOps(DevOpsAction::SetSettingsSection(s)));
    actions.push(Action::ShowPage(Page::Settings));
}

/// Right side of the status bar (right-to-left): "Update available", …
pub fn hook_status_items(ui: &mut egui::Ui, st: &State, v: &GlobalView, actions: &mut Vec<Action>) {
    if let Some(s) = crash::status_item(ui, &st.crashes) {
        open_section(actions, s);
    }
    if let Some(s) = updates::status_item(ui, &st.updates, &v.settings.ext.packaging) {
        open_section(actions, s);
    }
}

/// Installed first thing in `main` (before the window opens). After an
/// update it also waits for the previous process to exit (deleting its
/// `<exe>.old`), so the two never run side by side.
pub fn install_crash_handler() {
    updates::after_relaunch();
    crash::install();
}

/// The settings and data folders of gitvis (the name before Hadion),
/// copied on the first start under the new name. Runs before the window
/// opens, since eframe reads its settings then. On Linux and macOS the two
/// folders are one, so the first copy covers both.
pub fn carry_over_from_gitvis() {
    use hadion_core::devops::packaging::{migrate, paths};
    let pairs = [
        (eframe::storage_dir("gitvis"), eframe::storage_dir("hadion")),
        (paths::legacy_data_dir(), paths::data_dir()),
    ];
    for (old, new) in pairs {
        if let (Some(old), Some(new)) = (old, new)
            && let Err(e) = migrate::carry_over(&old, &new)
        {
            eprintln!("Could not copy {} to {}: {e}", old.display(), new.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(cmd: &str, arg: &str) -> Option<Act> {
        let mut v = script(cmd, arg)?;
        assert_eq!(v.len(), 1, "{cmd}");
        match v.remove(0) {
            Action::DevOps(DevOpsAction::Packaging(a)) => Some(a),
            _ => panic!("{cmd}: not a packaging action"),
        }
    }

    #[test]
    fn script_commands() {
        assert!(matches!(one("update-check", ""), Some(Act::CheckNow)));
        assert!(matches!(one("update-download", ""), Some(Act::Download)));
        assert!(matches!(
            one("update-restart", ""),
            Some(Act::ScriptRestart)
        ));
        assert!(matches!(
            one("update-confirm-restart", ""),
            Some(Act::Restart)
        ));
        assert!(matches!(
            one("update-channel", "pre"),
            Some(Act::SetChannel(Channel::PreRelease))
        ));
        assert!(matches!(
            one("update-channel", "stable"),
            Some(Act::SetChannel(Channel::Stable))
        ));
        assert!(matches!(one("update-source", " o/r "), Some(Act::SetSource(s)) if s == "o/r"));
        assert!(matches!(one("crash-open", "2"), Some(Act::ViewCrashAt(2))));
        assert!(matches!(
            one("crash-report", "x"),
            Some(Act::ReportCrashAt(0))
        ));
        assert!(script("not-ours", "").is_none());
        // Without both switches the crash test does nothing.
        if std::env::var("HADION_ALLOW_CRASH_TEST").as_deref() != Ok("1") {
            assert_eq!(script("crash-test", "").map(|v| v.len()), Some(0));
        }
    }
}
