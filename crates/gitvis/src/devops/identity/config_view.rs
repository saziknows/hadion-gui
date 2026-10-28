//! Settings → Git config: the common settings with explanations, every
//! entry grouped by section (scope, file and line, what overrides what),
//! the files git read, and an inline editor that changes a key in the
//! global config or in the active repository through a confirmation.

use egui::{Align, Align2, CornerRadius, FontId, Label, Layout, RichText, Sense, Ui, pos2, vec2};
use gitvis_core::devops::identity::config::{self, COMMON, Common, Kind, Snapshot};
use gitvis_core::devops::identity::{Scope, git_home, git_line, slash, tilde};

use super::widgets as w;
use super::{Act, ConfigTab, EditForm, Prefs, RawForm, State};
use crate::devops::settings_page::Section;
use crate::format;
use crate::state::{Action, RepoState, Settings};
use crate::theme::{medium, pal, radius, size, space};
use crate::views::form::{self, InputOpts};
use crate::views::icons::{self, Icon, IconButton};
use crate::views::insights::{self, Cell};
use crate::views::kit::{self, Col, Stat, Tone};

pub fn show(
    ui: &mut Ui,
    st: &mut State,
    settings: &mut Settings,
    repo: Option<&RepoState>,
    actions: &mut Vec<Action>,
) {
    let prefs = &mut settings.ext.identity;
    let tab = prefs.tab();
    let State {
        config: loaded,
        edit,
        raw,
        filter,
        busy,
        scroll,
        ..
    } = st;
    let mut new_tab = None;
    if w::header(
        ui,
        "Git config",
        "What git reads, from which file and line, and the settings most people change",
        loaded.loading,
        loaded.at,
        |ui| {
            new_tab = kit::segmented(
                ui,
                &[
                    (ConfigTab::Common, "Common settings"),
                    (ConfigTab::All, "All entries"),
                    (ConfigTab::Files, "Files"),
                ],
                tab,
            );
        },
    ) {
        actions.push(Act::Refresh(Section::GitConfig).into());
    }
    if let Some(t) = new_tab {
        prefs.config_tab = t.name().to_owned();
    }
    let repo_name = repo.map(|r| r.name.clone()).unwrap_or_default();
    let busy = *busy;
    let page_id = format!("identity_config_{}", tab.name());
    w::page(ui, &page_id, scroll, |ui| match &loaded.data {
        None => kit::loading(ui, "Reading git config --list --show-origin --show-scope"),
        Some(Err(e)) => {
            if w::error_card(ui, "Could not read the git config", e, "Try again") {
                actions.push(Act::Refresh(Section::GitConfig).into());
            }
        }
        Some(Ok(snap)) => match tab {
            ConfigTab::Common => common(ui, snap, edit, prefs, &repo_name, busy, actions),
            ConfigTab::All => all(
                ui, snap, edit, raw, filter, prefs, &repo_name, busy, actions,
            ),
            ConfigTab::Files => files(ui, snap, actions),
        },
    });
}

fn common(
    ui: &mut Ui,
    snap: &Snapshot,
    edit: &mut Option<EditForm>,
    prefs: &mut Prefs,
    repo_name: &str,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    kit::callout_with(
        ui,
        Tone::Neutral,
        "How git reads its configuration",
        "Files are read in this order; when a key is set in several of them, the value read last wins.",
        |ui| {
            ui.add_space(2.0);
            let small = |t: &str| RichText::new(t).size(size::SMALL);
            kit::kv(ui, 118.0, |kv| {
                kv.text("1  System", small("every user of this computer"));
                kv.text("2  Global", small("your own file (~/.gitconfig)"));
                kv.text("3  Local", small("this repository's .git/config"));
                kv.text(
                    "4  Worktree, -c",
                    small("worktree config, then command-line values"),
                );
            });
            ui.add_space(2.0);
            ui.add(
                Label::new(
                    small("Each setting below shows the value git uses in this repository and where it comes from. Edit changes it in your global config or in this repository only.")
                        .color(pal(ui).text),
                )
                .wrap(),
            );
        },
    );
    ui.add_space(space::S);
    insights::stats(
        ui,
        &[
            Stat::new("Entries", format::count(snap.entries.len())),
            Stat::new("System", format::count(snap.count(Scope::System))),
            Stat::new("Global", format::count(snap.count(Scope::Global))),
            Stat::new("Local", format::count(snap.count(Scope::Local))),
            Stat::new(
                "Keys in several scopes",
                format::count(snap.multi_scope_keys()),
            ),
        ],
    );
    let mut groups: Vec<&str> = Vec::new();
    for c in COMMON.iter() {
        if !groups.contains(&c.group) {
            groups.push(c.group);
        }
    }
    for g in groups {
        kit::section(ui, g, None);
        for c in COMMON.iter().filter(|c| c.group == g) {
            common_row(ui, c, snap, edit, prefs, repo_name, busy, actions);
        }
    }
}

/// Settings whose current value deserves its warning up front.
fn risky(c: &Common, snap: &Snapshot) -> bool {
    match c.key {
        "core.autocrlf" => snap
            .effective(c.key)
            .is_some_and(|e| e.value_text().eq_ignore_ascii_case("true")),
        "credential.helper" => snap
            .values(c.key)
            .iter()
            .any(|e| e.value_text().trim() == "store"),
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn common_row(
    ui: &mut Ui,
    c: &Common,
    snap: &Snapshot,
    edit: &mut Option<EditForm>,
    prefs: &mut Prefs,
    repo_name: &str,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let eff = snap.effective(c.key);
    let values = snap.values(c.key);
    let key = config::norm_key(c.key);
    let editing = edit
        .as_ref()
        .is_some_and(|f| config::norm_key(&f.key) == key);
    let avail = ui.available_width();
    let right_w = 330.0f32.min(avail * 0.45);
    let mut toggle = false;
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            vec2(avail - right_w - 16.0, 0.0),
            Layout::top_down(Align::Min),
            |ui| {
                ui.set_width(avail - right_w - 16.0);
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    kit::subtitle(ui, c.title);
                    ui.label(kit::mono(c.key).color(p.text_3));
                });
                kit::secondary(ui, c.about);
                if let Some((rec, why)) = c.recommended {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let matches = eff.is_some_and(|e| e.value_text().eq_ignore_ascii_case(rec));
                        if matches {
                            kit::badge(ui, "Recommended value", Tone::Success);
                        } else {
                            kit::badge(ui, &format!("Recommended: {rec}"), Tone::Accent);
                            ui.label(RichText::new(why).size(size::SMALL).color(p.text_2));
                        }
                    });
                }
                if eff.is_none() {
                    kit::secondary(ui, &format!("Not set. git's default: {}", c.default));
                }
                if values.len() > 1 {
                    let multi = config::is_multi(c.key);
                    kit::secondary(
                        ui,
                        &if multi {
                            format!(
                                "A list of {} values; every one counts, in this order:",
                                values.len()
                            )
                        } else {
                            format!(
                                "Set in {} places; git uses the last one read:",
                                values.len()
                            )
                        },
                    );
                    // One line per value: scope, file:line, value.
                    for (i, e) in values.iter().enumerate() {
                        let used = !multi && i + 1 == values.len();
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            w::scope_badge(ui, e.scope);
                            ui.label(
                                RichText::new(w::short_place(e))
                                    .size(size::SMALL)
                                    .color(p.text_2),
                            );
                            ui.label(kit::mono(show_value(e.value_text())).color(if used {
                                p.text_strong
                            } else {
                                p.text
                            }));
                            if used {
                                kit::badge(ui, "Used", Tone::Success);
                            } else if !multi {
                                kit::badge(ui, "Overridden", Tone::Neutral);
                            }
                        });
                    }
                }
            },
        );
        ui.allocate_ui_with_layout(
            vec2(right_w, 26.0),
            Layout::right_to_left(Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                toggle = IconButton::new(Icon::Edit, if editing { "Close" } else { "Edit" })
                    .compact()
                    .selected(editing)
                    .show(ui)
                    .clicked();
                match eff {
                    Some(e) => {
                        w::scope_badge(ui, e.scope);
                        ui.add(
                            Label::new(kit::mono(show_value(e.value_text())).color(p.text_strong))
                                .truncate(),
                        );
                    }
                    None => {
                        ui.label(RichText::new("Not set").size(size::SMALL).color(p.text_3));
                    }
                }
            },
        );
    });
    if toggle {
        *edit = if editing {
            None
        } else {
            let scope = match eff.map(|e| e.scope) {
                Some(s) if s.editable() => s,
                _ => prefs.scope,
            };
            Some(EditForm {
                key: c.key.to_owned(),
                value: current_in(snap, &key, scope)
                    .unwrap_or_else(|| eff.map(|e| e.value_text().to_owned()).unwrap_or_default()),
                scope,
            })
        };
    }
    if let Some(warning) = c.warning
        && (risky(c, snap) || editing)
    {
        ui.add_space(2.0);
        kit::callout(ui, Tone::Warning, "", warning);
    }
    if editing && let Some(form) = edit.as_mut() {
        ui.add_space(4.0);
        editor(
            ui,
            form,
            Some(c.kind),
            snap,
            prefs,
            repo_name,
            busy,
            actions,
        );
    }
    ui.add_space(space::S);
    kit::hairline(ui);
    ui.add_space(space::S);
}

/// `(empty)` for an empty value.
fn show_value(v: &str) -> String {
    if v.is_empty() {
        "(empty)".to_owned()
    } else {
        v.to_owned()
    }
}

/// The value of `key` (canonical) in one scope.
fn current_in(snap: &Snapshot, key: &str, scope: Scope) -> Option<String> {
    snap.entries
        .iter()
        .rev()
        .find(|e| e.scope == scope && e.key == key)
        .map(|e| e.value_text().to_owned())
}

/// The inline editor of one key.
#[allow(clippy::too_many_arguments)]
fn editor(
    ui: &mut Ui,
    f: &mut EditForm,
    kind: Option<Kind>,
    snap: &Snapshot,
    prefs: &mut Prefs,
    repo_name: &str,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let key = config::norm_key(&f.key);
    form::card(ui, Tone::Accent, |ui| {
        ui.label(
            RichText::new(format!("Change {}", f.key))
                .size(size::BODY)
                .family(crate::theme::semibold())
                .color(p.text_strong),
        );
        ui.add_space(4.0);
        w::label(ui, "Where");
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            if form::radio(
                ui,
                f.scope == Scope::Global,
                "Global: every repository of yours",
            ) {
                f.scope = Scope::Global;
                prefs.scope = Scope::Global;
            }
            let local = if repo_name.is_empty() {
                "This repository only".to_owned()
            } else {
                format!("Only this repository ({repo_name})")
            };
            if form::radio(ui, f.scope == Scope::Local, &local) {
                f.scope = Scope::Local;
                prefs.scope = Scope::Local;
            }
        });
        ui.add_space(4.0);
        w::label(ui, "Value");
        match kind {
            Some(Kind::Bool) => {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 16.0;
                    for v in ["true", "false"] {
                        if form::radio(ui, f.value.eq_ignore_ascii_case(v), v) {
                            f.value = v.to_owned();
                        }
                    }
                });
            }
            Some(Kind::Choice(opts)) => {
                for (v, what) in opts {
                    if form::radio(
                        ui,
                        f.value.eq_ignore_ascii_case(v),
                        &format!("{v}  ·  {what}"),
                    ) {
                        f.value = (*v).to_owned();
                    }
                }
            }
            Some(Kind::Text { hint, suggestions }) => {
                form::input(
                    ui,
                    ("identity_cfg_value", key.as_str()),
                    &mut f.value,
                    InputOpts::new(hint).mono(),
                );
                if !suggestions.is_empty() {
                    ui.add_space(2.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                        for &s in suggestions.iter() {
                            if kit::chip(ui, s, f.value == s).clicked() {
                                f.value = s.to_owned();
                            }
                        }
                    });
                }
            }
            None => {
                form::input(
                    ui,
                    ("identity_cfg_value", key.as_str()),
                    &mut f.value,
                    InputOpts::new("value").mono(),
                );
                if config::is_multi(&f.key) {
                    kit::secondary(
                        ui,
                        "This key holds a list: Set adds the value to the list; Unset removes this value only.",
                    );
                }
            }
        }
        let now = current_in(snap, &key, f.scope);
        ui.add_space(4.0);
        kit::secondary(
            ui,
            &match &now {
                Some(v) => format!("Now in the {} config: {}", f.scope.label(), show_value(v)),
                None => format!("Not set in the {} config yet.", f.scope.label()),
            },
        );
        let args = config::set_args(f.scope, &f.key, &f.value);
        match &args {
            Ok(a) => {
                ui.add_space(2.0);
                kit::code_block(ui, &git_line(a), actions);
            }
            Err(e) => form::error(ui, e),
        }
        // An empty value clears a list (credential.helper); elsewhere it is a slip.
        let changed = (now.as_deref() != Some(f.value.as_str()) && !f.value.is_empty())
            || config::is_multi(&f.key);
        form::actions_row(ui, |ui| {
            if form::primary(ui, "Set…", args.is_ok() && changed && !busy).clicked() {
                actions.push(
                    Act::SetConfig {
                        scope: f.scope,
                        key: f.key.clone(),
                        value: f.value.clone(),
                    }
                    .into(),
                );
            }
            if form::button(ui, "Unset…", now.is_some() && !busy).clicked() {
                actions.push(
                    Act::UnsetConfig {
                        scope: f.scope,
                        key: f.key.clone(),
                        value: now.clone(),
                    }
                    .into(),
                );
            }
            if form::button(ui, "Cancel", true).clicked() {
                actions.push(Act::CloseEditor.into());
            }
        });
    });
}

/// A collapsible group header: chevron, `remote "origin"`, count, scopes.
fn group_header(ui: &mut Ui, title: &str, count: usize, scopes: &[Scope], collapsed: bool) -> bool {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(rect, CornerRadius::same(radius::M), p.hover);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let cy = rect.center().y;
    icons::paint(
        painter,
        if collapsed {
            Icon::ChevronDown
        } else {
            Icon::ChevronUp
        },
        pos2(rect.left() + 12.0, cy),
        12.0,
        p.text_2,
    );
    // Badges first (right), then the title cut to the room left of them.
    let mut right = rect.right() - 8.0;
    for s in scopes.iter().rev() {
        right = kit::paint_badge(painter, &p, dark, right, cy, s.title(), w::scope_tone(*s)) - 4.0;
    }
    let count_text = format::count(count);
    let count_w = painter
        .layout_no_wrap(
            count_text.clone(),
            FontId::new(size::SMALL, medium()),
            p.text_3,
        )
        .size()
        .x;
    let g = crate::views::truncated(
        painter,
        title,
        FontId::monospace(size::MONO + 0.5),
        p.text_strong,
        (right - rect.left() - 26.0 - count_w - 20.0).max(60.0),
    );
    let cut = g.elided;
    let tw = g.size().x;
    painter.galley(
        pos2(rect.left() + 26.0, cy - g.size().y / 2.0),
        g,
        p.text_strong,
    );
    painter.text(
        pos2(rect.left() + 34.0 + tw, cy),
        Align2::LEFT_CENTER,
        count_text,
        FontId::new(size::SMALL, medium()),
        p.text_3,
    );
    let resp = if cut {
        resp.on_hover_text_at_pointer(title)
    } else {
        resp
    };
    resp.clicked()
}

/// Column widths of the entries table, the same for every group so the
/// columns line up down the page.
struct Widths {
    key: f32,
    value: f32,
    scope: f32,
    place: f32,
    /// 0 on a narrow page: the note is then drawn at the end of Where.
    note: f32,
    total: f32,
}

const GAP: f32 = 16.0;

impl Widths {
    fn of(total: f32) -> Widths {
        // Below this the five columns do not fit side by side.
        let narrow = total < 720.0;
        let scope = if narrow { 70.0 } else { 92.0 };
        let note = if narrow { 0.0 } else { 150.0 };
        let gaps = if narrow { 3.0 } else { 4.0 } * GAP;
        let rest = (total - scope - note - gaps - 12.0).max(240.0);
        let key = (rest * 0.26).clamp(80.0, 240.0);
        let value = (rest * 0.40).max(90.0);
        let place = (rest - key - value).max(70.0);
        Widths {
            key,
            value,
            scope,
            place,
            note,
            total,
        }
    }

    /// Left edge of each column.
    fn xs(&self, left: f32) -> [f32; 5] {
        let k = left + 8.0;
        let v = k + self.key + GAP;
        let s = v + self.value + GAP;
        let p = s + self.scope + GAP;
        [k, v, s, p, p + self.place + GAP]
    }
}

fn table_head(ui: &mut Ui, w: &Widths) {
    let p = *pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(w.total, 22.0), Sense::hover());
    let xs = w.xs(rect.left());
    let titles: &[&str] = if w.note > 0.0 {
        &["Key", "Value", "Scope", "Where", "Note"]
    } else {
        &["Key", "Value", "Scope", "Where"]
    };
    for (x, t) in xs.iter().zip(titles) {
        ui.painter().text(
            pos2(*x, rect.center().y),
            Align2::LEFT_CENTER,
            t.to_uppercase(),
            FontId::new(size::CAPTION, medium()),
            p.text_3,
        );
    }
}

/// `text` cut from the left (`…/agents/gitconfig:7`) to fit `max` points:
/// the file name and line are what matter in a path.
fn tail_fit(painter: &egui::Painter, text: &str, font: &FontId, max: f32) -> (String, bool) {
    let width = |s: &str| {
        painter
            .layout_no_wrap(s.to_owned(), font.clone(), egui::Color32::WHITE)
            .size()
            .x
    };
    if width(text) <= max {
        return (text.to_owned(), false);
    }
    let chars: Vec<char> = text.chars().collect();
    for start in 1..chars.len() {
        let s: String = std::iter::once('…')
            .chain(chars[start..].iter().copied())
            .collect();
        if width(&s) <= max {
            return (s, true);
        }
    }
    ("…".to_owned(), true)
}

/// One entry of the table; returns true when clicked.
fn entry_row(
    ui: &mut Ui,
    w: &Widths,
    e: &config::Entry,
    place: &str,
    note: Option<(String, Tone)>,
    editing: bool,
) -> bool {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let (rect, resp) = ui.allocate_exact_size(vec2(w.total, 28.0), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp.clicked();
    }
    let painter = ui.painter();
    if editing {
        painter.rect_filled(rect, CornerRadius::same(radius::M), p.selection);
    } else if resp.hovered() {
        painter.rect_filled(rect, CornerRadius::same(radius::M), p.hover);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let xs = w.xs(rect.left());
    let cy = rect.center().y;
    let mono = FontId::monospace(size::MONO);
    let key = crate::views::truncated(painter, e.name(), mono.clone(), p.text, w.key);
    painter.galley(pos2(xs[0], cy - key.size().y / 2.0), key, p.text);
    let empty = e.value_text().is_empty();
    let value = crate::views::truncated(
        painter,
        if empty { "(empty)" } else { e.value_text() },
        mono,
        if empty { p.text_3 } else { p.text_strong },
        w.value,
    );
    let value_cut = value.elided;
    painter.galley(
        pos2(xs[1], cy - value.size().y / 2.0),
        value,
        if empty { p.text_3 } else { p.text_strong },
    );
    let bw = kit::badge_width(painter, e.scope.title());
    kit::paint_badge(
        painter,
        &p,
        dark,
        xs[2] + bw,
        cy,
        e.scope.title(),
        w::scope_tone(e.scope),
    );
    let small = FontId::proportional(size::SMALL);
    // On a narrow page the note sits, shortened, at the end of the Where
    // column (the scope that overrides is in its own row).
    let note = note.map(|(t, tone)| {
        if w.note > 0.0 {
            (t, tone)
        } else if t.starts_with("Overridden") {
            ("Overridden".to_owned(), tone)
        } else if t == "List entry" {
            ("List".to_owned(), tone)
        } else {
            (t, tone)
        }
    });
    let inline_note = (w.note <= 0.0)
        .then_some(note.as_ref())
        .flatten()
        .map(|(t, _)| kit::badge_width(painter, t) + 6.0)
        .unwrap_or(0.0);
    let (place_text, place_cut) =
        tail_fit(painter, place, &small, (w.place - inline_note).max(40.0));
    painter.text(
        pos2(xs[3], cy),
        Align2::LEFT_CENTER,
        place_text,
        small,
        p.text_2,
    );
    if let Some((t, tone)) = &note {
        let right = if w.note > 0.0 {
            xs[4] + kit::badge_width(painter, t)
        } else {
            xs[3] + w.place
        };
        kit::paint_badge(painter, &p, dark, right, cy, t, *tone);
    }
    painter.hline(
        rect.x_range(),
        rect.bottom(),
        egui::Stroke::new(1.0, p.border),
    );
    // Off-screen information only: the whole value or path when cut.
    let resp = if value_cut || place_cut {
        let mut tip = String::new();
        if value_cut {
            tip.push_str(e.value_text());
        }
        if place_cut {
            if !tip.is_empty() {
                tip.push('\n');
            }
            tip.push_str(place);
        }
        resp.on_hover_text_at_pointer(tip)
    } else {
        resp
    };
    resp.clicked()
}

#[allow(clippy::too_many_arguments)]
fn all(
    ui: &mut Ui,
    snap: &Snapshot,
    edit: &mut Option<EditForm>,
    raw: &mut RawForm,
    filter: &mut String,
    prefs: &mut Prefs,
    repo_name: &str,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    let home = git_home();
    ui.allocate_ui_with_layout(
        vec2(ui.available_width().min(420.0), 32.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            kit::search_box(
                ui,
                "identity_cfg_filter",
                filter,
                "Filter keys, values, scopes, files",
            );
        },
    );
    let groups = config::groups(&snap.entries, filter);
    let shown: usize = groups.iter().map(|g| g.items.len()).sum();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(
            RichText::new(format!(
                "{} of {} entries · {} sections",
                format::count(shown),
                format::count(snap.entries.len()),
                format::count(groups.len())
            ))
            .size(size::SMALL)
            .color(pal(ui).text_2),
        );
        if filter.trim().is_empty() {
            if IconButton::text("Collapse all")
                .compact()
                .show(ui)
                .clicked()
            {
                prefs.collapsed = groups.iter().map(|g| g.id.clone()).collect();
            }
            if IconButton::text("Expand all").compact().show(ui).clicked() {
                prefs.collapsed.clear();
            }
        }
    });
    if groups.is_empty() {
        ui.add_space(space::S);
        kit::callout(
            ui,
            Tone::Neutral,
            "No entry matches",
            &format!(
                "Nothing in the {} entries contains \"{}\". Clear the filter to see them all.",
                snap.entries.len(),
                filter.trim()
            ),
        );
    }
    let edit_key = edit.as_ref().map(|f| config::norm_key(&f.key));
    let widths = Widths::of(ui.available_width());
    if !groups.is_empty() {
        ui.add_space(4.0);
        table_head(ui, &widths);
    }
    for g in &groups {
        let filtered = !filter.trim().is_empty();
        let collapsed = !filtered && prefs.collapsed.contains(&g.id);
        let mut scopes: Vec<Scope> = Vec::new();
        for &i in &g.items {
            if !scopes.contains(&snap.entries[i].scope) {
                scopes.push(snap.entries[i].scope);
            }
        }
        ui.add_space(4.0);
        if group_header(ui, &g.title, g.items.len(), &scopes, collapsed) && !filtered {
            if collapsed {
                prefs.collapsed.retain(|c| c != &g.id);
            } else {
                prefs.collapsed.push(g.id.clone());
            }
        }
        if collapsed {
            continue;
        }
        let mut clicked = None;
        for &i in &g.items {
            let e = &snap.entries[i];
            let note = match config::overridden_by(&snap.entries, i) {
                Some(o) => Some((format!("Overridden by {}", o.scope.label()), Tone::Neutral)),
                None if config::is_multi(&e.key) => Some(("List entry".to_owned(), Tone::Neutral)),
                None => None,
            };
            let editing = edit_key.as_deref() == Some(e.key.as_str())
                && edit.as_ref().is_some_and(|f| f.scope == e.scope);
            if entry_row(
                ui,
                &widths,
                e,
                &place(e, snap, home.as_deref()),
                note,
                editing,
            ) {
                clicked = Some(i);
            }
        }
        if let Some(i) = clicked {
            let e = &snap.entries[i];
            *edit = Some(EditForm {
                key: e.key.clone(),
                scope: if e.scope.editable() {
                    e.scope
                } else {
                    prefs.scope
                },
                value: e.value_text().to_owned(),
            });
        }
        if let Some(k) = &edit_key
            && g.items.iter().any(|&i| &snap.entries[i].key == k)
            && let Some(form) = edit.as_mut()
        {
            let kind = COMMON
                .iter()
                .find(|c| &config::norm_key(c.key) == k)
                .map(|c| c.kind);
            ui.add_space(4.0);
            editor(ui, form, kind, snap, prefs, repo_name, busy, actions);
        }
    }

    kit::section(ui, "Add a key", None);
    form::card(ui, Tone::Neutral, |ui| {
        kit::secondary(
            ui,
            "Any key git understands (git help config lists them), e.g. core.editor or url.git@github.com:.insteadOf.",
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            // Two inputs and the gap; each input adds its frame to its width.
            let half = ((ui.available_width() - 8.0) / 2.0).max(160.0) - w::INPUT_FRAME;
            form::input(
                ui,
                "identity_raw_key",
                &mut raw.key,
                InputOpts::new("section.name").mono().width(half),
            );
            form::input(
                ui,
                "identity_raw_value",
                &mut raw.value,
                InputOpts::new("value").mono().width(half),
            );
        });
        ui.add_space(4.0);
        let mut scope = prefs.scope;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            if form::radio(ui, scope == Scope::Global, "Global") {
                scope = Scope::Global;
            }
            let local = if repo_name.is_empty() {
                "This repository".to_owned()
            } else {
                format!("Only this repository ({repo_name})")
            };
            if form::radio(ui, scope == Scope::Local, &local) {
                scope = Scope::Local;
            }
        });
        prefs.scope = scope;
        if !raw.key.trim().is_empty()
            && let Ok(a) = config::set_args(scope, &raw.key, &raw.value)
        {
            kit::code_block(ui, &git_line(&a), actions);
        }
        form::error(ui, &raw.error);
        form::actions_row(ui, |ui| {
            if form::primary(ui, "Set…", !raw.key.trim().is_empty() && !busy).clicked() {
                actions.push(Act::AddRawKey(scope).into());
            }
        });
    });
}

fn files(ui: &mut Ui, snap: &Snapshot, actions: &mut Vec<Action>) {
    let home = git_home();
    if let Ok(v) = std::env::var("GIT_CONFIG_GLOBAL")
        && !v.trim().is_empty()
    {
        kit::callout(
            ui,
            Tone::Accent,
            "GIT_CONFIG_GLOBAL is set",
            &format!(
                "git reads and writes your global config at {} instead of ~/.gitconfig, and so does gitvis.",
                v.trim()
            ),
        );
        ui.add_space(space::S);
    }
    if std::env::var("GIT_CONFIG_NOSYSTEM").is_ok_and(|v| !v.is_empty() && v != "0") {
        kit::callout(
            ui,
            Tone::Neutral,
            "GIT_CONFIG_NOSYSTEM is set",
            "git skips the system config file.",
        );
        ui.add_space(space::S);
    }
    kit::secondary(
        ui,
        "The files git read in this repository, in reading order: a key in a later file wins. Included files (include.path and includeIf) appear where they were read. Click a file to open its folder.",
    );
    let mut rows: Vec<(Scope, std::path::PathBuf, usize, bool)> = snap
        .files
        .iter()
        .map(|f| (f.scope, f.path.clone(), f.entries, true))
        .collect();
    if let Some(g) = &snap.global_file
        && !rows
            .iter()
            .any(|(_, p, _, _)| gitvis_core::devops::identity::same_path(p, g))
    {
        rows.push((Scope::Global, g.clone(), 0, g.exists()));
    }
    let cells: Vec<Vec<Cell>> = rows
        .iter()
        .map(|(s, p, n, exists)| {
            vec![
                Cell::Badges(vec![(s.title().to_owned(), w::scope_tone(*s))]),
                Cell::Mono(tilde(p, home.as_deref())),
                Cell::Sec(if *exists {
                    format::plural(*n, "entry", "entries")
                } else {
                    "not created yet".to_owned()
                }),
            ]
        })
        .collect();
    let mut clicked = None;
    insights::grid(
        ui,
        "identity_cfg_files",
        &[Col::text("Scope"), Col::text("File"), Col::num("Entries")],
        &cells,
        true,
        |r, resp| {
            if resp.clicked() {
                clicked = Some(r);
            }
        },
    );
    if let Some(r) = clicked
        && let Some(dir) = rows[r].1.parent()
    {
        actions.push(Action::OpenPath(dir.to_path_buf()));
    }
    kit::section(ui, "How gitvis read this", None);
    kit::code_block(ui, &git_line(&config::LIST_ARGS), actions);
    kit::secondary(
        ui,
        &format!(
            "{} entries in {} ms, run in {}. Line numbers come from reading each file, since git does not print them.",
            format::count(snap.entries.len()),
            snap.elapsed_ms,
            slash(&snap.dir).trim_end_matches('/')
        ),
    );
}

/// Where an entry is, short: `.git/config:3` for the repository's own
/// files, `~/.gitconfig:7` otherwise.
fn place(e: &config::Entry, snap: &Snapshot, home: Option<&std::path::Path>) -> String {
    if let Some(f) = &e.file {
        let dir = slash(&snap.dir).trim_end_matches('/').to_owned();
        let file = slash(f);
        let (fl, dl) = if cfg!(windows) {
            (file.to_lowercase(), dir.to_lowercase())
        } else {
            (file.clone(), dir.clone())
        };
        if fl.starts_with(&format!("{dl}/")) {
            let rel = &file[dir.len() + 1..];
            return match e.line {
                Some(l) => format!("{rel}:{l}"),
                None => rel.to_owned(),
            };
        }
    }
    e.location(home)
}
