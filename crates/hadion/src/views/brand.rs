//! The Hadion logo (`docs/brand` has the full set and the rules): the Tile,
//! which is also the window icon, and the lockup (mark and wordmark) in its
//! dark- and light-background versions. Each image is scaled once to the
//! exact pixel size it is drawn at (Lanczos, on premultiplied alpha), since
//! the wgpu renderer has no mipmaps and a large texture drawn small aliases.

use egui::{Color32, ColorImage, Pos2, Rect, Response, Sense, TextureHandle, TextureOptions, Ui};
use egui::{pos2, vec2};

/// The Tile at 256 px: the window and taskbar icon (the exe carries
/// `assets/brand/hadion.ico`, embedded by `build.rs`).
pub const WINDOW_ICON: &[u8] = include_bytes!("../../assets/brand/tile-256.png");
const LOCKUP_DARK: &[u8] = include_bytes!("../../assets/brand/lockup-dark.png");
const LOCKUP_LIGHT: &[u8] = include_bytes!("../../assets/brand/lockup-light.png");

/// Width over height of the lockup images, clear space included.
const LOCKUP_ASPECT: f32 = 2000.0 / 658.0;

#[derive(Clone)]
struct Scaled {
    px: [usize; 2],
    tex: TextureHandle,
}

fn texture(
    ui: &Ui,
    name: &'static str,
    bytes: &'static [u8],
    px: [usize; 2],
) -> Option<TextureHandle> {
    let id = egui::Id::unique(("brand", name));
    let ctx = ui.ctx();
    if let Some(s) = ctx.data(|d| d.get_temp::<Scaled>(id))
        && s.px == px
    {
        return Some(s.tex);
    }
    let mut img = image::load_from_memory(bytes).ok()?.into_rgba8();
    // Premultiply first: transparent pixels are black, and scaling straight
    // alpha would pull that black into the edges.
    for p in img.pixels_mut() {
        let a = u16::from(p[3]);
        for c in 0..3 {
            p[c] = ((u16::from(p[c]) * a + 127) / 255) as u8;
        }
    }
    let mut scaled = image::imageops::resize(
        &img,
        px[0] as u32,
        px[1] as u32,
        image::imageops::FilterType::Lanczos3,
    );
    // Lanczos rings a little; a colour above its alpha would glow.
    for p in scaled.pixels_mut() {
        for c in 0..3 {
            p[c] = p[c].min(p[3]);
        }
    }
    let color = ColorImage::from_rgba_premultiplied(px, scaled.as_raw());
    let tex = ctx.load_texture(format!("brand-{name}"), color, TextureOptions::LINEAR);
    ctx.data_mut(|d| {
        d.insert_temp(
            id,
            Scaled {
                px,
                tex: tex.clone(),
            },
        )
    });
    Some(tex)
}

/// Draw `bytes` into `rect`, snapped to whole pixels so it is not blurred.
fn paint(ui: &Ui, rect: Rect, name: &'static str, bytes: &'static [u8]) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    let ppp = ui.ctx().pixels_per_point();
    let px = [
        (rect.width() * ppp).round().max(1.0) as usize,
        (rect.height() * ppp).round().max(1.0) as usize,
    ];
    let min = Pos2::new(
        (rect.min.x * ppp).round() / ppp,
        (rect.min.y * ppp).round() / ppp,
    );
    let rect = Rect::from_min_size(min, vec2(px[0] as f32 / ppp, px[1] as f32 / ppp));
    if let Some(tex) = texture(ui, name, bytes, px) {
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        ui.painter().image(tex.id(), rect, uv, Color32::WHITE);
    }
}

/// The Tile (the app icon) drawn into `rect` without taking layout space.
pub fn paint_tile(ui: &Ui, rect: Rect) {
    paint(ui, rect, "tile", WINDOW_ICON);
}

/// The lockup `width` points wide, in the version for the current theme.
/// The image includes the logo's clear space on every side.
pub fn lockup(ui: &mut Ui, width: f32) -> Response {
    let (name, bytes) = if ui.visuals().dark_mode {
        ("lockup-dark", LOCKUP_DARK)
    } else {
        ("lockup-light", LOCKUP_LIGHT)
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(width, width / LOCKUP_ASPECT), Sense::hover());
    paint(ui, rect, name, bytes);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_images_decode_with_the_expected_shapes() {
        let icon = image::load_from_memory(WINDOW_ICON).unwrap();
        assert_eq!((icon.width(), icon.height()), (256, 256));
        for bytes in [LOCKUP_DARK, LOCKUP_LIGHT] {
            let img = image::load_from_memory(bytes).unwrap();
            let aspect = img.width() as f32 / img.height() as f32;
            assert!((aspect - LOCKUP_ASPECT).abs() < 0.01, "{aspect}");
        }
    }
}
