//! Settings → Identities: `includeIf` rules with their files and the
//! identity in them, which identity applies in every open repository (and
//! in a typed folder), and the add / edit form.

use std::path::PathBuf;

use egui::{RichText, Ui};
use gitvis_core::devops::identity::config::Entry;
use gitvis_core::devops::identity::includes::{self, Applied, Condition, NewIdentity, Rule};
use gitvis_core::devops::identity::{global_config_file, slash, tilde};

use super::widgets as w;
use super::{Act, IdentityForm, IdentityReport, State, form_file};
use crate::devops::settings_page::Section;
use crate::state::{Action, RepoState, Settings};
use crate::theme::{pal, size, space};
use crate::views::form::{self, InputOpts};
use crate::views::icons::{Icon, IconButton};
use crate::views::insights::{self, Cell};
use crate::views::kit::{self, Col, Tone};

pub fn show(
    ui: &mut Ui,
    st: &mut State,
    settings: &mut Settings,
    _repo: Option<&RepoState>,
    actions: &mut Vec<Action>,
) {
    let State {
        identities,
        id_form,
        check,
        check_path,
        config,
        busy,
        scroll,
        ..
    } = st;
    let busy = *busy;
    let form_open = id_form.is_some();
    if w::header(
        ui,
        "Identities",
        "A different name and e-mail per folder, with includeIf rules in your global config",
        identities.loading,
        identities.at,
        |ui| {
            if !form_open
                && IconButton::new(Icon::Plus, "Add identity")
                    .primary(true)
                    .compact()
                    .show(ui)
                    .clicked()
            {
                actions.push(Act::OpenIdentityForm(None).into());
            }
        },
    ) {
        actions.push(Act::Refresh(Section::Identities).into());
    }
    // The global (or system) value itself: the effective one may come from
    // the repository's own config, which says nothing about other folders.
    // `None` while the config has not been read.
    let global = |key: &str| -> Option<Option<String>> {
        config.ok().map(|s| {
            s.values(key)
                .into_iter()
                .rfind(|e| e.scope <= gitvis_core::devops::identity::Scope::Global)
                .map(|e| e.value_text().to_owned())
        })
    };
    let global_name = global("user.name");
    let global_email = global("user.email");
    w::page(ui, "identity_identities", scroll, |ui| {
        if let Some(f) = id_form.as_mut() {
            identity_form(
                ui,
                f,
                identities.ok(),
                &mut settings.ext.identity.icase,
                busy,
                actions,
            );
            ui.add_space(space::L);
        }
        explainer(ui, actions);
        match &identities.data {
            None => kit::loading(ui, "Reading the includeIf rules and their files"),
            Some(Err(e)) => {
                if w::error_card(ui, "Could not read the identities", e, "Try again") {
                    actions.push(Act::Refresh(Section::Identities).into());
                }
            }
            Some(Ok(rep)) => {
                rules(
                    ui,
                    rep,
                    global_name.as_ref().map(Option::as_deref),
                    global_email.as_ref().map(Option::as_deref),
                    busy,
                    form_open,
                    actions,
                );
                applies(ui, rep, actions);
                check_folder(ui, check_path, check, rep, actions);
            }
        }
    });
}

fn explainer(ui: &mut Ui, actions: &mut Vec<Action>) {
    kit::callout_with(
        ui,
        Tone::Neutral,
        "How identities work",
        "An identity is a small config file (user.name, user.email, maybe user.signingkey) that your global config includes for every repository under a folder. Repositories anywhere else keep your global name and e-mail.",
        |ui| {
            ui.add_space(4.0);
            kit::code_block(
                ui,
                "[includeIf \"gitdir/i:C:/work/\"]\n\tpath = ~/.gitconfig-work",
                actions,
            );
            ui.add_space(4.0);
            let small = |t: &str| RichText::new(t).size(size::SMALL);
            kit::kv(ui, 104.0, |kv| {
                kv.text(
                    "Trailing slash",
                    small("C:/work/ matches every repository below C:/work; C:/work without it matches only a .git folder at exactly that path."),
                );
                kv.text(
                    "Case",
                    small("gitdir/i: ignores case, so C:/Work and c:/work both match; gitdir: is case-sensitive. Use gitdir/i: on Windows."),
                );
                kv.text("Slashes", small("Write paths with forward slashes."));
            });
        },
    );
}

/// `rules` gets the global name and e-mail as `None` while the config has
/// not been read yet, `Some(None)` when it has no such entry.
#[allow(clippy::too_many_arguments)]
fn rules(
    ui: &mut Ui,
    rep: &IdentityReport,
    global_name: Option<Option<&str>>,
    global_email: Option<Option<&str>>,
    busy: bool,
    form_open: bool,
    actions: &mut Vec<Action>,
) {
    let home = rep.home.as_deref();
    kit::section(ui, "Rules", Some(rep.rules.len()));
    if rep.rules.is_empty() {
        let who = match (global_name, global_email) {
            (Some(Some(n)), Some(Some(e))) => format!("{n} <{e}>"),
            (Some(None), Some(Some(e))) => e.to_owned(),
            (Some(Some(n)), Some(None)) => {
                format!("{n} with no global e-mail (git refuses to commit without one)")
            }
            (Some(None), Some(None)) => {
                "no global name and e-mail (git refuses to commit without them)".to_owned()
            }
            // The config has not been read yet: say nothing wrong.
            _ => "your global name and e-mail".to_owned(),
        };
        kit::callout_with(
            ui,
            Tone::Neutral,
            "No identities per folder yet",
            &format!(
                "Every repository commits as {who}, from {}. Add an identity to commit with another name or e-mail in the repositories under a folder, for example your work's.",
                rep.global_file
                    .as_deref()
                    .map(|f| tilde(f, home))
                    .unwrap_or_else(|| "~/.gitconfig".to_owned())
            ),
            |ui| {
                if !form_open {
                    ui.add_space(4.0);
                    if IconButton::new(Icon::Plus, "Add identity")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Act::OpenIdentityForm(None).into());
                    }
                }
            },
        );
        return;
    }
    for (i, r) in rep.rules.iter().enumerate() {
        rule_card(ui, i, r, rep, busy, actions);
        ui.add_space(space::S);
    }
}

fn rule_card(
    ui: &mut Ui,
    i: usize,
    r: &Rule,
    rep: &IdentityReport,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let home = rep.home.as_deref();
    let warnings = r.warnings();
    let shared = rep
        .rules
        .iter()
        .filter(|o| gitvis_core::devops::identity::same_path(&o.target, &r.target))
        .count()
        > 1;
    form::card(
        ui,
        if !r.exists {
            Tone::Danger
        } else if warnings.is_empty() {
            Tone::Neutral
        } else {
            Tone::Warning
        },
        |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.label(
                    kit::mono(if r.condition.pattern().is_empty() {
                        "(always)"
                    } else {
                        r.condition.pattern()
                    })
                    .size(size::BODY)
                    .color(p.text_strong),
                );
                kit::badge(ui, &format!("Rule {}", i + 1), Tone::Neutral);
                kit::badge(ui, r.condition.kind(), Tone::Neutral);
                w::scope_badge(ui, r.scope);
                if !r.exists {
                    kit::badge(ui, "File missing", Tone::Danger);
                }
            });
            ui.add_space(4.0);
            kit::kv(ui, 120.0, |kv| {
                kv.text("Applies to", r.condition.explain());
                kv.row("Identity file", |ui| {
                    w::copyable(ui, &tilde(&r.target, home), actions);
                });
                let id = &r.identity;
                for (label, v) in [
                    ("Name", &id.name),
                    ("E-mail", &id.email),
                    ("Signing key", &id.signing_key),
                ] {
                    match v {
                        Some(v) => kv.text(label, kit::mono(v.as_str())),
                        None => kv.muted(
                            label,
                            if r.exists {
                                "Not set in this file: the global value applies"
                            } else {
                                "—"
                            },
                        ),
                    }
                }
                if let Some(f) = &id.format {
                    kv.text("Signature format", kit::mono(f.as_str()));
                }
                if id.keys > 0 {
                    kv.muted("Keys in the file", &crate::format::count(id.keys));
                }
                kv.muted(
                    "Written in",
                    &match (&r.config_file, r.line) {
                        (Some(f), Some(l)) => format!("{}:{l}", tilde(f, home)),
                        (Some(f), None) => tilde(f, home),
                        (None, _) => "command line".to_owned(),
                    },
                );
            });
            for wn in &warnings {
                ui.add_space(2.0);
                kit::callout(ui, Tone::Warning, "", wn);
            }
            w::buttons(ui, |ui| {
                let editable = matches!(r.condition, Condition::GitDir { .. });
                if editable
                    && IconButton::new(Icon::Edit, "Edit")
                        .compact()
                        .enabled(!busy)
                        .show(ui)
                        .clicked()
                {
                    actions.push(Act::OpenIdentityForm(Some(i)).into());
                }
                if IconButton::new(Icon::Trash, "Remove rule…")
                    .compact()
                    .enabled(!busy)
                    .show(ui)
                    .clicked()
                {
                    actions.push(
                        Act::RemoveIdentity {
                            rule: i,
                            delete_file: false,
                        }
                        .into(),
                    );
                }
                if r.exists
                    && !shared
                    && IconButton::new(Icon::Trash, "Remove rule and file…")
                        .compact()
                        .danger()
                        .enabled(!busy)
                        .show(ui)
                        .clicked()
                {
                    actions.push(
                        Act::RemoveIdentity {
                            rule: i,
                            delete_file: true,
                        }
                        .into(),
                    );
                }
                if let Some(dir) = r.target.parent()
                    && dir.is_dir()
                    && IconButton::new(Icon::Folder, "Open folder")
                        .compact()
                        .show(ui)
                        .clicked()
                {
                    actions.push(Action::OpenPath(dir.to_path_buf()));
                }
            });
        },
    );
}

/// `identity rule C:/work/` or `global ~/.gitconfig:3`.
fn source(e: Option<&Entry>, matched: &[usize], rep: &IdentityReport) -> String {
    let Some(e) = e else {
        return "not set".to_owned();
    };
    if let Some(f) = &e.file
        && let Some(n) = matched.iter().find(|&&i| {
            rep.rules
                .get(i)
                .is_some_and(|r| gitvis_core::devops::identity::same_path(&r.target, f))
        })
    {
        return format!("{} · rule {}", w::short_place(e), n + 1);
    }
    format!("{} · {}", e.scope.label(), w::short_place(e))
}

fn applies(ui: &mut Ui, rep: &IdentityReport, _actions: &mut Vec<Action>) {
    kit::section(ui, "Which identity applies", Some(rep.repos.len()));
    kit::secondary(
        ui,
        "The name and e-mail git puts on commits in each open repository, and the file they come from (git config --show-origin in each).",
    );
    if rep.repos.is_empty() {
        kit::muted(ui, "No repository is open.");
        return;
    }
    let rows: Vec<Vec<Cell>> = rep
        .repos
        .iter()
        .map(|(name, a)| {
            if let Some(e) = &a.error {
                return vec![
                    Cell::Text(name.clone()),
                    Cell::Tone(e.clone(), Tone::Danger),
                    Cell::Muted(String::new()),
                    Cell::Muted(String::new()),
                    Cell::Muted(String::new()),
                ];
            }
            let v = |e: &Option<Entry>| e.as_ref().map(|e| e.value_text().to_owned());
            vec![
                Cell::Text(name.clone()),
                match v(&a.name) {
                    Some(n) => Cell::Text(n),
                    None => Cell::Tone("not set".to_owned(), Tone::Danger),
                },
                match v(&a.email) {
                    Some(n) => Cell::Mono(n),
                    None => Cell::Tone("not set".to_owned(), Tone::Danger),
                },
                Cell::Sec(source(a.email.as_ref(), &a.matched, rep)),
                match (v(&a.signing_key), v(&a.gpgsign).as_deref() == Some("true")) {
                    (Some(_), true) => insights::badges(&[("Signs commits", Tone::Success)]),
                    (Some(_), false) => {
                        insights::badges(&[("Key set, signing off", Tone::Neutral)])
                    }
                    (None, true) => insights::badges(&[("Signing on, no key", Tone::Warning)]),
                    (None, false) => Cell::Muted("off".to_owned()),
                },
            ]
        })
        .collect();
    insights::grid(
        ui,
        "identity_applies",
        &[
            Col::text("Repository"),
            Col::text("Name"),
            Col::text("E-mail"),
            Col::text("From"),
            Col::text("Signing"),
        ],
        &rows,
        false,
        |_, _| {},
    );
}

fn check_folder(
    ui: &mut Ui,
    path: &mut String,
    check: &super::Loaded<Applied>,
    rep: &IdentityReport,
    actions: &mut Vec<Action>,
) {
    kit::section(ui, "Check a folder", None);
    kit::secondary(
        ui,
        "Any folder: for a repository gitvis asks git; for a folder that is not a repository yet, it evaluates the folder rules for a repository you would create there.",
    );
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        // The Check button, the gap and the input frame.
        let wdt = (ui.available_width() - w::button_width(ui, "Check") - 8.0 - w::INPUT_FRAME)
            .clamp(200.0, 620.0);
        let resp = form::input(
            ui,
            "identity_check_path",
            path,
            InputOpts::new("C:/work/new-project").mono().width(wdt),
        );
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (form::button(ui, "Check", !path.trim().is_empty() && !check.loading).clicked() || enter)
            && !path.trim().is_empty()
        {
            actions.push(Act::CheckPath.into());
        }
    });
    if check.loading {
        kit::loading(ui, "Asking git");
        return;
    }
    let Some(Ok(a)) = &check.data else {
        return;
    };
    ui.add_space(4.0);
    form::card(ui, Tone::Neutral, |ui| {
        if let Some(e) = &a.error {
            form::error(ui, e);
            return;
        }
        let v = |e: &Option<Entry>| e.as_ref().map(|e| e.value_text().to_owned());
        kit::kv(ui, 120.0, |kv| {
            kv.text("Folder", kit::mono(slash(&a.path)));
            kv.text(
                "Repository",
                if a.repo {
                    "Yes: git answered".to_owned()
                } else {
                    if a.path.is_dir() {
                        "No: gitvis evaluated the folder rules for a repository created here".to_owned()
                    } else {
                        "The folder does not exist yet: gitvis evaluated the folder rules for a repository created there".to_owned()
                    }
                },
            );
            match v(&a.name) {
                Some(n) => kv.text("Name", n),
                None => kv.muted("Name", "not set"),
            }
            match v(&a.email) {
                Some(n) => kv.text("E-mail", kit::mono(n)),
                None => kv.muted("E-mail", "not set"),
            }
            kv.muted("From", &source(a.email.as_ref(), &a.matched, rep));
            let pats: Vec<String> = a
                .matched
                .iter()
                .filter_map(|&i| rep.rules.get(i))
                .map(|r| r.condition.raw())
                .collect();
            if pats.is_empty() {
                kv.muted("Matching rules", "none");
            } else {
                kv.text("Matching rules", kit::mono(pats.join(", ")));
            }
            match v(&a.signing_key) {
                Some(k) => kv.text("Signing key", kit::mono(k)),
                None => kv.muted("Signing key", "none"),
            }
        });
    });
}

fn identity_form(
    ui: &mut Ui,
    f: &mut IdentityForm,
    rep: Option<&IdentityReport>,
    icase_pref: &mut bool,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    let editing = f.editing.is_some();
    let home = rep.and_then(|r| r.home.clone());
    form::card(ui, Tone::Accent, |ui| {
        kit::title(
            ui,
            if editing {
                "Edit the identity"
            } else {
                "Add an identity"
            },
        );
        ui.add_space(4.0);
        form::field(
            ui,
            "Folder",
            Some(
                "Repositories inside this folder use the identity: pick the folder that holds them.",
            ),
            |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    // The Choose… button, the gap and the input frame: the row
                    // used to overflow its card.
                    let wdt = (ui.available_width()
                        - w::button_width(ui, "Choose…")
                        - 8.0
                        - w::INPUT_FRAME)
                        .max(200.0);
                    let before = f.folder.clone();
                    form::input(
                        ui,
                        "identity_form_folder",
                        &mut f.folder,
                        InputOpts::new("C:/work").mono().width(wdt),
                    );
                    if f.folder != before && f.slug_auto && !f.folder.trim().is_empty() {
                        f.slug = includes::slug_for(&f.folder);
                    }
                    if form::button(ui, "Choose…", true).clicked() {
                        actions.push(Act::PickIdentityFolder.into());
                    }
                });
            },
        );
        if form::checkbox(
            ui,
            &mut f.icase,
            "Ignore case when matching the folder (gitdir/i:, recommended on Windows)",
        )
        .changed()
        {
            *icase_pref = f.icase;
        }
        ui.add_space(space::S);
        form::field(ui, "Name", Some("As it appears on commits."), |ui| {
            form::input(
                ui,
                "identity_form_name",
                &mut f.name,
                InputOpts::new("Ada Lovelace").width(420.0),
            );
        });
        form::field(
            ui,
            "E-mail",
            Some("GitHub links a commit to the account that has this address."),
            |ui| {
                form::input(
                    ui,
                    "identity_form_email",
                    &mut f.email,
                    InputOpts::new("ada@work.example").mono().width(420.0),
                );
            },
        );
        form::field(
            ui,
            "Signing key (optional)",
            Some(
                "user.signingkey for these repositories: a GPG key id, or the path to an SSH key.",
            ),
            |ui| {
                form::input(
                    ui,
                    "identity_form_key",
                    &mut f.signing_key,
                    InputOpts::new("ABCDEF0123456789 or C:/Users/me/.ssh/id_work")
                        .mono()
                        .width(420.0),
                );
            },
        );
        if !editing {
            form::field(
                ui,
                "Identity file",
                Some("Written next to your global config."),
                |ui| {
                    // Centred on the input's height, so the fixed prefix sits
                    // on the same line as the typed text.
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), form::INPUT_H),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ui.label(kit::mono(".gitconfig-").color(pal(ui).text_2));
                            if form::input(
                                ui,
                                "identity_form_slug",
                                &mut f.slug,
                                InputOpts::new("work").mono().width(180.0),
                            )
                            .changed()
                            {
                                f.slug_auto = false;
                            }
                        },
                    );
                },
            );
        }
        // Live preview of the two files (pure: nothing is written).
        if let Some(rep) = rep {
            let global = global_config_file().unwrap_or_else(|| PathBuf::from("~/.gitconfig"));
            let new = NewIdentity {
                folder: f.folder.clone(),
                icase: f.icase,
                name: f.name.clone(),
                email: f.email.clone(),
                signing_key: f.signing_key.clone(),
                file: form_file(f),
            };
            let plan = match f.editing.and_then(|i| rep.rules.get(i)) {
                Some(r) => includes::plan_edit(r, &new, &global),
                None => includes::plan_add(&new, &rep.rules, &global, home.as_deref()),
            };
            match plan {
                Ok(p) => {
                    preview(ui, &tilde(&p.file, home.as_deref()), &p.file_text, actions);
                    if !p.global_text.is_empty() {
                        preview(
                            ui,
                            &format!("Added to {}", tilde(&p.global_file, home.as_deref())),
                            &p.global_text,
                            actions,
                        );
                    }
                    if p.file_exists && !editing {
                        kit::callout(
                            ui,
                            Tone::Warning,
                            "The file exists already",
                            "Its name and e-mail are replaced; other settings in it stay.",
                        );
                    }
                }
                Err(e) => {
                    kit::muted(ui, &e);
                }
            }
        }
        form::error(ui, &f.error);
        form::actions_row(ui, |ui| {
            if form::primary(
                ui,
                if editing {
                    "Save…"
                } else {
                    "Add identity…"
                },
                !busy,
            )
            .clicked()
            {
                actions.push(Act::SubmitIdentity.into());
            }
            if form::button(ui, "Cancel", true).clicked() {
                actions.push(Act::CloseIdentityForm.into());
            }
        });
    });
}

fn preview(ui: &mut Ui, title: &str, text: &str, actions: &mut Vec<Action>) {
    ui.add_space(2.0);
    w::label(ui, title);
    kit::code_block(ui, text.trim_end(), actions);
}
