//! The Issues side of the Review page: list with chips, label and search
//! filters; detail with body, comments, labels and assignees, commits that
//! mention it, a branch for it; the comment box; the New issue form.

use egui::{Align, Frame, Layout, Margin, RichText, Ui, vec2};
use hadion_core::Oid;
use hadion_core::devops::review::rest::CloseReason;
use hadion_core::devops::review::{Issue, IssueComment, local};

use super::view::{count_chip, detail_area, list_pane, scroll_pane, search_field};
use super::widgets::{self, ROW3_H, person, rel};
use super::{Act, BranchForm, IssueFilter, State, forms};
use crate::format;
use crate::state::{Action, RepoState, Settings};
use crate::theme::{Tone, pal, size, space};
use crate::views::form;
use crate::views::icons::{self, Icon, IconButton};
use crate::views::kit;

/// Commits shown under "Commits that mention #N".
const MAX_MENTIONS: usize = 60;

pub fn page(
    ui: &mut Ui,
    repo: &mut RepoState,
    _settings: &Settings,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let meta = repo.devops.review.meta();
    let full_name = repo
        .github_remote
        .as_ref()
        .map(|(o, r)| format!("{o}/{r}"))
        .unwrap_or_default();
    if meta.has_issues == Some(false) {
        detail_area(ui, |ui| {
            Frame::NONE.inner_margin(Margin::same(24)).show(ui, |ui| {
                if kit::empty_state(
                    ui,
                    Icon::Issue,
                    &format!("Issues are turned off for {full_name}"),
                    "The repository's settings hide issues on GitHub (projects like this one often track them in another repository). Pull requests are on the other tab.",
                    Some((Icon::External, "Open the repository on GitHub")),
                ) {
                    actions.push(Action::OpenUrl(format!("https://github.com/{full_name}")));
                }
            });
        });
        return;
    }
    list_pane(ui, "rv_issue_list", |ui| {
        issue_list(ui, &mut repo.devops.review, now, actions)
    });
    detail_area(ui, |ui| {
        let st = &repo.devops.review;
        if st.issues.form_open && st.issues.form.is_some() {
            forms::new_issue(ui, repo, actions);
            return;
        }
        match st.issues.selected {
            Some(n) => issue_detail(ui, repo, n, now, actions),
            None => {
                Frame::NONE.inner_margin(Margin::same(24)).show(ui, |ui| {
                    kit::empty_state(
                        ui,
                        Icon::Issue,
                        "Select an issue",
                        "Its description, comments, labels and assignees, and the commits that mention it appear here, with a branch to start on it.",
                        None,
                    );
                });
            }
        }
    });
}

fn issue_list(ui: &mut Ui, st: &mut State, now: i64, actions: &mut Vec<Action>) {
    let current_len = st
        .issues
        .current()
        .and_then(|l| l.data.as_ref())
        .map(|l| l.len() as u64);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for f in IssueFilter::ALL {
            let on = st.issues.filter == f;
            let count = if on { current_len } else { None };
            if count_chip(ui, f.label(), count, on) && !on {
                actions.push(Act::SetIssueFilter(f).into());
            }
        }
    });
    ui.add_space(space::S);
    // Label filter: labels the repository has (suggestions) or the loaded issues carry.
    let mut labels: Vec<String> = st
        .suggestions
        .data
        .as_ref()
        .map(|s| s.label_names())
        .unwrap_or_default();
    for l in st
        .issues
        .lists
        .values()
        .filter_map(|f| f.data.as_ref())
        .flatten()
    {
        for lab in &l.labels {
            if !labels.contains(&lab.name) {
                labels.push(lab.name.clone());
            }
        }
    }
    labels.sort_by_key(|l| l.to_lowercase());
    let mut options: Vec<(Option<String>, String)> = vec![(None, "Any label".to_owned())];
    options.extend(labels.into_iter().map(|l| (Some(l.clone()), l)));
    let mut label = st.issues.label.clone();
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Label")
                .size(size::SMALL)
                .color(pal(ui).text_2),
        );
        if form::select(
            ui,
            "rv_issue_label",
            &mut label,
            &options,
            ui.available_width().min(220.0),
        ) {
            actions.push(Act::SetIssueLabel(label.clone()).into());
        }
    });
    ui.add_space(space::S);
    let hint = format!(
        "Filter {}",
        format::plural(
            current_len.unwrap_or(0) as usize,
            "loaded issue",
            "loaded issues"
        )
    );
    search_field(ui, "rv_issue_search", &mut st.issues.search, &hint);
    ui.add_space(space::S);
    let Some(list) = st.issues.current() else {
        widgets::loading_block(ui, "Loading issues…", "");
        return;
    };
    if list.data.is_none() {
        if list.loading {
            widgets::loading_block(
                ui,
                "Loading issues…",
                "GET issues, newest update first (pull requests left out).",
            );
        } else if let Some(e) = &list.error
            && widgets::error_block(ui, "Could not load the issues", e)
        {
            actions.push(Act::Refresh.into());
        }
        return;
    }
    if let Some(e) = &list.error {
        kit::callout(ui, Tone::Danger, "The last refresh failed", e);
    }
    let visible: Vec<Issue> = st.issues.visible().into_iter().cloned().collect();
    if visible.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            kit::muted(
                ui,
                &if !st.issues.search.trim().is_empty() {
                    "No loaded issue matches the filter.".to_owned()
                } else {
                    match st.issues.filter {
                        IssueFilter::Open => "No open issues. Open one with New issue.".to_owned(),
                        IssueFilter::Closed => "No closed issues.".to_owned(),
                        IssueFilter::Assigned => "Nothing is assigned to you.".to_owned(),
                        IssueFilter::Created => {
                            "You have not opened any open issue here.".to_owned()
                        }
                        IssueFilter::Mentioned => "No open issue mentions you.".to_owned(),
                    }
                },
            );
        });
        return;
    }
    let selected = st.issues.selected;
    kit::list(
        ui,
        "rv_issue_rows",
        visible.len(),
        ROW3_H,
        |i| selected == Some(visible[i].number),
        |ui, rect, resp, i| {
            let issue = &visible[i];
            widgets::paint_issue_row(ui, rect, issue, selected == Some(issue.number), now);
            if resp.clicked() {
                actions.push(Act::OpenIssue(issue.number).into());
            }
            resp.context_menu(|ui| {
                ui.set_min_width(200.0);
                if ui.button("Open on GitHub").clicked() {
                    actions.push(Action::OpenUrl(issue.url.clone()));
                    ui.close();
                }
                if ui.button("Copy URL").clicked() {
                    actions.push(Action::Copy(issue.url.clone()));
                    ui.close();
                }
                if ui
                    .button(format!("Copy “Fixes #{}”", issue.number))
                    .clicked()
                {
                    actions.push(Action::Copy(format!("Fixes #{}", issue.number)));
                    ui.close();
                }
            });
        },
    );
}

/// Commits of the loaded graph whose summary mentions `#n`.
fn mentioning(repo: &RepoState, n: u64) -> Vec<(Oid, String, String, i64)> {
    let Some(snap) = repo.snapshot.as_ref() else {
        return Vec::new();
    };
    snap.commits
        .iter()
        .filter(|c| local::mentions(&c.summary, n))
        .take(MAX_MENTIONS + 1)
        .map(|c| (c.id, c.short.clone(), c.summary.clone(), c.time))
        .collect()
}

fn issue_detail(ui: &mut Ui, repo: &mut RepoState, n: u64, now: i64, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let st = &repo.devops.review;
    let detail = st.issues.details.get(&n);
    let issue = st.issues.find(n).cloned();
    let comments: Vec<IssueComment> = detail
        .and_then(|d| d.data.as_ref())
        .map(|(_, c)| c.clone())
        .unwrap_or_default();
    let loading = detail.is_some_and(|d| d.loading);
    let has_detail = detail.is_some();
    let error = detail.and_then(|d| d.error.clone());
    let changing = st.changing.is_some();
    let default_branch = st.meta().default_branch;
    let current_branch = repo
        .snapshot
        .as_ref()
        .and_then(|s| s.current_branch())
        .map(|b| b.name.clone());
    let commits = mentioning(repo, n);
    let Some(issue) = issue else {
        Frame::NONE
            .inner_margin(Margin::same(28))
            .show(ui, |ui| match error {
                Some(e) => {
                    if widgets::error_block(ui, &format!("Could not load issue #{n}"), &e) {
                        actions.push(Act::RefreshIssue(n).into());
                    }
                }
                None => widgets::loading_block(ui, &format!("Loading issue #{n}…"), ""),
            });
        return;
    };
    let sugg = repo
        .devops
        .review
        .suggestions
        .data
        .clone()
        .unwrap_or_default();
    let issues = &mut repo.devops.review.issues;
    scroll_pane(ui, ("rv_issue", n), |ui| {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(18.0, 22.0), egui::Sense::hover());
            let color = if issue.open {
                p.success
            } else if issue.state_reason == "not_planned" {
                p.text_3
            } else {
                p.purple
            };
            icons::paint(ui.painter(), Icon::Issue, r.center(), 16.0, color);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if loading {
                    ui.add(egui::Spinner::new().size(14.0));
                } else if IconButton::icon(Icon::Fetch)
                    .compact()
                    .show(ui)
                    .on_hover_text("Reload this issue")
                    .clicked()
                {
                    actions.push(Act::RefreshIssue(n).into());
                }
                if IconButton::icon(Icon::Copy)
                    .compact()
                    .show(ui)
                    .on_hover_text("Copy the URL")
                    .clicked()
                {
                    actions.push(Action::Copy(issue.url.clone()));
                }
                if IconButton::icon(Icon::External)
                    .compact()
                    .show(ui)
                    .on_hover_text("Open on GitHub")
                    .clicked()
                {
                    actions.push(Action::OpenUrl(issue.url.clone()));
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    kit::title(ui, &issue.title);
                });
            });
        });
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
            ui.add_space(26.0);
            ui.label(kit::mono(format!("#{n}")).color(p.text_2));
            let tone = if issue.open {
                Tone::Success
            } else {
                Tone::Purple
            };
            kit::badge(ui, issue.state_label(), tone);
            ui.label(
                RichText::new(format!(
                    "{} opened this {}  ·  {}",
                    if issue.author.is_empty() {
                        "ghost"
                    } else {
                        &issue.author
                    },
                    rel(issue.created, now),
                    format::plural(issue.comments as usize, "comment", "comments")
                ))
                .size(size::SMALL)
                .color(p.text_2),
            );
        });
        if let Some(e) = &error {
            kit::callout(ui, Tone::Danger, "The last reload failed", e);
        }

        // Details and editing.
        let editing = issues.edit.as_ref().is_some_and(|e| e.number == n);
        kit::section_with(ui, "Details", None, |ui| {
            if !editing
                && IconButton::new(Icon::Edit, "Edit labels and assignees")
                    .compact()
                    .show(ui)
                    .clicked()
            {
                actions.push(Act::EditIssue(n).into());
            }
        });
        let mut cancel_edit = false;
        if editing && let Some(e) = issues.edit.as_mut() {
            form::card(ui, Tone::Accent, |ui| {
                form::field(ui, "Labels", None, |ui| {
                    form::tags(
                        ui,
                        ("rv_issue_edit_labels", n),
                        &mut e.labels,
                        &mut e.label_buf,
                        "Label name, Enter to add",
                        &sugg.label_names(),
                    );
                });
                form::field(ui, "Assignees", None, |ui| {
                    form::tags(
                        ui,
                        ("rv_issue_edit_assignees", n),
                        &mut e.assignees,
                        &mut e.assignee_buf,
                        "Login, Enter to add",
                        &sugg.users,
                    );
                });
                form::actions_row(ui, |ui| {
                    if form::primary(ui, "Save", !changing).clicked() {
                        actions.push(Act::SaveIssueEdit(n).into());
                    }
                    if form::button(ui, "Cancel", true).clicked() {
                        cancel_edit = true;
                    }
                });
            });
        } else {
            kit::kv(ui, 110.0, |kv| {
                if issue.labels.is_empty() {
                    kv.muted("Labels", "none");
                } else {
                    kv.row("Labels", |ui| widgets::label_chips(ui, &issue.labels));
                }
                if issue.assignees.is_empty() {
                    kv.muted("Assignees", "nobody");
                } else {
                    kv.text("Assignees", issue.assignees.join(", "));
                }
                match &issue.milestone {
                    Some(m) => kv.text("Milestone", m.as_str()),
                    None => kv.muted("Milestone", "none"),
                }
                if let Some(c) = issue.created {
                    kv.text("Opened", widgets::when(c, now));
                }
                if let Some(u) = issue.updated {
                    kv.text("Updated", widgets::when(u, now));
                }
                if let Some(c) = issue.closed {
                    kv.text(
                        "Closed",
                        format!(
                            "{}{}",
                            widgets::when(c, now),
                            issue
                                .closed_by
                                .as_deref()
                                .map(|b| format!(" by {b}"))
                                .unwrap_or_default()
                        ),
                    );
                }
                if issue.locked {
                    kv.text("Locked", "Only collaborators can comment");
                }
            });
        }

        if cancel_edit {
            issues.edit = None;
        }

        // Working on it.
        kit::section(ui, "Work on it", None);
        let b = issues.branch.get_or_insert_with(|| BranchForm {
            number: n,
            name: local::issue_branch_name(n, &issue.title),
            from_current: false,
        });
        if b.number != n {
            *b = BranchForm {
                number: n,
                name: local::issue_branch_name(n, &issue.title),
                from_current: false,
            };
        }
        form::card(ui, Tone::Neutral, |ui| {
            form::field(
                ui,
                "Branch name",
                Some("Number and title, the way GitHub's own “Create a branch” names it."),
                |ui| {
                    form::input(
                        ui,
                        ("rv_issue_branch", n),
                        &mut b.name,
                        form::InputOpts::new("branch name").mono(),
                    );
                },
            );
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                let from_default = if default_branch.is_empty() {
                    "From the default branch".to_owned()
                } else {
                    format!("From {default_branch}")
                };
                if form::radio(ui, !b.from_current, &from_default) {
                    b.from_current = false;
                }
                if let Some(cur) = &current_branch
                    && form::radio(
                        ui,
                        b.from_current,
                        &format!("From the current branch ({cur})"),
                    )
                {
                    b.from_current = true;
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if form::primary(ui, "Create branch…", !b.name.trim().is_empty()).clicked() {
                    actions.push(Act::StartBranch(n).into());
                }
                let fixes = format!("Fixes #{n}");
                if kit::chip(ui, &fixes, false)
                    .on_hover_text("Copy: GitHub closes the issue when a pull request with this line in its description is merged")
                    .clicked()
                {
                    actions.push(Action::Copy(fixes));
                }
            });
        });

        // Commits that mention it.
        kit::section(
            ui,
            &format!("Commits that mention #{n}"),
            Some(commits.len().min(MAX_MENTIONS)),
        );
        if commits.is_empty() {
            kit::muted(
                ui,
                &format!("No commit in the loaded graph mentions #{n} in its summary."),
            );
        } else {
            for (oid, short, summary, time) in commits.iter().take(MAX_MENTIONS) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if kit::link(ui, short).clicked() {
                        actions.push(Action::JumpTo(*oid));
                    }
                    ui.add(
                        egui::Label::new(RichText::new(summary).size(size::SMALL).color(p.text))
                            .truncate(),
                    );
                    ui.label(
                        RichText::new(format::relative(*time, now))
                            .size(size::SMALL)
                            .color(p.text_3),
                    );
                });
            }
            if commits.len() > MAX_MENTIONS {
                kit::muted(ui, &format!("Showing the newest {MAX_MENTIONS}."));
            }
        }

        // Description.
        kit::section(ui, "Description", None);
        widgets::prose(ui, &issue.body, &issue.url, actions);

        // Comments.
        kit::section(ui, "Comments", Some(comments.len()));
        if !has_detail || (loading && comments.is_empty()) {
            kit::loading(ui, "Loading comments…");
        } else if comments.is_empty() {
            kit::muted(ui, "No comments yet.");
        }
        for c in &comments {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                person(ui, &c.author, true);
                ui.label(RichText::new("commented").size(size::SMALL).color(p.text_2));
                ui.label(
                    RichText::new(rel(c.created, now))
                        .size(size::SMALL)
                        .color(p.text_3),
                );
            });
            ui.horizontal(|ui| {
                ui.add_space(28.0);
                ui.vertical(|ui| widgets::comment_text(ui, &c.body));
            });
        }

        kit::section(ui, "Add a comment", None);
        let buf = issues.comment.entry(n).or_default();
        form::area(
            ui,
            ("rv_issue_comment", n),
            buf,
            "Write a comment (Markdown is fine)",
            4,
        );
        let has_text = !buf.trim().is_empty();
        form::actions_row(ui, |ui| {
            if form::primary(ui, "Comment", has_text && !changing).clicked() {
                actions.push(Act::IssueComment(n).into());
            }
            if issue.open {
                for reason in [CloseReason::NotPlanned, CloseReason::Completed] {
                    if form::button(ui, reason.label(), !changing).clicked() {
                        actions.push(
                            Act::SetIssueState {
                                number: n,
                                open: false,
                                reason,
                            }
                            .into(),
                        );
                    }
                }
            } else if form::button(ui, "Reopen issue", !changing).clicked() {
                actions.push(
                    Act::SetIssueState {
                        number: n,
                        open: true,
                        reason: CloseReason::Completed,
                    }
                    .into(),
                );
            }
        });
    });
}
