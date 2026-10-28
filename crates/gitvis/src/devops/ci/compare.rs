//! "Compare with the last green run": the failing run next to the newest
//! earlier successful run of the same workflow on the same branch, the
//! commits and files between them, and for each failing job a diff of the
//! normalised logs (first divergence, lines only in the failing run, hunks).

use egui::{Align2, CornerRadius, FontId, Frame, Margin, Rect, RichText, Sense, Ui, pos2, vec2};

use gitvis_core::devops::ci::compare::{LogDiff, Tag};
use gitvis_core::devops::ci::model::{Between, CommitsSource};
use gitvis_core::github::WorkflowRun;
use gitvis_core::joblog::LogKind;

use super::control::{COMPARE_MAX_JOBS, compare_calls};
use super::widgets::{self, outcome_text, outcome_tone, short};
use super::{Act, CompareReport, JobDiff};
use crate::format;
use crate::state::{Action, RepoState};
use crate::theme::{Tone, medium, pal, size, space};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit;
use crate::views::truncated;

const LINE_H: f32 = 19.0;

/// Draw the comparison of the selected run, if one is open for it.
pub fn show(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) -> bool {
    let Some(selected) = repo.gh_page.selected_run else {
        return false;
    };
    let Some(c) = repo
        .devops
        .ci
        .compare
        .as_ref()
        .filter(|c| c.bad == selected)
    else {
        return false;
    };
    let now = format::now_secs();
    let in_graph = |sha: &str| {
        gitvis_core::Oid::from_str(sha).ok().filter(|o| {
            repo.snapshot
                .as_ref()
                .is_some_and(|s| s.index_of.contains_key(o))
        })
    };
    let bad = repo
        .github
        .as_ref()
        .and_then(|d| d.run(c.bad))
        .cloned()
        .or_else(|| c.report.ok().map(|r| r.bad.clone()));
    let p = *pal(ui);
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(("ci_compare", c.bad))
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
                            widgets::header(ui, "Compare with the last green run", |ui| {
                                if IconButton::new(Icon::Close, "Back to the run")
                                    .compact()
                                    .show(ui)
                                    .clicked()
                                {
                                    actions.push(Act::CompareClose.into());
                                }
                            });
                            let Some(bad) = &bad else {
                                kit::muted(ui, "The failing run is no longer loaded.");
                                return;
                            };
                            run_line(ui, "Failing", bad, now, in_graph(&bad.sha), actions);
                            if c.finding {
                                kit::loading(
                                    ui,
                                    "Looking for the newest earlier green run of this workflow on this branch (1 API call)…",
                                );
                                return;
                            }
                            let Some(good) = &c.good else {
                                kit::callout(
                                    ui,
                                    Tone::Warning,
                                    "No green run to compare with",
                                    c.missing.as_deref().unwrap_or(
                                        "There is no earlier successful run of this workflow on this branch.",
                                    ),
                                );
                                return;
                            };
                            run_line(ui, "Green", good, now, in_graph(&good.sha), actions);
                            if let (Some(a), Some(b)) = (good.created, bad.created) {
                                widgets::note(
                                    ui,
                                    &format!(
                                        "The green run started {} earlier, {} runs before.",
                                        format::duration(b - a),
                                        bad.run_number.saturating_sub(good.run_number)
                                    ),
                                );
                            }
                            match &c.report.data {
                                None => plan(
                                    ui,
                                    repo,
                                    bad,
                                    good,
                                    c.report.loading,
                                    c.lookup_calls,
                                    actions,
                                ),
                                Some(Err(e)) => {
                                    let mut retry = false;
                                    kit::callout_with(
                                        ui,
                                        Tone::Danger,
                                        "The comparison failed",
                                        e,
                                        |ui| {
                                            retry = IconButton::new(Icon::Fetch, "Retry")
                                                .compact()
                                                .show(ui)
                                                .clicked();
                                        },
                                    );
                                    if retry {
                                        actions.push(Act::CompareStart.into());
                                    }
                                }
                                Some(Ok(r)) => report(
                                    ui,
                                    r,
                                    View {
                                        job: c.job,
                                        lookup_calls: c.lookup_calls,
                                        all_commits: c.all_commits,
                                        all_files: c.all_files,
                                    },
                                    now,
                                    &in_graph,
                                    actions,
                                ),
                            }
                        });
                });
        });
    true
}

fn run_line(
    ui: &mut Ui,
    label: &str,
    r: &WorkflowRun,
    now: i64,
    jump: Option<gitvis_core::Oid>,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (rect, _) = ui.allocate_exact_size(vec2(64.0, 20.0), Sense::hover());
        ui.painter().text(
            pos2(rect.left(), rect.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::new(size::SMALL, medium()),
            p.text_2,
        );
        kit::badge(ui, outcome_text(r.outcome()), outcome_tone(r.outcome()));
        let title = if r.title.is_empty() {
            &r.name
        } else {
            &r.title
        };
        let link = if title == &r.name {
            format!("{title} #{}", r.run_number)
        } else {
            format!("{} #{}: {title}", r.name, r.run_number)
        };
        let w = ui.available_width();
        ui.scope(|ui| {
            ui.set_max_width(w);
            let resp = ui
                .add(
                    egui::Label::new(RichText::new(link).color(p.accent))
                        .sense(Sense::click())
                        .truncate(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.clicked() {
                actions.push(Action::OpenRun {
                    run: r.id,
                    job: None,
                });
            }
        });
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.add_space(70.0);
        let sha = kit::link(ui, short(&r.sha)).on_hover_text(if jump.is_some() {
            "Show this commit in the graph"
        } else {
            "Copy the full SHA"
        });
        if sha.clicked() {
            match jump {
                Some(o) => actions.push(Action::JumpTo(o)),
                None => actions.push(Action::Copy(r.sha.clone())),
            }
        }
        let mut facts = vec![r.branch.clone(), r.event.clone()];
        if let Some(s) = r.started.or(r.created) {
            facts.push(widgets::when(s, now));
        }
        if let Some(d) = r.duration_secs(now) {
            facts.push(format::duration(d));
        }
        if r.attempt > 1 {
            facts.push(format!("attempt {}", r.attempt));
        }
        ui.label(
            RichText::new(facts.join("  ·  "))
                .size(size::SMALL)
                .color(p.text_2),
        );
    });
}

fn plan(
    ui: &mut Ui,
    repo: &RepoState,
    bad: &WorkflowRun,
    good: &WorkflowRun,
    loading: bool,
    spent: usize,
    actions: &mut Vec<Action>,
) {
    let (calls, exact) = compare_calls(repo, bad, good);
    let failing = repo
        .gh_page
        .runs
        .get(&bad.id)
        .and_then(|d| d.jobs.as_ref())
        .and_then(|j| j.as_ref().ok())
        .map(|j| j.iter().filter(|x| x.outcome().is_bad()).count());
    ui.add_space(space::S);
    let mut lines = vec![
        "• the commits and files between the two runs (local git when both commits are here, else 1 compare call)".to_owned(),
        "• the jobs of the green run (1 call)".to_owned(),
    ];
    match failing {
        Some(n) => lines.push(format!(
            "• the logs of {} in both runs (2 calls each){}",
            format::plural(n.min(COMPARE_MAX_JOBS), "failing job", "failing jobs"),
            if n > COMPARE_MAX_JOBS {
                format!("; {} more are left out", n - COMPARE_MAX_JOBS)
            } else {
                String::new()
            }
        )),
        None => lines.push(
            "• the jobs of the failing run (1 call), then the logs of each failing job in both runs (2 calls each)"
                .to_owned(),
        ),
    }
    if spent > 0 {
        lines.push(format!(
            "Finding the green run already took {}.",
            format::plural(spent, "call", "calls")
        ));
    }
    let title = if exact {
        format!(
            "This takes {}",
            format::plural(calls, "API call", "API calls")
        )
    } else {
        format!("This takes at least {calls} API calls")
    };
    let mut start = false;
    kit::callout_with(ui, Tone::Accent, &title, &lines.join("\n"), |ui| {
        ui.add_space(4.0);
        if loading {
            kit::loading(ui, "Comparing: fetching jobs and logs, then diffing…");
        } else {
            start = IconButton::new(
                Icon::Play,
                &format!(
                    "Compare ({}{calls} API calls)",
                    if exact { "" } else { "at least " }
                ),
            )
            .primary(true)
            .compact()
            .show(ui)
            .clicked();
        }
    });
    if start {
        actions.push(Act::CompareStart.into());
    }
    kit::secondary(
        ui,
        "Logs are compared line by line after replacing what always differs between runs: timestamps, durations, dates and times, run and job ids, commit SHAs and other hex ids, UUIDs and temporary paths.",
    );
}

/// What the report shows.
struct View {
    job: usize,
    lookup_calls: usize,
    all_commits: bool,
    all_files: bool,
}

fn report(
    ui: &mut Ui,
    r: &CompareReport,
    v: View,
    now: i64,
    in_graph: &dyn Fn(&str) -> Option<gitvis_core::Oid>,
    actions: &mut Vec<Action>,
) {
    let commits = r.between.as_ref().map(|b| b.total_commits as usize).ok();
    let files = r.between.as_ref().map(|b| b.files.len()).ok();
    kit::stats(
        ui,
        &[
            kit::Stat::new(
                "Commits in between",
                commits.map(format::count).unwrap_or_else(|| "—".to_owned()),
            ),
            kit::Stat::new(
                "Files changed",
                files.map(format::count).unwrap_or_else(|| "—".to_owned()),
            ),
            kit::Stat::new("Failing jobs compared", format::count(r.jobs.len()))
                .tone_if(!r.jobs.is_empty(), Tone::Danger),
            kit::Stat::new("API calls", format::count(r.calls + v.lookup_calls)).tip(
                "Finding the green run, the commit comparison, the jobs and the two logs of each failing job",
            ),
            kit::Stat::new("Took", format::millis(r.elapsed_ms)),
        ],
    );
    log_diffs(ui, r, &v, actions);
    match &r.between {
        Ok(b) => between(ui, b, &v, now, in_graph, actions),
        Err(e) => kit::callout(
            ui,
            Tone::Warning,
            "Could not list the commits in between",
            e,
        ),
    }
}

fn log_diffs(ui: &mut Ui, r: &CompareReport, v: &View, actions: &mut Vec<Action>) {
    kit::section(ui, "Log diff", Some(r.jobs.len()));
    if r.jobs.is_empty() {
        kit::muted(
            ui,
            "The failing run has no failed jobs (it may have been cancelled or failed before any job started).",
        );
        return;
    }
    if !r.skipped.is_empty() {
        widgets::note(
            ui,
            &format!(
                "{} more failing jobs not compared: {}",
                r.skipped.len(),
                r.skipped.join(", ")
            ),
        );
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for (i, j) in r.jobs.iter().enumerate() {
            let label = match &j.diff {
                Ok(d) => format!("{}  {}", j.name, format::count(d.changed)),
                Err(_) => format!("{}  —", j.name),
            };
            if kit::chip(ui, &label, i == v.job).clicked() && i != v.job {
                actions.push(Act::CompareJob(i).into());
            }
        }
    });
    if let Some(j) = r.jobs.get(v.job).or_else(|| r.jobs.first()) {
        job_diff(ui, j, actions);
    }
}

fn between(
    ui: &mut Ui,
    b: &Between,
    v: &View,
    now: i64,
    in_graph: &dyn Fn(&str) -> Option<gitvis_core::Oid>,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    kit::section(ui, "Commits in between", Some(b.total_commits as usize));
    widgets::note(
        ui,
        match b.source {
            CommitsSource::Local => "From the local repository (git log good..bad)",
            CommitsSource::Api => "From GitHub's compare API (a commit is not in this clone)",
        },
    );
    if b.source == CommitsSource::Api && b.behind_by > 0 {
        kit::callout(
            ui,
            Tone::Warning,
            "The runs are on diverged commits",
            &format!(
                "The failing commit is {} the green one (a force-push or a re-run on an older commit); the list shows what the failing commit has that the green one does not.",
                format::plural(b.behind_by as usize, "commit behind", "commits behind")
            ),
        );
    }
    if b.commits.is_empty() {
        kit::muted(
            ui,
            "Both runs are on the same commit: the failure is not caused by a code change (flaky test, runner, network, or an external dependency).",
        );
    }
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let max = if v.all_commits { 500 } else { 6 };
        for c in b.commits.iter().take(max) {
            let (rect, resp) =
                ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
            kit::row_background(ui, rect, resp.hovered(), false);
            let painter = ui.painter();
            let mid = rect.center().y;
            painter.text(
                pos2(rect.left() + 10.0, mid),
                Align2::LEFT_CENTER,
                short(&c.sha),
                FontId::monospace(size::MONO),
                p.accent,
            );
            let right = format!(
                "{}  ·  {}",
                c.author,
                c.time.map(|t| format::relative(t, now)).unwrap_or_default()
            );
            let rr = painter.text(
                pos2(rect.right() - 10.0, mid),
                Align2::RIGHT_CENTER,
                right,
                FontId::proportional(size::SMALL),
                p.text_2,
            );
            let g = truncated(
                painter,
                &c.summary,
                FontId::proportional(size::BODY - 0.5),
                p.text,
                (rr.left() - rect.left() - 90.0).max(40.0),
            );
            painter.galley(pos2(rect.left() + 76.0, mid - g.size().y / 2.0), g, p.text);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                match in_graph(&c.sha) {
                    Some(o) => actions.push(Action::JumpTo(o)),
                    None => actions.push(Action::Copy(c.sha.clone())),
                }
            }
        }
    });
    let max = if v.all_commits { 500 } else { 6 };
    if b.commits.len() > max
        && IconButton::text(&format!("Show all {}", b.commits.len()))
            .compact()
            .show(ui)
            .clicked()
    {
        actions.push(Act::CompareExpand { commits: true }.into());
    }
    kit::section(ui, "Files changed", Some(b.files.len()));
    if b.files.is_empty() {
        kit::muted(ui, "No file changed.");
        return;
    }
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let max = if v.all_files { 1000 } else { 8 };
        for f in b.files.iter().take(max) {
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
            let painter = ui.painter();
            let mid = rect.center().y;
            let (tone, letter) = match f.status.as_str() {
                "added" => (Tone::Success, "A"),
                "removed" => (Tone::Danger, "D"),
                "renamed" => (Tone::Accent, "R"),
                _ => (Tone::Warning, "M"),
            };
            painter.text(
                pos2(rect.left() + 14.0, mid),
                Align2::CENTER_CENTER,
                letter,
                FontId::new(size::SMALL, medium()),
                tone.color(&p),
            );
            let stats = format!("+{}  −{}", f.additions, f.deletions);
            let sr = painter.text(
                pos2(rect.right() - 10.0, mid),
                Align2::RIGHT_CENTER,
                stats,
                FontId::monospace(size::SMALL),
                p.text_2,
            );
            let path = match &f.old_path {
                Some(o) => format!("{o} → {}", f.path),
                None => f.path.clone(),
            };
            let g = truncated(
                painter,
                &path,
                FontId::monospace(size::MONO),
                p.text,
                (sr.left() - rect.left() - 40.0).max(40.0),
            );
            painter.galley(pos2(rect.left() + 30.0, mid - g.size().y / 2.0), g, p.text);
        }
    });
    let max = if v.all_files { 1000 } else { 8 };
    if b.files.len() > max
        && IconButton::text(&format!("Show all {}", b.files.len()))
            .compact()
            .show(ui)
            .clicked()
    {
        actions.push(Act::CompareExpand { commits: false }.into());
    }
}

fn job_diff(ui: &mut Ui, j: &JobDiff, actions: &mut Vec<Action>) {
    ui.add_space(space::S);
    kit::subtitle(ui, &j.name);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        if kit::link(ui, "Failing job log").clicked() {
            actions.push(Action::ShowLog {
                run: j.bad.run_id,
                job: j.bad.id,
                step: None,
            });
        }
        if let Some(g) = &j.good
            && kit::link(ui, "Green job on GitHub").clicked()
        {
            actions.push(Action::OpenUrl(g.url.clone()));
        }
    });
    let d = match &j.diff {
        Ok(d) => d,
        Err(e) => {
            kit::callout(ui, Tone::Warning, "No log diff", e);
            return;
        }
    };
    widgets::note(
        ui,
        &format!(
            "Green log {} lines, failing log {} lines, {} differ after normalising{}{}",
            format::count(d.good_lines),
            format::count(d.bad_lines),
            format::count(d.changed),
            if d.timed_out {
                " (the diff hit its 3 s budget; hunks may be coarse)"
            } else {
                ""
            },
            if d.truncated {
                " (cut to the first 60 hunks / 400 lines)"
            } else {
                ""
            }
        ),
    );
    if d.good_from > 0 || d.bad_from > 0 {
        widgets::note(
            ui,
            &format!(
                "The Set up job step is left out (lines 1–{} and 1–{}): the runner and image versions it prints differ in every run.",
                d.good_from, d.bad_from
            ),
        );
    }
    first_divergence(ui, d);
    only_in_failing(ui, d);
    hunks(ui, d);
}

fn first_divergence(ui: &mut Ui, d: &LogDiff) {
    kit::section(ui, "First divergence", None);
    let Some(f) = &d.first else {
        kit::muted(
            ui,
            "The logs match once normalised: the job failed the same way the green one passed, which points at something outside the log (an exit code, a timeout, a cancelled dependency).",
        );
        return;
    };
    widgets::note(
        ui,
        &format!(
            "Line {} of the green log, line {} of the failing log",
            f.good_no, f.bad_no
        ),
    );
    code(ui, |ui| {
        for (i, l) in f.before.iter().enumerate() {
            let n = f.bad_no.saturating_sub(f.before.len() - i);
            line(ui, Some(n), ' ', l, None);
        }
        if let Some(g) = &f.good_text {
            line(ui, Some(f.good_no), '−', g, Some(Tone::Success));
        }
        if let Some(b) = &f.bad_text {
            line(ui, Some(f.bad_no), '+', b, Some(Tone::Danger));
        }
    });
}

fn only_in_failing(ui: &mut Ui, d: &LogDiff) {
    let errors = d.only_bad.iter().filter(|l| l.is_error()).count();
    let warnings = d
        .only_bad
        .iter()
        .filter(|l| !l.is_error() && l.is_warning())
        .count();
    kit::section(ui, "Only in the failing run", Some(d.only_bad.len()));
    if d.only_bad.is_empty() {
        kit::muted(
            ui,
            "Every line of the failing log also appears in the green one.",
        );
        return;
    }
    let mut b = Vec::new();
    if errors > 0 {
        b.push((
            format::plural(errors, "error line", "error lines"),
            Tone::Danger,
        ));
    }
    if warnings > 0 {
        b.push((
            format::plural(warnings, "warning line", "warning lines"),
            Tone::Warning,
        ));
    }
    widgets::badges(ui, &b);
    code(ui, |ui| {
        for l in d.only_bad.iter().take(200) {
            let tone = if l.is_error() {
                Some(Tone::Danger)
            } else if l.is_warning() {
                Some(Tone::Warning)
            } else {
                None
            };
            line(ui, Some(l.no), '+', &l.text, tone);
        }
    });
    if d.only_bad.len() > 200 {
        kit::muted(ui, &format!("and {} more lines", d.only_bad.len() - 200));
    }
}

fn hunks(ui: &mut Ui, d: &LogDiff) {
    kit::section(ui, "Diff", Some(d.hunks.len()));
    if d.hunks.is_empty() {
        kit::muted(ui, "No differences.");
        return;
    }
    widgets::note(
        ui,
        "− only in the green log  ·  + only in the failing log  ·  3 lines of context",
    );
    for h in d.hunks.iter().take(30) {
        ui.add_space(4.0);
        widgets::note(
            ui,
            &format!("green line {}, failing line {}", h.good_start, h.bad_start),
        );
        code(ui, |ui| {
            for l in &h.lines {
                let (mark, tone, n) = match l.tag {
                    Tag::Same => (' ', None, l.bad_no),
                    Tag::OnlyGood => ('−', Some(Tone::Success), l.good_no),
                    Tag::OnlyBad => (
                        '+',
                        Some(if l.kind == LogKind::Error {
                            Tone::Danger
                        } else {
                            Tone::Warning
                        }),
                        l.bad_no,
                    ),
                };
                line(ui, n, mark, &l.text, tone);
            }
        });
    }
    if d.hunks.len() > 30 {
        kit::muted(ui, &format!("and {} more hunks", d.hunks.len() - 30));
    }
}

/// A sunken monospace block.
fn code(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    Frame::NONE
        .fill(if ui.visuals().dark_mode {
            p.chrome
        } else {
            p.raised
        })
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(crate::theme::radius::M))
        .inner_margin(Margin::symmetric(0, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            add(ui);
        });
}

/// One log line: number, mark, text; tinted by `tone`.
fn line(ui: &mut Ui, no: Option<usize>, mark: char, text: &str, tone: Option<Tone>) {
    let p = *pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), LINE_H), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    let mid = rect.center().y;
    let color = match tone {
        Some(Tone::Danger) => p.danger,
        Some(Tone::Warning) => p.warning,
        Some(Tone::Success) => p.success,
        _ => p.text,
    };
    if let Some(t) = tone {
        let c = t.color(&p);
        painter.rect_filled(
            rect,
            0,
            c.gamma_multiply(if ui.visuals().dark_mode { 0.12 } else { 0.09 }),
        );
        painter.rect_filled(
            Rect::from_min_size(rect.min, vec2(2.0, rect.height())),
            0,
            c,
        );
    }
    if let Some(n) = no {
        painter.text(
            pos2(rect.left() + 52.0, mid),
            Align2::RIGHT_CENTER,
            n.to_string(),
            FontId::monospace(size::CAPTION),
            p.text_3,
        );
    }
    painter.text(
        pos2(rect.left() + 64.0, mid),
        Align2::CENTER_CENTER,
        mark.to_string(),
        FontId::monospace(size::MONO),
        color,
    );
    let g = truncated(
        painter,
        text,
        FontId::monospace(size::MONO - 0.5),
        color,
        (rect.width() - 84.0).max(40.0),
    );
    painter.galley(pos2(rect.left() + 76.0, mid - g.size().y / 2.0), g, color);
}
