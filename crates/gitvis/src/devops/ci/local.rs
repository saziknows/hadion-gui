//! The Run locally tab: run a workflow job on this machine with `act`
//! (nektos/act, which needs Docker). Shows what is missing with install
//! hints, the jobs `act -l` lists (or the workflow files' jobs when act is
//! not installed), a form for event, inputs, runner image and a secrets
//! file (a path only), the exact command, and the run's output.

use egui::{Frame, Margin, Ui};

use gitvis_core::devops::ci::act::DEFAULT_IMAGE;
use gitvis_core::devops::ci::workflow::{InputKind, WorkflowFile};
use gitvis_core::devops::tools::Tool;

use super::control::act_run;
use super::widgets;
use super::{Act, ActState, Load, LocalInfo, Tools};
use crate::devops::console::Console;
use crate::format;
use crate::state::{Action, RepoState};
use crate::theme::{Tone, pal, space};
use crate::views::form;
use crate::views::icons::{Icon, IconButton};
use crate::views::kit;

/// A job the tab can run.
pub struct JobItem {
    pub workflow: String,
    pub job: String,
    pub name: String,
    pub events: Vec<String>,
    pub stage: Option<u32>,
}

/// The jobs the list shows, in order.
pub fn jobs(repo: &RepoState) -> Vec<JobItem> {
    let files = repo
        .devops
        .ci
        .local
        .ok()
        .map(|l| l.files.as_slice())
        .unwrap_or(&[]);
    items(&repo.devops.ci.act, files).0
}

/// Point the form at a job: its workflow, the job, and an event it has.
pub fn pick(act: &mut ActState, j: &JobItem) {
    act.workflow = j.workflow.clone();
    act.job = j.job.clone();
    if !j.events.contains(&act.event) {
        act.event = j
            .events
            .iter()
            .find(|e| *e != "schedule")
            .cloned()
            .unwrap_or_else(|| "push".to_owned());
    }
}

fn items(act: &ActState, files: &[WorkflowFile]) -> (Vec<JobItem>, bool) {
    if let Some(list) = act.list.ok() {
        let v = list
            .iter()
            .map(|j| JobItem {
                workflow: files
                    .iter()
                    .find(|f| f.file_name() == j.workflow_file)
                    .map(|f| f.path.clone())
                    .unwrap_or_else(|| format!(".github/workflows/{}", j.workflow_file)),
                job: j.job_id.clone(),
                name: j.job_name.clone(),
                events: j.events.clone(),
                stage: Some(j.stage),
            })
            .collect();
        return (v, true);
    }
    let v = files
        .iter()
        .flat_map(|f| {
            f.jobs.iter().map(move |j| JobItem {
                workflow: f.path.clone(),
                job: j.id.clone(),
                name: j.name.clone(),
                events: f.triggers.clone(),
                stage: None,
            })
        })
        .collect();
    (v, false)
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, console: &Console, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let repo_id = repo.id;
    let workdir = repo.workdir.clone();
    let ci = &mut repo.devops.ci;
    let files: Vec<WorkflowFile> = ci.local.ok().map(|l| l.files.clone()).unwrap_or_default();
    let local_load: &Load<LocalInfo> = &ci.local;
    let act = &mut ci.act;
    let tools = act.tools.ok().cloned();
    let (jobs, from_act) = items(act, &files);
    // Start on the first job so the form and the command are never empty.
    if act.workflow.is_empty()
        && let Some(j) = jobs.first()
    {
        pick(act, j);
    }
    // Left: tools and jobs.
    egui::Panel::left("ci_act_jobs")
        .resizable(true)
        .default_size(380.0)
        .size_range(300.0..=640.0)
        .frame(Frame::NONE.fill(p.panel).inner_margin(Margin {
            left: 10,
            right: 10,
            top: 12,
            bottom: 6,
        }))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            tools_section(ui, &act.tools, tools.as_ref(), actions);
            let can_list = tools.as_ref().is_some_and(|t| t.act.is_some());
            kit::section_with(ui, "Jobs", Some(jobs.len()), |ui| {
                if can_list
                    && !act.list.loading
                    && IconButton::icon(Icon::Fetch)
                        .compact()
                        .show(ui)
                        .on_hover_text("Run act -l again")
                        .clicked()
                {
                    actions.push(Act::ListActJobs.into());
                }
            });
            if from_act {
                widgets::note(ui, "From act -l");
            } else if local_load.loading {
                kit::loading(ui, "Reading the workflow files…");
            } else if act.list.loading {
                kit::loading(ui, "act -l…");
            } else {
                widgets::note(ui, "From the workflow files");
            }
            if let Some(e) = act.list.err() {
                kit::callout(ui, Tone::Danger, "act -l failed", e);
            }
            if jobs.is_empty() {
                kit::muted(
                    ui,
                    "No jobs: .github/workflows has no workflow files in this working tree.",
                );
                return;
            }
            let sel = (act.workflow.clone(), act.job.clone());
            let mut pick: Option<usize> = None;
            kit::list(
                ui,
                "ci_act_job_list",
                jobs.len(),
                kit::ROW_H,
                |i| sel.0 == jobs[i].workflow && sel.1 == jobs[i].job,
                |ui, rect, resp, i| {
                    let j = &jobs[i];
                    let file = j.workflow.rsplit('/').next().unwrap_or(&j.workflow);
                    let mut sub = format!("{file}  ·  {}", j.events.join(", "));
                    if let Some(s) = j.stage {
                        sub = format!("stage {s}  ·  {sub}");
                    }
                    let title = if j.name == j.job {
                        j.job.clone()
                    } else {
                        format!("{}  ({})", j.name, j.job)
                    };
                    kit::paint_row(
                        ui,
                        rect,
                        &kit::Row::new(&title).subtitle(&sub),
                        sel.0 == j.workflow && sel.1 == j.job,
                    );
                    if resp.clicked() {
                        pick = Some(i);
                    }
                },
            );
            if let Some(i) = pick {
                self::pick(act, &jobs[i]);
            }
        });
    // Bottom: the output of the shown run.
    let shown = act.shown.and_then(|id| console.get(id));
    if let Some(run) = shown {
        let avail = ui.available_height();
        egui::Panel::bottom("ci_act_output")
            .resizable(true)
            .default_size((avail * 0.55).max(220.0))
            .size_range(160.0..=(avail - 160.0).max(200.0))
            .frame(Frame::NONE.fill(if ui.visuals().dark_mode {
                p.chrome
            } else {
                p.canvas
            }))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.set_min_height(ui.available_height());
                crate::views::console::output(ui, run, "ci_act", actions);
            });
    }
    // Centre: the form.
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("ci_act_form")
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
                            ui.set_max_width(kit::DETAIL_MAX);
                            ui.spacing_mut().item_spacing.y = 8.0;
                            form_view(ui, act, &files, tools.as_ref(), &workdir, actions);
                            kit::section(ui, "Local runs", Some(act.runs.len()));
                            let runs = act.runs.clone();
                            if runs.is_empty() {
                                kit::muted(ui, "Nothing has run here yet.");
                            } else if let Some(id) = crate::views::console::run_list(
                                ui,
                                console,
                                repo_id,
                                act.shown,
                                |r| runs.contains(&r.id),
                            ) {
                                actions.push(Act::ShowActRun(Some(id)).into());
                            }
                        });
                });
        });
}

fn tools_section(
    ui: &mut Ui,
    load: &Load<Tools>,
    tools: Option<&Tools>,
    actions: &mut Vec<Action>,
) {
    kit::section_with(ui, "Tools", None, |ui| {
        if load.loading {
            ui.add(egui::Spinner::new().size(13.0));
        } else if IconButton::icon(Icon::Fetch)
            .compact()
            .show(ui)
            .on_hover_text("Look for act and Docker again")
            .clicked()
        {
            actions.push(Act::CheckTools.into());
        }
    });
    let Some(t) = tools else {
        if load.loading {
            kit::loading(ui, "Looking for act and Docker…");
        }
        return;
    };
    kit::kv(ui, 70.0, |kv| {
        match &t.act {
            Some(a) => kv.text("act", format!("{}  ·  {}", a.version, a.path.display())),
            None => kv.muted("act", "not installed"),
        }
        match (&t.docker, t.docker_running) {
            (Some(d), Some(true)) => kv.text("Docker", format!("{}  ·  running", d.version)),
            (Some(d), _) => kv.text("Docker", format!("{}  ·  not running", d.version)),
            (None, _) => kv.muted("Docker", "not installed"),
        }
    });
    if t.act.is_none()
        && widgets::tool_missing(
            ui,
            Tool::Act,
            "act runs a workflow job on this machine in Docker containers that mimic GitHub's runners, so a change can be tried before it is pushed.",
        )
    {
        actions.push(Act::CheckTools.into());
    }
    if t.docker.is_none() {
        if widgets::tool_missing(
            ui,
            Tool::Docker,
            "act starts every job in a Docker container.",
        ) {
            actions.push(Act::CheckTools.into());
        }
    } else if t.docker_running == Some(false) {
        let mut again = false;
        kit::callout_with(
            ui,
            Tone::Warning,
            "Docker is not running",
            "docker info did not answer. Start Docker Desktop (or the Docker service), then check again.",
            |ui| {
                again = IconButton::new(Icon::Fetch, "Check again")
                    .compact()
                    .show(ui)
                    .clicked();
            },
        );
        if again {
            actions.push(Act::CheckTools.into());
        }
    }
}

fn form_view(
    ui: &mut Ui,
    act: &mut ActState,
    files: &[WorkflowFile],
    tools: Option<&Tools>,
    workdir: &std::path::Path,
    actions: &mut Vec<Action>,
) {
    kit::title(ui, "Run a job locally");
    kit::secondary(
        ui,
        "act reads the workflow file, pulls a runner image and runs the job's steps in Docker on this machine. Nothing is sent to GitHub; secrets come from a dotenv file whose path is passed with --secret-file.",
    );
    ui.add_space(space::S);
    if files.is_empty() {
        kit::muted(ui, "There are no workflow files to run.");
        return;
    }
    let file = files.iter().find(|f| f.path == act.workflow);
    form::card(ui, Tone::Neutral, |ui| {
        let wf_options: Vec<(String, String)> = files
            .iter()
            .map(|f| (f.path.clone(), format!("{}  ({})", f.name, f.file_name())))
            .collect();
        form::field(ui, "Workflow", None, |ui| {
            if form::select(ui, "ci_act_wf", &mut act.workflow, &wf_options, 420.0) {
                act.job.clear();
                act.inputs.clear();
            }
        });
        if let Some(f) = file {
            let jobs: Vec<(String, String)> = f
                .jobs
                .iter()
                .map(|j| {
                    (
                        j.id.clone(),
                        if j.name == j.id {
                            j.id.clone()
                        } else {
                            format!("{}  ({})", j.name, j.id)
                        },
                    )
                })
                .collect();
            form::field(
                ui,
                "Job",
                Some("-j JOB: the job and the jobs it needs"),
                |ui| {
                    form::select(ui, "ci_act_job", &mut act.job, &jobs, 420.0);
                },
            );
            let mut events: Vec<String> = f
                .triggers
                .iter()
                .filter(|t| *t != "schedule")
                .cloned()
                .collect();
            for e in ["push", "pull_request", "workflow_dispatch"] {
                if !events.iter().any(|x| x == e) {
                    events.push(e.to_owned());
                }
            }
            let ev: Vec<(String, String)> = events.iter().map(|e| (e.clone(), e.clone())).collect();
            if act.event.is_empty() {
                act.event = events.first().cloned().unwrap_or_else(|| "push".to_owned());
            }
            form::field(ui, "Event", Some("The event act simulates"), |ui| {
                form::select(ui, "ci_act_event", &mut act.event, &ev, 260.0);
            });
            if act.event == "workflow_dispatch"
                && let Some(inputs) = f.dispatch.as_ref().filter(|i| !i.is_empty())
            {
                for i in inputs {
                    let v = act
                        .inputs
                        .entry(i.name.clone())
                        .or_insert_with(|| i.default.clone().unwrap_or_default());
                    let help = format!(
                        "--input {}=…  ·  {}{}",
                        i.name,
                        i.kind.label(),
                        if i.description.is_empty() {
                            String::new()
                        } else {
                            format!("  ·  {}", i.description)
                        }
                    );
                    form::field(ui, &i.name, Some(&help), |ui| match i.kind {
                        InputKind::Choice if !i.options.is_empty() => {
                            let o: Vec<(String, String)> =
                                i.options.iter().map(|x| (x.clone(), x.clone())).collect();
                            form::select(ui, ("ci_act_in", &i.name), v, &o, 260.0);
                        }
                        InputKind::Boolean => {
                            let mut b = v == "true";
                            if form::checkbox(
                                ui,
                                &mut b,
                                if v == "true" { "true" } else { "false" },
                            )
                            .changed()
                            {
                                *v = b.to_string();
                            }
                        }
                        _ => {
                            form::input(
                                ui,
                                ("ci_act_in", &i.name),
                                v,
                                form::InputOpts::new("").width(360.0).mono(),
                            );
                        }
                    });
                }
            }
        }
        form::field(
            ui,
            "Image for ubuntu-latest",
            Some(
                "-P ubuntu-latest=IMAGE. The medium catthehacker image has most tools GitHub's runner has.",
            ),
            |ui| {
                form::input(
                    ui,
                    "ci_act_image",
                    &mut act.image,
                    form::InputOpts::new(DEFAULT_IMAGE).width(420.0).mono(),
                );
            },
        );
        form::field(
            ui,
            "Secrets file",
            Some(
                "A dotenv file (NAME=value per line) passed with --secret-file. gitvis keeps only its path; the values never reach a command line, the log or the screen.",
            ),
            |ui| {
                ui.horizontal(|ui| {
                    match &act.secret_file {
                        Some(p) => {
                            ui.label(kit::mono(p.display().to_string()).color(pal(ui).text));
                            if IconButton::text("Clear").compact().show(ui).clicked() {
                                actions.push(Act::ClearSecretFile.into());
                            }
                        }
                        None => {
                            widgets::note(ui, "none");
                        }
                    }
                    if IconButton::new(Icon::Folder, "Pick…")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Act::PickSecretFile.into());
                    }
                });
            },
        );
    });
    // The command.
    let run = act_run(act, files);
    let spec = run.spec(workdir);
    kit::section(ui, "Command", None);
    kit::code_block(ui, &spec.display(), actions);
    let has_act = tools.is_some_and(|t| t.act.is_some());
    let docker_ok = tools.is_some_and(|t| t.docker.is_some() && t.docker_running != Some(false));
    let ready = !act.job.is_empty() && !act.workflow.is_empty();
    let reason = if tools.is_none() {
        Some("Looking for act and Docker…")
    } else if !has_act {
        Some("Install act to run jobs locally (see Tools).")
    } else if !docker_ok {
        Some("Docker must be installed and running.")
    } else if !ready {
        Some("Pick a job on the left or above.")
    } else {
        None
    };
    form::actions_row(ui, |ui| {
        if form::primary(ui, "Run with act", reason.is_none())
            .on_hover_text("Streams the output below; Stop kills act and its containers' processes")
            .clicked()
        {
            actions.push(Act::RunLocally.into());
        }
        if let Some(r) = reason {
            widgets::note(ui, r);
        }
    });
    let _ = format::count(0);
}
