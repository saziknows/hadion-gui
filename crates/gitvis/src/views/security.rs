//! Exposure page: secrets in file contents, sensitive files and leaky
//! configuration, each with where it is (staged, HEAD, history, pushed),
//! how it got there, and what to do about it.
//!
//! This module owns the page frame (section navigation, section headers,
//! progress) and the Secrets section; the other sections live in
//! `views::osint`.

use std::collections::HashSet;

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Label, Layout, Margin, RichText, Sense,
    Stroke, Ui, vec2,
};

use gitvis_core::github::{RepoInfo, SecretAlert};
use gitvis_core::ops::GitOp;
use gitvis_core::secrets::{
    self, CommitRef, ConfigFinding, ConfigFix, FileFinding, Finding, Place, Report, Severity,
};
use gitvis_core::{DiffTarget, Snapshot, repo};

use crate::format;
use crate::state::{
    Action, ExposureFilter, RepoState, ScanRun, SecurityPage, SecurityTab, Settings,
};
use crate::theme::{self, Palette, Tone, pal, radius, size, space};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Leading};
use crate::views::osint;

/// Width of the label column of fact tables on this page.
pub(crate) const KV_W: f32 = 132.0;

// ---------------------------------------------------------------------------
// Severity
// ---------------------------------------------------------------------------

/// Tone of a severity: red for critical, amber for high, neutral below.
pub fn severity_tone(s: Severity) -> Tone {
    match s {
        Severity::Critical => Tone::Danger,
        Severity::High => Tone::Warning,
        Severity::Medium | Severity::Low => Tone::Neutral,
    }
}

/// Colour of a severity dot.
pub(crate) fn severity_dot(p: &Palette, s: Severity) -> Color32 {
    match s {
        Severity::Critical => p.danger,
        Severity::High => p.warning,
        Severity::Medium => p.text_2,
        Severity::Low => p.text_3,
    }
}

/// One row of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemRef {
    Secret(usize),
    File(usize),
    Config(usize),
    Alert(usize),
}

fn alert_severity(a: &SecretAlert) -> Severity {
    if a.validity == "active" || a.publicly_leaked {
        Severity::Critical
    } else {
        Severity::High
    }
}

// ---------------------------------------------------------------------------
// Items
// ---------------------------------------------------------------------------

/// Items that pass the page's filters, most severe first. The key is the
/// finding's fingerprint (`alert:N` for GitHub alerts).
pub fn visible_items(repo: &RepoState) -> Vec<(String, ItemRef)> {
    visible_items_of(&repo.sec_page, &repo.dismissed, repo.sec_page.state)
}

fn visible_items_of(
    page: &SecurityPage,
    dismissed: &HashSet<String>,
    state: ExposureFilter,
) -> Vec<(String, ItemRef)> {
    let needle = page.filter.trim().to_lowercase();
    let matches = |texts: &[&str]| {
        needle.is_empty() || texts.iter().any(|t| t.to_lowercase().contains(&needle))
    };
    let mut rows: Vec<(Severity, u8, usize, bool, String, ItemRef)> = Vec::new();
    let keep = |fp: &str, sev: Severity| -> Option<bool> {
        if page.hidden.contains(&sev) {
            return None;
        }
        let d = dismissed.contains(fp);
        if d && !page.show_dismissed {
            return None;
        }
        Some(d)
    };
    if let Some(rep) = &page.report {
        for (i, f) in rep.findings.iter().enumerate() {
            let state_ok = match state {
                ExposureFilter::All => true,
                ExposureFilter::Staged => f.staged,
                ExposureFilter::AtHead => f.at_head,
                ExposureFilter::HistoryOnly => f.history_only(),
                ExposureFilter::Pushed => f.pushed,
                ExposureFilter::Files | ExposureFilter::Config => false,
            };
            if !state_ok {
                continue;
            }
            let paths = f.paths().join(" ");
            if !matches(&[f.name, f.rule, &paths, &f.redacted()]) {
                continue;
            }
            if let Some(d) = keep(&f.fingerprint, f.severity) {
                rows.push((
                    f.severity,
                    0,
                    i,
                    d,
                    f.fingerprint.clone(),
                    ItemRef::Secret(i),
                ));
            }
        }
        for (i, f) in rep.files.iter().enumerate() {
            let state_ok = match state {
                ExposureFilter::All | ExposureFilter::Files => true,
                ExposureFilter::Staged => f.staged,
                ExposureFilter::AtHead => f.tracked,
                ExposureFilter::HistoryOnly => !f.tracked && !f.untracked && f.added_in.is_some(),
                ExposureFilter::Pushed => f.pushed,
                ExposureFilter::Config => false,
            };
            if !state_ok || !matches(&[f.rule.name, f.rule.id, &f.path]) {
                continue;
            }
            if let Some(d) = keep(&f.fingerprint, f.severity) {
                rows.push((f.severity, 1, i, d, f.fingerprint.clone(), ItemRef::File(i)));
            }
        }
        for (i, c) in rep.config.iter().enumerate() {
            if !matches!(state, ExposureFilter::All | ExposureFilter::Config)
                || !matches(&[&c.title, c.id, &c.detail])
            {
                continue;
            }
            if let Some(d) = keep(&c.fingerprint, c.severity) {
                rows.push((
                    c.severity,
                    2,
                    i,
                    d,
                    c.fingerprint.clone(),
                    ItemRef::Config(i),
                ));
            }
        }
    }
    if let Some(Ok(alerts)) = &page.alerts {
        for (i, a) in alerts.iter().enumerate() {
            if !matches!(state, ExposureFilter::All | ExposureFilter::Pushed)
                || !matches(&[&a.display_name, &a.secret_type, &a.state])
            {
                continue;
            }
            let key = format!("alert:{}", a.number);
            let sev = alert_severity(a);
            if let Some(d) = keep(&key, sev) {
                rows.push((sev, 3, i, d, key, ItemRef::Alert(i)));
            }
        }
    }
    rows.sort_by(|a, b| {
        a.3.cmp(&b.3)
            .then(b.0.cmp(&a.0))
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
    });
    rows.into_iter().map(|r| (r.4, r.5)).collect()
}

fn dismissed_count(page: &SecurityPage, dismissed: &HashSet<String>) -> usize {
    let Some(rep) = &page.report else { return 0 };
    rep.findings
        .iter()
        .filter(|f| dismissed.contains(&f.fingerprint))
        .count()
        + rep
            .files
            .iter()
            .filter(|f| dismissed.contains(&f.fingerprint))
            .count()
        + rep
            .config
            .iter()
            .filter(|f| dismissed.contains(&f.fingerprint))
            .count()
}

/// Critical and high findings plus anything staged, not dismissed: the
/// number on the Secrets navigation item.
fn problem_count(page: &SecurityPage, dismissed: &HashSet<String>) -> usize {
    let serious = |s: Severity, staged: bool| s >= Severity::High || staged;
    let mut n = 0;
    if let Some(rep) = &page.report {
        n += rep
            .findings
            .iter()
            .filter(|f| serious(f.severity, f.staged) && !dismissed.contains(&f.fingerprint))
            .count();
        n += rep
            .files
            .iter()
            .filter(|f| serious(f.severity, f.staged) && !dismissed.contains(&f.fingerprint))
            .count();
        n += rep
            .config
            .iter()
            .filter(|c| serious(c.severity, false) && !dismissed.contains(&c.fingerprint))
            .count();
    }
    if let Some(Ok(alerts)) = &page.alerts {
        n += alerts
            .iter()
            .filter(|a| !dismissed.contains(&format!("alert:{}", a.number)))
            .count();
    }
    n
}

/// Open GitHub alerts of every kind, when any of them is known.
fn open_alerts(page: &SecurityPage) -> Option<usize> {
    let o = &page.osint;
    let parts = [
        page.alerts
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .map(Vec::len),
        o.dependabot
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .map(Vec::len),
        o.code_alerts
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .map(Vec::len),
    ];
    if parts.iter().all(Option::is_none) {
        None
    } else {
        Some(parts.iter().flatten().sum())
    }
}

// ---------------------------------------------------------------------------
// Page frame
// ---------------------------------------------------------------------------

struct Cx<'a> {
    snap: Option<&'a Snapshot>,
    dismissed: &'a HashSet<String>,
    github: Option<(String, String)>,
    info: Option<RepoInfo>,
    busy: bool,
    now: i64,
    workdir: std::path::PathBuf,
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, settings: &Settings, actions: &mut Vec<Action>) {
    let _ = settings;
    let items = visible_items_of(&repo.sec_page, &repo.dismissed, repo.sec_page.state);
    let RepoState {
        sec_page,
        dismissed,
        snapshot,
        github_remote,
        busy,
        workdir,
        ..
    } = repo;
    let cx = Cx {
        snap: snapshot.as_ref(),
        dismissed,
        github: github_remote.clone(),
        info: sec_page
            .info
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .cloned(),
        busy: busy.is_some(),
        now: format::now_secs(),
        workdir: workdir.clone(),
    };
    let groups = nav_groups(sec_page, cx.dismissed);
    let selected = SecurityTab::ALL
        .iter()
        .position(|t| *t == sec_page.tab)
        .unwrap_or(0);
    let mut picked = None;
    kit::page_with_nav(
        ui,
        "sec_nav",
        |ui| picked = nav(ui, groups, selected),
        |ui| content(ui, &cx, sec_page, &items, actions),
    );
    if let Some(tab) = picked.and_then(|i| SecurityTab::ALL.get(i)) {
        actions.push(Action::SetSecurityTab(*tab));
    }
}

type Group = (&'static str, Vec<kit::NavItem<'static>>);

fn nav_groups(page: &SecurityPage, dismissed: &HashSet<String>) -> Vec<Group> {
    let item = |t: SecurityTab| kit::NavItem::new(t.label()).hint(t.hint());
    let secrets = item(SecurityTab::Secrets).count(problem_count(page, dismissed), Tone::Danger);
    let github = item(SecurityTab::GitHub).count(open_alerts(page).unwrap_or(0), Tone::Warning);
    vec![
        ("Leaks", vec![secrets]),
        (
            "What the repository reveals",
            vec![
                item(SecurityTab::People),
                item(SecurityTab::Footprint),
                item(SecurityTab::Files),
                item(SecurityTab::History),
            ],
        ),
        (
            "Outside the repository",
            vec![github, item(SecurityTab::Supply)],
        ),
    ]
}

/// `kit::section_nav` with captions that wrap instead of running past the
/// pane ("What the repository reveals" is wider than the nav column).
fn nav(ui: &mut Ui, groups: Vec<Group>, selected: usize) -> Option<usize> {
    let p = *pal(ui);
    let mut picked = None;
    let mut offset = 0;
    for (gi, (title, items)) in groups.into_iter().enumerate() {
        ui.add_space(if gi == 0 { 2.0 } else { space::L });
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.add(Label::new(kit::caption_text(&p, title)).wrap());
        });
        ui.add_space(2.0);
        let n = items.len();
        let local = selected
            .checked_sub(offset)
            .filter(|i| *i < n)
            .unwrap_or(usize::MAX);
        if let Some(i) = kit::section_nav(ui, &[kit::NavGroup { title: "", items }], local) {
            picked = Some(offset + i);
        }
        offset += n;
    }
    picked
}

fn content(
    ui: &mut Ui,
    cx: &Cx,
    page: &mut SecurityPage,
    items: &[(String, ItemRef)],
    actions: &mut Vec<Action>,
) {
    header(ui, cx, page, actions);
    if page.tab == SecurityTab::Secrets {
        if let Some(scan) = &page.scan
            && scan_progress(ui, scan)
        {
            actions.push(Action::CancelScan);
        }
        secrets_body(ui, cx, page, items, actions);
        return;
    }
    if let Some(run) = &page.osint.run
        && kit::progress(ui, run, "Analysing", true)
    {
        actions.push(Action::CancelOsint);
    }
    let ocx = osint::Cx {
        snap: cx.snap,
        now: cx.now,
        github: cx.github.clone(),
        info: cx.info.clone(),
        workdir: cx.workdir.clone(),
        busy: cx.busy,
        dismissed: cx.dismissed.clone(),
    };
    osint::show(ui, &ocx, page, actions);
}

/// Section title, its hint, and its actions on the right.
fn header(ui: &mut Ui, cx: &Cx, page: &SecurityPage, actions: &mut Vec<Action>) {
    let tab = page.tab;
    kit::page_header(ui, tab.label(), tab.hint(), |ui| {
        match tab {
            SecurityTab::Secrets => secrets_actions(ui, cx, page, actions),
            SecurityTab::GitHub => github_actions(ui, cx, page, actions),
            _ => osint_actions(ui, cx, page, actions),
        }
        ui.add_space(space::XS);
        visibility_badges(ui, cx, page, actions);
    });
}

/// A caption-coloured status note in a header.
fn header_note(ui: &mut Ui, text: &str, tip: &str) {
    let c = pal(ui).text_3;
    kit::tip(
        ui.label(RichText::new(text).size(size::SMALL).color(c)),
        tip,
    );
}

fn secrets_actions(ui: &mut Ui, cx: &Cx, page: &SecurityPage, actions: &mut Vec<Action>) {
    let scanning = page.scanning();
    history_menu(ui, page, !scanning, actions);
    if IconButton::new(Icon::Search, "Scan working tree")
        .compact()
        .enabled(!scanning)
        .show(ui)
        .on_hover_text(
            "Read every tracked and untracked file for secrets and sensitive names; check HEAD, remotes and configuration.",
        )
        .clicked()
    {
        actions.push(Action::ScanSecrets { history: false });
    }
    if cx.github.is_some() {
        if page.gh_loading {
            let c = pal(ui).text_3;
            ui.add(egui::Spinner::new().size(13.0).color(c));
        } else if IconButton::icon(Icon::Fetch)
            .compact()
            .show(ui)
            .on_hover_text("Refresh repository visibility and GitHub secret-scanning alerts")
            .clicked()
        {
            actions.push(Action::FetchExposure);
        }
    }
    if let (Some(rep), Some(at)) = (&page.full, page.report_at)
        && !scanning
    {
        let s = &rep.stats;
        let mut t = plural(s.files_scanned, "file", "files");
        if s.history {
            t.push_str(&format!(" · {}", plural(s.commits, "commit", "commits")));
        }
        t.push_str(&format!(
            " · {}",
            format::relative(cx.now - at.elapsed().as_secs() as i64, cx.now)
        ));
        header_note(
            ui,
            &t,
            &format!(
                "Files and commits read by the last full scan, which took {}",
                format::millis(s.elapsed_ms)
            ),
        );
    }
    if page.stale && !scanning {
        kit::tip(
            kit::badge(ui, "Changed since the scan", Tone::Warning),
            "An operation changed the repository after the last full scan. Staged changes are still checked live.",
        );
    }
}

fn osint_actions(ui: &mut Ui, cx: &Cx, page: &SecurityPage, actions: &mut Vec<Action>) {
    let o = &page.osint;
    if o.running() {
        return;
    }
    let label = if o.report.is_some() {
        "Analyse again"
    } else {
        "Analyse"
    };
    if IconButton::new(Icon::Rerun, label)
        .compact()
        .show(ui)
        .on_hover_text(
            "Re-read commits, files, metadata, history leftovers and dependencies (local only)",
        )
        .clicked()
    {
        actions.push(Action::RunOsint);
    }
    if let (Some(rep), Some(at)) = (&o.report, o.report_at) {
        let s = &rep.stats;
        header_note(
            ui,
            &format!(
                "{} · {} · {}",
                plural(s.commits, "commit", "commits"),
                plural(s.files, "file", "files"),
                format::relative(cx.now - at.elapsed().as_secs() as i64, cx.now)
            ),
            &format!(
                "Commits and files read by the last analysis, which took {}",
                format::millis(s.elapsed_ms)
            ),
        );
    }
}

fn github_actions(ui: &mut Ui, cx: &Cx, page: &SecurityPage, actions: &mut Vec<Action>) {
    if cx.github.is_none() {
        return;
    }
    let o = &page.osint;
    if o.gh_loading || page.gh_loading {
        let c = pal(ui).text_2;
        ui.label(RichText::new("Loading").size(size::SMALL).color(c));
        ui.add(egui::Spinner::new().size(13.0).color(c));
    } else {
        if IconButton::new(Icon::Fetch, "Refresh")
            .compact()
            .show(ui)
            .on_hover_text("Fetch visibility, forks, alerts and the admin surface again")
            .clicked()
        {
            actions.push(Action::FetchGhSurface);
        }
        if let Some(at) = o.gh_at {
            header_note(
                ui,
                &format!(
                    "Updated {}",
                    format::relative(cx.now - at.elapsed().as_secs() as i64, cx.now)
                ),
                "",
            );
        }
    }
}

pub(crate) fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// "1 commit", "2,056 commits".
pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{} {many}", format::count(n))
    }
}

/// An absolute time, with the relative one while that is still short
/// ("2026-10-26 16:45  ·  2h ago"; old dates only once).
pub(crate) fn when(t: i64, now: i64) -> String {
    if now - t < 14 * 86_400 {
        format!("{}  ·  {}", format::absolute(t), format::relative(t, now))
    } else {
        format::absolute(t)
    }
}

/// Public / private, forks and archived as header badges (added right to
/// left). The visibility badge opens the security settings on GitHub.
fn visibility_badges(ui: &mut Ui, cx: &Cx, page: &SecurityPage, actions: &mut Vec<Action>) {
    if let Some(info) = &cx.info {
        if info.archived {
            kit::badge(ui, "Archived", Tone::Neutral);
        }
        if info.forks > 0 {
            kit::badge(
                ui,
                &plural(info.forks as usize, "fork", "forks"),
                Tone::Neutral,
            );
        }
        let public = info.is_public();
        let (text, tone) = if public {
            ("Public".to_owned(), Tone::Warning)
        } else {
            (capitalized(&info.visibility), Tone::Neutral)
        };
        let mut tip = if public {
            "Anyone can read this repository and its full history; forks keep copies of every commit."
                .to_owned()
        } else {
            "Only collaborators can read this repository. Secrets in it are still visible to all of them."
                .to_owned()
        };
        tip.push_str(&format!(
            "\n\n{} stars, {} watchers, default branch {}",
            format::count(info.stars as usize),
            format::count(info.watchers as usize),
            info.default_branch
        ));
        tip.push_str(&format!(
            "\nSecret scanning: {}\nPush protection: {}",
            info.secret_scanning
                .as_deref()
                .unwrap_or("not visible to this account"),
            info.push_protection
                .as_deref()
                .unwrap_or("not visible to this account")
        ));
        tip.push_str("\n\nClick to open the security settings on GitHub.");
        let resp = kit::badge(ui, &text, tone)
            .interact(Sense::click())
            .on_hover_text(tip)
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.clicked()
            && let Some((o, n)) = &cx.github
        {
            actions.push(Action::OpenUrl(format!(
                "https://github.com/{o}/{n}/settings/security_analysis"
            )));
        }
    } else if let Some(Err(e)) = &page.info
        && cx.github.is_some()
    {
        kit::tip(kit::badge(ui, "GitHub unavailable", Tone::Warning), e);
    }
}

fn history_menu(ui: &mut Ui, page: &SecurityPage, enabled: bool, actions: &mut Vec<Action>) {
    let label = if page.history_depth == 0 {
        "Scan history (all commits)".to_owned()
    } else {
        format!(
            "Scan history ({} commits)",
            format::count(page.history_depth)
        )
    };
    let resp = IconButton::new(Icon::Clock, &label)
        .compact()
        .enabled(enabled)
        .show(ui)
        .on_hover_text("Also read the added lines of past commits. Finds secrets that were committed and later removed: they are still in every clone.");
    if !enabled {
        return;
    }
    egui::Popup::menu(&resp)
        .id(egui::Id::unique("sec_history_menu"))
        .show(|ui| {
            let p = *pal(ui);
            ui.set_min_width(320.0);
            ui.label(kit::caption_text(&p, "Commits to read, newest first"));
            ui.add_space(2.0);
            for (n, label) in [
                (500, "500 commits: seconds"),
                (2_000, "2,000 commits: the default"),
                (10_000, "10,000 commits: a few minutes on a large repository"),
                (0, "Entire history: can take long; cancel any time"),
            ] {
                if ui.radio(page.history_depth == n, label).clicked() {
                    actions.push(Action::SetHistoryDepth(n));
                }
            }
            ui.separator();
            let mut all = page.all_refs;
            if ui
                .checkbox(&mut all, "All branches, tags and remotes (not only HEAD)")
                .clicked()
            {
                actions.push(Action::ToggleAllRefs);
            }
            ui.separator();
            ui.add(
                Label::new(
                    RichText::new(
                        "Only added lines are scanned (git log -p -U0), so a 2,000 commit scan of a large repository streams about 100 MB and takes 10 to 20 s.",
                    )
                    .size(size::SMALL)
                    .color(p.text_2),
                )
                .wrap(),
            );
            ui.add_space(space::XS);
            if IconButton::new(Icon::Search, "Scan working tree and history")
                .primary(true)
                .show(ui)
                .clicked()
            {
                actions.push(Action::ScanSecrets { history: true });
                ui.close();
            }
        });
}

/// The running scan with bytes read and, for history, the time left.
/// Returns true when cancel was clicked.
fn scan_progress(ui: &mut Ui, scan: &ScanRun) -> bool {
    let mut pr = scan.progress.clone();
    let elapsed = scan.started.elapsed().as_millis() as u64;
    let mut extra = Vec::new();
    if pr.bytes > 0 {
        extra.push(format::bytes(pr.bytes as usize));
    }
    if scan.history
        && pr.total.is_some_and(|t| t > 0)
        && pr.done > 0
        && pr.phase.starts_with("Scanning history")
    {
        let per = elapsed as f64 / pr.done as f64;
        let left = ((pr.total.unwrap_or(0).saturating_sub(pr.done)) as f64 * per / 1000.0) as i64;
        extra.push(format!("about {} left", format::duration(left)));
    }
    if !extra.is_empty() {
        let phase = if pr.phase.is_empty() {
            "Scanning".to_owned()
        } else {
            pr.phase.clone()
        };
        pr.phase = format!("{phase} ({})", extra.join(", "));
    }
    let view = ScanRun {
        started: scan.started,
        progress: pr,
        cancel: scan.cancel.clone(),
        history: scan.history,
        automatic: scan.automatic,
    };
    kit::progress(ui, &view, "Scanning", true)
}

// ---------------------------------------------------------------------------
// Secrets section
// ---------------------------------------------------------------------------

/// List filters edited by the list pane while the detail pane reads the page.
struct ListFilters {
    filter: String,
    state: ExposureFilter,
    hidden: HashSet<Severity>,
    show_dismissed: bool,
}

fn secrets_body(
    ui: &mut Ui,
    cx: &Cx,
    page: &mut SecurityPage,
    items: &[(String, ItemRef)],
    actions: &mut Vec<Action>,
) {
    if page.report.is_none() && page.alerts.is_none() {
        secrets_empty(ui, page, actions);
        return;
    }
    let mut lf = ListFilters {
        filter: page.filter.clone(),
        state: page.state,
        hidden: page.hidden.clone(),
        show_dismissed: page.show_dismissed,
    };
    {
        let page: &SecurityPage = page;
        kit::split_keyed(
            ui,
            "sec_list",
            page.selected.as_deref().unwrap_or(""),
            actions,
            |ui, actions| secrets_list(ui, cx, page, &mut lf, items, actions),
            |ui, actions| detail(ui, cx, page, actions),
        );
    }
    page.filter = lf.filter;
    page.state = lf.state;
    page.hidden = lf.hidden;
    page.show_dismissed = lf.show_dismissed;
}

fn secrets_empty(ui: &mut Ui, page: &SecurityPage, actions: &mut Vec<Action>) {
    if page.scanning() {
        kit::empty_state(
            ui,
            Icon::Shield,
            "Scanning",
            "Reading the working tree, HEAD and configuration. Findings appear here when the scan ends.",
            None,
        );
        return;
    }
    if kit::empty_state(
        ui,
        Icon::Shield,
        "Is this repository exposing anything it should not?",
        "A scan reads every file for credentials (cloud keys, tokens, private keys, passwords in URLs and configuration) and sensitive file names (.env, id_rsa, *.pem, credential stores, Terraform state). For each one it tells you whether it is staged, in HEAD or only in history, and whether it was pushed. Remote URLs and git configuration are checked for embedded passwords.",
        Some((Icon::Search, "Scan working tree")),
    ) {
        actions.push(Action::ScanSecrets { history: false });
    }
    ui.vertical_centered(|ui| {
        ui.set_max_width(440.0);
        ui.add_space(space::XS);
        history_menu(ui, page, true, actions);
        ui.add_space(space::L);
        kit::muted(
            ui,
            "Staged changes are checked automatically after every refresh, and the commit button asks before committing a possible secret. Secrets never appear in the activity log or on a command line.",
        );
        if let Some(e) = &page.error {
            ui.add_space(space::M);
            let c = pal(ui).danger;
            ui.add(Label::new(RichText::new(e).size(size::SMALL).color(c)).wrap());
        }
    });
}

fn state_hint(f: ExposureFilter) -> &'static str {
    match f {
        ExposureFilter::All => "Everything that passes the severity filters",
        ExposureFilter::Staged => "Would go into the next commit",
        ExposureFilter::AtHead => "Present in the HEAD tree right now",
        ExposureFilter::HistoryOnly => "Committed once and removed since, but still in every clone",
        ExposureFilter::Pushed => "Reachable from a remote-tracking branch: already shared",
        ExposureFilter::Files => "Sensitive file names",
        ExposureFilter::Config => "Remote URLs and git configuration",
    }
}

fn secrets_list(
    ui: &mut Ui,
    cx: &Cx,
    page: &SecurityPage,
    lf: &mut ListFilters,
    items: &[(String, ItemRef)],
    actions: &mut Vec<Action>,
) {
    kit::search_box(
        ui,
        "sec_filter",
        &mut lf.filter,
        "Filter by rule, path or file name",
    );
    ui.add_space(space::S);
    let anything = page.report.as_ref().is_some_and(|r| r.total() > 0)
        || page
            .alerts
            .as_ref()
            .is_some_and(|a| a.as_ref().is_ok_and(|a| !a.is_empty()));
    if anything {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for f in ExposureFilter::ALL {
                let n = visible_items_of(page, cx.dismissed, f).len();
                if f != ExposureFilter::All && n == 0 && lf.state != f {
                    continue;
                }
                let label = format!("{} {}", f.label(), format::count(n));
                if kit::tip(kit::chip(ui, &label, lf.state == f), state_hint(f)).clicked() {
                    lf.state = f;
                }
            }
            let dn = dismissed_count(page, cx.dismissed);
            if (dn > 0 || lf.show_dismissed)
                && kit::tip(
                    kit::chip(ui, &format!("Dismissed {dn}"), lf.show_dismissed),
                    "Show findings marked as false positives",
                )
                .clicked()
            {
                lf.show_dismissed = !lf.show_dismissed;
            }
        });
    }
    if let Some(rep) = &page.report
        && rep.total() > 0
    {
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for s in Severity::ALL {
                let n = rep.count(s);
                let shown = !lf.hidden.contains(&s);
                let label = format!("{} {}", s.label(), format::count(n));
                let tip = if shown {
                    "Shown; click to hide this severity"
                } else {
                    "Hidden; click to show this severity"
                };
                if kit::tip(kit::chip(ui, &label, shown), tip).clicked() && !lf.hidden.remove(&s) {
                    lf.hidden.insert(s);
                }
            }
        });
    }
    ui.add_space(space::S);

    if items.is_empty() {
        ui.add_space(space::S);
        let text = match &page.report {
            Some(r) if r.is_empty() && page.full.is_some() => {
                "Nothing found. The working tree, HEAD and configuration look clean."
            }
            Some(r) if r.is_empty() && page.scanning() => "Scanning; findings appear here.",
            Some(r) if r.is_empty() => {
                "Nothing in the uncommitted changes. Scan the working tree for the rest."
            }
            Some(_) => "No findings match the filters.",
            None => "No scan yet.",
        };
        ui.horizontal(|ui| {
            ui.add_space(space::XS);
            kit::muted(ui, text);
        });
        return;
    }
    let selected = page.selected.clone();
    let p = *pal(ui);
    kit::list(
        ui,
        "sec_items",
        items.len(),
        kit::ROW_H,
        |i| selected.as_deref() == Some(items[i].0.as_str()),
        |ui, rect, resp, i| {
            let (key, item) = &items[i];
            let sel = selected.as_deref() == Some(key.as_str());
            let dismissed = cx.dismissed.contains(key);
            let line = match (item, page.report.as_ref(), page.alerts.as_ref()) {
                (ItemRef::Secret(i), Some(rep), _) => {
                    rep.findings.get(*i).map(|f| secret_line(f, cx.now))
                }
                (ItemRef::File(i), Some(rep), _) => rep.files.get(*i).map(file_line),
                (ItemRef::Config(i), Some(rep), _) => rep.config.get(*i).map(config_line),
                (ItemRef::Alert(i), _, Some(Ok(alerts))) => {
                    alerts.get(*i).map(|a| alert_line(a, cx.now))
                }
                _ => None,
            };
            if let Some(mut line) = line {
                if dismissed {
                    line.badges.insert(0, ("Dismissed".into(), Tone::Neutral));
                    line.badges.truncate(3);
                }
                let row = kit::Row::new(&line.title)
                    .leading(Leading::Dot(severity_dot(&p, line.severity)))
                    .subtitle(&line.subtitle)
                    .meta(&line.meta)
                    .badges(&line.badges)
                    .dimmed(dismissed);
                kit::paint_row(ui, rect, &row, sel);
            }
            if resp.clicked() {
                actions.push(Action::SelectFinding(Some(key.clone())));
            }
            resp.context_menu(|ui| {
                ui.set_min_width(200.0);
                if dismissed {
                    if ui.button("Restore (not a false positive)").clicked() {
                        actions.push(Action::RestoreFinding(key.clone()));
                        ui.close();
                    }
                } else if ui.button("Dismiss as false positive").clicked() {
                    actions.push(Action::DismissFinding(key.clone()));
                    ui.close();
                }
                if let (ItemRef::Secret(i), Some(rep)) = (item, page.report.as_ref())
                    && let Some(f) = rep.findings.get(*i)
                {
                    if ui.button("Copy secret").clicked() {
                        actions.push(Action::Copy(f.secret.clone()));
                        ui.close();
                    }
                    if let Some(p) = f.paths().first()
                        && ui.button("Copy path").clicked()
                    {
                        actions.push(Action::Copy((*p).to_owned()));
                        ui.close();
                    }
                }
                if let (ItemRef::File(i), Some(rep)) = (item, page.report.as_ref())
                    && let Some(f) = rep.files.get(*i)
                    && ui.button("Copy path").clicked()
                {
                    actions.push(Action::Copy(f.path.clone()));
                    ui.close();
                }
            });
        },
    );
}

/// Owned content of one list row.
struct Line {
    severity: Severity,
    title: String,
    subtitle: String,
    meta: String,
    badges: Vec<(String, Tone)>,
}

/// Where a finding is, as at most three badges in reading order.
pub(crate) fn finding_badge_list(f: &Finding) -> Vec<(String, Tone)> {
    // (priority, display order, text, tone): the three most important are
    // kept, then shown in display order.
    let mut b: Vec<(u8, u8, &str, Tone)> = Vec::new();
    if f.at_head {
        b.push((2, 0, "In HEAD", Tone::Warning));
    } else if f.in_worktree {
        b.push((2, 0, "Uncommitted", Tone::Warning));
    } else if !f.commits.is_empty() {
        b.push((3, 0, "History", Tone::Neutral));
    }
    if f.staged {
        b.push((0, 1, "Staged", Tone::Danger));
    }
    if f.pushed {
        b.push((1, 2, "Pushed", Tone::Danger));
    }
    if f.test_only {
        b.push((4, 3, "Test files", Tone::Neutral));
    }
    b.sort_by_key(|x| x.0);
    b.truncate(3);
    b.sort_by_key(|x| x.1);
    b.into_iter().map(|x| (x.2.to_owned(), x.3)).collect()
}

fn secret_line(f: &Finding, now: i64) -> Line {
    let mut sub = match f.occurrences.first() {
        Some(o) if o.line > 0 => format!("{}:{}", o.path, o.line),
        Some(o) => o.path.clone(),
        None => String::new(),
    };
    let paths = f.paths().len();
    if paths > 1 {
        sub.push_str(&format!(" +{} more", paths - 1));
    }
    sub.push_str(&format!(" · {}", f.redacted()));
    Line {
        severity: f.severity,
        title: f.name.to_owned(),
        subtitle: sub,
        meta: f
            .first_commit()
            .map(|c| format::relative(c.time, now))
            .unwrap_or_default(),
        badges: finding_badge_list(f),
    }
}

fn file_line(f: &FileFinding) -> Line {
    let mut badges: Vec<(String, Tone)> = Vec::new();
    if f.staged {
        badges.push(("Staged".into(), Tone::Danger));
    } else if f.tracked {
        badges.push(("Tracked".into(), Tone::Warning));
    } else if f.untracked {
        badges.push(("Untracked".into(), Tone::Neutral));
    } else if f.added_in.is_some() {
        badges.push(("History".into(), Tone::Neutral));
    }
    if f.pushed {
        badges.push(("Pushed".into(), Tone::Danger));
    }
    if f.secrets > 0 {
        badges.push(("Has secrets".into(), Tone::Danger));
    }
    let mut sub = f.path.clone();
    if let Some(c) = &f.added_in {
        sub.push_str(&format!(" · added by {}", c.author));
    }
    Line {
        severity: f.severity,
        title: f.rule.name.to_owned(),
        subtitle: sub,
        meta: f
            .size
            .map(|s| format::bytes(s as usize))
            .unwrap_or_default(),
        badges,
    }
}

fn config_line(c: &ConfigFinding) -> Line {
    Line {
        severity: c.severity,
        title: c.title.clone(),
        subtitle: c.detail.lines().next().unwrap_or("").to_owned(),
        meta: String::new(),
        badges: vec![("Configuration".into(), Tone::Neutral)],
    }
}

fn alert_line(a: &SecretAlert, now: i64) -> Line {
    let mut badges = vec![("GitHub alert".to_owned(), Tone::Neutral)];
    if a.validity == "active" {
        badges.push(("Active".into(), Tone::Danger));
    }
    if a.publicly_leaked {
        badges.push(("Leaked".into(), Tone::Danger));
    }
    let mut sub = format!("#{} · {}", a.number, a.state);
    if a.push_protection_bypassed {
        sub.push_str(" · push protection bypassed");
    }
    Line {
        severity: alert_severity(a),
        title: a.display_name.clone(),
        subtitle: sub,
        meta: a
            .created
            .map(|t| format::relative(t, now))
            .unwrap_or_default(),
        badges,
    }
}

// ---------------------------------------------------------------------------
// Shared detail pieces (also used by `views::osint` and `views::insights`)
// ---------------------------------------------------------------------------

/// Top of a detail pane: badges and actions on one line, the title under
/// them.
pub(crate) fn detail_head(
    ui: &mut Ui,
    badges: &[(String, Tone)],
    title: &str,
    actions_ui: impl FnOnce(&mut Ui),
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            for (t, tone) in badges {
                kit::badge(ui, t, *tone);
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            actions_ui(ui);
        });
    });
    ui.add_space(2.0);
    kit::title(ui, title);
}

/// "Why it matters" note.
pub(crate) fn why(ui: &mut Ui, text: &str) {
    ui.add_space(space::XS);
    kit::callout(ui, Tone::Accent, "Why it matters", text);
}

/// A section header.
pub(crate) fn sec(ui: &mut Ui, title: &str, count: Option<usize>) {
    kit::section(ui, title, count);
}

/// A key-value row whose value is coloured text.
pub(crate) fn kv_colored(kv: &mut kit::Kv, label: &str, text: &str, color: Color32) {
    let t = RichText::new(text).color(color);
    kv.row(label, |ui| {
        ui.add(Label::new(t).wrap());
    });
}

/// Monospace block without a copy button (the value of a secret, a path).
pub(crate) fn code_box(ui: &mut Ui, text: &str) {
    let p = *pal(ui);
    code_frame(ui).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.add(Label::new(kit::mono(text).color(p.text)).wrap());
    });
}

fn code_frame(ui: &Ui) -> Frame {
    let p = pal(ui);
    Frame::NONE
        .fill(if ui.visuals().dark_mode {
            p.chrome
        } else {
            p.raised
        })
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(radius::M))
        .inner_margin(Margin::symmetric(12, 9))
}

/// A numbered "what to do" step: number, title, explanation, then `extra`
/// (buttons, commands).
pub(crate) fn step(ui: &mut Ui, n: usize, title: &str, body: &str, extra: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    ui.add_space(space::S);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = space::M;
        let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
        ui.painter().circle_filled(
            rect.center(),
            11.0,
            p.accent.gamma_multiply(if dark { 0.20 } else { 0.14 }),
        );
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            n.to_string(),
            FontId::new(size::SMALL, theme::semibold()),
            p.accent,
        );
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.add_space(2.0);
            ui.add(
                Label::new(
                    RichText::new(title)
                        .size(size::BODY)
                        .family(theme::semibold())
                        .color(p.text_strong),
                )
                .wrap(),
            );
            if !body.is_empty() {
                kit::secondary(ui, body);
            }
            extra(ui);
        });
    });
}

/// Buttons under a step, left to right.
pub(crate) fn step_buttons(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        add(ui)
    });
}

pub(crate) fn severity_badge(s: Severity) -> (String, Tone) {
    (s.label().to_owned(), severity_tone(s))
}

pub(crate) fn commit_text(c: &CommitRef, now: i64) -> String {
    format!(
        "{} · {} · {} · {}",
        repo::short_id(c.id),
        format::relative(c.time, now),
        c.author,
        c.summary
    )
}

/// A commit as a link to the graph when it is in it, plain text otherwise.
fn commit_value(
    ui: &mut Ui,
    snap: Option<&Snapshot>,
    c: &CommitRef,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let in_graph = snap.is_some_and(|s| s.index_of.contains_key(&c.id));
    let text = commit_text(c, now);
    if in_graph {
        let r = kit::link(ui, &text)
            .on_hover_text(format!("{}\nShow in the graph", format::absolute(c.time)));
        if r.clicked() {
            actions.push(Action::JumpTo(c.id));
        }
    } else {
        let t = pal(ui).text;
        ui.add(Label::new(RichText::new(text).color(t)).wrap())
            .on_hover_text(format::absolute(c.time));
    }
}

// ---------------------------------------------------------------------------
// Detail
// ---------------------------------------------------------------------------

fn detail(ui: &mut Ui, cx: &Cx, page: &SecurityPage, actions: &mut Vec<Action>) {
    let Some(key) = page.selected.clone() else {
        overview(ui, cx, page, actions);
        return;
    };
    if let Some(n) = key.strip_prefix("alert:") {
        if let Some(Ok(alerts)) = &page.alerts
            && let Some(a) = alerts.iter().find(|a| a.number.to_string() == n)
        {
            alert_detail(ui, cx, a, &key, actions);
        } else {
            overview(ui, cx, page, actions);
        }
        return;
    }
    let Some(rep) = &page.report else {
        overview(ui, cx, page, actions);
        return;
    };
    if let Some(f) = rep.findings.iter().find(|f| f.fingerprint == key) {
        let revealed = page.revealed.contains(&key);
        secret_detail(ui, cx, rep, f, revealed, actions);
    } else if let Some(f) = rep.files.iter().find(|f| f.fingerprint == key) {
        file_detail(ui, cx, rep, f, actions);
    } else if let Some(c) = rep.config.iter().find(|c| c.fingerprint == key) {
        config_detail(ui, cx, c, actions);
    } else {
        overview(ui, cx, page, actions);
    }
}

fn dismiss_button(ui: &mut Ui, cx: &Cx, key: &str, actions: &mut Vec<Action>) {
    if cx.dismissed.contains(key) {
        if IconButton::new(Icon::Undo, "Restore")
            .compact()
            .show(ui)
            .on_hover_text("This was marked as a false positive; show it again")
            .clicked()
        {
            actions.push(Action::RestoreFinding(key.to_owned()));
        }
    } else if IconButton::new(Icon::Close, "Dismiss")
        .compact()
        .show(ui)
        .on_hover_text("Not a secret (false positive). Hides it from the list and the counts; remembered for this repository.")
        .clicked()
    {
        actions.push(Action::DismissFinding(key.to_owned()));
    }
}

/// The context line with the secret highlighted (and redacted unless revealed).
fn context_line(ui: &mut Ui, text: &str, start: usize, end: usize, secret: &str, revealed: bool) {
    let p = *pal(ui);
    let mono = FontId::monospace(size::MONO);
    let (start, end) = if start <= end
        && end <= text.len()
        && text.is_char_boundary(start)
        && text.is_char_boundary(end)
    {
        (start, end)
    } else {
        (0, 0)
    };
    let shown = if revealed {
        text[start..end].to_owned()
    } else {
        secrets::redact(secret)
    };
    let mut job = LayoutJob::default();
    let plain = TextFormat {
        font_id: mono.clone(),
        color: p.text_2,
        ..Default::default()
    };
    let hot = TextFormat {
        font_id: mono,
        color: p.danger,
        background: p.danger.gamma_multiply(0.15),
        ..Default::default()
    };
    job.append(&text[..start], 0.0, plain.clone());
    job.append(&shown, 0.0, hot);
    job.append(&text[end..], 0.0, plain);
    job.wrap.max_width = ui.available_width() - 26.0;
    code_frame(ui).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.label(job);
    });
}

fn place_badge(place: Place) -> (String, Tone) {
    let tone = match place {
        Place::Staged => Tone::Danger,
        Place::Head | Place::Unstaged | Place::Untracked | Place::Worktree => Tone::Warning,
        Place::History | Place::Stash | Place::Unreachable => Tone::Neutral,
    };
    let label = match place {
        Place::Head => "HEAD".to_owned(),
        other => capitalized(other.label()),
    };
    (label, tone)
}

/// What an occurrence without a commit means.
fn place_text(place: Place) -> &'static str {
    match place {
        Place::Head => "In the HEAD tree; click to open the file",
        Place::Worktree => "In the file on disk; click for the diff",
        Place::Staged => "In the staged changes; click for the diff",
        Place::Unstaged => "In the unstaged changes; click for the diff",
        Place::Untracked => "In an untracked file; click for the diff",
        Place::History => "In a past commit",
        Place::Stash => "In a stash entry; click to copy its SHA",
        Place::Unreachable => "In an unreachable object; click to copy its SHA",
    }
}

fn occurrences(ui: &mut Ui, cx: &Cx, f: &Finding, actions: &mut Vec<Action>) {
    let h = (f.occurrences.len() as f32 * kit::ROW_H).min(kit::ROW_H * 5.0);
    egui::ScrollArea::vertical()
        .id_salt(("sec_occ", &f.fingerprint))
        .max_height(h)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for o in &f.occurrences {
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), kit::ROW_H), Sense::click());
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                kit::row_background(ui, rect, resp.hovered(), false);
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                let title = if o.line > 0 {
                    format!("{}:{}", o.path, o.line)
                } else {
                    o.path.clone()
                };
                let sub = if let Some(c) = o
                    .commit
                    .and_then(|id| f.commits.iter().find(|c| c.id == id))
                {
                    format!(
                        "{} · {} · {} · {}",
                        repo::short_id(c.id),
                        format::relative(c.time, cx.now),
                        c.author,
                        c.summary
                    )
                } else if let Some(id) = o.commit {
                    repo::short_id(id).to_string()
                } else {
                    place_text(o.place).to_owned()
                };
                let badges = [place_badge(o.place)];
                let row = kit::Row::new(&title).mono().subtitle(&sub).badges(&badges);
                kit::paint_row(ui, rect, &row, false);
                if resp.clicked() {
                    open_occurrence(cx, o, actions);
                }
                resp.context_menu(|ui| {
                    ui.set_min_width(180.0);
                    if ui.button("Copy path").clicked() {
                        actions.push(Action::Copy(o.path.clone()));
                        ui.close();
                    }
                    if ui.button("Copy path:line").clicked() {
                        actions.push(Action::Copy(format!("{}:{}", o.path, o.line)));
                        ui.close();
                    }
                    if let Some(id) = o.commit
                        && ui.button("Copy commit SHA").clicked()
                    {
                        actions.push(Action::Copy(id.to_string()));
                        ui.close();
                    }
                    if ui.button("Open file").clicked() {
                        actions.push(Action::OpenPath(cx.workdir.join(&o.path)));
                        ui.close();
                    }
                });
            }
        });
}

/// Click on an occurrence: the diff of the file where it is.
fn open_occurrence(cx: &Cx, o: &secrets::Occurrence, actions: &mut Vec<Action>) {
    match o.place {
        Place::Staged => actions.push(Action::OpenFile(DiffTarget::Staged {
            path: o.path.clone(),
            old_path: None,
        })),
        Place::Unstaged | Place::Worktree => actions.push(Action::OpenFile(DiffTarget::Unstaged {
            path: o.path.clone(),
        })),
        Place::Untracked => actions.push(Action::OpenFile(DiffTarget::Untracked {
            path: o.path.clone(),
        })),
        Place::History | Place::Head => {
            if let Some(id) = o.commit {
                let parent = cx
                    .snap
                    .and_then(|s| {
                        s.index_of
                            .get(&id)
                            .map(|&i| s.commits[i].parent_ids.first().copied())
                    })
                    .flatten();
                if cx.snap.is_some_and(|s| s.index_of.contains_key(&id)) {
                    actions.push(Action::OpenFile(DiffTarget::Commit {
                        id,
                        parent,
                        path: o.path.clone(),
                        old_path: None,
                    }));
                } else {
                    actions.push(Action::Copy(id.to_string()));
                }
            } else {
                actions.push(Action::OpenPath(cx.workdir.join(&o.path)));
            }
        }
        Place::Stash | Place::Unreachable => {
            if let Some(id) = o.commit {
                actions.push(Action::Copy(id.to_string()));
            }
        }
    }
}

fn secret_detail(
    ui: &mut Ui,
    cx: &Cx,
    rep: &Report,
    f: &Finding,
    revealed: bool,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let stats = &rep.stats;
    let mut badges = vec![severity_badge(f.severity)];
    if f.test_only {
        badges.push(("Test files".into(), Tone::Neutral));
    }
    detail_head(ui, &badges, f.name, |ui| {
        dismiss_button(ui, cx, &f.fingerprint, actions);
        if IconButton::new(Icon::Copy, "Copy secret")
            .compact()
            .show(ui)
            .on_hover_text("Copy the clear-text value, e.g. to find it in the provider console")
            .clicked()
        {
            actions.push(Action::Copy(f.secret.clone()));
        }
        if IconButton::text(if revealed { "Hide" } else { "Reveal" })
            .compact()
            .selected(revealed)
            .show(ui)
            .on_hover_text("Show the value in clear on this page")
            .clicked()
        {
            actions.push(Action::ToggleReveal(f.fingerprint.clone()));
        }
    });
    why(ui, f.why);
    if f.test_only {
        kit::secondary(
            ui,
            "Every occurrence is under a test, fixture, example, docs or vendored path (third_party, vendor, node_modules), so the severity was lowered one step.",
        );
    }

    // The value.
    sec(ui, "Value", None);
    let shown = if revealed {
        f.secret.clone()
    } else {
        f.redacted()
    };
    code_box(ui, &shown);
    ui.add_space(space::XS);
    kit::kv(ui, KV_W, |kv| {
        kv.text(
            "Length",
            plural(f.secret.chars().count(), "character", "characters"),
        );
        kv.text(
            "Entropy",
            format!("{:.2} bits per character", secrets::entropy(&f.secret)),
        );
        kv.text("Rule", kit::mono(f.rule));
        kv.text("Fingerprint", kit::mono(&f.fingerprint));
    });

    // Exposure facts.
    sec(ui, "Exposure", None);
    let (title, body, tone) = if f.staged && !f.at_head && f.commits.is_empty() {
        (
            "Staged",
            "It goes into the next commit unless you unstage it first.",
            Tone::Danger,
        )
    } else if f.at_head {
        (
            "In HEAD",
            "Every clone and every collaborator has it right now.",
            Tone::Danger,
        )
    } else if f.uncommitted_only() {
        (
            "Only in uncommitted changes",
            "Nothing has left this machine yet.",
            Tone::Warning,
        )
    } else if f.history_only() {
        (
            "Removed from HEAD, still in history",
            "Anyone who clones the repository can recover it.",
            Tone::Danger,
        )
    } else {
        ("In the working tree", "", Tone::Warning)
    };
    kit::callout(ui, tone, title, body);
    ui.add_space(space::S);
    kit::kv(ui, KV_W, |kv| {
        let (text, color) = if f.pushed {
            (
                "Yes: at least one commit carrying it is reachable from a remote-tracking branch. Assume it is public.",
                p.danger,
            )
        } else if stats.remotes == 0 && !rep.commands.is_empty() {
            (
                "No remotes are configured; it has not been shared through git.",
                p.text_2,
            )
        } else if f.commits.is_empty() && !f.at_head {
            ("Not committed, so not pushed.", p.text_2)
        } else if !stats.history && !f.at_head {
            (
                "Unknown: scan history to find the commits that carry it.",
                p.text_2,
            )
        } else {
            (
                "No: no remote-tracking branch reaches a commit that carries it (as of the last fetch).",
                p.success,
            )
        };
        kv_colored(kv, "Pushed", text, color);
        match f.first_commit() {
            Some(c) => {
                kv.row("First committed", |ui| {
                    commit_value(ui, cx.snap, c, cx.now, actions)
                });
                if f.commit_count > 1
                    && let Some(last) = f.last_commit()
                    && last.id != c.id
                {
                    kv.row("Last committed", |ui| {
                        commit_value(ui, cx.snap, last, cx.now, actions)
                    });
                }
            }
            None if stats.history => kv.muted("First committed", "Not in the scanned commits"),
            None => kv.muted("First committed", "Unknown: history was not scanned"),
        }
        kv.text(
            "Occurrences",
            format!(
                "{} in {}",
                plural(f.occurrence_count, "occurrence", "occurrences"),
                plural(f.paths().len(), "file", "files")
            ),
        );
        if f.commit_count > 0 {
            kv.text("Commits", plural(f.commit_count, "commit", "commits"));
        } else {
            kv.muted("Commits", "None scanned");
        }
        if let Some(info) = &cx.info {
            if info.is_public() {
                kv_colored(
                    kv,
                    "Visibility",
                    &format!(
                        "Public, {}: history is readable by anyone and copied into forks.",
                        plural(info.forks as usize, "fork", "forks")
                    ),
                    p.danger,
                );
            } else {
                kv.text(
                    "Visibility",
                    format!(
                        "{} repository: readable by its collaborators.",
                        capitalized(&info.visibility)
                    ),
                );
            }
        }
    });

    // Where.
    sec(ui, "Where", Some(f.occurrences.len()));
    occurrences(ui, cx, f, actions);
    if f.occurrence_count > f.occurrences.len() {
        kit::muted(
            ui,
            &format!(
                "and {} more occurrences not listed",
                format::count(f.occurrence_count - f.occurrences.len())
            ),
        );
    }
    if let Some(o) = f.occurrences.first() {
        ui.add_space(space::S);
        context_line(ui, &o.text, o.start, o.end, &f.secret, revealed);
    }

    // What to do.
    sec(ui, "What to do", None);
    let mut n = 1;
    step(
        ui,
        n,
        "Rotate the credential; treat it as compromised",
        "Removing it from the code does not undo the exposure: it already sits in your history and in anyone's clone. Issue a new one and revoke this one.",
        |ui| {
            if let Some(url) = f.rotate {
                step_buttons(ui, |ui| {
                    if IconButton::new(Icon::External, "Open the provider's key settings")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Action::OpenUrl(url.to_owned()));
                    }
                });
            }
        },
    );
    n += 1;
    if f.in_worktree || f.at_head {
        let paths = f.paths();
        let body = format!(
            "Delete the value from {} and read it from an environment variable or a secret manager instead. Commit a placeholder or an .env.example if the shape of the setting matters.",
            paths.iter().take(3).copied().collect::<Vec<_>>().join(", ")
        );
        step(ui, n, "Take it out of the code", &body, |ui| {
            step_buttons(ui, |ui| {
                if let Some(p) = paths.first() {
                    if IconButton::new(Icon::External, "Open file")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Action::OpenPath(cx.workdir.join(p)));
                    }
                    if f.staged
                        && IconButton::new(Icon::Minus, "Unstage file")
                            .compact()
                            .enabled(!cx.busy)
                            .show(ui)
                            .on_hover_text("Keep the change out of the next commit")
                            .clicked()
                    {
                        actions.push(Action::Run(GitOp::Unstage {
                            paths: vec![(*p).to_owned()],
                            unborn: cx.snap.is_some_and(|s| s.head.target().is_none()),
                        }));
                    }
                }
            });
        });
        n += 1;
    }
    if f.at_head || !f.commits.is_empty() {
        let instructions =
            secrets::purge_instructions(if revealed { &f.secret } else { "<secret>" });
        step(
            ui,
            n,
            "Purge it from history",
            "A commit that removes the line leaves the old one in place; every clone keeps it until the history is rewritten and force-pushed. git-filter-repo replaces the value in every commit:",
            |ui| {
                code_box(ui, &instructions);
                step_buttons(ui, |ui| {
                    if IconButton::new(Icon::Copy, "Copy commands with the real value")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Action::Copy(secrets::purge_instructions(&f.secret)));
                    }
                });
                kit::callout(
                    ui,
                    Tone::Warning,
                    "",
                    "Rewriting history changes every SHA after the first affected commit.",
                );
            },
        );
        n += 1;
    }
    if f.pushed {
        step(
            ui,
            n,
            "Assume it is public",
            "Bots scrape pushes to public hosts within minutes. Check the provider's audit log for use of the credential, and make sure the new one is not pushed the same way.",
            |ui| {
                if let Some((o, r)) = &cx.github {
                    step_buttons(ui, |ui| {
                        if IconButton::new(Icon::External, "GitHub secret-scanning alerts")
                            .compact()
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Action::OpenUrl(format!(
                                "https://github.com/{o}/{r}/security/secret-scanning"
                            )));
                        }
                    });
                }
            },
        );
        n += 1;
    }
    step(
        ui,
        n,
        "Not a secret?",
        "Dismiss it here (remembered for this repository), or add a gitvis:allow comment on the line so every scan skips it.",
        |ui| step_buttons(ui, |ui| dismiss_button(ui, cx, &f.fingerprint, actions)),
    );
}

fn file_detail(ui: &mut Ui, cx: &Cx, rep: &Report, f: &FileFinding, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    detail_head(ui, &[severity_badge(f.severity)], f.rule.name, |ui| {
        dismiss_button(ui, cx, &f.fingerprint, actions);
        if IconButton::new(Icon::Copy, "Copy path")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::Copy(f.path.clone()));
        }
        if IconButton::new(Icon::External, "Open")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::OpenPath(cx.workdir.join(&f.path)));
        }
    });
    why(ui, f.rule.why);
    sec(ui, "File", None);
    code_box(ui, &f.path);
    sec(ui, "Exposure", None);
    let (title, body, tone) = if f.staged {
        (
            "Staged",
            "The file goes into the next commit.",
            Tone::Danger,
        )
    } else if f.tracked {
        (
            "Tracked",
            "The file is in the index and in HEAD; every clone has it.",
            Tone::Danger,
        )
    } else if f.untracked {
        (
            "Untracked and not ignored",
            "The next git add . would commit it.",
            Tone::Warning,
        )
    } else {
        (
            "Deleted from the tree, still in history",
            "Anyone who clones the repository can recover it.",
            Tone::Danger,
        )
    };
    kit::callout(ui, tone, title, body);
    ui.add_space(space::S);
    kit::kv(ui, KV_W, |kv| {
        if let Some(size) = f.size {
            kv.text("Size", format::bytes(size as usize));
        }
        match &f.added_in {
            Some(c) => kv.row("Added in", |ui| {
                commit_value(ui, cx.snap, c, cx.now, actions)
            }),
            None if rep.stats.history => kv.muted("Added in", "Not in the scanned commits"),
            None => kv.muted("Added in", "Unknown: history was not scanned"),
        }
        if f.pushed {
            kv_colored(
                kv,
                "Pushed",
                "Yes: reachable from a remote-tracking branch.",
                p.danger,
            );
        } else {
            kv.muted("Pushed", "Not as far as the remote-tracking branches show");
        }
        if f.secrets > 0 {
            kv_colored(
                kv,
                "Contents",
                &format!(
                    "{} found inside this file (listed separately).",
                    plural(f.secrets, "secret", "secrets")
                ),
                p.danger,
            );
        } else {
            kv.muted(
                "Contents",
                "No known secret pattern inside; the name alone is the concern",
            );
        }
    });
    sec(ui, "What to do", None);
    let mut n = 1;
    if f.tracked || f.staged {
        step(
            ui,
            n,
            "Stop tracking it (the file stays on disk)",
            "git rm --cached removes the file from the index; the next commit records the deletion.",
            |ui| {
                step_buttons(ui, |ui| {
                    if IconButton::new(Icon::Minus, "Stop tracking")
                        .compact()
                        .enabled(!cx.busy)
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Action::Run(GitOp::Untrack {
                            paths: vec![f.path.clone()],
                        }));
                    }
                });
            },
        );
        n += 1;
    }
    let base = secrets::basename(&f.path).to_owned();
    step(
        ui,
        n,
        "Ignore it",
        "Add the file to .gitignore so no broad git add picks it up again.",
        |ui| {
            step_buttons(ui, |ui| {
                if IconButton::new(Icon::Plus, &format!("Ignore {}", f.path))
                    .compact()
                    .show(ui)
                    .clicked()
                {
                    actions.push(Action::AddToGitignore(f.path.clone()));
                }
                if base != f.path
                    && IconButton::new(Icon::Plus, &format!("Ignore every {base}"))
                        .compact()
                        .show(ui)
                        .on_hover_text("Adds the bare file name, which matches it in any directory")
                        .clicked()
                {
                    actions.push(Action::AddToGitignore(base.clone()));
                }
            });
        },
    );
    n += 1;
    if f.tracked || f.added_in.is_some() {
        let instructions = secrets::purge_file_instructions(&f.path);
        step(
            ui,
            n,
            "Remove it from history",
            "Stopping tracking leaves every old version in place. git-filter-repo deletes the file from all commits:",
            |ui| kit::code_block(ui, &instructions, actions),
        );
        n += 1;
    }
    if f.secrets > 0 || f.rule.id != "database-dump" {
        step(
            ui,
            n,
            "Rotate what is inside",
            "Keys and passwords in the file are compromised once committed. Issue new ones.",
            |_| {},
        );
    }
}

fn config_detail(ui: &mut Ui, cx: &Cx, c: &ConfigFinding, actions: &mut Vec<Action>) {
    detail_head(ui, &[severity_badge(c.severity)], &c.title, |ui| {
        dismiss_button(ui, cx, &c.fingerprint, actions);
    });
    why(ui, c.why);
    sec(ui, "Setting", None);
    code_box(ui, &c.detail);
    sec(ui, "What to do", None);
    match &c.fix {
        Some(ConfigFix::SetRemoteUrl { name, url }) => {
            step(
                ui,
                1,
                "Remove the password from the remote URL",
                &format!(
                    "Sets the URL of {name} to {url}; the credential helper or an SSH key then supplies authentication."
                ),
                |ui| {
                    step_buttons(ui, |ui| {
                        if IconButton::new(Icon::Check, "Set URL without the password")
                            .compact()
                            .enabled(!cx.busy)
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Action::Run(GitOp::SetRemoteUrl {
                                name: name.clone(),
                                url: url.clone(),
                            }));
                        }
                    });
                },
            );
            step(
                ui,
                2,
                "Rotate the password or token",
                "It was stored in plain text and shown in every git remote -v.",
                |_| {},
            );
        }
        None => match c.id {
            "credential-store" => step(
                ui,
                1,
                "Switch to an encrypted helper",
                "Use manager (Windows), osxkeychain (macOS) or libsecret (Linux), then delete ~/.git-credentials.",
                |ui| kit::code_block(ui, "git config --global credential.helper manager", actions),
            ),
            "ssl-verify-off" => step(
                ui,
                1,
                "Turn verification back on",
                "Unset the setting (or set it to true). If a corporate proxy needs a custom certificate, point http.sslCAInfo at it instead.",
                |ui| kit::code_block(ui, "git config --unset http.sslVerify", actions),
            ),
            "no-gitignore" => step(
                ui,
                1,
                "Create a .gitignore",
                "Use the Ignore buttons on the file findings, or start from a template for your stack (gitignore.io).",
                |_| {},
            ),
            _ => {}
        },
    }
}

fn alert_detail(ui: &mut Ui, cx: &Cx, a: &SecretAlert, key: &str, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let mut badges = vec![
        severity_badge(alert_severity(a)),
        ("GitHub alert".into(), Tone::Neutral),
    ];
    if a.publicly_leaked {
        badges.push(("Leaked".into(), Tone::Danger));
    }
    detail_head(ui, &badges, &a.display_name, |ui| {
        dismiss_button(ui, cx, key, actions);
        if IconButton::new(Icon::External, "Open on GitHub")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::OpenUrl(a.url.clone()));
        }
    });
    why(
        ui,
        "GitHub's secret scanning found this pattern in the repository's content on GitHub. The alert page shows the exact location and lets you resolve it after rotating the credential.",
    );
    sec(ui, "Alert", None);
    kit::kv(ui, KV_W, |kv| {
        kv.text("Number", format!("#{}", a.number));
        kv.text("Type", kit::mono(&a.secret_type));
        kv.text("State", capitalized(&a.state));
        if a.validity == "active" {
            kv_colored(
                kv,
                "Validity",
                "Active: the credential still works",
                p.danger,
            );
        } else {
            kv.muted("Validity", &capitalized(&a.validity));
        }
        if let Some(t) = a.created {
            kv.text("Opened", when(t, cx.now));
        }
        if a.push_protection_bypassed {
            kv_colored(kv, "Push protection", "Bypassed by the pusher", p.danger);
        }
        if a.publicly_leaked {
            kv_colored(
                kv,
                "Publicly leaked",
                "GitHub saw this secret in a public place",
                p.danger,
            );
        }
        if let Some(r) = &a.resolution {
            kv.text("Resolution", capitalized(r));
        }
    });
}

// ---------------------------------------------------------------------------
// Overview (nothing selected)
// ---------------------------------------------------------------------------

fn overview(ui: &mut Ui, cx: &Cx, page: &SecurityPage, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    if let Some(rep) = &page.report
        && rep.total() == 0
    {
        if page.full.is_some() {
            kit::callout(
                ui,
                Tone::Success,
                "Nothing found",
                "The working tree, HEAD and configuration look clean.",
            );
        } else if page.scanning() {
            kit::loading(ui, "Scanning the working tree");
        } else {
            kit::callout(
                ui,
                Tone::Neutral,
                "Nothing in your uncommitted changes",
                "Only the staged, unstaged and untracked changes were checked so far. Scan the working tree for everything else.",
            );
        }
    }
    if let Some(rep) = &page.report
        && rep.total() > 0
    {
        let staged = rep.staged().count() + rep.staged_files().count();
        let pushed = rep.findings.iter().filter(|f| f.pushed).count()
            + rep.files.iter().filter(|f| f.pushed).count();
        let history_only = rep.findings.iter().filter(|f| f.history_only()).count();
        let critical = rep.count(Severity::Critical);
        let high = rep.count(Severity::High);
        kit::stats(
            ui,
            &[
                kit::Stat::new("Findings", format::count(rep.total())),
                kit::Stat::new("Critical", format::count(critical))
                    .tone_if(critical > 0, Tone::Danger),
                kit::Stat::new("High", format::count(high)).tone_if(high > 0, Tone::Warning),
                kit::Stat::new("Staged", format::count(staged))
                    .tone_if(staged > 0, Tone::Danger)
                    .tip("Would go into the next commit"),
                kit::Stat::new("Pushed", format::count(pushed))
                    .tone_if(pushed > 0, Tone::Danger)
                    .tip("Reachable from a remote-tracking branch"),
                kit::Stat::new("History only", format::count(history_only))
                    .tone_if(history_only > 0, Tone::Warning)
                    .tip("Removed from HEAD but still recoverable from past commits"),
            ],
        );
        kit::secondary(
            ui,
            "Select a finding on the left for the details and what to do about it.",
        );

        sec(ui, "By kind", None);
        let mut rows = vec![
            vec!["Secrets".to_owned(), format::count(rep.findings.len())],
            vec!["Sensitive files".to_owned(), format::count(rep.files.len())],
            vec![
                "Configuration issues".to_owned(),
                format::count(rep.config.len()),
            ],
        ];
        if let Some(Ok(a)) = &page.alerts {
            rows.push(vec![
                "GitHub secret-scanning alerts".to_owned(),
                format::count(a.len()),
            ]);
        }
        let dismissed = dismissed_count(page, cx.dismissed);
        if dismissed > 0 {
            rows.push(vec![
                "Dismissed as false positives".to_owned(),
                format::count(dismissed),
            ]);
        }
        kit::table(ui, &[kit::Col::text("Kind"), kit::Col::num("Count")], &rows);

        sec(ui, "By severity", None);
        let max = Severity::ALL
            .iter()
            .map(|s| rep.count(*s))
            .max()
            .unwrap_or(0)
            .max(1);
        for s in Severity::ALL {
            let n = rep.count(s);
            kit::bar_row(
                ui,
                s.label(),
                n as f32 / max as f32,
                &format::count(n),
                severity_dot(&p, s),
            );
        }

        let mut by_rule: Vec<(String, u32)> = Vec::new();
        for f in &rep.findings {
            match by_rule.iter_mut().find(|(n, _)| n == f.name) {
                Some(e) => e.1 += 1,
                None => by_rule.push((f.name.to_owned(), 1)),
            }
        }
        by_rule.sort_by_key(|e| std::cmp::Reverse(e.1));
        if !by_rule.is_empty() {
            sec(ui, "Most common secrets", None);
            kit::count_bars(ui, &by_rule, p.accent, 6);
        }
    }
    if let Some(rep) = &page.report {
        sec(ui, "What was scanned", None);
        let s = &rep.stats;
        if page.full.is_some() {
            kit::kv(ui, KV_W, |kv| {
                kv.text(
                    "Files",
                    format!(
                        "{} listed (tracked and untracked), {} read",
                        format::count(s.files_listed),
                        format::count(s.files_scanned)
                    ),
                );
                kv.text(
                    "Skipped",
                    format!(
                        "{} binary, {} over {}, {} lockfiles or media",
                        format::count(s.skipped_binary),
                        format::count(s.skipped_large),
                        format::bytes(secrets::MAX_FILE_BYTES as usize),
                        format::count(s.skipped_kind)
                    ),
                );
                kv.text(
                    "Volume",
                    format!(
                        "{} in {} lines",
                        format::bytes(s.bytes as usize),
                        format::count(s.lines as usize)
                    ),
                );
                kv.text("Duration", format::millis(s.elapsed_ms));
                if s.history {
                    kv.text(
                        "History",
                        format!(
                            "{} read{} ({}), added lines only{}",
                            plural(s.commits, "commit", "commits"),
                            match s.commits_total {
                                Some(t) if t != s.commits => format!(" of {}", format::count(t)),
                                _ => String::new(),
                            },
                            if s.all_refs {
                                "all branches, tags and remotes"
                            } else {
                                "HEAD"
                            },
                            if s.cancelled { "; cancelled early" } else { "" }
                        ),
                    );
                } else {
                    kv.muted(
                        "History",
                        "Not scanned: use Scan history to find secrets that were committed and removed again",
                    );
                }
                kv.text(
                    "Remotes",
                    format!(
                        "{}, {} reachable from remote-tracking branches",
                        plural(s.remotes, "remote", "remotes"),
                        plural(s.pushed_commits, "commit", "commits")
                    ),
                );
                kv.text(
                    "HEAD",
                    if s.head_pushed {
                        "Pushed"
                    } else {
                        "Not pushed"
                    },
                );
                kv.text(
                    "Rules",
                    format!(
                        "{} content patterns with placeholder and entropy filters, sensitive-name rules, remote URL, credential helper and TLS checks. A gitvis:allow comment on a line skips it.",
                        secrets::rules().rules.len()
                    ),
                );
            });
        } else {
            kit::muted(
                ui,
                "Only the staged, unstaged and untracked changes were checked so far. Run a scan for the whole tree.",
            );
        }
        let mut cmds = rep.commands.clone();
        cmds.dedup();
        if !cmds.is_empty() {
            ui.add_space(space::S);
            kit::code_block(ui, &cmds.join("\n"), actions);
        }
    }

    if cx.github.is_some() {
        sec(ui, "GitHub", None);
        kit::kv(ui, KV_W, |kv| {
            match &cx.info {
                Some(info) => {
                    if info.is_public() {
                        kv_colored(
                            kv,
                            "Visibility",
                            &format!(
                                "Public, {} and {} stars",
                                plural(info.forks as usize, "fork", "forks"),
                                format::count(info.stars as usize)
                            ),
                            p.warning,
                        );
                    } else {
                        kv.text(
                            "Visibility",
                            format!(
                                "{}, {}",
                                capitalized(&info.visibility),
                                plural(info.forks as usize, "fork", "forks")
                            ),
                        );
                    }
                    status_row(kv, &p, "Secret scanning", &info.secret_scanning);
                    status_row(kv, &p, "Push protection", &info.push_protection);
                    if let Some(t) = info.pushed_at {
                        kv.text("Last push", when(t, cx.now));
                    }
                }
                None if page.gh_loading => kv.muted("Visibility", "Loading"),
                None => match &page.info {
                    Some(Err(e)) => kv_colored(kv, "Visibility", e, p.warning),
                    _ => kv.muted("Visibility", "Not loaded yet"),
                },
            }
            match &page.alerts {
                Some(Ok(a)) if a.is_empty() => {
                    kv_colored(kv, "Alerts", "No open secret-scanning alerts", p.success)
                }
                Some(Ok(a)) => kv_colored(
                    kv,
                    "Alerts",
                    &format!(
                        "{} (listed on the left)",
                        plural(
                            a.len(),
                            "open secret-scanning alert",
                            "open secret-scanning alerts"
                        )
                    ),
                    p.danger,
                ),
                Some(Err(e)) => kv.muted(
                    "Alerts",
                    &format!("Unavailable: {}", e.lines().next().unwrap_or("")),
                ),
                None => {}
            }
        });
        ui.add_space(space::S);
        if let Some((o, r)) = &cx.github
            && IconButton::new(Icon::External, "Security settings on GitHub")
                .compact()
                .show(ui)
                .clicked()
        {
            actions.push(Action::OpenUrl(format!(
                "https://github.com/{o}/{r}/settings/security_analysis"
            )));
        }
    }
}

/// `enabled` / `disabled` / unknown status of a GitHub setting.
pub(crate) fn status_row(kv: &mut kit::Kv, p: &Palette, label: &str, s: &Option<String>) {
    match s.as_deref() {
        Some("enabled") => kv_colored(kv, label, "Enabled", p.success),
        Some("disabled") => kv_colored(kv, label, "Disabled", p.warning),
        Some(other) => kv.text(label, capitalized(other)),
        None => kv.muted(label, "Not visible to this account (needs admin)"),
    }
}
