//! The Workflows tab: every workflow on GitHub plus local files not pushed
//! yet, with state and enable / disable; per workflow the history of its
//! last 100 runs (stat tiles, duration trend, runs by branch and event,
//! flaky runs), the on-demand job analysis, and the dispatch form.

use egui::{Color32, Ui, vec2};

use gitvis_core::devops::ci::history;
use gitvis_core::devops::ci::workflow::{InputKind, WorkflowFile};
use gitvis_core::github::{GitHubData, WorkflowRun};

use super::control::{ANALYSE_MAX_RUNS, dispatch_refs};
use super::widgets::{self, pct};
use super::{Act, Analysis, DispatchForm, History, Load, LocalInfo, State};
use crate::format;
use crate::state::{Action, RepoState, Settings};
use crate::theme::{Tone, pal, space};
use crate::views::form;
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Col};

/// One row of the workflow list.
pub struct Entry<'a> {
    pub path: String,
    pub name: String,
    pub id: Option<u64>,
    /// `active`, `disabled_manually`, `disabled_inactivity`, … (GitHub only).
    pub state: Option<&'a str>,
    pub file: Option<&'a WorkflowFile>,
    pub last: Option<&'a WorkflowRun>,
}

impl Entry<'_> {
    pub fn disabled(&self) -> bool {
        self.state.is_some_and(|s| s.starts_with("disabled"))
    }

    pub fn dynamic(&self) -> bool {
        self.path.starts_with("dynamic/")
    }
}

fn state_text(s: &str) -> String {
    match s {
        "active" => "Active".to_owned(),
        "disabled_manually" => "Disabled".to_owned(),
        "disabled_inactivity" => "Disabled for inactivity".to_owned(),
        "disabled_fork" => "Disabled in fork".to_owned(),
        "deleted" => "Deleted".to_owned(),
        other => other.replace('_', " "),
    }
}

/// GitHub's workflows and local-only files, workflows with a file in the
/// working tree first, then GitHub's dynamic ones, each by name.
pub fn entries_from<'a>(
    github: Option<&'a GitHubData>,
    local: Option<&'a LocalInfo>,
) -> Vec<Entry<'a>> {
    let files: &[WorkflowFile] = local.map(|l| l.files.as_slice()).unwrap_or(&[]);
    let mut out: Vec<Entry> = Vec::new();
    if let Some(d) = github {
        for w in &d.workflows {
            out.push(Entry {
                path: w.path.clone(),
                name: if w.name.is_empty() || w.name == w.path {
                    files
                        .iter()
                        .find(|f| f.path == w.path)
                        .map(|f| f.name.clone())
                        .unwrap_or_else(|| file_stem(&w.path))
                } else {
                    w.name.clone()
                },
                id: Some(w.id),
                state: Some(w.state.as_str()),
                file: files.iter().find(|f| f.path == w.path),
                last: d.runs.iter().find(|r| r.workflow_id == w.id),
            });
        }
    }
    for f in files {
        if !out.iter().any(|e| e.path == f.path) {
            out.push(Entry {
                path: f.path.clone(),
                name: f.name.clone(),
                id: None,
                state: None,
                file: Some(f),
                last: None,
            });
        }
    }
    out.sort_by(|a, b| {
        a.dynamic()
            .cmp(&b.dynamic())
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

pub fn entries(repo: &RepoState) -> Vec<Entry<'_>> {
    entries_from(repo.github.as_ref(), repo.devops.ci.local.ok())
}

/// `deploy` of `.github/workflows/deploy.yml`.
fn file_stem(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    base.trim_end_matches(".yml")
        .trim_end_matches(".yaml")
        .to_owned()
}

fn matches(e: &Entry, needle: &str) -> bool {
    needle.is_empty()
        || e.name.to_lowercase().contains(needle)
        || e.path.to_lowercase().contains(needle)
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, _settings: &Settings, actions: &mut Vec<Action>) {
    let refs = dispatch_refs(repo);
    let now = format::now_secs();
    let t = ui.input(|i| i.time);
    let RepoState {
        github,
        devops,
        snapshot,
        ..
    } = repo;
    let current = snapshot
        .as_ref()
        .and_then(|s| s.head.branch().map(str::to_owned));
    let State {
        local,
        wf_selected,
        wf_filter,
        wf_branch,
        wf_branch_applied,
        history,
        analysis,
        dispatch,
        wf_busy,
        scroll_to,
        ..
    } = &mut devops.ci;
    let list = entries_from(github.as_ref(), local.ok());
    let selected = wf_selected.clone();
    let key = selected.clone().unwrap_or_default();
    kit::split_keyed(
        ui,
        "ci_wf",
        &key,
        actions,
        |ui, actions| {
            list_pane(
                ui,
                &list,
                local,
                wf_filter,
                selected.as_deref(),
                now,
                t,
                actions,
            )
        },
        |ui, actions| {
            widgets::fit_detail(ui);
            let Some(path) = selected.as_deref() else {
                overview(ui, &list, local, actions);
                return;
            };
            let Some(e) = list.iter().find(|e| e.path == path) else {
                kit::callout(
                    ui,
                    Tone::Warning,
                    "Workflow not found",
                    &format!("{path} is neither on GitHub nor in the working tree."),
                );
                return;
            };
            detail(
                ui,
                e,
                DetailCx {
                    history,
                    analysis,
                    dispatch,
                    branch: wf_branch,
                    applied: wf_branch_applied,
                    busy: e.id.is_some_and(|id| wf_busy.contains(&id)),
                    refs: &refs,
                    scroll: scroll_to,
                    current: current.as_deref(),
                    now,
                    t,
                },
                actions,
            );
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn list_pane(
    ui: &mut Ui,
    list: &[Entry],
    local: &Load<LocalInfo>,
    filter: &mut String,
    selected: Option<&str>,
    now: i64,
    t: f64,
    actions: &mut Vec<Action>,
) {
    let hint = format!("Filter {} workflows", list.len());
    ui.horizontal(|ui| {
        if !filter.is_empty() {
            ui.set_max_width(ui.available_width() - 8.0);
        }
        kit::search_box(ui, "ci_wf_filter", filter, &hint);
    });
    ui.add_space(space::S);
    let needle = filter.trim().to_lowercase();
    let shown: Vec<&Entry> = list.iter().filter(|e| matches(e, &needle)).collect();
    if local.loading && list.is_empty() {
        kit::loading(ui, "Reading the workflow files…");
        return;
    }
    if list.is_empty() {
        ui.add_space(space::S);
        kit::muted(
            ui,
            "No workflows: GitHub lists none for this repository and .github/workflows has no .yml files. Add a workflow file to get started.",
        );
        return;
    }
    if shown.is_empty() {
        kit::muted(ui, "No workflows match the filter.");
        return;
    }
    let _ = now;
    kit::list(
        ui,
        "ci_wf_list",
        shown.len(),
        kit::ROW_H,
        |i| selected == Some(shown[i].path.as_str()),
        |ui, rect, resp, i| {
            let e = shown[i];
            let p = *pal(ui);
            let mut badges: Vec<(String, Tone)> = Vec::new();
            if e.disabled() {
                badges.push((state_text(e.state.unwrap_or("")), Tone::Warning));
            }
            if e.id.is_none() {
                badges.push(("Not on GitHub".to_owned(), Tone::Accent));
            }
            if e.file.is_some_and(|f| f.dispatch.is_some()) {
                badges.push(("Manual".to_owned(), Tone::Neutral));
            }
            if e.file.is_some_and(|f| f.parse_error.is_some()) {
                badges.push(("YAML error".to_owned(), Tone::Danger));
            }
            if let Some(r) = e.last
                && r.is_failure()
            {
                badges.push(("Last run failed".to_owned(), Tone::Danger));
            }
            let meta = e
                .last
                .and_then(|r| r.started.or(r.created))
                .map(|s| format::relative(s, now))
                .unwrap_or_default();
            let dot = match e.last {
                Some(r) => widgets::outcome_color(&p, r.outcome()),
                None => Color32::TRANSPARENT,
            };
            kit::paint_row(
                ui,
                rect,
                &kit::Row::new(&e.name)
                    .leading(kit::Leading::Dot(dot))
                    .subtitle(&e.path)
                    .meta(&meta)
                    .badges(&badges)
                    .dimmed(e.disabled()),
                selected == Some(e.path.as_str()),
            );
            let _ = t;
            if resp.clicked() {
                actions.push(Act::SelectWorkflow(Some(e.path.clone())).into());
            }
        },
    );
}

fn overview(ui: &mut Ui, list: &[Entry], local: &Load<LocalInfo>, actions: &mut Vec<Action>) {
    kit::title(ui, "Workflows");
    kit::secondary(
        ui,
        "Every workflow GitHub knows for this repository, plus workflow files in the working tree that are not on GitHub yet. Pick one for its run history, duration trend, flaky runs and job analysis, or to run it by hand.",
    );
    let on_gh = list.iter().filter(|e| e.id.is_some()).count();
    let disabled = list.iter().filter(|e| e.disabled()).count();
    let local_only = list.iter().filter(|e| e.id.is_none()).count();
    let manual = list
        .iter()
        .filter(|e| e.file.is_some_and(|f| f.dispatch.is_some()))
        .count();
    let failing = list
        .iter()
        .filter(|e| e.last.is_some_and(|r| r.is_failure()))
        .count();
    kit::stats(
        ui,
        &[
            kit::Stat::new("On GitHub", format::count(on_gh)),
            kit::Stat::new("Disabled", format::count(disabled))
                .tone_if(disabled > 0, Tone::Warning),
            kit::Stat::new("Only in the working tree", format::count(local_only)),
            kit::Stat::new("Can be run by hand", format::count(manual)),
            kit::Stat::new("Last run failed", format::count(failing))
                .tone_if(failing > 0, Tone::Danger),
        ],
    );
    if let Some(e) = local.err() {
        kit::callout(ui, Tone::Danger, "Could not read the workflow files", e);
    }
    let files: Vec<&Entry> = list.iter().filter(|e| e.file.is_some()).collect();
    if !files.is_empty() {
        kit::section(ui, "Triggers", Some(files.len()));
        let rows: Vec<Vec<String>> = files
            .iter()
            .map(|e| {
                let f = e.file.unwrap();
                vec![
                    e.name.clone(),
                    f.triggers.join(", "),
                    format::count(f.jobs.len()),
                    if f.dispatch.is_some() {
                        format::count(f.dispatch.as_ref().map(Vec::len).unwrap_or(0))
                    } else {
                        "—".to_owned()
                    },
                ]
            })
            .collect();
        kit::table(
            ui,
            &[
                Col::text("Workflow"),
                Col::text("Triggers"),
                Col::num("Jobs"),
                Col::num("Inputs"),
            ],
            &rows,
        );
    }
    let _ = actions;
}

struct DetailCx<'a> {
    history: &'a std::collections::HashMap<(u64, String), Load<History>>,
    analysis: &'a std::collections::HashMap<(u64, String), Load<Analysis>>,
    dispatch: &'a mut Option<DispatchForm>,
    branch: &'a mut String,
    applied: &'a str,
    busy: bool,
    refs: &'a (Vec<String>, Vec<String>),
    scroll: &'a mut Option<String>,
    current: Option<&'a str>,
    now: i64,
    t: f64,
}

fn detail(ui: &mut Ui, e: &Entry, cx: DetailCx, actions: &mut Vec<Action>) {
    let dispatchable = e.file.is_some_and(|f| f.dispatch.is_some());
    widgets::header(ui, &e.name, |ui| {
        if let Some(id) = e.id {
            if cx.busy {
                ui.add(egui::Spinner::new().size(14.0));
            } else if e.disabled() {
                if IconButton::new(Icon::Play, "Enable")
                    .compact()
                    .show(ui)
                    .on_hover_text("PUT actions/workflows/{id}/enable")
                    .clicked()
                {
                    actions.push(Act::AskToggleWorkflow { id, enable: true }.into());
                }
            } else if !e.dynamic()
                && IconButton::new(Icon::Stop, "Disable")
                    .compact()
                    .show(ui)
                    .on_hover_text("PUT actions/workflows/{id}/disable")
                    .clicked()
            {
                actions.push(Act::AskToggleWorkflow { id, enable: false }.into());
            }
        }
        if dispatchable
            && cx.dispatch.is_none()
            && IconButton::new(Icon::Play, "Run workflow")
                .compact()
                .primary(true)
                .enabled(!e.disabled())
                .show(ui)
                .on_hover_text("workflow_dispatch: pick a ref and the inputs")
                .clicked()
        {
            actions.push(Act::OpenDispatch(e.path.clone()).into());
        }
    });
    let mut facts: Vec<(String, Tone)> = Vec::new();
    match e.state {
        Some(s) => facts.push((
            state_text(s),
            if e.disabled() {
                Tone::Warning
            } else {
                Tone::Success
            },
        )),
        None => facts.push(("Not on GitHub yet".to_owned(), Tone::Accent)),
    }
    if let Some(f) = e.file {
        for t in f.triggers.iter().take(8) {
            facts.push((t.clone(), Tone::Neutral));
        }
    }
    widgets::badges(ui, &facts);
    kit::kv(ui, 92.0, |kv| {
        kv.text("File", kit::mono(&e.path));
        if let Some(id) = e.id {
            kv.text("Workflow id", kit::mono(id.to_string()));
        }
        if let Some(f) = e.file {
            kv.text(
                "Jobs",
                format!(
                    "{}: {}",
                    format::count(f.jobs.len()),
                    f.jobs
                        .iter()
                        .map(|j| j.name.as_str())
                        .take(12)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
            if !f.schedules.is_empty() {
                kv.text("Schedule", kit::mono(f.schedules.join("   ")));
            }
            if let Some(err) = &f.parse_error {
                kv.text("YAML", err.as_str());
            }
        } else if e.dynamic() {
            kv.muted(
                "File",
                "a dynamic workflow GitHub runs itself (Dependabot, Copilot, CodeQL default setup)",
            );
        } else {
            kv.muted("Local file", "not in the working tree (another branch?)");
        }
    });
    if e.id.is_none() {
        kit::callout(
            ui,
            Tone::Accent,
            "Only in the working tree",
            "GitHub has no runs for this file yet: commit and push it. A workflow_dispatch workflow can only be run by hand once it is on the default branch.",
        );
    }
    if let Some(form) = cx.dispatch.as_mut().filter(|f| f.path == e.path) {
        dispatch_form(ui, form, cx.refs, cx.current, cx.now, actions);
    }
    let Some(id) = e.id else {
        return;
    };
    // History.
    let key = (id, cx.applied.to_owned());
    let load = cx.history.get(&key);
    kit::section_with(ui, "Recent runs", None, |ui| {
        let loading = load.is_some_and(|l| l.loading);
        if loading {
            ui.add(egui::Spinner::new().size(13.0));
        } else if IconButton::icon(Icon::Fetch)
            .compact()
            .show(ui)
            .on_hover_text("Reload this workflow's runs (1 API call)")
            .clicked()
        {
            actions.push(
                Act::LoadHistory {
                    workflow: id,
                    force: true,
                }
                .into(),
            );
        }
        if let Some(l) = load {
            widgets::note(ui, &widgets::freshness(l, cx.now));
        }
    });
    branch_filter(ui, cx.branch, cx.applied, actions);
    let Some(load) = load else {
        kit::loading(ui, "Loading the run history…");
        return;
    };
    let retry = Act::LoadHistory {
        workflow: id,
        force: true,
    }
    .into();
    let Some(h) = widgets::loaded(
        ui,
        load,
        "the run history",
        "Loading the last 100 runs…",
        retry,
        actions,
    ) else {
        return;
    };
    if h.runs.is_empty() {
        kit::muted(
            ui,
            if cx.applied.is_empty() {
                "This workflow has never run."
            } else {
                "No runs of this workflow on that branch. Clear the branch filter to see all runs."
            },
        );
        return;
    }
    history_sections(
        ui,
        id,
        h,
        cx.analysis.get(&key),
        cx.scroll,
        cx.now,
        cx.t,
        actions,
    );
}

fn branch_filter(ui: &mut Ui, branch: &mut String, applied: &str, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let resp = form::input(
            ui,
            "ci_wf_branch",
            branch,
            form::InputOpts::new("All branches").width(220.0),
        );
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let dirty = branch.trim() != applied;
        if (enter && dirty)
            || IconButton::text("Apply")
                .compact()
                .enabled(dirty)
                .show(ui)
                .clicked()
        {
            actions.push(Act::ApplyBranch.into());
        }
        if !applied.is_empty()
            && IconButton::text("All branches")
                .compact()
                .show(ui)
                .clicked()
        {
            branch.clear();
            actions.push(Act::ApplyBranch.into());
        }
        widgets::note(
            ui,
            &if applied.is_empty() {
                "every branch".to_owned()
            } else {
                format!("only {applied}")
            },
        );
    });
}

/// Scroll here when the script driver asked for this section.
fn anchor(ui: &mut Ui, scroll: &mut Option<String>, name: &str) {
    if scroll.as_deref() == Some(name) {
        ui.scroll_to_cursor(Some(egui::Align::TOP));
        *scroll = None;
    }
}

#[allow(clippy::too_many_arguments)]
fn history_sections(
    ui: &mut Ui,
    workflow: u64,
    h: &History,
    analysis: Option<&Load<Analysis>>,
    scroll: &mut Option<String>,
    now: i64,
    t: f64,
    actions: &mut Vec<Action>,
) {
    let s = history::stats(&h.runs);
    let dur = |x: Option<i64>| x.map(format::duration).unwrap_or_else(|| "—".to_owned());
    let ago = |x: Option<(u64, u64, i64)>| {
        x.map(|(_, n, at)| format!("#{n}, {}", format::relative(at, now)))
            .unwrap_or_else(|| "never".to_owned())
    };
    kit::stats(
        ui,
        &[
            kit::Stat::new(
                "Runs loaded",
                if h.total as usize > h.runs.len() {
                    format!("{} of {}", h.runs.len(), format::count(h.total as usize))
                } else {
                    format::count(h.runs.len())
                },
            ),
            kit::Stat::new(
                "Success rate",
                s.success_rate.map(pct).unwrap_or_else(|| "—".to_owned()),
            )
            .tone_if(s.success_rate.is_some_and(|r| r < 0.8), Tone::Danger)
            .tone_if(s.success_rate.is_some_and(|r| r >= 0.95), Tone::Success)
            .tip("Passed / (passed + failed); cancelled and skipped runs are left out"),
            kit::Stat::new("Median duration", dur(s.median_secs)),
            kit::Stat::new("p90 duration", dur(s.p90_secs))
                .tip("9 in 10 finished runs took this long or less"),
            kit::Stat::new("Median queue", dur(s.median_queue_secs))
                .tip("Time from the run being created to a runner picking it up"),
            kit::Stat::new("Last failure", ago(s.last_failure))
                .tone_if(s.last_failure.is_some(), Tone::Danger),
            kit::Stat::new("Last success", ago(s.last_success)),
        ],
    );
    widgets::note(
        ui,
        &format!(
            "{} passed · {} failed · {} cancelled · {} skipped · {} running",
            s.passed, s.failed, s.cancelled, s.skipped, s.running
        ),
    );

    kit::section(ui, "Duration trend", Some(h.runs.len()));
    let mut chrono: Vec<&WorkflowRun> = h.runs.iter().collect();
    chrono.sort_by_key(|r| (r.created.unwrap_or(0), r.run_number));
    widgets::duration_chart(ui, &chrono, now, actions);
    widgets::outcome_legend(ui);

    anchor(ui, scroll, "flaky");
    flaky_section(ui, h, now, t, actions);

    let rows = |groups: Vec<history::Group>| -> Vec<Vec<String>> {
        groups
            .into_iter()
            .take(12)
            .map(|g| {
                vec![
                    g.key,
                    format::count(g.runs),
                    format::count(g.passed),
                    format::count(g.failed),
                    g.success_rate.map(pct).unwrap_or_else(|| "—".to_owned()),
                    g.median_secs
                        .map(format::duration)
                        .unwrap_or_else(|| "—".to_owned()),
                ]
            })
            .collect()
    };
    let cols = |first: &'static str| {
        [
            Col::text(first),
            Col::num("Runs"),
            Col::num("Passed"),
            Col::num("Failed"),
            Col::num("Success"),
            Col::num("Median"),
        ]
    };
    anchor(ui, scroll, "branches");
    let branches = history::by_branch(&h.runs);
    kit::section(ui, "By branch", Some(branches.len()));
    kit::table(ui, &cols("Branch"), &rows(branches));
    let events = history::by_event(&h.runs);
    kit::section(ui, "By event", Some(events.len()));
    kit::table(ui, &cols("Event"), &rows(events));

    anchor(ui, scroll, "jobs");
    jobs_section(ui, workflow, h, analysis, actions);

    anchor(ui, scroll, "runs");
    kit::section(ui, "Latest runs", Some(h.runs.len().min(15)));
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for r in h.runs.iter().take(15) {
            let caption = format!("{}  ·  {}  ·  {}", r.branch, r.event, r.actor);
            if widgets::run_row(ui, r, &caption, now, t).clicked() {
                actions.push(Action::OpenRun {
                    run: r.id,
                    job: None,
                });
            }
        }
    });
}

fn flaky_section(ui: &mut Ui, h: &History, now: i64, t: f64, actions: &mut Vec<Action>) {
    let f = history::flaky(&h.runs);
    let n = f.passed_on_retry.len() + f.mixed_shas.len();
    kit::section(ui, "Flaky signs", Some(n));
    if n == 0 {
        kit::muted(
            ui,
            "None in these runs: no run needed a re-run to pass, and no commit both failed and passed.",
        );
        return;
    }
    kit::secondary(
        ui,
        "Found in the run list alone (no extra API calls). A run that passed on a later attempt, or a commit with both a failed and a passed run, usually points at a flaky test or an unreliable step.",
    );
    let by_id = |id: u64| h.runs.iter().find(|r| r.id == id);
    if !f.passed_on_retry.is_empty() {
        ui.add_space(4.0);
        kit::subtitle(
            ui,
            &format!(
                "Passed on a re-run ({})",
                format::count(f.passed_on_retry.len())
            ),
        );
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for id in f.passed_on_retry.iter().take(12) {
                if let Some(r) = by_id(*id) {
                    let cap = format!("attempt {}  ·  {}", r.attempt, r.branch);
                    if widgets::run_row(ui, r, &cap, now, t).clicked() {
                        actions.push(Action::OpenRun {
                            run: r.id,
                            job: None,
                        });
                    }
                }
            }
        });
    }
    if !f.mixed_shas.is_empty() {
        ui.add_space(4.0);
        kit::subtitle(
            ui,
            &format!(
                "Commits that failed and passed ({})",
                format::count(f.mixed_shas.len())
            ),
        );
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for m in f.mixed_shas.iter().take(12) {
                let Some(r) = m.failed.first().and_then(|id| by_id(*id)) else {
                    continue;
                };
                let cap = format!(
                    "{}  ·  {}  ·  {} failed, {} passed",
                    widgets::short(&m.sha),
                    m.branch,
                    m.failed.len(),
                    m.passed.len()
                );
                if widgets::run_row(ui, r, &cap, now, t).clicked() {
                    actions.push(Action::OpenRun {
                        run: r.id,
                        job: None,
                    });
                }
            }
        });
    }
}

fn jobs_section(
    ui: &mut Ui,
    workflow: u64,
    h: &History,
    analysis: Option<&Load<Analysis>>,
    actions: &mut Vec<Action>,
) {
    let picked = history::analysis_runs(&h.runs, ANALYSE_MAX_RUNS);
    let calls = history::analysis_calls(&picked);
    let loading = analysis.is_some_and(|a| a.loading);
    kit::section_with(ui, "Jobs", None, |ui| {
        if loading {
            ui.add(egui::Spinner::new().size(13.0));
        } else if !picked.is_empty()
            && IconButton::new(
                Icon::Chart,
                &if analysis.and_then(|a| a.ok()).is_some() {
                    format!("Analyse jobs again ({calls} API calls)")
                } else {
                    format!("Analyse jobs ({calls} API calls)")
                },
            )
            .compact()
            .show(ui)
            .on_hover_text(format!(
                "GET the jobs of every attempt of the newest {} finished runs",
                picked.len()
            ))
            .clicked()
        {
            actions.push(Act::AnalyseJobs { workflow }.into());
        }
    });
    let Some(a) = analysis else {
        kit::secondary(
            ui,
            &format!(
                "Per job: failure rate, how often it failed and then passed on a re-run, median duration and its slowest step, across the newest {} finished runs (every attempt). This takes {calls} API calls of your 5,000 per hour.",
                picked.len()
            ),
        );
        return;
    };
    if loading && a.data.is_none() {
        kit::loading(
            ui,
            &format!(
                "Fetching the jobs of {} ({calls} API calls)…",
                format::plural(picked.len(), "run", "runs")
            ),
        );
        return;
    }
    let Some(r) = a.ok() else {
        if let Some(e) = a.err() {
            kit::callout(ui, Tone::Danger, "Could not analyse the jobs", e);
        }
        return;
    };
    widgets::note(
        ui,
        &format!(
            "{} across {} and {} attempts · {} API calls",
            format::plural(r.jobs.len(), "job", "jobs"),
            format::plural(r.runs, "run", "runs"),
            r.attempts,
            r.calls
        ),
    );
    if !r.failed.is_empty() {
        kit::callout(
            ui,
            Tone::Warning,
            &format!(
                "{} could not be fetched",
                format::plural(r.failed.len(), "attempt", "attempts")
            ),
            &r.failed
                .iter()
                .take(5)
                .map(|(id, e)| format!("run {id}: {e}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    let rows: Vec<Vec<String>> = r
        .jobs
        .iter()
        .take(40)
        .map(|j| {
            vec![
                j.name.clone(),
                format::count(j.runs),
                format::count(j.attempts),
                j.failure_rate.map(pct).unwrap_or_else(|| "—".to_owned()),
                format::count(j.failed_then_passed),
                j.median_secs
                    .map(format::duration)
                    .unwrap_or_else(|| "—".to_owned()),
                j.slowest_step
                    .as_ref()
                    .map(|(n, m, _)| {
                        let n: String = if n.chars().count() > 44 {
                            format!("{}…", n.chars().take(43).collect::<String>())
                        } else {
                            n.clone()
                        };
                        format!("{n} ({})", format::duration(*m))
                    })
                    .unwrap_or_else(|| "—".to_owned()),
            ]
        })
        .collect();
    kit::table(
        ui,
        &[
            Col::text("Job"),
            Col::num("Runs"),
            Col::num("Attempts"),
            Col::num("Failure rate"),
            Col::num("Failed, then passed"),
            Col::num("Median"),
            Col::text("Slowest step (median)"),
        ],
        &rows,
    );
    let flaky: Vec<&history::JobStats> =
        r.jobs.iter().filter(|j| j.failed_then_passed > 0).collect();
    if !flaky.is_empty() {
        kit::callout(
            ui,
            Tone::Warning,
            &format!(
                "{} failed and then passed on a re-run",
                format::plural(flaky.len(), "job", "jobs")
            ),
            &flaky
                .iter()
                .map(|j| format!("{} ({}×)", j.name, j.failed_then_passed))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
}

fn dispatch_form(
    ui: &mut Ui,
    form: &mut DispatchForm,
    refs: &(Vec<String>, Vec<String>),
    current: Option<&str>,
    now: i64,
    actions: &mut Vec<Action>,
) {
    ui.add_space(space::M);
    form::card(ui, Tone::Accent, |ui| {
        kit::subtitle(ui, &format!("Run {}", form.name));
        kit::secondary(
            ui,
            &format!(
                "workflow_dispatch on GitHub with {}.{}",
                if form.inputs.is_empty() {
                    "no inputs".to_owned()
                } else {
                    format::plural(form.inputs.len(), "input", "inputs")
                },
                if form.workflow_id.is_none() {
                    " The workflow is not on GitHub yet, so the request names its file."
                } else {
                    ""
                }
            ),
        );
        ui.add_space(space::S);
        let (branches, tags) = refs;
        form::field(
            ui,
            "Branch or tag",
            Some(&match current {
                Some(c) if branches.iter().any(|b| b == c) => {
                    format!("The workflow file as it is on this ref runs. {c} is checked out here.")
                }
                Some(c) => format!(
                    "The workflow file as it is on this ref runs. {c} (checked out here) is not on the GitHub remote, so the default is the default branch."
                ),
                None => "The workflow file as it is on this ref runs.".to_owned(),
            }),
            |ui| {
                ui.horizontal(|ui| {
                    form::input(
                        ui,
                        "ci_dispatch_ref",
                        &mut form.git_ref,
                        form::InputOpts::new("main").width(260.0).mono(),
                    );
                    let mut options: Vec<(String, String)> =
                        vec![(String::new(), "Pick…".to_owned())];
                    options.extend(
                        branches
                            .iter()
                            .take(300)
                            .map(|b| (b.clone(), format!("branch  {b}"))),
                    );
                    options.extend(
                        tags.iter()
                            .rev()
                            .take(100)
                            .map(|t| (t.clone(), format!("tag  {t}"))),
                    );
                    let mut pick = String::new();
                    if form::select(ui, "ci_dispatch_ref_pick", &mut pick, &options, 200.0)
                        && !pick.is_empty()
                    {
                        form.git_ref = pick;
                    }
                });
            },
        );
        for input in form.inputs.clone() {
            let label = if input.required {
                format!("{}  (required)", input.name)
            } else {
                input.name.clone()
            };
            let mut help = input.description.clone();
            let kind = match input.kind {
                InputKind::Choice => format!("choice of {}", input.options.join(", ")),
                k => k.label().to_owned(),
            };
            if !help.is_empty() {
                help.push_str("  ·  ");
            }
            help.push_str(&kind);
            if let Some(d) = &input.default {
                help.push_str(&format!("  ·  default {d}"));
            }
            let value = form.values.entry(input.name.clone()).or_default();
            form::field(ui, &label, Some(&help), |ui| match input.kind {
                InputKind::Boolean => {
                    let mut b = value == "true";
                    if form::checkbox(ui, &mut b, if value == "true" { "true" } else { "false" })
                        .changed()
                    {
                        *value = b.to_string();
                    }
                }
                InputKind::Choice if !input.options.is_empty() => {
                    let options: Vec<(String, String)> = input
                        .options
                        .iter()
                        .map(|o| (o.clone(), o.clone()))
                        .collect();
                    form::select(
                        ui,
                        ("ci_dispatch_choice", &input.name),
                        value,
                        &options,
                        260.0,
                    );
                }
                _ => {
                    form::input(
                        ui,
                        ("ci_dispatch_input", &input.name),
                        value,
                        form::InputOpts::new(input.default.as_deref().unwrap_or(""))
                            .width(420.0)
                            .mono(),
                    );
                }
            });
        }
        if let Some(e) = &form.error {
            form::error(ui, e);
        }
        if let Some((r, at)) = &form.sent {
            let secs = at.elapsed().as_secs() as i64;
            kit::callout(
                ui,
                Tone::Success,
                "Dispatched",
                &format!(
                    "GitHub accepted the run on {r} {}. The run list refreshes a few seconds after the dispatch.",
                    format::relative(now - secs, now)
                ),
            );
        }
        form::actions_row(ui, |ui| {
            if form::primary(
                ui,
                if form.sending {
                    "Sending…"
                } else {
                    "Run workflow…"
                },
                !form.sending,
            )
            .clicked()
            {
                actions.push(Act::AskDispatch.into());
            }
            if form::button(ui, "Close", true).clicked() {
                actions.push(Act::CloseDispatch.into());
            }
        });
    });
    ui.add_space(space::S);
    let _ = vec2(0.0, 0.0);
}
