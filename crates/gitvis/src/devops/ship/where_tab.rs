//! The "Where is my commit" tab: a vertical timeline of the checkpoints a
//! commit passes (authored, committed, pull request, branches, tags and
//! releases, CI, each environment), each done, not yet or unknown.

use egui::{Align2, FontId, Stroke, Ui, pos2};

use gitvis_core::Oid;
use gitvis_core::devops::ship::deploy;
use gitvis_core::devops::ship::whereis::{self, Located, Mark};
use gitvis_core::github::{Outcome, aggregate};

use super::{Act, count, ui as sui};
use crate::format;
use crate::state::{Action, RepoState, Settings};
use crate::theme::{Tone, pal, size};
use crate::views::icons::{self, Icon, IconButton};
use crate::views::{form, kit};

pub fn show(ui: &mut Ui, repo: &mut RepoState, _settings: &Settings, actions: &mut Vec<Action>) {
    let now = format::now_secs();
    crate::devops::pages::layout::page_header(
        ui,
        "Where is my commit",
        "Which pull request, branches, tags, releases, checks and environments contain a commit",
        |ui| {
            let st = &repo.devops.ship;
            if st.where_local.loading || st.where_pulls.loading || st.envs.loading {
                ui.add(egui::Spinner::new().size(14.0));
            }
        },
    );
    sui::scroll_page(ui, "ship_where_page", |ui| {
        // The question.
        let st = &mut repo.devops.ship;
        let mut go = false;
        // The buttons wrap under the input in a narrow window (a row wider
        // than the page widens everything below it).
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            let w = (ui.available_width() - 24.0).min(340.0);
            let resp = form::input(
                ui,
                "ship_where_input",
                &mut st.where_input,
                form::InputOpts::new("Commit id, branch or tag")
                    .width(w)
                    .mono(),
            );
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                go = true;
            }
            if form::primary(ui, "Find", !st.where_input.trim().is_empty()).clicked() {
                go = true;
            }
            if form::button(ui, "Selected commit", true)
                .on_hover_text("The commit selected in the history graph")
                .clicked()
            {
                actions.push(Act::WhereSelected.into());
            }
            if form::button(ui, "HEAD", true).clicked() {
                actions.push(Act::WhereHead.into());
            }
        });
        if go {
            actions.push(Act::WhereRev(st.where_input.trim().to_owned()).into());
        }
        ui.add_space(8.0);
        let st = &repo.devops.ship;
        let located = match &st.where_local.result {
            Some(Ok(l)) => l.clone(),
            Some(Err(e)) if !st.where_local.loading => {
                kit::callout(ui, Tone::Danger, "Could not find that commit", e);
                return;
            }
            _ => {
                if st.where_local.loading {
                    kit::loading(
                        ui,
                        "Reading branches, tags and deployed commits from local history…",
                    );
                } else {
                    kit::secondary(
                        ui,
                        "Type a commit id, branch or tag, or pick the selected commit.",
                    );
                }
                return;
            }
        };
        timeline(ui, repo, &located, now, actions);
    });
}

fn summary_line(repo: &RepoState, l: &Located) -> (Tone, String, String) {
    let order = repo.devops.ship.env_order();
    let reached: Vec<&whereis::EnvHit> = l.envs.iter().filter(|e| e.mark == Mark::Done).collect();
    let prod = l.envs.iter().find(|e| deploy::is_production(&e.env));
    let title = format!("{} {}", &l.sha[..10.min(l.sha.len())], l.summary);
    match prod {
        Some(p) if p.mark == Mark::Done => (
            Tone::Success,
            title,
            match &p.since {
                Some((t, _, _)) => format!(
                    "In {} since {} ({}).",
                    p.env,
                    format::absolute(*t),
                    format::relative(*t, format::now_secs())
                ),
                None => format!("In {}.", p.env),
            },
        ),
        _ => {
            let furthest = order
                .iter()
                .rev()
                .find(|e| reached.iter().any(|r| &r.env == *e));
            let text = match (furthest, l.first_tag()) {
                (Some(e), _) => format!(
                    "Deployed to {e}{}.",
                    match prod {
                        Some(p) if p.mark == Mark::NotYet => format!(", not in {} yet", p.env),
                        _ => String::new(),
                    }
                ),
                (None, Some((t, _))) => format!("Released in {t}; no environment known to run it."),
                (None, None) if !l.remote_branches.is_empty() => {
                    "Pushed, not released or deployed yet.".to_owned()
                }
                (None, None) if !l.local_branches.is_empty() => {
                    "Only on local branches: not pushed yet.".to_owned()
                }
                _ => "On no branch: reachable from nothing but its id.".to_owned(),
            };
            (Tone::Neutral, title, text)
        }
    }
}

fn timeline(ui: &mut Ui, repo: &RepoState, l: &Located, now: i64, actions: &mut Vec<Action>) {
    let st = &repo.devops.ship;
    let (tone, title, text) = summary_line(repo, l);
    kit::callout_with(ui, tone, &title, &text, |ui| {
        ui.horizontal(|ui| {
            if sui::in_graph(repo, &l.sha)
                && IconButton::new(Icon::Graph, "Show in graph")
                    .compact()
                    .show(ui)
                    .clicked()
            {
                actions.push(Act::JumpTo(l.sha.clone()).into());
            }
            if IconButton::new(Icon::Copy, "Copy id")
                .compact()
                .show(ui)
                .clicked()
            {
                actions.push(Action::Copy(l.sha.clone()));
            }
            if let Some((o, r)) = &repo.github_remote
                && IconButton::new(Icon::External, "Open on GitHub")
                    .compact()
                    .show(ui)
                    .clicked()
            {
                actions.push(Action::OpenUrl(format!(
                    "https://github.com/{o}/{r}/commit/{}",
                    l.sha
                )));
            }
        });
    });
    ui.add_space(12.0);

    let mut steps: Vec<Step> = Vec::new();
    // Authored and committed.
    steps.push(Step::new(
        Mark::Done,
        "Authored",
        vec![format!(
            "{}{} · {}",
            l.author,
            if l.author_email.is_empty() {
                String::new()
            } else {
                format!(" <{}>", l.author_email)
            },
            sui::when(l.author_time, now)
        )],
    ));
    let mut committed = vec![format!(
        "{} · {}",
        l.committer,
        sui::when(l.commit_time, now)
    )];
    if l.commit_time - l.author_time > 3_600 {
        committed.push(format!(
            "{} after it was written (rebased, amended or applied later)",
            sui::span(l.commit_time - l.author_time)
        ));
    }
    steps.push(Step::new(Mark::Done, "Committed", committed));

    // Pull request.
    steps.push(pull_step(repo, l, now));

    // Branches.
    let nb = l.local_branches.len() + l.remote_branches.len();
    let mut lines = vec![format!(
        "{} local, {} remote",
        count(l.local_branches.len(), "branch", "branches"),
        format::count(l.remote_branches.len())
    )];
    let mut names: Vec<&String> = l.remote_branches.iter().chain(&l.local_branches).collect();
    names.truncate(14);
    if !names.is_empty() {
        lines.push(
            names
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
                + if nb > 14 { ", …" } else { "" },
        );
    }
    steps.push(Step::new(
        if nb > 0 { Mark::Done } else { Mark::NotYet },
        "On branches",
        if nb > 0 {
            lines
        } else {
            vec!["No branch contains it: it was rewritten, or only its id keeps it.".to_owned()]
        },
    ));

    // Tags and releases.
    let releases: Vec<(String, String)> = st
        .releases
        .ok()
        .map(|list| {
            l.tags
                .iter()
                .filter_map(|(t, _)| {
                    list.iter()
                        .find(|r| &r.tag_name == t && !r.draft)
                        .map(|r| (r.tag_name.clone(), r.title().to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    let tag_step = match l.first_tag() {
        Some((t, time)) => {
            let mut lines = vec![format!(
                "First tag: {t} · {}",
                if *time > 0 {
                    sui::when(*time, now)
                } else {
                    String::new()
                }
            )];
            lines.push(format!(
                "{} {}",
                count(l.tags_total, "tag", "tags"),
                if l.tags_total == 1 {
                    "contains it"
                } else {
                    "contain it"
                }
            ));
            match (st.releases.ok(), releases.first()) {
                (Some(_), Some((tag, title))) => lines.push(format!(
                    "First release: {title}{} · {} {}",
                    if title != tag {
                        format!(" ({tag})")
                    } else {
                        String::new()
                    },
                    count(releases.len(), "release", "releases"),
                    if releases.len() == 1 {
                        "contains it"
                    } else {
                        "contain it"
                    }
                )),
                (Some(_), None) => lines.push("No published release contains it yet.".to_owned()),
                (None, _) if repo.github_remote.is_some() => {
                    lines.push("Releases are still loading.".to_owned())
                }
                _ => {}
            }
            Step::new(Mark::Done, "Released", lines).tag(t.clone())
        }
        None => Step::new(
            Mark::NotYet,
            "Released",
            vec![
                "No tag contains it yet: the next release from a branch that has it will."
                    .to_owned(),
            ],
        ),
    };
    steps.push(tag_step);

    // CI.
    steps.push(ci_step(repo, l, now));

    // Environments.
    if st.envs.loading && st.envs.result.is_none() {
        steps.push(Step::new(
            Mark::Unknown,
            "Environments",
            vec!["Loading environments and deployments…".to_owned()],
        ));
    } else if let Some(e) = st.envs.err() {
        steps.push(Step::new(
            Mark::Unknown,
            "Environments",
            vec![format!("Could not load environments: {e}")],
        ));
    } else if l.envs.is_empty() {
        steps.push(Step::new(
            Mark::Unknown,
            "Environments",
            vec![if repo.github_remote.is_none() {
                "Environments come from GitHub; this repository has no GitHub remote.".to_owned()
            } else {
                "This repository has no environment with deployments.".to_owned()
            }],
        ));
    }
    for e in &l.envs {
        let mut lines = Vec::new();
        match e.mark {
            Mark::Done => match &e.since {
                Some((t, id, sha)) => lines.push(format!(
                    "Since {} · deployment {id} of {}",
                    sui::when(*t, now),
                    &sha[..sha.len().min(10)]
                )),
                None => {
                    lines.push("Deployed (first deployment not in the history read).".to_owned())
                }
            },
            Mark::NotYet => {
                if let Some(c) = &e.current {
                    lines.push(format!(
                        "{} runs {}, which does not contain it.",
                        e.env,
                        &c[..c.len().min(10)]
                    ));
                }
            }
            Mark::Unknown => {}
        }
        if !e.note.is_empty() {
            lines.push(e.note.clone());
        }
        let mut s = Step::new(e.mark, &e.env, lines);
        if deploy::is_production(&e.env) {
            s.badge = Some(("Production".to_owned(), Tone::Purple));
        }
        steps.push(s);
    }

    let n = steps.len();
    for (i, s) in steps.into_iter().enumerate() {
        checkpoint(ui, &s, i + 1 == n, actions);
    }
}

struct Step {
    mark: Mark,
    title: String,
    lines: Vec<String>,
    badge: Option<(String, Tone)>,
    /// A tag to show in the graph.
    tag: Option<String>,
    link: Option<(String, String)>,
}

impl Step {
    fn new(mark: Mark, title: &str, lines: Vec<String>) -> Self {
        Self {
            mark,
            title: title.to_owned(),
            lines,
            badge: None,
            tag: None,
            link: None,
        }
    }

    fn tag(mut self, t: String) -> Self {
        self.tag = Some(t);
        self
    }
}

fn pull_step(repo: &RepoState, l: &Located, now: i64) -> Step {
    let st = &repo.devops.ship;
    if repo.github_remote.is_none() {
        return Step::new(
            Mark::Unknown,
            "Pull request",
            vec!["Needs a GitHub remote.".to_owned()],
        );
    }
    if st.where_pulls.loading {
        return Step::new(
            Mark::Unknown,
            "Pull request",
            vec!["Looking up the pull request (GET commits/{sha}/pulls)…".to_owned()],
        );
    }
    match &st.where_pulls.result {
        Some(Err(e)) if super::not_on_github(e) => Step::new(
            Mark::NotYet,
            "Pull request",
            vec!["GitHub does not have this commit yet: push it, then look again.".to_owned()],
        ),
        Some(Err(e)) => Step::new(Mark::Unknown, "Pull request", vec![e.clone()]),
        Some(Ok(list)) => match whereis::introducing(list, &l.sha) {
            None => Step::new(
                Mark::Unknown,
                "Pull request",
                vec!["No pull request is associated with it (pushed directly, or the remote does not have it).".to_owned()],
            ),
            Some(p) => {
                let mut lines = vec![format!("#{} {}", p.number, p.title)];
                let mark = match p.merged_at {
                    Some(t) => {
                        lines.push(format!(
                            "Merged into {} {}{}",
                            p.base,
                            sui::when(t, now),
                            if p.merged_by.is_empty() {
                                String::new()
                            } else {
                                format!(" by {}", p.merged_by)
                            }
                        ));
                        Mark::Done
                    }
                    None => {
                        lines.push(format!(
                            "{} {} → {} by {}",
                            if p.state == "open" { "Open" } else { "Closed without merging" },
                            p.head,
                            p.base,
                            p.user
                        ));
                        Mark::NotYet
                    }
                };
                if list.len() > 1 {
                    lines.push(format!(
                        "{} mention the commit",
                        count(list.len(), "pull request", "pull requests")
                    ));
                }
                let mut s = Step::new(mark, "Pull request", lines);
                s.link = Some((format!("Open #{}", p.number), p.html_url.clone()));
                s
            }
        },
        None => Step::new(Mark::Unknown, "Pull request", vec!["Not looked up yet.".to_owned()]),
    }
}

fn ci_step(repo: &RepoState, l: &Located, _now: i64) -> Step {
    let Some(oid) = Oid::from_str(&l.sha).ok() else {
        return Step::new(Mark::Unknown, "Checks", vec![]);
    };
    if repo.github_remote.is_none() {
        return Step::new(
            Mark::Unknown,
            "Checks",
            vec!["Needs a GitHub remote.".to_owned()],
        );
    }
    let Some(state) = repo.checks.get(&oid) else {
        return Step::new(
            Mark::Unknown,
            "Checks",
            vec!["Fetching check runs…".to_owned()],
        );
    };
    match &state.result {
        None => Step::new(
            Mark::Unknown,
            "Checks",
            vec!["Fetching check runs…".to_owned()],
        ),
        Some(Err(e)) if super::not_on_github(e) => Step::new(
            Mark::NotYet,
            "Checks",
            vec!["No checks yet: GitHub does not have this commit until it is pushed.".to_owned()],
        ),
        Some(Err(e)) => Step::new(Mark::Unknown, "Checks", vec![e.clone()]),
        Some(Ok(list)) if list.is_empty() => Step::new(
            Mark::Unknown,
            "Checks",
            vec!["No check reported on this commit.".to_owned()],
        ),
        Some(Ok(list)) => {
            let outcome = aggregate(list.iter().map(|c| c.outcome()));
            let passed = list.iter().filter(|c| c.outcome().is_good()).count();
            let failed: Vec<&str> = list
                .iter()
                .filter(|c| c.outcome().is_bad())
                .map(|c| c.name.as_str())
                .collect();
            let running: Vec<&str> = list
                .iter()
                .filter(|c| c.outcome().is_running())
                .map(|c| c.name.as_str())
                .collect();
            let mut lines = vec![format!(
                "{}: {passed} passed, {} failed, {} running, {} other",
                count(list.len(), "check", "checks"),
                failed.len(),
                running.len(),
                list.len() - passed - failed.len() - running.len()
            )];
            if !failed.is_empty() {
                lines.push(format!("Failed: {}", list_names(&failed)));
            }
            if !running.is_empty() {
                lines.push(format!("Running: {}", list_names(&running)));
            }
            let mark = match outcome {
                Outcome::Success | Outcome::Neutral | Outcome::Skipped => Mark::Done,
                o if o.is_bad() || o.is_running() => Mark::NotYet,
                _ => Mark::Unknown,
            };
            let mut s = Step::new(mark, "Checks", lines);
            s.badge = Some((
                outcome.label().to_owned(),
                match outcome {
                    o if o.is_good() => Tone::Success,
                    o if o.is_bad() => Tone::Danger,
                    o if o.is_running() => Tone::Accent,
                    _ => Tone::Neutral,
                },
            ));
            s
        }
    }
}

fn list_names(names: &[&str]) -> String {
    let mut s = names.iter().take(6).copied().collect::<Vec<_>>().join(", ");
    if names.len() > 6 {
        s.push_str(&format!(" and {} more", names.len() - 6));
    }
    s
}

fn mark_badge(m: Mark) -> (&'static str, Tone) {
    match m {
        Mark::Done => ("Done", Tone::Success),
        Mark::NotYet => ("Not yet", Tone::Warning),
        Mark::Unknown => ("Unknown", Tone::Neutral),
    }
}

/// One checkpoint: a dot on the timeline, its title and status, its lines.
fn checkpoint(ui: &mut Ui, s: &Step, last: bool, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let gap = 14.0;
    let top = ui.cursor().top();
    let left = ui.cursor().left();
    let resp = ui
        .horizontal_top(|ui| {
            ui.add_space(36.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    kit::subtitle(ui, &s.title);
                    let (text, tone) = mark_badge(s.mark);
                    kit::badge(ui, text, tone);
                    if let Some((b, t)) = &s.badge {
                        kit::badge(ui, b, *t);
                    }
                });
                for line in &s.lines {
                    kit::secondary(ui, line);
                }
                if s.tag.is_some() || s.link.is_some() {
                    ui.horizontal(|ui| {
                        if let Some(t) = &s.tag
                            && kit::link(ui, &format!("Show {t} in the graph")).clicked()
                        {
                            actions.push(Act::JumpToTag(t.clone()).into());
                        }
                        if let Some((label, url)) = &s.link
                            && kit::link(ui, label).clicked()
                        {
                            actions.push(Action::OpenUrl(url.clone()));
                        }
                    });
                }
            });
        })
        .response;
    let painter = ui.painter();
    let c = pos2(left + 13.0, top + 10.0);
    if !last {
        painter.line_segment(
            [
                pos2(c.x, c.y + 10.0),
                pos2(c.x, resp.rect.bottom() + gap + 1.0),
            ],
            Stroke::new(2.0, p.border_strong),
        );
    }
    match s.mark {
        Mark::Done => {
            painter.circle_filled(c, 8.0, p.success);
            icons::paint(painter, Icon::Check, c, 10.0, p.canvas);
        }
        Mark::NotYet => {
            painter.circle_filled(c, 8.0, p.canvas);
            painter.circle_stroke(c, 7.0, Stroke::new(2.0, p.warning));
        }
        Mark::Unknown => {
            painter.circle_filled(c, 8.0, p.canvas);
            painter.circle_stroke(c, 7.0, Stroke::new(1.5, p.text_3));
            painter.text(
                c,
                Align2::CENTER_CENTER,
                "?",
                FontId::proportional(size::CAPTION),
                p.text_3,
            );
        }
    }
    ui.add_space(gap);
}
