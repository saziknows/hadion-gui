//! The status bar: where you are, the state of the working tree, and what
//! ran last. Everything else lives on the pages (and the rail's dots).

use egui::{Align, Frame, Layout, Margin, RichText, Sense, Stroke, Ui};

use gitvis_core::Head;

use crate::format;
use crate::state::{Action, ActivityEntry, RepoState, Settings};
use crate::theme::{pal, size};
use crate::views::icons::{self, Icon};

pub fn show(
    ui: &mut Ui,
    repo: Option<&RepoState>,
    settings: &Settings,
    last: Option<&ActivityEntry>,
    globals: &crate::devops::global::Globals,
    view: &crate::devops::global::GlobalView,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    egui::Panel::bottom("statusbar")
        .show_separator_line(false)
        .frame(
            Frame::NONE
                .fill(p.panel)
                .stroke(Stroke::new(1.0, p.border))
                .inner_margin(Margin::symmetric(14, 4)),
        )
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                let text = |t: String| RichText::new(t).size(size::SMALL - 0.5).color(p.text_2);
                let Some(r) = repo else {
                    ui.label(text("No repository open".into()));
                    return;
                };
                let now = format::now_secs();

                // Repository and HEAD.
                let mut tip = format!("{}\nClick to copy the path", r.workdir.display());
                if let (Some(ms), Some(at)) = (r.load_ms, r.loaded_at) {
                    tip.push_str(&format!(
                        "\nLoaded in {}, {}",
                        format::millis(ms),
                        format::relative(now - at.elapsed().as_secs() as i64, now)
                    ));
                }
                let head = r.snapshot.as_ref().map(|snap| match &snap.head {
                    Head::Branch { name, target } => {
                        format!("{name} at {}", gitvis_core::repo::short_id(*target))
                    }
                    Head::Detached(t) => {
                        format!("detached at {}", gitvis_core::repo::short_id(*t))
                    }
                    Head::Unborn(n) => format!("{n}, no commits yet"),
                });
                let resp = ui
                    .add(
                        egui::Label::new(
                            RichText::new(match &head {
                                Some(h) => format!("{} · {h}", r.name),
                                None => r.name.clone(),
                            })
                            .size(size::SMALL - 0.5)
                            .color(p.text),
                        )
                        .sense(Sense::click()),
                    )
                    .on_hover_text(tip)
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if resp.clicked() {
                    actions.push(Action::Copy(r.workdir.display().to_string()));
                }

                if let Some(snap) = r.snapshot.as_ref() {
                    let st = &snap.status;
                    if st.is_clean() {
                        ui.label(text("working tree clean".into()));
                    } else {
                        let mut parts = Vec::new();
                        if !st.conflicted.is_empty() {
                            parts.push(format!("{} conflicted", st.conflicted.len()));
                        }
                        if !st.staged.is_empty() {
                            parts.push(format!("{} staged", st.staged.len()));
                        }
                        if !st.unstaged.is_empty() {
                            parts.push(format!("{} changed", st.unstaged.len()));
                        }
                        let c = if st.conflicted.is_empty() {
                            p.warning
                        } else {
                            p.danger
                        };
                        ui.label(
                            RichText::new(parts.join(", "))
                                .size(size::SMALL - 0.5)
                                .color(c),
                        );
                    }
                    if snap.truncated {
                        ui.label(text(format!(
                            "newest {} commits loaded",
                            format::count(snap.commits.len())
                        )))
                        .on_hover_text("Older history is not drawn in the graph.");
                    }
                }

                // Right side: what is running or what ran last.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if settings.safe_mode {
                        let resp = crate::views::kit::badge(
                            ui,
                            "Safe mode",
                            crate::theme::Tone::Warning,
                        )
                        .interact(Sense::click())
                        .on_hover_text(
                            "Changes outside this machine are refused. Click to open the setting.",
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if resp.clicked() {
                            actions.push(Action::DevOps(
                                crate::devops::DevOpsAction::SetSettingsSection(
                                    crate::devops::settings_page::Section::Safety,
                                ),
                            ));
                            actions.push(Action::ShowPage(crate::state::Page::Settings));
                        }
                        ui.add_space(6.0);
                    }
                    crate::devops::hooks::status_items(ui, globals, view, actions);
                    if let Some(label) = r.busy.as_deref() {
                        ui.add(egui::Spinner::new().size(11.0).color(p.accent));
                        ui.label(text(format!("{label}…")).color(p.text));
                    } else if r.loading {
                        ui.add(egui::Spinner::new().size(11.0).color(p.text_3));
                        ui.label(text("Reading repository…".into()));
                    } else if let Some(e) = last {
                        let (c, icon) = if e.ok {
                            (p.text_3, Icon::Check)
                        } else {
                            (p.danger, Icon::Error)
                        };
                        let resp = ui
                            .add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "{}  {}",
                                        e.label,
                                        format::millis(e.elapsed_ms)
                                    ))
                                    .size(size::SMALL - 0.5)
                                    .color(if e.ok {
                                        p.text_2
                                    } else {
                                        p.danger
                                    }),
                                )
                                .sense(Sense::click()),
                            )
                            .on_hover_text(format!(
                                "Last activity at {}. Click to open the activity log (Ctrl+L)",
                                e.at
                            ))
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if resp.clicked() {
                            actions.push(Action::ToggleActivity);
                        }
                        icons::show(ui, icon, 12.0, if e.ok { p.success } else { c });
                    }
                    if let Some(rate) = r.rate {
                        ui.add_space(8.0);
                        ui.label(text(format!(
                            "GitHub API {} left",
                            format::count(rate.remaining as usize)
                        )))
                        .on_hover_text(format!(
                            "{} of {} requests left, resets in {}",
                            rate.remaining,
                            rate.limit,
                            format::duration(rate.reset - now)
                        ));
                    }
                });
            });
        });
}
