//! The editor frame: header (file, operation, progress, the sides),
//! toolbar, file switcher, and which body to show (panes, a choice card,
//! the whole-file editor, loading, errors, resolved).

use egui::{Align, Frame, Key, Label, Layout, Margin, Modifiers, RichText, Ui, vec2};
use gitvis_core::Pending;
use gitvis_core::devops::conflict::load::Presentation;
use gitvis_core::devops::conflict::resolve::{self, Resolution};

use super::{Act, State, cards, panes};
use crate::format;
use crate::state::{Action, RepoState, Settings};
use crate::theme::{Tone, pal, semibold, size, space};
use crate::views::icons::{self, Icon, IconButton};
use crate::views::kit::{self, Leading, Row};
use crate::views::{form, menus};

pub fn show(ui: &mut Ui, repo: &mut RepoState, _settings: &Settings, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let busy = repo.busy.is_some() || repo.devops.conflict.busy.is_some();
    let pending = repo
        .snapshot
        .as_ref()
        .map(|s| s.pending)
        .unwrap_or(Pending::None);
    let st = &mut repo.devops.conflict;
    keyboard(ui, st, actions);
    egui::Panel::top("conflict_head")
        .resizable(false)
        .frame(Frame::NONE.fill(p.canvas))
        .show(ui, |ui| header(ui, st, busy, actions));
    // The panes keep at least 520 points: a narrow window narrows the file
    // list first (it was squeezing the three panes to a few words each).
    let hi = (ui.available_width() - 520.0).clamp(160.0, 440.0);
    let lo = 160.0f32.min(hi);
    egui::Panel::left("conflict_files")
        .resizable(true)
        .default_size(250.0f32.clamp(lo, hi))
        .size_range(lo..=hi)
        .frame(Frame::NONE.fill(p.panel).inner_margin(Margin {
            left: 6,
            right: 6,
            top: 10,
            bottom: 6,
        }))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            file_list(ui, st, actions);
        });
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas))
        .show(ui, |ui| body(ui, st, pending, busy, actions));
}

/// Alt+↓ / Alt+↑ (next / previous conflict) and Ctrl+S (save). The graph
/// consumes plain arrow presses before this view runs, so a press is also
/// recognised from the key going down.
fn keyboard(ui: &Ui, st: &mut State, actions: &mut Vec<Action>) {
    let (alt, down, up) = ui.input(|i| {
        (
            i.modifiers.alt,
            i.key_down(Key::ArrowDown),
            i.key_down(Key::ArrowUp),
        )
    });
    let (cd, cu, save) = ui.input_mut(|i| {
        (
            i.consume_key(Modifiers::ALT, Key::ArrowDown),
            i.consume_key(Modifiers::ALT, Key::ArrowUp),
            i.consume_key(Modifiers::COMMAND, Key::S),
        )
    });
    let (nd, nu) = (alt && down, alt && up);
    if cd || (nd && !st.keys.0) {
        actions.push(Act::Next.into());
    }
    if cu || (nu && !st.keys.1) {
        actions.push(Act::Prev.into());
    }
    st.keys = (nd, nu);
    if save {
        actions.push(Act::Save.into());
    }
}

fn header(ui: &mut Ui, st: &mut State, busy: bool, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let Some(path) = st.open.clone() else { return };
    let (dir, name) = format::split_path(&path);
    let entry = st.files().iter().find(|e| e.path == path).cloned();
    // Until the first read arrives the file counts as conflicted.
    let known = matches!(st.overview, Some(Ok(_)));
    let conflicted = entry.is_some() || !known;
    let s = st.current();
    let dirty = s.is_some_and(|s| s.dirty);
    let others = st.files().iter().filter(|e| e.path != path).count();
    let op = match &st.overview {
        Some(Ok(o)) => Some(o.op.clone()),
        _ => None,
    };
    let (ours, theirs) = st.sides(s);
    let mut dismiss = false;
    Frame::NONE
        .inner_margin(Margin {
            left: 20,
            right: 16,
            top: 12,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                icons::show(
                    ui,
                    if conflicted { Icon::Merge } else { Icon::Check },
                    18.0,
                    if conflicted { p.warning } else { p.success },
                );
                ui.label(
                    RichText::new(name)
                        .size(size::HEADING)
                        .family(semibold())
                        .color(p.text_strong),
                );
                if !dir.is_empty() {
                    ui.label(RichText::new(dir).size(size::BODY).color(p.text_2));
                }
                if let Some(e) = &entry {
                    kit::badge(ui, e.kind.label(), Tone::Warning);
                } else if known {
                    kit::badge(ui, "Resolved", Tone::Success);
                }
                if let Some(op) = &op
                    && op.kind != gitvis_core::devops::conflict::labels::OpKind::None
                {
                    kit::badge(ui, &op.title(), Tone::Neutral);
                }
                if dirty {
                    kit::badge(ui, "Unsaved changes", Tone::Accent);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if IconButton::new(Icon::Close, "Close")
                        .show(ui)
                        .on_hover_text("Back to the graph")
                        .clicked()
                    {
                        actions.push(Act::Close.into());
                    }
                    if conflicted
                        && IconButton::text("Open the raw diff")
                            .show(ui)
                            .on_hover_text("The combined diff of this file, as git shows it")
                            .clicked()
                    {
                        actions.push(Act::OpenRawDiff.into());
                    }
                });
            });
            // Progress.
            let status = match s {
                _ if !known => "Reading the conflicts…".to_owned(),
                Some(s) if conflicted && s.is_text() && s.hunk_count() == 0 => format!(
                    "No conflict markers left in this file; {} still conflicted",
                    format::plural(others + 1, "file", "files")
                ),
                Some(s) if conflicted && s.is_text() => {
                    let (done, total) = st.progress(s);
                    format!(
                        "{done} of {} resolved in this file; {} still conflicted",
                        format::plural(total, "conflict", "conflicts"),
                        format::plural(others + 1, "file", "files")
                    )
                }
                Some(_) if conflicted => format!(
                    "Choose a version below; {} still conflicted",
                    format::plural(others + 1, "file", "files")
                ),
                _ if conflicted => format!(
                    "{} conflicted",
                    format::plural(others + 1, "file", "files")
                ),
                _ if others == 0 => "No other file is conflicted".to_owned(),
                _ => format!(
                    "{} still conflicted",
                    format::plural(others, "other file", "other files")
                ),
            };
            // The status, then a toggle for the file's details (line
            // endings, marker style, where the base comes from…): they used
            // to be one long dot-separated line under it.
            let text_file = s.filter(|s| s.is_text() && conflicted);
            let details_id = ui.make_persistent_id("conflict_file_details");
            let mut open = ui.data(|d| d.get_temp::<bool>(details_id)).unwrap_or(false);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.label(
                    RichText::new(status)
                        .size(size::BODY)
                        .family(crate::theme::medium())
                        .color(p.text),
                );
                if text_file.is_some()
                    && kit::link(ui, if open { "Hide file details" } else { "File details" })
                        .clicked()
                {
                    open = !open;
                    ui.data_mut(|d| d.insert_temp(details_id, open));
                }
            });
            if let Some(op) = &op {
                ui.add(
                    Label::new(
                        RichText::new(op.summary())
                            .size(size::SMALL)
                            .color(p.text_2),
                    )
                    .wrap(),
                );
            }
            if let Some(s) = text_file {
                if !s.load.utf8 {
                    kit::callout(
                        ui,
                        Tone::Warning,
                        "Not UTF-8 text",
                        "Choose a side for each conflict; editing the text is off for this file.",
                    );
                }
                if open {
                    ui.add_space(2.0);
                    kit::kv(ui, 120.0, |kv| {
                        for (label, value) in meta_rows(s) {
                            kv.text(label, RichText::new(value).size(size::SMALL));
                        }
                    });
                }
            }
            if op.as_ref().is_some_and(|o| o.swapped()) && conflicted {
                ui.add_space(2.0);
                kit::callout(
                    ui,
                    Tone::Accent,
                    "In a rebase, ours and theirs are swapped",
                    &format!(
                        "Ours is {} (the upstream, plus the commits already replayed). Theirs is {}, the commit being applied.",
                        ours.name, theirs.name
                    ),
                );
            }
            // Notes.
            if let Some(s) = st.current()
                && let Some(newer) = &s.external
            {
                let _ = newer;
                ui.add_space(2.0);
                kit::callout_with(
                    ui,
                    Tone::Warning,
                    "The file changed on disk",
                    "Another program changed this file while you had unsaved edits here. Take the version on disk (your edits are discarded) or keep your edits (saving overwrites the disk version).",
                    |ui| {
                        ui.horizontal(|ui| {
                            if form::button(ui, "Take the disk version", !busy).clicked() {
                                actions.push(Act::ReloadFromDisk.into());
                            }
                            if form::button(ui, "Keep my edits", true).clicked() {
                                actions.push(Act::KeepEdits.into());
                            }
                        });
                    },
                );
            }
            if let Some(e) = st.last_error.clone() {
                ui.add_space(2.0);
                kit::callout(ui, Tone::Danger, "The last change failed", &e);
            }
            if let Some(n) = st.notice.clone() {
                ui.add_space(2.0);
                kit::callout_with(ui, Tone::Neutral, "", &n, |ui| {
                    if form::button(ui, "Dismiss", true).clicked() {
                        dismiss = true;
                    }
                });
            }
        });
    if dismiss {
        st.notice = None;
    }
    kit::hairline(ui);
    if let Some(s) = st.current()
        && conflicted
        && s.is_text()
        && s.hunk_count() > 0
        && st.whole.is_none()
    {
        toolbar(ui, st, busy, actions);
    }
}

/// The file's details, one per row: line endings, final newline, marker
/// style and size, where the base comes from, the stage merge, encoding
/// and how long the read took.
fn meta_rows(s: &super::Session) -> Vec<(&'static str, String)> {
    let d = &s.doc;
    let mut rows = vec![(
        "Line endings",
        if d.crlf_lines > 0 && d.lf_lines > 0 {
            format!("mixed: {} CRLF, {} LF", d.crlf_lines, d.lf_lines)
        } else {
            d.eol.label().to_owned()
        },
    )];
    rows.push((
        "Final newline",
        if d.final_newline { "yes" } else { "no" }.to_owned(),
    ));
    rows.push(("Marker style", d.style().label().to_owned()));
    rows.push(("Marker size", format!("{} characters", d.marker_size)));
    let hunks = s.hunks();
    let staged = hunks.iter().filter(|h| h.base_from_stages).count();
    let known = hunks.iter().filter(|h| h.base.is_some()).count();
    let added = s.load.entry.kind == gitvis_core::devops::conflict::ConflictKind::BothAdded;
    let base = if added {
        "no common ancestor (added on both sides)".to_owned()
    } else if staged > 0 {
        format!(
            "from the index stages for {staged} of {}",
            format::plural(hunks.len(), "conflict", "conflicts")
        )
    } else if known < hunks.len() {
        format!(
            "known for {known} of {}",
            format::plural(hunks.len(), "conflict", "conflicts")
        )
    } else {
        "in the markers".to_owned()
    };
    rows.push(("Base", base));
    if let Some(n) = s.load.stage_conflicts {
        rows.push((
            "Stage merge",
            format!(
                "{} in the diff3 merge of the stages",
                format::plural(n, "conflict", "conflicts")
            ),
        ));
    }
    rows.push((
        "Encoding",
        if s.load.utf8 {
            "UTF-8".to_owned()
        } else {
            "not UTF-8: choose sides, editing is off".to_owned()
        },
    ));
    rows.push(("Read in", format!("{} ms", s.load.elapsed_ms)));
    rows
}

fn toolbar(ui: &mut Ui, st: &mut State, busy: bool, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let Some(s) = st.current() else { return };
    let n = s.hunk_count();
    let focus = s.focus.min(n.saturating_sub(1));
    let dirty = s.dirty;
    let result = s.result();
    let (left, stray) = resolve::remaining(&result, s.doc.marker_size);
    let utf8 = s.load.utf8;
    let show_base = st.show_base;
    Frame::NONE
        .fill(p.panel)
        .inner_margin(Margin::symmetric(14, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // Save and Mark resolved first, at the right; the navigation and
            // the bulk choices wrap in the width they leave (they used to
            // run under Save and push Mark resolved off a narrow window).
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let mark = IconButton::new(Icon::Check, "Mark resolved")
                    .primary(true)
                    .enabled(left.is_empty() && !busy)
                    .show(ui);
                let mark = if left.is_empty() {
                    mark.on_hover_text("git add: record this file as resolved")
                } else {
                    mark
                };
                if mark.clicked() {
                    actions.push(Act::MarkResolved.into());
                }
                if IconButton::text("Save")
                    .enabled(dirty && !busy)
                    .show(ui)
                    .on_hover_text("Write the result to disk (Ctrl+S)")
                    .clicked()
                {
                    actions.push(Act::Save.into());
                }
                if busy {
                    ui.add(egui::Spinner::new().size(14.0).color(p.text_2));
                }
                ui.add_space(space::M);
                ui.with_layout(
                    // Top-aligned: a centred wrapping row fills the page height.
                    Layout::left_to_right(Align::Min).with_main_wrap(true),
                    |ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                if IconButton::new(Icon::ChevronUp, "Previous")
                    .compact()
                    .enabled(n > 0)
                    .show(ui)
                    .on_hover_text("Previous conflict (Alt+Up)")
                    .clicked()
                {
                    actions.push(Act::Prev.into());
                }
                if IconButton::new(Icon::ChevronDown, "Next")
                    .compact()
                    .enabled(n > 0)
                    .show(ui)
                    .on_hover_text("Next conflict (Alt+Down)")
                    .clicked()
                {
                    actions.push(Act::Next.into());
                }
                // Centred on the buttons' height (the row is top-aligned).
                let counter = RichText::new(if n == 0 {
                    "No conflicts".to_owned()
                } else {
                    format!("Conflict {} of {n}", focus + 1)
                })
                .size(size::SMALL)
                .color(p.text_2);
                let counter_w = ui
                    .painter()
                    .layout_no_wrap(
                        counter.text().to_owned(),
                        egui::FontId::proportional(size::SMALL),
                        p.text_2,
                    )
                    .size()
                    .x;
                ui.add_sized(vec2(counter_w + 2.0, 26.0), Label::new(counter));
                ui.add_space(space::M);
                if IconButton::text("Take all ours")
                    .compact()
                    .enabled(n > 0)
                    .show(ui)
                    .clicked()
                {
                    actions.push(Act::ResolveAll(Resolution::Ours).into());
                }
                if IconButton::text("Take all theirs")
                    .compact()
                    .enabled(n > 0)
                    .show(ui)
                    .clicked()
                {
                    actions.push(Act::ResolveAll(Resolution::Theirs).into());
                }
                ui.add_space(space::S);
                if kit::chip(ui, "Base pane", show_base)
                    .on_hover_text("Show the common ancestor as a fourth pane")
                    .clicked()
                {
                    actions.push(Act::SetBase(!show_base).into());
                }
                if IconButton::new(Icon::Edit, "Edit as text")
                    .compact()
                    .enabled(utf8)
                    .show(ui)
                    .on_hover_text("Edit the whole result in one text box")
                    .clicked()
                {
                    actions.push(Act::WholeEdit.into());
                }
                let more = IconButton::new(Icon::ChevronDown, "Recreate")
                    .compact()
                    .enabled(!busy)
                    .show(ui)
                    .on_hover_text("Write the conflict markers again from the index");
                egui::Popup::menu(&more)
                    .id(egui::Id::unique("conflict_recreate_menu"))
                    .show(|ui| {
                        menus::begin(ui, 260.0);
                        for (style, label) in [
                            ("merge", "Merge style (ours and theirs)"),
                            ("diff3", "diff3 style (with the base)"),
                            ("zdiff3", "zdiff3 style (base, smaller hunks)"),
                        ] {
                            menus::action(
                                ui,
                                label,
                                actions,
                                Act::Recreate(style.to_owned()).into(),
                            );
                        }
                    });
                    },
                );
            });
            if !left.is_empty() {
                ui.add_space(2.0);
                ui.label(
                    RichText::new(format!(
                        "Mark resolved needs every conflict resolved: {} of the result still {} conflict markers.",
                        resolve::lines_phrase(&left),
                        if left.len() == 1 { "has" } else { "have" }
                    ))
                    .size(size::SMALL)
                    .color(p.text_2),
                );
            } else if !stray.is_empty() {
                ui.add_space(2.0);
                ui.label(
                    RichText::new(format!(
                        "{} of the result {} a lone conflict marker outside any conflict; check it before marking resolved.",
                        resolve::lines_phrase(&stray),
                        if stray.len() == 1 { "has" } else { "have" }
                    ))
                    .size(size::SMALL)
                    .color(p.warning),
                );
            } else {
                // Same height as the hints above, so the panes stay put.
                ui.add_space(2.0);
                ui.label(
                    RichText::new(if dirty {
                        "Every conflict in this file is resolved. Mark resolved saves the result and stages it."
                    } else {
                        "Every conflict in this file is resolved and saved. Mark resolved stages it."
                    })
                    .size(size::SMALL)
                    .color(p.success),
                );
            }
        });
    kit::hairline(ui);
}

/// One file in the switcher.
struct Item {
    path: String,
    name: String,
    sub: String,
    badges: Vec<(String, Tone)>,
    dot: Tone,
    dimmed: bool,
}

fn items(st: &State) -> Vec<Item> {
    let mut paths: Vec<String> = st.files().iter().map(|e| e.path.clone()).collect();
    for p in &st.seen {
        if !paths.contains(p) {
            paths.push(p.clone());
        }
    }
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let (dir, name) = format::split_path(&path);
            let entry = st.files().iter().find(|e| e.path == path);
            let session = st.sessions.get(&path);
            let mut badges = Vec::new();
            let (dot, dimmed) = match (entry, session) {
                (None, _) if !matches!(st.overview, Some(Ok(_))) => (Tone::Neutral, false),
                (None, _) => {
                    badges.push(("Resolved".to_owned(), Tone::Success));
                    (Tone::Success, true)
                }
                (Some(_), Some(s)) if s.is_text() => {
                    if s.dirty {
                        badges.push(("Unsaved".to_owned(), Tone::Accent));
                    }
                    let left = s.unresolved();
                    if left > 0 {
                        badges.push((format!("{left} left"), Tone::Warning));
                        (Tone::Danger, false)
                    } else {
                        badges.push(("Ready".to_owned(), Tone::Success));
                        (Tone::Warning, false)
                    }
                }
                (Some(e), Some(s)) => {
                    let label = match s.presentation() {
                        Presentation::Binary => "Binary",
                        Presentation::TooLarge => "Too large",
                        Presentation::NotMerged => "Not merged",
                        Presentation::Special => "Link or submodule",
                        _ => e.kind.label(),
                    };
                    badges.push((label.to_owned(), Tone::Purple));
                    (Tone::Danger, false)
                }
                (Some(e), None) => {
                    badges.push((e.kind.code().to_owned(), Tone::Neutral));
                    (Tone::Danger, false)
                }
            };
            let sub = match entry {
                Some(e) if dir.is_empty() => e.kind.label().to_owned(),
                None if dir.is_empty() => "Staged as resolved".to_owned(),
                _ => dir.trim_end_matches('/').to_owned(),
            };
            Item {
                path: path.clone(),
                name: name.to_owned(),
                sub,
                badges,
                dot,
                dimmed,
            }
        })
        .collect()
}

fn file_list(ui: &mut Ui, st: &mut State, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let items = items(st);
    let open = st.open.clone().unwrap_or_default();
    let conflicted = st.files().len();
    kit::list_header(ui, "Conflicted files", Some(conflicted), |_| {});
    ui.add_space(4.0);
    if items.is_empty() {
        ui.add_space(8.0);
        kit::muted(
            ui,
            match &st.overview {
                None => "Reading the conflicted files…",
                Some(Err(_)) => "The conflicted files could not be read.",
                Some(Ok(_)) => "No file is conflicted.",
            },
        );
        return;
    }
    kit::list(
        ui,
        "conflict_file_list",
        items.len(),
        kit::ROW_H,
        |i| items[i].path == open,
        |ui, rect, resp, i| {
            let it = &items[i];
            let row = Row::new(&it.name)
                .subtitle(&it.sub)
                .badges(&it.badges)
                .leading(Leading::Dot(it.dot.color(&p)))
                .dimmed(it.dimmed);
            kit::paint_row(ui, rect, &row, it.path == open);
            if resp.clicked() && it.path != open {
                actions.push(Act::Open(it.path.clone()).into());
            }
        },
    );
}

fn body(ui: &mut Ui, st: &mut State, pending: Pending, busy: bool, actions: &mut Vec<Action>) {
    let Some(path) = st.open.clone() else { return };
    match &st.overview {
        None => return padded(ui, |ui| kit::loading(ui, "Reading the conflicts…")),
        Some(Err(e)) => {
            let e = e.clone();
            return error(ui, "Could not read the conflicts", &e, actions);
        }
        Some(Ok(_)) => {}
    }
    if !st.is_conflicted(&path) {
        return cards::resolved(ui, st, &path, pending, actions);
    }
    if let Some(e) = st.errors.get(&path).cloned() {
        return error(ui, &format!("Could not read {path}"), &e, actions);
    }
    let Some(s) = st.current() else {
        return padded(ui, |ui| kit::loading(ui, &format!("Reading {path}…")));
    };
    if st.whole.is_some() {
        return whole_editor(ui, st, &path, actions);
    }
    match s.presentation() {
        Presentation::Text if s.hunk_count() == 0 => cards::no_markers(ui, st, busy, actions),
        Presentation::Text => panes::show(ui, st, actions),
        _ => cards::choice_card(ui, st, busy, actions),
    }
}

pub fn padded(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    Frame::NONE
        .inner_margin(Margin {
            left: 28,
            right: 28,
            top: 20,
            bottom: 20,
        })
        .show(ui, add);
}

fn error(ui: &mut Ui, title: &str, e: &str, actions: &mut Vec<Action>) {
    padded(ui, |ui| {
        kit::callout_with(ui, Tone::Danger, title, e, |ui| {
            ui.add_space(4.0);
            if form::button(ui, "Try again", true).clicked() {
                actions.push(Act::Refresh.into());
            }
        });
    });
}

fn whole_editor(ui: &mut Ui, st: &mut State, path: &str, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let (eol, size) = st
        .current()
        .map(|s| (s.doc.eol.label(), s.doc.marker_size))
        .unwrap_or(("LF", 7));
    let Some(text) = st.whole.as_mut() else {
        return;
    };
    let (left, _) = resolve::remaining(text.as_bytes(), size);
    Frame::NONE
        .inner_margin(Margin::symmetric(20, 12))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    kit::subtitle(ui, "Edit the whole result");
                    kit::secondary(
                        ui,
                        &format!(
                            "Conflict markers left in the text become conflicts again when you apply ({} now). Line endings are written as {eol}. Nothing is saved until you save.",
                            left.len()
                        ),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if form::primary(ui, "Apply", true).clicked() {
                        actions.push(Act::WholeApply.into());
                    }
                    if form::button(ui, "Cancel", true).clicked() {
                        actions.push(Act::WholeCancel.into());
                    }
                });
            });
            ui.add_space(space::S);
            let h = ui.available_height();
            Frame::NONE
                .fill(if ui.visuals().dark_mode { p.chrome } else { p.canvas })
                .stroke(egui::Stroke::new(1.0, p.border_strong))
                .corner_radius(egui::CornerRadius::same(crate::theme::radius::M))
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    egui::ScrollArea::both()
                        .id_salt(("conflict_whole_scroll", path))
                        .auto_shrink([false, false])
                        .max_height(h - 16.0)
                        .show(ui, |ui| {
                            ui.add_sized(
                                vec2(ui.available_width(), (h - 24.0).max(100.0)),
                                egui::TextEdit::multiline(text)
                                    .id_salt(("conflict_whole", path))
                                    .font(egui::FontId::monospace(size::MONO))
                                    .frame(Frame::NONE)
                                    .code_editor()
                                    .lock_focus(true),
                            );
                        });
                });
        });
}
