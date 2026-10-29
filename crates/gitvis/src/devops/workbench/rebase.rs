//! Interactive rebase: pick a base, edit the plan (step per commit,
//! reorder by buttons, keys or drag, messages inline), see the resulting
//! history, then run it through `git rebase -i` with the todo shown in the
//! confirmation.

use std::path::PathBuf;

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Label, Layout, Margin, Rect, RichText,
    Sense, Stroke, Ui, pos2, vec2,
};
use gitvis_core::devops::workbench::rebase::{
    self as core, BaseChoice, Loaded, Outcome, Plan, RepoCheck, Step,
};
use gitvis_core::{Oid, Pending};

use super::{Act, Msg, ui as wui};
use crate::devops::ctx::Ctx;
use crate::devops::pages::{explain, layout};
use crate::format;
use crate::state::{Action, RepoState, Settings};
use crate::theme::{Palette, pal, radius, semibold, size};
use crate::views::form::{self, InputOpts};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Stat, Tone};

#[derive(Default)]
pub struct Rebase {
    pub base_input: String,
    pub suggestions: Option<Vec<BaseChoice>>,
    pub suggestions_loading: bool,
    pub loaded: Option<Result<Loaded, String>>,
    pub loading: bool,
    pub plan: Plan,
    pub focused: Option<usize>,
    /// Row being dragged by its handle.
    pub dragging: Option<usize>,
    pub running: bool,
    pub last: Option<Result<Outcome, String>>,
    /// Where the todo and messages go, fixed when the confirmation opens.
    pub work: Option<PathBuf>,
    /// Steps to set once the plan loads (`Act::RebasePlan`).
    pub pending_marks: Option<Vec<(String, Step)>>,
}

fn load(cx: &mut Ctx, repo: &mut RepoState, base: String) {
    let r = &mut repo.devops.workbench.rebase;
    r.base_input = base.clone();
    if r.loading {
        return;
    }
    r.loading = true;
    cx.read(format!("Load commits since {base}"), move |dir, log| {
        let mut cmds = Vec::new();
        let res = core::load(dir, &base, &mut cmds);
        log.commands.extend(cmds);
        Msg::RebaseLoaded(res)
    });
}

fn suggest(cx: &mut Ctx, repo: &mut RepoState, auto_load: bool) {
    let r = &mut repo.devops.workbench.rebase;
    if r.suggestions_loading {
        return;
    }
    r.suggestions_loading = true;
    cx.read("Find rebase bases", move |dir, log| {
        let mut cmds = Vec::new();
        let s = core::suggestions(dir, &mut cmds);
        log.commands.extend(cmds);
        Msg::RebaseSuggestions(s, auto_load)
    });
}

pub fn shown(cx: &mut Ctx, repo: &mut RepoState) {
    let r = &repo.devops.workbench.rebase;
    if r.suggestions.is_none() {
        let auto = r.loaded.is_none() && r.base_input.trim().is_empty() && !r.loading;
        suggest(cx, repo, auto);
    }
}

/// What the repository is in the middle of.
fn repo_check(repo: &RepoState) -> (Option<&'static str>, usize) {
    match &repo.snapshot {
        Some(s) => (
            match s.pending {
                Pending::None => None,
                Pending::Merge => Some("merge"),
                Pending::Rebase => Some("rebase"),
                Pending::CherryPick => Some("cherry-pick"),
                Pending::Revert => Some("revert"),
                Pending::Bisect => Some("bisect"),
                Pending::ApplyMailbox => Some("git am"),
            },
            s.status.conflicted.len(),
        ),
        None => (None, 0),
    }
}

fn problems(repo: &RepoState) -> Vec<String> {
    let (pending, conflicts) = repo_check(repo);
    core::validate(
        &repo.devops.workbench.rebase.plan,
        RepoCheck { pending, conflicts },
    )
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::RebaseFrom(sha) => {
            repo.devops.workbench.tab = super::Tab::Rebase;
            // From the selected commit on: its parent is the base.
            load(cx, repo, format!("{sha}^"));
            if repo.devops.workbench.rebase.suggestions.is_none() {
                suggest(cx, repo, false);
            }
        }
        Act::RebasePlan { base, marks } => {
            repo.devops.workbench.tab = super::Tab::Rebase;
            repo.devops.workbench.rebase.pending_marks = Some(marks);
            load(cx, repo, base);
            if repo.devops.workbench.rebase.suggestions.is_none() {
                suggest(cx, repo, false);
            }
        }
        Act::LoadRebase(base) => {
            let base = base.trim().to_owned();
            if !base.is_empty() {
                load(cx, repo, base);
            }
        }
        Act::RebaseStep(row, step) => {
            let r = &mut repo.devops.workbench.rebase;
            if row < r.plan.rows.len() {
                r.plan.set_step(row, step);
                r.focused = Some(row);
            }
        }
        Act::RebaseMove(from, to) => {
            let r = &mut repo.devops.workbench.rebase;
            if from < r.plan.rows.len() && to < r.plan.rows.len() {
                r.plan.move_row(from, to);
                r.focused = Some(to);
            }
        }
        Act::RebaseFocus(row) => repo.devops.workbench.rebase.focused = row,
        Act::RebaseReset => {
            let r = &mut repo.devops.workbench.rebase;
            if let Some(Ok(l)) = &r.loaded {
                r.plan = Plan::new(l.commits.clone());
                r.focused = None;
            }
        }
        Act::StartRebase => {
            let errors = problems(repo);
            if !errors.is_empty() {
                cx.toast(crate::state::ToastKind::Error, errors.join("\n"));
                return;
            }
            let r = &mut repo.devops.workbench.rebase;
            let Some(Ok(l)) = &r.loaded else { return };
            let work = core::work_dir();
            let todo = core::todo(&r.plan, &work);
            let mut details = core::command(&l.base_sha, &work.join("git-rebase-todo"));
            details.push(format!(
                "# {}\n{}",
                work.join("git-rebase-todo").display(),
                todo.text.trim_end()
            ));
            for (name, content) in &todo.messages {
                details.push(format!(
                    "# {}\n{}",
                    work.join(name).display(),
                    content.trim_end()
                ));
            }
            let rewritten = r.plan.rewritten();
            let pushed = rewritten.iter().filter(|c| c.pushed).count();
            let mut body = format!(
                "Rewrites {} of {} commits onto {} ({}). Uncommitted changes are stashed first and restored afterwards (--autostash); hooks are skipped for the message amends, as git does for picks.",
                rewritten.len(),
                r.plan.rows.len(),
                &l.base_sha[..10.min(l.base_sha.len())],
                l.base_subject
            );
            if pushed > 0 {
                body.push_str(&format!(
                    " {pushed} of them are already pushed{}: you will need to force-push.",
                    if l.remote_branches.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", l.remote_branches.join(", "))
                    }
                ));
            }
            r.work = Some(work);
            cx.confirm(
                "Start the interactive rebase?",
                body,
                "Start rebase",
                pushed > 0,
                details,
                Act::RunRebase.into(),
            );
        }
        Act::RunRebase => {
            if !problems(repo).is_empty() {
                return;
            }
            let r = &mut repo.devops.workbench.rebase;
            let (Some(Ok(l)), Some(work)) = (&r.loaded, r.work.take()) else {
                return;
            };
            if r.running {
                return;
            }
            r.running = true;
            let plan = r.plan.clone();
            let base = l.base_sha.clone();
            let head = l.head_sha.clone();
            cx.local_change("Interactive rebase", move |dir, log| {
                let mut cmds = Vec::new();
                let res = core::execute(dir, &base, &head, &plan, &work, &mut cmds);
                log.commands.extend(cmds);
                let result = match &res {
                    Ok(Outcome::Done(s)) | Ok(Outcome::Stopped(s)) => Ok(s.clone()),
                    Ok(Outcome::Conflict(s)) => Err(s.clone()),
                    Err(e) => Err(e.clone()),
                };
                (result, Some(Msg::RebaseDone(res)))
            });
        }
        _ => {}
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    let r = &mut repo.devops.workbench.rebase;
    match msg {
        Msg::RebaseSuggestions(s, auto) => {
            r.suggestions_loading = false;
            let first = s.first().map(|c| c.rev.clone());
            r.suggestions = Some(s);
            if auto
                && r.base_input.trim().is_empty()
                && r.loaded.is_none()
                && let Some(base) = first
            {
                load(cx, repo, base);
            }
        }
        Msg::RebaseLoaded(res) => {
            r.loading = false;
            if let Ok(l) = &res {
                r.plan = Plan::new(l.commits.clone());
                r.focused = None;
                r.dragging = None;
                if let Some(marks) = r.pending_marks.take() {
                    let missing =
                        gitvis_core::devops::history::multi::apply_marks(&mut r.plan, &marks);
                    if !missing.is_empty() {
                        cx.toast(
                            crate::state::ToastKind::Info,
                            format!(
                                "{} of the picked commits are not in this plan (merges, or outside {}..HEAD)",
                                missing.len(),
                                l.base
                            ),
                        );
                    }
                }
            }
            if res.is_err() {
                r.pending_marks = None;
            }
            r.loaded = Some(res);
        }
        Msg::RebaseDone(res) => {
            r.running = false;
            let done = matches!(res, Ok(Outcome::Done(_)));
            r.last = Some(res);
            // Show the new history of the same range.
            if done && let Some(Ok(l)) = &r.loaded {
                let base = l.base_sha.clone();
                r.suggestions = None;
                load(cx, repo, base);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

fn step_color(p: &Palette, s: Step) -> Color32 {
    match s {
        Step::Pick => p.border_strong,
        Step::Reword => p.accent,
        Step::Edit => p.warning,
        Step::Squash | Step::Fixup => p.purple,
        Step::Drop => p.danger,
    }
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, _settings: &Settings, actions: &mut Vec<Action>) {
    let now = format::now_secs();
    let in_graph: Box<dyn Fn(&str) -> bool> = match &repo.snapshot {
        Some(s) => {
            let ids: std::collections::HashSet<String> =
                s.commits.iter().map(|c| c.id.to_string()).collect();
            Box::new(move |sha| ids.contains(sha))
        }
        None => Box::new(|_| false),
    };
    let (pending, conflicts) = repo_check(repo);
    let dirty = repo
        .snapshot
        .as_ref()
        .map(|s| s.status.change_count())
        .unwrap_or(0);
    let errors = problems(repo);
    let r = &mut repo.devops.workbench.rebase;
    crate::devops::pages::layout::page_header(
        ui,
        "Interactive rebase",
        super::Tab::Rebase.hint(),
        |ui| {
            if let Some(Ok(l)) = &r.loaded
                && IconButton::new(Icon::Rerun, "Reload")
                    .compact()
                    .enabled(!r.loading)
                    .show(ui)
                    .on_hover_text("Read the commits again (the plan starts over)")
                    .clicked()
            {
                actions.push(Act::LoadRebase(l.base.clone()).into());
            }
        },
    );
    wui::page(ui, "wb_rebase_page", |ui| {
        base_bar(ui, r, actions);
        if pending == Some("rebase") {
            kit::callout(
                ui,
                Tone::Warning,
                "A rebase is in progress",
                "Continue, skip the commit or abort it from the bar at the top of the window. A new plan can start once it is finished.",
            );
        }
        if let Some(last) = &r.last {
            match last {
                Ok(Outcome::Done(s)) => kit::callout(ui, Tone::Success, "Rebase finished", s),
                Ok(Outcome::Stopped(s)) => {
                    kit::callout(ui, Tone::Warning, "The rebase stopped for editing", s)
                }
                Ok(Outcome::Conflict(s)) => {
                    kit::callout(ui, Tone::Danger, "The rebase stopped on conflicts", s)
                }
                Err(e) => kit::callout(ui, Tone::Danger, "The rebase did not run", e),
            }
        }
        if r.loading {
            kit::loading(ui, &format!("Reading the commits since {}…", r.base_input));
        }
        match &r.loaded {
            None if !r.loading => {
                ui.add_space(24.0);
                kit::empty_state(
                    ui,
                    Icon::Edit,
                    "Choose where the rebase starts",
                    "Type a branch, tag or commit as the base, or pick a suggestion.\nThe commits after it, up to HEAD, become the plan.\nIn the history graph: right-click a commit, Interactive rebase from here.",
                    None,
                );
            }
            None => {}
            Some(Err(e)) => {
                let e = e.clone();
                if wui::error_card(ui, "Could not read the commits", &e, "Try again") {
                    actions.push(Act::LoadRebase(r.base_input.clone()).into());
                }
            }
            Some(Ok(_)) => editor(ui, r, &errors, dirty, conflicts, now, &*in_graph, actions),
        }
    });
}

fn base_bar(ui: &mut Ui, r: &mut super::rebase::Rebase, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    // The label is centred on the input (form::row), and the input gives
    // way in a narrow window instead of pushing the button off the page.
    let w = (ui.available_width() - 44.0 - 150.0).clamp(160.0, 320.0);
    form::row(ui, 36.0, "Base", |ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let resp = form::input(
            ui,
            "wb_rebase_base",
            &mut r.base_input,
            InputOpts::new("Branch, tag or commit, e.g. origin/main or HEAD~5")
                .mono()
                .width(w),
        );
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (form::primary(
            ui,
            "Load commits",
            !r.loading && !r.base_input.trim().is_empty(),
        )
        .clicked()
            || enter)
            && !r.base_input.trim().is_empty()
        {
            actions.push(Act::LoadRebase(r.base_input.clone()).into());
        }
    });
    match &r.suggestions {
        None if r.suggestions_loading => {
            kit::loading(ui, "Looking for the upstream and the default branch…")
        }
        Some(s) if !s.is_empty() => {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                ui.label(
                    RichText::new("Suggestions")
                        .size(size::SMALL)
                        .color(p.text_3),
                );
                for c in s {
                    let on = r.base_input.trim() == c.rev;
                    if kit::tip(kit::chip(ui, &c.label, on), &c.why).clicked() {
                        actions.push(Act::LoadRebase(c.rev.clone()).into());
                    }
                }
            });
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn editor(
    ui: &mut Ui,
    r: &mut super::rebase::Rebase,
    errors: &[String],
    dirty: usize,
    conflicts: usize,
    now: i64,
    in_graph: &dyn Fn(&str) -> bool,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let Some(Ok(l)) = &r.loaded else { return };
    let l = l.clone();
    ui.add_space(4.0);
    ui.label(
        RichText::new(format!(
            "{} commit{} from {} {} up to HEAD {}",
            l.commits.len(),
            if l.commits.len() == 1 { "" } else { "s" },
            &l.base_sha[..10.min(l.base_sha.len())],
            l.base_subject,
            &l.head_sha[..10.min(l.head_sha.len())]
        ))
        .size(size::SMALL)
        .color(p.text_2),
    );
    if l.commits.is_empty() {
        kit::callout(
            ui,
            Tone::Neutral,
            "Nothing to rebase",
            "HEAD has no commits after this base. Pick an older base.",
        );
        return;
    }
    let count = |s: Step| r.plan.rows.iter().filter(|x| x.step == s).count();
    let result = core::preview(&r.plan);
    let rewritten = r.plan.rewritten();
    let pushed_rewritten = rewritten.iter().filter(|c| c.pushed).count();
    crate::views::insights::stats(
        ui,
        &[
            Stat::new("Commits", format::count(r.plan.rows.len())),
            Stat::new("After", format::count(result.len())).tip("Commits once the plan has run"),
            Stat::new("Reworded", format::count(count(Step::Reword))),
            Stat::new(
                "Folded",
                format::count(count(Step::Squash) + count(Step::Fixup)),
            )
            .tip("Squashed or fixed up into the commit above"),
            Stat::new("Edit stops", format::count(count(Step::Edit))),
            Stat::new("Dropped", format::count(count(Step::Drop)))
                .tone_if(count(Step::Drop) > 0, Tone::Danger),
            Stat::new("Rewritten", format::count(rewritten.len())).tip("Commits that get a new id"),
        ],
    );
    if pushed_rewritten > 0 {
        kit::callout(
            ui,
            Tone::Warning,
            "Already pushed: you will need to force-push",
            &format!(
                "{} of the {} rewritten commits are on {}.\nAfter the rebase the branch and its remote disagree: push with --force-with-lease, and tell anyone who has the old commits.",
                pushed_rewritten,
                rewritten.len(),
                if l.remote_branches.is_empty() {
                    "a remote-tracking branch".to_owned()
                } else {
                    l.remote_branches.join(", ")
                }
            ),
        );
    }
    if l.merges_skipped > 0 {
        kit::callout(
            ui,
            Tone::Neutral,
            "Merge commits are flattened",
            &format!(
                "{} merge commit{} in the range {} not in the plan.\nLike git rebase without --rebase-merges, the rebase replays the merged commits one by one and the merges disappear.",
                l.merges_skipped,
                if l.merges_skipped == 1 { "" } else { "s" },
                if l.merges_skipped == 1 { "is" } else { "are" }
            ),
        );
    }
    if l.not_ancestor {
        kit::callout(
            ui,
            Tone::Neutral,
            "The base is not an ancestor of HEAD",
            "The commits of HEAD that the base does not have are replayed on top of it.\nThis also moves the branch onto the base.",
        );
    }
    if dirty > conflicts && conflicts == 0 {
        kit::secondary(
            ui,
            &format!(
                "{} uncommitted change{} will be stashed before the rebase and restored after it.",
                dirty,
                if dirty == 1 { "" } else { "s" }
            ),
        );
    }

    kit::section_with(ui, "Plan, oldest first", Some(r.plan.rows.len()), |ui| {
        if IconButton::new(Icon::Undo, "Reset plan")
            .compact()
            .enabled(!r.plan.is_noop())
            .show(ui)
            .clicked()
        {
            actions.push(Act::RebaseReset.into());
        }
    });
    kit::secondary(
        ui,
        "Pick a step for each commit; drag a row by its handle to reorder it.",
    );
    explain::details(ui, "wb_rebase_keys", "Keyboard", false, |ui| {
        explain::terms(
            ui,
            110.0,
            &[
                ("Click a row", "Focuses it for the keys below."),
                (
                    "p r e s f d",
                    "Pick, reword, edit, squash, fix up or drop the focused commit.",
                ),
                ("↑ ↓", "Move the focus."),
                ("Alt+↑ ↓", "Move the commit."),
            ],
        );
    });
    plan_rows(ui, r, now, in_graph, actions);

    if r.plan.is_noop() && errors.len() == 1 {
        // Nothing wrong yet: nothing has been changed.
        kit::secondary(
            ui,
            "Every commit is picked in its original order: change a step or the order to make a plan.",
        );
    } else if !errors.is_empty() {
        kit::callout(
            ui,
            Tone::Danger,
            "The plan cannot run yet",
            &errors.join("\n"),
        );
    }

    kit::section(ui, "Resulting history, newest first", Some(result.len()));
    preview_rows(ui, &result, &l);

    ui.add_space(8.0);
    form::actions_row(ui, |ui| {
        let start = form::primary(ui, "Start rebase", errors.is_empty() && !r.running);
        let start = if errors.is_empty() {
            start.on_hover_text("Shows the exact todo file and commands first")
        } else {
            start.on_hover_text(errors.join("\n"))
        };
        if start.clicked() {
            actions.push(Act::StartRebase.into());
        }
        if r.running {
            ui.add(egui::Spinner::new().size(14.0));
        }
    });
}

/// The grip of a draggable row: two columns of dots.
fn grip(ui: &mut Ui, color: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(14.0, 26.0), Sense::click_and_drag());
    let c = if resp.hovered() || resp.dragged() {
        pal(ui).text
    } else {
        color
    };
    for dx in [-2.5, 2.5] {
        for dy in [-6.0, 0.0, 6.0] {
            ui.painter()
                .circle_filled(rect.center() + vec2(dx, dy), 1.4, c);
        }
    }
    resp.on_hover_cursor(egui::CursorIcon::Grab)
}

fn plan_rows(
    ui: &mut Ui,
    r: &mut super::rebase::Rebase,
    now: i64,
    in_graph: &dyn Fn(&str) -> bool,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let n = r.plan.rows.len();
    let groups = r.plan.groups();
    let typing = ui.ctx().text_edit_focused();
    // Keyboard on the focused row.
    if let Some(f) = r.focused.filter(|f| *f < n)
        && !typing
    {
        let keys: [(egui::Key, Step); 6] = [
            (egui::Key::P, Step::Pick),
            (egui::Key::R, Step::Reword),
            (egui::Key::E, Step::Edit),
            (egui::Key::S, Step::Squash),
            (egui::Key::F, Step::Fixup),
            (egui::Key::D, Step::Drop),
        ];
        ui.input(|i| {
            for (k, s) in keys {
                if i.key_pressed(k) && i.modifiers.is_none() {
                    actions.push(Act::RebaseStep(f, s).into());
                }
            }
            if i.key_pressed(egui::Key::ArrowUp) {
                if i.modifiers.alt && f > 0 {
                    actions.push(Act::RebaseMove(f, f - 1).into());
                } else if !i.modifiers.alt {
                    actions.push(Act::RebaseFocus(Some(f.saturating_sub(1))).into());
                }
            }
            if i.key_pressed(egui::Key::ArrowDown) {
                if i.modifiers.alt && f + 1 < n {
                    actions.push(Act::RebaseMove(f, f + 1).into());
                } else if !i.modifiers.alt {
                    actions.push(Act::RebaseFocus(Some((f + 1).min(n - 1))).into());
                }
            }
        });
    }
    let mut rects: Vec<Rect> = Vec::with_capacity(n);
    let mut drop_from: Option<usize> = None;
    let steps: Vec<(Step, &str)> = Step::ALL.iter().map(|s| (*s, s.label())).collect();
    for i in 0..n {
        let row = r.plan.rows[i].clone();
        let folded = row.step.folds() && i > 0;
        let focused = r.focused == Some(i);
        let fill = if focused {
            p.selection
        } else if r.dragging == Some(i) {
            p.hover
        } else {
            p.raised
        };
        let id = ui.make_persistent_id(("wb_rebase_row", &row.commit.sha));
        let frame = Frame::NONE
            .fill(fill)
            .stroke(Stroke::new(1.0, if focused { p.accent.gamma_multiply(0.6) } else { p.border }))
            .corner_radius(CornerRadius::same(radius::M))
            .inner_margin(Margin {
                left: 6,
                right: 8,
                top: 5,
                bottom: 5,
            })
            .show(ui, |ui| {
                ui.push_id(id, |ui| {
                    // One line on a wide pane. On a narrow one the subject
                    // and the details go under the steps: side by side they
                    // were drawn over each other.
                    let wide = ui.available_width() >= 900.0;
                    let meta = format!(
                        "{}  ·  {}  ·  {} file{}  +{} −{}",
                        row.commit.author,
                        format::relative(row.commit.time, now),
                        row.commit.files,
                        if row.commit.files == 1 { "" } else { "s" },
                        row.commit.added,
                        row.commit.deleted
                    );
                    let subject = {
                        let s = if row.step == Step::Reword {
                            row.message.lines().next().unwrap_or("").to_owned()
                        } else {
                            row.commit.subject.clone()
                        };
                        let t = RichText::new(s).size(size::BODY);
                        match row.step {
                            Step::Drop => t.color(p.text_3).strikethrough(),
                            Step::Fixup => t.color(p.text_2),
                            _ => t.color(p.text),
                        }
                    };
                    // A click on the subject focuses the row for the
                    // keyboard.
                    let subject_label = |ui: &mut Ui, text: RichText| -> bool {
                        ui.allocate_ui_with_layout(
                            vec2(ui.available_width(), 24.0),
                            Layout::left_to_right(Align::Center),
                            |ui| ui.add(Label::new(text).truncate().sense(Sense::click())),
                        )
                        .inner
                        .clicked()
                    };
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let g = grip(ui, p.text_3);
                        if g.drag_started() {
                            r.dragging = Some(i);
                        }
                        if g.drag_stopped() {
                            drop_from = Some(i);
                        }
                        if g.clicked() {
                            actions.push(Act::RebaseFocus(Some(i)).into());
                        }
                        ui.label(
                            RichText::new(format!("{:>2}", i + 1))
                                .monospace()
                                .size(size::SMALL)
                                .color(p.text_3),
                        );
                        if folded {
                            ui.label(RichText::new("↳").size(size::BODY).color(p.purple));
                        }
                        if let Some(s) = layout::segmented_on_card(ui, &steps, row.step) {
                            actions.push(Act::RebaseStep(i, s).into());
                        }
                        let sha_color = if in_graph(&row.commit.sha) {
                            p.accent
                        } else {
                            p.text_2
                        };
                        let sha = ui
                            .add(
                                Label::new(kit::mono(&row.commit.short).color(sha_color))
                                    .sense(Sense::click()),
                            )
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if sha.clicked()
                            && let Ok(oid) = Oid::from_str(&row.commit.sha)
                            && in_graph(&row.commit.sha)
                        {
                            actions.push(Action::JumpTo(oid));
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            if IconButton::icon(Icon::ChevronDown)
                                .compact()
                                .enabled(i + 1 < n)
                                .show(ui)
                                .on_hover_text("Move down (Alt+↓)")
                                .clicked()
                            {
                                actions.push(Act::RebaseMove(i, i + 1).into());
                            }
                            if IconButton::icon(Icon::ChevronUp)
                                .compact()
                                .enabled(i > 0)
                                .show(ui)
                                .on_hover_text("Move up (Alt+↑)")
                                .clicked()
                            {
                                actions.push(Act::RebaseMove(i, i - 1).into());
                            }
                            if !wide {
                                return;
                            }
                            if row.commit.pushed {
                                kit::badge(ui, "Pushed", Tone::Warning);
                            }
                            ui.label(RichText::new(&meta).size(size::SMALL).color(p.text_3));
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                if subject_label(ui, subject.clone()) {
                                    actions.push(Act::RebaseFocus(Some(i)).into());
                                }
                            });
                        });
                    });
                    if !wide {
                        ui.horizontal(|ui| {
                            ui.add_space(44.0);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                ui.horizontal(|ui| {
                                    if row.commit.pushed {
                                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                            kit::badge(ui, "Pushed", Tone::Warning);
                                            ui.with_layout(
                                                Layout::left_to_right(Align::Center),
                                                |ui| {
                                                    if subject_label(ui, subject.clone()) {
                                                        actions
                                                            .push(Act::RebaseFocus(Some(i)).into());
                                                    }
                                                },
                                            );
                                        });
                                    } else if subject_label(ui, subject.clone()) {
                                        actions.push(Act::RebaseFocus(Some(i)).into());
                                    }
                                });
                                ui.add(
                                    Label::new(
                                        RichText::new(&meta).size(size::SMALL).color(p.text_3),
                                    )
                                    .truncate(),
                                );
                            });
                        });
                    }
                    if row.step == Step::Reword {
                        ui.add_space(2.0);
                        let msg = &mut r.plan.rows[i].message;
                        ui.horizontal(|ui| {
                            ui.add_space(44.0);
                            ui.vertical(|ui| {
                                ui.label(
                                    RichText::new("New message")
                                        .size(size::SMALL)
                                        .color(p.text_2),
                                );
                                form::area(ui, ("wb_reword", &row.commit.sha), msg, "Commit message", 3);
                            });
                        });
                    }
                    if row.step == Step::Edit {
                        ui.horizontal(|ui| {
                            ui.add_space(44.0);
                            kit::secondary(
                                ui,
                                "The rebase stops after this commit: amend it (stage changes and commit with Amend), then Continue from the bar at the top.",
                            );
                        });
                    }
                });
            })
            .response;
        let rect = frame.rect;
        rects.push(rect);
        // Edit stop and fold markers on the left edge.
        ui.painter().rect_filled(
            Rect::from_min_max(rect.min, pos2(rect.left() + 3.0, rect.bottom())),
            CornerRadius {
                nw: radius::M,
                sw: radius::M,
                ne: 0,
                se: 0,
            },
            step_color(&p, row.step),
        );
        // The combined message after the last row of a squash group.
        if let Some(g) = groups
            .iter()
            .find(|g| g.members.last() == Some(&i) && r.plan.has_squash(g))
        {
            let head = g.head;
            let head_short = r.plan.rows[head].commit.short.clone();
            let folded_n = g.members.len();
            let sha = r.plan.rows[head].commit.sha.clone();
            if let Some(buf) = r.plan.squash_buffer(head) {
                Frame::NONE
                    .fill(p.purple.gamma_multiply(if dark { 0.08 } else { 0.06 }))
                    .corner_radius(CornerRadius::same(radius::M))
                    .inner_margin(Margin {
                        left: 50,
                        right: 10,
                        top: 6,
                        bottom: 8,
                    })
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new(format!(
                                "Combined message of {head_short} and the {folded_n} commit{} folded into it",
                                if folded_n == 1 { "" } else { "s" }
                            ))
                            .size(size::SMALL)
                            .color(p.text_2),
                        );
                        form::area(ui, ("wb_squash", &sha), buf, "Commit message", 5);
                    });
            }
        }
        ui.add_space(2.0);
    }
    // Drag and drop: an insertion line while dragging, a move on release.
    if let Some(from) = r.dragging {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        if let Some(pos) = ui.ctx().pointer_latest_pos() {
            let to = rects
                .iter()
                .position(|rc| pos.y < rc.center().y)
                .unwrap_or(rects.len());
            let y = match rects.get(to) {
                Some(rc) => rc.top() - 1.0,
                None => rects.last().map(|rc| rc.bottom() + 1.0).unwrap_or(0.0),
            };
            if let (Some(first), true) = (rects.first(), to != from && to != from + 1) {
                ui.painter()
                    .hline(first.left()..=first.right(), y, Stroke::new(2.0, p.accent));
            }
            if drop_from == Some(from) {
                let target = if to > from { to - 1 } else { to };
                if target != from {
                    actions.push(Act::RebaseMove(from, target).into());
                }
                r.dragging = None;
            }
        }
        if !ui.input(|i| i.pointer.any_down()) && drop_from.is_none() {
            r.dragging = None;
        }
    }
}

fn preview_rows(ui: &mut Ui, result: &[core::PreviewCommit], l: &Loaded) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    for c in result.iter().rev() {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
        let painter = ui.painter();
        let mid = rect.center().y;
        let color = if c.rewritten { p.accent } else { p.text_3 };
        painter.circle_filled(pos2(rect.left() + 10.0, mid), 4.5, color);
        painter.vline(
            rect.left() + 10.0,
            (mid + 5.0)..=(rect.bottom() + 1.0),
            Stroke::new(1.5, p.border_strong),
        );
        let mut right = rect.right() - 4.0;
        let mut badges: Vec<(String, Tone)> = Vec::new();
        if !c.rewritten {
            badges.push(("Unchanged".into(), Tone::Neutral));
        }
        if c.reworded {
            badges.push(("New message".into(), Tone::Accent));
        }
        if c.folded > 0 {
            badges.push((format!("{} folded in", c.folded), Tone::Purple));
        }
        if c.stops {
            badges.push(("Stops for edit".into(), Tone::Warning));
        }
        for (t, tone) in badges.iter().rev() {
            right = kit::paint_badge(painter, &p, dark, right, mid, t, *tone) - 6.0;
        }
        let src = c.sources.join(" + ");
        let sg = painter.layout_no_wrap(src, FontId::monospace(size::MONO - 0.5), p.text_2);
        let sx = right - sg.size().x;
        painter.galley(pos2(sx, mid - sg.size().y / 2.0), sg, p.text_2);
        let g = crate::views::truncated(
            painter,
            &c.subject,
            FontId::proportional(size::BODY),
            if c.rewritten { p.text } else { p.text_2 },
            (sx - rect.left() - 36.0).max(40.0),
        );
        painter.galley(pos2(rect.left() + 24.0, mid - g.size().y / 2.0), g, p.text);
    }
    // The base.
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    let painter = ui.painter();
    let mid = rect.center().y;
    painter.circle_stroke(
        pos2(rect.left() + 10.0, mid),
        4.5,
        Stroke::new(1.5, p.text_3),
    );
    let r = painter.text(
        pos2(rect.left() + 24.0, mid),
        Align2::LEFT_CENTER,
        "base",
        FontId::new(size::SMALL, semibold()),
        p.text_3,
    );
    painter.text(
        pos2(r.right() + 8.0, mid),
        Align2::LEFT_CENTER,
        format!(
            "{}  {}",
            &l.base_sha[..10.min(l.base_sha.len())],
            l.base_subject
        ),
        FontId::proportional(size::SMALL),
        p.text_2,
    );
}
