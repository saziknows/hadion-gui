//! The New pull request and New issue forms (inline on the page).

use egui::{RichText, Ui, vec2};
use gitvis_core::devops::review::Suggestions;

use super::view::scroll_pane;
use super::{Act, Fetch, NewIssueForm};
use crate::format;
use crate::state::{Action, RepoState};
use crate::theme::{Tone, pal, size, space};
use crate::views::form;
use crate::views::kit;

/// Where the current branch stands against its upstream.
enum PushState {
    NoBranch,
    NotPushed,
    Gone(String),
    Ahead(String, usize, usize),
    Behind(String, usize),
    UpToDate(String),
}

fn push_state(repo: &RepoState, head: &str) -> PushState {
    let Some(b) = repo.snapshot.as_ref().and_then(|s| s.branch(head)) else {
        return PushState::NoBranch;
    };
    match &b.upstream {
        None => PushState::NotPushed,
        Some(u) if b.upstream_gone => PushState::Gone(u.clone()),
        Some(u) if b.ahead > 0 => PushState::Ahead(u.clone(), b.ahead, b.behind),
        Some(u) if b.behind > 0 => PushState::Behind(u.clone(), b.behind),
        Some(u) => PushState::UpToDate(u.clone()),
    }
}

/// Branches of the GitHub remote, the default branch first.
fn base_options(repo: &RepoState, default: &str) -> Vec<(String, String)> {
    let mut names: Vec<String> = Vec::new();
    if !default.is_empty() {
        names.push(default.to_owned());
    }
    if let Some(snap) = repo.snapshot.as_ref() {
        for r in &snap.remotes {
            if gitvis_core::github::parse_remote(&r.url).as_ref() != repo.github_remote.as_ref() {
                continue;
            }
            for b in &r.branches {
                if b.branch != "HEAD" && !names.contains(&b.branch) {
                    names.push(b.branch.clone());
                }
                if names.len() >= 400 {
                    break;
                }
            }
        }
    }
    names.into_iter().map(|n| (n.clone(), n)).collect()
}

/// Suggestions status under the people and label inputs.
fn suggestions_note(ui: &mut Ui, s: &Fetch<Suggestions>, actions: &mut Vec<Action>) {
    if s.loading {
        kit::loading(ui, "Loading people, labels and teams to suggest…");
    } else if let Some(e) = &s.error {
        ui.horizontal_wrapped(|ui| {
            kit::muted(ui, &format!("No suggestions: {e}"));
            if kit::link(ui, "Retry").clicked() {
                actions.push(Act::LoadSuggestions.into());
            }
        });
    } else if let Some(d) = &s.data {
        let mut text = format!(
            "Suggestions: {} people from {}, {} labels",
            d.users.len(),
            d.users_source,
            d.labels.len()
        );
        if !d.teams.is_empty() {
            text.push_str(&format!(", {} teams", d.teams.len()));
        }
        text.push('.');
        if !d.teams_note.is_empty() && d.teams.is_empty() {
            text.push_str(&format!(" Teams: {}", d.teams_note));
        }
        kit::muted(ui, &text);
    }
}

pub fn new_pr(ui: &mut Ui, repo: &mut RepoState, _now: i64, actions: &mut Vec<Action>) {
    let meta = repo.devops.review.meta();
    let full_name = repo
        .github_remote
        .as_ref()
        .map(|(o, r)| format!("{o}/{r}"))
        .unwrap_or_default();
    let head = repo
        .devops
        .review
        .new_pr
        .as_ref()
        .map(|f| f.head.clone())
        .unwrap_or_default();
    let push = push_state(repo, &head);
    let bases = base_options(repo, &meta.default_branch);
    let changing = repo.devops.review.changing.is_some();
    let org = meta.owner_is_org.unwrap_or(false);
    let viewer = repo.devops.review.viewer().map(str::to_owned);
    let super::State {
        new_pr,
        suggestions,
        ..
    } = &mut repo.devops.review;
    let Some(f) = new_pr.as_mut() else {
        return;
    };
    let sugg = suggestions.data.clone().unwrap_or_default();
    scroll_pane(ui, "rv_new_pr", |ui| {
        let p = *pal(ui);
        kit::title(ui, "New pull request");
        kit::secondary(
            ui,
            &format!(
                "Opens a pull request on {full_name} from your current branch. Nothing is sent until you confirm."
            ),
        );

        kit::section(ui, "Branches", None);
        form::card(ui, Tone::Neutral, |ui| {
            form::row(ui, 90.0, "Base", |ui| {
                if bases.is_empty() {
                    form::text(ui, "rv_new_pr_base", &mut f.base, "Base branch");
                } else {
                    if form::select(
                        ui,
                        "rv_new_pr_base_sel",
                        &mut f.base,
                        &bases,
                        ui.available_width().min(280.0),
                    ) {
                        actions.push(Act::SetBase(f.base.clone()).into());
                    }
                }
            });
            ui.add_space(4.0);
            form::row(ui, 90.0, "Head", |ui| {
                if f.head.is_empty() {
                    ui.label(
                        RichText::new("No branch is checked out (detached HEAD)").color(p.danger),
                    );
                } else {
                    kit::badge(ui, &f.head, Tone::Neutral);
                    if f.head_ref != f.head {
                        ui.label(
                            RichText::new(format!("sent as {}", f.head_ref))
                                .size(size::SMALL)
                                .color(p.text_2),
                        );
                    }
                }
            });
            ui.add_space(space::S);
            match &push {
                PushState::NoBranch => {}
                PushState::NotPushed => push_callout(
                    ui,
                    Tone::Warning,
                    &format!("{} is not on GitHub yet", f.head),
                    "GitHub can only open a pull request for a pushed branch. Push it (with an upstream) first.",
                    actions,
                ),
                PushState::Gone(u) => push_callout(
                    ui,
                    Tone::Warning,
                    &format!("{u} no longer exists"),
                    "The upstream branch was deleted on the remote. Push the branch again.",
                    actions,
                ),
                PushState::Ahead(u, ahead, behind) => push_callout(
                    ui,
                    Tone::Warning,
                    &format!(
                        "{} not pushed to {u}",
                        format::plural(*ahead, "commit is", "commits are")
                    ),
                    &if *behind > 0 {
                        format!(
                            "The pull request would miss them. {u} also has {behind} commits you have not pulled."
                        )
                    } else {
                        "The pull request would miss them. Push first.".to_owned()
                    },
                    actions,
                ),
                PushState::Behind(u, behind) => kit::callout(
                    ui,
                    Tone::Neutral,
                    &format!(
                        "{u} has {} you have not pulled",
                        format::plural(*behind, "commit", "commits")
                    ),
                    "Everything local is pushed; the pull request uses the branch as it is on GitHub.",
                ),
                PushState::UpToDate(u) => kit::callout(
                    ui,
                    Tone::Success,
                    &format!("Pushed and up to date with {u}"),
                    "",
                ),
            }
            ui.add_space(space::S);
            match &f.commits {
                None => kit::loading(ui, "Reading the commits since the base…"),
                Some(Err(e)) => {
                    kit::muted(ui, &format!("Commits since the base: {e}"));
                }
                Some(Ok(c)) if c.is_empty() => kit::callout(
                    ui,
                    Tone::Warning,
                    &format!("No commits between {} and {}", f.base_ref, f.head),
                    "There is nothing to merge yet.",
                ),
                Some(Ok(c)) => {
                    kit::secondary(
                        ui,
                        &format!(
                            "{} since {} (git log {}..{}):",
                            format::plural(c.len(), "commit", "commits"),
                            f.base_ref,
                            f.base_ref,
                            f.head
                        ),
                    );
                    for (sha, subject) in c.iter().take(8) {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            ui.label(kit::mono(&sha[..sha.len().min(7)]).color(p.text_3));
                            ui.label(RichText::new(subject).size(size::SMALL).color(p.text));
                        });
                    }
                    if c.len() > 8 {
                        kit::muted(ui, &format!("and {} more", c.len() - 8));
                    }
                }
            }
        });

        kit::section(ui, "Title and description", None);
        form::field(ui, "Title", None, |ui| {
            if form::text(
                ui,
                "rv_new_pr_title",
                &mut f.title,
                "What this pull request does",
            )
            .changed()
            {
                f.title_edited = true;
            }
        });
        let templates: Vec<String> = match &f.templates {
            Some(Ok(t)) => t.iter().map(|t| t.path.clone()).collect(),
            _ => Vec::new(),
        };
        if templates.len() > 1 {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                ui.label(RichText::new("Template").size(size::SMALL).color(p.text_2));
                for (i, path) in templates.iter().enumerate() {
                    let name = path.rsplit('/').next().unwrap_or(path);
                    if kit::chip(ui, name, f.template == Some(i)).clicked() {
                        actions.push(Act::UseTemplate(i).into());
                    }
                }
            });
        }
        let help = match (&f.templates, f.template) {
            (Some(Ok(t)), Some(i)) if !t.is_empty() => {
                format!("Started from {}.", t[i].path)
            }
            (Some(Ok(t)), _) if t.is_empty() => {
                "No pull request template in .github/, docs/ or the root.".to_owned()
            }
            _ => String::new(),
        };
        form::field(ui, "Description", Some(&help), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("rv_new_pr_body_scroll")
                .max_height(380.0)
                .show(ui, |ui| {
                    form::area(ui, "rv_new_pr_body", &mut f.body, "Why and how", 10);
                });
        });
        let has_commits = matches!(&f.commits, Some(Ok(c)) if !c.is_empty());
        if form::button(ui, "Generate from commits", has_commits)
            .on_hover_text("Adds one bullet per commit subject (git log base..head, read locally)")
            .clicked()
        {
            actions.push(Act::GenerateBody.into());
        }

        kit::section(ui, "Reviewers, assignees and labels", None);
        form::field(ui, "Reviewers", None, |ui| {
            form::tags(
                ui,
                "rv_new_pr_reviewers",
                &mut f.reviewers,
                &mut f.reviewer_buf,
                "Type a login, Enter to add",
                &sugg.users,
            );
        });
        if org {
            form::field(ui, "Team reviewers", None, |ui| {
                form::tags(
                    ui,
                    "rv_new_pr_teams",
                    &mut f.teams,
                    &mut f.team_buf,
                    "Team slug, e.g. core",
                    &sugg.teams,
                );
            });
        }
        form::field(ui, "Assignees", None, |ui| {
            form::tags(
                ui,
                "rv_new_pr_assignees",
                &mut f.assignees,
                &mut f.assignee_buf,
                "Type a login, Enter to add",
                &sugg.users,
            );
            if let Some(me) = &viewer
                && !f.assignees.contains(me)
                && kit::link(ui, "Assign to me").clicked()
            {
                f.assignees.push(me.clone());
            }
        });
        form::field(ui, "Labels", None, |ui| {
            form::tags(
                ui,
                "rv_new_pr_labels",
                &mut f.labels,
                &mut f.label_buf,
                "Label name, Enter to add",
                &sugg.label_names(),
            );
        });
        suggestions_note(ui, suggestions, actions);
        ui.add_space(space::S);
        form::checkbox(ui, &mut f.draft, "Open as a draft");
        kit::muted(
            ui,
            "A draft cannot be merged and does not ask reviewers for a review until it is marked ready.",
        );
        form::error(ui, &f.error);
        let ready = !f.title.trim().is_empty() && !f.head.is_empty() && !changing;
        form::actions_row(ui, |ui| {
            let label = if f.draft {
                "Create draft pull request"
            } else {
                "Create pull request"
            };
            if form::primary(ui, label, ready).clicked() {
                actions.push(Act::CreatePr.into());
            }
            if form::button(ui, "Cancel", true)
                .on_hover_text("Close the form and keep what you typed")
                .clicked()
            {
                actions.push(Act::CloseForm.into());
            }
            if form::button(ui, "Discard", true).clicked() {
                actions.push(Act::DiscardForm.into());
            }
        });
    });
}

fn push_callout(ui: &mut Ui, tone: Tone, title: &str, body: &str, actions: &mut Vec<Action>) {
    kit::callout_with(ui, tone, title, body, |ui| {
        ui.add_space(4.0);
        if form::button(ui, "Push first…", true).clicked() {
            actions.push(Act::PushFirst.into());
        }
    });
}

pub fn new_issue(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) {
    let full_name = repo
        .github_remote
        .as_ref()
        .map(|(o, r)| format!("{o}/{r}"))
        .unwrap_or_default();
    let changing = repo.devops.review.changing.is_some();
    let viewer = repo.devops.review.viewer().map(str::to_owned);
    let super::State {
        issues,
        suggestions,
        ..
    } = &mut repo.devops.review;
    let Some(f) = issues.form.as_mut() else {
        return;
    };
    let sugg = suggestions.data.clone().unwrap_or_default();
    scroll_pane(ui, "rv_new_issue", |ui| {
        let p = *pal(ui);
        kit::title(ui, "New issue");
        kit::secondary(
            ui,
            &format!("Opens an issue on {full_name}. Nothing is sent until you confirm."),
        );
        issue_templates(ui, f, actions);
        kit::section(ui, "Issue", None);
        form::field(ui, "Title", None, |ui| {
            form::text(ui, "rv_new_issue_title", &mut f.title, "A short summary");
        });
        form::field(ui, "Description", None, |ui| {
            form::area(
                ui,
                "rv_new_issue_body",
                &mut f.body,
                "Steps, what happened, what you expected",
                10,
            );
        });
        form::field(ui, "Labels", None, |ui| {
            form::tags(
                ui,
                "rv_new_issue_labels",
                &mut f.labels,
                &mut f.label_buf,
                "Label name, Enter to add",
                &sugg.label_names(),
            );
        });
        form::field(ui, "Assignees", None, |ui| {
            form::tags(
                ui,
                "rv_new_issue_assignees",
                &mut f.assignees,
                &mut f.assignee_buf,
                "Type a login, Enter to add",
                &sugg.users,
            );
            if let Some(me) = &viewer
                && !f.assignees.contains(me)
                && kit::link(ui, "Assign to me").clicked()
            {
                f.assignees.push(me.clone());
            }
        });
        suggestions_note(ui, suggestions, actions);
        form::error(ui, &f.error);
        let _ = p;
        form::actions_row(ui, |ui| {
            if form::primary(ui, "Create issue", !f.title.trim().is_empty() && !changing).clicked()
            {
                actions.push(Act::CreateIssue.into());
            }
            if form::button(ui, "Cancel", true)
                .on_hover_text("Close the form and keep what you typed")
                .clicked()
            {
                actions.push(Act::CloseForm.into());
            }
            if form::button(ui, "Discard", true).clicked() {
                actions.push(Act::DiscardForm.into());
            }
        });
    });
}

fn issue_templates(ui: &mut Ui, f: &NewIssueForm, actions: &mut Vec<Action>) {
    let Some(t) = &f.templates else {
        kit::loading(ui, "Looking for issue templates…");
        return;
    };
    if t.is_empty() {
        kit::muted(
            ui,
            "No Markdown issue templates in .github/ISSUE_TEMPLATE/.",
        );
        return;
    }
    kit::section(ui, "Template", Some(t.len()));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for (i, tpl) in t.iter().enumerate() {
            let r = kit::chip(ui, &tpl.name, f.template == Some(i));
            let r = kit::tip(r, &tpl.about);
            if r.clicked() {
                actions.push(Act::UseIssueTemplate(i).into());
            }
        }
    });
    if let Some(i) = f.template
        && let Some(tpl) = t.get(i)
        && !tpl.about.is_empty()
    {
        kit::secondary(ui, &tpl.about);
    }
}
