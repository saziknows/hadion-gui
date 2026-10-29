//! Settings → AI assist: how gitvis reaches Claude (Auto, the owner's
//! Claude Code, or the Claude API), Claude Code's installation and sign-in,
//! the API key (masked input, keychain), model and effort per provider,
//! what is sent and how much, and this session's usage.

use chrono::{Local, TimeZone};
use egui::{Align, RichText, Ui};
use gitvis_core::devops::ai::cli::{self, PlanUsage};
use gitvis_core::devops::ai::runner::{self, KeySource};
use gitvis_core::devops::ai::{CliModel, Effort, Feature, Model, Provider, dollars};

use super::session::{self, CliState, KeyState};
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

/// Scroll to this part when a script asked for it (`ai-settings cli`).
/// Scroll to `part` when a script asked for it. It keeps doing so for a
/// moment, because the sections above still grow while the checks the page
/// starts come back; scrolling or clicking ends it.
fn scroll_here(ui: &mut Ui, part: &str) {
    let user = ui.input(|i| i.is_scrolling() || i.pointer.any_pressed());
    let wanted = session::with(|s| match &s.scroll_to {
        Some((p, at)) if p == part => {
            let keep = !user && at.elapsed() < std::time::Duration::from_millis(2500);
            if !keep {
                s.scroll_to = None;
            }
            keep
        }
        _ => false,
    });
    if wanted {
        ui.scroll_to_cursor(Some(Align::TOP));
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
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
    // First visit: look up the key and Claude Code (in the background).
    if repo.is_some() {
        if super::key_unknown() {
            actions.push(Act::CheckKey.into());
        }
        if super::cli_unknown() {
            actions.push(Act::CheckCli.into());
        }
    }
    let has_repo = repo.is_some();
    kit::scroll_page(ui, "settings_ai", |ui| {
        // Never wider than the page: `set_max_width` alone would widen a
        // narrow window's page past its edge.
        ui.set_max_width(ui.available_width().min(kit::PROSE));
        provider_card(ui, settings);
        claude_code_section(ui, settings, has_repo, actions);
        api_section(ui, settings, has_repo, actions);
        privacy_card(ui, settings);
        usage_section(ui);
        what_is_sent(ui);
    });
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

fn provider_card(ui: &mut Ui, settings: &mut Settings) {
    kit::section(ui, "How gitvis reaches Claude", None);
    let (cli_state, key, over) =
        session::with(|s| (s.cli.clone(), s.key.clone(), s.provider_override));
    form::card(ui, Tone::Neutral, |ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        let current = over.unwrap_or(settings.ext.ai.provider);
        let options: Vec<(Provider, &str)> =
            Provider::ALL.iter().map(|p| (*p, p.label())).collect();
        if let Some(p) = kit::segmented(ui, &options, current) {
            settings.ext.ai.provider = p;
            session::with(|s| s.provider_override = None);
        }
        kit::body(
            ui,
            match current {
                Provider::Auto => {
                    "Auto uses your Claude Code when it is signed in with a Claude plan, and otherwise the API key below."
                }
                Provider::ClaudeCode => {
                    "Requests run your own Claude Code CLI with its sign-in: your Claude subscription pays for them."
                }
                Provider::Api => {
                    "Requests go to the Claude API with the key below, billed per token to that API account."
                }
            },
        );
        let cli = cli_state.status();
        let key_ready = matches!(&key, KeyState::Known(k) if k.source().is_some());
        let (tone, title, text) = match (current, cli) {
            (Provider::Api, _) => {
                if key_ready {
                    (
                        Tone::Success,
                        "Ready",
                        "Requests go to the Claude API.".to_owned(),
                    )
                } else {
                    (
                        Tone::Warning,
                        "No API key yet",
                        "Store a key under Claude API below, or set ANTHROPIC_API_KEY.".to_owned(),
                    )
                }
            }
            (_, None) => (Tone::Neutral, "", "Looking for Claude Code…".to_owned()),
            (Provider::ClaudeCode, Some(st)) => cli_verdict(st),
            (Provider::Auto, Some(st)) => {
                if st.plan_ready() {
                    let (_, _, text) = cli_verdict(st);
                    (Tone::Success, "Ready: Claude Code", text)
                } else if key_ready {
                    (
                        Tone::Success,
                        "Ready: Claude API",
                        format!(
                            "{}, so requests go to the Claude API with the stored key.",
                            match (&st.install, &st.auth) {
                                (None, _) => "Claude Code is not installed".to_owned(),
                                (Some(_), Some(a)) if a.bills_api() =>
                                    "Claude Code is signed in with an API key, not a Claude plan"
                                        .to_owned(),
                                _ => "Claude Code is not signed in".to_owned(),
                            }
                        ),
                    )
                } else {
                    (
                        Tone::Warning,
                        "Not set up yet",
                        "Sign in to Claude Code with your Claude account, or store an API key: the sections below say how.".to_owned(),
                    )
                }
            }
        };
        if title.is_empty() {
            kit::loading(ui, &text);
        } else {
            kit::callout(ui, tone, title, &text);
        }
        if let Some(p) = over {
            kit::secondary(
                ui,
                &format!(
                    "The test script chose {} for this session (ai-provider); it is not saved. Pick an option above to clear it.",
                    p.label()
                ),
            );
        }
    });
}

/// Tone, title and one sentence for Claude Code's state.
fn cli_verdict(st: &cli::Status) -> (Tone, &'static str, String) {
    match (&st.install, &st.auth) {
        (None, _) => (
            Tone::Warning,
            "Claude Code is not installed",
            "Install it (see below), or choose the Claude API.".to_owned(),
        ),
        (Some(_), None) => (
            Tone::Warning,
            "Claude Code's sign-in could not be read",
            st.error.clone().unwrap_or_default(),
        ),
        (Some(_), Some(a)) if !a.logged_in => (
            Tone::Warning,
            "Claude Code is not signed in",
            format!("Run `{}` in a terminal, then Check again.", cli::SIGN_IN),
        ),
        (Some(_), Some(a)) if a.bills_api() => (
            Tone::Warning,
            "Claude Code bills an API account",
            format!(
                "It is signed in with {}: requests through it are billed per token, not covered by a Claude plan.",
                a.phrase()
            ),
        ),
        (Some(_), Some(a)) => (
            Tone::Success,
            "Ready",
            format!("Requests go through your Claude Code ({}).", a.label()),
        ),
    }
}

// ---------------------------------------------------------------------------
// Claude Code
// ---------------------------------------------------------------------------

fn percent(f: f64) -> String {
    format!("{:.0}%", (f * 100.0).clamp(0.0, 100.0))
}

fn when(secs: i64) -> String {
    match Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(t) => t.format("%a %H:%M").to_string(),
        _ => String::new(),
    }
}

/// `17% (resets Sat 16:30)`.
fn window(w: Option<(f64, Option<i64>)>) -> Option<String> {
    let (u, reset) = w?;
    Some(match reset {
        Some(r) => format!("{} used, resets {}", percent(u), when(r)),
        None => format!("{} used", percent(u)),
    })
}

fn plan_rows(kv: &mut kit::Kv, plan: &Option<(PlanUsage, std::time::Instant)>) {
    match plan {
        Some((p, at)) => {
            if let Some(w) = window(p.five_hour) {
                kv.text("5-hour limit", w);
            }
            if let Some(w) = window(p.seven_day) {
                kv.text("Weekly limit", w);
            }
            let mut note = format!(
                "as Claude Code reported {} ago",
                crate::format::millis(at.elapsed().as_millis() as u64)
            );
            if p.overage {
                note.push_str("; extra usage is being billed");
            }
            kv.muted("", &note);
        }
        None => kv.muted(
            "Plan usage",
            "shown after the first request through Claude Code",
        ),
    }
}

fn claude_code_section(
    ui: &mut Ui,
    settings: &mut Settings,
    has_repo: bool,
    actions: &mut Vec<Action>,
) {
    let (state, plan, model_over) =
        session::with(|s| (s.cli.clone(), s.plan.clone(), s.cli_model_override));
    scroll_here(ui, "cli");
    kit::section_with(ui, "Claude Code (your subscription)", None, |ui| {
        if IconButton::new(Icon::Rerun, "Check again")
            .compact()
            .enabled(has_repo && !matches!(state, CliState::Checking))
            .show(ui)
            .on_hover_text("Look for Claude Code and read `claude auth status` again")
            .clicked()
        {
            actions.push(Act::CheckCli.into());
        }
    });
    let tone = match state.status() {
        Some(st) if !st.plan_ready() => Tone::Warning,
        _ => Tone::Neutral,
    };
    form::card(ui, tone, |ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        let st = match &state {
            CliState::Unknown | CliState::Checking => {
                kit::loading(ui, "Looking for Claude Code and its sign-in…");
                if !has_repo {
                    kit::secondary(
                        ui,
                        "Open a repository first: the check runs in a repository's background queue.",
                    );
                }
                return;
            }
            CliState::Known(st) => st,
        };
        let Some(install) = &st.install else {
            kit::badge(ui, "Not installed", Tone::Warning);
            kit::body(
                ui,
                "gitvis can use the Claude Code CLI you install and sign in to yourself, so requests use your Claude subscription instead of an API key. Install it with one of these (in PowerShell or a terminal):",
            );
            for (how, cmd) in cli::INSTALL_HINTS {
                ui.label(
                    RichText::new(how)
                        .size(size::SMALL)
                        .family(semibold())
                        .color(pal(ui).text_2),
                );
                kit::code_block(ui, cmd, actions);
            }
            kit::body(ui, "Then sign in with your Claude account, in a terminal:");
            kit::code_block(ui, cli::SIGN_IN, actions);
            terminal_row(ui, actions);
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            match &st.auth {
                Some(a) if a.plan() => {
                    kit::badge(ui, "Uses your plan", Tone::Success);
                }
                Some(a) if a.bills_api() => {
                    kit::badge(ui, "Bills an API account", Tone::Warning);
                }
                Some(_) => {
                    kit::badge(ui, "Not signed in", Tone::Warning);
                }
                None => {
                    kit::badge(ui, "Sign-in unknown", Tone::Warning);
                }
            }
            if st.old() {
                kit::badge(ui, "Update needed", Tone::Warning);
            }
        });
        kit::kv(ui, 120.0, |kv| {
            kv.text("Program", kit::mono(install.program.display().to_string()));
            kv.muted(
                "Found",
                &match &install.shim {
                    Some(shim) => {
                        format!("through {} (the npm shim {})", install.how, shim.display())
                    }
                    None => format!("through {}", install.how),
                },
            );
            kv.text(
                "Version",
                match st.version {
                    Some(v) if st.old() => format!(
                        "{v}: gitvis needs {} or later for clear sign-in errors",
                        cli::MIN_VERSION
                    ),
                    Some(v) => v.to_string(),
                    None => format!("unknown ({})", st.version_text),
                },
            );
            if let Some(a) = &st.auth {
                let mut account = a.label();
                if let Some(e) = &a.email {
                    account.push_str(&format!(" · {e}"));
                }
                kv.text("Signed in", account);
            }
            // Limits belong to a Claude plan; an API sign-in has none.
            if st.auth.as_ref().is_some_and(|a| a.plan()) {
                plan_rows(kv, &plan);
            }
        });
        if let Some(a) = &st.auth {
            if !a.logged_in {
                kit::callout(
                    ui,
                    Tone::Warning,
                    "Sign in to use your plan",
                    "Run the command below in a terminal and follow the browser sign-in; gitvis never runs it for you. Then use Check again.",
                );
                kit::code_block(ui, cli::SIGN_IN, actions);
                terminal_row(ui, actions);
            } else if a.bills_api() {
                kit::callout(
                    ui,
                    Tone::Warning,
                    "This sign-in bills the API, not your plan",
                    &format!(
                        "Claude Code is signed in with {}. Requests through it are billed per token to that API account. To use your Claude subscription, sign in with your Claude account: run `{}` in a terminal.",
                        a.phrase(),
                        cli::SIGN_IN
                    ),
                );
                terminal_row(ui, actions);
            }
        }
        if let Some(e) = &st.error {
            form::error(ui, e);
        }
        if st.old() {
            kit::secondary(
                ui,
                "Update Claude Code in a terminal (`claude update`); gitvis never updates it for you.",
            );
        }
        ui.add_space(space::XS);
        form::row(ui, 120.0, "Model", |ui| {
            let options: Vec<(CliModel, &str)> =
                CliModel::ALL.iter().map(|m| (*m, m.label())).collect();
            let cur = model_over.unwrap_or(settings.ext.ai.cli_model);
            if let Some(m) = kit::segmented(ui, &options, cur) {
                settings.ext.ai.cli_model = m;
                session::with(|s| s.cli_model_override = None);
            }
        });
        kit::secondary(
            ui,
            "Default is the model your Claude Code uses. The others are names, not versions: Claude Code picks the model each name means, and every answer shows which one ran.",
        );
        ui.add_space(space::XS);
        effort_rows(ui, settings, true);
        ui.add_space(space::XS);
        kit::callout(
            ui,
            Tone::Neutral,
            "How gitvis runs it",
            "Your own Claude Code, from an empty folder, with every tool, MCP server, plugin, hook and CLAUDE.md switched off, for one turn, without saving the session. The material goes on standard input. gitvis never reads or stores Claude Code's credentials.",
        );
        let bills_api = st.auth.as_ref().is_some_and(|a| a.bills_api());
        kit::secondary(
            ui,
            if bills_api {
                "Signed in this way, Claude Code bills that API account per token; gitvis shows the cost Claude Code estimates for each request."
            } else {
                "With a Claude plan, requests count against the plan's limits instead of a per-token bill. Claude Code still prints what a request would cost at list prices; gitvis shows that estimate, but your plan covers it."
            },
        );
    });
}

fn terminal_row(ui: &mut Ui, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if IconButton::new(Icon::Terminal, "Open the terminal panel")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::ToggleTerminal);
        }
        kit::muted(ui, "Paste the command there and press Enter yourself.");
    });
}

// ---------------------------------------------------------------------------
// Claude API
// ---------------------------------------------------------------------------

fn api_section(ui: &mut Ui, settings: &mut Settings, has_repo: bool, actions: &mut Vec<Action>) {
    scroll_here(ui, "api");
    kit::section(ui, "Claude API (your key)", None);
    key_card(ui, has_repo, actions);
    ui.add_space(space::M);
    model_card(ui, settings);
    ui.add_space(space::M);
    form::card(ui, Tone::Neutral, |ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        effort_rows(ui, settings, false);
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
                    "API requests use the key stored in {} (service gitvis, entry {}).",
                    keychain_name(),
                    runner::key_name()
                ),
                Some(KeySource::Env) => {
                    "No key is stored, so API requests use ANTHROPIC_API_KEY from the environment gitvis was started in. Store a key below to use it instead.".to_owned()
                }
                None => {
                    "API requests need a Claude API key. Create one in the Claude Console (API keys), then paste it here; or set ANTHROPIC_API_KEY before starting gitvis.".to_owned()
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
            if k.env.is_some() {
                kit::secondary(
                    ui,
                    "gitvis removes ANTHROPIC_API_KEY when it runs Claude Code, so Claude Code keeps using its own sign-in.",
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
    let override_model = session::with(|s| s.model_override);
    form::card(ui, Tone::Neutral, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(
            RichText::new("API model")
                .size(size::SMALL)
                .family(semibold())
                .color(pal(ui).text_2),
        );
        ui.add_space(4.0);
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

/// Effort per feature, for Claude Code (five levels) or the API (three).
fn effort_rows(ui: &mut Ui, settings: &mut Settings, cli: bool) {
    ui.label(
        RichText::new(if cli {
            "Effort per feature (Claude Code)"
        } else {
            "Effort per feature (API)"
        })
        .size(size::SMALL)
        .family(semibold())
        .color(pal(ui).text_2),
    );
    kit::secondary(
        ui,
        "How long Claude thinks before answering. Higher effort takes longer and uses more output tokens.",
    );
    let levels: &[Effort] = if cli { &Effort::CLI } else { &Effort::ALL };
    let options: Vec<(Effort, &str)> = levels.iter().map(|e| (*e, e.short())).collect();
    for f in Feature::ALL {
        form::row(ui, 190.0, f.label(), |ui| {
            let cur = if cli {
                settings.ext.ai.cli_effort(f)
            } else {
                settings.ext.ai.effort(f)
            };
            if let Some(e) = kit::segmented(ui, &options, cur) {
                if cli {
                    *settings.ext.ai.cli_effort_mut(f) = e;
                } else {
                    *settings.ext.ai.effort_mut(f) = e;
                }
            }
            if cur == f.default_effort() {
                kit::muted(ui, "default");
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Both
// ---------------------------------------------------------------------------

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
            "Shows the way it goes (Claude Code or the API), the model, the effort, the size, the files or log lines included, how many values were redacted, the exact command or request and the start of the message, and waits for Send.",
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
            "Before anything leaves this machine, by either way, gitvis's secret scanner reads the whole payload: every value it finds is replaced with [REDACTED:rule], private-key blocks go entirely, and files with sensitive names (.env, id_rsa, …) are left out of diffs. Requests run only when you click.",
        );
    });
}

fn usage_section(ui: &mut Ui) {
    let rows = session::with(|s| s.usage.clone());
    let t = session::totals();
    scroll_here(ui, "usage");
    kit::section(ui, "This session", None);
    if rows.is_empty() {
        kit::muted(
            ui,
            "No requests yet. Tokens appear here after the first answer, with the estimated cost of API requests; nothing is kept after gitvis closes.",
        );
        return;
    }
    let mut tiles = vec![
        kit::Stat::new("Requests", crate::format::count(t.requests as usize)),
        kit::Stat::new("Failed", crate::format::count(t.failed as usize))
            .tone_if(t.failed > 0, Tone::Warning),
        kit::Stat::new(
            "Input tokens",
            crate::format::count(t.usage.input_tokens as usize),
        ),
        kit::Stat::new(
            "Output tokens",
            crate::format::count(t.usage.output_tokens as usize),
        ),
        kit::Stat::new("API cost (estimate)", dollars(t.cost)),
    ];
    if t.cli_requests > 0 {
        tiles.push(kit::Stat::new(
            "On your plan",
            crate::format::count(t.cli_requests as usize),
        ));
    }
    kit::stats(ui, &tiles);
    if t.cli_requests > 0 {
        kit::secondary(
            ui,
            &format!(
                "Your plan covers the requests through Claude Code signed in with your Claude account; Claude Code estimates them at {} at list prices.",
                dollars(t.cli_estimate)
            ),
        );
    }
    let cols = [
        kit::Col::text("Way"),
        kit::Col::text("Model"),
        kit::Col::text("Feature"),
        kit::Col::num("Requests"),
        kit::Col::num("Input"),
        kit::Col::num("Output"),
        kit::Col::num("Cache read"),
        kit::Col::num("Cost"),
    ];
    let table: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![
                match (r.cli, r.billed) {
                    (true, false) => "Claude Code",
                    (true, true) => "Claude Code (API key)",
                    _ => "API",
                }
                .to_owned(),
                Model::label_of(&r.model)
                    .trim_start_matches("Claude ")
                    .to_owned(),
                r.feature.short().to_owned(),
                if r.failed > 0 {
                    format!("{} ({} failed)", r.requests, r.failed)
                } else {
                    r.requests.to_string()
                },
                crate::format::count(r.usage.input_tokens as usize),
                crate::format::count(r.usage.output_tokens as usize),
                crate::format::count(r.usage.cache_read_input_tokens as usize),
                if r.cli && !r.billed {
                    "plan".to_owned()
                } else {
                    dollars(r.cost)
                },
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
