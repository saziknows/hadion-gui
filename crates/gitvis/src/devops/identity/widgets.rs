//! Pieces the four identity sections share: the section header with a
//! refresh button and the age of the data, scope badges, the missing-tool
//! and error cards, copyable values.

use std::time::Instant;

use egui::{Label, RichText, Sense, Ui};
use gitvis_core::devops::identity::Scope;

use crate::state::Action;
use crate::theme::{pal, size};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Tone};

/// The tone of a scope's badge: global is yours, local belongs to the
/// repository, command-line values are unusual.
pub fn scope_tone(scope: Scope) -> Tone {
    match scope {
        Scope::Global => Tone::Accent,
        Scope::Local | Scope::Worktree => Tone::Purple,
        Scope::Command => Tone::Warning,
        Scope::System | Scope::Unknown => Tone::Neutral,
    }
}

pub fn scope_badge(ui: &mut Ui, scope: Scope) {
    kit::badge(ui, scope.title(), scope_tone(scope));
}

/// `just now`, `12 s ago`, `4 min ago`.
pub fn age(at: Instant) -> String {
    let s = at.elapsed().as_secs();
    if s < 5 {
        "just now".to_owned()
    } else if s < 60 {
        format!("{s} s ago")
    } else if s < 3600 {
        format!("{} min ago", s / 60)
    } else {
        format!("{} h ago", s / 3600)
    }
}

/// The section header: title, subtitle, the age of the data and a refresh
/// button; `extra` adds widgets left of them (right-to-left order).
/// Returns true when refresh was clicked.
pub fn header(
    ui: &mut Ui,
    title: &str,
    subtitle: &str,
    loading: bool,
    at: Option<Instant>,
    extra: impl FnOnce(&mut Ui),
) -> bool {
    let mut refresh = false;
    kit::page_header(ui, title, subtitle, |ui| {
        refresh = IconButton::new(Icon::Rerun, "Refresh")
            .compact()
            .enabled(!loading)
            .show(ui)
            .clicked();
        if loading {
            ui.add(egui::Spinner::new().size(13.0).color(pal(ui).text_2));
        } else if let Some(at) = at {
            ui.label(
                RichText::new(format!("Read {}", age(at)))
                    .size(size::SMALL)
                    .color(pal(ui).text_3),
            );
        }
        ui.add_space(6.0);
        extra(ui);
    });
    refresh
}

/// A tool that is not installed: its name, why it is needed, how to get
/// it, and "Check again". Returns true when that was clicked.
pub fn missing_tool(ui: &mut Ui, name: &str, program: &str, why: &str, hint: &str) -> bool {
    let mut clicked = false;
    kit::callout_with(
        ui,
        Tone::Warning,
        &format!("{name} is not installed"),
        &format!("{why} gitvis looked for `{program}` on PATH and did not find it."),
        |ui| {
            ui.add_space(2.0);
            ui.add(
                Label::new(
                    RichText::new(format!("Install: {hint}"))
                        .size(size::SMALL)
                        .color(pal(ui).text),
                )
                .wrap(),
            );
            ui.add_space(4.0);
            clicked = IconButton::new(Icon::Rerun, "Check again")
                .compact()
                .show(ui)
                .clicked();
        },
    );
    clicked
}

/// An error with the exact message and a retry button. Returns true when
/// retry was clicked.
pub fn error_card(ui: &mut Ui, title: &str, error: &str, retry: &str) -> bool {
    let mut clicked = false;
    kit::callout_with(ui, Tone::Danger, title, error, |ui| {
        ui.add_space(4.0);
        clicked = IconButton::new(Icon::Rerun, retry)
            .compact()
            .show(ui)
            .clicked();
    });
    clicked
}

/// A monospace value that copies itself when clicked.
pub fn copyable(ui: &mut Ui, text: &str, actions: &mut Vec<Action>) {
    let c = pal(ui).text;
    let resp = ui
        .add(
            Label::new(kit::mono(text).color(c))
                .sense(Sense::click())
                .wrap(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.clicked() {
        actions.push(Action::Copy(text.to_owned()));
    }
}

/// A row of buttons that wraps.
pub fn buttons(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        add(ui)
    });
}

/// A row of badges that wraps.
pub fn badges(ui: &mut Ui, list: &[(String, Tone)]) {
    if list.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        for (t, tone) in list {
            kit::badge(ui, t, *tone);
        }
    });
}

/// A label in the form style above a value or control.
pub fn label(ui: &mut Ui, text: &str) {
    let c = pal(ui).text_2;
    ui.label(
        RichText::new(text)
            .size(size::SMALL)
            .family(crate::theme::medium())
            .color(c),
    );
}

/// `kit::scroll_page` with an optional one-shot scroll offset (the
/// `id-scroll` script command, so long sections can be screenshotted).
pub fn page(ui: &mut Ui, id: &str, scroll: &mut Option<f32>, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(p.canvas))
        .show(ui, |ui| {
            let mut area = egui::ScrollArea::vertical()
                .id_salt(id)
                .auto_shrink([false, false]);
            if let Some(y) = scroll.take() {
                area = area.vertical_scroll_offset(y);
            }
            area.show(ui, |ui| {
                egui::Frame::NONE
                    .inner_margin(egui::Margin {
                        left: 28,
                        right: 28,
                        top: 20,
                        bottom: 32,
                    })
                    .show(ui, |ui| {
                        ui.set_max_width(kit::DETAIL_MAX + 120.0);
                        ui.spacing_mut().item_spacing.y = 8.0;
                        add(ui)
                    });
            });
        });
}

/// `.gitconfig:7`: the file name and line of an entry (the full path is
/// on the Git config page).
pub fn short_place(e: &gitvis_core::devops::identity::config::Entry) -> String {
    match (&e.file, e.line) {
        (Some(f), l) => {
            let name = f
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| f.to_string_lossy().into_owned());
            match l {
                Some(l) => format!("{name}:{l}"),
                None => name,
            }
        }
        (None, _) => e.origin.label(),
    }
}
