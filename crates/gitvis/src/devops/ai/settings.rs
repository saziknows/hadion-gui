//! Settings → AI assist: the API key (masked input, keychain), the model,
//! effort per feature, what is sent and how much, and this session's usage.

use egui::{RichText, Ui};
use gitvis_core::devops::ai::runner::{self, KeySource};
use gitvis_core::devops::ai::{Effort, Feature, Model, dollars};

use super::session::{self, KeyState};
use super::{Act, BUDGETS_KB, Secret};
use crate::state::{Action, RepoState, Settings};
use crate::theme::{Tone, pal, semibold, size, space};
use crate::views::icons::{Icon, IconButton};
use crate::views::{form, kit};

/// The keychain's name on this platform.
fn keychain_name() -> &'static str {
    if cfg!(windows) {
        "the Windows Credential Manager"
    } else if cfg!(target_os = "macos") {
        "the macOS Keychain"
    } else {
        "the Secret Service keyring"
    }
}

pub fn show(
    ui: &mut Ui,
    settings: &mut Settings,
    repo: Option<&RepoState>,
    actions: &mut Vec<Action>,
) {
    kit::page_header(
        ui,
        "AI assist",
        "Claude writes commit messages and pull request descriptions, explains CI failures and commits, and summarises pull requests",
        |_| {},
    );
    // First visit: look the key up (in the background).
    if repo.is_some() && super::key_unknown() {
        actions.push(Act::CheckKey.into());
    }
    kit::scroll_page(ui, "settings_ai", |ui| {
        ui.set_max_width(kit::PROSE);
        key_card(ui, repo.is_some(), actions);
        ui.add_space(space::L);
        model_card(ui, settings);
        ui.add_space(space::L);
        effort_card(ui, settings);
        ui.add_space(space::L);
        privacy_card(ui, settings);
        ui.add_space(space::L);
        usage_section(ui);
        ui.add_space(space::L);
        what_is_sent(ui);
    });
}

fn key_card(ui: &mut Ui, has_repo: bool, actions: &mut Vec<Action>) {
    let (state, busy, error, curl) = session::with(|s| {
        (
            s.key.clone(),
            s.key_busy,
            s.key_error.clone(),
            s.curl.clone(),
        )
    });
    kit::section(ui, "API key", None);
    let tone = match &state {
        KeyState::Known(k) if k.source().is_none() => Tone::Warning,
        _ => Tone::Neutral,
    };
    form::card(ui, tone, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            match &state {
                KeyState::Unknown | KeyState::Checking => {
                    kit::loading(ui, "Looking up the key…");
                }
                KeyState::Known(k) => {
                    match k.source() {
                        Some(KeySource::Keychain) => {
                            kit::badge(ui, "Stored in the keychain", Tone::Success);
                        }
                        Some(KeySource::Env) => {
                            kit::badge(ui, "From ANTHROPIC_API_KEY", Tone::Accent);
                        }
                        None => {
                            kit::badge(ui, "No key", Tone::Warning);
                        }
                    }
                    if let Some(h) = k.hint() {
                        ui.label(
                            RichText::new(format!("key ending {h}"))
                                .size(size::SMALL)
                                .color(pal(ui).text_2),
                        );
                    }
                }
            }
            if let Some(b) = busy {
                kit::loading(ui, b);
            }
        });
        if let KeyState::Known(k) = &state {
            let text = match k.source() {
                Some(KeySource::Keychain) => format!(
                    "Requests use the key stored in {} (service gitvis, entry {}).",
                    keychain_name(),
                    runner::key_name()
                ),
                Some(KeySource::Env) => {
                    "No key is stored, so requests use ANTHROPIC_API_KEY from the environment gitvis was started in. Store a key below to use it instead.".to_owned()
                }
                None => {
                    "AI requests need a Claude API key. Create one in the Claude Console (API keys), then paste it here; or set ANTHROPIC_API_KEY before starting gitvis.".to_owned()
                }
            };
            kit::body(ui, &text);
            if k.keychain.is_some()
                && let Some(env) = &k.env
            {
                kit::secondary(
                    ui,
                    &format!("ANTHROPIC_API_KEY is also set (ending {env}); the stored key wins."),
                );
            }
            if let Some(e) = &k.keychain_error {
                form::error(ui, &format!("The keychain could not be read: {e}"));
            }
        }
        ui.add_space(space::XS);
        let id = ui.make_persistent_id("ai_key_buffer");
        let mut buf: String = ui.data_mut(|d| d.get_temp::<String>(id).unwrap_or_default());
        form::field(
            ui,
            "New key",
            Some(
                "Saved in the OS keychain; never in gitvis's settings, logs or a command line. The field is cleared when you save.",
            ),
            |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let w = (ui.available_width() - 190.0).max(160.0);
                    form::input(
                        ui,
                        "ai_key_input",
                        &mut buf,
                        form::InputOpts::new("sk-ant-…").password().width(w),
                    );
                    let can_save = has_repo && busy.is_none() && !buf.trim().is_empty();
                    if form::primary(ui, "Save", can_save).clicked() {
                        actions.push(Act::SaveKey(Secret(std::mem::take(&mut buf))).into());
                    }
                    let stored = matches!(&state, KeyState::Known(k) if k.keychain.is_some());
                    if form::danger(ui, "Remove", stored && busy.is_none()).clicked() {
                        actions.push(Act::RemoveKey.into());
                    }
                });
            },
        );
        ui.data_mut(|d| {
            if buf.is_empty() {
                d.remove::<String>(id);
            } else {
                d.insert_temp(id, buf);
            }
        });
        if let Some(e) = &error {
            form::error(ui, e);
        }
        if !has_repo {
            kit::secondary(
                ui,
                "Open a repository to store or check the key: keychain work runs in a repository's background queue.",
            );
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if IconButton::new(Icon::Rerun, "Check again")
                .compact()
                .enabled(has_repo && !matches!(state, KeyState::Checking))
                .show(ui)
                .clicked()
            {
                actions.push(Act::CheckKey.into());
            }
            match &curl {
                Some(Some(path)) => kit::secondary(ui, &format!("curl: {}", path.display())),
                Some(None) => kit::secondary(ui, &format!("curl not found. {}", runner::CURL_HINT)),
                None => kit::secondary(ui, "curl: not checked yet"),
            };
        });
    });
}

fn model_card(ui: &mut Ui, settings: &mut Settings) {
    kit::section(ui, "Model", None);
    let override_model = session::with(|s| s.model_override);
    form::card(ui, Tone::Neutral, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        for m in Model::ALL {
            if form::radio(ui, settings.ext.ai.model == m, m.label()) {
                settings.ext.ai.model = m;
                session::with(|s| s.model_override = None);
            }
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                kit::secondary(ui, &format!("{} · {}", m.id(), m.note()));
            });
            ui.add_space(4.0);
        }
        if let Some(m) = override_model {
            kit::callout(
                ui,
                Tone::Neutral,
                "Overridden for this session",
                &format!(
                    "The test script chose {} (ai-model); it is not saved. Pick a model above to clear it.",
                    m.label()
                ),
            );
        }
        kit::secondary(
            ui,
            "Prices are per million tokens (input / output). Cache reads are $0.20 on Opus and Sonnet; cache writes cost 1.25 times the input price. Costs shown in gitvis are estimates from list prices.",
        );
    });
}

fn effort_card(ui: &mut Ui, settings: &mut Settings) {
    kit::section(ui, "Effort per feature", None);
    form::card(ui, Tone::Neutral, |ui| {
        kit::secondary(
            ui,
            "How long Claude thinks before answering. Higher effort takes longer and uses more output tokens.",
        );
        ui.add_space(space::XS);
        let options: Vec<(Effort, &str)> = Effort::ALL.iter().map(|e| (*e, e.label())).collect();
        for f in Feature::ALL {
            form::row(ui, 230.0, f.label(), |ui| {
                let cur = settings.ext.ai.effort(f);
                if let Some(e) = kit::segmented(ui, &options, cur) {
                    *settings.ext.ai.effort_mut(f) = e;
                }
                if cur == f.default_effort() {
                    kit::muted(ui, "default");
                }
            });
        }
    });
}

fn privacy_card(ui: &mut Ui, settings: &mut Settings) {
    kit::section(ui, "What is sent", None);
    let ask_override = session::with(|s| s.ask_override);
    form::card(ui, Tone::Neutral, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        let mut ask = settings.ext.ai.ask;
        if form::checkbox(ui, &mut ask, "Ask before sending").clicked() {
            settings.ext.ai.ask = ask;
            session::with(|s| s.ask_override = None);
        }
        kit::secondary(
            ui,
            "Shows the model, the effort, the size, the files or log lines included, how many values were redacted and the start of the message, and waits for Send.",
        );
        if let Some(a) = ask_override {
            kit::secondary(
                ui,
                &format!(
                    "Overridden for this session by the test script: {}.",
                    if a { "ask" } else { "do not ask" }
                ),
            );
        }
        ui.add_space(space::S);
        let options: Vec<(u32, String)> =
            BUDGETS_KB.iter().map(|k| (*k, format!("{k} KB"))).collect();
        let opts: Vec<(u32, &str)> = options.iter().map(|(k, s)| (*k, s.as_str())).collect();
        form::row(ui, 160.0, "Diffs, at most", |ui| {
            if let Some(v) = kit::segmented(ui, &opts, settings.ext.ai.diff_kb) {
                settings.ext.ai.diff_kb = v;
            }
        });
        form::row(ui, 160.0, "CI logs, at most", |ui| {
            if let Some(v) = kit::segmented(ui, &opts, settings.ext.ai.log_kb) {
                settings.ext.ai.log_kb = v;
            }
        });
        kit::secondary(
            ui,
            "Larger inputs are cut file by file (each file at most a third of the budget); lock files and what does not fit go in as a stat line. Logs are sent as the error lines with 40 lines around them, newest first.",
        );
        ui.add_space(space::S);
        kit::callout(
            ui,
            Tone::Neutral,
            "Secrets are removed first",
            "Before anything leaves this machine, gitvis's secret scanner reads the whole payload: every value it finds is replaced with [REDACTED:rule], private-key blocks go entirely, and files with sensitive names (.env, id_rsa, …) are left out of diffs. Requests run only when you click.",
        );
    });
}

fn usage_section(ui: &mut Ui) {
    let rows = session::with(|s| s.usage.clone());
    let (n, failed, total, cost) = session::totals();
    kit::section(ui, "This session", None);
    if session::with(|s| std::mem::take(&mut s.scroll_to_usage)) {
        ui.scroll_to_cursor(Some(egui::Align::TOP));
    }
    if rows.is_empty() {
        kit::muted(
            ui,
            "No requests yet. Usage and estimated cost appear here after the first answer; nothing is kept after gitvis closes.",
        );
        return;
    }
    kit::stats(
        ui,
        &[
            kit::Stat::new("Requests", crate::format::count(n as usize)),
            kit::Stat::new("Failed", crate::format::count(failed as usize))
                .tone_if(failed > 0, Tone::Warning),
            kit::Stat::new(
                "Input tokens",
                crate::format::count(total.input_tokens as usize),
            ),
            kit::Stat::new(
                "Output tokens",
                crate::format::count(total.output_tokens as usize),
            ),
            kit::Stat::new(
                "Cache read",
                crate::format::count(total.cache_read_input_tokens as usize),
            ),
            kit::Stat::new("Estimated cost", dollars(cost)),
        ],
    );
    let cols = [
        kit::Col::text("Model"),
        kit::Col::text("Feature"),
        kit::Col::num("Requests"),
        kit::Col::num("Input"),
        kit::Col::num("Output"),
        kit::Col::num("Cache write"),
        kit::Col::num("Cache read"),
        kit::Col::num("Cost"),
    ];
    let table: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![
                Model::label_of(&r.model),
                r.feature.label().to_owned(),
                if r.failed > 0 {
                    format!("{} ({} failed)", r.requests, r.failed)
                } else {
                    r.requests.to_string()
                },
                crate::format::count(r.usage.input_tokens as usize),
                crate::format::count(r.usage.output_tokens as usize),
                crate::format::count(r.usage.cache_creation_input_tokens as usize),
                crate::format::count(r.usage.cache_read_input_tokens as usize),
                dollars(r.cost),
            ]
        })
        .collect();
    kit::table(ui, &cols, &table);
}

fn what_is_sent(ui: &mut Ui) {
    kit::section(ui, "What each feature sends", None);
    let p = *pal(ui);
    let rows: [(&str, &str); 5] = [
        (
            Feature::CommitMessage.action(),
            "The staged diff (within the budget), the diff stat, the staged file names, the last 20 commit subjects and the branch name.",
        ),
        (
            Feature::PrDescription.action(),
            "The commits between the branches (subjects and bodies), the diff stat, the chosen template and the branch names.",
        ),
        (
            Feature::ExplainFailure.action(),
            "The log's error lines with 40 lines of context and its last 30 lines, the step names and results, the annotations, the workflow file's header and this job, the commit subject.",
        ),
        (
            Feature::SummarisePr.action(),
            "The title, author, branches, description, the diff (within the budget) and up to 30 loaded review threads.",
        ),
        (
            Feature::ExplainCommit.action(),
            "The commit's hash, author, date, message, stat and diff (within the budget).",
        ),
    ];
    for (what, detail) in rows {
        ui.add_space(space::S);
        ui.label(
            RichText::new(what)
                .size(size::BODY)
                .family(semibold())
                .color(p.text_strong),
        );
        kit::secondary(ui, detail);
    }
}
