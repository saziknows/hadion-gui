//! Context menus shared by the sidebar, the graph, the diff and the
//! activity log, and the item style every menu in those views uses: one
//! full-width 28 px row per entry, destructive entries in the danger
//! colour, hairlines between groups.

use egui::{Color32, CornerRadius, FontId, Response, Sense, Stroke, Ui, pos2, vec2};

use gitvis_core::ops::{GitOp, MergeMode, ResetMode};
use gitvis_core::{
    BranchInfo, Commit, GraphFilter, RefKind, RefLabel, RemoteBranch, Snapshot, StashInfo, TagInfo,
};

use crate::state::{Action, Dialog, ToastKind};
use crate::theme::{pal, radius, size};

pub struct MenuCtx<'a> {
    pub snap: &'a Snapshot,
    pub filter: &'a GraphFilter,
    pub github: Option<&'a gitvis_core::github::GitHubData>,
    /// For the extension hooks (commit menu items of other areas).
    pub hook: Option<&'a crate::devops::hooks::HookCx<'a>>,
}

impl MenuCtx<'_> {
    fn head_branch(&self) -> Option<&str> {
        self.snap.current_branch().map(|b| b.name.as_str())
    }
}

// ---------------------------------------------------------------------------
// Item style
// ---------------------------------------------------------------------------

const ITEM_H: f32 = 28.0;

/// Start a menu: minimum width and the tight vertical rhythm of the items.
pub fn begin(ui: &mut Ui, min_width: f32) {
    ui.set_min_width(min_width);
    ui.spacing_mut().item_spacing.y = 0.0;
}

/// One menu entry. Menus lay out top-down justified, so the row takes the
/// menu's full width.
pub fn entry(ui: &mut Ui, label: &str, danger: bool) -> Response {
    entry_with_hint(ui, label, "", None, danger)
}

/// A menu entry with a short note on the right (a state, a shortcut).
pub fn entry_with_hint(
    ui: &mut Ui,
    label: &str,
    hint: &str,
    hint_color: Option<Color32>,
    danger: bool,
) -> Response {
    let p = *pal(ui);
    let font = FontId::proportional(size::BODY);
    let g = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, Color32::PLACEHOLDER);
    let hint_g = (!hint.is_empty()).then(|| {
        ui.painter().layout_no_wrap(
            hint.to_owned(),
            FontId::proportional(size::SMALL),
            Color32::PLACEHOLDER,
        )
    });
    let hint_w = hint_g.as_ref().map(|h| h.size().x + 24.0).unwrap_or(0.0);
    let (rect, resp) =
        ui.allocate_at_least(vec2(g.size().x + 24.0 + hint_w, ITEM_H), Sense::click());
    if ui.is_rect_visible(rect) {
        let hovered = resp.hovered();
        if hovered {
            let fill = if danger {
                p.danger
                    .gamma_multiply(if ui.visuals().dark_mode { 0.16 } else { 0.10 })
            } else {
                p.hover
            };
            ui.painter()
                .rect_filled(rect, CornerRadius::same(radius::S), fill);
        }
        let color = if danger {
            p.danger
        } else if hovered {
            p.text_strong
        } else {
            p.text
        };
        ui.painter().galley(
            pos2(rect.left() + 10.0, rect.center().y - g.size().y / 2.0),
            g,
            color,
        );
        if let Some(h) = hint_g {
            ui.painter().galley(
                pos2(
                    rect.right() - 10.0 - h.size().x,
                    rect.center().y - h.size().y / 2.0,
                ),
                h,
                hint_color.unwrap_or(p.text_3),
            );
        }
    }
    resp
}

/// A hairline between groups of entries.
pub fn separator(ui: &mut Ui) {
    let c = pal(ui).border;
    ui.add_space(4.0);
    let (rect, _) = ui.allocate_at_least(vec2(1.0, 1.0), Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, Stroke::new(1.0, c));
    ui.add_space(4.0);
}

/// An entry that pushes `action` and closes the menu when clicked.
pub fn action(ui: &mut Ui, label: &str, actions: &mut Vec<Action>, action: Action) {
    if entry(ui, label, false).clicked() {
        actions.push(action);
        ui.close();
    }
}

/// [`action`] for a destructive entry.
pub fn danger_action(ui: &mut Ui, label: &str, actions: &mut Vec<Action>, action: Action) {
    if entry(ui, label, true).clicked() {
        actions.push(action);
        ui.close();
    }
}

fn run(ui: &mut Ui, label: &str, actions: &mut Vec<Action>, op: GitOp) {
    action(ui, label, actions, Action::Run(op));
}

fn dialog(ui: &mut Ui, label: &str, actions: &mut Vec<Action>, d: Dialog) {
    action(ui, label, actions, Action::Dialog(d));
}

fn danger_dialog(ui: &mut Ui, label: &str, actions: &mut Vec<Action>, d: Dialog) {
    danger_action(ui, label, actions, Action::Dialog(d));
}

// ---------------------------------------------------------------------------
// Menus
// ---------------------------------------------------------------------------

fn visibility_items(ui: &mut Ui, cx: &MenuCtx, full: &str, actions: &mut Vec<Action>) {
    let hidden = cx.filter.hidden.iter().any(|h| h == full);
    action(
        ui,
        if hidden {
            "Show in graph"
        } else {
            "Hide from graph"
        },
        actions,
        Action::ToggleHidden(full.to_owned()),
    );
    let solo = cx.filter.solo.as_deref() == Some(full);
    action(
        ui,
        if solo {
            "Show all branches"
        } else {
            "Show only this branch"
        },
        actions,
        Action::Solo(if solo { None } else { Some(full.to_owned()) }),
    );
}

/// `origin/feature` -> (`origin`, `feature`), using the longest matching remote name.
pub fn split_remote<'a>(snap: &'a Snapshot, name: &str) -> Option<(&'a str, String)> {
    snap.remotes
        .iter()
        .filter(|r| name.starts_with(&format!("{}/", r.name)))
        .max_by_key(|r| r.name.len())
        .map(|r| (r.name.as_str(), name[r.name.len() + 1..].to_owned()))
}

/// The action a double-click on a ref performs.
pub fn checkout_action(snap: &Snapshot, label: &RefLabel) -> Option<Action> {
    match label.kind {
        RefKind::Local => (!label.is_head).then(|| {
            Action::Run(GitOp::Switch {
                branch: label.name.clone(),
            })
        }),
        RefKind::Remote => snap
            .remote_branch(&label.name)
            .map(|rb| checkout_remote(snap, rb)),
        RefKind::Tag => Some(Action::Dialog(detach_dialog(&label.name))),
    }
}

pub fn checkout_remote(snap: &Snapshot, rb: &RemoteBranch) -> Action {
    match snap.branch(&rb.branch) {
        Some(local) if !local.is_head => Action::Run(GitOp::Switch {
            branch: local.name.clone(),
        }),
        Some(_) => Action::Toast(
            ToastKind::Info,
            format!("{} is already checked out", rb.branch),
        ),
        None => Action::Run(GitOp::SwitchTracking {
            local: rb.branch.clone(),
            remote_branch: rb.name.clone(),
        }),
    }
}

pub fn detach_dialog(rev: &str) -> Dialog {
    Dialog::Confirm {
        title: format!("Check out {rev}?"),
        body: "This leaves you on a detached HEAD. Commits made here are not on any branch until you create one.".into(),
        confirm: "Check out".into(),
        danger: false,
        ops: vec![GitOp::Detach { commit: rev.to_owned() }],
    }
}

fn create_branch_dialog(start: &str, label: &str) -> Dialog {
    Dialog::CreateBranch {
        name: String::new(),
        start: start.to_owned(),
        start_label: label.to_owned(),
        checkout: true,
    }
}

fn merge_rebase_items(ui: &mut Ui, cx: &MenuCtx, from: &str, actions: &mut Vec<Action>) {
    if let Some(head) = cx.head_branch()
        && head != from
    {
        run(
            ui,
            &format!("Merge {from} into {head}"),
            actions,
            GitOp::Merge {
                from: from.to_owned(),
                mode: MergeMode::Default,
            },
        );
        run(
            ui,
            &format!("Rebase {head} onto {from}"),
            actions,
            GitOp::Rebase {
                onto: from.to_owned(),
                branch: None,
            },
        );
        separator(ui);
    }
}

pub fn local_branch(ui: &mut Ui, cx: &MenuCtx, b: &BranchInfo, actions: &mut Vec<Action>) {
    begin(ui, 220.0);
    if !b.is_head {
        run(
            ui,
            &format!("Checkout {}", b.name),
            actions,
            GitOp::Switch {
                branch: b.name.clone(),
            },
        );
        separator(ui);
    }
    merge_rebase_items(ui, cx, &b.name, actions);
    dialog(
        ui,
        "Create branch here…",
        actions,
        create_branch_dialog(&b.name, &b.name),
    );
    dialog(
        ui,
        "Create tag here…",
        actions,
        Dialog::CreateTag {
            name: String::new(),
            target: b.name.clone(),
            target_label: b.name.clone(),
            message: String::new(),
        },
    );
    separator(ui);
    if !cx.snap.remotes.is_empty() {
        let remotes: Vec<String> = cx.snap.remotes.iter().map(|r| r.name.clone()).collect();
        let (remote, set_upstream) =
            match b.upstream.as_deref().and_then(|u| split_remote(cx.snap, u)) {
                Some((r, _)) => (remotes.iter().position(|x| x == r).unwrap_or(0), false),
                None => (0, true),
            };
        dialog(
            ui,
            &format!("Push {}…", b.name),
            actions,
            Dialog::Push {
                branch: b.name.clone(),
                remotes,
                remote,
                set_upstream,
                force: false,
            },
        );
    }
    dialog(
        ui,
        "Rename…",
        actions,
        Dialog::RenameBranch {
            from: b.name.clone(),
            to: b.name.clone(),
        },
    );
    if !b.is_head {
        let remote = b
            .upstream
            .as_deref()
            .filter(|_| !b.upstream_gone)
            .and_then(|u| split_remote(cx.snap, u))
            .map(|(r, br)| (r.to_owned(), br));
        danger_dialog(
            ui,
            "Delete…",
            actions,
            Dialog::DeleteBranch {
                name: b.name.clone(),
                force: false,
                remote,
                delete_remote: false,
            },
        );
    }
    separator(ui);
    action(
        ui,
        "Copy branch name",
        actions,
        Action::Copy(b.name.clone()),
    );
    if let Some(gh) = cx.github
        && b.upstream.is_some()
    {
        action(
            ui,
            "Open on GitHub",
            actions,
            Action::OpenUrl(gh.branch_url(&b.name)),
        );
    }
    separator(ui);
    visibility_items(ui, cx, &b.full, actions);
    if let Some(h) = cx.hook {
        crate::devops::history::hook_ref_menu(ui, h, &b.name, actions);
    }
}

pub fn remote_branch(ui: &mut Ui, cx: &MenuCtx, rb: &RemoteBranch, actions: &mut Vec<Action>) {
    begin(ui, 220.0);
    let checkout = checkout_remote(cx.snap, rb);
    action(ui, &format!("Checkout {}", rb.branch), actions, checkout);
    separator(ui);
    merge_rebase_items(ui, cx, &rb.name, actions);
    dialog(
        ui,
        "Create branch here…",
        actions,
        create_branch_dialog(&rb.name, &rb.name),
    );
    separator(ui);
    danger_dialog(
        ui,
        &format!("Delete {} from {}…", rb.branch, rb.remote),
        actions,
        Dialog::Confirm {
            title: format!("Delete {} from {}?", rb.branch, rb.remote),
            body: "The branch is removed on the remote. Anyone else tracking it will lose it on their next fetch with pruning.".into(),
            confirm: "Delete remote branch".into(),
            danger: true,
            ops: vec![GitOp::DeleteRemoteBranch { remote: rb.remote.clone(), branch: rb.branch.clone() }],
        },
    );
    separator(ui);
    action(
        ui,
        "Copy branch name",
        actions,
        Action::Copy(rb.name.clone()),
    );
    if let Some(gh) = cx.github {
        action(
            ui,
            "Open on GitHub",
            actions,
            Action::OpenUrl(gh.branch_url(&rb.branch)),
        );
    }
    separator(ui);
    visibility_items(ui, cx, &rb.full, actions);
    if let Some(h) = cx.hook {
        crate::devops::history::hook_ref_menu(ui, h, &rb.name, actions);
    }
}

pub fn tag(ui: &mut Ui, cx: &MenuCtx, t: &TagInfo, actions: &mut Vec<Action>) {
    begin(ui, 220.0);
    dialog(
        ui,
        &format!("Checkout {} (detached)…", t.name),
        actions,
        detach_dialog(&t.name),
    );
    dialog(
        ui,
        "Create branch here…",
        actions,
        create_branch_dialog(&t.name, &t.name),
    );
    separator(ui);
    for r in &cx.snap.remotes {
        run(
            ui,
            &format!("Push tag to {}", r.name),
            actions,
            GitOp::PushTag {
                remote: r.name.clone(),
                name: t.name.clone(),
            },
        );
    }
    danger_dialog(
        ui,
        "Delete tag…",
        actions,
        Dialog::Confirm {
            title: format!("Delete tag {}?", t.name),
            body: "Only the local tag is removed. Tags already pushed stay on the remote.".into(),
            confirm: "Delete tag".into(),
            danger: true,
            ops: vec![GitOp::DeleteTag {
                name: t.name.clone(),
            }],
        },
    );
    separator(ui);
    action(ui, "Copy tag name", actions, Action::Copy(t.name.clone()));
    separator(ui);
    let hidden = cx.filter.hidden.contains(&t.full);
    action(
        ui,
        if hidden {
            "Show in graph"
        } else {
            "Hide from graph"
        },
        actions,
        Action::ToggleHidden(t.full.clone()),
    );
    if let Some(h) = cx.hook {
        crate::devops::history::hook_ref_menu(ui, h, &t.name, actions);
    }
}

pub fn stash(ui: &mut Ui, s: &StashInfo, actions: &mut Vec<Action>) {
    begin(ui, 200.0);
    run(ui, "Apply", actions, GitOp::StashApply { index: s.index });
    run(
        ui,
        "Pop (apply and drop)",
        actions,
        GitOp::StashPop { index: s.index },
    );
    separator(ui);
    action(ui, "Copy message", actions, Action::Copy(s.message.clone()));
    separator(ui);
    danger_dialog(
        ui,
        "Drop…",
        actions,
        Dialog::Confirm {
            title: format!("Drop stash@{{{}}}?", s.index),
            body: format!(
                "{}\n\nThe stashed changes cannot be recovered from gitvis afterwards.",
                s.message
            ),
            confirm: "Drop stash".into(),
            danger: true,
            ops: vec![GitOp::StashDrop { index: s.index }],
        },
    );
}

pub fn commit(ui: &mut Ui, cx: &MenuCtx, c: &Commit, actions: &mut Vec<Action>) {
    begin(ui, 240.0);
    let sha = c.id.to_string();
    let labels = cx.snap.refs.get(&c.id).map(|v| v.as_slice()).unwrap_or(&[]);
    let locals: Vec<&RefLabel> = labels
        .iter()
        .filter(|l| l.kind == RefKind::Local && !l.is_head)
        .collect();
    let is_head = cx.snap.head.target() == Some(c.id);

    for l in &locals {
        run(
            ui,
            &format!("Checkout {}", l.name),
            actions,
            GitOp::Switch {
                branch: l.name.clone(),
            },
        );
    }
    if !is_head {
        dialog(
            ui,
            "Checkout commit (detached)…",
            actions,
            detach_dialog(&sha),
        );
    }
    if let Some(head) = cx.head_branch() {
        if !is_head {
            separator(ui);
            for l in &locals {
                run(
                    ui,
                    &format!("Merge {} into {head}", l.name),
                    actions,
                    GitOp::Merge {
                        from: l.name.clone(),
                        mode: MergeMode::Default,
                    },
                );
            }
            run(
                ui,
                &format!("Rebase {head} onto this commit"),
                actions,
                GitOp::Rebase {
                    onto: sha.clone(),
                    branch: None,
                },
            );
            run(
                ui,
                "Cherry-pick this commit",
                actions,
                GitOp::CherryPick {
                    commit: sha.clone(),
                },
            );
        }
        run(
            ui,
            "Revert this commit",
            actions,
            GitOp::Revert {
                commit: sha.clone(),
            },
        );
        separator(ui);
        dialog(
            ui,
            &format!("Reset {head} to this commit…"),
            actions,
            Dialog::Reset {
                branch: head.to_owned(),
                target: sha.clone(),
                target_label: c.short.clone(),
                mode: ResetMode::Mixed,
            },
        );
    }
    separator(ui);
    if entry(ui, "Interactive rebase from here…", false).clicked() {
        actions.push(Action::ShowPage(crate::state::Page::Workbench));
        actions.push(crate::devops::workbench::Act::RebaseFrom(sha.clone()).into());
        ui.close();
    }
    dialog(
        ui,
        "Create branch here…",
        actions,
        create_branch_dialog(&sha, &c.short),
    );
    dialog(
        ui,
        "Create tag here…",
        actions,
        Dialog::CreateTag {
            name: String::new(),
            target: sha.clone(),
            target_label: c.short.clone(),
            message: String::new(),
        },
    );
    separator(ui);
    action(ui, "Copy SHA", actions, Action::Copy(sha.clone()));
    action(ui, "Copy message", actions, Action::Copy(c.summary.clone()));
    if entry(ui, "Where is this commit?", false).clicked() {
        actions.push(crate::devops::ship::Act::WhereIs(c.id).into());
        actions.push(Action::ShowPage(crate::state::Page::Ship));
        ui.close();
    }
    if let Some(gh) = cx.github {
        separator(ui);
        action(
            ui,
            "Open on GitHub",
            actions,
            Action::OpenUrl(gh.commit_url(&sha)),
        );
        let runs: Vec<_> = gh.runs_for_sha(&sha).collect();
        for run in runs.iter().take(6) {
            let outcome = run.outcome();
            let color = crate::views::icons::outcome_color(outcome, pal(ui).text_3);
            if entry_with_hint(
                ui,
                &format!("Show run {} #{}", run.name, run.run_number),
                outcome.label(),
                Some(color),
                false,
            )
            .clicked()
            {
                actions.push(Action::OpenRun {
                    run: run.id,
                    job: None,
                });
                ui.close();
            }
        }
        if runs.len() > 6 {
            action(
                ui,
                &format!("Show all {} runs…", runs.len()),
                actions,
                Action::ShowPage(crate::state::Page::GitHub),
            );
        }
    }
    if let Some(h) = cx.hook {
        crate::devops::hooks::commit_menu(ui, h, c, actions);
    }
}

pub fn wip(ui: &mut Ui, snap: &Snapshot, actions: &mut Vec<Action>) {
    begin(ui, 220.0);
    let st = &snap.status;
    if !st.unstaged.is_empty() {
        run(ui, "Stage all changes", actions, GitOp::StageAll);
    }
    if !st.staged.is_empty() {
        run(
            ui,
            "Unstage all changes",
            actions,
            GitOp::UnstageAll {
                unborn: snap.head.target().is_none(),
            },
        );
    }
    dialog(
        ui,
        "Stash changes…",
        actions,
        Dialog::Stash {
            message: String::new(),
            include_untracked: true,
        },
    );
    if snap.head.target().is_some() {
        separator(ui);
        let untracked: Vec<String> = st
            .unstaged
            .iter()
            .filter(|e| e.kind == gitvis_core::ChangeKind::Untracked)
            .map(|e| e.path.clone())
            .collect();
        let mut ops = vec![GitOp::Reset {
            target: "HEAD".into(),
            mode: ResetMode::Hard,
        }];
        if !untracked.is_empty() {
            ops.push(GitOp::DeleteUntracked { paths: untracked });
        }
        danger_dialog(
            ui,
            "Discard all changes…",
            actions,
            Dialog::Confirm {
                title: "Discard all changes?".into(),
                body: format!(
                    "{} will be reset to the last commit and untracked files deleted. This cannot be undone.",
                    crate::format::plural(st.change_count(), "change", "changes")
                ),
                confirm: "Discard everything".into(),
                danger: true,
                ops,
            },
        );
    }
}
