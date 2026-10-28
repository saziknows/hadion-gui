//! Issue-tracker links: the Settings → Integrations section (Jira, Linear,
//! custom patterns, where "Add KEY" puts the key, a tester), the chips the
//! hooks draw (commit panel, pull request, WIP message tools), the full
//! commit messages they read, and the optional title lookups (credentials
//! in the keychain; the lookup sends them on curl's standard input).

use std::path::PathBuf;

use egui::{RichText, Ui};

use gitvis_core::Commit;
use gitvis_core::RefKind;
use gitvis_core::devops::automation::links::{
    self, CustomRule, Link, LinkPrefs, Placement, Source, Tracker,
};
use gitvis_core::devops::automation::lookup::{self, IssueInfo, JIRA_TOKEN, LINEAR_KEY};
use gitvis_core::devops::keychain;

use super::shared::{self, Lookup};
use super::{Act, Msg, Secret};
use crate::devops::ctx::Ctx;
use crate::devops::global::GlobalCx;
use crate::devops::hooks::HookCx;
use crate::format;
use crate::state::{Action, RepoState, Settings, ToastKind};
use crate::theme::{Tone, pal, size, space};
use crate::views::form;
use crate::views::icons::{Icon, IconButton};
use crate::views::kit;

/// UI-only state of the Integrations section.
#[derive(Default)]
pub struct LinksUi {
    /// Masked inputs; cleared as soon as the value is sent to the keychain.
    pub jira_token: String,
    pub linear_key: String,
    /// Whether a value is stored (`None`: not checked yet).
    pub jira_stored: Option<Result<bool, String>>,
    pub linear_stored: Option<Result<bool, String>>,
    /// `Some(path)`: curl was found; `Some(None)`: missing.
    pub curl: Option<Option<PathBuf>>,
    pub checking: bool,
    pub saving: Option<&'static str>,
    pub jira_keys_buf: String,
    pub linear_keys_buf: String,
    /// The "Try it" box.
    pub try_text: String,
}

pub fn apply(cx: &mut Ctx, st: &mut LinksUi, g: &mut GlobalCx, act: Act) {
    match act {
        Act::CheckSecrets => {
            if st.checking {
                return;
            }
            st.checking = true;
            run_local(cx, "Check the keychain for tracker credentials", |log| {
                log.push(format!("keychain: is gitvis/{} stored?", kc(JIRA_TOKEN)));
                log.push(format!("keychain: is gitvis/{} stored?", kc(LINEAR_KEY)));
                let has = |n: &str| {
                    keychain::get(&kc(n))
                        .map(|v| v.is_some())
                        .map_err(|e| e.to_string())
                };
                Msg::Secrets {
                    jira: has(JIRA_TOKEN),
                    linear: has(LINEAR_KEY),
                    curl: gitvis_core::devops::proc::resolve(&lookup::curl_program()),
                }
            });
        }
        Act::SaveSecret { name, value } => {
            if value.expose().trim().is_empty() {
                return;
            }
            st.saving = Some(name);
            let label = format!("Store {name} in the keychain");
            run_local(cx, &label, move |log| {
                log.push(format!(
                    "keychain: set gitvis/{} (value not logged)",
                    kc(name)
                ));
                Msg::SecretSaved {
                    name,
                    stored: keychain::set(&kc(name), value.expose().trim())
                        .map(|_| true)
                        .map_err(|e| e.to_string()),
                }
            });
        }
        Act::RemoveSecret(name) => {
            st.saving = Some(name);
            let label = format!("Remove {name} from the keychain");
            run_local(cx, &label, move |log| {
                log.push(format!("keychain: delete gitvis/{}", kc(name)));
                Msg::SecretSaved {
                    name,
                    stored: keychain::delete(&kc(name))
                        .map(|_| false)
                        .map_err(|e| e.to_string()),
                }
            });
        }
        Act::Lookup(keys) => lookup_keys(cx, g, keys),
        Act::RetryLookups => {
            shared::lock()
                .lookups
                .retain(|_, l| !matches!(l, Lookup::Failed(_)));
        }
        Act::LinksTest(text) => st.try_text = text,
        Act::SetJira { base, keys } => {
            let j = &mut g.settings.ext.automation.links.jira;
            j.base_url = base;
            j.keys = keys;
        }
        Act::SetLinear { workspace, keys } => {
            let l = &mut g.settings.ext.automation.links.linear;
            l.workspace = workspace;
            l.keys = keys;
        }
        Act::AddCustomRule { name, pattern, url } => {
            g.settings
                .ext
                .automation
                .links
                .custom
                .push(CustomRule { name, pattern, url });
        }
        Act::SetLookup(on) => {
            let p = &mut g.settings.ext.automation.links;
            p.jira.lookup = on;
            p.linear.lookup = on;
            if on && p.jira.email.trim().is_empty() {
                p.jira.email = "test@example.com".into();
            }
            shared::lock().lookups.clear();
        }
        Act::TestCredentials(on) => {
            if std::env::var_os("GITVIS_KEYCHAIN_TEST").is_none_or(|v| v.is_empty()) {
                cx.toast(
                    ToastKind::Error,
                    "links-test-token needs GITVIS_KEYCHAIN_TEST=1 (it only writes gitvis-test- entries)",
                );
                return;
            }
            let label = if on {
                "Store fake test credentials"
            } else {
                "Remove the fake test credentials"
            };
            run_local(cx, label, move |log| {
                let mut result = Ok(on);
                for n in [JIRA_TOKEN, LINEAR_KEY] {
                    let name = kc(n);
                    log.push(format!(
                        "keychain: {} gitvis/{name}",
                        if on { "set" } else { "delete" }
                    ));
                    let r = if on {
                        keychain::set(&name, "fake-token-for-ui-tests")
                    } else {
                        keychain::delete(&name)
                    };
                    if let Err(e) = r {
                        result = Err(e.to_string());
                    }
                }
                Msg::SecretSaved {
                    name: JIRA_TOKEN,
                    stored: result,
                }
            });
            shared::lock().lookups.clear();
        }
        _ => {}
    }
}

/// The keychain entry name (a `gitvis-test-` one in UI tests).
fn kc(name: &str) -> String {
    lookup::keychain_name(name)
}

/// A local task (keychain, `curl`, git) logged as a read with its commands.
fn run_local(cx: &mut Ctx, label: &str, f: impl FnOnce(&mut Vec<String>) -> Msg + Send + 'static) {
    let ran = cx.read(label.to_owned(), move |_dir, log| {
        let mut cmds = Vec::new();
        let m = f(&mut cmds);
        log.commands.extend(cmds);
        m
    });
    if !ran {
        cx.toast(ToastKind::Error, "Open a repository first");
    }
}

/// Look up keys that are not cached yet (only for trackers with lookup on).
fn lookup_keys(cx: &mut Ctx, g: &GlobalCx, keys: Vec<(Tracker, String)>) {
    let prefs = g.settings.ext.automation.links.clone();
    let wanted: Vec<(Tracker, String)> = {
        let mut s = shared::lock();
        let mut out: Vec<(Tracker, String)> = Vec::new();
        for k in keys {
            let on = match k.0 {
                Tracker::Jira => prefs.jira.lookup && prefs.jira_on(),
                Tracker::Linear => prefs.linear.lookup && prefs.linear_on(),
                Tracker::Custom(_) => false,
            };
            if on && out.len() < 10 && !s.lookups.contains_key(&k) && !out.contains(&k) {
                s.lookups.insert(k.clone(), Lookup::Loading);
                out.push(k);
            }
        }
        out
    };
    if wanted.is_empty() {
        return;
    }
    let label = format!(
        "Look up {}",
        format::plural(wanted.len(), "issue title", "issue titles")
    );
    let ran = cx.read(label, move |dir, log| {
        let program = lookup::curl_program();
        let mut jira_token: Option<Result<Option<String>, String>> = None;
        let mut linear_key: Option<Result<Option<String>, String>> = None;
        let mut out = Vec::new();
        for (t, key) in wanted {
            let res = match t {
                Tracker::Jira => {
                    let tok = jira_token
                        .get_or_insert_with(|| {
                            keychain::get(&kc(JIRA_TOKEN)).map_err(|e| e.to_string())
                        })
                        .clone();
                    match tok {
                        Ok(Some(tok)) if !prefs.jira.email.trim().is_empty() => {
                            let req = lookup::jira_request(
                                &prefs.jira.base_url,
                                &key,
                                &prefs.jira.email,
                                &tok,
                            );
                            lookup::lookup(t, &req, &program, dir, &mut log.commands)
                                .map_err(|e| e.to_string())
                        }
                        Ok(Some(_)) => {
                            Err("Jira: add the account e-mail in Settings → Integrations".into())
                        }
                        Ok(None) => Err(
                            "Jira: no API token in the keychain (Settings → Integrations)".into(),
                        ),
                        Err(e) => Err(e),
                    }
                }
                Tracker::Linear => {
                    let k = linear_key
                        .get_or_insert_with(|| {
                            keychain::get(&kc(LINEAR_KEY)).map_err(|e| e.to_string())
                        })
                        .clone();
                    match k {
                        Ok(Some(k)) => {
                            let req = lookup::linear_request(&key, &k);
                            lookup::lookup(t, &req, &program, dir, &mut log.commands)
                                .map_err(|e| e.to_string())
                        }
                        Ok(None) => Err(
                            "Linear: no API key in the keychain (Settings → Integrations)".into(),
                        ),
                        Err(e) => Err(e),
                    }
                }
                Tracker::Custom(_) => Err("no lookup for custom patterns".into()),
            };
            out.push((t, key, res));
        }
        Msg::Looked(out)
    });
    if !ran {
        let mut s = shared::lock();
        s.lookups.retain(|_, l| !matches!(l, Lookup::Loading));
    }
}

pub fn on_msg(_cx: &mut Ctx, st: &mut LinksUi, _g: &mut GlobalCx, msg: Msg) {
    match msg {
        Msg::Secrets { jira, linear, curl } => {
            st.checking = false;
            st.jira_stored = Some(jira);
            st.linear_stored = Some(linear);
            st.curl = Some(curl);
        }
        Msg::SecretSaved { name, stored } => {
            st.saving = None;
            let slot = if name == JIRA_TOKEN {
                &mut st.jira_stored
            } else {
                &mut st.linear_stored
            };
            match stored {
                Ok(v) => {
                    *slot = Some(Ok(v));
                    // New credentials: failed lookups may work now.
                    shared::lock()
                        .lookups
                        .retain(|_, l| !matches!(l, Lookup::Failed(_)));
                }
                Err(e) => *slot = Some(Err(e)),
            }
        }
        Msg::Looked(v) => {
            let mut s = shared::lock();
            for (t, k, r) in v {
                s.lookups.insert(
                    (t, k),
                    match r {
                        Ok(i) => Lookup::Found(i),
                        Err(e) => Lookup::Failed(e),
                    },
                );
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Chips (hooks)
// ---------------------------------------------------------------------------

/// `ABC-123 · Login fails · In Progress` buttons that open the issue.
/// Pushes a lookup for keys not looked up yet (when lookups are on).
fn chips(ui: &mut Ui, prefs: &LinkPrefs, found: &[Link], actions: &mut Vec<Action>) {
    let mut missing: Vec<(Tracker, String)> = Vec::new();
    let infos: Vec<Option<Lookup>> = {
        let s = shared::lock();
        found
            .iter()
            .map(|l| s.lookups.get(&(l.tracker, l.key.clone())).cloned())
            .collect()
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        for (l, info) in found.iter().zip(infos) {
            let lookup_on = match l.tracker {
                Tracker::Jira => prefs.jira.lookup,
                Tracker::Linear => prefs.linear.lookup,
                Tracker::Custom(_) => false,
            };
            if lookup_on && info.is_none() {
                missing.push((l.tracker, l.key.clone()));
            }
            let label = match &info {
                Some(Lookup::Found(IssueInfo { title, status, .. })) => {
                    let mut t = format!("{} · {}", l.label, ellipsize(title, 48));
                    if !status.is_empty() {
                        t.push_str(&format!(" · {status}"));
                    }
                    t
                }
                Some(Lookup::Loading) => format!("{} · looking up…", l.label),
                _ => l.label.clone(),
            };
            let mut tip = format!("{} · {}", l.tracker.label(prefs), l.url);
            if let Some(Lookup::Failed(e)) = &info {
                tip.push_str(&format!("\nLookup failed: {e}"));
            }
            if let Some(Lookup::Found(i)) = &info
                && i.title.chars().count() > 48
            {
                tip.push_str(&format!("\n{}", i.title));
            }
            if IconButton::new(Icon::Link, &label)
                .compact()
                .show(ui)
                .on_hover_text(tip)
                .clicked()
            {
                actions.push(Action::OpenUrl(l.url.clone()));
            }
        }
    });
    if !missing.is_empty() {
        actions.push(Act::Lookup(missing).into());
    }
}

fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

fn caption(ui: &mut Ui, text: &str) {
    let p = *pal(ui);
    ui.label(kit::caption_text(&p, text));
}

/// Commit panel: keys in the full message and the branches at the commit.
pub fn hook_commit_panel(ui: &mut Ui, h: &HookCx, c: &Commit, actions: &mut Vec<Action>) {
    let prefs = &h.settings.ext.automation.links;
    if !prefs.any() {
        return;
    }
    // The full message once the panel's details have loaded.
    let message = h
        .details
        .get(&c.id)
        .map_or_else(|| c.summary.clone(), |d| d.message.clone());
    let branches: Vec<String> = h
        .snap
        .refs
        .get(&c.id)
        .map(|v| {
            v.iter()
                .filter(|r| r.kind != RefKind::Tag)
                .map(|r| r.name.clone())
                .collect()
        })
        .unwrap_or_default();
    let mut texts: Vec<(&str, Source)> = vec![(message.as_str(), Source::Text)];
    texts.extend(branches.iter().map(|b| (b.as_str(), Source::Branch)));
    let found = links::find_all(&texts, prefs);
    if found.is_empty() {
        return;
    }
    ui.add_space(space::S);
    caption(ui, &format!("Issues · {}", found.len()));
    ui.add_space(2.0);
    chips(ui, prefs, &found, actions);
}

/// Pull request conversation: keys in the title, body and head branch.
pub fn hook_pr(ui: &mut Ui, h: &HookCx, number: u64, actions: &mut Vec<Action>) {
    let prefs = &h.settings.ext.automation.links;
    if !prefs.any() {
        return;
    }
    let (title, body, head) = match h
        .devops
        .review
        .prs
        .get(&number)
        .and_then(|v| v.detail.data.as_ref())
    {
        Some(d) => (d.pr.title.clone(), d.body.clone(), d.pr.head.clone()),
        None => match h
            .github_data
            .and_then(|g| g.prs.iter().find(|p| p.number == number))
        {
            Some(p) => (p.title.clone(), p.body.clone(), p.head.clone()),
            None => return,
        },
    };
    let found = links::find_all(
        &[
            (title.as_str(), Source::Text),
            (head.as_str(), Source::Branch),
            (body.as_str(), Source::Text),
        ],
        prefs,
    );
    if found.is_empty() {
        return;
    }
    ui.add_space(space::XS);
    caption(ui, &format!("Linked issues · {}", found.len()));
    ui.add_space(2.0);
    chips(ui, prefs, &found, actions);
    ui.add_space(space::S);
}

/// WIP panel: "Add ABC-123" for keys in the branch name.
pub fn hook_wip(ui: &mut Ui, h: &HookCx, actions: &mut Vec<Action>) {
    let prefs = &h.settings.ext.automation.links;
    if !prefs.any() {
        return;
    }
    let Some(branch) = h.snap.head.branch() else {
        return;
    };
    // Keys the message already mentions need no button.
    let message = h.commit_message.to_ascii_uppercase();
    let found: Vec<Link> = links::find(branch, prefs, Source::Branch)
        .into_iter()
        .filter(|l| !message.contains(&l.key.to_ascii_uppercase()))
        .collect();
    if found.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for l in found.iter().take(3) {
            let (text, at_start) = links::message_insert(&l.key, prefs.placement);
            let where_ = match prefs.placement {
                Placement::Prefix => format!("Start the message with \"{text}\""),
                Placement::Suffix => format!("End the message with a line \"{}\"", l.key),
            };
            if IconButton::new(Icon::Plus, &format!("Add {}", l.key))
                .compact()
                .show(ui)
                .on_hover_text(format!(
                    "{where_} (from the branch name {branch}). {} · {}",
                    l.tracker.label(prefs),
                    l.url
                ))
                .clicked()
            {
                actions.push(Action::AddToCommitMessage { text, at_start });
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Settings → Integrations
// ---------------------------------------------------------------------------

fn stored_line(ui: &mut Ui, stored: &Option<Result<bool, String>>, name: &str) {
    let name = kc(name);
    match stored {
        None => kit::loading(ui, "Checking the keychain…"),
        Some(Ok(true)) => {
            ui.horizontal(|ui| {
                kit::badge(ui, "Stored", Tone::Success);
                kit::secondary(
                    ui,
                    &format!(
                        "In the OS keychain as gitvis / {name}; never in a settings file or a log."
                    ),
                );
            });
        }
        Some(Ok(false)) => {
            ui.horizontal(|ui| {
                kit::badge(ui, "Not stored", Tone::Neutral);
                kit::secondary(ui, "Paste it below; it goes straight to the OS keychain.");
            });
        }
        Some(Err(e)) => form::error(ui, &format!("Keychain: {e}")),
    }
}

/// What a masked credential input shows.
struct SecretField<'a> {
    id: &'a str,
    name: &'static str,
    stored: bool,
    saving: bool,
    hint: &'a str,
}

/// A masked input with Save / Remove; the buffer is cleared once sent.
fn secret_input(ui: &mut Ui, f: SecretField, buf: &mut String, actions: &mut Vec<Action>) {
    let SecretField {
        id,
        name,
        stored,
        saving,
        hint,
    } = f;
    ui.horizontal(|ui| {
        let w = (ui.available_width() - 190.0).max(160.0);
        let resp = form::input(
            ui,
            id,
            buf,
            form::InputOpts::new(hint).password().width(w).mono(),
        );
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (form::primary(ui, "Save", !buf.trim().is_empty() && !saving).clicked() || enter)
            && !buf.trim().is_empty()
        {
            actions.push(
                Act::SaveSecret {
                    name,
                    value: Secret::new(std::mem::take(buf)),
                }
                .into(),
            );
        }
        if stored && form::button(ui, "Remove", !saving).clicked() {
            actions.push(Act::RemoveSecret(name).into());
        }
        if saving {
            ui.add(egui::Spinner::new().size(14.0));
        }
    });
}

fn curl_line(ui: &mut Ui, st: &LinksUi, actions: &mut Vec<Action>) {
    match &st.curl {
        None => {}
        Some(Some(p)) => {
            kit::secondary(
                ui,
                &format!(
                    "Lookups run curl --config - ({}) with the credential on standard input, never on the command line.",
                    p.display()
                ),
            );
        }
        Some(None) => {
            kit::callout_with(
                ui,
                Tone::Warning,
                "curl is not installed",
                "Lookups call Jira and Linear through curl. Windows 10 and later include it in C:\\Windows\\System32; otherwise install it from https://curl.se/download.html (winget install cURL.cURL).",
                |ui| {
                    if IconButton::new(Icon::Rerun, "Check again")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Act::CheckSecrets.into());
                    }
                },
            );
        }
    }
}

pub fn settings(
    ui: &mut Ui,
    st: &mut LinksUi,
    settings: &mut Settings,
    repo: Option<&RepoState>,
    actions: &mut Vec<Action>,
) {
    kit::page_header(
        ui,
        "Integrations",
        "Jira, Linear and other issue trackers: links for keys in commits, branches and pull requests",
        |_| {},
    );
    // Key suggestions from the active repository's recent history.
    let suggestions: Vec<(String, usize)> = repo
        .and_then(|r| r.snapshot.as_ref())
        .map(|s| {
            let mut texts: Vec<&str> = s
                .commits
                .iter()
                .take(3_000)
                .map(|c| c.summary.as_str())
                .collect();
            texts.extend(s.branches.iter().map(|b| b.name.as_str()));
            let p = &settings.ext.automation.links;
            let known: Vec<String> = p.jira.keys.iter().chain(&p.linear.keys).cloned().collect();
            links::suggest_keys(texts, &known, 8)
        })
        .unwrap_or_default();
    let repo_name = repo.map(|r| r.name.clone()).unwrap_or_default();
    kit::scroll_page(ui, "automation_integrations", |ui| {
        ui.set_max_width(ui.available_width().min(kit::PROSE));
        let p = &mut settings.ext.automation.links;
        let (_, bad_rules) = links::compile_custom(&p.custom);
        kit::body(
            ui,
            "Issue keys such as ABC-123 become links to your tracker:",
        );
        kit::kv(ui, 128.0, |kv| {
            kv.text(
                "Commit panel",
                "in the message and the branches at the commit",
            );
            kv.text("Pull requests", "in the title, the body and the branch");
            kv.text(
                "Commit message",
                "\"Add KEY\" above it when the branch name has a key the message lacks",
            );
            kv.text(
                "Lookups",
                "titles and status; off by default, credentials stay in the OS keychain",
            );
        });

        // Jira.
        kit::section(ui, "Jira", None);
        form::card(ui, Tone::Neutral, |ui| {
            form::field(ui, "Site", Some("Keys link to SITE/browse/KEY."), |ui| {
                form::input(
                    ui,
                    "automation_jira_site",
                    &mut p.jira.base_url,
                    form::InputOpts::new("https://acme.atlassian.net").mono(),
                )
            });
            form::field(
                ui,
                "Project keys",
                Some(
                    "Only these prefixes become links (ABC for ABC-123). Empty: any upper-case KEY-123 that is not an acronym like UTF-8 or SHA-256.",
                ),
                |ui| {
                    let s: Vec<String> = suggestions.iter().map(|(k, _)| k.clone()).collect();
                    form::tags(
                        ui,
                        "automation_jira_keys",
                        &mut p.jira.keys,
                        &mut st.jira_keys_buf,
                        "ABC, then Enter",
                        &s,
                    );
                },
            );
            if !suggestions.is_empty() {
                suggest_row(ui, &suggestions, &repo_name, &mut p.jira.keys);
            }
            ui.add_space(space::XS);
            form::checkbox(ui, &mut p.jira.lookup, "Look up titles and status");
            kit::secondary(
                ui,
                "Off by default. When on, gitvis asks Jira for the title and status of keys it shows (GET /rest/api/3/issue/KEY), with your e-mail and an API token.",
            );
            if p.jira.lookup {
                ui.add_space(space::S);
                form::field(ui, "Account e-mail", None, |ui| {
                    form::input(
                        ui,
                        "automation_jira_email",
                        &mut p.jira.email,
                        form::InputOpts::new("you@example.com"),
                    )
                });
                form::field(
                    ui,
                    "API token",
                    Some("Create one at id.atlassian.com → Security → API tokens."),
                    |ui| {
                        stored_line(ui, &st.jira_stored, JIRA_TOKEN);
                        ui.add_space(4.0);
                        let stored = matches!(st.jira_stored, Some(Ok(true)));
                        let f = SecretField {
                            id: "automation_jira_token",
                            name: JIRA_TOKEN,
                            stored,
                            saving: st.saving == Some(JIRA_TOKEN),
                            hint: if stored {
                                "Paste a new token to replace it"
                            } else {
                                "Paste the API token"
                            },
                        };
                        secret_input(ui, f, &mut st.jira_token, actions);
                    },
                );
                curl_line(ui, st, actions);
            }
        });

        // Linear.
        kit::section(ui, "Linear", None);
        form::card(ui, Tone::Neutral, |ui| {
            form::field(
                ui,
                "Workspace",
                Some(
                    "The slug in https://linear.app/SLUG/…; keys link to linear.app/SLUG/issue/KEY.",
                ),
                |ui| {
                    form::input(
                        ui,
                        "automation_linear_ws",
                        &mut p.linear.workspace,
                        form::InputOpts::new("acme").mono(),
                    )
                },
            );
            form::field(
                ui,
                "Team keys",
                Some(
                    "Only these prefixes go to Linear (ENG for ENG-42). With Jira set up too, give Linear its keys.",
                ),
                |ui| {
                    let s: Vec<String> = suggestions.iter().map(|(k, _)| k.clone()).collect();
                    form::tags(
                        ui,
                        "automation_linear_keys",
                        &mut p.linear.keys,
                        &mut st.linear_keys_buf,
                        "ENG, then Enter",
                        &s,
                    );
                },
            );
            ui.add_space(space::XS);
            form::checkbox(ui, &mut p.linear.lookup, "Look up titles and status");
            kit::secondary(
                ui,
                "Off by default. When on, gitvis asks Linear's GraphQL API for the title and state of keys it shows, with a personal API key.",
            );
            if p.linear.lookup {
                ui.add_space(space::S);
                form::field(
                    ui,
                    "API key",
                    Some("Linear → Settings → Security & access → Personal API keys."),
                    |ui| {
                        stored_line(ui, &st.linear_stored, LINEAR_KEY);
                        ui.add_space(4.0);
                        let stored = matches!(st.linear_stored, Some(Ok(true)));
                        let f = SecretField {
                            id: "automation_linear_key",
                            name: LINEAR_KEY,
                            stored,
                            saving: st.saving == Some(LINEAR_KEY),
                            hint: if stored {
                                "Paste a new key to replace it"
                            } else {
                                "lin_api_…"
                            },
                        };
                        secret_input(ui, f, &mut st.linear_key, actions);
                    },
                );
                curl_line(ui, st, actions);
            }
        });

        // Custom.
        kit::section_with(ui, "Custom patterns", Some(p.custom.len()), |ui| {
            if IconButton::new(Icon::Plus, "Add a pattern")
                .compact()
                .show(ui)
                .clicked()
            {
                p.custom.push(CustomRule {
                    name: String::new(),
                    pattern: String::new(),
                    url: String::new(),
                });
            }
        });
        if p.custom.is_empty() {
            kit::secondary(
                ui,
                "Any other tracker: a regular expression and a URL. {0} is the whole match, {1}…{9} its groups, {key} group 1. Example: bug #?(\\d+) → https://bugs.example.com/show_bug.cgi?id={1}.",
            );
        }
        let mut remove = None;
        for (i, r) in p.custom.iter_mut().enumerate() {
            form::card(ui, Tone::Neutral, |ui| {
                ui.horizontal(|ui| {
                    let w = (ui.available_width() - 40.0).max(120.0);
                    form::input(
                        ui,
                        ("automation_custom_name", i),
                        &mut r.name,
                        form::InputOpts::new("Name (Bugzilla)").width(w),
                    );
                    if IconButton::icon(Icon::Trash)
                        .compact()
                        .show(ui)
                        .on_hover_text("Remove this pattern")
                        .clicked()
                    {
                        remove = Some(i);
                    }
                });
                ui.add_space(4.0);
                let bad = bad_rules
                    .iter()
                    .find(|(j, _)| *j == i)
                    .map(|(_, e)| e.clone());
                form::input(
                    ui,
                    ("automation_custom_re", i),
                    &mut r.pattern,
                    form::InputOpts::new("Regular expression, e.g. bug #?(\\d+)")
                        .mono()
                        .invalid(bad.is_some()),
                );
                if let Some(e) = bad {
                    form::error(ui, &e);
                }
                ui.add_space(4.0);
                form::input(
                    ui,
                    ("automation_custom_url", i),
                    &mut r.url,
                    form::InputOpts::new("URL template, e.g. https://bugs.example.com/{1}").mono(),
                );
            });
            ui.add_space(space::XS);
        }
        if let Some(i) = remove {
            p.custom.remove(i);
        }

        // Placement.
        kit::section(ui, "Adding a key to the commit message", None);
        form::card(ui, Tone::Neutral, |ui| {
            kit::secondary(
                ui,
                "When the branch name has a key the message does not, the commit panel offers \"Add KEY\".",
            );
            ui.add_space(4.0);
            if form::radio(
                ui,
                p.placement == Placement::Prefix,
                "At the start: \"ABC-123: Fix the login page\"",
            ) {
                p.placement = Placement::Prefix;
            }
            if form::radio(
                ui,
                p.placement == Placement::Suffix,
                "On a last line: \"ABC-123\"",
            ) {
                p.placement = Placement::Suffix;
            }
        });

        // Try it.
        kit::section(ui, "Try it", None);
        form::card(ui, Tone::Neutral, |ui| {
            form::input(
                ui,
                "automation_links_try",
                &mut st.try_text,
                form::InputOpts::new("Paste a commit message or a branch name").rows(3),
            );
            let text = st.try_text.trim().to_owned();
            if text.is_empty() {
                return;
            }
            ui.add_space(space::S);
            let found = links::find_all(
                &[
                    (text.as_str(), Source::Text),
                    (text.as_str(), Source::Branch),
                ],
                p,
            );
            if !p.any() {
                kit::muted(ui, "Set up Jira, Linear or a custom pattern above first.");
            } else if found.is_empty() {
                kit::muted(ui, "No key in this text with the settings above.");
            } else {
                kit::secondary(
                    ui,
                    &format!("{} found:", format::plural(found.len(), "key", "keys")),
                );
                let pr = p.clone();
                kit::kv(ui, 120.0, |kv| {
                    for l in &found {
                        let label = format!("{} ({})", l.label, l.tracker.label(&pr));
                        let url = l.url.clone();
                        kv.row(&label, |ui| {
                            if kit::link(ui, &url).clicked() {
                                actions.push(Action::OpenUrl(url.clone()));
                            }
                        });
                    }
                });
            }
        });
        // Let failed lookups be tried again.
        let failed = shared::lock()
            .lookups
            .values()
            .filter(|l| matches!(l, Lookup::Failed(_)))
            .count();
        if failed > 0 {
            ui.add_space(space::S);
            ui.horizontal(|ui| {
                kit::muted(
                    ui,
                    &format!(
                        "{} failed this session.",
                        format::plural(failed, "lookup", "lookups")
                    ),
                );
                if kit::link(ui, "Try them again").clicked() {
                    actions.push(Act::RetryLookups.into());
                }
            });
        }
        if shared::take_scroll_end() {
            ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
        }
    });
}

/// "Seen in <repo>: ABC (12) · WEB (3)" with a click to add.
fn suggest_row(ui: &mut Ui, suggestions: &[(String, usize)], repo: &str, keys: &mut Vec<String>) {
    let p = *pal(ui);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            RichText::new(if repo.is_empty() {
                "Seen in recent commits:".to_owned()
            } else {
                format!("Seen in {repo}:")
            })
            .size(size::SMALL)
            .color(p.text_2),
        );
        for (k, n) in suggestions {
            if keys.iter().any(|x| x.eq_ignore_ascii_case(k)) {
                continue;
            }
            if kit::chip(ui, &format!("{k} · {n}"), false)
                .on_hover_text(format!(
                    "{n} mentions of {k}-… in commit summaries and branch names; click to add"
                ))
                .clicked()
            {
                keys.push(k.clone());
            }
        }
    });
}
