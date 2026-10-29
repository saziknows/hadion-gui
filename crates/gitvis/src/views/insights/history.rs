//! Hotspots, Owners and Timeline (everything from the numstat pass).

use std::collections::HashMap;

use egui::{CornerRadius, FontId, Rect, RichText, Sense, Ui, pos2, vec2};

use gitvis_core::insights::InsightsReport;
use gitvis_core::insights::history::{DirOwnership, FileBlame, Hotspot, Month, Owner};

use super::{
    Cell, Cx, age_tone, bus_tone, button_row, days_text, detail_title, grid, list_none, list_top,
    mono, more, muted, num, path_menu, sec, text, when, why,
};
use crate::format;
use crate::state::{Action, InsightsTab};
use crate::theme::{self, pal, size};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Col, Leading, Row, Stat, Tone};

/// Hotspots that pass the filter, in list order.
pub fn visible_hotspots<'a>(rep: &'a InsightsReport, filter: &str) -> Vec<&'a Hotspot> {
    let needle = filter.trim().to_lowercase();
    rep.history
        .hotspots
        .iter()
        .filter(|x| needle.is_empty() || x.path.to_lowercase().contains(&needle))
        .collect()
}

/// Directories that pass the filter, in list order.
pub fn visible_dirs<'a>(rep: &'a InsightsReport, filter: &str) -> Vec<&'a DirOwnership> {
    let needle = filter.trim().to_lowercase();
    rep.history
        .ownership
        .iter()
        .filter(|o| needle.is_empty() || o.dir.to_lowercase().contains(&needle))
        .collect()
}

fn blame_actions(actions: &mut Vec<Action>, path: &str) {
    actions.push(Action::SetInsightsTab(InsightsTab::Ownership));
    actions.push(Action::SelectInsight(Some(format!("file:{path}"))));
    actions.push(Action::BlameFile(path.to_owned()));
}

// ---------------------------------------------------------------------------
// Hotspots
// ---------------------------------------------------------------------------

fn fix_heavy(x: &Hotspot) -> bool {
    x.fixes * 3 >= x.commits && x.commits >= 3
}

fn lone_author(x: &Hotspot) -> bool {
    x.authors == 1 && x.commits >= 5
}

fn hotspot_badges(x: &Hotspot) -> Vec<(String, Tone)> {
    let mut b = Vec::new();
    if fix_heavy(x) {
        b.push((format!("{} fixes", x.fixes), Tone::Danger));
    }
    if lone_author(x) {
        b.push(("1 author".to_owned(), Tone::Warning));
    }
    if x.size.is_none() {
        b.push(("Gone".to_owned(), Tone::Neutral));
    }
    b
}

pub fn hotspots(
    ui: &mut Ui,
    cx: &Cx,
    rep: &InsightsReport,
    selected: &mut Option<String>,
    filter: &mut String,
    actions: &mut Vec<Action>,
) {
    let h = &rep.history;
    let items = visible_hotspots(rep, filter);
    let sel = selected.clone();
    let mut f = filter.clone();
    kit::split_keyed(
        ui,
        "ins_hotspots",
        sel.as_deref().unwrap_or(""),
        actions,
        |ui, actions| {
            list_top(
                ui,
                "Hotspots",
                items.len(),
                sel.is_some(),
                Some(("ins_hotspots_filter", &mut f, "Filter paths")),
                actions,
            );
            if items.is_empty() {
                list_none(ui, "No hotspots match.");
                return;
            }
            kit::list(
                ui,
                "ins_hotspots_list",
                items.len(),
                kit::ROW_H,
                |i| sel.as_deref() == Some(items[i].path.as_str()),
                |ui, rect, resp, i| {
                    let x = items[i];
                    let sub = format!(
                        "{}  ·  {}  ·  +{} −{}",
                        super::count_of(x.commits as usize, "commit", "commits"),
                        super::count_of(x.authors as usize, "author", "authors"),
                        format::count(x.added as usize),
                        format::count(x.deleted as usize),
                    );
                    let meta = format::relative(x.last, cx.now);
                    let badges = hotspot_badges(x);
                    kit::paint_row(
                        ui,
                        rect,
                        &Row::new(&x.path).subtitle(&sub).meta(&meta).badges(&badges),
                        sel.as_deref() == Some(x.path.as_str()),
                    );
                    if resp.clicked() {
                        actions.push(Action::SelectInsight(Some(x.path.clone())));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Copy path").clicked() {
                            actions.push(Action::Copy(x.path.clone()));
                            ui.close();
                        }
                        if ui.button("Blame (Owners)").clicked() {
                            blame_actions(actions, &x.path);
                            ui.close();
                        }
                        if ui.button("Copy git log command").clicked() {
                            actions.push(Action::Copy(format!(
                                "git log --follow --stat -- \"{}\"",
                                x.path
                            )));
                            ui.close();
                        }
                    });
                },
            );
        },
        |ui, actions| match sel
            .as_deref()
            .and_then(|p| h.hotspots.iter().find(|x| x.path == p))
        {
            Some(x) => hotspot_detail(ui, cx, rep, x, actions),
            None => hotspots_overview(ui, cx, rep, actions),
        },
    );
    *filter = f;
}

fn hotspot_detail(
    ui: &mut Ui,
    cx: &Cx,
    rep: &InsightsReport,
    x: &Hotspot,
    actions: &mut Vec<Action>,
) {
    let h = &rep.history;
    detail_title(ui, &x.path, &hotspot_badges(x));
    ui.add_space(4.0);
    let score = format!("{:.1}", x.score);
    super::stats(
        ui,
        &[
            Stat::new("Commits", format::count(x.commits as usize))
                .tip("Non-merge commits touching the file"),
            Stat::new("Lines added", format::count(x.added as usize)),
            Stat::new("Lines deleted", format::count(x.deleted as usize)),
            Stat::new("Authors", x.authors.to_string())
                .tone_if(x.authors == 1, Tone::Warning)
                .tip("Distinct author e-mails"),
            Stat::new("Bug fixes", x.fixes.to_string())
                .tone_if(fix_heavy(x), Tone::Danger)
                .tip("Commits whose message mentions fix/bug/crash/regression"),
            Stat::new("Score", score)
                .tip("Σ ln(1 + lines) × 0.5^(age/365 d): recent churn counts more"),
        ],
    );
    kit::kv(ui, 120.0, |kv| {
        kv.text("First change", when(x.first, cx.now));
        kv.text("Last change", when(x.last, cx.now));
        match x.size {
            Some(s) => kv.text("Size in HEAD", format::bytes(s as usize)),
            None => kv.muted("Size in HEAD", "not in HEAD any more"),
        }
    });
    button_row(ui, |ui| {
        if IconButton::new(Icon::Search, "Blame")
            .compact()
            .show(ui)
            .clicked()
        {
            blame_actions(actions, &x.path);
        }
        if IconButton::new(Icon::Copy, "Copy path")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::Copy(x.path.clone()));
        }
        if IconButton::new(Icon::External, "Open")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::OpenPath(cx.workdir.join(&x.path)));
        }
    });
    if fix_heavy(x) {
        why(
            ui,
            "A third or more of this file's changes are bug fixes. Files that keep needing fixes are where defects cluster; a refactoring or more tests here pays off faster than anywhere else.",
        );
    }
    if lone_author(x) {
        why(
            ui,
            "A single author has ever changed this file. If they leave, nobody knows it: pair on the next change or write the design down.",
        );
    }
    let coupled: Vec<_> = h
        .couplings
        .iter()
        .filter(|c| c.a == x.path || c.b == x.path)
        .collect();
    kit::section(ui, "Changes together with", Some(coupled.len()));
    if coupled.is_empty() {
        kit::muted(
            ui,
            "No file co-changes with this one often enough (3 or more shared commits, at least half of the time).",
        );
        return;
    }
    let others: Vec<&String> = coupled
        .iter()
        .map(|c| if c.a == x.path { &c.b } else { &c.a })
        .collect();
    let rows: Vec<Vec<Cell>> = coupled
        .iter()
        .zip(&others)
        .take(30)
        .map(|(c, other)| {
            vec![
                mono(other.as_str()),
                num(c.together as usize),
                sec(format!("{:.0}%", c.confidence * 100.0)),
                super::badges(&[(
                    if c.cross_dir {
                        "Across directories"
                    } else {
                        ""
                    },
                    Tone::Warning,
                )]),
            ]
        })
        .collect();
    grid(
        ui,
        "ins_hot_coupled",
        &[
            Col::text("File"),
            Col::num("Shared commits"),
            Col::num("Confidence"),
            Col::text(""),
        ],
        &rows,
        false,
        |r, resp| path_menu(resp, cx, others[r], actions),
    );
    more(ui, coupled.len(), 30);
}

fn hotspots_overview(ui: &mut Ui, cx: &Cx, rep: &InsightsReport, actions: &mut Vec<Action>) {
    let h = &rep.history;
    let p = *pal(ui);
    super::stats(
        ui,
        &[
            Stat::new("Hotspots", format::count(h.hotspots.len())),
            Stat::new("Commits read", format::count(h.commits)),
            Stat::new("Paths seen", format::count(h.paths)),
            Stat::new("Coupled pairs", format::count(h.couplings.len())),
        ],
    );
    kit::secondary(
        ui,
        "Hotspots rank files by recent churn: the score adds ln(1 + lines changed) for every commit, halved for each year of age. Select a file on the left for its detail.",
    );

    kit::section(ui, "Temporal coupling", Some(h.couplings.len()));
    if h.couplings.is_empty() {
        kit::muted(
            ui,
            "No pair of files changes together often enough (3 or more shared commits, at least half of the time).",
        );
    } else {
        kit::secondary(
            ui,
            "Files that keep changing in the same commits although nothing in the code says they must. Pairs across directories are the surprising ones: a change in one usually needs a change in the other.",
        );
        let rows: Vec<Vec<Cell>> = h
            .couplings
            .iter()
            .take(40)
            .map(|c| {
                vec![
                    mono(&c.a),
                    mono(&c.b),
                    num(c.together as usize),
                    sec(format!("{:.0}%", c.confidence * 100.0)),
                    num(c.a_commits as usize),
                    num(c.b_commits as usize),
                    super::badges(&[(
                        if c.cross_dir {
                            "Across directories"
                        } else {
                            ""
                        },
                        Tone::Warning,
                    )]),
                ]
            })
            .collect();
        grid(
            ui,
            "ins_couplings",
            &[
                Col::text("File"),
                Col::text("Changes with"),
                Col::num("Together"),
                Col::num("Confidence"),
                Col::num("Commits A"),
                Col::num("Commits B"),
                Col::text(""),
            ],
            &rows,
            false,
            |r, resp| {
                let c = &h.couplings[r];
                resp.context_menu(|ui| {
                    if ui.button("Copy first path").clicked() {
                        actions.push(Action::Copy(c.a.clone()));
                        ui.close();
                    }
                    if ui.button("Copy second path").clicked() {
                        actions.push(Action::Copy(c.b.clone()));
                        ui.close();
                    }
                });
            },
        );
        more(ui, h.couplings.len(), 40);
    }

    kit::section(ui, "Code age by directory", Some(h.age.len()));
    if h.age.is_empty() {
        kit::muted(ui, "No directory has dated files in the commits read.");
        return;
    }
    kit::secondary(
        ui,
        "Median time since each file's last change. Stale is not bad in itself; stale and bug-prone is.",
    );
    let max = h
        .age
        .iter()
        .map(|a| a.median_age_days)
        .max()
        .unwrap_or(1)
        .max(1) as f32;
    let rows: Vec<Vec<Cell>> = h
        .age
        .iter()
        .take(60)
        .map(|a| {
            let tone = age_tone(a.median_age_days);
            let color = match tone {
                Tone::Neutral => p.accent,
                t => t.color(&p),
            };
            vec![
                mono(dir_name(&a.dir)),
                num(a.files as usize),
                Cell::Bar(
                    a.median_age_days as f32 / max,
                    days_text(a.median_age_days),
                    color,
                ),
                muted(format::absolute(a.oldest)),
                muted(format::absolute(a.newest)),
                num(a.stale_files as usize),
                num(a.active_files as usize),
            ]
        })
        .collect();
    grid(
        ui,
        "ins_age",
        &[
            Col::text("Directory"),
            Col::num("Files"),
            Col::text("Median age"),
            Col::text("Oldest change"),
            Col::text("Newest change"),
            Col::num("Untouched 2 y+"),
            Col::num("Changed in 90 d"),
        ],
        &rows,
        false,
        |_, _| {},
    );
    more(ui, h.age.len(), 60);
    let _ = cx;
}

// ---------------------------------------------------------------------------
// Ownership
// ---------------------------------------------------------------------------

fn owner_badges(o: &DirOwnership) -> Vec<(String, Tone)> {
    let mut b = Vec::new();
    if o.orphaned {
        b.push(("Orphaned".to_owned(), Tone::Warning));
    }
    if o.bus_factor == 1 && !o.orphaned {
        b.push(("Bus factor 1".to_owned(), bus_tone(1)));
    }
    b
}

#[allow(clippy::too_many_arguments)]
pub fn ownership(
    ui: &mut Ui,
    cx: &Cx,
    rep: &InsightsReport,
    selected: &mut Option<String>,
    filter: &mut String,
    blame: &HashMap<String, Option<Result<FileBlame, String>>>,
    actions: &mut Vec<Action>,
) {
    let h = &rep.history;
    let items = visible_dirs(rep, filter);
    let sel = selected.clone();
    let mut f = filter.clone();
    kit::split_keyed(
        ui,
        "ins_owners",
        sel.as_deref().unwrap_or(""),
        actions,
        |ui, actions| {
            list_top(
                ui,
                "Directories",
                items.len(),
                sel.is_some(),
                Some(("ins_owners_filter", &mut f, "Filter directories")),
                actions,
            );
            blame_box(ui, actions);
            if items.is_empty() {
                list_none(ui, "No directories match.");
                return;
            }
            kit::list(
                ui,
                "ins_owners_list",
                items.len(),
                kit::ROW_H,
                |i| sel.as_deref() == Some(items[i].dir.as_str()),
                |ui, rect, resp, i| {
                    let o = items[i];
                    let top = o
                        .owners
                        .iter()
                        .take(3)
                        .map(|w| format!("{} {:.0}%", w.name, w.share * 100.0))
                        .collect::<Vec<_>>()
                        .join("  ·  ");
                    let meta = super::count_of(o.files as usize, "file", "files");
                    let badges = owner_badges(o);
                    let lead = o
                        .owners
                        .first()
                        .map(|w| Leading::Avatar(w.name.as_str()))
                        .unwrap_or(Leading::None);
                    kit::paint_row(
                        ui,
                        rect,
                        &Row::new(dir_name(&o.dir))
                            .leading(lead)
                            .subtitle(&top)
                            .meta(&meta)
                            .badges(&badges),
                        sel.as_deref() == Some(o.dir.as_str()),
                    );
                    if resp.clicked() {
                        actions.push(Action::SelectInsight(Some(o.dir.clone())));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Copy directory").clicked() {
                            actions.push(Action::Copy(o.dir.clone()));
                            ui.close();
                        }
                        if ui.button("Copy git shortlog command").clicked() {
                            actions
                                .push(Action::Copy(format!("git shortlog -sne -- \"{}\"", o.dir)));
                            ui.close();
                        }
                    });
                },
            );
        },
        |ui, actions| {
            if let Some(path) = sel.as_deref().and_then(|k| k.strip_prefix("file:")) {
                blame_detail(ui, cx, path, blame.get(path), actions);
            } else if let Some(o) = sel
                .as_deref()
                .and_then(|d| h.ownership.iter().find(|o| o.dir == d))
            {
                owner_detail(ui, cx, o, actions);
            } else {
                owners_overview(ui, cx, rep, actions);
            }
        },
    );
    *filter = f;
}

/// "Blame a file" input; the text lives in egui's memory between frames.
fn blame_box(ui: &mut Ui, actions: &mut Vec<Action>) {
    let id = egui::Id::unique("ins_blame_path_text");
    let mut path: String = ui.data_mut(|d| d.get_temp(id).unwrap_or_default());
    let mut enter = false;
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        ui.set_width(ui.available_width() - 4.0);
        let resp = kit::search_box(
            ui,
            "ins_blame_path",
            &mut path,
            "Blame a file: path in HEAD, Enter",
        );
        enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    });
    ui.add_space(6.0);
    if enter && !path.trim().is_empty() {
        let p = path.trim().replace('\\', "/");
        actions.push(Action::SelectInsight(Some(format!("file:{p}"))));
        actions.push(Action::BlameFile(p));
    }
    ui.data_mut(|d| d.insert_temp(id, path));
}

/// `.` (files at the repository root) reads badly as a title.
fn dir_name(dir: &str) -> &str {
    if dir == "." { "(repository root)" } else { dir }
}

/// Owners as a table: name, e-mail, share, lines, commits, last commit.
fn owner_table(
    ui: &mut Ui,
    cx: &Cx,
    id: &str,
    owners: &[Owner],
    unit: &str,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let rows: Vec<Vec<Cell>> = owners
        .iter()
        .map(|w| {
            vec![
                text(&w.name),
                sec(&w.email),
                Cell::Bar(w.share as f32, format!("{:.0}%", w.share * 100.0), p.accent),
                num(w.lines as usize),
                num(w.commits as usize),
                muted(format::relative(w.last, cx.now)),
            ]
        })
        .collect();
    grid(
        ui,
        id,
        &[
            Col::text("Author"),
            Col::text("E-mail"),
            Col::text("Share"),
            Col::num(unit),
            Col::num("Commits"),
            Col::text("Last commit"),
        ],
        &rows,
        false,
        |r, resp| {
            let w = &owners[r];
            resp.context_menu(|ui| {
                if ui.button("Copy e-mail").clicked() {
                    actions.push(Action::Copy(w.email.clone()));
                    ui.close();
                }
                if ui.button("Copy name and e-mail").clicked() {
                    actions.push(Action::Copy(format!("{} <{}>", w.name, w.email)));
                    ui.close();
                }
            });
        },
    );
}

fn owner_detail(ui: &mut Ui, cx: &Cx, o: &DirOwnership, actions: &mut Vec<Action>) {
    detail_title(ui, dir_name(&o.dir), &owner_badges(o));
    ui.add_space(4.0);
    let last_tip = format::absolute(o.last_activity);
    super::stats(
        ui,
        &[
            Stat::new("Bus factor", o.bus_factor.to_string())
                .tone(bus_tone(o.bus_factor))
                .tip("Fewest people whose added lines together reach half of the directory"),
            Stat::new("Files", o.files.to_string()).tip("Files changed in the commits read"),
            Stat::new("Lines added", format::count(o.lines as usize)),
            Stat::new("Commits", format::count(o.commits as usize)),
            Stat::new("Last activity", format::relative(o.last_activity, cx.now)).tip(&last_tip),
        ],
    );
    if o.orphaned {
        why(
            ui,
            "One person wrote half or more of this directory and has not committed anywhere in the repository for over a year. Nobody left knows it: a good place for a documentation pass or a knowledge-transfer review.",
        );
    } else if o.bus_factor == 1 {
        why(
            ui,
            "A single person accounts for half or more of this directory. Reviews by others, pairing, or a design note reduce the risk of that knowledge leaving with them.",
        );
    }
    kit::section_with(ui, "Owners by lines added", Some(o.owners.len()), |ui| {
        if IconButton::new(Icon::Copy, "Copy owners")
            .compact()
            .show(ui)
            .clicked()
        {
            let text: Vec<String> = o
                .owners
                .iter()
                .map(|w| {
                    format!(
                        "{} <{}> {:.0}% ({} lines, last {})",
                        w.name,
                        w.email,
                        w.share * 100.0,
                        w.lines,
                        format::absolute(w.last)
                    )
                })
                .collect();
            actions.push(Action::Copy(text.join("\n")));
        }
    });
    owner_table(ui, cx, "ins_owner_table", &o.owners, "Lines", actions);
    ui.add_space(4.0);
    kit::secondary(
        ui,
        "Who to ask: the first owner still active. Ownership here counts lines added in the analysed commits, not surviving lines; blame a file for those.",
    );
}

fn blame_detail(
    ui: &mut Ui,
    cx: &Cx,
    path: &str,
    state: Option<&Option<Result<FileBlame, String>>>,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    match state {
        None => {
            detail_title(ui, path, &[]);
            button_row(ui, |ui| {
                if IconButton::new(Icon::Search, "Blame this file")
                    .show(ui)
                    .clicked()
                {
                    actions.push(Action::BlameFile(path.to_owned()));
                }
            });
        }
        Some(None) => {
            detail_title(ui, path, &[]);
            kit::loading(ui, "Running git blame…");
        }
        Some(Some(Err(e))) => {
            detail_title(ui, path, &[]);
            kit::callout(ui, Tone::Danger, "Blame failed", e);
        }
        Some(Some(Ok(b))) => {
            let badges = if b.bus_factor == 1 {
                vec![("Bus factor 1".to_owned(), bus_tone(1))]
            } else {
                Vec::new()
            };
            detail_title(ui, path, &badges);
            ui.add_space(4.0);
            let (oldest, newest) = (format::absolute(b.oldest), format::absolute(b.newest));
            super::stats(
                ui,
                &[
                    Stat::new("Lines", format::count(b.lines as usize)).tip("Lines in HEAD"),
                    Stat::new("Authors", b.owners.len().to_string())
                        .tip("People with surviving lines"),
                    Stat::new("Commits", b.commits.to_string())
                        .tip("Distinct commits still represented"),
                    Stat::new("Bus factor", b.bus_factor.to_string())
                        .tone(bus_tone(b.bus_factor))
                        .tip("Fewest people whose surviving lines reach half the file"),
                    Stat::new("Oldest line", format::relative(b.oldest, cx.now)).tip(&oldest),
                    Stat::new("Newest line", format::relative(b.newest, cx.now)).tip(&newest),
                    Stat::new("Blame took", format::millis(b.elapsed_ms)),
                ],
            );
            kit::section(ui, "Surviving lines by author", Some(b.owners.len()));
            owner_table(ui, cx, "ins_blame_table", &b.owners, "Lines", actions);
            if !b.by_year.is_empty() {
                kit::section(ui, "Lines by year of last change", None);
                let items: Vec<(String, u32)> =
                    b.by_year.iter().map(|(y, n)| (y.to_string(), *n)).collect();
                kit::count_bars(ui, &items, p.accent, 30);
            }
            kit::section(ui, "Command", None);
            kit::code_block(ui, &b.command, actions);
        }
    }
}

fn owners_overview(ui: &mut Ui, cx: &Cx, rep: &InsightsReport, actions: &mut Vec<Action>) {
    let h = &rep.history;
    let p = *pal(ui);
    let orphaned: Vec<&DirOwnership> = h.ownership.iter().filter(|o| o.orphaned).collect();
    let mut people: HashMap<&str, u64> = HashMap::new();
    for o in &h.ownership {
        for w in &o.owners {
            *people.entry(w.name.as_str()).or_insert(0) += w.lines;
        }
    }
    super::stats(
        ui,
        &[
            Stat::new("Directories", format::count(h.ownership.len())),
            Stat::new("Orphaned", orphaned.len().to_string())
                .tone_if(!orphaned.is_empty(), Tone::Warning)
                .tip("Directories whose main owner has not committed for over a year"),
            Stat::new(
                "Bus factor 1",
                h.ownership
                    .iter()
                    .filter(|o| o.bus_factor == 1)
                    .count()
                    .to_string(),
            )
            .tip("Directories where one person wrote half or more of the lines"),
            Stat::new("People", format::count(people.len())),
        ],
    );
    kit::secondary(
        ui,
        "Bus factor is the fewest people whose added lines reach half of a directory. Select a directory for its owners, or type a path on the left to blame one file (surviving lines per author).",
    );
    kit::section(ui, "Orphaned directories", Some(orphaned.len()));
    if orphaned.is_empty() {
        kit::muted(ui, "No directory depends on a single person who has left.");
    } else {
        let rows: Vec<Vec<Cell>> = orphaned
            .iter()
            .take(30)
            .map(|o| match o.owners.first() {
                Some(w) => vec![
                    mono(dir_name(&o.dir)),
                    text(&w.name),
                    sec(format!("{:.0}%", w.share * 100.0)),
                    muted(format::relative(w.last, cx.now)),
                ],
                None => vec![mono(dir_name(&o.dir))],
            })
            .collect();
        grid(
            ui,
            "ins_orphaned",
            &[
                Col::text("Directory"),
                Col::text("Main owner"),
                Col::num("Share"),
                Col::text("Last commit"),
            ],
            &rows,
            true,
            |r, resp| {
                if resp.clicked() {
                    actions.push(Action::SelectInsight(Some(orphaned[r].dir.clone())));
                }
            },
        );
        more(ui, orphaned.len(), 30);
    }
    let mut dist: Vec<(String, u32)> = Vec::new();
    for o in &h.ownership {
        let k = match o.bus_factor {
            0..=3 => format!("Bus factor {}", o.bus_factor),
            _ => "Bus factor 4+".to_owned(),
        };
        match dist.iter_mut().find(|(l, _)| *l == k) {
            Some((_, n)) => *n += 1,
            None => dist.push((k, 1)),
        }
    }
    dist.sort();
    kit::section(ui, "Directories by bus factor", None);
    kit::count_bars(ui, &dist, p.accent, 10);
    let mut people: Vec<(String, f64)> = people
        .into_iter()
        .map(|(n, l)| (n.to_owned(), l as f64))
        .collect();
    people.sort_by(|a, b| b.1.total_cmp(&a.1));
    kit::section(ui, "Lines added by person", Some(people.len()));
    kit::bars(ui, &people, p.accent, 20, |v| format::count(v as usize));
}

// ---------------------------------------------------------------------------
// Timeline
// ---------------------------------------------------------------------------

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

pub fn timeline(ui: &mut Ui, cx: &Cx, rep: &InsightsReport, actions: &mut Vec<Action>) {
    let t = &rep.history.timeline;
    kit::scroll_page(ui, "ins_timeline", |ui| {
        let p = *pal(ui);
        // No commits read (an empty repository): no age and no busiest
        // time, rather than "today" and "Mon 00:00".
        let none = t.months.is_empty() && t.heat.iter().flatten().all(|n| *n == 0);
        let first = if none {
            String::new()
        } else {
            format!("First commit {}", format::absolute(t.first_commit))
        };
        let busiest = if none {
            "–".to_owned()
        } else {
            format!(
                "{} {:02}:00",
                WEEKDAYS[(t.peak_weekday as usize).min(6)],
                t.peak_hour
            )
        };
        super::stats(
            ui,
            &[
                Stat::new(
                    "Age",
                    if none {
                        "–".to_owned()
                    } else {
                        days_text(t.age_days)
                    },
                )
                .tip(&first),
                Stat::new("Authors", t.authors.len().to_string())
                    .tip("Distinct author e-mails in the commits read"),
                Stat::new("Active", t.active_authors.to_string())
                    .tip("Committed in the last 90 days"),
                Stat::new("Months", t.months.len().to_string())
                    .tip("Calendar months between the first and last commit"),
                Stat::new("Files", format::count(t.total_files)).tip("Tracked files"),
                Stat::new("Size", format::bytes(t.total_bytes as usize)).tip("HEAD tree"),
                Stat::new("Busiest", busiest)
                    .tip("Weekday and hour (each author's local time) with most commits"),
            ],
        );
        kit::section(ui, "Commits per month", None);
        if t.months.is_empty() {
            kit::muted(ui, "No commits read.");
        } else {
            legend(
                ui,
                &[
                    (p.accent, "Commits"),
                    (p.purple, "Merges"),
                    (p.success, "First commit of an author"),
                    (p.danger, "Last commit of an author"),
                ],
            );
            month_chart(ui, &t.months);
            kit::section(ui, "Lines changed per month", None);
            legend(
                ui,
                &[(p.success, "Added (above)"), (p.danger, "Deleted (below)")],
            );
            lines_chart(ui, &t.months);
            kit::muted(ui, "Square-root scale. Hover a month for the numbers.");
        }

        kit::section(ui, "Contributors", Some(t.authors.len()));
        kit::secondary(
            ui,
            "Sorted by commits. Months counts the distinct months with a commit; inactive means nothing in the last 90 days.",
        );
        let rows: Vec<Vec<Cell>> = t
            .authors
            .iter()
            .take(60)
            .map(|a| {
                let active = cx.now - a.last < 90 * 86_400;
                vec![
                    text(&a.name),
                    sec(&a.email),
                    num(a.commits as usize),
                    super::added(a.added),
                    super::deleted(a.deleted),
                    muted(format::relative(a.first, cx.now)),
                    muted(format::relative(a.last, cx.now)),
                    num(a.active_months as usize),
                    super::badges(&[(if active { "" } else { "Inactive" }, Tone::Neutral)]),
                ]
            })
            .collect();
        grid(
            ui,
            "ins_contributors",
            &[
                Col::text("Author"),
                Col::text("E-mail"),
                Col::num("Commits"),
                Col::num("Added"),
                Col::num("Deleted"),
                Col::text("First"),
                Col::text("Last"),
                Col::num("Months"),
                Col::text(""),
            ],
            &rows,
            false,
            |r, resp| {
                let a = &t.authors[r];
                resp.context_menu(|ui| {
                    if ui.button("Copy e-mail").clicked() {
                        actions.push(Action::Copy(a.email.clone()));
                        ui.close();
                    }
                });
            },
        );
        more(ui, t.authors.len(), 60);

        kit::section(ui, "Languages", Some(t.languages.len()));
        if t.languages.is_empty() {
            kit::muted(ui, "No tracked files.");
        } else {
            let rows: Vec<Vec<Cell>> = t
                .languages
                .iter()
                .take(20)
                .map(|l| {
                    vec![
                        text(&l.name),
                        num(l.files),
                        sec(format::bytes(l.bytes as usize)),
                        Cell::Bar(l.share as f32, format!("{:.1}%", l.share * 100.0), p.accent),
                    ]
                })
                .collect();
            grid(
                ui,
                "ins_languages",
                &[
                    Col::text("Language"),
                    Col::num("Files"),
                    Col::num("Size"),
                    Col::text("Share"),
                ],
                &rows,
                false,
                |_, _| {},
            );
            more(ui, t.languages.len(), 20);
        }

        kit::section(ui, "When work happens", None);
        kit::secondary(
            ui,
            "Commits by weekday and hour in each author's own time zone (from the commit's UTC offset).",
        );
        ui.add_space(4.0);
        heat_map(ui, &t.heat);
        ui.add_space(12.0);
    });
}

/// A row of coloured keys for a chart.
fn legend(ui: &mut Ui, items: &[(egui::Color32, &str)]) {
    let p = *pal(ui);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (c, label) in items {
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().rect_filled(r, CornerRadius::same(2), *c);
            ui.label(RichText::new(*label).size(size::SMALL).color(p.text_2));
            ui.add_space(10.0);
        }
    });
    ui.add_space(2.0);
}

fn month_chart(ui: &mut Ui, months: &[Month]) {
    let p = *pal(ui);
    let n = months.len().max(1);
    let bw = (ui.available_width().min(1100.0) / n as f32).clamp(1.0, 28.0);
    let w = bw * n as f32;
    let h = 140.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h + 30.0), Sense::hover());
    let painter = ui.painter();
    let max = months.iter().map(|m| m.commits).max().unwrap_or(1).max(1) as f32;
    let gap = if bw > 6.0 { 1.0 } else { 0.0 };
    let base = rect.top() + h;
    painter.hline(
        rect.x_range(),
        base,
        egui::Stroke::new(1.0, p.border_strong),
    );
    for (i, m) in months.iter().enumerate() {
        let x0 = rect.left() + i as f32 * bw;
        let bh = h * m.commits as f32 / max;
        let r = Rect::from_min_max(pos2(x0 + gap, base - bh), pos2(x0 + bw - gap, base));
        painter.rect_filled(r, CornerRadius::ZERO, p.accent);
        let mh = h * m.merges as f32 / max;
        let mr = Rect::from_min_max(pos2(x0 + gap, base - mh), pos2(x0 + bw - gap, base));
        painter.rect_filled(mr, CornerRadius::ZERO, p.purple);
        if m.new_authors > 0 {
            painter.rect_filled(
                Rect::from_min_max(pos2(x0 + gap, base + 3.0), pos2(x0 + bw - gap, base + 6.0)),
                CornerRadius::ZERO,
                p.success,
            );
        }
        if m.departed > 0 {
            painter.rect_filled(
                Rect::from_min_max(pos2(x0 + gap, base + 8.0), pos2(x0 + bw - gap, base + 11.0)),
                CornerRadius::ZERO,
                p.danger,
            );
        }
        let year_start = m.ym.ends_with("-01") || i == 0;
        if year_start && bw * 12.0 > 34.0 {
            painter.text(
                pos2(x0, base + 15.0),
                egui::Align2::LEFT_TOP,
                &m.ym[..4.min(m.ym.len())],
                FontId::proportional(size::CAPTION),
                p.text_3,
            );
        }
    }
    if let Some(pos) = resp.hover_pos() {
        let i = ((pos.x - rect.left()) / bw).floor() as usize;
        if let Some(m) = months.get(i) {
            resp.on_hover_ui_at_pointer(|ui| {
                ui.label(RichText::new(&m.ym).family(theme::semibold()));
                kit::kv(ui, 110.0, |kv| {
                    kv.text("Commits", format!("{} ({} merges)", m.commits, m.merges));
                    kv.text("Authors", m.authors.to_string());
                    kv.text("New", m.new_authors.to_string());
                    kv.text("Departed", m.departed.to_string());
                    kv.text(
                        "Lines",
                        format!(
                            "+{} −{}",
                            format::count(m.added as usize),
                            format::count(m.deleted as usize)
                        ),
                    );
                    kv.text("Files", m.files.to_string());
                });
            });
        }
    }
}

fn lines_chart(ui: &mut Ui, months: &[Month]) {
    let p = *pal(ui);
    let n = months.len().max(1);
    let bw = (ui.available_width().min(1100.0) / n as f32).clamp(1.0, 28.0);
    let w = bw * n as f32;
    let h = 90.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let painter = ui.painter();
    let max = months
        .iter()
        .map(|m| m.added.max(m.deleted))
        .max()
        .unwrap_or(1)
        .max(1) as f32;
    let gap = if bw > 6.0 { 1.0 } else { 0.0 };
    let mid = rect.center().y;
    painter.hline(rect.x_range(), mid, egui::Stroke::new(1.0, p.border));
    for (i, m) in months.iter().enumerate() {
        let x0 = rect.left() + i as f32 * bw;
        let up = (h / 2.0) * (m.added as f32 / max).sqrt();
        let down = (h / 2.0) * (m.deleted as f32 / max).sqrt();
        painter.rect_filled(
            Rect::from_min_max(pos2(x0 + gap, mid - up), pos2(x0 + bw - gap, mid)),
            CornerRadius::ZERO,
            p.success,
        );
        painter.rect_filled(
            Rect::from_min_max(pos2(x0 + gap, mid), pos2(x0 + bw - gap, mid + down)),
            CornerRadius::ZERO,
            p.danger,
        );
    }
    if let Some(pos) = resp.hover_pos() {
        let i = ((pos.x - rect.left()) / bw).floor() as usize;
        if let Some(m) = months.get(i) {
            resp.on_hover_ui_at_pointer(|ui| {
                ui.label(RichText::new(&m.ym).family(theme::semibold()));
                ui.label(format!(
                    "+{} −{} lines, net {}",
                    format::count(m.added as usize),
                    format::count(m.deleted as usize),
                    m.added as i64 - m.deleted as i64
                ));
            });
        }
    }
}

/// Commits by weekday (rows) and hour (columns).
pub fn heat_map(ui: &mut Ui, heat: &[[u32; 24]; 7]) {
    let p = *pal(ui);
    let label_w = 40.0;
    // Narrower columns in a narrow pane, so the late hours are not cut off;
    // rows keep their height so the weekday labels do not touch.
    let cell = ((ui.available_width() - label_w) / 24.0).clamp(10.0, 16.0);
    let row = cell.max(15.0);
    let header_h = 18.0;
    let size = vec2(label_w + 24.0 * cell, header_h + 7.0 * row);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    let max = heat.iter().flatten().copied().max().unwrap_or(0).max(1) as f32;
    let font = FontId::proportional(size::CAPTION);
    for h in (0..24).step_by(3) {
        painter.text(
            pos2(rect.left() + label_w + h as f32 * cell + 1.0, rect.top()),
            egui::Align2::LEFT_TOP,
            format!("{h:02}"),
            font.clone(),
            p.text_3,
        );
    }
    for (d, name) in WEEKDAYS.iter().enumerate() {
        let y = rect.top() + header_h + d as f32 * row;
        painter.text(
            pos2(rect.left(), y + (row - 2.0) / 2.0),
            egui::Align2::LEFT_CENTER,
            *name,
            font.clone(),
            p.text_2,
        );
        for (h, &count) in heat[d].iter().enumerate() {
            let n = count as f32;
            let r = Rect::from_min_size(
                pos2(rect.left() + label_w + h as f32 * cell, y),
                vec2(cell - 2.0, row - 2.0),
            );
            let color = if n == 0.0 {
                p.raised
            } else {
                p.accent.gamma_multiply(0.25 + 0.75 * (n / max).sqrt())
            };
            painter.rect_filled(r, CornerRadius::same(2), color);
        }
    }
    if let Some(pos) = resp.hover_pos() {
        let h = ((pos.x - rect.left() - label_w) / cell).floor();
        let d = ((pos.y - rect.top() - header_h) / row).floor();
        if (0.0..24.0).contains(&h) && (0.0..7.0).contains(&d) {
            let n = heat[d as usize][h as usize];
            let text = format!(
                "{} {:02}:00–{:02}:59: {}",
                WEEKDAYS[d as usize],
                h as u32,
                h as u32,
                format::plural(n as usize, "commit", "commits")
            );
            resp.on_hover_text_at_pointer(text);
        }
    }
}
