//! The Validate tab: every workflow file in the working tree checked by
//! the built-in rules (and actionlint when installed), cached per file by
//! modification time and size, with a "changed since the last check" note.

use std::time::{Duration, Instant};

use egui::{Rect, RichText, Stroke, Ui, pos2, vec2};

use gitvis_core::devops::ci::lint::{Finding, Severity, Source};
use gitvis_core::devops::ci::{LintRun, LintedFile, stamp};
use gitvis_core::devops::tools::{self, Tool};

use super::widgets;
use super::{Act, State};
use crate::format;
use crate::state::{Action, RepoState};
use crate::theme::{Tone, pal, size, space};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit;

fn tone(s: Severity) -> Tone {
    match s {
        Severity::Error => Tone::Danger,
        Severity::Warning => Tone::Warning,
        Severity::Info => Tone::Neutral,
    }
}

fn count(f: &LintedFile, s: Severity) -> usize {
    f.findings.iter().filter(|x| x.severity == s).count()
}

fn total(run: &LintRun, s: Severity) -> usize {
    run.files.iter().map(|f| count(f, s)).sum()
}

/// Compare the files on disk with the last check, at most once a second.
fn poll_changes(repo: &mut RepoState) {
    let ci = &mut repo.devops.ci;
    let Some(run) = ci.lint.ok() else { return };
    if ci.lint.loading
        || ci
            .lint_polled
            .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
    {
        return;
    }
    let dir = &repo.workdir;
    let mut changed: Vec<String> = run
        .files
        .iter()
        .filter(|f| stamp(&dir.join(&f.path)) != f.stamp)
        .map(|f| f.path.clone())
        .collect();
    // New files (names only; nothing is read).
    if let Ok(rd) = std::fs::read_dir(dir.join(".github").join("workflows")) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let lower = name.to_ascii_lowercase();
            if !(lower.ends_with(".yml") || lower.ends_with(".yaml")) {
                continue;
            }
            let path = format!(".github/workflows/{name}");
            if !run.files.iter().any(|f| f.path == path) {
                changed.push(path);
            }
        }
    }
    ci.lint_changed = changed;
    ci.lint_polled = Some(Instant::now());
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) {
    poll_changes(repo);
    ui.ctx().request_repaint_after(Duration::from_secs(1));
    let workdir = repo.workdir.clone();
    let now = format::now_secs();
    let State {
        lint,
        lint_changed,
        lint_file,
        lint_filter,
        lint_min,
        ..
    } = &mut repo.devops.ci;
    let key = lint_file.clone().unwrap_or_default();
    let selected = lint_file.clone();
    let load = &*lint;
    let changed = lint_changed.clone();
    kit::split_keyed(
        ui,
        "ci_lint",
        &key,
        actions,
        |ui, actions| {
            let Some(run) = load.ok() else {
                widgets::list_state(
                    ui,
                    load,
                    "the workflow files",
                    "Checking the workflow files…",
                );
                return;
            };
            if run.files.is_empty() {
                kit::muted(
                    ui,
                    "No workflow files: .github/workflows has no .yml or .yaml files in this working tree.",
                );
                return;
            }
            let all = kit::Row::new("All files").subtitle("Every finding, file by file");
            let n = run.files.len() + 1;
            kit::list(
                ui,
                "ci_lint_files",
                n,
                kit::ROW_H,
                |i| {
                    if i == 0 {
                        selected.is_none()
                    } else {
                        selected.as_deref() == Some(run.files[i - 1].path.as_str())
                    }
                },
                |ui, rect, resp, i| {
                    let p = *pal(ui);
                    if i == 0 {
                        let mut b = Vec::new();
                        for s in [Severity::Error, Severity::Warning] {
                            let c = total(run, s);
                            if c > 0 {
                                let word = s.label().to_lowercase();
                                b.push((format::plural(c, &word, &format!("{word}s")), tone(s)));
                            }
                        }
                        kit::paint_row(
                            ui,
                            rect,
                            &kit::Row::new(all.title)
                                .leading(kit::Leading::Icon(Icon::List, p.text_3))
                                .subtitle(all.subtitle)
                                .badges(&b),
                            selected.is_none(),
                        );
                        if resp.clicked() {
                            actions.push(Act::SelectLintFile(None).into());
                        }
                        return;
                    }
                    let f = &run.files[i - 1];
                    let worst = [Severity::Error, Severity::Warning, Severity::Info]
                        .into_iter()
                        .find(|s| count(f, *s) > 0);
                    let dot = match worst {
                        Some(Severity::Error) => p.danger,
                        Some(Severity::Warning) => p.warning,
                        _ => p.success,
                    };
                    let mut b: Vec<(String, Tone)> = Vec::new();
                    if changed.contains(&f.path) {
                        b.push(("Changed".to_owned(), Tone::Accent));
                    }
                    let e = count(f, Severity::Error);
                    let w = count(f, Severity::Warning);
                    let n = count(f, Severity::Info);
                    if e > 0 {
                        b.push((format::plural(e, "error", "errors"), Tone::Danger));
                    }
                    if w > 0 {
                        b.push((format::plural(w, "warning", "warnings"), Tone::Warning));
                    }
                    if n > 0 && e + w == 0 {
                        b.push((format::plural(n, "note", "notes"), Tone::Neutral));
                    }
                    let name = f.path.rsplit('/').next().unwrap_or(&f.path);
                    let sub = format!("{}  ·  {} lines", f.path, format::count(f.lines));
                    kit::paint_row(
                        ui,
                        rect,
                        &kit::Row::new(name)
                            .leading(kit::Leading::Dot(dot))
                            .subtitle(&sub)
                            .badges(&b),
                        selected.as_deref() == Some(f.path.as_str()),
                    );
                    if resp.clicked() {
                        actions.push(Act::SelectLintFile(Some(f.path.clone())).into());
                    }
                },
            );
        },
        |ui, actions| {
            widgets::header(ui, "Validate workflows", |ui| {
                if load.loading {
                    ui.add(egui::Spinner::new().size(14.0));
                } else if IconButton::new(Icon::Rerun, "Check again")
                    .compact()
                    .show(ui)
                    .on_hover_text("Lint changed files again (unchanged ones come from the cache)")
                    .clicked()
                {
                    actions.push(Act::Validate.into());
                }
            });
            kit::secondary(
                ui,
                "Built-in rules over .github/workflows: YAML syntax, triggers, jobs and steps, needs, expressions, deprecated actions and commands, pull_request_target checkouts, permissions, timeouts, pinning, dispatch inputs, cron and matrix keys. actionlint's findings are added when it is installed.",
            );
            let Some(run) = load.ok() else {
                widgets::loaded(
                    ui,
                    load,
                    "the workflow files",
                    "Checking the workflow files…",
                    Act::Validate.into(),
                    actions,
                );
                return;
            };
            if !changed.is_empty() && !load.loading {
                let mut again = false;
                kit::callout_with(
                    ui,
                    Tone::Accent,
                    &format!(
                        "{} changed since the last check",
                        format::plural(changed.len(), "file", "files")
                    ),
                    &changed.join("\n"),
                    |ui| {
                        ui.add_space(2.0);
                        again = IconButton::new(Icon::Rerun, "Check again")
                            .compact()
                            .show(ui)
                            .clicked();
                    },
                );
                if again {
                    actions.push(Act::Validate.into());
                }
            }
            let at = load
                .at
                .map(|t| format::relative(now - t.elapsed().as_secs() as i64, now))
                .unwrap_or_default();
            let e = total(run, Severity::Error);
            let w = total(run, Severity::Warning);
            let n = total(run, Severity::Info);
            kit::stats(
                ui,
                &[
                    kit::Stat::new("Files", format::count(run.files.len())),
                    kit::Stat::new("Errors", format::count(e)).tone_if(e > 0, Tone::Danger),
                    kit::Stat::new("Warnings", format::count(w)).tone_if(w > 0, Tone::Warning),
                    kit::Stat::new("Notes", format::count(n)),
                    kit::Stat::new("Checked", at),
                ],
            );
            widgets::note(
                ui,
                &format!(
                    "{} linted, {} from the cache (unchanged modification time and size), {} ms",
                    run.linted, run.reused, run.elapsed_ms
                ),
            );
            if let (Some(Ok(n)), v) = (&run.actionlint, &run.actionlint_version) {
                widgets::note(
                    ui,
                    &format!(
                        "actionlint {} added {}",
                        v.as_deref().unwrap_or(""),
                        format::plural(*n, "finding", "findings")
                    ),
                );
            }
            // Filters.
            ui.add_space(space::S);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for (s, label) in [
                    (None, "All"),
                    (Some(Severity::Error), "Errors"),
                    (Some(Severity::Warning), "Warnings and errors"),
                ] {
                    if kit::chip(ui, label, *lint_min == s).clicked() {
                        *lint_min = s;
                    }
                }
                ui.add_space(8.0);
                ui.allocate_ui_with_layout(
                    vec2(260.0, 30.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        kit::search_box(ui, "ci_lint_filter", lint_filter, "Filter rules or text");
                    },
                );
            });
            let needle = lint_filter.trim().to_lowercase();
            let keep = |f: &Finding| {
                lint_min.is_none_or(|m| f.severity <= m)
                    && (needle.is_empty()
                        || f.rule.to_lowercase().contains(&needle)
                        || f.message.to_lowercase().contains(&needle))
            };
            let files: Vec<&LintedFile> = run
                .files
                .iter()
                .filter(|f| selected.as_deref().is_none_or(|s| s == f.path))
                .collect();
            let mut shown = 0usize;
            for f in files {
                let list: Vec<&Finding> = f.findings.iter().filter(|x| keep(x)).collect();
                if list.is_empty() && selected.is_none() {
                    continue;
                }
                file_header(ui, &f.path, list.len());
                if list.is_empty() {
                    kit::muted(
                        ui,
                        if f.findings.is_empty() {
                            "No findings: this file passes every rule."
                        } else {
                            "No findings match the filter."
                        },
                    );
                }
                let lines: Vec<&str> = f.text.lines().collect();
                for x in list {
                    let src = (x.line > 0)
                        .then(|| lines.get(x.line as usize - 1).copied())
                        .flatten();
                    finding(ui, x, src, &workdir, actions);
                    shown += 1;
                }
            }
            if shown == 0 && selected.is_none() {
                ui.add_space(space::M);
                kit::callout(
                    ui,
                    Tone::Success,
                    "Nothing to report",
                    if e + w + n == 0 {
                        "Every workflow file passes every rule."
                    } else {
                        "No findings match the filter."
                    },
                );
            }
            actionlint_note(ui, run, actions);
        },
    );
}

/// A file's name above its findings (not upper-cased like a section).
fn file_header(ui: &mut Ui, path: &str, n: usize) {
    let p = *pal(ui);
    ui.add_space(space::XL);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(kit::mono(path).color(p.text_strong));
        widgets::note(ui, &format::plural(n, "finding", "findings"));
    });
    ui.add_space(space::XS);
}

fn actionlint_note(ui: &mut Ui, run: &LintRun, actions: &mut Vec<Action>) {
    match (&run.actionlint, &run.actionlint_version) {
        (Some(Ok(_)), _) => {}
        (Some(Err(e)), _) => {
            kit::callout(ui, Tone::Warning, "actionlint failed", e);
        }
        (None, _) => {
            if !tools::available(Tool::Actionlint) {
                ui.add_space(space::XL);
                if widgets::tool_missing(
                    ui,
                    Tool::Actionlint,
                    "Optional: actionlint adds deeper checks (runner labels, action inputs, shellcheck on run scripts, expression types). The built-in rules run without it.",
                ) {
                    tools::refresh();
                    actions.push(Act::Validate.into());
                }
            }
        }
    }
}

fn finding(
    ui: &mut Ui,
    f: &Finding,
    src: Option<&str>,
    workdir: &std::path::Path,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let c = tone(f.severity).color(&p);
    let edge = match f.severity {
        Severity::Info => p.text_3,
        _ => c,
    };
    let resp = egui::Frame::NONE
        .inner_margin(egui::Margin {
            left: 14,
            right: 10,
            top: 8,
            bottom: 8,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width().min(kit::DETAIL_MAX));
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                kit::badge(ui, f.severity.label(), tone(f.severity));
                if f.source == Source::Actionlint {
                    kit::badge(ui, "actionlint", Tone::Purple);
                }
                ui.label(kit::mono(&f.rule).color(p.text_2));
                let loc = if f.line > 0 {
                    format!("{}:{}:{}", f.path_name(), f.line, f.column.max(1))
                } else {
                    f.path_name().to_owned()
                };
                if kit::link(ui, &loc).on_hover_text("Open the file").clicked() {
                    actions.push(Action::OpenPath(workdir.join(&f.file)));
                }
            });
            ui.add(
                egui::Label::new(RichText::new(&f.message).size(size::BODY).color(p.text)).wrap(),
            );
            if let Some(line) = src.filter(|l| !l.trim().is_empty()) {
                let shown: String = line.chars().take(160).collect();
                ui.add(
                    egui::Label::new(
                        kit::mono(format!("{:>4} │ {}", f.line, shown.trim_end())).color(p.text_2),
                    )
                    .truncate(),
                );
            }
            if !f.hint.is_empty() {
                ui.add(
                    egui::Label::new(RichText::new(&f.hint).size(size::SMALL).color(p.text_2))
                        .wrap(),
                );
            }
        })
        .response;
    let r = resp.rect;
    ui.painter().rect_filled(
        Rect::from_min_max(r.min, pos2(r.left() + 3.0, r.bottom())),
        egui::CornerRadius::same(1),
        edge,
    );
    ui.painter()
        .hline(r.x_range(), r.bottom(), Stroke::new(1.0, p.border));
}

trait PathName {
    fn path_name(&self) -> &str;
}

impl PathName for Finding {
    fn path_name(&self) -> &str {
        self.file.rsplit('/').next().unwrap_or(&self.file)
    }
}
