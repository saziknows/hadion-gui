//! Repository tabs and the git toolbar.
//!
//! The toolbar holds git actions only (pull, push, fetch, branch, stash,
//! undo), the current branch, and commit search on the history page; page
//! navigation lives in the rail.

use egui::{
    Align, Color32, CornerRadius, FontId, Frame, Key, Layout, Margin, Modifiers, Rect, RichText,
    Sense, Stroke, Ui, pos2, vec2,
};

use gitvis_core::ops::GitOp;
use gitvis_core::{Head, Pending, Snapshot};

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
    undo: Option<&gitvis_core::reflog::Undo>,
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
        for (i, repo) in repos.iter().enumerate() {
            let is_active = i == active;
            let font = if is_active {
                FontId::new(size::BODY - 0.5, theme::medium())
            } else {
                FontId::proportional(size::BODY - 0.5)
            };
            // Measured in the active (medium) weight for every tab, so the
            // strip does not shift when the active tab changes.
            let label_w = ui
                .painter()
                .layout_no_wrap(
                    repo.name.clone(),
                    FontId::new(size::BODY - 0.5, theme::medium()),
                    Color32::WHITE,
                )
                .size()
                .x;
            let w = label_w + 52.0;
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
            painter.text(
                pos2(rect.left() + 16.0, tab_rect.center().y),
                egui::Align2::LEFT_CENTER,
                &repo.name,
                font,
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
                ui.set_min_width(240.0);
                if ui.button("Open repository…        Ctrl+O").clicked() {
                    actions.push(Action::PickRepo);
                    ui.close();
                }
                if ui.button("Clone repository…   Ctrl+Shift+O").clicked() {
                    actions.push(Action::CloneDialog);
                    ui.close();
                }
            });
    });
}

fn toolbar_row(
    ui: &mut Ui,
    repo: &mut RepoState,
    settings: &Settings,
    undo: Option<&gitvis_core::reflog::Undo>,
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

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;

        // Pull, with a dropdown for the strategy.
        let pull_enabled = !busy && has_remotes && current.is_some();
        let pull = IconButton::new(Icon::Pull, "Pull")
            .enabled(pull_enabled)
            .show(ui)
            .on_hover_text(settings.pull.label());
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
        ui.add_space(2.0);

        // Push: becomes the primary action once there is something to push.
        let push_enabled = !busy && has_remotes && current.is_some() && !detached;
        let ahead = current.map(|b| b.ahead).unwrap_or(0);
        let push_label = if ahead > 0 {
            format!("Push {ahead}")
        } else {
            "Push".to_owned()
        };
        let push = IconButton::new(Icon::Push, &push_label)
            .primary(push_enabled && ahead > 0)
            .enabled(push_enabled)
            .show(ui)
            .on_hover_text(match current {
                Some(b) if b.upstream_gone => {
                    format!("Upstream of {} is gone: choose a remote", b.name)
                }
                Some(b) => match &b.upstream {
                    Some(u) => format!("Push {} to {u}", b.name),
                    None => format!("{} has no upstream yet: choose a remote", b.name),
                },
                None => "Nothing to push".into(),
            });
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
            ui.add_space(2.0);
            let pr = IconButton::new(Icon::PullRequest, "Pull request")
                .enabled(current.is_some())
                .show(ui)
                .on_hover_text("Open a pull request from the current branch");
            if pr.clicked() {
                actions.push(Action::ShowPage(Page::Review));
                actions.push(crate::devops::review::Act::NewPr.into());
            }
        }
        ui.add_space(2.0);
        if IconButton::new(Icon::Fetch, "Fetch")
            .enabled(!busy && has_remotes)
            .show(ui)
            .on_hover_text("Fetch all remotes and prune")
            .clicked()
        {
            actions.push(Action::Run(GitOp::Fetch));
        }

        ui.add_space(8.0);
        icons::divider(ui, 18.0);
        ui.add_space(8.0);

        if let Some(snap) = snap {
            let head = snap.head.target();
            if IconButton::new(Icon::Branch, "Branch")
                .enabled(!busy && head.is_some())
                .show(ui)
                .on_hover_text("Create a branch at HEAD")
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
        if IconButton::new(Icon::Stash, "Stash")
            .enabled(!busy && dirty)
            .show(ui)
            .on_hover_text("Stash all changes")
            .clicked()
        {
            actions.push(Action::Dialog(Dialog::Stash {
                message: String::new(),
                include_untracked: true,
            }));
        }
        if IconButton::new(Icon::Pop, "Pop")
            .enabled(!busy && has_stash)
            .show(ui)
            .on_hover_text("Apply and drop the newest stash")
            .clicked()
        {
            actions.push(Action::Run(GitOp::StashPop { index: 0 }));
        }

        ui.add_space(8.0);
        icons::divider(ui, 18.0);
        ui.add_space(8.0);
        let undo_btn = IconButton::new(Icon::Undo, "Undo")
            .enabled(!busy && undo.is_some())
            .show(ui)
            .on_hover_text(match undo {
                Some(u) => format!("{}  (Ctrl+Z)\n{}", u.label, u.detail),
                None => "Nothing to undo".to_owned(),
            });
        if undo_btn.clicked() {
            actions.push(Action::Undo);
        }

        ui.add_space(16.0);
        if let Some(snap) = snap {
            branch_chip(ui, snap);
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if *page == Page::Graph {
                search_field(
                    ui,
                    search,
                    search_hits,
                    *search_cursor,
                    focus_search,
                    actions,
                );
                crate::devops::history::toolbar_button(ui, &devops.history, actions);
            }
            if let Some(label) = busy_label.as_deref() {
                ui.label(RichText::new(label).size(size::SMALL).color(p.text_2));
                ui.add(egui::Spinner::new().size(13.0).color(p.accent));
            } else if *loading {
                ui.add(egui::Spinner::new().size(13.0).color(p.text_3));
            }
        });
    });
}

fn branch_chip(ui: &mut Ui, snap: &Snapshot) {
    let p = *pal(ui);
    let resp = Frame::NONE
        .fill(p.raised)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(theme::radius::M))
        .inner_margin(Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                match &snap.head {
                    Head::Branch { name, .. } => {
                        icons::show(ui, Icon::Branch, 14.0, p.accent);
                        ui.label(
                            RichText::new(name)
                                .family(theme::semibold())
                                .color(p.text_strong),
                        );
                        if let Some(b) = snap.current_branch() {
                            if b.upstream_gone {
                                kit::badge(ui, "upstream gone", Tone::Warning);
                            } else {
                                if b.ahead > 0 {
                                    kit::badge(ui, &format!("{} ahead", b.ahead), Tone::Success);
                                }
                                if b.behind > 0 {
                                    kit::badge(ui, &format!("{} behind", b.behind), Tone::Warning);
                                }
                                if b.ahead == 0
                                    && b.behind == 0
                                    && let Some(u) = &b.upstream
                                {
                                    ui.label(
                                        RichText::new(format!("up to date with {u}"))
                                            .size(size::SMALL)
                                            .color(p.text_3),
                                    );
                                }
                            }
                        }
                    }
                    Head::Detached(oid) => {
                        icons::show(ui, Icon::Branch, 14.0, p.warning);
                        ui.label(RichText::new("detached").color(p.warning));
                        ui.label(kit::mono(gitvis_core::repo::short_id(*oid)).color(p.text_strong));
                    }
                    Head::Unborn(name) => {
                        icons::show(ui, Icon::Branch, 14.0, p.text_3);
                        ui.label(
                            RichText::new(name)
                                .family(theme::semibold())
                                .color(p.text_strong),
                        );
                        ui.label(
                            RichText::new("no commits yet")
                                .size(size::SMALL)
                                .color(p.text_3),
                        );
                    }
                }
            });
        })
        .response;
    if let Some(b) = snap.current_branch() {
        let tip = match &b.upstream {
            Some(u) => format!("{}\n{} ahead, {} behind {u}", b.name, b.ahead, b.behind),
            None => format!("{}\nNo upstream configured", b.name),
        };
        resp.on_hover_text(tip);
    }
}

fn search_field(
    ui: &mut Ui,
    search: &mut String,
    hits: &[usize],
    cursor: usize,
    focus_search: &mut bool,
    actions: &mut Vec<Action>,
) {
    // A fixed id: the widgets around this one change with the query, and an
    // auto id would move (and drop focus) between keystrokes.
    let p = *pal(ui);
    let id = ui.make_persistent_id("commit_search");
    let focused = ui.memory(|m| m.has_focus(id));
    let has_query = !search.trim().is_empty();
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
            ui.set_max_width(300.0);
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
                        .desired_width(170.0),
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
                } else if !focused {
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
            Frame::NONE
                .fill(
                    p.warning
                        .gamma_multiply(if ui.visuals().dark_mode { 0.12 } else { 0.14 }),
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
