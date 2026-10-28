//! Settings → Terminal: default shell, font size, scrollback, cursor,
//! copy on select, start folder, the shells found (and where gitvis looked
//! for the missing ones), the keys, and the open terminals.

use egui::{FontId, RichText, Ui};
use gitvis_core::devops::terminal::Status;
use gitvis_core::devops::terminal::shells::native_path;

use super::{Act, CursorStyle, FONT_SIZES, SCROLLBACKS, Shells, State};
use crate::format;
use crate::state::{Action, Settings};
use crate::theme::{Tone, pal, size, space};
use crate::views::form;
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Col};

pub(super) fn show(
    ui: &mut Ui,
    st: &mut State,
    settings: &mut Settings,
    actions: &mut Vec<Action>,
) {
    kit::page_header(
        ui,
        "Terminal",
        "Shells in a panel under every page (Ctrl+`)",
        |ui| {
            if IconButton::new(Icon::Terminal, "New terminal")
                .primary(true)
                .show(ui)
                .clicked()
            {
                actions.push(Act::New(None).into());
            }
        },
    );
    kit::scroll_page(ui, "settings_terminal", |ui| {
        ui.set_max_width(kit::PROSE);
        shell_section(ui, st, settings);
        display_section(ui, settings);
        behaviour_section(ui, settings);
        detected_section(ui, st, actions);
        running_section(ui, st);
        keys_section(ui);
        if settings.safe_mode {
            ui.add_space(space::L);
        }
        ui.add_space(space::M);
        kit::callout(
            ui,
            if settings.safe_mode {
                Tone::Warning
            } else {
                Tone::Neutral
            },
            "Safe mode does not cover the terminal",
            "Commands typed in a terminal run as typed, like in any other terminal window: safe mode cannot block a git push or an API call made there.",
        );
    });
}

fn shell_section(ui: &mut Ui, st: &State, settings: &mut Settings) {
    let prefs = &mut settings.ext.terminal;
    kit::section(ui, "Shell", None);
    form::card(ui, Tone::Neutral, |ui| {
        match st.shells.ready() {
            Some(d) if !d.shells.is_empty() => {
                let first = d.shells[0].name.clone();
                let mut options: Vec<(String, String)> =
                    vec![(String::new(), format!("First found ({first})"))];
                options.extend(d.shells.iter().map(|s| (s.id.clone(), s.name.clone())));
                let current = d.pick(&prefs.default_shell);
                form::field(ui, "Default shell", None, |ui| {
                    form::select(
                        ui,
                        "term_default_shell",
                        &mut prefs.default_shell,
                        &options,
                        300.0,
                    );
                });
                if let Some(s) = current {
                    kit::secondary(ui, &format!("Starts {}", s.command_line()));
                }
                if !prefs.default_shell.is_empty() && d.find(&prefs.default_shell).is_none() {
                    form::error(
                        ui,
                        &format!(
                            "{} is not installed on this machine; the first shell found is used instead.",
                            prefs.default_shell
                        ),
                    );
                }
            }
            Some(_) => {
                kit::secondary(
                    ui,
                    "No shell was found on this machine (see Detected shells).",
                );
            }
            None => kit::loading(ui, "Looking for shells…"),
        }
        ui.add_space(space::S);
        form::checkbox(
            ui,
            &mut prefs.start_in_repo,
            "Start in the repository folder",
        );
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| "your home folder".into());
        kit::secondary(
            ui,
            &if prefs.start_in_repo {
                "New terminals open in the active repository's working folder. Use Go to repository in the panel to move an open one there.".to_owned()
            } else {
                format!("New terminals open in {home}.")
            },
        );
    });
}

fn display_section(ui: &mut Ui, settings: &mut Settings) {
    let p = *pal(ui);
    let prefs = &mut settings.ext.terminal;
    kit::section(ui, "Display", None);
    form::card(ui, Tone::Neutral, |ui| {
        let mut size_key = format!("{}", prefs.font_size);
        let options: Vec<(String, String)> = FONT_SIZES
            .iter()
            .map(|s| (format!("{s}"), format!("{s} pt")))
            .collect();
        form::row(ui, 130.0, "Font size", |ui| {
            if form::select(ui, "term_font_size", &mut size_key, &options, 120.0)
                && let Ok(v) = size_key.parse::<f32>()
            {
                prefs.font_size = v;
            }
        });
        form::row(ui, 130.0, "Cursor", |ui| {
            if let Some(c) = kit::segmented(
                ui,
                &[
                    (CursorStyle::Block, "Block"),
                    (CursorStyle::Underline, "Underline"),
                    (CursorStyle::Bar, "Bar"),
                ],
                prefs.cursor,
            ) {
                prefs.cursor = c;
            }
        });
        form::row(ui, 130.0, "", |ui| {
            form::checkbox(ui, &mut prefs.blink, "Blinking cursor");
        });
        ui.add_space(space::S);
        // A preview in the chosen size and the theme's terminal colours.
        let font = FontId::monospace(prefs.font_size.max(size::CAPTION));
        egui::Frame::NONE
            .fill(p.canvas)
            .stroke(egui::Stroke::new(1.0, p.border))
            .corner_radius(egui::CornerRadius::same(crate::theme::radius::M))
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                let dark = ui.visuals().dark_mode;
                let ansi = if dark {
                    &gitvis_core::devops::terminal::screen::ANSI_DARK
                } else {
                    &gitvis_core::devops::terminal::screen::ANSI_LIGHT
                };
                let c = |i: usize| egui::Color32::from_rgb(ansi[i][0], ansi[i][1], ansi[i][2]);
                let mut job = egui::text::LayoutJob::default();
                let fmt = |color| egui::TextFormat {
                    font_id: font.clone(),
                    color,
                    ..Default::default()
                };
                job.append("~/repo ", 0.0, fmt(c(4)));
                job.append("main ", 0.0, fmt(c(2)));
                job.append("$ git status --short\n", 0.0, fmt(p.text));
                job.append(" M ", 0.0, fmt(c(1)));
                job.append("src/main.rs   ", 0.0, fmt(p.text));
                job.append("?? ", 0.0, fmt(c(1)));
                job.append("notes.md   0O 1lI {}[]", 0.0, fmt(p.text));
                ui.label(job);
            });
    });
}

fn behaviour_section(ui: &mut Ui, settings: &mut Settings) {
    let prefs = &mut settings.ext.terminal;
    kit::section(ui, "History and selection", None);
    form::card(ui, Tone::Neutral, |ui| {
        let mut key = prefs.scrollback.to_string();
        let options: Vec<(String, String)> = SCROLLBACKS
            .iter()
            .map(|n| (n.to_string(), format!("{} lines", format::count(*n))))
            .collect();
        form::row(ui, 130.0, "Scrollback", |ui| {
            if form::select(ui, "term_scrollback", &mut key, &options, 160.0)
                && let Ok(v) = key.parse()
            {
                prefs.scrollback = v;
            }
        });
        // About 60 bytes per line of history plus its text.
        let estimate = prefs.scrollback * 160;
        kit::secondary(
            ui,
            &format!(
                "Applies to terminals opened from now on. A full history of 120-column lines takes about {} per terminal.",
                format::bytes(estimate)
            ),
        );
        ui.add_space(space::S);
        form::checkbox(ui, &mut prefs.copy_on_select, "Copy on select");
        kit::secondary(
            ui,
            "Selected text goes to the clipboard as soon as the mouse is released. Otherwise use Ctrl+C (with a selection), Ctrl+Shift+C or the right-click menu.",
        );
    });
}

fn detected_section(ui: &mut Ui, st: &mut State, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    // `term-settings-scroll` (scripts): bring this part of the page up.
    if std::mem::take(&mut st.settings_scroll) {
        ui.scroll_to_cursor(Some(egui::Align::TOP));
    }
    let count = st.shells.ready().map(|d| d.shells.len());
    kit::section_with(ui, "Detected shells", count, |ui| {
        let loading = matches!(st.shells, Shells::Loading(..));
        if IconButton::new(Icon::Rerun, "Check again")
            .compact()
            .enabled(!loading)
            .show(ui)
            .clicked()
        {
            actions.push(Act::Discover.into());
        }
    });
    match &st.shells {
        Shells::NotLoaded => {
            kit::muted(ui, "Not looked for yet.");
        }
        Shells::Loading(_, since) => kit::loading(
            ui,
            &format!(
                "Looking for shells… {}",
                format::millis(since.elapsed().as_millis() as u64)
            ),
        ),
        Shells::Ready(d) => {
            if d.shells.is_empty() {
                kit::callout(
                    ui,
                    Tone::Warning,
                    "No shell found",
                    "gitvis could not find any shell it knows. The list below says where it looked.",
                );
            } else {
                let rows: Vec<Vec<String>> = d
                    .shells
                    .iter()
                    .map(|s| {
                        vec![
                            s.name.clone(),
                            s.command_line(),
                            s.found_via.clone(),
                            s.id.clone(),
                        ]
                    })
                    .collect();
                kit::table(
                    ui,
                    &[
                        Col::text("Shell"),
                        Col::text("Command"),
                        Col::text("Found via"),
                        Col::text("Script name"),
                    ],
                    &rows,
                );
            }
            for (name, looked) in &d.missing {
                ui.add_space(space::S);
                ui.horizontal_wrapped(|ui| {
                    kit::badge(ui, "not found", Tone::Neutral);
                    ui.label(
                        RichText::new(name)
                            .size(size::SMALL)
                            .family(crate::theme::medium())
                            .color(p.text),
                    );
                });
                kit::secondary(ui, &format!("Looked at {looked}."));
                if name == "PowerShell 7" {
                    kit::secondary(ui, "Install it with:");
                    kit::code_block(ui, "winget install --id Microsoft.PowerShell", actions);
                }
            }
            if !d.commands.is_empty() {
                ui.add_space(space::S);
                kit::muted(ui, &format!("Ran: {}", d.commands.join(", ")));
            }
        }
    }
}

fn running_section(ui: &mut Ui, st: &State) {
    kit::section(ui, "Open terminals", Some(st.tabs.len()));
    if st.tabs.is_empty() {
        kit::secondary(
            ui,
            "None. Open the panel with Ctrl+` or New terminal above; a terminal keeps running while the panel is hidden.",
        );
        return;
    }
    let rows: Vec<Vec<String>> = st
        .tabs
        .iter()
        .map(|t| {
            let status = match t.status() {
                Status::Starting => "starting".to_owned(),
                Status::Running { pid: Some(pid) } => format!("running (pid {pid})"),
                Status::Running { pid: None } => "running".to_owned(),
                Status::Exited { code, .. } => format!("exited, code {code}"),
                Status::Failed(_) => "could not start".to_owned(),
            };
            let (received, sent, history) = t
                .session
                .try_term()
                .map(|term| {
                    (
                        format::bytes(term.bytes_in as usize),
                        format::bytes(term.bytes_out as usize),
                        format::count(term.screen.scrollback_len()),
                    )
                })
                .unwrap_or_default();
            vec![
                t.title(),
                native_path(&t.cwd),
                status,
                received,
                sent,
                history,
            ]
        })
        .collect();
    kit::table(
        ui,
        &[
            Col::text("Terminal"),
            Col::text("Folder"),
            Col::text("Status"),
            Col::num("Received"),
            Col::num("Sent"),
            Col::num("History"),
        ],
        &rows,
    );
}

fn keys_section(ui: &mut Ui) {
    kit::section(ui, "Keys", None);
    kit::body(
        ui,
        "While the terminal has focus every key goes to the shell, including Tab, the arrows and the app's own shortcuts (Ctrl+K, Ctrl+L, Ctrl+Z, Ctrl+W). Ctrl+` still hides the panel; click outside the terminal to use the app's shortcuts.",
    );
    ui.add_space(space::S);
    kit::kv(ui, 260.0, |kv| {
        kv.text("Ctrl+`", "Show or hide the terminal panel");
        kv.text(
            "Ctrl+C",
            "Copy when text is selected, otherwise interrupt (^C)",
        );
        kv.text("Ctrl+Shift+C, Ctrl+Insert", "Copy the selection");
        kv.text(
            "Ctrl+V, Ctrl+Shift+V, Shift+Insert",
            "Paste (bracketed when the program asks for it)",
        );
        kv.text("Shift+PgUp / Shift+PgDn", "Scroll the history a page");
        kv.text("Ctrl+Shift+Up / Down", "Scroll a line");
        kv.text(
            "Shift+Home / Shift+End",
            "Oldest line / back to the live screen",
        );
        kv.text("Double / triple click", "Select a word / a line");
        kv.text(
            "Shift+drag, Shift+right click",
            "Select and open the menu in programs that use the mouse",
        );
        kv.text(
            "Mouse wheel",
            "History; arrow keys in full-screen programs; mouse reports when the program asks",
        );
    });
}
