//! Bisect: start (bad, good, optional test command), mark the current
//! commit good / bad / skip, or let `git bisect run` decide in the
//! console; the log of tested commits, the remaining-steps estimate and
//! the first bad commit.

use std::path::PathBuf;
use std::time::Instant;

use egui::{RichText, Ui, vec2};
use hadion_core::Oid;
use hadion_core::devops::workbench::bisect::{
    self as core, BisectInfo, CommitInfo, Shell, Verdict,
};
use hadion_core::devops::workbench::shell::find_sh;
use hadion_core::devops::workbench::{git_line, git_raw, output_text};

use super::{Act, Msg, ui as wui};
use crate::devops::console::{Console, ConsoleRun};
use crate::devops::ctx::Ctx;
use crate::devops::pages::explain;
use crate::format;
use crate::state::{Action, RepoState, Selection};
use crate::theme::{pal, size};
use crate::views::form::{self, InputOpts};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Col, Stat, Tone};

#[derive(Default)]
pub struct Bisect {
    pub info: Option<Result<BisectInfo, String>>,
    pub loading: bool,
    pub at: Option<Instant>,
    pub bad: String,
    pub good: String,
    pub test_cmd: String,
    /// Run the test command with `cmd /C` instead of sh.
    pub cmd_shell: bool,
    /// Git for Windows' sh (`None` inside when not found).
    pub sh: Option<Option<PathBuf>>,
    /// A start / mark / reset is running.
    pub busy: bool,
    pub last_output: Option<Result<String, String>>,
    /// The console run of `git bisect run`.
    pub run_id: Option<u64>,
    /// Start this run once the bisection has started.
    pub pending_run: Option<(String, bool)>,
}

impl Bisect {
    fn shell(&self) -> Shell {
        match (&self.sh, self.cmd_shell) {
            (Some(Some(p)), false) => Shell::Sh(p.clone()),
            _ if !cfg!(windows) => Shell::Sh(PathBuf::from("/bin/sh")),
            _ => Shell::Cmd,
        }
    }
}

fn refresh(cx: &mut Ctx, repo: &mut RepoState) {
    let b = &mut repo.devops.workbench.bisect;
    if b.loading {
        return;
    }
    b.loading = true;
    cx.read("Read bisect state", move |dir, log| {
        let mut cmds = Vec::new();
        let r = core::read(dir, &mut cmds);
        log.commands.extend(cmds);
        Msg::Bisect(Box::new(r))
    });
}

pub fn shown(cx: &mut Ctx, repo: &mut RepoState) {
    let b = &mut repo.devops.workbench.bisect;
    if b.sh.is_none() {
        b.sh = Some(find_sh());
        if b.sh == Some(None) {
            b.cmd_shell = true;
        }
    }
    refresh(cx, repo);
}

/// Run a bisect subcommand as a change; its output comes back.
fn change(cx: &mut Ctx, repo: &mut RepoState, label: String, args: Vec<String>) {
    let b = &mut repo.devops.workbench.bisect;
    if b.busy {
        return;
    }
    b.busy = true;
    cx.local_change(label, move |dir, log| {
        let mut cmds = Vec::new();
        let r = git_raw(dir, &args, &[], &mut cmds);
        log.commands.extend(cmds);
        let res = match r {
            Ok(out) if out.success => Ok(output_text(&out)),
            Ok(out) => Err(output_text(&out)),
            Err(e) => Err(e),
        };
        let summary = match &res {
            Ok(text) => Ok(step_summary(text)),
            Err(e) => Err(e.clone()),
        };
        (summary, Some(Msg::BisectChanged(res)))
    });
}

/// `good` → `Good` (badges are sentence case).
fn sentence_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// A one-line summary of what a bisect step (or run) printed.
fn step_summary(text: &str) -> String {
    if let Some(sha) = core::first_bad_in(text) {
        return format!("Found the first bad commit: {}", &sha[..10.min(sha.len())]);
    }
    match core::parse_step(text) {
        core::StepResult::Next {
            left,
            steps,
            subject,
            ..
        } => format!(
            "Now testing \"{subject}\": {left} revision{} left after this one, about {steps} more step{}",
            if left == 1 { "" } else { "s" },
            if steps == 1 { "" } else { "s" }
        ),
        core::StepResult::FirstBad { sha } => {
            format!("Found the first bad commit: {}", &sha[..10.min(sha.len())])
        }
        core::StepResult::OnlySkipped => {
            "Only skipped commits are left: the culprit is one of them".to_owned()
        }
        core::StepResult::Waiting => "Waiting for a good and a bad commit".to_owned(),
        core::StepResult::Other => text.lines().next().unwrap_or("Done").to_owned(),
    }
}

fn run_line(shell: &Shell, cmd: &str) -> String {
    match shell {
        Shell::Sh(p) => git_line(&[
            "bisect".to_owned(),
            "run".to_owned(),
            p.to_string_lossy().into_owned(),
            "-c".to_owned(),
            cmd.to_owned(),
        ]),
        Shell::Cmd => "git bisect run cmd /C <script>".to_owned(),
    }
}

fn start_run(cx: &mut Ctx, repo: &mut RepoState, cmd: String, cmd_shell: bool) {
    let b = &mut repo.devops.workbench.bisect;
    b.cmd_shell = cmd_shell;
    let shell = b.shell();
    let target = match &shell {
        Shell::Sh(_) => cmd.clone(),
        Shell::Cmd => match core::write_cmd_script(&cmd) {
            Ok(p) => p.to_string_lossy().into_owned(),
            Err(e) => {
                cx.toast(crate::state::ToastKind::Error, e);
                return;
            }
        },
    };
    let spec = core::run_spec(&repo.workdir, &shell, &target);
    let id = cx.console(format!("git bisect run  ·  {cmd}"), spec);
    let b = &mut repo.devops.workbench.bisect;
    b.run_id = Some(id);
    b.test_cmd = cmd;
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::RefreshBisect => refresh(cx, repo),
        Act::StartBisect => {
            let b = &repo.devops.workbench.bisect;
            let bad = if b.bad.trim().is_empty() {
                "HEAD".to_owned()
            } else {
                b.bad.trim().to_owned()
            };
            let goods: Vec<String> = b
                .good
                .split([' ', ','])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
            if goods.is_empty() {
                cx.toast(
                    crate::state::ToastKind::Error,
                    "Enter a good commit: one where the problem did not exist yet.",
                );
                return;
            }
            let cmd = b.test_cmd.trim().to_owned();
            let mut details = vec![git_line(&core::start_args(&bad, &goods))];
            let shell = b.shell();
            if !cmd.is_empty() {
                details.push(run_line(&shell, &cmd));
                if shell == Shell::Cmd {
                    details.push(format!("# <script>.cmd\n{}", core::cmd_script_text(&cmd)));
                }
            }
            let body = if cmd.is_empty() {
                format!(
                    "Checks out a commit halfway between {} (good) and {bad} (bad).\nMark each commit good or bad until git names the first bad one; git bisect reset returns you to where you are now.",
                    goods.join(", ")
                )
            } else {
                format!(
                    "Checks out commits between {} (good) and {bad} (bad) and runs the test command on each: exit 0 means good, 125 skip, any other code up to 127 bad.",
                    goods.join(", ")
                )
            };
            crate::devops::pages::explain::confirm(
                cx,
                "Start bisecting?",
                body,
                "Start bisect",
                false,
                details,
                Act::StartBisectConfirmed {
                    bad,
                    goods,
                    run: (!cmd.is_empty()).then_some(cmd),
                    cmd_shell: shell == Shell::Cmd,
                }
                .into(),
            );
        }
        Act::StartBisectConfirmed {
            bad,
            goods,
            run,
            cmd_shell,
        } => {
            repo.devops.workbench.bisect.pending_run = run.map(|c| (c, cmd_shell));
            change(
                cx,
                repo,
                "Start bisect".to_owned(),
                core::start_args(&bad, &goods),
            );
        }
        Act::MarkBisect(v) => {
            let terms = match &repo.devops.workbench.bisect.info {
                Some(Ok(i)) => i.terms.clone(),
                _ => Default::default(),
            };
            change(
                cx,
                repo,
                format!("Bisect: mark {}", terms.word(v)),
                core::mark_args(&terms, v, None),
            );
        }
        Act::RunBisect => {
            let b = &repo.devops.workbench.bisect;
            let cmd = b.test_cmd.trim().to_owned();
            if cmd.is_empty() {
                cx.toast(
                    crate::state::ToastKind::Error,
                    "Enter a test command: it decides good (exit 0) or bad for each commit.",
                );
                return;
            }
            let shell = b.shell();
            let mut details = vec![run_line(&shell, &cmd)];
            if shell == Shell::Cmd {
                details.push(format!("# <script>.cmd\n{}", core::cmd_script_text(&cmd)));
            }
            crate::devops::pages::explain::confirm(
                cx,
                "Let the test command bisect?",
                "git checks out each candidate commit and runs the command in the working tree: exit 0 means good, 125 skip, any other code up to 127 bad, 128 or more stops the run. The output streams into the console.",
                "Run",
                false,
                details,
                Act::RunBisectConfirmed {
                    cmd,
                    cmd_shell: shell == Shell::Cmd,
                }
                .into(),
            );
        }
        Act::RunBisectConfirmed { cmd, cmd_shell } => start_run(cx, repo, cmd, cmd_shell),
        Act::ResetBisect => {
            let back = match &repo.devops.workbench.bisect.info {
                Some(Ok(i)) => i
                    .start_ref
                    .clone()
                    .unwrap_or_else(|| "where it started".into()),
                _ => "where it started".into(),
            };
            crate::devops::pages::explain::confirm(
                cx,
                "End the bisection?",
                format!("Checks out {back} again and forgets the good / bad marks."),
                "Reset",
                false,
                vec!["git bisect reset".to_owned()],
                Act::ResetBisectConfirmed.into(),
            );
        }
        Act::ResetBisectConfirmed => {
            repo.devops.workbench.bisect.run_id = None;
            change(
                cx,
                repo,
                "Bisect reset".to_owned(),
                vec!["bisect".into(), "reset".into()],
            );
        }
        _ => {}
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    match msg {
        Msg::Bisect(r) => {
            let b = &mut repo.devops.workbench.bisect;
            b.loading = false;
            b.at = Some(Instant::now());
            if let Ok(info) = r.as_ref() {
                if b.bad.trim().is_empty() {
                    b.bad = "HEAD".to_owned();
                }
                if b.good.trim().is_empty()
                    && let Some(t) = &info.latest_tag
                {
                    b.good = t.clone();
                }
            }
            b.info = Some(*r);
        }
        Msg::BisectChanged(r) => {
            let b = &mut repo.devops.workbench.bisect;
            b.busy = false;
            let ok = r.is_ok();
            b.last_output = Some(r);
            let pending = b.pending_run.take();
            refresh(cx, repo);
            if ok && let Some((cmd, cmd_shell)) = pending {
                start_run(cx, repo, cmd, cmd_shell);
            }
        }
        _ => {}
    }
}

pub fn on_console_done(cx: &mut Ctx, repo: &mut RepoState, run: &ConsoleRun) {
    if repo.devops.workbench.bisect.run_id != Some(run.id) {
        return;
    }
    let text = run.text();
    repo.devops.workbench.bisect.last_output = Some(match core::first_bad_in(&text) {
        Some(_) => Ok(text.clone()),
        None if run.succeeded() => Ok(text.clone()),
        None => Err(run.tail(12)),
    });
    refresh(cx, repo);
    // HEAD moved commit after commit.
    cx.action(Action::Reload(true));
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

fn commit_card(ui: &mut Ui, c: &CommitInfo, now: i64, in_graph: bool, actions: &mut Vec<Action>) {
    kit::kv(ui, 110.0, |kv| {
        kv.text("Commit", kit::mono(&c.sha));
        kv.text("Author", format!("{} <{}>", c.author, c.email));
        kv.text(
            "Date",
            format!(
                "{}  ·  {}",
                format::absolute(c.time),
                format::relative(c.time, now)
            ),
        );
        let files: Vec<String> = c
            .files
            .iter()
            .take(12)
            .map(|(p, a, d)| match (a, d) {
                (Some(a), Some(d)) => format!("{p}  +{a} −{d}"),
                _ => format!("{p}  binary"),
            })
            .collect();
        let more = c.files.len().saturating_sub(12);
        kv.row("Files", |ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            if files.is_empty() {
                kit::muted(ui, "No file changes");
            }
            for f in &files {
                ui.label(kit::mono(f).color(pal(ui).text));
            }
            if more > 0 {
                kit::muted(ui, &format!("and {more} more"));
            }
        });
    });
    let body: String = c
        .message
        .lines()
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned();
    if !body.is_empty() {
        kit::code_block(ui, &body, actions);
    }
    wui::buttons(ui, |ui| {
        if in_graph
            && IconButton::new(Icon::Graph, "Show in graph")
                .compact()
                .show(ui)
                .clicked()
            && let Ok(oid) = Oid::from_str(&c.sha)
        {
            actions.push(Action::JumpTo(oid));
        }
        if IconButton::new(Icon::Copy, "Copy id")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::Copy(c.sha.clone()));
        }
    });
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, console: &Console, actions: &mut Vec<Action>) {
    let now = format::now_secs();
    let selected_commit = match (&repo.selection, &repo.snapshot) {
        (Selection::Commit(id), Some(s)) => s
            .index_of
            .get(id)
            .and_then(|i| s.commits.get(*i))
            .map(|c| (c.short.clone(), c.summary.clone())),
        _ => None,
    };
    let graph_ids: std::collections::HashSet<String> = repo
        .snapshot
        .as_ref()
        .map(|s| s.commits.iter().map(|c| c.id.to_string()).collect())
        .unwrap_or_default();
    let busy = repo.is_busy();
    let b = &mut repo.devops.workbench.bisect;
    let active = matches!(&b.info, Some(Ok(i)) if i.active);
    kit::page_header(ui, "Bisect", super::Tab::Bisect.hint(), |ui| {
        if active
            && IconButton::new(Icon::Close, "Reset")
                .compact()
                .enabled(!b.busy && !busy)
                .show(ui)
                .on_hover_text("git bisect reset: end the bisection and go back")
                .clicked()
        {
            actions.push(Act::ResetBisect.into());
        }
        if IconButton::new(Icon::Rerun, "Refresh")
            .compact()
            .enabled(!b.loading)
            .show(ui)
            .on_hover_text("Read the bisect state again")
            .clicked()
        {
            actions.push(Act::RefreshBisect.into());
        }
    });
    wui::page(ui, "wb_bisect_page", |ui| {
        let live = matches!(&b.info, Some(Ok(i)) if i.active && i.first_bad.is_none());
        match &b.last_output {
            Some(Err(e)) => kit::callout(ui, Tone::Danger, "The last bisect command failed", e),
            Some(Ok(text)) if live => {
                let line = step_summary(text);
                if !line.is_empty() {
                    kit::callout(ui, Tone::Accent, "Last step", &line);
                }
            }
            _ => {}
        }
        let info = match &b.info {
            None => {
                kit::loading(ui, "Reading the bisect state…");
                return;
            }
            Some(Err(e)) => {
                let e = e.clone();
                if wui::error_card(ui, "Could not read the bisect state", &e, "Try again") {
                    actions.push(Act::RefreshBisect.into());
                }
                return;
            }
            Some(Ok(i)) => i.clone(),
        };
        if !info.active {
            start_form(ui, b, &info, selected_commit.as_ref(), busy, actions);
            return;
        }
        // Progress.
        let count = |v: Verdict| info.log.entries.iter().filter(|e| e.verdict == v).count();
        let mut tiles = vec![
            Stat::new("Tested", format::count(info.log.entries.len())),
            Stat::new("Good", format::count(count(Verdict::Good))),
            Stat::new("Bad", format::count(count(Verdict::Bad))),
            Stat::new("Skipped", format::count(count(Verdict::Skip))),
        ];
        let left;
        let steps;
        if let Some(v) = &info.vars {
            left = format::count(v.candidates);
            steps = format!("≈ {}", v.steps + 1);
            tiles.push(
                Stat::new("Candidates", left).tip("Commits that could still be the first bad one"),
            );
            tiles.push(
                Stat::new("Steps left", steps).tip("log2 of the candidates, as git estimates it"),
            );
        }
        crate::views::insights::stats(ui, &tiles);
        if let Some(fb) = &info.first_bad {
            kit::callout(
                ui,
                Tone::Danger,
                &format!("First bad commit: {} {}", fb.short, fb.subject),
                &format!(
                    "Found after testing {} commits. Reset to go back to {}.",
                    info.log.entries.len(),
                    info.start_ref.as_deref().unwrap_or("where you started")
                ),
            );
            kit::title(ui, &fb.subject);
            commit_card(ui, fb, now, graph_ids.contains(&fb.sha), actions);
            wui::buttons(ui, |ui| {
                if form::primary(ui, "Reset bisect", !b.busy && !busy).clicked() {
                    actions.push(Act::ResetBisect.into());
                }
            });
        } else if !info.log.candidates.is_empty() {
            kit::callout(
                ui,
                Tone::Warning,
                "Only skipped commits are left",
                &format!(
                    "The first bad commit is one of: {}",
                    info.log
                        .candidates
                        .iter()
                        .map(|(s, subj)| format!("{} {subj}", &s[..10.min(s.len())]))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            );
        } else if let Some(cur) = &info.current {
            kit::section(ui, "Testing now", None);
            kit::title(ui, &format!("{}  {}", cur.short, cur.subject));
            if !info.has_bad || info.good_count == 0 {
                kit::secondary(
                    ui,
                    &format!(
                        "git needs at least one {} and one {} commit before it can pick the next one.",
                        info.terms.bad, info.terms.good
                    ),
                );
            } else {
                kit::secondary(
                    ui,
                    "Check this commit (build it, run the test, try the feature), then say whether it has the problem.",
                );
            }
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let enabled = !b.busy && !busy;
                for (v, label, tone_primary) in [
                    (Verdict::Good, "Good", false),
                    (Verdict::Bad, "Bad", true),
                    (Verdict::Skip, "Skip", false),
                ] {
                    let word = info.terms.word(v).to_owned();
                    let resp = if tone_primary {
                        form::danger(ui, label, enabled)
                    } else if v == Verdict::Good {
                        form::primary(ui, label, enabled)
                    } else {
                        form::button(ui, label, enabled)
                    };
                    if resp.on_hover_text(format!("git bisect {word}")).clicked() {
                        actions.push(Act::MarkBisect(v).into());
                    }
                }
                if b.busy {
                    ui.add(egui::Spinner::new().size(14.0));
                }
            });
            kit::secondary(
                ui,
                &format!(
                    "Runs git bisect {} / {} / skip on HEAD ({}), then checks out the next commit.",
                    info.terms.good, info.terms.bad, cur.short
                ),
            );
            commit_card(ui, cur, now, graph_ids.contains(&cur.sha), actions);
        }
        if info.first_bad.is_none() {
            run_section(ui, b, console, busy, actions);
        } else if let Some(run) = b.run_id.and_then(|id| console.get(id)) {
            kit::section(ui, "Automatic run", None);
            console_box(ui, run, actions);
        }
        log_table(ui, &info, &graph_ids, actions);
    });
}

fn start_form(
    ui: &mut Ui,
    b: &mut Bisect,
    info: &BisectInfo,
    selected: Option<&(String, String)>,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    kit::title(ui, "Find the commit that broke it");
    kit::body(
        ui,
        "A binary search through history for the first commit with the problem.",
    );
    explain::steps(
        ui,
        &[
            "Give a bad commit (one that has the problem, usually HEAD) and a good one (an older commit without it).",
            "git checks out the commit halfway between them; you say good or bad.",
            "It halves the range again until one commit is left.",
        ],
    );
    kit::secondary(ui, "A test command can answer for you.");
    ui.add_space(6.0);
    form::card(ui, Tone::Neutral, |ui| {
        // Room for the "Use <commit>" button beside the input.
        let w = (ui.available_width() - 150.0).clamp(160.0, 360.0);
        form::field(
            ui,
            "Bad commit",
            Some("Has the problem. Default: HEAD."),
            |ui| {
                ui.horizontal(|ui| {
                    form::input(
                        ui,
                        "wb_bisect_bad",
                        &mut b.bad,
                        InputOpts::new("HEAD").mono().width(w),
                    );
                    if let Some((short, summary)) = selected
                        && form::button(ui, &format!("Use {short}"), true)
                            .on_hover_text(format!("The commit selected in the graph: {summary}"))
                            .clicked()
                    {
                        b.bad = short.clone();
                    }
                });
            },
        );
        let tag_help = match &info.latest_tag {
            Some(t) => format!("Did not have it yet. Default: the latest tag, {t}. Several: separate with spaces."),
            None => "Did not have it yet: an older commit, tag or branch. Several: separate with spaces.".to_owned(),
        };
        form::field(ui, "Good commit", Some(&tag_help), |ui| {
            ui.horizontal(|ui| {
                form::input(
                    ui,
                    "wb_bisect_good",
                    &mut b.good,
                    InputOpts::new("v1.0 or a commit id").mono().width(w),
                );
                if let Some((short, summary)) = selected
                    && form::button(ui, &format!("Use {short}"), true)
                        .on_hover_text(format!("The commit selected in the graph: {summary}"))
                        .clicked()
                {
                    b.good = short.clone();
                }
            });
        });
        form::field(
            ui,
            "Test command (optional)",
            Some(
                "Runs on each commit: exit 0 = good, 125 = skip, 1–127 = bad. Leave empty to answer yourself.",
            ),
            |ui| {
                form::input(
                    ui,
                    "wb_bisect_cmd",
                    &mut b.test_cmd,
                    InputOpts::new("e.g. cargo test parser or npm test").mono(),
                );
            },
        );
        shell_choice(ui, b);
        form::actions_row(ui, |ui| {
            if form::primary(ui, "Start bisect", !b.busy && !busy).clicked() {
                actions.push(Act::StartBisect.into());
            }
        });
    });
}

fn shell_choice(ui: &mut Ui, b: &mut Bisect) {
    let sh = b.sh.clone().flatten();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        ui.label(
            RichText::new("Shell")
                .size(size::SMALL)
                .family(crate::theme::medium())
                .color(pal(ui).text_2),
        );
        match &sh {
            Some(p) => {
                if form::radio(ui, !b.cmd_shell, "sh -c") {
                    b.cmd_shell = false;
                }
                kit::muted(ui, &p.display().to_string());
            }
            None => {
                kit::muted(ui, "sh not found (Git for Windows ships it)");
            }
        }
        if cfg!(windows) && form::radio(ui, b.cmd_shell || sh.is_none(), "cmd /C") {
            b.cmd_shell = true;
        }
    });
}

fn console_box(ui: &mut Ui, run: &ConsoleRun, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    egui::Frame::NONE
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(egui::CornerRadius::same(crate::theme::radius::M))
        .show(ui, |ui| {
            ui.allocate_ui(vec2(ui.available_width(), 340.0), |ui| {
                ui.set_min_height(340.0);
                crate::views::console::output(ui, run, "wb_bisect_run", actions);
            });
        });
}

fn run_section(
    ui: &mut Ui,
    b: &mut Bisect,
    console: &Console,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    kit::section(ui, "Automatic run", None);
    kit::secondary(
        ui,
        "git bisect run checks out each candidate and runs the command; its exit code decides.",
    );
    explain::details(ui, "wb_bisect_codes", "Exit codes", false, |ui| {
        explain::terms(
            ui,
            110.0,
            &[
                ("0", "Good"),
                ("125", "Skip (it cannot be tested)"),
                ("1–127", "Bad"),
                ("128 or more", "Stops the run"),
            ],
        );
    });
    let running = b
        .run_id
        .and_then(|id| console.get(id))
        .is_some_and(|r| r.running());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let w = (ui.available_width() - 140.0).max(240.0);
        form::input(
            ui,
            "wb_bisect_cmd_active",
            &mut b.test_cmd,
            InputOpts::new("Test command, e.g. cargo test parser")
                .mono()
                .width(w),
        );
        if form::primary(
            ui,
            "Run",
            !running && !b.busy && !busy && !b.test_cmd.trim().is_empty(),
        )
        .clicked()
        {
            actions.push(Act::RunBisect.into());
        }
    });
    shell_choice(ui, b);
    if let Some(run) = b.run_id.and_then(|id| console.get(id)) {
        console_box(ui, run, actions);
    }
}

fn log_table(
    ui: &mut Ui,
    info: &BisectInfo,
    graph_ids: &std::collections::HashSet<String>,
    actions: &mut Vec<Action>,
) {
    kit::section(ui, "Tested commits", Some(info.log.entries.len()));
    if info.log.entries.is_empty() {
        kit::muted(ui, "Nothing marked yet.");
        return;
    }
    let rows: Vec<Vec<crate::views::insights::Cell>> = info
        .log
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let tone = match e.verdict {
                Verdict::Good => Tone::Success,
                Verdict::Bad => Tone::Danger,
                Verdict::Skip => Tone::Neutral,
            };
            let short = e.sha[..10.min(e.sha.len())].to_owned();
            vec![
                crate::views::insights::Cell::Sec(format!("{}", i + 1)),
                crate::views::insights::Cell::Badges(vec![(
                    sentence_case(info.terms.word(e.verdict)),
                    tone,
                )]),
                if graph_ids.contains(&e.sha) {
                    crate::views::insights::Cell::MonoLink(short)
                } else {
                    crate::views::insights::Cell::Mono(short)
                },
                crate::views::insights::Cell::Text(e.subject.clone()),
            ]
        })
        .collect();
    let shas: Vec<String> = info.log.entries.iter().map(|e| e.sha.clone()).collect();
    crate::devops::pages::layout::wide(ui, "wb_bisect_log", 520.0, |ui| {
        crate::views::insights::grid(
            ui,
            "wb_bisect_log",
            &[
                Col::num("#"),
                Col::text("Verdict"),
                Col::text("Commit"),
                Col::text("Subject"),
            ],
            &rows,
            true,
            |r, resp| {
                if resp.clicked()
                    && graph_ids.contains(&shas[r])
                    && let Ok(oid) = Oid::from_str(&shas[r])
                {
                    actions.push(Action::JumpTo(oid));
                }
            },
        );
    });
    if let Some(start) = &info.log.start {
        ui.add_space(4.0);
        kit::muted(ui, &format!("Started with {start}"));
    }
}
