//! Environments, Actions secrets and variables.

use egui::{Align, Layout, RichText, Ui, vec2};

use gitvis_core::devops::govern::Feature;
use gitvis_core::devops::govern::config::{self, Scope, Secret, Variable};
use gitvis_core::devops::govern::envs::{self, Environment, PolicyMode};

use super::widgets;
use super::{Act, EnvEdit, Fetch, SecretForm, State, Tab, VariableForm};
use crate::format;
use crate::state::{Action, RepoState};
use crate::theme::{pal, size};
use crate::views::form;
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Leading, Row, Tone};

/// Scopes for secrets and variables: the repository, the organisation
/// (organisation repositories only) and every environment.
pub fn scopes(repo: &RepoState) -> Vec<Scope> {
    let st = &repo.devops.govern;
    let mut v = vec![Scope::Repository];
    if st.access.ok().is_some_and(|a| a.is_org()) {
        v.push(Scope::Organization);
    }
    if let Some(envs) = st.envs.ok() {
        v.extend(envs.iter().map(|e| Scope::Environment(e.name.clone())));
    }
    v
}

fn scope_label(s: &Scope) -> String {
    match s {
        Scope::Repository => "Repository".into(),
        Scope::Organization => "Organisation".into(),
        Scope::Environment(e) => e.clone(),
    }
}

fn admin(st: &State) -> bool {
    st.access.ok().is_some_and(|a| a.admin)
}

fn env_summary(e: &Environment) -> String {
    let mut parts = Vec::new();
    if !e.reviewers.is_empty() {
        parts.push(format::plural(e.reviewers.len(), "reviewer", "reviewers"));
    }
    if e.wait_timer > 0 {
        parts.push(format!("wait {}", wait_text(e.wait_timer)));
    }
    match e.policy {
        PolicyMode::All => {}
        PolicyMode::Protected => parts.push("protected branches".into()),
        PolicyMode::Custom => parts.push("selected branches".into()),
    }
    if !e.other_rules.is_empty() {
        parts.push(format::plural(
            e.other_rules.len(),
            "custom rule",
            "custom rules",
        ));
    }
    if parts.is_empty() {
        "no protection rules".into()
    } else {
        parts.join(" · ")
    }
}

fn wait_text(minutes: u32) -> String {
    if minutes < 60 {
        format!("{minutes} min")
    } else if minutes.is_multiple_of(60) && minutes < 1440 {
        format!("{} h", minutes / 60)
    } else if minutes < 1440 {
        format!("{} h {} min", minutes / 60, minutes % 60)
    } else {
        format!("{:.1} days", minutes as f64 / 1440.0)
    }
}

// ---------------------------------------------------------------------------
// Environments
// ---------------------------------------------------------------------------

pub fn environments(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) {
    let gh = widgets::gh(repo);
    let is_admin = admin(&repo.devops.govern);
    let State {
        access,
        envs: list,
        env_selected,
        policies,
        env_edit,
        policy_buf,
        policy_tag,
        ..
    } = &mut repo.devops.govern;
    let acc = access.ok();
    crate::devops::pages::layout::page_header(
        ui,
        "Environments",
        "Where deployments go, who approves them, how long they wait, which branches may deploy",
        |ui| {
            widgets::refresh(ui, list.loading, "Read the environments again", actions);
            if IconButton::new(Icon::Plus, "New environment")
                .compact()
                .enabled(is_admin && env_edit.is_none())
                .show(ui)
                .on_hover_text(if is_admin {
                    "Create an environment (PUT environments/NAME)"
                } else {
                    "Creating environments needs the admin role"
                })
                .clicked()
            {
                actions.push(Act::NewEnv.into());
            }
            if let Some(a) = acc {
                kit::badge(
                    ui,
                    &format!("Role: {}", a.role()),
                    if a.admin {
                        Tone::Success
                    } else {
                        Tone::Neutral
                    },
                );
            }
            ui.add_space(6.0);
            widgets::fetched(ui, list, "");
        },
    );
    if let Some(e) = &list.error {
        let e = e.clone();
        crate::devops::pages::layout::page(ui, "gov_env_err", |ui| {
            widgets::denied(ui, gh.as_ref(), acc, Feature::Environments, &e, actions);
        });
        return;
    }
    let Some(all) = list.ok() else {
        crate::devops::pages::layout::page(ui, "gov_env_wait", |ui| {
            widgets::loading(ui, "Reading environments…");
        });
        return;
    };
    let sel = env_selected.clone();
    let key = match (&env_edit, &sel) {
        (Some(_), _) => "edit".to_owned(),
        (None, Some(s)) => s.clone(),
        _ => String::new(),
    };
    crate::devops::pages::layout::split_keyed(
        ui,
        "gov_env",
        &key,
        actions,
        |ui, actions| {
            widgets::list_head(
                ui,
                "Environments",
                all.len(),
                sel.is_some(),
                Act::SelectEnv(None),
                actions,
            );
            if all.is_empty() {
                widgets::list_none(
                    ui,
                    "No environments. A workflow job with environment: NAME creates one on its first run; administrators can create one here.",
                );
                return;
            }
            kit::list(
                ui,
                "gov_env_list",
                all.len(),
                kit::ROW_H,
                |i| sel.as_deref() == Some(all[i].name.as_str()),
                |ui, rect, resp, i| {
                    let e = &all[i];
                    let sub = env_summary(e);
                    let protected =
                        !e.reviewers.is_empty() || e.wait_timer > 0 || e.policy != PolicyMode::All;
                    let p = pal(ui);
                    kit::paint_row(
                        ui,
                        rect,
                        &Row::new(&e.name)
                            .leading(Leading::Icon(
                                Icon::Cloud,
                                if protected { p.success } else { p.text_3 },
                            ))
                            .subtitle(&sub),
                        sel.as_deref() == Some(e.name.as_str()),
                    );
                    if resp.clicked() {
                        actions.push(Act::SelectEnv(Some(e.name.clone())).into());
                    }
                },
            );
        },
        |ui, actions| {
            if let Some(edit) = env_edit {
                env_form(ui, edit, actions);
                return;
            }
            match sel
                .as_deref()
                .and_then(|n| all.iter().find(|e| e.name == n))
            {
                Some(e) => env_detail(
                    ui,
                    e,
                    policies.get(&e.name),
                    is_admin,
                    policy_buf,
                    policy_tag,
                    gh.as_ref(),
                    actions,
                ),
                None => env_overview(ui, all, is_admin),
            }
        },
    );
}

fn env_overview(ui: &mut Ui, all: &[Environment], is_admin: bool) {
    let reviewers = all.iter().filter(|e| !e.reviewers.is_empty()).count();
    let waits = all.iter().filter(|e| e.wait_timer > 0).count();
    let limited = all.iter().filter(|e| e.policy != PolicyMode::All).count();
    let open = all
        .iter()
        .filter(|e| e.reviewers.is_empty() && e.wait_timer == 0 && e.policy == PolicyMode::All)
        .count();
    crate::views::insights::stats(
        ui,
        &[
            kit::Stat::new("Environments", all.len().to_string()),
            kit::Stat::new("With reviewers", reviewers.to_string()),
            kit::Stat::new("With a wait timer", waits.to_string()),
            kit::Stat::new("Branch-limited", limited.to_string()),
            kit::Stat::new("Unprotected", open.to_string())
                .tone_if(open > 0 && all.len() > 1, Tone::Warning)
                .tip("No reviewers, no wait timer, every branch may deploy"),
        ],
    );
    kit::secondary(
        ui,
        &format!(
            "Select an environment for its protection rules and branch policies.{}",
            if is_admin {
                " As an administrator you can create, edit and delete environments."
            } else {
                " Changing environments needs the admin role."
            }
        ),
    );
}

#[allow(clippy::too_many_arguments)]
fn env_detail(
    ui: &mut Ui,
    e: &Environment,
    policies: Option<&Fetch<Vec<envs::BranchPolicy>>>,
    is_admin: bool,
    policy_buf: &mut String,
    policy_tag: &mut bool,
    gh: Option<&gitvis_core::devops::gh::Gh>,
    actions: &mut Vec<Action>,
) {
    kit::title(ui, &e.name);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if IconButton::new(Icon::Edit, "Edit…")
            .compact()
            .enabled(is_admin)
            .show(ui)
            .clicked()
        {
            actions.push(Act::EditEnv(e.name.clone()).into());
        }
        if IconButton::new(Icon::Trash, "Delete…")
            .compact()
            .danger()
            .enabled(is_admin)
            .show(ui)
            .clicked()
        {
            actions.push(Act::ReviewDeleteEnv(e.name.clone()).into());
        }
        if let Some(gh) = gh
            && IconButton::new(Icon::External, "Open on GitHub")
                .compact()
                .show(ui)
                .clicked()
        {
            actions.push(Action::OpenUrl(
                gh.web_url(&format!("/settings/environments/{}/edit", e.id)),
            ));
        }
        if IconButton::new(Icon::Key, "Secrets")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Act::SetTab(Tab::Secrets).into());
            actions.push(Act::SetSecretScope(Scope::Environment(e.name.clone())).into());
        }
        if IconButton::new(Icon::List, "Variables")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Act::SetTab(Tab::Variables).into());
            actions.push(Act::SetVariableScope(Scope::Environment(e.name.clone())).into());
        }
    });
    ui.add_space(4.0);
    kit::kv(ui, 170.0, |kv| {
        if e.reviewers.is_empty() {
            kv.muted(
                "Required reviewers",
                "none: deployments start without approval",
            );
        } else {
            kv.text(
                "Required reviewers",
                e.reviewers
                    .iter()
                    .map(|r| match r.kind {
                        envs::ReviewerKind::User => r.handle.clone(),
                        envs::ReviewerKind::Team => format!("{} (team)", r.handle),
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            kv.text(
                "Prevent self-review",
                if e.prevent_self_review {
                    "Yes: whoever started the run cannot approve it"
                } else {
                    "No"
                },
            );
        }
        if e.wait_timer > 0 {
            kv.text("Wait timer", wait_text(e.wait_timer));
        } else {
            kv.muted("Wait timer", "none");
        }
        kv.text(
            "Administrators",
            if e.can_admins_bypass {
                "can bypass the rules"
            } else {
                "follow the rules too"
            },
        );
        kv.text("Deployment branches", e.policy.label());
        if !e.other_rules.is_empty() {
            kv.text("Custom rules", e.other_rules.join(", "));
        }
        kv.text("Created", widgets::when(e.created));
        kv.text("Updated", widgets::when(e.updated));
    });
    if e.policy != PolicyMode::Custom {
        ui.add_space(4.0);
        kit::muted(
            ui,
            match e.policy {
                PolicyMode::All => {
                    "Any branch or tag can deploy here. Choose “Selected branches and tags” in the editor to restrict it with patterns."
                }
                _ => "Only branches with protection rules can deploy here.",
            },
        );
        return;
    }
    kit::section(
        ui,
        "Branch and tag policies",
        policies.and_then(|p| p.ok()).map(|v| v.len()),
    );
    match policies {
        None => kit::loading(ui, "Reading the policies…"),
        Some(f) if f.loading => kit::loading(ui, "Reading the policies…"),
        Some(f) => {
            if let Some(err) = &f.error {
                kit::callout(ui, Tone::Danger, "Could not read the policies", err);
            }
            if let Some(list) = f.ok() {
                if list.is_empty() {
                    kit::muted(
                        ui,
                        "No patterns yet: nothing can deploy until one is added.",
                    );
                }
                for p in list {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        kit::badge(
                            ui,
                            if p.kind == "tag" { "Tag" } else { "Branch" },
                            Tone::Neutral,
                        );
                        ui.label(kit::mono(&p.name).color(pal(ui).text));
                        if is_admin
                            && IconButton::icon(Icon::Trash)
                                .compact()
                                .show(ui)
                                .on_hover_text("Remove this policy")
                                .clicked()
                        {
                            actions.push(
                                Act::ReviewDeletePolicy {
                                    env: e.name.clone(),
                                    id: p.id,
                                    name: p.name.clone(),
                                }
                                .into(),
                            );
                        }
                    });
                }
            }
        }
    }
    if is_admin {
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
            let iw = crate::devops::pages::layout::input_width(ui, 260.0);
            form::input(
                ui,
                ("gov_policy_new", &e.name),
                policy_buf,
                form::InputOpts::new("release/* or v*").mono().width(iw),
            );
            if form::radio(ui, !*policy_tag, "Branch") {
                *policy_tag = false;
            }
            if form::radio(ui, *policy_tag, "Tag") {
                *policy_tag = true;
            }
            if form::button(ui, "Add…", !policy_buf.trim().is_empty()).clicked() {
                actions.push(Act::ReviewAddPolicy(e.name.clone()).into());
            }
        });
    }
}

fn env_form(ui: &mut Ui, edit: &mut EnvEdit, actions: &mut Vec<Action>) {
    let creating = edit.editing.is_none();
    kit::title(
        ui,
        &if creating {
            "New environment".to_owned()
        } else {
            format!("Edit {}", edit.form.name)
        },
    );
    kit::secondary(
        ui,
        "Saving sends one PUT with every protection rule; reviewers are resolved to their numeric ids first (users/LOGIN, orgs/ORG/teams/SLUG).",
    );
    ui.add_space(4.0);
    form::card(ui, Tone::Neutral, |ui| {
        form::field(
            ui,
            "Name",
            Some(if creating {
                "Case-insensitive; no / \\ ? # or %."
            } else {
                "Renaming is not possible: create a new environment instead."
            }),
            |ui| {
                form::input(
                    ui,
                    "gov_env_name",
                    &mut edit.form.name,
                    form::InputOpts::new("production").enabled(creating),
                );
            },
        );
        form::field(
            ui,
            "Required reviewers",
            Some(
                "@login or org/team, up to six. Anyone listed can approve; one approval is enough.",
            ),
            |ui| {
                form::tags(
                    ui,
                    "gov_env_reviewers",
                    &mut edit.form.reviewers,
                    &mut edit.reviewer_buf,
                    "@octocat or acme/release-managers",
                    &[],
                );
            },
        );
        form::checkbox(
            ui,
            &mut edit.form.prevent_self_review,
            "Prevent self-review (the person who started the run cannot approve it)",
        );
        ui.add_space(4.0);
        form::field(
            ui,
            "Wait timer (minutes)",
            Some("0 to 43200 (30 days): jobs wait this long after approval before they start."),
            |ui| {
                form::input(
                    ui,
                    "gov_env_wait",
                    &mut edit.wait_text,
                    form::InputOpts::new("0").width(120.0),
                );
            },
        );
        form::checkbox(
            ui,
            &mut edit.form.can_admins_bypass,
            "Allow administrators to bypass these rules",
        );
        ui.add_space(4.0);
        form::field(ui, "Deployment branches and tags", None, |ui| {
            for m in [PolicyMode::All, PolicyMode::Protected, PolicyMode::Custom] {
                if form::radio(ui, edit.form.policy == m, m.label()) {
                    edit.form.policy = m;
                }
            }
        });
        if edit.form.policy == PolicyMode::Custom {
            kit::secondary(
                ui,
                "Add the branch and tag patterns after saving, on the environment's page.",
            );
        }
        form::error(ui, &edit.error);
        form::actions_row(ui, |ui| {
            let label = if edit.resolving {
                "Resolving reviewers…"
            } else {
                "Review…"
            };
            if form::primary(ui, label, !edit.resolving).clicked() {
                actions.push(Act::ReviewEnv.into());
            }
            if form::button(ui, "Cancel", true).clicked() {
                actions.push(Act::CancelEnv.into());
            }
        });
    });
}

// ---------------------------------------------------------------------------
// Secrets
// ---------------------------------------------------------------------------

fn scope_chips(
    ui: &mut Ui,
    scopes: &[Scope],
    current: &Scope,
    secrets: bool,
    actions: &mut Vec<Action>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        for s in scopes {
            if kit::chip(ui, &scope_label(s), s == current).clicked() && s != current {
                actions.push(
                    if secrets {
                        Act::SetSecretScope(s.clone())
                    } else {
                        Act::SetVariableScope(s.clone())
                    }
                    .into(),
                );
            }
        }
    });
    ui.add_space(4.0);
}

/// The column widths of a [`table_row`] / [`table_head`]: as given, or
/// scaled down together when they (with the gaps and room for the row's
/// buttons) are wider than the pane, so a narrow window cuts cells
/// instead of pushing the last columns and the buttons out of view.
fn fit_cols(ui: &Ui, widths: impl Iterator<Item = f32>) -> Vec<f32> {
    let ws: Vec<f32> = widths.collect();
    let gaps = 12.0 * ws.len() as f32;
    let room = (ui.available_width() - gaps - 80.0).max(64.0 * ws.len() as f32);
    let total: f32 = ws.iter().sum();
    if total <= room {
        ws
    } else {
        ws.iter().map(|w| (w * room / total).max(64.0)).collect()
    }
}

/// A table row with fixed columns and buttons on the right.
fn table_row(ui: &mut Ui, cols: &[(f32, RichText)], buttons: impl FnOnce(&mut Ui)) {
    let ws = fit_cols(ui, cols.iter().map(|c| c.0));
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), 30.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            for ((_, t), w) in cols.iter().zip(&ws) {
                ui.allocate_ui_with_layout(
                    vec2(*w, 28.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.set_width(*w);
                        ui.add(egui::Label::new(t.clone()).truncate());
                    },
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                buttons(ui);
            });
        },
    );
    kit::hairline(ui);
}

fn table_head(ui: &mut Ui, cols: &[(f32, &str)]) {
    let p = *pal(ui);
    let ws = fit_cols(ui, cols.iter().map(|c| c.0));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        for ((_, t), w) in cols.iter().zip(&ws) {
            ui.allocate_ui_with_layout(
                vec2(*w, 20.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_width(*w);
                    ui.add(egui::Label::new(kit::caption_text(&p, t)).truncate());
                },
            );
        }
    });
    kit::hairline(ui);
}

pub fn secrets(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) {
    let gh = widgets::gh(repo);
    let scopes = scopes(repo);
    let is_admin = admin(&repo.devops.govern);
    let State {
        access,
        secret_scope,
        secrets,
        secret_form,
        workflow_secrets,
        ..
    } = &mut repo.devops.govern;
    let acc = access.ok();
    let scope = secret_scope.clone();
    let current = secrets.get(&scope);
    let read_only = scope == Scope::Organization;
    crate::devops::pages::layout::page_header(
        ui,
        "Secrets",
        "Names and dates only: GitHub never returns a secret's value",
        |ui| {
            widgets::refresh(
                ui,
                current.is_some_and(|f| f.loading),
                "Read the secrets of this scope again",
                actions,
            );
            if IconButton::new(Icon::Plus, "New secret")
                .compact()
                .enabled(!read_only && secret_form.is_none())
                .show(ui)
                .on_hover_text(if read_only {
                    "Organisation secrets are managed in the organisation's settings"
                } else {
                    "Set a secret with gh secret set (the value goes on standard input)"
                })
                .clicked()
            {
                actions.push(Act::NewSecret(scope.clone(), None).into());
            }
            if let Some(a) = acc {
                kit::badge(
                    ui,
                    &format!("Role: {}", a.role()),
                    if a.admin {
                        Tone::Success
                    } else {
                        Tone::Neutral
                    },
                );
            }
            if let Some(f) = current {
                ui.add_space(6.0);
                widgets::fetched(ui, f, "");
            }
        },
    );
    let full = repo
        .github_remote
        .as_ref()
        .map(|(o, r)| format!("{o}/{r}"))
        .unwrap_or_default();
    crate::devops::pages::layout::page(ui, "gov_secrets", |ui| {
        scope_chips(ui, &scopes, &scope, true, actions);
        if let Some(f) = secret_form {
            secret_form_card(ui, f, &full, actions);
            if f.closing {
                *secret_form = None;
            } else {
                f.fresh = false;
            }
        }
        let Some(fetch) = secrets.get(&scope) else {
            widgets::loading(ui, "Reading secrets…");
            return;
        };
        if let Some(e) = &fetch.error {
            let feature = if scope == Scope::Organization {
                Feature::OrgSecrets
            } else {
                Feature::Secrets
            };
            widgets::denied(ui, gh.as_ref(), acc, feature, e, actions);
            return;
        }
        let Some(list) = fetch.ok() else {
            widgets::loading(ui, "Reading secrets…");
            return;
        };
        kit::section(
            ui,
            &format!("{} secrets", scope_label(&scope)),
            Some(list.len()),
        );
        if list.is_empty() {
            kit::muted(
                ui,
                match &scope {
                    Scope::Repository => {
                        "No repository secrets. Workflows read them as ${{ secrets.NAME }}; use New secret to add one."
                    }
                    Scope::Organization => {
                        "No organisation secrets are shared with this repository."
                    }
                    Scope::Environment(_) => {
                        "No secrets in this environment. Jobs that target it see them in addition to the repository's; use New secret to add one."
                    }
                },
            );
        } else {
            let cols = if read_only {
                vec![
                    (260.0, "Name"),
                    (230.0, "Updated"),
                    (230.0, "Created"),
                    (110.0, "Visibility"),
                ]
            } else {
                vec![(260.0, "Name"), (230.0, "Updated"), (230.0, "Created")]
            };
            table_head(ui, &cols);
            for s in list {
                secret_row(ui, s, read_only, is_admin, actions);
            }
        }
        if scope == Scope::Repository {
            workflow_refs(ui, workflow_secrets, secrets);
        }
    });
}

fn secret_row(ui: &mut Ui, s: &Secret, read_only: bool, is_admin: bool, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let mut cols = vec![
        (260.0, kit::mono(&s.name).color(p.text)),
        (
            230.0,
            RichText::new(widgets::when(s.updated))
                .size(size::SMALL)
                .color(p.text_2),
        ),
        (
            230.0,
            RichText::new(widgets::when(s.created))
                .size(size::SMALL)
                .color(p.text_2),
        ),
    ];
    if read_only {
        cols.push((
            110.0,
            RichText::new(s.visibility.clone().unwrap_or_default())
                .size(size::SMALL)
                .color(p.text_2),
        ));
    }
    table_row(ui, &cols, |ui| {
        if read_only {
            return;
        }
        if IconButton::icon(Icon::Trash)
            .compact()
            .enabled(is_admin)
            .show(ui)
            .on_hover_text("Delete this secret")
            .clicked()
        {
            actions.push(Act::ReviewDeleteSecret(s.scope.clone(), s.name.clone()).into());
        }
        if IconButton::new(Icon::Edit, "Update…")
            .compact()
            .enabled(is_admin)
            .show(ui)
            .clicked()
        {
            actions.push(Act::NewSecret(s.scope.clone(), Some(s.name.clone())).into());
        }
    });
}

fn workflow_refs(
    ui: &mut Ui,
    refs: &Fetch<Vec<(String, Vec<String>)>>,
    all: &std::collections::HashMap<Scope, Fetch<Vec<Secret>>>,
) {
    let Some(refs) = refs.ok() else { return };
    kit::section(ui, "Referenced by workflows", Some(refs.len()));
    if refs.is_empty() {
        kit::secondary(
            ui,
            "No workflow under .github/workflows reads secrets.NAME (GITHUB_TOKEN aside).",
        );
        return;
    }
    kit::secondary(
        ui,
        "secrets.NAME in .github/workflows, and the scopes that define each one (repository, organisation, environments).\nA secret no scope defines reads as an empty string.",
    );
    let p = *pal(ui);
    // Where each name is defined, among the scopes read so far.
    let mut order: Vec<&Scope> = all.keys().collect();
    order.sort_by_key(|s| match s {
        Scope::Repository => (0, String::new()),
        Scope::Organization => (1, String::new()),
        Scope::Environment(e) => (2, e.clone()),
    });
    table_head(
        ui,
        &[
            (260.0, "Secret"),
            (220.0, "Defined in"),
            (420.0, "Workflows"),
        ],
    );
    for (name, files) in refs {
        let found: Vec<String> = order
            .iter()
            .filter(|s| {
                all.get(s)
                    .and_then(|f| f.ok())
                    .is_some_and(|v| v.iter().any(|x| x.name.eq_ignore_ascii_case(name)))
            })
            .map(|s| scope_label(s))
            .collect();
        let (status, color) = if found.is_empty() {
            ("Not defined".to_owned(), p.warning)
        } else {
            (found.join(", "), p.success)
        };
        table_row(
            ui,
            &[
                (260.0, kit::mono(name).color(p.text)),
                (220.0, RichText::new(status).size(size::SMALL).color(color)),
                (
                    420.0,
                    RichText::new(files.join(", "))
                        .size(size::SMALL)
                        .color(p.text_2),
                ),
            ],
            |_| {},
        );
    }
}

fn secret_form_card(ui: &mut Ui, f: &mut SecretForm, full: &str, actions: &mut Vec<Action>) {
    let problem = if f.name.is_empty() {
        None
    } else {
        config::name_problem(&f.name)
    };
    form::card(ui, Tone::Accent, |ui| {
        kit::subtitle(
            ui,
            &if f.existing {
                format!("Update {} ({})", f.name, f.scope.label())
            } else {
                format!("New secret ({})", f.scope.label())
            },
        );
        ui.add_space(4.0);
        form::field(
            ui,
            "Name",
            Some(
                "Letters, digits and underscores; stored upper-case; not starting with a digit or GITHUB_.",
            ),
            |ui| {
                form::input(
                    ui,
                    "gov_secret_name",
                    &mut f.name,
                    form::InputOpts::new("DEPLOY_TOKEN")
                        .mono()
                        .invalid(problem.is_some())
                        .enabled(!f.existing),
                );
            },
        );
        if let Some(p) = &problem {
            form::error(ui, p);
        }
        let chars = f.value.chars().count();
        let help = format!(
            "Masked. Sent to gh secret set on standard input; never shown, logged or kept by gitvis, and cleared as soon as the request is queued. {}",
            format::plural(chars, "character", "characters")
        );
        form::field(ui, "Value", Some(&help), |ui| {
            // Drop editor state (cursor, undo history) of an earlier value.
            let id = ui.make_persistent_id(("form_input", "gov_secret_value"));
            if f.fresh || f.closing {
                ui.data_mut(|d| d.remove::<egui::text_edit::TextEditState>(id));
            }
            if !f.closing {
                form::password(ui, "gov_secret_value", &mut f.value, "Paste the value");
            }
        });
        if !full.is_empty() {
            ui.label(
                RichText::new(config::secret_set_preview(
                    &config::secret_set_args(full, &config::normalize_name(&f.name), f.scope.env()),
                    chars,
                ))
                .monospace()
                .size(size::MONO - 0.5)
                .color(pal(ui).text_2),
            );
        }
        form::actions_row(ui, |ui| {
            let ok = problem.is_none() && !f.name.trim().is_empty() && !f.value.is_empty();
            if form::primary(ui, "Review…", ok).clicked() {
                actions.push(Act::ReviewSecret.into());
            }
            if form::button(ui, "Cancel", true).clicked() {
                actions.push(Act::CancelSecret.into());
            }
        });
    });
    ui.add_space(8.0);
}

// ---------------------------------------------------------------------------
// Variables
// ---------------------------------------------------------------------------

pub fn variables(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) {
    let gh = widgets::gh(repo);
    let scopes = scopes(repo);
    let is_admin = admin(&repo.devops.govern);
    let State {
        access,
        var_scope,
        variables,
        variable_form,
        ..
    } = &mut repo.devops.govern;
    let acc = access.ok();
    let scope = var_scope.clone();
    let current = variables.get(&scope);
    let read_only = scope == Scope::Organization;
    crate::devops::pages::layout::page_header(
        ui,
        "Variables",
        "Configuration values for workflows (vars.NAME); not secret, shown in full",
        |ui| {
            widgets::refresh(
                ui,
                current.is_some_and(|f| f.loading),
                "Read the variables of this scope again",
                actions,
            );
            if IconButton::new(Icon::Plus, "New variable")
                .compact()
                .enabled(!read_only && variable_form.is_none())
                .show(ui)
                .clicked()
            {
                actions.push(Act::NewVariable(scope.clone()).into());
            }
            if let Some(a) = acc {
                kit::badge(
                    ui,
                    &format!("Role: {}", a.role()),
                    if a.admin {
                        Tone::Success
                    } else {
                        Tone::Neutral
                    },
                );
            }
            if let Some(f) = current {
                ui.add_space(6.0);
                widgets::fetched(ui, f, "");
            }
        },
    );
    crate::devops::pages::layout::page(ui, "gov_vars", |ui| {
        scope_chips(ui, &scopes, &scope, false, actions);
        if let Some(f) = variable_form {
            variable_form_card(ui, f, actions);
        }
        let Some(fetch) = variables.get(&scope) else {
            widgets::loading(ui, "Reading variables…");
            return;
        };
        if let Some(e) = &fetch.error {
            widgets::denied(ui, gh.as_ref(), acc, Feature::Variables, e, actions);
            return;
        }
        let Some(list) = fetch.ok() else {
            widgets::loading(ui, "Reading variables…");
            return;
        };
        kit::section(
            ui,
            &format!("{} variables", scope_label(&scope)),
            Some(list.len()),
        );
        if list.is_empty() {
            kit::muted(
                ui,
                match &scope {
                    Scope::Repository => {
                        "No repository variables. Workflows read them as ${{ vars.NAME }}; use New variable to add one."
                    }
                    Scope::Organization => {
                        "No organisation variables are shared with this repository."
                    }
                    Scope::Environment(_) => "No variables in this environment.",
                },
            );
            return;
        }
        table_head(ui, &[(240.0, "Name"), (360.0, "Value"), (220.0, "Updated")]);
        for v in list {
            variable_row(ui, v, read_only, is_admin, actions);
        }
    });
}

fn variable_row(
    ui: &mut Ui,
    v: &Variable,
    read_only: bool,
    is_admin: bool,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let value = if v.value.contains('\n') {
        format!(
            "{} … ({} lines)",
            v.value.lines().next().unwrap_or(""),
            v.value.lines().count()
        )
    } else {
        v.value.clone()
    };
    table_row(
        ui,
        &[
            (240.0, kit::mono(&v.name).color(p.text)),
            (360.0, kit::mono(value).color(p.text)),
            (
                220.0,
                RichText::new(widgets::when(v.updated))
                    .size(size::SMALL)
                    .color(p.text_2),
            ),
        ],
        |ui| {
            if read_only {
                return;
            }
            if IconButton::icon(Icon::Trash)
                .compact()
                .enabled(is_admin)
                .show(ui)
                .on_hover_text("Delete this variable")
                .clicked()
            {
                actions.push(Act::ReviewDeleteVariable(v.scope.clone(), v.name.clone()).into());
            }
            if IconButton::new(Icon::Edit, "Edit…")
                .compact()
                .enabled(is_admin)
                .show(ui)
                .clicked()
            {
                actions.push(Act::EditVariable(v.scope.clone(), v.name.clone()).into());
            }
            if IconButton::icon(Icon::Copy)
                .compact()
                .show(ui)
                .on_hover_text("Copy the value")
                .clicked()
            {
                actions.push(Action::Copy(v.value.clone()));
            }
        },
    );
}

fn variable_form_card(ui: &mut Ui, f: &mut VariableForm, actions: &mut Vec<Action>) {
    let problem = if f.name.is_empty() {
        None
    } else {
        config::name_problem(&f.name)
    };
    form::card(ui, Tone::Accent, |ui| {
        kit::subtitle(
            ui,
            &match &f.original {
                Some(o) => format!("Edit {o} ({})", f.scope.label()),
                None => format!("New variable ({})", f.scope.label()),
            },
        );
        ui.add_space(4.0);
        form::field(
            ui,
            "Name",
            Some(
                "Letters, digits and underscores; stored upper-case; not starting with a digit or GITHUB_.",
            ),
            |ui| {
                form::input(
                    ui,
                    "gov_var_name",
                    &mut f.name,
                    form::InputOpts::new("DEPLOY_REGION")
                        .mono()
                        .invalid(problem.is_some()),
                );
            },
        );
        if let Some(p) = &problem {
            form::error(ui, p);
        }
        form::field(
            ui,
            "Value",
            Some(&format!(
                "Visible to anyone who can read the workflow logs. {}",
                format::plural(f.value.chars().count(), "character", "characters")
            )),
            |ui| {
                form::area(ui, "gov_var_value", &mut f.value, "eu-west-1", 3);
            },
        );
        form::actions_row(ui, |ui| {
            let ok = problem.is_none() && !f.name.trim().is_empty();
            if form::primary(ui, "Review…", ok).clicked() {
                actions.push(Act::ReviewVariable.into());
            }
            if form::button(ui, "Cancel", true).clicked() {
                actions.push(Act::CancelVariable.into());
            }
        });
    });
    ui.add_space(8.0);
}
