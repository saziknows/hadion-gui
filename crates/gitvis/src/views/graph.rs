//! The commit graph: virtualised rows with lanes, nodes, ref labels and metadata.

use egui::epaint::CubicBezierShape;
use egui::{
    Align2, Color32, CornerRadius, FontId, Painter, Rect, Sense, Shape, Stroke, StrokeKind, Ui,
    pos2, vec2,
};

use gitvis_core::{Commit, Oid, RefKind, RefLabel, Snapshot};

use crate::devops::history;
use crate::format;
use crate::state::{Action, Dialog, DragBranch, DropTarget, RepoState, Selection, Settings};
use crate::theme;
use crate::views::icons::{Icon, IconButton};
use crate::views::kit;
use crate::views::menus::{self, MenuCtx};
use crate::views::{pill, pill_width, truncated};

pub const ROW_H: f32 = 30.0;
const LANE_W: f32 = 16.0;
const GRAPH_PAD: f32 = 10.0;
const MAX_LANES: u16 = 18;
const NODE_R: f32 = 4.5;
const LINE_W: f32 = 2.0;
const AUTHOR_W: f32 = 150.0;
const DATE_W: f32 = 82.0;
const SHA_W: f32 = 64.0;

pub fn background(dark: bool) -> Color32 {
    theme::palette(dark).canvas
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, settings: &Settings, actions: &mut Vec<Action>) {
    // Author / path / date / message filter (dims the other rows).
    history::filter_bar(ui, repo, actions);
    // Escape clears the commits picked with Ctrl / Shift+click.
    if repo.devops.history.has_picks()
        && ui.memory(|m| m.focused().is_none())
        && ui.input(|i| i.key_pressed(egui::Key::Escape))
    {
        actions.push(history::Act::ClearPicks.into());
    }
    let scroll_to = repo.scroll_to_row.take();
    let selection = repo.selection;
    let mut visible = repo.visible_rows.clone();
    let hits = std::mem::take(&mut repo.search_hits);
    let cursor_row = hits.get(repo.search_cursor).copied();

    {
        let Some(snap) = repo.snapshot.as_ref() else {
            if repo.loading {
                ui.add_space(ui.available_height() * 0.4);
                ui.vertical_centered(|ui| kit::loading(ui, "Reading the repository…"));
            } else if let Some(e) = repo.load_error.as_deref() {
                egui::Frame::NONE
                    .inner_margin(egui::Margin::same(24))
                    .show(ui, |ui| {
                        kit::callout_with(
                            ui,
                            kit::Tone::Danger,
                            "Could not read the repository",
                            e,
                            |ui| {
                                ui.add_space(4.0);
                                if IconButton::new(Icon::Rerun, "Try again")
                                    .compact()
                                    .show(ui)
                                    .clicked()
                                {
                                    actions.push(Action::Reload(true));
                                }
                            },
                        );
                    });
            }
            repo.search_hits = hits;
            return;
        };
        let n = snap.row_count();
        if n == 0 {
            crate::devops::history::empty_state(
                ui,
                Icon::Graph,
                "No commits yet",
                "Stage files in the panel on the right and make the first commit.",
                None,
            );
            repo.search_hits = hits;
            return;
        }

        let show_author = settings.show_author && ui.available_width() > 760.0;
        // The short SHA is also in the details panel: a narrow graph gives
        // its room to the summary.
        let show_sha = ui.available_width() > 520.0;
        let hook = crate::devops::hooks::HookCx {
            snap,
            devops: &repo.devops,
            settings,
            github_data: repo.github.as_ref(),
            busy: repo.busy.is_some(),
            commit_message: &repo.commit_message,
            details: &repo.details,
        };
        let mut cx = RowCtx {
            snap,
            filter_cx: MenuCtx {
                snap,
                filter: &repo.filter,
                github: repo.github.as_ref(),
                hook: Some(&hook),
            },
            ci: &repo.ci,
            t: ui.input(|i| i.time),
            dark: settings.dark,
            now: format::now_secs(),
            relative_dates: settings.relative_dates,
            graph_w: 0.0,
            show_author,
            show_sha,
            head: snap.head.target(),
            head_branch: snap.current_branch().map(|b| b.name.clone()),
            selection,
            hits: &hits,
            cursor_row,
            hist: &repo.devops.history,
        };

        let avail_h = ui.available_height();
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut area = egui::ScrollArea::vertical()
            .id_salt("graph")
            .auto_shrink([false, false]);
        if let Some(row) = scroll_to {
            let off = (row as f32 * ROW_H - avail_h * 0.5 + ROW_H * 0.5).max(0.0);
            area = area.vertical_scroll_offset(off);
        }
        area.show_rows(ui, ROW_H, n, |ui, range| {
            visible = range.clone();
            // Only the lanes used by the visible rows need room; wide
            // histories stay readable instead of pushing the text away.
            let used = widest_lane(snap, range.clone());
            let lanes = (used + 1).clamp(1, MAX_LANES) as f32;
            let target = GRAPH_PAD * 2.0 + lanes * LANE_W;
            cx.graph_w =
                ui.ctx()
                    .animate_value_with_time(ui.make_persistent_id("graph_w"), target, 0.15);
            for row in range {
                draw_row(ui, &cx, row, actions);
            }
        });
    }
    // Signatures follow the visible rows (verified lazily, in batches).
    if *repo.devops.history.verified_rows() != visible {
        actions.push(history::Act::VisibleRows(visible.clone()).into());
    }
    repo.visible_rows = visible;
    repo.search_hits = hits;
}

fn widest_lane(snap: &Snapshot, rows: std::ops::Range<usize>) -> u16 {
    let mut max = 0;
    for row in rows {
        let Some(r) = snap.graph.rows.get(row) else {
            break;
        };
        max = max.max(r.col);
        for s in r.top.iter().chain(&r.bottom) {
            max = max.max(s.from).max(s.to);
        }
    }
    max
}

struct RowCtx<'a> {
    snap: &'a Snapshot,
    filter_cx: MenuCtx<'a>,
    dark: bool,
    now: i64,
    relative_dates: bool,
    graph_w: f32,
    show_author: bool,
    show_sha: bool,
    head: Option<Oid>,
    head_branch: Option<String>,
    selection: Selection,
    hits: &'a [usize],
    cursor_row: Option<usize>,
    ci: &'a std::collections::HashMap<Oid, crate::state::CiInfo>,
    /// Animation clock for running CI dots.
    t: f64,
    /// Picked commits, the filter and signatures.
    hist: &'a history::State,
}

impl RowCtx<'_> {
    fn lane(&self, col: u16) -> Color32 {
        theme::lane_color(col, self.dark)
    }
}

fn draw_row(ui: &mut Ui, cx: &RowCtx, row: usize, actions: &mut Vec<Action>) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let snap = cx.snap;
    let commit_idx = snap.commit_of_row(row);
    let commit: Option<&Commit> = commit_idx.and_then(|i| snap.commits.get(i));
    let is_wip = commit_idx.is_none();
    let is_selected = match (commit, cx.selection) {
        (None, Selection::Wip) => true,
        (Some(c), Selection::Commit(id)) => c.id == id,
        _ => false,
    };
    let is_hit = !cx.hits.is_empty() && cx.hits.binary_search(&row).is_ok();
    let is_picked = commit.is_some_and(|c| cx.hist.is_picked(&c.id));
    let dimmed = commit_idx.is_some_and(|i| cx.hist.dimmed(i));
    let visuals = ui.visuals().clone();
    let pl = *theme::palette(cx.dark);
    let painter = ui.painter().clone();

    // Background
    if is_selected || is_picked {
        painter.rect_filled(rect, 0, pl.selection);
    } else if resp.hovered() {
        painter.rect_filled(rect, 0, pl.hover.gamma_multiply(0.7));
    }
    if is_picked {
        let bar = Rect::from_min_max(rect.min, pos2(rect.left() + 3.0, rect.bottom()));
        painter.rect_filled(bar, 0, pl.accent);
    }
    if is_hit {
        let bar = Rect::from_min_max(rect.min, pos2(rect.left() + 3.0, rect.bottom()));
        let c = if cx.cursor_row == Some(row) {
            theme::AMBER
        } else {
            theme::AMBER.gamma_multiply(0.5)
        };
        painter.rect_filled(bar, 0, c);
        if !is_selected {
            painter.rect_filled(rect, 0, theme::AMBER.gamma_multiply(0.07));
        }
    }
    // Drop target highlight
    if resp.dnd_hover_payload::<DragBranch>().is_some() {
        painter.rect_stroke(
            rect.shrink(1.0),
            CornerRadius::same(4),
            Stroke::new(1.5, theme::ACCENT),
            StrokeKind::Inside,
        );
    }

    // Graph
    let grow = &snap.graph.rows[row];
    let x_of = |c: u16| rect.left() + GRAPH_PAD + c as f32 * LANE_W + LANE_W / 2.0;
    let (top, mid, bottom) = (rect.top(), rect.center().y, rect.bottom());
    let graph_clip = Rect::from_min_max(rect.min, pos2(rect.left() + cx.graph_w, rect.bottom()))
        .intersect(painter.clip_rect());
    let gp = painter.with_clip_rect(graph_clip);
    for s in &grow.top {
        draw_segment(
            &gp,
            x_of(s.from),
            top,
            x_of(s.to),
            mid,
            cx.lane(s.color),
            s.dashed,
        );
    }
    for s in &grow.bottom {
        draw_segment(
            &gp,
            x_of(s.from),
            mid,
            x_of(s.to),
            bottom,
            cx.lane(s.color),
            s.dashed,
        );
    }
    let center = pos2(x_of(grow.col), mid);
    let node_color = cx.lane(grow.col);
    let bg = background(cx.dark);
    match commit {
        None => {
            gp.circle_filled(center, NODE_R, bg);
            gp.circle_stroke(center, NODE_R, Stroke::new(1.5, pl.text_3));
        }
        Some(c) => {
            if cx.head == Some(c.id) {
                gp.circle_filled(center, NODE_R + 3.0, node_color.gamma_multiply(0.3));
                gp.circle_filled(center, NODE_R + 0.5, node_color);
                gp.circle_filled(center, 1.8, bg);
            } else if c.parent_ids.len() > 1 {
                gp.circle_filled(center, NODE_R, bg);
                gp.circle_stroke(center, NODE_R - 0.75, Stroke::new(2.0, node_color));
            } else {
                gp.circle_filled(center, NODE_R, node_color);
            }
        }
    }

    // Columns on the right
    let mut right = rect.right() - 10.0;
    let sha_x = if cx.show_sha { right - SHA_W } else { right };
    right = sha_x - 8.0;
    let date_x = right - DATE_W;
    right = date_x - 8.0;
    let author_x = if cx.show_author {
        right - AUTHOR_W
    } else {
        right
    };
    if cx.show_author {
        right = author_x - 10.0;
    }
    let text_right = right;

    // Labels + summary
    let mut x = rect.left() + cx.graph_w + 4.0;
    let text_font = FontId::proportional(theme::size::BODY);
    match commit {
        None => {
            let st = &snap.status;
            let text = format!(
                "Uncommitted changes · {}{}",
                format::plural(st.change_count(), "file", "files"),
                if st.conflicted.is_empty() {
                    String::new()
                } else {
                    format!(" · {} conflicted", st.conflicted.len())
                }
            );
            let galley = truncated(
                &painter,
                &text,
                FontId::new(theme::size::BODY, theme::medium()),
                pl.text_2,
                text_right - x,
            );
            painter.galley(pos2(x, mid - galley.size().y / 2.0), galley, pl.text_2);
        }
        Some(c) => {
            // Signature badge at the right end of the summary column.
            let mut text_right = text_right;
            if let Some(sig) = cx.hist.signature(&c.id)
                && let Some(hit) =
                    history::paint_signature(&painter, &pl, pos2(text_right - 7.0, mid), sig)
            {
                text_right -= 22.0;
                let r = ui.interact(hit, ui.make_persistent_id(("sig", row)), Sense::click());
                if r.clicked() {
                    actions.push(Action::Select(Selection::Commit(c.id)));
                }
                r.on_hover_text(history::signature_tip(sig));
            }
            if let Some(labels) = snap.refs.get(&c.id) {
                x = draw_labels(ui, cx, row, rect, x, text_right, labels, c, actions);
            }
            let color = if cx.head == Some(c.id) || is_selected {
                pl.text_strong
            } else {
                pl.text
            };
            let font = if cx.head == Some(c.id) {
                FontId::new(theme::size::BODY, theme::medium())
            } else {
                text_font.clone()
            };
            let galley = truncated(&painter, &c.summary, font, color, text_right - x);
            painter.galley(pos2(x, mid - galley.size().y / 2.0), galley, color);

            // Author
            if cx.show_author {
                crate::views::avatar(&painter, pos2(author_x + 9.0, mid), 8.0, &c.author, cx.dark);
                let galley = truncated(
                    &painter,
                    &c.author,
                    FontId::proportional(theme::size::SMALL),
                    pl.text_2,
                    AUTHOR_W - 26.0,
                );
                painter.galley(
                    pos2(author_x + 24.0, mid - galley.size().y / 2.0),
                    galley,
                    pl.text_2,
                );
            }
            // Date
            let date = if cx.relative_dates {
                format::relative(c.time, cx.now)
            } else {
                format::absolute(c.time)
            };
            let galley = truncated(
                &painter,
                &date,
                FontId::proportional(theme::size::SMALL),
                pl.text_3,
                DATE_W,
            );
            painter.galley(pos2(date_x, mid - galley.size().y / 2.0), galley, pl.text_3);
            // CI status from GitHub, if known for this commit: hover for the
            // summary, click to open the run.
            if let Some(ci) = cx.ci.get(&c.id) {
                let center = pos2(sha_x - 10.0, mid);
                crate::views::icons::outcome_dot(&painter, ci.outcome, center, 4.5, cx.t);
                if ci.outcome.is_running() {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(90));
                }
                let hit = Rect::from_center_size(center, vec2(16.0, ROW_H));
                let dot = ui.interact(hit, ui.make_persistent_id(("ci", row)), Sense::click());
                if dot.hovered() {
                    painter.circle_stroke(
                        center,
                        6.5,
                        Stroke::new(1.0, visuals.strong_text_color().gamma_multiply(0.5)),
                    );
                }
                if dot.clicked() {
                    match ci.run {
                        Some(run) => actions.push(Action::OpenRun { run, job: None }),
                        None => actions.push(Action::Select(Selection::Commit(c.id))),
                    }
                }
                dot.on_hover_text(format!(
                    "{} · {}\n\nClick to open the workflow run",
                    ci.outcome.label(),
                    ci.summary
                ));
            }
            // SHA
            if cx.show_sha {
                painter.text(
                    pos2(sha_x, mid),
                    Align2::LEFT_CENTER,
                    &c.short,
                    FontId::monospace(theme::size::MONO - 0.5),
                    pl.text_3,
                );
            }
        }
    }

    // Rows outside the filter are dimmed.
    if dimmed {
        painter.rect_filled(rect, 0, background(cx.dark).gamma_multiply(0.62));
    }

    // Interaction: Ctrl+click picks a commit, Shift+click a range.
    let mods = ui.input(|i| i.modifiers);
    match commit {
        Some(c) if resp.clicked() && mods.command => {
            actions.push(history::Act::Pick(history::PickEvent::Toggle(c.id)).into());
        }
        Some(c) if resp.clicked() && mods.shift => {
            actions.push(history::Act::Pick(history::PickEvent::Range(c.id)).into());
        }
        _ if resp.clicked() || resp.secondary_clicked() => {
            if resp.clicked() && cx.hist.has_picks() {
                actions.push(history::Act::ClearPicks.into());
            }
            actions.push(Action::Select(match commit {
                None => Selection::Wip,
                Some(c) => Selection::Commit(c.id),
            }));
        }
        _ => {}
    }
    if resp.double_clicked()
        && let Some(c) = commit
    {
        let locals: Vec<&RefLabel> = snap
            .refs
            .get(&c.id)
            .map(|v| {
                v.iter()
                    .filter(|l| l.kind == RefKind::Local && !l.is_head)
                    .collect()
            })
            .unwrap_or_default();
        if locals.len() == 1 {
            actions.push(Action::Run(gitvis_core::ops::GitOp::Switch {
                branch: locals[0].name.clone(),
            }));
        }
    }
    if let Some(payload) = resp.dnd_release_payload::<DragBranch>() {
        let target_commit = match commit {
            Some(c) => Some(c.id),
            None => cx.head,
        };
        if let Some(target_commit) = target_commit {
            let local_branches: Vec<String> = snap
                .refs
                .get(&target_commit)
                .map(|v| {
                    v.iter()
                        .filter(|l| l.kind == RefKind::Local)
                        .map(|l| l.name.clone())
                        .collect()
                })
                .unwrap_or_default();
            actions.push(Action::Dialog(Dialog::Drop {
                source: (*payload).clone(),
                target: DropTarget {
                    commit: target_commit,
                    short: gitvis_core::repo::short_id(target_commit),
                    local_branches,
                    is_head: cx.head == Some(target_commit),
                },
                head_branch: cx.head_branch.clone(),
            }));
        }
    }
    match commit {
        Some(c) => {
            resp.context_menu(|ui| menus::commit(ui, &cx.filter_cx, c, actions));
        }
        None => {
            resp.context_menu(|ui| menus::wip(ui, snap, actions));
        }
    }
    if is_wip {
        resp.on_hover_text("Working tree changes. Select to stage, unstage and commit.");
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_labels(
    ui: &mut Ui,
    cx: &RowCtx,
    row: usize,
    rect: Rect,
    mut x: f32,
    text_right: f32,
    labels: &[RefLabel],
    commit: &Commit,
    actions: &mut Vec<Action>,
) -> f32 {
    let painter = ui.painter().clone();
    let mid = rect.center().y;
    let lane = cx.lane(cx.snap.graph.rows[row].col);
    // Leave at least 40% of the text area for the summary.
    let max_x = x + (text_right - x) * 0.6;
    let weak = ui.visuals().weak_text_color();
    let dark = cx.dark;

    for (k, label) in labels.iter().enumerate() {
        let remaining = labels.len() - k;
        let w = pill_width(&painter, &label.name);
        let more_w = if remaining > 1 {
            pill_width(&painter, &format!("+{}", remaining - 1)) + 4.0
        } else {
            0.0
        };
        if k > 0 && x + w + more_w > max_x {
            let rest: Vec<&str> = labels[k..].iter().map(|l| l.name.as_str()).collect();
            let r = pill(
                &painter,
                pos2(x, mid - 9.0),
                &format!("+{}", remaining),
                weak.gamma_multiply(0.15),
                weak.gamma_multiply(0.5),
                weak,
                f32::INFINITY,
            );
            let resp = ui.interact(r, ui.make_persistent_id(("more", row)), Sense::hover());
            resp.on_hover_text(rest.join("\n"));
            return r.right() + 8.0;
        }
        // The first pill always shows, cut to fit when the name is very long.
        let pill_max = if k == 0 {
            (max_x - x).max(60.0)
        } else {
            f32::INFINITY
        };
        let (fill, stroke, text) = match label.kind {
            RefKind::Local if label.is_head => (theme::ACCENT, theme::ACCENT, Color32::WHITE),
            RefKind::Local => (
                lane.gamma_multiply(if dark { 0.2 } else { 0.16 }),
                lane.gamma_multiply(0.9),
                if dark {
                    lane.lerp_to_gamma(Color32::WHITE, 0.25)
                } else {
                    lane.lerp_to_gamma(Color32::BLACK, 0.35)
                },
            ),
            RefKind::Remote => (
                weak.gamma_multiply(0.14),
                weak.gamma_multiply(0.5),
                if dark {
                    Color32::from_gray(190)
                } else {
                    Color32::from_gray(90)
                },
            ),
            RefKind::Tag => (
                theme::AMBER.gamma_multiply(0.15),
                theme::AMBER.gamma_multiply(0.8),
                if dark {
                    theme::AMBER
                } else {
                    theme::AMBER.lerp_to_gamma(Color32::BLACK, 0.35)
                },
            ),
        };
        let r = pill(
            &painter,
            pos2(x, mid - 9.0),
            &label.name,
            fill,
            stroke,
            text,
            pill_max,
        );
        let id = ui.make_persistent_id(("pill", row, k));
        let resp = ui.interact(r, id, Sense::click_and_drag());
        if resp.hovered() {
            painter.rect_stroke(
                r,
                CornerRadius::same(9),
                Stroke::new(1.0, ui.visuals().strong_text_color().gamma_multiply(0.6)),
                StrokeKind::Inside,
            );
        }
        if resp.clicked() {
            actions.push(Action::Select(Selection::Commit(commit.id)));
        }
        if resp.double_clicked()
            && let Some(a) = menus::checkout_action(cx.snap, label)
        {
            actions.push(a);
        }
        if label.kind != RefKind::Tag {
            resp.dnd_set_drag_payload(DragBranch {
                name: label.name.clone(),
                is_remote: label.kind == RefKind::Remote,
                target: commit.id,
            });
        }
        match label.kind {
            RefKind::Local => {
                if let Some(b) = cx.snap.branch(&label.name) {
                    resp.context_menu(|ui| menus::local_branch(ui, &cx.filter_cx, b, actions));
                }
            }
            RefKind::Remote => {
                if let Some(rb) = cx.snap.remote_branch(&label.name) {
                    resp.context_menu(|ui| menus::remote_branch(ui, &cx.filter_cx, rb, actions));
                }
            }
            RefKind::Tag => {
                if let Some(t) = cx.snap.tags.iter().find(|t| t.name == label.name) {
                    resp.context_menu(|ui| menus::tag(ui, &cx.filter_cx, t, actions));
                }
            }
        }
        let hint = match label.kind {
            RefKind::Local if label.is_head => format!(
                "{} (checked out)\n\nDrag onto a commit to merge or rebase",
                label.name
            ),
            RefKind::Local => format!(
                "{}\n\nDouble-click to check out · drag onto a commit to merge or rebase",
                label.name
            ),
            RefKind::Remote => format!(
                "{}\n\nDouble-click to check out · drag onto a commit to merge",
                label.name
            ),
            RefKind::Tag => format!("tag {}", label.name),
        };
        resp.on_hover_text(hint);
        x = r.right() + 4.0;
    }
    x + 4.0
}

fn draw_segment(p: &Painter, x0: f32, y0: f32, x1: f32, y1: f32, color: Color32, dashed: bool) {
    let stroke = Stroke::new(LINE_W, color);
    if (x0 - x1).abs() < 0.5 {
        if dashed {
            p.extend(Shape::dashed_line(
                &[pos2(x0, y0), pos2(x1, y1)],
                stroke,
                3.0,
                3.0,
            ));
        } else {
            p.line_segment([pos2(x0, y0), pos2(x1, y1)], stroke);
        }
        return;
    }
    let k = (y1 - y0) * 0.9;
    let bez = CubicBezierShape::from_points_stroke(
        [
            pos2(x0, y0),
            pos2(x0, y0 + k),
            pos2(x1, y1 - k),
            pos2(x1, y1),
        ],
        false,
        Color32::TRANSPARENT,
        stroke,
    );
    if dashed {
        let pts = bez.flatten(Some(0.3));
        p.extend(Shape::dashed_line(&pts, stroke, 3.0, 3.0));
    } else {
        p.add(bez);
    }
}

/// The floating label that follows the pointer while a branch is dragged.
pub fn drag_overlay(ctx: &egui::Context) {
    let Some(payload) = egui::DragAndDrop::payload::<DragBranch>(ctx) else {
        return;
    };
    let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) else {
        return;
    };
    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::unique("drag_ghost"),
    ));
    pill(
        &painter,
        pos + vec2(12.0, -9.0),
        &payload.name,
        theme::ACCENT,
        theme::ACCENT,
        Color32::WHITE,
        f32::INFINITY,
    );
}
