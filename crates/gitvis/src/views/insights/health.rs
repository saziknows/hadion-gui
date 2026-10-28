//! Health: landmines, line endings, executable bits, weight, submodules.

use egui::Ui;

use gitvis_core::insights::InsightsReport;
use gitvis_core::insights::health::{Health, Landmine, LandmineKind, Weight};

use super::{
    Cell, Cx, button_row, detail_title, dot_color, grid, group, list_none, list_top, mono, more,
    muted, num, path_label, path_lines, sec, sev_badge, severity_tone, why, yes_no,
};
use crate::format;
use crate::state::{Action, ScanRun};
use crate::theme::pal;
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Col, Leading, Row, Stat, Tone};

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    cx: &Cx,
    rep: &InsightsReport,
    selected: &mut Option<String>,
    weight: &Option<Result<Weight, String>>,
    weight_run: &Option<ScanRun>,
    actions: &mut Vec<Action>,
) {
    let h = &rep.health;
    let sel = selected.clone();
    kit::split_keyed(
        ui,
        "ins_health",
        sel.as_deref().unwrap_or(""),
        actions,
        |ui, actions| landmine_list(ui, h, &sel, actions),
        |ui, actions| match sel
            .as_deref()
            .and_then(|k| k.strip_prefix("mine:"))
            .and_then(|n| n.parse::<usize>().ok())
            .and_then(|n| h.landmines.get(n))
        {
            Some(m) => landmine_detail(ui, cx, m, actions),
            None => overview(ui, cx, h, weight, weight_run, actions),
        },
    );
}

fn landmine_list(ui: &mut Ui, h: &Health, sel: &Option<String>, actions: &mut Vec<Action>) {
    list_top(
        ui,
        "Checkout landmines",
        h.landmines.len(),
        sel.is_some(),
        None,
        actions,
    );
    if h.landmines.is_empty() {
        list_none(
            ui,
            "No landmines: every path checks out on Windows, macOS and Linux alike.",
        );
        return;
    }
    let p = *pal(ui);
    kit::list(
        ui,
        "ins_health_list",
        h.landmines.len(),
        kit::ROW_H,
        |i| sel.as_deref() == Some(&format!("mine:{i}")),
        |ui, rect, resp, i| {
            let m = &h.landmines[i];
            let title = m
                .paths
                .first()
                .map(String::as_str)
                .unwrap_or(m.kind.label());
            let meta = if m.paths.len() > 1 {
                format!("{} paths", m.paths.len())
            } else {
                String::new()
            };
            let badges = [sev_badge(m.severity)];
            kit::paint_row(
                ui,
                rect,
                &Row::new(title)
                    .leading(Leading::Dot(dot_color(&p, severity_tone(m.severity))))
                    .subtitle(m.kind.label())
                    .meta(&meta)
                    .badges(&badges),
                sel.as_deref() == Some(&format!("mine:{i}")),
            );
            if resp.clicked() {
                actions.push(Action::SelectInsight(Some(format!("mine:{i}"))));
            }
            resp.context_menu(|ui| {
                if ui.button("Copy paths").clicked() {
                    actions.push(Action::Copy(m.paths.join("\n")));
                    ui.close();
                }
            });
        },
    );
}

fn landmine_detail(ui: &mut Ui, cx: &Cx, m: &Landmine, actions: &mut Vec<Action>) {
    detail_title(ui, m.kind.label(), &[sev_badge(m.severity)]);
    kit::body(ui, &m.detail);
    ui.add_space(4.0);
    why(ui, m.kind.why());
    kit::section(ui, "Paths", Some(m.paths.len()));
    path_lines(ui, cx, &m.paths, 200, actions);
    kit::section(ui, "How to fix", None);
    let (text, code) = fix_for(m);
    kit::body(ui, text);
    if !code.is_empty() {
        kit::code_block(ui, &code, actions);
    }
}

/// Explanation and command lines for a landmine.
fn fix_for(m: &Landmine) -> (&'static str, String) {
    let first = m.paths.first().cloned().unwrap_or_default();
    match m.kind {
        LandmineKind::CaseCollision => (
            "Rename one of the colliding paths (git tracks the rename when done through git mv on a case-sensitive file system):",
            format!("git mv \"{first}\" \"{first}.renamed\""),
        ),
        LandmineKind::WindowsReserved
        | LandmineKind::InvalidChars
        | LandmineKind::TrailingSpaceOrDot => (
            "Rename the file to a portable name on a Linux or macOS machine (Windows cannot even check it out), then commit:",
            format!("git mv \"{first}\" \"<new name>\""),
        ),
        LandmineKind::LongPath => (
            "Shorten the directory nesting or the file name; as a stop-gap Windows clones work with long paths enabled:",
            "git config --global core.longpaths true".into(),
        ),
        LandmineKind::UnicodeNormalization => (
            "Keep one normalisation form (NFC is what most editors write) and rename the other path. macOS users should set:",
            "git config core.precomposeunicode true".into(),
        ),
        LandmineKind::Symlink => (
            "Replace the link with the file it points to, or make the build resolve the path itself. Windows users need Developer Mode and:",
            "git config core.symlinks true".into(),
        ),
        LandmineKind::IgnoredTracked => (
            "Either stop tracking the files (keeping them on disk) or remove the .gitignore rule that matches them:",
            format!("git rm --cached -- \"{first}\""),
        ),
        LandmineKind::HugeDirectory => (
            "Split the directory into subdirectories or generate its contents at build time instead of committing them.",
            String::new(),
        ),
        LandmineKind::NestedRepository => (
            "Remove the nested .git contents from the index and add the project as a submodule, or flatten it into the tree:",
            format!(
                "git rm -r --cached -- \"{}\"",
                first.split("/.git/").next().unwrap_or(&first)
            ),
        ),
        LandmineKind::EmptyExecutable => (
            "Clear the executable bit (or delete the placeholder):",
            format!("git update-index --chmod=-x -- \"{first}\""),
        ),
    }
}

fn overview(
    ui: &mut Ui,
    cx: &Cx,
    h: &Health,
    weight: &Option<Result<Weight, String>>,
    weight_run: &Option<ScanRun>,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let le = &h.line_endings;
    let w = &h.weight;
    let bad_subs = h.submodules.iter().any(|s| s.status != "ok");
    super::stats(
        ui,
        &[
            Stat::new("Landmines", h.landmines.len().to_string())
                .tone_if(!h.landmines.is_empty(), Tone::Danger)
                .tip("Paths that break a checkout on some platform"),
            Stat::new("Tracked files", format::count(h.files)),
            Stat::new("Symlinks", format::count(h.symlinks)),
            Stat::new("Fighting line endings", le.fighting.len().to_string())
                .tone_if(!le.fighting.is_empty(), Tone::Danger)
                .tip("Working tree line endings differ from the index: the next commit would rewrite every line"),
            Stat::new(
                "Scripts without +x",
                h.exec_bits.scripts_without.len().to_string(),
            )
            .tone_if(!h.exec_bits.scripts_without.is_empty(), Tone::Warning)
            .tip("Shebang scripts that are not executable"),
            Stat::new("Submodules", h.submodules.len().to_string())
                .tone_if(bad_subs, Tone::Warning)
                .tip("Submodules in .gitmodules / gitlinks in the index"),
        ],
    );

    if !h.landmines.is_empty() {
        kit::section(ui, "Landmines by kind", None);
        let mut kinds: Vec<(String, u32)> = Vec::new();
        for m in &h.landmines {
            match kinds.iter_mut().find(|(k, _)| k == m.kind.label()) {
                Some((_, n)) => *n += 1,
                None => kinds.push((m.kind.label().to_owned(), 1)),
            }
        }
        kinds.sort_by_key(|k| std::cmp::Reverse(k.1));
        kit::count_bars(ui, &kinds, p.danger, 12);
    }

    // Line endings.
    kit::section(ui, "Line endings and attributes", None);
    kit::kv(ui, 140.0, |kv| {
        kv.text("Stored with LF", format::count(le.lf));
        kv.text("Stored with CRLF", format::count(le.crlf));
        if le.autocrlf.is_empty() {
            kv.muted("core.autocrlf", "unset");
        } else {
            kv.text("core.autocrlf", le.autocrlf.as_str());
        }
        if le.eol.is_empty() {
            kv.muted("core.eol", "unset (platform default)");
        } else {
            kv.text("core.eol", le.eol.as_str());
        }
        if le.attributes_files.is_empty() {
            kv.muted(".gitattributes", "none");
        } else {
            kv.text(
                ".gitattributes",
                format!(
                    "{} ({} eol/text rules)",
                    le.attributes_files.join(", "),
                    if le.attributes_rules { "with" } else { "no" }
                ),
            );
        }
    });
    if le.crlf > 0 && le.lf > 0 && !le.attributes_rules {
        ui.add_space(4.0);
        why(
            ui,
            "The index mixes CRLF and LF files and nothing pins the line endings. Every contributor's autocrlf setting decides what gets committed, which is how whole-file diffs happen. Add a .gitattributes with `* text=auto` and renormalise.",
        );
        kit::code_block(
            ui,
            "printf '* text=auto\\n' >> .gitattributes\ngit add --renormalize .\ngit commit -m \"Normalize line endings\"",
            actions,
        );
    }
    path_group(
        ui,
        cx,
        "Mixed line endings inside one file",
        &le.mixed,
        actions,
    );
    path_group(
        ui,
        cx,
        "Working tree disagrees with the index (fighting)",
        &le.fighting,
        actions,
    );
    path_group(ui, cx, "UTF-8 byte order mark", &le.bom, actions);
    path_group(ui, cx, "Not valid UTF-8", &le.non_utf8, actions);
    path_group(
        ui,
        cx,
        "Attributes say binary but the content is text",
        &le.misattributed,
        actions,
    );
    if !le.crlf_examples.is_empty() && le.crlf < le.lf {
        path_group(ui, cx, "CRLF files (sample)", &le.crlf_examples, actions);
    }

    // Executable bits.
    kit::section(ui, "Executable bits", None);
    kit::kv(ui, 140.0, |kv| {
        match h.exec_bits.file_mode {
            Some(true) => kv.text("core.fileMode", "true (modes are tracked)"),
            Some(false) => kv.text(
                "core.fileMode",
                "false (mode changes are ignored on this machine)",
            ),
            None => kv.muted("core.fileMode", "unset"),
        }
        kv.text("Executables", format::count(h.exec_bits.executables));
    });
    for (title, paths, flag) in [
        (
            "Scripts without the executable bit",
            &h.exec_bits.scripts_without,
            "+x",
        ),
        (
            "Non-scripts with the executable bit",
            &h.exec_bits.non_scripts_with,
            "-x",
        ),
    ] {
        if paths.is_empty() {
            continue;
        }
        path_group(ui, cx, title, paths, actions);
        let cmd: Vec<String> = paths
            .iter()
            .take(20)
            .map(|p| format!("git update-index --chmod={flag} -- \"{p}\""))
            .collect();
        kit::code_block(ui, &cmd.join("\n"), actions);
    }

    // Weight.
    kit::section(ui, "Weight", None);
    let packs_tip = format!(
        "Pack files holding {}",
        format::bytes(w.pack_bytes as usize)
    );
    super::stats(
        ui,
        &[
            Stat::new("HEAD tree", format::bytes(w.head_bytes as usize))
                .tip("Sum of the blob sizes in HEAD"),
            Stat::new("Packed", format::bytes(w.pack_bytes as usize)),
            Stat::new("Loose", format::bytes(w.loose_bytes as usize))
                .tip("Size of the loose objects"),
            Stat::new("Loose objects", format::count(w.loose_objects as usize))
                .tone_if(w.loose_objects > 5_000, Tone::Warning)
                .tip("Unpacked objects in .git/objects; git gc packs them"),
            Stat::new("Packs", w.packs.to_string())
                .tone_if(w.packs > 20, Tone::Warning)
                .tip(&packs_tip),
            Stat::new("LFS pointers", w.lfs_pointers.to_string())
                .tip("Files stored through Git LFS"),
            Stat::new("LFS candidates", w.lfs_candidates.len().to_string())
                .tone_if(!w.lfs_candidates.is_empty(), Tone::Warning)
                .tip("Binaries of 1 MB or more stored as plain blobs"),
            Stat::new("Refs", format!("{} / {}", w.refs.0, w.refs.1))
                .tone_if(w.refs.0 > 1_000, Tone::Warning)
                .tip("Loose / packed. Loose refs are one file each; git pack-refs folds them into packed-refs"),
        ],
    );
    ui.add_space(4.0);
    kit::kv(ui, 140.0, |kv| {
        kv.text("Commit-graph", yes_no(w.commit_graph));
        kv.text("Multi-pack index", yes_no(w.multi_pack_index));
        kv.text("Shallow clone", yes_no(w.shallow));
        match &w.partial_clone {
            Some(f) => kv.text("Partial clone", f.as_str()),
            None => kv.text("Partial clone", "No"),
        }
        kv.text("Alternates", yes_no(w.alternates));
        if !w.worktrees.is_empty() {
            kv.text("Linked worktrees", w.worktrees.join("\n"));
        }
    });
    for a in &w.advice {
        kit::callout(ui, Tone::Accent, "Suggestion", a);
    }
    if !w.count_objects.is_empty() {
        let text: Vec<String> = w
            .count_objects
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect();
        kit::code_block(
            ui,
            &format!("git count-objects -v\n{}", text.join("\n")),
            actions,
        );
    }
    if !w.head_blobs.is_empty() {
        kit::section(ui, "Largest files in HEAD", Some(w.head_blobs.len()));
        let items: Vec<(String, f64)> = w
            .head_blobs
            .iter()
            .map(|b| (b.path.clone(), b.size as f64))
            .collect();
        kit::bars(ui, &items, p.accent, 25, |v| format::bytes(v as usize));
    }
    if !w.dirs_by_size.is_empty() {
        kit::section(ui, "HEAD size by top-level directory", None);
        let items: Vec<(String, f64)> = w
            .dirs_by_size
            .iter()
            .map(|(d, b)| (d.clone(), *b as f64))
            .collect();
        kit::bars(ui, &items, p.accent, 15, |v| format::bytes(v as usize));
    }
    if !w.ext_by_size.is_empty() {
        kit::section(ui, "HEAD size by extension", None);
        let total: u64 = w.ext_by_size.iter().map(|(_, _, b)| *b).sum::<u64>().max(1);
        let rows: Vec<Vec<Cell>> = w
            .ext_by_size
            .iter()
            .take(15)
            .map(|(e, n, b)| {
                vec![
                    ext_cell(e),
                    num(*n),
                    sec(format::bytes(*b as usize)),
                    Cell::Bar(
                        *b as f32 / total as f32,
                        super::percent(*b, total),
                        p.accent,
                    ),
                ]
            })
            .collect();
        grid(
            ui,
            "ins_head_ext",
            &[
                Col::text("Extension"),
                Col::num("Files"),
                Col::num("Size"),
                Col::text("Share"),
            ],
            &rows,
            false,
            |_, _| {},
        );
        more(ui, w.ext_by_size.len(), 15);
    }
    if !w.lfs_candidates.is_empty() {
        kit::section(
            ui,
            "Large binaries that could live in LFS",
            Some(w.lfs_candidates.len()),
        );
        let rows: Vec<Vec<Cell>> = w
            .lfs_candidates
            .iter()
            .map(|b| vec![mono(&b.path), sec(format::bytes(b.size as usize))])
            .collect();
        grid(
            ui,
            "ins_lfs",
            &[Col::text("Path"), Col::num("Size")],
            &rows,
            false,
            |r, resp| super::path_menu(resp, cx, &w.lfs_candidates[r].path, actions),
        );
        kit::code_block(
            ui,
            "git lfs install\ngit lfs track \"*.bin\"   # pattern of the large files\ngit add .gitattributes",
            actions,
        );
    }

    // History weight (on demand).
    kit::section(ui, "Every object in history", None);
    match (weight_run, weight) {
        (Some(run), _) => {
            if kit::progress(ui, run, "Walking objects", true) {
                actions.push(Action::CancelWeight);
            }
        }
        (None, None) => {
            kit::secondary(
                ui,
                "Measures every object ever committed, not only what HEAD holds.",
            );
            kit::kv(ui, 140.0, |kv| {
                kv.text(
                    "Finds",
                    "the biggest blobs ever committed, dead weight (no longer in HEAD), the extensions and directories the history is made of",
                );
                kv.text(
                    "How",
                    kit::mono("git rev-list --objects --all | git cat-file --batch-check"),
                );
                kv.text("Takes", "seconds, up to a minute on a large repository");
            });
            button_row(ui, |ui| {
                if IconButton::new(Icon::Search, "Measure history")
                    .show(ui)
                    .clicked()
                {
                    actions.push(Action::RunWeight);
                }
            });
        }
        (None, Some(Err(e))) => {
            kit::callout(ui, Tone::Danger, "Measuring failed", e);
            button_row(ui, |ui| {
                if IconButton::new(Icon::Rerun, "Try again")
                    .compact()
                    .show(ui)
                    .clicked()
                {
                    actions.push(Action::RunWeight);
                }
            });
        }
        (None, Some(Ok(wt))) => weight_section(ui, cx, wt, actions),
    }

    // Submodules.
    if !h.submodules.is_empty() {
        kit::section(ui, "Submodules", Some(h.submodules.len()));
        for s in &h.submodules {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                kit::subtitle(ui, &s.path);
                kit::badge(
                    ui,
                    s.status,
                    if s.status == "ok" {
                        Tone::Neutral
                    } else {
                        Tone::Warning
                    },
                );
            });
            kit::kv(ui, 140.0, |kv| {
                kv.text("URL", s.url.as_str());
                if let Some(b) = &s.branch {
                    kv.text("Branch", b.as_str());
                }
                if let Some(r) = s.recorded {
                    kv.text("Recorded", kit::mono(r.to_string()));
                }
                if let Some(c) = s.checked_out {
                    kv.text("Checked out", kit::mono(c.to_string()));
                }
            });
            if !s.detail.is_empty() {
                kit::secondary(ui, &s.detail);
            }
        }
    }
    ui.add_space(12.0);
}

fn ext_cell(e: &str) -> Cell {
    if e.is_empty() {
        muted("(none)")
    } else {
        mono(e)
    }
}

fn weight_section(ui: &mut Ui, cx: &Cx, w: &Weight, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    super::stats(
        ui,
        &[
            Stat::new("Blobs", format::count(w.total_blobs as usize))
                .tip("Distinct blobs reachable from any ref"),
            Stat::new("Uncompressed", format::bytes(w.total_bytes as usize))
                .tip("Sum of blob sizes"),
            Stat::new("On disk", format::bytes(w.total_disk as usize))
                .tip("Compressed, delta'd size in the packs"),
            Stat::new("Commits", format::count(w.commits as usize)),
            Stat::new("Trees", format::count(w.trees as usize)),
            Stat::new("Dead weight", format::bytes(w.deleted_bytes as usize))
                .tone_if(w.deleted_bytes > 10 * 1024 * 1024, Tone::Warning)
                .tip("Big blobs whose path is no longer in HEAD: every clone still downloads them"),
        ],
    );
    if w.cancelled {
        kit::callout(ui, Tone::Warning, "Cancelled", "The numbers are partial.");
    }
    button_row(ui, |ui| {
        if IconButton::new(Icon::Rerun, "Measure again")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::RunWeight);
        }
    });
    if !w.blobs.is_empty() {
        kit::section(ui, "Largest blobs ever committed", Some(w.blobs.len()));
        kit::secondary(
            ui,
            "Blobs marked Gone are no longer in HEAD (dead weight). Rewriting history (git filter-repo) is the only way to shrink clones; coordinate with everyone who has a clone. Right-click a row for the commands.",
        );
        let rows: Vec<Vec<Cell>> = w
            .blobs
            .iter()
            .take(60)
            .map(|b| {
                vec![
                    mono(&b.path),
                    sec(format::bytes(b.size as usize)),
                    sec(format::bytes(b.disk_size as usize)),
                    super::badges(&[(if b.in_head { "" } else { "Gone" }, Tone::Danger)]),
                ]
            })
            .collect();
        grid(
            ui,
            "ins_blobs",
            &[
                Col::text("Path"),
                Col::num("Size"),
                Col::num("On disk"),
                Col::text(""),
            ],
            &rows,
            false,
            |r, resp| {
                let b = &w.blobs[r];
                resp.context_menu(|ui| {
                    if ui.button("Copy path").clicked() {
                        actions.push(Action::Copy(b.path.clone()));
                        ui.close();
                    }
                    if ui.button("Copy blob id").clicked() {
                        actions.push(Action::Copy(b.oid.to_string()));
                        ui.close();
                    }
                    if ui.button("Copy filter-repo command").clicked() {
                        actions.push(Action::Copy(format!(
                            "git filter-repo --path \"{}\" --invert-paths",
                            b.path
                        )));
                        ui.close();
                    }
                });
            },
        );
        more(ui, w.blobs.len(), 60);
    }
    if !w.by_ext.is_empty() {
        kit::section(ui, "History size by extension", None);
        let total: u64 = w.by_ext.iter().map(|(_, _, b)| *b).sum::<u64>().max(1);
        let rows: Vec<Vec<Cell>> = w
            .by_ext
            .iter()
            .take(20)
            .map(|(e, n, b)| {
                vec![
                    ext_cell(e),
                    num(*n as usize),
                    sec(format::bytes(*b as usize)),
                    Cell::Bar(
                        *b as f32 / total as f32,
                        super::percent(*b, total),
                        p.accent,
                    ),
                ]
            })
            .collect();
        grid(
            ui,
            "ins_hist_ext",
            &[
                Col::text("Extension"),
                Col::num("Blobs"),
                Col::num("Size"),
                Col::text("Share"),
            ],
            &rows,
            false,
            |_, _| {},
        );
        more(ui, w.by_ext.len(), 20);
    }
    if !w.by_dir.is_empty() {
        kit::section(ui, "History size by top-level directory", None);
        let items: Vec<(String, f64)> = w
            .by_dir
            .iter()
            .map(|(d, b)| (d.clone(), *b as f64))
            .collect();
        kit::bars(ui, &items, p.accent, 20, |v| format::bytes(v as usize));
    }
    let _ = cx;
}

/// A titled group of paths (nothing when empty).
fn path_group(ui: &mut Ui, cx: &Cx, title: &str, paths: &[String], actions: &mut Vec<Action>) {
    if paths.is_empty() {
        return;
    }
    group(ui, title, Some(paths.len()));
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        for p in paths.iter().take(40) {
            path_label(ui, cx, p, actions);
        }
    });
    more(ui, paths.len(), 40);
}
