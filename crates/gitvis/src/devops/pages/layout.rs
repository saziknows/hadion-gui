//! Pane frames of the Ship, Govern and Tools pages: the list / detail split
//! and the scrolling page.
//!
//! They look like `kit::split_keyed` and `kit::scroll_page` and add two
//! things a narrow window needs: the content is never wider than its pane
//! (`Ui::set_max_width` sets the width, it can grow a pane past the window
//! edge), and the list pane gives way so the detail keeps a readable width.

use std::hash::Hash;

use egui::{Frame, Margin, Ui};

use crate::state::Action;
use crate::theme::pal;
use crate::views::kit;

/// A scroll offset for the next scrolling frame drawn (the script driver's
/// `ship-scroll N`, which works on every page built from these frames).
static PENDING_SCROLL: std::sync::Mutex<Option<f32>> = std::sync::Mutex::new(None);

/// Scroll the next detail pane or page drawn to `y`.
pub fn scroll_next(y: f32) {
    if let Ok(mut s) = PENDING_SCROLL.lock() {
        *s = Some(y.max(0.0));
    }
}

fn take_pending_scroll() -> Option<f32> {
    PENDING_SCROLL.lock().ok().and_then(|mut s| s.take())
}

/// `kit::page_with_nav` with a narrower navigation column in a narrow
/// window (its labels still fit), so the content keeps more of the room:
/// 196 points below 1100, as on main.
pub fn page_with_nav(
    ui: &mut Ui,
    id: &str,
    nav: impl FnOnce(&mut Ui),
    content: impl FnOnce(&mut Ui),
) {
    let p = *pal(ui);
    let nav_w = if ui.available_width() < 1100.0 {
        196.0
    } else {
        228.0
    };
    egui::Panel::left(id)
        .resizable(false)
        .exact_size(nav_w)
        .frame(Frame::NONE.fill(p.panel).inner_margin(Margin {
            left: 8,
            right: 8,
            top: 12,
            bottom: 8,
        }))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            egui::ScrollArea::vertical()
                .id_salt((id, "nav"))
                .auto_shrink([false, false])
                .show(ui, nav);
        });
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas))
        .show(ui, content);
}

/// Content that needs `min_w` points, such as a table with many columns:
/// drawn as it is when the pane is wide enough, else in a horizontal
/// scroll area at `min_w`, so the columns keep readable widths instead of
/// being squeezed (their headers were drawn over each other).
pub fn wide<R>(
    ui: &mut Ui,
    id: impl Hash + std::fmt::Debug,
    min_w: f32,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    if ui.available_width() >= min_w {
        return add(ui);
    }
    egui::ScrollArea::horizontal()
        .id_salt(("wide", id))
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.set_min_width(min_w);
            ui.set_max_width(min_w);
            add(ui)
        })
        .inner
}

/// The `width` for a `form::input` of at most `max` points that still fits
/// what is left of the row (the input's frame adds 22 points to it).
pub fn input_width(ui: &Ui, max: f32) -> f32 {
    (ui.available_width() - 24.0).clamp(60.0, max)
}

/// The narrowest a list pane gets.
pub const LIST_MIN: f32 = 240.0;

/// Cap the width of `ui` at `max` without ever growing it past what it
/// has (`set_max_width` alone would).
pub fn cap_width(ui: &mut Ui, max: f32) {
    let w = ui.available_width().min(max);
    ui.set_max_width(w);
}

/// The list pane on the left, sized from the room like `kit::split_keyed`
/// on main: 40% of it on a first show (260 to 420 points, and not more
/// than `default`), and never more than 55% (260 to 760), so the detail
/// keeps a readable width in a narrow window.
pub fn list_pane(ui: &mut Ui, id: &str, default: f32, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    let avail = ui.available_width();
    let max = (avail * 0.55).clamp(260.0, 760.0);
    let default = default.min((avail * 0.4).clamp(260.0, 420.0)).min(max);
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
    scroll(ui, salt, kit::DETAIL_MAX, add);
}

/// A full-width scrolling page below a page header.
pub fn page(ui: &mut Ui, salt: impl Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui)) {
    scroll(ui, salt, kit::DETAIL_MAX + 120.0, add);
}

/// A scrolling page for an empty, loading or error state: not capped, so
/// a centred `kit::empty_state` is centred in the whole area (callouts cap
/// their own width).
pub fn state_page(ui: &mut Ui, salt: impl Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui)) {
    scroll(ui, salt, f32::INFINITY, add);
}

fn scroll(ui: &mut Ui, salt: impl Hash + std::fmt::Debug, max: f32, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    let side = 28;
    let offset = take_pending_scroll();
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

/// The badges of a list row that fit beside its subtitle: `kit::paint_row`
/// draws them right to left without a bound, so in a narrow list pane they
/// ran over the subtitle and out of the row's left edge. Badges come in
/// order of importance; the ones that do not fit are replaced by "+N"
/// (the detail pane shows them all).
pub fn fit_badges(ui: &Ui, row_w: f32, badges: &[(String, kit::Tone)]) -> Vec<(String, kit::Tone)> {
    // Room left of the badges: the row's padding and leading mark, and at
    // least ~45% of the rest for the subtitle.
    let room = ((row_w - 48.0) * 0.55).max(0.0);
    let painter = ui.painter();
    let gap = 4.0;
    let width = |b: &[(String, kit::Tone)]| -> f32 {
        b.iter()
            .map(|(t, _)| kit::badge_width(painter, t) + gap)
            .sum()
    };
    if width(badges) <= room {
        return badges.to_vec();
    }
    let mut kept: Vec<(String, kit::Tone)> = Vec::new();
    for (i, b) in badges.iter().enumerate() {
        let more = format!("+{}", badges.len() - i - 1);
        let mut next = kept.clone();
        next.push(b.clone());
        let tail = if i + 1 < badges.len() {
            kit::badge_width(painter, &more) + gap
        } else {
            0.0
        };
        if width(&next) + tail > room {
            break;
        }
        kept = next;
    }
    let dropped = badges.len() - kept.len();
    if dropped > 0 {
        let more = format!("+{dropped}");
        if width(&kept) + kit::badge_width(painter, &more) + gap <= room {
            kept.push((more, kit::Tone::Neutral));
        }
    }
    kept
}

/// `kit::segmented` on a raised surface (a card, a plan row). In the light
/// theme the control's track is the raised colour too and disappears, so
/// it gets a hairline outline there.
pub fn segmented_on_card<T: Copy + PartialEq>(
    ui: &mut Ui,
    options: &[(T, &str)],
    value: T,
) -> Option<T> {
    if ui.visuals().dark_mode {
        return kit::segmented(ui, options, value);
    }
    let p = *pal(ui);
    Frame::NONE
        .stroke(egui::Stroke::new(1.0, p.border_strong))
        .corner_radius(egui::CornerRadius::same(crate::theme::radius::M))
        .show(ui, |ui| kit::segmented(ui, options, value))
        .inner
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
    list_pane(ui, id, 420.0, |ui| list_ui(ui, actions));
    detail_pane(ui, (id, "detail", key), |ui| detail_ui(ui, actions));
}
