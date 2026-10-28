//! The Compare tab: two revision inputs with ref suggestions, a swap
//! button, two-dot or three-dot mode, then the merge base, the counts, the
//! commits only on each side and the changed files with their diff.

use std::collections::HashMap;

use egui::{Frame, Margin, RichText, Ui, vec2};
use gitvis_core::Oid;
use gitvis_core::devops::history::compare::{self as core, Comparison, Mode as RangeMode};
use gitvis_core::devops::history::numstat;

use super::widgets::{self, DiffShow};
use super::{Act, Msg};
use crate::devops::ctx::Ctx;
use crate::format;
use crate::state::{Action, RepoState, Settings};
use crate::theme::{pal, size};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Row, Stat, Tone};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum Tab {
    #[default]
    Files,
    Commits,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    A,
    B,
}

#[derive(Default)]
pub struct Compare {
    pub a: String,
    pub b: String,
    /// B is the working tree.
    pub worktree: bool,
    pub mode: RangeMode,
    pub seq: u64,
    pub loading: bool,
    pub result: Option<Result<Comparison, String>>,
    pub tab: Tab,
    pub selected: Option<usize>,
    pub commit: Option<(Side, usize)>,
    /// Diffs by path, for the current result.
    pub diffs: HashMap<String, Option<Result<DiffShow, String>>>,
    /// Select this path once the result arrives.
    pub select_after: Option<String>,
    pub file_filter: String,
    /// Give the B input the focus on the next frame.
    pub focus_b: bool,
}

impl Compare {
    fn request(&self) -> core::Request {
        core::Request {
            a: self.a.trim().to_owned(),
            b: (!self.worktree).then(|| self.b.trim().to_owned()),
            mode: self.mode,
        }
    }
}

fn run(cx: &mut Ctx, repo: &mut RepoState) {
    let c = &mut repo.devops.history.compare;
    let req = c.request();
    if req.a.is_empty() || req.b.as_deref().is_some_and(str::is_empty) {
        c.result = Some(Err(
            "Enter both revisions: a branch, a tag, a commit SHA or an expression like HEAD~3."
                .into(),
        ));
        return;
    }
    c.seq += 1;
    c.loading = true;
    c.selected = None;
    c.commit = None;
    c.diffs.clear();
    let seq = c.seq;
    cx.read(format!("Compare {}", req.title()), move |dir, log| {
        let mut cmds = Vec::new();
        let r = core::run(dir, &req, &mut cmds);
        log.commands.extend(cmds);
        Msg::Compared(seq, Box::new(r))
    });
}

fn select_file(cx: &mut Ctx, repo: &mut RepoState, i: usize) {
    let c = &mut repo.devops.history.compare;
    let Some(Ok(cmp)) = &c.result else { return };
    let Some(f) = cmp.files.get(i).cloned() else {
        return;
    };
    c.selected = Some(i);
    c.tab = Tab::Files;
    if c.diffs.contains_key(&f.path) {
        return;
    }
    c.diffs.insert(f.path.clone(), None);
    let cmp = cmp.clone();
    let seq = c.seq;
    let dark = cx.dark;
    cx.read(
        format!("Diff of {} ({})", f.path, cmp.request.title()),
        move |dir, log| {
            let mut cmds = Vec::new();
            let r =
                core::file_diff(dir, &cmp, &f, &mut cmds).map(|d| DiffShow::new(d, &f.path, dark));
            log.commands.extend(cmds);
            Msg::CompareDiff(seq, f.path.clone(), Box::new(r))
        },
    );
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::Compare { a, b, select } => {
            let c = &mut repo.devops.history.compare;
            c.a = a;
            c.worktree = b.is_none();
            if let Some(b) = b {
                c.b = b;
            }
            c.select_after = select;
            repo.devops.history.mode = super::Mode::Compare;
            super::show_page(cx, repo);
            run(cx, repo);
        }
        Act::CompareFrom(a) => {
            let c = &mut repo.devops.history.compare;
            c.a = a;
            c.worktree = false;
            c.focus_b = true;
            c.result = None;
            repo.devops.history.mode = super::Mode::Compare;
            super::show_page(cx, repo);
        }
        Act::RunCompare => run(cx, repo),
        Act::SwapCompare => {
            let c = &mut repo.devops.history.compare;
            if c.worktree {
                return;
            }
            std::mem::swap(&mut c.a, &mut c.b);
            if c.result.is_some() {
                run(cx, repo);
            }
        }
        Act::SetCompareMode(m) => {
            let c = &mut repo.devops.history.compare;
            c.mode = m;
            if c.result.is_some() {
                run(cx, repo);
            }
        }
        Act::SetCompareTab(t) => repo.devops.history.compare.tab = t,
        Act::SelectCompareFile(i) => select_file(cx, repo, i),
        Act::SelectCompareCommit(side, i) => {
            let c = &mut repo.devops.history.compare;
            c.tab = Tab::Commits;
            c.commit = Some((side, i));
        }
        _ => {}
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    let c = &mut repo.devops.history.compare;
    match msg {
        Msg::Compared(seq, r) if seq == c.seq => {
            let r = *r;
            c.loading = false;
            let first = match &r {
                Ok(cmp) => match c.select_after.take() {
                    Some(p) => cmp
                        .files
                        .iter()
                        .position(|f| f.path == p || f.old_path.as_deref() == Some(p.as_str()))
                        .or(if cmp.files.is_empty() { None } else { Some(0) }),
                    None => (!cmp.files.is_empty()).then_some(0),
                },
                Err(_) => None,
            };
            c.result = Some(r);
            if let Some(i) = first {
                select_file(cx, repo, i);
            }
        }
        Msg::CompareDiff(seq, path, r) if seq == c.seq => {
            c.diffs.insert(path, Some(*r));
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

pub fn show(ui: &mut Ui, repo: &mut RepoState, _settings: &Settings, actions: &mut Vec<Action>) {
    let suggestions = super::ref_suggestions(repo);
    let in_graph: std::collections::HashSet<String> = match &repo.snapshot {
        Some(s) => match &repo.devops.history.compare.result {
            Some(Ok(cmp)) => cmp
                .only_a
                .iter()
                .chain(&cmp.only_b)
                .map(|c| c.sha.clone())
                .chain([cmp.a.sha.clone(), cmp.b_sha().to_owned()])
                .chain(cmp.merge_base.iter().map(|m| m.sha.clone()))
                .filter(|sha| Oid::from_str(sha).is_ok_and(|o| s.index_of.contains_key(&o)))
                .collect(),
            _ => Default::default(),
        },
        None => Default::default(),
    };
    let snap_suggest = quick_pairs(repo);
    let c = &mut repo.devops.history.compare;
    // A short window: numbers on one line instead of tiles, and the
    // explanations and the A / B details behind a "Details" toggle, so the
    // file list and the diff keep their room.
    let compact = ui.available_height() < 600.0;
    let details_id = ui.make_persistent_id("hist_cmp_details_open");
    let details_open = ui.data(|d| d.get_temp::<bool>(details_id)).unwrap_or(false);
    let show_details = !compact || details_open;
    revision_bar(ui, c, &suggestions, show_details, actions);
    let p = *pal(ui);
    let empty = |ui: &mut Ui, add: &mut dyn FnMut(&mut Ui)| {
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(p.canvas).inner_margin(Margin::same(24)))
            .show(ui, |ui| add(ui));
    };
    if c.loading {
        let title = c.request().title();
        empty(ui, &mut |ui| {
            kit::loading(ui, &format!("Comparing {title}…"))
        });
        return;
    }
    let cmp = match &c.result {
        None => {
            empty(ui, &mut |ui| {
                kit::empty_state(
                    ui,
                    Icon::Merge,
                    "Pick two revisions",
                    "Type a branch, tag, commit SHA or an expression like HEAD~3 in A and B, or tick Working tree for B. Right-click a commit in the graph for Compare with HEAD, with the working tree, or with anything else.",
                    None,
                );
                if !snap_suggest.is_empty() {
                    ui.add_space(12.0);
                    ui.vertical_centered(|ui| {
                        ui.set_max_width(520.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                            for (label, a, b) in &snap_suggest {
                                if kit::chip(ui, label, false).clicked() {
                                    actions.push(
                                        Act::Compare {
                                            a: a.clone(),
                                            b: b.clone(),
                                            select: None,
                                        }
                                        .into(),
                                    );
                                }
                            }
                        });
                    });
                }
            });
            return;
        }
        Some(Err(e)) => {
            let e = e.clone();
            empty(ui, &mut |ui| {
                kit::callout_with(ui, Tone::Danger, "Could not compare", &e, |ui| {
                    ui.add_space(4.0);
                    if IconButton::new(Icon::Rerun, "Try again")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Act::RunCompare.into());
                    }
                });
            });
            return;
        }
        Some(Ok(cmp)) => cmp.clone(),
    };
    if let Some(open) = summary(ui, &cmp, &in_graph, compact, details_open, actions) {
        ui.data_mut(|d| d.insert_temp(details_id, open));
    }
    let tab = c.tab;
    let selected = c.selected;
    let commit_sel = c.commit;
    let (list_min, list_max) = widgets::split_range(ui.available_width(), 280.0, 760.0, 380.0);
    egui::Panel::left("hist_cmp_list")
        .resizable(true)
        .default_size(420.0f32.clamp(list_min, list_max))
        .size_range(list_min..=list_max)
        .frame(Frame::NONE.fill(p.panel).inner_margin(Margin {
            left: 8,
            right: 8,
            top: 10,
            bottom: 6,
        }))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            let files_label = format!("Files  {}", format::count(cmp.files.len()));
            let commits_label = format!("Commits  {}", format::count(cmp.count_a + cmp.count_b));
            ui.horizontal(|ui| {
                if let Some(t) = kit::segmented(
                    ui,
                    &[
                        (Tab::Files, files_label.as_str()),
                        (Tab::Commits, commits_label.as_str()),
                    ],
                    tab,
                ) {
                    actions.push(Act::SetCompareTab(t).into());
                }
            });
            ui.add_space(8.0);
            match tab {
                Tab::Files => file_list(ui, c, &cmp, selected, actions),
                Tab::Commits => commit_lists(ui, &cmp, commit_sel, &in_graph, actions),
            }
        });
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas).inner_margin(Margin {
            left: 20,
            right: 20,
            top: 14,
            bottom: 14,
        }))
        .show(ui, |ui| match tab {
            Tab::Files => file_detail(ui, c, &cmp, actions),
            Tab::Commits => commit_detail(ui, &cmp, commit_sel, &in_graph, actions),
        });
}

/// One-click comparisons for the empty state: the current branch with its
/// upstream, with the default branch, and the last commit.
fn quick_pairs(repo: &RepoState) -> Vec<(String, String, Option<String>)> {
    let Some(snap) = repo.snapshot.as_ref() else {
        return Vec::new();
    };
    let mut v = Vec::new();
    if let Some(b) = snap.current_branch() {
        if let Some(u) = &b.upstream {
            v.push((
                format!("{} with {u}", b.name),
                u.clone(),
                Some(b.name.clone()),
            ));
        }
        for main in ["main", "master", "develop"] {
            if b.name != main && snap.branch(main).is_some() {
                v.push((
                    format!("What {} adds to {main}", b.name),
                    main.to_owned(),
                    Some(b.name.clone()),
                ));
                break;
            }
        }
    }
    if snap.head.target().is_some() {
        v.push((
            "The last commit".into(),
            "HEAD~1".into(),
            Some("HEAD".into()),
        ));
        v.push(("HEAD with the working tree".into(), "HEAD".into(), None));
    }
    v
}

fn revision_bar(
    ui: &mut Ui,
    c: &mut Compare,
    suggestions: &[String],
    explain: bool,
    actions: &mut Vec<Action>,
) {
    Frame::NONE
        .inner_margin(Margin {
            left: 24,
            right: 16,
            top: 10,
            bottom: 10,
        })
        .show(ui, |ui| {
            let mut submit = false;
            // The inputs give up width first (everything else in the row
            // takes about 500 points); a window narrower still wraps the
            // row instead of pushing the Compare button off the page.
            let input_w = ((ui.available_width() - 500.0) / 2.0).clamp(120.0, 230.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                widgets::bar_label(ui, "A");
                let (ra, picked_a) = widgets::rev_input(
                    ui,
                    "cmp_a",
                    &mut c.a,
                    "main, v1.2, a1b2c3d, HEAD~3",
                    suggestions,
                    input_w,
                    false,
                );
                submit |= widgets::submitted(ui, &ra) || picked_a;
                if IconButton::text("Swap")
                    .compact()
                    .enabled(!c.worktree)
                    .show(ui)
                    .on_hover_text("Swap A and B")
                    .clicked()
                {
                    actions.push(Act::SwapCompare.into());
                }
                widgets::bar_label(ui, "B");
                if c.worktree {
                    let mut wt = "Working tree".to_owned();
                    crate::views::form::input(
                        ui,
                        ("hist_rev", "cmp_b_wt"),
                        &mut wt,
                        crate::views::form::InputOpts::new("")
                            .width(input_w)
                            .enabled(false),
                    );
                } else {
                    let (rb, picked_b) = widgets::rev_input(
                        ui,
                        "cmp_b",
                        &mut c.b,
                        "devops, HEAD",
                        suggestions,
                        input_w,
                        false,
                    );
                    if c.focus_b {
                        rb.request_focus();
                        c.focus_b = false;
                    }
                    submit |= widgets::submitted(ui, &rb) || picked_b;
                }
                if crate::views::form::checkbox(ui, &mut c.worktree, "Working tree").changed()
                    && c.result.is_some()
                {
                    submit = true;
                }
                ui.add_space(6.0);
                if let Some(m) = kit::segmented(
                    ui,
                    &[(RangeMode::TwoDot, "A..B"), (RangeMode::ThreeDot, "A...B")],
                    c.mode,
                ) {
                    actions.push(Act::SetCompareMode(m).into());
                }
                ui.add_space(6.0);
                if crate::views::form::primary(ui, "Compare", !c.loading).clicked() {
                    submit = true;
                }
            });
            if submit {
                actions.push(Act::RunCompare.into());
            }
            // What the mode means (behind "Details" in a short window,
            // unless there is no result yet to show details for).
            if explain || c.result.is_none() {
                ui.add_space(4.0);
                kit::secondary(ui, c.mode.explain());
            }
        });
    kit::hairline(ui);
}

/// The numbers, the ahead / behind sentence and what A and B resolved to.
/// `compact`: numbers on one line, and the rest behind a "Details" toggle
/// (`details_open`); returns the toggle's new state when it was clicked.
fn summary(
    ui: &mut Ui,
    cmp: &Comparison,
    in_graph: &std::collections::HashSet<String>,
    compact: bool,
    details_open: bool,
    actions: &mut Vec<Action>,
) -> Option<bool> {
    let mut toggled = None;
    let p = *pal(ui);
    let (added, deleted) = numstat::totals(&cmp.files);
    let b_name = cmp
        .b
        .as_ref()
        .map(|b| b.input.clone())
        .unwrap_or_else(|| "the working tree".into());
    let base_tip = cmp
        .merge_base
        .as_ref()
        .map(|m| format!("{}  ·  {}", m.sha, m.subject))
        .unwrap_or_default();
    Frame::NONE
        .inner_margin(Margin {
            left: 24,
            right: 16,
            top: if compact { 8 } else { 12 },
            bottom: if compact { 8 } else { 12 },
        })
        .show(ui, |ui| {
            if compact {
                ui.spacing_mut().item_spacing.y = 4.0;
            }
            let only_a = format!("Only in {}", cmp.a.input);
            let only_b = format!("Only in {b_name}");
            let stats = [
                Stat::new(
                    "Merge base",
                    cmp.merge_base
                        .as_ref()
                        .map(|m| m.short().to_owned())
                        .unwrap_or_else(|| "none".into()),
                )
                .tip(&base_tip),
                Stat::new(&only_a, format::count(cmp.count_a)),
                Stat::new(&only_b, format::count(cmp.count_b)),
                Stat::new("Files changed", format::count(cmp.files.len())),
                Stat::new(
                    "Lines",
                    format!(
                        "+{} \u{2212}{}",
                        format::count(added as usize),
                        format::count(deleted as usize)
                    ),
                ),
                Stat::new("Time", format::millis(cmp.elapsed_ms)),
            ];
            if compact {
                widgets::compact_stats(ui, &stats);
            } else {
                crate::views::insights::stats(ui, &stats);
                ui.add_space(6.0);
            }
            let ahead = if cmp.count_b == 0 && cmp.count_a == 0 {
                format!("{} and {b_name} point at the same history.", cmp.a.input)
            } else {
                format!(
                    "{b_name} is {} ahead of {} and {} behind it.",
                    format::plural(cmp.count_b, "commit", "commits"),
                    cmp.a.input,
                    format::count(cmp.count_a)
                )
            };
            if compact {
                // The sentence, then the toggle for the rest.
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.label(RichText::new(&ahead).size(size::BODY).color(p.text));
                    let label = if details_open {
                        "Hide details"
                    } else {
                        "Details"
                    };
                    if kit::link(ui, label).clicked() {
                        toggled = Some(!details_open);
                    }
                });
                if !details_open {
                    return;
                }
                kit::secondary(ui, cmp.request.mode.explain());
            } else {
                kit::body(ui, &ahead);
            }
            side_line(ui, "A", &cmp.a, in_graph, actions);
            match &cmp.b {
                Some(b) => {
                    side_line(ui, "B", b, in_graph, actions);
                    ui.horizontal(|ui| {
                        if kit::link(ui, "Compare A with the working tree").clicked() {
                            actions.push(
                                Act::Compare {
                                    a: cmp.a.input.clone(),
                                    b: None,
                                    select: None,
                                }
                                .into(),
                            );
                        }
                    });
                }
                None => {
                    ui.label(
                        RichText::new(
                            "B is the working tree: tracked files as they are on disk (untracked files are not compared).",
                        )
                        .size(size::SMALL)
                        .color(p.text_2),
                    );
                }
            }
            if cmp.truncated() {
                kit::muted(
                    ui,
                    &format!(
                        "The commit lists show the newest {} of each side.",
                        format::count(core::MAX_COMMITS)
                    ),
                );
            }
        });
    kit::hairline(ui);
    toggled
}

/// `A = main  a1b2c3d4e5  Subject…  Open the commit`.
fn side_line(
    ui: &mut Ui,
    label: &str,
    r: &core::Resolved,
    in_graph: &std::collections::HashSet<String>,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(
            RichText::new(format!("{label} = {}", r.input))
                .size(size::SMALL)
                .family(crate::theme::medium())
                .color(p.text),
        );
        ui.label(kit::mono(r.short()).color(p.text_2));
        let link = in_graph.contains(&r.sha) && Oid::from_str(&r.sha).is_ok();
        let room = ui.available_width() - if link { 130.0 } else { 0.0 };
        ui.scope(|ui| {
            ui.set_max_width(room.max(60.0));
            ui.add(
                egui::Label::new(RichText::new(&r.subject).size(size::SMALL).color(p.text_2))
                    .truncate(),
            );
        });
        if link
            && let Ok(oid) = Oid::from_str(&r.sha)
            && kit::link(ui, "Open the commit").clicked()
        {
            actions.push(Action::JumpTo(oid));
        }
    });
}

fn file_list(
    ui: &mut Ui,
    c: &mut Compare,
    cmp: &Comparison,
    selected: Option<usize>,
    actions: &mut Vec<Action>,
) {
    if cmp.files.is_empty() {
        kit::muted(
            ui,
            "No file differs. The two sides have the same content (the commits may still differ, e.g. a revert, or merges).",
        );
        return;
    }
    if cmp.files.len() > 8 {
        widgets::search(ui, "hist_cmp_filter", &mut c.file_filter, "Filter files");
        ui.add_space(6.0);
    }
    let needle = c.file_filter.trim().to_lowercase();
    let shown: Vec<usize> = (0..cmp.files.len())
        .filter(|&i| needle.is_empty() || cmp.files[i].path.to_lowercase().contains(&needle))
        .collect();
    if shown.is_empty() {
        kit::muted(ui, "No changed file matches the filter.");
        return;
    }
    let two_lines = cmp.files.iter().any(|f| file_note(f).is_some());
    widgets::rows_ui(ui, |ui| {
        kit::list(
            ui,
            "hist_cmp_files",
            shown.len(),
            if two_lines { 46.0 } else { 32.0 },
            |k| selected == Some(shown[k]),
            |ui, rect, resp, k| {
                let f = &cmp.files[shown[k]];
                let sub = file_note(f);
                widgets::paint_file_row(
                    ui,
                    rect,
                    widgets::change_kind(f.change),
                    &f.path,
                    sub.as_deref(),
                    f.added,
                    f.deleted,
                    f.binary,
                    selected == Some(shown[k]),
                );
                if resp.clicked() {
                    actions.push(Act::SelectCompareFile(shown[k]).into());
                }
            },
        )
    });
}

/// The second line of a file row: renames, copies, mode changes.
fn file_note(f: &numstat::FileStat) -> Option<String> {
    match (&f.old_path, f.change) {
        (Some(old), numstat::Change::Renamed(s)) => {
            Some(format!("renamed from {old}  ·  {s}% similar"))
        }
        (Some(old), numstat::Change::Copied(s)) => {
            Some(format!("copied from {old}  ·  {s}% similar"))
        }
        _ if f.mode_changed() => Some(format!("mode {} → {}", f.old_mode, f.new_mode)),
        _ if f.submodule() => Some("submodule".to_owned()),
        _ => None,
    }
}

fn file_detail(ui: &mut Ui, c: &Compare, cmp: &Comparison, actions: &mut Vec<Action>) {
    let Some(f) = c.selected.and_then(|i| cmp.files.get(i)) else {
        if cmp.files.is_empty() {
            kit::empty_state(
                ui,
                Icon::Check,
                "Same content",
                "Both sides have the same files. See the Commits tab for what differs in history.",
                None,
            );
        } else {
            kit::empty_state(
                ui,
                Icon::List,
                "Pick a file",
                "Select a changed file for its diff.",
                None,
            );
        }
        return;
    };
    let p = *pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        kit::subtitle(ui, &f.path);
        kit::badge(
            ui,
            &f.change.label(),
            match f.change {
                numstat::Change::Added => Tone::Success,
                numstat::Change::Deleted => Tone::Danger,
                numstat::Change::Renamed(_) | numstat::Change::Copied(_) => Tone::Purple,
                _ => Tone::Neutral,
            },
        );
        if !f.binary {
            ui.label(
                RichText::new(format!("+{}", f.added))
                    .size(size::SMALL)
                    .color(p.success),
            );
            ui.label(
                RichText::new(format!("\u{2212}{}", f.deleted))
                    .size(size::SMALL)
                    .color(p.danger),
            );
        }
    });
    if let Some(old) = &f.old_path {
        kit::secondary(ui, &format!("Was {old}"));
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        let b_rev = cmp.b.as_ref().map(|b| b.sha.clone());
        if let Some(rev) = &b_rev
            && f.change != numstat::Change::Deleted
        {
            if IconButton::new(Icon::Clock, "Blame in B")
                .compact()
                .show(ui)
                .clicked()
            {
                actions.push(
                    Act::Blame {
                        rev: rev.clone(),
                        path: f.path.clone(),
                        line: None,
                    }
                    .into(),
                );
            }
            if IconButton::new(Icon::External, "Open B's version")
                .compact()
                .show(ui)
                .clicked()
            {
                actions.push(
                    Act::OpenVersion {
                        rev: rev.clone(),
                        path: f.path.clone(),
                    }
                    .into(),
                );
            }
        }
        if f.change != numstat::Change::Added
            && IconButton::new(Icon::External, "Open A's version")
                .compact()
                .show(ui)
                .clicked()
        {
            actions.push(
                Act::OpenVersion {
                    rev: cmp.a.sha.clone(),
                    path: f.old_path.clone().unwrap_or_else(|| f.path.clone()),
                }
                .into(),
            );
        }
        if IconButton::new(Icon::Clock, "File history")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::ShowPage(crate::state::Page::Workbench));
            actions.push(crate::devops::workbench::Act::FileHistory(f.path.clone()).into());
        }
        if IconButton::new(Icon::Copy, "Copy the diff command")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::Copy(gitvis_core::devops::history::git_line(
                &core::file_diff_args(
                    &cmp.a.sha,
                    cmp.b.as_ref().map(|b| b.sha.as_str()),
                    cmp.request.mode,
                    &f.path,
                    f.old_path.as_deref(),
                ),
            )));
        }
    });
    ui.add_space(6.0);
    match c.diffs.get(&f.path) {
        None | Some(None) => kit::loading(ui, "Loading the diff…"),
        Some(Some(Err(e))) => kit::callout(ui, Tone::Danger, "Could not load the diff", e),
        Some(Some(Ok(d))) => {
            if f.submodule() {
                kit::muted(ui, "A submodule: the diff shows the commit it points at.");
            }
            widgets::diff_view(ui, &f.path, d);
        }
    }
}

fn commit_lists(
    ui: &mut Ui,
    cmp: &Comparison,
    sel: Option<(Side, usize)>,
    in_graph: &std::collections::HashSet<String>,
    actions: &mut Vec<Action>,
) {
    let b_name = cmp
        .b
        .as_ref()
        .map(|b| b.input.clone())
        .unwrap_or_else(|| "the working tree (HEAD)".into());
    let total_h = ui.available_height();
    let lists = [
        (
            Side::B,
            format!("Only in {b_name}"),
            &cmp.only_b,
            cmp.count_b,
        ),
        (
            Side::A,
            format!("Only in {}", cmp.a.input),
            &cmp.only_a,
            cmp.count_a,
        ),
    ];
    let non_empty = lists.iter().filter(|l| !l.2.is_empty()).count().max(1) as f32;
    let empty_count = 2.0 - non_empty;
    for (side, title, list, count) in lists {
        kit::list_header(ui, &title, Some(count), |_| {});
        ui.add_space(2.0);
        if list.is_empty() {
            kit::muted(ui, "No commits.");
            ui.add_space(8.0);
            continue;
        }
        // Shared out of what the pane has; 64 is the least a list (a
        // scroll area) takes.
        let h = ((total_h - 70.0 - empty_count * 40.0) / non_empty).max(64.0);
        let now = format::now_secs();
        ui.allocate_ui(vec2(ui.available_width(), h), |ui| {
            // Rows edge to edge: see `widgets::rows_ui`.
            ui.spacing_mut().item_spacing.y = 0.0;
            kit::list(
                ui,
                &format!("hist_cmp_commits_{side:?}"),
                list.len(),
                kit::ROW_H,
                |i| sel == Some((side, i)),
                |ui, rect, resp, i| {
                    let c = &list[i];
                    let sub = format!("{}  ·  {}", c.short(), c.author);
                    let meta = format::relative(c.time, now);
                    let badges: Vec<(String, Tone)> = if c.parents.len() > 1 {
                        vec![("Merge".into(), Tone::Neutral)]
                    } else {
                        Vec::new()
                    };
                    kit::paint_row(
                        ui,
                        rect,
                        &Row::new(&c.subject)
                            .subtitle(&sub)
                            .meta(&meta)
                            .badges(&badges),
                        sel == Some((side, i)),
                    );
                    if resp.clicked() {
                        actions.push(Act::SelectCompareCommit(side, i).into());
                    }
                    if resp.double_clicked()
                        && in_graph.contains(&c.sha)
                        && let Ok(oid) = Oid::from_str(&c.sha)
                    {
                        actions.push(Action::JumpTo(oid));
                    }
                },
            );
        });
        ui.add_space(8.0);
    }
}

fn commit_detail(
    ui: &mut Ui,
    cmp: &Comparison,
    sel: Option<(Side, usize)>,
    in_graph: &std::collections::HashSet<String>,
    actions: &mut Vec<Action>,
) {
    let found = sel.and_then(|(side, i)| {
        let list = match side {
            Side::A => &cmp.only_a,
            Side::B => &cmp.only_b,
        };
        list.get(i).map(|c| (side, c))
    });
    let Some((side, c)) = found else {
        kit::empty_state(
            ui,
            Icon::Graph,
            "Commits on each side",
            "The list shows the commits reachable from one side but not the other. Select one for its details; double-click to open it in the graph.",
            None,
        );
        return;
    };
    let now = format::now_secs();
    crate::views::insights::detail_title(
        ui,
        &c.subject,
        &[(
            match side {
                Side::A => format!("Only in {}", cmp.a.input),
                Side::B => format!(
                    "Only in {}",
                    cmp.b.as_ref().map(|b| b.input.as_str()).unwrap_or("HEAD")
                ),
            },
            Tone::Accent,
        )],
    );
    ui.add_space(4.0);
    kit::kv(ui, 110.0, |kv| {
        kv.text("Commit", kit::mono(&c.sha));
        kv.text("Author", format!("{} <{}>", c.author, c.email));
        kv.text(
            "Date",
            format!(
                "{}  ·  {}",
                format::absolute(c.time),
                format::relative(c.time, now)
            ),
        );
        if c.parents.len() > 1 {
            kv.text(
                "Merge of",
                kit::mono(
                    c.parents
                        .iter()
                        .map(|p| gitvis_core::devops::history::short(p))
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
            );
        }
    });
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        if in_graph.contains(&c.sha)
            && let Ok(oid) = Oid::from_str(&c.sha)
            && IconButton::new(Icon::Graph, "Open the commit")
                .compact()
                .show(ui)
                .clicked()
        {
            actions.push(Action::JumpTo(oid));
        }
        if IconButton::new(Icon::Folder, "Browse files")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(
                Act::Browse {
                    rev: c.sha.clone(),
                    path: None,
                }
                .into(),
            );
        }
        if IconButton::new(Icon::Copy, "Copy SHA")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::Copy(c.sha.clone()));
        }
    });
    if !in_graph.contains(&c.sha) {
        kit::muted(
            ui,
            "This commit is not in the graph (it is hidden by a branch filter, or older than the loaded history).",
        );
    }
}
