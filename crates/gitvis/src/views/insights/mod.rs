//! The Insights page: repository health, hotspots, ownership, branches,
//! commit quality, releases, timeline, debt, CI, licences, history search
//! and recovery. Views only read state and push `Action`s.
//!
//! Layout: a section navigation on the left (`kit::page_with_nav`), and for
//! each section a `kit::page_header` with the analysis controls, then either
//! a list + detail split or a scrolling page.

mod ci;
mod debt;
mod health;
mod history;
mod license;
mod quality;
mod recover;
mod refs;
mod search;

use std::path::PathBuf;

use egui::{
    Color32, CornerRadius, FontId, Rect, Response, RichText, Sense, Stroke, Ui, pos2, vec2,
};

use gitvis_core::insights::Severity;
use gitvis_core::{Oid, Snapshot, repo};

use crate::format;
use crate::state::{Action, InsightsPage, InsightsTab, RepoState, Settings};
use crate::theme::{self, pal, radius, size};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Col, NavGroup, NavItem, Tone};
use crate::views::truncated;

/// What the sections need from the repository besides the page state.
pub struct Cx<'a> {
    pub snap: Option<&'a Snapshot>,
    pub now: i64,
    pub workdir: PathBuf,
    pub busy: bool,
}

impl Cx<'_> {
    /// Is the commit shown in the graph (so a click can jump to it)?
    pub fn in_graph(&self, id: &Oid) -> bool {
        self.snap.is_some_and(|s| s.index_of.contains_key(id))
    }
}

/// The navigation: groups of sections, in display order.
const NAV: [(&str, &[InsightsTab]); 5] = [
    (
        "Repository",
        &[
            InsightsTab::Health,
            InsightsTab::Branches,
            InsightsTab::Releases,
        ],
    ),
    (
        "Code",
        &[
            InsightsTab::Hotspots,
            InsightsTab::Ownership,
            InsightsTab::Debt,
        ],
    ),
    ("History", &[InsightsTab::Commits, InsightsTab::Timeline]),
    (
        "Pipelines and legal",
        &[InsightsTab::Ci, InsightsTab::Licences],
    ),
    ("Tools", &[InsightsTab::Search, InsightsTab::Recovery]),
];

/// Keys of the current section's list, in display order (for the script
/// driver's `ins-item N`).
pub fn item_keys(repo: &RepoState) -> Vec<String> {
    let page = &repo.ins_page;
    match page.tab {
        InsightsTab::Search => page
            .search
            .as_ref()
            .and_then(|s| s.as_ref().ok())
            .map(|s| s.hits.iter().map(|h| h.id.to_string()).collect())
            .unwrap_or_default(),
        InsightsTab::Recovery => page
            .lost
            .as_ref()
            .and_then(|l| l.as_ref().ok())
            .map(|l| l.commits.iter().map(|c| c.id.to_string()).collect())
            .unwrap_or_default(),
        tab => {
            let Some(rep) = &page.report else {
                return Vec::new();
            };
            match tab {
                InsightsTab::Hotspots => history::visible_hotspots(rep, &page.filter)
                    .iter()
                    .map(|h| h.path.clone())
                    .collect(),
                InsightsTab::Ownership => history::visible_dirs(rep, &page.filter)
                    .iter()
                    .map(|o| o.dir.clone())
                    .collect(),
                InsightsTab::Branches => refs::branch_list(rep, page)
                    .iter()
                    .map(|b| b.full.clone())
                    .collect(),
                InsightsTab::Debt => debt::visible(rep, &page.filter)
                    .iter()
                    .map(|m| debt::marker_key(m))
                    .collect(),
                InsightsTab::Ci => (0..rep.ci.findings.len())
                    .map(|i| format!("ci:{i}"))
                    .collect(),
                InsightsTab::Health => (0..rep.health.landmines.len())
                    .map(|i| format!("mine:{i}"))
                    .collect(),
                _ => Vec::new(),
            }
        }
    }
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, _settings: &Settings, actions: &mut Vec<Action>) {
    let RepoState {
        ins_page,
        snapshot,
        busy,
        workdir,
        ..
    } = repo;
    let cx = Cx {
        snap: snapshot.as_ref(),
        now: format::now_secs(),
        workdir: workdir.clone(),
        busy: busy.is_some(),
    };
    let counts: Vec<(InsightsTab, Option<(usize, Tone)>)> = InsightsTab::ALL
        .iter()
        .map(|t| (*t, nav_count(ins_page, *t)))
        .collect();
    let tab = ins_page.tab;
    let mut clicked: Option<InsightsTab> = None;
    kit::page_with_nav(
        ui,
        "ins_nav",
        |ui| nav(ui, tab, &counts, &mut clicked),
        |ui| content(ui, &cx, ins_page, actions),
    );
    if let Some(t) = clicked {
        actions.push(Action::SetInsightsTab(t));
    }
}

// ---------------------------------------------------------------------------
// Navigation, header and empty state
// ---------------------------------------------------------------------------

fn nav(
    ui: &mut Ui,
    tab: InsightsTab,
    counts: &[(InsightsTab, Option<(usize, Tone)>)],
    clicked: &mut Option<InsightsTab>,
) {
    let order: Vec<InsightsTab> = NAV.iter().flat_map(|(_, t)| t.iter().copied()).collect();
    let groups: Vec<NavGroup> = NAV
        .iter()
        .map(|(title, tabs)| NavGroup {
            title,
            items: tabs
                .iter()
                .map(|t| {
                    let item = NavItem::new(t.label()).hint(t.hint());
                    match counts.iter().find(|(k, _)| k == t).and_then(|(_, c)| *c) {
                        Some((n, tone)) => item.count(n, tone),
                        None => item,
                    }
                })
                .collect(),
        })
        .collect();
    let selected = order.iter().position(|t| *t == tab).unwrap_or(0);
    if let Some(i) = kit::section_nav(ui, &groups, selected) {
        *clicked = order.get(i).copied();
    }
}

/// A count on a navigation item, only for problems.
fn nav_count(page: &InsightsPage, t: InsightsTab) -> Option<(usize, Tone)> {
    let rep = page.report.as_ref()?;
    match t {
        InsightsTab::Health => {
            let m = &rep.health.landmines;
            let tone = if m.iter().any(|m| m.severity >= Severity::High) {
                Tone::Danger
            } else {
                Tone::Warning
            };
            Some((m.len(), tone))
        }
        InsightsTab::Ci => {
            let f = rep
                .ci
                .findings
                .iter()
                .filter(|f| f.severity >= Severity::Medium);
            let high = rep.ci.findings.iter().any(|f| f.severity >= Severity::High);
            Some((f.count(), if high { Tone::Danger } else { Tone::Warning }))
        }
        InsightsTab::Licences => Some((
            rep.license
                .findings
                .iter()
                .filter(|f| f.severity >= Severity::Medium)
                .count(),
            Tone::Warning,
        )),
        InsightsTab::Ownership => Some((
            rep.history.ownership.iter().filter(|o| o.orphaned).count(),
            Tone::Warning,
        )),
        _ => None,
    }
}

fn content(ui: &mut Ui, cx: &Cx, page: &mut InsightsPage, actions: &mut Vec<Action>) {
    let tab = page.tab;
    kit::page_header(ui, tab.label(), tab.hint(), |ui| match tab {
        InsightsTab::Search => {}
        InsightsTab::Recovery => {
            if matches!(page.lost, Some(Ok(_)))
                && page.lost_run.is_none()
                && IconButton::new(Icon::Rerun, "Look again")
                    .compact()
                    .show(ui)
                    .on_hover_text("Run git fsck again")
                    .clicked()
            {
                actions.push(Action::RunLost);
            }
        }
        _ => analysis_actions(ui, cx, page, actions),
    });
    match tab {
        InsightsTab::Search => return search::show(ui, cx, page, actions),
        InsightsTab::Recovery => return recover::show(ui, cx, page, actions),
        _ => {}
    }
    if let Some(run) = &page.run
        && kit::progress(ui, run, "Analysing", true)
    {
        actions.push(Action::CancelInsights);
    }
    if page.report.is_none() {
        empty_state(ui, page, actions);
        return;
    }
    let InsightsPage {
        report,
        selected,
        filter,
        weight,
        weight_run,
        dating_run,
        blame,
        show_remote,
        ..
    } = page;
    let rep = report.as_ref().expect("checked above");
    match tab {
        InsightsTab::Health => health::show(ui, cx, rep, selected, weight, weight_run, actions),
        InsightsTab::Hotspots => history::hotspots(ui, cx, rep, selected, filter, actions),
        InsightsTab::Ownership => history::ownership(ui, cx, rep, selected, filter, blame, actions),
        InsightsTab::Branches => {
            refs::branches(ui, cx, rep, selected, filter, show_remote, actions)
        }
        InsightsTab::Commits => quality::show(ui, cx, rep, actions),
        InsightsTab::Releases => refs::releases(ui, cx, rep, actions),
        InsightsTab::Timeline => history::timeline(ui, cx, rep, actions),
        InsightsTab::Debt => debt::show(ui, cx, rep, selected, filter, dating_run, actions),
        InsightsTab::Ci => ci::show(ui, cx, rep, selected, actions),
        InsightsTab::Licences => license::show(ui, cx, rep, actions),
        InsightsTab::Search | InsightsTab::Recovery => {}
    }
}

/// Right side of the header (laid out right to left): analyse again, the
/// history depth, and what the last analysis read.
fn analysis_actions(ui: &mut Ui, cx: &Cx, page: &InsightsPage, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    if IconButton::new(Icon::Rerun, "Analyse again")
        .compact()
        .enabled(!page.running())
        .show(ui)
        .on_hover_text(
            "Re-read the index, the history (numstat), branches, tags, debt markers, workflows and licences (local only)",
        )
        .clicked()
    {
        actions.push(Action::RunInsights);
    }
    depth_menu(ui, page, actions);
    if let (Some(rep), Some(at)) = (&page.report, page.report_at) {
        let s = &rep.stats;
        let mut tip = format!(
            "{} of git log --numstat parsed",
            format::bytes(s.log_bytes as usize)
        );
        for (phase, ms) in &s.phases {
            tip.push_str(&format!("\n{phase}: {}", format::millis(*ms)));
        }
        if s.cancelled {
            tip.push_str("\nThe analysis was cancelled: later parts are empty.");
        }
        let text = format!(
            "{} commits  ·  {} files  ·  {}  ·  {}{}",
            format::count(s.commits),
            format::count(s.files),
            format::millis(s.elapsed_ms),
            format::relative(cx.now - at.elapsed().as_secs() as i64, cx.now),
            if s.cancelled { "  ·  cancelled" } else { "" }
        );
        let color = if s.cancelled { p.warning } else { p.text_2 };
        ui.add_space(6.0);
        kit::tip(
            ui.label(RichText::new(text).size(size::SMALL).color(color)),
            &tip,
        );
    }
}

fn depth_menu(ui: &mut Ui, page: &InsightsPage, actions: &mut Vec<Action>) {
    let mut label = if page.depth == 0 {
        "All commits".to_owned()
    } else {
        format!("{} commits", format::count(page.depth))
    };
    if page.all_refs {
        label.push_str(", all branches");
    }
    let resp = IconButton::new(Icon::ChevronDown, &label)
        .compact()
        .show(ui)
        .on_hover_text("How many of the newest commits the history pass reads with --numstat (line counts need the file contents, so this is the slow part)");
    egui::Popup::menu(&resp)
        .id(egui::Id::unique("insights_depth"))
        .show(|ui| {
            ui.set_min_width(240.0);
            let p = *pal(ui);
            ui.label(kit::caption_text(&p, "Commits the history pass reads"));
            ui.add_space(2.0);
            for (n, text) in [
                (2_000, "Newest 2,000 commits"),
                (10_000, "Newest 10,000 commits"),
                (50_000, "Newest 50,000 commits"),
                (0, "All commits"),
            ] {
                if crate::views::form::radio(ui, page.depth == n, text) {
                    actions.push(Action::SetInsightsDepth(n));
                    ui.close();
                }
            }
            ui.separator();
            if crate::views::form::checkbox(ui, &mut page.all_refs.clone(), "All branches and tags")
                .on_hover_text(
                    "Walk every ref instead of HEAD only. Hotspots and ownership then include unmerged work.",
                )
                .clicked()
            {
                actions.push(Action::ToggleInsightsAllRefs);
            }
        });
}

fn empty_state(ui: &mut Ui, page: &InsightsPage, actions: &mut Vec<Action>) {
    scroll_empty(ui, "ins_empty", |ui| {
        if let Some(e) = &page.error {
            if kit::empty_state(
                ui,
                Icon::Warning,
                "The analysis failed",
                e,
                Some((Icon::Rerun, "Try again")),
            ) {
                actions.push(Action::RunInsights);
            }
        } else if page.running() {
            kit::empty_state(
                ui,
                Icon::Chart,
                "Analysing the repository",
                "Reading the index, the history, branches, tags, debt markers, workflows and licences.",
                None,
            );
        } else if kit::empty_state(
            ui,
            Icon::Chart,
            "Not analysed yet",
            "The analysis reads the index, the history (with line counts), branches, tags, debt markers, workflows and licences. Everything stays on this machine.",
            Some((Icon::Search, "Analyse")),
        ) {
            actions.push(Action::RunInsights);
        }
    });
}

/// A page-filling empty state that scrolls when the window is too short
/// for it (otherwise its lower part is cut off). The content keeps the
/// viewport's height, so `kit::empty_state` still places itself by it.
pub(crate) fn scroll_empty(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    let h = ui.available_height();
    egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_max_height(h);
            add(ui)
        });
}

// ---------------------------------------------------------------------------
// Shared pieces (composed of kit parts)
// ---------------------------------------------------------------------------

/// One cell of a [`grid`].
pub(crate) enum Cell {
    /// Primary text.
    Text(String),
    /// Secondary text.
    Sec(String),
    /// Caption-coloured text: dates, "none".
    Muted(String),
    /// Monospace text (paths, ids).
    Mono(String),
    /// Monospace in the accent colour (a commit that can be shown).
    MonoLink(String),
    /// Text in a status colour.
    Tone(String, Tone),
    /// Badges (none when empty).
    Badges(Vec<(String, Tone)>),
    /// A bar for `0..1` with its value text on the right.
    Bar(f32, String, Color32),
}

impl Cell {
    fn text(&self) -> &str {
        match self {
            Cell::Text(s)
            | Cell::Sec(s)
            | Cell::Muted(s)
            | Cell::Mono(s)
            | Cell::MonoLink(s)
            | Cell::Tone(s, _)
            | Cell::Bar(_, s, _) => s,
            Cell::Badges(_) => "",
        }
    }
}

pub(crate) fn text(s: impl Into<String>) -> Cell {
    Cell::Text(s.into())
}

pub(crate) fn sec(s: impl Into<String>) -> Cell {
    Cell::Sec(s.into())
}

pub(crate) fn muted(s: impl Into<String>) -> Cell {
    Cell::Muted(s.into())
}

pub(crate) fn mono(s: impl Into<String>) -> Cell {
    Cell::Mono(s.into())
}

pub(crate) fn num(n: usize) -> Cell {
    Cell::Sec(format::count(n))
}

/// "+12" in green, or a quiet "0".
pub(crate) fn added(n: u64) -> Cell {
    if n == 0 {
        muted("0")
    } else {
        Cell::Tone(format!("+{}", format::count(n as usize)), Tone::Success)
    }
}

/// "−12" in red, or a quiet "0".
pub(crate) fn deleted(n: u64) -> Cell {
    if n == 0 {
        muted("0")
    } else {
        Cell::Tone(format!("−{}", format::count(n as usize)), Tone::Danger)
    }
}

/// Badges for a cell; empty texts are skipped.
pub(crate) fn badges(b: &[(&str, Tone)]) -> Cell {
    Cell::Badges(
        b.iter()
            .filter(|(t, _)| !t.is_empty())
            .map(|(t, tone)| ((*t).to_owned(), *tone))
            .collect(),
    )
}

/// Cap the widest flexible columns at one width so the total fits `room`.
fn fit_columns(widths: &mut [f32], flex: &[bool], room: f32) {
    let mut ws: Vec<f32> = widths
        .iter()
        .zip(flex)
        .filter(|(_, f)| **f)
        .map(|(w, _)| *w)
        .collect();
    ws.sort_by(f32::total_cmp);
    let n = ws.len();
    let mut remaining = room;
    let mut cap = f32::MAX;
    for (i, w) in ws.iter().enumerate() {
        let share = remaining / (n - i) as f32;
        if *w <= share {
            remaining -= w;
        } else {
            cap = share;
            break;
        }
    }
    for (w, f) in widths.iter_mut().zip(flex) {
        if *f {
            *w = w.min(cap).max(48.0);
        }
    }
}

/// A painted table in the style of `kit::table`, with richer cells and rows
/// that respond: `on_row` gets each row's response (for a click or a context
/// menu). With `click`, rows show a hover background and a pointing hand.
/// Not virtualised: callers cap the rows and add "and N more".
pub(crate) fn grid(
    ui: &mut Ui,
    id: &str,
    cols: &[Col],
    rows: &[Vec<Cell>],
    click: bool,
    mut on_row: impl FnMut(usize, &Response),
) {
    if cols.is_empty() || rows.is_empty() {
        return;
    }
    let row_h = 30.0;
    let head_h = 24.0;
    let gap = 20.0;
    let font = FontId::proportional(size::SMALL + 0.5);
    let mono_font = FontId::monospace(size::MONO);
    let head_font = FontId::new(size::CAPTION, theme::medium());
    let painter = ui.painter().clone();
    let measure = |s: &str, f: &FontId| {
        painter
            .layout_no_wrap(s.to_owned(), f.clone(), Color32::WHITE)
            .size()
            .x
    };
    let bar_track = 120.0;
    let cell_w = |c: &Cell| -> f32 {
        match c {
            Cell::Mono(s) | Cell::MonoLink(s) => measure(s, &mono_font),
            Cell::Badges(b) => b
                .iter()
                .map(|(t, _)| kit::badge_width(&painter, t) + 4.0)
                .sum::<f32>(),
            Cell::Bar(_, s, _) => bar_track + 10.0 + measure(s, &font),
            other => measure(other.text(), &font),
        }
    };
    let mut widths: Vec<f32> = cols
        .iter()
        .map(|c| measure(&c.title.to_uppercase(), &head_font) + 4.0)
        .collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(cols.len()) {
            widths[i] = widths[i].max(cell_w(cell));
        }
    }
    let avail = ui.available_width();
    let gaps = gap * (cols.len() - 1) as f32;
    if widths.iter().sum::<f32>() + gaps > avail {
        let flex: Vec<bool> = cols.iter().map(|c| !c.numeric).collect();
        let fixed: f32 = cols
            .iter()
            .zip(&widths)
            .filter(|(c, _)| c.numeric)
            .map(|(_, w)| *w)
            .sum();
        fit_columns(&mut widths, &flex, (avail - fixed - gaps).max(60.0));
    }
    let table_w = widths.iter().sum::<f32>() + gaps;
    let g = Geometry {
        widths,
        gap,
        table_w,
        row_h,
        head_h,
    };
    if table_w > avail + 0.5 {
        // Even the narrowest columns do not fit (a narrow pane): scroll
        // sideways instead of drawing past the edge.
        // The bar stays visible (the floating style hides it until hovered),
        // so the columns off to the right are found; room under the last
        // row keeps the bar from covering it.
        ui.scope(|ui| {
            let s = &mut ui.spacing_mut().scroll;
            s.dormant_handle_opacity = 0.6;
            s.dormant_background_opacity = 0.4;
            egui::ScrollArea::horizontal()
                .id_salt((id, "grid_x"))
                .auto_shrink([false, true])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                .show(ui, |ui| {
                    paint_grid(ui, id, cols, rows, click, &g, &mut on_row);
                    ui.add_space(12.0);
                });
        });
    } else {
        paint_grid(ui, id, cols, rows, click, &g, &mut on_row);
    }
    ui.add_space(2.0);
}

/// Column widths and row metrics of a [`grid`].
struct Geometry {
    widths: Vec<f32>,
    gap: f32,
    table_w: f32,
    row_h: f32,
    head_h: f32,
}

fn paint_grid(
    ui: &mut Ui,
    id: &str,
    cols: &[Col],
    rows: &[Vec<Cell>],
    click: bool,
    g: &Geometry,
    on_row: &mut impl FnMut(usize, &Response),
) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let font = FontId::proportional(size::SMALL + 0.5);
    let mono_font = FontId::monospace(size::MONO);
    let head_font = FontId::new(size::CAPTION, theme::medium());
    let painter = ui.painter().clone();
    let Geometry {
        widths,
        gap,
        table_w,
        row_h,
        head_h,
    } = g;
    let (gap, table_w, row_h, head_h) = (*gap, *table_w, *row_h, *head_h);
    let (rect, _) = ui.allocate_exact_size(
        vec2(table_w, head_h + row_h * rows.len() as f32),
        Sense::hover(),
    );
    let mut xs = Vec::with_capacity(cols.len());
    let mut x = rect.left();
    for w in widths.iter() {
        xs.push(x);
        x += w + gap;
    }
    for (i, c) in cols.iter().enumerate() {
        // Cut to the column: a squeezed column's title ran into the next.
        let t = truncated(
            &painter,
            &c.title.to_uppercase(),
            head_font.clone(),
            p.text_3,
            widths[i],
        );
        let x = if c.numeric {
            xs[i] + widths[i] - t.size().x
        } else {
            xs[i]
        };
        painter.galley(
            pos2(x, rect.top() + head_h / 2.0 - t.size().y / 2.0),
            t,
            p.text_3,
        );
    }
    let pointer = ui.ctx().pointer_hover_pos();
    for (r, row) in rows.iter().enumerate() {
        let y = rect.top() + head_h + r as f32 * row_h;
        let row_rect = Rect::from_min_size(pos2(rect.left(), y), vec2(table_w, row_h));
        let resp = ui.interact(row_rect, ui.scope_id().with((id, r)), Sense::click());
        if click && resp.hovered() {
            painter.rect_filled(
                row_rect.expand2(vec2(6.0, -1.0)),
                CornerRadius::same(radius::M),
                p.hover,
            );
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        painter.hline(row_rect.x_range(), y, Stroke::new(1.0, p.border));
        let cy = y + row_h / 2.0;
        let mut tip: Option<String> = None;
        for (i, cell) in row.iter().enumerate().take(cols.len()) {
            let (x0, w) = (xs[i], widths[i]);
            match cell {
                Cell::Badges(b) => {
                    let mut left = x0;
                    for (t, tone) in b {
                        let bw = kit::badge_width(&painter, t);
                        if left + bw > x0 + w + 0.5 {
                            break;
                        }
                        kit::paint_badge(&painter, &p, dark, left + bw, cy, t, *tone);
                        left += bw + 4.0;
                    }
                }
                Cell::Bar(f, s, color) => {
                    let g = painter.layout_no_wrap(s.clone(), font.clone(), p.text_2);
                    let track = Rect::from_min_max(
                        pos2(x0, cy - 3.0),
                        pos2((x0 + w - g.size().x - 10.0).max(x0 + 10.0), cy + 3.0),
                    );
                    painter.rect_filled(track, CornerRadius::same(3), p.raised);
                    let fw =
                        (track.width() * f.clamp(0.0, 1.0)).max(if *f > 0.0 { 3.0 } else { 0.0 });
                    painter.rect_filled(
                        Rect::from_min_size(track.min, vec2(fw, track.height())),
                        CornerRadius::same(3),
                        *color,
                    );
                    painter.galley(
                        pos2(x0 + w - g.size().x, cy - g.size().y / 2.0),
                        g,
                        p.text_2,
                    );
                }
                _ => {
                    let (s, f, color) = match cell {
                        Cell::Text(s) => (s, font.clone(), p.text),
                        Cell::Sec(s) => (s, font.clone(), p.text_2),
                        Cell::Muted(s) => (s, font.clone(), p.text_3),
                        Cell::Mono(s) => (s, mono_font.clone(), p.text),
                        Cell::MonoLink(s) => (s, mono_font.clone(), p.accent),
                        Cell::Tone(s, t) => (s, font.clone(), tone_color(&p, *t)),
                        _ => unreachable!("handled above"),
                    };
                    let g = truncated(&painter, s, f, color, w);
                    let gx = if cols[i].numeric {
                        x0 + w - g.size().x
                    } else {
                        x0
                    };
                    let cell_rect = Rect::from_min_size(pos2(x0, y), vec2(w, row_h));
                    if g.elided && pointer.is_some_and(|q| cell_rect.contains(q)) {
                        tip = Some(s.clone());
                    }
                    painter.galley(pos2(gx, cy - g.size().y / 2.0), g, color);
                }
            }
        }
        let resp = match tip {
            Some(t) => resp.on_hover_text_at_pointer(t),
            None => resp,
        };
        on_row(r, &resp);
    }
}

/// `kit::stats` split into rows that fit the available width. The kit's
/// wrapped row only wraps after a tile has already overflowed (its label
/// wraps into the leftover space and the tile's minimum width pushes past
/// the edge), which widens the whole page.
pub(crate) fn stats(ui: &mut Ui, items: &[kit::Stat]) {
    let avail = ui.available_width();
    let painter = ui.painter().clone();
    let width = |s: &kit::Stat| {
        let v = painter
            .layout_no_wrap(
                s.value.clone(),
                FontId::new(size::DISPLAY, theme::semibold()),
                Color32::WHITE,
            )
            .size()
            .x;
        let l = painter
            .layout_no_wrap(
                s.label.to_owned(),
                FontId::proportional(size::SMALL),
                Color32::WHITE,
            )
            .size()
            .x;
        v.max(l).max(108.0) + 30.0
    };
    let mut start = 0;
    let mut x = 0.0;
    for (i, s) in items.iter().enumerate() {
        let w = width(s);
        if i > start && x + w > avail {
            kit::stats(ui, &items[start..i]);
            ui.add_space(2.0);
            start = i;
            x = 0.0;
        }
        x += w + 10.0;
    }
    if start < items.len() {
        kit::stats(ui, &items[start..]);
    }
}

/// Colour of a tone, with the neutral tone as primary text.
pub(crate) fn tone_color(p: &theme::Palette, t: Tone) -> Color32 {
    match t {
        Tone::Neutral => p.text,
        t => t.color(p),
    }
}

/// Colour of a list dot for a tone (neutral dots are quiet).
pub(crate) fn dot_color(p: &theme::Palette, t: Tone) -> Color32 {
    match t {
        Tone::Neutral => p.text_3,
        t => t.color(p),
    }
}

/// A commit for [`commit_table`].
pub(crate) struct CommitLine<'a> {
    pub id: Oid,
    pub time: i64,
    pub author: &'a str,
    pub summary: &'a str,
}

/// Commits as a table: id (a link when the commit is in the graph),
/// summary, author and age, with `extra` columns: the first `lead` of them
/// (and of each row's extra cells) before the commit, the rest after it. A
/// click shows the commit in the graph; the context menu copies.
pub(crate) fn commit_table(
    ui: &mut Ui,
    cx: &Cx,
    id: &str,
    extra: &[Col],
    lead: usize,
    commits: Vec<(CommitLine, Vec<Cell>)>,
    actions: &mut Vec<Action>,
) {
    let lead = lead.min(extra.len());
    let mut cols: Vec<Col> = extra[..lead].to_vec();
    cols.extend([
        Col::text("Commit"),
        Col::text("Summary"),
        Col::text("Author"),
        Col::text("When"),
    ]);
    cols.extend_from_slice(&extra[lead..]);
    let mut keys: Vec<(Oid, &str)> = Vec::with_capacity(commits.len());
    let rows: Vec<Vec<Cell>> = commits
        .into_iter()
        .map(|(c, mut more)| {
            keys.push((c.id, c.summary));
            let short = repo::short_id(c.id);
            let after = more.split_off(lead.min(more.len()));
            let mut row = more;
            row.extend([
                if cx.in_graph(&c.id) {
                    Cell::MonoLink(short)
                } else {
                    Cell::Mono(short)
                },
                text(c.summary),
                sec(c.author),
                muted(format::relative(c.time, cx.now)),
            ]);
            row.extend(after);
            row
        })
        .collect();
    grid(ui, id, &cols, &rows, true, |r, resp| {
        let (oid, summary) = keys[r];
        if resp.clicked() && cx.in_graph(&oid) {
            actions.push(Action::JumpTo(oid));
        }
        commit_menu(resp, oid, summary, actions);
    });
}

pub(crate) fn commit_menu(resp: &Response, id: Oid, summary: &str, actions: &mut Vec<Action>) {
    resp.context_menu(|ui| {
        if ui.button("Copy commit id").clicked() {
            actions.push(Action::Copy(id.to_string()));
            ui.close();
        }
        if !summary.is_empty() && ui.button("Copy summary").clicked() {
            actions.push(Action::Copy(summary.to_owned()));
            ui.close();
        }
    });
}

/// A commit reference inside a key-value row: short id (accent when it can
/// be shown in the graph) and the summary.
pub(crate) fn commit_ref(ui: &mut Ui, cx: &Cx, id: Oid, summary: &str, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let shown = cx.in_graph(&id);
        let resp = ui.add(
            egui::Label::new(kit::mono(repo::short_id(id)).color(if shown {
                p.accent
            } else {
                p.text_2
            }))
            .sense(Sense::click()),
        );
        let resp = if shown {
            resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        } else {
            resp
        };
        if resp.clicked() && shown {
            actions.push(Action::JumpTo(id));
        }
        commit_menu(&resp, id, summary, actions);
        if !summary.is_empty() {
            ui.add(egui::Label::new(RichText::new(summary).color(p.text)).wrap());
        }
    });
}

/// A path with a copy / open / blame context menu (no tooltip).
pub(crate) fn path_label(ui: &mut Ui, cx: &Cx, path: &str, actions: &mut Vec<Action>) -> Response {
    let c = pal(ui).text;
    let resp = ui.add(
        egui::Label::new(kit::mono(path).color(c))
            .sense(Sense::click())
            .wrap(),
    );
    path_menu(&resp, cx, path, actions);
    resp
}

pub(crate) fn path_menu(resp: &Response, cx: &Cx, path: &str, actions: &mut Vec<Action>) {
    resp.context_menu(|ui| {
        if ui.button("Copy path").clicked() {
            actions.push(Action::Copy(path.to_owned()));
            ui.close();
        }
        if ui.button("Open file").clicked() {
            actions.push(Action::OpenPath(cx.workdir.join(path)));
            ui.close();
        }
        if ui.button("Blame (Owners)").clicked() {
            actions.push(Action::SetInsightsTab(InsightsTab::Ownership));
            actions.push(Action::SelectInsight(Some(format!("file:{path}"))));
            actions.push(Action::BlameFile(path.to_owned()));
            ui.close();
        }
    });
}

/// Paths one per line, at most `max`.
pub(crate) fn path_lines(
    ui: &mut Ui,
    cx: &Cx,
    paths: &[String],
    max: usize,
    actions: &mut Vec<Action>,
) {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        for p in paths.iter().take(max) {
            path_label(ui, cx, p, actions);
        }
    });
    more(ui, paths.len(), max);
}

/// "and N more" under a capped list.
pub(crate) fn more(ui: &mut Ui, total: usize, shown: usize) {
    if total > shown {
        kit::muted(ui, &format!("and {} more", format::count(total - shown)));
    }
}

/// A titled group inside a section: semibold title and a quiet count.
pub(crate) fn group(ui: &mut Ui, title: &str, count: Option<usize>) {
    let p = *pal(ui);
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        kit::subtitle(ui, title);
        if let Some(n) = count {
            ui.label(
                RichText::new(format::count(n))
                    .size(size::SMALL)
                    .color(p.text_3),
            );
        }
    });
}

/// Title of a detail pane with badges under it.
pub(crate) fn detail_title(ui: &mut Ui, title: &str, badge_list: &[(String, Tone)]) {
    kit::title(ui, title);
    if !badge_list.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            for (t, tone) in badge_list {
                kit::badge(ui, t, *tone);
            }
        });
    }
}

/// A row of buttons.
pub(crate) fn button_row(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        add(ui)
    });
}

/// "Why it matters" note.
pub(crate) fn why(ui: &mut Ui, text: &str) {
    kit::callout(ui, Tone::Accent, "Why it matters", text);
}

/// The list pane's header: title, count, a way back to the overview while
/// an item is selected, and the filter box.
pub(crate) fn list_top(
    ui: &mut Ui,
    title: &str,
    count: usize,
    selected: bool,
    filter: Option<(&str, &mut String, &str)>,
    actions: &mut Vec<Action>,
) {
    kit::list_header(ui, title, Some(count), |ui| {
        if selected {
            ui.add_space(4.0);
            if kit::link(ui, "Overview").clicked() {
                actions.push(Action::SelectInsight(None));
            }
        }
    });
    ui.add_space(6.0);
    if let Some((id, text, hint)) = filter {
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.set_width(ui.available_width() - 4.0);
            kit::search_box(ui, id, text, hint);
        });
        ui.add_space(6.0);
    }
}

/// A "nothing here" note in a list pane.
pub(crate) fn list_none(ui: &mut Ui, text: &str) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        kit::muted(ui, text);
    });
}

pub(crate) fn severity_tone(s: Severity) -> Tone {
    match s {
        Severity::Critical | Severity::High => Tone::Danger,
        Severity::Medium => Tone::Warning,
        Severity::Low => Tone::Neutral,
    }
}

pub(crate) fn sev_badge(s: Severity) -> (String, Tone) {
    (s.label().to_owned(), severity_tone(s))
}

/// Age of something in days: red past two years, amber past one.
pub(crate) fn age_tone(days: u32) -> Tone {
    if days > 730 {
        Tone::Danger
    } else if days > 365 {
        Tone::Warning
    } else {
        Tone::Neutral
    }
}

/// Bus factor 1 is worth a warning; anything else is information.
pub(crate) fn bus_tone(bus: u32) -> Tone {
    if bus == 1 {
        Tone::Warning
    } else {
        Tone::Neutral
    }
}

/// "1 commit", "12,345 commits".
pub(crate) fn count_of(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", format::count(n), if n == 1 { one } else { many })
}

pub(crate) fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        "–".into()
    } else {
        format!("{:.0}%", part as f64 * 100.0 / whole as f64)
    }
}

pub(crate) fn days_text(days: u32) -> String {
    if days < 1 {
        "today".into()
    } else if days < 60 {
        format!("{days} d")
    } else if days < 730 {
        format!("{:.1} mo", days as f32 / 30.4)
    } else {
        format!("{:.1} y", days as f32 / 365.25)
    }
}

/// "2024-03-12 10:04  ·  3.4 mo ago" (or "3d ago" when recent).
pub(crate) fn when(t: i64, now: i64) -> String {
    let age = if now - t < 14 * 86_400 {
        format::relative(t, now)
    } else {
        format!(
            "{} ago",
            days_text(gitvis_core::insights::days_between(t, now))
        )
    };
    format!("{}  ·  {age}", format::absolute(t))
}

pub(crate) fn yes_no(b: bool) -> &'static str {
    if b { "Yes" } else { "No" }
}
