//! Access and rules: branch protection (form, rules in effect, rulesets)
//! and CODEOWNERS (validation, coverage, "who owns this path", editor).

use std::time::Instant;

use egui::{RichText, Ui};

use gitvis_core::devops::govern::Feature;
use gitvis_core::devops::govern::codeowners::{self, Issue, Level};
use gitvis_core::devops::govern::protection::{self, ProtectionForm};

use super::control::ANALYZE_DEBOUNCE;
use super::widgets;
use super::{Act, CoState, State};
use crate::devops::pages::{explain, layout};
use crate::format;
use crate::state::{Action, RepoState};
use crate::theme::{pal, size};
use crate::views::form;
use crate::views::icons::{Icon, IconButton};
use crate::views::insights::{Cell, grid, mono, muted, num, sec, text};
use crate::views::kit::{self, Col, Tone};

// ---------------------------------------------------------------------------
// Branch protection
// ---------------------------------------------------------------------------

/// Check names seen on recent commits and runs, for the contexts input.
fn seen_checks(repo: &RepoState) -> Vec<String> {
    let mut v: Vec<String> = repo
        .checks
        .values()
        .filter_map(|c| c.result.as_ref().and_then(|r| r.as_ref().ok()))
        .flat_map(|runs| runs.iter().map(|r| r.name.clone()))
        .collect();
    if let Some(g) = &repo.github {
        v.extend(g.runs.iter().take(60).map(|r| r.name.clone()));
    }
    v
}

pub fn protection(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) {
    let gh = widgets::gh(repo);
    let seen = seen_checks(repo);
    let State {
        access,
        protected,
        branch,
        protection,
        prot_branch,
        prot_edit,
        context_buf,
        rules,
        rulesets,
        ..
    } = &mut repo.devops.govern;
    let acc = access.ok();
    let admin = acc.is_some_and(|a| a.admin);
    let default = acc.map(|a| a.default_branch.clone()).unwrap_or_default();
    let choices = protection::branch_choices(
        &default,
        protected.ok().map(|v| v.as_slice()).unwrap_or(&[]),
    );
    kit::page_header(
        ui,
        "Branch protection",
        "Classic protection rules (admins only) and the rules every ruleset applies",
        |ui| {
            widgets::refresh(
                ui,
                protection.loading || rules.loading,
                "Read the protection, the rules in effect and the rulesets again",
                actions,
            );
            if !choices.is_empty() {
                let mut pick = branch.clone();
                let options: Vec<(String, String)> = choices
                    .iter()
                    .map(|b| {
                        (
                            b.clone(),
                            if *b == default {
                                format!("{b} (default)")
                            } else {
                                b.clone()
                            },
                        )
                    })
                    .collect();
                if form::select(ui, "gov_prot_branch", &mut pick, &options, 220.0) {
                    actions.push(Act::SelectBranch(pick).into());
                }
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
        },
    );
    let title_branch = prot_branch.clone();
    crate::devops::pages::layout::page(ui, "gov_prot", |ui| {
        if title_branch.is_empty() {
            if access.loading || protected.loading {
                widgets::loading(ui, "Reading the default and protected branches…");
            } else if let Some(e) = &access.error {
                widgets::failed(
                    ui,
                    "Could not read the repository",
                    e,
                    Act::Refresh,
                    actions,
                );
            } else {
                kit::muted(ui, "No branch to show.");
            }
            return;
        }
        if let Some(e) = &protected.error {
            kit::callout(
                ui,
                Tone::Warning,
                "The protected branch list is not available",
                e,
            );
        }
        kit::subtitle(ui, &format!("Classic protection of {title_branch}"));
        ui.add_space(2.0);
        if protection.loading {
            kit::loading(ui, &format!("Reading the protection of {title_branch}…"));
        } else if let Some(e) = &protection.error {
            widgets::denied(ui, gh.as_ref(), acc, Feature::Protection, e, actions);
        } else if let Some(current) = protection.ok() {
            let mut cancel = false;
            match (current, prot_edit.as_mut()) {
                (None, None) => {
                    kit::callout_with(
                        ui,
                        Tone::Warning,
                        &format!("{title_branch} is not protected"),
                        "Anyone with write access can push to it directly, force-push or delete it, unless a ruleset below says otherwise.",
                        |ui| {
                            if admin
                                && IconButton::new(Icon::Lock, "Protect this branch…")
                                    .compact()
                                    .show(ui)
                                    .clicked()
                            {
                                actions.push(Act::StartProtecting.into());
                            }
                        },
                    );
                }
                (Some(p), None) => {
                    if protection_summary(ui, p, admin) {
                        actions.push(Act::StartProtecting.into());
                    }
                    form_cards(ui, &mut p.clone(), false, context_buf, &seen);
                }
                (_, Some(edit)) => {
                    if current.is_none() {
                        kit::callout(
                            ui,
                            Tone::Accent,
                            "New protection rule",
                            "GitHub's recommended starting point: a pull request with one approval, stale approvals dismissed, no force pushes, no deletions.\nAdjust it, then review.",
                        );
                    }
                    form_cards(ui, edit, true, context_buf, &seen);
                    let changes = protection::changes(current.as_ref(), edit);
                    if !changes.is_empty() {
                        // One change per line, not a `;`-separated run.
                        ui.add_space(4.0);
                        kit::subtitle(ui, &format::plural(changes.len(), "change", "changes"));
                        let items: Vec<&str> = changes.iter().map(String::as_str).collect();
                        explain::bullets(ui, &items);
                    }
                    form::actions_row(ui, |ui| {
                        if form::primary(ui, "Review and save…", true).clicked() {
                            actions.push(Act::ReviewProtection.into());
                        }
                        if form::button(ui, "Cancel", true).clicked() {
                            cancel = true;
                        }
                        if current.is_some()
                            && form::danger(ui, "Remove protection…", true).clicked()
                        {
                            actions.push(Act::ReviewRemoveProtection.into());
                        }
                    });
                }
            }
            if cancel {
                *prot_edit = None;
            }
        }

        kit::section(ui, &format!("Rules in effect on {title_branch}"), None);
        kit::secondary(
            ui,
            "What GitHub enforces besides classic protection: the rules of every ruleset that targets the branch (repository and organisation).",
        );
        kit::secondary(ui, "Read access is enough to see them.");
        if rules.loading {
            kit::loading(ui, "Reading the rules…");
        } else if let Some(e) = &rules.error {
            widgets::denied(ui, gh.as_ref(), acc, Feature::Rulesets, e, actions);
        } else if let Some(list) = rules.ok() {
            if list.is_empty() {
                kit::muted(ui, "No ruleset applies to this branch.");
            } else {
                let names = rulesets.ok();
                let cells: Vec<Vec<Cell>> = list
                    .iter()
                    .map(|r| {
                        let name = names
                            .and_then(|v| v.iter().find(|x| x.id == r.ruleset_id))
                            .map(|x| x.name.clone())
                            .unwrap_or_else(|| format!("#{}", r.ruleset_id));
                        vec![
                            text(r.describe()),
                            sec(name),
                            Cell::Badges(vec![(
                                if r.source_type == "Organization" {
                                    format!("Organisation {}", r.source)
                                } else {
                                    r.source_type.clone()
                                },
                                Tone::Neutral,
                            )]),
                        ]
                    })
                    .collect();
                grid(
                    ui,
                    "gov_prot_rules",
                    &[Col::text("Rule"), Col::text("Ruleset"), Col::text("From")],
                    &cells,
                    false,
                    |_, _| {},
                );
            }
        }
        kit::section(ui, "Rulesets", rulesets.ok().map(|v| v.len()));
        if rulesets.loading {
            kit::loading(ui, "Reading the rulesets…");
        } else if let Some(e) = &rulesets.error {
            widgets::denied(ui, gh.as_ref(), acc, Feature::Rulesets, e, actions);
        } else if let Some(list) = rulesets.ok() {
            if list.is_empty() {
                kit::secondary(
                    ui,
                    "No rulesets. Rulesets are managed on GitHub (Settings → Rules → Rulesets); gitvis shows them read-only.",
                );
            } else {
                let cells: Vec<Vec<Cell>> = list
                    .iter()
                    .map(|r| {
                        vec![
                            text(r.name.as_str()),
                            sec(r.target.as_str()),
                            Cell::Badges(vec![(
                                sentence(&r.enforcement),
                                match r.enforcement.as_str() {
                                    "active" => Tone::Success,
                                    "evaluate" => Tone::Warning,
                                    _ => Tone::Neutral,
                                },
                            )]),
                            sec(format!(
                                "{} {}",
                                if r.source_type == "Organization" {
                                    "Organisation"
                                } else {
                                    r.source_type.as_str()
                                },
                                r.source
                            )),
                            muted(widgets::when(r.updated)),
                        ]
                    })
                    .collect();
                crate::devops::pages::layout::wide(ui, "gov_prot_rulesets", 640.0, |ui| {
                    grid(
                        ui,
                        "gov_prot_rulesets",
                        &[
                            Col::text("Ruleset"),
                            Col::text("Target"),
                            Col::text("Enforcement"),
                            Col::text("Source"),
                            Col::text("Updated"),
                        ],
                        &cells,
                        true,
                        |i, resp| {
                            if resp.clicked()
                                && let Some(r) = list.get(i)
                                && !r.html_url.is_empty()
                            {
                                actions.push(Action::OpenUrl(r.html_url.clone()));
                            }
                        },
                    );
                });
            }
        }
    });
}

/// `active` → `Active`.
fn sentence(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn on_off(b: bool) -> (String, Tone) {
    if b {
        ("On".into(), Tone::Success)
    } else {
        ("Off".into(), Tone::Neutral)
    }
}

/// One line on what the rule enforces; true when "Edit" was clicked.
fn protection_summary(ui: &mut Ui, p: &ProtectionForm, admin: bool) -> bool {
    let mut parts = Vec::new();
    if p.require_pr {
        parts.push(format!(
            "pull request with {}",
            format::plural(p.approvals as usize, "approval", "approvals")
        ));
    }
    if p.require_checks {
        parts.push(format::plural(
            p.contexts.len(),
            "required check",
            "required checks",
        ));
    }
    if !p.allow_force_pushes {
        parts.push("no force pushes".into());
    }
    if !p.allow_deletions {
        parts.push("no deletions".into());
    }
    if p.enforce_admins {
        parts.push("administrators included".into());
    }
    let mut edit = false;
    kit::callout_with(
        ui,
        Tone::Success,
        "Protected",
        &if parts.is_empty() {
            "A protection rule exists but enforces nothing beyond its defaults.".to_owned()
        } else {
            format!("Enforces: {}.", parts.join(", "))
        },
        |ui| {
            if admin {
                edit = IconButton::new(Icon::Edit, "Edit the rule…")
                    .compact()
                    .show(ui)
                    .clicked();
            } else {
                kit::secondary(ui, "Read-only: changing it needs the admin role.");
            }
        },
    );
    edit
}

/// The protection form; read-only unless `editable`.
fn form_cards(
    ui: &mut Ui,
    f: &mut ProtectionForm,
    editable: bool,
    context_buf: &mut String,
    seen: &[String],
) {
    ui.add_space(6.0);
    ui.add_enabled_ui(editable, |ui| {
        form::card(ui, Tone::Neutral, |ui| {
            kit::subtitle(ui, "Pull requests");
            ui.add_space(2.0);
            form::checkbox(ui, &mut f.require_pr, "Require a pull request before merging");
            if f.require_pr {
                ui.indent("gov_prot_pr", |ui| {
                    form::row(ui, 170.0, "Required approvals", |ui| {
                        let opts: Vec<(u8, &str)> =
                            [(0, "0"), (1, "1"), (2, "2"), (3, "3"), (4, "4"), (5, "5"), (6, "6")]
                                .to_vec();
                        if let Some(n) = layout::segmented_on_card(ui, &opts, f.approvals) {
                            f.approvals = n;
                        }
                    });
                    form::checkbox(
                        ui,
                        &mut f.dismiss_stale,
                        "Dismiss stale approvals when new commits are pushed",
                    );
                    form::checkbox(ui, &mut f.code_owner_reviews, "Require review from code owners");
                    form::checkbox(
                        ui,
                        &mut f.last_push_approval,
                        "Require approval of the most recent push",
                    );
                    if let Some(d) = &f.dismissal_restrictions {
                        kit::secondary(ui, &format!("Who can dismiss reviews: {}", d.describe()));
                    }
                    if let Some(b) = &f.bypass_allowances {
                        kit::secondary(ui, &format!("Allowed to bypass: {}", b.describe()));
                    }
                });
            }
        });
        ui.add_space(6.0);
        form::card(ui, Tone::Neutral, |ui| {
            kit::subtitle(ui, "Status checks");
            ui.add_space(2.0);
            form::checkbox(ui, &mut f.require_checks, "Require status checks to pass before merging");
            if f.require_checks {
                ui.indent("gov_prot_checks", |ui| {
                    form::checkbox(ui, &mut f.strict, "Require branches to be up to date before merging");
                    let suggestions = protection::suggest_contexts(
                        &f.contexts,
                        seen.iter().map(String::as_str),
                    );
                    if editable {
                        form::field(
                            ui,
                            "Required checks",
                            Some(if suggestions.is_empty() {
                                "Check run names as they appear on a pull request (job names for GitHub Actions)."
                            } else {
                                "Type to see names from recent check runs; Enter or a comma adds one."
                            }),
                            |ui| {
                                form::tags(
                                    ui,
                                    "gov_prot_contexts",
                                    &mut f.contexts,
                                    context_buf,
                                    "build (ubuntu-latest)",
                                    &suggestions,
                                );
                            },
                        );
                    } else if !f.contexts.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                            ui.label(
                                RichText::new("Required checks")
                                    .size(size::SMALL)
                                    .color(pal(ui).text_2),
                            );
                            for c in &f.contexts {
                                kit::badge(ui, c, Tone::Neutral);
                            }
                        });
                    }
                    if f.contexts.is_empty() {
                        kit::secondary(ui, "No required checks yet: merging is not blocked by any check.");
                    }
                });
            }
        });
        ui.add_space(6.0);
        form::card(ui, Tone::Neutral, |ui| {
            kit::subtitle(ui, "Rules");
            ui.add_space(2.0);
            form::checkbox(ui, &mut f.enforce_admins, "Include administrators");
            form::checkbox(ui, &mut f.linear_history, "Require linear history");
            form::checkbox(
                ui,
                &mut f.conversation_resolution,
                "Require conversation resolution before merging",
            );
            form::checkbox(ui, &mut f.allow_force_pushes, "Allow force pushes");
            form::checkbox(ui, &mut f.allow_deletions, "Allow deletions");
            form::checkbox(ui, &mut f.lock_branch, "Lock branch (read-only)");
            form::checkbox(ui, &mut f.allow_fork_syncing, "Allow fork syncing while locked");
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let (t, tone) = on_off(f.required_signatures);
                ui.label(
                    RichText::new("Signed commits required")
                        .size(size::SMALL)
                        .color(pal(ui).text_2),
                );
                kit::badge(ui, &t, tone);
            });
        });
        ui.add_space(6.0);
        form::card(ui, Tone::Neutral, |ui| {
            kit::subtitle(ui, "Who can push");
            ui.add_space(2.0);
            match &f.restrictions {
                Some(r) => kit::body(
                    ui,
                    &format!(
                        "Only {} (kept as it is when saving; edit on GitHub).",
                        r.describe()
                    ),
                ),
                None => kit::secondary(
                    ui,
                    "No push restriction: everyone with write access (organisation repositories can restrict this on GitHub).",
                ),
            }
        });
    });
}

// ---------------------------------------------------------------------------
// CODEOWNERS
// ---------------------------------------------------------------------------

fn level_tone(l: Level) -> Tone {
    match l {
        Level::Error => Tone::Danger,
        Level::Warning => Tone::Warning,
        Level::Info => Tone::Neutral,
    }
}

/// All problems: syntax and owners, file checks, GitHub's errors.
fn problems(co: &CoState) -> Vec<Issue> {
    let mut v: Vec<Issue> = co
        .parsed
        .as_ref()
        .map(|p| p.issues.clone())
        .unwrap_or_default();
    if co.analysis_gen == co.generation
        && let Some(a) = co.analysis.ok()
    {
        v.extend(a.issues.iter().cloned());
    }
    if !co.editing
        && let Some(e) = co.errors.ok()
    {
        v.extend(codeowners::github_issues(e));
    }
    v.sort_by(|a, b| a.line.cmp(&b.line).then(b.level.cmp(&a.level)));
    v
}

pub fn codeowners(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) {
    let gh = widgets::gh(repo);
    let has_gh = gh.is_some();
    let State { access, co, .. } = &mut repo.devops.govern;
    let acc = access.ok();
    let path = co.path();
    let exists = co.file.ok().is_some_and(|f| f.path.is_some());
    kit::page_header(
        ui,
        "CODEOWNERS",
        &if exists {
            format!("{path}: who is asked to review which paths")
        } else {
            "Who is asked to review which paths".to_owned()
        },
        |ui| {
            widgets::refresh(
                ui,
                co.file.loading || co.analysis.loading,
                "Read the file and GitHub's error list again",
                actions,
            );
            if co.file.ok().is_some() {
                if co.editing {
                    if IconButton::new(Icon::Check, "Review and save…")
                        .primary(true)
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Act::ReviewCodeownersSave.into());
                    }
                    if IconButton::new(Icon::Close, "Discard")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Act::EditCodeowners(false).into());
                    }
                } else if IconButton::new(Icon::Edit, if exists { "Edit" } else { "Create" })
                    .compact()
                    .show(ui)
                    .clicked()
                {
                    actions.push(Act::EditCodeowners(true).into());
                }
            }
        },
    );
    // Debounced re-analysis while editing.
    if co.editing && co.generation != co.analysis_gen && !co.analysis.loading {
        match co.edited_at {
            Some(t) if t.elapsed() < ANALYZE_DEBOUNCE => {
                ui.ctx()
                    .request_repaint_after(ANALYZE_DEBOUNCE.saturating_sub(t.elapsed()));
            }
            _ => actions.push(Act::AnalyzeCodeowners.into()),
        }
    }
    crate::devops::pages::layout::page(ui, "gov_co", |ui| {
        if co.file.loading && co.file.ok().is_none() {
            widgets::loading(ui, "Reading CODEOWNERS and the tracked files…");
            return;
        }
        if let Some(e) = &co.file.error {
            widgets::failed(
                ui,
                "Could not read CODEOWNERS",
                e,
                Act::LoadCodeowners,
                actions,
            );
            return;
        }
        let Some(file) = co.file.ok() else { return };
        if file.path.is_none() && !co.editing {
            if kit::empty_state(
                ui,
                Icon::Lock,
                "No CODEOWNERS file",
                "GitHub looks for .github/CODEOWNERS, then CODEOWNERS, then docs/CODEOWNERS.\nCode owners are requested for review automatically, and branch protection can require their approval.",
                Some((Icon::Plus, "Create .github/CODEOWNERS")),
            ) {
                actions.push(Act::EditCodeowners(true).into());
            }
            return;
        }
        if !file.ignored.is_empty() {
            kit::callout(
                ui,
                Tone::Warning,
                "More than one CODEOWNERS file",
                &format!(
                    "GitHub uses {} and ignores {}.",
                    file.path.as_deref().unwrap_or("?"),
                    file.ignored.join(", ")
                ),
            );
        }
        overview_tiles(ui, co, has_gh);
        who_owns(ui, co, actions);
        if co.editing {
            editor(ui, co);
        }
        problem_list(ui, co, has_gh, gh.as_ref(), acc, actions);
        coverage(ui, co);
        rules_table(ui, co);
    });
}

fn overview_tiles(ui: &mut Ui, co: &CoState, has_gh: bool) {
    let Some(p) = &co.parsed else { return };
    let errors = p.errors();
    let warnings = p
        .issues
        .iter()
        .filter(|i| i.level == Level::Warning)
        .count()
        + co.analysis
            .ok()
            .filter(|_| co.analysis_gen == co.generation)
            .map(|a| a.issues.len())
            .unwrap_or(0);
    let mut tiles = vec![
        kit::Stat::new("Rules", p.rules.len().to_string()),
        kit::Stat::new("Owners", p.owners().len().to_string()),
        kit::Stat::new("Errors", errors.to_string()).tone_if(errors > 0, Tone::Danger),
        kit::Stat::new("Warnings", warnings.to_string()).tone_if(warnings > 0, Tone::Warning),
    ];
    if has_gh && let Some(e) = co.errors.ok() {
        tiles.push(
            kit::Stat::new("GitHub errors", e.len().to_string())
                .tone_if(!e.is_empty(), Tone::Danger)
                .tip("Errors GitHub reports for the file on the default branch"),
        );
    }
    if let Some(a) = co
        .analysis
        .ok()
        .filter(|_| co.analysis_gen == co.generation)
    {
        let pct = if a.files == 0 {
            0.0
        } else {
            a.owned as f64 * 100.0 / a.files as f64
        };
        tiles.push(
            kit::Stat::new("Covered", format!("{pct:.0}%"))
                .tone_if(pct >= 99.5, Tone::Success)
                .tip("Tracked files with a code owner"),
        );
        tiles.push(
            kit::Stat::new("Unowned files", format::count(a.files - a.owned))
                .tone_if(a.files > a.owned, Tone::Warning),
        );
    }
    crate::views::insights::stats(ui, &tiles);
    let a = co
        .analysis
        .ok()
        .filter(|_| co.analysis_gen == co.generation);
    kit::secondary(
        ui,
        &format!(
            "{} ({} comments, {} blank), {}.{}",
            format::plural(p.lines, "line", "lines"),
            p.comment_lines,
            p.blank_lines,
            format::bytes(p.bytes),
            match a {
                Some(a) => format!(
                    " Matched against {} tracked files in {}.",
                    format::count(a.files),
                    format::millis(a.elapsed_ms)
                ),
                None if co.analysis.loading => " Matching against the tracked files…".to_owned(),
                None => String::new(),
            }
        ),
    );
}

fn who_owns(ui: &mut Ui, co: &mut CoState, actions: &mut Vec<Action>) {
    kit::section(ui, "Who owns this path", None);
    // The frame adds its padding and border to the width.
    let w = (ui.available_width() - 24.0).min(520.0);
    let resp = form::input(
        ui,
        "gov_co_test",
        &mut co.test_path,
        form::InputOpts::new("src/main.rs or docs/").mono().width(w),
    );
    if resp.changed() {
        actions.push(Act::TestPath(co.test_path.clone()).into());
    }
    let (Some(l), Some(p)) = (&co.lookup, &co.parsed) else {
        kit::secondary(
            ui,
            "Type a file or directory path: the rules that match it are listed, and the last one decides.",
        );
        return;
    };
    let subject = if l.is_dir {
        format!("a new file directly in {}/", l.path.trim_end_matches('/'))
    } else {
        l.path.clone()
    };
    if l.is_dir {
        kit::secondary(
            ui,
            &format!(
                "A directory: the rules below are those of {subject} (an imaginary file named {}).",
                codeowners::PROBE
            ),
        );
        kit::secondary(ui, "Its existing files are listed further down.");
    }
    if l.matching.is_empty() {
        kit::callout(
            ui,
            Tone::Warning,
            "No rule matches",
            &format!("Nobody is requested for review when {subject} changes."),
        );
    } else {
        let cells: Vec<Vec<Cell>> = l
            .matching
            .iter()
            .rev()
            .map(|&i| {
                let r = &p.rules[i];
                let wins = l.winner == Some(i);
                vec![
                    num(r.line),
                    mono(r.pattern.as_str()),
                    if r.owners.is_empty() {
                        muted("no owners")
                    } else {
                        text(r.owner_list())
                    },
                    Cell::Badges(if wins {
                        vec![("Decides".into(), Tone::Success)]
                    } else {
                        vec![("Overridden".into(), Tone::Neutral)]
                    }),
                ]
            })
            .collect();
        grid(
            ui,
            "gov_co_lookup",
            &[
                Col::num("Line"),
                Col::text("Pattern"),
                Col::text("Owners"),
                Col::text(""),
            ],
            &cells,
            false,
            |_, _| {},
        );
        if let Some(w) = l.winner {
            let r = &p.rules[w];
            kit::body(
                ui,
                &if r.owners.is_empty() {
                    format!("Line {} leaves {subject} without an owner.", r.line)
                } else {
                    format!("Owners of {subject}: {}.", r.owner_list())
                },
            );
        }
    }
    if l.files_below > 0 {
        crate::views::insights::group(
            ui,
            &format!("Files below {}", l.path.trim_end_matches('/')),
            Some(l.files_below),
        );
        let cells: Vec<Vec<Cell>> = l
            .below
            .iter()
            .take(15)
            .map(|(rule, n)| match rule {
                Some(i) => {
                    let r = &p.rules[*i];
                    vec![
                        num(*n),
                        mono(r.pattern.as_str()),
                        if r.owners.is_empty() {
                            muted("no owners")
                        } else {
                            text(r.owner_list())
                        },
                        sec(format!("line {}", r.line)),
                    ]
                }
                None => vec![
                    num(*n),
                    muted("no rule"),
                    Cell::Tone("unowned".into(), Tone::Warning),
                    muted("–"),
                ],
            })
            .collect();
        crate::devops::pages::layout::wide(ui, "gov_co_below", 560.0, |ui| {
            grid(
                ui,
                "gov_co_below",
                &[
                    Col::num("Files"),
                    Col::text("Decided by"),
                    Col::text("Owners"),
                    Col::text("Where"),
                ],
                &cells,
                false,
                |_, _| {},
            );
        });
    }
}

fn editor(ui: &mut Ui, co: &mut CoState) {
    kit::section(ui, "Editor", None);
    kit::secondary(
        ui,
        "Problems below update as you type; the checks against the tracked files run once typing pauses.",
    );
    kit::secondary(
        ui,
        "Saving writes the file in the working tree: commit and push it for GitHub to use it.",
    );
    let rows = (co.text.lines().count() + 2).clamp(8, 32);
    let resp = form::code(ui, "gov_co_editor", &mut co.text, rows);
    if resp.changed() {
        co.edited_at = Some(Instant::now());
        co.reparse();
    }
}

fn problem_list(
    ui: &mut Ui,
    co: &CoState,
    has_gh: bool,
    gh: Option<&gitvis_core::devops::gh::Gh>,
    acc: Option<&gitvis_core::devops::govern::Access>,
    actions: &mut Vec<Action>,
) {
    let list = problems(co);
    kit::section(ui, "Problems", Some(list.len()));
    if has_gh && !co.editing {
        if let Some(e) = &co.errors.error {
            widgets::denied(ui, gh, acc, Feature::CodeownersErrors, e, actions);
        } else if co.errors.loading {
            kit::loading(ui, "Asking GitHub for its CODEOWNERS errors…");
        }
    }
    if list.is_empty() {
        kit::callout(
            ui,
            Tone::Success,
            "No problems",
            "Valid owners, supported patterns, every rule matches files and decides some of them.",
        );
        return;
    }
    let lines: Vec<&str> = co.current_text().lines().collect();
    let analysis = co
        .analysis
        .ok()
        .filter(|_| co.analysis_gen == co.generation);
    // One group per kind of problem, most serious first.
    const GROUPS: [(&[&str], &str, &str); 8] = [
        (
            &["github"],
            "GitHub reports",
            "Errors GitHub found in the file on the default branch; it skips these lines.",
        ),
        (
            &["owner"],
            "Invalid owners",
            "Owners must be @user, @org/team or an email address with write access to the repository.",
        ),
        (
            &["negation", "range", "escape"],
            "Unsupported syntax",
            "Negation (!), character ranges ([ ]) and escaped # do not work in CODEOWNERS: GitHub skips these lines, and so does the matching here.",
        ),
        (&["size"], "File size", ""),
        (
            &["shadowed"],
            "Rules that never decide an owner",
            "Later rules match every file these rules match, and the last match wins.",
        ),
        (
            &["no-match"],
            "Rules that match no tracked file",
            "A typo, a moved or deleted directory, or files that are not committed yet.",
        ),
        (
            &["duplicate"],
            "Duplicate patterns",
            "Only the later line of the same pattern takes effect.",
        ),
        (
            &["no-owners"],
            "Rules without owners",
            "Intentional exceptions: files they decide have no code owner.",
        ),
    ];
    for (kinds, title, why) in GROUPS {
        let items: Vec<&Issue> = list.iter().filter(|i| kinds.contains(&i.kind)).collect();
        if items.is_empty() {
            continue;
        }
        let tone = items
            .iter()
            .map(|i| i.level)
            .max()
            .map(level_tone)
            .unwrap_or(Tone::Neutral);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            kit::subtitle(ui, title);
            kit::badge(ui, &items.len().to_string(), tone);
        });
        if !why.is_empty() {
            kit::secondary(ui, why);
        }
        let cells: Vec<Vec<Cell>> = items
            .iter()
            .take(60)
            .map(|i| {
                let src = if i.line > 0 {
                    lines
                        .get(i.line - 1)
                        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                let detail = match i.kind {
                    "shadowed" => co
                        .parsed
                        .as_ref()
                        .and_then(|p| p.rule_at_line(i.line).map(|r| (p, r)))
                        .and_then(|(p, r)| analysis.and_then(|a| a.stats.get(r)).map(|s| (p, s)))
                        .map(|(p, s)| {
                            let by: Vec<String> = s
                                .lost_to
                                .iter()
                                .take(3)
                                .map(|(w, n)| {
                                    format!(
                                        "line {} {} ({})",
                                        p.rules[*w].line,
                                        p.rules[*w].pattern,
                                        format::plural(*n, "file", "files")
                                    )
                                })
                                .collect();
                            format!(
                                "{} matched, taken by {}",
                                format::plural(s.matched, "file", "files"),
                                by.join(", ")
                            )
                        })
                        .unwrap_or_default(),
                    "no-match" | "no-owners" => String::new(),
                    _ => i.message.lines().next().unwrap_or("").to_owned(),
                };
                vec![
                    if i.line > 0 {
                        num(i.line)
                    } else {
                        muted("file")
                    },
                    mono(src),
                    if detail.is_empty() {
                        muted("")
                    } else {
                        sec(detail)
                    },
                ]
            })
            .collect();
        let with_detail = cells
            .iter()
            .any(|c| !matches!(c.get(2), Some(Cell::Muted(t)) if t.is_empty()));
        let cells: Vec<Vec<Cell>> = if with_detail {
            cells
        } else {
            cells
                .into_iter()
                .map(|mut c| {
                    c.truncate(2);
                    c
                })
                .collect()
        };
        let cols: &[Col] = if with_detail {
            &[Col::num("Line"), Col::text("Rule"), Col::text("Detail")]
        } else {
            &[Col::num("Line"), Col::text("Rule")]
        };
        crate::devops::pages::layout::wide(ui, ("gov_co_problems", title), 560.0, |ui| {
            grid(
                ui,
                &format!("gov_co_problems_{title}"),
                cols,
                &cells,
                false,
                |_, _| {},
            );
        });
        crate::views::insights::more(ui, items.len(), 60);
    }
}

fn coverage(ui: &mut Ui, co: &CoState) {
    let Some(a) = co
        .analysis
        .ok()
        .filter(|_| co.analysis_gen == co.generation)
    else {
        return;
    };
    let unowned = a.files - a.owned;
    kit::section(ui, "Coverage", None);
    if unowned == 0 {
        kit::callout(
            ui,
            Tone::Success,
            "Every tracked file has an owner",
            &format!("{} files.", format::count(a.files)),
        );
        return;
    }
    kit::body(
        ui,
        &format!(
            "{} of {} tracked files have no code owner: {} match no rule, {} fall under a rule with no owners.",
            format::count(unowned),
            format::count(a.files),
            format::count(a.unmatched),
            format::count(a.explicitly_unowned)
        ),
    );
    let items: Vec<(String, u32)> = a
        .unowned_dirs
        .iter()
        .map(|(d, n)| (d.clone(), *n as u32))
        .collect();
    crate::views::insights::group(
        ui,
        "Directories with the most unowned files",
        Some(items.len()),
    );
    kit::count_bars(ui, &items, pal(ui).warning, 15);
    if !a.unowned_examples.is_empty() {
        crate::views::insights::group(ui, "Examples", None);
        for f in a.unowned_examples.iter().take(8) {
            ui.label(kit::mono(f).color(pal(ui).text));
        }
    }
}

fn rules_table(ui: &mut Ui, co: &CoState) {
    let Some(p) = &co.parsed else { return };
    if p.rules.is_empty() {
        return;
    }
    let a = co
        .analysis
        .ok()
        .filter(|_| co.analysis_gen == co.generation);
    kit::section(ui, "Rules", Some(p.rules.len()));
    explain::terms(
        ui,
        90.0,
        &[
            ("Matches", "Tracked files the pattern matches."),
            (
                "Decides",
                "Files whose owners this rule sets: it is their last match.",
            ),
        ],
    );
    let cells: Vec<Vec<Cell>> = p
        .rules
        .iter()
        .enumerate()
        .take(400)
        .map(|(i, r)| {
            let stat = a.and_then(|a| a.stats.get(i));
            vec![
                num(r.line),
                mono(r.pattern.as_str()),
                if r.owners.is_empty() {
                    muted("no owners")
                } else {
                    text(r.owner_list())
                },
                match stat {
                    _ if r.regex.is_none() => Cell::Tone("skipped".into(), Tone::Danger),
                    Some(s) if s.matched == 0 => Cell::Tone("0".into(), Tone::Warning),
                    Some(s) => num(s.matched),
                    None => muted("…"),
                },
                match stat {
                    _ if r.regex.is_none() => muted("–"),
                    Some(s) if s.won == 0 && s.matched > 0 => Cell::Tone("0".into(), Tone::Warning),
                    Some(s) => num(s.won),
                    None => muted("…"),
                },
            ]
        })
        .collect();
    crate::devops::pages::layout::wide(ui, "gov_co_rules", 600.0, |ui| {
        grid(
            ui,
            "gov_co_rules",
            &[
                Col::num("Line"),
                Col::text("Pattern"),
                Col::text("Owners"),
                Col::num("Matches"),
                Col::num("Decides"),
            ],
            &cells,
            false,
            |_, _| {},
        );
    });
    crate::views::insights::more(ui, p.rules.len(), 400);
    let owners = p.owners();
    if !owners.is_empty() {
        kit::section(ui, "Owners", Some(owners.len()));
        let cells: Vec<Vec<Cell>> = owners
            .iter()
            .take(60)
            .map(|(o, n)| {
                let kind = codeowners::owner_kind(o);
                vec![
                    text(o.as_str()),
                    match kind {
                        codeowners::OwnerKind::User => sec("user"),
                        codeowners::OwnerKind::Team => sec("team"),
                        codeowners::OwnerKind::Email => sec("email"),
                        codeowners::OwnerKind::Invalid => {
                            Cell::Tone("invalid".into(), Tone::Danger)
                        }
                    },
                    num(*n),
                ]
            })
            .collect();
        grid(
            ui,
            "gov_co_owners",
            &[Col::text("Owner"), Col::text("Kind"), Col::num("Rules")],
            &cells,
            false,
            |_, _| {},
        );
    }
}
