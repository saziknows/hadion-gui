//! Repository tabs and the git toolbar.
//!
//! The toolbar holds git actions only (pull, push, fetch, branch, stash,
//! undo), the current branch, and commit search on the history page; page
//! navigation lives in the rail.

use egui::{
    Align, Color32, CornerRadius, FontId, Frame, Key, Layout, Margin, Modifiers, Rect, RichText,
    Sense, Stroke, Ui, pos2, vec2,
};

use hadion_core::ops::GitOp;
use hadion_core::{Head, Pending, Snapshot};

use crate::state::{Action, Dialog, Page, PullPref, RepoState, Settings};
use crate::theme::{self, Tone, pal, size};
use crate::views::icons::{self, Icon, IconButton};
use crate::views::kit;
use crate::views::menus::split_remote;

const TAB_H: f32 = 36.0;

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    repos: &mut [RepoState],
    active: usize,
    settings: &Settings,
    cloning: Option<&str>,
    undo: Option<&hadion_core::reflog::Undo>,
    focus_search: &mut bool,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    egui::Panel::top("tabs")
        .show_separator_line(false)
        .frame(Frame::NONE.fill(p.chrome))
        .show(ui, |ui| tabs(ui, repos, active, cloning, actions));

    if repos.is_empty() {
        return;
    }
    let repo = &mut repos[active];
    egui::Panel::top("toolbar")
        .frame(Frame::NONE.fill(p.panel).inner_margin(Margin {
            left: 12,
            right: 12,
            top: 7,
            bottom: 7,
        }))
        .show(ui, |ui| {
            toolbar_row(ui, repo, settings, undo, focus_search, actions);
        });
    if let Some(snap) = repo.snapshot.as_ref()
        && snap.pending != Pending::None
    {
        pending_banner(ui, repo, actions);
    }
}

/// Space a tab takes besides its name: padding, the close button, the gap.
const TAB_CHROME: f32 = 54.0;
/// The shortest a cut tab name gets.
const TAB_NAME_MIN: f32 = 56.0;

/// The longest a tab name may be drawn so all tabs fit `room`: names
/// shorter than the cap keep their width, the longer ones share the rest.
fn tab_name_cap(widths: &[f32], room: f32) -> f32 {
    let n = widths.len() as f32;
    let fits = |cap: f32| widths.iter().map(|w| w.min(cap)).sum::<f32>() + n * TAB_CHROME <= room;
    let widest = widths.iter().copied().fold(0.0, f32::max);
    if fits(widest) {
        return f32::INFINITY;
    }
    let (mut lo, mut hi) = (TAB_NAME_MIN, widest);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

fn tabs(
    ui: &mut Ui,
    repos: &[RepoState],
    active: usize,
    cloning: Option<&str>,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    ui.spacing_mut().item_spacing.x = 0.0;
    ui.horizontal(|ui| {
        ui.set_height(TAB_H);
        ui.add_space(6.0);
        // Measured in the active (medium) weight for every tab, so the
        // strip does not shift when the active tab changes.
        let name_font = FontId::new(size::BODY - 0.5, theme::medium());
        let widths: Vec<f32> = repos
            .iter()
            .map(|r| {
                ui.painter()
                    .layout_no_wrap(r.name.clone(), name_font.clone(), Color32::WHITE)
                    .size()
                    .x
            })
            .collect();
        let cloning_w = cloning.map_or(0.0, |name| {
            8.0 + 11.0 + 6.0 + text_w(ui, &format!("Cloning {name}…"), size::SMALL)
        });
        // Many or long names share the strip (names are cut with an
        // ellipsis); when even that does not fit, the tabs scroll sideways
        // and the active one is kept in view. They used to run off the
        // window, active tab and + button included.
        let room = (ui.available_width() - cloning_w - 6.0 - 26.0 - 8.0).max(TAB_CHROME);
        let cap = tab_name_cap(&widths, room);
        let need: f32 = widths.iter().map(|w| w.min(cap) + TAB_CHROME).sum();
        let mut draw = |ui: &mut Ui, scroll: bool| {
            for (i, repo) in repos.iter().enumerate() {
                let resp = tab(ui, repo, i, i == active, widths[i].min(cap), actions);
                if scroll && i == active {
                    let id = egui::Id::unique("repo_tabs_shown");
                    if ui.data(|d| d.get_temp::<usize>(id)) != Some(active) {
                        resp.scroll_to_me(None);
                        ui.data_mut(|d| d.insert_temp(id, active));
                    }
                }
            }
        };
        if need <= room + 0.5 {
            draw(ui, false);
        } else {
            egui::ScrollArea::horizontal()
                .id_salt("repo_tabs")
                .max_width(room)
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        draw(ui, true);
                    });
                });
        }
        if let Some(name) = cloning {
            ui.add_space(8.0);
            ui.add(egui::Spinner::new().size(11.0).color(p.text_3));
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!("Cloning {name}…"))
                    .size(size::SMALL)
                    .color(p.text_2),
            );
        }
        ui.add_space(6.0);
        let plus = IconButton::icon(Icon::Plus)
            .compact()
            .show(ui)
            .on_hover_text("Open or clone a repository");
        egui::Popup::menu(&plus)
            .id(egui::Id::unique("open_menu"))
            .show(|ui| {
                crate::views::menus::begin(ui, 240.0);
                if crate::views::menus::entry_with_hint(
                    ui,
                    "Open repository…",
                    "Ctrl+O",
                    None,
                    false,
                )
                .clicked()
                {
                    actions.push(Action::PickRepo);
                    ui.close();
                }
                if crate::views::menus::entry_with_hint(
                    ui,
                    "Clone repository…",
                    "Ctrl+Shift+O",
                    None,
                    false,
                )
                .clicked()
                {
                    actions.push(Action::CloneDialog);
                    ui.close();
                }
            });
    });
}

/// One repository tab, its name cut to `name_w`.
fn tab(
    ui: &mut Ui,
    repo: &RepoState,
    i: usize,
    is_active: bool,
    name_w: f32,
    actions: &mut Vec<Action>,
) -> egui::Response {
    let p = *pal(ui);
    let font = if is_active {
        FontId::new(size::BODY - 0.5, theme::medium())
    } else {
        FontId::proportional(size::BODY - 0.5)
    };
    let w = name_w + TAB_CHROME - 2.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, TAB_H), Sense::click());
    // From the pointer position, not `resp.hovered()`: the close
    // button drawn on top of the tab takes the hover, which used to
    // hide the button again on the next frame (flicker).
    let hovered = ui.rect_contains_pointer(rect);
    let tab_rect = Rect::from_min_max(pos2(rect.left(), rect.top() + 6.0), rect.max);
    let corners = CornerRadius {
        nw: 8,
        ne: 8,
        sw: 0,
        se: 0,
    };
    let painter = ui.painter();
    if is_active {
        painter.rect_filled(tab_rect, corners, p.panel);
    } else if hovered {
        painter.rect_filled(tab_rect, corners, p.hover.gamma_multiply(0.6));
    }
    let text_color = if is_active {
        p.text_strong
    } else if hovered {
        p.text
    } else {
        p.text_2
    };
    let g = crate::views::truncated(painter, &repo.name, font, text_color, name_w + 1.0);
    let cut = g.elided;
    painter.galley(
        pos2(rect.left() + 16.0, tab_rect.center().y - g.size().y / 2.0),
        g,
        text_color,
    );
    let right_center = pos2(rect.right() - 20.0, tab_rect.center().y);
    if repo.loading || repo.is_busy() {
        kit::overlay(
            ui,
            Rect::from_center_size(right_center, vec2(12.0, 12.0)),
            egui::Spinner::new().size(11.0).color(p.text_3),
        );
    } else if hovered || is_active {
        let close = kit::overlay(
            ui,
            Rect::from_center_size(right_center, vec2(26.0, 26.0)),
            IconButton::icon(Icon::Close).compact(),
        );
        if close.on_hover_text("Close (Ctrl+W)").clicked() {
            actions.push(Action::CloseRepo(i));
        }
    }
    if resp.clicked() {
        actions.push(Action::Activate(i));
    }
    if resp.middle_clicked() {
        actions.push(Action::CloseRepo(i));
    }
    ui.add_space(2.0);
    // A cut name gets its full text on hover.
    if cut {
        resp.on_hover_text(&repo.name)
    } else {
        resp
    }
}

/// How much of the toolbar's left side is drawn in full, from everything
/// labelled to icons only; a narrow window steps down until it fits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Fit {
    /// Every button labelled; the branch chip says "up to date with …".
    Full,
    /// The chip drops its "up to date" note (the tooltip keeps it).
    NoNote,
    /// Fetch, Branch, Stash, Pop and Undo are icons.
    Secondary,
    /// Every button is an icon; the chip is cut to what is left.
    Icons,
}

/// How the right side (commit search, Filter) is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RightFit {
    search_w: f32,
    kbd: bool,
    filter_label: bool,
}

const RIGHT_FITS: [RightFit; 4] = [
    RightFit {
        search_w: 170.0,
        kbd: true,
        filter_label: true,
    },
    RightFit {
        search_w: 140.0,
        kbd: false,
        filter_label: true,
    },
    RightFit {
        search_w: 120.0,
        kbd: false,
        filter_label: false,
    },
    RightFit {
        search_w: 96.0,
        kbd: false,
        filter_label: false,
    },
];

/// Width of `text` in the button font (IconButton: body size, medium).
fn text_w(ui: &Ui, text: &str, size: f32) -> f32 {
    ui.painter()
        .layout_no_wrap(
            text.to_owned(),
            FontId::new(size, theme::medium()),
            Color32::WHITE,
        )
        .size()
        .x
}

/// Width of a toolbar `IconButton` (11 points of padding a side, a 15
/// point icon and a 7 point gap; 30 square without a label).
fn button_w(ui: &Ui, label: &str, labelled: bool) -> f32 {
    if labelled {
        22.0 + 15.0 + 7.0 + text_w(ui, label, size::BODY)
    } else {
        30.0
    }
}

/// An icon button that shows its label only when `labelled`; the tooltip
/// then starts with the label.
fn tool_button(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    labelled: bool,
    enabled: bool,
) -> egui::Response {
    let b = if labelled {
        IconButton::new(icon, label)
    } else {
        IconButton::icon(icon)
    };
    b.enabled(enabled).show(ui)
}

fn tool_tip(label: &str, labelled: bool, tip: &str) -> String {
    if labelled || tip.starts_with(label) {
        tip.to_owned()
    } else {
        format!("{label}\n{tip}")
    }
}

/// Spacing between toolbar items, and the space around a divider.
const GAP: f32 = 2.0;
const DIVIDER: f32 = 8.0 + GAP + 1.0 + GAP + 8.0;

fn toolbar_row(
    ui: &mut Ui,
    repo: &mut RepoState,
    settings: &Settings,
    undo: Option<&hadion_core::reflog::Undo>,
    focus_search: &mut bool,
    actions: &mut Vec<Action>,
) {
    let RepoState {
        snapshot,
        search,
        search_hits,
        search_cursor,
        busy: busy_label,
        loading,
        page,
        github_remote,
        devops,
        ..
    } = repo;
    let busy = busy_label.is_some();
    let snap = snapshot.as_ref();
    let current = snap.and_then(|s| s.current_branch());
    let has_remotes = snap.is_some_and(|s| !s.remotes.is_empty());
    let detached = snap.is_some_and(|s| matches!(s.head, Head::Detached(_)));
    let has_stash = snap.is_some_and(|s| !s.stashes.is_empty());
    let dirty = snap.is_some_and(|s| !s.status.is_clean());
    let p = *pal(ui);
    let on_graph = *page == Page::Graph;

    // Work out what fits: the left side keeps its labels for as long as
    // possible (the search box narrows first), then gives them up.
    let ahead = current.map(|b| b.ahead).unwrap_or(0);
    let push_label = if ahead > 0 {
        format!("Push {ahead}")
    } else {
        "Push".to_owned()
    };
    let row_w = ui.available_width();
    let filter_label = crate::devops::history::toolbar_label(&devops.history);
    let search_id = search_id();
    let search_focused = ui.memory(|m| m.has_focus(search_id));
    let right_w = |r: &RightFit| -> f32 {
        let mut w = 0.0;
        if on_graph {
            w += search_width(
                ui,
                r,
                search,
                search_hits.len(),
                *search_cursor,
                search_focused,
            );
            w += 8.0;
            w += if r.filter_label {
                18.0 + 14.0 + 7.0 + text_w(ui, &filter_label, size::SMALL)
            } else {
                26.0
            };
        }
        if let Some(label) = busy_label.as_deref() {
            w += 8.0 + 13.0 + 8.0 + text_w(ui, label, size::SMALL).min(BUSY_MAX);
        } else if *loading {
            w += 8.0 + 13.0;
        }
        w
    };
    let left_w = |fit: Fit| -> f32 {
        let main = fit < Fit::Icons;
        let second = fit < Fit::Secondary;
        let mut items = vec![
            button_w(ui, "Pull", main),
            26.0,
            button_w(ui, &push_label, main),
        ];
        if github_remote.is_some() {
            items.push(button_w(ui, "Pull request", main));
        }
        items.push(button_w(ui, "Fetch", second));
        items.push(DIVIDER);
        if snap.is_some() {
            items.push(button_w(ui, "Branch", second));
        }
        items.push(button_w(ui, "Stash", second));
        items.push(button_w(ui, "Pop", second));
        items.push(DIVIDER);
        items.push(button_w(ui, "Undo", second));
        let n = items.len() as f32;
        let mut w = items.iter().sum::<f32>() + GAP * (n - 1.0) + 16.0;
        if let Some(s) = snap {
            w += chip_width(ui, s, fit == Fit::Full);
        }
        w
    };
    let mut choice = None;
    'fit: for fit in [Fit::Full, Fit::NoNote, Fit::Secondary, Fit::Icons] {
        for r in RIGHT_FITS {
            if left_w(fit) + 16.0 + right_w(&r) <= row_w {
                choice = Some((fit, r));
                break 'fit;
            }
        }
    }
    let (fit, right_fit) = choice.unwrap_or((Fit::Icons, RIGHT_FITS[3]));

    // The right side first (it is narrow and keeps its place), then the
    // left side in what is left, clipped so it can never draw over it.
    let (row, _) = ui.allocate_exact_size(vec2(row_w, 30.0), Sense::hover());
    let mut right_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("toolbar_right")
            .max_rect(row)
            .layout(Layout::right_to_left(Align::Center)),
    );
    {
        let ui = &mut right_ui;
        ui.spacing_mut().item_spacing.x = 8.0;
        if on_graph {
            search_field(
                ui,
                &right_fit,
                search,
                search_hits,
                *search_cursor,
                focus_search,
                actions,
            );
            crate::devops::history::toolbar_button(
                ui,
                &devops.history,
                right_fit.filter_label,
                actions,
            );
        }
        if let Some(label) = busy_label.as_deref() {
            ui.scope(|ui| {
                ui.set_max_width(ui.available_width().min(BUSY_MAX));
                ui.add(
                    egui::Label::new(RichText::new(label).size(size::SMALL).color(p.text_2))
                        .truncate(),
                );
            });
            ui.add(egui::Spinner::new().size(13.0).color(p.accent));
        } else if *loading {
            ui.add(egui::Spinner::new().size(13.0).color(p.text_3));
        }
    }
    let right_used = if right_ui.min_rect().width() > 0.5 {
        right_ui.min_rect().width() + 16.0
    } else {
        0.0
    };
    let left_rect = Rect::from_min_max(row.min, pos2(row.right() - right_used, row.bottom()));
    let mut left_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("toolbar_left")
            .max_rect(left_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    left_ui.shrink_clip_rect(left_rect);
    let main = fit < Fit::Icons;
    let second = fit < Fit::Secondary;
    {
        let ui = &mut left_ui;
        ui.spacing_mut().item_spacing.x = GAP;

        // Pull, with a dropdown for the strategy.
        let pull_enabled = !busy && has_remotes && current.is_some();
        let pull = tool_button(ui, Icon::Pull, "Pull", main, pull_enabled).on_hover_text(tool_tip(
            "Pull",
            main,
            settings.pull.label(),
        ));
        if pull.clicked() {
            actions.push(Action::Run(GitOp::Pull {
                mode: settings.pull.mode(),
            }));
        }
        let chevron = IconButton::icon(Icon::ChevronDown)
            .compact()
            .enabled(pull_enabled)
            .show(ui)
            .on_hover_text("Pull strategy");
        // Explicit popup ids: the buttons' auto ids can shift with hover state.
        egui::Popup::menu(&chevron)
            .id(egui::Id::unique("pull_menu"))
            .show(|ui| {
                ui.set_min_width(240.0);
                for pp in [PullPref::Merge, PullPref::FastForwardOnly, PullPref::Rebase] {
                    if ui.button(pp.label()).clicked() {
                        actions.push(Action::SetPull(pp));
                        actions.push(Action::Run(GitOp::Pull { mode: pp.mode() }));
                        ui.close();
                    }
                }
            });

        // Push: becomes the primary action once there is something to push.
        let push_enabled = !busy && has_remotes && current.is_some() && !detached;
        let push_tip = match current {
            Some(b) if b.upstream_gone => {
                format!("Upstream of {} is gone: choose a remote", b.name)
            }
            Some(b) => match &b.upstream {
                Some(u) => format!("Push {} to {u}", b.name),
                None => format!("{} has no upstream yet: choose a remote", b.name),
            },
            None => "Nothing to push".into(),
        };
        let push = if main {
            IconButton::new(Icon::Push, &push_label)
        } else {
            IconButton::icon(Icon::Push)
        }
        .primary(push_enabled && ahead > 0)
        .enabled(push_enabled)
        .show(ui)
        .on_hover_text(tool_tip(&push_label, main, &push_tip));
        if push.clicked()
            && let (Some(b), Some(snap)) = (current, snap)
        {
            let upstream = b
                .upstream
                .as_deref()
                .filter(|_| !b.upstream_gone)
                .and_then(|u| split_remote(snap, u));
            match upstream {
                Some((remote, _)) if b.behind == 0 => actions.push(Action::Run(GitOp::Push {
                    remote: remote.to_owned(),
                    branch: b.name.clone(),
                    set_upstream: false,
                    force: false,
                })),
                _ => {
                    let remotes: Vec<String> =
                        snap.remotes.iter().map(|r| r.name.clone()).collect();
                    let remote = upstream
                        .as_ref()
                        .and_then(|(r, _)| remotes.iter().position(|x| x == r))
                        .unwrap_or(0);
                    actions.push(Action::Dialog(Dialog::Push {
                        branch: b.name.clone(),
                        remotes,
                        remote,
                        set_upstream: upstream.is_none(),
                        force: false,
                    }));
                }
            }
        }
        if github_remote.is_some() {
            let pr = tool_button(
                ui,
                Icon::PullRequest,
                "Pull request",
                main,
                current.is_some(),
            )
            .on_hover_text(tool_tip(
                "Pull request",
                main,
                "Open a pull request from the current branch",
            ));
            if pr.clicked() {
                actions.push(Action::ShowPage(Page::Review));
                actions.push(crate::devops::review::Act::NewPr.into());
            }
        }
        if tool_button(ui, Icon::Fetch, "Fetch", second, !busy && has_remotes)
            .on_hover_text(tool_tip("Fetch", second, "Fetch all remotes and prune"))
            .clicked()
        {
            actions.push(Action::Run(GitOp::Fetch));
        }

        divider(ui);

        if let Some(snap) = snap {
            let head = snap.head.target();
            if tool_button(ui, Icon::Branch, "Branch", second, !busy && head.is_some())
                .on_hover_text(tool_tip("Branch", second, "Create a branch at HEAD"))
                .clicked()
            {
                let (start, label) = match snap.current_branch() {
                    Some(b) => (b.name.clone(), b.name.clone()),
                    None => (
                        head.map(|h| h.to_string()).unwrap_or_default(),
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
        }
        if tool_button(ui, Icon::Stash, "Stash", second, !busy && dirty)
            .on_hover_text(tool_tip("Stash", second, "Stash all changes"))
            .clicked()
        {
            actions.push(Action::Dialog(Dialog::Stash {
                message: String::new(),
                include_untracked: true,
            }));
        }
        if tool_button(ui, Icon::Pop, "Pop", second, !busy && has_stash)
            .on_hover_text(tool_tip("Pop", second, "Apply and drop the newest stash"))
            .clicked()
        {
            actions.push(Action::Run(GitOp::StashPop { index: 0 }));
        }

        divider(ui);
        let undo_tip = match undo {
            Some(u) => format!("{}  (Ctrl+Z)\n{}", u.label, u.detail),
            None => "Nothing to undo".to_owned(),
        };
        let undo_btn = tool_button(ui, Icon::Undo, "Undo", second, !busy && undo.is_some())
            .on_hover_text(tool_tip("Undo", second, &undo_tip));
        if undo_btn.clicked() {
            actions.push(Action::Undo);
        }

        ui.add_space(16.0);
        if let Some(snap) = snap {
            // Whatever is left: a long branch name is cut, not overlapped.
            let room = ui.available_width();
            if room >= 60.0 {
                branch_chip(ui, snap, fit == Fit::Full, room);
            }
        }
    }
}

/// A thin vertical divider with space around it.
fn divider(ui: &mut Ui) {
    ui.add_space(8.0);
    icons::divider(ui, 18.0);
    ui.add_space(8.0);
}

/// Most a busy label ("Fetching…") takes on the toolbar.
const BUSY_MAX: f32 = 180.0;

/// What the branch chip says after the branch name.
fn chip_note(snap: &Snapshot) -> Option<String> {
    let b = snap.current_branch()?;
    if b.upstream_gone || b.ahead > 0 || b.behind > 0 {
        return None;
    }
    b.upstream.as_ref().map(|u| format!("up to date with {u}"))
}

/// The chip's badges: ahead / behind / upstream gone.
fn chip_badges(snap: &Snapshot) -> Vec<(String, Tone)> {
    let Some(b) = snap.current_branch() else {
        return Vec::new();
    };
    if b.upstream_gone {
        return vec![("upstream gone".into(), Tone::Warning)];
    }
    let mut v = Vec::new();
    if b.ahead > 0 {
        v.push((format!("{} ahead", b.ahead), Tone::Success));
    }
    if b.behind > 0 {
        v.push((format!("{} behind", b.behind), Tone::Warning));
    }
    v
}

/// The chip's main text and its colour.
fn chip_name(snap: &Snapshot) -> (String, bool) {
    match &snap.head {
        Head::Branch { name, .. } => (name.clone(), false),
        Head::Detached(oid) => (
            format!("detached {}", hadion_core::repo::short_id(*oid)),
            true,
        ),
        Head::Unborn(name) => (format!("{name}  ·  no commits yet"), false),
    }
}

/// Width of the branch chip (frame margins and border, icon, name, badges
/// and, with `note`, the "up to date" note).
fn chip_width(ui: &Ui, snap: &Snapshot, note: bool) -> f32 {
    let (name, _) = chip_name(snap);
    let name_w = ui
        .painter()
        .layout_no_wrap(
            name,
            FontId::new(size::BODY, theme::semibold()),
            Color32::WHITE,
        )
        .size()
        .x;
    let mut w = 22.0 + 14.0 + 6.0 + name_w;
    for (b, _) in chip_badges(snap) {
        w += 6.0 + kit::badge_width(ui.painter(), &b);
    }
    if note && let Some(n) = chip_note(snap) {
        w += 6.0 + text_w(ui, &n, size::SMALL);
    }
    w + 2.0
}

/// The current branch, with ahead / behind badges (and, with `note`, "up
/// to date with …"), never wider than `max_w`: a long name is cut.
fn branch_chip(ui: &mut Ui, snap: &Snapshot, note: bool, max_w: f32) {
    let p = *pal(ui);
    let (name, detached) = chip_name(snap);
    let badges = chip_badges(snap);
    let note = if note { chip_note(snap) } else { None };
    // Room for the name once the frame, the icon and the badges are drawn.
    let fixed = chip_width(ui, snap, note.is_some())
        - ui.painter()
            .layout_no_wrap(
                name.clone(),
                FontId::new(size::BODY, theme::semibold()),
                Color32::WHITE,
            )
            .size()
            .x;
    let name_room = (max_w - fixed).max(24.0);
    let resp = Frame::NONE
        .fill(p.raised)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(theme::radius::M))
        .inner_margin(Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let icon_c = if detached {
                    p.warning
                } else if snap.head.target().is_none() {
                    p.text_3
                } else {
                    p.accent
                };
                icons::show(ui, Icon::Branch, 14.0, icon_c);
                let g = crate::views::truncated(
                    ui.painter(),
                    &name,
                    FontId::new(size::BODY, theme::semibold()),
                    if detached { p.warning } else { p.text_strong },
                    name_room,
                );
                let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                ui.painter()
                    .galley(r.min, g, if detached { p.warning } else { p.text_strong });
                for (b, tone) in &badges {
                    kit::badge(ui, b, *tone);
                }
                if let Some(n) = &note {
                    ui.label(RichText::new(n).size(size::SMALL).color(p.text_3));
                }
            });
        })
        .response;
    let tip = match snap.current_branch() {
        Some(b) => match &b.upstream {
            Some(u) => format!("{}\n{} ahead, {} behind {u}", b.name, b.ahead, b.behind),
            None => format!("{}\nNo upstream configured", b.name),
        },
        None => name,
    };
    resp.on_hover_text(tip);
}

/// Width of the commit search box drawn with `r` (frame, icon, input, and
/// the match counter or the Ctrl+F hint).
fn search_width(
    ui: &Ui,
    r: &RightFit,
    search: &str,
    hits: usize,
    cursor: usize,
    focused: bool,
) -> f32 {
    let mut w = 22.0 + 14.0 + 6.0 + r.search_w;
    if !search.trim().is_empty() {
        w += if hits > 0 {
            6.0 + text_w(ui, &format!("{} of {}", cursor + 1, hits), size::SMALL)
                + 6.0
                + 26.0
                + 6.0
                + 26.0
        } else {
            6.0 + text_w(ui, "no matches", size::SMALL)
        };
    } else if r.kbd && !focused {
        w += 6.0 + 52.0;
    }
    w
}

/// The commit search's id: fixed, so focus survives the toolbar changing
/// around it (and the width check can read it before it is drawn).
fn search_id() -> egui::Id {
    egui::Id::unique("commit_search")
}

fn search_field(
    ui: &mut Ui,
    fit: &RightFit,
    search: &mut String,
    hits: &[usize],
    cursor: usize,
    focus_search: &mut bool,
    actions: &mut Vec<Action>,
) {
    // A fixed id: the widgets around this one change with the query, and an
    // auto id would move (and drop focus) between keystrokes.
    let p = *pal(ui);
    let id = search_id();
    let focused = ui.memory(|m| m.has_focus(id));
    let has_query = !search.trim().is_empty();
    // Exactly as wide as its content: inside the right-to-left toolbar a
    // looser limit made the frame span the whole limit (empty space after
    // the Ctrl+F hint) and took room from the left side.
    let content_w = search_width(ui, fit, search, hits.len(), cursor, focused) - 22.0;
    let stroke = if focused { p.accent } else { p.border };
    Frame::NONE
        .fill(if ui.visuals().dark_mode {
            p.chrome
        } else {
            p.canvas
        })
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(CornerRadius::same(theme::radius::M))
        .inner_margin(Margin::symmetric(10, 3))
        .show(ui, |ui| {
            ui.set_max_width(ui.available_width().min(content_w));
            // Explicit: `horizontal` would inherit the toolbar's right-to-left layout.
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                icons::show(ui, Icon::Search, 14.0, p.text_3);
                let resp = ui.add(
                    egui::TextEdit::singleline(search)
                        .id(id)
                        .frame(Frame::NONE)
                        .margin(Margin::ZERO)
                        .align(egui::Align2::LEFT_CENTER)
                        .min_size(vec2(0.0, 24.0))
                        .hint_text(RichText::new("Search commits").color(p.text_3))
                        .desired_width(fit.search_w),
                );
                if *focus_search {
                    resp.request_focus();
                    *focus_search = false;
                }
                if resp.changed() {
                    actions.push(Action::SearchChanged);
                }
                if resp.has_focus() {
                    let (enter, shift) = ui.input_mut(|i| {
                        let shift = i.modifiers.shift;
                        (
                            i.consume_key(
                                if shift {
                                    Modifiers::SHIFT
                                } else {
                                    Modifiers::NONE
                                },
                                Key::Enter,
                            ),
                            shift,
                        )
                    });
                    if enter {
                        actions.push(Action::SearchStep(if shift { -1 } else { 1 }));
                    }
                    if ui.input(|i| i.key_pressed(Key::Escape)) {
                        search.clear();
                        actions.push(Action::SearchChanged);
                        resp.surrender_focus();
                    }
                }
                if has_query {
                    let n = hits.len();
                    if n > 0 {
                        ui.label(
                            RichText::new(format!("{} of {}", cursor + 1, n))
                                .size(size::SMALL)
                                .color(p.text_2),
                        );
                        if IconButton::icon(Icon::ChevronUp)
                            .compact()
                            .show(ui)
                            .on_hover_text("Previous match (Shift+Enter)")
                            .clicked()
                        {
                            actions.push(Action::SearchStep(-1));
                        }
                        if IconButton::icon(Icon::ChevronDown)
                            .compact()
                            .show(ui)
                            .on_hover_text("Next match (Enter)")
                            .clicked()
                        {
                            actions.push(Action::SearchStep(1));
                        }
                    } else {
                        ui.label(
                            RichText::new("no matches")
                                .size(size::SMALL)
                                .color(p.warning),
                        );
                    }
                } else if !focused && fit.kbd {
                    icons::kbd(ui, "Ctrl+F");
                }
            });
        });
}

fn pending_banner(ui: &mut Ui, repo: &RepoState, actions: &mut Vec<Action>) {
    let Some(snap) = repo.snapshot.as_ref() else {
        return;
    };
    let p = *pal(ui);
    let pending = snap.pending;
    let conflicts = snap.status.conflicted.len();
    let busy = repo.is_busy();
    egui::Panel::top("pending")
        .show_separator_line(false)
        .frame(
            // Opaque: under a top-level panel a translucent fill shows the
            // window's clear colour, which stays dark in the light theme.
            Frame::NONE
                .fill(
                    p.chrome
                        .lerp_to_gamma(p.warning, if ui.visuals().dark_mode { 0.12 } else { 0.14 }),
                )
                .inner_margin(Margin::symmetric(14, 6)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                icons::show(ui, Icon::Warning, 16.0, p.warning);
                ui.label(
                    RichText::new(format!("{} in progress", pending.label()))
                        .family(theme::semibold())
                        .color(p.text_strong),
                );
                if pending == Pending::Bisect {
                    ui.label(
                        RichText::new("mark commits good or bad on the Tools page").color(p.text_2),
                    );
                } else if conflicts > 0 {
                    ui.label(
                        RichText::new(format!(
                            "{} to resolve",
                            crate::format::plural(conflicts, "conflict", "conflicts")
                        ))
                        .color(p.warning),
                    );
                } else {
                    ui.label(RichText::new("all conflicts resolved").color(p.text_2));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if pending == Pending::Bisect {
                        // A bisect has nothing to continue: open it, or end it.
                        if IconButton::text("Reset bisect")
                            .danger()
                            .enabled(!busy)
                            .show(ui)
                            .on_hover_text("git bisect reset: go back to where the bisect started")
                            .clicked()
                        {
                            actions.push(Action::Run(GitOp::Abort(pending)));
                        }
                        if IconButton::new(Icon::Bug, "Open bisect")
                            .primary(true)
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Action::ShowPage(crate::state::Page::Workbench));
                            actions.push(
                                crate::devops::workbench::Act::SetTab(
                                    crate::devops::workbench::Tab::Bisect,
                                )
                                .into(),
                            );
                        }
                        return;
                    }
                    if IconButton::text("Abort")
                        .danger()
                        .enabled(!busy)
                        .show(ui)
                        .on_hover_text(format!("{} --abort", pending.label().to_lowercase()))
                        .clicked()
                    {
                        actions.push(Action::Run(GitOp::Abort(pending)));
                    }
                    if pending == Pending::Rebase
                        && IconButton::text("Skip commit")
                            .enabled(!busy)
                            .show(ui)
                            .on_hover_text("rebase --skip")
                            .clicked()
                    {
                        actions.push(Action::Run(GitOp::SkipRebaseStep));
                    }
                    if pending == Pending::ApplyMailbox
                        && IconButton::text("Skip patch")
                            .enabled(!busy)
                            .show(ui)
                            .on_hover_text("am --skip")
                            .clicked()
                    {
                        actions.push(Action::Run(GitOp::SkipMailboxPatch));
                    }
                    let cont = IconButton::new(Icon::Check, "Continue")
                        .primary(true)
                        .enabled(!busy && conflicts == 0)
                        .show(ui)
                        .on_hover_text(if conflicts == 0 {
                            "Finish the operation"
                        } else {
                            "Resolve all conflicts first"
                        });
                    if cont.clicked() {
                        actions.push(Action::Run(GitOp::Continue(pending)));
                    }
                });
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_names_share_the_strip() {
        // Everything fits: no cap.
        assert_eq!(tab_name_cap(&[80.0, 90.0], 1000.0), f32::INFINITY);
        // Too wide: the long names are cut to a common cap, the short one
        // keeps its width, and the total fits.
        let widths = [60.0, 300.0, 280.0];
        let room = 500.0;
        let cap = tab_name_cap(&widths, room);
        assert!((TAB_NAME_MIN..280.0).contains(&cap));
        let total: f32 = widths.iter().map(|w| w.min(cap) + TAB_CHROME).sum();
        assert!(total <= room + 0.5, "{total} > {room}");
        // Not even the minimum fits: the cap stays at the minimum (the
        // strip scrolls).
        assert_eq!(tab_name_cap(&[300.0; 12], 400.0), TAB_NAME_MIN);
    }

    #[test]
    fn toolbar_labels_go_before_overlap() {
        assert!(Fit::Full < Fit::NoNote && Fit::NoNote < Fit::Secondary);
        assert!(Fit::Secondary < Fit::Icons);
        assert_eq!(
            tool_tip("Fetch", true, "Fetch all remotes"),
            "Fetch all remotes"
        );
        assert_eq!(
            tool_tip("Fetch", false, "Fetch all remotes and prune"),
            "Fetch all remotes and prune",
            "a tip that already names the button is kept as it is"
        );
        assert_eq!(
            tool_tip("Pop", false, "Apply and drop"),
            "Pop\nApply and drop"
        );
        assert!(
            RIGHT_FITS
                .windows(2)
                .all(|w| w[0].search_w >= w[1].search_w)
        );
    }
}
