//! The start screen when no repository is open: the logo, Open and
//! Clone, and the recent repositories.

use egui::{Color32, FontId, Frame, Rect, RichText, Ui, UiBuilder, pos2};

use crate::format;
use crate::state::{Action, Settings};
use crate::theme::{Tone, medium, pal, size, space};
use crate::views::icons::{Icon, IconButton};
use crate::views::menus;
use crate::views::{brand, kit};

/// Width of the centred column.
const COLUMN: f32 = 560.0;

pub fn show(ui: &mut Ui, settings: &Settings, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas))
        .show(ui, |ui| {
            let full = ui.available_rect_before_wrap();
            let col_w = COLUMN.min(full.width() - 2.0 * space::XL);
            let has_recent = !settings.recent.is_empty();
            ui.add_space(if has_recent {
                (full.height() * 0.12).clamp(24.0, 110.0)
            } else {
                (full.height() * 0.26).clamp(24.0, 220.0)
            });

            // Hero: the logo (with its own clear space), tagline, actions.
            ui.vertical_centered(|ui| {
                brand::lockup(ui, 260.0);
                ui.add_space(2.0);
                ui.label(
                    RichText::new("Fast, visual branch management for Git")
                        .size(size::BODY)
                        .color(p.text_2),
                );
                ui.add_space(space::L + 4.0);
            });

            // Buttons, centred as a group.
            let open_label = "Open repository…";
            let clone_label = "Clone repository…";
            let text_w = |s: &str| {
                ui.painter()
                    .layout_no_wrap(
                        s.to_owned(),
                        FontId::new(size::BODY, medium()),
                        Color32::WHITE,
                    )
                    .size()
                    .x
            };
            // IconButton metrics: 11 points of padding a side, a 15 point
            // icon and a 7 point gap.
            let group_w =
                text_w(open_label) + 22.0 + text_w(clone_label) + 22.0 + 15.0 + 7.0 + space::S;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::S;
                ui.add_space(((ui.available_width() - group_w) / 2.0).max(0.0));
                if IconButton::text(open_label)
                    .primary(true)
                    .show(ui)
                    .on_hover_text("Ctrl+O")
                    .clicked()
                {
                    actions.push(Action::PickRepo);
                }
                if IconButton::new(Icon::Pull, clone_label)
                    .show(ui)
                    .on_hover_text("Ctrl+Shift+O")
                    .clicked()
                {
                    actions.push(Action::CloneDialog);
                }
            });
            ui.add_space(space::S);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("or drop a repository folder onto the window")
                        .size(size::SMALL)
                        .color(p.text_3),
                );
            });

            if !has_recent {
                return;
            }

            // Recent repositories as a list in the same column.
            ui.add_space(space::XL + space::S);
            let top = ui.cursor().top();
            let col = Rect::from_min_max(
                pos2(full.center().x - col_w / 2.0, top),
                pos2(full.center().x + col_w / 2.0, full.bottom() - space::XL),
            );
            ui.scope_builder(UiBuilder::new().max_rect(col), |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(kit::caption_text(&p, "Recent"));
                    ui.label(
                        RichText::new(format::count(settings.recent.len()))
                            .size(size::CAPTION)
                            .family(medium())
                            .color(p.text_3),
                    );
                });
                ui.add_space(4.0);
                let recent = &settings.recent;
                let names: Vec<String> = recent
                    .iter()
                    .map(|path| {
                        path.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string())
                    })
                    .collect();
                let paths: Vec<String> = recent.iter().map(|p| p.display().to_string()).collect();
                let missing: Vec<bool> = recent.iter().map(|p| !p.exists()).collect();
                let missing_badge = [("Missing".to_owned(), Tone::Danger)];
                // Rows edge to edge: `show_rows` (inside `kit::list`) adds
                // the caller's spacing to the row pitch.
                ui.spacing_mut().item_spacing.y = 0.0;
                kit::list(
                    ui,
                    "welcome_recent",
                    recent.len(),
                    kit::ROW_H,
                    |_| false,
                    |ui, rect, resp, i| {
                        let row = kit::Row::new(&names[i])
                            .leading(kit::Leading::Avatar(&names[i]))
                            .subtitle(&paths[i])
                            .badges(if missing[i] { &missing_badge } else { &[] })
                            .dimmed(missing[i]);
                        kit::paint_row(ui, rect, &row, false);
                        if resp.clicked() {
                            actions.push(Action::OpenRepo(recent[i].clone()));
                        }
                        resp.context_menu(|ui| {
                            menus::begin(ui, 180.0);
                            menus::action(ui, "Open", actions, Action::OpenRepo(recent[i].clone()));
                            menus::action(ui, "Copy path", actions, Action::Copy(paths[i].clone()));
                        });
                    },
                );
            });
        });
}
