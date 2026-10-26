//! File history: every commit that changed one file (renames followed),
//! with the commit's diff of the file, a jump to the graph and "open this
//! version".

use std::collections::HashMap;
use std::path::PathBuf;

use egui::{Frame, Margin, RichText, Sense, Ui, vec2};
use gitvis_core::Oid;
use gitvis_core::devops::workbench::git_line;
use gitvis_core::devops::workbench::history::{
    self as core, FileCommit, FileHistory, MAX_COMMITS, suggest,
};

use super::{Act, Msg, ui as wui};
use crate::devops::ctx::Ctx;
use crate::format;
use crate::state::{Action, DiffDoc, RepoState};
use crate::theme::{pal, size};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Row, Stat, Tone};

#[derive(Default)]
pub struct History {
    pub input: String,
    pub files: Option<Result<Vec<String>, String>>,
    pub files_loading: bool,
    /// Suggestions for `suggest_for`.
    pub suggest_for: String,
    pub suggestions: Vec<String>,
    /// The pointer was over the suggestion list last frame.
    pub popup_hover: bool,
    /// A suggestion was taken: hide the list until the text changes.
    pub dismissed: bool,
    pub result: Option<Result<FileHistory, String>>,
    pub loading: bool,
    pub selected: Option<usize>,
    /// Diffs by `sha:path`; `None` while loading.
    pub diffs: HashMap<String, Option<Result<DiffDoc, String>>>,
}

impl History {
    fn commits(&self) -> &[FileCommit] {
        match &self.result {
            Some(Ok(h)) => &h.commits,
            _ => &[],
        }
    }
}

fn diff_key(c: &FileCommit) -> String {
    format!("{}:{}", c.sha, c.path)
}

fn load_files(cx: &mut Ctx, repo: &mut RepoState) {
    let h = &mut repo.devops.workbench.history;
    if h.files_loading {
        return;
    }
    h.files_loading = true;
    cx.read("List tracked files", |dir, log| {
        let mut cmds = Vec::new();
        let r = core::tracked_files(dir, &mut cmds);
        log.commands.extend(cmds);
        Msg::TrackedFiles(r)
    });
}

pub fn shown(cx: &mut Ctx, repo: &mut RepoState) {
    if repo.devops.workbench.history.files.is_none() {
        load_files(cx, repo);
    }
}

fn load_diff(cx: &mut Ctx, repo: &mut RepoState, c: FileCommit) {
    let key = diff_key(&c);
    let h = &mut repo.devops.workbench.history;
    if h.diffs.contains_key(&key) {
        return;
    }
    h.diffs.insert(key.clone(), None);
    let dark = cx.dark;
    cx.read(
        format!("Diff of {} in {}", c.path, c.short()),
        move |dir, log| {
            let args = core::show_args(&c.sha, &c.path, c.renamed_from.as_deref());
            log.command(git_line(&args));
            let r = gitvis_core::cli::run(dir, &args, None)
                .map_err(|e| e.to_string())
                .map(|out| {
                    let mut diffs = gitvis_core::diff::parse(&out.stdout);
                    if diffs.is_empty() {
                        return DiffDoc {
                            diff: Default::default(),
                            spans: Vec::new(),
                            language: None,
                        };
                    }
                    let diff = diffs.remove(0);
                    let spans = crate::highlight::highlight(&diff, &c.path, dark);
                    DiffDoc {
                        language: crate::highlight::language_for(&c.path),
                        diff,
                        spans,
                    }
                });
            Msg::HistoryDiff(key, r)
        },
    );
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::FileHistory(path) => {
            let path = path.trim().replace('\\', "/");
            let h = &mut repo.devops.workbench.history;
            h.input = path.clone();
            h.suggest_for = path.clone();
            h.dismissed = true;
            if path.is_empty() || h.loading {
                return;
            }
            repo.devops.workbench.tab = super::Tab::History;
            let h = &mut repo.devops.workbench.history;
            h.loading = true;
            h.selected = None;
            h.diffs.clear();
            if h.files.is_none() {
                load_files(cx, repo);
            }
            cx.read(format!("File history of {path}"), move |dir, log| {
                let mut cmds = Vec::new();
                let r = core::history(dir, &path, MAX_COMMITS, &mut cmds);
                log.commands.extend(cmds);
                Msg::History(r)
            });
        }
        Act::SelectHistory(sel) => {
            let h = &mut repo.devops.workbench.history;
            let sel = sel.filter(|i| *i < h.commits().len());
            h.selected = sel;
            if let Some(i) = sel {
                let c = h.commits()[i].clone();
                load_diff(cx, repo, c);
            }
        }
        Act::OpenVersion { sha, path } => {
            cx.read(
                format!("Save {path} at {}", &sha[..10.min(sha.len())]),
                move |dir, log| {
                    let spec = format!("{sha}:{path}");
                    log.command(git_line(&["show", spec.as_str()]));
                    let r = gitvis_core::cli::run(dir, &["show", &spec], None)
                        .map_err(|e| e.to_string())
                        .and_then(|out| {
                            let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
                            let folder = std::env::temp_dir()
                                .join("gitvis-versions")
                                .join(&sha[..10.min(sha.len())]);
                            std::fs::create_dir_all(&folder).map_err(|e| {
                                format!("Could not create {}: {e}", folder.display())
                            })?;
                            let file: PathBuf = folder.join(name);
                            std::fs::write(&file, &out.stdout)
                                .map_err(|e| format!("Could not write {}: {e}", file.display()))?;
                            Ok(file)
                        });
                    Msg::VersionSaved(r)
                },
            );
        }
        _ => {}
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    let h = &mut repo.devops.workbench.history;
    match msg {
        Msg::TrackedFiles(r) => {
            h.files_loading = false;
            h.files = Some(r);
            h.suggest_for.clear();
        }
        Msg::History(r) => {
            h.loading = false;
            h.result = Some(r);
        }
        Msg::HistoryDiff(key, r) => {
            h.diffs.insert(key, Some(r));
        }
        Msg::VersionSaved(Ok(p)) => cx.action(Action::OpenPath(p)),
        Msg::VersionSaved(Err(e)) => cx.toast(
            crate::state::ToastKind::Error,
            format!("Could not open the version: {e}"),
        ),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

pub fn show(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let now = format::now_secs();
    let graph: std::collections::HashSet<String> = repo
        .snapshot
        .as_ref()
        .map(|s| s.commits.iter().map(|c| c.id.to_string()).collect())
        .unwrap_or_default();
    let h = &mut repo.devops.workbench.history;
    kit::page_header(ui, "File history", super::Tab::History.hint(), |ui| {
        if let Some(Ok(r)) = &h.result {
            ui.label(
                RichText::new(format!(
                    "{}  ·  {}{}",
                    format::plural(r.commits.len(), "commit", "commits"),
                    format::millis(r.elapsed_ms),
                    if r.truncated { "  ·  capped" } else { "" }
                ))
                .size(size::SMALL)
                .color(p.text_2),
            );
        }
    });
    query_bar(ui, h, actions);
    if h.loading {
        wui::empty_pane(ui, |ui| {
            kit::loading(ui, &format!("Reading the history of {}…", h.input))
        });
        return;
    }
    let result = match &h.result {
        None => {
            wui::empty_pane(ui, |ui| {
                kit::empty_state(
                    ui,
                    Icon::Clock,
                    "Pick a file",
                    "Type a path (letters in order are enough: \"kitrs\" finds src/views/kit.rs) and press Enter. The list shows every commit that changed it, following renames, with each commit's diff. From a commit's file list, right-click a file and choose File history.",
                    None,
                );
            });
            return;
        }
        Some(Err(e)) => {
            let e = e.clone();
            let input = h.input.clone();
            wui::empty_pane(ui, |ui| {
                if wui::error_card(ui, "Could not read the history", &e, "Try again") {
                    actions.push(Act::FileHistory(input).into());
                }
            });
            return;
        }
        Some(Ok(r)) => r.clone(),
    };
    let selected = h.selected;
    wui::list_pane(ui, "wb_hist_list", 420.0, |ui| {
        kit::list_header(ui, "Commits", Some(result.commits.len()), |ui| {
            if selected.is_some() && kit::link(ui, "Overview").clicked() {
                actions.push(Act::SelectHistory(None).into());
            }
        });
        ui.add_space(6.0);
        if result.commits.is_empty() {
            kit::muted(ui, "No commit changed this file.");
            return;
        }
        kit::list(
            ui,
            "wb_hist_rows",
            result.commits.len(),
            kit::ROW_H,
            |i| selected == Some(i),
            |ui, rect, resp, i| {
                let c = &result.commits[i];
                let mut badges: Vec<(String, Tone)> = Vec::new();
                if c.renamed_from.is_some() {
                    badges.push(("Renamed".into(), Tone::Accent));
                }
                if c.parents.len() > 1 {
                    badges.push(("Merge".into(), Tone::Neutral));
                }
                if c.binary {
                    badges.push(("Binary".into(), Tone::Neutral));
                }
                let mut sub = format!(
                    "{}  ·  {}  ·  +{} −{}",
                    c.short(),
                    c.author,
                    c.added,
                    c.deleted
                );
                if c.path != result.path {
                    sub.push_str(&format!("  ·  {}", c.path));
                }
                let meta = format::relative(c.time, now);
                kit::paint_row(
                    ui,
                    rect,
                    &Row::new(&c.subject)
                        .subtitle(&sub)
                        .meta(&meta)
                        .badges(&badges),
                    selected == Some(i),
                );
                if resp.clicked() {
                    actions.push(Act::SelectHistory(Some(i)).into());
                }
                if resp.double_clicked()
                    && graph.contains(&c.sha)
                    && let Ok(oid) = Oid::from_str(&c.sha)
                {
                    actions.push(Action::JumpTo(oid));
                }
                resp.context_menu(|ui| {
                    if graph.contains(&c.sha)
                        && ui.button("Show in graph").clicked()
                        && let Ok(oid) = Oid::from_str(&c.sha)
                    {
                        actions.push(Action::JumpTo(oid));
                        ui.close();
                    }
                    if ui.button("Open this version").clicked() {
                        actions.push(
                            Act::OpenVersion {
                                sha: c.sha.clone(),
                                path: c.path.clone(),
                            }
                            .into(),
                        );
                        ui.close();
                    }
                    if ui.button("Copy commit id").clicked() {
                        actions.push(Action::Copy(c.sha.clone()));
                        ui.close();
                    }
                });
            },
        );
    });
    let h = &repo.devops.workbench.history;
    match selected.and_then(|i| result.commits.get(i)) {
        Some(c) => wui::detail_pane(ui, ("wb_hist", &c.sha), |ui| {
            detail(ui, h, c, graph.contains(&c.sha), now, actions);
        }),
        None => wui::detail_pane(ui, "wb_hist_overview", |ui| {
            overview(ui, &result, now, actions);
        }),
    }
}

fn query_bar(ui: &mut Ui, h: &mut History, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    Frame::NONE
        .inner_margin(Margin {
            left: 24,
            right: 16,
            top: 10,
            bottom: 10,
        })
        .show(ui, |ui| {
            let mut submit = false;
            let resp = ui
                .horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let width = (ui.available_width() - 150.0).clamp(260.0, 760.0);
                    let resp = ui
                        .allocate_ui(vec2(width, 28.0), |ui| {
                            kit::search_box(
                                ui,
                                "wb_hist_path",
                                &mut h.input,
                                "File path, e.g. src/main.rs (fuzzy)",
                            )
                        })
                        .inner;
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submit = true;
                    }
                    if IconButton::new(Icon::Clock, "Show history")
                        .compact()
                        .primary(true)
                        .show(ui)
                        .clicked()
                    {
                        submit = true;
                    }
                    resp
                })
                .inner;
            // Suggestions follow the text.
            if h.input != h.suggest_for {
                h.suggest_for = h.input.clone();
                h.dismissed = false;
                h.suggestions = match &h.files {
                    Some(Ok(files)) if !h.input.trim().is_empty() => suggest(&h.input, files, 8)
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    _ => Vec::new(),
                };
            }
            let exact = matches!(&h.files, Some(Ok(f)) if f.iter().any(|x| *x == h.input.trim()));
            if submit {
                let path = if !exact && !h.dismissed {
                    h.suggestions
                        .first()
                        .cloned()
                        .unwrap_or_else(|| h.input.clone())
                } else {
                    h.input.clone()
                };
                actions.push(Act::FileHistory(path).into());
            }
            let open = (resp.has_focus() || h.popup_hover)
                && !h.dismissed
                && !h.suggestions.is_empty()
                && !exact;
            h.popup_hover = false;
            if open {
                let area = egui::Area::new(egui::Id::unique("wb_hist_suggest"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(resp.rect.left_bottom() + vec2(0.0, 4.0))
                    .show(ui.ctx(), |ui| {
                        Frame::popup(ui.style()).fill(p.overlay).show(ui, |ui| {
                            ui.set_width(resp.rect.width());
                            ui.spacing_mut().item_spacing.y = 0.0;
                            for s in h.suggestions.clone() {
                                let (rect, r) = ui.allocate_exact_size(
                                    vec2(ui.available_width(), 26.0),
                                    Sense::click(),
                                );
                                kit::row_background(ui, rect, r.hovered(), false);
                                let (dir, name) = format::split_path(&s);
                                let painter = ui.painter();
                                let rr = painter.text(
                                    rect.left_center() + vec2(8.0, 0.0),
                                    egui::Align2::LEFT_CENTER,
                                    dir,
                                    egui::FontId::monospace(size::MONO),
                                    p.text_3,
                                );
                                painter.text(
                                    egui::pos2(rr.right(), rect.center().y),
                                    egui::Align2::LEFT_CENTER,
                                    name,
                                    egui::FontId::monospace(size::MONO),
                                    p.text_strong,
                                );
                                if r.clicked() {
                                    h.input = s.clone();
                                    h.dismissed = true;
                                    actions.push(Act::FileHistory(s.clone()).into());
                                }
                            }
                            if let Some(Ok(f)) = &h.files {
                                ui.add_space(4.0);
                                kit::muted(
                                    ui,
                                    &format!(
                                        "Best of {} tracked files; Enter takes the first",
                                        format::count(f.len())
                                    ),
                                );
                            }
                        });
                    });
                h.popup_hover = ui.rect_contains_pointer(area.response.rect);
            }
            if h.files_loading {
                ui.add_space(2.0);
                kit::muted(ui, "Reading the tracked files for suggestions…");
            } else if let Some(Err(e)) = &h.files {
                kit::muted(ui, &format!("No suggestions: {e}"));
            }
        });
    kit::hairline(ui);
}

fn detail(
    ui: &mut Ui,
    h: &History,
    c: &FileCommit,
    in_graph: bool,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let mut badges: Vec<(String, Tone)> = Vec::new();
    if c.renamed_from.is_some() {
        badges.push(("Renamed".into(), Tone::Accent));
    }
    if c.parents.len() > 1 {
        badges.push(("Merge".into(), Tone::Neutral));
    }
    if c.parents.is_empty() {
        badges.push(("First commit".into(), Tone::Neutral));
    }
    if !in_graph {
        badges.push(("Not in the graph".into(), Tone::Neutral));
    }
    crate::views::insights::detail_title(ui, &c.subject, &badges);
    ui.add_space(4.0);
    kit::kv(ui, 120.0, |kv| {
        kv.row("Commit", |ui| wui::copyable(ui, &c.sha, actions));
        kv.text("Author", format!("{} <{}>", c.author, c.email));
        kv.text(
            "Date",
            format!(
                "{}  ·  {}",
                format::absolute(c.time),
                format::relative(c.time, now)
            ),
        );
        kv.text("Path then", kit::mono(&c.path));
        if let Some(old) = &c.renamed_from {
            kv.text("Renamed from", kit::mono(old));
        }
        kv.row("Changes", |ui| wui::churn(ui, c.added, c.deleted));
    });
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
        if IconButton::new(Icon::External, "Open this version")
            .compact()
            .show(ui)
            .on_hover_text("git show COMMIT:PATH into a temporary file, opened with its program")
            .clicked()
        {
            actions.push(
                Act::OpenVersion {
                    sha: c.sha.clone(),
                    path: c.path.clone(),
                }
                .into(),
            );
        }
        if IconButton::new(Icon::Copy, "Copy git show command")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::Copy(git_line(&core::show_args(
                &c.sha,
                &c.path,
                c.renamed_from.as_deref(),
            ))));
        }
    });
    kit::section(ui, "Change to the file", None);
    match h.diffs.get(&diff_key(c)) {
        None | Some(None) => kit::loading(ui, "Loading the diff…"),
        Some(Some(Err(e))) => kit::callout(ui, Tone::Danger, "Could not load the diff", e),
        Some(Some(Ok(doc))) => {
            if let Some(lang) = &doc.language {
                kit::muted(ui, lang);
            }
            wui::diff_view(ui, &c.sha, doc);
        }
    }
}

fn overview(ui: &mut Ui, r: &FileHistory, now: i64, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    kit::title(ui, &r.path);
    kit::secondary(
        ui,
        "Select a commit for its diff of the file. Renames are followed (git log --follow), so older commits may show an earlier name.",
    );
    ui.add_space(4.0);
    let first = r.commits.last();
    let last = r.commits.first();
    let (a, d) = r.churn();
    let first_abs = first.map(|c| format::absolute(c.time)).unwrap_or_default();
    let last_abs = last.map(|c| format::absolute(c.time)).unwrap_or_default();
    crate::views::insights::stats(
        ui,
        &[
            Stat::new("Commits", format::count(r.commits.len())),
            Stat::new("Authors", format::count(r.authors.len())),
            Stat::new(
                "First commit",
                first
                    .map(|c| format::relative(c.time, now))
                    .unwrap_or_else(|| "–".into()),
            )
            .tip(&first_abs),
            Stat::new(
                "Last change",
                last.map(|c| format::relative(c.time, now))
                    .unwrap_or_else(|| "–".into()),
            )
            .tip(&last_abs),
            Stat::new("Renames", format::count(r.renames())),
            Stat::new(
                "Churn",
                format!(
                    "+{} −{}",
                    format::count(a as usize),
                    format::count(d as usize)
                ),
            )
            .tip("Lines added and removed over all the commits"),
        ],
    );
    if r.truncated {
        kit::callout(
            ui,
            Tone::Warning,
            "Capped",
            &format!(
                "Only the newest {} commits are listed.",
                format::count(MAX_COMMITS)
            ),
        );
    }
    if r.names.len() > 1 {
        kit::section(ui, "Names over time, newest first", Some(r.names.len()));
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for n in &r.names {
                ui.label(kit::mono(n).color(p.text));
            }
        });
    }
    if !r.authors.is_empty() {
        kit::section(ui, "Authors", Some(r.authors.len()));
        kit::count_bars(ui, &r.authors, p.accent, 15);
    }
    kit::section(ui, "Command", None);
    kit::code_block(ui, &r.command, actions);
}
