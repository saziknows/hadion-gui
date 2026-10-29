//! The refs sidebar of the History page: local branches, GitHub pull
//! requests and runs, remotes, tags and stashes.
//!
//! Rows are painted (fixed height, one indent step per folder level) and
//! their hover buttons are separate interactions placed on top, with the
//! name cut to the space left of them.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Color32, CornerRadius, FontId, Frame, Galley, Id, Margin, Painter, Pos2, Rect, Response, Sense,
    Stroke, StrokeKind, Ui, pos2, vec2,
};

use hadion_core::{BranchInfo, RemoteBranch, Snapshot, StashInfo, TagInfo};

use crate::format;
use crate::state::{Action, Dialog, DragBranch, Page, RepoState, Selection, Settings};
use crate::theme::{self, Palette, Tone, pal, radius, size};
use crate::views::icons::{self, Icon};
use crate::views::kit;
use crate::views::menus::{self, MenuCtx};
use crate::views::truncated;

/// Height of every row.
const ROW_H: f32 = 26.0;
/// One folder level.
const INDENT: f32 = 14.0;
/// Centre of a depth-0 row's icon, from the row's left edge.
const ICON_X: f32 = 20.0;
/// Start of a depth-0 row's text.
const TEXT_X: f32 = 32.0;
/// Right padding of rows.
const PAD_R: f32 = 10.0;
/// Hover button size and the space it reserves.
const BTN: f32 = 20.0;
const BTN_SLOT: f32 = 24.0;

pub fn show(ui: &mut Ui, repo: &mut RepoState, settings: &Settings, actions: &mut Vec<Action>) {
    Frame::NONE
        .inner_margin(Margin {
            left: 8,
            right: 8,
            top: 10,
            bottom: 4,
        })
        .show(ui, |ui| {
            kit::search_box(
                ui,
                "sidebar_filter",
                &mut repo.sidebar_filter,
                "Filter refs",
            );
        });

    let filter = repo.sidebar_filter.trim().to_lowercase();
    let RepoState {
        snapshot,
        collapsed,
        filter: graph_filter,
        selection,
        github,
        github_remote,
        github_error,
        github_loading,
        devops,
        busy,
        commit_message,
        details,
        ..
    } = repo;
    let Some(snap) = snapshot.as_ref() else {
        Frame::NONE
            .inner_margin(Margin::symmetric(14, 10))
            .show(ui, |ui| kit::loading(ui, "Loading…"));
        return;
    };
    let hook = crate::devops::hooks::HookCx {
        snap,
        devops: &*devops,
        settings,
        github_data: github.as_ref(),
        busy: busy.is_some(),
        commit_message: commit_message.as_str(),
        details: &*details,
    };
    let cx = Ctx {
        snap,
        menu: MenuCtx {
            snap,
            filter: graph_filter,
            github: github.as_ref(),
            hook: Some(&hook),
        },
        filter: &filter,
        dark: settings.dark,
        selection: *selection,
        p: *pal(ui),
    };

    egui::ScrollArea::vertical()
        .id_salt("sidebar")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;

            // Local branches
            let local_count = count_matching(&cx, snap.branches.iter().map(|b| b.name.as_str()));
            let head = section_header(
                ui,
                &cx,
                "Local",
                local_count,
                collapsed,
                "sec:local",
                true,
                &[HeaderButton::new(Glyph::Icon(Icon::Plus), "Create branch")],
            );
            if head.clicked == Some(0) {
                let (start, label) = match snap.current_branch() {
                    Some(b) => (b.name.clone(), b.name.clone()),
                    None => (
                        snap.head
                            .target()
                            .map(|h| h.to_string())
                            .unwrap_or_default(),
                        "HEAD".to_owned(),
                    ),
                };
                actions.push(Action::Dialog(Dialog::CreateBranch {
                    name: String::new(),
                    start,
                    start_label: label,
                    checkout: true,
                }));
            }
            if head.open {
                let tree = Tree::build(snap.branches.iter().filter(|b| cx.matches(&b.name)), |b| {
                    &b.name
                });
                tree.show(
                    ui,
                    0,
                    "local",
                    collapsed,
                    &cx,
                    &mut |ui, cx, b, rect, resp, depth| {
                        local_row(ui, cx, b, rect, resp, depth, actions)
                    },
                );
                if snap.branches.is_empty() {
                    empty_hint(ui, &cx, 0, "No branches yet");
                } else if local_count == 0 {
                    empty_hint(ui, &cx, 0, "No match");
                }
            }

            // GitHub: pull requests and workflow runs (above the often huge remote list)
            if github_remote.is_some() {
                github_section(
                    ui,
                    &cx,
                    github.as_ref(),
                    github_error.as_deref(),
                    *github_loading,
                    collapsed,
                    format::now_secs(),
                    actions,
                );
            }

            // Remotes
            let remote_total: usize = snap
                .remotes
                .iter()
                .map(|r| count_matching(&cx, r.branches.iter().map(|b| b.name.as_str())))
                .sum();
            let remotes_hidden = cx.menu.filter.hide_remotes;
            let head = section_header(
                ui,
                &cx,
                "Remote",
                remote_total,
                collapsed,
                "sec:remote",
                false,
                &[HeaderButton::new(
                    Glyph::Eye {
                        open: !remotes_hidden,
                    },
                    if remotes_hidden {
                        "Show remote branches in the graph"
                    } else {
                        "Hide remote branches in the graph"
                    },
                )
                .always(remotes_hidden)],
            );
            if head.clicked == Some(0) {
                actions.push(Action::ToggleRemotes);
            }
            if head.open {
                for remote in &snap.remotes {
                    let key = format!("remote:{}", remote.name);
                    let n = count_matching(&cx, remote.branches.iter().map(|b| b.name.as_str()));
                    if cx.filtering() && n == 0 {
                        continue;
                    }
                    let sub_open =
                        sub_header(ui, &cx, &remote.name, &remote.url, n, collapsed, &key);
                    if sub_open {
                        let tree = Tree::build(
                            remote.branches.iter().filter(|b| cx.matches(&b.name)),
                            |b| &b.branch,
                        );
                        tree.show(
                            ui,
                            1,
                            &key,
                            collapsed,
                            &cx,
                            &mut |ui, cx, rb, rect, resp, depth| {
                                remote_row(ui, cx, rb, rect, resp, depth, actions)
                            },
                        );
                        if remote.branches.is_empty() {
                            empty_hint(ui, &cx, 1, "No branches fetched");
                        }
                    }
                }
                if snap.remotes.is_empty() {
                    empty_hint(ui, &cx, 0, "No remotes");
                } else if cx.filtering() && remote_total == 0 {
                    empty_hint(ui, &cx, 0, "No match");
                }
            }

            // Tags
            let tags_hidden = cx.menu.filter.hide_tags;
            let tag_count = count_matching(&cx, snap.tags.iter().map(|t| t.name.as_str()));
            let head = section_header(
                ui,
                &cx,
                "Tags",
                tag_count,
                collapsed,
                "sec:tags",
                false,
                &[HeaderButton::new(
                    Glyph::Eye { open: !tags_hidden },
                    if tags_hidden {
                        "Show tags in the graph"
                    } else {
                        "Hide tags in the graph"
                    },
                )
                .always(tags_hidden)],
            );
            if head.clicked == Some(0) {
                actions.push(Action::ToggleTags);
            }
            if head.open {
                for t in snap.tags.iter().filter(|t| cx.matches(&t.name)) {
                    let (rect, resp) = row_alloc(ui, Sense::click());
                    if ui.is_rect_visible(rect) {
                        tag_row(ui, &cx, t, rect, resp, actions);
                    }
                }
                if snap.tags.is_empty() {
                    empty_hint(ui, &cx, 0, "No tags");
                } else if tag_count == 0 {
                    empty_hint(ui, &cx, 0, "No match");
                }
            }

            // Stashes
            let stash_count = count_matching(&cx, snap.stashes.iter().map(|s| s.message.as_str()));
            let head = section_header(
                ui,
                &cx,
                "Stashes",
                stash_count,
                collapsed,
                "sec:stash",
                false,
                &[],
            );
            if head.open {
                for s in snap.stashes.iter().filter(|s| cx.matches(&s.message)) {
                    let (rect, resp) = row_alloc(ui, Sense::click());
                    if ui.is_rect_visible(rect) {
                        stash_row(ui, &cx, s, rect, resp, actions);
                    }
                }
                if snap.stashes.is_empty() {
                    empty_hint(ui, &cx, 0, "No stashes");
                } else if stash_count == 0 {
                    empty_hint(ui, &cx, 0, "No match");
                }
            }
            ui.add_space(16.0);
        });
}

struct Ctx<'a> {
    snap: &'a Snapshot,
    menu: MenuCtx<'a>,
    filter: &'a str,
    dark: bool,
    selection: Selection,
    p: Palette,
}

impl Ctx<'_> {
    fn matches(&self, name: &str) -> bool {
        self.filter.is_empty() || name.to_lowercase().contains(self.filter)
    }

    fn filtering(&self) -> bool {
        !self.filter.is_empty()
    }

    fn lane_color_of(&self, target: hadion_core::Oid) -> Color32 {
        let Some(&i) = self.snap.index_of.get(&target) else {
            return self.p.text_3;
        };
        let row = self.snap.row_of_commit(i);
        theme::lane_color(self.snap.graph.rows[row].col, self.dark)
    }

    fn is_selected(&self, target: hadion_core::Oid) -> bool {
        self.selection == Selection::Commit(target)
    }
}

fn count_matching<'a>(cx: &Ctx, names: impl Iterator<Item = &'a str>) -> usize {
    names.filter(|n| cx.matches(n)).count()
}

/// Whether the pointer is over a row, its hover buttons included (unlike
/// `hovered`, which turns false over a button). Rows share their edges, so
/// the bottom edge belongs to the next row only.
fn row_hovered(ui: &Ui, resp: &Response) -> bool {
    resp.contains_pointer()
        && ui
            .ctx()
            .pointer_hover_pos()
            .is_some_and(|p| p.y < resp.rect.bottom())
}

fn row_alloc(ui: &mut Ui, sense: Sense) -> (Rect, Response) {
    ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), sense)
}

fn icon_x(rect: Rect, depth: usize) -> f32 {
    rect.left() + ICON_X + depth as f32 * INDENT
}

fn text_x(rect: Rect, depth: usize) -> f32 {
    rect.left() + TEXT_X + depth as f32 * INDENT
}

fn empty_hint(ui: &mut Ui, cx: &Ctx, depth: usize, text: &str) {
    let (rect, _) = row_alloc(ui, Sense::hover());
    paint_text(
        ui.painter(),
        text_x(rect, depth),
        rect.right() - PAD_R,
        rect.center().y,
        text,
        FontId::proportional(size::SMALL),
        cx.p.text_3,
    );
}

fn chevron(painter: &Painter, center: Pos2, open: bool, color: Color32) {
    let s = 3.5;
    let pts = if open {
        vec![
            pos2(center.x - s, center.y - s * 0.5),
            pos2(center.x, center.y + s * 0.5),
            pos2(center.x + s, center.y - s * 0.5),
        ]
    } else {
        vec![
            pos2(center.x - s * 0.5, center.y - s),
            pos2(center.x + s * 0.5, center.y),
            pos2(center.x - s * 0.5, center.y + s),
        ]
    };
    painter.line(pts, Stroke::new(1.5, color));
}

/// Uppercase caption laid out with the kit's caption style.
fn caption_galley(painter: &Painter, text: &str, color: Color32) -> Arc<Galley> {
    let mut job = LayoutJob::default();
    job.append(
        &text.to_uppercase(),
        0.0,
        TextFormat {
            font_id: FontId::new(size::CAPTION, theme::medium()),
            color,
            extra_letter_spacing: 0.7,
            ..Default::default()
        },
    );
    painter.layout_job(job)
}

/// Text cut to `right`; returns whether it had to be cut.
fn paint_text(
    painter: &Painter,
    x: f32,
    right: f32,
    y: f32,
    text: &str,
    font: FontId,
    color: Color32,
) -> bool {
    let g = truncated(painter, text, font, color, (right - x).max(12.0));
    let elided = g.elided;
    painter.galley(pos2(x, y - g.size().y / 2.0), g, color);
    elided
}

fn body_font() -> FontId {
    FontId::proportional(size::BODY)
}

/// What a hover button draws.
#[derive(Clone, Copy)]
enum Glyph {
    Icon(Icon),
    Eye { open: bool },
    Spinner,
}

fn paint_eye(painter: &Painter, c: Pos2, open: bool, color: Color32) {
    let clip = Rect::from_center_size(c, vec2(14.0, 8.0)).intersect(painter.clip_rect());
    let st = Stroke::new(1.3, color);
    let lens = painter.with_clip_rect(clip);
    lens.circle_stroke(pos2(c.x, c.y + 2.5), 6.5, st);
    lens.circle_stroke(pos2(c.x, c.y - 2.5), 6.5, st);
    if open {
        painter.circle_filled(c, 2.0, color);
    } else {
        painter.line_segment([pos2(c.x - 6.0, c.y + 5.0), pos2(c.x + 6.0, c.y - 5.0)], st);
    }
}

/// A small ghost button painted on top of a row; it does not take part in
/// the layout, so rows keep their height whether it shows or not.
fn hover_button(ui: &Ui, rect: Rect, id: Id, glyph: Glyph, tip: &str) -> Response {
    let p = *pal(ui);
    if let Glyph::Spinner = glyph {
        egui::Spinner::new()
            .size(12.0)
            .color(p.text_3)
            .paint_at(ui, Rect::from_center_size(rect.center(), vec2(12.0, 12.0)));
        return ui.interact(rect, id, Sense::hover());
    }
    let resp = ui.interact(rect, id, Sense::click());
    let hovered = resp.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(radius::S), p.hover);
    }
    let color = if hovered { p.text_strong } else { p.text_2 };
    match glyph {
        Glyph::Icon(icon) => icons::paint(ui.painter(), icon, rect.center(), 13.0, color),
        Glyph::Eye { open } => paint_eye(ui.painter(), rect.center(), open, color),
        Glyph::Spinner => {}
    }
    kit::tip(resp, tip).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The right-most hover button slot of a row; returns its rect.
fn button_slot(rect: Rect, right: f32) -> Rect {
    Rect::from_center_size(pos2(right - BTN / 2.0, rect.center().y), vec2(BTN, BTN))
}

struct HeaderButton<'a> {
    glyph: Glyph,
    tip: &'a str,
    /// Shown without hovering the header (a non-default state, a spinner).
    always: bool,
}

impl<'a> HeaderButton<'a> {
    fn new(glyph: Glyph, tip: &'a str) -> Self {
        Self {
            glyph,
            tip,
            always: false,
        }
    }

    fn always(mut self, always: bool) -> Self {
        self.always = always;
        self
    }
}

struct HeaderResp {
    open: bool,
    /// Index of the button clicked this frame.
    clicked: Option<usize>,
}

/// While filtering, groups whose rows the filter applies to stay open so
/// every match shows; the GitHub lists are not filtered and keep their
/// state.
fn forced_open(cx: &Ctx, key: &str) -> bool {
    cx.filtering() && key != "sec:github" && !key.starts_with("github:")
}

/// Section header: chevron, caption, count, and buttons on hover.
#[allow(clippy::too_many_arguments)]
fn section_header(
    ui: &mut Ui,
    cx: &Ctx,
    title: &str,
    count: usize,
    collapsed: &mut HashSet<String>,
    key: &str,
    first: bool,
    buttons: &[HeaderButton],
) -> HeaderResp {
    let p = cx.p;
    ui.add_space(if first { 4.0 } else { 10.0 });
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
    let forced = forced_open(cx, key);
    let open = forced || !collapsed.contains(key);
    let hovered = row_hovered(ui, &resp);
    let color = if hovered { p.text_2 } else { p.text_3 };
    let painter = ui.painter();
    let y = rect.center().y;
    chevron(painter, pos2(icon_x(rect, 0), y), open, color);
    let g = caption_galley(painter, title, color);
    let tx = text_x(rect, 0);
    let cap_w = g.size().x;
    painter.galley(pos2(tx, y - g.size().y / 2.0), g, color);
    painter.text(
        pos2(tx + cap_w + 6.0, y),
        egui::Align2::LEFT_CENTER,
        format::count(count),
        FontId::new(size::CAPTION, theme::medium()),
        p.text_3,
    );

    let mut clicked = None;
    let mut right = rect.right() - PAD_R + 2.0;
    for (i, b) in buttons.iter().enumerate() {
        if hovered || b.always || matches!(b.glyph, Glyph::Spinner) {
            let r = button_slot(rect, right);
            let id = ui.make_persistent_id(("sidebar_head", key, i));
            if hover_button(ui, r, id, b.glyph, b.tip).clicked() {
                clicked = Some(i);
            }
        }
        right -= BTN_SLOT;
    }
    if resp.clicked() && !forced {
        if open {
            collapsed.insert(key.to_owned());
        } else {
            collapsed.remove(key);
        }
    }
    HeaderResp { open, clicked }
}

/// A collapsible group inside a section (a remote, pull requests, runs).
fn sub_header(
    ui: &mut Ui,
    cx: &Ctx,
    name: &str,
    tip: &str,
    count: usize,
    collapsed: &mut HashSet<String>,
    key: &str,
) -> bool {
    let p = cx.p;
    let (rect, resp) = row_alloc(ui, Sense::click());
    let forced = forced_open(cx, key);
    let open = forced || !collapsed.contains(key);
    let hovered = row_hovered(ui, &resp);
    kit::row_background(ui, rect, hovered, false);
    let y = rect.center().y;
    let painter = ui.painter();
    chevron(
        painter,
        pos2(icon_x(rect, 0), y),
        open,
        if hovered { p.text_2 } else { p.text_3 },
    );
    let g = painter.layout_no_wrap(
        name.to_owned(),
        FontId::new(size::BODY, theme::medium()),
        p.text,
    );
    let x = text_x(rect, 0);
    let w = g.size().x;
    painter.galley(pos2(x, y - g.size().y / 2.0), g, p.text);
    painter.text(
        pos2(x + w + 6.0, y),
        egui::Align2::LEFT_CENTER,
        format::count(count),
        FontId::proportional(size::SMALL),
        p.text_3,
    );
    if resp.clicked() && !forced {
        if open {
            collapsed.insert(key.to_owned());
        } else {
            collapsed.remove(key);
        }
    }
    kit::tip(resp, tip);
    open
}

fn ahead_behind_badges(b: &BranchInfo, compact: bool) -> Vec<(String, Tone)> {
    let mut out = Vec::new();
    if b.upstream_gone {
        out.push(("gone".to_owned(), Tone::Warning));
        return out;
    }
    if b.ahead > 0 {
        out.push((
            if compact {
                format!("\u{2191}{}", b.ahead)
            } else {
                format!("{} ahead", format::count(b.ahead))
            },
            Tone::Success,
        ));
    }
    if b.behind > 0 {
        out.push((
            if compact {
                format!("\u{2193}{}", b.behind)
            } else {
                format!("{} behind", format::count(b.behind))
            },
            Tone::Warning,
        ));
    }
    out
}

fn badges_width(painter: &Painter, badges: &[(String, Tone)]) -> f32 {
    badges
        .iter()
        .map(|(t, _)| kit::badge_width(painter, t) + 4.0)
        .sum()
}

/// Paint `meta` (a number, an index) right-aligned at `right` when the
/// text left of it keeps at least 110 points; returns where that text ends.
fn paint_meta(painter: &Painter, p: &Palette, x: f32, right: f32, y: f32, meta: &str) -> f32 {
    let g = painter.layout_no_wrap(meta.to_owned(), FontId::proportional(size::SMALL), p.text_3);
    let w = g.size().x;
    if right - w - 8.0 - x < 110.0 {
        return right;
    }
    painter.galley(pos2(right - w, y - g.size().y / 2.0), g, p.text_3);
    right - w - 8.0
}

/// Smallest width a branch name gets before the badges shrink.
const NAME_MIN: f32 = 56.0;

#[allow(clippy::too_many_arguments)]
fn local_row(
    ui: &mut Ui,
    cx: &Ctx,
    b: &BranchInfo,
    rect: Rect,
    resp: Response,
    depth: usize,
    actions: &mut Vec<Action>,
) {
    let p = cx.p;
    let hidden = cx.menu.filter.hidden.contains(&b.full);
    let hovered = row_hovered(ui, &resp);
    let selected = cx.is_selected(b.target) && !hidden;
    kit::row_background(ui, rect, hovered, selected);
    let y = rect.center().y;
    let color = if hidden {
        p.text_3
    } else {
        cx.lane_color_of(b.target)
    };
    let c = pos2(icon_x(rect, depth), y);
    if b.is_head {
        ui.painter().circle_filled(c, 4.0, color);
    } else {
        ui.painter().circle_stroke(c, 3.5, Stroke::new(1.5, color));
    }

    // Right side: the eye on hover (or while hidden), then the badges.
    let mut right = rect.right() - PAD_R;
    if hovered || hidden {
        let r = button_slot(rect, right);
        let id = ui.make_persistent_id(("sidebar_eye", &b.full));
        let tip = if hidden {
            "Show in graph"
        } else {
            "Hide from graph"
        };
        if hover_button(ui, r, id, Glyph::Eye { open: !hidden }, tip).clicked() {
            actions.push(Action::ToggleHidden(b.full.clone()));
        }
        right -= BTN_SLOT;
    }
    let x = text_x(rect, depth);
    if !hidden {
        let painter = ui.painter();
        let mut badges = ahead_behind_badges(b, false);
        if right - x - badges_width(painter, &badges) < NAME_MIN {
            badges = ahead_behind_badges(b, true);
        }
        for (text, tone) in badges.iter().rev() {
            right = kit::paint_badge(painter, &p, cx.dark, right, y, text, *tone) - 4.0;
        }
        if !badges.is_empty() {
            right -= 2.0;
        }
    }

    let (font, text_color) = if hidden {
        (body_font(), p.text_3)
    } else if b.is_head {
        (FontId::new(size::BODY, theme::semibold()), p.text_strong)
    } else if selected {
        (body_font(), p.text_strong)
    } else {
        (body_font(), p.text)
    };
    let name = if cx.filtering() {
        b.name.as_str()
    } else {
        b.name.rsplit('/').next().unwrap_or(&b.name)
    };
    let elided = paint_text(ui.painter(), x, right, y, name, font, text_color);

    if resp.clicked() {
        actions.push(Action::JumpTo(b.target));
    }
    if resp.double_clicked() && !b.is_head {
        actions.push(Action::Run(hadion_core::ops::GitOp::Switch {
            branch: b.name.clone(),
        }));
    }
    resp.dnd_set_drag_payload(DragBranch {
        name: b.name.clone(),
        is_remote: false,
        target: b.target,
    });
    resp.context_menu(|ui| menus::local_branch(ui, &cx.menu, b, actions));
    // Only what the row does not show: the full name when cut, the upstream.
    let mut tip = Vec::new();
    if elided {
        tip.push(b.name.clone());
    }
    match (&b.upstream, b.upstream_gone) {
        (Some(u), true) => tip.push(format!("Upstream {u} is gone")),
        (Some(u), false) => tip.push(format!("Tracks {u}")),
        _ => {}
    }
    kit::tip(resp, &tip.join("\n"));
}

#[allow(clippy::too_many_arguments)]
fn remote_row(
    ui: &mut Ui,
    cx: &Ctx,
    rb: &RemoteBranch,
    rect: Rect,
    resp: Response,
    depth: usize,
    actions: &mut Vec<Action>,
) {
    let p = cx.p;
    let all_hidden = cx.menu.filter.hide_remotes;
    let hidden = cx.menu.filter.hidden.contains(&rb.full) || all_hidden;
    let hovered = row_hovered(ui, &resp);
    let selected = cx.is_selected(rb.target) && !hidden;
    kit::row_background(ui, rect, hovered, selected);
    let y = rect.center().y;
    let color = if hidden {
        p.text_3
    } else {
        cx.lane_color_of(rb.target)
    };
    ui.painter().rect_stroke(
        Rect::from_center_size(pos2(icon_x(rect, depth), y), vec2(7.5, 7.5)),
        CornerRadius::same(2),
        Stroke::new(1.5, color),
        StrokeKind::Inside,
    );
    let mut right = rect.right() - PAD_R;
    if hovered || (hidden && !all_hidden) {
        let r = button_slot(rect, right);
        let id = ui.make_persistent_id(("sidebar_eye", &rb.full));
        let own_hidden = cx.menu.filter.hidden.contains(&rb.full);
        let tip = if own_hidden {
            "Show in graph"
        } else {
            "Hide from graph"
        };
        if hover_button(ui, r, id, Glyph::Eye { open: !own_hidden }, tip).clicked() {
            actions.push(Action::ToggleHidden(rb.full.clone()));
        }
        right -= BTN_SLOT;
    }
    let text_color = if hidden {
        p.text_3
    } else if selected {
        p.text_strong
    } else {
        p.text
    };
    let name = if cx.filtering() {
        rb.name.as_str()
    } else {
        rb.branch.rsplit('/').next().unwrap_or(&rb.branch)
    };
    let elided = paint_text(
        ui.painter(),
        text_x(rect, depth),
        right,
        y,
        name,
        body_font(),
        text_color,
    );

    if resp.clicked() {
        actions.push(Action::JumpTo(rb.target));
    }
    if resp.double_clicked() {
        actions.push(menus::checkout_remote(cx.snap, rb));
    }
    resp.dnd_set_drag_payload(DragBranch {
        name: rb.name.clone(),
        is_remote: true,
        target: rb.target,
    });
    resp.context_menu(|ui| menus::remote_branch(ui, &cx.menu, rb, actions));
    if elided {
        resp.on_hover_text(&rb.name);
    }
}

fn paint_tag_glyph(painter: &Painter, c: Pos2, color: Color32) {
    painter.add(egui::Shape::convex_polygon(
        vec![
            pos2(c.x - 4.5, c.y),
            pos2(c.x - 1.0, c.y - 4.0),
            pos2(c.x + 4.5, c.y - 4.0),
            pos2(c.x + 4.5, c.y + 4.0),
            pos2(c.x - 1.0, c.y + 4.0),
        ],
        color.gamma_multiply(0.25),
        Stroke::new(1.3, color),
    ));
}

fn tag_row(
    ui: &mut Ui,
    cx: &Ctx,
    t: &TagInfo,
    rect: Rect,
    resp: Response,
    actions: &mut Vec<Action>,
) {
    let p = cx.p;
    let all_hidden = cx.menu.filter.hide_tags;
    let hidden = cx.menu.filter.hidden.contains(&t.full) || all_hidden;
    let hovered = row_hovered(ui, &resp);
    let selected = cx.is_selected(t.target) && !hidden;
    kit::row_background(ui, rect, hovered, selected);
    let y = rect.center().y;
    paint_tag_glyph(
        ui.painter(),
        pos2(icon_x(rect, 0), y),
        if hidden { p.text_3 } else { p.warning },
    );
    let mut right = rect.right() - PAD_R;
    if hovered || (hidden && !all_hidden) {
        let r = button_slot(rect, right);
        let id = ui.make_persistent_id(("sidebar_eye", &t.full));
        let own_hidden = cx.menu.filter.hidden.contains(&t.full);
        let tip = if own_hidden {
            "Show in graph"
        } else {
            "Hide from graph"
        };
        if hover_button(ui, r, id, Glyph::Eye { open: !own_hidden }, tip).clicked() {
            actions.push(Action::ToggleHidden(t.full.clone()));
        }
        right -= BTN_SLOT;
    }
    let text_color = if hidden {
        p.text_3
    } else if selected {
        p.text_strong
    } else {
        p.text
    };
    let elided = paint_text(
        ui.painter(),
        text_x(rect, 0),
        right,
        y,
        &t.name,
        body_font(),
        text_color,
    );
    if resp.clicked() {
        actions.push(Action::JumpTo(t.target));
    }
    if resp.double_clicked() {
        actions.push(Action::Dialog(menus::detach_dialog(&t.name)));
    }
    resp.context_menu(|ui| menus::tag(ui, &cx.menu, t, actions));
    if elided {
        resp.on_hover_text(&t.name);
    }
}

fn stash_row(
    ui: &mut Ui,
    cx: &Ctx,
    s: &StashInfo,
    rect: Rect,
    resp: Response,
    actions: &mut Vec<Action>,
) {
    let p = cx.p;
    let hovered = row_hovered(ui, &resp);
    kit::row_background(ui, rect, hovered, false);
    let y = rect.center().y;
    icons::paint(
        ui.painter(),
        Icon::Stash,
        pos2(icon_x(rect, 0), y),
        12.0,
        p.purple,
    );
    let idx = format!("{{{}}}", s.index);
    let g = ui
        .painter()
        .layout_no_wrap(idx, FontId::monospace(size::MONO - 0.5), p.text_3);
    let right = rect.right() - PAD_R;
    let idx_w = g.size().x;
    ui.painter()
        .galley(pos2(right - idx_w, y - g.size().y / 2.0), g, p.text_3);
    let elided = paint_text(
        ui.painter(),
        text_x(rect, 0),
        right - idx_w - 8.0,
        y,
        &s.message,
        body_font(),
        p.text,
    );
    resp.context_menu(|ui| menus::stash(ui, s, actions));
    if elided {
        resp.on_hover_text(&s.message);
    }
}

/// Pull requests and workflow runs from GitHub (via `gh`).
#[allow(clippy::too_many_arguments)]
fn github_section(
    ui: &mut Ui,
    cx: &Ctx,
    data: Option<&hadion_core::github::GitHubData>,
    error: Option<&str>,
    loading: bool,
    collapsed: &mut HashSet<String>,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let p = cx.p;
    let count = data.map(|d| d.prs.len() + d.runs.len()).unwrap_or(0);
    let head = section_header(
        ui,
        cx,
        "GitHub",
        count,
        collapsed,
        "sec:github",
        false,
        &[
            // Rightmost first: the spinner sits in the corner while loading.
            if loading {
                HeaderButton::new(Glyph::Spinner, "")
            } else {
                HeaderButton::new(
                    Glyph::Icon(Icon::Fetch),
                    "Refresh pull requests and workflow runs",
                )
            },
            HeaderButton::new(
                Glyph::Icon(Icon::External),
                "Open the GitHub page: runs, jobs, logs, pull requests (Ctrl+Shift+G)",
            ),
        ],
    );
    match head.clicked {
        Some(0) => actions.push(Action::RefreshGitHub),
        Some(1) => actions.push(Action::ShowPage(Page::GitHub)),
        _ => {}
    }
    if !head.open {
        return;
    }
    if let Some(err) = error {
        // The first line of the error ("Not Found (HTTP 404)") and what it
        // usually means; the full text (with GitHub's documentation link,
        // which used to wrap across the pane) is one click away.
        let first = err.lines().next().unwrap_or(err).trim();
        let hint = if err.contains("HTTP 404") {
            "GitHub does not know this repository under that name, or this account cannot see it."
        } else if err.contains("HTTP 401") || err.contains("HTTP 403") {
            "Sign in again with gh auth login, or check the token's access."
        } else {
            ""
        };
        Frame::NONE
            .inner_margin(Margin {
                left: TEXT_X as i8,
                right: 12,
                top: 2,
                bottom: 6,
            })
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.add(
                    egui::Label::new(
                        egui::RichText::new("Could not load pull requests and runs")
                            .size(size::SMALL)
                            .color(p.text),
                    )
                    .wrap(),
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(first)
                            .size(size::SMALL)
                            .color(p.warning),
                    )
                    .wrap(),
                );
                if !hint.is_empty() {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(hint).size(size::SMALL).color(p.text_2),
                        )
                        .wrap(),
                    );
                }
                // Whole links that move to the next line in a narrow pane
                // (a wrapping link broke "Copy the error" over three lines).
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(12.0, 2.0);
                    let link = |ui: &mut Ui, text: &str| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(text).color(p.accent))
                                .sense(Sense::click())
                                .wrap_mode(egui::TextWrapMode::Extend),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                    };
                    if link(ui, "Try again").clicked() {
                        actions.push(Action::RefreshGitHub);
                    }
                    if link(ui, "Copy the error").clicked() {
                        actions.push(Action::Copy(err.to_owned()));
                    }
                });
            });
        return;
    }
    let Some(d) = data else {
        empty_hint(
            ui,
            cx,
            0,
            if loading {
                "Loading…"
            } else {
                "Not loaded yet"
            },
        );
        return;
    };

    if sub_header(
        ui,
        cx,
        "Pull requests",
        "",
        d.prs.len(),
        collapsed,
        "github:prs",
    ) {
        if d.prs.is_empty() {
            empty_hint(ui, cx, 1, "No open pull requests");
        }
        for pr in &d.prs {
            let (rect, resp) = row_alloc(ui, Sense::click());
            if !ui.is_rect_visible(rect) {
                continue;
            }
            kit::row_background(ui, rect, row_hovered(ui, &resp), false);
            let y = rect.center().y;
            icons::paint(
                ui.painter(),
                Icon::PullRequest,
                pos2(icon_x(rect, 1), y),
                12.0,
                if pr.draft { p.text_3 } else { p.success },
            );
            let text_right = paint_meta(
                ui.painter(),
                &p,
                text_x(rect, 1),
                rect.right() - PAD_R,
                y,
                &format!("#{}", pr.number),
            );
            paint_text(
                ui.painter(),
                text_x(rect, 1),
                text_right,
                y,
                &pr.title,
                body_font(),
                if pr.draft { p.text_2 } else { p.text },
            );
            if resp.clicked() {
                actions.push(Action::OpenPr(pr.number));
            }
            resp.context_menu(|ui| {
                menus::begin(ui, 200.0);
                menus::action(ui, "Show pull request", actions, Action::OpenPr(pr.number));
                menus::action(
                    ui,
                    "Check out PR branch",
                    actions,
                    Action::Run(hadion_core::ops::GitOp::GhPrCheckout { number: pr.number }),
                );
                menus::separator(ui);
                menus::action(
                    ui,
                    "Open on GitHub",
                    actions,
                    Action::OpenUrl(pr.url.clone()),
                );
                menus::action(ui, "Copy URL", actions, Action::Copy(pr.url.clone()));
            });
            // The row shows the title and number; the rest is here.
            let mut tip = vec![
                format!("#{} {}", pr.number, pr.title),
                format!("{} \u{2192} {}", pr.head, pr.base),
                format!("By {}{}", pr.author, if pr.draft { ", draft" } else { "" }),
            ];
            if !pr.labels.is_empty() {
                tip.push(format!("Labels: {}", pr.labels.join(", ")));
            }
            if let Some(u) = pr.updated {
                tip.push(format!("Updated {}", format::relative(u, now)));
            }
            resp.on_hover_text(tip.join("\n"));
        }
    }

    if sub_header(
        ui,
        cx,
        "Actions",
        "",
        d.runs.len(),
        collapsed,
        "github:runs",
    ) {
        if d.runs.is_empty() {
            empty_hint(ui, cx, 1, "No workflow runs");
        }
        let t = ui.input(|i| i.time);
        for run in d.runs.iter().take(40) {
            let (rect, resp) = row_alloc(ui, Sense::click());
            if !ui.is_rect_visible(rect) {
                continue;
            }
            kit::row_background(ui, rect, row_hovered(ui, &resp), false);
            let y = rect.center().y;
            icons::outcome_dot(
                ui.painter(),
                run.outcome(),
                pos2(icon_x(rect, 1), y),
                4.5,
                t,
            );
            if run.is_running() {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(80));
            }
            let right = rect.right() - PAD_R;
            let x = text_x(rect, 1);
            let text_right = paint_meta(
                ui.painter(),
                &p,
                x,
                right,
                y,
                &format!("#{}", run.run_number),
            );
            // Workflow name, then its branch in the secondary colour.
            let name = truncated(
                ui.painter(),
                &run.name,
                body_font(),
                p.text,
                (text_right - x).max(12.0) * 0.62,
            );
            let name_w = name.size().x;
            ui.painter()
                .galley(pos2(x, y - name.size().y / 2.0), name, p.text);
            if text_right - (x + name_w + 8.0) > 24.0 {
                paint_text(
                    ui.painter(),
                    x + name_w + 8.0,
                    text_right,
                    y,
                    &run.branch,
                    FontId::proportional(size::SMALL),
                    p.text_2,
                );
            }
            if resp.clicked() {
                actions.push(Action::OpenRun {
                    run: run.id,
                    job: None,
                });
            }
            resp.context_menu(|ui| {
                menus::begin(ui, 200.0);
                menus::action(
                    ui,
                    "Show run",
                    actions,
                    Action::OpenRun {
                        run: run.id,
                        job: None,
                    },
                );
                menus::action(
                    ui,
                    "Open on GitHub",
                    actions,
                    Action::OpenUrl(run.url.clone()),
                );
            });
            let mut tip = vec![
                run.title.clone(),
                format!(
                    "{} #{}: {}",
                    run.name,
                    run.run_number,
                    run.outcome().label()
                ),
                format!("{} by {} on {}", run.event, run.actor, run.branch),
            ];
            if let Some(s) = run.started.or(run.created) {
                let mut line = format!("Started {}", format::relative(s, now));
                if let Some(dsecs) = run.duration_secs(now) {
                    line.push_str(&format!(", took {}", format::duration(dsecs)));
                }
                tip.push(line);
            }
            resp.on_hover_text(tip.join("\n"));
        }
        if d.runs.len() > 40 {
            let (rect, resp) = row_alloc(ui, Sense::click());
            if ui.is_rect_visible(rect) {
                kit::row_background(ui, rect, row_hovered(ui, &resp), false);
                paint_text(
                    ui.painter(),
                    text_x(rect, 1),
                    rect.right() - PAD_R,
                    rect.center().y,
                    &format!("All {} runs \u{2192}", format::count(d.runs.len())),
                    body_font(),
                    p.accent,
                );
                if resp.clicked() {
                    actions.push(Action::ShowPage(Page::GitHub));
                }
                resp.on_hover_cursor(egui::CursorIcon::PointingHand);
            }
        }
    }
}

/// Folder tree over `/`-separated names.
struct Tree<'a, T> {
    folders: BTreeMap<String, Tree<'a, T>>,
    leaves: Vec<(&'a str, &'a T)>,
}

type LeafFn<'f, T> = dyn FnMut(&mut Ui, &Ctx, &T, Rect, Response, usize) + 'f;

impl<'a, T> Tree<'a, T> {
    fn build(items: impl Iterator<Item = &'a T>, name_of: impl Fn(&'a T) -> &'a str) -> Self {
        let mut root = Tree {
            folders: BTreeMap::new(),
            leaves: Vec::new(),
        };
        for item in items {
            let name = name_of(item);
            let mut node = &mut root;
            let mut parts = name.split('/').peekable();
            while let Some(part) = parts.next() {
                if parts.peek().is_none() {
                    node.leaves.push((part, item));
                } else {
                    node = node.folders.entry(part.to_owned()).or_insert_with(|| Tree {
                        folders: BTreeMap::new(),
                        leaves: Vec::new(),
                    });
                }
            }
        }
        root
    }

    /// Show folders (collapsible) and leaves at `depth`. While filtering,
    /// the tree is flat: every leaf sits at the starting depth with its
    /// full name.
    fn show(
        &self,
        ui: &mut Ui,
        depth: usize,
        key_prefix: &str,
        collapsed: &mut HashSet<String>,
        cx: &Ctx,
        leaf: &mut LeafFn<'_, T>,
    ) {
        let flat = cx.filtering();
        for (name, sub) in &self.folders {
            let key = format!("{key_prefix}/{name}");
            let open = flat || !collapsed.contains(&key);
            if !flat {
                let (rect, resp) = row_alloc(ui, Sense::click());
                if ui.is_rect_visible(rect) {
                    let p = cx.p;
                    let hovered = row_hovered(ui, &resp);
                    kit::row_background(ui, rect, hovered, false);
                    let y = rect.center().y;
                    chevron(
                        ui.painter(),
                        pos2(icon_x(rect, depth), y),
                        open,
                        if hovered { p.text_2 } else { p.text_3 },
                    );
                    paint_text(
                        ui.painter(),
                        text_x(rect, depth),
                        rect.right() - PAD_R,
                        y,
                        name,
                        body_font(),
                        p.text_2,
                    );
                }
                if resp.clicked() {
                    if open {
                        collapsed.insert(key.clone());
                    } else {
                        collapsed.remove(&key);
                    }
                }
            }
            if open {
                sub.show(
                    ui,
                    if flat { depth } else { depth + 1 },
                    &key,
                    collapsed,
                    cx,
                    leaf,
                );
            }
        }
        for (_, item) in &self.leaves {
            let (rect, resp) = row_alloc(ui, Sense::click_and_drag());
            if !ui.is_rect_visible(rect) {
                continue;
            }
            leaf(ui, cx, item, rect, resp, depth);
        }
    }
}
