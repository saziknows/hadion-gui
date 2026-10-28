//! Explanations on the Ship, Govern and Tools pages, built to be read
//! quickly: a plain lead sentence (`kit::body`), definitions as rows
//! ([`terms`]), the method behind a number folded away ([`details`]) and
//! lists with a hanging indent ([`bullets`]).
//!
//! The pages give a lot of depth (exact commands, rules, thresholds); the
//! depth stays, but behind a closed [`details`] so the page reads as a few
//! short lines first.

use std::sync::atomic::{AtomicBool, Ordering};

use egui::{Align, CornerRadius, Label, Layout, RichText, Sense, Ui, vec2};

use crate::theme::{medium, pal, radius, size};
use crate::views::icons::{self, Icon};
use crate::views::kit;

/// Script override: every [`details`] drawn open (`explain open`), for
/// screenshots of what is folded away.
static OPEN_ALL: AtomicBool = AtomicBool::new(false);

/// Draw every [`details`] open (or back to their own state).
pub fn set_open_all(open: bool) {
    OPEN_ALL.store(open, Ordering::Relaxed);
}

/// A row that folds `add` away until it is clicked: a chevron and `title`
/// (e.g. "How this is computed"). Closed unless `open_by_default`; the
/// state is kept per `id`. Returns whether it is open.
pub fn details(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    title: &str,
    open_by_default: bool,
    add: impl FnOnce(&mut Ui),
) -> bool {
    let p = *pal(ui);
    let id = ui.make_persistent_id(("explain_details", id));
    let mut open = ui.data_mut(|d| *d.get_temp_mut_or(id, open_by_default));
    let font = egui::FontId::new(size::SMALL + 0.5, medium());
    let g = ui
        .painter()
        .layout_no_wrap(title.to_owned(), font, egui::Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 34.0, 28.0), Sense::click());
    let hovered = resp.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(radius::M), p.hover);
    }
    let fg = if hovered { p.text_strong } else { p.text };
    let shown = open || OPEN_ALL.load(Ordering::Relaxed);
    icons::paint(
        ui.painter(),
        if shown {
            Icon::ChevronUp
        } else {
            Icon::ChevronDown
        },
        egui::pos2(rect.left() + 13.0, rect.center().y),
        13.0,
        if hovered { p.text_strong } else { p.text_2 },
    );
    ui.painter().galley(
        egui::pos2(rect.left() + 26.0, rect.center().y - g.size().y / 2.0),
        g,
        fg,
    );
    if resp
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        open = !open;
        ui.data_mut(|d| d.insert_temp(id, open));
    }
    let shown = open || OPEN_ALL.load(Ordering::Relaxed);
    if shown {
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.horizontal_top(|ui| {
                ui.add_space(26.0);
                ui.vertical(add);
            });
        });
    }
    shown
}

/// Definitions: the term in a column on the left, its explanation on the
/// right, one row each; a `\n` in an explanation starts a new line. On a
/// narrow pane the term goes above its explanation.
pub fn terms(ui: &mut Ui, term_w: f32, rows: &[(&str, &str)]) {
    let p = *pal(ui);
    let stacked = ui.available_width() < term_w + 280.0;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = if stacked { 4.0 } else { 9.0 };
        for (term, text) in rows {
            let term_rt = RichText::new(*term)
                .size(size::SMALL + 0.5)
                .family(medium())
                .color(p.text_strong);
            let lines = |ui: &mut Ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                for line in text.split('\n') {
                    ui.add(
                        Label::new(RichText::new(line).size(size::SMALL + 0.5).color(p.text))
                            .wrap(),
                    );
                }
            };
            if stacked {
                ui.add(Label::new(term_rt).wrap());
                lines(ui);
                ui.add_space(6.0);
                continue;
            }
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.allocate_ui_with_layout(
                    vec2(term_w, 18.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.set_width(term_w - 16.0);
                        ui.add(Label::new(term_rt).wrap());
                    },
                );
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    let w = ui.available_width().min(kit::PROSE);
                    ui.set_max_width(w);
                    lines(ui)
                });
            });
        }
    });
}

/// Secondary text for a row of other widgets. `kit::secondary` and
/// `kit::muted` keep the full width from where they start, so after other
/// widgets on a wrapping row they run past the pane (and widen everything
/// below); this wraps with the row instead.
pub fn note(ui: &mut Ui, text: &str) -> egui::Response {
    let c = pal(ui).text_2;
    ui.add(Label::new(RichText::new(text).size(size::SMALL).color(c)).wrap())
}

/// [`note`] in the quiet colour (counts, "none", timestamps).
pub fn quiet(ui: &mut Ui, text: &str) -> egui::Response {
    let c = pal(ui).text_3;
    ui.add(Label::new(RichText::new(text).size(size::SMALL).color(c)).wrap())
}

/// A paragraph with each sentence on its own line, so an explanation built
/// as one paragraph reads as a few short facts ("… role. Your role …"
/// breaks after "role."; "e.g. Alpine" and "1.2.3" do not).
pub fn one_per_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if matches!(c, '.' | '!' | '?') {
            let mut ahead = chars.clone();
            if ahead.next() == Some(' ')
                && ahead.next().is_some_and(char::is_uppercase)
                && !out.ends_with("e.g.")
                && !out.ends_with("i.e.")
            {
                chars.next();
                out.push('\n');
            }
        }
    }
    out
}

/// A list with a hanging indent: wrapped lines start under the text, not
/// under the bullet.
pub fn bullets(ui: &mut Ui, items: &[&str]) {
    let p = *pal(ui);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 5.0;
        for t in items {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add_sized(
                    vec2(16.0, 18.0),
                    Label::new(RichText::new("•").size(size::BODY).color(p.text_3)),
                );
                ui.vertical(|ui| {
                    let w = ui.available_width().min(kit::PROSE);
                    ui.set_max_width(w);
                    ui.add(Label::new(RichText::new(*t).size(size::BODY).color(p.text)).wrap());
                });
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentences_go_on_their_own_lines() {
        assert_eq!(
            one_per_line(
                "Only admins see it. Your role on cli/cli: read. Rules below are readable."
            ),
            "Only admins see it.\nYour role on cli/cli: read.\nRules below are readable."
        );
        assert_eq!(
            one_per_line("Use a tag, e.g. Alpine 3.20. Then scan."),
            "Use a tag, e.g. Alpine 3.20.\nThen scan."
        );
        assert_eq!(one_per_line("version 1.2.3 is out"), "version 1.2.3 is out");
        assert_eq!(one_per_line("Done! Next? Yes."), "Done!\nNext?\nYes.");
    }
}
