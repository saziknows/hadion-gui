//! OSINT sections of the Exposure page: people, footprint, file metadata,
//! history leftovers, GitHub surface and supply chain. The page frame
//! (navigation, section header, progress) is drawn by `views::security`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use egui::{Align2, CornerRadius, FontId, Rect, Response, RichText, Sense, Ui, pos2, vec2};

use gitvis_core::github::RepoInfo;
use gitvis_core::osint::{EmailKind, Identity, JunkFile, MetaFinding, OsintReport, Unreachable};
use gitvis_core::secrets::{self, CommitRef, Finding, Severity};
use gitvis_core::{Oid, Snapshot, repo};

use crate::format;
use crate::state::{Action, OsintState, RepoState, SecurityPage, SecurityTab};
use crate::theme::{Tone, pal, size, space};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Leading};
use crate::views::security::{
    KV_W, capitalized, code_box, detail_head, finding_badge_list, kv_colored, plural, sec,
    severity_badge, severity_dot, severity_tone, status_row, step, step_buttons, when, why,
};

/// What the sections need from the repository besides the OSINT state.
pub struct Cx<'a> {
    pub snap: Option<&'a Snapshot>,
    pub now: i64,
    pub github: Option<(String, String)>,
    pub info: Option<RepoInfo>,
    pub workdir: PathBuf,
    pub busy: bool,
    pub dismissed: HashSet<String>,
}

type Served = HashMap<Oid, Option<Result<bool, String>>>;

const DROP_COMMAND: &str = "git reflog expire --expire=now --all && git gc --prune=now";

// ---------------------------------------------------------------------------
// Items (shared with the script driver)
// ---------------------------------------------------------------------------

fn people_items<'a>(rep: &'a OsintReport, filter: &str) -> Vec<&'a Identity> {
    let needle = filter.trim().to_lowercase();
    rep.people
        .identities
        .iter()
        .filter(|i| {
            needle.is_empty()
                || i.email.to_lowercase().contains(&needle)
                || i.name().to_lowercase().contains(&needle)
                || i.domain.contains(&needle)
        })
        .collect()
}

fn footprint_items<'a>(
    rep: &'a OsintReport,
    dismissed: &HashSet<String>,
    filter: &str,
) -> Vec<&'a Finding> {
    let needle = filter.trim().to_lowercase();
    rep.footprint
        .iter()
        .filter(|f| !dismissed.contains(&f.fingerprint))
        .filter(|f| {
            needle.is_empty()
                || f.name.to_lowercase().contains(&needle)
                || f.secret.to_lowercase().contains(&needle)
                || f.paths().iter().any(|p| p.to_lowercase().contains(&needle))
        })
        .collect()
}

enum FileItem<'a> {
    Meta(&'a MetaFinding),
    Junk(&'a JunkFile),
}

impl FileItem<'_> {
    fn path(&self) -> &str {
        match self {
            FileItem::Meta(m) => &m.path,
            FileItem::Junk(j) => &j.path,
        }
    }
}

fn file_items<'a>(rep: &'a OsintReport, filter: &str) -> Vec<FileItem<'a>> {
    let needle = filter.trim().to_lowercase();
    rep.metadata
        .iter()
        .map(FileItem::Meta)
        .chain(rep.junk.iter().map(FileItem::Junk))
        .filter(|it| needle.is_empty() || it.path().to_lowercase().contains(&needle))
        .collect()
}

/// Keys of the current section's list, in display order (for the script
/// driver).
pub fn item_keys(repo: &RepoState) -> Vec<String> {
    let page = &repo.sec_page;
    let Some(rep) = &page.osint.report else {
        return Vec::new();
    };
    let filter = &page.osint.filter;
    match page.tab {
        SecurityTab::People => people_items(rep, filter)
            .into_iter()
            .map(|i| i.email.clone())
            .collect(),
        SecurityTab::Footprint => footprint_items(rep, &repo.dismissed, filter)
            .into_iter()
            .map(|f| f.fingerprint.clone())
            .collect(),
        SecurityTab::Files => file_items(rep, filter)
            .iter()
            .map(|it| it.path().to_owned())
            .collect(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Section body
// ---------------------------------------------------------------------------

pub fn show(ui: &mut Ui, cx: &Cx, page: &mut SecurityPage, actions: &mut Vec<Action>) {
    let tab = page.tab;
    let SecurityPage {
        osint,
        info,
        alerts,
        ..
    } = page;
    if osint.report.is_none() && tab != SecurityTab::GitHub {
        if let Some(e) = &osint.error {
            if kit::empty_state(
                ui,
                Icon::Warning,
                "The analysis failed",
                e,
                Some((Icon::Rerun, "Try again")),
            ) {
                actions.push(Action::RunOsint);
            }
        } else if osint.running() {
            kit::empty_state(
                ui,
                Icon::Search,
                "Analysing",
                "Reading commits, files, metadata and history leftovers. Local only; nothing leaves the machine.",
                None,
            );
        } else if kit::empty_state(
            ui,
            Icon::Search,
            "Not analysed yet",
            "The analysis reads commits, files, file metadata, history leftovers and dependencies on this machine; nothing leaves it.",
            Some((Icon::Search, "Analyse")),
        ) {
            actions.push(Action::RunOsint);
        }
        return;
    }
    match tab {
        SecurityTab::People => people_tab(ui, cx, osint, actions),
        SecurityTab::Footprint => footprint_tab(ui, cx, osint, actions),
        SecurityTab::Files => files_tab(ui, cx, osint, actions),
        SecurityTab::History => history_tab(ui, cx, osint, actions),
        SecurityTab::GitHub => github_tab(ui, cx, osint, info, alerts, actions),
        SecurityTab::Supply => supply_tab(ui, cx, osint, actions),
        SecurityTab::Secrets => {}
    }
}

// ---------------------------------------------------------------------------
// Shared pieces (also used by `views::insights`)
// ---------------------------------------------------------------------------

pub(crate) fn filter_box(ui: &mut Ui, filter: &mut String, id: &str, hint: &str) {
    kit::search_box(ui, id, filter, hint);
    ui.add_space(space::S);
}

pub(crate) fn copy_button(ui: &mut Ui, label: &str, text: &str, actions: &mut Vec<Action>) {
    if IconButton::new(Icon::Copy, label)
        .compact()
        .show(ui)
        .clicked()
    {
        actions.push(Action::Copy(text.to_owned()));
    }
}

/// Rows outside a virtualised list, without gaps between them.
fn rows(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        add(ui)
    });
}

fn row_rect(ui: &mut Ui, row: &kit::Row, clickable: bool) -> (Rect, Response) {
    let two = !row.subtitle.is_empty() || !row.badges.is_empty();
    let h = if two { kit::ROW_H } else { kit::ROW_H_ONE };
    let sense = if clickable {
        Sense::click()
    } else {
        Sense::hover()
    };
    ui.allocate_exact_size(vec2(ui.available_width(), h), sense)
}

/// One list row for short lists inside a scrolling page.
fn static_row(ui: &mut Ui, row: &kit::Row, clickable: bool) -> Response {
    let (rect, resp) = row_rect(ui, row, clickable);
    if ui.is_rect_visible(rect) {
        kit::row_background(ui, rect, clickable && resp.hovered(), false);
        kit::paint_row(ui, rect, row, false);
    }
    if clickable && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// [`static_row`] with an icon button at its right end. Returns the row's
/// response and whether the button was clicked.
fn static_row_with(
    ui: &mut Ui,
    row: &kit::Row,
    clickable: bool,
    icon: Icon,
    tip: &str,
) -> (Response, bool) {
    let (rect, resp) = row_rect(ui, row, clickable);
    let button =
        Rect::from_center_size(pos2(rect.right() - 20.0, rect.center().y), vec2(26.0, 26.0));
    if ui.is_rect_visible(rect) {
        kit::row_background(ui, rect, clickable && resp.hovered(), false);
        let inner = Rect::from_min_max(rect.min, pos2(rect.right() - 34.0, rect.bottom()));
        kit::paint_row(ui, inner, row, false);
    }
    let clicked = kit::overlay(ui, button, IconButton::icon(icon).compact())
        .on_hover_text(tip)
        .clicked();
    if clickable && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    (resp, clicked)
}

/// A commit row: summary, short id and author, time; clicking shows it in
/// the graph when it is there, the context menu copies the SHA.
fn commit_row(
    ui: &mut Ui,
    cx: &Cx,
    c: &CommitRef,
    leading: Leading,
    sub_extra: &str,
    badges: &[(String, Tone)],
    actions: &mut Vec<Action>,
) {
    let in_graph = cx.snap.is_some_and(|s| s.index_of.contains_key(&c.id));
    let mut sub = format!("{} · {}", repo::short_id(c.id), c.author);
    if !sub_extra.is_empty() {
        sub = format!("{sub_extra} · {sub}");
    }
    let meta = format::relative(c.time, cx.now);
    let row = kit::Row::new(&c.summary)
        .leading(leading)
        .subtitle(&sub)
        .meta(&meta)
        .badges(badges);
    let resp = static_row(ui, &row, in_graph);
    if in_graph && resp.clicked() {
        actions.push(Action::JumpTo(c.id));
    }
    resp.context_menu(|ui| {
        if ui.button("Copy commit SHA").clicked() {
            actions.push(Action::Copy(c.id.to_string()));
            ui.close();
        }
        if in_graph && ui.button("Show in the graph").clicked() {
            actions.push(Action::JumpTo(c.id));
            ui.close();
        }
    });
}

/// "and 12 more" under a cut list.
fn more(ui: &mut Ui, shown: usize, total: usize) {
    if total > shown {
        ui.add_space(space::XS);
        kit::muted(ui, &format!("and {} more", format::count(total - shown)));
    }
}

/// Short text with an ellipsis.
fn short(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n).collect::<String>())
    } else {
        s.to_owned()
    }
}

/// Weekday × hour heat map of commits.
pub(crate) fn heatmap(ui: &mut Ui, heat: &[[u32; 24]; 7]) {
    const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let cell = 18.0;
    let label_w = 40.0;
    let header_h = 20.0;
    let size = vec2(label_w + 24.0 * cell, header_h + 7.0 * cell);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    let font = FontId::proportional(size::CAPTION);
    let max = heat.iter().flatten().copied().max().unwrap_or(0).max(1) as f32;
    for h in (0..24).step_by(3) {
        painter.text(
            pos2(rect.left() + label_w + h as f32 * cell, rect.top()),
            Align2::LEFT_TOP,
            format!("{h:02}"),
            font.clone(),
            p.text_3,
        );
    }
    let empty = if dark { p.raised } else { p.hover };
    for (d, name) in DAYS.iter().enumerate() {
        let y = rect.top() + header_h + d as f32 * cell;
        painter.text(
            pos2(rect.left(), y + (cell - 2.0) / 2.0),
            Align2::LEFT_CENTER,
            *name,
            font.clone(),
            if d >= 5 { p.warning } else { p.text_3 },
        );
        for (h, &count) in heat[d].iter().enumerate() {
            let r = Rect::from_min_size(
                pos2(rect.left() + label_w + h as f32 * cell, y),
                vec2(cell - 2.0, cell - 2.0),
            );
            let color = if count == 0 {
                empty
            } else {
                p.accent
                    .gamma_multiply(0.22 + 0.78 * (count as f32 / max).sqrt())
            };
            painter.rect_filled(r, CornerRadius::same(3), color);
        }
    }
    if let Some(pos) = resp.hover_pos() {
        let h = ((pos.x - rect.left() - label_w) / cell).floor();
        let d = ((pos.y - rect.top() - header_h) / cell).floor();
        if (0.0..24.0).contains(&h) && (0.0..7.0).contains(&d) {
            let n = heat[d as usize][h as usize];
            resp.on_hover_text_at_pointer(format!(
                "{} {:02}:00 to {:02}:59: {}",
                DAYS[d as usize],
                h as u32,
                h as u32,
                plural(n as usize, "commit", "commits")
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// People
// ---------------------------------------------------------------------------

fn kind_badge(k: EmailKind) -> (String, Tone) {
    let tone = if k == EmailKind::Personal {
        Tone::Warning
    } else {
        Tone::Neutral
    };
    (capitalized(k.label()), tone)
}

fn kind_why(k: EmailKind) -> &'static str {
    match k {
        EmailKind::Personal => {
            "A personal mailbox names the human behind the commits directly: it links the work to a private identity, is the address phishing goes to, and cannot be rotated like a token."
        }
        EmailKind::Corporate => {
            "A company address names the employer and usually follows a guessable pattern (first.last@), which gives an attacker the format of every colleague's address."
        }
        EmailKind::NoReply => {
            "A GitHub noreply address reveals only the GitHub login and keeps the real mailbox out of the history. This is the safe choice for public repositories."
        }
        EmailKind::Invalid => {
            "Not a deliverable address: usually a misconfigured machine (user@localhost) or a placeholder. Harmless for privacy, bad for attribution."
        }
    }
}

/// The address is readable by anyone in pushed commits of a public
/// repository.
fn exposed(id: &Identity, public: bool) -> bool {
    public && id.pushed > 0 && matches!(id.kind(), EmailKind::Personal | EmailKind::Corporate)
}

fn display_name(id: &Identity) -> &str {
    if id.name().is_empty() {
        &id.email
    } else {
        id.name()
    }
}

fn people_tab(ui: &mut Ui, cx: &Cx, o: &mut OsintState, actions: &mut Vec<Action>) {
    let Some(rep) = &o.report else { return };
    let ids = people_items(rep, &o.filter);
    let selected = o.selected.clone();
    let mut filter = o.filter.clone();
    let public = cx.info.as_ref().is_some_and(|i| i.is_public());
    kit::split_keyed(
        ui,
        "osint_people",
        selected.as_deref().unwrap_or(""),
        actions,
        |ui, actions| {
            filter_box(
                ui,
                &mut filter,
                "osint_people_filter",
                "Filter by name, e-mail or domain",
            );
            kit::list_header(ui, "Addresses", Some(ids.len()), |_| {});
            ui.add_space(space::XS);
            if ids.is_empty() {
                kit::muted(ui, "No address matches the filter.");
                return;
            }
            kit::list(
                ui,
                "osint_people_list",
                ids.len(),
                kit::ROW_H,
                |i| selected.as_deref() == Some(ids[i].email.as_str()),
                |ui, rect, resp, i| {
                    let id = ids[i];
                    let sel = selected.as_deref() == Some(id.email.as_str());
                    let mut badges = vec![kind_badge(id.kind())];
                    if !id.aliases.is_empty() {
                        badges.push((plural(id.aliases.len(), "alias", "aliases"), Tone::Neutral));
                    }
                    if exposed(id, public) {
                        badges.push(("Public".into(), Tone::Danger));
                    }
                    let sub = if id.name().is_empty() {
                        String::new()
                    } else {
                        id.email.clone()
                    };
                    let meta = plural(id.commits, "commit", "commits");
                    let row = kit::Row::new(display_name(id))
                        .leading(Leading::Avatar(display_name(id)))
                        .subtitle(&sub)
                        .meta(&meta)
                        .badges(&badges);
                    kit::paint_row(ui, rect, &row, sel);
                    if resp.clicked() {
                        actions.push(Action::SelectOsint(Some(id.email.clone())));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Copy e-mail").clicked() {
                            actions.push(Action::Copy(id.email.clone()));
                            ui.close();
                        }
                        if ui.button("Copy git log command").clicked() {
                            actions.push(Action::Copy(format!(
                                "git log --all --author=\"{}\"",
                                id.email
                            )));
                            ui.close();
                        }
                    });
                },
            );
        },
        |ui, actions| match selected
            .as_deref()
            .and_then(|e| rep.people.identities.iter().find(|i| i.email == e))
        {
            Some(id) => person_detail(ui, cx, rep, id, public, actions),
            None => people_overview(ui, rep, public, actions),
        },
    );
    o.filter = filter;
}

fn offset_label(min: i32) -> String {
    let sign = if min < 0 { '-' } else { '+' };
    let a = min.abs();
    format!("UTC{sign}{:02}:{:02}", a / 60, a % 60)
}

/// "3 days", "8 months", "4.2 years".
fn span_text(secs: i64) -> String {
    let days = secs.max(0) / 86_400;
    if days < 1 {
        "Under a day".to_owned()
    } else if days < 60 {
        plural(days as usize, "day", "days")
    } else if days < 730 {
        plural((days / 30) as usize, "month", "months")
    } else {
        format!("{:.1} years", days as f64 / 365.25)
    }
}

fn percent(n: usize, total: usize) -> String {
    let pct = n * 100 / total.max(1);
    if pct == 0 && n > 0 {
        "<1%".to_owned()
    } else {
        format!("{pct}%")
    }
}

fn person_detail(
    ui: &mut Ui,
    cx: &Cx,
    rep: &OsintReport,
    id: &Identity,
    public: bool,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let kind = id.kind();
    let mut badges = vec![kind_badge(kind)];
    if !id.aliases.is_empty() {
        badges.push((plural(id.aliases.len(), "alias", "aliases"), Tone::Neutral));
    }
    detail_head(ui, &badges, display_name(id), |ui| {
        copy_button(ui, "Copy e-mail", &id.email, actions);
        copy_button(
            ui,
            "Copy git log",
            &format!("git log --all --author=\"{}\"", id.email),
            actions,
        );
    });
    ui.label(kit::mono(&id.email).color(p.text_2));
    if exposed(id, public) {
        ui.add_space(space::XS);
        kit::callout(
            ui,
            Tone::Danger,
            "Readable by anyone",
            &format!(
                "The repository is public: this address is in {} that anyone can read, and GitHub's commit API returns it in JSON.",
                plural(id.pushed, "pushed commit", "pushed commits")
            ),
        );
    }
    why(ui, kind_why(kind));

    sec(ui, "Activity", None);
    kit::stats(
        ui,
        &[
            kit::Stat::new("Authored", format::count(id.commits)),
            kit::Stat::new("Committed", format::count(id.committed)),
            kit::Stat::new("Pushed", format::count(id.pushed))
                .tone_if(exposed(id, public), Tone::Danger)
                .tip("Authored commits reachable from a remote-tracking branch"),
            kit::Stat::new(
                "Active for",
                if id.commits > 0 {
                    span_text(id.last - id.first)
                } else {
                    "None".to_owned()
                },
            ),
        ],
    );
    if id.commits > 0 {
        ui.add_space(space::XS);
        kit::kv(ui, KV_W, |kv| {
            kv.text("First commit", when(id.first, cx.now));
            kv.text("Last commit", when(id.last, cx.now));
        });
    }

    if id.names.len() > 1 {
        sec(ui, "Names used", Some(id.names.len()));
        let rows: Vec<Vec<String>> = id
            .names
            .iter()
            .map(|(n, c)| vec![n.clone(), format::count(*c)])
            .collect();
        kit::table(
            ui,
            &[kit::Col::text("Name"), kit::Col::num("Commits")],
            &rows,
        );
    }

    if !id.aliases.is_empty() {
        sec(ui, "Other addresses", Some(id.aliases.len()));
        kit::secondary(
            ui,
            "Addresses used with the same name: probably the same person with several mailboxes.",
        );
        rows(ui, |ui| {
            for a in &id.aliases {
                let commits = rep
                    .people
                    .identities
                    .iter()
                    .find(|i| &i.email == a)
                    .map(|i| plural(i.commits, "commit", "commits"))
                    .unwrap_or_default();
                let kind = gitvis_core::osint::email_kind(a);
                let badges = [kind_badge(kind)];
                let row = kit::Row::new(a)
                    .leading(Leading::Avatar(a))
                    .subtitle(&commits)
                    .badges(&badges);
                if static_row(ui, &row, true).clicked() {
                    actions.push(Action::SelectOsint(Some(a.clone())));
                }
            }
        });
    }

    if !id.offsets.is_empty() {
        sec(ui, "Time zones", Some(id.offsets.len()));
        let total: usize = id.offsets.iter().map(|(_, n)| n).sum();
        let rows: Vec<Vec<String>> = id
            .offsets
            .iter()
            .take(10)
            .map(|(o, n)| vec![offset_label(*o), format::count(*n), percent(*n, total)])
            .collect();
        kit::table(
            ui,
            &[
                kit::Col::text("UTC offset"),
                kit::Col::num("Commits"),
                kit::Col::num("Share"),
            ],
            &rows,
        );
        more(ui, 10, id.offsets.len());
        ui.add_space(space::XS);
        kit::secondary(
            ui,
            if id.offsets.len() > 1 {
                "Several offsets usually mean travel, daylight-saving changes, or more than one machine; a single one pins the time zone."
            } else {
                "A single offset pins the time zone."
            },
        );
    }

    if id.commits > 0 {
        sec(ui, "Working hours", None);
        kit::kv(ui, KV_W, |kv| {
            if let Some(h) = id.peak_hour() {
                kv.text(
                    "Busiest hour",
                    format!("{h:02}:00 to {h:02}:59, local time"),
                );
            }
            kv.text(
                "Weekends",
                format!("{} of the commits", percent(id.weekend, id.commits)),
            );
            kv.text(
                "Night",
                format!(
                    "{} of the commits (22:00 to 06:00)",
                    percent(id.night, id.commits)
                ),
            );
        });
        ui.add_space(space::M);
        heatmap(ui, &id.heat);
        ui.add_space(space::XS);
        kit::secondary(
            ui,
            "Authored commits by local weekday and hour (author time at the offset recorded in each commit). Working hours give away the time zone and whether this is a day job.",
        );
    }

    sec(ui, "What to do", None);
    let mut n = 1;
    if matches!(kind, EmailKind::Personal | EmailKind::Corporate) {
        step(
            ui,
            n,
            "Commit with a noreply address in public repositories",
            "GitHub's noreply address (Settings, Emails, “Keep my email addresses private”) keeps the mailbox out of the history; past commits keep the old address unless history is rewritten.",
            |ui| {
                kit::code_block(
                    ui,
                    "git config user.email \"ID+login@users.noreply.github.com\"",
                    actions,
                )
            },
        );
        n += 1;
    }
    if !id.aliases.is_empty() {
        step(
            ui,
            n,
            "Decide whether the aliases should be linked",
            "A .mailmap file maps old addresses to one canonical identity for git log and git shortlog; it does not change history, but it documents the link for everyone.",
            |_| {},
        );
        n += 1;
    }
    step(
        ui,
        n,
        "Know what the history says about you",
        "This is what any clone shows: the first command lists every commit, the second the exact times with offsets.",
        |ui| {
            kit::code_block(
                ui,
                &format!(
                    "git log --all --author=\"{}\"\ngit log --all --author=\"{}\" --format=%ai",
                    id.email, id.email
                ),
                actions,
            )
        },
    );
}

fn people_overview(ui: &mut Ui, rep: &OsintReport, public: bool, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let people = &rep.people;
    let personal = people.count(EmailKind::Personal);
    let exposed_n = people
        .identities
        .iter()
        .filter(|i| exposed(i, public))
        .count();
    let with_aliases = people
        .identities
        .iter()
        .filter(|i| !i.aliases.is_empty())
        .count();
    let mut tiles = vec![
        kit::Stat::new("Addresses", format::count(people.identities.len())),
        kit::Stat::new("Commits", format::count(people.total_commits)),
        kit::Stat::new("Personal addresses", format::count(personal))
            .tone_if(personal > 0, Tone::Warning),
    ];
    if public {
        tiles.push(
            kit::Stat::new("Readable by anyone", format::count(exposed_n))
                .tone_if(exposed_n > 0, Tone::Danger)
                .tip("Personal or company addresses in pushed commits of this public repository"),
        );
    }
    tiles.push(
        kit::Stat::new("With aliases", format::count(with_aliases))
            .tip("Addresses that share a name with another address"),
    );
    kit::stats(ui, &tiles);
    kit::secondary(
        ui,
        "Every author and committer address in the history, what kind of address it is, and what the commit times give away. Select an address on the left.",
    );

    sec(ui, "Your configuration", None);
    let k = (!people.local_email.is_empty())
        .then(|| gitvis_core::osint::email_kind(&people.local_email));
    kit::kv(ui, KV_W, |kv| {
        match k {
            None => kv_colored(
                kv,
                "user.email",
                "Not set: git invents one from the machine name",
                p.warning,
            ),
            Some(k) => kv.row("user.email", |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(kit::mono(&people.local_email).color(p.text));
                    let (t, tone) = kind_badge(k);
                    kit::badge(ui, &t, tone);
                    if !people.local_scope.is_empty() {
                        ui.label(
                            RichText::new(format!("{} scope", people.local_scope))
                                .size(size::SMALL)
                                .color(p.text_3),
                        );
                    }
                });
            }),
        }
        if !people.local_name.is_empty() {
            kv.text("user.name", &people.local_name);
        }
    });
    if public && k.is_some_and(|k| matches!(k, EmailKind::Personal | EmailKind::Corporate)) {
        ui.add_space(space::S);
        kit::callout_with(
            ui,
            Tone::Danger,
            "Your next commit publishes this address",
            "The repository is public. Use GitHub's noreply address for public work:",
            |ui| {
                kit::code_block(
                    ui,
                    "git config user.email \"ID+login@users.noreply.github.com\"",
                    actions,
                )
            },
        );
    }

    sec(ui, "Kinds of address", None);
    let mut rows = Vec::new();
    for k in [
        EmailKind::Personal,
        EmailKind::Corporate,
        EmailKind::NoReply,
        EmailKind::Invalid,
    ] {
        let n = people.count(k);
        if n == 0 {
            continue;
        }
        let commits: usize = people
            .identities
            .iter()
            .filter(|i| i.kind() == k)
            .map(|i| i.commits)
            .sum();
        rows.push(vec![
            capitalized(k.label()),
            format::count(n),
            format::count(commits),
        ]);
    }
    kit::table(
        ui,
        &[
            kit::Col::text("Kind"),
            kit::Col::num("Addresses"),
            kit::Col::num("Commits"),
        ],
        &rows,
    );

    let mut domains: Vec<(String, u32)> = Vec::new();
    for i in &people.identities {
        match domains.iter_mut().find(|(d, _)| *d == i.domain) {
            Some(e) => e.1 += 1,
            None => domains.push((i.domain.clone(), 1)),
        }
    }
    domains.sort_by_key(|d| std::cmp::Reverse(d.1));
    if !domains.is_empty() {
        sec(ui, "Domains", Some(domains.len()));
        kit::count_bars(ui, &domains, p.accent, 12);
    }

    let mut heat = [[0u32; 24]; 7];
    for i in &people.identities {
        for (row, src) in heat.iter_mut().zip(i.heat.iter()) {
            for (cell, v) in row.iter_mut().zip(src.iter()) {
                *cell += v;
            }
        }
    }
    sec(ui, "When the team works", None);
    heatmap(ui, &heat);
    ui.add_space(space::XS);
    kit::secondary(
        ui,
        "All authored commits by local weekday and hour. A clear nine-to-five block in one time zone tells an outsider where the team sits and when nobody is watching.",
    );
}

// ---------------------------------------------------------------------------
// Footprint
// ---------------------------------------------------------------------------

const PII_RULES: [&str; 5] = [
    "iban",
    "payment-card",
    "us-ssn",
    "phone-number",
    "email-address",
];

fn footprint_tab(ui: &mut Ui, cx: &Cx, o: &mut OsintState, actions: &mut Vec<Action>) {
    let Some(rep) = &o.report else { return };
    let items = footprint_items(rep, &cx.dismissed, &o.filter);
    let selected = o.selected.clone();
    let mut filter = o.filter.clone();
    kit::split_keyed(
        ui,
        "osint_footprint",
        selected.as_deref().unwrap_or(""),
        actions,
        |ui, actions| {
            filter_box(
                ui,
                &mut filter,
                "osint_fp_filter",
                "Filter by kind, value or path",
            );
            kit::list_header(ui, "Identifiers", Some(items.len()), |_| {});
            ui.add_space(space::XS);
            if items.is_empty() {
                kit::muted(ui, "No infrastructure identifiers or personal data found.");
                return;
            }
            let p = *pal(ui);
            kit::list(
                ui,
                "osint_fp_list",
                items.len(),
                kit::ROW_H,
                |i| selected.as_deref() == Some(items[i].fingerprint.as_str()),
                |ui, rect, resp, i| {
                    let f = items[i];
                    let sel = selected.as_deref() == Some(f.fingerprint.as_str());
                    let mut sub = f.secret.clone();
                    if let Some(first) = f.occurrences.first() {
                        sub.push_str(&format!(" · {}:{}", first.path, first.line));
                    }
                    let files = f.paths().len();
                    let meta = if files > 1 {
                        plural(files, "file", "files")
                    } else if f.occurrence_count > 1 {
                        format!("{}×", f.occurrence_count)
                    } else {
                        String::new()
                    };
                    let badges = finding_badge_list(f);
                    let row = kit::Row::new(f.name)
                        .leading(Leading::Dot(severity_dot(&p, f.severity)))
                        .subtitle(&sub)
                        .meta(&meta)
                        .badges(&badges);
                    kit::paint_row(ui, rect, &row, sel);
                    if resp.clicked() {
                        actions.push(Action::SelectOsint(Some(f.fingerprint.clone())));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Copy value").clicked() {
                            actions.push(Action::Copy(f.secret.clone()));
                            ui.close();
                        }
                        if ui.button("Dismiss (expected here)").clicked() {
                            actions.push(Action::DismissFinding(f.fingerprint.clone()));
                            ui.close();
                        }
                    });
                },
            );
        },
        |ui, actions| match selected
            .as_deref()
            .and_then(|k| rep.footprint.iter().find(|f| f.fingerprint == k))
        {
            Some(f) => footprint_detail(ui, cx, f, actions),
            None => footprint_overview(ui, cx, rep),
        },
    );
    o.filter = filter;
}

fn footprint_overview(ui: &mut Ui, cx: &Cx, rep: &OsintReport) {
    let live: Vec<&Finding> = rep
        .footprint
        .iter()
        .filter(|f| !cx.dismissed.contains(&f.fingerprint))
        .collect();
    let pii = live.iter().filter(|f| PII_RULES.contains(&f.rule)).count();
    let pushed = live.iter().filter(|f| f.pushed).count();
    kit::stats(
        ui,
        &[
            kit::Stat::new("Identifiers", format::count(live.len())),
            kit::Stat::new("Personal data", format::count(pii))
                .tone_if(pii > 0, Tone::Danger)
                .tip("IBANs, card numbers, social security and phone numbers, e-mail addresses"),
            kit::Stat::new("Pushed", format::count(pushed)).tone_if(pushed > 0, Tone::Warning),
            kit::Stat::new("Files read", format::count(rep.stats.files)),
        ],
    );
    kit::secondary(
        ui,
        "Infrastructure identifiers and personal data named in the files. None of these are credentials, but together they map your systems and your people. Select one on the left.",
    );
    let mut by_rule: Vec<(&str, usize, Severity)> = Vec::new();
    for f in &live {
        match by_rule.iter_mut().find(|(n, _, _)| *n == f.name) {
            Some(e) => e.1 += 1,
            None => by_rule.push((f.name, 1, f.severity)),
        }
    }
    by_rule.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)));
    if !by_rule.is_empty() {
        sec(ui, "By kind", Some(by_rule.len()));
        let rows: Vec<Vec<String>> = by_rule
            .iter()
            .map(|(n, c, s)| vec![(*n).to_owned(), s.label().to_owned(), format::count(*c)])
            .collect();
        kit::table(
            ui,
            &[
                kit::Col::text("Kind"),
                kit::Col::text("Severity"),
                kit::Col::num("Distinct values"),
            ],
            &rows,
        );
    }
    let dismissed = rep.footprint.len() - live.len();
    if dismissed > 0 {
        ui.add_space(space::S);
        kit::muted(
            ui,
            &format!(
                "{} dismissed as expected here.",
                plural(dismissed, "identifier", "identifiers")
            ),
        );
    }
}

fn footprint_detail(ui: &mut Ui, cx: &Cx, f: &Finding, actions: &mut Vec<Action>) {
    let mut badges = vec![severity_badge(f.severity)];
    if f.test_only {
        badges.push(("Test files".into(), Tone::Neutral));
    }
    detail_head(ui, &badges, f.name, |ui| {
        if IconButton::new(Icon::Close, "Dismiss")
            .compact()
            .show(ui)
            .on_hover_text("Expected here; hide it (remembered for this repository)")
            .clicked()
        {
            actions.push(Action::DismissFinding(f.fingerprint.clone()));
        }
        copy_button(ui, "Copy value", &f.secret, actions);
    });
    why(ui, f.why);
    sec(ui, "Value", None);
    code_box(ui, &f.secret);
    sec(ui, "Where", Some(f.occurrence_count));
    let h = (f.occurrences.len() as f32 * kit::ROW_H).min(kit::ROW_H * 6.0);
    egui::ScrollArea::vertical()
        .id_salt(("fp_occ", &f.fingerprint))
        .max_height(h)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            rows(ui, |ui| {
                for o in &f.occurrences {
                    let title = format!("{}:{}", o.path, o.line);
                    let ctx = o.text.trim();
                    let row = kit::Row::new(&title).mono().subtitle(ctx);
                    let resp = static_row(ui, &row, true);
                    if resp.clicked() {
                        actions.push(Action::OpenPath(cx.workdir.join(&o.path)));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Copy path").clicked() {
                            actions.push(Action::Copy(o.path.clone()));
                            ui.close();
                        }
                        if ui.button("Open file").clicked() {
                            actions.push(Action::OpenPath(cx.workdir.join(&o.path)));
                            ui.close();
                        }
                    });
                }
            });
        });
    more(ui, f.occurrences.len(), f.occurrence_count);
    sec(ui, "What to do", None);
    let mut n = 1;
    if PII_RULES.contains(&f.rule) {
        step(
            ui,
            n,
            "Remove the personal data",
            "Replace it with synthetic values in fixtures and exports; real records do not belong in a repository, private or not.",
            |_| {},
        );
        n += 1;
        let path = f.paths().first().copied().unwrap_or("").to_owned();
        step(
            ui,
            n,
            "Rewrite history if it was pushed",
            "A later commit leaves the old content in every clone. git-filter-repo can drop the file from all commits:",
            |ui| kit::code_block(ui, &secrets::purge_file_instructions(&path), actions),
        );
        n += 1;
    } else {
        step(
            ui,
            n,
            "Decide whether it belongs in this repository",
            "Hostnames, account IDs and bucket names are fine in private deployment repositories and a gift in public ones. Move environment-specific values to deployment configuration or a secrets manager, and template them in the code.",
            |_| {},
        );
        n += 1;
        step(
            ui,
            n,
            "Assume it is known",
            "If the repository is public or was ever pushed somewhere public, treat the identifier as enumerated: check the bucket's access policy, the account's trust policies, the host's exposure.",
            |_| {},
        );
        n += 1;
    }
    step(
        ui,
        n,
        "Expected here?",
        "Dismiss it, or put a gitvis:allow comment on the line.",
        |ui| {
            step_buttons(ui, |ui| {
                if IconButton::new(Icon::Close, "Dismiss")
                    .compact()
                    .show(ui)
                    .clicked()
                {
                    actions.push(Action::DismissFinding(f.fingerprint.clone()));
                }
            })
        },
    );
}

// ---------------------------------------------------------------------------
// Files (metadata and junk)
// ---------------------------------------------------------------------------

fn has_gps(m: &MetaFinding) -> bool {
    m.fields.iter().any(|(k, _)| k.starts_with("GPS"))
}

fn files_tab(ui: &mut Ui, cx: &Cx, o: &mut OsintState, actions: &mut Vec<Action>) {
    let Some(rep) = &o.report else { return };
    let items = file_items(rep, &o.filter);
    let selected = o.selected.clone();
    let mut filter = o.filter.clone();
    kit::split_keyed(
        ui,
        "osint_files",
        selected.as_deref().unwrap_or(""),
        actions,
        |ui, actions| {
            filter_box(ui, &mut filter, "osint_files_filter", "Filter by path");
            kit::list_header(ui, "Files", Some(items.len()), |_| {});
            ui.add_space(space::XS);
            if items.is_empty() {
                kit::muted(
                    ui,
                    "No image, document or binary carries metadata, and no junk files are tracked.",
                );
                return;
            }
            let p = *pal(ui);
            kit::list(
                ui,
                "osint_files_list",
                items.len(),
                kit::ROW_H,
                |i| selected.as_deref() == Some(items[i].path()),
                |ui, rect, resp, i| {
                    let item = &items[i];
                    let key = item.path();
                    let sel = selected.as_deref() == Some(key);
                    let (sev, sub, size, badges) = match item {
                        FileItem::Meta(m) => {
                            let mut b = vec![(m.kind.to_owned(), Tone::Neutral)];
                            if has_gps(m) {
                                b.push(("Location".into(), Tone::Danger));
                            }
                            (m.severity, m.path.clone(), m.size, b)
                        }
                        FileItem::Junk(j) => (
                            Severity::Low,
                            format!("{} · {}", j.path, j.reason),
                            j.size,
                            vec![("Junk".to_owned(), Tone::Neutral)],
                        ),
                    };
                    let meta = format::bytes(size as usize);
                    let row = kit::Row::new(secrets::basename(key))
                        .leading(Leading::Dot(severity_dot(&p, sev)))
                        .subtitle(&sub)
                        .meta(&meta)
                        .badges(&badges);
                    kit::paint_row(ui, rect, &row, sel);
                    if resp.clicked() {
                        actions.push(Action::SelectOsint(Some(key.to_owned())));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Copy path").clicked() {
                            actions.push(Action::Copy(key.to_owned()));
                            ui.close();
                        }
                        if ui.button("Open file").clicked() {
                            actions.push(Action::OpenPath(cx.workdir.join(key)));
                            ui.close();
                        }
                    });
                },
            );
        },
        |ui, actions| {
            let sel = selected.as_deref();
            if let Some(m) = sel.and_then(|k| rep.metadata.iter().find(|m| m.path == k)) {
                meta_detail(ui, cx, m, actions);
            } else if let Some(j) = sel.and_then(|k| rep.junk.iter().find(|j| j.path == k)) {
                junk_detail(ui, cx, j, actions);
            } else {
                files_overview(ui, rep);
            }
        },
    );
    o.filter = filter;
}

fn files_overview(ui: &mut Ui, rep: &OsintReport) {
    let gps = rep.metadata.iter().filter(|m| has_gps(m)).count();
    kit::stats(
        ui,
        &[
            kit::Stat::new("With metadata", format::count(rep.metadata.len())),
            kit::Stat::new("With a location", format::count(gps))
                .tone_if(gps > 0, Tone::Danger)
                .tip("Images with GPS coordinates"),
            kit::Stat::new("Junk files", format::count(rep.junk.len()))
                .tone_if(!rep.junk.is_empty(), Tone::Warning),
        ],
    );
    kit::secondary(
        ui,
        "Metadata inside committed images, PDFs, Office documents and binaries (camera, GPS, author, company, local build paths), and files that only exist because a developer machine made them. Select one on the left.",
    );
    let mut by_kind: Vec<(String, u32)> = Vec::new();
    for m in &rep.metadata {
        match by_kind.iter_mut().find(|(k, _)| k == m.kind) {
            Some(e) => e.1 += 1,
            None => by_kind.push((m.kind.to_owned(), 1)),
        }
    }
    if !rep.junk.is_empty() {
        by_kind.push(("Junk".to_owned(), rep.junk.len() as u32));
    }
    by_kind.sort_by_key(|e| std::cmp::Reverse(e.1));
    if !by_kind.is_empty() {
        sec(ui, "By kind", None);
        let p = *pal(ui);
        kit::count_bars(ui, &by_kind, p.accent, 10);
    }
    sec(ui, "What is checked", None);
    kit::kv(ui, KV_W, |kv| {
        kv.text(
            "Images",
            "JPEG and TIFF EXIF (camera, owner, serial numbers, GPS), PNG text chunks and XMP",
        );
        kv.text(
            "Documents",
            "PDF Info and XMP; Office Open XML core and app properties; OpenDocument meta.xml",
        );
        kv.text(
            "Binaries",
            "Printable strings of executables, libraries and bytecode for absolute paths with a user name",
        );
        kv.text(
            "Junk",
            ".DS_Store, Thumbs.db, editor swap and backup files, logs, crash dumps, compiled Python, IDE workspace files",
        );
    });
}

fn file_actions(ui: &mut Ui, cx: &Cx, path: &str, actions: &mut Vec<Action>) {
    copy_button(ui, "Copy path", path, actions);
    if IconButton::new(Icon::External, "Open")
        .compact()
        .show(ui)
        .clicked()
    {
        actions.push(Action::OpenPath(cx.workdir.join(path)));
    }
}

fn meta_detail(ui: &mut Ui, cx: &Cx, m: &MetaFinding, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let mut badges = vec![
        severity_badge(m.severity),
        (m.kind.to_owned(), Tone::Neutral),
    ];
    if has_gps(m) {
        badges.push(("Location".into(), Tone::Danger));
    }
    detail_head(ui, &badges, secrets::basename(&m.path), |ui| {
        file_actions(ui, cx, &m.path, actions)
    });
    why(ui, m.why);
    sec(ui, "File", None);
    kit::kv(ui, KV_W, |kv| {
        kv.text("Path", kit::mono(&m.path));
        kv.text("Size", format::bytes(m.size as usize));
    });
    sec(ui, "Fields", Some(m.fields.len()));
    kit::kv(ui, KV_W, |kv| {
        for (k, v) in &m.fields {
            if k == "Map" {
                kv.row("Map", |ui| {
                    if kit::link(ui, "Open the location on OpenStreetMap").clicked() {
                        actions.push(Action::OpenUrl(v.clone()));
                    }
                });
            } else if k.starts_with("GPS") {
                kv_colored(kv, k, v, p.danger);
            } else {
                kv.text(k, v);
            }
        }
    });
    sec(ui, "What to do", None);
    step(
        ui,
        1,
        "Strip the metadata and commit the clean file",
        match m.kind {
            "JPEG" | "PNG" | "TIFF" => {
                "exiftool -all= removes every tag; most image editors have an “export without metadata” option."
            }
            "PDF" => {
                "exiftool -all= or qpdf --empty; re-exporting from the source document with metadata disabled is cleaner."
            }
            "Office document" | "OpenDocument" => {
                "File, Info, Inspect Document (Word, Excel, PowerPoint) or File, Properties, Reset (LibreOffice) before saving a copy."
            }
            _ => {
                "Rebuild the artifact on a CI machine with a neutral path, or stop committing build output altogether."
            }
        },
        |ui| kit::code_block(ui, &format!("exiftool -all= \"{}\"", m.path), actions),
    );
    step(
        ui,
        2,
        "Remember that the old version stays in history",
        "Committing the stripped file does not remove the original from past commits; rewrite history if the metadata matters (GPS, serial numbers).",
        |ui| kit::code_block(ui, &secrets::purge_file_instructions(&m.path), actions),
    );
}

fn junk_detail(ui: &mut Ui, cx: &Cx, j: &JunkFile, actions: &mut Vec<Action>) {
    detail_head(
        ui,
        &[("Junk".to_owned(), Tone::Neutral)],
        secrets::basename(&j.path),
        |ui| file_actions(ui, cx, &j.path, actions),
    );
    why(
        ui,
        "Files a developer machine made say which tools, editors and operating systems the team uses, and sometimes hold local paths, user names or crash data.",
    );
    sec(ui, "File", None);
    kit::kv(ui, KV_W, |kv| {
        kv.text("Path", kit::mono(&j.path));
        kv.text("Size", format::bytes(j.size as usize));
        kv.text("What it is", j.reason);
    });
    sec(ui, "What to do", None);
    let base = secrets::basename(&j.path).to_owned();
    step(
        ui,
        1,
        "Stop tracking it and ignore it",
        "git rm --cached keeps the file on disk; a .gitignore pattern keeps it from coming back.",
        |ui| {
            step_buttons(ui, |ui| {
                if IconButton::new(Icon::Minus, "Stop tracking")
                    .compact()
                    .enabled(!cx.busy)
                    .show(ui)
                    .clicked()
                {
                    actions.push(Action::Run(gitvis_core::ops::GitOp::Untrack {
                        paths: vec![j.path.clone()],
                    }));
                }
                if IconButton::new(Icon::Plus, &format!("Ignore {base}"))
                    .compact()
                    .show(ui)
                    .clicked()
                {
                    actions.push(Action::AddToGitignore(base.clone()));
                }
            });
        },
    );
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

fn served_badge(state: Option<&Option<Result<bool, String>>>) -> Option<(String, Tone)> {
    match state {
        None => None,
        Some(None) => Some(("Checking".into(), Tone::Neutral)),
        Some(Some(Ok(true))) => Some(("On GitHub".into(), Tone::Danger)),
        Some(Some(Ok(false))) => Some(("Not on GitHub".into(), Tone::Success)),
        Some(Some(Err(_))) => Some(("Check failed".into(), Tone::Warning)),
    }
}

/// Rows of commits with their "still served by GitHub?" state.
fn served_rows(
    ui: &mut Ui,
    cx: &Cx,
    commits: &[CommitRef],
    served: &Served,
    actions: &mut Vec<Action>,
) {
    rows(ui, |ui| {
        for c in commits {
            let state = served.get(&c.id);
            let badges: Vec<(String, Tone)> = served_badge(state).into_iter().collect();
            let extra = match state {
                Some(Some(Err(e))) => format!(
                    "check failed: {}",
                    short(e.lines().next().unwrap_or(""), 60)
                ),
                _ => String::new(),
            };
            commit_row(ui, cx, c, Leading::None, &extra, &badges, actions);
        }
    });
}

/// "Ask GitHub about N commits" for the ones not checked yet.
fn ask_github(
    ui: &mut Ui,
    cx: &Cx,
    commits: &[CommitRef],
    served: &Served,
    actions: &mut Vec<Action>,
) {
    if cx.github.is_none() {
        return;
    }
    let shas: Vec<Oid> = commits.iter().map(|c| c.id).take(30).collect();
    let pending = shas.iter().filter(|s| !served.contains_key(s)).count();
    if pending > 0
        && IconButton::new(
            Icon::Play,
            &format!(
                "Ask GitHub about {}",
                plural(pending.min(30), "commit", "commits")
            ),
        )
        .compact()
        .show(ui)
        .on_hover_text("One API request per commit: does GitHub still return it by SHA?")
        .clicked()
    {
        actions.push(Action::CheckServed(shas));
    }
}

fn history_tab(ui: &mut Ui, cx: &Cx, o: &mut OsintState, actions: &mut Vec<Action>) {
    let Some(rep) = &o.report else { return };
    let a = &rep.archaeology;
    let m = &rep.messages;
    let served = &o.served;
    let unreachable = o.unreachable.as_ref();
    let unreachable_run = o.unreachable_run.as_ref();
    let pr_refs = o.pr_refs.as_ref();
    let pr_running = o.pr_refs_running;
    kit::scroll_page(ui, "osint_history", |ui| {
        let p = *pal(ui);
        let stash_secrets = a.stash_findings.len();
        kit::stats(
            ui,
            &[
                kit::Stat::new("Incident messages", format::count(m.incidents.len()))
                    .tone_if(!m.incidents.is_empty(), Tone::Warning),
                kit::Stat::new("Rewritten commits", format::count(a.rewritten_total))
                    .tone_if(a.rewritten_total > 0, Tone::Warning),
                kit::Stat::new("Stashes", format::count(a.stashes.len()))
                    .tone_if(stash_secrets > 0, Tone::Danger),
                kit::Stat::new("Leftover branches", format::count(a.backup_branches.len())),
                kit::Stat::new(
                    "Deleted sensitive files",
                    format::count(a.deleted_files.len()),
                )
                .tone_if(!a.deleted_files.is_empty(), Tone::Warning),
            ],
        );

        // Incidents.
        sec(
            ui,
            "Commit messages that describe security events",
            Some(m.incidents.len()),
        );
        if m.incidents.is_empty() {
            kit::muted(
                ui,
                "No commit message mentions removed secrets, leaks, disabled checks, hacks or accidents.",
            );
        }
        rows(ui, |ui| {
            for inc in m.incidents.iter().take(60) {
                let dot = if inc.pushed { p.danger } else { p.warning };
                let badges: Vec<(String, Tone)> = if inc.pushed {
                    vec![("Pushed".into(), Tone::Danger)]
                } else {
                    Vec::new()
                };
                commit_row(
                    ui,
                    cx,
                    &inc.commit,
                    Leading::Dot(dot),
                    &capitalized(inc.reason),
                    &badges,
                    actions,
                );
            }
        });
        more(ui, 60, m.incidents.len());

        // Rewritten commits.
        sec(
            ui,
            "Rewritten commits still in .git",
            Some(a.rewritten_total),
        );
        if a.rewritten.is_empty() {
            kit::muted(
                ui,
                "Nothing is reachable only through the reflog: amends, rebases and resets left no leftovers (or they were garbage-collected).",
            );
        } else {
            kit::secondary(
                ui,
                "Reachable only through the reflog: amended, rebased or reset-away commits. They stay in this clone until the reflog expires (90 days) and git gc runs, and if they were pushed before the rewrite, the server still serves them by SHA.",
            );
            ask_github(ui, cx, &a.rewritten, served, actions);
            let shown = a.rewritten.len().min(40);
            served_rows(ui, cx, &a.rewritten[..shown], served, actions);
            more(ui, shown, a.rewritten_total);
            ui.add_space(space::S);
            kit::secondary(ui, "To drop them from this clone:");
            kit::code_block(ui, DROP_COMMAND, actions);
        }

        // Stashes.
        sec(ui, "Stashes", Some(a.stashes.len()));
        if a.stashes.is_empty() {
            kit::muted(ui, "No stash entries.");
        } else {
            kit::secondary(
                ui,
                "Stashes are local, but stash entries survive branch deletions and are rarely reviewed; a secret stashed “for later” is a secret in .git.",
            );
            rows(ui, |ui| {
                for s in &a.stashes {
                    let sub = format!("stash@{{{}}}", s.index);
                    let meta = format::relative(s.time, cx.now);
                    let badges: Vec<(String, Tone)> = if s.findings > 0 {
                        vec![(plural(s.findings, "secret", "secrets"), Tone::Danger)]
                    } else {
                        Vec::new()
                    };
                    let row = kit::Row::new(&s.message)
                        .subtitle(&sub)
                        .meta(&meta)
                        .badges(&badges);
                    let resp = static_row(ui, &row, false);
                    resp.context_menu(|ui| {
                        if ui.button("Copy stash SHA").clicked() {
                            actions.push(Action::Copy(s.id.to_string()));
                            ui.close();
                        }
                    });
                }
            });
            if !a.stash_findings.is_empty() {
                ui.add_space(space::S);
                kit::subtitle(ui, "Secrets inside stashes");
                secret_table(ui, &a.stash_findings);
            }
        }

        // Backup branches.
        sec(
            ui,
            "Branches that look like leftovers",
            Some(a.backup_branches.len()),
        );
        if a.backup_branches.is_empty() {
            kit::muted(
                ui,
                "No branch named backup, old, wip, tmp, scratch or similar.",
            );
        }
        rows(ui, |ui| {
            for b in a.backup_branches.iter().take(60) {
                let in_graph = cx.snap.is_some_and(|s| s.index_of.contains_key(&b.tip));
                let meta = format::relative(b.time, cx.now);
                let badges: Vec<(String, Tone)> = if b.remote {
                    vec![("On the remote".into(), Tone::Danger)]
                } else {
                    Vec::new()
                };
                let reason = capitalized(&b.reason);
                let row = kit::Row::new(&b.name)
                    .mono()
                    .subtitle(&reason)
                    .meta(&meta)
                    .badges(&badges);
                let resp = static_row(ui, &row, in_graph);
                if in_graph && resp.clicked() {
                    actions.push(Action::JumpTo(b.tip));
                }
                resp.context_menu(|ui| {
                    if ui.button("Copy branch name").clicked() {
                        actions.push(Action::Copy(b.name.clone()));
                        ui.close();
                    }
                });
            }
        });
        more(ui, 60, a.backup_branches.len());

        // Deleted sensitive files.
        sec(
            ui,
            "Deleted files with sensitive names",
            Some(a.deleted_files.len()),
        );
        if a.deleted_files.is_empty() {
            kit::muted(
                ui,
                "No deleted file in the scanned commits has a sensitive name.",
            );
        } else {
            kit::secondary(
                ui,
                "Deleting a file does not remove it: the parent of the deleting commit still has it, and so does every clone. The copy button gives the command that prints it.",
            );
        }
        rows(ui, |ui| {
            for d in a.deleted_files.iter().take(60) {
                let in_graph = cx
                    .snap
                    .is_some_and(|s| s.index_of.contains_key(&d.commit.id));
                let sub = format!(
                    "{} · deleted in {}",
                    d.rule.name,
                    repo::short_id(d.commit.id)
                );
                let meta = format::relative(d.commit.time, cx.now);
                let badges: Vec<(String, Tone)> = if d.pushed {
                    vec![("Pushed".into(), Tone::Danger)]
                } else {
                    Vec::new()
                };
                let row = kit::Row::new(&d.path)
                    .mono()
                    .leading(Leading::Dot(severity_dot(&p, d.rule.severity)))
                    .subtitle(&sub)
                    .meta(&meta)
                    .badges(&badges);
                let (resp, copy) = static_row_with(
                    ui,
                    &row,
                    in_graph,
                    Icon::Copy,
                    "Copy the command that prints the deleted file",
                );
                if copy {
                    actions.push(Action::Copy(d.resurrect()));
                }
                if in_graph && resp.clicked() {
                    actions.push(Action::JumpTo(d.commit.id));
                }
            }
        });
        more(ui, 60, a.deleted_files.len());

        // Unreachable objects.
        sec(ui, "Unreachable objects", None);
        kit::secondary(
            ui,
            "git fsck --unreachable lists objects no ref and no reflog reaches: leftovers of deleted branches, rebases and git add of files never committed. They are still readable here and, if ever pushed, on the server. Seconds on small repositories, up to a minute on large ones.",
        );
        ui.add_space(space::XS);
        if let Some(run) = unreachable_run {
            if kit::progress(ui, run, "Finding unreachable objects", true) {
                run.cancel.store(true, Ordering::Relaxed);
            }
        } else if IconButton::new(
            Icon::Search,
            if unreachable.is_some() {
                "Run again"
            } else {
                "Find unreachable objects"
            },
        )
        .compact()
        .show(ui)
        .clicked()
        {
            actions.push(Action::RunUnreachable);
        }
        match unreachable {
            Some(Ok(u)) => unreachable_section(ui, cx, u, served, actions),
            Some(Err(e)) => kit::callout(ui, Tone::Danger, "The search failed", e),
            None => {}
        }

        // PR refs.
        sec(ui, "Pull-request refs on the remote", None);
        kit::secondary(
            ui,
            "GitHub advertises refs/pull/N/head for every pull request ever opened, including closed ones and force-pushed-over versions. Heads that are not in any branch are commits anyone can still fetch by number.",
        );
        ui.add_space(space::XS);
        if pr_running {
            kit::loading(ui, "Listing pull-request refs (git ls-remote)");
        } else if IconButton::new(
            Icon::Fetch,
            if pr_refs.is_some() {
                "Check again"
            } else {
                "List pull-request refs"
            },
        )
        .compact()
        .show(ui)
        .on_hover_text("git ls-remote --refs origin refs/pull/*/head (network)")
        .clicked()
        {
            actions.push(Action::CheckPrRefs);
        }
        match pr_refs {
            Some(Ok(pr)) => {
                ui.add_space(space::S);
                kit::stats(
                    ui,
                    &[
                        kit::Stat::new("Advertised heads", format::count(pr.total)),
                        kit::Stat::new("Not in any branch", format::count(pr.unknown_heads))
                            .tone_if(pr.unknown_heads > 0, Tone::Warning)
                            .tip("Heads this clone has no branch for"),
                    ],
                );
                kit::kv(ui, KV_W, |kv| {
                    kv.text("Remote", &pr.remote);
                    kv.text("Took", format::millis(pr.elapsed_ms));
                });
                if !pr.sample.is_empty() {
                    ui.add_space(space::S);
                    kit::subtitle(ui, "Newest");
                    let rows: Vec<Vec<String>> = pr
                        .sample
                        .iter()
                        .map(|(n, sha)| vec![format!("#{n}"), sha[..sha.len().min(12)].to_owned()])
                        .collect();
                    kit::table(
                        ui,
                        &[kit::Col::text("Pull request"), kit::Col::text("Head")],
                        &rows,
                    );
                }
                ui.add_space(space::S);
                kit::secondary(ui, "Fetch one to see what it contains:");
                kit::code_block(ui, "git fetch origin refs/pull/N/head", actions);
            }
            Some(Err(e)) => kit::callout(ui, Tone::Danger, "Listing failed", e),
            None => {}
        }

        // Trailers, tickets, domains.
        sec(ui, "Trailers and references in messages", None);
        if m.trailers.is_empty() && m.tickets.is_empty() && m.domains.is_empty() {
            kit::muted(
                ui,
                "No trailers, ticket keys or URLs in the scanned messages.",
            );
        }
        if !m.trailers.is_empty() {
            kit::subtitle(ui, "Trailers");
            let mut rows: Vec<Vec<String>> = Vec::new();
            for t in m.trailers.iter().take(12) {
                for (i, (v, n)) in t.values.iter().take(6).enumerate() {
                    rows.push(vec![
                        if i == 0 {
                            format!("{} ({})", t.kind, format::count(t.total))
                        } else {
                            String::new()
                        },
                        short(v, 60),
                        format::count(*n),
                    ]);
                }
            }
            kit::table(
                ui,
                &[
                    kit::Col::text("Trailer"),
                    kit::Col::text("Value"),
                    kit::Col::num("Count"),
                ],
                &rows,
            );
        }
        if !m.tickets.is_empty() {
            ui.add_space(space::M);
            kit::subtitle(ui, "Ticket keys");
            kit::secondary(
                ui,
                "Ticket prefixes name the tracker projects and, with the numbers, how big they are.",
            );
            let rows: Vec<Vec<String>> = m
                .tickets
                .iter()
                .take(12)
                .map(|(k, n, s)| {
                    vec![
                        k.clone(),
                        s.iter().take(3).cloned().collect::<Vec<_>>().join(", "),
                        format::count(*n),
                    ]
                })
                .collect();
            kit::table(
                ui,
                &[
                    kit::Col::text("Project"),
                    kit::Col::text("Examples"),
                    kit::Col::num("Mentions"),
                ],
                &rows,
            );
        }
        if !m.domains.is_empty() {
            ui.add_space(space::M);
            kit::subtitle(ui, "Domains in URLs");
            let items: Vec<(String, u32)> = m
                .domains
                .iter()
                .map(|(d, n)| (d.clone(), *n as u32))
                .collect();
            kit::count_bars(ui, &items, p.accent, 15);
        }
        if !m.internal_urls.is_empty() {
            ui.add_space(space::M);
            kit::callout(
                ui,
                Tone::Warning,
                &format!(
                    "{} on internal-looking hosts",
                    plural(m.internal_urls.len(), "URL", "URLs")
                ),
                "Intranet hosts in commit messages name internal systems an outsider should not know about.",
            );
            ui.add_space(space::S);
            let rows: Vec<Vec<String>> = m
                .internal_urls
                .iter()
                .take(20)
                .map(|(u, c)| vec![u.clone(), repo::short_id(c.id).to_string()])
                .collect();
            kit::table(
                ui,
                &[kit::Col::text("URL"), kit::Col::text("Commit")],
                &rows,
            );
            more(ui, 20, m.internal_urls.len());
        }

        if !a.local_hooks.is_empty() {
            sec(ui, "Local hooks", Some(a.local_hooks.len()));
            kit::secondary(
                ui,
                "Local only; listed so you know what runs on commit and push on this machine.",
            );
            ui.add_space(space::XS);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                for h in &a.local_hooks {
                    kit::badge(ui, h, Tone::Neutral);
                }
            });
        }
    });
}

/// Secrets as a table: kind, severity, redacted value, files.
fn secret_table(ui: &mut Ui, findings: &[Finding]) {
    let rows: Vec<Vec<String>> = findings
        .iter()
        .take(40)
        .map(|f| {
            vec![
                f.name.to_owned(),
                f.severity.label().to_owned(),
                f.redacted(),
                f.paths().join(", "),
            ]
        })
        .collect();
    kit::table(
        ui,
        &[
            kit::Col::text("Secret"),
            kit::Col::text("Severity"),
            kit::Col::text("Value"),
            kit::Col::text("File"),
        ],
        &rows,
    );
    more(ui, 40, findings.len());
}

fn unreachable_section(
    ui: &mut Ui,
    cx: &Cx,
    u: &Unreachable,
    served: &Served,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    ui.add_space(space::S);
    kit::stats(
        ui,
        &[
            kit::Stat::new("Commits", format::count(u.commit_count)),
            kit::Stat::new("Blobs", format::count(u.blobs)),
            kit::Stat::new("Trees", format::count(u.trees)),
            kit::Stat::new("Secrets found", format::count(u.findings.len()))
                .tone_if(!u.findings.is_empty(), Tone::Danger),
        ],
    );
    kit::kv(ui, KV_W, |kv| {
        kv.text("Blob size", format::bytes(u.blob_bytes as usize));
        kv.text(
            "Read for secrets",
            format!(
                "{} ({})",
                plural(u.scanned_blobs, "blob", "blobs"),
                format::bytes(u.scanned_bytes as usize)
            ),
        );
        kv.text("Took", format::millis(u.elapsed_ms));
        if u.cancelled {
            kv_colored(
                kv,
                "Cancelled",
                "The search stopped early; counts are partial",
                p.warning,
            );
        }
    });
    if !u.findings.is_empty() {
        ui.add_space(space::M);
        kit::subtitle(ui, "Secrets in unreachable objects");
        rows(ui, |ui| {
            for f in u.findings.iter().take(40) {
                let sub = format!("{} · {}", f.redacted(), f.paths().join(", "));
                let badges = [severity_badge(f.severity)];
                let row = kit::Row::new(f.name)
                    .leading(Leading::Dot(severity_dot(&p, f.severity)))
                    .subtitle(&sub)
                    .badges(&badges);
                let (_, copy) =
                    static_row_with(ui, &row, false, Icon::Copy, "Copy the clear-text value");
                if copy {
                    actions.push(Action::Copy(f.secret.clone()));
                }
            }
        });
        more(ui, 40, u.findings.len());
    }
    if !u.commits.is_empty() {
        ui.add_space(space::M);
        kit::subtitle(ui, "Unreachable commits");
        ask_github(ui, cx, &u.commits, served, actions);
        let shown = u.commits.len().min(30);
        served_rows(ui, cx, &u.commits[..shown], served, actions);
        more(ui, shown, u.commit_count);
    }
    ui.add_space(space::S);
    kit::secondary(ui, "To drop them from this clone:");
    kit::code_block(ui, DROP_COMMAND, actions);
}

// ---------------------------------------------------------------------------
// GitHub
// ---------------------------------------------------------------------------

fn dep_severity(s: &str) -> Severity {
    match s.to_lowercase().as_str() {
        "critical" => Severity::Critical,
        "high" | "error" => Severity::High,
        "medium" | "moderate" | "warning" => Severity::Medium,
        _ => Severity::Low,
    }
}

fn dep_badge(s: &str) -> (String, Tone) {
    if s.is_empty() {
        ("Unknown".into(), Tone::Neutral)
    } else {
        (
            capitalized(&s.to_lowercase()),
            severity_tone(dep_severity(s)),
        )
    }
}

/// A `gh api` part that may be missing (no access) or failed.
fn part_row<T>(
    kv: &mut kit::Kv,
    label: &str,
    part: &Option<Result<T, String>>,
    ok: impl FnOnce(&mut kit::Kv, &T),
) {
    match part {
        Some(Ok(v)) => ok(kv, v),
        Some(Err(e)) => kv.muted(label, &short(e.lines().next().unwrap_or(""), 90)),
        None => {}
    }
}

#[allow(clippy::type_complexity)]
fn github_tab(
    ui: &mut Ui,
    cx: &Cx,
    o: &mut OsintState,
    info: &Option<Result<RepoInfo, String>>,
    alerts: &Option<Result<Vec<gitvis_core::github::SecretAlert>, String>>,
    actions: &mut Vec<Action>,
) {
    let Some((owner, name)) = cx.github.clone() else {
        kit::empty_state(
            ui,
            Icon::Shield,
            "No GitHub remote",
            "Add a remote on github.com to see visibility, forks, alerts and the admin surface.",
            None,
        );
        return;
    };
    let o = &*o;
    let codeowners = o.report.as_ref().and_then(|r| r.codeowners.as_ref());
    let loading = o.gh_loading;
    kit::scroll_page(ui, "osint_github", |ui| {
        let p = *pal(ui);
        let i = info.as_ref().and_then(|r| r.as_ref().ok());
        let secret_n = alerts.as_ref().and_then(|r| r.as_ref().ok()).map(Vec::len);
        let dep_n = o
            .dependabot
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .map(Vec::len);
        let code_n = o
            .code_alerts
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .map(Vec::len);
        let total = secret_n.unwrap_or(0) + dep_n.unwrap_or(0) + code_n.unwrap_or(0);
        let mut tiles = Vec::new();
        if let Some(i) = i {
            tiles.push(kit::Stat::new("Forks", format::count(i.forks as usize)));
            tiles.push(kit::Stat::new("Stars", format::count(i.stars as usize)));
            tiles.push(kit::Stat::new(
                "Watchers",
                format::count(i.watchers as usize),
            ));
        }
        if secret_n.is_some() || dep_n.is_some() || code_n.is_some() {
            tiles.push(
                kit::Stat::new("Open alerts", format::count(total))
                    .tone_if(total > 0, Tone::Danger)
                    .tip("Secret scanning, Dependabot and code scanning"),
            );
        }
        if !tiles.is_empty() {
            kit::stats(ui, &tiles);
        }

        sec(ui, "Repository", None);
        kit::kv(ui, KV_W, |kv| {
            kv.row("Repository", |ui| {
                let url = i
                    .map(|i| i.url.clone())
                    .filter(|u| !u.is_empty())
                    .unwrap_or_else(|| format!("https://github.com/{owner}/{name}"));
                if kit::link(ui, &format!("{owner}/{name}")).clicked() {
                    actions.push(Action::OpenUrl(url));
                }
            });
            match info {
                Some(Ok(i)) => {
                    if i.is_public() {
                        kv_colored(
                            kv,
                            "Visibility",
                            "Public: anyone can read the code and the full history",
                            p.warning,
                        );
                    } else {
                        kv.text("Visibility", capitalized(&i.visibility));
                    }
                    kv.text(
                        "Owner",
                        format!(
                            "{owner} ({})",
                            if i.owner_is_org() {
                                "organization"
                            } else {
                                "user"
                            }
                        ),
                    );
                    if !i.default_branch.is_empty() {
                        kv.text("Default branch", kit::mono(&i.default_branch));
                    }
                    if i.archived {
                        kv.text("Archived", "Yes: read-only on GitHub");
                    }
                    status_row(kv, &p, "Secret scanning", &i.secret_scanning);
                    status_row(kv, &p, "Push protection", &i.push_protection);
                    status_row(kv, &p, "Dependabot updates", &i.dependabot_alerts);
                    if let Some(t) = i.pushed_at {
                        kv.text("Last push", when(t, cx.now));
                    }
                }
                Some(Err(e)) => kv_colored(kv, "Visibility", e, p.warning),
                None => kv.muted("Visibility", "Loading"),
            }
        });
        ui.add_space(space::S);
        if IconButton::new(Icon::External, "Security settings on GitHub")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::OpenUrl(format!(
                "https://github.com/{owner}/{name}/settings/security_analysis"
            )));
        }

        // Forks.
        sec(ui, "Forks", i.map(|i| i.forks as usize));
        kit::secondary(
            ui,
            "Forks share the object network with the parent on GitHub: a commit that was ever pushed here can be fetched through any fork by SHA, and forks keep their own copies of branches that were deleted or purged upstream.",
        );
        match &o.forks {
            Some(Ok(f)) if f.is_empty() => {
                kit::muted(ui, "No forks.");
            }
            Some(Ok(f)) => {
                ui.add_space(space::XS);
                rows(ui, |ui| {
                    for fk in f.iter().take(15) {
                        let sub = format!(
                            "Forked {} · pushed {}",
                            fk.created
                                .map(|t| format::relative(t, cx.now))
                                .unwrap_or_else(|| "?".into()),
                            fk.pushed
                                .map(|t| format::relative(t, cx.now))
                                .unwrap_or_else(|| "?".into())
                        );
                        let meta = plural(fk.stars as usize, "star", "stars");
                        let row = kit::Row::new(&fk.full_name).subtitle(&sub).meta(&meta);
                        if static_row(ui, &row, true).clicked() {
                            actions.push(Action::OpenUrl(fk.url.clone()));
                        }
                    }
                });
                let total = i.map(|i| i.forks as usize).unwrap_or(f.len());
                more(ui, f.len().min(15), total);
            }
            Some(Err(e)) => kit::callout(ui, Tone::Warning, "Forks unavailable", e),
            None if loading => kit::loading(ui, "Loading forks"),
            None => {}
        }

        // Alerts.
        sec(ui, "Alerts GitHub raises", None);
        kit::kv(ui, KV_W, |kv| {
            let count = |kv: &mut kit::Kv, label: &str, n: usize| {
                if n == 0 {
                    kv_colored(kv, label, "None open", p.success);
                } else {
                    kv_colored(kv, label, &format!("{} open", format::count(n)), p.danger);
                }
            };
            part_row(kv, "Secret scanning", alerts, |kv, a| {
                count(kv, "Secret scanning", a.len())
            });
            part_row(kv, "Dependabot", &o.dependabot, |kv, d| {
                count(kv, "Dependabot", d.len())
            });
            part_row(kv, "Code scanning", &o.code_alerts, |kv, c| {
                count(kv, "Code scanning", c.len())
            });
        });
        if secret_n.is_some_and(|n| n > 0) {
            ui.add_space(space::XS);
            kit::muted(
                ui,
                "Secret-scanning alerts are listed in the Secrets section.",
            );
        }
        if let Some(Ok(d)) = &o.dependabot
            && !d.is_empty()
        {
            ui.add_space(space::M);
            kit::subtitle(ui, "Dependabot");
            rows(ui, |ui| {
                for al in d.iter().take(25) {
                    let title = format!("{} ({})", al.package, al.ecosystem);
                    let sub = if al.manifest.is_empty() {
                        al.summary.clone()
                    } else {
                        format!("{} · {}", al.summary, al.manifest)
                    };
                    let badges = [dep_badge(&al.severity)];
                    let row = kit::Row::new(&title)
                        .leading(Leading::Dot(severity_dot(&p, dep_severity(&al.severity))))
                        .subtitle(&sub)
                        .meta(&al.advisory)
                        .badges(&badges);
                    if static_row(ui, &row, true).clicked() {
                        actions.push(Action::OpenUrl(al.url.clone()));
                    }
                }
            });
            more(ui, 25, d.len());
        }
        if let Some(Ok(c)) = &o.code_alerts
            && !c.is_empty()
        {
            ui.add_space(space::M);
            kit::subtitle(ui, "Code scanning");
            rows(ui, |ui| {
                for al in c.iter().take(25) {
                    let sub = format!("{}:{} · {} · {}", al.path, al.line, al.tool, al.description);
                    let badges = [dep_badge(&al.severity)];
                    let row = kit::Row::new(&al.rule)
                        .leading(Leading::Dot(severity_dot(&p, dep_severity(&al.severity))))
                        .subtitle(&sub)
                        .badges(&badges);
                    if static_row(ui, &row, true).clicked() {
                        actions.push(Action::OpenUrl(al.url.clone()));
                    }
                }
            });
            more(ui, 25, c.len());
        }

        // Admin surface.
        sec(ui, "Admin surface", None);
        kit::secondary(
            ui,
            "Only visible with admin rights on the repository; each line says what the account could see.",
        );
        ui.add_space(space::XS);
        match &o.admin {
            Some(ad) => {
                kit::kv(ui, KV_W, |kv| {
                    let list = |kv: &mut kit::Kv,
                                label: &str,
                                part: &Option<Result<Vec<String>, String>>,
                                none: &str| {
                        part_row(kv, label, part, |kv, v| {
                            if v.is_empty() {
                                kv.muted(label, none);
                            } else {
                                kv.row(label, |ui| {
                                    ui.label(
                                        RichText::new(format::count(v.len())).color(p.warning),
                                    );
                                    for line in v.iter().take(20) {
                                        ui.label(
                                            RichText::new(line).size(size::SMALL).color(p.text_2),
                                        );
                                    }
                                    if v.len() > 20 {
                                        ui.label(
                                            RichText::new(format!("and {} more", v.len() - 20))
                                                .size(size::SMALL)
                                                .color(p.text_3),
                                        );
                                    }
                                });
                            }
                        });
                    };
                    list(kv, "Deploy keys", &ad.deploy_keys, "None configured");
                    list(kv, "Webhooks", &ad.webhooks, "None configured");
                    list(kv, "Environments", &ad.environments, "None defined");
                    list(
                        kv,
                        "Actions secrets",
                        &ad.secrets,
                        "None (names only; values are never readable)",
                    );
                    list(kv, "Actions variables", &ad.variables, "None");
                    part_row(kv, "Default branch", &ad.protection, |kv, s| {
                        if s == "not protected" {
                            kv_colored(kv, "Default branch", "Not protected", p.warning);
                        } else {
                            kv.text("Default branch", capitalized(s));
                        }
                    });
                    part_row(kv, "Collaborators", &ad.collaborators, |kv, n| {
                        kv.text(
                            "Collaborators",
                            format!("{} with direct access", format::count(*n)),
                        );
                    });
                });
            }
            None if loading => kit::loading(ui, "Loading the admin surface"),
            None => {
                kit::muted(ui, "Not loaded.");
            }
        }

        // CODEOWNERS.
        sec(ui, "CODEOWNERS", None);
        match codeowners {
            Some(c) => {
                kit::kv(ui, KV_W, |kv| {
                    kv.text("File", kit::mono(&c.path));
                    kv.text(
                        "Rules",
                        format!(
                            "{} naming {}",
                            plural(c.rules, "rule", "rules"),
                            plural(c.owners.len(), "owner", "owners")
                        ),
                    );
                });
                if !c.owners.is_empty() {
                    ui.add_space(space::S);
                    let rows: Vec<Vec<String>> = c
                        .owners
                        .iter()
                        .take(20)
                        .map(|(o, n)| vec![o.clone(), format::count(*n)])
                        .collect();
                    kit::table(
                        ui,
                        &[kit::Col::text("Owner"), kit::Col::num("Rules")],
                        &rows,
                    );
                    more(ui, 20, c.owners.len());
                }
                ui.add_space(space::XS);
                kit::secondary(
                    ui,
                    "An org chart in a file: teams, their names and who reviews what.",
                );
            }
            None => {
                kit::muted(ui, "No CODEOWNERS file.");
            }
        }

        // Dorks.
        sec(ui, "Dork yourself", None);
        kit::secondary(
            ui,
            "The GitHub code-search queries an attacker runs against an owner first (.env files, private keys, registry tokens, credential files, Terraform state and more), run with your own account against every repository of the owner. Twelve requests; GitHub allows ten code searches a minute, so the run pauses briefly between queries.",
        );
        ui.add_space(space::XS);
        if o.dorks_running {
            kit::loading(ui, "Searching");
        } else if IconButton::new(
            Icon::Search,
            if o.dorks.is_some() {
                "Run the searches again"
            } else {
                "Run 12 code searches"
            },
        )
        .compact()
        .show(ui)
        .clicked()
        {
            actions.push(Action::RunDorks);
        }
        if let Some(results) = &o.dorks {
            ui.add_space(space::S);
            rows(ui, |ui| {
                for r in results {
                    let (badge, sub) = match (&r.error, r.total) {
                        (Some(e), _) => (
                            ("Failed".to_owned(), Tone::Warning),
                            short(e.lines().next().unwrap_or(""), 120),
                        ),
                        (None, 0) => (("Nothing found".to_owned(), Tone::Success), r.query.clone()),
                        (None, n) => (
                            (plural(n as usize, "file", "files"), Tone::Danger),
                            r.query.clone(),
                        ),
                    };
                    let badges = [badge];
                    let row = kit::Row::new(&r.label).subtitle(&sub).badges(&badges);
                    static_row(ui, &row, false);
                    for h in r.hits.iter().take(8) {
                        let title = format!("{}: {}", h.repo, h.path);
                        let row =
                            kit::Row::new(&title).leading(Leading::Icon(Icon::External, p.text_3));
                        if static_row(ui, &row, true).clicked() {
                            actions.push(Action::OpenUrl(h.url.clone()));
                        }
                    }
                    if r.hits.len() > 8 {
                        kit::muted(
                            ui,
                            &format!("and {} more on GitHub", r.total.saturating_sub(8)),
                        );
                    }
                }
            });
            ui.add_space(space::S);
            if IconButton::new(Icon::External, "Open code search on GitHub")
                .compact()
                .show(ui)
                .clicked()
            {
                actions.push(Action::OpenUrl(format!(
                    "https://github.com/search?type=code&q=org%3A{owner}+filename%3A.env"
                )));
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Supply chain
// ---------------------------------------------------------------------------

fn supply_tab(ui: &mut Ui, cx: &Cx, o: &mut OsintState, actions: &mut Vec<Action>) {
    let Some(rep) = &o.report else { return };
    let s = &rep.supply;
    let osv = o.osv.as_ref();
    let osv_run = o.osv_run.as_ref();
    kit::scroll_page(ui, "osint_supply", |ui| {
        let p = *pal(ui);
        let mut tiles = vec![
            kit::Stat::new("Packages", format::count(s.packages.len())),
            kit::Stat::new("Lockfiles", format::count(s.lockfiles.len())),
            kit::Stat::new("Git dependencies", format::count(s.git_deps.len()))
                .tone_if(!s.git_deps.is_empty(), Tone::Warning),
            kit::Stat::new("Hooks and drivers", format::count(s.hooks.len())),
        ];
        if let Some(Ok(v)) = osv {
            tiles.push(
                kit::Stat::new("Known vulnerabilities", format::count(v.len()))
                    .tone_if(!v.is_empty(), Tone::Danger),
            );
        }
        kit::stats(ui, &tiles);

        sec(
            ui,
            "Dependencies pinned to git repositories",
            Some(s.git_deps.len()),
        );
        if s.git_deps.is_empty() {
            kit::muted(ui, "Every dependency comes from a registry.");
        } else {
            kit::secondary(
                ui,
                "A git URL dependency trusts whoever controls that repository and branch, bypasses registry scanning, and breaks when the repository moves. Pin to a commit at least.",
            );
            ui.add_space(space::XS);
            rows(ui, |ui| {
                for d in s.git_deps.iter().take(80) {
                    let loc = if d.line > 0 {
                        format!("{}:{}", d.file, d.line)
                    } else {
                        d.file.clone()
                    };
                    let row = kit::Row::new(&d.name).mono().subtitle(&d.source).meta(&loc);
                    let resp = static_row(ui, &row, true);
                    if resp.clicked() {
                        actions.push(Action::OpenPath(cx.workdir.join(&d.file)));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Copy source").clicked() {
                            actions.push(Action::Copy(d.source.clone()));
                            ui.close();
                        }
                    });
                }
            });
            more(ui, 80, s.git_deps.len());
        }

        sec(ui, "Packages", Some(s.packages.len()));
        if s.lockfiles.is_empty() {
            kit::muted(
                ui,
                "No lockfile or pinned manifest found (package-lock.json, yarn.lock, pnpm-lock.yaml, Cargo.lock, requirements.txt, Pipfile.lock, go.mod, Gemfile.lock, composer.lock).",
            );
        } else {
            let mut by_eco: Vec<(&str, usize)> = Vec::new();
            for pk in &s.packages {
                match by_eco.iter_mut().find(|(e, _)| *e == pk.ecosystem) {
                    Some(e) => e.1 += 1,
                    None => by_eco.push((pk.ecosystem, 1)),
                }
            }
            by_eco.sort_by_key(|e| std::cmp::Reverse(e.1));
            let rows_e: Vec<Vec<String>> = by_eco
                .iter()
                .map(|(e, n)| vec![(*e).to_owned(), format::count(*n)])
                .collect();
            kit::table(
                ui,
                &[kit::Col::text("Ecosystem"), kit::Col::num("Packages")],
                &rows_e,
            );
            ui.add_space(space::M);
            let rows_l: Vec<Vec<String>> = s
                .lockfiles
                .iter()
                .map(|(f, n)| vec![f.clone(), format::count(*n)])
                .collect();
            kit::table(
                ui,
                &[kit::Col::text("Lockfile"), kit::Col::num("Packages")],
                &rows_l,
            );
            ui.add_space(space::M);
            if let Some(run) = osv_run {
                if kit::progress(ui, run, "Checking osv.dev", true) {
                    run.cancel.store(true, Ordering::Relaxed);
                }
            } else if IconButton::new(
                Icon::Search,
                &format!(
                    "Check {} against osv.dev",
                    plural(s.packages.len(), "package", "packages")
                ),
            )
            .compact()
            .show(ui)
            .on_hover_text(
                "Sends package names and versions to https://api.osv.dev (through curl). Nothing else leaves the machine.",
            )
            .clicked()
            {
                actions.push(Action::RunOsv);
            }
            match osv {
                Some(Ok(v)) if v.is_empty() => {
                    ui.add_space(space::S);
                    kit::callout(
                        ui,
                        Tone::Success,
                        "No known vulnerabilities",
                        "osv.dev has no advisory for any of the listed package versions.",
                    );
                }
                Some(Ok(v)) => {
                    let affected: HashSet<(&str, &str)> = v
                        .iter()
                        .map(|x| (x.package.name.as_str(), x.package.version.as_str()))
                        .collect();
                    ui.add_space(space::S);
                    kit::subtitle(
                        ui,
                        &format!(
                            "{} affecting {}",
                            plural(v.len(), "advisory", "advisories"),
                            plural(affected.len(), "package", "packages")
                        ),
                    );
                    rows(ui, |ui| {
                        for vu in v.iter().take(100) {
                            let title = format!("{}@{}", vu.package.name, vu.package.version);
                            let mut sub = vu.id.clone();
                            if !vu.aliases.is_empty() {
                                sub.push_str(&format!(" ({})", vu.aliases.join(", ")));
                            }
                            if !vu.summary.is_empty() {
                                sub.push_str(&format!(" · {}", vu.summary));
                            }
                            let meta = format!("{} ({})", vu.package.file, vu.package.ecosystem);
                            let badges = [dep_badge(&vu.severity)];
                            let row = kit::Row::new(&title)
                                .leading(Leading::Dot(severity_dot(&p, dep_severity(&vu.severity))))
                                .subtitle(&sub)
                                .meta(&meta)
                                .badges(&badges);
                            if static_row(ui, &row, true).clicked() {
                                actions.push(Action::OpenUrl(vu.url.clone()));
                            }
                        }
                    });
                    more(ui, 100, v.len());
                }
                Some(Err(e)) => {
                    ui.add_space(space::S);
                    kit::callout(ui, Tone::Danger, "The check failed", e);
                }
                None => {}
            }
        }

        sec(ui, "Hooks and drivers", Some(s.hooks.len()));
        if s.hooks.is_empty() {
            kit::muted(
                ui,
                "No committed hooks, hook frameworks, filter drivers or LFS configuration.",
            );
        }
        for h in &s.hooks {
            ui.add_space(space::S);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                kit::badge(ui, &capitalized(h.kind), severity_tone(h.severity));
                ui.label(kit::mono(&h.path).color(p.text));
            });
            if !h.detail.is_empty() {
                kit::secondary(ui, &h.detail);
            }
        }
    });
}
