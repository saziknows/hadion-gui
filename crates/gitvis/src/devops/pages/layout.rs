//! Pane frames of the Ship, Govern and Tools pages: the list / detail split
//! and the scrolling page.
//!
//! They look like `kit::split_keyed` and `kit::scroll_page` and add two
//! things a narrow window needs: the content is never wider than its pane
//! (`Ui::set_max_width` sets the width, it can grow a pane past the window
//! edge), and the list pane gives way so the detail keeps a readable width.

use std::hash::Hash;

use egui::{Align, Frame, Label, Layout, Margin, RichText, Ui};

use crate::state::Action;
use crate::theme::{pal, semibold, size};
use crate::views::kit;

/// The narrowest a list pane gets.
pub const LIST_MIN: f32 = 240.0;
/// The width the detail pane keeps before the list pane gives way.
pub const DETAIL_MIN: f32 = 360.0;

/// Cap the width of `ui` at `max` without ever growing it past what it
/// has (`set_max_width` alone would).
pub fn cap_width(ui: &mut Ui, max: f32) {
    let w = ui.available_width().min(max);
    ui.set_max_width(w);
}

/// The list pane on the left, `default` points wide on a wide window. It
/// never takes so much that the detail gets under [`DETAIL_MIN`]: a
/// remembered width larger than that is clamped while the window is
/// narrow, and comes back when it is wide again.
pub fn list_pane(ui: &mut Ui, id: &str, default: f32, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    let avail = ui.available_width();
    let max = (avail - DETAIL_MIN).clamp(LIST_MIN, 760.0);
    let default = default.min(max).max(LIST_MIN.min(max));
    egui::Panel::left(id)
        .resizable(true)
        .default_size(default)
        .size_range(LIST_MIN.min(max)..=max)
        .frame(Frame::NONE.fill(p.panel).inner_margin(Margin {
            left: 8,
            right: 8,
            top: 12,
            bottom: 6,
        }))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui)
        });
}

/// The scrollable, padded detail pane, capped at `kit::DETAIL_MAX`.
pub fn detail_pane(ui: &mut Ui, salt: impl Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui)) {
    scroll(ui, salt, kit::DETAIL_MAX, 28, None, add);
}

/// A full-width scrolling page below a page header.
pub fn page(ui: &mut Ui, salt: impl Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui)) {
    scroll(ui, salt, kit::DETAIL_MAX + 120.0, 28, None, add);
}

/// [`page`] with a scroll offset to apply once (the script driver's
/// `ship-scroll N`).
pub fn page_at(
    ui: &mut Ui,
    salt: impl Hash + std::fmt::Debug,
    offset: &mut Option<f32>,
    add: impl FnOnce(&mut Ui),
) {
    scroll(ui, salt, kit::DETAIL_MAX + 120.0, 28, offset.take(), add);
}

fn scroll(
    ui: &mut Ui,
    salt: impl Hash + std::fmt::Debug,
    max: f32,
    side: i8,
    offset: Option<f32>,
    add: impl FnOnce(&mut Ui),
) {
    let p = *pal(ui);
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas))
        .show(ui, |ui| {
            let mut area = egui::ScrollArea::vertical()
                .id_salt(salt)
                .auto_shrink([false, false]);
            if let Some(o) = offset {
                area = area.vertical_scroll_offset(o);
            }
            area.show(ui, |ui| {
                Frame::NONE
                    .inner_margin(Margin {
                        left: side,
                        right: side,
                        top: 20,
                        bottom: 32,
                    })
                    .show(ui, |ui| {
                        cap_width(ui, max);
                        ui.spacing_mut().item_spacing.y = 8.0;
                        add(ui)
                    });
            });
        });
}

/// `kit::page_header` that fits a narrow window. The actions sit on the
/// right while the title keeps room beside them; when it would not (their
/// width is remembered from the last frame), they move to a row under the
/// title. The subtitle wraps. In `kit::page_header` a long subtitle pushes
/// the actions over the edge and widens the page below.
pub fn page_header(ui: &mut Ui, title: &str, subtitle: &str, actions_ui: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    Frame::NONE
        .inner_margin(Margin {
            left: 24,
            right: 16,
            top: 14,
            bottom: 10,
        })
        .show(ui, |ui| {
            let w = ui.available_width();
            let id = ui.make_persistent_id(("page_header_actions", title));
            let actions_w: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(0.0);
            let title_font = egui::FontId::new(size::HEADING, semibold());
            let title_w = ui
                .painter()
                .layout_no_wrap(title.to_owned(), title_font, egui::Color32::WHITE)
                .size()
                .x;
            // Room the title side needs: the title, and a readable subtitle.
            let need = title_w.max(if subtitle.is_empty() { 0.0 } else { 300.0 });
            let one_row = actions_w + 16.0 + need <= w;
            let text = |ui: &mut Ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add(
                    Label::new(
                        RichText::new(title)
                            .size(size::HEADING)
                            .family(semibold())
                            .color(p.text_strong),
                    )
                    .truncate(),
                );
                if !subtitle.is_empty() {
                    ui.add(
                        Label::new(RichText::new(subtitle).size(size::SMALL).color(p.text_2))
                            .wrap(),
                    );
                }
            };
            let measured = if one_row {
                // Title and one subtitle line: the height the actions
                // centre on.
                let h = if subtitle.is_empty() { 26.0 } else { 44.0 };
                ui.allocate_ui_with_layout(
                    egui::vec2(w, h),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        let r = ui
                            .scope(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                actions_ui(ui)
                            })
                            .response
                            .rect
                            .width();
                        ui.add_space(10.0);
                        ui.with_layout(Layout::top_down(Align::Min), text);
                        r
                    },
                )
                .inner
            } else {
                ui.with_layout(Layout::top_down(Align::Min), text);
                ui.add_space(6.0);
                // A row of their own, still right-aligned (they are written
                // right to left, the main action first).
                ui.allocate_ui_with_layout(
                    egui::vec2(w, 30.0),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        ui.scope(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            actions_ui(ui)
                        })
                        .response
                        .rect
                        .width()
                    },
                )
                .inner
            };
            if (measured - actions_w).abs() > 0.5 {
                ui.data_mut(|d| d.insert_temp(id, measured));
                ui.ctx().request_repaint();
            }
        });
    kit::hairline(ui);
}

/// `kit::split_keyed` that stays inside a narrow window: list pane on the
/// left (see [`list_pane`]), scrollable detail on the right whose scroll
/// position is kept per `key`.
pub fn split_keyed(
    ui: &mut Ui,
    id: &str,
    key: &str,
    actions: &mut Vec<Action>,
    list_ui: impl FnOnce(&mut Ui, &mut Vec<Action>),
    detail_ui: impl FnOnce(&mut Ui, &mut Vec<Action>),
) {
    split_keyed_at(ui, id, key, None, actions, list_ui, detail_ui);
}

/// [`split_keyed`] with a scroll offset for the detail, applied once (the
/// script driver's `ship-scroll N`).
pub fn split_keyed_at(
    ui: &mut Ui,
    id: &str,
    key: &str,
    offset: Option<f32>,
    actions: &mut Vec<Action>,
    list_ui: impl FnOnce(&mut Ui, &mut Vec<Action>),
    detail_ui: impl FnOnce(&mut Ui, &mut Vec<Action>),
) {
    list_pane(ui, id, 420.0, |ui| list_ui(ui, actions));
    scroll(ui, (id, "detail", key), kit::DETAIL_MAX, 28, offset, |ui| {
        detail_ui(ui, actions)
    });
}
