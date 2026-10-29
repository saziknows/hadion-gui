//! Icons from the Lucide icon font (`assets/fonts/Lucide.ttf`, lucide-static
//! 1.53.0, ISC licence), drawn as glyphs so they stay crisp at any size, and
//! the ghost-style icon button built on them.
//!
//! To add an icon, pick it on lucide.dev, look its codepoint up in
//! `font/codepoints.json` of the same lucide-static release, and add a
//! variant plus a line in [`Icon::glyph`].

use std::f32::consts::TAU;

use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Painter, Pos2, Rect, Response, Sense,
    Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};

use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Pull,
    Push,
    Fetch,
    Branch,
    Stash,
    Pop,
    Search,
    Gear,
    Plus,
    Minus,
    Close,
    ChevronDown,
    ChevronUp,
    Undo,
    Warning,
    Info,
    Check,
    Error,
    /// Activity log: lines with leading dots.
    List,
    /// A short diagonal arrow out of a box: open externally.
    External,
    /// Play triangle in a circle: workflow runs / GitHub Actions.
    Play,
    /// Two overlapping sheets: copy.
    Copy,
    /// A clock face.
    Clock,
    /// Pull request: two branches joined by an arrow.
    PullRequest,
    /// Stop square in a circle: cancel.
    Stop,
    /// Circular arrow: re-run.
    Rerun,
    /// Shield with a check: exposure / security.
    Shield,
    /// Three bars of different heights: insights / analytics.
    Chart,
    /// Commit nodes on a line with a branch: the history page.
    Graph,
    /// A house: the home page.
    Home,
    /// A rocket: releases and deployments (Ship).
    Rocket,
    /// A padlock: governance, protection, secrets.
    Lock,
    /// A terminal window with a prompt: the workbench and console.
    Terminal,
    /// A circle with a dot: an issue.
    Issue,
    /// A tray: the inbox.
    Inbox,
    /// A cube: a package or container image.
    Package,
    /// A cloud: environments, clusters.
    Cloud,
    /// A price tag: tags and releases.
    Tag,
    /// A bug: bisect.
    Bug,
    /// A folder: worktrees, directories.
    Folder,
    /// Two lines joining: merge.
    Merge,
    /// A speech bubble: comments and reviews.
    Comment,
    /// A key: secrets and variables.
    Key,
    /// A gauge: metrics.
    Gauge,
    /// An eye: watch, reveal.
    Eye,
    /// A pencil: edit.
    Edit,
    /// A bin: delete.
    Trash,
    /// A bell: notifications.
    Bell,
    /// Two chain links: a link.
    Link,
    /// A ship's wheel: Kubernetes / Helm.
    Wheel,
    /// An arrow down onto a line: download.
    Download,
    /// A five-pointed star: favourites.
    Star,
    /// A wrench: the Tools page.
    Wrench,
    /// A pulse line: the activity log.
    Activity,
    /// Sparkles: an AI action (Claude).
    Sparkles,
}

/// The Lucide icon font's family (`theme::install_fonts` registers it).
pub const FONT_FAMILY: &str = "lucide";

/// Lucide draws on a 24-unit canvas with a 2-unit margin, so the glyph is
/// set this much larger than the box for the ink to fill it.
const GLYPH_SCALE: f32 = 1.07;

fn family() -> FontFamily {
    static FAMILY: std::sync::OnceLock<FontFamily> = std::sync::OnceLock::new();
    FAMILY
        .get_or_init(|| FontFamily::Name(FONT_FAMILY.into()))
        .clone()
}

impl Icon {
    /// The icon's codepoint in the Lucide font; the Lucide name is in the
    /// comment.
    pub fn glyph(self) -> char {
        let cp: u32 = match self {
            Icon::Pull => 0xe455,        // arrow-down-to-line
            Icon::Push => 0xe45a,        // arrow-up-from-line
            Icon::Fetch => 0xe145,       // refresh-cw
            Icon::Branch => 0xe0e2,      // git-branch
            Icon::Stash => 0xe041,       // archive
            Icon::Pop => 0xe2cd,         // archive-restore
            Icon::Search => 0xe151,      // search
            Icon::Gear => 0xe154,        // settings
            Icon::Plus => 0xe13d,        // plus
            Icon::Minus => 0xe11c,       // minus
            Icon::Close => 0xe1b2,       // x
            Icon::ChevronDown => 0xe06d, // chevron-down
            Icon::ChevronUp => 0xe070,   // chevron-up
            Icon::Undo => 0xe2a1,        // undo-2
            Icon::Warning => 0xe193,     // triangle-alert
            Icon::Info => 0xe0f9,        // info
            Icon::Check => 0xe06c,       // check
            Icon::Error => 0xe084,       // circle-x
            Icon::List => 0xe106,        // list
            Icon::External => 0xe0b9,    // external-link
            Icon::Play => 0xe080,        // circle-play
            Icon::Copy => 0xe09e,        // copy
            Icon::Clock => 0xe087,       // clock
            Icon::PullRequest => 0xe0e5, // git-pull-request
            Icon::Stop => 0xe083,        // circle-stop
            Icon::Rerun => 0xe149,       // rotate-cw
            Icon::Shield => 0xe1ff,      // shield-check
            Icon::Chart => 0xe2a3,       // chart-column
            Icon::Graph => 0xe554,       // git-graph
            Icon::Home => 0xe0f5,        // house
            Icon::Rocket => 0xe286,      // rocket
            Icon::Lock => 0xe10b,        // lock
            Icon::Terminal => 0xe20a,    // square-terminal
            Icon::Issue => 0xe345,       // circle-dot
            Icon::Inbox => 0xe0f7,       // inbox
            Icon::Package => 0xe129,     // package
            Icon::Cloud => 0xe088,       // cloud
            Icon::Tag => 0xe17f,         // tag
            Icon::Bug => 0xe20c,         // bug
            Icon::Folder => 0xe0d7,      // folder
            Icon::Merge => 0xe0e4,       // git-merge
            Icon::Comment => 0xe117,     // message-square
            Icon::Key => 0xe4a3,         // key-round
            Icon::Gauge => 0xe1bf,       // gauge
            Icon::Eye => 0xe0ba,         // eye
            Icon::Edit => 0xe1f9,        // pencil
            Icon::Trash => 0xe18e,       // trash-2
            Icon::Bell => 0xe059,        // bell
            Icon::Link => 0xe102,        // link
            Icon::Wheel => 0xe502,       // ship-wheel
            Icon::Download => 0xe0b2,    // download
            Icon::Star => 0xe176,        // star
            Icon::Wrench => 0xe1b1,      // wrench
            Icon::Activity => 0xe038,    // activity
            Icon::Sparkles => 0xe412,    // sparkles
        };
        char::from_u32(cp).unwrap_or('\u{fffd}')
    }
}

/// Draw `icon` centred on `center` inside a `size`×`size` box.
pub fn paint(p: &Painter, icon: Icon, center: Pos2, size: f32, color: Color32) {
    p.text(
        center,
        Align2::CENTER_CENTER,
        icon.glyph(),
        FontId::new(size * GLYPH_SCALE, family()),
        color,
    );
}

/// Dot for a run / job / check outcome: filled green check, red cross,
/// amber arc while running, hollow while queued, grey otherwise. Running
/// outcomes animate; the caller should request a repaint for them.
pub fn outcome_dot(p: &Painter, outcome: gitvis_core::github::Outcome, c: Pos2, r: f32, t: f64) {
    use gitvis_core::github::Outcome;
    let weak = Color32::from_gray(128);
    match outcome {
        Outcome::Success => {
            p.circle_filled(c, r, theme::GREEN);
            let s = r * 1.1;
            p.line(
                vec![
                    pos2(c.x - s * 0.5, c.y + s * 0.02),
                    pos2(c.x - s * 0.12, c.y + s * 0.4),
                    pos2(c.x + s * 0.55, c.y - s * 0.4),
                ],
                Stroke::new((r * 0.3).clamp(1.2, 2.0), Color32::WHITE),
            );
        }
        Outcome::Failure | Outcome::TimedOut | Outcome::ActionRequired | Outcome::Stale => {
            p.circle_filled(c, r, theme::RED);
            let d = r * 0.42;
            let st = Stroke::new((r * 0.3).clamp(1.2, 2.0), Color32::WHITE);
            p.line_segment([pos2(c.x - d, c.y - d), pos2(c.x + d, c.y + d)], st);
            p.line_segment([pos2(c.x - d, c.y + d), pos2(c.x + d, c.y - d)], st);
        }
        Outcome::InProgress => {
            let st = Stroke::new((r * 0.32).clamp(1.3, 2.2), theme::AMBER);
            p.circle_stroke(
                c,
                r * 0.85,
                Stroke::new(st.width, theme::AMBER.gamma_multiply(0.25)),
            );
            let a0 = (t * 2.4) as f32 % TAU;
            let pts: Vec<Pos2> = (0..=14)
                .map(|i| {
                    let a = a0 + TAU * 0.3 * i as f32 / 14.0;
                    pos2(c.x + r * 0.85 * a.cos(), c.y + r * 0.85 * a.sin())
                })
                .collect();
            p.line(pts, st);
        }
        Outcome::Queued => {
            p.circle_stroke(
                c,
                r * 0.85,
                Stroke::new((r * 0.3).clamp(1.2, 2.0), theme::AMBER),
            );
        }
        Outcome::Cancelled => {
            p.circle_filled(c, r, weak);
            p.line_segment(
                [pos2(c.x - r * 0.45, c.y), pos2(c.x + r * 0.45, c.y)],
                Stroke::new((r * 0.3).clamp(1.2, 2.0), Color32::WHITE),
            );
        }
        Outcome::Skipped => {
            p.circle_stroke(c, r * 0.85, Stroke::new((r * 0.28).clamp(1.2, 2.0), weak));
            p.line_segment(
                [
                    pos2(c.x - r * 0.4, c.y + r * 0.4),
                    pos2(c.x + r * 0.4, c.y - r * 0.4),
                ],
                Stroke::new((r * 0.28).clamp(1.2, 2.0), weak),
            );
        }
        Outcome::Neutral | Outcome::Unknown => {
            p.circle_filled(c, r * 0.8, weak);
        }
    }
}

/// Text colour matching [`outcome_dot`].
pub fn outcome_color(outcome: gitvis_core::github::Outcome, weak: Color32) -> Color32 {
    use gitvis_core::github::Outcome;
    match outcome {
        Outcome::Success => theme::GREEN,
        Outcome::Failure | Outcome::TimedOut | Outcome::ActionRequired | Outcome::Stale => {
            theme::RED
        }
        Outcome::InProgress | Outcome::Queued => theme::AMBER,
        _ => weak,
    }
}

/// Allocate space for an icon and draw it.
pub fn show(ui: &mut Ui, icon: Icon, size: f32, color: Color32) -> Rect {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint(ui.painter(), icon, rect.center(), size, color);
    rect
}

/// A button with an optional icon and label. Ghost by default (no fill
/// until hovered); `primary` fills it with the accent colour.
pub struct IconButton<'a> {
    icon: Option<Icon>,
    label: Option<&'a str>,
    primary: bool,
    danger: bool,
    enabled: bool,
    compact: bool,
    selected: bool,
}

impl<'a> IconButton<'a> {
    pub fn new(icon: Icon, label: &'a str) -> Self {
        Self {
            icon: Some(icon),
            label: Some(label),
            primary: false,
            danger: false,
            enabled: true,
            compact: false,
            selected: false,
        }
    }

    /// Keep the hover fill on, for toggles.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn icon(icon: Icon) -> Self {
        Self {
            label: None,
            ..Self::new(icon, "")
        }
    }

    pub fn text(label: &'a str) -> Self {
        Self {
            icon: None,
            ..Self::new(Icon::Plus, label)
        }
    }

    pub fn primary(mut self, primary: bool) -> Self {
        self.primary = primary;
        self
    }

    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let p = *theme::pal(ui);
        let height = if self.compact { 26.0 } else { 30.0 };
        let icon_size = if self.compact { 14.0 } else { 15.0 };
        let font = FontId::new(
            if self.compact {
                theme::size::SMALL
            } else {
                theme::size::BODY
            },
            theme::medium(),
        );

        let base_text = if self.primary {
            Color32::WHITE
        } else if self.danger {
            p.danger
        } else {
            p.text
        };
        let galley = self
            .label
            .filter(|l| !l.is_empty())
            .map(|l| ui.painter().layout_no_wrap(l.to_owned(), font, base_text));
        let text_w = galley.as_ref().map(|g| g.size().x).unwrap_or(0.0);
        let icon_w = if self.icon.is_some() { icon_size } else { 0.0 };
        let gap = if self.icon.is_some() && galley.is_some() {
            7.0
        } else {
            0.0
        };
        let pad = if galley.is_some() {
            if self.compact { 9.0 } else { 11.0 }
        } else {
            (height - icon_size) / 2.0
        };
        let width = pad * 2.0 + icon_w + gap + text_w;

        let sense = if self.enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, resp) = ui.allocate_exact_size(vec2(width, height), sense);
        if !ui.is_rect_visible(rect) {
            return resp;
        }

        let hovered = self.enabled && resp.hovered();
        let pressed = self.enabled && resp.is_pointer_button_down_on();
        let fill = if self.primary {
            // Primary + danger: a filled destructive button.
            let base = if self.danger { p.danger } else { p.accent };
            if pressed {
                base.lerp_to_gamma(Color32::BLACK, 0.15)
            } else if hovered {
                base.lerp_to_gamma(Color32::WHITE, 0.10)
            } else {
                base
            }
        } else if pressed {
            p.border_strong
        } else if self.selected {
            p.selection
        } else if hovered {
            p.hover
        } else {
            Color32::TRANSPARENT
        };
        let alpha = if self.enabled { 1.0 } else { 0.38 };
        let painter = ui.painter();
        if fill != Color32::TRANSPARENT {
            painter.rect_filled(
                rect,
                CornerRadius::same(theme::radius::M),
                fill.gamma_multiply(alpha),
            );
        }
        let text_color = if self.primary || self.danger {
            base_text
        } else if hovered || self.selected {
            p.text_strong
        } else {
            base_text
        }
        .gamma_multiply(alpha);

        let mut x = rect.left() + pad;
        if let Some(icon) = self.icon {
            paint(
                painter,
                icon,
                pos2(x + icon_size / 2.0, rect.center().y),
                icon_size,
                text_color,
            );
            x += icon_size + gap;
        }
        if let Some(galley) = galley {
            painter.galley_with_override_text_color(
                pos2(x, rect.center().y - galley.size().y / 2.0),
                galley,
                text_color,
            );
        }
        if self.enabled {
            resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        } else {
            resp
        }
    }
}

impl egui::Widget for IconButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        self.show(ui)
    }
}

/// Thin vertical divider for toolbars.
pub fn divider(ui: &mut Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, height), Sense::hover());
    ui.painter()
        .rect_filled(rect, 0, ui.visuals().widgets.noninteractive.bg_stroke.color);
}

/// A small keyboard-shortcut badge like `Ctrl+F`.
pub fn kbd(ui: &mut Ui, text: &str) {
    let font = FontId::proportional(11.0);
    let color = ui.visuals().weak_text_color();
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let (rect, _) = ui.allocate_exact_size(galley.size() + vec2(10.0, 6.0), Sense::hover());
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(4),
        Stroke::new(1.0, color.gamma_multiply(0.5)),
        StrokeKind::Inside,
    );
    ui.painter().galley(
        pos2(rect.left() + 5.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
}
