//! The palette overlay: a card near the top of the window over a dimmed
//! backdrop (a click on it closes), the input, prefix chips while it is
//! empty, a status line (loading, nothing to search, errors), the
//! virtualised result rows and the key hints.

use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Key, Layout, Margin, Modifiers, Rect,
    RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};
use gitvis_core::devops::palette::{Kind, Scope, Target, frecency, scope};

use super::results::{self, Entry, Mode, Row};
use super::verbs::{self, Verb};
use super::{Act, Cx, Drill, State, UseRecord};
use crate::devops::global::GlobalView;
use crate::state::{Action, ToastKind};
use crate::theme::{Palette, Tone, medium, pal, palette, radius, semibold, size, space};
use crate::views::icons::{self, Icon, IconButton};
use crate::views::kit;

/// Card width (narrower windows get the window width less a margin).
pub const WIDTH: f32 = 640.0;
/// Height of a result row.
pub const ROW_H: f32 = 36.0;
/// Rows visible before the list scrolls.
pub const VISIBLE: usize = 12;
const INPUT_H: f32 = 52.0;
const FOOTER_H: f32 = 34.0;
const INPUT_ID: &str = "palette_input";

/// Keys the palette takes before the input sees them.
#[derive(Default)]
struct Keys {
    up: bool,
    down: bool,
    page_up: bool,
    page_down: bool,
    enter: bool,
    actions: bool,
    tab: bool,
    esc: bool,
    back: bool,
}

impl Keys {
    fn read(ctx: &egui::Context, st: &State) -> Self {
        let drill_empty = st.drill.is_some() && st.query.is_empty();
        ctx.input_mut(|i| Keys {
            up: i.consume_key(Modifiers::NONE, Key::ArrowUp),
            down: i.consume_key(Modifiers::NONE, Key::ArrowDown),
            page_up: i.consume_key(Modifiers::NONE, Key::PageUp),
            page_down: i.consume_key(Modifiers::NONE, Key::PageDown),
            actions: i.consume_key(Modifiers::COMMAND, Key::Enter),
            enter: i.consume_key(Modifiers::NONE, Key::Enter),
            tab: i.consume_key(Modifiers::NONE, Key::Tab),
            esc: i.consume_key(Modifiers::NONE, Key::Escape),
            back: drill_empty && i.consume_key(Modifiers::NONE, Key::Backspace),
        })
    }
}

pub fn show(ctx: &egui::Context, st: &mut State, v: &GlobalView, actions: &mut Vec<Action>) {
    let cx = Cx::of_view(v);
    request_files(st, &cx, actions);
    let keys = Keys::read(ctx, st);
    let style = ctx.global_style();
    let dark = style.visuals.dark_mode;
    let p = palette(dark);
    let screen = ctx.content_rect();
    let width = WIDTH.min(screen.width() - 2.0 * space::L);
    let top = (screen.height() * 0.11).clamp(40.0, 120.0);
    let id = egui::Id::unique("command_palette");
    let area = egui::Modal::default_area(id).anchor(Align2::CENTER_TOP, vec2(0.0, top));
    let frame = Frame::NONE
        .fill(p.overlay)
        .stroke(Stroke::new(1.0, p.border_strong))
        .corner_radius(CornerRadius::same(radius::L))
        .inner_margin(Margin::symmetric(6, 0))
        .shadow(style.visuals.window_shadow);
    let modal = egui::Modal::new(id)
        .area(area)
        .frame(frame)
        .backdrop_color(Color32::from_black_alpha(if dark { 120 } else { 70 }))
        .show(ctx, |ui| {
            ui.set_width(width - 12.0);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            body(ui, st, &cx, keys, actions);
        });
    if modal.backdrop_response.clicked() {
        st.open = false;
    }
}

/// `git ls-files` once per repository, and again when HEAD moved.
fn request_files(st: &State, cx: &Cx, actions: &mut Vec<Action>) {
    let (Some(r), Some(snap)) = (cx.repo(), cx.snap()) else {
        return;
    };
    let head = snap.head.target().map(|o| o.to_string());
    let want = match st.files.get(&r.id) {
        None => true,
        Some(f) if f.loading => false,
        // An error is retried from the status line, not every frame.
        Some(f) => f.head != head && !matches!(f.paths, Some(Err(_))),
    };
    if want {
        actions.push(Act::LoadFiles { repo: r.id, head }.into());
    }
}

fn body(ui: &mut Ui, st: &mut State, cx: &Cx, keys: Keys, actions: &mut Vec<Action>) {
    input_row(ui, st);
    results::refresh(st, cx);
    handle(st, cx, &keys, actions);
    if !st.open {
        return;
    }
    // Tab, a prefix or going back may have changed the input.
    results::refresh(st, cx);
    if let Some(s) = st.restore_selected.take() {
        st.selected = st.results.selectable(s);
        st.scroll_to_selected = true;
    }
    kit::hairline(ui);
    if st.results.mode == Mode::Start && st.drill.is_none() {
        prefix_chips(ui, st);
    }
    status_line(ui, st, cx, actions);
    if st.results.mode == Mode::NoResults {
        no_results(ui, st);
    }
    list(ui, st, cx, actions);
    footer(ui, st, cx);
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

fn input_row(ui: &mut Ui, st: &mut State) {
    let p = *pal(ui);
    let w = ui.available_width();
    let (scope, _) = scope::parse(&st.query);
    let id = ui.make_persistent_id(INPUT_ID);
    ui.allocate_ui_with_layout(
        vec2(w, INPUT_H),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_size(vec2(w, INPUT_H));
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.add_space(10.0);
            icons::show(ui, Icon::Search, 16.0, p.text_3);
            if let Some(d) = &st.drill {
                drill_chip(ui, d);
            }
            let badge = (scope != Scope::All && st.drill.is_none()).then(|| scope.label());
            let reserve = badge
                .map(|b| kit::badge_width(ui.painter(), b) + 24.0)
                .unwrap_or(14.0);
            let hint = if st.drill.is_some() {
                "Filter the actions".to_owned()
            } else {
                "Search everything, or start with  >  @  #  /  :  !  ~  ?".to_owned()
            };
            let resp = ui.add(
                egui::TextEdit::singleline(&mut st.query)
                    .id(id)
                    .frame(Frame::NONE)
                    .margin(Margin::ZERO)
                    .align(Align2::LEFT_CENTER)
                    .min_size(vec2(0.0, 26.0))
                    .font(FontId::proportional(size::TITLE))
                    .text_color(p.text_strong)
                    .hint_text(RichText::new(hint).size(size::BODY).color(p.text_3))
                    .desired_width((ui.available_width() - reserve).max(80.0)),
            );
            if st.focus || !resp.has_focus() {
                resp.request_focus();
                st.focus = false;
            }
            if st.caret_end {
                let mut state = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
                let end = CCursor::new(st.query.chars().count());
                state.cursor.set_char_range(Some(CCursorRange::one(end)));
                state.store(ui.ctx(), id);
                st.caret_end = false;
            }
            if let Some(b) = badge {
                kit::badge(ui, b, Tone::Accent);
            }
        },
    );
}

/// The item whose actions are listed, before the input.
fn drill_chip(ui: &mut Ui, d: &Drill) {
    let p = *pal(ui);
    let font = FontId::new(size::SMALL, medium());
    let text = crate::views::truncated(ui.painter(), &d.item.title, font, p.text_strong, 220.0);
    let (icon, color) = kind_icon(&p, &d.item);
    let w = text.size().x + 50.0;
    let (rect, _) = ui.allocate_exact_size(vec2(w, 26.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(radius::M),
        p.selection,
        Stroke::new(1.0, p.accent.gamma_multiply(0.5)),
        StrokeKind::Inside,
    );
    icons::paint(
        ui.painter(),
        icon,
        pos2(rect.left() + 14.0, rect.center().y),
        13.0,
        color,
    );
    let ty = rect.center().y - text.size().y / 2.0;
    let tw = text.size().x;
    ui.painter().galley(pos2(rect.left() + 26.0, ty), text, p.text_strong);
    ui.painter().text(
        pos2(rect.left() + 32.0 + tw, rect.center().y),
        Align2::LEFT_CENTER,
        "›",
        FontId::proportional(size::BODY),
        p.text_2,
    );
}

/// Chips for the prefixes while the input is empty.
fn prefix_chips(ui: &mut Ui, st: &mut State) {
    ui.add_space(space::S + 2.0);
    let mut picked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        ui.add_space(8.0);
        for pr in &scope::PREFIXES {
            let label = format!("{}  {}", pr.ch, chip_label(pr.scope));
            if kit::chip(ui, &label, false).clicked() {
                picked = Some(pr.ch);
            }
        }
    });
    ui.add_space(space::XS);
    if let Some(c) = picked {
        st.query = c.to_string();
        st.caret_end = true;
        st.focus = true;
    }
}

fn chip_label(s: Scope) -> &'static str {
    match s {
        Scope::Commands => "Commands",
        Scope::Refs => "Branches",
        Scope::Prs => "Pull requests",
        Scope::Files => "Files",
        Scope::Commits => "Commits",
        Scope::Runs => "Runs",
        Scope::Repos => "Repositories",
        Scope::Help => "Help",
        Scope::All => "Everything",
    }
}

// ---------------------------------------------------------------------------
// Keys and running
// ---------------------------------------------------------------------------

fn handle(st: &mut State, cx: &Cx, keys: &Keys, actions: &mut Vec<Action>) {
    if keys.esc {
        if st.drill.is_some() {
            back(st);
        } else {
            st.open = false;
        }
        return;
    }
    if keys.back {
        back(st);
        return;
    }
    let page = (VISIBLE - 1) as i64;
    for (pressed, delta) in [
        (keys.up, -1),
        (keys.down, 1),
        (keys.page_up, -page),
        (keys.page_down, page),
    ] {
        if pressed {
            st.selected = st.results.step(st.selected, delta);
            st.scroll_to_selected = true;
        }
    }
    if keys.tab {
        tab(st);
    }
    if keys.actions || std::mem::take(&mut st.actions_requested) {
        if let Some(e) = st.results.entry(st.selected).cloned() {
            drill(st, cx, &e, actions);
        }
    } else if keys.enter || std::mem::take(&mut st.run_requested) {
        if let Some(e) = st.results.entry(st.selected).cloned() {
            run_default(st, cx, &e, actions);
        }
    }
}

/// Leave an item's actions for the results it came from.
fn back(st: &mut State) {
    if let Some(d) = st.drill.take() {
        st.query = d.query;
        st.restore_selected = Some(d.selected);
        st.caret_end = true;
    }
}

/// Tab: on a prefix row, use the prefix; on any row of an unprefixed
/// search, narrow the search to that row's kind; in a narrowed search,
/// complete the input with the row's title.
fn tab(st: &mut State) {
    if st.drill.is_some() {
        return;
    }
    let (scope_now, text) = scope::parse(&st.query);
    let text = text.to_owned();
    let Some(e) = st.results.entry(st.selected) else {
        return;
    };
    let next = match (&e.item.target, scope_now) {
        (Target::Prefix(s), _) => {
            let keep = if st.results.mode == Mode::NoResults {
                text.as_str()
            } else {
                ""
            };
            scope::compose(*s, keep)
        }
        (_, Scope::All) => scope::compose(e.item.kind().scope(), &text),
        (_, s) => scope::compose(s, &e.item.title),
    };
    st.query = next;
    st.caret_end = true;
}

fn run_default(st: &mut State, cx: &Cx, e: &Entry, actions: &mut Vec<Action>) {
    let verb = e
        .verb
        .or_else(|| verbs::default_verb(&e.item, cx, &st.cache.catalog));
    match verb {
        Some(v) => run(st, cx, e, v, actions),
        None => actions.push(Action::Toast(
            ToastKind::Info,
            format!("{} is not available here", e.item.title),
        )),
    }
}

/// List the actions of `e` (with only one action and nothing to forget,
/// that action runs).
fn drill(st: &mut State, cx: &Cx, e: &Entry, actions: &mut Vec<Action>) {
    if st.drill.is_some() {
        // Already listing actions: Ctrl+Enter runs the highlighted one.
        run_default(st, cx, e, actions);
        return;
    }
    if let Target::Prefix(_) = e.item.target {
        run_default(st, cx, e, actions);
        return;
    }
    let n = verbs::verbs(&e.item, cx, &st.cache.catalog).len();
    if n <= 1 && !e.recent {
        run_default(st, cx, e, actions);
        return;
    }
    st.drill = Some(Drill {
        item: e.item.clone(),
        recent: e.recent,
        query: std::mem::take(&mut st.query),
        selected: st.selected,
    });
    st.caret_end = true;
}

fn run(st: &mut State, cx: &Cx, e: &Entry, verb: Verb, actions: &mut Vec<Action>) {
    match (verb, &e.item.target) {
        (Verb::UsePrefix, Target::Prefix(s)) => {
            let (_, text) = scope::parse(&st.query);
            let keep = if st.results.mode == Mode::NoResults {
                text.to_owned()
            } else {
                String::new()
            };
            st.query = scope::compose(*s, &keep);
            st.drill = None;
            st.caret_end = true;
            return;
        }
        (Verb::Forget, _) => {
            let repo = if e.item.target.per_repo() {
                cx.repo_key()
            } else {
                String::new()
            };
            actions.push(
                Act::Forget {
                    key: e.item.key.clone(),
                    repo,
                }
                .into(),
            );
            back(st);
            return;
        }
        _ => {}
    }
    let catalog = &st.cache.catalog;
    let acts = verbs::actions(verb, &e.item, cx, catalog);
    let runs = !acts.iter().all(|a| matches!(a, Action::Toast(..)));
    let keep_open = matches!(&e.item.target, Target::Command(id)
        if catalog.get(id).is_some_and(|c| c.keep_open));
    if runs {
        let default = verbs::default_verb(&e.item, cx, catalog);
        let key = if Some(verb) == default {
            e.item.key.clone()
        } else {
            frecency::with_verb(&e.item.key, verb.id())
        };
        let repo = if e.item.target.per_repo() {
            cx.repo_key()
        } else {
            String::new()
        };
        actions.extend(acts);
        actions.push(
            Act::Used(UseRecord {
                key,
                repo,
                title: e.item.title.clone(),
                subtitle: e.item.subtitle.clone(),
            })
            .into(),
        );
    } else {
        actions.extend(acts);
    }
    if !keep_open {
        st.open = false;
    }
}

// ---------------------------------------------------------------------------
// Status, empty and no-results states
// ---------------------------------------------------------------------------

enum Status {
    Loading(String),
    Note(String),
    Error(String, Action),
}

fn status(st: &State, cx: &Cx) -> Option<Status> {
    if st.drill.is_some() {
        return None;
    }
    let (scope, text) = scope::parse(&st.query);
    let Some(r) = cx.repo() else {
        return match scope {
            Scope::Commands | Scope::Repos | Scope::Help => None,
            Scope::All if text.is_empty() => None,
            _ => Some(Status::Note(
                "No repository is open: only commands and recent repositories can be searched."
                    .into(),
            )),
        };
    };
    if r.snapshot.is_none() && matches!(scope, Scope::Refs | Scope::Commits | Scope::Files) {
        return Some(Status::Loading(format!("Loading {}…", r.name)));
    }
    let github = r.github_remote.is_some();
    let gh_state = |what: &str| -> Option<Status> {
        if !github {
            return Some(Status::Note(format!(
                "{} has no GitHub remote, so there are no {what} to search.",
                r.name
            )));
        }
        if r.github_loading {
            return Some(Status::Loading(format!("Loading {what} from GitHub…")));
        }
        if let Some(e) = &r.github_error {
            return Some(Status::Error(
                format!("Could not load {what} from GitHub: {e}"),
                Action::RefreshGitHub,
            ));
        }
        None
    };
    match scope {
        Scope::Files | Scope::All if scope == Scope::Files || !text.is_empty() => {
            match st.files.get(&r.id) {
                Some(f) if f.loading => Some(Status::Loading(
                    "Listing the files of HEAD (git ls-files)…".into(),
                )),
                Some(f) => match &f.paths {
                    Some(Err(e)) => Some(Status::Error(
                        format!("Could not list the files: {e}"),
                        Act::LoadFiles {
                            repo: r.id,
                            head: r
                                .snapshot
                                .as_ref()
                                .and_then(|s| s.head.target())
                                .map(|o| o.to_string()),
                        }
                        .into(),
                    )),
                    Some(Ok(p)) if p.is_empty() && scope == Scope::Files => Some(Status::Note(
                        "Nothing is tracked yet: git ls-files lists no files.".into(),
                    )),
                    _ => None,
                },
                None => Some(Status::Loading("Listing the files of HEAD…".into())),
            }
        }
        Scope::Prs => gh_state("pull requests").or_else(|| {
            let issues = r.devops.review.issues.lists.values().any(|f| f.data.is_some());
            (!issues).then(|| {
                Status::Note(
                    "Issues are listed here once the Review page has shown them.".into(),
                )
            })
        }),
        Scope::Runs => gh_state("workflow runs").or_else(|| {
            let none = r.github.as_ref().is_none_or(|g| g.runs.is_empty());
            none.then(|| Status::Note("No workflow runs were loaded for this repository.".into()))
        }),
        Scope::Commits => r.snapshot.as_ref().and_then(|s| {
            s.truncated.then(|| {
                Status::Note(format!(
                    "Searching the newest {} commits the graph loaded.",
                    crate::format::count(s.commits.len())
                ))
            })
        }),
        _ => None,
    }
}

fn status_line(ui: &mut Ui, st: &State, cx: &Cx, actions: &mut Vec<Action>) {
    let Some(s) = status(st, cx) else {
        return;
    };
    let p = *pal(ui);
    let w = ui.available_width();
    ui.add_space(space::XS);
    ui.allocate_ui_with_layout(vec2(w, 30.0), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_size(vec2(w, 30.0));
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.add_space(14.0);
        let (text, retry) = match s {
            Status::Loading(t) => {
                ui.add(egui::Spinner::new().size(13.0).color(p.accent));
                (t, None)
            }
            Status::Note(t) => {
                icons::show(ui, Icon::Info, 14.0, p.text_2);
                (t, None)
            }
            Status::Error(t, a) => {
                icons::show(ui, Icon::Error, 14.0, p.danger);
                (t, Some(a))
            }
        };
        let room = ui.available_width() - if retry.is_some() { 90.0 } else { 8.0 };
        let g = crate::views::truncated(
            ui.painter(),
            &text,
            FontId::proportional(size::SMALL),
            p.text_2,
            room.max(60.0),
        );
        let (r, resp) = ui.allocate_exact_size(vec2(g.size().x, 20.0), Sense::hover());
        let elided = g.elided;
        ui.painter()
            .galley(pos2(r.left(), r.center().y - g.size().y / 2.0), g, p.text_2);
        if elided {
            resp.on_hover_text_at_pointer(&text);
        }
        if let Some(a) = retry
            && IconButton::new(Icon::Rerun, "Retry")
                .compact()
                .show(ui)
                .clicked()
        {
            actions.push(a);
        }
    });
}

fn no_results(ui: &mut Ui, st: &State) {
    let p = *pal(ui);
    let (scope, text) = scope::parse(&st.query);
    ui.add_space(space::M);
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            let what = match scope {
                Scope::All => "Nothing".to_owned(),
                s => format!("No {}", chip_label(s).to_lowercase()),
            };
            ui.label(
                RichText::new(format!("{what} matches \u{201c}{text}\u{201d}"))
                    .size(size::BODY)
                    .family(semibold())
                    .color(p.text_strong),
            );
            ui.label(
                RichText::new(
                    "Letters only need to appear in order (\"fh\" finds \"file history\"). Try fewer of them, or search one kind:",
                )
                .size(size::SMALL)
                .color(p.text_2),
            );
        });
    });
    ui.add_space(space::S);
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

fn list(ui: &mut Ui, st: &mut State, cx: &Cx, actions: &mut Vec<Action>) {
    let n = st.results.rows.len();
    if n == 0 {
        ui.add_space(space::L);
        ui.vertical_centered(|ui| {
            let p = *pal(ui);
            ui.label(
                RichText::new("Type to search commands, branches, files, commits and more")
                    .size(size::SMALL)
                    .color(p.text_2),
            );
        });
        ui.add_space(space::L);
        return;
    }
    let visible = n.min(VISIBLE);
    let view_h = visible as f32 * ROW_H;
    ui.add_space(space::XS);
    let mut area = egui::ScrollArea::vertical()
        .id_salt("palette_rows")
        .auto_shrink([false, true])
        .max_height(view_h);
    if std::mem::take(&mut st.scroll_to_selected) {
        let top = st.selected as f32 * ROW_H;
        let off = st.scroll_offset;
        let new = if top < off {
            Some(top)
        } else if top + ROW_H > off + view_h {
            Some(top + ROW_H - view_h)
        } else if n <= VISIBLE {
            Some(0.0)
        } else {
            None
        };
        if let Some(o) = new {
            area = area.vertical_scroll_offset(o.max(0.0));
        }
    }
    let now = crate::format::now_secs();
    let mut clicked: Option<(usize, bool)> = None;
    let out = area.show_rows(ui, ROW_H, n, |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for i in range {
            let (rect, resp) =
                ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
            if !ui.is_rect_visible(rect) {
                continue;
            }
            let selected = i == st.selected;
            let State {
                results,
                ranker,
                cache,
                ..
            } = &mut *st;
            let mode = results.mode;
            let query = results.query.clone();
            match &mut results.rows[i] {
                Row::Header(t) => header(ui, rect, t),
                Row::Entry(e) => {
                    if e.hl.is_none() {
                        e.hl = Some(results::highlight(ranker.matcher(), mode, &query, e));
                    }
                    let hovered = resp.hovered();
                    kit::row_background(ui, rect, hovered, selected);
                    entry_row(ui, rect, e, mode, selected, &cache.catalog, now);
                    if hovered {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        clicked = Some((i, false));
                    } else if resp.secondary_clicked() {
                        clicked = Some((i, true));
                    }
                }
            }
        }
    });
    st.scroll_offset = out.state.clamped_offset().y;
    if let Some((i, secondary)) = clicked {
        st.selected = i;
        if let Some(e) = st.results.entry(i).cloned() {
            if secondary {
                drill(st, cx, &e, actions);
            } else {
                run_default(st, cx, &e, actions);
            }
        }
    }
    ui.add_space(space::XS);
}

fn header(ui: &Ui, rect: Rect, text: &str) {
    let p = *pal(ui);
    let g = ui
        .painter()
        .layout_no_wrap(text.to_uppercase(), FontId::new(size::CAPTION, medium()), p.text_3);
    ui.painter().galley(
        pos2(rect.left() + 14.0, rect.bottom() - 9.0 - g.size().y),
        g,
        p.text_3,
    );
}

#[allow(clippy::too_many_arguments)]
fn entry_row(
    ui: &Ui,
    rect: Rect,
    e: &Entry,
    mode: Mode,
    selected: bool,
    catalog: &super::commands::Catalog,
    now: i64,
) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let painter = ui.painter();
    let cy = rect.center().y;
    let (icon, icon_color) = match (mode, e.verb) {
        (Mode::Actions, Some(v)) => (verb_icon(v), p.text_2),
        _ => match (&e.item.target, catalog) {
            (Target::Command(id), c) => (
                c.get(id).map(|c| c.icon).unwrap_or(Icon::Play),
                p.text_2,
            ),
            _ => kind_icon(&p, &e.item),
        },
    };
    icons::paint(painter, icon, pos2(rect.left() + 22.0, cy), 15.0, icon_color);
    let left = rect.left() + 40.0;
    // Right side: a shortcut, or a badge.
    let mut right = rect.right() - 12.0;
    let asks = mode == Mode::Actions && e.verb.is_some_and(|v| v.asks_first(&e.item));
    if asks {
        right = kit::paint_badge(painter, &p, dark, right, cy, "Asks first", Tone::Neutral) - 10.0;
    } else if mode != Mode::Actions {
        if e.item.shortcut {
            right = paint_kbd(painter, &p, right, cy, &e.item.badge) - 10.0;
        } else {
            let tone = badge_tone(&e.item);
            right = kit::paint_badge(painter, &p, dark, right, cy, e.item.badge_text(), tone) - 10.0;
        }
    }
    let avail = (right - left).max(40.0);
    // Files show the name first, then the folder.
    let (title, title_offset, mut sub) = match (&e.item.target, &e.title) {
        (Target::File(path), None) => {
            let (dir, name) = gitvis_core::devops::palette::sources::split_path(path);
            let off = if dir.is_empty() {
                0
            } else {
                dir.chars().count() + 1
            };
            (name.to_owned(), off, dir.to_owned())
        }
        _ => (e.shown_title().to_owned(), 0, e.item.subtitle.clone()),
    };
    if mode == Mode::Actions {
        sub.clear();
    }
    if let (Some(t), true) = (e.item.time, mode != Mode::Actions) {
        let when = crate::format::relative(t, now);
        if !when.is_empty() {
            if sub.is_empty() {
                sub = when;
            } else {
                sub = format!("{sub} · {when}");
            }
        }
    }
    let hl = e.hl.as_deref().unwrap_or(&[]);
    let title_max = if sub.is_empty() { avail } else { avail * 0.66 };
    let title_color = if selected { p.text_strong } else { p.text };
    let tg = painter.layout_job(highlighted(
        &title,
        hl,
        title_offset,
        FontId::new(size::BODY, medium()),
        FontId::new(size::BODY, semibold()),
        title_color,
        p.accent,
        title_max,
    ));
    let tw = tg.size().x;
    painter.galley(pos2(left, cy - tg.size().y / 2.0), tg, title_color);
    let sub_room = avail - tw - 12.0;
    if !sub.is_empty() && sub_room > 40.0 {
        // The folder of a file carries the rest of the highlights.
        let dir_hl: Vec<u32> = if title_offset > 0 {
            hl.iter()
                .copied()
                .filter(|&i| (i as usize) < title_offset - 1)
                .collect()
        } else {
            Vec::new()
        };
        let sg = painter.layout_job(highlighted(
            &sub,
            &dir_hl,
            0,
            FontId::proportional(size::SMALL),
            FontId::new(size::SMALL, semibold()),
            p.text_2,
            p.accent,
            sub_room,
        ));
        painter.galley(pos2(left + tw + 12.0, cy - sg.size().y / 2.0 + 0.5), sg, p.text_2);
    }
}

/// A one-line layout of `text` with the characters at `hl` (indices into
/// a longer text that starts `offset` characters earlier) emphasised, cut
/// with `…` at `max_w`.
#[allow(clippy::too_many_arguments)]
fn highlighted(
    text: &str,
    hl: &[u32],
    offset: usize,
    font: FontId,
    strong: FontId,
    color: Color32,
    accent: Color32,
    max_w: f32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = max_w.max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    let plain = TextFormat {
        font_id: font,
        color,
        ..Default::default()
    };
    let hot = TextFormat {
        font_id: strong,
        color: accent,
        ..Default::default()
    };
    let mut run = String::new();
    let mut run_hot = false;
    let mut hi = hl.iter().peekable();
    for (i, c) in text.chars().enumerate() {
        let idx = (i + offset) as u32;
        while hi.peek().is_some_and(|&&h| h < idx) {
            hi.next();
        }
        let is_hot = hi.peek().is_some_and(|&&h| h == idx);
        if is_hot != run_hot && !run.is_empty() {
            job.append(&run, 0.0, if run_hot { hot.clone() } else { plain.clone() });
            run.clear();
        }
        run_hot = is_hot;
        run.push(c);
    }
    if !run.is_empty() {
        job.append(&run, 0.0, if run_hot { hot } else { plain });
    }
    job
}

/// A keyboard shortcut drawn like a key; returns its left edge.
fn paint_kbd(painter: &egui::Painter, p: &Palette, right: f32, cy: f32, text: &str) -> f32 {
    let g = painter.layout_no_wrap(text.to_owned(), FontId::new(size::CAPTION, medium()), p.text_2);
    let w = g.size().x + 12.0;
    let rect = Rect::from_min_size(pos2(right - w, cy - 10.0), vec2(w, 20.0));
    painter.rect(
        rect,
        CornerRadius::same(radius::S),
        p.raised,
        Stroke::new(1.0, p.border_strong),
        StrokeKind::Inside,
    );
    painter.galley(
        pos2(rect.left() + 6.0, rect.center().y - g.size().y / 2.0),
        g,
        p.text_2,
    );
    rect.left()
}

fn badge_tone(item: &gitvis_core::devops::palette::Item) -> Tone {
    match (item.kind(), item.badge.as_str()) {
        (Kind::Run, "Failed" | "Timed out" | "Action required") => Tone::Danger,
        (Kind::Run, "Running" | "Queued") => Tone::Accent,
        (Kind::Run, "Passed") => Tone::Success,
        (Kind::Pr, "Open") => Tone::Success,
        (Kind::Pr, "Merged") => Tone::Purple,
        (Kind::Pr, "Closed") => Tone::Danger,
        (Kind::Issue, "Open issue") => Tone::Success,
        (Kind::Branch, "Current branch") => Tone::Accent,
        (Kind::Repo, "Current tab") => Tone::Accent,
        _ => Tone::Neutral,
    }
}

fn kind_icon(p: &Palette, item: &gitvis_core::devops::palette::Item) -> (Icon, Color32) {
    let tone = |t: Tone| match t {
        Tone::Neutral => p.text_2,
        t => t.color(p),
    };
    match item.kind() {
        Kind::Command => (Icon::Play, p.text_2),
        Kind::Task => (Icon::Terminal, p.text_2),
        Kind::Branch => (Icon::Branch, tone(badge_tone(item))),
        Kind::Remote => (Icon::Cloud, p.text_2),
        Kind::Tag => (Icon::Tag, p.text_2),
        Kind::Pr => (Icon::PullRequest, tone(badge_tone(item))),
        Kind::Issue => (Icon::Issue, tone(badge_tone(item))),
        Kind::File => (Icon::List, p.text_2),
        Kind::Commit => (Icon::Graph, p.text_2),
        Kind::Run => (Icon::Play, tone(badge_tone(item))),
        Kind::Repo => (Icon::Folder, p.text_2),
        Kind::Prefix => (Icon::Search, p.text_2),
    }
}

fn verb_icon(v: Verb) -> Icon {
    match v {
        Verb::Run => Icon::Play,
        Verb::ShowTask => Icon::Terminal,
        Verb::Checkout | Verb::CheckoutCommit | Verb::CheckoutPr => Icon::Branch,
        Verb::Jump => Icon::Graph,
        Verb::Merge => Icon::Merge,
        Verb::Rebase => Icon::Rerun,
        Verb::CopyName | Verb::CopyPath | Verb::CopySha | Verb::CopyLink => Icon::Copy,
        Verb::WhereIs => Icon::Cloud,
        Verb::History => Icon::Clock,
        Verb::OpenFile | Verb::OpenBrowser => Icon::External,
        Verb::ShowInFolder | Verb::Switch | Verb::OpenRepo | Verb::OpenFolder => Icon::Folder,
        Verb::OpenReview => Icon::PullRequest,
        Verb::OpenRun => Icon::Play,
        Verb::UsePrefix => Icon::Search,
        Verb::Forget => Icon::Trash,
    }
}

// ---------------------------------------------------------------------------
// Footer
// ---------------------------------------------------------------------------

fn footer(ui: &mut Ui, st: &State, cx: &Cx) {
    kit::hairline(ui);
    let p = *pal(ui);
    let w = ui.available_width();
    let entry = st.results.entry(st.selected);
    let catalog = &st.cache.catalog;
    let default = entry.and_then(|e| {
        e.verb
            .or_else(|| verbs::default_verb(&e.item, cx, catalog))
    });
    let more = entry.is_some_and(|e| {
        st.drill.is_none()
            && !matches!(e.item.target, Target::Prefix(_))
            && (verbs::verbs(&e.item, cx, catalog).len() > 1 || e.recent)
    });
    ui.allocate_ui_with_layout(vec2(w, FOOTER_H), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_size(vec2(w, FOOTER_H));
        ui.spacing_mut().item_spacing.x = 5.0;
        ui.add_space(10.0);
        key_hint(ui, &p, "↑↓", "Move");
        if let Some(v) = default {
            let label = match (v, &entry.map(|e| &e.item.target)) {
                (Verb::Run, Some(Target::Command(_))) => "Run",
                _ => v.short(),
            };
            key_hint(ui, &p, "Enter", label);
        }
        if more {
            key_hint(ui, &p, "Ctrl+Enter", "All actions");
        }
        if st.drill.is_none() && entry.is_some() {
            let (scope_now, _) = scope::parse(&st.query);
            let tab = match entry.map(|e| &e.item.target) {
                Some(Target::Prefix(_)) => "Use",
                _ if scope_now == Scope::All => "Narrow",
                _ => "Complete",
            };
            key_hint(ui, &p, "Tab", tab);
        }
        key_hint(ui, &p, "Esc", if st.drill.is_some() { "Back" } else { "Close" });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(10.0);
            ui.label(
                RichText::new(counts(st))
                    .size(size::SMALL)
                    .color(p.text_3),
            );
        });
    });
}

fn key_hint(ui: &mut Ui, p: &Palette, key: &str, label: &str) {
    let g = ui
        .painter()
        .layout_no_wrap(key.to_owned(), FontId::new(size::CAPTION, medium()), p.text_2);
    let (rect, _) = ui.allocate_exact_size(vec2(g.size().x + 10.0, 18.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(radius::S),
        p.raised,
        Stroke::new(1.0, p.border),
        StrokeKind::Inside,
    );
    ui.painter().galley(
        pos2(rect.left() + 5.0, rect.center().y - g.size().y / 2.0),
        g,
        p.text_2,
    );
    ui.label(RichText::new(label).size(size::SMALL).color(p.text_2));
    ui.add_space(8.0);
}

/// `1,812 matches · 4.1 ms`, `Recent and suggested`, `5 actions`.
fn counts(st: &State) -> String {
    let r = &st.results;
    let ms = |us: u64| {
        if us >= 10_000 {
            format!("{} ms", us / 1000)
        } else {
            format!("{:.1} ms", us as f64 / 1000.0)
        }
    };
    match r.mode {
        Mode::Start => "Recent and suggested".to_owned(),
        Mode::Actions => crate::format::plural(r.entries(), "action", "actions"),
        Mode::Help => crate::format::plural(r.entries(), "prefix", "prefixes"),
        Mode::NoResults => format!(
            "0 of {} · {}",
            crate::format::count(r.searched),
            ms(r.micros)
        ),
        Mode::Search => {
            let shown = r.entries();
            if r.query.is_empty() {
                if shown < r.matched {
                    format!(
                        "First {} of {}",
                        crate::format::count(shown),
                        crate::format::count(r.matched)
                    )
                } else {
                    crate::format::count(shown)
                }
            } else if shown < r.matched {
                format!(
                    "Best {} of {} matches · {}",
                    crate::format::count(shown),
                    crate::format::count(r.matched),
                    ms(r.micros)
                )
            } else {
                format!(
                    "{} · {}",
                    crate::format::plural(r.matched, "match", "matches"),
                    ms(r.micros)
                )
            }
        }
    }
}
