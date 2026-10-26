//! Pieces shared by the Packages, Kubernetes and Terraform tabs.

use std::hash::Hash;
use std::path::PathBuf;

use egui::{Frame, Margin, RichText, Sense, Ui};

use gitvis_core::devops::runtime::matching::{Distance, MatchKind, RefMatch};
use gitvis_core::{Commit, Oid, Snapshot, repo};

use super::{Act, RefCache};
use crate::devops::console::Console;
use crate::format;
use crate::state::Action;
use crate::theme::{pal, size, space};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Tone};

/// What the views need from the repository.
pub struct Cx<'a> {
    pub snap: Option<&'a Snapshot>,
    pub now: i64,
    pub github: bool,
    pub workdir: PathBuf,
}

impl Cx<'_> {
    pub fn commit(&self, id: Oid) -> Option<&Commit> {
        let s = self.snap?;
        s.index_of.get(&id).and_then(|i| s.commits.get(*i))
    }

    pub fn in_graph(&self, id: Oid) -> bool {
        self.snap.is_some_and(|s| s.index_of.contains_key(&id))
    }
}

/// Badges of a list row.
pub type Badges = Vec<(String, Tone)>;

/// `3m ago`.
pub fn ago(t: Option<i64>, now: i64) -> String {
    t.map(|t| format::relative(t, now)).unwrap_or_default()
}

/// `2026-10-26 16:46 · 3m ago`, `2026-10-02 17:40 · 24 days ago`.
pub fn when(t: Option<i64>, now: i64) -> String {
    match t {
        Some(t) => format!("{}  ·  {}", format::absolute(t), since(t, now)),
        None => "unknown".into(),
    }
}

/// `3m ago` for recent times, `24 days ago`, `5 months ago`, `2 years ago`.
pub fn since(t: i64, now: i64) -> String {
    let d = now - t;
    if d < 14 * 86_400 {
        return format::relative(t, now);
    }
    let days = d / 86_400;
    if days < 60 {
        format!("{days} days ago")
    } else if days < 730 {
        format!("{} months ago", days / 30)
    } else {
        format!("{} years ago", days / 365)
    }
}

/// kubectl's AGE column: `40s`, `12m`, `5h`, `3d`, `2y`.
pub fn age(t: Option<i64>, now: i64) -> String {
    let Some(t) = t else {
        return "–".into();
    };
    let s = (now - t).max(0);
    if s < 120 {
        format!("{s}s")
    } else if s < 3600 * 2 {
        format!("{}m", s / 60)
    } else if s < 86_400 * 2 {
        format!("{}h", s / 3600)
    } else if s < 86_400 * 730 {
        format!("{}d", s / 86_400)
    } else {
        format!("{}y", s / (86_400 * 365))
    }
}

/// `fetched 2m ago · 412 ms`.
pub fn fetched(at: Option<i64>, ms: Option<u64>, now: i64) -> String {
    match (at, ms) {
        (Some(t), Some(ms)) if ms > 0 => {
            format!(
                "fetched {} · {}",
                format::relative(t, now),
                format::millis(ms)
            )
        }
        (Some(t), _) => format!("fetched {}", format::relative(t, now)),
        _ => String::new(),
    }
}

/// The list pane on the left (the frame of `kit::split_keyed`).
pub fn list_pane(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    egui::Panel::left(id)
        .resizable(true)
        .default_size(400.0)
        .size_range(300.0..=700.0)
        .frame(Frame::NONE.fill(p.panel).inner_margin(Margin {
            left: 8,
            right: 8,
            top: 12,
            bottom: 6,
        }))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui)
        });
}

/// The scrollable detail pane (as in `kit::split_keyed`).
pub fn detail_pane(ui: &mut Ui, salt: impl Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(salt)
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
                            add(ui)
                        });
                });
        });
}

/// A full-width page in the remaining area (empty and error states).
pub fn page(ui: &mut Ui, salt: &str, add: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(salt)
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
                            add(ui)
                        });
                });
        });
}

/// A console run in a resizable panel at the bottom of the remaining area,
/// with a way to hide it. Returns false (and draws nothing) when the run
/// is gone.
pub fn console_panel(
    ui: &mut Ui,
    id: &str,
    console: &Console,
    run: Option<u64>,
    hide: Act,
    actions: &mut Vec<Action>,
) -> bool {
    let Some(run) = run.and_then(|r| console.get(r)) else {
        return false;
    };
    let p = *pal(ui);
    let fill = if ui.visuals().dark_mode {
        p.chrome
    } else {
        p.canvas
    };
    let avail = ui.available_height();
    egui::Panel::bottom(id)
        .resizable(true)
        .default_size((avail * 0.34).max(200.0))
        .size_range(140.0..=(avail - 120.0).max(160.0))
        .frame(Frame::NONE.fill(fill))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height(ui.available_height());
            kit::hairline(ui);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.label(kit::caption_text(&p, "Console"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(8.0);
                    if IconButton::new(Icon::ChevronDown, "Hide")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(hide.clone().into());
                    }
                });
            });
            crate::views::console::output(ui, run, id, actions);
        });
    true
}

/// A missing tool: its name, why it is needed, how to install it, and a
/// "Check again" button.
pub fn missing_tool(
    ui: &mut Ui,
    title: &str,
    why: &str,
    hints: &[&str],
    check: Act,
    actions: &mut Vec<Action>,
) {
    kit::callout_with(ui, Tone::Warning, title, why, |ui| {
        ui.add_space(4.0);
        kit::secondary(ui, "Install it with:");
        for h in hints {
            kit::code_block(ui, h, actions);
        }
        ui.add_space(4.0);
        if IconButton::new(Icon::Rerun, "Check again")
            .compact()
            .show(ui)
            .on_hover_text("Look for it on PATH again")
            .clicked()
        {
            actions.push(check.clone().into());
        }
    });
}

/// An error with its full text, an optional hint (with a command to copy)
/// and a retry button.
pub fn error_block(
    ui: &mut Ui,
    title: &str,
    message: &str,
    hint: Option<(&str, Option<&str>)>,
    retry: Option<(&str, Act)>,
    actions: &mut Vec<Action>,
) {
    kit::callout_with(ui, Tone::Danger, title, "", |ui| {
        ui.add_space(2.0);
        kit::code_block(ui, message, actions);
        if let Some((text, cmd)) = hint {
            if !text.is_empty() {
                ui.add_space(2.0);
                kit::body(ui, text);
            }
            if let Some(c) = cmd {
                kit::code_block(ui, c, actions);
            }
        }
        if let Some((label, act)) = retry {
            ui.add_space(4.0);
            if IconButton::new(Icon::Rerun, label)
                .compact()
                .show(ui)
                .clicked()
            {
                actions.push(act.into());
            }
        }
    });
}

/// Short text for a match: `commit 9a9d91b · 3 commits behind origin/main`.
pub fn match_text(m: &RefMatch, dist: Option<&Distance>) -> String {
    let rel = dist.map(Distance::text).filter(|t| !t.is_empty());
    let with = |s: String| match &rel {
        Some(r) => format!("{s} · {r}"),
        None => s,
    };
    match m.kind {
        MatchKind::Commit => with(format!("commit {}", m.name)),
        MatchKind::Tag => with(format!("tag {}", m.name)),
        MatchKind::Branch => format!("branch {} (moves with each push)", m.name),
        MatchKind::PullRequest => format!("pull request {}", m.name),
        MatchKind::Floating => format!("{} (floating, no commit)", m.via),
    }
}

/// A badge for a match kind.
pub fn match_badge(m: &RefMatch) -> (String, Tone) {
    match m.kind {
        MatchKind::Commit => ("Commit".into(), Tone::Accent),
        MatchKind::Tag => ("Git tag".into(), Tone::Accent),
        MatchKind::Branch => ("Branch".into(), Tone::Neutral),
        MatchKind::PullRequest => ("Pull request".into(), Tone::Purple),
        MatchKind::Floating => ("Floating".into(), Tone::Neutral),
    }
}

/// A commit as an accent link (when it is in the graph) with its summary.
pub fn commit_line(ui: &mut Ui, cx: &Cx, id: Oid, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let shown = cx.in_graph(id);
        let resp = ui.add(
            egui::Label::new(kit::mono(repo::short_id(id)).color(if shown {
                p.accent
            } else {
                p.text_2
            }))
            .sense(Sense::click()),
        );
        let resp = if shown {
            resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        } else {
            resp
        };
        if resp.clicked() && shown {
            actions.push(Action::JumpTo(id));
        }
        resp.context_menu(|ui| {
            if ui.button("Copy commit id").clicked() {
                actions.push(Action::Copy(id.to_string()));
                ui.close();
            }
        });
        match cx.commit(id) {
            Some(c) => {
                ui.add(egui::Label::new(RichText::new(&c.summary).color(p.text)).wrap());
                ui.label(
                    RichText::new(format!(
                        "{} · {}",
                        c.author,
                        format::relative(c.time, cx.now)
                    ))
                    .size(size::SMALL)
                    .color(p.text_2),
                );
            }
            None => {
                ui.label(
                    RichText::new("not in the loaded history (fetch, or the branch is hidden)")
                        .size(size::SMALL)
                        .color(p.text_2),
                );
            }
        }
    });
}

/// Rows describing which commit something is: the commit, how far it is
/// from HEAD and the base branch, and a jump to the graph.
pub fn match_rows(
    ui: &mut Ui,
    cx: &Cx,
    refs: &mut RefCache,
    m: &RefMatch,
    label: &str,
    actions: &mut Vec<Action>,
) {
    let dist = m.commit.map(|id| refs.distance(cx.snap, id));
    kit::kv(ui, 120.0, |kv| {
        kv.row(label, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (b, t) = match_badge(m);
                kit::badge(ui, &b, t);
                ui.label(kit::mono(&m.via));
                if m.via != m.name && m.kind != MatchKind::Commit {
                    ui.label(RichText::new(format!("→ {}", m.name)).color(pal(ui).text_2));
                }
            });
        });
        if let Some(id) = m.commit {
            kv.row(
                if m.kind == MatchKind::Branch {
                    "Branch tip now"
                } else {
                    "Commit"
                },
                |ui| commit_line(ui, cx, id, actions),
            );
            if let Some(d) = &dist {
                let mut parts = Vec::new();
                if let Some((name, behind, ahead)) = &d.base {
                    parts.push(gitvis_core::devops::runtime::matching::relation(
                        *behind, *ahead, name,
                    ));
                }
                if let Some((behind, ahead)) = d.head {
                    let head =
                        gitvis_core::devops::runtime::matching::relation(behind, ahead, "HEAD");
                    if !parts.contains(&head) {
                        parts.push(head);
                    }
                }
                if parts.is_empty() {
                    kv.muted("Distance", "Not in the loaded history");
                } else {
                    let truncated = refs.idx.as_ref().is_some_and(|i| i.truncated);
                    let mut text = parts.join("  ·  ");
                    if truncated {
                        text.push_str("  (history truncated: at least)");
                    }
                    kv.text("Distance", text);
                }
            }
        }
        match m.kind {
            MatchKind::Branch => kv.muted(
                "Note",
                "A branch tag moves with each push: the image may have been built from an older tip.",
            ),
            MatchKind::Floating => kv.muted(
                "Note",
                "This tag moves with every build and says nothing about the commit. Pin images by a sha tag or a digest.",
            ),
            MatchKind::PullRequest => kv.muted(
                "Note",
                "Built from a pull request's head; the commit is on the pull request branch.",
            ),
            _ => {}
        }
    });
    if let Some(id) = m.commit
        && cx.in_graph(id)
    {
        ui.horizontal(|ui| {
            ui.add_space(120.0);
            if IconButton::new(Icon::Graph, "Show in graph")
                .compact()
                .show(ui)
                .clicked()
            {
                actions.push(Action::JumpTo(id));
            }
        });
    }
}

/// A row of small buttons.
pub fn buttons(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        add(ui)
    });
}

/// A copy button showing the text it copies.
pub fn copy_line(ui: &mut Ui, text: &str, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if IconButton::icon(Icon::Copy)
            .compact()
            .show(ui)
            .on_hover_text("Copy")
            .clicked()
        {
            actions.push(Action::Copy(text.to_owned()));
        }
        ui.add(
            egui::Label::new(kit::mono(text).color(p.text)).wrap_mode(egui::TextWrapMode::Truncate),
        );
    });
}

/// A quiet note (`kit::muted` without its response).
pub fn note(ui: &mut Ui, text: &str) {
    kit::muted(ui, text);
}

/// Space between blocks.
pub fn gap(ui: &mut Ui) {
    ui.add_space(space::XS);
}
