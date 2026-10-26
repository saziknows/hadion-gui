//! Modal dialogs. Each returns whether it should stay open.
//!
//! Layout: a semibold title, body text, explanations in the secondary
//! colour, inputs in the kit's input style, and the buttons right-aligned
//! at the bottom (the main action rightmost; destructive actions in the
//! danger colour; Cancel as a ghost button). Enter confirms where it
//! makes sense, Esc cancels.

use egui::{
    Align, Color32, CornerRadius, FontId, Frame, Key, Label, Layout, Margin, Response, RichText,
    Sense, Stroke, Ui, pos2, vec2,
};

use gitvis_core::Snapshot;
use gitvis_core::ops::{GitOp, MergeMode, ResetMode};

use crate::state::{Action, Dialog};
use crate::theme::{Tone, medium, pal, palette, radius, semibold, size, space};
use crate::views::icons::IconButton;
use crate::views::kit;

/// Content width of a dialog.
const WIDTH: f32 = 440.0;

pub fn show(
    ctx: &egui::Context,
    dialog: &mut Dialog,
    fresh: &mut bool,
    snap: Option<&Snapshot>,
    actions: &mut Vec<Action>,
) -> bool {
    let style = ctx.global_style();
    let p = palette(style.visuals.dark_mode);
    let frame = Frame::NONE
        .fill(p.overlay)
        .stroke(Stroke::new(1.0, p.border_strong))
        .corner_radius(CornerRadius::same(radius::L))
        .inner_margin(Margin {
            left: space::XL as i8,
            right: space::XL as i8,
            top: (space::L + 4.0) as i8,
            bottom: (space::L + 4.0) as i8,
        })
        .shadow(style.visuals.window_shadow);
    // Confirmations that show request bodies or commands get more room.
    let width = match dialog {
        Dialog::ConfirmAction { details, .. } if !details.is_empty() => 620.0,
        _ => WIDTH,
    };
    let modal = egui::Modal::new(egui::Id::unique("dialog"))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = space::S;
            body(ui, dialog, fresh, snap, actions)
        });
    modal.inner && !modal.should_close()
}

enum Outcome {
    Open,
    Confirm,
    Cancel,
}

fn title(ui: &mut Ui, text: &str) {
    let c = pal(ui).text_strong;
    ui.add(
        Label::new(
            RichText::new(text)
                .size(size::TITLE)
                .family(semibold())
                .color(c),
        )
        .wrap(),
    );
    ui.add_space(2.0);
}

/// Running text of a dialog.
fn text(ui: &mut Ui, s: &str) {
    let c = pal(ui).text;
    ui.add(Label::new(RichText::new(s).size(size::BODY).color(c)).wrap());
}

/// A secondary line: where something starts, what an option does.
fn explain(ui: &mut Ui, s: &str) {
    let c = pal(ui).text_2;
    ui.add(Label::new(RichText::new(s).size(size::SMALL).color(c)).wrap());
}

fn label(ui: &mut Ui, s: &str) {
    let c = pal(ui).text_2;
    ui.add_space(2.0);
    ui.label(RichText::new(s).size(size::SMALL).family(medium()).color(c));
}

/// A text input in the kit's style: sunken fill, hairline border that
/// turns accent while focused.
fn input(
    ui: &mut Ui,
    id_salt: &str,
    value: &mut String,
    hint: &str,
    rows: usize,
    width: f32,
) -> Response {
    let p = *pal(ui);
    let id = ui.make_persistent_id(("dialog_input", id_salt));
    let focused = ui.memory(|m| m.has_focus(id));
    Frame::NONE
        .fill(if ui.visuals().dark_mode {
            p.chrome
        } else {
            p.canvas
        })
        .stroke(Stroke::new(
            1.0,
            if focused { p.accent } else { p.border_strong },
        ))
        .corner_radius(CornerRadius::same(radius::M))
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            let edit = if rows > 1 {
                egui::TextEdit::multiline(value).desired_rows(rows)
            } else {
                // Centre single-line text vertically (egui puts it at the top).
                egui::TextEdit::singleline(value)
                    .margin(Margin::ZERO)
                    .align(egui::Align2::LEFT_CENTER)
                    .min_size(egui::vec2(0.0, 22.0))
            };
            ui.add(
                edit.id(id)
                    .frame(Frame::NONE)
                    .font(FontId::proportional(size::BODY))
                    .text_color(p.text)
                    .hint_text(RichText::new(hint).color(p.text_3))
                    .desired_width(width),
            )
        })
        .inner
}

/// A labelled single-line input; takes the focus when the dialog opens.
fn field(
    ui: &mut Ui,
    id_salt: &str,
    label_text: &str,
    value: &mut String,
    fresh: &mut bool,
    hint: &str,
) -> Response {
    label(ui, label_text);
    let resp = input(ui, id_salt, value, hint, 1, f32::INFINITY);
    if *fresh {
        resp.request_focus();
        *fresh = false;
    }
    resp
}

/// A checkbox whose box stays visible on the dialog surface (egui's own
/// box is filled with the input colour, which the overlay swallows).
fn checkbox(ui: &mut Ui, value: &mut bool, s: &str) -> Response {
    let p = *pal(ui);
    let g = ui.painter().layout_no_wrap(
        s.to_owned(),
        FontId::proportional(size::BODY),
        Color32::PLACEHOLDER,
    );
    let (rect, mut resp) =
        ui.allocate_exact_size(vec2(16.0 + 8.0 + g.size().x, 22.0), Sense::click());
    if resp.clicked() {
        *value = !*value;
        resp.mark_changed();
    }
    let hovered = resp.hovered();
    let bx =
        egui::Rect::from_center_size(pos2(rect.left() + 8.0, rect.center().y), vec2(16.0, 16.0));
    if *value {
        let fill = if hovered {
            p.accent.lerp_to_gamma(Color32::WHITE, 0.10)
        } else {
            p.accent
        };
        ui.painter()
            .rect_filled(bx, CornerRadius::same(radius::S), fill);
        crate::views::icons::paint(
            ui.painter(),
            crate::views::icons::Icon::Check,
            bx.center(),
            11.0,
            Color32::WHITE,
        );
    } else {
        ui.painter().rect(
            bx,
            CornerRadius::same(radius::S),
            if ui.visuals().dark_mode {
                p.chrome
            } else {
                p.canvas
            },
            Stroke::new(1.0, if hovered { p.text_3 } else { p.border_strong }),
            egui::StrokeKind::Inside,
        );
    }
    ui.painter().galley(
        pos2(bx.right() + 8.0, rect.center().y - g.size().y / 2.0),
        g,
        if hovered { p.text_strong } else { p.text },
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A filled button in the danger colour, sized like the kit's buttons.
fn danger_button(ui: &mut Ui, s: &str, enabled: bool) -> Response {
    let p = *pal(ui);
    let g = ui.painter().layout_no_wrap(
        s.to_owned(),
        FontId::new(size::BODY, medium()),
        Color32::WHITE,
    );
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 22.0, 30.0), sense);
    let fill = if !enabled {
        p.danger.gamma_multiply(0.38)
    } else if resp.is_pointer_button_down_on() {
        p.danger.lerp_to_gamma(Color32::BLACK, 0.15)
    } else if resp.hovered() {
        p.danger.lerp_to_gamma(Color32::WHITE, 0.10)
    } else {
        p.danger
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(radius::M), fill);
    ui.painter().galley(
        pos2(rect.left() + 11.0, rect.center().y - g.size().y / 2.0),
        g,
        Color32::WHITE,
    );
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// Right-aligned Cancel / Confirm buttons. Enter confirms when `enter_submits`.
fn buttons(
    ui: &mut Ui,
    confirm: &str,
    enabled: bool,
    danger: bool,
    enter_submits: bool,
) -> Outcome {
    let mut outcome = Outcome::Open;
    ui.add_space(space::S);
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = space::S;
            let btn = if danger {
                danger_button(ui, confirm, enabled)
            } else {
                IconButton::text(confirm)
                    .primary(true)
                    .enabled(enabled)
                    .show(ui)
            };
            if btn.clicked() {
                outcome = Outcome::Confirm;
            }
            if IconButton::text("Cancel").show(ui).clicked() {
                outcome = Outcome::Cancel;
            }
        });
    });
    if enabled && enter_submits && ui.input(|i| i.key_pressed(Key::Enter)) {
        outcome = Outcome::Confirm;
    }
    if ui.input(|i| i.key_pressed(Key::Escape)) {
        outcome = Outcome::Cancel;
    }
    outcome
}

fn error_line(ui: &mut Ui, err: &Option<String>) {
    if let Some(e) = err {
        let c = pal(ui).danger;
        ui.add(Label::new(RichText::new(e).size(size::SMALL).color(c)).wrap());
    }
}

/// A selectable card with a radio mark, a title and what it does.
fn choice(ui: &mut Ui, selected: bool, title_text: &str, detail: &str, danger: bool) -> bool {
    let p = *pal(ui);
    let resp = card(ui, title_text, selected, |ui, hovered| {
        let (mark, _) = ui.allocate_exact_size(vec2(16.0, 18.0), Sense::hover());
        let c = mark.center();
        let ring = if selected { p.accent } else { p.border_strong };
        ui.painter().circle_stroke(
            c,
            6.5,
            Stroke::new(1.5, if hovered && !selected { p.text_3 } else { ring }),
        );
        if selected {
            ui.painter().circle_filled(c, 3.5, p.accent);
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let tc = if danger && selected {
                p.danger
            } else if selected {
                p.text_strong
            } else {
                p.text
            };
            ui.label(
                RichText::new(title_text)
                    .size(size::BODY)
                    .family(medium())
                    .color(tc),
            );
            explain(ui, detail);
        });
    });
    resp.clicked()
}

/// An action card: clicking it runs the action.
fn option(ui: &mut Ui, title_text: &str, detail: &str) -> bool {
    let p = *pal(ui);
    card(ui, title_text, false, |ui, hovered| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(
                RichText::new(title_text)
                    .size(size::BODY)
                    .family(medium())
                    .color(if hovered { p.text_strong } else { p.text }),
            );
            explain(ui, detail);
        });
    })
    .clicked()
}

/// A full-width bordered card that reacts to hover and clicks.
fn card(ui: &mut Ui, id_salt: &str, selected: bool, add: impl FnOnce(&mut Ui, bool)) -> Response {
    let p = *pal(ui);
    let id = ui.make_persistent_id(("dialog_card", id_salt));
    let hovered = ui.ctx().read_response(id).is_some_and(|r| r.hovered());
    let (fill, stroke) = if selected {
        (p.selection, p.accent.gamma_multiply(0.6))
    } else if hovered {
        (p.hover, p.text_3)
    } else {
        (Color32::TRANSPARENT, p.border_strong)
    };
    let inner = Frame::NONE
        .fill(fill)
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(CornerRadius::same(radius::M))
        .inner_margin(Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                add(ui, hovered)
            });
        });
    let resp = ui.interact(inner.response.rect, id, Sense::click());
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// `https://host/user/repo.git` or `git@host:user/repo` -> `repo`.
pub fn repo_name_from_url(url: &str) -> String {
    let u = url.trim().trim_end_matches(['/', '\\']);
    let last = u.rsplit(['/', ':', '\\']).next().unwrap_or(u);
    last.strip_suffix(".git").unwrap_or(last).trim().to_owned()
}

pub fn validate_ref_name(name: &str, existing: &[&str]) -> Result<(), String> {
    let n = name.trim();
    if n.is_empty() {
        return Err(String::new());
    }
    if n != name {
        return Err("No leading or trailing spaces".into());
    }
    if n.chars()
        .any(|c| c.is_whitespace() || "~^:?*[\\".contains(c) || c.is_control())
    {
        return Err("Names cannot contain spaces or ~ ^ : ? * [ \\".into());
    }
    if n.contains("..") || n.contains("@{") || n == "@" || n.contains("//") {
        return Err("Invalid sequence in name".into());
    }
    if n.starts_with('-') || n.starts_with('/') || n.starts_with('.') {
        return Err("Names cannot start with - / or .".into());
    }
    if n.ends_with('/') || n.ends_with('.') || n.ends_with(".lock") {
        return Err("Names cannot end with / . or .lock".into());
    }
    if existing.iter().any(|e| e.eq_ignore_ascii_case(n)) {
        return Err(format!("{n} already exists"));
    }
    Ok(())
}

fn body(
    ui: &mut Ui,
    dialog: &mut Dialog,
    fresh: &mut bool,
    snap: Option<&Snapshot>,
    actions: &mut Vec<Action>,
) -> bool {
    let branch_names: Vec<&str> = snap
        .map(|s| s.branches.iter().map(|b| b.name.as_str()).collect())
        .unwrap_or_default();
    let tag_names: Vec<&str> = snap
        .map(|s| s.tags.iter().map(|t| t.name.as_str()).collect())
        .unwrap_or_default();

    let outcome = match dialog {
        Dialog::CreateBranch {
            name,
            start,
            start_label,
            checkout,
        } => {
            title(ui, "Create branch");
            explain(ui, &format!("Starts at {start_label}"));
            field(
                ui,
                "branch_name",
                "Branch name",
                name,
                fresh,
                "feature/my-change",
            );
            let valid = validate_ref_name(name, &branch_names);
            error_line(ui, &valid.as_ref().err().filter(|e| !e.is_empty()).cloned());
            checkbox(ui, checkout, "Check out the new branch");
            let o = buttons(ui, "Create branch", valid.is_ok(), false, true);
            if matches!(o, Outcome::Confirm) {
                actions.push(Action::Run(GitOp::CreateBranch {
                    name: name.trim().to_owned(),
                    start: start.clone(),
                    switch: *checkout,
                }));
            }
            o
        }
        Dialog::CreateTag {
            name,
            target,
            target_label,
            message,
        } => {
            title(ui, "Create tag");
            explain(ui, &format!("Points at {target_label}"));
            field(ui, "tag_name", "Tag name", name, fresh, "v1.2.0");
            let valid = validate_ref_name(name, &tag_names);
            error_line(ui, &valid.as_ref().err().filter(|e| !e.is_empty()).cloned());
            label(ui, "Message (optional, makes an annotated tag)");
            let msg = input(ui, "tag_message", message, "", 3, f32::INFINITY);
            let o = buttons(ui, "Create tag", valid.is_ok(), false, !msg.has_focus());
            if matches!(o, Outcome::Confirm) {
                actions.push(Action::Run(GitOp::CreateTag {
                    name: name.trim().to_owned(),
                    target: target.clone(),
                    message: Some(message.clone()).filter(|m| !m.trim().is_empty()),
                }));
            }
            o
        }
        Dialog::RenameBranch { from, to } => {
            title(ui, &format!("Rename {from}"));
            field(ui, "rename_to", "New name", to, fresh, "");
            let others: Vec<&str> = branch_names
                .iter()
                .copied()
                .filter(|b| *b != from.as_str())
                .collect();
            let valid = validate_ref_name(to, &others);
            let unchanged = to.trim() == from;
            error_line(ui, &valid.as_ref().err().filter(|e| !e.is_empty()).cloned());
            let o = buttons(ui, "Rename", valid.is_ok() && !unchanged, false, true);
            if matches!(o, Outcome::Confirm) {
                actions.push(Action::Run(GitOp::RenameBranch {
                    from: from.clone(),
                    to: to.trim().to_owned(),
                }));
            }
            o
        }
        Dialog::DeleteBranch {
            name,
            force,
            remote,
            delete_remote,
        } => {
            title(ui, &format!("Delete branch {name}?"));
            text(
                ui,
                "The branch is removed locally. Commits that are not reachable from another branch become unreachable.",
            );
            ui.add_space(2.0);
            checkbox(ui, force, "Force delete even if it is not fully merged");
            if let Some((r, b)) = remote {
                checkbox(
                    ui,
                    delete_remote,
                    &format!("Also delete {r}/{b} on the remote"),
                );
            }
            let o = buttons(ui, "Delete branch", true, true, true);
            if matches!(o, Outcome::Confirm) {
                let mut ops = vec![GitOp::DeleteBranch {
                    name: name.clone(),
                    force: *force,
                }];
                if let (Some((r, b)), true) = (remote.as_ref(), *delete_remote) {
                    ops.push(GitOp::DeleteRemoteBranch {
                        remote: r.clone(),
                        branch: b.clone(),
                    });
                }
                actions.push(Action::RunSeq(ops));
            }
            o
        }
        Dialog::Confirm {
            title: t,
            body,
            confirm,
            danger,
            ops,
        } => {
            title(ui, t);
            text(ui, body);
            let o = buttons(ui, confirm, true, *danger, true);
            if matches!(o, Outcome::Confirm) {
                actions.push(Action::RunSeq(ops.clone()));
            }
            o
        }
        Dialog::ConfirmAction {
            title: t,
            body,
            confirm,
            danger,
            details,
            action,
        } => {
            title(ui, t);
            if !body.is_empty() {
                text(ui, body);
            }
            if !details.is_empty() {
                let p = *pal(ui);
                ui.add_space(2.0);
                label(ui, "What gitvis will run");
                Frame::NONE
                    .fill(if ui.visuals().dark_mode {
                        p.chrome
                    } else {
                        p.canvas
                    })
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(CornerRadius::same(radius::M))
                    .inner_margin(Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        egui::ScrollArea::vertical()
                            .id_salt("confirm_details")
                            .max_height(260.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for d in details.iter() {
                                    ui.add(
                                        Label::new(
                                            RichText::new(d)
                                                .family(egui::FontFamily::Monospace)
                                                .size(size::MONO - 0.5)
                                                .color(p.text),
                                        )
                                        .wrap(),
                                    );
                                }
                            });
                    });
            }
            let o = buttons(ui, confirm, true, *danger, false);
            if matches!(o, Outcome::Confirm) {
                actions.push((**action).clone());
            }
            o
        }
        Dialog::Reset {
            branch,
            target,
            target_label,
            mode,
        } => {
            title(ui, &format!("Reset {branch} to {target_label}"));
            explain(ui, "What happens to the changes after that commit:");
            ui.add_space(2.0);
            ui.spacing_mut().item_spacing.y = 6.0;
            for (m, name, detail) in [
                (ResetMode::Soft, "Soft", "Keep all changes, staged"),
                (
                    ResetMode::Mixed,
                    "Mixed",
                    "Keep the changes in the working tree, unstaged",
                ),
                (ResetMode::Hard, "Hard", "Discard all changes"),
            ] {
                if choice(ui, *mode == m, name, detail, m == ResetMode::Hard) {
                    *mode = m;
                }
            }
            ui.spacing_mut().item_spacing.y = space::S;
            if *mode == ResetMode::Hard {
                ui.add_space(2.0);
                kit::callout(
                    ui,
                    Tone::Danger,
                    "",
                    "Hard reset throws away uncommitted work. This cannot be undone.",
                );
            }
            let o = buttons(ui, "Reset", true, *mode == ResetMode::Hard, true);
            if matches!(o, Outcome::Confirm) {
                actions.push(Action::Run(GitOp::Reset {
                    target: target.clone(),
                    mode: *mode,
                }));
            }
            o
        }
        Dialog::Push {
            branch,
            remotes,
            remote,
            set_upstream,
            force,
        } => {
            title(ui, &format!("Push {branch}"));
            if remotes.is_empty() {
                kit::callout(ui, Tone::Warning, "", "This repository has no remotes.");
                buttons(ui, "Push", false, false, false)
            } else {
                label(ui, "Remote");
                if remotes.len() <= 4 {
                    let opts: Vec<(usize, &str)> = remotes
                        .iter()
                        .enumerate()
                        .map(|(i, r)| (i, r.as_str()))
                        .collect();
                    if let Some(i) = kit::segmented(ui, &opts, *remote) {
                        *remote = i;
                    }
                } else {
                    egui::ComboBox::from_id_salt("push_remote")
                        .width(220.0)
                        .selected_text(remotes.get(*remote).cloned().unwrap_or_default())
                        .show_ui(ui, |ui| {
                            for (i, r) in remotes.iter().enumerate() {
                                ui.selectable_value(remote, i, r);
                            }
                        });
                }
                ui.add_space(2.0);
                checkbox(
                    ui,
                    set_upstream,
                    "Set as upstream (track this remote branch)",
                );
                checkbox(ui, force, "Force push (with lease)");
                if *force {
                    kit::callout(
                        ui,
                        Tone::Warning,
                        "",
                        "Overwrites the remote branch if nobody else pushed since your last fetch.",
                    );
                }
                let o = buttons(
                    ui,
                    if *force { "Force push" } else { "Push" },
                    true,
                    *force,
                    true,
                );
                if matches!(o, Outcome::Confirm) {
                    actions.push(Action::Run(GitOp::Push {
                        remote: remotes[*remote].clone(),
                        branch: branch.clone(),
                        set_upstream: *set_upstream,
                        force: *force,
                    }));
                }
                o
            }
        }
        Dialog::Stash {
            message,
            include_untracked,
        } => {
            title(ui, "Stash changes");
            field(
                ui,
                "stash_message",
                "Message (optional)",
                message,
                fresh,
                "What were you working on?",
            );
            checkbox(ui, include_untracked, "Include untracked files");
            let o = buttons(ui, "Stash", true, false, true);
            if matches!(o, Outcome::Confirm) {
                actions.push(Action::Run(GitOp::StashSave {
                    message: Some(message.clone()).filter(|m| !m.trim().is_empty()),
                    include_untracked: *include_untracked,
                }));
            }
            o
        }
        Dialog::Clone {
            url,
            dir,
            name,
            name_edited,
        } => {
            title(ui, "Clone repository");
            let url_resp = field(
                ui,
                "clone_url",
                "Repository URL",
                url,
                fresh,
                "https://github.com/user/repo.git",
            );
            if url_resp.changed() && !*name_edited {
                *name = repo_name_from_url(url);
            }
            label(ui, "Destination folder");
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::S;
                // The button's width (IconButton: 11 points of padding a
                // side) and the input frame's margins and border.
                let browse_w = ui
                    .painter()
                    .layout_no_wrap(
                        "Browse…".to_owned(),
                        FontId::new(size::BODY, medium()),
                        Color32::WHITE,
                    )
                    .size()
                    .x
                    + 22.0;
                input(
                    ui,
                    "clone_dir",
                    dir,
                    "",
                    1,
                    (ui.available_width() - browse_w - space::S - 22.0).max(120.0),
                );
                if IconButton::text("Browse…").show(ui).clicked()
                    && let Some(p) = rfd::FileDialog::new()
                        .set_title("Choose where to clone")
                        .set_directory(dir.as_str())
                        .pick_folder()
                {
                    *dir = p.display().to_string();
                }
            });
            label(ui, "Folder name");
            if input(ui, "clone_name", name, "", 1, f32::INFINITY).changed() {
                *name_edited = true;
            }
            let dest = std::path::Path::new(dir.trim()).join(name.trim());
            let err = if url.trim().is_empty() {
                Some(String::new())
            } else if dir.trim().is_empty() || !std::path::Path::new(dir.trim()).is_dir() {
                Some("Destination folder does not exist".to_owned())
            } else if name.trim().is_empty() {
                Some("Folder name is required".to_owned())
            } else if dest.exists() {
                Some(format!("{} already exists", dest.display()))
            } else {
                None
            };
            error_line(ui, &err.clone().filter(|e| !e.is_empty()));
            if err.is_none() {
                explain(ui, &format!("Creates {}", dest.display()));
            }
            let o = buttons(ui, "Clone", err.is_none(), false, true);
            if matches!(o, Outcome::Confirm) {
                actions.push(Action::Clone {
                    url: url.trim().to_owned(),
                    dest,
                });
            }
            o
        }
        Dialog::Drop {
            source,
            target,
            head_branch,
        } => {
            title(
                ui,
                &format!(
                    "{}  \u{2192}  {}",
                    source.name,
                    target
                        .local_branches
                        .first()
                        .map(String::as_str)
                        .unwrap_or(&target.short)
                ),
            );
            if source.target == target.commit {
                text(
                    ui,
                    "Both point at the same commit, so there is nothing to do.",
                );
                buttons(ui, "OK", true, false, true)
            } else {
                explain(ui, "Choose what to do:");
                ui.spacing_mut().item_spacing.y = 6.0;
                let mut chosen: Option<Vec<GitOp>> = None;
                let mut targets: Vec<&String> = target.local_branches.iter().collect();
                if let Some(h) = head_branch {
                    targets.sort_by_key(|b| *b != h);
                }
                for b in &targets {
                    if **b == source.name {
                        continue;
                    }
                    let is_head = head_branch.as_deref() == Some(b.as_str());
                    let label = format!("Merge {} into {b}", source.name);
                    let hint = if is_head {
                        "git merge".to_owned()
                    } else {
                        format!("Checks out {b} first, then merges")
                    };
                    if option(ui, &label, &hint) {
                        let mut ops = Vec::new();
                        if !is_head {
                            ops.push(GitOp::Switch {
                                branch: (*b).clone(),
                            });
                        }
                        ops.push(GitOp::Merge {
                            from: source.name.clone(),
                            mode: MergeMode::Default,
                        });
                        chosen = Some(ops);
                    }
                }
                if targets.is_empty()
                    && target.is_head
                    && option(
                        ui,
                        &format!("Merge {} into HEAD", source.name),
                        "Detached HEAD",
                    )
                {
                    chosen = Some(vec![GitOp::Merge {
                        from: source.name.clone(),
                        mode: MergeMode::Default,
                    }]);
                }
                if !source.is_remote {
                    let onto_label = targets
                        .first()
                        .map(|b| (*b).clone())
                        .unwrap_or_else(|| target.short.clone());
                    let onto = targets
                        .first()
                        .map(|b| (*b).clone())
                        .unwrap_or_else(|| target.commit.to_string());
                    if onto_label != source.name
                        && option(
                            ui,
                            &format!("Rebase {} onto {onto_label}", source.name),
                            &format!("Replays {} on top of {onto_label}", source.name),
                        )
                    {
                        chosen = Some(vec![GitOp::Rebase {
                            onto,
                            branch: Some(source.name.clone()),
                        }]);
                    }
                }
                ui.spacing_mut().item_spacing.y = space::S;
                if let Some(ops) = chosen {
                    actions.push(Action::RunSeq(ops));
                    Outcome::Confirm
                } else {
                    ui.add_space(space::S);
                    let mut o = Outcome::Open;
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if IconButton::text("Cancel").show(ui).clicked() {
                                o = Outcome::Cancel;
                            }
                        });
                    });
                    if ui.input(|i| i.key_pressed(Key::Escape)) {
                        o = Outcome::Cancel;
                    }
                    o
                }
            }
        }
    };
    matches!(outcome, Outcome::Open)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_urls() {
        assert_eq!(
            repo_name_from_url("https://github.com/user/repo.git"),
            "repo"
        );
        assert_eq!(repo_name_from_url("git@github.com:user/repo"), "repo");
        assert_eq!(repo_name_from_url("https://host/team/repo/"), "repo");
        assert_eq!(repo_name_from_url("C:\\src\\bare.git"), "bare");
        assert_eq!(repo_name_from_url("  "), "");
    }

    #[test]
    fn ref_name_rules() {
        assert!(validate_ref_name("feature/x", &["main"]).is_ok());
        assert!(validate_ref_name("main", &["main"]).is_err());
        assert!(validate_ref_name("bad name", &[]).is_err());
        assert!(validate_ref_name("-lead", &[]).is_err());
        assert!(validate_ref_name("a..b", &[]).is_err());
    }
}
