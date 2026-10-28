//! Files at a commit: `git ls-tree -r -l` as a folder tree with sizes and
//! a search, and the selected file's content (`git show REV:PATH`, syntax
//! colours, binary and size guards) with Open this version, Blame, File
//! history and Compare with the working tree.

use std::collections::HashSet;
use std::path::PathBuf;

use egui::{FontId, Frame, Margin, RichText, Shape, Stroke, Ui, pos2, vec2};
use gitvis_core::devops::history::git_line;
use gitvis_core::devops::history::tree::{self as core, Content, EntryKind, Tree};

use super::widgets::{self, TextDoc};
use super::{Act, Msg};
use crate::devops::ctx::Ctx;
use crate::format;
use crate::state::{Action, Page, RepoState, Settings, ToastKind};
use crate::theme::{pal, size};
use crate::views::icons::{self, Icon, IconButton};
use crate::views::kit::{self, Tone};
use crate::views::truncated;

/// A file's content, ready to draw.
#[derive(Clone, Debug)]
pub enum ContentShow {
    Text(TextDoc),
    Binary { size: u64 },
    TooLarge { size: u64 },
    Submodule { commit: String },
}

#[derive(Default)]
pub struct Browse {
    /// The revision input.
    pub rev: String,
    pub seq: u64,
    pub loading: bool,
    pub tree: Option<Result<Tree, String>>,
    pub expanded: HashSet<String>,
    pub query: String,
    pub selected: Option<String>,
    /// The selected file's content.
    pub content: Option<(String, Option<Result<ContentShow, String>>)>,
    /// Select this file once the tree is loaded.
    pub select_after: Option<String>,
    /// The flattened rows and what they were made from (tree, search,
    /// open folders), so a large tree is not walked every frame.
    rows_cache: Option<(u64, std::rc::Rc<Vec<(usize, usize)>>)>,
}

impl Browse {
    fn rows(&mut self) -> std::rc::Rc<Vec<(usize, usize)>> {
        use std::hash::{Hash, Hasher};
        let Some(Ok(tree)) = &self.tree else {
            return Default::default();
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        tree.sha.hash(&mut h);
        tree.entries.len().hash(&mut h);
        self.query.hash(&mut h);
        let mut open: Vec<&String> = self.expanded.iter().collect();
        open.sort();
        open.hash(&mut h);
        let key = h.finish();
        if let Some((k, rows)) = &self.rows_cache
            && *k == key
        {
            return rows.clone();
        }
        let rows = std::rc::Rc::new(tree.rows(&self.expanded, &self.query));
        self.rows_cache = Some((key, rows.clone()));
        rows
    }
}

fn load(cx: &mut Ctx, repo: &mut RepoState, rev: String) {
    let b = &mut repo.devops.history.browse;
    let rev = if rev.trim().is_empty() {
        "HEAD".to_owned()
    } else {
        rev.trim().to_owned()
    };
    b.rev = rev.clone();
    b.seq += 1;
    b.loading = true;
    b.content = None;
    b.selected = None;
    let seq = b.seq;
    cx.read(format!("Files at {rev}"), move |dir, log| {
        let mut cmds = Vec::new();
        let r = core::load(dir, &rev, &mut cmds);
        log.commands.extend(cmds);
        Msg::Tree(seq, r)
    });
}

pub fn shown(cx: &mut Ctx, repo: &mut RepoState) {
    let b = &repo.devops.history.browse;
    if b.tree.is_none() && !b.loading {
        let rev = b.rev.clone();
        load(cx, repo, rev);
    }
}

fn select(cx: &mut Ctx, repo: &mut RepoState, path: String) {
    let path = path.trim().replace('\\', "/");
    let b = &mut repo.devops.history.browse;
    let Some(Ok(tree)) = &b.tree else {
        b.select_after = Some(path);
        return;
    };
    let Some(n) = tree.find(&path) else {
        cx.toast(
            ToastKind::Info,
            format!(
                "{path} is not in {} ({})",
                b.rev,
                gitvis_core::devops::history::short(&tree.sha)
            ),
        );
        return;
    };
    let entry = tree.entries[tree.nodes[n].entry.unwrap_or(0)].clone();
    for a in Tree::ancestors(&path) {
        b.expanded.insert(a);
    }
    b.selected = Some(path.clone());
    if matches!(&b.content, Some((p, _)) if *p == path) {
        return;
    }
    b.content = Some((path.clone(), None));
    let sha = tree.sha.clone();
    let seq = b.seq;
    let dark = cx.dark;
    cx.read(
        format!(
            "Read {path} at {}",
            gitvis_core::devops::history::short(&sha)
        ),
        move |dir, log| {
            let mut cmds = Vec::new();
            let r = core::content(dir, &sha, &entry, &mut cmds).map(|c| match c {
                Content::Text(bytes) => ContentShow::Text(TextDoc::new(&bytes, &entry.path, dark)),
                Content::Binary { size } => ContentShow::Binary { size },
                Content::TooLarge { size } => ContentShow::TooLarge { size },
                Content::Submodule { commit } => ContentShow::Submodule { commit },
            });
            log.commands.extend(cmds);
            Msg::Content(seq, path.clone(), r)
        },
    );
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::Browse { rev, path } => {
            repo.devops.history.mode = super::Mode::Files;
            super::show_page(cx, repo);
            let b = &mut repo.devops.history.browse;
            let same = !b.loading
                && b.rev == rev.trim()
                && matches!(&b.tree, Some(Ok(t)) if t.rev == rev.trim());
            if same {
                if let Some(p) = path {
                    select(cx, repo, p);
                }
                return;
            }
            b.select_after = path;
            b.query.clear();
            b.expanded.clear();
            load(cx, repo, rev);
        }
        Act::BrowseRev => {
            let rev = repo.devops.history.browse.rev.clone();
            repo.devops.history.browse.expanded.clear();
            load(cx, repo, rev);
        }
        Act::BrowseSelect(path) => {
            repo.devops.history.mode = super::Mode::Files;
            super::show_page(cx, repo);
            let b = &repo.devops.history.browse;
            if b.tree.is_none() && !b.loading {
                repo.devops.history.browse.select_after = Some(path);
                let rev = repo.devops.history.browse.rev.clone();
                load(cx, repo, rev);
            } else {
                select(cx, repo, path);
            }
        }
        Act::BrowseToggle(dir) => {
            let b = &mut repo.devops.history.browse;
            if !b.expanded.remove(&dir) {
                b.expanded.insert(dir);
            }
        }
        Act::OpenVersion { rev, path } => {
            cx.read(
                format!(
                    "Save {path} at {}",
                    gitvis_core::devops::history::short(&rev)
                ),
                move |dir, log| {
                    let args = core::show_args(&rev, &path);
                    log.command(git_line(&args));
                    let r = gitvis_core::cli::run(dir, &args, None)
                        .map_err(|e| e.to_string())
                        .and_then(|out| {
                            let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
                            let folder = std::env::temp_dir()
                                .join("gitvis-versions")
                                .join(gitvis_core::devops::history::short(&rev));
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
    let b = &mut repo.devops.history.browse;
    match msg {
        Msg::Tree(seq, r) if seq == b.seq => {
            b.loading = false;
            // Open the top-level folders of small trees.
            if let Ok(t) = &r
                && t.nodes[0].children.len() <= 3
            {
                for &c in &t.nodes[0].children {
                    if t.nodes[c].dir {
                        b.expanded.insert(t.nodes[c].path.clone());
                    }
                }
            }
            b.tree = Some(r);
            if let Some(p) = b.select_after.take() {
                select(cx, repo, p);
            }
        }
        Msg::Content(seq, path, r) if seq == b.seq => {
            if let Some((p, slot)) = &mut b.content
                && *p == path
            {
                *slot = Some(r);
            }
        }
        Msg::VersionSaved(Ok(p)) => cx.action(Action::OpenPath(p)),
        Msg::VersionSaved(Err(e)) => {
            cx.toast(ToastKind::Error, format!("Could not open the version: {e}"))
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

pub fn show(ui: &mut Ui, repo: &mut RepoState, _settings: &Settings, actions: &mut Vec<Action>) {
    let suggestions = super::ref_suggestions(repo);
    let workdir = repo.workdir.clone();
    let p = *pal(ui);
    let b = &mut repo.devops.history.browse;
    Frame::NONE
        .inner_margin(Margin {
            left: 24,
            right: 16,
            top: 10,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.spacing_mut().interact_size.y = crate::views::form::INPUT_H;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.label(RichText::new("Revision").size(size::SMALL).color(p.text_2));
                let (r, picked) = widgets::rev_input(
                    ui,
                    "browse_rev",
                    &mut b.rev,
                    "HEAD, a branch, a tag or a SHA",
                    &suggestions,
                    260.0,
                    false,
                );
                if widgets::submitted(ui, &r) || picked {
                    actions.push(Act::BrowseRev.into());
                }
                if crate::views::form::primary(ui, "Show files", !b.loading).clicked() {
                    actions.push(Act::BrowseRev.into());
                }
                if let Some(Ok(t)) = &b.tree {
                    let (bytes, files) = t.totals();
                    ui.label(
                        RichText::new(format!(
                            "{}  ·  {}  ·  {}  ·  {}",
                            gitvis_core::devops::history::short(&t.sha),
                            format::plural(files, "file", "files"),
                            format::bytes(bytes as usize),
                            format::millis(t.elapsed_ms)
                        ))
                        .size(size::SMALL)
                        .color(p.text_2),
                    );
                }
            });
        });
    kit::hairline(ui);
    let empty = |ui: &mut Ui, add: &mut dyn FnMut(&mut Ui)| {
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(p.canvas).inner_margin(Margin::same(24)))
            .show(ui, |ui| add(ui));
    };
    if b.loading {
        let rev = b.rev.clone();
        empty(ui, &mut |ui| {
            kit::loading(ui, &format!("Listing the files at {rev}…"))
        });
        return;
    }
    let rows = b.rows();
    let tree = match &b.tree {
        None => {
            empty(ui, &mut |ui| {
                kit::empty_state(
                    ui,
                    Icon::Folder,
                    "Pick a revision",
                    "Type a branch, tag or commit and press Enter to see every file it contains. Right-click a commit in the graph for Browse files at this commit.",
                    None,
                );
            });
            return;
        }
        Some(Err(e)) => {
            let e = e.clone();
            empty(ui, &mut |ui| {
                kit::callout_with(ui, Tone::Danger, "Could not list the files", &e, |ui| {
                    ui.add_space(4.0);
                    if IconButton::new(Icon::Rerun, "Try again")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Act::BrowseRev.into());
                    }
                });
            });
            return;
        }
        Some(Ok(t)) => t,
    };
    let selected = b.selected.clone();
    egui::Panel::left("hist_browse_tree")
        .resizable(true)
        .default_size(380.0)
        .size_range(260.0..=760.0)
        .frame(Frame::NONE.fill(p.panel).inner_margin(Margin {
            left: 8,
            right: 8,
            top: 10,
            bottom: 6,
        }))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            widgets::search(
                ui,
                "hist_browse_q",
                &mut b.query,
                "Find a file (kitrs finds kit.rs)",
            );
            ui.add_space(6.0);
            if tree.entries.is_empty() {
                kit::muted(ui, "This commit has no files.");
                return;
            }
            if rows.is_empty() {
                kit::muted(ui, "No file matches the search.");
                return;
            }
            let searching = !b.query.trim().is_empty();
            kit::list(
                ui,
                "hist_browse_rows",
                rows.len(),
                28.0,
                |i| {
                    selected.as_deref() == Some(tree.nodes[rows[i].0].path.as_str())
                        && !tree.nodes[rows[i].0].dir
                },
                |ui, rect, resp, i| {
                    let (n, depth) = rows[i];
                    let node = &tree.nodes[n];
                    let painter = ui.painter();
                    let mid = rect.center().y;
                    let x0 = rect.left() + 10.0 + depth as f32 * 16.0;
                    let open = searching || b.expanded.contains(&node.path);
                    if node.dir {
                        let c = pos2(x0 + 4.0, mid);
                        let pts = if open {
                            vec![
                                c + vec2(-4.0, -2.0),
                                c + vec2(4.0, -2.0),
                                c + vec2(0.0, 3.0),
                            ]
                        } else {
                            vec![
                                c + vec2(-2.0, -4.0),
                                c + vec2(3.0, 0.0),
                                c + vec2(-2.0, 4.0),
                            ]
                        };
                        painter.add(Shape::convex_polygon(pts, p.text_3, Stroke::NONE));
                        icons::paint(painter, Icon::Folder, pos2(x0 + 20.0, mid), 14.0, p.text_3);
                    } else {
                        let entry = node.entry.map(|e| &tree.entries[e]);
                        let c = if entry.is_some_and(|e| e.kind == EntryKind::Commit) {
                            p.purple
                        } else {
                            p.border_strong
                        };
                        painter.circle_filled(pos2(x0 + 20.0, mid), 2.5, c);
                    }
                    let meta = if node.dir {
                        format!(
                            "{}  ·  {}",
                            format::plural(node.files, "file", "files"),
                            format::bytes(node.size as usize)
                        )
                    } else {
                        let e = node.entry.map(|e| &tree.entries[e]);
                        match e {
                            Some(e) if e.kind == EntryKind::Commit => "submodule".into(),
                            _ => format::bytes(node.size as usize),
                        }
                    };
                    let mg =
                        painter.layout_no_wrap(meta, FontId::proportional(size::SMALL), p.text_3);
                    let mw = mg.size().x;
                    painter.galley(
                        pos2(rect.right() - 12.0 - mw, mid - mg.size().y / 2.0),
                        mg,
                        p.text_3,
                    );
                    let name_x = x0 + 32.0;
                    let color = if node.dir {
                        p.text
                    } else {
                        p.text_2.lerp_to_gamma(p.text, 0.6)
                    };
                    let g = truncated(
                        painter,
                        &node.name,
                        if node.dir {
                            FontId::new(size::BODY - 0.5, crate::theme::medium())
                        } else {
                            FontId::proportional(size::BODY - 0.5)
                        },
                        color,
                        (rect.right() - 24.0 - mw - name_x).max(20.0),
                    );
                    painter.galley(pos2(name_x, mid - g.size().y / 2.0), g, color);
                    if resp.clicked() {
                        if node.dir {
                            if !searching {
                                actions.push(Act::BrowseToggle(node.path.clone()).into());
                            }
                        } else {
                            actions.push(Act::BrowseSelect(node.path.clone()).into());
                        }
                    }
                    if !node.dir {
                        let path = node.path.clone();
                        let sha = tree.sha.clone();
                        resp.context_menu(|ui| {
                            file_menu(ui, &sha, &path, actions);
                        });
                    }
                },
            );
        });
    let tree_sha = tree.sha.clone();
    let tree_rev = tree.rev.clone();
    let entry = selected
        .as_deref()
        .and_then(|s| tree.find(s))
        .and_then(|n| tree.nodes[n].entry)
        .map(|e| tree.entries[e].clone());
    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(p.canvas).inner_margin(Margin {
            left: 20,
            right: 20,
            top: 14,
            bottom: 14,
        }))
        .show(ui, |ui| {
            let Some(entry) = entry else {
                kit::empty_state(
                    ui,
                    Icon::Folder,
                    "Pick a file",
                    "Select a file in the tree to read it as it was at this commit.",
                    None,
                );
                return;
            };
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                kit::subtitle(ui, &entry.path);
                if entry.executable() {
                    kit::badge(ui, "Executable", Tone::Neutral);
                }
                if entry.symlink() {
                    kit::badge(ui, "Symlink", Tone::Neutral);
                }
                if let Some(s) = entry.size {
                    ui.label(RichText::new(format::bytes(s as usize)).size(size::SMALL).color(p.text_2));
                }
                if let Some((_, Some(Ok(ContentShow::Text(d))))) = &b.content
                    && let Some(lang) = &d.language
                {
                    ui.label(RichText::new(lang).size(size::SMALL).color(p.text_3));
                }
            });
            ui.label(
                RichText::new(format!(
                    "At {} ({})",
                    tree_rev,
                    gitvis_core::devops::history::short(&tree_sha)
                ))
                .size(size::SMALL)
                .color(p.text_2),
            );
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                if IconButton::new(Icon::External, "Open this version").compact().show(ui).clicked() {
                    actions.push(Act::OpenVersion { rev: tree_sha.clone(), path: entry.path.clone() }.into());
                }
                if IconButton::new(Icon::Clock, "Blame").compact().show(ui).clicked() {
                    actions.push(Act::Blame { rev: tree_sha.clone(), path: entry.path.clone(), line: None }.into());
                }
                if IconButton::new(Icon::List, "File history").compact().show(ui).clicked() {
                    actions.push(Action::ShowPage(Page::Workbench));
                    actions.push(crate::devops::workbench::Act::FileHistory(entry.path.clone()).into());
                }
                if IconButton::new(Icon::Merge, "Compare with working tree").compact().show(ui).clicked() {
                    actions.push(Act::Compare { a: tree_sha.clone(), b: None, select: Some(entry.path.clone()) }.into());
                }
                if workdir.join(&entry.path).is_file()
                    && IconButton::new(Icon::Folder, "Open the current file").compact().show(ui).clicked()
                {
                    actions.push(Action::OpenPath(workdir.join(&entry.path)));
                }
            });
            ui.add_space(6.0);
            match &b.content {
                Some((path, slot)) if *path == entry.path => match slot {
                    None => kit::loading(ui, "Reading the file…"),
                    Some(Err(e)) => kit::callout(ui, Tone::Danger, "Could not read the file", e),
                    Some(Ok(ContentShow::Text(d))) => widgets::text_view(ui, &entry.path, d, None, None),
                    Some(Ok(ContentShow::Binary { size })) => kit::callout(
                        ui,
                        Tone::Neutral,
                        "Binary file",
                        &format!("{} of binary data. Open this version to view it with its program.", format::bytes(*size as usize)),
                    ),
                    Some(Ok(ContentShow::TooLarge { size })) => kit::callout(
                        ui,
                        Tone::Neutral,
                        "Too large to show here",
                        &format!(
                            "{} is over the {} the viewer reads. Open this version to view it in its program.",
                            format::bytes(*size as usize),
                            format::bytes(core::MAX_VIEW_BYTES as usize)
                        ),
                    ),
                    Some(Ok(ContentShow::Submodule { commit })) => kit::callout(
                        ui,
                        Tone::Neutral,
                        "Submodule",
                        &format!("This path is a submodule, pinned at commit {commit}."),
                    ),
                },
                _ => kit::loading(ui, "Reading the file…"),
            }
        });
}

fn file_menu(ui: &mut Ui, sha: &str, path: &str, actions: &mut Vec<Action>) {
    use crate::views::menus;
    menus::begin(ui, 220.0);
    menus::action(
        ui,
        "Open this version",
        actions,
        Act::OpenVersion {
            rev: sha.to_owned(),
            path: path.to_owned(),
        }
        .into(),
    );
    menus::action(
        ui,
        "Blame",
        actions,
        Act::Blame {
            rev: sha.to_owned(),
            path: path.to_owned(),
            line: None,
        }
        .into(),
    );
    menus::action(
        ui,
        "Compare with the working tree",
        actions,
        Act::Compare {
            a: sha.to_owned(),
            b: None,
            select: Some(path.to_owned()),
        }
        .into(),
    );
    menus::separator(ui);
    menus::action(ui, "Copy path", actions, Action::Copy(path.to_owned()));
}
