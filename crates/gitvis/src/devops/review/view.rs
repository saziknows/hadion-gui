//! The Review page shell: header with the Pull requests | Issues switch,
//! the pull request list with its chips and search, and the pull request
//! detail with its header and sub-tabs (Conversation, Files, Commits,
//! Checks).

use egui::{Align, Color32, FontId, Frame, Layout, Margin, Response, RichText, Ui, vec2};
use gitvis_core::Oid;
use gitvis_core::devops::review::{PrState, PrSummary};

use super::widgets::{self, ROW3_H};
use super::{Act, Mode, PrFilter, PrTab, State, conversation, files, forms, issues};
use crate::devops::console::Console;
use crate::format;
use crate::state::{Action, RepoState, Settings};
use crate::theme::{Tone, medium, pal, size, space};
use crate::views::icons::{self, Icon, IconButton};
use crate::views::kit;

pub fn show(
    ui: &mut Ui,
    repo: &mut RepoState,
    _console: &Console,
    settings: &Settings,
    actions: &mut Vec<Action>,
) {
    let Some((owner, name)) = repo.github_remote.clone() else {
        return;
    };
    let now = format::now_secs();
    header(ui, repo, &owner, &name, now, actions);
    if repo.devops.review.gh_ok == Some(false) {
        gh_missing(ui, actions);
        return;
    }
    match repo.devops.review.mode {
        Mode::Prs => prs_page(ui, repo, settings, now, actions),
        Mode::Issues => issues::page(ui, repo, settings, now, actions),
    }
}

/// The GitHub CLI is not installed: what it is, why the page needs it, how
/// to get it, and a way to look again.
fn gh_missing(ui: &mut Ui, actions: &mut Vec<Action>) {
    use gitvis_core::devops::tools::Tool;
    detail_area(ui, |ui| {
        Frame::NONE.inner_margin(Margin::same(28)).show(ui, |ui| {
            ui.set_max_width(ui.available_width().min(kit::PROSE));
            kit::title(ui, &format!("{} ({}) is not installed", Tool::Gh.name(), Tool::Gh.program()));
            ui.add_space(4.0);
            kit::body(
                ui,
                "The Review page lists, reads and changes pull requests and issues through `gh api`, signed in with your own `gh auth login`. gitvis keeps no token of its own.",
            );
            ui.add_space(space::S);
            kit::section(ui, "Install it", None);
            kit::code_block(ui, Tool::Gh.install_hint(), actions);
            ui.add_space(space::S);
            if crate::views::form::primary(ui, "Check again", true).clicked() {
                actions.push(Act::CheckGh.into());
            }
        });
    });
}

fn header(
    ui: &mut Ui,
    repo: &RepoState,
    owner: &str,
    name: &str,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let st = &repo.devops.review;
    let (loading, at) = match st.mode {
        Mode::Prs => {
            let l = st.current_list();
            (l.loading, l.at)
        }
        Mode::Issues => st
            .issues
            .current()
            .map(|l| (l.loading, l.at))
            .unwrap_or((false, None)),
    };
    let mut sub = vec![format!("{owner}/{name}")];
    if loading {
        sub.push("refreshing…".to_owned());
    } else if let Some(at) = at {
        sub.push(format!(
            "refreshed {}",
            format::relative(now - at.elapsed().as_secs() as i64, now)
        ));
    }
    if let Some(rate) = repo.rate {
        let mut s = format!(
            "{} of {} API requests left",
            format::count(rate.remaining as usize),
            format::count(rate.limit as usize)
        );
        if rate.remaining < rate.limit / 5 {
            s.push_str(&format!(
                ", resets in {}",
                format::duration((rate.reset - now).max(0))
            ));
        }
        sub.push(s);
    }
    if let Some(c) = &st.changing {
        sub.push(format!("{c}…"));
    }
    let mode = st.mode;
    let issues_off = st.meta().has_issues == Some(false);
    let gh_ok = st.gh_ok != Some(false);
    let options = [(Mode::Prs, "Pull requests"), (Mode::Issues, "Issues")];
    // The "New …" button, two icon buttons, the switch and their gaps.
    let controls_w = segmented_width(ui, &options) + 190.0 + 2.0 * 30.0 + 22.0;
    let (subtitle, dropped) = crate::views::github::fit_subtitle(ui, "Review", &sub, controls_w);
    kit::page_header(ui, "Review", &subtitle, |ui| {
        let (label, act) = match mode {
            Mode::Prs => ("New pull request", Act::NewPr),
            Mode::Issues => ("New issue", Act::NewIssue),
        };
        let enabled = !(mode == Mode::Issues && issues_off) && gh_ok;
        if IconButton::new(Icon::Plus, label)
            .primary(true)
            .enabled(enabled)
            .show(ui)
            .clicked()
        {
            actions.push(act.into());
        }
        if loading {
            ui.add_space(6.0);
            ui.add(egui::Spinner::new().size(14.0));
            ui.add_space(6.0);
        } else if IconButton::icon(Icon::Fetch)
            .show(ui)
            .on_hover_text({
                let what = match mode {
                    Mode::Prs => "Refresh the list and the selected pull request",
                    Mode::Issues => "Refresh the list and the selected issue",
                };
                // What the subtitle had no room for.
                if dropped.is_empty() {
                    what.to_owned()
                } else {
                    format!("{what}\n\n{}", dropped.join("\n"))
                }
            })
            .clicked()
        {
            actions.push(Act::Refresh.into());
        }
        if IconButton::icon(Icon::External)
            .show(ui)
            .on_hover_text(match mode {
                Mode::Prs => "Pull requests on GitHub",
                Mode::Issues => "Issues on GitHub",
            })
            .clicked()
        {
            actions.push(Action::OpenUrl(format!(
                "https://github.com/{owner}/{name}/{}",
                match mode {
                    Mode::Prs => "pulls",
                    Mode::Issues => "issues",
                }
            )));
        }
        ui.add_space(6.0);
        let w = segmented_width(ui, &options);
        ui.allocate_ui_with_layout(vec2(w, 28.0), Layout::left_to_right(Align::Center), |ui| {
            if let Some(m) = kit::segmented(ui, &options, mode) {
                actions.push(Act::SetMode(m).into());
            }
        });
    });
}

/// Width `kit::segmented` takes, so it can sit in a right-to-left toolbar.
pub fn segmented_width<T>(ui: &Ui, options: &[(T, &str)]) -> f32 {
    let font = FontId::new(size::SMALL, medium());
    let labels: f32 = options
        .iter()
        .map(|(_, l)| {
            ui.painter()
                .layout_no_wrap((*l).to_owned(), font.clone(), Color32::WHITE)
                .size()
                .x
                + 20.0
        })
        .sum();
    labels + 2.0 * options.len().saturating_sub(1) as f32 + 4.0
}

/// The list pane on the left. It never takes more than 45% of the page,
/// so the detail (and its tabs) keep room on a narrow window.
pub fn list_pane(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    let w = ui.available_width();
    egui::Panel::left(id)
        .resizable(true)
        .default_size((w * 0.36).clamp(300.0, 400.0))
        .size_range(300.0..=(w * 0.45).clamp(300.0, 720.0))
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

/// A scroll offset asked for by the `review-scroll` script command, taken
/// by the next scroll area of the page that is drawn.
static SCRIPT_SCROLL: std::sync::Mutex<Option<f32>> = std::sync::Mutex::new(None);

pub fn request_scroll(y: f32) {
    if let Ok(mut s) = SCRIPT_SCROLL.lock() {
        *s = Some(y);
    }
}

/// A vertical scroll area that applies a scripted scroll offset once.
pub fn scroll_area(salt: impl std::hash::Hash + std::fmt::Debug) -> egui::ScrollArea {
    let area = egui::ScrollArea::vertical()
        .id_salt(salt)
        .auto_shrink([false, false]);
    match SCRIPT_SCROLL.lock().ok().and_then(|mut s| s.take()) {
        Some(y) => area.vertical_scroll_offset(y),
        None => area,
    }
}

/// A padded, width-capped, scrollable pane with its own scroll position.
pub fn scroll_pane(
    ui: &mut Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    add: impl FnOnce(&mut Ui),
) {
    scroll_area(salt).show(ui, |ui| {
        Frame::NONE
            .inner_margin(Margin {
                left: 28,
                right: 28,
                top: 16,
                bottom: 28,
            })
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width().min(kit::DETAIL_MAX));
                ui.spacing_mut().item_spacing.y = 8.0;
                add(ui)
            });
    });
}

/// The detail area to the right of a list.
pub fn detail_area(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas))
        .show(ui, add);
}

/// `kit::search_box` on a row of its own, kept inside the pane's width.
pub fn search_field(ui: &mut Ui, id: &str, text: &mut String, hint: &str) -> Response {
    ui.horizontal(|ui| {
        if !text.is_empty() {
            ui.set_max_width(ui.available_width() - 8.0);
        }
        kit::search_box(ui, id, text, hint)
    })
    .inner
}

/// A chip with a count after its label.
pub fn count_chip(ui: &mut Ui, label: &str, count: Option<u64>, on: bool) -> bool {
    let text = match count {
        Some(n) => format!("{label}  {}", format::count(n as usize)),
        None => label.to_owned(),
    };
    kit::chip(ui, &text, on).clicked()
}

// ---------------------------------------------------------------------------
// Pull request list
// ---------------------------------------------------------------------------

fn prs_page(
    ui: &mut Ui,
    repo: &mut RepoState,
    settings: &Settings,
    now: i64,
    actions: &mut Vec<Action>,
) {
    list_pane(ui, "rv_pr_list", |ui| {
        pr_list(ui, &mut repo.devops.review, now, actions)
    });
    detail_area(ui, |ui| {
        let st = &repo.devops.review;
        if st.form_open && st.new_pr.is_some() {
            // AI and other areas' tools for the new pull request (kept from
            // widening the form under them on a narrow window).
            if let Some(h) = crate::devops::hooks::HookCx::of(repo, settings) {
                widgets::contained(ui, |ui| crate::devops::hooks::new_pr_tools(ui, &h, actions));
            }
            forms::new_pr(ui, repo, now, actions);
            return;
        }
        match st.selected {
            Some(n) => pr_detail(ui, repo, settings, n, now, actions),
            None => {
                Frame::NONE.inner_margin(Margin::same(24)).show(ui, |ui| {
                    kit::empty_state(
                        ui,
                        Icon::PullRequest,
                        "Select a pull request",
                        "Its conversation, merge box, changed files with review threads, commits and checks appear here.",
                        None,
                    );
                });
            }
        }
    });
}

fn pr_list(ui: &mut Ui, st: &mut State, now: i64, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let open = st.open.data.as_ref();
    let drafts = open.map(|l| l.items.iter().filter(|p| p.draft).count() as u64);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for f in PrFilter::ALL {
            let count = match f {
                PrFilter::Open => open.map(|l| l.total),
                PrFilter::Draft => drafts.filter(|n| *n > 0),
                PrFilter::ReviewRequested => open.map(|l| l.requested_total).filter(|n| *n > 0),
                PrFilter::Mine => open.map(|l| l.mine_total).filter(|n| *n > 0),
                PrFilter::Closed => st.closed.data.as_ref().map(|l| l.total),
            };
            if count_chip(ui, f.label(), count, st.filter == f) && st.filter != f {
                actions.push(Act::SetFilter(f).into());
            }
        }
    });
    ui.add_space(space::S);
    let list = st.current_list();
    let loaded = list.data.as_ref().map(|l| l.items.len()).unwrap_or(0);
    let hint = format!(
        "Filter {}",
        format::plural(loaded, "loaded pull request", "loaded pull requests")
    );
    search_field(ui, "rv_pr_search", &mut st.search, &hint);
    ui.add_space(space::S);

    let list = st.current_list();
    if list.data.is_none() {
        if list.loading {
            widgets::loading_block(
                ui,
                "Loading pull requests…",
                "One GraphQL query per page of 20: reviews, checks and mergeability. GitHub computes mergeability for each pull request, which takes a few seconds on big repositories.",
            );
        } else if let Some(e) = &list.error
            && widgets::error_block(ui, "Could not load the pull requests", e)
        {
            actions.push(Act::Refresh.into());
        }
        return;
    }
    if let Some(e) = &list.error {
        kit::callout_with(ui, Tone::Danger, "The last refresh failed", e, |ui| {
            if crate::views::form::button(ui, "Retry", true).clicked() {
                actions.push(Act::Refresh.into());
            }
        });
        ui.add_space(4.0);
    }
    let data = list.data.as_ref().expect("checked above");
    let visible = st.visible();
    let pages = matches!(
        st.filter,
        PrFilter::Open | PrFilter::Draft | PrFilter::Closed
    );
    let more = pages && data.has_more;
    if data.light {
        kit::callout(
            ui,
            Tone::Warning,
            "Mergeability left out",
            "GitHub ran out of time computing it for the whole page; each pull request's detail still shows it.",
        );
    }
    if visible.is_empty() && !more {
        ui.add_space(space::S);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            kit::muted(ui, &empty_reason(st, data.total));
        });
        return;
    }
    if visible.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            kit::muted(
                ui,
                &format!(
                    "{} Load more to look further.",
                    empty_reason(st, data.total)
                ),
            );
        });
    }
    let selected = st.selected;
    let requested: Vec<u64> = data.requested.iter().map(|p| p.number).collect();
    let rows: Vec<PrSummary> = visible.into_iter().cloned().collect();
    let total_rows = rows.len() + more as usize;
    let more_loading = st.more_loading;
    let shown = data.items.len();
    let total = data.total;
    kit::list(
        ui,
        "rv_pr_rows",
        total_rows,
        ROW3_H,
        |i| rows.get(i).is_some_and(|r| selected == Some(r.number)),
        |ui, rect, resp, i| {
            let Some(pr) = rows.get(i) else {
                // The "load more" row.
                let text = if more_loading {
                    "Loading more…".to_owned()
                } else {
                    format!(
                        "Showing {} of {}  ·  Load {} more",
                        format::count(shown),
                        format::count(total as usize),
                        gitvis_core::devops::review::graphql::PAGE
                    )
                };
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    text,
                    FontId::new(size::SMALL, medium()),
                    p.accent,
                );
                if resp.clicked() && !more_loading {
                    actions.push(Act::LoadMore.into());
                }
                return;
            };
            widgets::paint_pr_row(
                ui,
                rect,
                pr,
                requested.contains(&pr.number),
                selected == Some(pr.number),
                now,
            );
            if resp.clicked() {
                actions.push(Act::OpenPr(pr.number).into());
            }
            resp.context_menu(|ui| pr_menu(ui, pr, actions));
        },
    );
}

fn empty_reason(st: &State, total: u64) -> String {
    if !st.search.trim().is_empty() {
        return "No loaded pull request matches the filter.".to_owned();
    }
    match st.filter {
        PrFilter::Open if total == 0 => {
            "No open pull requests. Create one with New pull request.".to_owned()
        }
        PrFilter::Open => "No open pull requests on this page.".to_owned(),
        PrFilter::Draft => "No drafts among the loaded open pull requests.".to_owned(),
        PrFilter::ReviewRequested => {
            "Nobody is waiting for your review here (GitHub search: review-requested:@me)."
                .to_owned()
        }
        PrFilter::Mine => {
            "You have no open pull requests here (GitHub search: author:@me).".to_owned()
        }
        PrFilter::Closed => "No closed or merged pull requests.".to_owned(),
    }
}

fn pr_menu(ui: &mut Ui, pr: &PrSummary, actions: &mut Vec<Action>) {
    ui.set_min_width(220.0);
    if ui.button("Show pull request").clicked() {
        actions.push(Act::OpenPr(pr.number).into());
        ui.close();
    }
    if ui.button("Check out locally…").clicked() {
        actions.push(Act::Checkout(pr.number).into());
        ui.close();
    }
    if ui.button("Open on GitHub").clicked() {
        actions.push(Action::OpenUrl(pr.url.clone()));
        ui.close();
    }
    ui.separator();
    if ui.button("Copy URL").clicked() {
        actions.push(Action::Copy(pr.url.clone()));
        ui.close();
    }
    if ui.button("Copy head SHA").clicked() {
        actions.push(Action::Copy(pr.head_oid.clone()));
        ui.close();
    }
    if ui.button("Copy branch name").clicked() {
        actions.push(Action::Copy(pr.head.clone()));
        ui.close();
    }
}

// ---------------------------------------------------------------------------
// Pull request detail
// ---------------------------------------------------------------------------

fn pr_detail(
    ui: &mut Ui,
    repo: &mut RepoState,
    settings: &Settings,
    n: u64,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let st = &repo.devops.review;
    let summary = st.find(n).cloned();
    let view = st.prs.get(&n);
    let detail_loading = view.is_some_and(|v| v.detail.loading);
    let detail_error = view.and_then(|v| v.detail.error.clone());
    let Some(pr) = summary else {
        Frame::NONE.inner_margin(Margin::same(28)).show(ui, |ui| {
            if let Some(e) = detail_error {
                if widgets::error_block(ui, &format!("Could not load pull request #{n}"), &e) {
                    actions.push(Act::RefreshPr(n).into());
                }
            } else {
                widgets::loading_block(ui, &format!("Loading pull request #{n}…"), "");
            }
        });
        return;
    };
    let p = *pal(ui);
    let tab = st.tab;
    let pending = view.map(|v| v.pending.len()).unwrap_or(0);
    let commits = view
        .and_then(|v| v.commits.data.as_ref())
        .map(|c| c.len() as u64)
        .unwrap_or(pr.commits);
    let unresolved = view
        .and_then(|v| v.detail.data.as_ref())
        .map(|d| d.unresolved_threads());
    let requested = st.is_requested(n);
    egui::Panel::top("rv_pr_head")
        .frame(Frame::NONE.fill(p.canvas).inner_margin(Margin {
            left: 28,
            right: 20,
            top: 16,
            bottom: 8,
        }))
        .show(ui, |ui| {
            ui.set_max_width(ui.available_width().min(kit::DETAIL_MAX + 40.0));
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(18.0, 22.0), egui::Sense::hover());
                icons::paint(
                    ui.painter(),
                    Icon::PullRequest,
                    r.center(),
                    16.0,
                    widgets::pr_icon_color(&p, &pr),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if detail_loading {
                        ui.add(egui::Spinner::new().size(14.0));
                    } else if IconButton::icon(Icon::Fetch)
                        .compact()
                        .show(ui)
                        .on_hover_text("Reload this pull request")
                        .clicked()
                    {
                        actions.push(Act::RefreshPr(n).into());
                    }
                    if IconButton::icon(Icon::Copy)
                        .compact()
                        .show(ui)
                        .on_hover_text("Copy the URL")
                        .clicked()
                    {
                        actions.push(Action::Copy(pr.url.clone()));
                    }
                    if IconButton::icon(Icon::External)
                        .compact()
                        .show(ui)
                        .on_hover_text("Open on GitHub")
                        .clicked()
                    {
                        actions.push(Action::OpenUrl(pr.url.clone()));
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        kit::title(ui, &pr.title);
                    });
                });
            });
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
                ui.add_space(26.0);
                ui.label(kit::mono(format!("#{}", pr.number)).color(p.text_2));
                let state_tone = match pr.state {
                    PrState::Merged => Tone::Purple,
                    PrState::Closed => Tone::Danger,
                    PrState::Open if pr.draft => Tone::Neutral,
                    PrState::Open => Tone::Success,
                };
                let state_text = if pr.draft && pr.is_open() {
                    "Draft"
                } else {
                    pr.state.label()
                };
                kit::badge(ui, state_text, state_tone);
                ui.label(
                    RichText::new(format!(
                        "{} {} {} into {} from {}",
                        if pr.author.is_empty() {
                            "ghost"
                        } else {
                            &pr.author
                        },
                        match pr.state {
                            PrState::Open => "wants to merge",
                            PrState::Merged => "merged",
                            PrState::Closed => "wanted to merge",
                        },
                        format::plural(commits as usize, "commit", "commits"),
                        pr.base,
                        pr.head
                    ))
                    .size(size::SMALL)
                    .color(p.text_2),
                );
                if let Some(c) = pr.created {
                    ui.label(
                        RichText::new(format!("· opened {}", format::relative(c, now)))
                            .size(size::SMALL)
                            .color(p.text_3),
                    )
                    .on_hover_text(format::absolute_long(c));
                }
            });
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                ui.add_space(26.0);
                for (text, tone) in widgets::pr_badges(&pr, requested)
                    .into_iter()
                    .filter(|(t, _)| !matches!(t.as_str(), "Draft" | "Merged" | "Closed"))
                {
                    kit::badge(ui, &text, tone);
                }
                if let Some(u) = unresolved.filter(|u| *u > 0) {
                    kit::badge(
                        ui,
                        &format::plural(u, "unresolved thread", "unresolved threads"),
                        Tone::Warning,
                    );
                }
                widgets::label_chips(ui, &pr.labels);
            });
            ui.add_space(space::M);
            let files_label = if pending > 0 {
                format!("Files {}  ·  {pending} pending", pr.changed_files)
            } else {
                format!("Files {}", pr.changed_files)
            };
            let conv_label = format!("Conversation {}", pr.total_comments);
            let commits_label = format!("Commits {commits}");
            let options = [
                (PrTab::Conversation, conv_label.as_str()),
                (PrTab::Files, files_label.as_str()),
                (PrTab::Commits, commits_label.as_str()),
                (PrTab::Checks, "Checks"),
            ];
            if let Some(t) = kit::segmented(ui, &options, tab) {
                actions.push(Act::SetTab(t).into());
            }
            ui.add_space(4.0);
        });
    // A hairline under the header.
    let r = ui.available_rect_before_wrap();
    ui.painter()
        .hline(r.x_range(), r.top(), egui::Stroke::new(1.0, p.border));

    match tab {
        PrTab::Conversation => {
            detail_area(ui, |ui| {
                scroll_pane(ui, ("rv_conv", n), |ui| {
                    // AI summary, issue links and other areas' blocks.
                    if let Some(h) = crate::devops::hooks::HookCx::of(repo, settings) {
                        widgets::contained(ui, |ui| {
                            crate::devops::hooks::pr_tools(ui, &h, n, actions)
                        });
                    }
                    conversation::show(ui, repo, n, now, actions);
                });
            });
        }
        PrTab::Files => files::show(ui, repo, settings, n, now, actions),
        PrTab::Commits => detail_area(ui, |ui| commits_tab(ui, repo, n, now, actions)),
        PrTab::Checks => detail_area(ui, |ui| {
            scroll_pane(ui, ("rv_checks", n), |ui| {
                let oid = Oid::from_str(&pr.head_oid).ok();
                match oid {
                    Some(oid) => {
                        kit::secondary(
                            ui,
                            &format!(
                                "Check runs and statuses on the head commit {} of {}.",
                                &pr.head_oid[..pr.head_oid.len().min(10)],
                                pr.head
                            ),
                        );
                        crate::views::github::checks_section(
                            ui,
                            oid,
                            repo.checks.get(&oid),
                            now,
                            actions,
                        );
                    }
                    None => {
                        kit::muted(ui, "The head commit is not known yet.");
                    }
                }
                let runs_bad = oid
                    .and_then(|o| repo.checks.get(&o))
                    .and_then(|c| c.result.as_ref())
                    .and_then(|r| r.as_ref().ok())
                    .is_some_and(|list| list.iter().any(|c| c.outcome().is_bad()));
                let runs_loaded = oid
                    .and_then(|o| repo.checks.get(&o))
                    .is_some_and(|c| matches!(c.result, Some(Ok(_))));
                if runs_loaded && !runs_bad && pr.checks.is_some_and(|c| c.is_bad()) {
                    ui.add_space(space::S);
                    kit::callout(
                        ui,
                        Tone::Warning,
                        "GitHub's summary says checks are failing",
                        "None of the check runs and commit statuses listed above failed. The failing one is beyond the first 100 of either list: open the pull request on GitHub to see every check.",
                    );
                }
            });
        }),
    }
}

fn commits_tab(ui: &mut Ui, repo: &RepoState, n: u64, now: i64, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let st = &repo.devops.review;
    let Some(v) = st.prs.get(&n) else {
        return;
    };
    let snap = repo.snapshot.as_ref();
    Frame::NONE
        .inner_margin(Margin {
            left: 20,
            right: 20,
            top: 12,
            bottom: 8,
        })
        .show(ui, |ui| {
            let Some(commits) = v.commits.data.as_ref() else {
                if let Some(e) = &v.commits.error {
                    if widgets::error_block(ui, "Could not load the commits", e) {
                        actions.push(Act::RefreshPr(n).into());
                    }
                } else {
                    widgets::loading_block(ui, "Loading commits…", "GET pulls/n/commits (GitHub lists at most 250).");
                }
                return;
            };
            if commits.is_empty() {
                kit::muted(ui, "This pull request has no commits.");
                return;
            }
            let in_graph = commits
                .iter()
                .filter(|c| {
                    Oid::from_str(&c.sha)
                        .ok()
                        .is_some_and(|o| snap.is_some_and(|s| s.index_of.contains_key(&o)))
                })
                .count();
            kit::secondary(
                ui,
                &if in_graph == commits.len() {
                    format!(
                        "{}, all in the loaded graph: click one to show it on the History page.",
                        format::plural(commits.len(), "commit", "commits")
                    )
                } else {
                    format!(
                        "{}; {in_graph} in the loaded graph (click to show them). Check out the pull request to fetch the rest.",
                        format::plural(commits.len(), "commit", "commits")
                    )
                },
            );
            ui.add_space(space::S);
            let rows: Vec<(String, String, String, Option<Oid>, bool)> = commits
                .iter()
                .map(|c| {
                    let oid = Oid::from_str(&c.sha)
                        .ok()
                        .filter(|o| snap.is_some_and(|s| s.index_of.contains_key(o)));
                    let who = c.author_login.clone().unwrap_or_else(|| c.author_name.clone());
                    let mut sub = format!("{}  ·  {}", &c.sha[..c.sha.len().min(10)], who);
                    if let Some(d) = c.date {
                        sub.push_str(&format!("  ·  {}", format::absolute(d)));
                    }
                    if c.parents > 1 {
                        sub.push_str("  ·  merge commit");
                    }
                    (
                        c.subject().to_owned(),
                        sub,
                        widgets::rel(c.date, now),
                        oid,
                        c.verified,
                    )
                })
                .collect();
            kit::list(
                ui,
                &format!("rv_commits_list_{n}"),
                rows.len(),
                kit::ROW_H,
                |_| false,
                |ui, rect, resp, i| {
                    let (title, sub, meta, oid, verified) = &rows[i];
                    let badges: Vec<(String, Tone)> = if *verified {
                        vec![("Verified".to_owned(), Tone::Success)]
                    } else {
                        Vec::new()
                    };
                    kit::paint_row(
                        ui,
                        rect,
                        &kit::Row::new(title)
                            .leading(kit::Leading::Dot(if oid.is_some() {
                                p.accent
                            } else {
                                p.text_3
                            }))
                            .subtitle(sub)
                            .meta(meta)
                            .badges(&badges)
                            .dimmed(oid.is_none()),
                        false,
                    );
                    if resp.clicked() {
                        match oid {
                            Some(o) => actions.push(Action::JumpTo(*o)),
                            None => actions.push(Action::Toast(
                                crate::state::ToastKind::Info,
                                "That commit is not in the loaded graph. Check out the pull request (or fetch) to see it.".to_owned(),
                            )),
                        }
                    }
                },
            );
        });
}
