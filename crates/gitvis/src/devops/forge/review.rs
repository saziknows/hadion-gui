//! The Review page on GitLab (merge requests) and Azure DevOps (pull
//! requests): list with filters, detail (description, branches, approvals
//! or votes, pipelines, discussions, changed files), create, merge /
//! complete, approve / vote, comment.

use egui::{Frame, Margin, RichText, Ui, vec2};

use gitvis_core::devops::forge::azure::{self, Vote};
use gitvis_core::devops::forge::model::{ChangeRequest, thread_stats};
use gitvis_core::devops::forge::{ForgeKind, ForgeRemote};

use super::{Act, CreateForm, Detail, ReviewView, State, gate};
use crate::format;
use crate::state::{Action, RepoState};
use crate::theme::{Tone, pal, size, space};
use crate::views::form;
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Col, Leading, Row};

/// `!12` on GitLab, `#12` on Azure DevOps.
pub fn reference(kind: ForgeKind, n: u64) -> String {
    match kind {
        ForgeKind::GitLab => format!("!{n}"),
        ForgeKind::Azure => format!("#{n}"),
    }
}

/// Badge for a merge status.
pub fn merge_badge(cr: &ChangeRequest) -> Option<(String, Tone)> {
    if cr.conflicts {
        return Some(("Conflicts".into(), Tone::Danger));
    }
    let s = cr.merge_status.as_str();
    Some(match s {
        "mergeable" | "can_be_merged" | "succeeded" => ("Mergeable".into(), Tone::Success),
        "not_approved" => ("Needs approval".into(), Tone::Warning),
        "ci_still_running" => ("Pipeline running".into(), Tone::Accent),
        "ci_must_pass" => ("Pipeline must pass".into(), Tone::Warning),
        "discussions_not_resolved" => ("Unresolved threads".into(), Tone::Warning),
        "need_rebase" => ("Needs rebase".into(), Tone::Warning),
        "draft_status" => ("Draft".into(), Tone::Neutral),
        "blocked_status" => ("Blocked".into(), Tone::Warning),
        "conflict" | "cannot_be_merged" => ("Conflicts".into(), Tone::Danger),
        "checking" | "unchecked" | "queued" => ("Checking".into(), Tone::Neutral),
        "" => return None,
        other => (
            sentence(&gitvis_core::devops::forge::model::words(other)),
            Tone::Neutral,
        ),
    })
}

/// `requested changes` → `Requested changes`.
pub fn sentence(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// Azure votes summary: `2 approved`, `1 rejected`.
fn vote_badges(cr: &ChangeRequest) -> Vec<(String, Tone)> {
    let count = |f: &dyn Fn(i64) -> bool| {
        cr.reviewers
            .iter()
            .filter(|r| r.vote.is_some_and(f))
            .count()
    };
    let mut out = Vec::new();
    let approved = count(&|v| v >= 5);
    let rejected = count(&|v| v == -10);
    let waiting = count(&|v| v == -5);
    if approved > 0 {
        out.push((format!("{approved} approved"), Tone::Success));
    }
    if waiting > 0 {
        out.push((format!("{waiting} waiting for author"), Tone::Warning));
    }
    if rejected > 0 {
        out.push((format!("{rejected} rejected"), Tone::Danger));
    }
    out
}

fn visible(st: &State) -> Vec<&ChangeRequest> {
    let me = st.me();
    let t = st.filter.trim().to_lowercase();
    st.reviews
        .data
        .as_ref()
        .map(|v| {
            v.iter()
                .filter(|c| match st.view {
                    ReviewView::All => true,
                    ReviewView::Mine => c.author.is(me),
                    ReviewView::Ready => !c.draft,
                    ReviewView::Drafts => c.draft,
                })
                .filter(|c| {
                    t.is_empty()
                        || c.title.to_lowercase().contains(&t)
                        || c.source.to_lowercase().contains(&t)
                        || c.author.display().to_lowercase().contains(&t)
                        || c.number.to_string() == t.trim_start_matches(['!', '#'])
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Numbers of the listed requests in display order (script `mr N`).
pub fn visible_numbers(repo: &RepoState) -> Vec<u64> {
    visible(&repo.devops.forge)
        .iter()
        .map(|c| c.number)
        .collect()
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) {
    let Some(remote) = repo.forge_remote.clone() else {
        return;
    };
    let title = match remote.kind {
        ForgeKind::GitLab => "Merge requests",
        ForgeKind::Azure => "Pull requests",
    };
    if !gate::show(ui, &remote, &repo.devops.forge, title, actions) {
        return;
    }
    header(ui, &remote, &repo.devops.forge, title, actions);
    let remote_branches: Vec<String> = repo
        .snapshot
        .as_ref()
        .map(|s| {
            s.remotes
                .iter()
                .filter(|r| r.name == "origin")
                .flat_map(|r| r.branches.iter().map(|b| b.branch.clone()))
                .filter(|b| b != "HEAD")
                .collect()
        })
        .unwrap_or_default();
    let st = &mut repo.devops.forge;
    if let Some(form) = st.create.as_mut() {
        create_form(ui, &remote, form, &remote_branches, actions);
        return;
    }
    list_pane(ui, &remote, st, actions);
    let Some(n) = st.selected else {
        empty(ui, |ui| {
            kit::empty_state(
                ui,
                Icon::PullRequest,
                &format!("Select a {}", remote.kind.review_noun()),
                match remote.kind {
                    ForgeKind::GitLab => {
                        "Its description, approvals, pipelines, discussions and changed files appear here."
                    }
                    ForgeKind::Azure => {
                        "Its description, reviewers and votes, and completion options appear here."
                    }
                },
                None,
            );
        });
        return;
    };
    let listed = st
        .reviews
        .data
        .as_ref()
        .and_then(|v| v.iter().find(|c| c.number == n).cloned());
    let can_write = st.can_write();
    let me = st.me().to_owned();
    let State {
        details,
        merge,
        comment,
        writing,
        ..
    } = st;
    let detail = details.entry(n).or_default();
    detail_pane(ui, ("forge_review", n), |ui| {
        let cr = detail
            .cr
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .cloned()
            .or(listed);
        if let Some(Err(e)) = &detail.cr {
            let mut retry = false;
            kit::callout_with(ui, Tone::Danger, "Could not load the details", e, |ui| {
                retry = IconButton::new(Icon::Fetch, "Retry")
                    .compact()
                    .show(ui)
                    .clicked();
            });
            if retry {
                actions.push(Act::RefreshReview(n).into());
            }
        }
        let Some(cr) = cr else {
            kit::loading(ui, &format!("Loading {}…", reference(remote.kind, n)));
            return;
        };
        detail_view(ui, &remote, &cr, detail, can_write, &me, actions);
        if can_write {
            merge_card(ui, &remote, &cr, merge, writing, actions);
            if remote.kind == ForgeKind::GitLab {
                comment_card(ui, &cr, comment, writing, actions);
            }
        } else {
            kit::callout(
                ui,
                Tone::Neutral,
                "Read only",
                "Approving, commenting and merging need a sign-in: run `glab auth login`, then Check again.",
            );
        }
    });
}

fn header(ui: &mut Ui, remote: &ForgeRemote, st: &State, title: &str, actions: &mut Vec<Action>) {
    let now = format::now_secs();
    let mode = gate::mode_line(st);
    let mut sub = vec![format!("{} on {}", remote.path, remote.host)];
    if let Some(v) = &st.reviews.data {
        sub.push(format!("{} open", v.len()));
    }
    if st.reviews.loading {
        sub.push("loading…".into());
    } else if let Some(at) = st.reviews.at {
        sub.push(format!("fetched {}", format::relative(at, now)));
    }
    if !mode.is_empty() {
        sub.push(mode);
    }
    let can_write = st.can_write();
    // "New …", the web link and Refresh, with their gaps.
    let controls_w = 330.0;
    let (subtitle, _) = crate::views::github::fit_subtitle(ui, title, &sub, controls_w);
    kit::page_header(ui, title, &subtitle, |ui| {
        if IconButton::new(Icon::Plus, &format!("New {}", remote.kind.review_noun()))
            .primary(true)
            .enabled(can_write)
            .show(ui)
            .clicked()
        {
            actions.push(Act::NewReview.into());
        }
        if IconButton::icon(Icon::External)
            .show(ui)
            .on_hover_text(format!(
                "Open the {}s on the web",
                remote.kind.review_noun()
            ))
            .clicked()
        {
            let url = match remote.kind {
                ForgeKind::GitLab => format!("{}/-/merge_requests", remote.web_url),
                ForgeKind::Azure => azure::Target::of(remote)
                    .map(|t| t.prs_url())
                    .unwrap_or_else(|| remote.web_url.clone()),
            };
            actions.push(Action::OpenUrl(url));
        }
        if st.reviews.loading {
            ui.add(egui::Spinner::new().size(14.0));
        } else if IconButton::new(Icon::Fetch, "Refresh").show(ui).clicked() {
            actions.push(Act::RefreshReviews.into());
        }
    });
    gate::public_strip(ui, remote, st, actions);
}

fn empty(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas).inner_margin(Margin::same(24)))
        .show(ui, add);
}

pub(super) fn detail_pane(
    ui: &mut Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    add: impl FnOnce(&mut Ui),
) {
    let p = *pal(ui);
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(salt)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    Frame::NONE
                        .inner_margin(Margin {
                            left: 28,
                            right: 28,
                            top: 20,
                            bottom: 28,
                        })
                        .show(ui, |ui| {
                            ui.set_max_width(ui.available_width().min(kit::DETAIL_MAX));
                            ui.spacing_mut().item_spacing.y = 8.0;
                            add(ui)
                        });
                });
        });
}

/// The list pane on the left; at most 45% of the page, so the detail keeps
/// room on a narrow window.
pub(super) fn list_frame(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    let w = ui.available_width();
    egui::Panel::left(id)
        .resizable(true)
        .default_size((w * 0.38).clamp(300.0, 440.0))
        .size_range(300.0..=(w * 0.45).clamp(300.0, 760.0))
        .frame(Frame::NONE.fill(p.panel).inner_margin(Margin {
            left: 8,
            right: 8,
            top: 12,
            bottom: 6,
        }))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui)
        });
}

fn list_pane(ui: &mut Ui, remote: &ForgeRemote, st: &mut State, actions: &mut Vec<Action>) {
    list_frame(ui, "forge_reviews", |ui| {
        let total = st.reviews.data.as_ref().map(|v| v.len());
        kit::list_header(ui, "Open", total, |_| {});
        ui.add_space(space::XS);
        ui.horizontal(|ui| {
            ui.set_max_width(ui.available_width() - 8.0);
            kit::search_box(
                ui,
                "forge_review_filter",
                &mut st.filter,
                "Filter by title, branch, author or number",
            );
        });
        ui.add_space(space::XS);
        let options: Vec<(ReviewView, &str)> =
            ReviewView::ALL.iter().map(|v| (*v, v.label())).collect();
        if let Some(v) = kit::segmented(ui, &options, st.view) {
            actions.push(Act::SetView(v).into());
        }
        ui.add_space(space::S);
        if let Some(e) = &st.reviews.error {
            let mut retry = false;
            kit::callout_with(ui, Tone::Danger, "Could not load the list", e, |ui| {
                retry = IconButton::new(Icon::Fetch, "Retry")
                    .compact()
                    .show(ui)
                    .clicked();
            });
            if retry {
                actions.push(Act::RefreshReviews.into());
            }
        }
        let Some(all) = &st.reviews.data else {
            if st.reviews.loading {
                kit::loading(ui, &format!("Loading {}s…", remote.kind.review_noun()));
            } else if st.reviews.error.is_none() {
                kit::muted(ui, "Not loaded yet.");
            }
            return;
        };
        if all.is_empty() {
            kit::muted(
                ui,
                &format!(
                    "No open {}s on {}. Create one with the button above.",
                    remote.kind.review_noun(),
                    remote.path
                ),
            );
            return;
        }
        let rows = visible(st);
        if rows.is_empty() {
            kit::muted(ui, "Nothing matches the filter.");
            return;
        }
        let now = format::now_secs();
        let selected = st.selected;
        let numbers: Vec<u64> = rows.iter().map(|c| c.number).collect();
        let me = st.me().to_owned();
        kit::list(
            ui,
            "forge_review_list",
            rows.len(),
            kit::ROW_H,
            |i| selected == Some(numbers[i]),
            |ui, rect, resp, i| {
                let p = *pal(ui);
                let c = rows[i];
                let mut badges: Vec<(String, Tone)> = Vec::new();
                if c.draft {
                    badges.push(("Draft".into(), Tone::Neutral));
                }
                if let Some(b) = merge_badge(c).filter(|b| b.0 != "Draft") {
                    badges.push(b);
                }
                badges.extend(vote_badges(c));
                if c.author.is(&me) {
                    badges.push(("Yours".into(), Tone::Purple));
                }
                let sub = format!(
                    "{}  ·  {}  ·  {} → {}",
                    reference(remote.kind, c.number),
                    if c.author.name.is_empty() {
                        c.author.login.clone()
                    } else {
                        c.author.name.clone()
                    },
                    c.source,
                    c.target
                );
                let meta = c
                    .updated
                    .or(c.created)
                    .map(|t| format::relative(t, now))
                    .unwrap_or_default();
                kit::paint_row(
                    ui,
                    rect,
                    &Row::new(&c.title)
                        .leading(Leading::Icon(
                            Icon::PullRequest,
                            if c.draft { p.text_3 } else { p.success },
                        ))
                        .subtitle(&sub)
                        .meta(&meta)
                        .badges(&badges),
                    selected == Some(c.number),
                );
                if resp.clicked() {
                    actions.push(Act::Select(c.number).into());
                }
            },
        );
    });
}

fn people(list: &[gitvis_core::devops::forge::model::Person]) -> String {
    list.iter()
        .map(|p| p.display())
        .collect::<Vec<_>>()
        .join(", ")
}

#[allow(clippy::too_many_arguments)]
fn detail_view(
    ui: &mut Ui,
    remote: &ForgeRemote,
    cr: &ChangeRequest,
    d: &Detail,
    can_write: bool,
    me: &str,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let now = format::now_secs();
    kit::title(ui, &cr.title);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        kit::badge(ui, &reference(remote.kind, cr.number), Tone::Neutral);
        if cr.draft {
            kit::badge(ui, "Draft", Tone::Neutral);
        }
        if !cr.state.is_empty() {
            kit::badge(
                ui,
                &sentence(&gitvis_core::devops::forge::model::words(&cr.state)),
                Tone::Neutral,
            );
        }
        // A draft's merge status is "Draft" too: one badge is enough.
        if let Some((t, tone)) = merge_badge(cr).filter(|b| !(cr.draft && b.0 == "Draft")) {
            kit::badge(ui, &t, tone);
        }
        for (t, tone) in vote_badges(cr) {
            kit::badge(ui, &t, tone);
        }
        if cr.auto_merge {
            kit::badge(
                ui,
                match remote.kind {
                    ForgeKind::GitLab => "Merges when the pipeline succeeds",
                    ForgeKind::Azure => "Auto-complete set",
                },
                Tone::Accent,
            );
        }
        if d.loading {
            ui.add(egui::Spinner::new().size(12.0));
        }
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if IconButton::new(Icon::External, "Open on the web")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::OpenUrl(cr.web_url.clone()));
        }
        if IconButton::new(Icon::Fetch, "Refresh")
            .compact()
            .enabled(!d.loading)
            .show(ui)
            .clicked()
        {
            actions.push(Act::RefreshReview(cr.number).into());
        }
        if let Some(at) = d.at {
            ui.label(
                RichText::new(format!("loaded {}", format::relative(at, now)))
                    .size(size::SMALL)
                    .color(p.text_3),
            );
        }
    });
    kit::section(ui, "Facts", None);
    kit::kv(ui, 130.0, |kv| {
        kv.text("Author", cr.author.display());
        if let Some(t) = cr.created {
            kv.text(
                "Opened",
                format!("{} ({})", format::relative(t, now), format::absolute(t)),
            );
        }
        kv.text("Branches", format!("{}  →  {}", cr.source, cr.target));
        if !cr.sha.is_empty() {
            kv.text(
                "Head",
                kit::mono(cr.sha.chars().take(12).collect::<String>()),
            );
        }
        if let Some(t) = cr.updated {
            kv.text(
                "Updated",
                format!("{} ({})", format::relative(t, now), format::absolute(t)),
            );
        }
        if !cr.labels.is_empty() {
            kv.text("Labels", cr.labels.join(", "));
        }
        if !cr.assignees.is_empty() {
            kv.text("Assignees", people(&cr.assignees));
        }
        if remote.kind == ForgeKind::GitLab && !cr.reviewers.is_empty() {
            kv.text(
                "Reviewers",
                people(
                    &cr.reviewers
                        .iter()
                        .map(|r| r.person.clone())
                        .collect::<Vec<_>>(),
                ),
            );
        }
        if let Some((done, total)) = cr.tasks {
            kv.text("Tasks", format!("{done} of {total} done"));
        }
        if remote.kind == ForgeKind::GitLab {
            kv.text(
                "Activity",
                format!(
                    "{}  ·  {} up, {} down",
                    format::plural(cr.comments as usize, "comment", "comments"),
                    cr.upvotes,
                    cr.downvotes
                ),
            );
        }
        if let Some(c) = &cr.changes_count {
            kv.text(
                "Changes",
                if c == "1" {
                    "1 file".to_owned()
                } else {
                    format!("{c} files")
                },
            );
        }
        kv.text(
            "On merge",
            format!(
                "{}, {}",
                if cr.squash { "squash" } else { "keep commits" },
                if cr.remove_source {
                    "delete the source branch"
                } else {
                    "keep the source branch"
                }
            ),
        );
        if let Some(e) = &cr.merge_error {
            kv.row("Merge error", |ui| {
                ui.label(RichText::new(e).color(pal(ui).danger));
            });
        }
        if let Some(can) = cr.can_merge {
            kv.text("You can merge", if can { "yes" } else { "no" });
        }
    });

    // Description.
    kit::section(ui, "Description", None);
    if cr.description.trim().is_empty() {
        kit::muted(ui, "No description.");
    } else {
        let text: String = cr.description.chars().take(6_000).collect();
        form::card(ui, Tone::Neutral, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(("forge_desc", cr.number))
                .max_height(260.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    kit::body(ui, &text);
                });
        });
        if cr.description.chars().count() > 6_000 {
            kit::muted(ui, "The description continues on the web.");
        }
    }

    match remote.kind {
        ForgeKind::GitLab => gitlab_sections(ui, remote, cr, d, can_write, me, actions),
        ForgeKind::Azure => azure_sections(ui, cr, can_write, me, actions),
    }
    let _ = p;
}

fn part_error(ui: &mut Ui, what: &str, e: &str) {
    let hint = if e.contains("401") {
        " Reading it needs a sign-in (glab auth login)."
    } else {
        ""
    };
    kit::callout(
        ui,
        Tone::Warning,
        &format!("Could not load {what}"),
        &format!("{e}{hint}"),
    );
}

#[allow(clippy::too_many_arguments)]
fn gitlab_sections(
    ui: &mut Ui,
    remote: &ForgeRemote,
    cr: &ChangeRequest,
    d: &Detail,
    can_write: bool,
    me: &str,
    actions: &mut Vec<Action>,
) {
    let now = format::now_secs();
    // Approvals.
    kit::section(ui, "Approvals", None);
    match &d.approvals {
        None if d.loading => kit::loading(ui, "Loading approvals…"),
        None => {
            kit::muted(ui, "Not loaded.");
        }
        Some(Err(e)) => part_error(ui, "the approvals", e),
        Some(Ok(a)) => {
            let mut line = if a.approved {
                "Approved".to_owned()
            } else {
                "Not approved yet".to_owned()
            };
            if let Some(req) = a.required {
                line.push_str(&format!("  ·  {req} required"));
            }
            if let Some(left) = a.left {
                line.push_str(&format!(", {left} left"));
            }
            kit::body(ui, &line);
            for (person, at) in &a.by {
                kit::secondary(
                    ui,
                    &format!(
                        "Approved by {}{}",
                        person.display(),
                        at.map(|t| format!(" {}", format::relative(t, now)))
                            .unwrap_or_default()
                    ),
                );
            }
            if can_write {
                let mine = a.by.iter().any(|(p, _)| p.is(me));
                ui.horizontal(|ui| {
                    if mine {
                        if form::button(ui, "Remove my approval", true).clicked() {
                            actions.push(Act::Unapprove(cr.number).into());
                        }
                    } else if form::primary(ui, "Approve", true).clicked() {
                        actions.push(Act::Approve(cr.number).into());
                    }
                });
            }
        }
    }

    // Pipelines of the merge request.
    kit::section(
        ui,
        "Pipelines",
        d.pipelines
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .map(|v| v.len()),
    );
    match &d.pipelines {
        None if d.loading => kit::loading(ui, "Loading pipelines…"),
        None => {
            kit::muted(ui, "Not loaded.");
        }
        Some(Err(e)) => part_error(ui, "the pipelines", e),
        Some(Ok(v)) if v.is_empty() => {
            kit::muted(ui, "No pipelines ran for this merge request.");
        }
        Some(Ok(v)) => {
            let rows: Vec<Vec<String>> = v
                .iter()
                .take(8)
                .map(|p| {
                    vec![
                        format!("#{}", p.id),
                        p.status.clone(),
                        p.reference.clone(),
                        p.sha.chars().take(8).collect(),
                        p.created
                            .map(|t| format::relative(t, now))
                            .unwrap_or_default(),
                    ]
                })
                .collect();
            kit::table(
                ui,
                &[
                    Col::text("Pipeline"),
                    Col::text("Status"),
                    Col::text("Ref"),
                    Col::text("Commit"),
                    Col::text("Created"),
                ],
                &rows,
            );
            if let Some(head) = &cr.pipeline
                && kit::link(
                    ui,
                    &format!(
                        "Open head pipeline #{} ({}) on the web",
                        head.id, head.status
                    ),
                )
                .clicked()
            {
                actions.push(Action::OpenUrl(head.web_url.clone()));
            }
        }
    }

    // Discussions.
    kit::section(ui, "Discussions", None);
    match &d.threads {
        None if d.loading => kit::loading(ui, "Loading discussions…"),
        None => {
            kit::muted(ui, "Not loaded.");
        }
        Some(Err(e)) => part_error(ui, "the discussions", e),
        Some(Ok(threads)) => {
            let s = thread_stats(threads);
            kit::body(
                ui,
                &format!(
                    "{}, {} unresolved of {} resolvable  ·  {}  ·  {} system notes",
                    format::plural(s.threads, "thread", "threads"),
                    s.unresolved,
                    s.resolvable,
                    format::plural(s.notes, "comment", "comments"),
                    s.system
                ),
            );
            let mut notes: Vec<(&gitvis_core::devops::forge::model::Note, bool)> = threads
                .iter()
                .flat_map(|t| {
                    t.notes
                        .iter()
                        .map(move |n| (n, t.resolvable && !t.resolved))
                })
                .filter(|(n, _)| !n.system)
                .collect();
            notes.sort_by_key(|n| std::cmp::Reverse(n.0.created));
            for (n, open) in notes.iter().take(6) {
                form::card(
                    ui,
                    if *open { Tone::Warning } else { Tone::Neutral },
                    |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(n.author.display())
                                    .size(size::SMALL)
                                    .family(crate::theme::medium())
                                    .color(pal(ui).text_strong),
                            );
                            if let Some(t) = n.created {
                                kit::muted(ui, &format::relative(t, now));
                            }
                            if *open {
                                kit::badge(ui, "Unresolved", Tone::Warning);
                            }
                        });
                        let body: String = n.body.chars().take(700).collect();
                        kit::body(ui, &body);
                    },
                );
            }
            if notes.len() > 6 {
                kit::muted(
                    ui,
                    &format!("{} older comments on the web.", notes.len() - 6),
                );
            }
        }
    }

    // Changed files.
    let files = d.files.as_ref().and_then(|r| r.as_ref().ok());
    kit::section(ui, "Changed files", files.map(|f| f.len()));
    match &d.files {
        None if d.loading => kit::loading(ui, "Loading changes…"),
        None => {
            kit::muted(ui, "Not loaded.");
        }
        Some(Err(e)) => part_error(ui, "the changes", e),
        Some(Ok(f)) if f.is_empty() => {
            kit::muted(ui, "No changes.");
        }
        Some(Ok(f)) => {
            let add: u64 = f.iter().map(|x| x.additions).sum();
            let del: u64 = f.iter().map(|x| x.deletions).sum();
            kit::secondary(
                ui,
                &format!(
                    "+{add}  −{del} lines in {}",
                    format::plural(f.len(), "file", "files")
                ),
            );
            let rows: Vec<Vec<String>> = f
                .iter()
                .take(60)
                .map(|x| {
                    let kind = if x.new_file {
                        "added"
                    } else if x.deleted {
                        "deleted"
                    } else if x.renamed {
                        "renamed"
                    } else {
                        "modified"
                    };
                    let path = if x.renamed {
                        format!("{} → {}", x.old_path, x.new_path)
                    } else {
                        x.new_path.clone()
                    };
                    vec![
                        path,
                        kind.into(),
                        format!("+{}", x.additions),
                        format!("−{}", x.deletions),
                    ]
                })
                .collect();
            kit::table(
                ui,
                &[
                    Col::text("File"),
                    Col::text("Change"),
                    Col::num("Added"),
                    Col::num("Removed"),
                ],
                &rows,
            );
            if f.len() > 60 {
                kit::muted(ui, &format!("and {} more files", f.len() - 60));
            }
        }
    }
    let _ = remote;
}

fn azure_sections(
    ui: &mut Ui,
    cr: &ChangeRequest,
    can_write: bool,
    me: &str,
    actions: &mut Vec<Action>,
) {
    kit::section(ui, "Reviewers and votes", Some(cr.reviewers.len()));
    if cr.reviewers.is_empty() {
        kit::muted(ui, "No reviewers.");
    } else {
        let rows: Vec<Vec<String>> = cr
            .reviewers
            .iter()
            .map(|r| {
                vec![
                    r.person.display(),
                    azure::vote_label(r.vote.unwrap_or(0)).to_owned(),
                    if r.required { "required" } else { "optional" }.to_owned(),
                    if r.declined { "declined" } else { "" }.to_owned(),
                ]
            })
            .collect();
        kit::table(
            ui,
            &[
                Col::text("Reviewer"),
                Col::text("Vote"),
                Col::text("Required"),
                Col::text(""),
            ],
            &rows,
        );
    }
    if can_write {
        let mine = cr
            .reviewers
            .iter()
            .find(|r| r.person.is(me))
            .and_then(|r| r.vote)
            .unwrap_or(0);
        kit::secondary(ui, &format!("Your vote: {}", azure::vote_label(mine)));
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for v in Vote::ALL {
                let on = v.value() == mine && v != Vote::Reset;
                if IconButton::text(v.label()).selected(on).show(ui).clicked() {
                    actions.push(Act::Vote(cr.number, v).into());
                }
            }
        });
    }
}

fn merge_card(
    ui: &mut Ui,
    remote: &ForgeRemote,
    cr: &ChangeRequest,
    f: &mut super::MergeForm,
    writing: &std::collections::HashSet<String>,
    actions: &mut Vec<Action>,
) {
    let gitlab = remote.kind == ForgeKind::GitLab;
    kit::section(ui, if gitlab { "Merge" } else { "Complete" }, None);
    form::card(
        ui,
        if cr.conflicts {
            Tone::Danger
        } else {
            Tone::Neutral
        },
        |ui| {
            if cr.conflicts {
                kit::callout(
                    ui,
                    Tone::Danger,
                    "Conflicts",
                    "The source branch conflicts with the target. Resolve them before merging.",
                );
            }
            if cr.draft {
                kit::secondary(ui, "This is a draft: mark it ready before merging.");
            }
            form::checkbox(ui, &mut f.squash, "Squash commits");
            form::checkbox(ui, &mut f.remove_source, "Delete the source branch");
            if gitlab {
                form::checkbox(
                    ui,
                    &mut f.when_pipeline_succeeds,
                    "Merge when the pipeline succeeds",
                );
            }
            form::field(
                ui,
                if gitlab {
                    if f.squash {
                        "Squash commit message"
                    } else {
                        "Merge commit message"
                    }
                } else {
                    "Merge commit message (one line)"
                },
                Some("Empty keeps the forge's default message."),
                |ui| form::text(ui, ("forge_merge_msg", cr.number), &mut f.message, ""),
            );
            let busy = writing
                .iter()
                .any(|w| w.ends_with(&format!("#{}", cr.number)));
            form::actions_row(ui, |ui| {
                let label = if gitlab { "Merge…" } else { "Complete…" };
                if form::danger(ui, label, !busy).clicked() {
                    actions.push(Act::Merge(cr.number).into());
                }
                if busy {
                    ui.add(egui::Spinner::new().size(14.0));
                }
            });
        },
    );
}

fn comment_card(
    ui: &mut Ui,
    cr: &ChangeRequest,
    comment: &mut String,
    writing: &std::collections::HashSet<String>,
    actions: &mut Vec<Action>,
) {
    kit::section(ui, "Comment", None);
    form::area(
        ui,
        ("forge_comment", cr.number),
        comment,
        "Write a comment (Markdown)",
        4,
    );
    let busy = writing.iter().any(|w| w.starts_with("Comment"));
    form::actions_row(ui, |ui| {
        if form::primary(ui, "Comment…", !comment.trim().is_empty() && !busy).clicked() {
            actions.push(Act::Comment(cr.number).into());
        }
    });
}

fn branch_chips(ui: &mut Ui, value: &mut String, branches: &[String]) {
    if branches.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        kit::muted(ui, "On origin:");
        for b in branches.iter().take(10) {
            if kit::chip(ui, b, value == b).clicked() {
                *value = b.clone();
            }
        }
        if branches.len() > 10 {
            kit::muted(ui, &format!("and {} more", branches.len() - 10));
        }
    });
}

fn create_form(
    ui: &mut Ui,
    remote: &ForgeRemote,
    f: &mut CreateForm,
    branches: &[String],
    actions: &mut Vec<Action>,
) {
    let gitlab = remote.kind == ForgeKind::GitLab;
    let noun = remote.kind.review_noun();
    egui::ScrollArea::vertical()
        .id_salt("forge_create")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            Frame::NONE.inner_margin(Margin::symmetric(28, 20)).show(ui, |ui| {
                ui.set_max_width(ui.available_width().min(kit::PROSE + 80.0));
                kit::title(ui, &format!("New {noun}"));
                kit::secondary(
                    ui,
                    &format!(
                        "Opens a {noun} on {} ({}). Push the source branch first: it must exist on the remote.",
                        remote.host, remote.path
                    ),
                );
                ui.add_space(space::S);
                form::field(ui, "Source branch", None, |ui| {
                    form::text(ui, "forge_new_source", &mut f.source, "feature/my-change");
                });
                branch_chips(ui, &mut f.source, branches);
                form::field(ui, "Target branch", None, |ui| {
                    form::text(ui, "forge_new_target", &mut f.target, "main");
                });
                branch_chips(ui, &mut f.target, branches);
                form::field(ui, "Title", None, |ui| {
                    form::text(ui, "forge_new_title", &mut f.title, "What this changes");
                });
                form::field(ui, "Description", Some("Markdown."), |ui| {
                    form::area(ui, "forge_new_desc", &mut f.description, "Why, how, how to test", 6);
                });
                form::checkbox(ui, &mut f.draft, if gitlab { "Draft (adds \"Draft:\" to the title)" } else { "Draft" });
                form::checkbox(ui, &mut f.remove_source, "Delete the source branch when merged");
                form::checkbox(ui, &mut f.squash, "Squash commits when merged");
                if !gitlab {
                    form::checkbox(ui, &mut f.auto_complete, "Auto-complete when policies pass");
                }
                ui.add_space(space::S);
                if gitlab {
                    form::field(ui, "Assignees", Some("Numeric GitLab user ids (shown on each user's profile page)."), |ui| {
                        form::tags(ui, "forge_new_assignees", &mut f.assignees, &mut f.assignee_buf, "Add a user id", &[]);
                    });
                    form::field(ui, "Reviewers", Some("Numeric GitLab user ids."), |ui| {
                        form::tags(ui, "forge_new_reviewers", &mut f.reviewers, &mut f.reviewer_buf, "Add a user id", &[]);
                    });
                } else {
                    form::field(ui, "Reviewers", Some("Names or e-mail addresses."), |ui| {
                        form::tags(ui, "forge_new_reviewers", &mut f.reviewers, &mut f.reviewer_buf, "Add a reviewer", &[]);
                    });
                }
                form::field(ui, "Labels", None, |ui| {
                    form::tags(ui, "forge_new_labels", &mut f.labels, &mut f.label_buf, "Add a label", &[]);
                });
                form::error(ui, &f.error);
                form::actions_row(ui, |ui| {
                    if form::primary(ui, &format!("Create {noun}…"), true).clicked() {
                        actions.push(Act::SubmitCreate.into());
                    }
                    if form::button(ui, "Cancel", true).clicked() {
                        actions.push(Act::CloseCreate.into());
                    }
                });
                kit::muted(
                    ui,
                    if gitlab {
                        "Sent as POST projects/:id/merge_requests with the JSON body on standard input; the confirmation shows it."
                    } else {
                        "Sent as az repos pr create; the confirmation shows the exact command."
                    },
                );
            });
        });
}
