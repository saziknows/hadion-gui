//! Design tokens: palette, type scale, spacing, fonts.
//!
//! Every view takes its colours from [`Palette`] (through [`pal`]) and its
//! sizes from [`size`] / [`space`]; the components in `views::kit` are built
//! on these and are what pages should use. The legacy constants (`ACCENT`,
//! `GREEN`, …) stay for status colours that read well in both themes.

use std::sync::Arc;

use egui::epaint::text::{FontData, FontDefinitions, FontTweak, VariationCoords};
use egui::{
    Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle, Theme,
    ThemePreference, Ui, Vec2, Visuals,
};
use hadion_core::ChangeKind;

pub const ACCENT: Color32 = Color32::from_rgb(0x5b, 0x8d, 0xff);
pub const GREEN: Color32 = Color32::from_rgb(0x3f, 0xbf, 0x7f);
pub const RED: Color32 = Color32::from_rgb(0xef, 0x5d, 0x68);
pub const AMBER: Color32 = Color32::from_rgb(0xe8, 0xa8, 0x38);
pub const PURPLE: Color32 = Color32::from_rgb(0xa9, 0x8b, 0xf5);

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

/// Surfaces, text and semantic colours of one theme.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// Window chrome: the repository tab strip and the navigation rail.
    pub chrome: Color32,
    /// Side panes and bars: toolbar, sidebars, list panes, status bar.
    pub panel: Color32,
    /// Main content: the graph, detail panes, pages.
    pub canvas: Color32,
    /// Cards, tiles, inputs and code blocks on the canvas.
    pub raised: Color32,
    /// Hover fill for rows and ghost buttons.
    pub hover: Color32,
    /// Menus, popups, tooltips.
    pub overlay: Color32,
    /// Hairlines between panes and around cards.
    pub border: Color32,
    pub border_strong: Color32,
    /// Primary text.
    pub text: Color32,
    /// Secondary text: labels, subtitles, explanations.
    pub text_2: Color32,
    /// Tertiary text: captions, timestamps, placeholders.
    pub text_3: Color32,
    /// Brightest text: titles and selected items.
    pub text_strong: Color32,
    pub accent: Color32,
    /// Selected row / active nav item background.
    pub selection: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
    pub purple: Color32,
}

pub const DARK: Palette = Palette {
    chrome: Color32::from_rgb(0x11, 0x13, 0x17),
    panel: Color32::from_rgb(0x17, 0x19, 0x1e),
    canvas: Color32::from_rgb(0x1c, 0x1f, 0x25),
    raised: Color32::from_rgb(0x24, 0x28, 0x30),
    hover: Color32::from_rgb(0x2a, 0x2e, 0x37),
    overlay: Color32::from_rgb(0x26, 0x2a, 0x32),
    border: Color32::from_rgb(0x2a, 0x2e, 0x36),
    border_strong: Color32::from_rgb(0x3a, 0x40, 0x4b),
    text: Color32::from_rgb(0xdc, 0xdf, 0xe6),
    text_2: Color32::from_rgb(0xa0, 0xa7, 0xb4),
    text_3: Color32::from_rgb(0x70, 0x78, 0x86),
    text_strong: Color32::from_rgb(0xf4, 0xf5, 0xf8),
    accent: ACCENT,
    selection: Color32::from_rgb(0x24, 0x33, 0x55),
    success: GREEN,
    warning: AMBER,
    danger: RED,
    purple: PURPLE,
};

pub const LIGHT: Palette = Palette {
    chrome: Color32::from_rgb(0xe9, 0xeb, 0xef),
    panel: Color32::from_rgb(0xf4, 0xf5, 0xf7),
    canvas: Color32::from_rgb(0xff, 0xff, 0xff),
    raised: Color32::from_rgb(0xf3, 0xf4, 0xf6),
    hover: Color32::from_rgb(0xe7, 0xe9, 0xee),
    overlay: Color32::from_rgb(0xff, 0xff, 0xff),
    border: Color32::from_rgb(0xe2, 0xe5, 0xea),
    border_strong: Color32::from_rgb(0xcb, 0xd0, 0xd8),
    text: Color32::from_rgb(0x22, 0x26, 0x2e),
    text_2: Color32::from_rgb(0x55, 0x5d, 0x6b),
    text_3: Color32::from_rgb(0x88, 0x90, 0x9d),
    text_strong: Color32::from_rgb(0x0f, 0x12, 0x17),
    accent: Color32::from_rgb(0x2f, 0x6b, 0xe6),
    selection: Color32::from_rgb(0xdd, 0xe7, 0xfd),
    success: Color32::from_rgb(0x1e, 0x95, 0x5a),
    warning: Color32::from_rgb(0xb0, 0x72, 0x14),
    danger: Color32::from_rgb(0xd2, 0x3b, 0x48),
    purple: Color32::from_rgb(0x7a, 0x5a, 0xdb),
};

/// The palette for the current theme.
pub fn pal(ui: &Ui) -> &'static Palette {
    palette(ui.visuals().dark_mode)
}

pub fn palette(dark: bool) -> &'static Palette {
    if dark { &DARK } else { &LIGHT }
}

/// Semantic colour of a badge, dot, stat or callout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tone {
    #[default]
    Neutral,
    Accent,
    Success,
    Warning,
    Danger,
    Purple,
}

impl Tone {
    pub fn color(self, p: &Palette) -> Color32 {
        match self {
            Tone::Neutral => p.text_2,
            Tone::Accent => p.accent,
            Tone::Success => p.success,
            Tone::Warning => p.warning,
            Tone::Danger => p.danger,
            Tone::Purple => p.purple,
        }
    }
}

// ---------------------------------------------------------------------------
// Type scale and spacing
// ---------------------------------------------------------------------------

/// Font sizes in points. Nothing in the UI should be smaller than `CAPTION`.
pub mod size {
    /// Uppercase section captions, badges.
    pub const CAPTION: f32 = 11.0;
    /// Secondary text: subtitles, labels, explanations, metadata.
    pub const SMALL: f32 = 12.5;
    /// Default text.
    pub const BODY: f32 = 13.5;
    /// Titles of a selected item (detail pane header, card title).
    pub const TITLE: f32 = 15.5;
    /// Page titles.
    pub const HEADING: f32 = 19.0;
    /// Big numbers in stat tiles.
    pub const DISPLAY: f32 = 21.0;
    /// Code, hashes, paths in code blocks.
    pub const MONO: f32 = 12.5;
}

/// Spacing on a 4-point grid.
pub mod space {
    pub const XS: f32 = 4.0;
    pub const S: f32 = 8.0;
    pub const M: f32 = 12.0;
    pub const L: f32 = 16.0;
    pub const XL: f32 = 24.0;
}

/// Corner radii.
pub mod radius {
    pub const S: u8 = 4;
    pub const M: u8 = 6;
    pub const L: u8 = 8;
}

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

const FAMILY_MEDIUM: &str = "inter-medium";
const FAMILY_SEMIBOLD: &str = "inter-semibold";

/// Inter at weight 500.
pub fn medium() -> FontFamily {
    FontFamily::Name(FAMILY_MEDIUM.into())
}

/// Inter at weight 600.
pub fn semibold() -> FontFamily {
    FontFamily::Name(FAMILY_SEMIBOLD.into())
}

/// Load Inter (one variable font pinned at three weights), JetBrains Mono
/// and the Lucide icon font, keeping egui's bundled fonts as fallbacks for
/// glyphs Inter and JetBrains Mono lack.
pub fn install_fonts(ctx: &egui::Context) {
    static INTER: &[u8] = include_bytes!("../assets/fonts/InterVariable.ttf");
    static MONO: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Variable.ttf");
    static ICONS: &[u8] = include_bytes!("../assets/fonts/Lucide.ttf");
    let mut fonts = FontDefinitions::default();
    let weighted = |bytes: &'static [u8], weight: f32| {
        Arc::new(FontData::from_static(bytes).tweak(FontTweak {
            coords: VariationCoords::new([(b"wght", weight)]),
            ..Default::default()
        }))
    };
    fonts
        .font_data
        .insert("inter-400".into(), weighted(INTER, 400.0));
    fonts
        .font_data
        .insert("inter-500".into(), weighted(INTER, 500.0));
    fonts
        .font_data
        .insert("inter-600".into(), weighted(INTER, 600.0));
    fonts
        .font_data
        .insert("jetbrains-mono".into(), weighted(MONO, 400.0));

    let fallbacks = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let with = |primary: &str| {
        let mut v = vec![primary.to_owned()];
        v.extend(fallbacks.iter().cloned());
        v
    };
    fonts
        .families
        .insert(FontFamily::Proportional, with("inter-400"));
    fonts.families.insert(medium(), with("inter-500"));
    fonts.families.insert(semibold(), with("inter-600"));
    let mono_fallbacks = fonts
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();
    let mut mono = vec!["jetbrains-mono".to_owned()];
    mono.extend(mono_fallbacks);
    fonts.families.insert(FontFamily::Monospace, mono);
    fonts
        .font_data
        .insert("lucide".into(), Arc::new(FontData::from_static(ICONS)));
    fonts.families.insert(
        FontFamily::Name(crate::views::icons::FONT_FAMILY.into()),
        vec!["lucide".to_owned()],
    );
    ctx.set_fonts(fonts);
}

// ---------------------------------------------------------------------------
// Colours for data
// ---------------------------------------------------------------------------

const LANES: [Color32; 12] = [
    Color32::from_rgb(0x5b, 0x8d, 0xff),
    Color32::from_rgb(0x3f, 0xbf, 0x7f),
    Color32::from_rgb(0xf2, 0x8c, 0x52),
    Color32::from_rgb(0xa9, 0x8b, 0xf5),
    Color32::from_rgb(0xef, 0x5d, 0x68),
    Color32::from_rgb(0x34, 0xc0, 0xcc),
    Color32::from_rgb(0xe8, 0xa8, 0x38),
    Color32::from_rgb(0xe8, 0x6c, 0xb5),
    Color32::from_rgb(0x9c, 0xc9, 0x52),
    Color32::from_rgb(0x74, 0xa9, 0xf2),
    Color32::from_rgb(0xcd, 0xa4, 0x62),
    Color32::from_rgb(0x8f, 0x9b, 0xad),
];

pub fn lane_color(lane: u16, dark: bool) -> Color32 {
    let c = LANES[lane as usize % LANES.len()];
    if dark {
        c
    } else {
        c.lerp_to_gamma(Color32::BLACK, 0.18)
    }
}

pub fn kind_color(kind: ChangeKind) -> Color32 {
    match kind {
        ChangeKind::Added | ChangeKind::Untracked => GREEN,
        ChangeKind::Deleted | ChangeKind::Conflicted => RED,
        ChangeKind::Renamed | ChangeKind::Copied => PURPLE,
        ChangeKind::Modified | ChangeKind::TypeChange => AMBER,
    }
}

/// Stable colour for an author name.
pub fn name_color(name: &str, dark: bool) -> Color32 {
    let mut h: u32 = 2166136261;
    for b in name.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    lane_color((h % LANES.len() as u32) as u16, dark)
}

// ---------------------------------------------------------------------------
// egui style
// ---------------------------------------------------------------------------

pub fn apply(ctx: &egui::Context, dark: bool) {
    ctx.set_theme(if dark {
        ThemePreference::Dark
    } else {
        ThemePreference::Light
    });
    let p = palette(dark);
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.panel_fill = p.panel;
    v.window_fill = p.overlay;
    v.extreme_bg_color = p.canvas;
    v.text_edit_bg_color = Some(if dark { p.chrome } else { p.canvas });
    v.faint_bg_color = p.raised;
    v.code_bg_color = p.raised;
    v.weak_text_color = Some(p.text_3);
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    v.window_stroke = Stroke::new(1.0, p.border_strong);
    v.window_corner_radius = CornerRadius::same(radius::L);
    v.menu_corner_radius = CornerRadius::same(radius::M);
    v.window_shadow = Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 110 } else { 40 }),
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 14,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 90 } else { 30 }),
    };
    v.striped = false;
    v.selection.bg_fill = if dark {
        p.accent.gamma_multiply(0.35)
    } else {
        p.accent.gamma_multiply(0.25)
    };
    v.selection.stroke = Stroke::new(1.0, p.text_strong);

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.panel;
    w.noninteractive.weak_bg_fill = p.panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.bg_fill = p.raised;
    w.inactive.weak_bg_fill = p.raised;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.bg_fill = p.hover;
    w.hovered.weak_bg_fill = p.hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.border_strong);
    w.hovered.fg_stroke = Stroke::new(1.0, p.text_strong);
    w.active.bg_fill = p.border_strong;
    w.active.weak_bg_fill = p.border_strong;
    w.active.bg_stroke = Stroke::NONE;
    w.active.fg_stroke = Stroke::new(1.0, p.text_strong);
    w.open.bg_fill = p.hover;
    w.open.weak_bg_fill = p.hover;
    w.open.fg_stroke = Stroke::new(1.0, p.text_strong);
    ctx.set_visuals_of(if dark { Theme::Dark } else { Theme::Light }, v);

    ctx.all_styles_mut(|style| {
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(size::SMALL));
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(size::BODY));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::new(size::BODY, medium()));
        style
            .text_styles
            .insert(TextStyle::Monospace, FontId::monospace(size::MONO));
        style
            .text_styles
            .insert(TextStyle::Heading, FontId::new(size::HEADING, semibold()));
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(12.0, 5.0);
        style.spacing.interact_size.y = 24.0;
        style.spacing.menu_margin = Margin::same(6);
        style.spacing.window_margin = Margin::same(16);
        style.spacing.tooltip_width = 380.0;
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.scroll.floating_allocated_width = 0.0;
        style.interaction.selectable_labels = false;
        style.interaction.tooltip_delay = 0.45;
        // No "grace" chaining: moving from one tooltip-bearing widget to the
        // next waits the full delay again instead of popping tooltips in
        // rapid succession while the pointer crosses a dense page.
        style.interaction.tooltip_grace_time = 0.0;
        style.url_in_tooltip = false;
        for w in [
            &mut style.visuals.widgets.noninteractive,
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
            &mut style.visuals.widgets.open,
        ] {
            w.corner_radius = CornerRadius::same(radius::M);
            w.expansion = 0.0;
        }
    });
}
