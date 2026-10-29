//! Recovery: lost commits and stashes.

use egui::{Frame, Margin, Ui};

use gitvis_core::insights::recover::{Lost, LostCommit, LostKind};
use gitvis_core::ops::GitOp;
use gitvis_core::repo;

use super::{
    Cell, Cx, button_row, detail_title, dot_color, grid, list_none, list_top, more, muted,
    path_lines, sec, text, when,
};
use crate::format;
use crate::state::{Action, InsightsPage};
use crate::theme::pal;
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Col, Leading, Row, Stat, Tone};

pub fn show(ui: &mut Ui, cx: &Cx, page: &mut InsightsPage, actions: &mut Vec<Action>) {
    if let Some(run) = &page.lost_run
        && kit::progress(ui, run, "Looking for lost work", true)
    {
        actions.push(Action::CancelLost);
    }
    let sel = page.selected.clone();
    match &page.lost {
        None => super::scroll_empty(ui, "ins_lost_empty", |ui| {
            if page.lost_run.is_some() {
                kit::empty_state(
                    ui,
                    Icon::Undo,
                    "Running git fsck…",
                    "Looking for commits and stashes that no ref reaches.",
                    None,
                );
            } else if kit::empty_state(
                ui,
                Icon::Undo,
                "Commits and stashes no branch reaches",
                "Amended or reset-away commits, deleted branches and dropped stashes stay in the object store until git gc expires them. This lists them with their files and offers to bring them back as a branch or stash.",
                Some((Icon::Search, "Find lost work")),
            ) {
                actions.push(Action::RunLost);
            }
        }),
        Some(Err(e)) => {
            Frame::NONE
                .inner_margin(Margin::symmetric(24, 16))
                .show(ui, |ui| {
                    kit::callout(ui, Tone::Danger, "git fsck failed", e);
                    button_row(ui, |ui| {
                        if IconButton::new(Icon::Rerun, "Try again")
                            .compact()
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Action::RunLost);
                        }
                    });
                });
        }
        Some(Ok(lost)) => {
            let selected = sel
                .as_deref()
                .and_then(|k| lost.commits.iter().find(|c| c.id.to_string() == k));
            kit::split_keyed(
                ui,
                "ins_lost",
                sel.as_deref().unwrap_or(""),
                actions,
                |ui, actions| lost_list(ui, cx, lost, &sel, actions),
                |ui, actions| match selected {
                    Some(c) => lost_detail(ui, cx, c, actions),
                    None => overview(ui, cx, lost, actions),
                },
            );
        }
    }
}

fn kind_badge(k: LostKind) -> Option<(String, Tone)> {
    match k {
        LostKind::Commit => None,
        LostKind::Stash => Some(("Stash".into(), Tone::Accent)),
        LostKind::Merge => Some(("Merge".into(), Tone::Neutral)),
    }
}

fn badges_of(c: &LostCommit) -> Vec<(String, Tone)> {
    let mut b: Vec<(String, Tone)> = kind_badge(c.kind).into_iter().collect();
    if c.reflog.is_none() {
        b.push(("Dangling".into(), Tone::Danger));
    }
    if c.has_child {
        b.push(("Chain".into(), Tone::Neutral));
    }
    b
}

fn lost_list(ui: &mut Ui, cx: &Cx, lost: &Lost, sel: &Option<String>, actions: &mut Vec<Action>) {
    list_top(
        ui,
        "Lost commits",
        lost.commits.len(),
        sel.is_some(),
        None,
        actions,
    );
    if lost.commits.is_empty() {
        list_none(ui, "Nothing lost: every commit is reachable from a ref.");
        return;
    }
    let p = *pal(ui);
    kit::list(
        ui,
        "ins_lost_list",
        lost.commits.len(),
        kit::ROW_H,
        |i| sel.as_deref() == Some(lost.commits[i].id.to_string().as_str()),
        |ui, rect, resp, i| {
            let c = &lost.commits[i];
            let badges = badges_of(c);
            let sub = format!(
                "{}  ·  {}  ·  {}",
                repo::short_id(c.id),
                c.author,
                super::count_of(c.files_total as usize, "file", "files")
            );
            let meta = format::relative(c.time, cx.now);
            let dot = if c.reflog.is_none() {
                dot_color(&p, Tone::Danger)
            } else {
                p.text_3
            };
            kit::paint_row(
                ui,
                rect,
                &Row::new(&c.summary)
                    .leading(Leading::Dot(dot))
                    .subtitle(&sub)
                    .meta(&meta)
                    .badges(&badges),
                sel.as_deref() == Some(c.id.to_string().as_str()),
            );
            if resp.clicked() {
                actions.push(Action::SelectInsight(Some(c.id.to_string())));
            }
            resp.context_menu(|ui| {
                if ui.button("Copy commit id").clicked() {
                    actions.push(Action::Copy(c.id.to_string()));
                    ui.close();
                }
            });
        },
    );
}

fn lost_detail(ui: &mut Ui, cx: &Cx, c: &LostCommit, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    detail_title(ui, &c.summary, &badges_of(c));
    ui.add_space(4.0);
    kit::kv(ui, 100.0, |kv| {
        kv.text("Commit", kit::mono(c.id.to_string()));
        kv.text(
            "Kind",
            match c.kind {
                LostKind::Commit => "Commit",
                LostKind::Stash => "Stash",
                LostKind::Merge => "Merge",
            },
        );
        kv.text("When", when(c.time, cx.now));
        kv.text("Author", c.author.as_str());
        if c.parents.is_empty() {
            kv.muted("Parents", "none");
        } else {
            kv.text(
                "Parents",
                kit::mono(
                    c.parents
                        .iter()
                        .map(|p| repo::short_id(*p))
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
            );
        }
        match &c.reflog {
            Some(r) => kv.text("Reflog", kit::mono(r)),
            None => kv.row("Reflog", |ui| {
                ui.label(
                    egui::RichText::new("Not in any reflog: only git fsck finds it")
                        .color(p.danger),
                );
            }),
        }
    });
    if c.has_child {
        kit::secondary(
            ui,
            "Another lost commit builds on this one: restore the newest of the chain to get all of them.",
        );
    }
    kit::section(ui, "Bring it back", None);
    button_row(ui, |ui| {
        match c.kind {
            LostKind::Stash => {
                if IconButton::new(Icon::Stash, "Restore as stash")
                    .primary(true)
                    .enabled(!cx.busy)
                    .show(ui)
                    .on_hover_text(format!("git stash store -m \"{}\" {}", c.summary, c.id))
                    .clicked()
                {
                    actions.push(Action::Run(GitOp::StashStore {
                        commit: c.id.to_string(),
                        message: c.summary.clone(),
                    }));
                }
            }
            _ => {
                if IconButton::new(Icon::Branch, &format!("Branch {}", c.suggested_branch))
                    .primary(true)
                    .enabled(!cx.busy)
                    .show(ui)
                    .on_hover_text(format!("git branch {} {}", c.suggested_branch, c.id))
                    .clicked()
                {
                    actions.push(Action::Run(GitOp::CreateBranch {
                        name: c.suggested_branch.clone(),
                        start: c.id.to_string(),
                        switch: false,
                    }));
                }
                if IconButton::new(Icon::Plus, "Cherry-pick onto HEAD")
                    .enabled(!cx.busy && c.kind == LostKind::Commit)
                    .show(ui)
                    .on_hover_text(format!("git cherry-pick {}", c.id))
                    .clicked()
                {
                    actions.push(Action::Run(GitOp::CherryPick {
                        commit: c.id.to_string(),
                    }));
                }
            }
        }
        if IconButton::new(Icon::Copy, "Copy id")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::Copy(c.id.to_string()));
        }
    });
    kit::code_block(
        ui,
        &match c.kind {
            LostKind::Stash => format!(
                "git stash store -m \"{}\" {}\ngit stash list",
                c.summary, c.id
            ),
            _ => format!(
                "git branch {} {}\ngit show --stat {}",
                c.suggested_branch, c.id, c.id
            ),
        },
        actions,
    );
    kit::section(ui, "Files changed", Some(c.files_total as usize));
    if c.files.is_empty() {
        kit::muted(
            ui,
            if c.files_total == 0 {
                "No file list (older than the newest 200, or an empty commit)."
            } else {
                "No file list."
            },
        );
    } else {
        path_lines(ui, cx, &c.files, c.files.len(), actions);
        more(ui, c.files_total as usize, c.files.len());
    }
}

fn overview(ui: &mut Ui, cx: &Cx, lost: &Lost, actions: &mut Vec<Action>) {
    super::stats(
        ui,
        &[
            Stat::new("Lost commits", format::count(lost.commits.len()))
                .tip("Commit objects no ref reaches"),
            Stat::new("Stashes", lost.stashes.to_string())
                .tone_if(lost.stashes > 0, Tone::Warning)
                .tip("Dropped or cleared stashes"),
            Stat::new("In a reflog", lost.in_reflog.to_string())
                .tip("Still named by a reflog entry"),
            Stat::new("Dangling", lost.dangling.to_string())
                .tone_if(lost.dangling > 0, Tone::Danger)
                .tip("Only git fsck finds them; the next gc removes them"),
            Stat::new("Blobs", format::count(lost.blobs))
                .tip("Unreachable blobs (contents without a commit)"),
            Stat::new("Trees", format::count(lost.trees)).tip("Unreachable trees"),
            Stat::new("Took", format::millis(lost.elapsed_ms)),
        ],
    );
    kit::kv(ui, 200.0, |kv| {
        kv.text("gc.reflogExpire", lost.reflog_expire.as_str());
        kv.text(
            "gc.reflogExpireUnreachable",
            lost.reflog_expire_unreachable.as_str(),
        );
    });
    if lost.cancelled {
        kit::callout(ui, Tone::Warning, "Cancelled", "The list is partial.");
    }
    ui.add_space(4.0);
    kit::callout_with(
        ui,
        Tone::Accent,
        "Why it matters",
        "A lost commit is not gone: its objects stay until git gc prunes them.",
        |ui| {
            let small = |t: &str| egui::RichText::new(t).size(crate::theme::size::SMALL);
            kit::kv(ui, 110.0, |kv| {
                kv.text(
                    "Pruned when",
                    small(
                        "older than gc.pruneExpire (two weeks by default) and no reflog names them",
                    ),
                );
                kv.text(
                    "Restoring",
                    small("as a branch makes it permanent again: select a commit on the left to see its files and restore it"),
                );
            });
        },
    );
    let stashes: Vec<&LostCommit> = lost
        .commits
        .iter()
        .filter(|c| c.kind == LostKind::Stash)
        .collect();
    if !stashes.is_empty() {
        kit::section(ui, "Dropped stashes", Some(stashes.len()));
        let rows: Vec<Vec<Cell>> = stashes
            .iter()
            .take(20)
            .map(|s| {
                vec![
                    text(&s.summary),
                    Cell::Mono(repo::short_id(s.id)),
                    muted(format::relative(s.time, cx.now)),
                    sec(super::count_of(s.files_total as usize, "file", "files")),
                ]
            })
            .collect();
        grid(
            ui,
            "ins_lost_stashes",
            &[
                Col::text("Stash"),
                Col::text("Commit"),
                Col::text("When"),
                Col::num("Files"),
            ],
            &rows,
            true,
            |r, resp| {
                let s = stashes[r];
                if resp.clicked() {
                    actions.push(Action::SelectInsight(Some(s.id.to_string())));
                }
                resp.context_menu(|ui| {
                    if ui
                        .add_enabled(!cx.busy, egui::Button::new("Restore as stash"))
                        .clicked()
                    {
                        actions.push(Action::Run(GitOp::StashStore {
                            commit: s.id.to_string(),
                            message: s.summary.clone(),
                        }));
                        ui.close();
                    }
                    if ui.button("Copy commit id").clicked() {
                        actions.push(Action::Copy(s.id.to_string()));
                        ui.close();
                    }
                });
            },
        );
        more(ui, stashes.len(), 20);
    }
    kit::section(ui, "Commands", None);
    kit::code_block(ui, &lost.commands.join("\n"), actions);
}
