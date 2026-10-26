//! Branches and Releases.

use egui::Ui;

use gitvis_core::Oid;
use gitvis_core::insights::InsightsReport;
use gitvis_core::insights::refs::BranchNote;
use gitvis_core::ops::GitOp;
use gitvis_core::repo;

use super::{
    Cell, CommitLine, Cx, button_row, commit_menu, commit_ref, commit_table, count_of, days_text,
    detail_title, grid, list_none, list_top, more, muted, num, path_lines, sec, text, when, why,
};
use crate::format;
use crate::state::{Action, Dialog, InsightsPage};
use crate::theme::pal;
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Col, Leading, Row, Stat, Tone};

// ---------------------------------------------------------------------------
// Branches
// ---------------------------------------------------------------------------

/// Branches shown by the list, in order (local, then remote when enabled),
/// filtered by the page filter.
pub fn branch_list<'a>(rep: &'a InsightsReport, page: &InsightsPage) -> Vec<&'a BranchNote> {
    visible_branches(rep, &page.filter, page.show_remote)
}

fn visible_branches<'a>(
    rep: &'a InsightsReport,
    filter: &str,
    remote: bool,
) -> Vec<&'a BranchNote> {
    let needle = filter.trim().to_lowercase();
    let b = &rep.refs.branches;
    b.local
        .iter()
        .chain(if remote { b.remote.iter() } else { [].iter() })
        .filter(|x| needle.is_empty() || x.name.to_lowercase().contains(&needle))
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub fn branches(
    ui: &mut Ui,
    cx: &Cx,
    rep: &InsightsReport,
    selected: &mut Option<String>,
    filter: &mut String,
    show_remote: &mut bool,
    actions: &mut Vec<Action>,
) {
    let b = &rep.refs.branches;
    let mut remote = *show_remote;
    let items = visible_branches(rep, filter, remote);
    let sel = selected.clone();
    let mut f = filter.clone();
    kit::split_keyed(
        ui,
        "ins_branches",
        sel.as_deref().unwrap_or(""),
        actions,
        |ui, actions| {
            list_top(
                ui,
                "Branches",
                items.len(),
                sel.is_some(),
                Some(("ins_branches_filter", &mut f, "Filter branches")),
                actions,
            );
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                let label = format!("Remote-tracking  {}", format::count(b.remote.len()));
                if kit::chip(ui, &label, remote).clicked() {
                    remote = !remote;
                }
            });
            ui.add_space(6.0);
            if items.is_empty() {
                list_none(ui, "No branches match.");
                return;
            }
            let p = *pal(ui);
            kit::list(
                ui,
                "ins_branches_list",
                items.len(),
                kit::ROW_H,
                |i| sel.as_deref() == Some(items[i].full.as_str()),
                |ui, rect, resp, i| {
                    let x = items[i];
                    let mut badges = branch_badges(x);
                    badges.truncate(3);
                    let meta = format::relative(x.tip_time, cx.now);
                    let color = if x.is_head { p.accent } else { p.text_3 };
                    kit::paint_row(
                        ui,
                        rect,
                        &Row::new(&x.name)
                            .leading(Leading::Icon(Icon::Branch, color))
                            .subtitle(&x.summary)
                            .meta(&meta)
                            .badges(&badges)
                            .dimmed(x.merged && !x.is_head),
                        sel.as_deref() == Some(x.full.as_str()),
                    );
                    if resp.clicked() {
                        actions.push(Action::SelectInsight(Some(x.full.clone())));
                    }
                    resp.context_menu(|ui| branch_menu(ui, x, actions));
                },
            );
        },
        |ui, actions| match sel
            .as_deref()
            .and_then(|k| b.local.iter().chain(b.remote.iter()).find(|x| x.full == k))
        {
            Some(x) => branch_detail(ui, cx, x, actions),
            None => branches_overview(ui, cx, rep, actions),
        },
    );
    *filter = f;
    *show_remote = remote;
}

/// Badges of a branch, most important first.
fn branch_badges(x: &BranchNote) -> Vec<(String, Tone)> {
    let mut badges: Vec<(String, Tone)> = Vec::new();
    if x.is_head {
        badges.push(("HEAD".into(), Tone::Accent));
    }
    if x.upstream_gone {
        badges.push(("Gone".into(), Tone::Danger));
    }
    if !x.conflicts.is_empty() {
        badges.push((
            count_of(x.conflicts.len(), "conflict", "conflicts"),
            Tone::Danger,
        ));
    }
    if x.stale {
        badges.push(("Stale".into(), Tone::Warning));
    }
    if x.merged && !x.is_head {
        badges.push(("Merged".into(), Tone::Neutral));
    }
    let ahead = x.ahead.filter(|a| *a > 0);
    let behind = x.behind.filter(|b| *b > 0);
    if ahead.is_some() || behind.is_some() {
        let mut t = String::new();
        if let Some(a) = ahead {
            t.push_str(&format!("↑{a}"));
        }
        if let Some(b) = behind {
            if !t.is_empty() {
                t.push(' ');
            }
            t.push_str(&format!("↓{b}"));
        }
        badges.push((t, Tone::Neutral));
    }
    if !x.duplicates.is_empty() {
        badges.push((
            count_of(x.duplicates.len(), "duplicate", "duplicates"),
            Tone::Neutral,
        ));
    }
    badges
}

fn delete_dialog(x: &BranchNote) -> Action {
    Action::Dialog(Dialog::DeleteBranch {
        name: x.name.clone(),
        force: !x.merged,
        remote: None,
        delete_remote: false,
    })
}

fn branch_menu(ui: &mut Ui, x: &BranchNote, actions: &mut Vec<Action>) {
    if ui.button("Show tip in the graph").clicked() {
        actions.push(Action::JumpTo(x.tip));
        ui.close();
    }
    if ui.button("Copy name").clicked() {
        actions.push(Action::Copy(x.name.clone()));
        ui.close();
    }
    if !x.is_remote && !x.is_head {
        ui.separator();
        if ui.button("Switch to branch").clicked() {
            actions.push(Action::Run(GitOp::Switch {
                branch: x.name.clone(),
            }));
            ui.close();
        }
        if ui.button("Delete branch…").clicked() {
            actions.push(delete_dialog(x));
            ui.close();
        }
    }
}

fn branch_detail(ui: &mut Ui, cx: &Cx, x: &BranchNote, actions: &mut Vec<Action>) {
    detail_title(ui, &x.name, &branch_badges(x));
    ui.add_space(4.0);
    kit::kv(ui, 130.0, |kv| {
        kv.text("Full name", kit::mono(&x.full));
        kv.row("Tip", |ui| commit_ref(ui, cx, x.tip, &x.summary, actions));
        kv.text("Author", x.author.as_str());
        kv.text("Last commit", when(x.tip_time, cx.now));
        kv.text(
            "Merged into HEAD",
            if x.is_head {
                "This is HEAD"
            } else if x.merged {
                "Yes"
            } else {
                "No"
            },
        );
        if let Some(u) = &x.upstream {
            kv.text(
                "Upstream",
                if x.upstream_gone {
                    format!("{u} (gone)")
                } else {
                    u.clone()
                },
            );
        }
        if let (Some(a), Some(b)) = (x.ahead, x.behind) {
            kv.text("Ahead / behind", format!("{a} ahead, {b} behind"));
        }
        if let Some(u) = x.unique {
            kv.text(
                "Unique commits",
                format!("{} not reachable from HEAD", format::count(u as usize)),
            );
        }
        if x.analysed {
            kv.text(
                "Files changed",
                format!(
                    "{} since the merge base with HEAD",
                    format::count(x.changed_files as usize)
                ),
            );
        }
    });
    button_row(ui, |ui| {
        if IconButton::new(Icon::Graph, "Show tip")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::JumpTo(x.tip));
        }
        if !x.is_remote && !x.is_head {
            if IconButton::new(Icon::Branch, "Switch")
                .compact()
                .enabled(!cx.busy)
                .show(ui)
                .clicked()
            {
                actions.push(Action::Run(GitOp::Switch {
                    branch: x.name.clone(),
                }));
            }
            if IconButton::new(Icon::Close, "Delete…")
                .compact()
                .danger()
                .show(ui)
                .clicked()
            {
                actions.push(delete_dialog(x));
            }
        }
        if IconButton::new(Icon::Copy, "Copy name")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::Copy(x.name.clone()));
        }
    });

    if x.upstream_gone {
        why(
            ui,
            "The upstream branch was deleted on the remote (usually after its pull request merged). The local branch is a leftover: delete it, or re-point it with git branch --unset-upstream.",
        );
    } else if x.merged && !x.is_head {
        why(
            ui,
            "Everything on this branch is already in HEAD. Deleting it loses nothing; it only removes clutter.",
        );
    } else if x.stale {
        why(
            ui,
            "No commit for more than 90 days and not merged. Either finish it, rebase it, or delete it; stale branches hide the ones that matter.",
        );
    }

    if x.analysed {
        kit::section(ui, "Predicted merge conflicts", Some(x.conflicts.len()));
        if x.conflicts.is_empty() {
            kit::muted(
                ui,
                "No file was changed on both this branch and HEAD since their merge base: a merge should apply cleanly.",
            );
        } else {
            kit::secondary(
                ui,
                "Files changed on both sides since the merge base and still different. A merge or rebase will stop on some of these.",
            );
            path_lines(ui, cx, &x.conflicts, 60, actions);
            kit::code_block(
                ui,
                &format!(
                    "git merge-tree --write-tree HEAD {}   # dry-run merge (git 2.38 or newer)",
                    x.name
                ),
                actions,
            );
        }
        kit::section(
            ui,
            "Patches already in HEAD or upstream",
            Some(x.duplicates.len()),
        );
        if x.duplicates.is_empty() {
            kit::muted(
                ui,
                "git cherry found no commit whose patch already exists elsewhere.",
            );
        } else {
            kit::secondary(
                ui,
                "Commits whose patch-id matches one in the compared ref: cherry-picked or re-applied already. A rebase drops them automatically.",
            );
            let rows: Vec<Vec<Cell>> = x
                .duplicates
                .iter()
                .map(|d| vec![id_cell(cx, d.id), text(&d.summary), sec(&d.equivalent_in)])
                .collect();
            grid(
                ui,
                "ins_branch_dupes",
                &[
                    Col::text("Commit"),
                    Col::text("Summary"),
                    Col::text("Already in"),
                ],
                &rows,
                true,
                |r, resp| {
                    let d = &x.duplicates[r];
                    if resp.clicked() && cx.in_graph(&d.id) {
                        actions.push(Action::JumpTo(d.id));
                    }
                    commit_menu(resp, d.id, &d.summary, actions);
                },
            );
        }
    } else if !x.is_remote {
        kit::secondary(
            ui,
            "Conflict prediction and duplicate detection run for the newest 30 local branches.",
        );
    }
}

fn id_cell(cx: &Cx, id: Oid) -> Cell {
    let short = repo::short_id(id);
    if cx.in_graph(&id) {
        Cell::MonoLink(short)
    } else {
        Cell::Mono(short)
    }
}

/// An extra numeric column of a branch table: title and value.
type ExtraCol<'a> = (&'a str, &'a dyn Fn(&BranchNote) -> Cell);

/// A table of branches; a click selects the branch.
fn branch_table(
    ui: &mut Ui,
    cx: &Cx,
    id: &str,
    list: &[&BranchNote],
    extra: Option<ExtraCol>,
    actions: &mut Vec<Action>,
) {
    let mut cols = vec![Col::text("Branch")];
    if let Some((title, _)) = extra {
        cols.push(Col::num(title));
    }
    cols.extend([
        Col::text("Last commit"),
        Col::text("Author"),
        Col::text("Summary"),
    ]);
    let rows: Vec<Vec<Cell>> = list
        .iter()
        .take(100)
        .map(|x| {
            let mut row = vec![text(&x.name)];
            if let Some((_, f)) = extra {
                row.push(f(x));
            }
            row.extend([
                muted(format::relative(x.tip_time, cx.now)),
                sec(&x.author),
                sec(&x.summary),
            ]);
            row
        })
        .collect();
    grid(ui, id, &cols, &rows, true, |r, resp| {
        let x = list[r];
        if resp.clicked() {
            actions.push(Action::SelectInsight(Some(x.full.clone())));
        }
        resp.context_menu(|ui| branch_menu(ui, x, actions));
    });
    more(ui, list.len(), 100);
}

fn branches_overview(ui: &mut Ui, cx: &Cx, rep: &InsightsReport, actions: &mut Vec<Action>) {
    let b = &rep.refs.branches;
    let stale_tip = format!("Not merged and no commit for {} days", b.stale_days);
    super::stats(
        ui,
        &[
            Stat::new("Local", format::count(b.local.len())).tip("Branches under refs/heads"),
            Stat::new("Remote", format::count(b.remote.len())).tip("Remote-tracking branches"),
            Stat::new("Merged", format::count(b.merged))
                .tip("Tips reachable from HEAD: safe to delete"),
            Stat::new("Gone", format::count(b.gone))
                .tone_if(b.gone > 0, Tone::Danger)
                .tip("Upstream deleted on the remote"),
            Stat::new("Stale", format::count(b.stale))
                .tone_if(b.stale > 0, Tone::Warning)
                .tip(&stale_tip),
            Stat::new("Unmerged", format::count(b.unmerged)).tip("Branches with work not in HEAD"),
            Stat::new("Throwaway", format::count(b.throwaway))
                .tone_if(b.throwaway > 0, Tone::Warning)
                .tip("Unmerged branches named tmp/test/wip/old/backup…"),
        ],
    );
    if let Some(h) = &b.head {
        kit::kv(ui, 130.0, |kv| kv.text("Current branch", h.as_str()));
    }
    kit::secondary(
        ui,
        "Select a branch for its detail: unique commits, predicted merge conflicts (files both sides changed since the merge base) and patches already present elsewhere.",
    );

    let conflicting: Vec<&BranchNote> =
        b.local.iter().filter(|x| !x.conflicts.is_empty()).collect();
    if !conflicting.is_empty() {
        kit::section(
            ui,
            "Branches that will conflict with HEAD",
            Some(conflicting.len()),
        );
        branch_table(
            ui,
            cx,
            "ins_br_conflicts",
            &conflicting,
            Some(("Files on both sides", &|x: &BranchNote| {
                num(x.conflicts.len())
            })),
            actions,
        );
    }
    let stale: Vec<&BranchNote> = b
        .local
        .iter()
        .chain(b.remote.iter())
        .filter(|x| x.stale)
        .collect();
    if !stale.is_empty() {
        kit::section(ui, "Stale branches", Some(stale.len()));
        kit::secondary(
            ui,
            &format!(
                "Not merged and no commit for {} days, local and remote-tracking.",
                b.stale_days
            ),
        );
        branch_table(ui, cx, "ins_br_stale", &stale, None, actions);
    }
    let gone: Vec<&BranchNote> = b.local.iter().filter(|x| x.upstream_gone).collect();
    if !gone.is_empty() {
        kit::section(
            ui,
            "Local branches whose upstream is gone",
            Some(gone.len()),
        );
        branch_table(ui, cx, "ins_br_gone", &gone, None, actions);
        let names: Vec<&str> = gone.iter().map(|x| x.name.as_str()).collect();
        kit::code_block(ui, &format!("git branch -D {}", names.join(" ")), actions);
    }
    let merged: Vec<&BranchNote> = b.local.iter().filter(|x| x.merged && !x.is_head).collect();
    if !merged.is_empty() {
        kit::section(ui, "Local branches already merged", Some(merged.len()));
        branch_table(ui, cx, "ins_br_merged", &merged, None, actions);
        let names: Vec<&str> = merged.iter().map(|x| x.name.as_str()).collect();
        kit::code_block(ui, &format!("git branch -d {}", names.join(" ")), actions);
    }
    kit::section(ui, "Remote-tracking refs", None);
    kit::secondary(
        ui,
        "Branches deleted on the remote stay as remote-tracking refs until pruned. Pruning needs the network, so gitvis does not do it unasked.",
    );
    if let Some(n) = b.prunable {
        kit::kv(ui, 130.0, |kv| {
            kv.text(
                "Prunable",
                count_of(n, "remote-tracking ref", "remote-tracking refs"),
            )
        });
    }
    kit::code_block(
        ui,
        "git remote prune origin --dry-run\ngit fetch --prune",
        actions,
    );
}

// ---------------------------------------------------------------------------
// Releases
// ---------------------------------------------------------------------------

pub fn releases(ui: &mut Ui, cx: &Cx, rep: &InsightsReport, actions: &mut Vec<Action>) {
    let r = &rep.refs.releases;
    kit::scroll_page(ui, "ins_releases", |ui| {
        let p = *pal(ui);
        super::stats(
            ui,
            &[
                Stat::new("Last tag", r.last_tag.clone().unwrap_or_else(|| "None".into()))
                    .tip("Newest tag reachable from HEAD (git describe --tags --abbrev=0)"),
                Stat::new(
                    "Since the last tag",
                    r.since_last_days
                        .map(days_text)
                        .unwrap_or_else(|| "–".into()),
                )
                .tone_if(r.since_last_days.is_some_and(|d| d > 180), Tone::Warning),
                Stat::new("Unreleased", format::count(r.unreleased_total as usize))
                    .tip("Commits on HEAD after the last tag"),
                Stat::new(
                    "Cadence",
                    r.cadence_days
                        .map(|d| days_text(d as u32))
                        .unwrap_or_else(|| "–".into()),
                )
                .tip("Mean gap between the newest 10 version tags"),
                Stat::new("Tags", format::count(r.tags.len())),
                Stat::new("Orphan tags", r.orphan.to_string())
                    .tone_if(r.orphan > 0, Tone::Warning)
                    .tip("Tags whose commit is on no branch"),
                Stat::new(
                    "Next version",
                    r.suggested_next.clone().unwrap_or_else(|| "–".into()),
                )
                .tip("Suggested from the unreleased conventional commits: feat → minor, breaking → major, else patch"),
            ],
        );
        kit::kv(ui, 130.0, |kv| {
            kv.text("Version scheme", r.scheme);
            kv.text(
                "Tag kinds",
                format!(
                    "{} annotated, {} lightweight, {} signed",
                    format::count(r.annotated),
                    format::count(r.lightweight),
                    format::count(r.signed)
                ),
            );
        });
        if r.lightweight > r.annotated && r.tags.len() >= 3 {
            why(
                ui,
                "Most tags are lightweight: no tagger, date or message, and nothing to sign. Release tags should be annotated (git tag -a) so the release carries who made it and when.",
            );
        }

        type Unreleased = (Oid, String, String, i64, Option<String>);
        let mut groups: Vec<(String, Vec<&Unreleased>)> = Vec::new();
        for c in &r.unreleased {
            let k = c.4.clone().unwrap_or_else(|| "other".into());
            match groups.iter_mut().find(|(g, _)| *g == k) {
                Some((_, v)) => v.push(c),
                None => groups.push((k, vec![c])),
            }
        }
        kit::section_with(
            ui,
            "Unreleased since the last tag",
            Some(r.unreleased_total as usize),
            |ui| {
                if !groups.is_empty()
                    && IconButton::new(Icon::Copy, "Copy changelog draft")
                        .compact()
                        .show(ui)
                        .clicked()
                {
                    let changelog: String = groups
                        .iter()
                        .map(|(g, cs)| {
                            let lines: Vec<String> = cs
                                .iter()
                                .map(|c| format!("- {} ({})", c.1, &c.0.to_string()[..7]))
                                .collect();
                            format!("## {g}\n{}\n", lines.join("\n"))
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    actions.push(Action::Copy(changelog));
                }
            },
        );
        if r.unreleased.is_empty() {
            kit::muted(
                ui,
                if r.last_tag.is_some() {
                    "Nothing new since the last tag."
                } else {
                    "No tag reachable from HEAD."
                },
            );
        } else {
            if !r.unreleased_by_type.is_empty() {
                kit::count_bars(ui, &r.unreleased_by_type, p.accent, 12);
            }
            kit::secondary(
                ui,
                "Grouped by type as a changelog draft. Click a commit to show it in the graph.",
            );
            let mut rows: Vec<(CommitLine, Vec<Cell>)> = Vec::new();
            let mut hidden = 0;
            for (g, cs) in &groups {
                for (i, (id, summary, author, time, _)) in cs.iter().take(80).enumerate() {
                    rows.push((
                        CommitLine {
                            id: *id,
                            time: *time,
                            author,
                            summary,
                        },
                        vec![if i == 0 { text(g) } else { muted("") }],
                    ));
                }
                hidden += cs.len().saturating_sub(80);
            }
            commit_table(
                ui,
                cx,
                "ins_unreleased",
                &[Col::text("Type")],
                1,
                rows,
                actions,
            );
            more(ui, hidden, 0);
        }

        kit::section(ui, "Version bumps", Some(r.bumps.len()));
        if r.bumps.is_empty() {
            kit::muted(ui, "Fewer than two version-like tags.");
        } else {
            let rows: Vec<Vec<Cell>> = r
                .bumps
                .iter()
                .take(40)
                .map(|b| {
                    let tone = match b.kind {
                        "major" => Tone::Warning,
                        "minor" => Tone::Accent,
                        _ => Tone::Neutral,
                    };
                    vec![
                        sec(&b.from),
                        text(&b.to),
                        super::badges(&[(&capitalised(b.kind), tone)]),
                        muted(format::absolute(b.time)),
                        sec(days_text(b.days)),
                        b.commits
                            .map(|c| num(c as usize))
                            .unwrap_or_else(|| muted("–")),
                    ]
                })
                .collect();
            grid(
                ui,
                "ins_bumps",
                &[
                    Col::text("From"),
                    Col::text("To"),
                    Col::text("Kind"),
                    Col::text("Date"),
                    Col::num("After the previous"),
                    Col::num("Commits"),
                ],
                &rows,
                false,
                |_, _| {},
            );
            more(ui, r.bumps.len(), 40);
        }

        kit::section(ui, "Tags", Some(r.tags.len()));
        if r.tags.is_empty() {
            kit::muted(ui, "No tags.");
        } else {
            let rows: Vec<Vec<Cell>> = r
                .tags
                .iter()
                .take(150)
                .map(|t| {
                    vec![
                        if cx.in_graph(&t.target) {
                            Cell::Tone(t.name.clone(), Tone::Accent)
                        } else {
                            text(&t.name)
                        },
                        if t.annotated {
                            sec("Annotated")
                        } else {
                            muted("Lightweight")
                        },
                        super::badges(&[
                            (if t.signed { "Signed" } else { "" }, Tone::Success),
                            (if t.on_branch { "" } else { "Orphan" }, Tone::Warning),
                            (if t.prerelease { "Pre-release" } else { "" }, Tone::Neutral),
                        ]),
                        muted(format::relative(t.time, cx.now)),
                        sec(&t.tagger),
                        sec(&t.message),
                        t.commits_since_previous
                            .map(|c| num(c as usize))
                            .unwrap_or_else(|| muted("–")),
                    ]
                })
                .collect();
            grid(
                ui,
                "ins_tags",
                &[
                    Col::text("Tag"),
                    Col::text("Kind"),
                    Col::text(""),
                    Col::text("Date"),
                    Col::text("Tagger"),
                    Col::text("Message"),
                    Col::num("Commits since previous"),
                ],
                &rows,
                true,
                |i, resp| {
                    let t = &r.tags[i];
                    if resp.clicked() {
                        actions.push(Action::JumpTo(t.target));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Copy name").clicked() {
                            actions.push(Action::Copy(t.name.clone()));
                            ui.close();
                        }
                        if ui.button("Copy target id").clicked() {
                            actions.push(Action::Copy(t.target.to_string()));
                            ui.close();
                        }
                    });
                },
            );
            more(ui, r.tags.len(), 150);
        }
        ui.add_space(12.0);
    });
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}
