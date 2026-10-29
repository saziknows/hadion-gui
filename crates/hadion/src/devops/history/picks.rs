//! Several commits picked in the graph (Ctrl+click toggles, Shift+click
//! picks a range from the selected commit, Escape clears) and the details
//! panel for them: the commits, their totals, and Compare, Cherry-pick,
//! Create patch files and Squash / Reword / Drop in an interactive rebase.
//! `RepoState::multi` holds every picked commit (the selection at the
//! first Ctrl+click included), in the order they were picked.

use std::collections::HashSet;

use egui::{RichText, Ui, vec2};
use hadion_core::Oid;
use hadion_core::devops::history::git_line;
use hadion_core::devops::history::multi::{self as core, RebaseAction, Totals};

use super::{Act, Msg};
use crate::devops::ctx::Ctx;
use crate::devops::hooks::HookCx;
use crate::format;
use crate::state::{Action, Page, RepoState, Selection, ToastKind};
use crate::theme::{pal, size};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Row, Stat, Tone};

/// A click in the graph with a modifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickEvent {
    /// Ctrl+click: add or remove one commit.
    Toggle(Oid),
    /// Shift+click: every commit row from the selected commit to this one.
    Range(Oid),
}

#[derive(Default)]
pub struct Picks {
    /// The picked commits (mirror of `RepoState::multi` for lookups).
    pub set: HashSet<Oid>,
    /// Oldest first (graph order).
    pub ordered: Vec<Oid>,
    pub seq: u64,
    pub totals: Option<Result<Totals, String>>,
    pub loading: bool,
    /// Every pick is HEAD or one of its ancestors.
    pub in_head: bool,
    /// How many picks HEAD already contains.
    pub in_head_count: usize,
    pub merges: usize,
    /// The oldest pick has no parent.
    pub root: bool,
    pub authors: usize,
    /// Author times of the oldest and newest pick.
    pub span: (i64, i64),
    /// Where the last Ctrl+click was (a range starts there when nothing
    /// is selected).
    pub anchor: Option<Oid>,
}

/// Recompute what the panel shows and read the totals.
fn changed(cx: &mut Ctx, repo: &mut RepoState) {
    derive(cx, repo, true);
}

/// Recompute the panel's numbers; read the line totals again only when
/// `totals` (the picks changed, not just the snapshot).
fn derive(cx: &mut Ctx, repo: &mut RepoState, totals: bool) {
    let Some(snap) = repo.snapshot.as_ref() else {
        return;
    };
    let pk = &mut repo.devops.history.picks;
    pk.set = repo.multi.iter().copied().collect();
    pk.ordered = core::oldest_first(&repo.multi, &snap.index_of);
    if totals {
        pk.totals = None;
        pk.loading = false;
    }
    if repo.multi.len() < 2 {
        return;
    }
    pk.in_head_count = snap
        .head
        .target()
        .map(|h| core::reachable(h, &pk.ordered, &snap.commits, &snap.index_of).len())
        .unwrap_or(0);
    pk.in_head = pk.in_head_count == pk.ordered.len();
    let commits: Vec<&hadion_core::Commit> = pk
        .ordered
        .iter()
        .filter_map(|id| snap.index_of.get(id).map(|&i| &snap.commits[i]))
        .collect();
    pk.merges = commits.iter().filter(|c| c.parent_ids.len() > 1).count();
    pk.root = commits.first().is_some_and(|c| c.parent_ids.is_empty());
    pk.authors = commits
        .iter()
        .map(|c| c.email.to_lowercase())
        .collect::<HashSet<_>>()
        .len();
    let times: Vec<i64> = commits.iter().map(|c| c.time).collect();
    pk.span = (
        times.iter().copied().min().unwrap_or(0),
        times.iter().copied().max().unwrap_or(0),
    );
    if !totals {
        return;
    }
    pk.seq += 1;
    pk.loading = true;
    let seq = pk.seq;
    let shas: Vec<String> = pk.ordered.iter().map(|o| o.to_string()).collect();
    cx.read(
        format!("Totals of {} picked commits", shas.len()),
        move |dir, log| {
            let mut cmds = Vec::new();
            let r = core::totals(dir, &shas, &mut cmds);
            log.commands.extend(cmds);
            Msg::Totals(seq, r)
        },
    );
}

fn primary(repo: &RepoState) -> Option<Oid> {
    match repo.selection {
        Selection::Commit(id) => Some(id),
        _ => None,
    }
}

/// One commit left: it becomes the plain selection.
fn collapse(cx: &mut Ctx, repo: &mut RepoState) {
    if repo.multi.len() == 1 {
        let only = repo.multi[0];
        repo.multi.clear();
        cx.action(Action::Select(Selection::Commit(only)));
    }
}

pub fn on_snapshot(cx: &mut Ctx, repo: &mut RepoState) {
    let Some(snap) = repo.snapshot.as_ref() else {
        return;
    };
    let before = repo.multi.len();
    repo.multi.retain(|id| snap.index_of.contains_key(id));
    let dropped = repo.multi.len() != before;
    if dropped {
        collapse(cx, repo);
    }
    // Commit indices and HEAD may have moved; the totals only change
    // with the picks.
    if !repo.multi.is_empty() || !repo.devops.history.picks.set.is_empty() {
        derive(cx, repo, dropped);
    }
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::Pick(PickEvent::Toggle(oid)) => {
            if repo.multi.is_empty() {
                if let Some(p) = primary(repo)
                    && p != oid
                {
                    repo.multi.push(p);
                }
                repo.multi.push(oid);
            } else if let Some(i) = repo.multi.iter().position(|x| *x == oid) {
                repo.multi.remove(i);
            } else {
                repo.multi.push(oid);
            }
            repo.devops.history.picks.anchor = Some(oid);
            collapse(cx, repo);
            changed(cx, repo);
        }
        Act::Pick(PickEvent::Range(oid)) => {
            let Some(snap) = repo.snapshot.as_ref() else {
                return;
            };
            let from = primary(repo).or(repo.devops.history.picks.anchor);
            let (Some(from), Some(&to_i)) = (from, snap.index_of.get(&oid)) else {
                return apply(cx, repo, Act::Pick(PickEvent::Toggle(oid)));
            };
            let Some(&from_i) = snap.index_of.get(&from) else {
                return;
            };
            let (lo, hi) = (from_i.min(to_i), from_i.max(to_i));
            repo.multi = snap.commits[lo..=hi].iter().map(|c| c.id).collect();
            collapse(cx, repo);
            changed(cx, repo);
        }
        Act::ClearPicks => {
            repo.multi.clear();
            repo.devops.history.picks.anchor = None;
            changed(cx, repo);
        }
        Act::PickFirst(n) => {
            let Some(snap) = repo.snapshot.as_ref() else {
                return;
            };
            repo.multi = snap.commits.iter().take(n).map(|c| c.id).collect();
            if let Some(first) = repo.multi.first().copied() {
                cx.action(Action::Select(Selection::Commit(first)));
            }
            if repo.page != Page::Graph {
                cx.action(Action::ShowPage(Page::Graph));
            }
            collapse(cx, repo);
            changed(cx, repo);
        }
        Act::MultiCompare => {
            let o = &repo.devops.history.picks.ordered;
            if o.len() < 2 {
                return;
            }
            let (a, b) = (o[0].to_string(), o[o.len() - 1].to_string());
            super::compare::apply(
                cx,
                repo,
                Act::Compare {
                    a,
                    b: Some(b),
                    select: None,
                },
            );
        }
        Act::MultiCherryPick => {
            let pk = &repo.devops.history.picks;
            if let Some(why) = cherry_pick_blocked(repo) {
                cx.toast(ToastKind::Info, why);
                return;
            }
            let shas: Vec<String> = pk.ordered.iter().map(|o| o.to_string()).collect();
            let onto = head_name(repo);
            let already = if pk.in_head_count > 0 {
                format!(
                    " {} of them are already on {onto}: git stops at those with \"the previous cherry-pick is now empty\" (skip them from the bar).",
                    pk.in_head_count
                )
            } else {
                String::new()
            };
            cx.confirm(
                format!("Cherry-pick {} commits onto {onto}?", shas.len()),
                format!(
                    "git applies them oldest first, each as a new commit on {onto}. If one conflicts, the cherry-pick stops there: resolve the files, then Continue or Abort from the bar at the top of the window; the rest stay queued.{already}"
                ),
                "Cherry-pick",
                false,
                vec![git_line(&core::cherry_pick_args(&shas))],
                Act::MultiCherryPickRun(shas).into(),
            );
        }
        Act::MultiCherryPickRun(shas) => {
            let n = shas.len();
            cx.local_change(format!("Cherry-pick {n} commits"), move |dir, log| {
                let args = core::cherry_pick_args(&shas);
                log.command(git_line(&args));
                let r = hadion_core::cli::run(dir, &args, None)
                    .map(|_| format!("Cherry-picked {n} commits"))
                    .map_err(|e| e.to_string());
                (r, None::<Msg>)
            });
        }
        Act::MultiPatches(dir) => {
            let pk = &repo.devops.history.picks;
            if pk.ordered.len() < 2 {
                return;
            }
            let dir = match dir {
                Some(d) => d,
                None => match rfd::FileDialog::new()
                    .set_title("Save the patch files in…")
                    .set_directory(&repo.workdir)
                    .pick_folder()
                {
                    Some(d) => d,
                    None => return,
                },
            };
            let shas: Vec<String> = pk.ordered.iter().map(|o| o.to_string()).collect();
            let cmds: Vec<String> = core::format_patch_commands(&dir, &shas)
                .iter()
                .map(|a| git_line(a))
                .collect();
            let merges = pk.merges;
            cx.confirm(
                format!("Create {} patch files?", shas.len()),
                format!(
                    "One file per commit, numbered oldest first (0001-…, 0002-…), in {}. Apply them elsewhere with git am.{}",
                    dir.display(),
                    if merges > 0 {
                        format!(" {merges} picked merge commits produce no patch: git format-patch skips merges.")
                    } else {
                        String::new()
                    }
                ),
                "Create patch files",
                false,
                cmds,
                Act::MultiPatchesRun(dir, shas).into(),
            );
        }
        Act::MultiPatchesRun(dir, shas) => {
            let n = shas.len();
            cx.local_change(format!("Create {n} patch files"), move |workdir, log| {
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    let e = format!("Could not create {}: {e}", dir.display());
                    return (Err(e.clone()), Some(Msg::Patches(Err(e))));
                }
                for args in core::format_patch_commands(&dir, &shas) {
                    log.command(git_line(&args));
                    if let Err(e) = hadion_core::cli::run(workdir, &args, None) {
                        let e = e.to_string();
                        return (Err(e.clone()), Some(Msg::Patches(Err(e))));
                    }
                }
                (
                    Ok(format!("Created {n} patch files in {}", dir.display())),
                    Some(Msg::Patches(Ok(dir))),
                )
            });
        }
        Act::MultiRebase(action) => {
            if let Some(why) = rebase_blocked(repo) {
                cx.toast(ToastKind::Info, why);
                return;
            }
            let pk = &repo.devops.history.picks;
            let shas: Vec<String> = pk.ordered.iter().map(|o| o.to_string()).collect();
            let base = format!("{}^", shas[0]);
            let marks = core::rebase_marks(&shas, action);
            cx.action(Action::ShowPage(Page::Workbench));
            cx.action(crate::devops::workbench::Act::RebasePlan { base, marks });
        }
        _ => {}
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    match msg {
        Msg::Totals(seq, r) => {
            let pk = &mut repo.devops.history.picks;
            if seq == pk.seq {
                pk.loading = false;
                pk.totals = Some(r);
            }
        }
        Msg::Patches(Ok(dir)) => cx.action(Action::OpenPath(dir)),
        _ => {}
    }
}

fn head_name(repo: &RepoState) -> String {
    repo.snapshot
        .as_ref()
        .and_then(|s| s.current_branch().map(|b| b.name.clone()))
        .unwrap_or_else(|| "HEAD (detached)".into())
}

fn cherry_pick_blocked(repo: &RepoState) -> Option<String> {
    let pk = &repo.devops.history.picks;
    if repo.busy.is_some() {
        return Some("Another operation is running.".into());
    }
    if pk.merges > 0 {
        return Some(format!(
            "{} of the picked commits are merges; cherry-picking a merge needs a parent (-m), so pick only regular commits.",
            pk.merges
        ));
    }
    if pk.in_head {
        return Some("Every picked commit is already in HEAD's history.".into());
    }
    None
}

fn rebase_blocked(repo: &RepoState) -> Option<String> {
    let pk = &repo.devops.history.picks;
    if pk.ordered.len() < 2 {
        return Some("Pick at least two commits.".into());
    }
    if !pk.in_head {
        return Some(
            "An interactive rebase rewrites the current branch, and some picked commits are not in HEAD's history. Check out their branch first.".into(),
        );
    }
    if pk.merges > 0 {
        return Some(format!(
            "{} of the picked commits are merges, which an interactive rebase leaves out (it linearises the history).",
            pk.merges
        ));
    }
    if pk.root {
        return Some(
            "The oldest picked commit is the repository's first commit; a rebase from the root is not offered here.".into(),
        );
    }
    None
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

/// The details panel for the picked commits; false when fewer than two.
pub fn panel(ui: &mut Ui, h: &HookCx, picked: &[Oid], actions: &mut Vec<Action>) -> bool {
    if picked.len() < 2 {
        return false;
    }
    let p = *pal(ui);
    let pk = &h.devops.history.picks;
    let snap = h.snap;
    let now = format::now_secs();
    ui.add_space(4.0);
    kit::title(
        ui,
        &format!("{} commits picked", format::count(picked.len())),
    );
    kit::secondary(
        ui,
        "Ctrl+click adds or removes a commit, Shift+click picks every row from the selected one, Esc clears.",
    );
    ui.add_space(4.0);
    let (files, lines) = match &pk.totals {
        Some(Ok(t)) => (
            format::count(t.files),
            format!(
                "+{} \u{2212}{}",
                format::count(t.added as usize),
                format::count(t.deleted as usize)
            ),
        ),
        Some(Err(_)) => ("?".into(), "?".into()),
        None => ("…".into(), "…".into()),
    };
    let span = if pk.span.1 > pk.span.0 {
        format::duration(pk.span.1 - pk.span.0)
    } else {
        "same time".into()
    };
    if ui.available_width() >= 400.0 {
        crate::views::insights::stats(
            ui,
            &[
                Stat::new("Files", files),
                Stat::new("Lines", lines),
                Stat::new("Authors", format::count(pk.authors)),
                Stat::new("Span", span),
            ],
        );
    } else {
        // A narrow panel: one line per number instead of stacked tiles.
        kit::kv(ui, 80.0, |kv| {
            kv.text("Files", files);
            kv.text("Lines", lines);
            kv.text("Authors", format::count(pk.authors));
            kv.text("Span", span);
        });
    }
    if let Some(Ok(t)) = &pk.totals
        && t.capped
    {
        kit::muted(
            ui,
            &format!(
                "Totals cover the oldest {} commits.",
                format::count(core::MAX_TOTALS)
            ),
        );
    }
    if let Some(Err(e)) = &pk.totals {
        kit::muted(ui, &format!("Totals unavailable: {e}"));
    }

    kit::section(ui, "Actions", None);
    let n = picked.len();
    let onto = snap
        .current_branch()
        .map(|b| b.name.clone())
        .unwrap_or_else(|| "HEAD".into());
    let block_cp = if h.busy {
        Some("Another operation is running.".to_owned())
    } else if pk.merges > 0 {
        Some(format!("{} picked commits are merges.", pk.merges))
    } else if pk.in_head {
        Some(format!("All {n} are already on {onto}."))
    } else {
        None
    };
    let block_rb = if !pk.in_head {
        Some("Some picked commits are not in HEAD's history.".to_owned())
    } else if pk.merges > 0 {
        Some(format!("{} picked commits are merges.", pk.merges))
    } else if pk.root {
        Some("The repository's first commit is picked.".to_owned())
    } else if h.busy {
        Some("Another operation is running.".to_owned())
    } else {
        None
    };
    action_row(
        ui,
        Icon::Merge,
        "Compare oldest with newest",
        "Opens Compare with the oldest picked commit as A and the newest as B.",
        None,
        actions,
        Act::MultiCompare,
    );
    action_row(
        ui,
        Icon::Branch,
        &format!("Cherry-pick onto {onto}"),
        &format!("git cherry-pick, oldest first: {n} new commits on {onto}."),
        block_cp.as_deref(),
        actions,
        Act::MultiCherryPick,
    );
    action_row(
        ui,
        Icon::Download,
        "Create patch files…",
        "git format-patch: one numbered .patch per commit in a folder you choose.",
        None,
        actions,
        Act::MultiPatches(None),
    );
    ui.add_space(6.0);
    ui.label(
        RichText::new("In an interactive rebase of the current branch")
            .size(size::SMALL)
            .family(crate::theme::medium())
            .color(p.text_2),
    );
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for a in [
            RebaseAction::Squash,
            RebaseAction::Reword,
            RebaseAction::Drop,
        ] {
            let icon = match a {
                RebaseAction::Squash => Icon::Merge,
                RebaseAction::Reword => Icon::Edit,
                RebaseAction::Drop => Icon::Trash,
            };
            if IconButton::new(icon, a.label())
                .compact()
                .enabled(block_rb.is_none())
                .show(ui)
                .clicked()
            {
                actions.push(Act::MultiRebase(a).into());
            }
        }
    });
    match &block_rb {
        Some(why) => {
            kit::muted(ui, why);
        }
        None => {
            kit::secondary(
                ui,
                "Opens Tools > Interactive rebase from the oldest pick's parent with these commits marked (squash moves them next to the oldest). Nothing runs until you start it there.",
            );
        }
    }

    kit::section_with(ui, "Picked commits", Some(n), |ui| {
        if IconButton::new(Icon::Close, "Clear")
            .compact()
            .show(ui)
            .on_hover_text("Clear the picked commits (Esc)")
            .clicked()
        {
            actions.push(Act::ClearPicks.into());
        }
    });
    // Newest first, like the graph.
    let rows: Vec<Oid> = pk.ordered.iter().rev().copied().collect();
    let per = match &pk.totals {
        Some(Ok(t)) => Some(&t.per_commit),
        _ => None,
    };
    // Rows edge to edge: `show_rows` (inside `kit::list`) adds the
    // caller's item spacing to the row pitch.
    ui.spacing_mut().item_spacing.y = 0.0;
    kit::list(
        ui,
        "hist_picked",
        rows.len(),
        kit::ROW_H,
        |_| false,
        |ui, rect, resp, i| {
            let id = rows[i];
            let Some(c) = snap.index_of.get(&id).map(|&k| &snap.commits[k]) else {
                return;
            };
            let mut sub = format!("{}  ·  {}", c.short, c.author);
            if let Some((f, a, d)) = per.and_then(|m| m.get(&id.to_string())) {
                sub.push_str(&format!(
                    "  ·  {} +{a} \u{2212}{d}",
                    format::plural(*f, "file", "files")
                ));
            }
            let meta = format::relative(c.time, now);
            let badges: Vec<(String, Tone)> = if c.parent_ids.len() > 1 {
                vec![("Merge".into(), Tone::Neutral)]
            } else {
                Vec::new()
            };
            kit::paint_row(
                ui,
                rect,
                &Row::new(&c.summary)
                    .subtitle(&sub)
                    .meta(&meta)
                    .badges(&badges),
                false,
            );
            if resp.clicked() {
                actions.push(Action::JumpTo(id));
            }
        },
    );
    true
}

/// A button with its one-line explanation (or why it is unavailable).
fn action_row(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    help: &str,
    blocked: Option<&str>,
    actions: &mut Vec<Action>,
    act: Act,
) {
    ui.horizontal(|ui| {
        if IconButton::new(icon, label)
            .compact()
            .enabled(blocked.is_none())
            .show(ui)
            .clicked()
        {
            actions.push(act.into());
        }
    });
    match blocked {
        Some(why) => {
            kit::muted(ui, why);
        }
        None => {
            kit::secondary(ui, help);
        }
    }
    ui.add_space(2.0);
}
