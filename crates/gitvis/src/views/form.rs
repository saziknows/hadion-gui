//! Form controls for pages (create a pull request, dispatch a workflow,
//! publish a release, set a secret, …), in the same style as the dialogs:
//! sunken inputs with a hairline border that turns accent while focused,
//! labels above inputs in the medium weight, help text in the secondary
//! colour, buttons right-aligned with the main action rightmost.
//!
//! Every input takes an `id_salt` so it keeps focus when the layout around
//! it changes (see CLAUDE.md: widgets that move need a stable id).

#![allow(dead_code)] // Used by the DevOps pages; removed at integration.
use std::hash::Hash;

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Label, Layout, Margin, Response, RichText,
    Sense, Stroke, Ui, pos2, vec2,
};

use crate::theme::{Tone, medium, pal, radius, size, space};
use crate::views::icons::{self, Icon, IconButton};

/// Height of a single-line input, matching the kit's buttons.
pub const INPUT_H: f32 = 30.0;

fn input_frame(ui: &Ui, focused: bool, invalid: bool) -> Frame {
    let p = *pal(ui);
    Frame::NONE
        .fill(if ui.visuals().dark_mode {
            p.chrome
        } else {
            p.canvas
        })
        .stroke(Stroke::new(
            1.0,
            if invalid {
                p.danger
            } else if focused {
                p.accent
            } else {
                p.border_strong
            },
        ))
        .corner_radius(CornerRadius::same(radius::M))
        .inner_margin(Margin::symmetric(10, 4))
}

/// Options for [`input`].
#[derive(Clone, Copy, Default)]
pub struct InputOpts<'a> {
    pub hint: &'a str,
    /// `f32::INFINITY` (the default when 0) fills the row.
    pub width: f32,
    pub password: bool,
    pub mono: bool,
    pub invalid: bool,
    /// Rows for a multi-line input (0 or 1 = single line).
    pub rows: usize,
    pub enabled: bool,
}

impl<'a> InputOpts<'a> {
    pub fn new(hint: &'a str) -> Self {
        Self {
            hint,
            width: f32::INFINITY,
            password: false,
            mono: false,
            invalid: false,
            rows: 1,
            enabled: true,
        }
    }
    pub fn width(mut self, w: f32) -> Self {
        self.width = w;
        self
    }
    pub fn password(mut self) -> Self {
        self.password = true;
        self
    }
    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }
    pub fn invalid(mut self, invalid: bool) -> Self {
        self.invalid = invalid;
        self
    }
    pub fn rows(mut self, rows: usize) -> Self {
        self.rows = rows;
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// A text input. Single-line text is vertically centred.
pub fn input(
    ui: &mut Ui,
    id_salt: impl Hash + std::fmt::Debug,
    value: &mut String,
    opts: InputOpts,
) -> Response {
    let p = *pal(ui);
    let id = ui.make_persistent_id(("form_input", id_salt));
    let focused = ui.memory(|m| m.has_focus(id));
    let width = if opts.width <= 0.0 {
        f32::INFINITY
    } else {
        opts.width
    };
    let font = if opts.mono {
        FontId::monospace(size::MONO)
    } else {
        FontId::proportional(size::BODY)
    };
    input_frame(ui, focused, opts.invalid)
        .show(ui, |ui| {
            ui.add_enabled_ui(opts.enabled, |ui| {
                let edit = if opts.rows > 1 {
                    egui::TextEdit::multiline(value)
                        .desired_rows(opts.rows)
                        .margin(Margin::symmetric(0, 4))
                } else {
                    egui::TextEdit::singleline(value)
                        .margin(Margin::ZERO)
                        .align(Align2::LEFT_CENTER)
                        .min_size(vec2(0.0, INPUT_H - 8.0))
                        .password(opts.password)
                };
                ui.add(
                    edit.id(id)
                        .frame(Frame::NONE)
                        .font(font)
                        .text_color(p.text)
                        .hint_text(RichText::new(opts.hint).color(p.text_3))
                        .desired_width(width),
                )
            })
            .inner
        })
        .inner
}

/// Single-line input filling the row.
pub fn text(
    ui: &mut Ui,
    id_salt: impl Hash + std::fmt::Debug,
    value: &mut String,
    hint: &str,
) -> Response {
    input(ui, id_salt, value, InputOpts::new(hint))
}

/// A masked input for secret values. The caller must clear the string
/// once the value is sent.
pub fn password(
    ui: &mut Ui,
    id_salt: impl Hash + std::fmt::Debug,
    value: &mut String,
    hint: &str,
) -> Response {
    input(ui, id_salt, value, InputOpts::new(hint).password())
}

/// A multi-line input (descriptions, comments).
pub fn area(
    ui: &mut Ui,
    id_salt: impl Hash + std::fmt::Debug,
    value: &mut String,
    hint: &str,
    rows: usize,
) -> Response {
    input(ui, id_salt, value, InputOpts::new(hint).rows(rows.max(2)))
}

/// A multi-line monospace editor (YAML, JSON, CODEOWNERS, commands).
pub fn code(
    ui: &mut Ui,
    id_salt: impl Hash + std::fmt::Debug,
    value: &mut String,
    rows: usize,
) -> Response {
    input(
        ui,
        id_salt,
        value,
        InputOpts::new("").rows(rows.max(2)).mono(),
    )
}

/// A label above a control, with optional help text under it.
pub fn field<R>(ui: &mut Ui, label: &str, help: Option<&str>, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = *pal(ui);
    ui.add_space(2.0);
    ui.label(
        RichText::new(label)
            .size(size::SMALL)
            .family(medium())
            .color(p.text_2),
    );
    ui.add_space(2.0);
    let r = add(ui);
    if let Some(h) = help.filter(|h| !h.is_empty()) {
        ui.add_space(2.0);
        ui.add(Label::new(RichText::new(h).size(size::SMALL).color(p.text_3)).wrap());
    }
    ui.add_space(space::S);
    r
}

/// A label on the left (fixed width) and a control on the right, for
/// compact settings forms.
pub fn row<R>(ui: &mut Ui, label_w: f32, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = *pal(ui);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(label_w, INPUT_H), Sense::hover());
        ui.painter().text(
            pos2(rect.left(), rect.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::new(size::SMALL, medium()),
            p.text_2,
        );
        add(ui)
    })
    .inner
}

/// A checkbox whose box stays visible on every surface.
pub fn checkbox(ui: &mut Ui, value: &mut bool, label: &str) -> Response {
    let p = *pal(ui);
    let g = ui.painter().layout_no_wrap(
        label.to_owned(),
        FontId::proportional(size::BODY),
        Color32::PLACEHOLDER,
    );
    let (rect, mut resp) =
        ui.allocate_exact_size(vec2(16.0 + 8.0 + g.size().x, 24.0), Sense::click());
    if resp.clicked() {
        *value = !*value;
        resp.mark_changed();
    }
    let hovered = resp.hovered();
    let bx =
        egui::Rect::from_center_size(pos2(rect.left() + 8.0, rect.center().y), vec2(16.0, 16.0));
    if *value {
        let fill = if hovered {
            p.accent.lerp_to_gamma(Color32::WHITE, 0.10)
        } else {
            p.accent
        };
        ui.painter()
            .rect_filled(bx, CornerRadius::same(radius::S), fill);
        icons::paint(ui.painter(), Icon::Check, bx.center(), 11.0, Color32::WHITE);
    } else {
        ui.painter().rect(
            bx,
            CornerRadius::same(radius::S),
            if ui.visuals().dark_mode {
                p.chrome
            } else {
                p.canvas
            },
            Stroke::new(1.0, if hovered { p.text_3 } else { p.border_strong }),
            egui::StrokeKind::Inside,
        );
    }
    ui.painter().galley(
        pos2(bx.right() + 8.0, rect.center().y - g.size().y / 2.0),
        g,
        if hovered { p.text_strong } else { p.text },
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A radio mark with a label; returns true when clicked.
pub fn radio(ui: &mut Ui, selected: bool, label: &str) -> bool {
    let p = *pal(ui);
    let g = ui.painter().layout_no_wrap(
        label.to_owned(),
        FontId::proportional(size::BODY),
        Color32::PLACEHOLDER,
    );
    let (rect, resp) = ui.allocate_exact_size(vec2(16.0 + 8.0 + g.size().x, 24.0), Sense::click());
    let c = pos2(rect.left() + 8.0, rect.center().y);
    let hovered = resp.hovered();
    ui.painter().circle_stroke(
        c,
        7.0,
        Stroke::new(
            1.2,
            if selected {
                p.accent
            } else if hovered {
                p.text_3
            } else {
                p.border_strong
            },
        ),
    );
    if selected {
        ui.painter().circle_filled(c, 4.0, p.accent);
    }
    ui.painter().galley(
        pos2(c.x + 16.0, rect.center().y - g.size().y / 2.0),
        g,
        if hovered { p.text_strong } else { p.text },
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// A drop-down choice. `options` are `(value, label)`; returns true when
/// the value changed.
pub fn select<T: PartialEq + Clone>(
    ui: &mut Ui,
    id_salt: impl Hash + std::fmt::Debug,
    value: &mut T,
    options: &[(T, String)],
    width: f32,
) -> bool {
    let p = *pal(ui);
    let current = options
        .iter()
        .find(|(v, _)| v == value)
        .map(|(_, l)| l.clone())
        .unwrap_or_default();
    let mut changed = false;
    let id = ui.make_persistent_id(("form_select", id_salt));
    ui.scope(|ui| {
        let v = ui.visuals_mut();
        let fill = if v.dark_mode { p.chrome } else { p.canvas };
        v.widgets.inactive.weak_bg_fill = fill;
        v.widgets.inactive.bg_fill = fill;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.border_strong);
        v.widgets.hovered.weak_bg_fill = fill;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.text_3);
        v.widgets.open.weak_bg_fill = fill;
        v.widgets.open.bg_stroke = Stroke::new(1.0, p.accent);
        ui.spacing_mut().interact_size.y = INPUT_H;
        ui.spacing_mut().button_padding = vec2(10.0, 6.0);
        egui::ComboBox::from_id_salt(id)
            .selected_text(RichText::new(current).size(size::BODY).color(p.text))
            .width(width)
            .height(320.0)
            .show_ui(ui, |ui| {
                for (v, label) in options {
                    if ui
                        .selectable_label(*v == *value, RichText::new(label).size(size::BODY))
                        .clicked()
                        && *v != *value
                    {
                        *value = v.clone();
                        changed = true;
                    }
                }
            });
    });
    changed
}

/// Chips with a remove cross, then an input that adds an entry on Enter
/// or comma; `suggestions` matching the typed text are offered underneath.
/// Returns true when the list changed.
pub fn tags(
    ui: &mut Ui,
    id_salt: impl Hash + Copy + std::fmt::Debug,
    items: &mut Vec<String>,
    buffer: &mut String,
    hint: &str,
    suggestions: &[String],
) -> bool {
    let p = *pal(ui);
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        let mut remove = None;
        for (i, item) in items.iter().enumerate() {
            let g = ui.painter().layout_no_wrap(
                item.clone(),
                FontId::new(size::SMALL, medium()),
                p.text,
            );
            let (rect, resp) =
                ui.allocate_exact_size(vec2(g.size().x + 34.0, 24.0), Sense::click());
            ui.painter().rect(
                rect,
                CornerRadius::same(radius::M),
                if resp.hovered() { p.hover } else { p.raised },
                Stroke::new(1.0, p.border),
                egui::StrokeKind::Inside,
            );
            ui.painter().galley(
                pos2(rect.left() + 10.0, rect.center().y - g.size().y / 2.0),
                g,
                p.text,
            );
            icons::paint(
                ui.painter(),
                Icon::Close,
                pos2(rect.right() - 12.0, rect.center().y),
                10.0,
                if resp.hovered() {
                    p.text_strong
                } else {
                    p.text_3
                },
            );
            if resp
                .on_hover_text("Remove")
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                remove = Some(i);
            }
        }
        if let Some(i) = remove {
            items.remove(i);
            changed = true;
        }
    });
    let resp = input(ui, (id_salt, "tags"), buffer, InputOpts::new(hint));
    let commit = |items: &mut Vec<String>, value: &str| -> bool {
        let v = value.trim().trim_start_matches('@').to_owned();
        if v.is_empty() || items.iter().any(|i| i.eq_ignore_ascii_case(&v)) {
            return false;
        }
        items.push(v);
        true
    };
    if buffer.contains(',') {
        let parts: Vec<String> = buffer.split(',').map(str::to_owned).collect();
        for part in &parts[..parts.len() - 1] {
            changed |= commit(items, part);
        }
        *buffer = parts.last().cloned().unwrap_or_default();
    }
    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        changed |= commit(items, &buffer.clone());
        buffer.clear();
        resp.request_focus();
    }
    let needle = buffer.trim().to_lowercase();
    if !needle.is_empty() {
        let matches: Vec<&String> = suggestions
            .iter()
            .filter(|s| s.to_lowercase().contains(&needle))
            .filter(|s| !items.iter().any(|i| i.eq_ignore_ascii_case(s)))
            .take(8)
            .collect();
        if !matches.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                for m in matches {
                    if IconButton::text(m).compact().show(ui).clicked() {
                        changed |= commit(items, m);
                        buffer.clear();
                    }
                }
            });
        }
    }
    changed
}

/// The main action of a form.
pub fn primary(ui: &mut Ui, label: &str, enabled: bool) -> Response {
    IconButton::text(label)
        .primary(true)
        .enabled(enabled)
        .show(ui)
}

/// A secondary (ghost) button.
pub fn button(ui: &mut Ui, label: &str, enabled: bool) -> Response {
    IconButton::text(label).enabled(enabled).show(ui)
}

/// A filled button in the danger colour.
pub fn danger(ui: &mut Ui, label: &str, enabled: bool) -> Response {
    let p = *pal(ui);
    let g = ui.painter().layout_no_wrap(
        label.to_owned(),
        FontId::new(size::BODY, medium()),
        Color32::WHITE,
    );
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 22.0, 30.0), sense);
    let fill = if !enabled {
        p.danger.gamma_multiply(0.38)
    } else if resp.is_pointer_button_down_on() {
        p.danger.lerp_to_gamma(Color32::BLACK, 0.15)
    } else if resp.hovered() {
        p.danger.lerp_to_gamma(Color32::WHITE, 0.10)
    } else {
        p.danger
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(radius::M), fill);
    ui.painter().galley(
        pos2(rect.left() + 11.0, rect.center().y - g.size().y / 2.0),
        g,
        Color32::WHITE,
    );
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// A right-aligned row of buttons; add the main action first (it ends up
/// rightmost).
pub fn actions_row(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.add_space(space::S);
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), 32.0),
        Layout::right_to_left(Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = space::S;
            add(ui)
        },
    );
}

/// A framed block on a page (a form, a merge box): raised fill, hairline
/// border, comfortable padding. `tone` tints the border (e.g. Danger for
/// destructive settings).
pub fn card<R>(ui: &mut Ui, tone: Tone, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = *pal(ui);
    let border = match tone {
        Tone::Neutral => p.border,
        t => t.color(&p).gamma_multiply(0.7),
    };
    Frame::NONE
        .fill(p.raised)
        .stroke(Stroke::new(1.0, border))
        .corner_radius(CornerRadius::same(radius::L))
        .inner_margin(Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A validation or request error under a form.
pub fn error(ui: &mut Ui, message: &str) {
    if message.is_empty() {
        return;
    }
    let c = pal(ui).danger;
    ui.add(Label::new(RichText::new(message).size(size::SMALL).color(c)).wrap());
}
