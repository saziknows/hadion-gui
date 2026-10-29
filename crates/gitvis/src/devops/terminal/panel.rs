//! The panel: a tab strip (shell mark, title, exit badge, close; "+" with
//! the shells found; on the right the active terminal's size, "cd to the
//! repository" and hide) above the active terminal's grid.

use std::time::{Duration, Instant};

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align, Align2, Color32, CornerRadius, EventFilter, FontId, Id, Layout, Pos2, Rect, RichText,
    Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2,
};
use gitvis_core::devops::terminal::Status;
use gitvis_core::devops::terminal::keys::{
    self, Key as TKey, Mods, MouseButton, MouseEvent, MouseKind, MouseMode,
};
use gitvis_core::devops::terminal::screen::{
    ANSI_DARK, ANSI_LIGHT, Point, Selection, View, resolve,
};
use gitvis_core::devops::terminal::shells::native_path;

use super::{Act, CursorStyle, State, Tab, act};
use crate::devops::global::GlobalView;
use crate::format;
use crate::state::Action;
use crate::theme::{self, Palette, Tone, pal, radius, size, space};
use crate::views::icons::{Icon, IconButton};
use crate::views::{kit, truncated};

const STRIP_H: f32 = 34.0;
const TAB_H: f32 = 30.0;
const PAD_X: f32 = 10.0;
const PAD_Y: f32 = 6.0;
const SCROLLBAR_W: f32 = 10.0;

fn grid_id() -> Id {
    Id::unique("terminal_grid")
}

fn c32([r, g, b]: [u8; 3]) -> Color32 {
    Color32::from_rgb(r, g, b)
}

fn rgb(c: Color32) -> [u8; 3] {
    [c.r(), c.g(), c.b()]
}

pub(super) fn show(ui: &mut Ui, st: &mut State, v: &GlobalView, actions: &mut Vec<Action>) {
    let ctx = ui.ctx().clone();
    // The panel just opened: focus the terminal, and start one if none is
    // open (like toggling a terminal in an editor).
    let pass = ctx.cumulative_pass_nr();
    let opened = st.last_pass.is_none_or(|last| last + 2 < pass);
    st.last_pass = Some(pass);
    if opened {
        st.focus_next = true;
        if st.tabs.is_empty() && st.pending_new.is_empty() {
            act(st, v, actions, Act::New(None));
        }
    }
    if st.env.egui.is_none() {
        st.env.egui = Some(ctx.clone());
    }
    // Without a repository the acts run here, so keep the prefs current.
    if v.repos.is_empty() && st.env.prefs != v.settings.ext.terminal {
        st.env.prefs = v.settings.ext.terminal.clone();
    }

    let full = ui.available_rect_before_wrap();
    let strip_rect = Rect::from_min_size(full.min, vec2(full.width(), STRIP_H));
    let body = Rect::from_min_max(pos2(full.left(), strip_rect.bottom()), full.max);
    let mut strip_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(strip_rect)
            .layout(Layout::right_to_left(Align::Center)),
    );
    strip(&mut strip_ui, st, v, actions);

    let p = *pal(ui);
    ui.painter().rect_filled(body, 0, p.canvas);
    let mut body_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(body)
            .layout(Layout::top_down(Align::Center)),
    );
    body_view(&mut body_ui, st, v, body, actions);
    ui.allocate_rect(full, Sense::hover());
}

// ---------------------------------------------------------------------------
// Tab strip
// ---------------------------------------------------------------------------

fn strip(ui: &mut Ui, st: &mut State, v: &GlobalView, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    ui.spacing_mut().item_spacing.x = 4.0;
    ui.add_space(6.0);
    if IconButton::icon(Icon::ChevronDown)
        .compact()
        .show(ui)
        .on_hover_text("Hide the terminal (Ctrl+`). The shells keep running.")
        .clicked()
    {
        actions.push(Action::ToggleTerminal);
    }
    let now = Instant::now();
    if let Some(at) = st.copied_at {
        if now.duration_since(at) < Duration::from_millis(1600) {
            ui.label(RichText::new("Copied").size(size::SMALL).color(p.success));
            ui.ctx().request_repaint_after(Duration::from_millis(400));
        } else {
            st.copied_at = None;
        }
    }
    let repo = v.active_repo();
    let mut picked: Option<Act> = None;
    if let Some(tab) = st.tabs.get(st.active) {
        match tab.status().clone() {
            Status::Exited { code, after_ms } => {
                let id = tab.id;
                if IconButton::new(Icon::Rerun, "Restart")
                    .compact()
                    .show(ui)
                    .on_hover_text(format!(
                        "Start {} again in {}",
                        tab.shell.name,
                        native_path(&tab.cwd)
                    ))
                    .clicked()
                {
                    picked = Some(Act::Restart(id));
                }
                ui.label(
                    RichText::new(format!(
                        "Exited with code {code} after {}",
                        format::millis(after_ms)
                    ))
                    .size(size::SMALL)
                    .color(if code == 0 { p.text_2 } else { p.danger }),
                );
            }
            Status::Running { .. } => {
                let alt = tab.view.as_ref().is_some_and(|(_, v)| v.alt_screen);
                let tip = match repo {
                    Some(r) if alt => format!(
                        "A full-screen program is running; quit it to change to {}",
                        native_path(&r.workdir)
                    ),
                    Some(r) => format!(
                        "Type the command that changes to {}\n{}",
                        native_path(&r.workdir),
                        gitvis_core::devops::terminal::shells::cd_command(
                            tab.shell.kind,
                            &r.workdir
                        )
                    ),
                    None => "No repository is open".to_owned(),
                };
                let enabled = repo.is_some() && !alt;
                if IconButton::new(Icon::Folder, "Go to repository")
                    .compact()
                    .enabled(enabled)
                    .show(ui)
                    .on_hover_text(tip)
                    .clicked()
                {
                    picked = Some(Act::CdRepo);
                }
                if let Some((_, view)) = &tab.view {
                    ui.label(
                        RichText::new(format!("{} × {}", view.cols, view.rows))
                            .size(size::SMALL)
                            .color(p.text_3),
                    )
                    .on_hover_text("Columns × rows");
                }
            }
            _ => {}
        }
    }
    if let Some(a) = picked {
        act(st, v, actions, a);
    }
    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.add_space(6.0);
        tabs(ui, st, v, actions);
        plus_menu(ui, st, v, actions);
    });
}

fn tabs(ui: &mut Ui, st: &mut State, v: &GlobalView, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let now = Instant::now();
    let n = st.tabs.len().max(1);
    // Room left for the "+" button; titles share the rest.
    let room = (ui.available_width() - 40.0).max(120.0);
    let per_tab = (room / n as f32 - 2.0).clamp(90.0, 280.0);
    let mut picked: Option<Act> = None;
    for (i, tab) in st.tabs.iter().enumerate() {
        let is_active = i == st.active;
        let status = tab.status().clone();
        let badge = match &status {
            Status::Exited { code, .. } => Some((
                format!("exit {code}"),
                if *code == 0 {
                    Tone::Neutral
                } else {
                    Tone::Danger
                },
            )),
            Status::Failed(_) => Some(("failed".to_owned(), Tone::Danger)),
            _ => None,
        };
        let painter = ui.painter().clone();
        let mark_font = FontId::monospace(size::SMALL);
        let mark = painter.layout_no_wrap(
            tab.shell.kind.mark().to_owned(),
            mark_font.clone(),
            p.text_3,
        );
        let badge_w = badge
            .as_ref()
            .map(|(t, _)| kit::badge_width(&painter, t) + 6.0)
            .unwrap_or(0.0);
        let fixed = 12.0 + mark.size().x + 8.0 + badge_w + 30.0;
        let title_font = if is_active {
            FontId::new(size::SMALL + 0.5, theme::medium())
        } else {
            FontId::proportional(size::SMALL + 0.5)
        };
        let title_color = if is_active { p.text_strong } else { p.text_2 };
        let title = truncated(
            &painter,
            &tab.title(),
            title_font,
            title_color,
            (per_tab - fixed).max(30.0),
        );
        let w = fixed + title.size().x;
        let (rect, resp) = ui.allocate_exact_size(vec2(w, TAB_H), Sense::click());
        let hovered = ui.rect_contains_pointer(rect);
        let tab_rect = Rect::from_min_max(pos2(rect.left(), rect.top() + 4.0), rect.max);
        let corners = CornerRadius {
            nw: radius::L,
            ne: radius::L,
            sw: 0,
            se: 0,
        };
        if is_active {
            painter.rect_filled(tab_rect, corners, p.canvas);
        } else if hovered {
            painter.rect_filled(tab_rect, corners, p.hover.gamma_multiply(0.6));
        }
        if let Some(until) = tab.flash_until
            && until > now
        {
            let left = (until - now).as_secs_f32() / 0.9;
            painter.rect_filled(
                tab_rect,
                corners,
                p.warning.gamma_multiply(0.35 * left.clamp(0.0, 1.0)),
            );
            ui.ctx().request_repaint_after(Duration::from_millis(30));
        }
        let cy = tab_rect.center().y;
        let mark_w = mark.size().x;
        painter.galley(
            pos2(rect.left() + 12.0, cy - mark.size().y / 2.0),
            mark,
            p.text_3,
        );
        let x = rect.left() + 12.0 + mark_w + 8.0;
        painter.galley(
            pos2(x, cy - title.size().y / 2.0),
            title.clone(),
            title_color,
        );
        if let Some((text, tone)) = &badge {
            kit::paint_badge(&painter, &p, dark, rect.right() - 30.0, cy, text, *tone);
        }
        let close_center = pos2(rect.right() - 16.0, cy);
        if matches!(status, Status::Starting) {
            kit::overlay(
                ui,
                Rect::from_center_size(close_center, vec2(12.0, 12.0)),
                egui::Spinner::new().size(11.0).color(p.text_3),
            );
        } else if hovered || is_active {
            let close = kit::overlay(
                ui,
                Rect::from_center_size(close_center, vec2(24.0, 24.0)),
                IconButton::icon(Icon::Close).compact(),
            );
            if close
                .on_hover_text("Close this terminal (ends the shell)")
                .clicked()
            {
                picked = Some(Act::Close(tab.id));
            }
        }
        let resp = resp.on_hover_ui(|ui| tab_tooltip(ui, tab));
        if resp.clicked() && picked.is_none() {
            picked = Some(Act::Activate(i));
        }
        if resp.middle_clicked() {
            picked = Some(Act::Close(tab.id));
        }
    }
    if let Some(a) = picked {
        act(st, v, actions, a);
    }
}

fn tab_tooltip(ui: &mut Ui, tab: &Tab) {
    let p = *pal(ui);
    ui.set_max_width(460.0);
    ui.label(
        RichText::new(&tab.shell.name)
            .family(theme::semibold())
            .color(p.text_strong),
    );
    ui.label(kit::mono(tab.shell.command_line()).color(p.text_2));
    ui.label(
        RichText::new(format!("In {}", native_path(&tab.cwd)))
            .size(size::SMALL)
            .color(p.text),
    );
    let status = match tab.status() {
        Status::Starting => "Starting".to_owned(),
        Status::Running { pid: Some(pid) } => format!("Running, process {pid}"),
        Status::Running { pid: None } => "Running".to_owned(),
        Status::Exited { code, after_ms } => {
            format!(
                "Exited with code {code} after {}",
                format::millis(*after_ms)
            )
        }
        Status::Failed(e) => format!("Could not start: {e}"),
    };
    ui.label(RichText::new(status).size(size::SMALL).color(p.text_2));
    if let Some(t) = tab.session.try_term() {
        ui.label(
            RichText::new(format!(
                "{} received, {} sent, {} history lines{}",
                format::bytes(t.bytes_in as usize),
                format::bytes(t.bytes_out as usize),
                format::count(t.screen.scrollback_len()),
                if tab.restarts > 0 {
                    format!(", restarted {}×", tab.restarts)
                } else {
                    String::new()
                }
            ))
            .size(size::SMALL)
            .color(p.text_3),
        );
    }
}

fn plus_menu(ui: &mut Ui, st: &mut State, v: &GlobalView, actions: &mut Vec<Action>) {
    let plus = IconButton::icon(Icon::Plus)
        .compact()
        .show(ui)
        .on_hover_text("New terminal");
    let mut picked: Option<Act> = None;
    let default = v.settings.ext.terminal.default_shell.clone();
    if st.open_menu.as_deref() == Some("new") {
        st.open_menu = None;
        egui::Popup::open_id(ui.ctx(), Id::unique("terminal_new_menu"));
    }
    egui::Popup::menu(&plus)
        .id(Id::unique("terminal_new_menu"))
        .show(|ui| {
            ui.set_min_width(260.0);
            match &st.shells {
                super::Shells::Ready(d) => {
                    let default_id = d.pick(&default).map(|s| s.id.clone());
                    for s in &d.shells {
                        let label = if Some(&s.id) == default_id.as_ref() {
                            format!("{}  (default)", s.name)
                        } else {
                            s.name.clone()
                        };
                        if ui
                            .add(egui::Button::new(label).shortcut_text(s.kind.mark()))
                            .on_hover_text(s.command_line())
                            .clicked()
                        {
                            picked = Some(Act::New(Some(s.id.clone())));
                            ui.close();
                        }
                    }
                    if d.shells.is_empty() {
                        ui.label("No shells found");
                    }
                    ui.separator();
                    if ui.button("Look for shells again").clicked() {
                        picked = Some(Act::Discover);
                        ui.close();
                    }
                }
                _ => {
                    kit::loading(ui, "Looking for shells…");
                }
            }
        });
    if plus.clicked() {
        st.discover(false);
    }
    if let Some(a) = picked {
        act(st, v, actions, a);
    }
}

// ---------------------------------------------------------------------------
// Body
// ---------------------------------------------------------------------------

fn body_view(ui: &mut Ui, st: &mut State, v: &GlobalView, body: Rect, actions: &mut Vec<Action>) {
    if st.tabs.is_empty() {
        st.focused = false;
        if !st.pending_new.is_empty() || matches!(st.shells, super::Shells::Loading(..)) {
            ui.add_space(space::XL);
            kit::loading(ui, "Looking for shells…");
            return;
        }
        let shells: Vec<(String, String)> = st
            .shells
            .ready()
            .map(|d| {
                d.shells
                    .iter()
                    .map(|s| (s.id.clone(), s.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let dir = st.start_dir();
        if shells.is_empty() && st.shells.ready().is_some() {
            if kit::empty_state(
                ui,
                Icon::Terminal,
                "No shell found",
                "gitvis looks for PowerShell 7, Windows PowerShell, cmd, Git Bash and WSL (or $SHELL elsewhere). Settings → Terminal lists where it looked.",
                Some((Icon::Rerun, "Check again")),
            ) {
                act(st, v, actions, Act::Discover);
            }
            return;
        }
        let folder = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| native_path(&dir));
        kit::empty_state(
            ui,
            Icon::Terminal,
            "No terminal open",
            &format!("Pick a shell; it starts in {folder}. Ctrl+` hides this panel."),
            None,
        );
        ui.add_space(space::M);
        // The shells as one centred row of buttons.
        let font = FontId::new(size::BODY, theme::medium());
        let widths: Vec<f32> = shells
            .iter()
            .map(|(_, name)| {
                ui.painter()
                    .layout_no_wrap(name.clone(), font.clone(), Color32::WHITE)
                    .size()
                    .x
                    + 2.0 * 11.0
                    + 15.0
                    + 7.0
            })
            .collect();
        let gap = space::S;
        let total = widths.iter().sum::<f32>() + gap * widths.len().saturating_sub(1) as f32;
        let mut picked = None;
        ui.allocate_ui_with_layout(
            vec2(body.width(), 32.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = gap;
                ui.add_space(((body.width() - total) / 2.0).max(space::M));
                for (id, name) in &shells {
                    if IconButton::new(Icon::Terminal, name).show(ui).clicked() {
                        picked = Some(Act::New(Some(id.clone())));
                    }
                }
            },
        );
        if let Some(a) = picked {
            act(st, v, actions, a);
        }
        return;
    }
    st.active = st.active.min(st.tabs.len() - 1);
    let status = st.tabs[st.active].status().clone();
    if let Status::Failed(err) = status {
        st.focused = false;
        failed_view(ui, st, v, &err, actions);
        return;
    }
    grid(ui, st, v, body, actions);
    if matches!(status, Status::Starting) {
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(body.shrink2(vec2(PAD_X, PAD_Y)))
                .layout(Layout::top_down(Align::Min)),
        );
        let name = st.tabs[st.active].shell.name.clone();
        kit::loading(&mut child, &format!("Starting {name}…"));
    }
}

fn failed_view(ui: &mut Ui, st: &mut State, v: &GlobalView, err: &str, actions: &mut Vec<Action>) {
    let tab = &st.tabs[st.active];
    let (id, name, cmd) = (tab.id, tab.shell.name.clone(), tab.shell.command_line());
    ui.add_space(space::XL);
    ui.scope(|ui| {
        // Never wider than the panel: `set_max_width` also widens.
        ui.set_max_width(ui.available_width().min(kit::PROSE));
        kit::callout(ui, Tone::Danger, &format!("Could not start {name}"), err);
        ui.add_space(space::S);
        kit::secondary(ui, "The command gitvis ran:");
        kit::code_block(ui, &cmd, actions);
        ui.add_space(space::S);
        let mut picked = None;
        ui.horizontal(|ui| {
            if IconButton::new(Icon::Rerun, "Try again")
                .primary(true)
                .show(ui)
                .clicked()
            {
                picked = Some(Act::Restart(id));
            }
            if IconButton::new(Icon::Close, "Close").show(ui).clicked() {
                picked = Some(Act::Close(id));
            }
        });
        if let Some(a) = picked {
            act(st, v, actions, a);
        }
    });
}

// ---------------------------------------------------------------------------
// The grid
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum MenuPick {
    Copy,
    CopyAll,
    Paste,
    SelectAll,
    Clear,
    Restart,
    Close,
}

/// Width of a character at the terminal font (non-ASCII ones are measured
/// once per font size).
fn glyph_w(st: &mut State, ctx: &egui::Context, font: &FontId, c: char, cell_w: f32) -> f32 {
    if c.is_ascii() {
        return cell_w;
    }
    if st.glyph_size != font.size {
        st.glyphs.clear();
        st.glyph_size = font.size;
    }
    *st.glyphs
        .entry(c)
        .or_insert_with(|| ctx.fonts_mut(|f| f.glyph_width(font, c)))
}

fn grid(ui: &mut Ui, st: &mut State, v: &GlobalView, rect: Rect, actions: &mut Vec<Action>) {
    let ctx = ui.ctx().clone();
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let prefs = v.settings.ext.terminal.clone();
    let font = FontId::monospace(prefs.font_size.max(size::CAPTION));
    let (cw, ch) = ctx.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    let inner = Rect::from_min_max(
        pos2(rect.left() + PAD_X, rect.top() + PAD_Y),
        pos2(rect.right() - PAD_X - SCROLLBAR_W, rect.bottom() - PAD_Y),
    );
    let rows = ((inner.height() / ch).floor() as u16).max(2);
    let cols = ((inner.width() / cw).floor() as u16).max(2);
    let active = st.active;
    let alive = st.tabs[active].status().is_alive();
    let shown = st.tabs[active].view.as_ref().map(|(_, v)| (v.rows, v.cols));
    if alive && shown != Some((rows, cols)) && !st.tabs[active].session.resize(rows, cols) {
        ctx.request_repaint();
    }

    // Interaction.
    let id = grid_id();
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    if st.focus_next {
        resp.request_focus();
        st.focus_next = false;
    }
    if resp.clicked() || resp.drag_started() || resp.secondary_clicked() || resp.middle_clicked() {
        resp.request_focus();
    }
    if resp.clicked_elsewhere() && resp.has_focus() {
        resp.surrender_focus();
    }
    let focused = resp.has_focus();
    st.focused = focused;
    if focused {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                id,
                EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
    }
    if resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::Text);
    }

    st.tabs[active].view();
    let Some((generation, view)) = st.tabs[active].view.take() else {
        return;
    };
    mouse(ui, st, &resp, &view, inner, cw, ch, &prefs);
    let tab_id = st.tabs[active].id;
    let menu = context_menu(ui, st, &resp, &view);
    draw(
        ui, st, &view, rect, inner, &font, cw, ch, focused, alive, &prefs, dark, &p,
    );
    scrollbar(ui, st, &view, rect, inner);
    if view.offset > 0 {
        jump_to_bottom(ui, st, &view, inner, &p);
    }
    if focused && let Some((cr, cc)) = view.cursor {
        let cursor_rect = Rect::from_min_size(
            pos2(inner.left() + cc as f32 * cw, inner.top() + cr as f32 * ch),
            vec2(cw, ch),
        );
        ctx.output_mut(|o| {
            o.ime = Some(egui::output::IMEOutput {
                purpose: egui::IMEPurpose::Terminal,
                rect: inner,
                cursor_rect,
                should_interrupt_composition: false,
            })
        });
    }
    if let Some(t) = st.tabs.get_mut(active)
        && t.view.is_none()
    {
        t.view = Some((generation, view));
    }
    if let Some(pick) = menu {
        st.focus_next = true;
        match pick {
            MenuPick::Copy => super::copy_selection(st),
            MenuPick::CopyAll => {
                if let Some(t) = st.tabs.get(active) {
                    let text = t.session.term().screen.all_text();
                    ctx.copy_text(text);
                    st.copied_at = Some(Instant::now());
                }
            }
            MenuPick::Paste => {
                st.paste_pending = Some(tab_id);
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            }
            MenuPick::SelectAll => {
                if let Some(t) = st.tabs.get_mut(active) {
                    t.selection = Some(t.session.term().screen.select_all());
                }
            }
            MenuPick::Clear => act(st, v, actions, Act::Clear(tab_id)),
            MenuPick::Restart => act(st, v, actions, Act::Restart(tab_id)),
            MenuPick::Close => act(st, v, actions, Act::Close(tab_id)),
        }
    }
}

/// Wheel lines this frame (positive = towards older lines).
fn wheel_lines(ui: &Ui, rows: u16, ch: f32) -> f32 {
    ui.input(|i| {
        i.events
            .iter()
            .map(|e| match e {
                egui::Event::MouseWheel { unit, delta, .. } => match unit {
                    egui::MouseWheelUnit::Line => delta.y,
                    egui::MouseWheelUnit::Point => delta.y / ch,
                    egui::MouseWheelUnit::Page => delta.y * rows as f32,
                },
                _ => 0.0,
            })
            .sum()
    })
}

#[allow(clippy::too_many_arguments)]
fn mouse(
    ui: &mut Ui,
    st: &mut State,
    resp: &egui::Response,
    view: &View,
    inner: Rect,
    cw: f32,
    ch: f32,
    prefs: &super::Prefs,
) {
    let active = st.active;
    let (rows, cols) = (view.rows, view.cols);
    let cell_of = |pos: Pos2| -> (u16, u16) {
        let r = ((pos.y - inner.top()) / ch)
            .floor()
            .clamp(0.0, rows as f32 - 1.0) as u16;
        let c = ((pos.x - inner.left()) / cw)
            .floor()
            .clamp(0.0, cols as f32 - 1.0) as u16;
        (r, c)
    };
    let (modifiers, pointer, pressed, released) = ui.input(|i| {
        let b = [
            egui::PointerButton::Primary,
            egui::PointerButton::Middle,
            egui::PointerButton::Secondary,
        ];
        (
            i.modifiers,
            i.pointer.interact_pos(),
            b.map(|x| i.pointer.button_pressed(x)),
            b.map(|x| i.pointer.button_released(x)),
        )
    });
    let mods = Mods {
        ctrl: modifiers.ctrl,
        alt: modifiers.alt,
        shift: modifiers.shift,
    };
    let hovered = resp.hovered();
    let report = view.mouse_mode != MouseMode::None && !modifiers.shift;
    let tab = &mut st.tabs[active];
    let modes = keys::Modes {
        app_cursor: view.app_cursor,
        bracketed_paste: view.bracketed_paste,
    };

    // Wheel.
    if hovered {
        tab.wheel += wheel_lines(ui, rows, ch);
        let lines = tab.wheel.trunc();
        if lines != 0.0 {
            tab.wheel -= lines;
            let n = lines.abs().min(15.0) as usize;
            let up = lines > 0.0;
            if report {
                if let Some(pos) = pointer {
                    let (row, col) = cell_of(pos);
                    let button = if up {
                        MouseButton::WheelUp
                    } else {
                        MouseButton::WheelDown
                    };
                    for _ in 0..n {
                        let ev = MouseEvent {
                            kind: MouseKind::Press,
                            button,
                            row,
                            col,
                            mods,
                        };
                        if let Some(b) =
                            keys::encode_mouse(ev, view.mouse_mode, view.mouse_encoding)
                        {
                            tab.session.write(b);
                        }
                    }
                }
            } else if view.alt_screen {
                // Full-screen programs without mouse support: arrow keys.
                let key = if up { TKey::Up } else { TKey::Down };
                if let Some(b) = keys::encode_key(key, Mods::NONE, modes) {
                    tab.session.write(b.repeat(n));
                }
            } else {
                tab.scroll_by(if up { n as isize } else { -(n as isize) });
            }
        }
    }

    if report {
        tab.selecting = false;
        let buttons = [MouseButton::Left, MouseButton::Middle, MouseButton::Right];
        if let Some(pos) = pointer {
            let (row, col) = cell_of(pos);
            let send = |tab: &mut Tab, kind, button| {
                let ev = MouseEvent {
                    kind,
                    button,
                    row,
                    col,
                    mods,
                };
                if let Some(b) = keys::encode_mouse(ev, view.mouse_mode, view.mouse_encoding) {
                    tab.session.write(b);
                }
            };
            for (k, button) in buttons.iter().enumerate() {
                if pressed[k] && inner.expand(PAD_X).contains(pos) {
                    send(tab, MouseKind::Press, *button);
                    tab.mouse_down = Some(*button);
                    tab.last_mouse_cell = Some((row, col));
                }
                if released[k] && tab.mouse_down == Some(*button) {
                    send(tab, MouseKind::Release, *button);
                    tab.mouse_down = None;
                }
            }
            if tab.last_mouse_cell != Some((row, col)) {
                if let Some(b) = tab.mouse_down {
                    send(tab, MouseKind::Motion, b);
                    tab.last_mouse_cell = Some((row, col));
                } else if view.mouse_mode == MouseMode::AnyMotion && hovered {
                    send(tab, MouseKind::Motion, MouseButton::None);
                    tab.last_mouse_cell = Some((row, col));
                }
            }
        }
        return;
    }
    tab.mouse_down = None;

    // Selection.
    let point = |pos: Pos2| {
        let (r, c) = cell_of(pos);
        Point {
            row: view.top_row + r as usize,
            col: c,
        }
    };
    if resp.drag_started_by(egui::PointerButton::Primary)
        && let Some(origin) = ui.input(|i| i.pointer.press_origin())
    {
        let a = point(origin);
        tab.selection = Some(Selection::new(a, a));
        tab.selecting = true;
    }
    if tab.selecting
        && let Some(pos) = pointer
    {
        // Past the top or bottom edge: scroll while selecting.
        if pos.y < inner.top() {
            tab.scroll_by(1);
            ui.ctx().request_repaint_after(Duration::from_millis(40));
        } else if pos.y > inner.bottom() {
            tab.scroll_by(-1);
            ui.ctx().request_repaint_after(Duration::from_millis(40));
        }
        if let Some(sel) = &mut tab.selection {
            sel.head = point(pos);
        }
    }
    if resp.drag_stopped() && tab.selecting {
        tab.selecting = false;
        if tab.selection.is_some_and(|s| s.anchor == s.head) {
            tab.selection = None;
        } else if prefs.copy_on_select {
            super::copy_selection(st);
            return;
        }
    }
    if resp.triple_clicked()
        && let Some(pos) = pointer
    {
        let pt = point(pos);
        tab.selection = Some(Selection::new(
            Point {
                row: pt.row,
                col: 0,
            },
            Point {
                row: pt.row,
                col: cols - 1,
            },
        ));
    } else if resp.double_clicked()
        && let Some(pos) = pointer
    {
        let pt = point(pos);
        let (c0, c1) = tab.session.term().screen.word_at(pt);
        tab.selection = Some(Selection::new(
            Point {
                row: pt.row,
                col: c0,
            },
            Point {
                row: pt.row,
                col: c1,
            },
        ));
    } else if resp.clicked() {
        tab.selection = None;
    }
    if (resp.double_clicked() || resp.triple_clicked()) && prefs.copy_on_select {
        super::copy_selection(st);
    }
}

fn context_menu(ui: &Ui, st: &mut State, resp: &egui::Response, view: &View) -> Option<MenuPick> {
    let shift = ui.input(|i| i.modifiers.shift);
    let popup_id = egui::Popup::default_response_id(resp);
    if st.open_menu.as_deref() == Some("context") {
        st.open_menu = None;
        egui::Popup::open_id(ui.ctx(), popup_id);
    }
    let open = egui::Popup::is_id_open(ui.ctx(), popup_id);
    // Programs that read the mouse get the right click; Shift opens the menu.
    if !(open || view.mouse_mode == MouseMode::None || shift) {
        return None;
    }
    let tab = &st.tabs[st.active];
    let has_selection = tab.selection.is_some();
    let alive = tab.status().is_alive();
    let mut pick = None;
    resp.context_menu(|ui| {
        ui.set_min_width(220.0);
        let mut item = |ui: &mut Ui, label: &str, shortcut: &str, enabled: bool, p: MenuPick| {
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::new(label).shortcut_text(shortcut.to_owned()),
                )
                .clicked()
            {
                pick = Some(p);
                ui.close();
            }
        };
        item(ui, "Copy", "Ctrl+Shift+C", has_selection, MenuPick::Copy);
        item(ui, "Paste", "Ctrl+V", alive, MenuPick::Paste);
        item(ui, "Select all", "", true, MenuPick::SelectAll);
        item(ui, "Copy all output", "", true, MenuPick::CopyAll);
        ui.separator();
        item(ui, "Clear", "", true, MenuPick::Clear);
        item(ui, "Restart shell", "", true, MenuPick::Restart);
        item(ui, "Close terminal", "", true, MenuPick::Close);
    });
    pick
}

#[allow(clippy::too_many_arguments)]
fn draw(
    ui: &Ui,
    st: &mut State,
    view: &View,
    rect: Rect,
    inner: Rect,
    font: &FontId,
    cw: f32,
    ch: f32,
    focused: bool,
    alive: bool,
    prefs: &super::Prefs,
    dark: bool,
    p: &Palette,
) {
    let ctx = ui.ctx().clone();
    let painter = ui.painter_at(rect);
    let ansi = if dark { &ANSI_DARK } else { &ANSI_LIGHT };
    let (dfg, dbg) = (rgb(p.text), rgb(p.canvas));
    let bold_fg = rgb(p.text_strong);
    let selection = st.tabs[st.active].selection;
    let sel_fill = ui.visuals().selection.bg_fill;
    let bold_coords = egui::epaint::text::VariationCoords::new([(b"wght", 700.0)]);
    for (vr, line) in view.lines.iter().enumerate() {
        let y = inner.top() + vr as f32 * ch;
        if y > rect.bottom() {
            break;
        }
        // Backgrounds, then the selection, then the text.
        for run in &line.runs {
            let (_, bg) = resolve(&run.style, ansi, dfg, dbg);
            if bg != dbg {
                painter.rect_filled(
                    Rect::from_min_size(
                        pos2(inner.left() + run.col as f32 * cw, y),
                        vec2(run.cells as f32 * cw, ch),
                    ),
                    0,
                    c32(bg),
                );
            }
        }
        if let Some(sel) = selection
            && let Some((c0, c1)) = sel.cols_in_row(view.top_row + vr, view.cols)
        {
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(inner.left() + c0 as f32 * cw, y),
                    pos2(inner.left() + (c1 + 1) as f32 * cw, y + ch),
                ),
                0,
                sel_fill,
            );
        }
        if line.runs.is_empty() {
            continue;
        }
        let mut job = LayoutJob::default();
        job.wrap.max_width = f32::INFINITY;
        let mut x_acc = 0.0f32;
        for run in &line.runs {
            let (mut fg, _) = resolve(&run.style, ansi, dfg, dbg);
            if run.style.bold && fg == dfg && !run.style.inverse {
                fg = bold_fg;
            }
            let color = c32(fg);
            let format = TextFormat {
                font_id: font.clone(),
                color,
                italics: run.style.italic,
                underline: if run.style.underline {
                    Stroke::new(1.0, color)
                } else {
                    Stroke::NONE
                },
                coords: if run.style.bold {
                    bold_coords.clone()
                } else {
                    Default::default()
                },
                line_height: Some(ch),
                valign: Align::TOP,
                ..Default::default()
            };
            // One section per stretch of characters that are one cell
            // wide in the font; others (wide, fallback glyphs) are placed
            // by their column.
            let simple = !run.wide && run.text.chars().count() == run.cells as usize;
            if !simple {
                let w: f32 = run
                    .text
                    .chars()
                    .take(1)
                    .map(|c| glyph_w(st, &ctx, font, c, cw))
                    .sum();
                job.append(&run.text, run.col as f32 * cw - x_acc, format);
                x_acc = run.col as f32 * cw + w;
                continue;
            }
            let mut seg = String::new();
            let mut seg_col = run.col;
            let mut seg_w = 0.0f32;
            for (i, c) in run.text.chars().enumerate() {
                let col = run.col + i as u16;
                let w = glyph_w(st, &ctx, font, c, cw);
                let fits = (w - cw).abs() < 0.05;
                if fits {
                    if seg.is_empty() {
                        seg_col = col;
                    }
                    seg.push(c);
                    seg_w += w;
                    continue;
                }
                if !seg.is_empty() {
                    job.append(&seg, seg_col as f32 * cw - x_acc, format.clone());
                    x_acc = seg_col as f32 * cw + seg_w;
                    seg.clear();
                    seg_w = 0.0;
                }
                job.append(&c.to_string(), col as f32 * cw - x_acc, format.clone());
                x_acc = col as f32 * cw + w;
            }
            if !seg.is_empty() {
                job.append(&seg, seg_col as f32 * cw - x_acc, format);
                x_acc = seg_col as f32 * cw + seg_w;
            }
        }
        let galley = ctx.fonts_mut(|f| f.layout_job(job));
        painter.galley(pos2(inner.left(), y), galley, c32(dfg));
    }

    // The cursor.
    let Some((cr, cc)) = view.cursor else {
        return;
    };
    if view.cursor_hidden || !alive || view.offset > 0 {
        return;
    }
    if prefs.blink && focused {
        let t = ui.input(|i| i.time);
        let phase = (t * 1000.0 / 530.0) as u64;
        ctx.request_repaint_after(Duration::from_millis(530 - ((t * 1000.0) as u64 % 530)));
        if phase % 2 == 1 {
            return;
        }
    }
    let line = view.lines.get(cr as usize);
    let under = line.and_then(|l| {
        l.runs
            .iter()
            .find(|r| r.col <= cc && cc < r.col + r.cells)
            .map(|r| {
                let ch = if r.wide || r.cells == 1 {
                    r.text.clone()
                } else {
                    r.text
                        .chars()
                        .nth((cc - r.col) as usize)
                        .map(String::from)
                        .unwrap_or_default()
                };
                (ch, r.wide)
            })
    });
    let width = if under.as_ref().is_some_and(|(_, w)| *w) {
        2.0
    } else {
        1.0
    };
    let cell = Rect::from_min_size(
        pos2(inner.left() + cc as f32 * cw, inner.top() + cr as f32 * ch),
        vec2(cw * width, ch),
    );
    let color = if focused { p.accent } else { p.text_3 };
    match prefs.cursor {
        CursorStyle::Block if focused => {
            painter.rect_filled(cell, CornerRadius::same(1), color);
            if let Some((text, _)) = under
                && !text.trim().is_empty()
            {
                painter.text(
                    cell.left_top(),
                    Align2::LEFT_TOP,
                    text,
                    font.clone(),
                    c32(dbg),
                );
            }
        }
        CursorStyle::Block => {
            painter.rect_stroke(
                cell.shrink(0.5),
                CornerRadius::same(1),
                Stroke::new(1.0, color),
                StrokeKind::Inside,
            );
        }
        CursorStyle::Underline => {
            painter.rect_filled(
                Rect::from_min_max(pos2(cell.left(), cell.bottom() - 2.0), cell.max),
                0,
                color,
            );
        }
        CursorStyle::Bar => {
            painter.rect_filled(
                Rect::from_min_max(cell.min, pos2(cell.left() + 2.0, cell.bottom())),
                0,
                color,
            );
        }
    }
}

fn scrollbar(ui: &mut Ui, st: &mut State, view: &View, rect: Rect, inner: Rect) {
    // Full-screen programs have no history (the wheel moves them instead).
    if view.scrollback == 0 || view.alt_screen {
        return;
    }
    let p = *pal(ui);
    let track = Rect::from_min_max(
        pos2(rect.right() - SCROLLBAR_W, inner.top()),
        pos2(rect.right() - 2.0, inner.bottom()),
    );
    let total = (view.scrollback + view.rows as usize) as f32;
    let h = track.height();
    let thumb_h = (h * view.rows as f32 / total).max(24.0).min(h);
    // 0 at the oldest line, 1 on the live screen.
    let pos_frac = (view.scrollback - view.offset) as f32 / view.scrollback as f32;
    let thumb = Rect::from_min_size(
        pos2(track.left(), track.top() + pos_frac * (h - thumb_h)),
        vec2(track.width(), thumb_h),
    );
    let resp = ui.interact(track, grid_id().with("scrollbar"), Sense::click_and_drag());
    let hot = resp.hovered() || resp.dragged();
    ui.painter().rect_filled(
        thumb.shrink2(vec2(if hot { 0.0 } else { 1.5 }, 0.0)),
        CornerRadius::same(4),
        if hot { p.text_3 } else { p.border_strong },
    );
    if (resp.dragged() || resp.clicked())
        && let Some(pos) = resp.interact_pointer_pos()
    {
        // Pointer position → first line shown.
        let frac = ((pos.y - track.top() - thumb_h / 2.0) / (h - thumb_h).max(1.0)).clamp(0.0, 1.0);
        let top = (frac * view.scrollback as f32).round() as usize;
        let offset = view.scrollback.saturating_sub(top);
        let tab = &mut st.tabs[st.active];
        tab.session.term().screen.set_offset(offset);
        tab.session.touch();
    }
    resp.on_hover_text_at_pointer(format!(
        "{} history lines, {} shown above the screen",
        format::count(view.scrollback),
        format::count(view.offset)
    ));
}

fn jump_to_bottom(ui: &mut Ui, st: &mut State, view: &View, inner: Rect, p: &Palette) {
    let label = format!("{} lines below", format::count(view.offset));
    let w = ui
        .painter()
        .layout_no_wrap(
            label.clone(),
            FontId::new(size::SMALL, theme::medium()),
            p.text,
        )
        .size()
        .x
        + 44.0;
    let r = Rect::from_min_size(
        pos2(inner.right() - w - 8.0, inner.bottom() - 34.0),
        vec2(w, 28.0),
    );
    ui.painter().rect(
        r,
        CornerRadius::same(radius::M),
        p.raised,
        Stroke::new(1.0, p.border_strong),
        StrokeKind::Inside,
    );
    let b = kit::overlay(ui, r, IconButton::new(Icon::ChevronDown, &label).compact());
    if b.on_hover_text("Back to the live screen (Shift+End)")
        .clicked()
    {
        let tab = &mut st.tabs[st.active];
        tab.scroll_to_bottom();
        st.focus_next = true;
    }
}
