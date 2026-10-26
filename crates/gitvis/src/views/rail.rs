//! The navigation rail: one entry per page of the open repository, with the
//! activity log and settings at the bottom. Pages only show a dot when
//! something needs attention; the tooltip says what.

use egui::{Align2, Color32, CornerRadius, FontId, Layout, Rect, Response, Sense, Ui, pos2, vec2};

use crate::state::{Action, Page, PullPref, RepoState, Settings};
use crate::theme::{self, Tone, pal, size};
use crate::views::icons::{self, Icon};

pub const WIDTH: f32 = 72.0;

/// What a rail entry signals.
struct Item<'a> {
    icon: Icon,
    label: &'a str,
    selected: bool,
    dot: Option<Tone>,
    tip: String,
}

fn entry(ui: &mut Ui, item: &Item) -> Response {
    let p = *pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(WIDTH, 56.0), Sense::click());
    let pill = Rect::from_center_size(pos2(rect.center().x, rect.top() + 20.0), vec2(44.0, 28.0));
    if item.selected {
        ui.painter()
            .rect_filled(pill, CornerRadius::same(theme::radius::L), p.selection);
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(pill, CornerRadius::same(theme::radius::L), p.hover);
    }
    let fg = if item.selected {
        p.text_strong
    } else if resp.hovered() {
        p.text
    } else {
        p.text_2
    };
    icons::paint(ui.painter(), item.icon, pill.center(), 18.0, fg);
    let font = if item.selected {
        FontId::new(size::CAPTION, theme::semibold())
    } else {
        FontId::new(size::CAPTION, theme::medium())
    };
    ui.painter().text(
        pos2(rect.center().x, rect.top() + 44.0),
        Align2::CENTER_CENTER,
        item.label,
        font,
        fg,
    );
    if let Some(tone) = item.dot {
        let c = tone.color(&p);
        let at = pos2(pill.center().x + 11.0, pill.center().y - 9.0);
        ui.painter().circle_filled(at, 5.0, p.chrome);
        ui.painter().circle_filled(at, 3.5, c);
    }
    resp.on_hover_text(&item.tip)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn show(ui: &mut Ui, repo: Option<&RepoState>, settings: &Settings, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    ui.spacing_mut().item_spacing.y = 2.0;
    // App mark, level with the repository tabs.
    let (top, _) = ui.allocate_exact_size(vec2(WIDTH, 36.0), Sense::hover());
    let mark = Rect::from_center_size(pos2(top.center().x, top.center().y + 3.0), vec2(26.0, 26.0));
    ui.painter()
        .rect_filled(mark, CornerRadius::same(theme::radius::M), p.accent);
    icons::paint(
        ui.painter(),
        Icon::Graph,
        mark.center(),
        15.0,
        Color32::WHITE,
    );
    ui.add_space(10.0);
    if let Some(r) = repo {
        let page = r.page;
        let go = |target: Page| Action::ShowPage(target);

        let history = Item {
            icon: Icon::Graph,
            label: "History",
            selected: page == Page::Graph,
            dot: None,
            tip: "Commit graph, branches, working tree".into(),
        };
        if entry(ui, &history).clicked() && page != Page::Graph {
            actions.push(go(Page::Graph));
        }

        if r.github_remote.is_some() {
            let runs = r.github.as_ref().map(|d| d.runs.as_slice()).unwrap_or(&[]);
            let failed = runs.iter().filter(|x| x.is_failure()).count();
            let running = runs.iter().filter(|x| x.is_running()).count();
            let mut tip = "Workflow runs, jobs, logs and pull requests (Ctrl+Shift+G)".to_owned();
            if failed > 0 {
                tip.push_str(&format!(
                    "\n{}",
                    crate::format::plural(failed, "failed run", "failed runs")
                ));
            }
            if running > 0 {
                tip.push_str(&format!("\n{running} running"));
            }
            let item = Item {
                icon: Icon::Play,
                label: "Actions",
                selected: page == Page::GitHub,
                dot: if failed > 0 {
                    Some(Tone::Danger)
                } else if running > 0 {
                    Some(Tone::Accent)
                } else {
                    None
                },
                tip,
            };
            if entry(ui, &item).clicked() && page != Page::GitHub {
                actions.push(go(Page::GitHub));
            }
        }

        let sp = &r.sec_page;
        let staged = sp.staged_count();
        let urgent = sp.urgent_count(&r.dismissed);
        let mut tip =
            "Secrets, sensitive files and what the repository reveals (Ctrl+Shift+S)".to_owned();
        if staged > 0 {
            tip.push_str(&format!(
                "\n{} in the staged changes",
                crate::format::plural(staged, "possible secret", "possible secrets")
            ));
        } else if urgent > 0 {
            tip.push_str(&format!(
                "\n{} critical or high",
                crate::format::plural(urgent, "finding", "findings")
            ));
        }
        let item = Item {
            icon: Icon::Shield,
            label: "Exposure",
            selected: page == Page::Security,
            dot: if staged > 0 || urgent > 0 {
                Some(Tone::Danger)
            } else {
                None
            },
            tip,
        };
        if entry(ui, &item).clicked() && page != Page::Security {
            actions.push(go(Page::Security));
        }

        let item = Item {
            icon: Icon::Chart,
            label: "Insights",
            selected: page == Page::Insights,
            dot: r.ins_page.running().then_some(Tone::Accent),
            tip: "Health, hotspots, owners, branches, releases, debt, CI, licences, search, recovery (Ctrl+Shift+I)".into(),
        };
        if entry(ui, &item).clicked() && page != Page::Insights {
            actions.push(go(Page::Insights));
        }
    }

    // Bottom: activity log and settings.
    ui.with_layout(Layout::bottom_up(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.add_space(8.0);
        let gear = entry(
            ui,
            &Item {
                icon: Icon::Gear,
                label: "Settings",
                selected: false,
                dot: None,
                tip: "Theme, dates, pull strategy, scanning".into(),
            },
        );
        egui::Popup::menu(&gear)
            .id(egui::Id::unique("settings_menu"))
            .align(egui::RectAlign::RIGHT_END)
            .show(|ui| settings_menu(ui, settings, actions));
        if entry(
            ui,
            &Item {
                icon: Icon::List,
                label: "Activity",
                selected: settings.show_activity,
                dot: None,
                tip: "Every git command, scan and API call (Ctrl+L)".into(),
            },
        )
        .clicked()
        {
            actions.push(Action::ToggleActivity);
        }
    });
}

fn settings_menu(ui: &mut Ui, settings: &Settings, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    ui.set_min_width(260.0);
    ui.label(crate::views::kit::caption_text(&p, "Display"));
    let mut dark = settings.dark;
    if ui.checkbox(&mut dark, "Dark theme").clicked() {
        actions.push(Action::ToggleTheme);
    }
    let mut rel = settings.relative_dates;
    if ui.checkbox(&mut rel, "Relative dates").clicked() {
        actions.push(Action::ToggleRelativeDates);
    }
    let mut author = settings.show_author;
    if ui.checkbox(&mut author, "Author column").clicked() {
        actions.push(Action::ToggleAuthor);
    }
    let mut status_bar = settings.show_status_bar;
    if ui.checkbox(&mut status_bar, "Status bar").clicked() {
        actions.push(Action::ToggleStatusBar);
    }
    ui.separator();
    ui.label(crate::views::kit::caption_text(&p, "Activity"));
    let mut verbose = settings.verbose;
    if ui
        .checkbox(&mut verbose, "Verbose activity log")
        .on_hover_text("Record repository reads and GitHub API calls too, with full output")
        .clicked()
    {
        actions.push(Action::ToggleVerbose);
    }
    let mut scan = settings.scan_on_open;
    if ui
        .checkbox(&mut scan, "Scan for secrets on open")
        .on_hover_text("Read every file of the working tree for credentials when a repository is opened. Staged changes are always checked before a commit.")
        .clicked()
    {
        actions.push(Action::ToggleScanOnOpen);
    }
    ui.separator();
    ui.label(crate::views::kit::caption_text(&p, "Default pull"));
    for pp in [PullPref::Merge, PullPref::FastForwardOnly, PullPref::Rebase] {
        if ui.radio(settings.pull == pp, pp.label()).clicked() {
            actions.push(Action::SetPull(pp));
        }
    }
    ui.separator();
    if ui.button("Reload repository   F5").clicked() {
        actions.push(Action::Reload(true));
        ui.close();
    }
}
