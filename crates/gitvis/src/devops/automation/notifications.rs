//! Desktop notifications: the poll timer (`tick`), the diff of each poll
//! against the previous ones, delivery, the in-app list, the Settings
//! section and the status bar item with its popup.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use chrono::Timelike;
use egui::{
    Align, Align2, CornerRadius, FontId, Frame, Layout, Margin, RichText, Sense, Stroke, Ui, pos2,
    vec2,
};

use gitvis_core::devops::automation::notify::{
    self as core, Delivery, Event, EventKind, LOW_RATE, MAX_REPOS, Plan, RunWatch, Seen, Watch,
};
use gitvis_core::devops::gh::Gh;
use gitvis_core::devops::home::iso_utc;
use gitvis_core::github::RateLimit;

use super::{Act, Msg, NotifyPrefs, PollDone};
use crate::devops::ctx::Ctx;
use crate::devops::global::{GlobalCx, GlobalView};
use crate::format;
use crate::state::{Action, Page, Settings, ToastKind};
use crate::theme::{Tone, medium, pal, radius, semibold, size, space};
use crate::views::form;
use crate::views::icons::{self, Icon, IconButton};
use crate::views::kit;

/// Minutes between checks the settings offer.
pub const INTERVALS: [u32; 4] = [2, 5, 10, 15];
/// Events kept in the in-app list.
const MAX_RECENT: usize = 60;
/// The first check after turning on (or starting) waits this long.
const FIRST_DELAY: Duration = Duration::from_secs(4);

/// One event in the in-app list.
#[derive(Clone, Debug)]
pub struct Recent {
    pub event: Event,
    /// Arrived during quiet hours (no desktop notification).
    pub quiet: bool,
}

/// What the last check did.
#[derive(Clone, Debug)]
pub struct LastPoll {
    pub epoch: i64,
    pub requests: usize,
    pub elapsed_ms: u64,
    pub errors: Vec<String>,
    pub new: usize,
    pub first: bool,
    pub stopped_low: bool,
}

#[derive(Default)]
pub struct Notify {
    pub seen: Seen,
    pub polling: bool,
    pub next: Option<Instant>,
    pub last: Option<LastPoll>,
    /// Epoch of the last check that read the notifications (for `since`).
    pub last_ok: Option<i64>,
    pub rate: Option<RateLimit>,
    pub graphql_left: Option<u64>,
    /// The last check was skipped because the API budget was low.
    pub low: Option<RateLimit>,
    pub login: Option<String>,
    /// Newest first.
    pub recent: Vec<Recent>,
    pub unseen: usize,
    pub popup: bool,
    pub delivered: Option<Result<String, String>>,
    pub checks: usize,
}

fn interval(p: &NotifyPrefs) -> Duration {
    Duration::from_secs(60 * u64::from(p.interval_min.clamp(1, 120)))
}

pub fn watch(p: &NotifyPrefs) -> Watch {
    Watch {
        runs: p.runs,
        reviews: p.reviews,
        mentions: p.mentions,
        approvals: p.approvals,
    }
}

/// Are we inside the quiet hours right now?
pub fn quiet_now(p: &NotifyPrefs) -> bool {
    if !p.quiet {
        return false;
    }
    let (Some(from), Some(to)) = (
        core::parse_hhmm(&p.quiet_from),
        core::parse_hhmm(&p.quiet_to),
    ) else {
        return false;
    };
    let now = chrono::Local::now();
    core::in_quiet_hours(now.hour() * 60 + now.minute(), from, to)
}

/// The repositories whose runs are read (the active one first), as
/// `(owner, repo)`.
fn run_repos(g: &GlobalCx) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let order = std::iter::once(g.active).chain(0..g.repos.len());
    for i in order {
        let Some((o, n)) = g.repos.get(i).and_then(|r| r.github_remote.as_ref()) else {
            continue;
        };
        if !out
            .iter()
            .any(|(a, b)| a.eq_ignore_ascii_case(o) && b.eq_ignore_ascii_case(n))
        {
            out.push((o.clone(), n.clone()));
        }
    }
    out.truncate(MAX_REPOS);
    out
}

fn plan(st: &Notify, g: &GlobalCx) -> Plan {
    let p = &g.settings.ext.automation.notify;
    Plan {
        watch: watch(p),
        since: st.last_ok.map(|e| iso_utc(e - 60)),
        repos: run_repos(g),
        login: st
            .login
            .clone()
            .or_else(|| g.settings.home.login.clone())
            .filter(|l| !l.is_empty()),
    }
}

/// The lowest REST budget we know of that has not reset yet.
fn known_rate(st: &Notify, g: &GlobalCx, now: i64) -> Option<RateLimit> {
    std::iter::once(st.rate)
        .chain(g.repos.iter().map(|r| r.rate))
        .flatten()
        .filter(|r| r.reset > now)
        .min_by_key(|r| r.remaining)
}

fn start_poll(cx: &mut Ctx, st: &mut Notify, g: &GlobalCx) {
    if st.polling {
        return;
    }
    let p = &g.settings.ext.automation.notify;
    let now = format::now_secs();
    st.next = Some(Instant::now() + interval(p));
    if let Some(r) = known_rate(st, g, now)
        && r.remaining < LOW_RATE
    {
        st.low = Some(r);
        // Try again after the reset (or the next interval, whichever is later).
        let wait = Duration::from_secs((r.reset - now).max(0) as u64 + 5);
        st.next = Some(Instant::now() + wait.max(interval(p)));
        return;
    }
    st.low = None;
    st.polling = true;
    let plan = plan(st, g);
    cx.user_read("Check for notifications", move |gh: &Gh, log| {
        let started = Instant::now();
        let poll = core::poll(gh, &plan, format::now_secs(), &mut log.calls);
        Msg::Polled(Box::new(PollDone {
            poll,
            requests: log.calls.len(),
            elapsed_ms: started.elapsed().as_millis() as u64,
        }))
    });
}

pub fn poll_summary(d: &PollDone) -> Result<String, String> {
    let p = &d.poll;
    let mut parts = vec![format!(
        "{} in {}",
        format::plural(d.requests, "request", "requests"),
        format::millis(d.elapsed_ms)
    )];
    if let Some(r) = &p.reviews {
        parts.push(format!("{} review requests", r.len()));
    }
    if let Some(t) = &p.threads {
        parts.push(format::plural(t.len(), "notification", "notifications"));
    }
    if !p.runs.is_empty() {
        let n: usize = p.runs.iter().map(|(_, r)| r.len()).sum();
        parts.push(format!("{n} runs in {} repositories", p.runs.len()));
    }
    if p.stopped_low {
        parts.push("stopped: API budget low".into());
    }
    let text = parts.join(", ");
    if !p.errors.is_empty() && p.threads.is_none() && p.reviews.is_none() && p.runs.is_empty() {
        Err(format!("{text}\n{}", p.errors.join("\n")))
    } else if p.errors.is_empty() {
        Ok(text)
    } else {
        Ok(format!("{text}\n{}", p.errors.join("\n")))
    }
}

/// Deliver in the background (`notify-rust` may take a moment).
fn deliver(cx: &mut Ctx, notes: Vec<core::Desktop>) {
    if notes.is_empty() {
        return;
    }
    let label = format!(
        "Show {}",
        format::plural(notes.len(), "desktop notification", "desktop notifications")
    );
    cx.user_read(label, move |_gh, log| {
        log.command(core::delivery().describe());
        Msg::Delivered(core::deliver(&notes, &iso_utc(format::now_secs())))
    });
}

pub fn apply(cx: &mut Ctx, st: &mut Notify, g: &mut GlobalCx, act: Act) {
    match act {
        Act::SetEnabled(on) => {
            let p = &mut g.settings.ext.automation.notify;
            p.enabled = on;
            if on {
                p.paused = false;
                st.next = Some(Instant::now() + FIRST_DELAY);
            } else {
                st.next = None;
                st.seen.reset();
                st.last_ok = None;
            }
        }
        Act::SetPaused(paused) => {
            g.settings.ext.automation.notify.paused = paused;
            st.next = (!paused).then(|| Instant::now() + FIRST_DELAY);
        }
        Act::PollNow => {
            st.next = Some(Instant::now());
            start_poll(cx, st, g);
        }
        Act::NotifyTest => {
            let d = core::delivery();
            if !matches!(d, Delivery::Log(_))
                || std::env::var("GITVIS_NOTIFY").as_deref() != Ok("log")
            {
                cx.toast(
                    ToastKind::Error,
                    "notify-test needs GITVIS_NOTIFY=log: it never shows a real notification",
                );
                return;
            }
            let e = sample(format::now_secs());
            push_recent(st, vec![e.clone()], false);
            deliver(cx, core::desktop_for(&[e]));
        }
        Act::SendTest => {
            let e = sample(format::now_secs());
            deliver(cx, core::desktop_for(&[e]));
        }
        Act::ShowList(open) => {
            st.popup = open;
            if open {
                st.unseen = 0;
            }
        }
        Act::OpenEvent(key) => {
            st.popup = false;
            let Some(e) = st
                .recent
                .iter()
                .find(|r| r.event.key == key)
                .map(|r| r.event.clone())
            else {
                return;
            };
            open_event(cx, g, &e);
        }
        Act::ClearRecent => {
            st.recent.clear();
            st.unseen = 0;
        }
        _ => {}
    }
}

/// Open what an event is about: the pull request or run in gitvis when the
/// repository is open, the web page otherwise.
fn open_event(cx: &mut Ctx, g: &GlobalCx, e: &Event) {
    let open = crate::devops::home::repo_index(g.repos, &e.repo);
    match (open, e.run, e.number) {
        (Some(i), Some(run), _) => {
            cx.action(Action::Activate(i));
            cx.action(Action::OpenRun { run, job: None });
        }
        (Some(i), None, Some(n)) if e.pr => {
            cx.action(Action::Activate(i));
            cx.action(Action::OpenPr(n));
        }
        _ if !e.url.is_empty() => cx.action(Action::OpenUrl(e.url.clone())),
        (Some(i), _, _) => {
            cx.action(Action::Activate(i));
            cx.action(Action::ShowPage(Page::GitHub));
        }
        _ => {}
    }
}

fn sample(now: i64) -> Event {
    Event {
        kind: EventKind::RunFailed,
        key: format!("test:{now}"),
        repo: "gitvis/test".into(),
        title: "CI failed on feature/notifications".into(),
        detail: "A test notification from gitvis (Settings → Notifications)".into(),
        url: String::new(),
        number: None,
        pr: false,
        run: None,
        at: now,
    }
}

fn push_recent(st: &mut Notify, events: Vec<Event>, quiet: bool) {
    let mut added = 0;
    for e in events.into_iter().rev() {
        if st.recent.iter().any(|r| r.event.key == e.key) {
            continue;
        }
        st.recent.insert(0, Recent { event: e, quiet });
        added += 1;
    }
    st.recent.truncate(MAX_RECENT);
    if !st.popup {
        st.unseen += added;
    }
}

pub fn on_msg(cx: &mut Ctx, st: &mut Notify, g: &mut GlobalCx, msg: Msg) {
    match msg {
        Msg::Polled(d) => {
            let PollDone {
                poll,
                requests,
                elapsed_ms,
            } = *d;
            st.polling = false;
            st.checks += 1;
            if poll.rate.is_some() {
                st.rate = poll.rate;
            }
            if poll.graphql_left.is_some() {
                st.graphql_left = poll.graphql_left;
            }
            if let Some(l) = poll.login.clone().filter(|l| !l.is_empty()) {
                st.login = Some(l);
            }
            if poll.threads.is_some() {
                st.last_ok = Some(poll.at);
            }
            let p = g.settings.ext.automation.notify.clone();
            let first = !st.seen.primed;
            let open = p
                .only_open
                .then(|| crate::devops::home::open_names(g.repos));
            let open: Option<&HashSet<String>> = open.as_ref();
            let events = core::diff(&mut st.seen, &poll, &watch(&p), open);
            st.last = Some(LastPoll {
                epoch: poll.at,
                requests,
                elapsed_ms,
                errors: poll.errors.clone(),
                new: events.len(),
                first,
                stopped_low: poll.stopped_low,
            });
            if poll.stopped_low {
                st.low = poll.rate;
            }
            if events.is_empty() || !p.enabled {
                return;
            }
            let quiet = quiet_now(&p);
            push_recent(st, events.clone(), quiet);
            if !quiet {
                deliver(cx, core::desktop_for(&events));
            }
        }
        Msg::Delivered(r) => st.delivered = Some(r),
        _ => {}
    }
}

/// Every frame: start a check when one is due, else wake up for the next.
pub fn tick(cx: &mut Ctx, st: &mut Notify, g: &mut GlobalCx) {
    let p = &g.settings.ext.automation.notify;
    if !p.enabled || p.paused {
        st.next = None;
        if !p.enabled && st.seen.primed {
            st.seen.reset();
            st.last_ok = None;
        }
        return;
    }
    let now = Instant::now();
    let due = *st.next.get_or_insert(now + FIRST_DELAY);
    if now >= due {
        if !st.polling {
            start_poll(cx, st, g);
        }
    } else {
        g.egui.request_repaint_after(due - now);
    }
}

// ---------------------------------------------------------------------------
// Status line
// ---------------------------------------------------------------------------

/// `Watching · last check 2 min ago, next in 3 min`.
fn status_text(st: &Notify, p: &NotifyPrefs, now: i64) -> (String, Tone) {
    if !p.enabled {
        return ("Off".into(), Tone::Neutral);
    }
    if p.paused {
        return ("Paused: no checks until you resume".into(), Tone::Warning);
    }
    if let Some(r) = st.low {
        return (
            format!(
                "Waiting for the API budget: {} of {} requests left (checks pause below {LOW_RATE}), resets {}",
                format::count(r.remaining as usize),
                format::count(r.limit as usize),
                format::clock(r.reset)
            ),
            Tone::Warning,
        );
    }
    if st.polling {
        return ("Checking now…".into(), Tone::Accent);
    }
    let mut s = String::from("Watching");
    if let Some(l) = &st.last {
        s.push_str(&format!(
            " · last check {} ({})",
            format::relative(l.epoch, now),
            format::clock(l.epoch)
        ));
    }
    if let Some(n) = st.next {
        let secs = n.saturating_duration_since(Instant::now()).as_secs();
        s.push_str(&format!(
            " · next {}",
            if secs < 60 {
                format!("in {secs} s")
            } else {
                format!("in {} min", secs.div_ceil(60))
            }
        ));
    }
    if quiet_now(p) {
        s.push_str(" · quiet hours");
    }
    (s, Tone::Success)
}

fn kind_style(kind: EventKind) -> (Icon, Tone) {
    match kind {
        EventKind::RunFailed => (Icon::Error, Tone::Danger),
        EventKind::RunFinished => (Icon::Check, Tone::Success),
        EventKind::ReviewRequested => (Icon::PullRequest, Tone::Accent),
        EventKind::Mentioned => (Icon::Comment, Tone::Accent),
        EventKind::ApprovalRequested => (Icon::Rocket, Tone::Warning),
    }
}

/// One event row: icon, title, repository · what · when. Returns true when
/// clicked.
fn event_row(ui: &mut Ui, r: &Recent, now: i64) -> bool {
    let p = *pal(ui);
    let h = 50.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    let hovered = ui.rect_contains_pointer(rect);
    kit::row_background(ui, rect, hovered, false);
    let (icon, tone) = kind_style(r.event.kind);
    let painter = ui.painter();
    icons::paint(
        painter,
        icon,
        pos2(rect.left() + 18.0, rect.top() + 17.0),
        14.0,
        tone.color(&p),
    );
    let x = rect.left() + 36.0;
    let right = rect.right() - 10.0;
    let when = format::relative(r.event.at, now);
    let wg = painter.layout_no_wrap(when, FontId::proportional(size::SMALL), p.text_3);
    let ww = wg.size().x;
    painter.galley(pos2(right - ww, rect.top() + 9.0), wg, p.text_3);
    let tg = crate::views::truncated(
        painter,
        &r.event.title,
        FontId::new(size::BODY, medium()),
        p.text,
        (right - ww - 10.0 - x).max(40.0),
    );
    painter.galley(pos2(x, rect.top() + 8.0), tg, p.text);
    let mut sub = format!("{} · {}", r.event.repo, r.event.detail);
    if r.quiet {
        sub.push_str(" · during quiet hours");
    }
    let sg = crate::views::truncated(
        painter,
        &sub,
        FontId::proportional(size::SMALL),
        p.text_2,
        (right - x).max(40.0),
    );
    painter.galley(pos2(x, rect.top() + 28.0), sg, p.text_2);
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.clicked()
}

/// The status bar item (drawn right-to-left) and, when open, the list.
pub fn status_item(ui: &mut Ui, st: &Notify, v: &GlobalView, actions: &mut Vec<Action>) {
    let pr = &v.settings.ext.automation.notify;
    if !pr.enabled {
        return;
    }
    let p = *pal(ui);
    let now = format::now_secs();
    let (text, color) = if st.unseen > 0 {
        (format!("{} new", st.unseen), p.accent)
    } else if pr.paused {
        ("Notifications paused".to_owned(), p.text_3)
    } else if st.low.is_some() {
        ("Notifications waiting".to_owned(), p.warning)
    } else {
        (String::new(), p.text_3)
    };
    let font = FontId::new(size::SMALL - 0.5, medium());
    let g = ui.painter().layout_no_wrap(text.clone(), font, color);
    let w = 18.0
        + if text.is_empty() {
            0.0
        } else {
            g.size().x + 4.0
        };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 18.0), Sense::click());
    let hovered = ui.rect_contains_pointer(rect);
    if hovered || st.popup {
        ui.painter().rect_filled(
            rect.expand2(vec2(4.0, 1.0)),
            CornerRadius::same(radius::S),
            p.hover,
        );
    }
    icons::paint(
        ui.painter(),
        Icon::Bell,
        pos2(rect.left() + 8.0, rect.center().y),
        12.0,
        if st.unseen > 0 { p.accent } else { color },
    );
    if !text.is_empty() {
        ui.painter().galley(
            pos2(rect.left() + 18.0, rect.center().y - g.size().y / 2.0),
            g,
            color,
        );
    }
    let (tip, _) = status_text(st, pr, now);
    let resp = resp
        .on_hover_text(format!("Notifications: {tip}\nClick for the list"))
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.clicked() {
        actions.push(Act::ShowList(!st.popup).into());
    }
    if st.popup {
        popup(ui, st, pr, rect, now, actions);
    }
}

fn popup(
    ui: &mut Ui,
    st: &Notify,
    pr: &NotifyPrefs,
    anchor: egui::Rect,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let width = 400.0;
    let area = egui::Area::new(egui::Id::unique("automation_notification_list"))
        .order(egui::Order::Foreground)
        .pivot(Align2::RIGHT_BOTTOM)
        .fixed_pos(pos2(anchor.right() + 4.0, anchor.top() - 8.0))
        .show(ui.ctx(), |ui| {
            Frame::NONE
                .fill(p.overlay)
                .stroke(Stroke::new(1.0, p.border_strong))
                .corner_radius(CornerRadius::same(radius::L))
                .inner_margin(Margin::symmetric(12, 10))
                .shadow(ui.visuals().popup_shadow)
                .show(ui, |ui| {
                    ui.set_width(width);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Notifications")
                                .size(size::TITLE)
                                .family(semibold())
                                .color(p.text_strong),
                        );
                        if !st.recent.is_empty() {
                            ui.label(
                                RichText::new(format::count(st.recent.len()))
                                    .size(size::SMALL)
                                    .color(p.text_3),
                            );
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            if IconButton::icon(Icon::Close)
                                .compact()
                                .show(ui)
                                .on_hover_text("Close")
                                .clicked()
                            {
                                actions.push(Act::ShowList(false).into());
                            }
                            if IconButton::icon(Icon::Gear)
                                .compact()
                                .show(ui)
                                .on_hover_text("Notification settings")
                                .clicked()
                            {
                                actions.push(Act::ShowList(false).into());
                                actions.push(
                                    crate::devops::DevOpsAction::SetSettingsSection(
                                        crate::devops::settings_page::Section::Notifications,
                                    )
                                    .into(),
                                );
                                actions.push(Action::ShowPage(Page::Settings));
                            }
                            let label = if pr.paused { "Resume" } else { "Pause" };
                            if IconButton::text(label).compact().show(ui).clicked() {
                                actions.push(Act::SetPaused(!pr.paused).into());
                            }
                            if IconButton::new(Icon::Rerun, "Check now")
                                .compact()
                                .enabled(!st.polling)
                                .show(ui)
                                .clicked()
                            {
                                actions.push(Act::PollNow.into());
                            }
                        });
                    });
                    let (line, tone) = status_text(st, pr, now);
                    let c = match tone {
                        Tone::Neutral | Tone::Success => p.text_2,
                        t => t.color(&p),
                    };
                    ui.add(
                        egui::Label::new(RichText::new(line).size(size::SMALL).color(c)).wrap(),
                    );
                    ui.add_space(space::XS);
                    kit::hairline(ui);
                    ui.add_space(space::XS);
                    if st.recent.is_empty() {
                        ui.add_space(space::S);
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "Nothing new since gitvis started. Every {} minutes gitvis checks your runs, review requests, mentions and deployment approvals; what is new appears here and as a desktop notification.",
                                    pr.interval_min
                                ))
                                .size(size::SMALL)
                                .color(p.text_2),
                            )
                            .wrap(),
                        );
                        ui.add_space(space::S);
                        return;
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("automation_notification_scroll")
                        .max_height(420.0)
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            for r in st.recent.iter().take(30) {
                                if event_row(ui, r, now) {
                                    actions.push(Act::OpenEvent(r.event.key.clone()).into());
                                }
                            }
                        });
                    ui.add_space(space::XS);
                    ui.horizontal(|ui| {
                        if kit::link(ui, "Clear the list").clicked() {
                            actions.push(Act::ClearRecent.into());
                        }
                        ui.add_space(space::M);
                        if kit::link(ui, "Open the inbox").clicked() {
                            actions.push(Act::ShowList(false).into());
                            actions.push(Action::ShowPage(Page::Home));
                            actions.push(
                                crate::devops::home::Act::SetTab(
                                    crate::devops::home::Tab::Notifications,
                                )
                                .into(),
                            );
                        }
                    });
                });
        });
    // Close on a click elsewhere or Escape.
    let rect = area.response.rect;
    let (clicked_outside, escape) = ui.input(|i| {
        let outside = i.pointer.any_pressed()
            && i.pointer
                .interact_pos()
                .is_some_and(|pos| !rect.contains(pos) && !anchor.expand(4.0).contains(pos));
        (outside, i.key_pressed(egui::Key::Escape))
    });
    if clicked_outside || escape {
        actions.push(Act::ShowList(false).into());
    }
}

// ---------------------------------------------------------------------------
// Settings → Notifications
// ---------------------------------------------------------------------------

pub fn settings(ui: &mut Ui, st: &mut Notify, settings: &mut Settings, actions: &mut Vec<Action>) {
    kit::page_header(
        ui,
        "Notifications",
        "Desktop notifications for runs, review requests, mentions and approvals",
        |_| {},
    );
    let now = format::now_secs();
    kit::scroll_page(ui, "automation_notifications", |ui| {
        ui.set_max_width(ui.available_width().min(kit::PROSE));
        let p = &mut settings.ext.automation.notify;
        let on = p.enabled;
        form::card(ui, if on { Tone::Accent } else { Tone::Neutral }, |ui| {
            let mut v = on;
            if form::checkbox(ui, &mut v, "Desktop notifications").clicked() {
                actions.push(Act::SetEnabled(v).into());
            }
            ui.add_space(space::XS);
            kit::body(
                ui,
                if on {
                    "gitvis checks GitHub in the background with your gh login and shows a desktop notification for what is new since the last check. The first check only takes note of what is there."
                } else {
                    "Off. When on, gitvis checks GitHub in the background with your gh login every few minutes and shows a desktop notification for the events chosen under What to watch."
                },
            );
            if !on {
                ui.add_space(2.0);
                kit::secondary(
                    ui,
                    "Those are: one of your runs finishing, a review request, a mention, a deployment waiting for your approval.",
                );
            }
            if !on {
                return;
            }
            ui.add_space(space::S);
            let (line, tone) = status_text(st, p, now);
            let delivery = core::delivery();
            kit::kv(ui, 120.0, |kv| {
                kv.row("Status", |ui| {
                    let c = match tone {
                        Tone::Neutral => pal(ui).text,
                        t => t.color(pal(ui)),
                    };
                    ui.add(egui::Label::new(RichText::new(&line).color(c)).wrap());
                });
                match &st.last {
                    Some(l) => {
                        let mut s = format!(
                            "{} · {} · {}",
                            format::plural(l.requests, "request", "requests"),
                            format::millis(l.elapsed_ms),
                            if l.first {
                                "first check: took note of what is there".to_owned()
                            } else {
                                format::plural(l.new, "new event", "new events")
                            }
                        );
                        if l.stopped_low {
                            s.push_str(" · stopped early, API budget low");
                        }
                        kv.text("Last check", s);
                        if !l.errors.is_empty() {
                            kv.row("Errors", |ui| {
                                for e in &l.errors {
                                    form::error(ui, e);
                                }
                            });
                        }
                    }
                    None => kv.muted("Last check", "none yet"),
                }
                if let Some(r) = st.rate {
                    kv.text(
                        "API budget",
                        format!(
                            "{} of {} REST requests left, resets {}{}",
                            format::count(r.remaining as usize),
                            format::count(r.limit as usize),
                            format::clock(r.reset),
                            match st.graphql_left {
                                Some(g) =>
                                    format!(" · {} GraphQL points left", format::count(g as usize)),
                                None => String::new(),
                            }
                        ),
                    );
                }
                kv.text("Delivery", delivery.describe());
                if let Some(d) = &st.delivered {
                    match d {
                        Ok(s) => kv.muted("Last delivery", s),
                        Err(e) => kv.row("Last delivery", |ui| form::error(ui, e)),
                    }
                }
            });
            form::actions_row(ui, |ui| {
                if form::button(ui, "Send a test notification", true).clicked() {
                    actions.push(Act::SendTest.into());
                }
                let label = if p.paused { "Resume" } else { "Pause" };
                if form::button(ui, label, true).clicked() {
                    actions.push(Act::SetPaused(!p.paused).into());
                }
                if form::primary(ui, "Check now", !st.polling).clicked() {
                    actions.push(Act::PollNow.into());
                }
            });
        });

        kit::section(ui, "How often", None);
        let opts: Vec<(u32, String)> = INTERVALS.iter().map(|m| (*m, format!("{m} min"))).collect();
        let refs: Vec<(u32, &str)> = opts.iter().map(|(m, l)| (*m, l.as_str())).collect();
        let current = if INTERVALS.contains(&p.interval_min) {
            p.interval_min
        } else {
            5
        };
        if let Some(m) = kit::segmented(ui, &refs, current) {
            p.interval_min = m;
            if let Some(n) = st.next.as_mut() {
                *n = (*n).min(Instant::now() + interval(p));
            }
        }
        let cost = plan_preview(p, st.login.clone(), st);
        let per_hour = cost * (60 / p.interval_min.max(1) as usize);
        kit::secondary(
            ui,
            &format!(
                "{} per check, about {} an hour of the 5,000 your GitHub account gets. Checks pause below {LOW_RATE} requests left.",
                format::plural(cost, "request", "requests"),
                format::count(per_hour)
            ),
        );

        kit::section(ui, "What to watch", None);
        form::card(ui, Tone::Neutral, |ui| {
            form::row(ui, 230.0, "Workflow runs on my branches", |ui| {
                if let Some(r) = kit::segmented(
                    ui,
                    &[
                        (RunWatch::Off, "Off"),
                        (RunWatch::Failures, "Failures only"),
                        (RunWatch::All, "All finished"),
                    ],
                    p.runs,
                ) {
                    p.runs = r;
                }
            });
            kit::secondary(
                ui,
                &format!(
                    "Runs you started (actor = your login) in the repositories open in gitvis, at most {MAX_REPOS}, newest 20 each."
                ),
            );
            ui.add_space(space::S);
            form::checkbox(ui, &mut p.reviews, "New review requests");
            form::checkbox(ui, &mut p.mentions, "Mentions");
            form::checkbox(ui, &mut p.approvals, "Deployments waiting for my approval");
            kit::secondary(
                ui,
                "Review requests and mentions come from the inbox search (review-requested:@me, mentions:@me); approvals from GitHub notifications with the reason approval_requested (environments with required reviewers).",
            );
        });

        kit::section(ui, "Scope and quiet hours", None);
        form::card(ui, Tone::Neutral, |ui| {
            form::checkbox(ui, &mut p.only_open, "Only repositories open in gitvis");
            kit::secondary(
                ui,
                "Review requests, mentions and approvals in other repositories are ignored. Runs are always read for open repositories only.",
            );
            ui.add_space(space::S);
            ui.horizontal(|ui| {
                form::checkbox(ui, &mut p.quiet, "Quiet hours from");
                let bad_from = core::parse_hhmm(&p.quiet_from).is_none();
                form::input(
                    ui,
                    "automation_quiet_from",
                    &mut p.quiet_from,
                    form::InputOpts::new("22:00")
                        .width(64.0)
                        .mono()
                        .invalid(bad_from)
                        .enabled(p.quiet),
                );
                ui.label(RichText::new("to").size(size::BODY).color(pal(ui).text_2));
                let bad_to = core::parse_hhmm(&p.quiet_to).is_none();
                form::input(
                    ui,
                    "automation_quiet_to",
                    &mut p.quiet_to,
                    form::InputOpts::new("07:00")
                        .width(64.0)
                        .mono()
                        .invalid(bad_to)
                        .enabled(p.quiet),
                );
            });
            if p.quiet
                && (core::parse_hhmm(&p.quiet_from).is_none()
                    || core::parse_hhmm(&p.quiet_to).is_none())
            {
                form::error(ui, "Use 24-hour times like 22:00 and 07:30.");
            }
            kit::secondary(
                ui,
                "Local time. Checks continue during quiet hours, but nothing pops up: what arrives waits in the list behind the bell in the status bar.",
            );
        });

        kit::section(ui, "What one check sends", None);
        let preview = Plan {
            watch: watch(p),
            since: st.last_ok.map(|e| iso_utc(e - 60)),
            repos: Vec::new(),
            login: st.login.clone(),
        };
        let mut lines = preview.describe();
        if p.runs != RunWatch::Off {
            lines.push(format!(
                "gh api repos/OWNER/REPO/actions/runs?actor={}&per_page=20&exclude_pull_requests=true   (each open repository, at most {MAX_REPOS})",
                st.login.as_deref().unwrap_or("LOGIN")
            ));
        }
        if lines.is_empty() {
            kit::muted(ui, "Nothing is watched: turn something on above.");
        } else {
            kit::code_block(ui, &lines.join("\n"), actions);
        }
        kit::secondary(
            ui,
            "Every request is a read through gh api with your login and shows in the activity log (verbose). Nothing is marked as read on GitHub.",
        );

        kit::section_with(ui, "Recent", Some(st.recent.len()), |ui| {
            if !st.recent.is_empty() && IconButton::text("Clear").compact().show(ui).clicked() {
                actions.push(Act::ClearRecent.into());
            }
        });
        if st.recent.is_empty() {
            kit::muted(
                ui,
                if on {
                    "Nothing new yet. New events appear here and behind the bell in the status bar."
                } else {
                    "Turn notifications on to see new events here."
                },
            );
        } else {
            for r in st.recent.iter().take(MAX_RECENT) {
                if event_row(ui, r, now) {
                    actions.push(Act::OpenEvent(r.event.key.clone()).into());
                }
            }
        }
        if super::shared::take_scroll_end() {
            ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
        }
    });
}

/// Requests per check for the current settings (runs counted for the
/// repositories with a GitHub remote last seen, at least one).
fn plan_preview(p: &NotifyPrefs, login: Option<String>, st: &Notify) -> usize {
    let plan = Plan {
        watch: watch(p),
        since: None,
        repos: (0..st.seen.run_repos().max(1))
            .map(|i| (format!("o{i}"), "r".to_owned()))
            .collect(),
        login,
    };
    plan.cost()
}
