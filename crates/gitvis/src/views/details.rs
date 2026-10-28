//! Right panel: selected commit, or the working tree with staging and commit.

use std::collections::HashMap;

use egui::{
    Align, Align2, CornerRadius, FontId, Key, Layout, Modifiers, Rect, RichText, Sense, Ui, pos2,
    vec2,
};

use gitvis_core::ops::GitOp;
use gitvis_core::secrets::{Place, Report};
use gitvis_core::{
    ChangeKind, CommitDetails, DiffTarget, FileEntry, Head, Pending, RefKind, Snapshot,
};

use crate::devops::hooks::{self, HookCx, WipList};
use crate::format;
use crate::state::{Action, Dialog, Page, RepoState, Selection, Settings};
use crate::theme::{self, Tone, pal, size};
use crate::views::icons::{self, Icon, IconButton};
use crate::views::kit;
use crate::views::truncated;

const FILE_ROW_H: f32 = 28.0;

/// What the commit panel needs besides the commit itself.
pub struct CommitCx<'a> {
    pub snap: &'a Snapshot,
    pub settings: &'a Settings,
    pub selected_file: Option<&'a DiffTarget>,
    pub checks: Option<&'a crate::state::ChecksState>,
    pub has_github: bool,
    pub contains: Option<&'a Option<Vec<String>>>,
    /// The pull request the commit belongs to (Review page link).
    pub pr: Option<u64>,
    /// For the extension hooks (`devops::hooks`).
    pub hook: &'a HookCx<'a>,
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, settings: &Settings, actions: &mut Vec<Action>) {
    let RepoState {
        multi,
        github,
        snapshot,
        selection,
        details,
        details_loading,
        commit_message,
        amend,
        busy,
        file,
        checks,
        contains,
        github_remote,
        sec_page,
        devops,
        ..
    } = repo;
    let Some(snap) = snapshot.as_ref() else {
        return;
    };
    // A copy: the message box below edits the message itself.
    let message_now = commit_message.clone();
    let hook = HookCx {
        snap,
        devops: &*devops,
        settings,
        github_data: github.as_ref(),
        busy: busy.is_some(),
        commit_message: &message_now,
        details: &*details,
    };
    // Several commits picked in the graph: their own panel.
    if !multi.is_empty() && hooks::multi_selection(ui, &hook, multi, actions) {
        return;
    }
    let selected_file = file.as_ref();
    match *selection {
        Selection::None => {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| ui.label(RichText::new("Select a commit").weak()));
        }
        Selection::Wip => {
            wip_panel(
                ui,
                snap,
                commit_message,
                amend,
                busy.is_some(),
                selected_file,
                sec_page.report.as_ref(),
                &hook,
                actions,
            );
        }
        Selection::Commit(id) => {
            let Some(idx) = snap.index_of.get(&id) else {
                return;
            };
            let commit = &snap.commits[*idx];
            match details.get(&id) {
                Some(d) => {
                    let cx = CommitCx {
                        snap,
                        settings,
                        selected_file,
                        checks: checks.get(&id),
                        has_github: github_remote.is_some(),
                        contains: contains.get(&id),
                        pr: github_remote.as_ref().and_then(|_| {
                            crate::devops::review::pr_for_commit(
                                &hook.devops.review,
                                id,
                                &commit.summary,
                            )
                        }),
                        hook: &hook,
                    };
                    commit_panel(ui, &cx, commit, d, actions)
                }
                None => {
                    header(ui, &commit.summary);
                    ui.add_space(8.0);
                    if details_loading.is_some() || true {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().size(14.0));
                            ui.label(RichText::new("Loading details…").weak());
                        });
                    }
                }
            }
        }
    }
}

fn header(ui: &mut Ui, text: &str) {
    ui.add_space(4.0);
    kit::title(ui, text);
}

/// Commit message body as prose: hard-wrapped lines inside a paragraph are
/// joined, blank lines separate paragraphs, list items and indented or
/// trailer lines keep their own line.
fn reflow(body: &str) -> String {
    let mut out = String::new();
    let mut para = String::new();
    let flush = |para: &mut String, out: &mut String| {
        if !para.is_empty() {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(para.trim_end());
            para.clear();
        }
    };
    for line in body.lines() {
        let t = line.trim_end();
        if t.trim().is_empty() {
            flush(&mut para, &mut out);
            continue;
        }
        let trimmed = t.trim_start();
        let numbered = trimmed
            .split_once('.')
            .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        let trailer = trimmed
            .split_once(": ")
            .is_some_and(|(k, _)| !k.is_empty() && !k.contains(' ') && k.contains('-'));
        let own_line = trimmed.starts_with("- ")
            || trimmed.starts_with("* ")
            || trimmed.starts_with('\u{2022}')
            || t.starts_with("    ")
            || t.starts_with('\t')
            || numbered
            || trailer;
        if para.is_empty() {
            para.push_str(t);
        } else if own_line {
            para.push('\n');
            para.push_str(t);
        } else {
            para.push(' ');
            para.push_str(trimmed);
        }
    }
    flush(&mut para, &mut out);
    out
}

fn ref_tone(kind: RefKind, is_head: bool) -> Tone {
    match kind {
        RefKind::Local if is_head => Tone::Accent,
        RefKind::Local => Tone::Success,
        RefKind::Remote => Tone::Neutral,
        RefKind::Tag => Tone::Warning,
    }
}

fn commit_panel(
    ui: &mut Ui,
    cx: &CommitCx,
    commit: &gitvis_core::Commit,
    d: &CommitDetails,
    actions: &mut Vec<Action>,
) {
    let snap = cx.snap;
    let settings = cx.settings;
    let selected_file = cx.selected_file;
    let p = *pal(ui);
    let now = format::now_secs();
    header(ui, &commit.summary);

    // Who and when, in one line under the title.
    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
        crate::views::avatar(ui.painter(), r.center(), 10.0, &d.author, settings.dark);
        let name = ui
            .add(
                egui::Label::new(
                    RichText::new(&d.author)
                        .family(theme::medium())
                        .color(p.text_strong),
                )
                .sense(Sense::click()),
            )
            .on_hover_text(format!(
                "{}\nClick to copy the e-mail address",
                d.author_email
            ))
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if name.clicked() {
            actions.push(Action::Copy(d.author_email.clone()));
        }
        ui.label(
            RichText::new(format!(
                "committed {}",
                format::relative(d.author_time, now)
            ))
            .color(p.text_2),
        )
        .on_hover_text(format::absolute_long(d.author_time));
    });

    // Refs as badges.
    if let Some(labels) = snap.refs.get(&d.id) {
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            for l in labels {
                kit::badge(ui, &l.name, ref_tone(l.kind, l.is_head)).on_hover_text(&l.full);
            }
        });
    }

    // Message body.
    let body = d
        .message
        .split_once('\n')
        .map(|(_, rest)| rest.trim())
        .unwrap_or("");
    if !body.is_empty() {
        ui.add_space(8.0);
        kit::body(ui, &reflow(body));
    }

    // Details.
    kit::section(ui, "Details", None);
    let sha = d.id.to_string();
    kit::kv(ui, 92.0, |kv| {
        if let Some(n) = cx.pr {
            kv.row("Pull request", |ui| {
                if kit::link(ui, &format!("Pull request #{n}")).clicked() {
                    actions.push(Action::OpenPr(n));
                }
            });
        }
        kv.row("Commit", |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(kit::mono(&sha[..12.min(sha.len())]).color(p.text));
                if IconButton::icon(Icon::Copy)
                    .compact()
                    .show(ui)
                    .on_hover_text(format!("Copy {sha}"))
                    .clicked()
                {
                    actions.push(Action::Copy(sha.clone()));
                }
            });
        });
        if !d.parents.is_empty() {
            let label = if d.parents.len() > 1 {
                "Parents"
            } else {
                "Parent"
            };
            kv.row(label, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    for par in &d.parents {
                        let short = gitvis_core::repo::short_id(*par);
                        let in_graph = snap.index_of.contains_key(par);
                        let summary = snap
                            .index_of
                            .get(par)
                            .map(|&i| snap.commits[i].summary.clone())
                            .unwrap_or_default();
                        let color = if in_graph { p.accent } else { p.text_3 };
                        let r = ui.add(
                            egui::Label::new(kit::mono(short).color(color)).sense(Sense::click()),
                        );
                        let r = kit::tip(r, &summary);
                        if in_graph && r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            actions.push(Action::JumpTo(*par));
                        }
                    }
                });
            });
        }
        kv.text(
            "Authored",
            format!(
                "{}  \u{b7}  {}",
                format::absolute_long(d.author_time),
                format::relative(d.author_time, now)
            ),
        );
        let same_person = d.committer == d.author && d.committer_email == d.author_email;
        if !same_person {
            kv.row("Committer", |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&d.committer).color(p.text));
                    ui.label(
                        RichText::new(&d.committer_email)
                            .size(size::SMALL)
                            .color(p.text_3),
                    );
                });
            });
        }
        let delay = d.commit_time - d.author_time;
        if delay.abs() >= 60 {
            kv.text(
                "Committed",
                format!(
                    "{}  \u{b7}  {} after authoring",
                    format::absolute(d.commit_time),
                    format::duration(delay.abs())
                ),
            );
        }
        match &d.signature {
            Some(kind) => kv.row("Signature", |ui| {
                ui.horizontal(|ui| {
                    icons::show(ui, Icon::Check, 13.0, p.success);
                    ui.label(RichText::new(format!("{kind} signature present")).color(p.text))
                        .on_hover_text("The commit object carries a signature; whether it verifies is shown below, under Signature.");
                });
            }),
            None => kv.muted("Signature", "not signed"),
        }
        kv.row("In branches", |ui| match cx.contains {
            None => {
                if kit::link(ui, "Find branches")
                    .on_hover_text("git branch -a --contains: every local and remote branch whose history includes this commit")
                    .clicked()
                {
                    actions.push(Action::FindContaining(d.id));
                }
            }
            Some(None) => kit::loading(ui, "Searching\u{2026}"),
            Some(Some(branches)) => {
                if branches.is_empty() {
                    ui.label(RichText::new("no branch (unreachable)").color(p.text_3));
                } else {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                        for b in branches.iter().take(12) {
                            kit::badge(ui, b, Tone::Neutral);
                        }
                        if branches.len() > 12 {
                            ui.label(
                                RichText::new(format!("and {} more", branches.len() - 12))
                                    .size(size::SMALL)
                                    .color(p.text_3),
                            );
                        }
                        if IconButton::icon(Icon::Copy)
                            .compact()
                            .show(ui)
                            .on_hover_text("Copy the list")
                            .clicked()
                        {
                            actions.push(Action::Copy(branches.join("\n")));
                        }
                    });
                }
            }
        });
        kv.row("Tree", |ui| {
            let tree = d.tree.to_string();
            let r = ui
                .add(
                    egui::Label::new(kit::mono(&tree[..12.min(tree.len())]).color(p.text_3))
                        .sense(Sense::click()),
                )
                .on_hover_text("The tree object this commit points at. Click to copy.")
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if r.clicked() {
                actions.push(Action::Copy(tree.clone()));
            }
        });
    });

    // Checks from GitHub for this commit.
    if cx.has_github {
        crate::views::github::checks_section(ui, d.id, cx.checks, now, actions);
    }

    // Signatures, issue links, AI and other areas' additions.
    hooks::commit_panel(ui, cx.hook, commit, actions);

    // Files.
    let (adds, dels) = d
        .files
        .iter()
        .fold((0, 0), |(a, b), f| (a + f.additions, b + f.deletions));
    kit::section_with(ui, "Files changed", Some(d.files.len()), |ui| {
        if !d.stats_skipped {
            if dels > 0 {
                ui.label(
                    RichText::new(format!("\u{2212}{}", format::count(dels)))
                        .size(size::SMALL)
                        .color(p.danger),
                );
            }
            if adds > 0 {
                ui.label(
                    RichText::new(format!("+{}", format::count(adds)))
                        .size(size::SMALL)
                        .color(p.success),
                );
            }
        } else {
            ui.label(
                RichText::new("line counts skipped (very large commit)")
                    .size(size::SMALL)
                    .color(p.text_3),
            );
        }
    });

    let parent = d.parents.first().copied();
    egui::ScrollArea::vertical()
        .id_salt("commit_files")
        .auto_shrink([false, false])
        .show_rows(ui, FILE_ROW_H, d.files.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for i in range {
                let f = &d.files[i];
                let target = DiffTarget::Commit {
                    id: d.id,
                    parent,
                    path: f.path.clone(),
                    old_path: f.old_path.clone(),
                };
                let selected = selected_file == Some(&target);
                let resp = file_row(
                    ui,
                    f.kind,
                    &f.path,
                    f.old_path.as_deref(),
                    Some((f.additions, f.deletions, f.binary)),
                    selected,
                    false,
                    |_, _| {},
                );
                if resp.clicked() {
                    actions.push(Action::OpenFile(target.clone()));
                }
                let exists = snap.workdir.join(&f.path).is_file();
                resp.context_menu(|ui| {
                    ui.set_min_width(180.0);
                    if exists && ui.button("Open file").clicked() {
                        actions.push(Action::OpenPath(snap.workdir.join(&f.path)));
                        ui.close();
                    }
                    if ui.button("Copy path").clicked() {
                        actions.push(Action::Copy(f.path.clone()));
                        ui.close();
                    }
                    if ui.button("File history").clicked() {
                        actions.push(Action::ShowPage(crate::state::Page::Workbench));
                        actions.push(
                            crate::devops::workbench::Act::FileHistory(f.path.clone()).into(),
                        );
                        ui.close();
                    }
                    hooks::commit_file_menu(ui, cx.hook, commit, &f.path, actions);
                });
            }
        });
}

/// One file row with a kind badge, dimmed directory, file name and
/// optional stats. `extra` draws hover buttons inside the given rect.
#[allow(clippy::too_many_arguments)]
fn file_row(
    ui: &mut Ui,
    kind: ChangeKind,
    path: &str,
    old_path: Option<&str>,
    stats: Option<(usize, usize, bool)>,
    selected: bool,
    has_actions: bool,
    extra: impl FnOnce(&mut Ui, Rect),
) -> egui::Response {
    let pl = *pal(ui);
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), FILE_ROW_H), Sense::click());
    kit::row_background(ui, rect, resp.hovered(), selected);
    let painter = ui.painter().clone();
    let mid = rect.center().y;
    let badge = Rect::from_center_size(pos2(rect.left() + 16.0, mid), vec2(18.0, 18.0));
    let color = theme::kind_color(kind);
    painter.rect_filled(
        badge,
        CornerRadius::same(theme::radius::S),
        color.gamma_multiply(0.18),
    );
    painter.text(
        badge.center(),
        Align2::CENTER_CENTER,
        kind.letter(),
        FontId::new(size::CAPTION, theme::semibold()),
        color,
    );

    let mut right = rect.right() - 10.0;
    // Only rows with actions reserve space on hover; an empty child scope
    // would hand egui a degenerate rect and break the row layout.
    let show_actions = has_actions && resp.hovered();
    let hover_w = if show_actions { 76.0 } else { 0.0 };
    let stat_font = FontId::proportional(size::SMALL);
    if let Some((a, d, binary)) = stats
        && !show_actions
    {
        if binary {
            let r = painter.text(
                pos2(right, mid),
                Align2::RIGHT_CENTER,
                "binary",
                stat_font.clone(),
                pl.text_3,
            );
            right = r.left() - 10.0;
        } else {
            if d > 0 {
                let r = painter.text(
                    pos2(right, mid),
                    Align2::RIGHT_CENTER,
                    format!("\u{2212}{d}"),
                    stat_font.clone(),
                    pl.danger,
                );
                right = r.left() - 6.0;
            }
            if a > 0 {
                let r = painter.text(
                    pos2(right, mid),
                    Align2::RIGHT_CENTER,
                    format!("+{a}"),
                    stat_font.clone(),
                    pl.success,
                );
                right = r.left() - 6.0;
            }
            right -= 4.0;
        }
    }
    if show_actions {
        let extra_rect = Rect::from_min_max(
            pos2(right - hover_w, rect.top()),
            pos2(right, rect.bottom()),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(extra_rect)
                .layout(Layout::right_to_left(Align::Center)),
            |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                extra(ui, extra_rect);
            },
        );
        right -= hover_w;
    }

    let x = rect.left() + 34.0;
    let (dir, name) = format::split_path(path);
    let text_color = if selected { pl.text_strong } else { pl.text };
    let font = FontId::proportional(size::BODY - 0.5);
    let max_w = (right - x).max(10.0);
    let name_galley = painter.layout_no_wrap(name.to_owned(), font.clone(), text_color);
    let name_w = name_galley.size().x.min(max_w);
    let dir_w = (max_w - name_w - 2.0).max(0.0);
    let mut cursor = x;
    if !dir.is_empty() && dir_w > 12.0 {
        let g = truncated(&painter, dir, font.clone(), pl.text_3, dir_w);
        painter.galley(pos2(cursor, mid - g.size().y / 2.0), g.clone(), pl.text_3);
        cursor += g.size().x;
    }
    let g = truncated(&painter, name, font, text_color, (right - cursor).max(10.0));
    painter.galley(pos2(cursor, mid - g.size().y / 2.0), g, text_color);

    // The path is already on the row; only renames need a tooltip.
    match old_path {
        Some(old) => resp.on_hover_text(format!("Renamed from {old}")),
        None => resp,
    }
}

#[allow(clippy::too_many_arguments)]
fn wip_panel(
    ui: &mut Ui,
    snap: &Snapshot,
    message: &mut String,
    amend: &mut bool,
    busy: bool,
    selected_file: Option<&DiffTarget>,
    exposure: Option<&Report>,
    hook: &HookCx,
    actions: &mut Vec<Action>,
) {
    let st = &snap.status;
    let unborn = snap.head.target().is_none();

    // Secrets and sensitive files in the uncommitted changes, per path, and
    // the ones that would go into the next commit.
    let mut flagged: HashMap<&str, usize> = HashMap::new();
    let mut staged_secrets: Vec<String> = Vec::new();
    if let Some(rep) = exposure {
        for f in &rep.findings {
            for o in &f.occurrences {
                if matches!(o.place, Place::Staged | Place::Unstaged | Place::Untracked) {
                    *flagged.entry(o.path.as_str()).or_default() += 1;
                }
            }
            if f.staged {
                let where_ = f
                    .occurrences
                    .iter()
                    .find(|o| o.place == Place::Staged)
                    .map(|o| format!("{}:{}", o.path, o.line))
                    .unwrap_or_default();
                staged_secrets.push(format!("{} · {where_} · {}", f.name, f.redacted()));
            }
        }
        for f in &rep.files {
            if f.staged || f.untracked {
                *flagged.entry(f.path.as_str()).or_default() += 1;
            }
            if f.staged {
                staged_secrets.push(format!("{} · {}", f.rule.name, f.path));
            }
        }
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        kit::title(ui, "Working tree");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if IconButton::new(Icon::Stash, "Stash")
                .compact()
                .enabled(!busy && !st.is_clean())
                .show(ui)
                .on_hover_text("Stash all changes")
                .clicked()
            {
                actions.push(Action::Dialog(Dialog::Stash {
                    message: String::new(),
                    include_untracked: true,
                }));
            }
        });
    });
    ui.add_space(4.0);

    // Commit box goes at the bottom; lists take the rest. Its height is
    // last frame's (the secrets callout and hook rows make it grow).
    let box_h_id = egui::Id::unique("wip_commit_box_h");
    let commit_h = ui
        .data(|d| d.get_temp::<f32>(box_h_id))
        .unwrap_or(150.0)
        .max(150.0);
    let lists_h = (ui.available_height() - commit_h).max(80.0);
    let (lists_rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), lists_h), Sense::hover());
    ui.scope_builder(egui::UiBuilder::new().max_rect(lists_rect), |ui| {
        egui::ScrollArea::vertical()
            .id_salt("wip_lists")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                if !st.conflicted.is_empty() {
                    list_header(ui, "Conflicted", st.conflicted.len(), |_| {});
                    for e in &st.conflicted {
                        let target = DiffTarget::Conflicted {
                            path: e.path.clone(),
                        };
                        let path = e.path.clone();
                        let resp = file_row(
                            ui,
                            e.kind,
                            &e.path,
                            None,
                            None,
                            selected_file == Some(&target),
                            true,
                            |ui, _| {
                                if IconButton::text("Ours")
                                    .compact()
                                    .show(ui)
                                    .on_hover_text("Keep our version")
                                    .clicked()
                                {
                                    actions
                                        .push(Action::Run(GitOp::TakeOurs { path: path.clone() }));
                                }
                                if IconButton::text("Theirs")
                                    .compact()
                                    .show(ui)
                                    .on_hover_text("Take their version")
                                    .clicked()
                                {
                                    actions.push(Action::Run(GitOp::TakeTheirs {
                                        path: path.clone(),
                                    }));
                                }
                            },
                        );
                        if resp.clicked() {
                            // The conflict editor (base / ours / theirs); the
                            // raw diff stays one click away inside it.
                            let _ = target;
                            actions.push(crate::devops::conflict::Act::Open(e.path.clone()).into());
                        }
                        conflict_menu(&resp, snap, e, hook, actions);
                    }
                    ui.add_space(8.0);
                }

                list_header(ui, "Unstaged", st.unstaged.len(), |ui| {
                    if !st.unstaged.is_empty()
                        && IconButton::text("Stage all")
                            .compact()
                            .enabled(!busy)
                            .show(ui)
                            .clicked()
                    {
                        actions.push(Action::Run(GitOp::StageAll));
                    }
                });
                if st.unstaged.is_empty() {
                    empty(ui, "No unstaged changes");
                }
                for e in &st.unstaged {
                    let target = if e.kind == ChangeKind::Untracked {
                        DiffTarget::Untracked {
                            path: e.path.clone(),
                        }
                    } else {
                        DiffTarget::Unstaged {
                            path: e.path.clone(),
                        }
                    };
                    let path = e.path.clone();
                    let kind = e.kind;
                    let resp = file_row(
                        ui,
                        e.kind,
                        &e.path,
                        None,
                        None,
                        selected_file == Some(&target),
                        true,
                        |ui, _| {
                            if IconButton::icon(Icon::Plus)
                                .compact()
                                .show(ui)
                                .on_hover_text("Stage")
                                .clicked()
                            {
                                actions.push(Action::Run(GitOp::Stage {
                                    paths: vec![path.clone()],
                                }));
                            }
                            if IconButton::icon(Icon::Undo)
                                .compact()
                                .show(ui)
                                .on_hover_text(if kind == ChangeKind::Untracked {
                                    "Delete file"
                                } else {
                                    "Discard changes"
                                })
                                .clicked()
                            {
                                actions.push(Action::Dialog(discard_dialog(&path, kind)));
                            }
                        },
                    );
                    if let Some(&n) = flagged.get(e.path.as_str()) {
                        flag_row(ui, &resp, n);
                    }
                    if resp.clicked() {
                        actions.push(Action::OpenFile(target));
                    }
                    unstaged_menu(&resp, snap, e, hook, actions);
                }
                ui.add_space(8.0);

                list_header(ui, "Staged", st.staged.len(), |ui| {
                    if !st.staged.is_empty()
                        && IconButton::text("Unstage all")
                            .compact()
                            .enabled(!busy)
                            .show(ui)
                            .clicked()
                    {
                        actions.push(Action::Run(GitOp::UnstageAll { unborn }));
                    }
                });
                if st.staged.is_empty() {
                    empty(ui, "Nothing staged");
                }
                for e in &st.staged {
                    let target = DiffTarget::Staged {
                        path: e.path.clone(),
                        old_path: e.old_path.clone(),
                    };
                    let path = e.path.clone();
                    let resp = file_row(
                        ui,
                        e.kind,
                        &e.path,
                        e.old_path.as_deref(),
                        None,
                        selected_file == Some(&target),
                        true,
                        |ui, _| {
                            if IconButton::icon(Icon::Minus)
                                .compact()
                                .show(ui)
                                .on_hover_text("Unstage")
                                .clicked()
                            {
                                actions.push(Action::Run(GitOp::Unstage {
                                    paths: vec![path.clone()],
                                    unborn,
                                }));
                            }
                        },
                    );
                    if let Some(&n) = flagged.get(e.path.as_str()) {
                        flag_row(ui, &resp, n);
                    }
                    if resp.clicked() {
                        actions.push(Action::OpenFile(target));
                    }
                    staged_menu(&resp, snap, e, unborn, hook, actions);
                }
                ui.add_space(6.0);
            });
    });

    // Commit box
    let box_top = ui.cursor().top();
    ui.separator();
    let pending = snap.pending;
    let can_commit = !busy
        && (!st.staged.is_empty() || *amend || pending == Pending::Merge)
        && st.conflicted.is_empty();
    let has_message = !message.trim().is_empty();
    let ctrl_enter = ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter));

    let hint = match pending {
        Pending::Merge => "Merge commit message (leave empty to keep git's)",
        _ if *amend => "New message (leave empty to keep the current one)",
        _ => "Summary\n\nDescription",
    };
    if !staged_secrets.is_empty() {
        let title = format!(
            "{} in the staged changes",
            format::plural(staged_secrets.len(), "possible secret", "possible secrets")
        );
        kit::callout_with(ui, Tone::Danger, &title, "", |ui| {
            let p = *pal(ui);
            for s in staged_secrets.iter().take(4) {
                ui.add(
                    egui::Label::new(RichText::new(s).size(size::SMALL).color(p.text_2)).truncate(),
                );
            }
            if staged_secrets.len() > 4 {
                ui.label(
                    RichText::new(format!("and {} more", staged_secrets.len() - 4))
                        .size(size::SMALL)
                        .color(p.text_3),
                );
            }
            if IconButton::new(Icon::Shield, "Review on the Exposure page")
                .compact()
                .show(ui)
                .clicked()
            {
                actions.push(Action::ShowPage(Page::Security));
            }
        });
        ui.add_space(6.0);
    }
    // AI message, issue keys and other areas' tools.
    hooks::wip_message_tools(ui, hook, actions);
    let edit = ui.add(
        egui::TextEdit::multiline(message)
            .id_salt("commit_message")
            .hint_text(hint)
            .desired_rows(3)
            .desired_width(f32::INFINITY),
    );
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let head_exists = !unborn;
        if ui
            .add_enabled(head_exists && !busy, egui::Checkbox::new(amend, "Amend"))
            .on_hover_text("Replace the last commit")
            .changed()
            && *amend
            && message.is_empty()
            && let Some(head) = snap
                .head
                .target()
                .and_then(|h| snap.index_of.get(&h))
                .map(|&i| &snap.commits[i])
        {
            *message = head.summary.clone();
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = match (pending, *amend) {
                (Pending::Merge, _) => "Commit merge".to_owned(),
                (_, true) => "Amend commit".to_owned(),
                _ => format!(
                    "Commit {}",
                    format::plural(st.staged.len(), "file", "files")
                ),
            };
            let allowed = can_commit && (has_message || *amend || pending == Pending::Merge);
            let btn = IconButton::new(Icon::Check, &label)
                .primary(true)
                .enabled(allowed)
                .show(ui)
                .on_hover_text(if allowed {
                    "Ctrl+Enter"
                } else if !st.conflicted.is_empty() {
                    "Resolve conflicts first"
                } else if st.staged.is_empty() && !*amend {
                    "Stage some changes first"
                } else {
                    "Write a commit message"
                });
            let fire = btn.clicked() || (ctrl_enter && edit.has_focus() && allowed);
            if fire {
                let op = if pending == Pending::Merge && !has_message {
                    GitOp::Continue(Pending::Merge)
                } else {
                    GitOp::Commit {
                        message: message.clone(),
                        amend: *amend,
                    }
                };
                if staged_secrets.is_empty() {
                    actions.push(Action::Run(op));
                } else {
                    let list: Vec<String> =
                        staged_secrets.iter().take(8).map(|s| format!("• {s}")).collect();
                    let mut body = format!(
                        "The staged changes contain {}:\n\n{}",
                        format::plural(
                            staged_secrets.len(),
                            "possible secret",
                            "possible secrets"
                        ),
                        list.join("\n")
                    );
                    if staged_secrets.len() > 8 {
                        body.push_str(&format!("\n… {} more", staged_secrets.len() - 8));
                    }
                    body.push_str(
                        "\n\nOnce committed they stay in history even after you delete them; once pushed, everyone who can read the repository has them. Unstage the files or move the values to environment variables first.",
                    );
                    actions.push(Action::Dialog(Dialog::Confirm {
                        title: "Commit possible secrets?".into(),
                        body,
                        confirm: "Commit anyway".into(),
                        danger: true,
                        ops: vec![op],
                    }));
                }
            }
            let p = *pal(ui);
            if let Head::Branch { name, .. } = &snap.head {
                ui.label(
                    RichText::new(format!("to {name}"))
                        .size(size::SMALL)
                        .color(p.text_2),
                );
            } else if let Head::Detached(_) = &snap.head {
                ui.label(
                    RichText::new("detached HEAD")
                        .size(size::SMALL)
                        .color(p.warning),
                );
            }
        });
    });
    let box_h = (ui.cursor().top() - box_top + 8.0).max(150.0);
    if (box_h - commit_h).abs() > 1.0 {
        ui.data_mut(|d| d.insert_temp(box_h_id, box_h));
        ui.ctx().request_repaint();
    }
}

/// Red shield on a working-tree row whose file holds a possible secret.
fn flag_row(ui: &Ui, resp: &egui::Response, n: usize) {
    let rect = resp.rect;
    // Hover actions occupy the right 76 px; sit left of them when shown.
    let x = if resp.hovered() {
        rect.right() - 6.0 - 76.0 - 12.0
    } else {
        rect.right() - 6.0 - 10.0
    };
    icons::paint(
        ui.painter(),
        Icon::Shield,
        pos2(x, rect.center().y),
        13.0,
        theme::RED,
    );
    resp.clone().on_hover_text(format!(
        "{} in this file · see the Exposure page",
        format::plural(n, "possible secret", "possible secrets")
    ));
}

fn list_header(ui: &mut Ui, title: &str, count: usize, extra: impl FnOnce(&mut Ui)) {
    let p = *pal(ui);
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(kit::caption_text(&p, title));
        ui.label(
            RichText::new(count.to_string())
                .size(size::CAPTION)
                .color(p.text_3),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| extra(ui));
    });
    ui.add_space(4.0);
}

fn empty(ui: &mut Ui, text: &str) {
    let p = *pal(ui);
    ui.horizontal(|ui| {
        ui.add_space(34.0);
        ui.label(RichText::new(text).size(size::SMALL).color(p.text_3));
    });
    ui.add_space(4.0);
}

fn discard_dialog(path: &str, kind: ChangeKind) -> Dialog {
    let name = path.rsplit('/').next().unwrap_or(path);
    if kind == ChangeKind::Untracked {
        Dialog::Confirm {
            title: format!("Delete {name}?"),
            body: format!("{path} is untracked. Deleting it cannot be undone."),
            confirm: "Delete file".into(),
            danger: true,
            ops: vec![GitOp::DeleteUntracked {
                paths: vec![path.to_owned()],
            }],
        }
    } else {
        Dialog::Confirm {
            title: format!("Discard changes in {name}?"),
            body: format!("Unstaged changes in {path} are lost. This cannot be undone."),
            confirm: "Discard".into(),
            danger: true,
            ops: vec![GitOp::Discard {
                paths: vec![path.to_owned()],
            }],
        }
    }
}

fn common_file_items(ui: &mut Ui, snap: &Snapshot, path: &str, actions: &mut Vec<Action>) {
    ui.separator();
    if ui.button("Open file").clicked() {
        actions.push(Action::OpenPath(snap.workdir.join(path)));
        ui.close();
    }
    if ui.button("Copy path").clicked() {
        actions.push(Action::Copy(path.to_owned()));
        ui.close();
    }
    if ui.button("File history").clicked() {
        actions.push(Action::ShowPage(crate::state::Page::Workbench));
        actions.push(crate::devops::workbench::Act::FileHistory(path.to_owned()).into());
        ui.close();
    }
}

fn unstaged_menu(
    resp: &egui::Response,
    snap: &Snapshot,
    e: &FileEntry,
    hook: &HookCx,
    actions: &mut Vec<Action>,
) {
    resp.context_menu(|ui| {
        ui.set_min_width(180.0);
        if ui.button("Stage").clicked() {
            actions.push(Action::Run(GitOp::Stage {
                paths: vec![e.path.clone()],
            }));
            ui.close();
        }
        let label = if e.kind == ChangeKind::Untracked {
            "Delete file…"
        } else {
            "Discard changes…"
        };
        if ui.button(RichText::new(label).color(theme::RED)).clicked() {
            actions.push(Action::Dialog(discard_dialog(&e.path, e.kind)));
            ui.close();
        }
        common_file_items(ui, snap, &e.path, actions);
        hooks::wip_file_menu(ui, hook, &e.path, WipList::Unstaged, actions);
    });
}

fn staged_menu(
    resp: &egui::Response,
    snap: &Snapshot,
    e: &FileEntry,
    unborn: bool,
    hook: &HookCx,
    actions: &mut Vec<Action>,
) {
    resp.context_menu(|ui| {
        ui.set_min_width(180.0);
        if ui.button("Unstage").clicked() {
            actions.push(Action::Run(GitOp::Unstage {
                paths: vec![e.path.clone()],
                unborn,
            }));
            ui.close();
        }
        common_file_items(ui, snap, &e.path, actions);
        hooks::wip_file_menu(ui, hook, &e.path, WipList::Staged, actions);
    });
}

fn conflict_menu(
    resp: &egui::Response,
    snap: &Snapshot,
    e: &FileEntry,
    hook: &HookCx,
    actions: &mut Vec<Action>,
) {
    resp.context_menu(|ui| {
        ui.set_min_width(200.0);
        if ui.button("Keep ours").clicked() {
            actions.push(Action::Run(GitOp::TakeOurs {
                path: e.path.clone(),
            }));
            ui.close();
        }
        if ui.button("Take theirs").clicked() {
            actions.push(Action::Run(GitOp::TakeTheirs {
                path: e.path.clone(),
            }));
            ui.close();
        }
        if ui
            .button("Mark as resolved")
            .on_hover_text("Stage the file as it is on disk")
            .clicked()
        {
            actions.push(Action::Run(GitOp::MarkResolved {
                paths: vec![e.path.clone()],
            }));
            ui.close();
        }
        common_file_items(ui, snap, &e.path, actions);
        hooks::wip_file_menu(ui, hook, &e.path, WipList::Conflicted, actions);
    });
}
