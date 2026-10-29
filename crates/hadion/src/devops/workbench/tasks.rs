//! Tasks: what the repository can run (discovered in the background),
//! favourites, recently run, run history, and the selected task's latest
//! output from the console.

use std::collections::HashMap;
use std::time::Instant;

use egui::{Align2, FontId, Frame, RichText, Sense, Ui, pos2, vec2};
use hadion_core::devops::tools::{self, Installed, Tool};
use hadion_core::devops::workbench::shell::split_args;
use hadion_core::devops::workbench::tasks::{Discovery, Source, Task, discover};

use super::{Act, Msg, RunRecord, State, ui as wui};
use crate::devops::console::{Console, ConsoleRun};
use crate::devops::ctx::Ctx;
use crate::devops::pages::explain;
use crate::format;
use crate::state::{Action, RepoState, Settings};
use crate::theme::{pal, semibold, size};
use crate::views::form::{self, InputOpts};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Col, Row, Stat, Tone};

#[derive(Default)]
pub struct Tasks {
    pub discovery: Option<Result<Discovery, String>>,
    pub loading: bool,
    pub at: Option<Instant>,
    /// Task id → its program is installed.
    pub available: HashMap<String, bool>,
    pub tools: HashMap<Tool, Option<Installed>>,
    pub tools_loading: bool,
    pub filter: String,
    pub selected: Option<String>,
    /// Extra arguments typed per task.
    pub args: HashMap<String, String>,
    /// Console runs per task, oldest first.
    pub runs: HashMap<String, Vec<u64>>,
    /// Console run → (task, arguments).
    pub run_task: HashMap<u64, (String, String)>,
    /// Script commands that arrived before the list did.
    pub pending_select: Option<usize>,
    pub pending_run: Option<usize>,
}

impl Tasks {
    fn discovery(&self) -> Option<&Discovery> {
        self.discovery.as_ref().and_then(|d| d.as_ref().ok())
    }

    /// The latest console run of a task.
    pub fn latest_run<'a>(&self, console: &'a Console, id: &str) -> Option<&'a ConsoleRun> {
        self.runs
            .get(id)?
            .iter()
            .rev()
            .find_map(|r| console.get(*r))
    }
}

/// Tasks in list order (by project, then as found) that match `filter`.
pub fn visible<'a>(d: &'a Discovery, filter: &str) -> Vec<&'a Task> {
    let f = filter.trim().to_lowercase();
    let order: HashMap<&str, usize> = d
        .projects
        .iter()
        .enumerate()
        .map(|(i, p)| (p.dir.as_str(), i))
        .collect();
    let mut v: Vec<(usize, usize, &Task)> = d
        .tasks
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            f.is_empty()
                || t.name.to_lowercase().contains(&f)
                || t.command_line(&[]).to_lowercase().contains(&f)
                || t.dir.to_lowercase().contains(&f)
                || t.description.to_lowercase().contains(&f)
                || t.source.label().to_lowercase().contains(&f)
        })
        .map(|(i, t)| {
            (
                order.get(t.dir.as_str()).copied().unwrap_or(usize::MAX),
                i,
                t,
            )
        })
        .collect();
    v.sort_by_key(|(p, i, _)| (*p, *i));
    v.into_iter().map(|(_, _, t)| t).collect()
}

fn discover_now(cx: &mut Ctx, repo: &mut RepoState) {
    let t = &mut repo.devops.workbench.tasks;
    if t.loading {
        return;
    }
    t.loading = true;
    cx.read("Find tasks", |dir, log| {
        // "Check again" after installing something must look again.
        tools::refresh();
        log.command(format!(
            "scan {} and two levels below for package.json, Makefile, justfile, Taskfile, Cargo.toml, composer.json, deno.json, go.mod, *.sln, gradlew, pom.xml, pyproject.toml, compose.yaml",
            dir.display()
        ));
        let d = discover(dir);
        let available: Vec<(String, bool)> = d
            .tasks
            .iter()
            .map(|t| (t.id.clone(), t.available(dir)))
            .collect();
        Msg::Discovered(Ok(d), available)
    });
}

fn detect_tools(cx: &mut Ctx, repo: &mut RepoState, wanted: Vec<Tool>) {
    let t = &mut repo.devops.workbench.tasks;
    if t.tools_loading || wanted.is_empty() {
        return;
    }
    t.tools_loading = true;
    cx.read("Check task tools", move |_, log| {
        let mut out = Vec::new();
        for tool in wanted {
            log.command(format!("{} --version", tool.program()));
            out.push((tool, tools::detect(tool)));
        }
        Msg::Tools(out)
    });
}

/// Project files change; a list older than this is read again when the
/// page is shown.
const STALE_SECS: u64 = 60;

pub fn shown(cx: &mut Ctx, repo: &mut RepoState) {
    let t = &repo.devops.workbench.tasks;
    let stale = t.at.is_none_or(|at| at.elapsed().as_secs() > STALE_SECS);
    if (t.discovery.is_none() || stale) && !t.loading {
        discover_now(cx, repo);
    }
}

fn find_task(wb: &State, id: &str) -> Option<Task> {
    wb.tasks.discovery()?.task(id).cloned()
}

fn run_now(cx: &mut Ctx, repo: &mut RepoState, task: &Task, extra: Vec<String>) {
    let spec = task.spec(&repo.workdir, &extra);
    let mut title = task.command_line(&extra);
    if !task.dir.is_empty() {
        title.push_str(&format!("  ·  {}", task.dir));
    }
    let id = cx.console(title, spec);
    let wb = &mut repo.devops.workbench;
    wb.tasks.runs.entry(task.id.clone()).or_default().push(id);
    wb.tasks
        .run_task
        .insert(id, (task.id.clone(), extra.join(" ")));
    wb.tasks.selected = Some(task.id.clone());
    wb.touch_recent(&task.id);
}

fn run_task(cx: &mut Ctx, repo: &mut RepoState, id: &str) {
    let wb = &repo.devops.workbench;
    let Some(task) = find_task(wb, id) else {
        return;
    };
    let extra = split_args(wb.tasks.args.get(id).map(String::as_str).unwrap_or(""));
    if task.destructive {
        let line = task.command_line(&extra);
        crate::devops::pages::explain::confirm(
            cx,
            format!("Run {}?", task.name),
            format!(
                "{}. It runs in {}.",
                task.description,
                task.cwd(&repo.workdir).display()
            ),
            "Run",
            true,
            vec![line],
            Act::RunTaskConfirmed(task.id.clone(), extra).into(),
        );
        return;
    }
    run_now(cx, repo, &task, extra);
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::DiscoverTasks => discover_now(cx, repo),
        Act::SelectTask(id) => repo.devops.workbench.tasks.selected = id,
        Act::SelectTaskIndex(n) => {
            let t = &mut repo.devops.workbench.tasks;
            match t.discovery() {
                Some(d) => {
                    let id = visible(d, &t.filter).get(n).map(|t| t.id.clone());
                    if id.is_some() {
                        t.selected = id;
                    }
                }
                None => t.pending_select = Some(n),
            }
        }
        Act::RunTask(id) => run_task(cx, repo, &id),
        Act::RunTaskIndex(n) => {
            let t = &mut repo.devops.workbench.tasks;
            let id = match t.discovery() {
                Some(d) => visible(d, &t.filter).get(n).map(|t| t.id.clone()),
                None => {
                    t.pending_run = Some(n);
                    None
                }
            };
            if let Some(id) = id {
                run_task(cx, repo, &id);
            }
        }
        Act::RunTaskConfirmed(id, extra) => {
            if let Some(task) = find_task(&repo.devops.workbench, &id) {
                run_now(cx, repo, &task, extra);
            }
        }
        Act::ToggleFavourite(id) => {
            let f = &mut repo.devops.workbench.favourites;
            if let Some(pos) = f.iter().position(|x| *x == id) {
                f.remove(pos);
            } else {
                f.push(id);
            }
        }
        _ => {}
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    match msg {
        Msg::Discovered(result, available) => {
            let t = &mut repo.devops.workbench.tasks;
            t.loading = false;
            t.at = Some(Instant::now());
            t.available = available.into_iter().collect();
            let wanted = result.as_ref().map(|d| d.tools()).unwrap_or_default();
            if let Ok(d) = &result
                && let Some(sel) = &t.selected
                && d.task(sel).is_none()
            {
                t.selected = None;
            }
            t.discovery = Some(result);
            if let Some(n) = t.pending_select.take() {
                apply(cx, repo, Act::SelectTaskIndex(n));
            }
            if let Some(n) = repo.devops.workbench.tasks.pending_run.take() {
                apply(cx, repo, Act::RunTaskIndex(n));
            }
            repo.devops.workbench.tasks.tools.clear();
            detect_tools(cx, repo, wanted);
        }
        Msg::Tools(found) => {
            let t = &mut repo.devops.workbench.tasks;
            t.tools_loading = false;
            t.tools.extend(found);
        }
        _ => {}
    }
}

pub fn on_console_done(_cx: &mut Ctx, repo: &mut RepoState, run: &ConsoleRun) {
    let wb = &mut repo.devops.workbench;
    let Some((task, args)) = wb.tasks.run_task.get(&run.id).cloned() else {
        return;
    };
    wb.record(RunRecord {
        task,
        at: format::now_secs(),
        status: run.status_text(),
        ok: run.succeeded(),
        ms: run.elapsed_ms(),
        code: run.exit_code(),
        args,
    });
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

/// Status dot colour of a task: running, last run passed / failed, never.
fn status_tone(wb: &State, console: &Console, id: &str) -> Option<Tone> {
    if let Some(run) = wb.tasks.latest_run(console, id) {
        return Some(crate::views::console::status_tone(run));
    }
    wb.records
        .iter()
        .rev()
        .find(|r| r.task == id)
        .map(|r| if r.ok { Tone::Success } else { Tone::Danger })
}

pub fn show(
    ui: &mut Ui,
    repo: &mut RepoState,
    console: &Console,
    _settings: &Settings,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let now = format::now_secs();
    let workdir = repo.workdir.clone();
    let wb = &mut repo.devops.workbench;
    kit::page_header(ui, "Tasks", super::Tab::Tasks.hint(), |ui| {
        if IconButton::new(Icon::Rerun, "Scan again")
            .compact()
            .enabled(!wb.tasks.loading)
            .show(ui)
            .on_hover_text("Read the project files again and check which tools are installed")
            .clicked()
        {
            actions.push(Act::DiscoverTasks.into());
        }
        if let (Some(Ok(d)), Some(at)) = (&wb.tasks.discovery, wb.tasks.at) {
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!(
                    "{}  ·  {}  ·  {}  ·  {}",
                    format::plural(d.tasks.len(), "task", "tasks"),
                    format::plural(d.projects.len(), "project", "projects"),
                    format::millis(d.elapsed_ms),
                    format::relative(now - at.elapsed().as_secs() as i64, now)
                ))
                .size(size::SMALL)
                .color(p.text_2),
            );
        }
    });
    let discovery = match &wb.tasks.discovery {
        None => {
            wui::empty_pane(ui, |ui| {
                if wb.tasks.loading {
                    kit::empty_state(
                        ui,
                        Icon::Terminal,
                        "Looking for tasks",
                        "Reading package.json, Makefile, justfile, Taskfile, Cargo.toml and other project files in this folder and two levels below.",
                        None,
                    );
                } else if kit::empty_state(
                    ui,
                    Icon::Terminal,
                    "Tasks not read yet",
                    "Scan the repository for scripts, targets and commands.",
                    Some((Icon::Search, "Scan")),
                ) {
                    actions.push(Act::DiscoverTasks.into());
                }
            });
            return;
        }
        Some(Err(e)) => {
            let e = e.clone();
            wui::empty_pane(ui, |ui| {
                if wui::error_card(ui, "Could not read the tasks", &e, "Try again") {
                    actions.push(Act::DiscoverTasks.into());
                }
            });
            return;
        }
        Some(Ok(d)) => d,
    };
    if discovery.tasks.is_empty() {
        let problems: Vec<String> = discovery
            .problems
            .iter()
            .map(|p| format!("{}: {}", p.file, p.message))
            .collect();
        crate::devops::pages::layout::page(ui, "wb_tasks_none", |ui| {
            // A file that could not be read explains the empty list: it
            // comes first.
            for pr in &problems {
                kit::callout(ui, Tone::Warning, "Could not read a file", pr);
            }
            if kit::empty_state(
                ui,
                Icon::Terminal,
                "No tasks found",
                "Nothing in this folder or two levels below defines a task.\nAdd one, then scan again.",
                Some((Icon::Rerun, "Scan again")),
            ) {
                actions.push(Act::DiscoverTasks.into());
            }
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                crate::devops::pages::layout::cap_width(ui, 440.0);
                explain::details(ui, "wb_tasks_sources", "What Hadion looks for", false, |ui| {
                    explain::bullets(
                        ui,
                        &[
                            "package.json scripts",
                            "Makefile targets",
                            "justfile recipes",
                            "Taskfile tasks",
                            "Cargo, composer, deno, Go, .NET, Gradle, Maven and Python projects",
                            "docker compose files",
                        ],
                    );
                });
            });
        });
        return;
    }

    let selected = wb.tasks.selected.clone();
    let rows = visible(discovery, &wb.tasks.filter);
    let mut filter = wb.tasks.filter.clone();
    wui::list_pane(ui, "wb_tasks_list", 400.0, |ui| {
        task_list(ui, wb, discovery, &rows, &mut filter, console, actions);
    });
    let wb = &mut repo.devops.workbench;
    wb.tasks.filter = filter;
    let Some(id) = selected else {
        wui::detail_pane(ui, "wb_tasks_overview", |ui| {
            overview(ui, wb, console, actions);
        });
        return;
    };
    let Some(task) = wb.tasks.discovery().and_then(|d| d.task(&id)).cloned() else {
        wui::empty_pane(ui, |ui| {
            kit::muted(ui, "The task is gone; scan again.");
        });
        return;
    };
    if let Some(run) = wb.tasks.latest_run(console, &id) {
        let avail = ui.available_height();
        let fill = if ui.visuals().dark_mode {
            p.chrome
        } else {
            p.canvas
        };
        egui::Panel::bottom("wb_task_output")
            .resizable(true)
            .default_size((avail * 0.55).max(220.0))
            .size_range(190.0..=(avail - 140.0).max(190.0))
            .frame(Frame::NONE.fill(fill))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.set_min_height(ui.available_height());
                crate::views::console::output(ui, run, "wb_task", actions);
            });
    }
    wui::detail_pane(ui, ("wb_task", &id), |ui| {
        detail(ui, wb, &task, &workdir, console, now, actions);
    });
}

fn group_label(d: &Discovery, dir: &str, repo_name: &str) -> (String, String) {
    let p = d.project(dir);
    let name = p.map(|p| p.name.clone()).unwrap_or_default();
    let role = match p {
        Some(p) if p.workspace => "workspace root",
        Some(p) if p.member_of.is_some() => "workspace member",
        _ => "",
    };
    let join = |a: &str, b: &str| match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_owned(),
        (_, true) => a.to_owned(),
        _ => format!("{a}  ·  {b}"),
    };
    if dir.is_empty() {
        let n = if name.is_empty() {
            repo_name.to_owned()
        } else {
            name
        };
        (n, join("repository root", role))
    } else if name.is_empty() || name == dir.rsplit('/').next().unwrap_or(dir) {
        (dir.to_owned(), role.to_owned())
    } else {
        (name, join(dir, role))
    }
}

enum Item<'a> {
    Header(String, String, usize),
    Task(&'a Task, bool),
}

fn task_list(
    ui: &mut Ui,
    wb: &State,
    d: &Discovery,
    rows: &[&Task],
    filter: &mut String,
    console: &Console,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let hint = format!("Filter {} tasks", format::count(d.tasks.len()));
    wui::search_field(ui, "wb_task_filter", filter, &hint);
    ui.add_space(6.0);
    if rows.is_empty() {
        ui.add_space(8.0);
        kit::muted(ui, "No task matches the filter.");
        return;
    }
    // Favourites and recently run first, then by project.
    let mut items: Vec<Item> = Vec::new();
    if filter.trim().is_empty() {
        let fav: Vec<&Task> = wb.favourites.iter().filter_map(|id| d.task(id)).collect();
        if !fav.is_empty() {
            items.push(Item::Header("Favourites".into(), String::new(), fav.len()));
            items.extend(fav.into_iter().map(|t| Item::Task(t, true)));
        }
        let recent: Vec<&Task> = wb
            .recent
            .iter()
            .filter(|id| !wb.favourites.contains(id))
            .filter_map(|id| d.task(id))
            .take(5)
            .collect();
        if !recent.is_empty() {
            items.push(Item::Header(
                "Recently run".into(),
                String::new(),
                recent.len(),
            ));
            items.extend(recent.into_iter().map(|t| Item::Task(t, true)));
        }
    }
    let root_name = d
        .root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut last_dir: Option<&str> = None;
    for t in rows {
        if last_dir != Some(t.dir.as_str()) {
            let n = rows.iter().filter(|x| x.dir == t.dir).count();
            let (title, sub) = group_label(d, &t.dir, &root_name);
            items.push(Item::Header(title, sub, n));
            last_dir = Some(t.dir.as_str());
        }
        items.push(Item::Task(t, false));
    }
    let selected = wb.tasks.selected.as_deref();
    egui::ScrollArea::vertical()
        .id_salt("wb_task_rows")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, item) in items.iter().enumerate() {
                match item {
                    Item::Header(title, sub, n) => {
                        let (rect, _) = ui.allocate_exact_size(
                            vec2(ui.available_width(), if i == 0 { 28.0 } else { 36.0 }),
                            Sense::hover(),
                        );
                        if !ui.is_rect_visible(rect) {
                            continue;
                        }
                        let y = rect.bottom() - 11.0;
                        let painter = ui.painter();
                        let r = painter.text(
                            pos2(rect.left() + 10.0, y),
                            Align2::LEFT_CENTER,
                            title,
                            FontId::new(size::SMALL, semibold()),
                            p.text_strong,
                        );
                        if !sub.is_empty() {
                            painter.text(
                                pos2(r.right() + 8.0, y),
                                Align2::LEFT_CENTER,
                                sub,
                                FontId::proportional(size::SMALL),
                                p.text_3,
                            );
                        }
                        painter.text(
                            pos2(rect.right() - 10.0, y),
                            Align2::RIGHT_CENTER,
                            format::count(*n),
                            FontId::proportional(size::SMALL),
                            p.text_3,
                        );
                    }
                    Item::Task(t, pinned) => {
                        let (rect, resp) = ui.allocate_exact_size(
                            vec2(ui.available_width(), kit::ROW_H),
                            Sense::click(),
                        );
                        if !ui.is_rect_visible(rect) {
                            continue;
                        }
                        let on = selected == Some(t.id.as_str());
                        kit::row_background(ui, rect, resp.hovered(), on);
                        let dot = match status_tone(wb, console, &t.id) {
                            Some(tone) => tone.color(&p),
                            None => p.border_strong,
                        };
                        let mut badges: Vec<(String, Tone)> = Vec::new();
                        let running = wb
                            .tasks
                            .latest_run(console, &t.id)
                            .is_some_and(|r| r.running());
                        if running {
                            badges.push(("Running".into(), Tone::Accent));
                        }
                        if wb.tasks.available.get(&t.id) == Some(&false) {
                            badges.push((format!("No {}", t.program_name()), Tone::Warning));
                        }
                        let command = t.command_line(&[]);
                        let meta = if *pinned && !t.dir.is_empty() {
                            format!("{}  ·  {}", t.source.label(), t.dir)
                        } else {
                            t.source.label().to_owned()
                        };
                        kit::paint_row(
                            ui,
                            rect,
                            &Row::new(&t.name)
                                .leading(kit::Leading::Dot(dot))
                                .subtitle(&command)
                                .meta(&meta)
                                .badges(&crate::devops::pages::layout::fit_badges(
                                    ui,
                                    rect.width(),
                                    &badges,
                                )),
                            on,
                        );
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if resp.clicked() {
                            actions.push(Act::SelectTask(Some(t.id.clone())).into());
                        }
                        if resp.double_clicked() {
                            actions.push(Act::RunTask(t.id.clone()).into());
                        }
                        resp.context_menu(|ui| {
                            if ui.button("Run").clicked() {
                                actions.push(Act::RunTask(t.id.clone()).into());
                                ui.close();
                            }
                            let fav = wb.favourites.contains(&t.id);
                            if ui
                                .button(if fav {
                                    "Remove from favourites"
                                } else {
                                    "Add to favourites"
                                })
                                .clicked()
                            {
                                actions.push(Act::ToggleFavourite(t.id.clone()).into());
                                ui.close();
                            }
                            if ui.button("Copy command").clicked() {
                                actions.push(Action::Copy(command.clone()));
                                ui.close();
                            }
                        });
                    }
                }
            }
        });
}

/// Name of the program for the "No make" badge.
trait ProgramName {
    fn program_name(&self) -> String;
}

impl ProgramName for Task {
    fn program_name(&self) -> String {
        std::path::Path::new(&self.program)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.program.clone())
    }
}

fn why_tool(source: Source) -> &'static str {
    match source {
        Source::Npm => "It runs the package.json scripts.",
        Source::Make => "It runs the Makefile targets.",
        Source::Just => "It runs the justfile recipes.",
        Source::Taskfile => "It runs the Taskfile tasks.",
        Source::Cargo => "It builds, tests and runs Rust packages.",
        Source::Composer => "It runs the composer.json scripts.",
        Source::Deno => "It runs the deno.json tasks.",
        Source::Go => "It builds and tests Go modules.",
        Source::Dotnet => "It builds and tests .NET solutions and projects.",
        Source::Gradle => "It builds Gradle projects.",
        Source::Maven => "It builds Maven projects.",
        Source::Python => "It runs the Python entry points, tasks and tests.",
        Source::Compose => "It starts and stops the compose services.",
    }
}

#[allow(clippy::too_many_arguments)]
fn detail(
    ui: &mut Ui,
    wb: &mut State,
    task: &Task,
    workdir: &std::path::Path,
    console: &Console,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let id = task.id.clone();
    let available = wb.tasks.available.get(&id).copied().unwrap_or(true);
    let fav = wb.favourites.contains(&id);
    kit::title(ui, &task.name);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        kit::badge(ui, task.source.label(), Tone::Neutral);
        if !task.dir.is_empty() {
            kit::badge(ui, &task.dir, Tone::Neutral);
        }
        if fav {
            kit::badge(ui, "Favourite", Tone::Accent);
        }
        if task.destructive {
            kit::badge(ui, "Asks first", Tone::Warning);
        }
        if let Some(run) = wb.tasks.latest_run(console, &id) {
            kit::badge(
                ui,
                &run.status_text(),
                crate::views::console::status_tone(run),
            );
        }
    });
    ui.add_space(4.0);
    let extra = split_args(wb.tasks.args.get(&id).map(String::as_str).unwrap_or(""));
    let command = task.command_line(&extra);
    kit::kv(ui, 120.0, |kv| {
        kv.row("Command", |ui| wui::copyable(ui, &command, actions));
        kv.text(
            "Runs in",
            kit::mono(task.cwd(workdir).display().to_string()),
        );
        kv.row("Source", |ui| {
            let text = match task.line {
                Some(l) => format!("{}, line {l}", task.file),
                None => task.file.clone(),
            };
            if kit::link(ui, &text).clicked() {
                actions.push(Action::OpenPath(workdir.join(&task.file)));
            }
        });
        if task.description.is_empty() {
            kv.muted(
                "Description",
                match task.source {
                    Source::Make => "None in the Makefile: add a ## comment above the target or after its prerequisites.",
                    Source::Just => "None in the justfile: add a # comment above the recipe.",
                    Source::Taskfile => "None: add desc: to the task.",
                    _ => "None",
                },
            );
        } else {
            kv.text("Description", task.description.as_str());
        }
        if !task.params.is_empty() {
            kv.text(
                "Parameters",
                format!("{} (type the values below)", task.params.join(", ")),
            );
        }
        if let Some(tool) = task.tool {
            match wb.tasks.tools.get(&tool) {
                Some(Some(inst)) => kv.text(
                    "Tool",
                    format!("{}  ·  {}", inst.version, inst.path.display()),
                ),
                Some(None) => kv.muted("Tool", &format!("{} not found on PATH", tool.name())),
                None if wb.tasks.tools_loading => kv.muted("Tool", "Checking…"),
                None => kv.muted("Tool", tool.name()),
            }
        }
    });
    if !available && wui::missing_tool(ui, task.tool, &task.program, why_tool(task.source)) {
        actions.push(Act::DiscoverTasks.into());
    }
    ui.add_space(6.0);
    let buf = wb.tasks.args.entry(id.clone()).or_default();
    let hint = if task.params.is_empty() {
        "Extra arguments, e.g. --watch (optional)".to_owned()
    } else {
        format!("Values for {}", task.params.join(" "))
    };
    let resp = form::input(ui, ("wb_task_args", &id), buf, InputOpts::new(&hint).mono());
    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if task.args_separator {
        kit::muted(ui, "Arguments go after -- so they reach the script.");
    }
    let running = wb
        .tasks
        .latest_run(console, &id)
        .is_some_and(|r| r.running());
    wui::buttons(ui, |ui| {
        let run = IconButton::new(Icon::Play, if running { "Run again" } else { "Run" })
            .primary(true)
            .enabled(available)
            .show(ui);
        let run = if available {
            run.on_hover_text(&command)
        } else {
            run.on_hover_text("Install the tool first")
        };
        if run.clicked() || (enter && available) {
            actions.push(Act::RunTask(id.clone()).into());
        }
        if IconButton::new(
            Icon::Star,
            if fav {
                "Favourite"
            } else {
                "Add to favourites"
            },
        )
        .selected(fav)
        .show(ui)
        .on_hover_text(if fav {
            "Remove from favourites"
        } else {
            "Keep it at the top of the list"
        })
        .clicked()
        {
            actions.push(Act::ToggleFavourite(id.clone()).into());
        }
        if IconButton::new(Icon::Copy, "Copy command")
            .show(ui)
            .clicked()
        {
            actions.push(Action::Copy(command.clone()));
        }
        if IconButton::new(Icon::External, "Open file")
            .show(ui)
            .clicked()
        {
            actions.push(Action::OpenPath(workdir.join(&task.file)));
        }
    });
    if let Some(body) = &task.body {
        kit::section(ui, "What it runs", None);
        kit::code_block(ui, body, actions);
    }

    // Run history: this session's console runs and earlier sessions'.
    let records: Vec<&RunRecord> = wb.records.iter().rev().filter(|r| r.task == id).collect();
    kit::section(ui, "Run history", Some(records.len()));
    if records.is_empty() {
        kit::secondary(
            ui,
            "Not run from Hadion yet. Runs are kept with their status, duration and exit code.",
        );
    } else {
        let ok = records.iter().filter(|r| r.ok).count();
        let avg = records.iter().map(|r| r.ms).sum::<u64>() / records.len() as u64;
        kit::secondary(
            ui,
            &format!(
                "{} of {} passed  ·  {} on average",
                ok,
                records.len(),
                format::millis(avg)
            ),
        );
        let rows: Vec<Vec<String>> = records
            .iter()
            .take(15)
            .map(|r| {
                vec![
                    format!(
                        "{}  ·  {}",
                        format::absolute(r.at),
                        format::relative(r.at, now)
                    ),
                    r.status.clone(),
                    format::millis(r.ms),
                    r.code.map(|c| c.to_string()).unwrap_or_else(|| "–".into()),
                    if r.args.is_empty() {
                        "–".into()
                    } else {
                        r.args.clone()
                    },
                ]
            })
            .collect();
        crate::devops::pages::layout::wide(ui, "wb_task_runs", 600.0, |ui| {
            kit::table(
                ui,
                &[
                    Col::text("When"),
                    Col::text("Status"),
                    Col::num("Duration"),
                    Col::num("Exit code"),
                    Col::text("Arguments"),
                ],
                &rows,
            );
        });
        if records.len() > 15 {
            kit::muted(ui, &format!("and {} older runs", records.len() - 15));
        }
    }
}

fn overview(ui: &mut Ui, wb: &State, console: &Console, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let Some(d) = wb.tasks.discovery() else {
        return;
    };
    kit::title(ui, "What this repository can run");
    kit::secondary(
        ui,
        "Select a task to see its exact command, where it comes from and its output.\nDouble-click runs it; extra arguments can be typed before running.",
    );
    ui.add_space(4.0);
    let mut sources: Vec<(Source, usize)> = Vec::new();
    for t in &d.tasks {
        match sources.iter_mut().find(|(s, _)| *s == t.source) {
            Some((_, n)) => *n += 1,
            None => sources.push((t.source, 1)),
        }
    }
    let missing: Vec<&Task> = d
        .tasks
        .iter()
        .filter(|t| wb.tasks.available.get(&t.id) == Some(&false))
        .collect();
    let session_runs = wb
        .tasks
        .run_task
        .keys()
        .filter(|id| console.get(**id).is_some())
        .count();
    let tiles = [
        Stat::new("Tasks", format::count(d.tasks.len())),
        Stat::new("Projects", format::count(d.projects.len())),
        Stat::new("Kinds", format::count(sources.len())),
        Stat::new("Favourites", format::count(wb.favourites.len())),
        Stat::new("Runs this session", format::count(session_runs)),
        Stat::new("Need a tool", format::count(missing.len()))
            .tone_if(!missing.is_empty(), Tone::Warning),
    ];
    crate::views::insights::stats(ui, &tiles);
    for pr in &d.problems {
        kit::callout(
            ui,
            Tone::Warning,
            &format!("Could not read {}", pr.file),
            &pr.message,
        );
    }
    // One card per missing program.
    let mut seen: Vec<String> = Vec::new();
    for t in &missing {
        if seen.contains(&t.program) {
            continue;
        }
        seen.push(t.program.clone());
        let n = missing.iter().filter(|x| x.program == t.program).count();
        if wui::missing_tool(
            ui,
            t.tool,
            &t.program,
            &format!(
                "{} {} {} it.",
                why_tool(t.source),
                n,
                if n == 1 { "task needs" } else { "tasks need" }
            ),
        ) {
            actions.push(Act::DiscoverTasks.into());
        }
    }
    kit::section(ui, "By kind", Some(sources.len()));
    let bars: Vec<(String, u32)> = sources
        .iter()
        .map(|(s, n)| (s.label().to_owned(), *n as u32))
        .collect();
    kit::count_bars(ui, &bars, p.accent, 20);

    kit::section(ui, "Projects", Some(d.projects.len()));
    let rows: Vec<Vec<String>> = d
        .projects
        .iter()
        .map(|pr| {
            vec![
                if pr.dir.is_empty() {
                    "(root)".to_owned()
                } else {
                    pr.dir.clone()
                },
                pr.name.clone(),
                pr.sources
                    .iter()
                    .map(|s| s.label())
                    .collect::<Vec<_>>()
                    .join(", "),
                pr.package_manager
                    .as_ref()
                    .map(|(pm, why)| format!("{} ({why})", pm.program()))
                    .unwrap_or_else(|| "–".into()),
                match (&pr.member_of, pr.workspace) {
                    (_, true) => "Workspace root".to_owned(),
                    (Some(root), _) if root.is_empty() => "Member of the root".to_owned(),
                    (Some(root), _) => format!("Member of {root}"),
                    _ => "–".to_owned(),
                },
                format::count(d.tasks.iter().filter(|t| t.dir == pr.dir).count()),
            ]
        })
        .collect();
    crate::devops::pages::layout::wide(ui, "wb_tools_installed", 600.0, |ui| {
        kit::table(
            ui,
            &[
                Col::text("Folder"),
                Col::text("Name"),
                Col::text("Kinds"),
                Col::text("Package manager"),
                Col::text("Workspace"),
                Col::num("Tasks"),
            ],
            &rows,
        );
    });
    if !wb.tasks.tools.is_empty() {
        kit::section(ui, "Tools", Some(wb.tasks.tools.len()));
        let mut tools: Vec<(&Tool, &Option<Installed>)> = wb.tasks.tools.iter().collect();
        tools.sort_by_key(|(t, _)| t.name());
        let rows: Vec<Vec<String>> = tools
            .iter()
            .map(|(t, inst)| match inst {
                Some(i) => vec![
                    t.name().to_owned(),
                    i.version.clone(),
                    i.path.display().to_string(),
                ],
                None => vec![
                    t.name().to_owned(),
                    "Not installed".to_owned(),
                    t.install_hint().to_owned(),
                ],
            })
            .collect();
        crate::devops::pages::layout::wide(ui, "wb_tools_table", 600.0, |ui| {
            kit::table(
                ui,
                &[
                    Col::text("Tool"),
                    Col::text("Version"),
                    Col::text("Path or how to install"),
                ],
                &rows,
            );
        });
    }
    ui.add_space(6.0);
    kit::secondary(
        ui,
        &format!(
            "Scanned {}, read {} in {}.",
            format::plural(d.dirs_scanned, "folder", "folders"),
            format::plural(d.files_read, "file", "files"),
            format::millis(d.elapsed_ms)
        ),
    );
    kit::secondary(
        ui,
        "The root and two levels below; node_modules, target, build output and hidden folders are skipped.",
    );
}
