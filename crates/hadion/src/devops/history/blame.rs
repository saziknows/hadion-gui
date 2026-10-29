//! Blame of a file at any revision: a gutter per run of lines (short SHA,
//! author, date, summary) with the age as a colour scale, the code with
//! syntax colours, and a menu on each run: Show commit, Blame the previous
//! version (re-blame at the commit's parent, keeping the line in view),
//! Blame at this commit, Compare with its parent.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Margin, Rect, RichText, Sense, Ui, pos2, vec2,
};
use hadion_core::Oid;
use hadion_core::devops::history::blame::{self as core, Blame};

use super::widgets::{self, LINE_H};
use super::{Act, Msg};
use crate::devops::ctx::Ctx;
use crate::format;
use crate::state::{Action, RepoState, Settings, Span, ToastKind};
use crate::theme::{Palette, pal, size};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Stat, Tone};
use crate::views::truncated;

/// A blame ready to draw.
#[derive(Clone, Debug)]
pub struct BlameShow {
    pub blame: Blame,
    pub spans: Vec<Vec<Span>>,
    /// Per commit: its id when it parses (to find it in the graph).
    pub oids: Vec<Option<Oid>>,
    pub longest: usize,
    pub language: Option<String>,
    /// The theme the colours were made for.
    pub dark: bool,
}

#[derive(Default)]
pub struct BlameView {
    pub rev: String,
    pub path: String,
    pub seq: u64,
    pub loading: bool,
    pub cancel: Arc<AtomicBool>,
    pub result: Option<Result<BlameShow, String>>,
    /// The line to bring into view once loaded (1-based).
    pub scroll_to: Option<u32>,
    pub mark: Option<u32>,
    /// Where "Back" returns: (rev, path, line).
    pub back: Vec<(String, String, Option<u32>)>,
}

fn run(cx: &mut Ctx, repo: &mut RepoState, rev: String, path: String, line: Option<u32>) {
    let v = &mut repo.devops.history.blame;
    let rev = if rev.trim().is_empty() {
        "HEAD".to_owned()
    } else {
        rev.trim().to_owned()
    };
    let path = path.trim().replace('\\', "/");
    v.rev = rev.clone();
    v.path = path.clone();
    if path.is_empty() {
        v.result = Some(Err("Enter the path of a file to blame.".into()));
        return;
    }
    v.cancel.store(true, Ordering::Relaxed);
    v.cancel = Arc::new(AtomicBool::new(false));
    v.seq += 1;
    v.loading = true;
    v.scroll_to = line;
    v.mark = line;
    let seq = v.seq;
    let cancel = v.cancel.clone();
    let dark = cx.dark;
    cx.read(
        format!(
            "Blame {path} at {}",
            hadion_core::devops::history::short(&rev)
        ),
        move |dir, log| {
            let mut cmds = Vec::new();
            let r = core::run(dir, &rev, &path, &cancel, &mut cmds).map(|blame| {
                let texts: Vec<String> = blame.lines.iter().map(|l| l.text.clone()).collect();
                let spans = widgets::highlight_lines(&texts, &path, dark);
                BlameShow {
                    oids: blame
                        .commits
                        .iter()
                        .map(|c| Oid::from_str(&c.sha).ok())
                        .collect(),
                    longest: texts
                        .iter()
                        .map(|t| t.chars().count())
                        .max()
                        .unwrap_or(0)
                        .min(1500),
                    language: crate::highlight::language_for(&path),
                    spans,
                    dark,
                    blame,
                }
            });
            log.commands.extend(cmds);
            Msg::Blamed(seq, Box::new(r))
        },
    );
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::Blame { rev, path, line } => {
            repo.devops.history.mode = super::Mode::Blame;
            super::show_page(cx, repo);
            repo.devops.history.blame.back.clear();
            run(cx, repo, rev, path, line);
        }
        Act::BlamePrevious(index) => {
            let v = &mut repo.devops.history.blame;
            let Some(Ok(show)) = &v.result else { return };
            let Some(line) = show.blame.lines.get(index) else {
                return;
            };
            let commit = &show.blame.commits[line.commit as usize];
            let Some((prev, prev_path)) = commit.previous.clone() else {
                cx.toast(
                    ToastKind::Info,
                    if commit.boundary {
                        format!(
                            "{} is the oldest commit in reach: there is no earlier version to blame.",
                            commit.short()
                        )
                    } else {
                        format!("{} added this file: there is no earlier version.", commit.short())
                    },
                );
                return;
            };
            v.back
                .push((v.rev.clone(), v.path.clone(), Some(line.line)));
            let at = line.orig;
            run(cx, repo, prev, prev_path, Some(at));
        }
        Act::BlameBack => {
            let Some((rev, path, line)) = repo.devops.history.blame.back.pop() else {
                return;
            };
            run(cx, repo, rev, path, line);
        }
        Act::CancelBlame => {
            let v = &mut repo.devops.history.blame;
            v.cancel.store(true, Ordering::Relaxed);
            v.loading = false;
            v.seq += 1;
        }
        Act::BlameMark(l) => repo.devops.history.blame.mark = l,
        _ => {}
    }
}

pub fn on_msg(_cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    let v = &mut repo.devops.history.blame;
    if let Msg::Blamed(seq, r) = msg
        && seq == v.seq
    {
        v.loading = false;
        v.result = Some(*r);
    }
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

const GUTTER_W: f32 = 330.0;

/// Newest lines in the accent colour, the oldest fading to the border.
fn age_color(p: &Palette, age: f32) -> Color32 {
    p.border_strong.lerp_to_gamma(p.accent, age.clamp(0.0, 1.0))
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, _settings: &Settings, actions: &mut Vec<Action>) {
    let suggestions = super::ref_suggestions(repo);
    let graph = repo.snapshot.as_ref().map(|s| &s.index_of);
    let p = *pal(ui);
    let v = &mut repo.devops.history.blame;
    let mut submit = false;
    Frame::NONE
        .inner_margin(Margin {
            left: 24,
            right: 16,
            top: 10,
            bottom: 10,
        })
        .show(ui, |ui| {
            // The file input gives up width first; a window narrower still
            // wraps the row instead of pushing the Blame button off it.
            let file_w = (ui.available_width() - 470.0).clamp(160.0, 320.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                if !v.back.is_empty()
                    && IconButton::new(Icon::Undo, "Back")
                        .compact()
                        .show(ui)
                        .on_hover_text("Return to the blame before \"previous version\"")
                        .clicked()
                {
                    actions.push(Act::BlameBack.into());
                }
                widgets::bar_label(ui, "Revision");
                let (r, picked) = widgets::rev_input(
                    ui,
                    "blame_rev",
                    &mut v.rev,
                    "HEAD",
                    &suggestions,
                    200.0,
                    false,
                );
                submit |= widgets::submitted(ui, &r) || picked;
                widgets::bar_label(ui, "File");
                let r = crate::views::form::input(
                    ui,
                    ("hist_blame", "path"),
                    &mut v.path,
                    crate::views::form::InputOpts::new("src/main.rs")
                        .width(file_w)
                        .mono(),
                );
                submit |= widgets::submitted(ui, &r);
                if v.loading {
                    if IconButton::new(Icon::Stop, "Cancel")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Act::CancelBlame.into());
                    }
                } else if crate::views::form::primary(ui, "Blame", true).clicked() {
                    submit = true;
                }
            });
        });
    if submit {
        actions.push(
            Act::Blame {
                rev: v.rev.clone(),
                path: v.path.clone(),
                line: None,
            }
            .into(),
        );
    }
    kit::hairline(ui);
    let empty = |ui: &mut Ui, add: &mut dyn FnMut(&mut Ui)| {
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(p.canvas).inner_margin(Margin::same(24)))
            .show(ui, |ui| add(ui));
    };
    if v.loading {
        let (rev, path) = (v.rev.clone(), v.path.clone());
        empty(ui, &mut |ui| {
            kit::loading(ui, &format!("git blame {path} at {rev}…"));
            kit::muted(
                ui,
                "Large files with a long history take a few seconds (-M -C follow moved and copied lines).",
            );
        });
        return;
    }
    let show = match &v.result {
        None => {
            empty(ui, &mut |ui| {
                kit::empty_state(
                    ui,
                    Icon::Clock,
                    "Blame a file",
                    "Enter a revision and a file path. Each run of lines shows the commit that last changed it, its author and age; click it to see the commit or step back to the version before. Right-click a file in a commit's file list for Blame this version.",
                    None,
                );
            });
            return;
        }
        Some(Err(e)) => {
            let e = e.clone();
            let retry = Act::Blame {
                rev: v.rev.clone(),
                path: v.path.clone(),
                line: None,
            };
            let can_back = !v.back.is_empty();
            empty(ui, &mut |ui| {
                kit::callout_with(ui, Tone::Danger, "Could not blame the file", &e, |ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if IconButton::new(Icon::Rerun, "Try again")
                            .compact()
                            .show(ui)
                            .clicked()
                        {
                            actions.push(retry.clone().into());
                        }
                        if can_back
                            && IconButton::new(Icon::Undo, "Back")
                                .compact()
                                .show(ui)
                                .clicked()
                        {
                            actions.push(Act::BlameBack.into());
                        }
                    });
                });
            });
            return;
        }
        Some(Ok(s)) => s,
    };
    let b = &show.blame;
    let now = format::now_secs();
    // A short window gets the numbers on one line instead of tiles, so the
    // code keeps its room.
    let compact = ui.available_height() < 520.0;
    Frame::NONE
        .inner_margin(Margin {
            left: 24,
            right: 16,
            top: if compact { 8 } else { 10 },
            bottom: 8,
        })
        .show(ui, |ui| {
            if compact {
                ui.spacing_mut().item_spacing.y = 4.0;
            }
            let authors = b.authors();
            let (oldest, newest) = b.time_range().unwrap_or((now, now));
            let stats = [
                Stat::new("Lines", format::count(b.lines.len())),
                Stat::new(
                    "Commits",
                    format::count(b.commits.iter().filter(|c| c.lines > 0).count()),
                ),
                Stat::new("Authors", format::count(authors.len())),
                Stat::new("Oldest line", format::relative(oldest, now)),
                Stat::new("Newest line", format::relative(newest, now)),
                Stat::new("Time", format::millis(b.elapsed_ms)),
            ];
            if compact {
                widgets::compact_stats(ui, &stats);
            } else {
                crate::views::insights::stats(ui, &stats);
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(8.0, 4.0);
                ui.label(RichText::new("Age").size(size::SMALL).color(p.text_2));
                let (rect, _) = ui.allocate_exact_size(vec2(140.0, 8.0), Sense::hover());
                let steps = 14;
                for k in 0..steps {
                    let t = k as f32 / (steps - 1) as f32;
                    let r = Rect::from_min_size(
                        pos2(
                            rect.left() + k as f32 * rect.width() / steps as f32,
                            rect.top(),
                        ),
                        vec2(rect.width() / steps as f32 + 0.5, rect.height()),
                    );
                    ui.painter()
                        .rect_filled(r, CornerRadius::ZERO, age_color(&p, t));
                }
                ui.label(
                    RichText::new(format!(
                        "{}  →  {}",
                        format::absolute(oldest),
                        format::absolute(newest)
                    ))
                    .size(size::SMALL)
                    .color(p.text_2),
                );
                if let Some(a) = authors.first() {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("Most lines")
                            .size(size::SMALL)
                            .color(p.text_2),
                    );
                    ui.label(
                        RichText::new(format!("{} ({}%)", a.0, a.1 * 100 / b.lines.len().max(1)))
                            .size(size::SMALL)
                            .family(crate::theme::medium())
                            .color(p.text),
                    );
                }
                if let Some(lang) = &show.language {
                    kit::badge(ui, lang, Tone::Neutral);
                }
                if b.truncated {
                    kit::badge(ui, "First 100,000 lines", Tone::Warning);
                }
            });
            // The hint first, then the exact command in the code font.
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(10.0, 2.0);
                ui.label(
                    RichText::new("Click a commit in the gutter for its menu.")
                        .size(size::SMALL)
                        .color(p.text_2),
                );
                ui.label(kit::mono(&b.command).size(size::MONO - 1.0).color(p.text_3));
            });
        });
    let scroll_to = v.scroll_to.take();
    let mark = v.mark;
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas).inner_margin(Margin {
            left: 16,
            right: 16,
            top: 4,
            bottom: 12,
        }))
        .show(ui, |ui| {
            if b.lines.is_empty() {
                kit::muted(ui, "The file is empty at this revision.");
                return;
            }
            lines(ui, show, graph, mark, scroll_to, now, actions);
        });
}

#[allow(clippy::too_many_arguments)]
fn lines(
    ui: &mut Ui,
    show: &BlameShow,
    graph: Option<&std::collections::HashMap<Oid, usize>>,
    mark: Option<u32>,
    scroll_to: Option<u32>,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let b = &show.blame;
    let font = FontId::monospace(size::MONO);
    let num_font = FontId::monospace(size::CAPTION);
    let digits = b.lines.len().to_string().len() as f32;
    let num_w = 14.0 + digits * 7.0;
    let code_x = GUTTER_W + num_w + 10.0;
    let width = (widgets::code_extent(ui, &font, show.longest) + code_x + 30.0)
        .max(ui.available_width() - 2.0);
    let avail_h = ui.available_height();
    let mut area = egui::ScrollArea::both()
        .id_salt(("hist_blame", &b.path, &b.rev))
        .auto_shrink([false, false]);
    if let Some(l) = scroll_to {
        area = area.vertical_scroll_offset(
            ((l.saturating_sub(1)) as f32 * LINE_H - avail_h * 0.35).max(0.0),
        );
    }
    // Rows edge to edge: see `widgets::rows_ui`.
    ui.spacing_mut().item_spacing.y = 0.0;
    area.show_rows(ui, LINE_H, b.lines.len(), |ui, range| {
        let top = ui.cursor().top();
        let (rect, _) =
            ui.allocate_exact_size(vec2(width, LINE_H * range.len() as f32), Sense::hover());
        let painter = ui.painter_at(rect);
        // The gutter's hairline.
        painter.vline(
            rect.left() + GUTTER_W,
            rect.y_range(),
            egui::Stroke::new(1.0, p.border),
        );
        // The first row in view carries its run's commit too, so the gutter
        // is never blank while scrolling through a long run.
        let clip_top = ui.clip_rect().top();
        let first_in_view = range
            .clone()
            .enumerate()
            .find(|(k, _)| top + (*k as f32 + 0.5) * LINE_H >= clip_top)
            .map(|(_, i)| i)
            .unwrap_or(range.start);
        let labelled = |i: usize| b.starts_group(i) || i == first_in_view;
        for (k, i) in range.clone().enumerate() {
            let line = &b.lines[i];
            let commit = &b.commits[line.commit as usize];
            let y = top + k as f32 * LINE_H;
            let lr = Rect::from_min_size(pos2(rect.left(), y), vec2(width, LINE_H));
            let mid = lr.center().y;
            let starts = b.starts_group(i);
            let first = labelled(i);
            // The line under the commit line carries the summary.
            let summary_row = !first && i > first_in_view && labelled(i - 1);
            if mark == Some(line.line) {
                painter.rect_filled(lr, CornerRadius::ZERO, p.accent.gamma_multiply(0.16));
            }
            if starts && i > 0 {
                painter.hline(
                    lr.x_range(),
                    lr.top(),
                    egui::Stroke::new(1.0, p.border.gamma_multiply(0.6)),
                );
            }
            // Age bar.
            painter.rect_filled(
                Rect::from_min_size(pos2(rect.left(), y), vec2(3.0, LINE_H)),
                CornerRadius::ZERO,
                age_color(&p, b.age(line.commit)),
            );
            let gutter = Rect::from_min_size(pos2(rect.left(), y), vec2(GUTTER_W, LINE_H));
            let resp = ui.interact(
                gutter,
                ui.make_persistent_id(("blame_gutter", i)),
                Sense::click(),
            );
            if resp.hovered() {
                painter.rect_filled(
                    gutter.shrink2(vec2(3.0, 0.0)),
                    CornerRadius::ZERO,
                    p.hover.gamma_multiply(0.7),
                );
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if first {
                let x = rect.left() + 10.0;
                let sha_c = if commit.uncommitted() {
                    p.warning
                } else {
                    p.text_2
                };
                let sha = if commit.uncommitted() {
                    "not committed"
                } else {
                    commit.short()
                };
                let g = painter.layout_no_wrap(
                    sha.to_owned(),
                    FontId::monospace(size::MONO - 0.5),
                    sha_c,
                );
                let sha_w = 84.0;
                painter.galley(pos2(x, mid - g.size().y / 2.0), g, sha_c);
                let date = format::relative(commit.time, now);
                let dg = painter.layout_no_wrap(date, FontId::proportional(size::SMALL), p.text_3);
                let dw = dg.size().x;
                painter.galley(
                    pos2(rect.left() + GUTTER_W - 10.0 - dw, mid - dg.size().y / 2.0),
                    dg,
                    p.text_3,
                );
                let ag = truncated(
                    &painter,
                    &commit.author,
                    FontId::proportional(size::SMALL),
                    p.text,
                    (GUTTER_W - 30.0 - sha_w - dw).max(20.0),
                );
                painter.galley(pos2(x + sha_w, mid - ag.size().y / 2.0), ag, p.text);
            } else if summary_row {
                let sg = truncated(
                    &painter,
                    &commit.summary,
                    FontId::proportional(size::SMALL),
                    p.text_2,
                    GUTTER_W - 20.0,
                );
                painter.galley(
                    pos2(rect.left() + 10.0, mid - sg.size().y / 2.0),
                    sg,
                    p.text_2,
                );
            }
            painter.text(
                pos2(rect.left() + GUTTER_W + num_w, mid),
                Align2::RIGHT_CENTER,
                line.line.to_string(),
                num_font.clone(),
                p.text_3,
            );
            let spans = show
                .spans
                .get(i)
                .filter(|_| show.dark == dark)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let g =
                widgets::code_galley(&painter, &line.text, spans, &font, p.text, p.text_3, dark);
            painter.galley(
                pos2(rect.left() + code_x, mid - g.size().y / 2.0),
                g,
                p.text,
            );

            let oid = show.oids.get(line.commit as usize).copied().flatten();
            let in_graph = oid.is_some_and(|o| graph.is_some_and(|g| g.contains_key(&o)));
            let open = resp.clicked() || resp.secondary_clicked();
            if open {
                actions.push(Act::BlameMark(Some(line.line)).into());
            }
            egui::Popup::menu(&resp)
                .id(egui::Id::unique(("blame_menu", i)))
                .open_memory(open.then_some(egui::SetOpenCommand::Bool(true)))
                .at_pointer_fixed()
                .show(|ui| {
                    use crate::views::menus;
                    menus::begin(ui, 260.0);
                    let p = *pal(ui);
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("{}  ·  {}", commit.short(), commit.summary))
                                .size(size::SMALL)
                                .color(p.text_2),
                        )
                        .truncate(),
                    );
                    ui.add_space(4.0);
                    menus::separator(ui);
                    if commit.uncommitted() {
                        menus::entry(ui, "These lines are not committed yet", false);
                        return;
                    }
                    if let Some(oid) = oid.filter(|_| in_graph) {
                        menus::action(ui, "Show commit", actions, Action::JumpTo(oid));
                    } else {
                        menus::action(
                            ui,
                            "Show commit (files at it)",
                            actions,
                            Act::Browse {
                                rev: commit.sha.clone(),
                                path: Some(commit.filename.clone()),
                            }
                            .into(),
                        );
                    }
                    if commit.previous.is_some() {
                        menus::action(
                            ui,
                            "Blame the previous version",
                            actions,
                            Act::BlamePrevious(i).into(),
                        );
                    }
                    menus::action(
                        ui,
                        "Blame at this commit",
                        actions,
                        Act::Blame {
                            rev: commit.sha.clone(),
                            path: if commit.filename.is_empty() {
                                b.path.clone()
                            } else {
                                commit.filename.clone()
                            },
                            line: Some(line.orig),
                        }
                        .into(),
                    );
                    if let Some((parent, _)) = &commit.previous {
                        menus::action(
                            ui,
                            "Compare with its parent",
                            actions,
                            Act::Compare {
                                a: parent.clone(),
                                b: Some(commit.sha.clone()),
                                select: Some(commit.filename.clone()),
                            }
                            .into(),
                        );
                    }
                    menus::separator(ui);
                    menus::action(ui, "Copy SHA", actions, Action::Copy(commit.sha.clone()));
                });
        }
    });
}
