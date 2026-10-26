//! A pull request's Conversation tab: description, merge box, details,
//! the timeline of comments, reviews and events, and a comment box.

use egui::{RichText, Ui, vec2};
use gitvis_core::Oid;
use gitvis_core::devops::review::{
    MergeLevel, MergeMethod, MergeState, Mergeable, PrDetail, PrState, RepoMeta, ReviewDecision,
    ReviewState, TimelineItem, TimelineKind, local, merge_status,
};

use super::widgets::{self, person, rel};
use super::{Act, MergeForm};
use crate::format;
use crate::state::{Action, RepoState};
use crate::theme::{Tone, pal, size, space};
use crate::views::form;
use crate::views::icons::{self, Icon};
use crate::views::kit;

pub fn show(ui: &mut Ui, repo: &mut RepoState, n: u64, now: i64, actions: &mut Vec<Action>) {
    let meta = repo.devops.review.meta();
    let full_name = repo
        .github_remote
        .as_ref()
        .map(|(o, r)| format!("{o}/{r}"))
        .unwrap_or_default();
    let snap = repo.snapshot.as_ref();
    let changing = repo.devops.review.changing.is_some();
    let busy = repo.busy.is_some();
    let v = repo.devops.review.prs.entry(n).or_default();
    let super::PrView {
        detail,
        merge,
        comment,
        ..
    } = v;
    let Some(d) = detail.data.as_ref() else {
        if let Some(e) = &detail.error {
            if widgets::error_block(ui, &format!("Could not load pull request #{n}"), e) {
                actions.push(Act::RefreshPr(n).into());
            }
        } else {
            widgets::loading_block(
                ui,
                "Loading the conversation…",
                "One GraphQL query: description, merge state, the newest 100 timeline entries and the review threads.",
            );
        }
        return;
    };
    if let Some(e) = &detail.error {
        kit::callout(ui, Tone::Danger, "The last reload failed", e);
    }
    let head_in_graph = Oid::from_str(&d.pr.head_oid)
        .ok()
        .filter(|o| snap.is_some_and(|s| s.index_of.contains_key(o)));

    // Description.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        person(ui, &d.pr.author, true);
        ui.label(
            RichText::new(format!("opened this {}", rel(d.pr.created, now)))
                .size(size::SMALL)
                .color(pal(ui).text_2),
        );
    });
    ui.add_space(2.0);
    widgets::prose(ui, &d.body, &d.pr.url, actions);

    merge_box(
        ui, d, &meta, merge, &full_name, n, changing, busy, now, actions,
    );
    details(ui, d, head_in_graph, now, actions);
    timeline(ui, d, now);

    kit::section(ui, "Add a comment", None);
    form::area(
        ui,
        ("rv_conv_comment", n),
        comment,
        "Write a comment for the conversation (Markdown is fine)",
        4,
    );
    form::actions_row(ui, |ui| {
        if form::primary(ui, "Comment", !comment.trim().is_empty() && !changing).clicked() {
            actions.push(Act::PostComment(n).into());
        }
    });
}

fn level_tone(d: &PrDetail, level: MergeLevel) -> Tone {
    match level {
        MergeLevel::Ready => Tone::Success,
        MergeLevel::Caution => Tone::Warning,
        MergeLevel::Blocked
            if d.pr.mergeable == Mergeable::Conflicting
                || d.pr.merge_state == MergeState::Dirty =>
        {
            Tone::Danger
        }
        MergeLevel::Blocked if d.pr.draft => Tone::Neutral,
        MergeLevel::Blocked => Tone::Warning,
        MergeLevel::Done if d.pr.state == PrState::Merged => Tone::Purple,
        MergeLevel::Done | MergeLevel::Unknown => Tone::Neutral,
    }
}

#[allow(clippy::too_many_arguments)]
fn merge_box(
    ui: &mut Ui,
    d: &PrDetail,
    meta: &RepoMeta,
    form_state: &mut MergeForm,
    full_name: &str,
    n: u64,
    changing: bool,
    busy: bool,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let status = merge_status(&d.pr);
    let tone = level_tone(d, status.level);
    kit::section(ui, "Merge", None);
    form::card(ui, tone, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let (icon, color) = match tone {
                Tone::Success => (Icon::Check, p.success),
                Tone::Danger => (Icon::Error, p.danger),
                Tone::Warning => (Icon::Warning, p.warning),
                Tone::Purple => (Icon::Merge, p.purple),
                _ => (Icon::Info, p.text_2),
            };
            icons::show(ui, icon, 16.0, color);
            kit::subtitle(ui, status.title);
        });
        if !status.detail.is_empty() {
            kit::secondary(ui, &status.detail);
        }
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            match d.pr.review_decision {
                Some(ReviewDecision::Approved) => {
                    kit::badge(ui, "Approved", Tone::Success);
                }
                Some(ReviewDecision::ChangesRequested) => {
                    kit::badge(ui, "Changes requested", Tone::Danger);
                }
                Some(ReviewDecision::ReviewRequired) => {
                    kit::badge(ui, "Review required", Tone::Warning);
                }
                None => {
                    kit::badge(ui, "No required review", Tone::Neutral);
                }
            }
            if let Some(c) = d.pr.checks {
                let t = if c.is_bad() {
                    Tone::Danger
                } else if c.is_running() {
                    Tone::Warning
                } else {
                    Tone::Success
                };
                kit::badge(ui, c.label(), t);
            }
            let m = match d.pr.mergeable {
                Mergeable::Mergeable => ("No conflicts", Tone::Neutral),
                Mergeable::Conflicting => ("Conflicts", Tone::Danger),
                Mergeable::Unknown => ("Conflicts not computed yet", Tone::Neutral),
            };
            kit::badge(ui, m.0, m.1);
            kit::badge(
                ui,
                &format!("Merge state: {}", d.pr.merge_state.code()),
                Tone::Neutral,
            );
            let open = d.unresolved_threads();
            if open > 0 {
                kit::badge(
                    ui,
                    &format::plural(open, "unresolved thread", "unresolved threads"),
                    Tone::Warning,
                );
            }
        });
        if let Some(am) = &d.auto_merge {
            ui.add_space(2.0);
            kit::callout(
                ui,
                Tone::Accent,
                "Auto-merge is on",
                &format!(
                    "{} by {}{}: GitHub merges as soon as the required reviews and checks pass.",
                    am.method.map(|m| m.label()).unwrap_or("Merge"),
                    if am.enabled_by.is_empty() {
                        "someone"
                    } else {
                        &am.enabled_by
                    },
                    am.enabled_at
                        .map(|t| format!(" {}", format::relative(t, now)))
                        .unwrap_or_default()
                ),
            );
        }
        match d.pr.state {
            PrState::Merged => {
                ui.add_space(4.0);
                kit::secondary(
                    ui,
                    &format!(
                        "Merged by {}{}.",
                        d.merged_by.as_deref().unwrap_or("someone"),
                        d.pr.merged
                            .map(|t| format!(" on {}", widgets::when(t, now)))
                            .unwrap_or_default()
                    ),
                );
                if form::button(ui, "Check out locally", !busy).clicked() {
                    actions.push(Act::Checkout(n).into());
                }
                return;
            }
            PrState::Closed => {
                form::actions_row(ui, |ui| {
                    if form::primary(ui, "Reopen pull request", !changing).clicked() {
                        actions.push(Act::SetOpen { pr: n, open: true }.into());
                    }
                });
                return;
            }
            PrState::Open => {}
        }
        ui.add_space(space::S);
        kit::hairline(ui);
        ui.add_space(space::S);

        // Method.
        let allowed = meta.allowed_methods();
        let method = form_state
            .method
            .filter(|m| allowed.contains(m))
            .or(meta.preferred_method())
            .unwrap_or(MergeMethod::Merge);
        let told = meta.allow_merge_commit.is_some()
            || meta.allow_squash_merge.is_some()
            || meta.allow_rebase_merge.is_some();
        let help = if !told {
            "GitHub did not say which methods the repository allows; all three are offered."
                .to_owned()
        } else if allowed.len() == 1 {
            "The repository allows only this method (the others are switched off in its settings)."
                .to_owned()
        } else {
            format!(
                "The repository allows {}.",
                allowed
                    .iter()
                    .map(|m| m.label().to_lowercase())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        form::field(ui, "Merge method", Some(&help), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                for m in &allowed {
                    if form::radio(ui, method == *m, m.label()) {
                        form_state.method = Some(*m);
                    }
                }
            });
        });
        if method.has_message() {
            let head_owner =
                d.pr.head_repo
                    .as_deref()
                    .and_then(|r| r.split('/').next())
                    .unwrap_or("")
                    .to_owned();
            let default =
                local::default_merge_title(method, meta, &d.pr.title, n, &head_owner, &d.pr.head);
            if form_state.title == form_state.default_title {
                form_state.title = default.clone();
            }
            form_state.default_title = default;
            form::field(ui, "Commit title", None, |ui| {
                form::text(
                    ui,
                    ("rv_merge_title", n),
                    &mut form_state.title,
                    "Commit title",
                );
            });
            let hint = local::default_merge_message_hint(method, meta);
            form::field(ui, "Commit message", None, |ui| {
                form::area(ui, ("rv_merge_msg", n), &mut form_state.message, hint, 3);
            });
        } else {
            kit::secondary(
                ui,
                "Rebase and merge replays each commit onto the base branch; there is no merge commit to name.",
            );
        }

        // Branch deletion.
        if meta.delete_branch_on_merge == Some(true) {
            kit::secondary(
                ui,
                &format!(
                    "The repository deletes {} on GitHub after merging.",
                    d.pr.head
                ),
            );
        } else if d.pr.head_in_repo(full_name) {
            let mut del = form_state.delete_branch.unwrap_or(false);
            if form::checkbox(
                ui,
                &mut del,
                &format!("Delete {} on GitHub after merging", d.pr.head),
            )
            .changed()
            {
                form_state.delete_branch = Some(del);
            }
        } else {
            kit::secondary(
                ui,
                &format!(
                    "The branch lives in {}; it stays there after merging.",
                    d.pr.head_repo.as_deref().unwrap_or("a deleted fork")
                ),
            );
        }
        if !meta.permission.is_empty() && !meta.can_write() {
            ui.add_space(2.0);
            kit::callout(
                ui,
                Tone::Neutral,
                &format!("Your permission: {}", meta.permission.to_lowercase()),
                "GitHub will refuse merging, updating the branch, auto-merge and closing unless you can write to the repository. Reviewing and commenting work.",
            );
        }
        let conflicted =
            d.pr.mergeable == Mergeable::Conflicting || d.pr.merge_state == MergeState::Dirty;
        form::actions_row(ui, |ui| {
            let merge_label = match method {
                MergeMethod::Merge => "Merge",
                MergeMethod::Squash => "Squash and merge",
                MergeMethod::Rebase => "Rebase and merge",
            };
            if form::primary(ui, merge_label, !changing && !d.pr.draft && !conflicted).clicked() {
                actions.push(Act::Merge(n).into());
            }
            if d.auto_merge.is_some() {
                if form::button(ui, "Disable auto-merge", !changing).clicked() {
                    actions.push(
                        Act::AutoMerge {
                            pr: n,
                            enable: false,
                        }
                        .into(),
                    );
                }
            } else if meta.allow_auto_merge != Some(false)
                && form::button(ui, "Enable auto-merge", !changing && !d.pr.draft).clicked()
            {
                actions.push(
                    Act::AutoMerge {
                        pr: n,
                        enable: true,
                    }
                    .into(),
                );
            }
            if form::button(ui, "Update branch", !changing).clicked() {
                actions.push(Act::UpdateBranch(n).into());
            }
        });
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = space::S;
            if d.pr.draft {
                if form::button(ui, "Ready for review", !changing).clicked() {
                    actions.push(
                        Act::SetDraft {
                            pr: n,
                            draft: false,
                        }
                        .into(),
                    );
                }
            } else if form::button(ui, "Convert to draft", !changing).clicked() {
                actions.push(Act::SetDraft { pr: n, draft: true }.into());
            }
            if form::button(ui, "Close pull request", !changing).clicked() {
                actions.push(Act::SetOpen { pr: n, open: false }.into());
            }
            if form::button(ui, "Check out locally", !busy).clicked() {
                actions.push(Act::Checkout(n).into());
            }
            if form::button(ui, "Open on GitHub", true).clicked() {
                actions.push(Action::OpenUrl(d.pr.url.clone()));
            }
        });
    });
}

fn details(
    ui: &mut Ui,
    d: &PrDetail,
    head_in_graph: Option<Oid>,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    kit::section(ui, "Details", None);
    kit::kv(ui, 110.0, |kv| {
        kv.row("Branches", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
                kit::badge(ui, &d.pr.head, Tone::Neutral);
                ui.label(RichText::new("into").size(size::SMALL).color(p.text_2));
                kit::badge(ui, &d.pr.base, Tone::Neutral);
                if d.pr.cross_repo {
                    ui.label(
                        RichText::new(format!(
                            "from {}",
                            d.pr.head_repo.as_deref().unwrap_or("a deleted fork")
                        ))
                        .size(size::SMALL)
                        .color(p.text_2),
                    );
                }
                if d.maintainer_can_modify {
                    ui.label(
                        RichText::new("· maintainers can push to it")
                            .size(size::SMALL)
                            .color(p.text_3),
                    );
                }
            });
        });
        kv.row("Head", |ui| {
            ui.horizontal(|ui| {
                let short = &d.pr.head_oid[..d.pr.head_oid.len().min(12)];
                match head_in_graph {
                    Some(oid) => {
                        if kit::link(ui, short).clicked() {
                            actions.push(Action::JumpTo(oid));
                        }
                        ui.label(
                            RichText::new("in the loaded graph")
                                .size(size::SMALL)
                                .color(p.text_3),
                        );
                    }
                    None => {
                        ui.label(kit::mono(short).color(p.text));
                        ui.label(
                            RichText::new("not in the loaded graph (check out to fetch it)")
                                .size(size::SMALL)
                                .color(p.text_3),
                        );
                    }
                }
            });
        });
        kv.row("Changes", |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(
                    RichText::new(format!("+{}", format::count(d.pr.additions as usize)))
                        .color(p.success),
                );
                ui.label(
                    RichText::new(format!(
                        "\u{2212}{}",
                        format::count(d.pr.deletions as usize)
                    ))
                    .color(p.danger),
                );
                ui.label(
                    RichText::new(format!(
                        "in {}, {}",
                        format::plural(d.pr.changed_files as usize, "file", "files"),
                        format::plural(d.pr.commits as usize, "commit", "commits")
                    ))
                    .size(size::SMALL)
                    .color(p.text_2),
                );
            });
        });
        let reviewers: Vec<String> = d.pr.review_requests.iter().map(|r| r.display()).collect();
        if reviewers.is_empty() {
            kv.muted("Review requests", "none open");
        } else {
            kv.text("Review requests", reviewers.join(", "));
        }
        if d.pr.assignees.is_empty() {
            kv.muted("Assignees", "nobody");
        } else {
            kv.text("Assignees", d.pr.assignees.join(", "));
        }
        if let Some(m) = &d.milestone {
            kv.text("Milestone", m.as_str());
        }
        kv.text(
            "Comments",
            format!(
                "{} in the conversation, {} reviews, {} threads on the code ({} unresolved)",
                d.pr.comments,
                d.pr.reviews,
                d.threads_total,
                d.unresolved_threads()
            ),
        );
        if let Some(c) = d.pr.created {
            kv.text("Opened", widgets::when(c, now));
        }
        if let Some(u) = d.pr.updated {
            kv.text("Updated", widgets::when(u, now));
        }
        if d.locked {
            kv.text("Locked", "Only collaborators can comment");
        }
    });
}

/// What a timeline entry says after its author.
fn verb(item: &TimelineItem) -> String {
    match &item.kind {
        TimelineKind::Comment => "commented".to_owned(),
        TimelineKind::Review(s) => s.verb().to_owned(),
        TimelineKind::Merged => "merged the pull request".to_owned(),
        TimelineKind::Closed => "closed the pull request".to_owned(),
        TimelineKind::Reopened => "reopened the pull request".to_owned(),
        TimelineKind::ReadyForReview => "marked it ready for review".to_owned(),
        TimelineKind::ConvertedToDraft => "converted it to a draft".to_owned(),
        TimelineKind::ForcePushed => "force-pushed the branch".to_owned(),
        TimelineKind::ReviewRequested => format!("requested a review from {}", item.detail),
        TimelineKind::Labeled => format!("added the label {}", item.detail),
        TimelineKind::Other(t) => t.clone(),
    }
}

fn review_badge(s: ReviewState) -> Option<(&'static str, Tone)> {
    match s {
        ReviewState::Approved => Some(("Approved", Tone::Success)),
        ReviewState::ChangesRequested => Some(("Changes requested", Tone::Danger)),
        ReviewState::Dismissed => Some(("Dismissed", Tone::Neutral)),
        ReviewState::Pending => Some(("Pending", Tone::Warning)),
        ReviewState::Commented => None,
    }
}

fn timeline(ui: &mut Ui, d: &PrDetail, now: i64) {
    let p = *pal(ui);
    kit::section(ui, "Conversation", Some(d.timeline_total as usize));
    if d.timeline.is_empty() {
        kit::muted(ui, "No comments, reviews or events yet.");
        return;
    }
    if d.timeline_total as usize > d.timeline.len() {
        kit::muted(
            ui,
            &format!(
                "Showing the newest {} of {} entries; older ones are on GitHub.",
                d.timeline.len(),
                d.timeline_total
            ),
        );
        ui.add_space(4.0);
    }
    let mut i = 0;
    while i < d.timeline.len() {
        let item = &d.timeline[i];
        let speaks = matches!(item.kind, TimelineKind::Comment | TimelineKind::Review(_))
            && (item.has_body()
                || matches!(item.kind, TimelineKind::Review(s) if s != ReviewState::Commented));
        if speaks {
            entry(ui, item, now);
            i += 1;
            continue;
        }
        // Group events of the same kind by the same person within a minute
        // (a bot adding five labels is one line).
        let mut j = i + 1;
        while j < d.timeline.len() {
            let next = &d.timeline[j];
            let same = std::mem::discriminant(&next.kind) == std::mem::discriminant(&item.kind)
                && next.author == item.author
                && matches!(
                    item.kind,
                    TimelineKind::Labeled | TimelineKind::ReviewRequested
                )
                && match (next.at, item.at) {
                    (Some(a), Some(b)) => (a - b).abs() <= 60,
                    _ => false,
                };
            if !same {
                break;
            }
            j += 1;
        }
        let group = &d.timeline[i..j];
        let text = if group.len() > 1 {
            let what: Vec<&str> = group.iter().map(|g| g.detail.as_str()).collect();
            match item.kind {
                TimelineKind::Labeled => format!("added the labels {}", what.join(", ")),
                _ => format!("requested reviews from {}", what.join(", ")),
            }
        } else if matches!(item.kind, TimelineKind::Review(_)) {
            format!(
                "reviewed{}",
                if item.review_comments > 0 {
                    format!(
                        " with {} on the code",
                        format::plural(item.review_comments as usize, "comment", "comments")
                    )
                } else {
                    String::new()
                }
            )
        } else {
            verb(item)
        };
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.add_space(5.0);
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 18.0), egui::Sense::hover());
            ui.painter().circle_filled(r.center(), 3.0, p.text_3);
            ui.add_space(4.0);
            ui.label(
                RichText::new(if item.author.is_empty() {
                    "ghost"
                } else {
                    &item.author
                })
                .size(size::SMALL)
                .color(p.text),
            );
            ui.label(RichText::new(text).size(size::SMALL).color(p.text_2));
            ui.label(
                RichText::new(rel(item.at, now))
                    .size(size::SMALL)
                    .color(p.text_3),
            );
        });
        i = j;
    }
}

fn entry(ui: &mut Ui, item: &TimelineItem, now: i64) {
    let p = *pal(ui);
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        person(ui, &item.author, true);
        ui.label(RichText::new(verb(item)).size(size::SMALL).color(p.text_2));
        if let TimelineKind::Review(s) = item.kind
            && let Some((text, tone)) = review_badge(s)
        {
            kit::badge(ui, text, tone);
        }
        ui.label(
            RichText::new(rel(item.at, now))
                .size(size::SMALL)
                .color(p.text_3),
        );
    });
    if item.has_body() {
        ui.horizontal(|ui| {
            ui.add_space(28.0);
            ui.vertical(|ui| widgets::comment_text(ui, &item.body));
        });
    }
    if item.review_comments > 0 {
        ui.horizontal(|ui| {
            ui.add_space(28.0);
            kit::muted(
                ui,
                &format!(
                    "{} on the code: see the Files tab.",
                    format::plural(item.review_comments as usize, "comment", "comments")
                ),
            );
        });
    }
    ui.add_space(4.0);
}
