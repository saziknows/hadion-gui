//! The navigation rail: one entry per page of the open repository, with the
//! activity log, the terminal and settings at the bottom. Expanded, an entry
//! is its icon and label on one row; collapsed (the toggle at the bottom or
//! Ctrl+B, and in [`RailMode::Auto`] any window narrower than
//! [`AUTO_COLLAPSE_BELOW`]) only the icon, and the flyout beside the rail
//! names the page. Pages only show a dot when something needs attention; the
//! flyout says what.
//!
//! Hover is a soft wash over the whole row (it fades in and out); the current
//! page gets the selection fill and an accent bar at the rail's edge, open
//! panels (terminal, activity) the fill only.

use egui::{
    Align2, Color32, CornerRadius, FontId, Id, Layout, Rect, RectAlign, Response, RichText, Sense,
    Ui, pos2, vec2,
};

use crate::devops::{self, Console};
use crate::state::{Action, Page, RailMode, RepoState, Settings};
use crate::theme::{self, Tone, pal, size};
use crate::views::brand;
use crate::views::icons::{self, Icon};

/// Width of the expanded rail (icons and labels).
pub const EXPANDED: f32 = 152.0;
/// Width of the collapsed rail (icons only).
pub const COLLAPSED: f32 = 60.0;
/// [`RailMode::Auto`] collapses the rail in windows narrower than this: at
/// the default 1,440 the expanded rail leaves the History page's graph too
/// little room for commit messages.
pub const AUTO_COLLAPSE_BELOW: f32 = 1600.0;
/// Space between the rail's edges and its entries' tiles.
const INSET: f32 = 8.0;
/// Gap between entries.
const GAP: f32 = 4.0;
/// The icon column: icons (and the Hadion Tile) sit at the same x expanded
/// and collapsed, so they stay put while the rail changes width.
const ICON_X: f32 = COLLAPSED / 2.0;
// The longest label ("Exposure", about 60 points) fits between the icon
// column and the tile's end, with room for a dot.
const _: () = assert!(ICON_X + 20.0 + 60.0 + 24.0 <= EXPANDED - INSET);

/// Whether the rail shows icons only.
pub fn collapsed(ctx: &egui::Context, settings: &Settings) -> bool {
    match settings.rail {
        RailMode::Auto => ctx.content_rect().width() < AUTO_COLLAPSE_BELOW,
        RailMode::Expanded => false,
        RailMode::Collapsed => true,
    }
}

/// The rail's width this frame: it eases between the two widths when it
/// collapses or expands.
pub fn width(ctx: &egui::Context, settings: &Settings) -> f32 {
    let target = if collapsed(ctx, settings) {
        COLLAPSED
    } else {
        EXPANDED
    };
    ctx.animate_value_with_time(Id::unique("rail_width"), target, 0.14)
}

/// How tall the rail's entries are: a short window tightens them, and if
/// even that does not fit, the top group scrolls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Density {
    /// 36-point rows.
    Regular,
    /// 30-point rows.
    Compact,
}

impl Density {
    fn entry_h(self) -> f32 {
        match self {
            Density::Regular => 36.0,
            Density::Compact => 30.0,
        }
    }

    /// The roomiest that fits `entries` into `height` (with at least 12
    /// between the groups and 12 under the last one).
    fn fitting(entries: usize, height: f32) -> Density {
        if entries as f32 * (Density::Regular.entry_h() + GAP) + 24.0 <= height {
            Density::Regular
        } else {
            Density::Compact
        }
    }
}

/// How entries are drawn this frame.
#[derive(Clone, Copy)]
struct Look {
    width: f32,
    h: f32,
    /// 0 collapsed … 1 expanded (the rail eases between them).
    open: f32,
}

/// What a rail entry signals.
struct Item<'a> {
    icon: Icon,
    label: &'a str,
    /// The current page, or for a panel toggle, the panel is open.
    selected: bool,
    /// A page rather than a panel toggle: when selected it also gets the
    /// accent bar and an accent icon.
    page: bool,
    /// Drawn in the tertiary colour until hovered (the collapse toggle).
    quiet: bool,
    dot: Option<Tone>,
    tip: String,
}

impl<'a> Item<'a> {
    fn page(icon: Icon, label: &'a str, selected: bool, dot: Option<Tone>, tip: String) -> Self {
        Item {
            icon,
            label,
            selected,
            page: true,
            quiet: false,
            dot,
            tip,
        }
    }

    fn panel(icon: Icon, label: &'a str, open: bool, tip: &str) -> Self {
        Item {
            icon,
            label,
            selected: open,
            page: false,
            quiet: false,
            dot: None,
            tip: tip.to_owned(),
        }
    }
}

fn entry(ui: &mut Ui, item: &Item, look: Look) -> Response {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    // The whole row reacts, the inset included, so the target is the
    // rail's full width.
    let (rect, resp) = ui.allocate_exact_size(vec2(look.width, look.h), Sense::click());
    let tile = Rect::from_min_max(
        pos2(rect.left() + INSET, rect.top()),
        pos2(rect.right() - INSET, rect.bottom()),
    );
    let radius = CornerRadius::same(theme::radius::L);
    let hover = ui
        .ctx()
        .animate_bool_with_time(resp.id.with("hover"), resp.hovered(), 0.12);

    if item.selected {
        ui.painter().rect_filled(tile, radius, p.selection);
    }
    // A translucent wash rather than a solid fill: it reads the same on the
    // dark and the light chrome, and over the selection.
    let alpha = hover
        * if resp.is_pointer_button_down_on() {
            28.0
        } else {
            16.0
        };
    if alpha >= 1.0 {
        let wash = if dark {
            Color32::from_white_alpha(alpha as u8)
        } else {
            Color32::from_black_alpha(alpha as u8)
        };
        ui.painter().rect_filled(tile, radius, wash);
    }
    if item.selected && item.page {
        let bar = Rect::from_center_size(
            pos2(rect.left() + 3.0, tile.center().y),
            vec2(3.0, (look.h * 0.5).max(14.0)),
        );
        ui.painter()
            .rect_filled(bar, CornerRadius::same(2), p.accent);
    }

    let rest = if item.quiet { p.text_3 } else { p.text_2 };
    let fg = if item.selected {
        p.text_strong
    } else {
        rest.lerp_to_gamma(p.text, hover)
    };
    let icon_at = pos2(rect.left() + ICON_X, tile.center().y);
    let icon_c = if item.selected && item.page {
        p.accent
    } else {
        fg
    };
    let icon_size = if look.h < 34.0 { 17.0 } else { 18.0 };
    icons::paint(ui.painter(), item.icon, icon_at, icon_size, icon_c);

    // Labels fade in over the second half of the expansion.
    let label_a = ((look.open - 0.5) * 2.0).clamp(0.0, 1.0);
    if label_a > 0.0 {
        let family = if item.selected {
            theme::semibold()
        } else {
            theme::medium()
        };
        ui.painter().text(
            pos2(rect.left() + ICON_X + 20.0, tile.center().y),
            Align2::LEFT_CENTER,
            item.label,
            FontId::new(size::BODY, family),
            fg.gamma_multiply(label_a),
        );
    }
    if let Some(tone) = item.dot {
        let c = tone.color(&p);
        if look.open >= 0.5 {
            // Expanded: at the end of the row, clear of the label.
            ui.painter()
                .circle_filled(pos2(tile.right() - 12.0, tile.center().y), 3.5, c);
        } else {
            // Collapsed: on the icon's corner, ringed in the row's colour.
            let at = pos2(icon_at.x + 10.0, icon_at.y - 8.0);
            let ring = if item.selected { p.selection } else { p.chrome };
            ui.painter().circle_filled(at, 5.0, ring);
            ui.painter().circle_filled(at, 3.5, c);
        }
    }
    flyout(&resp, item, look.open < 0.5);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The entry's explanation beside the rail, never over the entries below
/// it; with the page's name first while the labels are not drawn. It waits
/// for the pointer to rest (`tooltip_delay`), so crossing the rail shows
/// nothing.
fn flyout(resp: &Response, item: &Item, with_label: bool) {
    const FALLBACKS: [RectAlign; 2] = [RectAlign::RIGHT_END, RectAlign::RIGHT_START];
    let mut tip = egui::Tooltip::for_enabled(resp).gap(6.0);
    tip.popup = tip
        .popup
        .align(RectAlign::RIGHT)
        .align_alternatives(&FALLBACKS);
    tip.show(|ui| {
        if with_label {
            ui.label(
                RichText::new(item.label)
                    .family(theme::semibold())
                    .color(pal(ui).text_strong),
            );
        }
        ui.label(&item.tip);
    });
}

pub fn show(
    ui: &mut Ui,
    width: f32,
    repo: Option<&RepoState>,
    home: &devops::home::State,
    console: &Console,
    settings: &Settings,
    actions: &mut Vec<Action>,
) {
    ui.spacing_mut().item_spacing.y = GAP;
    let open = ((width - COLLAPSED) / (EXPANDED - COLLAPSED)).clamp(0.0, 1.0);
    let is_collapsed = collapsed(ui.ctx(), settings);
    // The Hadion Tile, level with the repository tabs, in the icon column.
    let (top, _) = ui.allocate_exact_size(vec2(width, 36.0), Sense::hover());
    let mark = Rect::from_center_size(
        pos2(top.left() + ICON_X, top.center().y + 3.0),
        vec2(26.0, 26.0),
    );
    brand::paint_tile(ui, mark);
    ui.add_space(12.0);
    // The bottom group: activity, terminal, settings and the toggle; the top
    // group's size depends on the repository (Review and Actions only with a
    // remote).
    let bottom = 4;
    let mut look = Look {
        width,
        h: Density::fitting(bottom, ui.available_height()).entry_h(),
        open,
    };
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
            Item::page(Icon::Home, "Home", page == Page::Home, dot, tip),
            Page::Home,
        ));
        items.push((
            Item::page(
                Icon::Graph,
                "History",
                page == Page::Graph,
                None,
                "Commit graph, branches, working tree".into(),
            ),
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
                Item::page(Icon::PullRequest, "Review", page == Page::Review, dot, tip),
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
                Item::page(Icon::Play, "Actions", page == Page::GitHub, dot, tip),
                Page::GitHub,
            ));
        }

        let (tip, dot) = with_badge(
            "Releases, environments, where a commit is, DORA, packages, Kubernetes, Terraform (Ctrl+Shift+D)".into(),
            devops::ship::rail_badge(r, console).or_else(|| devops::runtime::rail_badge(r, console)),
        );
        items.push((
            Item::page(Icon::Rocket, "Ship", page == Page::Ship, dot, tip),
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
            Item::page(
                Icon::Shield,
                "Exposure",
                page == Page::Security,
                (staged > 0 || urgent > 0).then_some(Tone::Danger),
                tip,
            ),
            Page::Security,
        ));

        items.push((
            Item::page(
                Icon::Chart,
                "Insights",
                page == Page::Insights,
                r.ins_page.running().then_some(Tone::Accent),
                "Health, hotspots, owners, branches, releases, debt, CI, licences, search, recovery (Ctrl+Shift+I)".into(),
            ),
            Page::Insights,
        ));

        let (tip, dot) = with_badge(
            "Alerts, SBOM, images, branch protection, CODEOWNERS, environments, secrets and variables (Ctrl+Shift+A)".into(),
            devops::govern::rail_badge(r, console),
        );
        items.push((
            Item::page(Icon::Lock, "Govern", page == Page::Govern, dot, tip),
            Page::Govern,
        ));

        let (tip, dot) = with_badge(
            "Tasks, interactive rebase, bisect, worktrees, file history, console (Ctrl+Shift+T)"
                .into(),
            devops::workbench::rail_badge(r, console),
        );
        items.push((
            Item::page(Icon::Wrench, "Tools", page == Page::Workbench, dot, tip),
            Page::Workbench,
        ));

        look.h = Density::fitting(items.len() + bottom, ui.available_height()).entry_h();
        // Even compact rows do not fit (not at the smallest window, but a
        // larger UI scale could): the top group scrolls above the bottom one.
        let bottom_h = bottom as f32 * (look.h + GAP) + 24.0;
        let top_h = (ui.available_height() - bottom_h).max(look.h);
        ui.allocate_ui_with_layout(
            vec2(width, top_h),
            Layout::top_down(egui::Align::Min),
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("rail_top")
                    .max_height(top_h)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = GAP;
                        for (item, target) in &items {
                            if entry(ui, item, look).clicked() && page != *target {
                                actions.push(Action::ShowPage(*target));
                            }
                        }
                    });
            },
        );
    }

    // Bottom, from the window's edge up: the toggle, settings, terminal and
    // the activity log.
    ui.with_layout(Layout::bottom_up(egui::Align::Min), |ui| {
        ui.spacing_mut().item_spacing.y = GAP;
        ui.add_space(12.0);
        let (icon, label, tip) = if is_collapsed {
            (
                Icon::PanelLeftOpen,
                "Expand",
                "Show the page names beside the icons (Ctrl+B)",
            )
        } else {
            (
                Icon::PanelLeftClose,
                "Collapse",
                "Icons only, for more room (Ctrl+B)",
            )
        };
        let toggle = Item {
            quiet: true,
            ..Item::panel(icon, label, false, tip)
        };
        if entry(ui, &toggle, look).clicked() {
            actions.push(Action::ToggleRail);
        }
        let on_settings = repo.is_some_and(|r| r.page == Page::Settings);
        let mut tip =
            "Settings: appearance, safety, git config, keys, accounts, AI, updates (Ctrl+,)"
                .to_owned();
        if settings.safe_mode {
            tip.push_str("\nSafe mode is on");
        }
        let gear = entry(
            ui,
            &Item::page(
                Icon::Gear,
                "Settings",
                on_settings,
                settings.safe_mode.then_some(Tone::Warning),
                tip,
            ),
            look,
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
            &Item::panel(
                Icon::Terminal,
                "Terminal",
                settings.show_terminal,
                "Shells in the repository folder (Ctrl+`)",
            ),
            look,
        )
        .clicked()
        {
            actions.push(Action::ToggleTerminal);
        }
        if entry(
            ui,
            &Item::panel(
                Icon::Activity,
                "Activity",
                settings.show_activity,
                "Every git command, scan and API call (Ctrl+L)",
            ),
            look,
        )
        .clicked()
        {
            actions.push(Action::ToggleActivity);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rail_entries_fit_the_window() {
        // A GitHub repository: 9 pages, 3 bottom entries and the toggle. The
        // tallest window has regular rows.
        assert_eq!(Density::fitting(13, 900.0 - 52.0), Density::Regular);
        // At the smallest window (560) about 508 points are left below the
        // Tile: compact rows, and they fit.
        assert_eq!(Density::fitting(13, 508.0), Density::Compact);
        assert!(13.0 * (Density::Compact.entry_h() + GAP) + 24.0 <= 508.0);
        // Without a remote (no Review and Actions), 11 entries fit regular
        // rows there.
        assert_eq!(Density::fitting(11, 508.0), Density::Regular);
    }
}
