//! A pull request's Files tab: the changed files on the left, the selected
//! file's patch on the right with old and new line numbers and syntax
//! colours, review threads inline under their lines (resolved and outdated
//! ones collapsed), pending comments, a composer for the selected line or
//! range (click, shift-click or drag), and the review bar.

use std::collections::{HashMap, HashSet};

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Margin, Rect, RichText, Sense, Stroke, Ui, pos2,
    vec2,
};
use hadion_core::devops::review::{
    DraftComment, FileStatus, PrDetail, PrFile, ReviewEvent, ReviewThread, patch, rest,
};
use hadion_core::diff::LineKind;

use super::view::search_field;
use super::widgets::{self, person, rel};
use super::{Act, LineSel, Parsed, PrView, State, viewed_key};
use crate::format;
use crate::state::{Action, RepoState, Settings, Span};
use crate::theme::{Palette, Tone, medium, pal, radius, semibold, size, space};
use crate::views::form;
use crate::views::icons::{self, Icon};
use crate::views::{kit, truncated};

const LINE_H: f32 = 20.0;
const NUM_W: f32 = 46.0;
const MARK_W: f32 = 16.0;
const HUNK_H: f32 = 26.0;
const FILE_ROW_H: f32 = 30.0;
/// Where code starts in a line row.
const CODE_X: f32 = NUM_W * 2.0 + MARK_W + 6.0;
/// Patches longer than this are shown without syntax colours.
const MAX_HIGHLIGHT_LINES: usize = 6_000;

pub fn show(
    ui: &mut Ui,
    repo: &mut RepoState,
    _settings: &Settings,
    n: u64,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let changing = repo.devops.review.changing.is_some();
    let State { prs, viewed, .. } = &mut repo.devops.review;
    let v = prs.entry(n).or_default();

    // A third of the detail at most by default, half when dragged: on a
    // narrow window the diff beside it was squeezed to a sliver.
    let w = ui.available_width();
    egui::Panel::left("rv_files")
        .resizable(true)
        .default_size((w * 0.32).clamp(180.0, 300.0))
        .size_range(170.0..=(w * 0.5).clamp(170.0, 560.0))
        .frame(Frame::NONE.fill(p.panel).inner_margin(Margin {
            left: 8,
            right: 8,
            top: 10,
            bottom: 6,
        }))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            file_list(ui, v, viewed, n, actions);
        });
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas))
        .show(ui, |ui| {
            review_bar(ui, v, viewed, n, changing, actions);
            let Some(files) = v.files.data.as_ref() else {
                return;
            };
            let Some(file) = v
                .file
                .as_ref()
                .and_then(|path| files.iter().find(|f| &f.path == path))
                .cloned()
            else {
                Frame::NONE.inner_margin(Margin::same(24)).show(ui, |ui| {
                    kit::empty_state(
                        ui,
                        Icon::List,
                        "Select a file",
                        "Its patch appears here with the review threads under their lines.",
                        None,
                    );
                });
                return;
            };
            let mut next = false;
            file_header(
                ui,
                &file,
                viewed,
                n,
                v.detail.data.as_ref(),
                &mut next,
                actions,
            );
            if next {
                jump_next(v);
            }
            diff(ui, v, &file, n, dark, changing, now, actions);
        });
}

// ---------------------------------------------------------------------------
// File list
// ---------------------------------------------------------------------------

fn status_mark(s: FileStatus, p: &Palette) -> (&'static str, Color32) {
    match s {
        FileStatus::Added | FileStatus::Copied => ("A", p.success),
        FileStatus::Removed => ("D", p.danger),
        FileStatus::Renamed => ("R", p.accent),
        FileStatus::Modified | FileStatus::Changed => ("M", p.warning),
        FileStatus::Unchanged => ("·", p.text_3),
    }
}

fn file_list(
    ui: &mut Ui,
    v: &mut PrView,
    viewed: &HashSet<String>,
    n: u64,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let Some(files) = v.files.data.as_ref() else {
        if let Some(e) = &v.files.error {
            if widgets::error_block(ui, "Could not load the files", e) {
                actions.push(Act::RefreshPr(n).into());
            }
        } else {
            widgets::loading_block(
                ui,
                "Loading changed files…",
                "GET pulls/n/files, 100 per page.",
            );
        }
        return;
    };
    let (add, del) = files
        .iter()
        .fold((0u64, 0u64), |(a, d), f| (a + f.additions, d + f.deletions));
    let seen = files
        .iter()
        .filter(|f| viewed.contains(&viewed_key(n, f)))
        .count();
    let threads: HashMap<&str, (usize, usize)> = {
        let mut m: HashMap<&str, (usize, usize)> = HashMap::new();
        if let Some(d) = v.detail.data.as_ref() {
            for t in &d.threads {
                let e = m.entry(t.path.as_str()).or_default();
                e.0 += 1;
                if !t.resolved {
                    e.1 += 1;
                }
            }
        }
        m
    };
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            RichText::new(format::plural(files.len(), "file", "files"))
                .size(size::BODY)
                .family(semibold())
                .color(p.text_strong),
        );
        ui.label(
            RichText::new(format!("+{}", format::count(add as usize)))
                .size(size::SMALL)
                .color(p.success),
        );
        ui.label(
            RichText::new(format!("\u{2212}{}", format::count(del as usize)))
                .size(size::SMALL)
                .color(p.danger),
        );
        ui.label(
            RichText::new(format!("·  {seen} viewed"))
                .size(size::SMALL)
                .color(p.text_3),
        );
    });
    ui.add_space(space::S);
    search_field(ui, "rv_file_filter", &mut v.file_filter, "Filter files");
    ui.add_space(space::XS);
    let needle = v.file_filter.trim().to_lowercase();
    let rows: Vec<usize> = files
        .iter()
        .enumerate()
        .filter(|(_, f)| needle.is_empty() || f.path.to_lowercase().contains(&needle))
        .map(|(i, _)| i)
        .collect();
    if rows.is_empty() {
        ui.add_space(space::S);
        kit::muted(
            ui,
            if files.is_empty() {
                "This pull request changes no files."
            } else {
                "No file matches the filter."
            },
        );
        return;
    }
    let selected = v.file.clone();
    let mut clicked: Option<String> = None;
    kit::list(
        ui,
        &format!("rv_file_rows_{n}"),
        rows.len(),
        FILE_ROW_H,
        |i| selected.as_deref() == Some(files[rows[i]].path.as_str()),
        |ui, rect, resp, i| {
            let f = &files[rows[i]];
            let seen = viewed.contains(&viewed_key(n, f));
            let painter = ui.painter();
            let mid = rect.center().y;
            let (mark, color) = status_mark(f.status, &p);
            painter.text(
                pos2(rect.left() + 16.0, mid),
                Align2::CENTER_CENTER,
                mark,
                FontId::new(size::SMALL, semibold()),
                color,
            );
            let mut right = rect.right() - 10.0;
            if seen {
                icons::paint(
                    painter,
                    Icon::Check,
                    pos2(right - 6.0, mid),
                    11.0,
                    p.success,
                );
                right -= 18.0;
            }
            let stat = format!(
                "+{} \u{2212}{}",
                format::count(f.additions as usize),
                format::count(f.deletions as usize)
            );
            let g = painter.layout_no_wrap(stat, FontId::proportional(size::SMALL), p.text_3);
            right -= g.size().x;
            painter.galley(pos2(right, mid - g.size().y / 2.0), g, p.text_3);
            right -= 8.0;
            if let Some((all, open)) = threads.get(f.path.as_str()) {
                let text = all.to_string();
                let tone = if *open > 0 { p.warning } else { p.text_3 };
                let g = painter.layout_no_wrap(text, FontId::proportional(size::SMALL), tone);
                right -= g.size().x;
                painter.galley(pos2(right, mid - g.size().y / 2.0), g, tone);
                icons::paint(painter, Icon::Comment, pos2(right - 9.0, mid), 11.0, tone);
                right -= 22.0;
            }
            let (dir, name) = format::split_path(&f.path);
            let fg = if seen { p.text_3 } else { p.text };
            let x = rect.left() + 30.0;
            let ng = truncated(
                painter,
                name,
                FontId::new(size::SMALL + 0.5, medium()),
                fg,
                (right - x).max(20.0),
            );
            let nw = ng.size().x;
            painter.galley(pos2(x, mid - ng.size().y / 2.0), ng, fg);
            if !dir.is_empty() && right - x - nw > 40.0 {
                let dg = truncated(
                    painter,
                    dir.trim_end_matches('/'),
                    FontId::proportional(size::SMALL),
                    p.text_3,
                    right - x - nw - 8.0,
                );
                painter.galley(pos2(x + nw + 8.0, mid - dg.size().y / 2.0), dg, p.text_3);
            }
            if resp.clicked() {
                clicked = Some(f.path.clone());
            }
        },
    );
    if let Some(path) = clicked
        && v.file.as_deref() != Some(path.as_str())
    {
        v.file = Some(path);
        v.sel = None;
        v.composer_error.clear();
    }
}

// ---------------------------------------------------------------------------
// Review bar
// ---------------------------------------------------------------------------

fn review_bar(
    ui: &mut Ui,
    v: &mut PrView,
    viewed: &HashSet<String>,
    n: u64,
    changing: bool,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let head = v
        .detail
        .data
        .as_ref()
        .map(|d| d.pr.head_oid.clone())
        .unwrap_or_default();
    let total = v.files.data.as_ref().map(|f| f.len()).unwrap_or(0);
    let seen = v
        .files
        .data
        .as_ref()
        .map(|f| {
            f.iter()
                .filter(|f| viewed.contains(&viewed_key(n, f)))
                .count()
        })
        .unwrap_or(0);
    Frame::NONE
        .fill(p.panel)
        .inner_margin(Margin::symmetric(16, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // The button first (right to left), then the text in what is
            // left, cut rather than drawn under the button.
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), 30.0),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if !v.review_open {
                        let label = if v.pending.is_empty() {
                            "Review changes".to_owned()
                        } else {
                            format!("Finish review ({})", v.pending.len())
                        };
                        if form::primary(ui, &label, true).clicked() {
                            v.review_open = true;
                        }
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.label(
                            RichText::new(if head.is_empty() {
                                "Review".to_owned()
                            } else {
                                format!("Reviewing {}", &head[..head.len().min(7)])
                            })
                            .size(size::BODY)
                            .family(medium())
                            .color(p.text_strong),
                        );
                        if !v.pending.is_empty() {
                            kit::badge(ui, &format!("{} pending", v.pending.len()), Tone::Warning);
                        }
                        let viewed_text = format!("{seen} of {total} files viewed");
                        let viewed_w = ui
                            .painter()
                            .layout_no_wrap(
                                viewed_text.clone(),
                                egui::FontId::proportional(size::SMALL),
                                p.text_3,
                            )
                            .size()
                            .x;
                        // Only when it fits: it is also in the file list.
                        if total > 0 && ui.available_width() > viewed_w + 8.0 {
                            ui.label(RichText::new(viewed_text).size(size::SMALL).color(p.text_3));
                        }
                        if v.pending.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    RichText::new("Select a line, or drag over lines, to comment")
                                        .size(size::SMALL)
                                        .color(p.text_2),
                                )
                                .truncate(),
                            );
                        }
                    });
                },
            );
            if v.review_open {
                ui.add_space(space::S);
                form::card(ui, Tone::Neutral, |ui| {
                    kit::subtitle(ui, "Finish your review");
                    kit::secondary(
                        ui,
                        &format!(
                            "Sends the summary and {} in one review on {}.",
                            format::plural(v.pending.len(), "pending comment", "pending comments"),
                            if head.is_empty() {
                                "the head"
                            } else {
                                &head[..head.len().min(7)]
                            }
                        ),
                    );
                    ui.add_space(4.0);
                    for e in ReviewEvent::ALL {
                        ui.horizontal(|ui| {
                            if form::radio(ui, v.review_event == e, e.label()) {
                                v.review_event = e;
                            }
                            ui.label(RichText::new(e.hint()).size(size::SMALL).color(p.text_2));
                        });
                    }
                    ui.add_space(4.0);
                    form::area(
                        ui,
                        ("rv_review_body", n),
                        &mut v.review_body,
                        "Summary (needed to request changes)",
                        3,
                    );
                    let problem =
                        rest::review_problem(v.review_event, &v.review_body, v.pending.len());
                    if let Some(pr) = problem {
                        ui.add_space(2.0);
                        kit::muted(ui, pr);
                    }
                    if !v.pending.is_empty() {
                        ui.add_space(2.0);
                        let list: Vec<String> = v
                            .pending
                            .iter()
                            .map(|c| format!("{} {}", c.path, c.range_label()))
                            .collect();
                        kit::secondary(ui, &format!("Pending: {}", list.join("; ")));
                    }
                    form::actions_row(ui, |ui| {
                        if form::primary(ui, "Submit review", problem.is_none() && !changing)
                            .clicked()
                        {
                            actions.push(Act::SubmitReview(n).into());
                        }
                        if form::button(ui, "Cancel", true).clicked() {
                            v.review_open = false;
                        }
                    });
                });
            }
        });
    let r = ui.available_rect_before_wrap();
    ui.painter()
        .hline(r.x_range(), r.top(), Stroke::new(1.0, p.border));
}

// ---------------------------------------------------------------------------
// File header
// ---------------------------------------------------------------------------

fn file_header(
    ui: &mut Ui,
    f: &PrFile,
    viewed: &HashSet<String>,
    n: u64,
    detail: Option<&PrDetail>,
    next: &mut bool,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    Frame::NONE
        .inner_margin(Margin {
            left: 16,
            right: 16,
            top: 10,
            bottom: 8,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if let Some(d) = detail
                        && icons::IconButton::icon(Icon::External)
                            .compact()
                            .show(ui)
                            .on_hover_text("This pull request's files on GitHub")
                            .clicked()
                    {
                        actions.push(Action::OpenUrl(format!("{}/files", d.pr.url)));
                    }
                    let key = viewed_key(n, f);
                    let mut seen = viewed.contains(&key);
                    if form::checkbox(ui, &mut seen, "Viewed").changed() {
                        actions.push(Act::ToggleViewed(key).into());
                    }
                    if detail.is_some_and(|d| d.threads_for(&f.path).next().is_some())
                        && form::button(ui, "Next thread", true)
                            .on_hover_text(
                                "Scroll to the next review thread of this file, unresolved ones first",
                            )
                            .clicked()
                    {
                        *next = true;
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let (dir, name) = format::split_path(&f.path);
                        ui.add(
                            egui::Label::new(
                                RichText::new(name)
                                    .size(size::TITLE)
                                    .family(semibold())
                                    .color(p.text_strong),
                            )
                            .truncate(),
                        );
                        if !dir.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(dir.trim_end_matches('/'))
                                        .size(size::SMALL)
                                        .color(p.text_3),
                                )
                                .truncate(),
                            );
                        }
                    });
                });
            });
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
                let (_, color) = status_mark(f.status, &p);
                let tone = if color == p.success {
                    Tone::Success
                } else if color == p.danger {
                    Tone::Danger
                } else if color == p.accent {
                    Tone::Accent
                } else {
                    Tone::Neutral
                };
                kit::badge(ui, f.status.label(), tone);
                ui.label(
                    RichText::new(format!("+{}", format::count(f.additions as usize)))
                        .size(size::SMALL)
                        .color(p.success),
                );
                ui.label(
                    RichText::new(format!("\u{2212}{}", format::count(f.deletions as usize)))
                        .size(size::SMALL)
                        .color(p.danger),
                );
                if let Some(prev) = &f.previous {
                    ui.label(
                        RichText::new(format!("renamed from {prev}"))
                            .size(size::SMALL)
                            .color(p.text_2),
                    );
                }
                if let Some(d) = detail {
                    let all = d.threads_for(&f.path).count();
                    if all > 0 {
                        let open = d.threads_for(&f.path).filter(|t| !t.resolved).count();
                        ui.label(
                            RichText::new(format!(
                                "{}, {open} unresolved",
                                format::plural(all, "thread", "threads")
                            ))
                            .size(size::SMALL)
                            .color(p.text_2),
                        );
                    }
                }
            });
        });
}

// ---------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------

/// Parse (and colour) a file's patch once per file version and theme.
fn ensure_parsed<'a>(
    parsed: &'a mut HashMap<String, Parsed>,
    f: &PrFile,
    dark: bool,
) -> &'a Parsed {
    let stale = parsed
        .get(&f.path)
        .is_none_or(|x| x.sha != f.sha || x.dark != dark);
    if stale {
        let text = f.patch.as_deref().unwrap_or("");
        let hunks = patch::parse_patch(text);
        let empty_spans: Vec<Vec<Vec<Span>>> = hunks
            .iter()
            .map(|h| vec![Vec::new(); h.lines.len()])
            .collect();
        let empty_emph: Vec<Vec<Vec<(u32, u32)>>> = hunks
            .iter()
            .map(|h| vec![Vec::new(); h.lines.len()])
            .collect();
        let total: usize = hunks.iter().map(|h| h.lines.len()).sum();
        let core = hadion_core::diff::parse(text.as_bytes()).into_iter().next();
        let same_shape = core.as_ref().is_some_and(|fd| {
            fd.hunks.len() == hunks.len()
                && fd
                    .hunks
                    .iter()
                    .zip(&hunks)
                    .all(|(a, b)| a.lines.len() == b.lines.len())
        });
        let (spans, emph) = match core {
            Some(fd) if same_shape => {
                let spans = if total <= MAX_HIGHLIGHT_LINES {
                    crate::highlight::highlight(&fd, &f.path, dark)
                } else {
                    empty_spans
                };
                let emph = fd
                    .hunks
                    .iter()
                    .map(|h| h.lines.iter().map(|l| l.emph.clone()).collect())
                    .collect();
                (spans, emph)
            }
            _ => (empty_spans, empty_emph),
        };
        parsed.insert(
            f.path.clone(),
            Parsed {
                sha: f.sha.clone(),
                dark,
                hunks,
                spans,
                emph,
            },
        );
    }
    &parsed[&f.path]
}

fn kind_fill(kind: LineKind, dark: bool, p: &Palette) -> (Color32, Color32) {
    let (line, gutter) = if dark { (0.12, 0.20) } else { (0.10, 0.18) };
    match kind {
        LineKind::Added => (
            p.success.gamma_multiply(line),
            p.success.gamma_multiply(gutter),
        ),
        LineKind::Removed => (
            p.danger.gamma_multiply(line),
            p.danger.gamma_multiply(gutter),
        ),
        _ => (Color32::TRANSPARENT, p.panel),
    }
}

fn line_galley(
    painter: &egui::Painter,
    text: &str,
    spans: &[Span],
    font: &FontId,
    default: Color32,
) -> std::sync::Arc<egui::Galley> {
    if spans.is_empty() {
        return painter.layout_no_wrap(text.to_owned(), font.clone(), default);
    }
    let byte_at: Vec<usize> = text
        .char_indices()
        .map(|(b, _)| b)
        .chain(std::iter::once(text.len()))
        .collect();
    let nchars = byte_at.len() as u32 - 1;
    let mut job = LayoutJob::default();
    let fmt = |color: Color32| TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    let mut pos = 0u32;
    for s in spans {
        let (start, end) = (s.start.min(nchars), s.end.min(nchars));
        if start > pos {
            job.append(
                &text[byte_at[pos as usize]..byte_at[start as usize]],
                0.0,
                fmt(default),
            );
        }
        if end > start.max(pos) {
            let from = start.max(pos);
            job.append(
                &text[byte_at[from as usize]..byte_at[end as usize]],
                0.0,
                fmt(s.color),
            );
        }
        pos = pos.max(end);
    }
    if pos < nchars {
        job.append(&text[byte_at[pos as usize]..], 0.0, fmt(default));
    }
    painter.layout_job(job)
}

#[allow(clippy::too_many_arguments)]
fn diff(
    ui: &mut Ui,
    v: &mut PrView,
    f: &PrFile,
    n: u64,
    dark: bool,
    changing: bool,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let PrView {
        detail,
        parsed,
        sel,
        dragging,
        composer,
        composer_error,
        pending,
        replies,
        opened,
        closed,
        jump,
        ..
    } = v;
    if *dragging && !ui.input(|i| i.pointer.primary_down()) {
        *dragging = false;
    }
    let parsed = ensure_parsed(parsed, f, dark);
    let hunks = &parsed.hunks;
    let detail = detail.data.as_ref();
    let threads: Vec<&ReviewThread> = detail
        .map(|d| d.threads_for(&f.path).collect())
        .unwrap_or_default();
    let mut placed: HashMap<patch::LinePos, Vec<&ReviewThread>> = HashMap::new();
    let mut unplaced: Vec<&ReviewThread> = Vec::new();
    for t in &threads {
        match t
            .anchor()
            .and_then(|(side, line)| patch::find_line(hunks, side, line))
        {
            Some(pos) => placed.entry(pos).or_default().push(t),
            None => unplaced.push(t),
        }
    }
    let mut drafts: HashMap<patch::LinePos, Vec<usize>> = HashMap::new();
    for (i, c) in pending.iter().enumerate() {
        if c.path == f.path
            && let Some(pos) = patch::find_line(hunks, c.side, c.line)
        {
            drafts.entry(pos).or_default().push(i);
        }
    }
    let sel_here = sel.as_ref().filter(|s| s.path == f.path).cloned();
    let composer_at = sel_here.as_ref().map(|s| s.range().1);
    let mono = FontId::monospace(size::MONO);
    let mut remove_draft: Option<usize> = None;

    super::view::scroll_area(("rv_diff", n, f.path.as_str()))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let w = ui.available_width();
            if !unplaced.is_empty() {
                Frame::NONE.inner_margin(Margin::symmetric(16, 8)).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    kit::subtitle(
                        ui,
                        &format!(
                            "{} on earlier versions or the whole file",
                            format::plural(unplaced.len(), "thread", "threads")
                        ),
                    );
                    kit::secondary(
                        ui,
                        "These threads point at lines that are no longer in this diff (outdated), or at the file itself.",
                    );
                    for t in &unplaced {
                        thread_block(
                            ui, t, n, 0.0, jump, opened, closed, replies, changing, now, actions,
                        );
                    }
                });
                kit::hairline(ui);
            }
            if f.patch.is_none() {
                Frame::NONE.inner_margin(Margin::same(16)).show(ui, |ui| {
                    kit::callout(
                        ui,
                        Tone::Neutral,
                        "No patch to show",
                        "GitHub sends no patch for binary files, very large diffs and pure renames. Open the file on GitHub to see it.",
                    );
                });
                return;
            }
            for (hi, hunk) in hunks.iter().enumerate() {
                let (rect, _) = ui.allocate_exact_size(vec2(w, HUNK_H), Sense::hover());
                if ui.is_rect_visible(rect) {
                    ui.painter().rect_filled(rect, 0, p.raised);
                    ui.painter().text(
                        pos2(rect.left() + 12.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        &hunk.header,
                        FontId::monospace(size::MONO),
                        p.text_2,
                    );
                }
                for (li, line) in hunk.lines.iter().enumerate() {
                    let pos = patch::LinePos { hunk: hi, line: li };
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(w, LINE_H), Sense::click_and_drag());
                    let commentable = line.anchor().is_some();
                    if commentable {
                        if resp.drag_started() {
                            *sel = Some(LineSel {
                                path: f.path.clone(),
                                anchor: pos,
                                end: pos,
                            });
                            *dragging = true;
                            composer_error.clear();
                        } else if resp.clicked() {
                            let shift = ui.input(|i| i.modifiers.shift);
                            match sel.as_mut() {
                                Some(s) if shift && s.path == f.path => s.end = pos,
                                _ => {
                                    *sel = Some(LineSel {
                                        path: f.path.clone(),
                                        anchor: pos,
                                        end: pos,
                                    })
                                }
                            }
                            composer_error.clear();
                        }
                        if *dragging
                            && ui.rect_contains_pointer(rect)
                            && let Some(s) = sel.as_mut()
                            && s.path == f.path
                            && pos.hunk == s.anchor.hunk
                        {
                            s.end = pos;
                        }
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                    }
                    if ui.is_rect_visible(rect) {
                        let selected = sel
                            .as_ref()
                            .is_some_and(|s| s.path == f.path && s.contains(pos));
                        paint_line(
                            ui,
                            rect,
                            line,
                            parsed
                                .spans
                                .get(hi)
                                .and_then(|h| h.get(li))
                                .map(Vec::as_slice)
                                .unwrap_or(&[]),
                            parsed
                                .emph
                                .get(hi)
                                .and_then(|h| h.get(li))
                                .map(Vec::as_slice)
                                .unwrap_or(&[]),
                            &mono,
                            selected,
                            commentable && resp.hovered(),
                            dark,
                        );
                    }
                    if let Some(ts) = placed.get(&pos) {
                        ui.add_space(4.0);
                        for t in ts {
                            thread_block(
                                ui, t, n, CODE_X, jump, opened, closed, replies, changing, now,
                                actions,
                            );
                        }
                        ui.add_space(4.0);
                    }
                    if let Some(ds) = drafts.get(&pos) {
                        ui.add_space(4.0);
                        for i in ds {
                            if draft_block(ui, &pending[*i]) {
                                remove_draft = Some(*i);
                            }
                        }
                        ui.add_space(4.0);
                    }
                    if composer_at == Some(pos)
                        && let Some(s) = &sel_here
                    {
                        ui.add_space(4.0);
                        composer_block(
                            ui,
                            hunks,
                            s,
                            composer,
                            composer_error,
                            n,
                            changing,
                            sel,
                            actions,
                        );
                        ui.add_space(4.0);
                    }
                }
            }
            ui.add_space(40.0);
        });
    if let Some(i) = remove_draft {
        actions.push(Act::RemovePending(n, i).into());
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_line(
    ui: &Ui,
    rect: Rect,
    line: &patch::PatchLine,
    spans: &[Span],
    emph: &[(u32, u32)],
    font: &FontId,
    selected: bool,
    hovered: bool,
    dark: bool,
) {
    let p = *pal(ui);
    let painter = ui.painter();
    let (bg, gutter_bg) = kind_fill(line.kind, dark, &p);
    if bg != Color32::TRANSPARENT {
        painter.rect_filled(rect, 0, bg);
    }
    painter.rect_filled(
        Rect::from_min_max(
            rect.min,
            pos2(rect.left() + NUM_W * 2.0 + MARK_W, rect.bottom()),
        ),
        0,
        gutter_bg,
    );
    if selected {
        painter.rect_filled(
            rect,
            0,
            p.accent.gamma_multiply(if dark { 0.22 } else { 0.16 }),
        );
        painter.rect_filled(
            Rect::from_min_size(rect.min, vec2(3.0, rect.height())),
            0,
            p.accent,
        );
    }
    let mid = rect.center().y;
    let num_font = FontId::monospace(size::CAPTION + 0.5);
    for (x, n) in [
        (rect.left() + NUM_W - 8.0, line.old),
        (rect.left() + NUM_W * 2.0 - 8.0, line.new),
    ] {
        if let Some(n) = n {
            painter.text(
                pos2(x, mid),
                Align2::RIGHT_CENTER,
                n.to_string(),
                num_font.clone(),
                p.text_3,
            );
        }
    }
    if hovered && !selected {
        let r = Rect::from_center_size(pos2(rect.left() + 10.0, mid), vec2(14.0, 14.0));
        painter.rect_filled(r, CornerRadius::same(radius::S), p.accent);
        icons::paint(painter, Icon::Plus, r.center(), 9.0, Color32::WHITE);
    }
    let (marker, mcolor) = match line.kind {
        LineKind::Added => ("+", p.success),
        LineKind::Removed => ("\u{2212}", p.danger),
        _ => ("", Color32::TRANSPARENT),
    };
    if !marker.is_empty() {
        painter.text(
            pos2(rect.left() + NUM_W * 2.0 + 4.0, mid),
            Align2::LEFT_CENTER,
            marker,
            font.clone(),
            mcolor,
        );
    }
    let x = rect.left() + CODE_X;
    let char_w = painter
        .layout_no_wrap("0".to_owned(), font.clone(), p.text)
        .size()
        .x;
    if !emph.is_empty() {
        let c = match line.kind {
            LineKind::Added => p.success,
            _ => p.danger,
        }
        .gamma_multiply(if dark { 0.34 } else { 0.30 });
        for &(s, e) in emph {
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(x + s as f32 * char_w, rect.top() + 1.0),
                    pos2(x + e as f32 * char_w, rect.bottom() - 1.0),
                ),
                CornerRadius::same(2),
                c,
            );
        }
    }
    let color = if line.kind == LineKind::NoNewline {
        p.text_3
    } else {
        p.text
    };
    let g = line_galley(painter, &line.text, spans, font, color);
    let clip = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    clip.galley(pos2(x, mid - g.size().y / 2.0), g, color);
}

/// A card indented to the code column.
fn indented<R>(ui: &mut Ui, indent: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let w = (ui.available_width() - indent - 16.0).clamp(240.0, kit::PROSE + 80.0);
    ui.horizontal(|ui| {
        ui.add_space(indent);
        ui.vertical(|ui| {
            ui.set_width(w);
            ui.spacing_mut().item_spacing.y = 6.0;
            add(ui)
        })
        .inner
    })
    .inner
}

#[allow(clippy::too_many_arguments)]
fn thread_block(
    ui: &mut Ui,
    t: &ReviewThread,
    n: u64,
    indent: f32,
    jump: &mut Option<String>,
    opened: &mut HashSet<String>,
    closed: &mut HashSet<String>,
    replies: &mut HashMap<String, String>,
    changing: bool,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let default_open = !t.resolved && !t.outdated;
    let open = if default_open {
        !closed.contains(&t.id)
    } else {
        opened.contains(&t.id)
    };
    indented(ui, indent, |ui| {
        let border = if t.resolved {
            p.border
        } else {
            p.warning.gamma_multiply(0.55)
        };
        let frame = Frame::NONE
            .fill(p.raised)
            .stroke(Stroke::new(1.0, border))
            .corner_radius(CornerRadius::same(radius::L))
            .inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                // Header: a click target with the resolve button laid over it.
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
                let painter = ui.painter();
                let mid = rect.center().y;
                icons::paint(
                    painter,
                    if open {
                        Icon::ChevronUp
                    } else {
                        Icon::ChevronDown
                    },
                    pos2(rect.left() + 6.0, mid),
                    10.0,
                    p.text_2,
                );
                let first = t.comments.first();
                let who = first.map(|c| c.author.as_str()).unwrap_or("");
                crate::views::avatar(
                    painter,
                    pos2(rect.left() + 26.0, mid),
                    9.0,
                    if who.is_empty() { "?" } else { who },
                    dark,
                );
                let mut x = rect.left() + 40.0;
                let g = painter.layout_no_wrap(
                    if who.is_empty() {
                        "ghost".to_owned()
                    } else {
                        who.to_owned()
                    },
                    FontId::new(size::SMALL + 0.5, medium()),
                    p.text_strong,
                );
                let gw = g.size().x;
                painter.galley(pos2(x, mid - g.size().y / 2.0), g, p.text_strong);
                x += gw + 8.0;
                let mut facts = format!(
                    "{}  ·  {}",
                    format::plural(
                        t.comments_total.max(t.comments.len() as u64) as usize,
                        "comment",
                        "comments"
                    ),
                    t.line_label()
                );
                if let Some(c) = first.and_then(|c| c.created) {
                    facts.push_str(&format!("  ·  {}", format::relative(c, now)));
                }
                let button_w = 92.0;
                let mut badges: Vec<(String, Tone)> = Vec::new();
                if t.resolved {
                    badges.push(("Resolved".to_owned(), Tone::Success));
                }
                if t.outdated {
                    badges.push(("Outdated".to_owned(), Tone::Neutral));
                }
                x = widgets::paint_badges_from(
                    painter,
                    &p,
                    dark,
                    x,
                    mid,
                    &badges,
                    rect.right() - button_w,
                );
                let fg = truncated(
                    painter,
                    &facts,
                    FontId::proportional(size::SMALL),
                    p.text_2,
                    (rect.right() - button_w - x - 8.0).max(20.0),
                );
                painter.galley(pos2(x + 2.0, mid - fg.size().y / 2.0), fg, p.text_2);
                let br =
                    Rect::from_min_max(pos2(rect.right() - button_w + 4.0, rect.top()), rect.max);
                let label = if t.resolved { "Unresolve" } else { "Resolve" };
                let b = kit::overlay(ui, br, icons::IconButton::text(label).compact());
                if b.clicked() && !changing {
                    actions.push(
                        Act::Resolve {
                            pr: n,
                            thread: t.id.clone(),
                            resolve: !t.resolved,
                        }
                        .into(),
                    );
                } else if resp.clicked() {
                    let set_open = !open;
                    if default_open {
                        if set_open {
                            closed.remove(&t.id);
                        } else {
                            closed.insert(t.id.clone());
                        }
                    } else if set_open {
                        opened.insert(t.id.clone());
                    } else {
                        opened.remove(&t.id);
                    }
                }
                if ui.rect_contains_pointer(rect) && !ui.rect_contains_pointer(br) {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if !open {
                    return;
                }
                if let Some(by) = &t.resolved_by {
                    kit::muted(ui, &format!("Resolved by {by}"));
                }
                for c in &t.comments {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        person(ui, &c.author, false);
                        ui.label(
                            RichText::new(rel(c.created, now))
                                .size(size::SMALL)
                                .color(p.text_3),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(26.0);
                        ui.vertical(|ui| widgets::comment_text(ui, &c.body));
                    });
                }
                let missing = t.comments_total as usize > t.comments.len();
                if missing {
                    kit::muted(
                        ui,
                        &format!(
                            "{} more on GitHub.",
                            format::plural(
                                t.comments_total as usize - t.comments.len(),
                                "comment",
                                "comments"
                            )
                        ),
                    );
                }
                if t.reply_to().is_some() {
                    ui.add_space(4.0);
                    let buf = replies.entry(t.id.clone()).or_default();
                    form::area(ui, ("rv_reply", n, t.id.as_str()), buf, "Reply…", 2);
                    let can = !buf.trim().is_empty() && !changing;
                    form::actions_row(ui, |ui| {
                        if form::primary(ui, "Reply", can).clicked() {
                            actions.push(
                                Act::Reply {
                                    pr: n,
                                    thread: t.id.clone(),
                                }
                                .into(),
                            );
                        }
                    });
                }
            });
        if jump.as_deref() == Some(t.id.as_str()) {
            ui.scroll_to_rect(frame.response.rect, Some(egui::Align::TOP));
            *jump = None;
        }
    });
}

/// A pending comment; returns true when Remove was clicked.
fn draft_block(ui: &mut Ui, c: &DraftComment) -> bool {
    let mut remove = false;
    indented(ui, CODE_X, |ui| {
        form::card(ui, Tone::Warning, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                kit::badge(ui, "Pending", Tone::Warning);
                ui.label(
                    RichText::new(format!("Your comment on {}", c.range_label()))
                        .size(size::SMALL)
                        .color(pal(ui).text_2),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    remove = form::button(ui, "Remove", true).clicked();
                });
            });
            widgets::comment_text(ui, &c.body);
        });
    });
    remove
}

#[allow(clippy::too_many_arguments)]
fn composer_block(
    ui: &mut Ui,
    hunks: &[patch::PatchHunk],
    s: &LineSel,
    composer: &mut String,
    composer_error: &mut String,
    n: u64,
    changing: bool,
    sel: &mut Option<LineSel>,
    actions: &mut Vec<Action>,
) {
    let (a, b) = s.range();
    let anchor = patch::range_anchor(hunks, a, b);
    let can_suggest = patch::suggestion_lines(hunks, a, b).is_some();
    indented(ui, CODE_X, |ui| {
        form::card(ui, Tone::Accent, |ui| {
            let what = match &anchor {
                Ok((start, end)) => format!("Comment on {}", range_words(*start, *end)),
                Err(e) => (*e).to_owned(),
            };
            kit::subtitle(ui, &what);
            kit::secondary(
                ui,
                "Shift-click or drag to cover more lines. Review comments wait here until you finish the review; a single comment is published right away.",
            );
            ui.add_space(2.0);
            form::area(ui, ("rv_composer", n), composer, "Leave a comment", 4);
            form::error(ui, composer_error);
            let ok = anchor.is_ok() && !composer.trim().is_empty();
            form::actions_row(ui, |ui| {
                if form::primary(ui, "Add review comment", ok).clicked() {
                    actions.push(Act::AddPending(n).into());
                }
                if form::button(ui, "Add single comment", ok && !changing).clicked() {
                    actions.push(Act::SingleComment(n).into());
                }
                if form::button(ui, "Suggest a change", can_suggest).clicked() {
                    actions.push(Act::Suggest(n).into());
                }
                if form::button(ui, "Cancel", true).clicked() {
                    *sel = None;
                    composer_error.clear();
                }
            });
        });
    });
}

/// `line 12`, `lines 10–14`, `old line 9`, `old line 9 to line 11`.
fn range_words(
    start: Option<(hadion_core::devops::review::Side, u32)>,
    end: (hadion_core::devops::review::Side, u32),
) -> String {
    use hadion_core::devops::review::Side::{Left, Right};
    match (start, end) {
        (None, (Right, l)) => format!("line {l}"),
        (None, (Left, l)) => format!("old line {l}"),
        (Some((Right, a)), (Right, b)) => format!("lines {a}–{b}"),
        (Some((Left, a)), (Left, b)) => format!("old lines {a}–{b}"),
        (Some((Left, a)), (Right, b)) => format!("old line {a} to line {b}"),
        (Some((Right, a)), (Left, b)) => format!("line {a} to old line {b}"),
    }
}

/// Threads of the selected file in reading order: the ones without a line
/// in the diff first, then by line.
fn ordered_threads<'a>(d: &'a PrDetail, path: &'a str) -> Vec<&'a ReviewThread> {
    let mut t: Vec<&ReviewThread> = d.threads_for(path).collect();
    t.sort_by_key(|t| (t.anchor().is_some(), t.line.unwrap_or(0)));
    t
}

/// Scroll to the next thread of the selected file (unresolved ones first,
/// then the rest), opening it.
pub fn jump_next(v: &mut PrView) {
    let (Some(d), Some(path)) = (v.detail.data.as_ref(), v.file.as_ref()) else {
        return;
    };
    let all = ordered_threads(d, path);
    let mut order: Vec<&ReviewThread> = all.iter().copied().filter(|t| !t.resolved).collect();
    order.extend(all.iter().copied().filter(|t| t.resolved));
    if order.is_empty() {
        return;
    }
    let at = v
        .last_jump
        .as_ref()
        .and_then(|id| order.iter().position(|t| &t.id == id))
        .map(|i| (i + 1) % order.len())
        .unwrap_or(0);
    let id = order[at].id.clone();
    v.closed.remove(&id);
    v.opened.insert(id.clone());
    v.last_jump = Some(id.clone());
    v.jump = Some(id);
}
