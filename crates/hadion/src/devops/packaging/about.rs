//! The About section: this build, where its files are, the last update,
//! licences (Hadion, the bundled fonts, the main crates) and links.

use std::path::PathBuf;

use egui::{Align, Layout, RichText, Ui, vec2};
use hadion_core::devops::packaging::{release, time, update};

use super::{Act, info, updates};
use crate::format;
use crate::state::{Action, Settings};
use crate::theme::{Tone, pal, size, space};
use crate::views::form;
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Col};

const MIT: &str = include_str!("../../../../../LICENSE-MIT");
const APACHE: &str = include_str!("../../../../../LICENSE-APACHE");
const INTER_OFL: &str = include_str!("../../../assets/fonts/Inter-OFL.txt");
const JETBRAINS_OFL: &str = include_str!("../../../assets/fonts/JetBrainsMono-OFL.txt");
const LUCIDE: &str = include_str!("../../../assets/fonts/Lucide-LICENSE.txt");

/// `(crate, licence, what Hadion uses it for)`.
const CRATES: &[(&str, &str, &str)] = &[
    (
        "egui, eframe",
        "MIT or Apache-2.0",
        "Window, widgets, rendering",
    ),
    (
        "git2, libgit2-sys",
        "MIT or Apache-2.0",
        "Reading repositories",
    ),
    (
        "libgit2 (bundled C library)",
        "GPL-2.0 with linking exception",
        "Git object database",
    ),
    ("syntect", "MIT", "Syntax highlighting"),
    (
        "serde, serde_json, serde_yaml",
        "MIT or Apache-2.0",
        "Settings, GitHub answers, workflows",
    ),
    ("regex", "MIT or Apache-2.0", "Secret scanner rules"),
    ("aho-corasick", "Unlicense or MIT", "Scanner keyword gate"),
    ("similar", "Apache-2.0", "Diffs"),
    ("flate2", "MIT or Apache-2.0", "ZIP and deflate"),
    ("sha2", "MIT or Apache-2.0", "Update verification"),
    ("keyring", "MIT or Apache-2.0", "OS keychain"),
    ("portable-pty, vt100", "MIT", "Terminal panel"),
    ("notify", "CC0-1.0", "Watching the working tree"),
    ("notify-rust", "MIT or Apache-2.0", "Desktop notifications"),
    ("rfd", "MIT", "File dialogs"),
    ("image", "MIT or Apache-2.0", "Screenshots"),
    ("chrono", "MIT or Apache-2.0", "Dates"),
    ("open", "MIT", "Opening links and folders"),
];

#[derive(Default)]
pub struct State {
    /// The update log's most recent attempt.
    last_update: Vec<String>,
    /// The licence shown in full (0 MIT, 1 Apache-2.0, 2 Inter, 3 JetBrains
    /// Mono, 4 Lucide).
    licence_open: Option<usize>,
}

impl State {
    pub fn refresh(&mut self) {
        self.last_update = info::update_log()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| update::last_attempt(&t))
            .unwrap_or_default();
    }
}

/// The text "Copy details" puts on the clipboard (for bug reports).
fn details_text() -> String {
    let b = info::build();
    let mut lines = vec![
        b.one_line(),
        format!(
            "Commit:     {}",
            if b.commit.is_empty() {
                "unknown"
            } else {
                &b.commit
            }
        ),
        format!("Built:      {}", b.date_label()),
        format!(
            "Compiler:   {}",
            if b.rustc.is_empty() {
                "unknown"
            } else {
                &b.rustc
            }
        ),
        format!("OS:         {}", info::os()),
    ];
    if let Some(e) = info::exe() {
        lines.push(format!("Executable: {}", e.display()));
    }
    lines.join("\n")
}

/// A folder to open for a path (a file opens its folder).
fn folder_of(p: &std::path::Path, is_file: bool) -> PathBuf {
    if is_file {
        p.parent()
            .map(|d| d.to_path_buf())
            .unwrap_or_else(|| p.to_path_buf())
    } else {
        p.to_path_buf()
    }
}

pub fn show(
    ui: &mut Ui,
    st: &mut State,
    upd: &updates::State,
    settings: &mut Settings,
    actions: &mut Vec<Action>,
) {
    let b = info::build();
    kit::page_header(ui, "About Hadion", &b.one_line(), |ui| {
        if IconButton::new(Icon::Copy, "Copy details")
            .show(ui)
            .clicked()
        {
            actions.push(Action::Copy(details_text()));
        }
    });
    let prefs = &settings.ext.packaging;
    kit::scroll_page(ui, "packaging_about", |ui| {
        ui.set_max_width(ui.available_width().min(super::WIDTH));
        crate::views::brand::lockup(ui, 220.0);
        crate::views::insights::stats(
            ui,
            &[
                kit::Stat::new("Version", b.version.clone()),
                kit::Stat::new("Commit", b.commit_label()).tone_if(b.dirty, Tone::Warning),
                kit::Stat::new("Profile", b.profile.clone()),
                kit::Stat::new(
                    "Built",
                    time::parse_utc(&b.date)
                        .map(|t| format::relative(t, format::now_secs()))
                        .unwrap_or_else(|| "unknown".into()),
                ),
            ],
        );

        kit::section(ui, "Build", None);
        kit::kv(ui, 120.0, |kv| {
            kv.text("Version", b.version.clone());
            kv.row("Commit", |ui| {
                ui.horizontal(|ui| {
                    ui.label(kit::mono(if b.commit.is_empty() {
                        "unknown"
                    } else {
                        &b.commit
                    }));
                    if b.dirty {
                        kit::badge(ui, "Uncommitted changes", Tone::Warning);
                    }
                });
            });
            kv.text("Built", b.date_label());
            kv.text("Target", kit::mono(&b.target));
            kv.text("Profile", b.profile.clone());
            if b.rustc.is_empty() {
                kv.muted("Compiler", "unknown");
            } else {
                kv.text("Compiler", kit::mono(&b.rustc));
            }
            kv.text("OS", info::os());
            if b.remote.is_empty() {
                kv.muted("Built from", "no GitHub remote recorded");
            } else {
                kv.text("Built from", format!("github.com/{}", b.remote));
            }
        });
        if b.commit.is_empty() {
            kit::muted(
                ui,
                "This build was made outside a git checkout (or without git), so its commit is unknown.",
            );
        }

        kit::section(ui, "Files", None);
        let rows: Vec<(&str, Option<PathBuf>, bool)> = vec![
            ("Executable", info::exe(), true),
            ("Settings file", info::settings_file(), true),
            ("Data folder", info::data_dir(), false),
            ("Crash reports", info::crash_dir(), false),
            ("Update log", info::update_log(), true),
        ];
        kit::kv(ui, 120.0, |kv| {
            for (label, path, is_file) in rows {
                kv.row(label, |ui| match path {
                    None => {
                        ui.label(
                            RichText::new("not available on this system").color(pal(ui).text_3),
                        );
                    }
                    Some(p) => {
                        let exists = p.exists();
                        // Path on the left (wrapping), the button at the
                        // right end of the same line.
                        ui.horizontal_top(|ui| {
                            let w = (ui.available_width() - 84.0).max(120.0);
                            ui.allocate_ui_with_layout(
                                vec2(w, 0.0),
                                Layout::top_down(Align::Min),
                                |ui| {
                                    ui.set_width(w);
                                    ui.add(
                                        egui::Label::new(kit::mono(p.display().to_string()).color(
                                            if exists { pal(ui).text } else { pal(ui).text_2 },
                                        ))
                                        .wrap(),
                                    );
                                    if !exists {
                                        kit::badge(ui, "Not created yet", Tone::Neutral);
                                    }
                                },
                            );
                            if IconButton::new(Icon::Folder, "Open")
                                .compact()
                                .show(ui)
                                .clicked()
                            {
                                actions.push(Act::OpenFolder(folder_of(&p, is_file)).into());
                            }
                        });
                    }
                });
            }
        });

        kit::section(ui, "Last update", None);
        let lines = if st.last_update.is_empty() {
            &upd.last_log
        } else {
            &st.last_update
        };
        if lines.is_empty() {
            kit::muted(
                ui,
                "No update has been checked for or installed on this machine yet. Settings → Updates does that, and every step is written to the update log.",
            );
        } else {
            kit::code_block(ui, &lines.join("\n"), actions);
        }

        kit::section(ui, "Licences", None);
        form::card(ui, Tone::Neutral, |ui| {
            ui.horizontal(|ui| {
                kit::subtitle(ui, "Hadion");
                kit::badge(ui, "MIT OR Apache-2.0", Tone::Accent);
            });
            kit::secondary(
                ui,
                "Copyright (c) 2026 Dylan Westraadt. You may use, change and redistribute Hadion under either licence, at your option.",
            );
            ui.add_space(space::S);
            for (i, (name, badge, note, url, text)) in [
                (
                    "MIT License",
                    "MIT",
                    "LICENSE-MIT, beside Hadion in the installer and the zip.",
                    "https://opensource.org/license/mit",
                    MIT,
                ),
                (
                    "Apache License 2.0",
                    "Apache-2.0",
                    "LICENSE-APACHE, beside Hadion in the installer and the zip.",
                    "https://www.apache.org/licenses/LICENSE-2.0",
                    APACHE,
                ),
                (
                    "Inter",
                    "SIL Open Font License 1.1",
                    "Bundled in the executable (variable font, weights 400 / 500 / 600); the licence file ships beside Hadion in the installer and the zip.",
                    "https://rsms.me/inter/",
                    INTER_OFL,
                ),
                (
                    "JetBrains Mono",
                    "SIL Open Font License 1.1",
                    "Bundled in the executable (variable font); the licence file ships beside Hadion in the installer and the zip.",
                    "https://www.jetbrains.com/lp/mono/",
                    JETBRAINS_OFL,
                ),
                (
                    "Lucide icons",
                    "ISC",
                    "Every icon in Hadion (lucide-static 1.53.0, bundled in the executable); the licence file ships beside Hadion in the installer and the zip.",
                    "https://lucide.dev/",
                    LUCIDE,
                ),
            ]
            .into_iter()
            .enumerate()
            {
                ui.horizontal(|ui| {
                    kit::subtitle(ui, name);
                    kit::badge(ui, badge, Tone::Neutral);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let open = st.licence_open == Some(i);
                        if IconButton::new(
                            if open {
                                Icon::ChevronUp
                            } else {
                                Icon::ChevronDown
                            },
                            if open { "Hide licence" } else { "View licence" },
                        )
                        .compact()
                        .show(ui)
                        .clicked()
                        {
                            st.licence_open = if open { None } else { Some(i) };
                        }
                        if IconButton::new(Icon::External, "Website")
                            .compact()
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Action::OpenUrl(url.to_owned()));
                        }
                    });
                });
                kit::secondary(ui, note);
                if st.licence_open == Some(i) {
                    kit::code_block(ui, text.trim(), actions);
                }
                ui.add_space(space::S);
            }
        });
        ui.add_space(space::S);
        kit::secondary(
            ui,
            "Main Rust crates compiled into Hadion. Every crate, with its licence texts, is listed in THIRD-PARTY-LICENSES.txt in the licenses folder beside the executable.",
        );
        let rows: Vec<Vec<String>> = CRATES
            .iter()
            .map(|(c, l, u)| vec![(*c).to_owned(), (*l).to_owned(), (*u).to_owned()])
            .collect();
        kit::table(
            ui,
            &[
                Col::text("Crate"),
                Col::text("Licence"),
                Col::text("Used for"),
            ],
            &rows,
        );

        kit::section(ui, "Links", None);
        let repo = if release::valid_repo(&prefs.source) {
            prefs.source.trim().to_owned()
        } else {
            b.remote.clone()
        };
        let mut links: Vec<(String, String)> = Vec::new();
        if release::valid_repo(&repo) {
            links.push((
                format!("Source: github.com/{repo}"),
                format!("https://github.com/{repo}"),
            ));
            links.push((
                "Releases".to_owned(),
                format!("https://github.com/{repo}/releases"),
            ));
            links.push((
                "Report a problem".to_owned(),
                format!("https://github.com/{repo}/issues/new"),
            ));
        }
        links.push((
            "egui".to_owned(),
            "https://github.com/emilk/egui".to_owned(),
        ));
        links.push(("libgit2".to_owned(), "https://libgit2.org".to_owned()));
        for (label, url) in links {
            ui.horizontal(|ui| {
                if kit::link(ui, &label).clicked() {
                    actions.push(Action::OpenUrl(url.clone()));
                }
                ui.label(RichText::new(&url).size(size::SMALL).color(pal(ui).text_3));
            });
        }
        if !release::valid_repo(&repo) {
            kit::muted(
                ui,
                "Set the source repository in Updates to get links to its releases and issues.",
            );
        }
    });
}
