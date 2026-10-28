//! Crash reports: the panic hook that writes them, and the Crash reports
//! section that lists, shows, copies, deletes and (after a confirmation
//! showing exactly what goes into the link) reports them on GitHub.
//! Nothing is ever uploaded by gitvis itself.

use std::cell::Cell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use egui::{Frame, Margin, RichText, Sense, Ui, vec2};
use gitvis_core::devops::packaging::crash::{self as core, CrashInfo, Listed};
use gitvis_core::devops::packaging::{issue, release, time};

use super::{Act, Prefs, info};
use crate::devops::ctx::Ctx;
use crate::devops::settings_page::Section;
use crate::format;
use crate::state::{Action, Settings, ToastKind};
use crate::theme::{Tone, pal, radius, size, space};
use crate::views::form;
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Leading, Row};

/// Panics recorded by the hook in this process.
static PANICS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}

/// Chain a hook in front of the default one that writes a report.
pub fn install() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        // A panic while writing a report (on the same thread) must not
        // recurse: it would abort the process.
        let first = IN_HOOK.try_with(|c| !c.replace(true)).unwrap_or(false);
        if first {
            if let Some(path) = write_report(panic) {
                eprintln!("gitvis: crash report written to {}", path.display());
            }
            let _ = IN_HOOK.try_with(|c| c.set(false));
        }
        PANICS.fetch_add(1, Ordering::Relaxed);
        default(panic);
    }));
}

fn payload(panic: &std::panic::PanicHookInfo) -> String {
    if let Some(s) = panic.payload().downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = panic.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "(a panic without a text message)".to_owned()
    }
}

fn write_report(panic: &std::panic::PanicHookInfo) -> Option<PathBuf> {
    let dir = info::crash_dir()?;
    let now = time::now_secs();
    let b = info::build();
    let thread = std::thread::current();
    let report = CrashInfo {
        time: time::iso(now),
        version: b.version.clone(),
        commit: b.commit_label(),
        build_date: b.date_label(),
        target: format!("{} ({})", b.target, b.profile),
        os: info::os(),
        exe: info::exe()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        thread: thread
            .name()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("unnamed ({:?})", thread.id())),
        message: payload(panic),
        location: panic
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default(),
        backtrace: std::backtrace::Backtrace::force_capture().to_string(),
    };
    let text = core::finish(&core::format_report(&report));
    core::write_new(&dir, &time::compact(now), &text).ok()
}

/// `crash-test`: panic on purpose with a fake credential in the message,
/// so the report shows the redaction working.
pub fn crash_test() {
    let fake = format!("ghp_{}", "z1Yx2Wv3Ut4Sr5Qp6On7Ml8Kj9Ih0Hg1Fe2Dc3Ba4");
    panic!(
        "crash test requested by GITVIS_SCRIPT (GITVIS_ALLOW_CRASH_TEST=1); a fake token to redact: {fake}"
    );
}

// ---------------------------------------------------------------------------
// State and controller
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct State {
    pub loaded: bool,
    pub list: Vec<Listed>,
    pub error: Option<String>,
    /// Reports newer than `Prefs::crash_seen`.
    pub unseen: usize,
    /// One of them came from the UI thread: the window closed.
    pub unseen_fatal: bool,
    /// Names that were unseen when the section was opened (badged "New").
    pub fresh: HashSet<String>,
    /// The newest report when the section was opened with unseen ones.
    pub closed_at: Option<String>,
    /// Panics of this process already accounted for.
    pub panics_seen: usize,
    /// The report being shown: name and text (or the read error).
    pub open: Option<(String, Result<String, String>)>,
}

pub fn reload(st: &mut State, prefs: &Prefs) {
    st.loaded = true;
    match info::crash_dir() {
        None => {
            st.list.clear();
            st.error =
                Some("No folder for crash reports on this system (no LOCALAPPDATA / HOME)".into());
        }
        Some(dir) => match core::list(&dir, 200) {
            Ok(l) => {
                st.list = l;
                st.error = None;
            }
            Err(e) => {
                st.list.clear();
                st.error = Some(format!("Could not read {}: {e}", dir.display()));
            }
        },
    }
    let unseen: Vec<&Listed> = st
        .list
        .iter()
        .filter(|l| l.name.as_str() > prefs.crash_seen.as_str())
        .collect();
    st.unseen = unseen.len();
    st.unseen_fatal = unseen.iter().any(|l| l.summary.fatal());
    if let Some((name, _)) = &st.open
        && !st.list.iter().any(|l| &l.name == name)
    {
        st.open = None;
    }
}

/// Once per session: find reports written since the last look.
pub fn start(st: &mut State, prefs: &Prefs) {
    st.panics_seen = PANICS.load(Ordering::Relaxed);
    reload(st, prefs);
}

/// A background thread of this process panicked: list its report.
pub fn tick(st: &mut State, prefs: &Prefs) {
    let n = PANICS.load(Ordering::Relaxed);
    if n != st.panics_seen {
        st.panics_seen = n;
        reload(st, prefs);
    }
}

/// The section was opened: everything listed counts as seen from now on.
pub fn on_shown(st: &mut State, prefs: &mut Prefs) {
    reload(st, prefs);
    if st.unseen > 0 {
        st.fresh = st
            .list
            .iter()
            .filter(|l| l.name.as_str() > prefs.crash_seen.as_str())
            .map(|l| l.name.clone())
            .collect();
        st.closed_at = st
            .list
            .iter()
            .find(|l| l.summary.fatal() && st.fresh.contains(&l.name))
            .map(|l| l.summary.time.clone());
        // Open the newest new report straight away.
        if st.open.is_none()
            && let Some(first) = st.list.first()
        {
            let name = first.name.clone();
            view(st, Some(name));
        }
    }
    if let Some(newest) = st.list.first()
        && newest.name.as_str() > prefs.crash_seen.as_str()
    {
        prefs.crash_seen = newest.name.clone();
    }
    st.unseen = 0;
    st.unseen_fatal = false;
}

fn path_of(st: &State, name: &str) -> Option<PathBuf> {
    st.list
        .iter()
        .find(|l| l.name == name)
        .map(|l| l.path.clone())
}

pub fn view(st: &mut State, name: Option<String>) {
    st.open = name.map(|n| {
        let text = match path_of(st, &n) {
            Some(p) => std::fs::read_to_string(&p)
                .map_err(|e| format!("Could not read {}: {e}", p.display())),
            None => Err(format!("{n} is not in the crash folder any more")),
        };
        (n, text)
    });
}

fn text_of(st: &State, name: &str) -> Option<String> {
    if let Some((n, Ok(t))) = &st.open
        && n == name
    {
        return Some(t.clone());
    }
    std::fs::read_to_string(path_of(st, name)?).ok()
}

pub fn copy(cx: &mut Ctx, st: &State, name: &str) {
    match text_of(st, name) {
        Some(t) => cx.action(Action::Copy(t)),
        None => cx.toast(ToastKind::Error, format!("Could not read {name}")),
    }
}

pub fn confirm_delete(cx: &mut Ctx, st: &State, name: Option<String>) {
    let paths: Vec<PathBuf> = match &name {
        Some(n) => path_of(st, n).into_iter().collect(),
        None => st.list.iter().map(|l| l.path.clone()).collect(),
    };
    if paths.is_empty() {
        return;
    }
    let details: Vec<String> = paths
        .iter()
        .map(|p| format!("delete {}", p.display()))
        .collect();
    let (title, act) = match name {
        Some(n) => (format!("Delete {n}?"), Act::DeleteCrashNow(n)),
        None => (
            format!(
                "Delete all {}?",
                format::plural(paths.len(), "crash report", "crash reports")
            ),
            Act::DeleteAllCrashesNow,
        ),
    };
    cx.confirm(
        title,
        "The files are removed from this machine. Nothing was ever uploaded, so this is the only copy.",
        "Delete",
        true,
        details,
        act.into(),
    );
}

pub fn delete(cx: &mut Ctx, st: &mut State, name: Option<&str>, prefs: &Prefs) {
    let paths: Vec<PathBuf> = match name {
        Some(n) => path_of(st, n).into_iter().collect(),
        None => st.list.iter().map(|l| l.path.clone()).collect(),
    };
    let mut errors = Vec::new();
    for p in &paths {
        if let Err(e) = std::fs::remove_file(p)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            errors.push(format!("{}: {e}", p.display()));
        }
    }
    reload(st, prefs);
    if errors.is_empty() {
        cx.toast(
            ToastKind::Success,
            format!(
                "Deleted {}",
                format::plural(paths.len(), "crash report", "crash reports")
            ),
        );
    } else {
        cx.toast(ToastKind::Error, errors.join("\n"));
    }
}

/// The issue link for a report, or why there is none.
fn issue_link(st: &State, name: &str, prefs: &Prefs) -> Result<issue::IssueLink, String> {
    let repo = prefs.source.trim();
    if !release::valid_repo(repo) {
        return Err(
            "Set the source repository in Settings → Updates first: crash reports are filed there."
                .into(),
        );
    }
    let text = text_of(st, name).ok_or_else(|| format!("Could not read {name}"))?;
    let summary = core::summarize(&text);
    let body = format!(
        "gitvis closed unexpectedly. Report `{name}` (secrets already removed by gitvis):\n\n```text\n{}\n```\n",
        core::for_issue(&text, info::home().as_deref()).trim_end()
    );
    let note = "\n```\n\n(Cut to fit a link. The full report is on the reporter's machine.)\n";
    issue::new_issue(
        repo,
        &core::issue_title(&summary),
        &body,
        note,
        issue::MAX_BODY_ENCODED,
    )
}

pub fn confirm_report(cx: &mut Ctx, st: &State, name: &str, prefs: &Prefs) {
    match issue_link(st, name, prefs) {
        Err(e) => cx.toast(ToastKind::Error, e),
        Ok(link) => {
            let mut details = vec![
                format!(
                    "open https://github.com/{}/issues/new in your browser",
                    link.repo
                ),
                String::new(),
                format!("title = {}", link.title),
                String::new(),
                "body =".to_owned(),
            ];
            details.extend(link.body.lines().map(str::to_owned));
            details.push(String::new());
            details.push(format!(
                "The whole link ({} characters):",
                format::count(link.url.len())
            ));
            details.push(link.url.clone());
            cx.confirm(
                "Report this crash on GitHub?",
                format!(
                    "Your browser opens a new issue on {} with the text below filled in. Nothing is sent until you submit the form there.{}",
                    link.repo,
                    if link.truncated {
                        " The report was cut to fit a link."
                    } else {
                        ""
                    }
                ),
                "Open the browser",
                false,
                details,
                Action::OpenUrl(link.url),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

/// "gitvis closed unexpectedly": returns the section to open when clicked.
pub fn status_item(ui: &mut Ui, st: &State) -> Option<Section> {
    if st.unseen == 0 {
        return None;
    }
    let text = if st.unseen_fatal {
        "gitvis closed unexpectedly"
    } else {
        "A background task crashed"
    };
    let resp = kit::badge(ui, text, Tone::Warning)
        .interact(Sense::click())
        .on_hover_text("A crash report was saved on this machine. Click to read it.")
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    ui.add_space(6.0);
    resp.clicked().then_some(Section::CrashReports)
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

pub fn show(ui: &mut Ui, st: &mut State, settings: &mut Settings, actions: &mut Vec<Action>) {
    let dir = info::crash_dir();
    kit::page_header(
        ui,
        "Crash reports",
        "Written on this machine when gitvis panics, with secrets removed. Nothing is uploaded.",
        |ui| {
            if IconButton::new(Icon::Trash, "Delete all")
                .enabled(!st.list.is_empty())
                .show(ui)
                .clicked()
            {
                actions.push(Act::DeleteAllCrashes.into());
            }
            if IconButton::new(Icon::Folder, "Open folder")
                .enabled(dir.is_some())
                .show(ui)
                .clicked()
            {
                actions.push(Act::OpenCrashFolder.into());
            }
            if IconButton::new(Icon::Rerun, "Refresh").show(ui).clicked() {
                actions.push(Act::ReloadCrashes.into());
            }
        },
    );
    let prefs = &settings.ext.packaging;
    kit::scroll_page(ui, "packaging_crashes", |ui| {
        ui.set_max_width(super::WIDTH);
        if let Some(t) = &st.closed_at {
            kit::callout(
                ui,
                Tone::Warning,
                "gitvis closed unexpectedly",
                &format!(
                    "The last session ended with a crash at {}. Its report is open below: the message, where it happened and the backtrace. Nothing was sent anywhere.",
                    time::parse_utc(t)
                        .map(time::human)
                        .unwrap_or_else(|| t.clone())
                ),
            );
        }
        let total: u64 = st.list.iter().map(|l| l.bytes).sum();
        kit::stats(
            ui,
            &[
                kit::Stat::new("Reports", format::count(st.list.len())),
                kit::Stat::new(
                    "Newest",
                    st.list
                        .first()
                        .and_then(|l| time::parse_utc(&l.summary.time))
                        .map(|t| format::relative(t, format::now_secs()))
                        .unwrap_or_else(|| "none".into()),
                ),
                kit::Stat::new("On disk", format::bytes(total as usize)),
            ],
        );
        kit::kv(ui, 110.0, |kv| {
            kv.text(
                "Folder",
                kit::mono(
                    dir.as_ref()
                        .map(|d| d.display().to_string())
                        .unwrap_or_else(|| "none".into()),
                ),
            );
            kv.muted(
                "Override",
                "GITVIS_CRASH_DIR (or GITVIS_DATA_DIR) moves it, e.g. for tests",
            );
        });

        kit::section(ui, "Reports", Some(st.list.len()));
        if let Some(e) = &st.error {
            let mut retry = false;
            kit::callout_with(ui, Tone::Danger, "Could not list the reports", e, |ui| {
                retry = IconButton::new(Icon::Rerun, "Try again")
                    .compact()
                    .show(ui)
                    .clicked();
            });
            if retry {
                actions.push(Act::ReloadCrashes.into());
            }
        } else if !st.loaded {
            kit::loading(ui, "Reading the crash folder…");
        } else if st.list.is_empty() {
            form::card(ui, Tone::Neutral, |ui| {
                ui.horizontal(|ui| {
                    crate::views::icons::show(ui, Icon::Check, 16.0, pal(ui).success);
                    kit::subtitle(ui, "No crash reports");
                });
                kit::secondary(
                    ui,
                    "gitvis has not crashed on this machine, or its reports were deleted. When it panics it writes the version, the thread, the message, where it happened and a backtrace here, with secrets removed, and offers the report on the next start.",
                );
            });
        } else {
            report_list(ui, st, actions);
        }

        if let Some((name, text)) = &st.open {
            ui.add_space(space::L);
            report_detail(ui, name, text, prefs, actions);
        }
    });
}

fn report_list(ui: &mut Ui, st: &State, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let open = st.open.as_ref().map(|(n, _)| n.as_str());
    Frame::NONE
        .fill(p.raised)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(radius::L)
        .inner_margin(Margin::same(4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            for l in &st.list {
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), kit::ROW_H), Sense::click());
                let selected = open == Some(l.name.as_str());
                kit::row_background(ui, rect, resp.hovered(), selected);
                let s = &l.summary;
                let title = if s.headline.is_empty() {
                    "(no message)".to_owned()
                } else {
                    s.headline.clone()
                };
                let when = time::parse_utc(&s.time)
                    .map(time::human)
                    .unwrap_or_else(|| s.time.clone());
                let thread = if s.thread.is_empty() {
                    "unknown thread".to_owned()
                } else {
                    format!("{} thread", s.thread)
                };
                let subtitle = format!(
                    "{when}  ·  {thread}  ·  gitvis {} ({})",
                    s.version, s.commit
                );
                let mut badges: Vec<(String, Tone)> = Vec::new();
                if st.fresh.contains(&l.name) {
                    badges.push(("New".into(), Tone::Accent));
                }
                if !s.redacted.is_empty() {
                    badges.push(("Secrets removed".into(), Tone::Warning));
                }
                let lead = if s.fatal() { p.danger } else { p.warning };
                let meta = format::bytes(l.bytes as usize);
                kit::paint_row(
                    ui,
                    rect,
                    &Row::new(&title)
                        .leading(Leading::Icon(Icon::Bug, lead))
                        .subtitle(&subtitle)
                        .meta(&meta)
                        .badges(&badges),
                    selected,
                );
                if resp
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    actions.push(
                        Act::ViewCrash(if selected { None } else { Some(l.name.clone()) }).into(),
                    );
                }
            }
        });
}

fn report_detail(
    ui: &mut Ui,
    name: &str,
    text: &Result<String, String>,
    prefs: &Prefs,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    form::card(ui, Tone::Neutral, |ui| {
        ui.horizontal(|ui| {
            kit::title(ui, name);
        });
        let Ok(text) = text else {
            if let Err(e) = text {
                form::error(ui, e);
            }
            return;
        };
        let s = core::summarize(text);
        ui.add_space(space::XS);
        kit::kv(ui, 110.0, |kv| {
            kv.text(
                "Time",
                time::parse_utc(&s.time)
                    .map(time::human)
                    .unwrap_or_else(|| s.time.clone()),
            );
            kv.text("Version", format!("gitvis {} ({})", s.version, s.commit));
            kv.text(
                "Thread",
                if s.fatal() {
                    format!("{} — the window closed", s.thread)
                } else {
                    format!("{} — a background task; the window stayed open", s.thread)
                },
            );
            kv.text("Location", kit::mono(&s.location));
            if s.redacted.is_empty() {
                kv.muted("Removed", "nothing looked secret");
            } else {
                kv.text("Removed", s.redacted.clone());
            }
        });
        let valid = release::valid_repo(&prefs.source);
        form::actions_row(ui, |ui| {
            if form::primary(ui, "Report on GitHub", valid).clicked() {
                actions.push(Act::ReportCrash(name.to_owned()).into());
            }
            if IconButton::new(Icon::Copy, "Copy").show(ui).clicked() {
                actions.push(Act::CopyCrash(name.to_owned()).into());
            }
            if IconButton::new(Icon::Trash, "Delete")
                .danger()
                .show(ui)
                .clicked()
            {
                actions.push(Act::DeleteCrash(name.to_owned()).into());
            }
        });
        if valid {
            kit::muted(
                ui,
                &format!(
                    "Opens a prefilled issue on {} in your browser after showing exactly what goes into the link. Your home folder is shown as ~.",
                    prefs.source.trim()
                ),
            );
        } else {
            ui.horizontal_wrapped(|ui| {
                kit::muted(ui, "Reporting needs the source repository:");
                if kit::link(ui, "set it in Updates").clicked() {
                    actions.push(Action::DevOps(
                        crate::devops::DevOpsAction::SetSettingsSection(Section::Updates),
                    ));
                }
            });
        }
        ui.add_space(space::S);
        ui.label(kit::caption_text(&p, "Report"));
        report_text(ui, text, actions);
    });
}

/// The report in monospace, with a copy button on hover.
fn report_text(ui: &mut Ui, text: &str, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let resp = Frame::NONE
        .fill(if ui.visuals().dark_mode {
            p.chrome
        } else {
            p.canvas
        })
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(radius::M)
        .inner_margin(Margin {
            left: 12,
            right: 40,
            top: 9,
            bottom: 9,
        })
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.add(
                egui::Label::new(
                    RichText::new(text)
                        .family(egui::FontFamily::Monospace)
                        .size(size::MONO - 0.5)
                        .color(p.text),
                )
                .wrap(),
            );
        })
        .response;
    if ui.rect_contains_pointer(resp.rect) {
        let r = egui::Rect::from_min_size(
            egui::pos2(resp.rect.right() - 32.0, resp.rect.top() + 5.0),
            vec2(26.0, 24.0),
        );
        if kit::overlay(ui, r, IconButton::icon(Icon::Copy).compact())
            .on_hover_text("Copy the report")
            .clicked()
        {
            actions.push(Action::Copy(text.to_owned()));
        }
    }
}
