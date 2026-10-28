//! Toasts: short notices stacked in the bottom-left corner, next to the
//! rail and above the status bar. A click dismisses one.
//!
//! Bottom-left, not bottom-right: on the History page the bottom-right is
//! the WIP panel's Commit button (and the diff's stage buttons), which a
//! 12-second error toast used to cover. At most [`VISIBLE`] show at once;
//! everything they say is in the activity log too.

use egui::{Align2, CornerRadius, Frame, Label, Margin, Rect, RichText, Sense, Stroke, pos2, vec2};

use crate::state::{Toast, ToastKind};
use crate::theme::{Tone, palette, radius, semibold, size};

/// Width of a toast card.
const WIDTH: f32 = 340.0;
/// Space kept free at the bottom: the status bar (about 33 points) plus a
/// gap, so a toast never covers it.
const BOTTOM: f32 = 46.0;
/// Toasts shown at once (the newest); a burst no longer stacks up the
/// window.
const VISIBLE: usize = 3;

pub fn show(ctx: &egui::Context, toasts: &mut Vec<Toast>) {
    toasts.retain(|t| t.created.elapsed().as_secs_f32() < t.ttl_secs());
    if toasts.is_empty() {
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(500));
    let p = *palette(ctx.global_style().visuals.dark_mode);
    let shadow = ctx.global_style().visuals.popup_shadow;
    let first = toasts.len().saturating_sub(VISIBLE);

    let mut dismiss: Option<usize> = None;
    egui::Area::new(egui::Id::unique("toasts"))
        .anchor(
            Align2::LEFT_BOTTOM,
            vec2(crate::views::rail::WIDTH + 12.0, -BOTTOM),
        )
        .order(egui::Order::Foreground)
        .interactable(true)
        .show(ctx, |ui| {
            ui.set_width(WIDTH);
            ui.spacing_mut().item_spacing.y = 8.0;
            // Oldest first, so the newest sits next to the corner.
            for (i, t) in toasts.iter().enumerate().skip(first) {
                let tone = match t.kind {
                    ToastKind::Info => Tone::Accent,
                    ToastKind::Success => Tone::Success,
                    ToastKind::Error => Tone::Danger,
                };
                let stripe = tone.color(&p);
                let resp = Frame::NONE
                    .fill(p.overlay)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(CornerRadius::same(radius::L))
                    .inner_margin(Margin {
                        left: 18,
                        right: 14,
                        top: 10,
                        bottom: 11,
                    })
                    .shadow(shadow)
                    .show(ui, |ui| {
                        ui.set_width(WIDTH - 32.0);
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.add(
                            Label::new(
                                RichText::new(&t.title)
                                    .size(size::BODY)
                                    .family(semibold())
                                    .color(p.text_strong),
                            )
                            .wrap(),
                        );
                        if !t.body.is_empty() {
                            ui.add(
                                Label::new(
                                    RichText::new(&t.body).size(size::SMALL).color(p.text_2),
                                )
                                .wrap(),
                            );
                        }
                    })
                    .response;
                let r = resp.rect;
                ui.painter().rect_filled(
                    Rect::from_min_max(
                        pos2(r.left() + 1.0, r.top() + 1.0),
                        pos2(r.left() + 4.0, r.bottom() - 1.0),
                    ),
                    CornerRadius {
                        nw: radius::L - 1,
                        sw: radius::L - 1,
                        ne: 0,
                        se: 0,
                    },
                    stripe,
                );
                let resp = resp.interact(Sense::click());
                if resp.hovered() {
                    ui.painter().rect_stroke(
                        r,
                        CornerRadius::same(radius::L),
                        Stroke::new(1.0, p.border_strong),
                        egui::StrokeKind::Inside,
                    );
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    dismiss = Some(i);
                }
            }
        });
    if let Some(i) = dismiss {
        toasts.remove(i);
    }
}
