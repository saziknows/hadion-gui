//! Graph filters: author (picked from the loaded commits' authors or
//! typed), path, date range and message text. Matching rows are drawn
//! normally and the rest dimmed, with a count and next / previous match.
//! Author and dates match as you type; path and message run one `git log`
//! on Enter. The filter persists per repository.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use egui::{Align, Frame, Layout, Margin, RichText, Ui, vec2};
use gitvis_core::devops::history::filter::{self as core, QuerySet, Spec};

use super::widgets;
use super::{Act, Field, Msg};
use crate::devops::ctx::Ctx;
use crate::format;
use crate::state::{Action, RepoState};
use crate::theme::{pal, size};
use crate::views::form::{self, InputOpts};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Tone};

#[derive(Default)]
pub struct Filter {
    /// The bar is shown.
    pub open: bool,
    pub spec: Spec,
    /// The (path, message) the last git query ran for.
    pub queried: Option<(String, String)>,
    pub query: Option<Result<QuerySet, String>>,
    pub querying: bool,
    pub cancel: Arc<AtomicBool>,
    pub seq: u64,
    /// Per commit index: matches (empty when no filter is active).
    pub mask: Vec<bool>,
    /// Rows of the matching commits.
    pub rows: Vec<usize>,
    pub total: usize,
    pub since_err: Option<String>,
    pub until_err: Option<String>,
    /// Authors of the loaded commits: label, value, commits.
    pub authors: Vec<String>,
    /// The ref tips the query ran against.
    refs_sig: u64,
}

impl Filter {
    /// Dimming applies.
    pub fn active(&self) -> bool {
        !self.mask.is_empty()
    }

    /// Does commit `index` match (true without a filter)?
    pub fn matches(&self, index: usize) -> bool {
        self.mask.get(index).copied().unwrap_or(true)
    }

    /// The path / message typed differ from what the git query ran for.
    pub fn git_pending(&self) -> bool {
        self.spec.needs_git() && self.queried.as_ref() != Some(&self.spec.git_key())
    }
}

fn utc_offset() -> i64 {
    chrono::Local::now().offset().local_minus_utc() as i64
}

fn refs_signature(repo: &RepoState) -> u64 {
    let mut h = DefaultHasher::new();
    if let Some(s) = &repo.snapshot {
        s.head.target().hash(&mut h);
        for b in &s.branches {
            b.target.hash(&mut h);
        }
        for r in &s.remotes {
            for b in &r.branches {
                b.target.hash(&mut h);
            }
        }
        for t in &s.tags {
            t.target.hash(&mut h);
        }
        s.stashes.len().hash(&mut h);
    }
    h.finish()
}

/// Recompute the mask from the spec, the loaded commits and the query.
pub fn recompute(repo: &mut RepoState) {
    let Some(snap) = repo.snapshot.as_ref() else {
        return;
    };
    let f = &mut repo.devops.history.filter;
    let (local, since_err, until_err) = core::compile(&f.spec, utc_offset());
    f.since_err = since_err;
    f.until_err = until_err;
    f.total = snap.commits.len();
    let set = match (&f.query, &f.queried) {
        (Some(Ok(q)), Some(key)) if f.spec.needs_git() && *key == f.spec.git_key() => Some(&q.ids),
        _ => None,
    };
    if local.is_empty() && set.is_none() {
        f.mask.clear();
        f.rows.clear();
        return;
    }
    f.mask = core::mask(&snap.commits, &local, set);
    f.rows = core::rows(&f.mask, snap.wip_row);
}

fn query(cx: &mut Ctx, repo: &mut RepoState) {
    let refs_sig = refs_signature(repo);
    let f = &mut repo.devops.history.filter;
    f.cancel.store(true, Ordering::Relaxed);
    if !f.spec.needs_git() {
        f.querying = false;
        f.query = None;
        f.queried = None;
        return;
    }
    f.cancel = Arc::new(AtomicBool::new(false));
    f.seq += 1;
    f.querying = true;
    f.queried = Some(f.spec.git_key());
    let seq = f.seq;
    let spec = f.spec.clone();
    let cancel = f.cancel.clone();
    let label = format!("Commits matching {}", spec.describe());
    f.refs_sig = refs_sig;
    cx.read(label, move |dir, log| {
        let mut cmds = Vec::new();
        let r = core::query(dir, &spec, core::MAX_IDS, &cancel, &mut cmds);
        log.commands.extend(cmds);
        Msg::FilterQueried(seq, r)
    });
}

fn authors(repo: &mut RepoState) {
    let Some(snap) = repo.snapshot.as_ref() else {
        return;
    };
    repo.devops.history.filter.authors = core::authors(&snap.commits)
        .into_iter()
        .take(400)
        .map(|(n, _, _)| n)
        .fold(Vec::new(), |mut v, n| {
            if !v.contains(&n) {
                v.push(n);
            }
            v
        });
}

pub fn on_snapshot(cx: &mut Ctx, repo: &mut RepoState) {
    // The author suggestions are only needed while the bar is open.
    if repo.devops.history.filter.open {
        authors(repo);
    } else {
        repo.devops.history.filter.authors.clear();
    }
    let f = &repo.devops.history.filter;
    let rerun = f.spec.needs_git()
        && !f.querying
        && (f.queried.is_none() || f.refs_sig != refs_signature(repo));
    if rerun && !repo.devops.history.filter.git_pending_typed() {
        query(cx, repo);
    }
    recompute(repo);
}

impl Filter {
    /// Typed but not applied: wait for Enter instead of querying on reload.
    fn git_pending_typed(&self) -> bool {
        self.queried.is_some() && self.git_pending()
    }
}

fn jump(cx: &mut Ctx, repo: &RepoState, row: usize) {
    let Some(snap) = repo.snapshot.as_ref() else {
        return;
    };
    if let Some(i) = snap.commit_of_row(row)
        && let Some(c) = snap.commits.get(i)
    {
        cx.action(Action::JumpTo(c.id));
    }
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::ToggleFilterBar => {
            let f = &mut repo.devops.history.filter;
            f.open = !f.open;
            if f.open {
                authors(repo);
            }
        }
        Act::FilterEdited => recompute(repo),
        Act::FilterApply => {
            if repo.devops.history.filter.git_pending()
                || !repo.devops.history.filter.spec.needs_git()
            {
                query(cx, repo);
            }
            recompute(repo);
        }
        Act::FilterClear => {
            let f = &mut repo.devops.history.filter;
            f.cancel.store(true, Ordering::Relaxed);
            f.spec = Spec::default();
            f.query = None;
            f.queried = None;
            f.querying = false;
            f.seq += 1;
            recompute(repo);
        }
        Act::FilterSet(field, value) => {
            let f = &mut repo.devops.history.filter;
            f.open = true;
            let slot = match field {
                Field::Author => &mut f.spec.author,
                Field::Path => &mut f.spec.path,
                Field::Since => &mut f.spec.since,
                Field::Until => &mut f.spec.until,
                Field::Message => &mut f.spec.message,
            };
            *slot = value;
            if matches!(field, Field::Path | Field::Message) {
                query(cx, repo);
            }
            recompute(repo);
            if repo.devops.history.filter.authors.is_empty() {
                authors(repo);
            }
            if repo.page != crate::state::Page::Graph {
                cx.action(Action::ShowPage(crate::state::Page::Graph));
            }
        }
        Act::FilterStep(forward) => {
            let from = repo.selected_row();
            if let Some(r) = core::step(&repo.devops.history.filter.rows, from, forward) {
                jump(cx, repo, r);
            }
        }
        _ => {}
    }
}

pub fn on_msg(_cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    let f = &mut repo.devops.history.filter;
    if let Msg::FilterQueried(seq, r) = msg {
        if seq != f.seq {
            return;
        }
        f.querying = false;
        if matches!(&r, Err(e) if e == "cancelled") {
            return;
        }
        f.query = Some(r);
        recompute(repo);
    }
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

/// The Filter button next to the commit search (toolbar, History page).
pub fn toolbar_button(ui: &mut Ui, s: &super::State, actions: &mut Vec<Action>) {
    let f = &s.filter;
    let n = f.spec.count();
    let label = if n > 0 {
        format!("Filter  {n}")
    } else {
        "Filter".to_owned()
    };
    let resp = IconButton::new(Icon::List, &label)
        .compact()
        .selected(f.open || n > 0)
        .show(ui)
        .on_hover_text("Filter the graph by author, path, date or message: matching commits stay bright, the rest are dimmed");
    if resp.clicked() {
        actions.push(Act::ToggleFilterBar.into());
    }
}

/// The filter bar above the graph (drawn while open or active).
pub fn filter_bar(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) {
    let f = &mut repo.devops.history.filter;
    if !f.open && !f.spec.is_active() {
        return;
    }
    let p = *pal(ui);
    Frame::NONE
        .fill(p.panel)
        .inner_margin(Margin {
            left: 12,
            right: 10,
            top: 8,
            bottom: 8,
        })
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            let mut edited = false;
            let mut apply = false;
            ui.spacing_mut().interact_size.y = form::INPUT_H;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                let label = |ui: &mut Ui, t: &str| {
                    ui.label(RichText::new(t).size(size::SMALL).color(p.text_2));
                };
                // A full-height strut first, so the first label centres on the
                // inputs like the others.
                ui.allocate_exact_size(vec2(0.0, form::INPUT_H), egui::Sense::hover());
                label(ui, "Author");
                let (r, picked) = widgets::suggest_input(
                    ui,
                    "flt_author",
                    &mut f.spec.author,
                    "name or e-mail",
                    &f.authors,
                    150.0,
                    false,
                    false,
                );
                edited |= r.changed() || picked;
                label(ui, "Path");
                let r = form::input(
                    ui,
                    ("hist_flt", "path"),
                    &mut f.spec.path,
                    InputOpts::new("src/ or a file").width(170.0).mono(),
                );
                apply |= widgets::submitted(ui, &r);
                label(ui, "From");
                let r = form::input(
                    ui,
                    ("hist_flt", "since"),
                    &mut f.spec.since,
                    InputOpts::new("YYYY-MM-DD")
                        .width(96.0)
                        .invalid(f.since_err.is_some()),
                );
                edited |= r.changed();
                label(ui, "to");
                let r = form::input(
                    ui,
                    ("hist_flt", "until"),
                    &mut f.spec.until,
                    InputOpts::new("YYYY-MM-DD")
                        .width(96.0)
                        .invalid(f.until_err.is_some()),
                );
                edited |= r.changed();
                label(ui, "Message");
                let r = form::input(
                    ui,
                    ("hist_flt", "message"),
                    &mut f.spec.message,
                    InputOpts::new("text in the message").width(150.0),
                );
                apply |= widgets::submitted(ui, &r);
                if f.git_pending() && form::primary(ui, "Search", !f.querying).clicked() {
                    apply = true;
                }
            });
            if edited {
                actions.push(Act::FilterEdited.into());
            }
            if apply {
                actions.push(Act::FilterApply.into());
            }
            ui.add_space(2.0);
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), 26.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if f.querying {
                        ui.add(egui::Spinner::new().size(13.0).color(p.accent));
                        ui.label(
                            RichText::new(format!(
                                "Searching the history: {}",
                                core::log_args(&f.spec).join(" ")
                            ))
                            .size(size::SMALL)
                            .color(p.text_2),
                        );
                    } else if let Some(e) = f.since_err.as_ref().or(f.until_err.as_ref()) {
                        ui.label(RichText::new(e).size(size::SMALL).color(p.danger));
                    } else if let Some(Err(e)) = &f.query {
                        ui.label(
                            RichText::new(format!("The path / message search failed: {e}"))
                                .size(size::SMALL)
                                .color(p.danger),
                        );
                    } else if f.active() {
                        let n = f.rows.len();
                        ui.label(
                            RichText::new(format!(
                                "{} of {} commits match",
                                format::count(n),
                                format::count(f.total)
                            ))
                            .size(size::SMALL)
                            .family(crate::theme::medium())
                            .color(if n == 0 {
                                p.warning
                            } else {
                                p.text
                            }),
                        );
                        if let Some(Ok(q)) = &f.query
                            && q.truncated
                        {
                            kit::badge(ui, "Search capped", Tone::Warning);
                        }
                    } else if f.git_pending() {
                        ui.label(
                            RichText::new("Press Enter in Path or Message to search the history.")
                                .size(size::SMALL)
                                .color(p.text_2),
                        );
                    } else {
                        ui.label(
                            RichText::new("Matching commits stay bright; the others are dimmed.")
                                .size(size::SMALL)
                                .color(p.text_2),
                        );
                    }
                    if f.git_pending() && f.active() {
                        ui.label(
                            RichText::new("· press Enter to apply the path / message")
                                .size(size::SMALL)
                                .color(p.text_3),
                        );
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        if IconButton::icon(Icon::Close)
                            .compact()
                            .show(ui)
                            .on_hover_text("Hide the filter bar (the filter stays)")
                            .clicked()
                        {
                            actions.push(Act::ToggleFilterBar.into());
                        }
                        if f.spec.is_active()
                            && IconButton::new(Icon::Trash, "Clear")
                                .compact()
                                .show(ui)
                                .clicked()
                        {
                            actions.push(Act::FilterClear.into());
                        }
                        let has = !f.rows.is_empty();
                        if IconButton::icon(Icon::ChevronDown)
                            .compact()
                            .enabled(has)
                            .show(ui)
                            .on_hover_text("Next match")
                            .clicked()
                        {
                            actions.push(Act::FilterStep(true).into());
                        }
                        if IconButton::icon(Icon::ChevronUp)
                            .compact()
                            .enabled(has)
                            .show(ui)
                            .on_hover_text("Previous match")
                            .clicked()
                        {
                            actions.push(Act::FilterStep(false).into());
                        }
                    });
                },
            );
        });
    kit::hairline(ui);
}
