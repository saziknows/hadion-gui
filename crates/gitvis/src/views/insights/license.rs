//! Licences.

use egui::Ui;

use gitvis_core::insights::InsightsReport;
use gitvis_core::insights::license::is_copyleft;

use super::{
    Cell, Cx, grid, mono, more, muted, num, path_lines, path_menu, sec, sev_badge, text, why,
};
use crate::state::Action;
use crate::theme::pal;
use crate::views::kit::{self, Col, Stat, Tone};

fn years(y: Option<(u32, u32)>) -> Cell {
    match y {
        Some((a, b)) if a == b => sec(a.to_string()),
        Some((a, b)) => sec(format!("{a}–{b}")),
        None => muted("–"),
    }
}

pub fn show(ui: &mut Ui, cx: &Cx, rep: &InsightsReport, actions: &mut Vec<Action>) {
    let l = &rep.license;
    kit::scroll_page(ui, "ins_licences", |ui| {
        let p = *pal(ui);
        let unknown = l
            .dependencies
            .iter()
            .filter(|d| d.license.is_none())
            .count();
        let headers_tip = format!("Of {} text files scanned", l.files_scanned);
        super::stats(
            ui,
            &[
                Stat::new("Project licence", l.main().unwrap_or("None").to_owned())
                    .tone_if(l.main().is_none(), Tone::Danger)
                    .tip("Licence recognised in the LICENSE / COPYING file"),
                Stat::new("Licence files", l.project.len().to_string())
                    .tip("LICENSE, COPYING, NOTICE… files"),
                Stat::new("Declared", l.declared.len().to_string())
                    .tip("Licence fields in manifests (package.json, Cargo.toml, …)"),
                Stat::new("Dependencies", l.dependencies.len().to_string())
                    .tip("Dependencies with a locally known licence source"),
                Stat::new("Unknown", unknown.to_string())
                    .tone_if(unknown > 0, Tone::Warning)
                    .tip("Dependencies whose licence could not be read offline"),
                Stat::new("Copyleft", l.copyleft.len().to_string())
                    .tone_if(!l.copyleft.is_empty(), Tone::Warning)
                    .tip("GPL / LGPL / AGPL / SSPL / EUPL dependencies"),
                Stat::new("Foreign holders", l.foreign.len().to_string())
                    .tone_if(!l.foreign.is_empty(), Tone::Warning)
                    .tip("Copyright holders in file headers other than the project's own"),
                Stat::new("Files with headers", l.files_with_headers.to_string()).tip(&headers_tip),
            ],
        );

        kit::section(ui, "Findings", Some(l.findings.len()));
        if l.findings.is_empty() {
            kit::muted(ui, "Nothing to flag.");
        }
        for (i, f) in l.findings.iter().enumerate() {
            if i > 0 {
                ui.add_space(10.0);
            }
            let (label, tone) = sev_badge(f.severity);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                kit::subtitle(ui, &f.title);
                kit::badge(ui, &label, tone);
            });
            kit::body(ui, &f.detail);
            why(ui, f.why);
            path_lines(ui, cx, &f.paths, 20, actions);
        }

        kit::section(ui, "Project licence files", Some(l.project.len()));
        if l.project.is_empty() {
            kit::muted(ui, "No LICENSE, COPYING or NOTICE file.");
        } else {
            let rows: Vec<Vec<Cell>> = l
                .project
                .iter()
                .map(|f| {
                    vec![
                        mono(&f.path),
                        if f.id == "Unknown" {
                            Cell::Tone(f.id.clone(), Tone::Warning)
                        } else {
                            text(&f.id)
                        },
                        sec(f.confidence),
                        years(f.years),
                        sec(f.holders.join("; ")),
                    ]
                })
                .collect();
            grid(
                ui,
                "ins_lic_project",
                &[
                    Col::text("File"),
                    Col::text("Licence"),
                    Col::text("Match"),
                    Col::text("Years"),
                    Col::text("Holders"),
                ],
                &rows,
                false,
                |r, resp| path_menu(resp, cx, &l.project[r].path, actions),
            );
        }
        if !l.declared.is_empty() {
            kit::section(ui, "Declared in manifests", Some(l.declared.len()));
            let rows: Vec<Vec<Cell>> = l
                .declared
                .iter()
                .map(|(f, v)| vec![mono(f), text(v)])
                .collect();
            grid(
                ui,
                "ins_lic_declared",
                &[Col::text("Manifest"), Col::text("Licence")],
                &rows,
                false,
                |r, resp| path_menu(resp, cx, &l.declared[r].0, actions),
            );
        }

        kit::section(ui, "Dependency licences", Some(l.dependencies.len()));
        if l.dependencies.is_empty() {
            kit::muted(
                ui,
                "No dependency licence source found offline (package-lock v2+, node_modules, the local cargo registry, vendored LICENSE files).",
            );
        } else {
            kit::count_bars(ui, &l.dep_summary, p.accent, 20);
        }
        if !l.copyleft.is_empty() {
            kit::section(ui, "Copyleft dependencies", Some(l.copyleft.len()));
            let rows: Vec<Vec<Cell>> = l
                .copyleft
                .iter()
                .take(60)
                .map(|d| {
                    let lic = d.license.as_deref().unwrap_or("?");
                    let tone = if lic.to_uppercase().starts_with("AGPL") {
                        Tone::Danger
                    } else {
                        Tone::Warning
                    };
                    vec![
                        text(&d.name),
                        sec(&d.version),
                        Cell::Tone(lic.to_owned(), tone),
                        sec(d.ecosystem),
                        sec(&d.source),
                    ]
                })
                .collect();
            grid(
                ui,
                "ins_lic_copyleft",
                &[
                    Col::text("Package"),
                    Col::text("Version"),
                    Col::text("Licence"),
                    Col::text("Ecosystem"),
                    Col::text("Source"),
                ],
                &rows,
                false,
                |_, _| {},
            );
            more(ui, l.copyleft.len(), 60);
        }
        if !l.foreign.is_empty() {
            kit::section(
                ui,
                "Copyright holders from elsewhere",
                Some(l.foreign.len()),
            );
            kit::secondary(
                ui,
                "File headers naming a holder other than this project's: copied code keeps its original licence terms, which must be honoured (attribution, notices, sometimes copyleft).",
            );
            let rows: Vec<Vec<Cell>> = l
                .foreign
                .iter()
                .take(40)
                .map(|g| {
                    vec![
                        text(&g.holder),
                        num(g.files),
                        if g.licenses.is_empty() {
                            muted("–")
                        } else {
                            sec(g.licenses.join(", "))
                        },
                        years(g.years),
                        mono(
                            g.examples
                                .iter()
                                .take(3)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(", "),
                        ),
                    ]
                })
                .collect();
            grid(
                ui,
                "ins_lic_foreign",
                &[
                    Col::text("Holder"),
                    Col::num("Files"),
                    Col::text("Licences"),
                    Col::text("Years"),
                    Col::text("Examples"),
                ],
                &rows,
                false,
                |r, resp| {
                    let g = &l.foreign[r];
                    resp.context_menu(|ui| {
                        for e in g.examples.iter().take(3) {
                            if ui.button(format!("Copy {e}")).clicked() {
                                actions.push(Action::Copy(e.clone()));
                                ui.close();
                            }
                        }
                        if ui.button("Copy holder").clicked() {
                            actions.push(Action::Copy(g.holder.clone()));
                            ui.close();
                        }
                    });
                },
            );
            more(ui, l.foreign.len(), 40);
        }
        if !l.holders.is_empty() {
            kit::section(ui, "All header holders", Some(l.holders.len()));
            let items: Vec<(String, u32)> = l
                .holders
                .iter()
                .map(|g| (g.holder.clone(), g.files as u32))
                .collect();
            kit::count_bars(ui, &items, p.accent, 15);
        }
        if !l.spdx_headers.is_empty() {
            kit::section(
                ui,
                "SPDX-License-Identifier headers",
                Some(l.spdx_headers.len()),
            );
            let items: Vec<(String, u32)> = l
                .spdx_headers
                .iter()
                .map(|(id, n)| {
                    (
                        format!("{id}{}", if is_copyleft(id) { " (copyleft)" } else { "" }),
                        *n as u32,
                    )
                })
                .collect();
            kit::count_bars(ui, &items, p.accent, 15);
        }
        ui.add_space(12.0);
    });
}
