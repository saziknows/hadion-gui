//! Bodies other than the panes: the choice card for conflicts without
//! text hunks (binary, delete / modify, one-sided adds, both deleted, too
//! large, not merged by git), a conflicted file without markers, and the
//! resolved states.

use egui::{Frame, Margin, Ui};
use gitvis_core::Pending;
use gitvis_core::devops::conflict::actions;
use gitvis_core::devops::conflict::load::{Blob, Presentation, Working};
use gitvis_core::devops::conflict::markers;

use super::{Act, State};
use crate::format;
use crate::state::Action;
use crate::theme::{Tone, space};
use crate::views::form;
use crate::views::icons::Icon;
use crate::views::kit::{self, Col};

fn page(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            Frame::NONE
                .inner_margin(Margin {
                    left: 28,
                    right: 28,
                    top: 20,
                    bottom: 28,
                })
                .show(ui, |ui| {
                    // Never wider than the pane: `set_max_width` can also widen.
                    ui.set_max_width(ui.available_width().min(kit::DETAIL_MAX));
                    ui.spacing_mut().item_spacing.y = 8.0;
                    add(ui)
                });
        });
}

fn blob_row(role: &str, name: &str, b: Option<&Blob>, absent: &str) -> Vec<String> {
    match b {
        None => vec![
            role.to_owned(),
            name.to_owned(),
            absent.to_owned(),
            String::new(),
            String::new(),
        ],
        Some(b) => {
            let what = if b.is_symlink() {
                "Symbolic link".to_owned()
            } else if b.is_submodule() {
                "Submodule".to_owned()
            } else if b.bytes.is_none() {
                "Too large to read".to_owned()
            } else if b.binary {
                "Binary".to_owned()
            } else {
                let lines = b.bytes.as_deref().map(markers::line_count).unwrap_or(0);
                format!("Text, {}", format::plural(lines, "line", "lines"))
            };
            vec![
                role.to_owned(),
                name.to_owned(),
                what,
                format::bytes(b.size),
                b.oid.chars().take(10).collect(),
            ]
        }
    }
}

/// Binary, delete / modify and the other conflicts without text hunks.
pub fn choice_card(ui: &mut Ui, st: &mut State, busy: bool, actions: &mut Vec<Action>) {
    let Some(s) = st.current() else { return };
    let (ours, theirs) = st.sides(Some(s));
    let path = s.path().to_owned();
    let kind = s.load.entry.kind;
    let presentation = s.presentation();
    let (title, why) = match presentation {
        Presentation::Binary => (
            "Binary file: choose a version".to_owned(),
            format!(
                "Git cannot merge binary content line by line, so it left the version from {} in the working tree and kept all three versions in the index. Keep one of them, or delete the file.",
                ours.name
            ),
        ),
        Presentation::TooLarge => (
            "Too large for the editor".to_owned(),
            format!(
                "This file is larger than {} on at least one side. Choose a version here, or open the raw diff.",
                format::bytes(gitvis_core::devops::conflict::load::MAX_TEXT)
            ),
        ),
        Presentation::NotMerged => {
            let a = &s.load.attrs;
            let reason = match &a.driver {
                Some(d) => format!("the merge driver \u{201c}{d}\u{201d} from .gitattributes"),
                None => "a binary or -merge attribute in .gitattributes".to_owned(),
            };
            (
                "Git did not merge this file's text".to_owned(),
                format!(
                    "Because of {reason}, git did not merge the text: the working file holds the version from {} with no conflict markers. Keep a version, or delete the file.",
                    ours.name
                ),
            )
        }
        Presentation::Special => (
            "Symbolic link or submodule".to_owned(),
            "A link or submodule cannot be merged as text. Keep one side's version, or delete it."
                .to_owned(),
        ),
        _ => (
            format!("{}: choose what to keep", kind.label()),
            kind.explain().to_owned(),
        ),
    };
    let specs = actions::choice_specs(kind, &path, &ours.name, &theirs.name);
    let working = match &s.load.working {
        Working::Text(b) => vec![
            "Working tree".to_owned(),
            "on disk now".to_owned(),
            format!(
                "Text, {}",
                format::plural(markers::line_count(b), "line", "lines")
            ),
            format::bytes(b.len()),
            String::new(),
        ],
        Working::Binary { size } => vec![
            "Working tree".into(),
            "on disk now".into(),
            "Binary".into(),
            format::bytes(*size),
            String::new(),
        ],
        Working::TooLarge { size } => vec![
            "Working tree".into(),
            "on disk now".into(),
            "Too large to read".into(),
            format::bytes(*size),
            String::new(),
        ],
        Working::Missing => vec![
            "Working tree".into(),
            "on disk now".into(),
            "No file".into(),
            String::new(),
            String::new(),
        ],
    };
    let absent_ours = match kind {
        gitvis_core::devops::conflict::ConflictKind::DeletedByUs
        | gitvis_core::devops::conflict::ConflictKind::BothDeleted => "Deleted",
        _ => "Not there",
    };
    let absent_theirs = match kind {
        gitvis_core::devops::conflict::ConflictKind::DeletedByThem
        | gitvis_core::devops::conflict::ConflictKind::BothDeleted => "Deleted",
        _ => "Not there",
    };
    let rows = vec![
        blob_row(
            "Base",
            "common ancestor",
            s.load.stages[0].as_ref(),
            "None (added on both sides)",
        ),
        blob_row("Ours", &ours.name, s.load.stages[1].as_ref(), absent_ours),
        blob_row(
            "Theirs",
            &theirs.name,
            s.load.stages[2].as_ref(),
            absent_theirs,
        ),
        working,
    ];
    let code = kind.code();
    page(ui, "conflict_card", |ui| {
        ui.horizontal(|ui| {
            kit::title(ui, &title);
            kit::badge(ui, code, Tone::Warning);
        });
        kit::body(ui, &why);
        kit::section(ui, "Versions", None);
        kit::table(
            ui,
            &[
                Col::text("Version"),
                Col::text("From"),
                Col::text("Content"),
                Col::num("Size"),
                Col::text("Object"),
            ],
            &rows,
        );
        kit::section(ui, "Choose", Some(specs.len()));
        for spec in &specs {
            form::card(
                ui,
                if spec.danger {
                    Tone::Danger
                } else {
                    Tone::Neutral
                },
                |ui| {
                    kit::subtitle(ui, &spec.title);
                    kit::secondary(ui, &spec.explain);
                    ui.add_space(4.0);
                    kit::code_block(ui, &actions::render(&spec.commands).join("\n"), actions);
                    form::actions_row(ui, |ui| {
                        let clicked = if spec.danger {
                            form::danger(ui, &spec.title, !busy).clicked()
                        } else {
                            form::primary(ui, &spec.title, !busy).clicked()
                        };
                        if clicked {
                            actions.push(Act::Choose(spec.choice).into());
                        }
                    });
                },
            );
            ui.add_space(space::S);
        }
        kit::muted(
            ui,
            "Every choice is confirmed first and then stages the result, so the file counts as resolved.",
        );
        ui.add_space(space::S);
        if form::button(ui, "Open the raw diff", true).clicked() {
            actions.push(Act::OpenRawDiff.into());
        }
    });
}

/// A conflicted text file whose working copy has no markers left.
pub fn no_markers(ui: &mut Ui, st: &mut State, busy: bool, actions: &mut Vec<Action>) {
    let Some(s) = st.current() else { return };
    let path = s.path().to_owned();
    page(ui, "conflict_no_markers", |ui| {
        if kit::empty_state(
            ui,
            Icon::Info,
            "No conflict markers left",
            &format!(
                "{path} is still conflicted in the index, but its text has no conflict markers: it was resolved by hand or in another program. Mark it resolved to stage it as it is, or write the markers again from the index."
            ),
            Some((Icon::Check, "Mark resolved")),
        ) && !busy
        {
            actions.push(Act::MarkResolved.into());
        }
        ui.add_space(space::S);
        ui.vertical_centered(|ui| {
            if form::button(ui, "Recreate the conflict (zdiff3 style)", !busy).clicked() {
                actions.push(Act::Recreate("zdiff3".into()).into());
            }
        });
    });
}

/// The open file is no longer conflicted.
pub fn resolved(
    ui: &mut Ui,
    st: &mut State,
    path: &str,
    pending: Pending,
    actions: &mut Vec<Action>,
) {
    let (_, name) = format::split_path(path);
    let left = st.files().len();
    let next = st.next_file();
    let notice = st.notice.clone();
    page(ui, "conflict_resolved", |ui| {
        let (title, body, action) = match &next {
            Some(n) => (
                format!("{name} is resolved"),
                format!(
                    "It is staged as resolved. {} still conflicted.",
                    format::plural(left, "file is", "files are")
                ),
                Some((
                    Icon::ChevronDown,
                    format!("Next: {}", format::split_path(n).1),
                )),
            ),
            None if pending != Pending::None => (
                "All conflicts are resolved".to_owned(),
                format!(
                    "Every conflicted file is staged. Continue the {} to record the result, or look over the staged changes first.",
                    pending.label().to_lowercase()
                ),
                Some((
                    Icon::Check,
                    format!("Continue the {}", pending.label().to_lowercase()),
                )),
            ),
            None => (
                "No conflicts left".to_owned(),
                "Nothing in this repository is conflicted any more.".to_owned(),
                None,
            ),
        };
        let clicked = kit::empty_state(
            ui,
            Icon::Check,
            &title,
            &body,
            action.as_ref().map(|(i, l)| (*i, l.as_str())),
        );
        if clicked {
            actions.push(if next.is_some() {
                Act::NextFile.into()
            } else {
                Act::Continue.into()
            });
        }
        ui.add_space(space::S);
        ui.vertical_centered(|ui| {
            if form::button(ui, "Close the editor", true).clicked() {
                actions.push(Act::Close.into());
            }
        });
        if let Some(n) = notice {
            ui.add_space(space::L);
            ui.vertical_centered(|ui| {
                ui.set_max_width(ui.available_width().min(520.0));
                kit::callout(ui, Tone::Warning, "", &n);
            });
        }
    });
}
