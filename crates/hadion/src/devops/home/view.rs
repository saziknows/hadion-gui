//! The home page frame: section navigation, the repository overview (stat
//! tiles, one card per open repository, "Fetch all" with its progress and
//! results). The inbox lists are in [`super::inbox`].

use egui::{
    Color32, CornerRadius, FontId, Frame, Margin, Painter, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui, pos2, vec2,
};

use hadion_core::github::Outcome;
use hadion_core::model::{ChangeKind, Head, Pending};

use super::{Act, FailedRun, State, Tab, failed_runs, inbox};
use crate::format;
use crate::state::{Action, RepoState, Settings};
use crate::theme::{Tone, medium, pal, semibold, size, space};
use crate::views::icons::{self, Icon, IconButton};
use crate::views::kit::{self, NavGroup, NavItem, Stat};
use crate::views::truncated;

/// Card size on the overview.
const CARD_MIN_W: f32 = 360.0;
const CARD_H: f32 = 196.0;
const GAP: f32 = 12.0;

pub fn show(
    ui: &mut Ui,
    state: &mut State,
    repos: &[RepoState],
    active: usize,
    settings: &Settings,
    actions: &mut Vec<Action>,
) {
    let now = format::now_secs();
    let failed = failed_runs(repos, state.login(), now);
    let still = failed.iter().filter(|f| !f.failure.superseded).count();
    let mut clicked: Option<Tab> = None;
    let mut nav_data = NavData::of(state, repos.len(), still);
    nav_data.groups = settings.ext.automation.groups.len();
    kit::page_with_nav(
        ui,
        "home_nav",
        |ui| clicked = nav(ui, &nav_data, now),
        |ui| match state.tab {
            Tab::Overview => overview(ui, state, repos, active, &failed, now, actions),
            Tab::Groups => crate::devops::automation::groups_page(ui, repos, settings, actions),
            Tab::Notifications => inbox::notifications(ui, state, repos, now, actions),
            Tab::Failed => inbox::failed(ui, state, repos, &failed, now, actions),
            t => inbox::search(ui, state, repos, t, now, actions),
        },
    );
    if let Some(t) = clicked {
        actions.push(Act::SetTab(t).into());
    }
    if state.fetching() || state.loading {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(200));
    }
}

/// What the navigation shows (read before the page borrows the state).
struct NavData {
    tab: Tab,
    repos: usize,
    groups: usize,
    unread: usize,
    buckets: [usize; 4],
    still_failing: usize,
    login: Option<String>,
    loading: bool,
    fetched_epoch: Option<i64>,
}

impl NavData {
    fn of(state: &State, repos: usize, still_failing: usize) -> Self {
        let b = |t: Tab| -> usize {
            match (t.bucket(), &state.search) {
                (Some(b), Some(s)) => s.bucket(b).total as usize,
                _ => 0,
            }
        };
        Self {
            tab: state.tab,
            repos,
            groups: 0,
            unread: state.unread(),
            buckets: [
                b(Tab::Reviews),
                b(Tab::Mine),
                b(Tab::Mentions),
                b(Tab::Assigned),
            ],
            still_failing,
            login: state.login.clone(),
            loading: state.loading,
            fetched_epoch: state.fetched_epoch,
        }
    }
}

fn nav(ui: &mut Ui, state: &NavData, now: i64) -> Option<Tab> {
    let p = *pal(ui);
    let bucket = |t: Tab| -> usize {
        match t {
            Tab::Reviews => state.buckets[0],
            Tab::Mine => state.buckets[1],
            Tab::Mentions => state.buckets[2],
            Tab::Assigned => state.buckets[3],
            _ => 0,
        }
    };
    let still_failing = state.still_failing;
    let groups = vec![
        NavGroup {
            title: "Workspace",
            items: vec![
                NavItem::new(Tab::Overview.label())
                    .count(state.repos, Tone::Neutral)
                    .hint(Tab::Overview.hint()),
                NavItem::new(Tab::Groups.label())
                    .count(state.groups, Tone::Neutral)
                    .hint(Tab::Groups.hint()),
            ],
        },
        NavGroup {
            title: "Inbox",
            items: vec![
                NavItem::new(Tab::Notifications.label())
                    .count(state.unread, Tone::Accent)
                    .hint(Tab::Notifications.hint()),
                NavItem::new(Tab::Reviews.label())
                    .count(bucket(Tab::Reviews), Tone::Accent)
                    .hint(Tab::Reviews.hint()),
                NavItem::new(Tab::Mine.label())
                    .count(bucket(Tab::Mine), Tone::Neutral)
                    .hint(Tab::Mine.hint()),
                NavItem::new(Tab::Mentions.label())
                    .count(bucket(Tab::Mentions), Tone::Neutral)
                    .hint(Tab::Mentions.hint()),
                NavItem::new(Tab::Assigned.label())
                    .count(bucket(Tab::Assigned), Tone::Neutral)
                    .hint(Tab::Assigned.hint()),
                NavItem::new(Tab::Failed.label())
                    .count(still_failing, Tone::Danger)
                    .hint(Tab::Failed.hint()),
            ],
        },
    ];
    let selected = Tab::ALL.iter().position(|t| *t == state.tab).unwrap_or(0);
    let clicked = kit::section_nav(ui, &groups, selected).and_then(|i| Tab::ALL.get(i).copied());
    // Who and when, under the navigation.
    ui.add_space(space::XL);
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.label(kit::caption_text(&p, "GitHub"));
            let who = match state.login.as_deref() {
                Some(l) => format!("Signed in as @{l}"),
                None if state.loading => "Signing in…".to_owned(),
                None => "Not fetched yet".to_owned(),
            };
            ui.label(RichText::new(who).size(size::SMALL).color(p.text_2));
            let when = if state.loading {
                "Fetching the inbox…".to_owned()
            } else if let Some(e) = state.fetched_epoch {
                format!("Inbox fetched {}", format::relative(e, now))
            } else {
                String::new()
            };
            if !when.is_empty() {
                ui.label(RichText::new(when).size(size::SMALL).color(p.text_3));
            }
        });
    });
    clicked
}

/// The scrollable, padded body under a page header.
pub(super) fn body(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            Frame::NONE
                .inner_margin(Margin {
                    left: 24,
                    right: 24,
                    top: 16,
                    bottom: 28,
                })
                .show(ui, |ui| {
                    ui.set_max_width(ui.available_width().min(kit::DETAIL_MAX + 240.0));
                    ui.spacing_mut().item_spacing.y = 8.0;
                    add(ui)
                });
        });
}

/// `fetching…` / `fetched 3m ago`, then the rate limit: subtitle parts a
/// page header may drop when its buttons need the room.
pub(super) fn fetched_parts(state: &State, now: i64) -> Vec<String> {
    let mut parts = Vec::new();
    if state.loading {
        parts.push("fetching…".to_owned());
    } else if let Some(e) = state.fetched_epoch {
        parts.push(format!("fetched {}", format::relative(e, now)));
    } else {
        parts.push("not fetched yet".to_owned());
    }
    if let Some(r) = state.rate {
        parts.push(format!(
            "{} of {} API requests left",
            format::count(r.remaining as usize),
            format::count(r.limit as usize)
        ));
    }
    parts
}

/// A Home page header whose subtitle gives way to its buttons on a narrow
/// window (`controls_w` is their width); what it drops is listed in the
/// Refresh button's tooltip.
pub(super) fn header_fit(
    ui: &mut Ui,
    title: &str,
    parts: &[String],
    controls_w: f32,
    state: &State,
    actions: &mut Vec<Action>,
    extra: impl FnOnce(&mut Ui, &mut Vec<Action>),
) {
    let (subtitle, dropped) = crate::views::github::fit_subtitle(ui, title, parts, controls_w);
    kit::page_header(ui, title, &subtitle, |ui| {
        extra(ui, actions);
        refresh_button(ui, state, &dropped, actions);
    });
}

/// Refresh button (or spinner) for a page header's right side. `more`
/// are subtitle parts the header had no room for.
pub(super) fn refresh_button(
    ui: &mut Ui,
    state: &State,
    more: &[String],
    actions: &mut Vec<Action>,
) {
    let mut tip = "Fetch notifications and the four searches again (2 requests)".to_owned();
    if let Some(e) = state.fetched_epoch {
        tip.push_str(&format!("\nLast fetched at {}", format::clock(e)));
    }
    for m in more {
        tip.push('\n');
        tip.push_str(m);
    }
    if state.loading {
        ui.add_space(6.0);
        ui.add(egui::Spinner::new().size(14.0));
        ui.add_space(6.0);
    } else if IconButton::new(Icon::Fetch, "Refresh")
        .show(ui)
        .on_hover_text(tip)
        .clicked()
    {
        actions.push(Act::Refresh.into());
    }
}

// ---------------------------------------------------------------------------
// Overview
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn overview(
    ui: &mut Ui,
    state: &mut State,
    repos: &[RepoState],
    active: usize,
    failed: &[FailedRun],
    now: i64,
    actions: &mut Vec<Action>,
) {
    let dirty = repos
        .iter()
        .filter(|r| r.snapshot.as_ref().is_some_and(|s| !s.status.is_clean()))
        .count();
    let mut sub = vec![
        format::plural(repos.len(), "repository open", "repositories open"),
        format!("{dirty} with uncommitted changes"),
    ];
    if let Some(l) = &state.login {
        sub.push(format!("GitHub: @{l}"));
    }
    // "Fetch all", "Refresh" and their gaps.
    header_fit(ui, "Home", &sub, 230.0, state, actions, |ui, actions| {
        let busy = state.fetching();
        if busy {
            ui.add_space(6.0);
            ui.add(egui::Spinner::new().size(14.0));
            ui.add_space(4.0);
        } else if IconButton::new(Icon::Fetch, "Fetch all")
            .primary(true)
            .show(ui)
            .on_hover_text("git fetch --all --prune in every open repository, one after another")
            .clicked()
        {
            actions.push(Act::FetchAll.into());
        }
    });
    body(ui, "home_overview", |ui| {
        tiles(ui, state, repos, failed);
        fetch_status(ui, state, actions);
        kit::section(ui, "Open repositories", Some(repos.len()));
        kit::secondary(
            ui,
            "Click a repository to open its history. Nothing is fetched for this page: it shows what each repository already loaded.",
        );
        ui.add_space(space::XS);
        cards(ui, state, repos, active, now, actions);
        sources(ui, state, now);
    });
}

fn tiles(ui: &mut Ui, state: &State, repos: &[RepoState], failed: &[FailedRun]) {
    let dirty = repos
        .iter()
        .filter(|r| r.snapshot.as_ref().is_some_and(|s| !s.status.is_clean()))
        .count();
    let urgent: usize = repos
        .iter()
        .map(|r| r.sec_page.urgent_count(&r.dismissed))
        .sum();
    let still = failed.iter().filter(|f| !f.failure.superseded).count();
    let known = |n: usize| {
        if state.fetched_at.is_some() || state.notifications.is_some() {
            format::count(n)
        } else {
            "–".to_owned()
        }
    };
    let mine = state
        .search
        .as_ref()
        .map(|s| s.mine.total as usize)
        .unwrap_or(0);
    kit::stats(
        ui,
        &[
            Stat::new("Open repositories", format::count(repos.len())),
            Stat::new("With changes", format::count(dirty))
                .tip("Repositories with staged, unstaged, untracked or conflicted files"),
            Stat::new("Unread notifications", known(state.unread()))
                .tip("Unread GitHub notification threads from the last fetch"),
            Stat::new("Review requests", known(state.review_requests()))
                .tip("Open pull requests waiting for your review (GitHub search)"),
            Stat::new("My open pull requests", known(mine)),
            Stat::new("Failed runs, 24 h", format::count(still))
                .tone_if(still > 0, Tone::Danger)
                .tip("Still-failing workflow runs on your branches in the open repositories"),
            Stat::new("Urgent findings", format::count(urgent))
                .tone_if(urgent > 0, Tone::Danger)
                .tip("Critical and high Exposure findings, dismissed ones excluded"),
        ],
    );
}

fn fetch_status(ui: &mut Ui, state: &State, _actions: &mut Vec<Action>) {
    let p = *pal(ui);
    if let Some(run) = &state.fetch {
        let (current, done) = run
            .progress
            .lock()
            .map(|g| (g.current.clone(), g.done.clone()))
            .unwrap_or_default();
        kit::section(ui, "Fetching", None);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.add(egui::Spinner::new().size(14.0).color(p.accent));
            let text = match &current {
                Some(c) => format!("Fetching {c}  ({} of {})", done.len() + 1, run.names.len()),
                None => "Starting…".to_owned(),
            };
            ui.label(RichText::new(text).size(size::BODY).color(p.text));
            ui.label(
                RichText::new(format::millis(run.started.elapsed().as_millis() as u64))
                    .size(size::SMALL)
                    .color(p.text_3),
            );
        });
        for o in &done {
            outcome_line(ui, o);
        }
        return;
    }
    let Some(f) = &state.fetched else { return };
    kit::section_with(ui, "Last fetch all", Some(f.outcomes.len()), |ui| {
        ui.label(
            RichText::new(format!(
                "{}  ·  {}",
                format::absolute(f.at),
                format::millis(f.elapsed_ms)
            ))
            .size(size::SMALL)
            .color(p.text_3),
        );
    });
    for o in &f.outcomes {
        outcome_line(ui, o);
    }
    if !f.skipped.is_empty() {
        kit::muted(ui, &format!("Skipped: {}", f.skipped.join(", ")));
    }
    for o in f.outcomes.iter().filter(|o| !o.ok) {
        kit::callout(
            ui,
            Tone::Danger,
            &format!("{} could not fetch", o.name),
            &o.output,
        );
    }
}

fn outcome_line(ui: &mut Ui, o: &hadion_core::devops::home::fetch::Outcome) {
    let p = *pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (icon, c) = if o.ok {
            (Icon::Check, p.success)
        } else {
            (Icon::Error, p.danger)
        };
        icons::show(ui, icon, 13.0, c);
        ui.label(
            RichText::new(&o.name)
                .size(size::BODY)
                .family(medium())
                .color(p.text),
        );
        let what = if o.ok {
            o.changes.summary()
        } else {
            "failed".to_owned()
        };
        ui.label(RichText::new(what).size(size::SMALL).color(if o.ok {
            p.text_2
        } else {
            p.danger
        }));
        if !o.changes.remotes.is_empty() {
            ui.label(
                RichText::new(format!("remotes: {}", o.changes.remotes.join(", ")))
                    .size(size::SMALL)
                    .color(p.text_3),
            );
        }
        ui.label(
            RichText::new(format::millis(o.elapsed_ms))
                .size(size::SMALL)
                .color(p.text_3),
        );
    });
}

/// Where each number on the overview comes from: folded away by default,
/// one source per row.
fn sources(ui: &mut Ui, state: &State, now: i64) {
    crate::views::github::details_section(
        ui,
        "home_sources",
        "Where this comes from",
        state.sources_open,
        |ui| {
            let search_cost = match state.search.as_ref().and_then(|s| s.rate.clone()) {
                Some(r) => format!(
                    "cost {}, {} of {} points left",
                    r.cost,
                    format::count(r.remaining as usize),
                    format::count(r.limit as usize)
                ),
                None => "cost 1".to_owned(),
            };
            kit::kv(ui, 120.0, |kv| {
                kv.text(
                    "Repositories",
                    "What each open tab already loaded: branches and status, its GitHub runs and pull requests, its Exposure and Insights results.",
                );
                kv.text(
                    "Notifications",
                    kit::mono("gh api notifications?all=false&per_page=50"),
                );
                kv.text(
                    "Searches",
                    format!("One GraphQL search for all four lists ({search_cost})"),
                );
                kv.text("Last fetch", fetched_parts(state, now).join(", "));
                kv.text(
                    "Failed runs",
                    format!(
                        "Runs you started, or that ran on the head branch of one of your pull requests, in the last {} hours.",
                        super::FAILED_WINDOW_SECS / 3600
                    ),
                );
            });
            if state.login.is_none() {
                kit::secondary(
                    ui,
                    "Your GitHub login is not known yet, so the \"mine\" counts stay at zero until the inbox is fetched.",
                );
            }
        },
    );
}

// ---------------------------------------------------------------------------
// Repository cards
// ---------------------------------------------------------------------------

/// What a card shows, gathered from the repository state.
struct Summary {
    name: String,
    path: String,
    head: String,
    detached: bool,
    upstream: Option<String>,
    gone: bool,
    ahead: usize,
    behind: usize,
    staged: usize,
    changed: usize,
    untracked: usize,
    conflicted: usize,
    stashes: usize,
    pending: Option<&'static str>,
    loading: bool,
    error: Option<String>,
    busy: Option<String>,
    remote: RemoteLine,
    urgent: usize,
    scanning: bool,
    insights: bool,
    loaded: Option<(i64, u64)>,
}

enum RemoteLine {
    None,
    /// A remote that is neither GitHub nor a known forge.
    Plain(String),
    Forge {
        label: String,
    },
    GitHub {
        name: String,
        loading: bool,
        error: Option<String>,
        ci: Option<(Outcome, String)>,
        /// HEAD is on no remote branch, so GitHub has no CI for it.
        unpushed: bool,
        prs: Option<usize>,
        mine: usize,
        failed: usize,
        fixed: usize,
    },
}

fn summarize(r: &RepoState, login: &str, failed: &[FailedRun], i: usize, now: i64) -> Summary {
    let snap = r.snapshot.as_ref();
    let (head, detached) = match snap.map(|s| &s.head) {
        Some(Head::Branch { name, .. }) => (name.clone(), false),
        Some(Head::Unborn(name)) => (format!("{name} (no commits)"), false),
        Some(Head::Detached(id)) => (format!("detached at {}", &id.to_string()[..7]), true),
        None => (String::new(), false),
    };
    let branch = snap.and_then(|s| s.current_branch());
    let st = snap.map(|s| &s.status);
    let untracked = st
        .map(|s| {
            s.unstaged
                .iter()
                .filter(|f| f.kind == ChangeKind::Untracked)
                .count()
        })
        .unwrap_or(0);
    let pending = snap
        .map(|s| s.pending)
        .filter(|p| *p != Pending::None)
        .map(|p| p.label());
    let remote = if let Some((o, n)) = &r.github_remote {
        let head_id = snap.and_then(|s| s.head.target());
        let unpushed = head_id.is_some_and(|h| {
            hadion_core::github::not_on_github(&h.to_string())
                == Some(hadion_core::github::NotOnGitHub::Unpushed)
        });
        let ci = head_id
            .filter(|_| !unpushed)
            .and_then(|h| r.ci.get(&h))
            .map(|c| (c.outcome, c.summary.clone()));
        let mine_failed: Vec<&FailedRun> = failed.iter().filter(|f| f.repo == i).collect();
        RemoteLine::GitHub {
            name: format!("{o}/{n}"),
            loading: r.github_loading && r.github.is_none(),
            error: r
                .github_error
                .as_deref()
                .filter(|_| r.github.is_none())
                .map(|e| {
                    e.lines()
                        .map(str::trim)
                        .find(|l| !l.is_empty())
                        .unwrap_or("")
                        .chars()
                        .filter(|c| !c.is_control())
                        .collect()
                }),
            ci,
            unpushed,
            prs: r.github.as_ref().map(|g| g.prs.len()),
            mine: r
                .github
                .as_ref()
                .map(|g| hadion_core::devops::home::runs::my_prs(&g.prs, login).len())
                .unwrap_or(0),
            failed: mine_failed.iter().filter(|f| !f.failure.superseded).count(),
            fixed: mine_failed.iter().filter(|f| f.failure.superseded).count(),
        }
    } else if let Some(f) = &r.forge_remote {
        RemoteLine::Forge {
            label: format!("{} · {}/{}", f.kind.label(), f.host, f.path),
        }
    } else if let Some(re) = snap.and_then(|s| {
        s.remotes
            .iter()
            .find(|r| r.name == "origin")
            .or(s.remotes.first())
    }) {
        let more = snap.map(|s| s.remotes.len()).unwrap_or(1) - 1;
        RemoteLine::Plain(format!(
            "{} · {}{}",
            re.name,
            re.url,
            if more > 0 {
                format!("  (+{more} more)")
            } else {
                String::new()
            }
        ))
    } else {
        RemoteLine::None
    };
    Summary {
        name: r.name.clone(),
        path: r.workdir.display().to_string(),
        head,
        detached,
        upstream: branch.and_then(|b| b.upstream.clone()),
        gone: branch.is_some_and(|b| b.upstream_gone),
        ahead: branch.map(|b| b.ahead).unwrap_or(0),
        behind: branch.map(|b| b.behind).unwrap_or(0),
        staged: st.map(|s| s.staged.len()).unwrap_or(0),
        changed: st.map(|s| s.unstaged.len()).unwrap_or(0) - untracked,
        untracked,
        conflicted: st.map(|s| s.conflicted.len()).unwrap_or(0),
        stashes: snap.map(|s| s.stashes.len()).unwrap_or(0),
        pending,
        loading: r.loading && snap.is_none(),
        error: r.load_error.clone(),
        busy: r.busy.clone(),
        remote,
        urgent: r.sec_page.urgent_count(&r.dismissed),
        scanning: r.sec_page.scanning(),
        insights: r.ins_page.running(),
        loaded: r
            .loaded_at
            .map(|t| (now - t.elapsed().as_secs() as i64, r.load_ms.unwrap_or(0))),
    }
}

fn cards(
    ui: &mut Ui,
    state: &State,
    repos: &[RepoState],
    active: usize,
    now: i64,
    actions: &mut Vec<Action>,
) {
    if repos.is_empty() {
        kit::muted(ui, "No repositories are open.");
        return;
    }
    let failed = failed_runs(repos, state.login(), now);
    let avail = ui.available_width();
    let cols = (((avail + GAP) / (CARD_MIN_W + GAP)).floor() as usize).clamp(1, 4);
    let w = (avail - GAP * (cols as f32 - 1.0)) / cols as f32;
    let rows = repos.len().div_ceil(cols);
    let total_h = rows as f32 * CARD_H + (rows.saturating_sub(1)) as f32 * GAP;
    let (area, _) = ui.allocate_exact_size(vec2(avail, total_h), Sense::hover());
    for (i, r) in repos.iter().enumerate() {
        let (row, col) = (i / cols, i % cols);
        let rect = Rect::from_min_size(
            pos2(
                area.left() + col as f32 * (w + GAP),
                area.top() + row as f32 * (CARD_H + GAP),
            ),
            vec2(w, CARD_H),
        );
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let resp = ui.interact(
            rect,
            ui.make_persistent_id(("home_card", r.id)),
            Sense::click(),
        );
        let hovered = ui.rect_contains_pointer(rect);
        let s = summarize(r, state.login(), &failed, i, now);
        card(ui, rect, &s, i == active, hovered, now);
        if hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if resp.clicked() {
            actions.push(Act::Activate(i).into());
        }
    }
}

/// Paint text pieces left to right from `x`, stopping at `max_x`; returns
/// the x after the last piece.
fn pieces(
    painter: &Painter,
    x: f32,
    y: f32,
    max_x: f32,
    parts: &[(String, FontId, Color32)],
) -> f32 {
    let mut x = x;
    for (t, f, c) in parts {
        if x >= max_x - 8.0 || t.is_empty() {
            continue;
        }
        let g = truncated(painter, t, f.clone(), *c, max_x - x);
        let w = g.size().x;
        painter.galley(pos2(x, y - g.size().y / 2.0), g, *c);
        x += w;
    }
    x
}

fn outcome_tone(o: Outcome) -> Tone {
    if o.is_bad() {
        Tone::Danger
    } else if o.is_running() {
        Tone::Accent
    } else if o.is_good() {
        Tone::Success
    } else {
        Tone::Neutral
    }
}

fn ci_word(o: Outcome) -> &'static str {
    if o.is_bad() {
        "failing"
    } else if o.is_running() {
        "running"
    } else if o.is_good() {
        "passing"
    } else if o == Outcome::Cancelled {
        "cancelled"
    } else {
        "no result"
    }
}

fn card(ui: &Ui, rect: Rect, s: &Summary, active: bool, hovered: bool, now: i64) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let painter = ui.painter();
    let border = if active {
        p.accent.gamma_multiply(0.75)
    } else if hovered {
        p.border_strong
    } else {
        p.border
    };
    painter.rect(
        rect,
        CornerRadius::same(crate::theme::radius::L),
        if hovered { p.hover } else { p.raised },
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    let x = rect.left() + 16.0;
    let right = rect.right() - 14.0;
    let body = FontId::proportional(size::SMALL);
    let strong = FontId::new(size::SMALL, medium());

    // Title line: status dot, name, badges on the right.
    let y = rect.top() + 22.0;
    let dot = match &s.remote {
        RemoteLine::GitHub {
            ci: Some((o, _)), ..
        } => outcome_tone(*o).color(&p),
        _ => p.text_3,
    };
    painter.circle_filled(pos2(x + 4.0, y), 4.5, dot);
    let mut title_right = right;
    if active {
        title_right =
            kit::paint_badge(painter, &p, dark, title_right, y, "Active", Tone::Accent) - 6.0;
    }
    if let Some(pend) = s.pending {
        title_right = kit::paint_badge(
            painter,
            &p,
            dark,
            title_right,
            y,
            &format!("{pend} in progress"),
            Tone::Warning,
        ) - 6.0;
    }
    let g = truncated(
        painter,
        &s.name,
        FontId::new(size::TITLE, semibold()),
        p.text_strong,
        title_right - x - 18.0,
    );
    painter.galley(pos2(x + 16.0, y - g.size().y / 2.0), g, p.text_strong);

    // Path.
    let y = y + 22.0;
    pieces(
        painter,
        x,
        y,
        right,
        &[(
            s.path.clone(),
            FontId::monospace(size::CAPTION + 0.5),
            p.text_3,
        )],
    );

    if s.loading || s.error.is_some() {
        let y = y + 30.0;
        let (t, c) = match &s.error {
            Some(e) => (format!("Could not load: {e}"), p.danger),
            None => ("Loading…".to_owned(), p.text_2),
        };
        pieces(painter, x, y, right, &[(t, body.clone(), c)]);
        return;
    }

    // Branch and upstream.
    let y = y + 26.0;
    icons::paint(painter, Icon::Branch, pos2(x + 6.0, y), 13.0, p.text_2);
    let mut parts = vec![(
        s.head.clone(),
        FontId::new(size::BODY, medium()),
        if s.detached { p.warning } else { p.text },
    )];
    match &s.upstream {
        Some(u) if s.gone => {
            parts.push((format!("   {u} is gone"), body.clone(), p.warning));
        }
        Some(u) => {
            if s.ahead > 0 {
                parts.push((format!("   ↑{}", s.ahead), strong.clone(), p.accent));
            }
            if s.behind > 0 {
                parts.push((format!("   ↓{}", s.behind), strong.clone(), p.warning));
            }
            let sync = if s.ahead == 0 && s.behind == 0 {
                format!("   up to date with {u}")
            } else {
                format!("   {u}")
            };
            parts.push((sync, body.clone(), p.text_3));
        }
        None if !s.detached => parts.push(("   no upstream".into(), body.clone(), p.text_3)),
        None => {}
    }
    pieces(painter, x + 18.0, y, right, &parts);

    // Working tree.
    let y = y + 22.0;
    let mut parts: Vec<(String, FontId, Color32)> = Vec::new();
    let mut add = |n: usize, what: &str, c: Color32| {
        if n > 0 {
            if !parts.is_empty() {
                parts.push(("  ·  ".into(), body.clone(), p.text_3));
            }
            parts.push((format!("{n} {what}"), body.clone(), c));
        }
    };
    add(s.conflicted, "conflicted", p.danger);
    add(s.staged, "staged", p.text);
    add(s.changed, "changed", p.text);
    add(s.untracked, "untracked", p.text_2);
    if parts.is_empty() {
        parts.push(("Clean working tree".into(), body.clone(), p.text_2));
    }
    if s.stashes > 0 {
        parts.push((
            format!("  ·  {}", format::plural(s.stashes, "stash", "stashes")),
            body.clone(),
            p.text_2,
        ));
    }
    pieces(painter, x, y, right, &parts);

    // Remote: CI on HEAD, pull requests, failed runs.
    let y = y + 22.0;
    match &s.remote {
        RemoteLine::None => {
            pieces(
                painter,
                x,
                y,
                right,
                &[("No remote".into(), body.clone(), p.text_3)],
            );
        }
        RemoteLine::Plain(label) => {
            pieces(
                painter,
                x,
                y,
                right,
                &[(label.clone(), body.clone(), p.text_2)],
            );
            pieces(
                painter,
                x,
                y + 22.0,
                right,
                &[(
                    "Not on GitHub, GitLab or Azure DevOps: no pull requests or CI here".into(),
                    body.clone(),
                    p.text_3,
                )],
            );
        }
        RemoteLine::Forge { label } => {
            pieces(
                painter,
                x,
                y,
                right,
                &[(label.clone(), body.clone(), p.text_2)],
            );
            pieces(
                painter,
                x,
                y + 22.0,
                right,
                &[(
                    "Merge requests and pipelines on the Review and Actions pages".into(),
                    body.clone(),
                    p.text_3,
                )],
            );
        }
        RemoteLine::GitHub {
            name,
            loading,
            error,
            ci,
            unpushed,
            prs,
            mine,
            failed,
            fixed,
        } => {
            let mut parts = vec![(format!("{name}  ·  "), body.clone(), p.text_3)];
            if *loading {
                parts.push(("loading GitHub data…".into(), body.clone(), p.text_2));
            } else if let Some(e) = error {
                parts.push((format!("GitHub: {e}"), body.clone(), p.danger));
            } else if *unpushed {
                parts.push(("HEAD is not on GitHub yet".into(), body.clone(), p.text_2));
            } else {
                match ci {
                    Some((o, summary)) => {
                        parts.push(("CI on HEAD ".into(), body.clone(), p.text_2));
                        parts.push((
                            ci_word(*o).into(),
                            strong.clone(),
                            outcome_tone(*o).color(&p),
                        ));
                        if !summary.is_empty() {
                            parts.push((format!("  ({summary})"), body.clone(), p.text_3));
                        }
                    }
                    None => parts.push(("no CI run for HEAD".into(), body.clone(), p.text_3)),
                }
            }
            pieces(painter, x, y, right, &parts);
            if !*loading && error.is_none() {
                let y = y + 22.0;
                let mut parts = vec![(
                    match prs {
                        Some(n) => format::plural(*n, "open pull request", "open pull requests"),
                        None => "pull requests not loaded".into(),
                    },
                    body.clone(),
                    p.text,
                )];
                if *mine > 0 {
                    parts.push((format!("  ·  {mine} mine"), body.clone(), p.text));
                }
                if *failed > 0 {
                    parts.push((
                        format!(
                            "  ·  {} on your branches today",
                            format::plural(*failed, "failed run", "failed runs")
                        ),
                        strong.clone(),
                        p.danger,
                    ));
                } else if *fixed > 0 {
                    parts.push((
                        format!("  ·  {fixed} failed today, fixed since"),
                        body.clone(),
                        p.text_2,
                    ));
                }
                pieces(painter, x, y, right, &parts);
            }
        }
    }

    // Footer: findings, background work, load time.
    let y = rect.bottom() - 20.0;
    painter.hline(
        (rect.left() + 12.0)..=(rect.right() - 12.0),
        y - 16.0,
        Stroke::new(1.0, p.border),
    );
    let mut bx = x;
    let mut badge = |text: &str, tone: Tone| {
        let w = kit::badge_width(painter, text);
        if bx + w < right - 120.0 {
            kit::paint_badge(painter, &p, dark, bx + w, y, text, tone);
            bx += w + 6.0;
        }
    };
    if s.urgent > 0 {
        badge(
            &format::plural(s.urgent, "urgent finding", "urgent findings"),
            Tone::Danger,
        );
    }
    if let Some(b) = &s.busy {
        badge(&format!("Running: {b}"), Tone::Accent);
    }
    if s.scanning {
        badge("Scanning", Tone::Accent);
    }
    if s.insights {
        badge("Insights running", Tone::Accent);
    }
    if s.urgent == 0 && s.busy.is_none() && !s.scanning && !s.insights {
        pieces(
            painter,
            x,
            y,
            right - 120.0,
            &[("No urgent findings".into(), body.clone(), p.text_3)],
        );
    }
    if let Some((at, ms)) = s.loaded {
        let t = format!(
            "loaded {} in {}",
            format::relative(at, now),
            format::millis(ms)
        );
        let g = painter.layout_no_wrap(t, FontId::proportional(size::SMALL), p.text_3);
        painter.galley(pos2(right - g.size().x, y - g.size().y / 2.0), g, p.text_3);
    }
}
