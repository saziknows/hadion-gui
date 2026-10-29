//! Pieces shared by the git-extras tabs: pane frames, the missing-tool
//! card, an error card with retry, small value widgets and a read-only diff
//! renderer with syntax colours (the same look as the Tools page's other
//! tabs, re-exported from the workbench's helpers).

use egui::{Align2, Color32, FontId, Label, Response, RichText, Sense, Ui, pos2, vec2};

use crate::state::DiffDoc;
use crate::theme::{pal, size};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Tone};

pub use crate::devops::workbench::ui::{
    buttons, churn, copyable, detail_pane, empty_pane, error_card, list_pane, page,
};

/// A tool that is not installed: its name, why it is needed, how to get
/// it, and "Check again". Returns true when that was clicked.
pub fn missing_tool(ui: &mut Ui, name: &str, program: &str, why: &str, hint: &str) -> bool {
    let mut clicked = false;
    kit::callout_with(
        ui,
        Tone::Warning,
        &format!("{name} is not installed"),
        &format!("{why} Hadion looked for `{program}` and did not find it."),
        |ui| {
            ui.add_space(2.0);
            ui.add(
                Label::new(
                    RichText::new(format!("Install: {hint}"))
                        .size(size::SMALL)
                        .color(pal(ui).text),
                )
                .wrap(),
            );
            ui.add_space(4.0);
            clicked = IconButton::new(Icon::Rerun, "Check again")
                .compact()
                .show(ui)
                .clicked();
        },
    );
    clicked
}

/// A path with the platform's separators (git prints forward slashes).
pub fn native_path(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().into_owned();
    if cfg!(windows) {
        s.replace('/', "\\")
    } else {
        s
    }
}

/// A path relative to `base` when it is inside it, else the native path.
pub fn short_path(p: &std::path::Path, base: &std::path::Path) -> String {
    let norm = |x: &std::path::Path| x.to_string_lossy().replace('\\', "/").to_lowercase();
    let (ps, bs) = (norm(p), norm(base));
    let bs = bs.trim_end_matches('/');
    if ps.starts_with(&format!("{bs}/")) {
        let full = p.to_string_lossy().replace('\\', "/");
        return full.get(bs.len() + 1..).unwrap_or(&full).to_owned();
    }
    native_path(p)
}

/// `when` as "2026-10-27 14:03 · 3 hours ago".
pub fn when(secs: i64, now: i64) -> String {
    format!(
        "{}  ·  {}",
        crate::format::absolute(secs),
        crate::format::relative(secs, now)
    )
}

/// "loaded 2 min ago" for a page header.
pub fn loaded_ago(at: std::time::Instant) -> String {
    let now = crate::format::now_secs();
    format!(
        "loaded {}",
        crate::format::relative(now - at.elapsed().as_secs() as i64, now)
    )
}

/// One fact on the right of a page header.
pub fn header_note(ui: &mut Ui, text: &str) {
    let c = pal(ui).text_2;
    ui.add_space(6.0);
    ui.label(RichText::new(text).size(size::SMALL).color(c));
}

/// A simple selectable one-line row (inside a scroll pane, not
/// virtualised): `leading` letter, title, right-aligned meta. Returns the
/// response.
pub fn simple_row(
    ui: &mut Ui,
    leading: Option<(&str, Color32)>,
    title: &str,
    meta: &str,
    selected: bool,
    mono: bool,
) -> Response {
    let p = *pal(ui);
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), kit::ROW_H_ONE), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    kit::row_background(ui, rect, resp.hovered(), selected);
    let painter = ui.painter();
    let mid = rect.center().y;
    let mut x = rect.left() + 12.0;
    if let Some((letter, color)) = leading {
        painter.text(
            pos2(x + 5.0, mid),
            Align2::CENTER_CENTER,
            letter,
            FontId::monospace(size::MONO),
            color,
        );
        x += 22.0;
    }
    let mut right = rect.right() - 10.0;
    if !meta.is_empty() {
        let r = painter.text(
            pos2(right, mid),
            Align2::RIGHT_CENTER,
            meta,
            FontId::proportional(size::SMALL),
            p.text_2,
        );
        right = r.left() - 10.0;
    }
    let font = if mono {
        FontId::monospace(size::MONO)
    } else {
        FontId::proportional(size::BODY)
    };
    let color = if selected { p.text_strong } else { p.text };
    let g = crate::views::truncated(painter, title, font, color, (right - x).max(20.0));
    painter.galley(pos2(x, mid - g.size().y / 2.0), g, color);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// A read-only unified diff of one stash or patch file.
pub fn diff_view(ui: &mut Ui, id: &str, doc: &DiffDoc) {
    crate::devops::workbench::ui::diff_view_or(
        ui,
        id,
        doc,
        "No text changes (a rename, a mode change or an empty file).",
    );
}

/// A diff document of one patch file, highlighted (call off the UI thread).
pub fn diff_doc(raw: &[u8], path: &str, dark: bool) -> DiffDoc {
    let mut diffs = hadion_core::diff::parse(raw);
    if diffs.is_empty() {
        return DiffDoc {
            diff: Default::default(),
            spans: Vec::new(),
            language: None,
        };
    }
    let diff = diffs.remove(0);
    let spans = crate::highlight::highlight(&diff, path, dark);
    DiffDoc {
        language: crate::highlight::language_for(path),
        diff,
        spans,
    }
}
