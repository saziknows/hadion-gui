//! The navigation rail: one entry per page of the open repository, with the
//! activity log and settings at the bottom. Pages only show a dot when
//! something needs attention; the tooltip says what.

use egui::{Align2, Color32, CornerRadius, FontId, Layout, Rect, Response, Sense, Ui, pos2, vec2};

use crate::devops::{self, Console};
use crate::state::{Action, Page, RepoState, Settings};
use crate::theme::{self, Tone, pal, size};
use crate::views::icons::{self, Icon};

pub const WIDTH: f32 = 72.0;
/// Gap between rail entries.
const GAP: f32 = 2.0;

/// How tall the rail's entries are: a short window first tightens them,
/// then drops their labels (the tooltip then starts with the label), so
/// the top and bottom groups never overlap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Density {
    /// Icon pill and label (51 points).
    Full,
    /// Smaller pill and label (40 points).
    Compact,
    /// Icon pill only (34 points).
    Icons,
}

impl Density {
    fn entry_h(self) -> f32 {
        match self {
            Density::Full => 51.0,
            Density::Compact => 40.0,
            Density::Icons => 34.0,
        }
    }

    /// The densest that still fits `entries` into `height` (with a gap of
    /// at least 12 between the groups and 8 under the last one).
    fn fitting(entries: usize, height: f32) -> Density {
        [Density::Full, Density::Compact, Density::Icons]
            .into_iter()
            .find(|d| entries as f32 * (d.entry_h() + GAP) + 20.0 <= height)
            .unwrap_or(Density::Icons)
    }
}

/// What a rail entry signals.
struct Item<'a> {
    icon: Icon,
    label: &'a str,
    selected: bool,
    dot: Option<Tone>,
    tip: String,
}

fn entry(ui: &mut Ui, item: &Item, density: Density) -> Response {
    let p = *pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(WIDTH, density.entry_h()), Sense::click());
    let (pill_y, pill_h, label_y) = match density {
        Density::Full => (18.0, 26.0, Some(40.0)),
        Density::Compact => (13.0, 24.0, Some(31.5)),
        Density::Icons => (17.0, 28.0, None),
    };
    let pill = Rect::from_center_size(
        pos2(rect.center().x, rect.top() + pill_y),
        vec2(44.0, pill_h),
    );
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
    if let Some(y) = label_y {
        let font = if item.selected {
            FontId::new(size::CAPTION, theme::semibold())
        } else {
            FontId::new(size::CAPTION, theme::medium())
        };
        ui.painter().text(
            pos2(rect.center().x, rect.top() + y),
            Align2::CENTER_CENTER,
            item.label,
            font,
            fg,
        );
    }
    if let Some(tone) = item.dot {
        let c = tone.color(&p);
        let at = pos2(pill.center().x + 11.0, pill.center().y - 9.0);
        ui.painter().circle_filled(at, 5.0, p.chrome);
        ui.painter().circle_filled(at, 3.5, c);
    }
    let resp = if label_y.is_some() {
        resp.on_hover_text(&item.tip)
    } else {
        // The label is not drawn: the tooltip names the page first.
        resp.on_hover_text(format!("{}\n{}", item.label, item.tip))
    };
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn show(
    ui: &mut Ui,
    repo: Option<&RepoState>,
    home: &devops::home::State,
    console: &Console,
    settings: &Settings,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    ui.spacing_mut().item_spacing.y = GAP;
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
    // Three entries at the bottom; the top group's size depends on the
    // repository (Review and Actions only with a remote).
    const BOTTOM: usize = 3;
    let mut density = Density::fitting(BOTTOM, ui.available_height());
    if let Some(r) = repo {
        let page = r.page;
        let has_remote = r.github_remote.is_some() || r.forge_remote.is_some();
        let forge = r
            .forge_remote
            .as_ref()
            .filter(|_| r.github_remote.is_none());
        let mut items: Vec<(Item, Page)> = Vec::new();

        let with_badge =
            |mut tip: String, badge: Option<(Tone, String)>| -> (String, Option<Tone>) {
                match badge {
                    Some((tone, line)) => {
                        if !line.is_empty() {
                            tip.push('\n');
                            tip.push_str(&line);
                        }
                        (tip, Some(tone))
                    }
                    None => (tip, None),
                }
            };

        let (tip, dot) = with_badge(
            "Every open repository and your GitHub inbox (Ctrl+Shift+H)".into(),
            devops::home::rail_badge(home),
        );
        items.push((
            Item {
                icon: Icon::Home,
                label: "Home",
                selected: page == Page::Home,
                dot,
                tip,
            },
            Page::Home,
        ));
        items.push((
            Item {
                icon: Icon::Graph,
                label: "History",
                selected: page == Page::Graph,
                dot: None,
                tip: "Commit graph, branches, working tree".into(),
            },
            Page::Graph,
        ));

        if has_remote {
            let noun = match forge {
                Some(f) => format!(
                    "{} {}s and reviews on {} (Ctrl+Shift+R)",
                    f.kind.label(),
                    f.kind.review_noun(),
                    f.host
                ),
                None => "Pull requests, reviews and issues (Ctrl+Shift+R)".to_owned(),
            };
            let badge = if forge.is_some() {
                devops::forge::rail_badge(r, console, Page::Review)
            } else {
                devops::review::rail_badge(r, console)
            };
            let (tip, dot) = with_badge(noun, badge);
            items.push((
                Item {
                    icon: Icon::PullRequest,
                    label: "Review",
                    selected: page == Page::Review,
                    dot,
                    tip,
                },
                Page::Review,
            ));

            let (tip, dot) = if forge.is_some() {
                with_badge(
                    "Pipelines, jobs and logs (Ctrl+Shift+G)".into(),
                    devops::forge::rail_badge(r, console, Page::GitHub),
                )
            } else {
                let runs = r.github.as_ref().map(|d| d.runs.as_slice()).unwrap_or(&[]);
                let failed = runs.iter().filter(|x| x.is_failure()).count();
                let running = runs.iter().filter(|x| x.is_running()).count();
                let mut tip =
                    "Workflow runs, workflows, artifacts, caches, validation (Ctrl+Shift+G)"
                        .to_owned();
                if failed > 0 {
                    tip.push_str(&format!(
                        "\n{}",
                        crate::format::plural(failed, "failed run", "failed runs")
                    ));
                }
                if running > 0 {
                    tip.push_str(&format!("\n{running} running"));
                }
                let base = if failed > 0 {
                    Some(Tone::Danger)
                } else if running > 0 {
                    Some(Tone::Accent)
                } else {
                    None
                };
                match devops::ci::rail_badge(r, console) {
                    Some((tone, line)) => {
                        if !line.is_empty() {
                            tip.push('\n');
                            tip.push_str(&line);
                        }
                        (tip, base.or(Some(tone)))
                    }
                    None => (tip, base),
                }
            };
            items.push((
                Item {
                    icon: Icon::Play,
                    label: "Actions",
                    selected: page == Page::GitHub,
                    dot,
                    tip,
                },
                Page::GitHub,
            ));
        }

        let (tip, dot) = with_badge(
            "Releases, environments, where a commit is, DORA, packages, Kubernetes, Terraform (Ctrl+Shift+D)".into(),
            devops::ship::rail_badge(r, console).or_else(|| devops::runtime::rail_badge(r, console)),
        );
        items.push((
            Item {
                icon: Icon::Rocket,
                label: "Ship",
                selected: page == Page::Ship,
                dot,
                tip,
            },
            Page::Ship,
        ));

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
        items.push((
            Item {
                icon: Icon::Shield,
                label: "Exposure",
                selected: page == Page::Security,
                dot: if staged > 0 || urgent > 0 {
                    Some(Tone::Danger)
                } else {
                    None
                },
                tip,
            },
            Page::Security,
        ));

        items.push((
            Item {
                icon: Icon::Chart,
                label: "Insights",
                selected: page == Page::Insights,
                dot: r.ins_page.running().then_some(Tone::Accent),
                tip: "Health, hotspots, owners, branches, releases, debt, CI, licences, search, recovery (Ctrl+Shift+I)".into(),
            },
            Page::Insights,
        ));

        let (tip, dot) = with_badge(
            "Alerts, SBOM, images, branch protection, CODEOWNERS, environments, secrets and variables (Ctrl+Shift+A)".into(),
            devops::govern::rail_badge(r, console),
        );
        items.push((
            Item {
                icon: Icon::Lock,
                label: "Govern",
                selected: page == Page::Govern,
                dot,
                tip,
            },
            Page::Govern,
        ));

        let (tip, dot) = with_badge(
            "Tasks, interactive rebase, bisect, worktrees, file history, console (Ctrl+Shift+T)"
                .into(),
            devops::workbench::rail_badge(r, console),
        );
        items.push((
            Item {
                icon: Icon::Wrench,
                label: "Tools",
                selected: page == Page::Workbench,
                dot,
                tip,
            },
            Page::Workbench,
        ));

        density = Density::fitting(items.len() + BOTTOM, ui.available_height());
        // Even icons alone do not fit (not at the smallest window, but a
        // larger UI scale could): the top group scrolls above the bottom one.
        let bottom_h = BOTTOM as f32 * (density.entry_h() + GAP) + 20.0;
        let top_h = (ui.available_height() - bottom_h).max(density.entry_h());
        ui.allocate_ui_with_layout(
            vec2(WIDTH, top_h),
            Layout::top_down(egui::Align::Center),
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("rail_top")
                    .max_height(top_h)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = GAP;
                        for (item, target) in &items {
                            if entry(ui, item, density).clicked() && page != *target {
                                actions.push(Action::ShowPage(*target));
                            }
                        }
                    });
            },
        );
    }

    // Bottom: settings, terminal and the activity log.
    ui.with_layout(Layout::bottom_up(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.y = GAP;
        ui.add_space(8.0);
        let on_settings = repo.is_some_and(|r| r.page == Page::Settings);
        let mut tip =
            "Settings: appearance, safety, git config, keys, accounts, AI, updates (Ctrl+,)"
                .to_owned();
        if settings.safe_mode {
            tip.push_str("\nSafe mode is on");
        }
        let gear = entry(
            ui,
            &Item {
                icon: Icon::Gear,
                label: "Settings",
                selected: on_settings,
                dot: settings.safe_mode.then_some(Tone::Warning),
                tip,
            },
            density,
        );
        if gear.clicked() && repo.is_some() {
            actions.push(Action::ShowPage(if on_settings {
                Page::Graph
            } else {
                Page::Settings
            }));
        }
        if entry(
            ui,
            &Item {
                icon: Icon::Terminal,
                label: "Terminal",
                selected: settings.show_terminal,
                dot: None,
                tip: "Shells in the repository folder (Ctrl+`)".into(),
            },
            density,
        )
        .clicked()
        {
            actions.push(Action::ToggleTerminal);
        }
        if entry(
            ui,
            &Item {
                icon: Icon::Activity,
                label: "Activity",
                selected: settings.show_activity,
                dot: None,
                tip: "Every git command, scan and API call (Ctrl+L)".into(),
            },
            density,
        )
        .clicked()
        {
            actions.push(Action::ToggleActivity);
        }
    });
}
