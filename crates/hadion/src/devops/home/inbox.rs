//! The inbox lists: notifications (with reasons and mark as read), the four
//! searches (review requests, my pull requests, mentions, assigned) and
//! failed runs on the user's branches. Rows open the pull request or run in
//! Hadion when its repository is open, else the page on GitHub.

use egui::{Align2, FontId, Rect, Sense, Ui, pos2, vec2};

use hadion_core::devops::home::notify::{self, Notification, Reason, SubjectKind};
use hadion_core::devops::home::runs::Why;
use hadion_core::devops::home::search::{Bucket, Item, checks_label, decision_label};
use hadion_core::devops::home::{RepoGroup, group_by_repo};

use super::view::{body, fetched_parts};
use super::{Act, FailedRun, State, Tab, Target, failed_runs, open_names, repo_index};
use crate::format;
use crate::state::{Action, RepoState};
use crate::theme::{Tone, pal, size, space};
use crate::views::form;
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Leading, Row};

const ROW_H: f32 = kit::ROW_H;
const GROUP_H: f32 = 34.0;

fn reason_tone(r: Reason) -> Tone {
    match r {
        Reason::ReviewRequested | Reason::ApprovalRequested => Tone::Accent,
        Reason::Mention | Reason::TeamMention => Tone::Purple,
        Reason::Assign => Tone::Warning,
        Reason::SecurityAlert => Tone::Danger,
        _ => Tone::Neutral,
    }
}

fn kind_icon(k: SubjectKind) -> Icon {
    match k {
        SubjectKind::PullRequest => Icon::PullRequest,
        SubjectKind::Issue => Icon::Issue,
        SubjectKind::Commit => Icon::Graph,
        SubjectKind::Release => Icon::Tag,
        SubjectKind::CheckSuite => Icon::Play,
        SubjectKind::Discussion => Icon::Comment,
        SubjectKind::SecurityAlert => Icon::Shield,
        SubjectKind::Other => Icon::Bell,
    }
}

/// What the current notification filter keeps.
fn visible_notifications<'a>(state: &'a State, repos: &[RepoState]) -> Vec<&'a Notification> {
    let open = open_names(repos);
    let mut f = state.filter.clone();
    f.text = state.text.clone();
    state
        .notifications
        .as_deref()
        .map(|n| notify::filtered(n, &f, &open))
        .unwrap_or_default()
}

fn visible_items<'a>(state: &'a State, repos: &[RepoState], b: Bucket) -> Vec<&'a Item> {
    let open = open_names(repos);
    let t = state.text.trim().to_lowercase();
    state
        .search
        .as_ref()
        .map(|s| {
            s.bucket(b)
                .items
                .iter()
                .filter(|i| !state.search_only_open || open.contains(&i.repo.to_ascii_lowercase()))
                .filter(|i| {
                    t.is_empty()
                        || i.title.to_lowercase().contains(&t)
                        || i.repo.to_lowercase().contains(&t)
                        || i.author.to_lowercase().contains(&t)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What `inbox-item N` opens: the current list in display order.
pub(super) fn targets(state: &State, repos: &[RepoState]) -> Vec<Target> {
    let grouped = |keys: Vec<(String, Target)>| -> Vec<Target> {
        if !state.group {
            return keys.into_iter().map(|(_, t)| t).collect();
        }
        let refs: Vec<&(String, Target)> = keys.iter().collect();
        let order: Vec<String> = group_by_repo(&refs, |k| &k.0)
            .into_iter()
            .flat_map(|g| g.items.into_iter().map(|k| target_key(&k.1)))
            .collect();
        let mut keys = keys;
        let mut out = Vec::new();
        for k in order {
            if let Some(p) = keys.iter().position(|(_, t)| target_key(t) == k) {
                out.push(keys.remove(p).1);
            }
        }
        out
    };
    match state.tab {
        Tab::Overview => (0..repos.len()).map(Target::Repo).collect(),
        Tab::Notifications => grouped(
            visible_notifications(state, repos)
                .into_iter()
                .map(|n| (n.repo.clone(), Target::Notification(n.id.clone())))
                .collect(),
        ),
        Tab::Failed => failed_runs(repos, state.login(), format::now_secs())
            .into_iter()
            .map(|f| Target::Run {
                repo: f.repo,
                run: f.failure.run.id,
            })
            .collect(),
        t => match t.bucket() {
            Some(b) => grouped(
                visible_items(state, repos, b)
                    .into_iter()
                    .map(|i| {
                        (
                            i.repo.clone(),
                            Target::Item {
                                repo: i.repo.clone(),
                                number: i.number,
                                pr: i.pr,
                                url: i.url.clone(),
                            },
                        )
                    })
                    .collect(),
            ),
            None => Vec::new(),
        },
    }
}

fn target_key(t: &Target) -> String {
    match t {
        Target::Notification(id) => format!("n:{id}"),
        Target::Item { url, .. } => format!("i:{url}"),
        Target::Run { repo, run } => format!("r:{repo}:{run}"),
        Target::Repo(i) => format!("p:{i}"),
    }
}

// ---------------------------------------------------------------------------
// Shared pieces
// ---------------------------------------------------------------------------

/// An inbox page header: the subtitle gives way to the buttons on a narrow
/// window (`extra_w` is the width of `extra`'s buttons); `web_tip` is the
/// link button's tooltip.
#[allow(clippy::too_many_arguments)]
fn header(
    ui: &mut Ui,
    title: &str,
    parts: &[String],
    extra_w: f32,
    web: &str,
    web_tip: &str,
    state: &State,
    actions: &mut Vec<Action>,
    extra: impl FnOnce(&mut Ui, &mut Vec<Action>),
) {
    // The link and "Refresh" buttons, and the gaps.
    let controls_w = extra_w + 150.0;
    super::view::header_fit(
        ui,
        title,
        parts,
        controls_w,
        state,
        actions,
        |ui, actions| {
            extra(ui, actions);
            if IconButton::icon(Icon::External)
                .show(ui)
                .on_hover_text(web_tip)
                .clicked()
            {
                actions.push(Action::OpenUrl(web.to_owned()));
            }
        },
    );
}

/// Loading, error and "nothing fetched" states shared by the inbox tabs.
/// Returns true when the caller should draw its list.
fn ready(
    ui: &mut Ui,
    state: &State,
    error: Option<&str>,
    have: bool,
    what: &str,
    actions: &mut Vec<Action>,
) -> bool {
    if let Some(e) = error {
        let mut retry = false;
        kit::callout_with(
            ui,
            Tone::Danger,
            &format!("Could not fetch {what}"),
            &format!(
                "{e}\n\nhadion reads your inbox through the GitHub CLI. If it is not signed in, run `gh auth login` in a terminal, then retry."
            ),
            |ui| {
                retry = IconButton::new(Icon::Fetch, "Retry")
                    .compact()
                    .show(ui)
                    .clicked();
            },
        );
        if retry {
            actions.push(Act::Refresh.into());
        }
        if !have {
            return false;
        }
    }
    if !have {
        if state.loading {
            ui.add_space(space::L);
            kit::loading(ui, &format!("Fetching {what}…"));
        } else if kit::empty_state(
            ui,
            Icon::Inbox,
            "Not fetched yet",
            "The inbox is fetched when this page opens. Fetch it now?",
            Some((Icon::Fetch, "Fetch the inbox")),
        ) {
            actions.push(Act::Refresh.into());
        }
        return false;
    }
    true
}

fn group_row(ui: &mut Ui, g_repo: &str, n: usize, open: Option<usize>, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), GROUP_H), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let mid = rect.top() + GROUP_H * 0.6;
    let painter = ui.painter();
    let r = painter.text(
        pos2(rect.left() + 8.0, mid),
        Align2::LEFT_CENTER,
        g_repo,
        FontId::new(size::BODY, crate::theme::semibold()),
        p.text_strong,
    );
    let r = painter.text(
        pos2(r.right() + 8.0, mid),
        Align2::LEFT_CENTER,
        format::count(n),
        FontId::proportional(size::SMALL),
        p.text_3,
    );
    if let Some(i) = open {
        let br = Rect::from_min_size(pos2(r.right() + 10.0, mid - 12.0), vec2(118.0, 24.0));
        if kit::overlay(ui, br, IconButton::text("Open in Hadion").compact())
            .on_hover_text("Switch to this repository's history")
            .clicked()
        {
            actions.push(Act::Activate(i).into());
        }
    }
    painter_hline(ui, rect);
}

fn painter_hline(ui: &Ui, rect: Rect) {
    let c = pal(ui).border;
    ui.painter().hline(
        (rect.left() + 4.0)..=(rect.right() - 4.0),
        rect.bottom() - 1.0,
        egui::Stroke::new(1.0, c),
    );
}

fn when(t: Option<i64>, now: i64) -> String {
    t.map(|t| format::relative(t, now)).unwrap_or_default()
}

fn checkbox_row(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        add(ui)
    });
}

fn text_filter(ui: &mut Ui, state: &mut State, id: &str, hint: &str) {
    let w = ui.available_width().min(420.0);
    ui.allocate_ui_with_layout(
        vec2(w, 30.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            kit::search_box(ui, id, &mut state.text, hint);
        },
    );
}

fn none_match(ui: &mut Ui, total: usize, actions: &mut Vec<Action>) {
    ui.add_space(space::L);
    let mut clear = false;
    kit::callout_with(
        ui,
        Tone::Neutral,
        "Nothing matches the filters",
        &format!(
            "{} hidden by the filters above.",
            format::plural(total, "item is", "items are")
        ),
        |ui| {
            clear = IconButton::text("Clear filters")
                .compact()
                .show(ui)
                .clicked()
        },
    );
    if clear {
        actions.push(Act::ClearFilters.into());
    }
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

pub(super) fn notifications(
    ui: &mut Ui,
    state: &mut State,
    repos: &[RepoState],
    now: i64,
    actions: &mut Vec<Action>,
) {
    let all = state.notifications.as_ref().map(|n| n.len()).unwrap_or(0);
    let unread = state.unread();
    let mut sub = vec![format!(
        "{} fetched, {} unread",
        format::count(all),
        format::count(unread)
    )];
    sub.extend(fetched_parts(state, now));
    let can_mark_all = state.notifications.is_some() && unread > 0 && !state.marking_all;
    header(
        ui,
        "Notifications",
        &sub,
        160.0,
        "https://github.com/notifications",
        "Open your notifications on GitHub",
        state,
        actions,
        |ui, actions| {
            if state.marking_all {
                ui.add(egui::Spinner::new().size(14.0));
            } else if IconButton::new(Icon::Check, "Mark all as read")
                .enabled(can_mark_all)
                .show(ui)
                .on_hover_text(
                    "Marks every notification up to the last fetch as read on GitHub (asks first)",
                )
                .clicked()
            {
                actions.push(Act::MarkAllRead.into());
            }
        },
    );
    let error = state.notif_error.clone();
    let have = state.notifications.is_some();
    let mut toggles: Vec<Reason> = Vec::new();
    let mut set_all = false;
    body(ui, "home_notifications", |ui| {
        if !ready(ui, state, error.as_deref(), have, "notifications", actions) {
            return;
        }
        let list = state.notifications.clone().unwrap_or_default();
        if list.is_empty() {
            kit::empty_state(
                ui,
                Icon::Inbox,
                "No unread notifications",
                "GitHub lists unread threads here (all=false). Threads you have read are on github.com/notifications.",
                None,
            );
            return;
        }
        // Filters.
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            if kit::chip(
                ui,
                &format!("All  {}", list.len()),
                state.filter.reasons.is_empty(),
            )
            .clicked()
            {
                set_all = true;
            }
            for (r, n) in notify::reason_counts(&list) {
                let on = state.filter.reasons.contains(&r);
                if kit::chip(ui, &format!("{}  {n}", r.label()), on)
                    .on_hover_text(r.explain())
                    .clicked()
                {
                    toggles.push(r);
                }
            }
        });
        checkbox_row(ui, |ui| {
            form::checkbox(ui, &mut state.filter.unread_only, "Unread only");
            form::checkbox(
                ui,
                &mut state.filter.only_open,
                "Only repositories open in Hadion",
            );
            form::checkbox(ui, &mut state.group, "Group by repository");
        });
        text_filter(
            ui,
            state,
            "home_notif_filter",
            "Filter by title or repository",
        );
        let shown = visible_notifications(state, repos);
        if shown.is_empty() {
            none_match(ui, list.len(), actions);
            return;
        }
        kit::muted(
            ui,
            &format!(
                "{} shown. Click to open: pull requests and workflow runs of repositories open in Hadion open here, the rest on GitHub.",
                format::count(shown.len())
            ),
        );
        let marking = state.marking.clone();
        if state.group {
            for g in group_by_repo(&shown, |n| &n.repo) {
                let RepoGroup { repo, items } = g;
                group_row(ui, &repo, items.len(), repo_index(repos, &repo), actions);
                for n in items {
                    notification_row(ui, n, &marking, now, true, actions);
                }
            }
        } else {
            for n in shown {
                notification_row(ui, n, &marking, now, false, actions);
            }
        }
    });
    if set_all {
        actions.push(Act::SetReasons(Vec::new()).into());
    }
    for r in toggles {
        actions.push(Act::ToggleReason(r).into());
    }
}

fn notification_row(
    ui: &mut Ui,
    n: &Notification,
    marking: &std::collections::HashSet<String>,
    now: i64,
    grouped: bool,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let hovered = ui.rect_contains_pointer(rect);
    kit::row_background(ui, rect, hovered, false);
    let mut sub = Vec::new();
    if !grouped {
        sub.push(n.repo.clone());
    }
    if let Some(num) = n.number {
        sub.push(format!("#{num}"));
    }
    sub.push(n.kind.label().to_owned());
    if let Some(t) = n.updated {
        sub.push(format!("updated {}", format::absolute(t)));
    }
    if n.private {
        sub.push("private".into());
    }
    let sub = sub.join("  ·  ");
    let mut badges = vec![(n.reason.label().to_owned(), reason_tone(n.reason))];
    if n.unread {
        badges.push(("Unread".to_owned(), Tone::Accent));
    }
    let busy = marking.contains(&n.id);
    let show_button = hovered && n.unread && !busy;
    let meta = if busy {
        "marking…".to_owned()
    } else if show_button {
        String::new()
    } else {
        when(n.updated, now)
    };
    let icon_c = if n.unread { p.accent } else { p.text_3 };
    kit::paint_row(
        ui,
        rect,
        &Row::new(&n.title)
            .leading(Leading::Icon(kind_icon(n.kind), icon_c))
            .subtitle(&sub)
            .meta(&meta)
            .badges(&badges),
        false,
    );
    let mut button_hit = false;
    if show_button {
        let br = Rect::from_min_size(
            pos2(rect.right() - 104.0, rect.top() + 6.0),
            vec2(92.0, 24.0),
        );
        let b = kit::overlay(ui, br, IconButton::new(Icon::Check, "Mark read").compact());
        if b.clicked() {
            button_hit = true;
            actions.push(Act::MarkRead(n.id.clone()).into());
        }
    }
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.clicked() && !button_hit {
        actions.push(Act::OpenNotification(n.id.clone()).into());
    }
}

// ---------------------------------------------------------------------------
// Searches
// ---------------------------------------------------------------------------

fn empty_text(b: Bucket) -> (&'static str, &'static str) {
    match b {
        Bucket::ReviewRequests => (
            "No review requests",
            "No open pull request is waiting for your review (or for a team you are on).",
        ),
        Bucket::Mine => ("No open pull requests", "You have no open pull requests."),
        Bucket::Mentions => ("No mentions", "No open issue or pull request mentions you."),
        Bucket::Assigned => (
            "Nothing assigned",
            "No open issue or pull request is assigned to you.",
        ),
    }
}

pub(super) fn search(
    ui: &mut Ui,
    state: &mut State,
    repos: &[RepoState],
    tab: Tab,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let Some(b) = tab.bucket() else { return };
    let found = state.search.as_ref().map(|s| s.bucket(b).clone());
    let mut sub = Vec::new();
    if let Some(f) = &found {
        sub.push(format!(
            "{} of {} shown",
            format::count(f.items.len()),
            format::count(f.total as usize)
        ));
    }
    sub.extend(fetched_parts(state, now));
    let web = b.web_url();
    // The search itself is for the curious: on the link, not in the subtitle.
    let tip = format!("Open this search on GitHub\n{}", b.query());
    header(
        ui,
        b.label(),
        &sub,
        0.0,
        &web,
        &tip,
        state,
        actions,
        |_, _| {},
    );
    let error = state.search_error.clone();
    let have = found.is_some();
    body(ui, &format!("home_search_{tab:?}"), |ui| {
        if !ready(
            ui,
            state,
            error.as_deref(),
            have,
            "the GitHub searches",
            actions,
        ) {
            return;
        }
        let Some(found) = found else { return };
        if found.items.is_empty() {
            let (t, b) = empty_text(b);
            kit::empty_state(ui, Icon::Inbox, t, b, None);
            return;
        }
        checkbox_row(ui, |ui| {
            form::checkbox(
                ui,
                &mut state.search_only_open,
                "Only repositories open in Hadion",
            );
            form::checkbox(ui, &mut state.group, "Group by repository");
        });
        text_filter(
            ui,
            state,
            "home_search_filter",
            "Filter by title, repository or author",
        );
        let shown = visible_items(state, repos, b);
        if shown.is_empty() {
            none_match(ui, found.items.len(), actions);
            return;
        }
        if found.total as usize > found.items.len() {
            kit::muted(
                ui,
                &format!(
                    "GitHub found {}; the newest {} are listed. Open the search on GitHub for the rest.",
                    format::count(found.total as usize),
                    found.items.len()
                ),
            );
        }
        if state.group {
            for g in group_by_repo(&shown, |i| &i.repo) {
                group_row(
                    ui,
                    &g.repo,
                    g.items.len(),
                    repo_index(repos, &g.repo),
                    actions,
                );
                for i in g.items {
                    item_row(ui, i, repos, now, true, actions);
                }
            }
        } else {
            for i in shown {
                item_row(ui, i, repos, now, false, actions);
            }
        }
    });
}

fn item_row(
    ui: &mut Ui,
    i: &Item,
    repos: &[RepoState],
    now: i64,
    grouped: bool,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let hovered = ui.rect_contains_pointer(rect);
    kit::row_background(ui, rect, hovered, false);
    let mut sub = Vec::new();
    if !grouped {
        sub.push(i.repo.clone());
    }
    sub.push(format!("#{}", i.number));
    if !i.author.is_empty() {
        sub.push(format!("by @{}", i.author));
    }
    if let (Some(h), Some(b)) = (&i.head, &i.base) {
        sub.push(format!("{h} → {b}"));
    }
    if i.comments > 0 {
        sub.push(format::plural(i.comments as usize, "comment", "comments"));
    }
    if let Some(t) = i.created {
        sub.push(format!("opened {}", format::relative(t, now)));
    }
    let sub = sub.join("  ·  ");
    let mut badges: Vec<(String, Tone)> = Vec::new();
    if i.draft {
        badges.push(("Draft".into(), Tone::Neutral));
    }
    if let Some(d) = &i.review_decision {
        let tone = match d.as_str() {
            "APPROVED" => Tone::Success,
            "CHANGES_REQUESTED" => Tone::Danger,
            _ => Tone::Warning,
        };
        badges.push((decision_label(d).into(), tone));
    }
    if let Some(c) = &i.checks {
        let tone = match c.as_str() {
            "SUCCESS" => Tone::Success,
            "FAILURE" | "ERROR" => Tone::Danger,
            _ => Tone::Accent,
        };
        badges.push((checks_label(c).into(), tone));
    }
    for l in i.labels.iter().take(2) {
        badges.push((l.clone(), Tone::Neutral));
    }
    let open_here = i.pr && repo_index(repos, &i.repo).is_some();
    let meta = format!(
        "{}{}",
        when(i.updated, now),
        if open_here {
            "  ·  opens in Hadion"
        } else {
            ""
        }
    );
    kit::paint_row(
        ui,
        rect,
        &Row::new(&i.title)
            .leading(Leading::Icon(
                if i.pr { Icon::PullRequest } else { Icon::Issue },
                if i.draft { p.text_3 } else { p.success },
            ))
            .subtitle(&sub)
            .meta(&meta)
            .badges(&badges),
        false,
    );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.clicked() {
        actions.push(
            Act::OpenItem {
                repo: i.repo.clone(),
                number: i.number,
                pr: i.pr,
                url: i.url.clone(),
            }
            .into(),
        );
    }
}

// ---------------------------------------------------------------------------
// Failed runs
// ---------------------------------------------------------------------------

pub(super) fn failed(
    ui: &mut Ui,
    state: &mut State,
    repos: &[RepoState],
    failed: &[FailedRun],
    now: i64,
    actions: &mut Vec<Action>,
) {
    let still = failed.iter().filter(|f| !f.failure.superseded).count();
    let with_gh = repos.iter().filter(|r| r.github.is_some()).count();
    let sub = vec![
        format!(
            "{} still failing, {} fixed since",
            still,
            failed.len() - still
        ),
        format!("last {} hours", super::FAILED_WINDOW_SECS / 3600),
        format!(
            "from the runs of {} loaded",
            format::plural(with_gh, "repository", "repositories")
        ),
    ];
    super::view::header_fit(
        ui,
        "Failed runs on your branches",
        &sub,
        120.0,
        state,
        actions,
        |_, _| {},
    );
    body(ui, "home_failed", |ui| {
        kit::secondary(
            ui,
            "A run is yours when you started it, or when it ran on the head branch of one of your open pull requests.",
        );
        kit::secondary(
            ui,
            "The runs come from each open repository's newest workflow runs, already loaded: this page makes no requests of its own.",
        );
        if state.login.is_none() {
            if state.loading {
                kit::loading(ui, "Finding out who you are on GitHub…");
            } else {
                let mut fetch = false;
                kit::callout_with(
                    ui,
                    Tone::Warning,
                    "Your GitHub login is not known yet",
                    "It comes with the inbox fetch.",
                    |ui| {
                        fetch = IconButton::new(Icon::Fetch, "Fetch the inbox")
                            .compact()
                            .show(ui)
                            .clicked()
                    },
                );
                if fetch {
                    actions.push(Act::Refresh.into());
                }
            }
            return;
        }
        if with_gh == 0 {
            kit::empty_state(
                ui,
                Icon::Play,
                "No GitHub data loaded",
                "None of the open repositories has loaded its GitHub runs. Open a repository with a github.com remote, or its Actions page.",
                None,
            );
            return;
        }
        if failed.is_empty() {
            kit::empty_state(
                ui,
                Icon::Check,
                "No failed runs on your branches",
                &format!(
                    "Nothing you started, and nothing on your pull requests' branches, failed in the last {} hours.",
                    super::FAILED_WINDOW_SECS / 3600
                ),
                None,
            );
            return;
        }
        for f in failed {
            run_row(ui, f, repos, now, actions);
        }
    });
}

fn run_row(ui: &mut Ui, f: &FailedRun, repos: &[RepoState], now: i64, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let run = f.failure.run;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let hovered = ui.rect_contains_pointer(rect);
    kit::row_background(ui, rect, hovered, false);
    let repo = repos.get(f.repo).map(|r| r.name.as_str()).unwrap_or("");
    let title = if run.title.is_empty() {
        run.head_message.lines().next().unwrap_or("").to_owned()
    } else {
        run.title.clone()
    };
    let sub = [
        repo.to_owned(),
        run.branch.clone(),
        format!("{} #{}", run.name, run.run_number),
        format!("by @{}", run.actor),
        run.event.clone(),
        run.conclusion.clone().unwrap_or_default(),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join("  ·  ");
    let mut badges = vec![if f.failure.superseded {
        ("Fixed by a later run".to_owned(), Tone::Success)
    } else {
        ("Still failing".to_owned(), Tone::Danger)
    }];
    if let Why::PrBranch(n) = f.failure.why {
        badges.push((format!("Your pull request #{n}"), Tone::Neutral));
    }
    let meta = when(run.updated.or(run.created), now);
    kit::paint_row(
        ui,
        rect,
        &Row::new(&title)
            .leading(Leading::Icon(
                Icon::Play,
                if f.failure.superseded {
                    p.text_3
                } else {
                    p.danger
                },
            ))
            .subtitle(&sub)
            .meta(&meta)
            .badges(&badges),
        false,
    );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.clicked() {
        actions.push(
            Act::OpenRun {
                repo: f.repo,
                run: run.id,
            }
            .into(),
        );
    }
}
