//! The Terraform tab: roots, the changes touching them, validate / fmt /
//! init / plan / drift with parsed results.

use egui::{Frame, Margin, RichText, Ui};

use gitvis_core::devops::runtime::terraform::{
    self, Binary, Command, Diagnostic, DriftOutcome, PlanAction, PlanOutcome, ResourceChange,
    TfRoot,
};

use super::common::{self, Cx};
use super::{Act, PlanResult, RootRuns, State, TfState};
use crate::devops::console::Console;
use crate::format;
use crate::state::Action;
use crate::theme::{pal, size};
use crate::views::icons::{Icon, IconButton};
use crate::views::insights::{Cell, detail_title, grid, list_none, list_top};
use crate::views::kit::{self, Col, Leading, Row, Stat, Tone};

fn action_tone(a: PlanAction) -> Tone {
    match a {
        PlanAction::Create => Tone::Success,
        PlanAction::Update | PlanAction::Drifted => Tone::Warning,
        PlanAction::Delete | PlanAction::Replace | PlanAction::DriftDeleted => Tone::Danger,
        PlanAction::Import => Tone::Accent,
        _ => Tone::Neutral,
    }
}

/// Changed paths: the working tree (staged, unstaged, untracked,
/// conflicted) and the commits not pushed yet.
fn changed_paths(cx: &Cx, tf: &TfState) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    if let Some(s) = cx.snap {
        for f in s
            .status
            .staged
            .iter()
            .chain(&s.status.unstaged)
            .chain(&s.status.conflicted)
        {
            v.push(f.path.clone());
            if let Some(o) = &f.old_path {
                v.push(o.clone());
            }
        }
    }
    if let Some((_, Ok(files))) = &tf.unpushed {
        v.extend(files.iter().cloned());
    }
    v.sort();
    v.dedup();
    v
}

pub fn show(ui: &mut Ui, cx: &Cx, rt: &mut State, console: &Console, actions: &mut Vec<Action>) {
    let tf = &mut rt.tf;
    header(ui, cx, tf, actions);
    let binary = tf.binary();
    let scan = match &tf.scan {
        None => {
            common::page(ui, "tf_loading", |ui| {
                kit::empty_state(
                    ui,
                    Icon::Cloud,
                    "Looking for Terraform roots",
                    "Reading the *.tf and *.tofu files of the repository (git ls-files) and telling root modules from the modules they call.",
                    None,
                );
            });
            return;
        }
        Some(Err(e)) => {
            let e = e.clone();
            common::page(ui, "tf_err", |ui| {
                common::error_block(
                    ui,
                    "Could not read the configuration files",
                    &e,
                    None,
                    Some(("Try again", Act::TfRescan)),
                    actions,
                );
            });
            return;
        }
        Some(Ok(s)) => s.clone(),
    };
    if binary.is_none() && tf.tools.is_some() {
        Frame::NONE
            .inner_margin(Margin {
                left: 24,
                right: 16,
                top: 10,
                bottom: 4,
            })
            .show(ui, |ui| {
                common::missing_tool(
                    ui,
                    "Neither Terraform nor OpenTofu is installed",
                    "Format checks, validate, plan and drift run the terraform (or tofu) binary in the root's directory. The roots and their configuration below are read from the files and need neither.",
                    &[Binary::Terraform.install_hint(), Binary::Tofu.install_hint()],
                    Act::TfCheckTools,
                    actions,
                );
            });
    }
    if scan.roots.is_empty() {
        common::page(ui, "tf_none", |ui| {
            kit::empty_state(
                ui,
                Icon::Cloud,
                "No Terraform roots in this repository",
                &if scan.modules.is_empty() {
                    "No *.tf or *.tofu files outside .terraform/. Roots are directories with configuration files that are not called as a module by another directory.".to_owned()
                } else {
                    format!(
                        "{} with configuration were found, all of them modules (called by another directory, or under modules/). A root is where terraform init and plan run; it usually has a backend block.",
                        format::plural(scan.modules.len(), "directory", "directories")
                    )
                },
                Some((Icon::Rerun, "Look again")),
            )
            .then(|| actions.push(Act::TfRescan.into()));
        });
        return;
    }
    let changed = changed_paths(cx, tf);
    let touched = terraform::touched(&scan.roots, &changed);
    if !touched.is_empty() {
        Frame::NONE
            .inner_margin(Margin {
                left: 24,
                right: 16,
                top: 10,
                bottom: 6,
            })
            .show(ui, |ui| {
                let names: Vec<String> = touched
                    .iter()
                    .map(|(i, files)| {
                        format!(
                            "{} ({})",
                            scan.roots[*i].info.display_dir(),
                            format::plural(files.len(), "file", "files")
                        )
                    })
                    .collect();
                let upstream = tf
                    .unpushed
                    .as_ref()
                    .map(|(u, _)| format!(" and the commits not pushed to {u}"))
                    .unwrap_or_default();
                kit::callout(
                    ui,
                    Tone::Accent,
                    &format!(
                        "Your changes touch {}: {}",
                        format::plural(touched.len(), "root", "roots"),
                        names.join(", ")
                    ),
                    &format!(
                        "From the working tree{upstream}. Validate and plan these before you push."
                    ),
                );
            });
    }
    let sel = tf.selected.clone();
    common::list_pane(ui, "tf_list", |ui| {
        root_list(ui, tf, &scan.roots, &touched, actions)
    });
    common::console_panel(
        ui,
        "tf_console",
        console,
        tf.shown_run,
        Act::TfShowRun(None),
        actions,
    );
    common::detail_pane(ui, ("tf_detail", sel.clone()), |ui| {
        match sel
            .as_deref()
            .and_then(|s| scan.roots.iter().position(|r| r.info.dir == s))
        {
            Some(i) => {
                let files = touched
                    .iter()
                    .find(|(t, _)| *t == i)
                    .map(|(_, f)| f.clone())
                    .unwrap_or_default();
                root_detail(ui, cx, tf, console, &scan.roots[i], &files, actions)
            }
            None => {
                kit::empty_state(
                    ui,
                    Icon::Cloud,
                    "Select a root",
                    "Its providers, backend, modules and the results of validate, plan and drift checks appear here.",
                    None,
                );
            }
        }
    });
}

fn header(ui: &mut Ui, cx: &Cx, tf: &TfState, actions: &mut Vec<Action>) {
    let mut parts: Vec<String> = Vec::new();
    match &tf.tools {
        None => parts.push("checking for terraform and tofu…".into()),
        Some(_) => {
            for b in [Binary::Terraform, Binary::Tofu] {
                if let Some(i) = tf.installed(b) {
                    parts.push(if i.version.is_empty() {
                        b.name().to_owned()
                    } else {
                        i.version.clone()
                    });
                }
            }
            if parts.is_empty() {
                parts.push("Terraform and OpenTofu not installed".into());
            }
        }
    }
    if let Some(Ok(s)) = &tf.scan {
        parts.push(format::plural(s.roots.len(), "root", "roots"));
        parts.push(format::plural(
            s.modules.len(),
            "module directory",
            "module directories",
        ));
        parts.push(format!(
            "{} read",
            format::plural(s.files_read, "file", "files")
        ));
    }
    if let Some(t) = tf.scanned {
        parts.push(format!(
            "scanned {} · {}",
            format::relative(t, cx.now),
            format::millis(tf.scan_ms)
        ));
    }
    let both = tf.installed(Binary::Terraform).is_some() && tf.installed(Binary::Tofu).is_some();
    let current = tf.binary();
    let scanning = tf.scanning;
    kit::page_header(ui, "Terraform", &parts.join(" · "), |ui| {
        if scanning {
            ui.add(egui::Spinner::new().size(14.0));
        }
        if IconButton::new(Icon::Rerun, "Rescan")
            .compact()
            .enabled(!scanning)
            .show(ui)
            .on_hover_text("Read the configuration files again (local only)")
            .clicked()
        {
            actions.push(Act::TfRescan.into());
        }
        if both
            && let Some(cur) = current
            && let Some(b) = kit::segmented(
                ui,
                &[(Binary::Tofu, "OpenTofu"), (Binary::Terraform, "Terraform")],
                cur,
            )
        {
            actions.push(Act::TfBinary(b).into());
        }
    });
}

fn root_list(
    ui: &mut Ui,
    tf: &TfState,
    roots: &[TfRoot],
    touched: &[(usize, Vec<String>)],
    actions: &mut Vec<Action>,
) {
    list_top(ui, "Roots", roots.len(), false, None, actions);
    if roots.is_empty() {
        list_none(ui, "No roots.");
        return;
    }
    let p = *pal(ui);
    let rows: Vec<(String, String, String, Vec<(String, Tone)>)> = roots
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let info = &r.info;
            let sub = format!(
                "backend {} · {} · {} · {}",
                info.backend.as_deref().unwrap_or("local (none set)"),
                format::plural(info.files.len(), "file", "files"),
                format::plural(info.providers.len(), "provider", "providers"),
                format::plural(info.modules.len(), "module", "modules")
            );
            let mut badges = Vec::new();
            if touched.iter().any(|(t, _)| *t == i) {
                badges.push(("Changed".to_owned(), Tone::Accent));
            }
            if let Some(runs) = tf.roots.get(&info.dir) {
                if runs
                    .drift
                    .as_ref()
                    .is_some_and(|d| d.outcome == DriftOutcome::Drift)
                {
                    badges.push(("Drift".to_owned(), Tone::Warning));
                }
                if let Some(v) = &runs.validation
                    && let Ok(v) = &v.result
                    && v.errors > 0
                {
                    badges.push((format::plural(v.errors, "error", "errors"), Tone::Danger));
                }
                if let Some(pl) = &runs.plan
                    && pl.outcome == PlanOutcome::Changes
                {
                    let n = plan_counts(pl).total();
                    badges.push((format::plural(n, "change", "changes"), Tone::Warning));
                }
            }
            let meta = info.required_version.clone().unwrap_or_default();
            (info.display_dir().to_owned(), sub, meta, badges)
        })
        .collect();
    let dirs: Vec<String> = roots.iter().map(|r| r.info.dir.clone()).collect();
    let sel = tf.selected.clone();
    kit::list(
        ui,
        "tf_rows",
        rows.len(),
        kit::ROW_H,
        |i| sel.as_deref() == Some(dirs[i].as_str()),
        |ui, rect, resp, i| {
            let (title, sub, meta, badges) = &rows[i];
            kit::paint_row(
                ui,
                rect,
                &Row::new(title)
                    .leading(Leading::Icon(Icon::Folder, p.text_2))
                    .subtitle(sub)
                    .meta(meta)
                    .badges(badges)
                    .mono(),
                sel.as_deref() == Some(dirs[i].as_str()),
            );
            if resp.clicked() {
                actions.push(Act::TfSelect(Some(dirs[i].clone())).into());
            }
        },
    );
}

fn plan_counts(p: &PlanResult) -> terraform::Counts {
    match &p.json {
        Some(Ok(j)) => j.counts(),
        _ => p.text.counts(),
    }
}

fn running(console: &Console, runs: Option<&RootRuns>, cmd: Command) -> bool {
    runs.and_then(|r| r.last.get(&cmd))
        .and_then(|id| console.get(*id))
        .is_some_and(|r| r.running())
}

fn root_detail(
    ui: &mut Ui,
    cx: &Cx,
    tf: &TfState,
    console: &Console,
    root: &TfRoot,
    touched: &[String],
    actions: &mut Vec<Action>,
) {
    let info = &root.info;
    let mut badges = vec![(
        format!("backend {}", info.backend.as_deref().unwrap_or("local")),
        Tone::Neutral,
    )];
    badges.push(if info.initialized {
        ("Initialised".to_owned(), Tone::Neutral)
    } else {
        ("Not initialised".to_owned(), Tone::Neutral)
    });
    if info.mentions_tofu {
        badges.push(("Mentions OpenTofu".to_owned(), Tone::Neutral));
    }
    detail_title(ui, info.display_dir(), &badges);
    let dir_abs = if info.dir.is_empty() {
        cx.workdir.clone()
    } else {
        cx.workdir.join(&info.dir)
    };
    kit::kv(ui, 130.0, |kv| {
        kv.row("Directory", |ui| {
            common::copy_line(ui, &dir_abs.display().to_string(), actions)
        });
        match &info.backend {
            Some(b) if b == "cloud" => kv.text("Backend", "HCP Terraform (cloud block)"),
            Some(b) => kv.text("Backend", b.as_str()),
            None => kv.muted("Backend", "none set: state is a local terraform.tfstate"),
        }
        match &info.required_version {
            Some(v) => kv.text("Version constraint", kit::mono(v)),
            None => kv.muted("Version constraint", "none"),
        }
        kv.text(
            "Lock file",
            if info.lock_file {
                format!(
                    ".terraform.lock.hcl · {} of {} providers locked",
                    info.providers.iter().filter(|p| p.locked.is_some()).count(),
                    info.providers.len()
                )
            } else {
                "none (init creates it)".to_owned()
            },
        );
        kv.text(
            "Initialised",
            if info.initialized {
                ".terraform/ exists (providers and modules installed)"
            } else {
                "no .terraform/ yet: validate and plan need Init first"
            },
        );
        if !info.tfvars.is_empty() {
            let auto = info.auto_tfvars();
            kv.text(
                "Variable files",
                info.tfvars
                    .iter()
                    .map(|f| {
                        if auto.contains(&f.as_str()) {
                            format!("{f} (read automatically)")
                        } else {
                            f.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
    });
    if !touched.is_empty() {
        kit::callout_with(
            ui,
            Tone::Accent,
            &format!(
                "{} touch this root",
                format::plural(touched.len(), "changed file", "changed files")
            ),
            "",
            |ui| {
                for f in touched.iter().take(12) {
                    ui.label(kit::mono(f).color(pal(ui).text));
                }
                if touched.len() > 12 {
                    common::note(ui, &format!("and {} more", touched.len() - 12));
                }
            },
        );
    }

    let runs = tf.roots.get(&info.dir);
    let binary = tf.binary();
    kit::section(ui, "Run", None);
    if let Some(b) = binary {
        kit::secondary(
            ui,
            &format!(
                "Runs {} in {}. Output streams into the console below; results are parsed here. Nothing is ever applied.",
                b.word(),
                info.display_dir()
            ),
        );
    }
    for cmd in Command::ALL {
        let busy = running(console, runs, cmd);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(150.0, 30.0), egui::Sense::hover());
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            let label = if busy {
                format!("{}…", cmd.label())
            } else {
                cmd.label().to_owned()
            };
            let enabled = binary.is_some() && !busy;
            let resp = IconButton::text(&label)
                .primary(cmd == Command::Validate)
                .enabled(enabled)
                .show(&mut child);
            let resp = if binary.is_none() {
                resp.on_hover_text("Install Terraform or OpenTofu first")
            } else {
                resp.on_hover_text(
                    terraform::spec(binary.unwrap_or_default(), &dir_abs, cmd, None).display(),
                )
            };
            if resp.clicked() {
                actions.push(if cmd == Command::Init {
                    Act::TfAskInit(info.dir.clone()).into()
                } else {
                    Act::TfRun {
                        root: info.dir.clone(),
                        cmd,
                    }
                    .into()
                });
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if cmd.network() {
                        kit::badge(ui, "Network", Tone::Neutral);
                    }
                    if busy {
                        ui.add(egui::Spinner::new().size(12.0));
                    }
                    if let Some((text, tone)) = last_result(runs, cmd, cx.now) {
                        kit::badge(ui, &text, tone);
                    }
                    if let Some(id) = runs.and_then(|r| r.last.get(&cmd))
                        && console.get(*id).is_some()
                        && tf.shown_run != Some(*id)
                        && kit::link(ui, "Show output").clicked()
                    {
                        actions.push(Act::TfShowRun(Some(*id)).into());
                    }
                });
                ui.add(
                    egui::Label::new(
                        RichText::new(cmd.explain())
                            .size(size::SMALL)
                            .color(pal(ui).text_2),
                    )
                    .wrap(),
                );
            });
        });
        ui.add_space(2.0);
    }

    if let Some(r) = runs {
        results(ui, cx, info, r, binary, actions);
    }
    configuration(ui, root, actions);
}

/// `Passed · 2m ago`-style badge for a command's last result.
fn last_result(runs: Option<&RootRuns>, cmd: Command, now: i64) -> Option<(String, Tone)> {
    let r = runs?;
    let ago = |t: i64| format::relative(t, now);
    Some(match cmd {
        Command::Fmt => {
            let f = r.fmt.as_ref()?;
            if f.error.is_some() {
                (format!("Failed · {}", ago(f.at)), Tone::Danger)
            } else if f.files.is_empty() {
                (format!("Formatted · {}", ago(f.at)), Tone::Success)
            } else {
                (
                    format!(
                        "{} to format · {}",
                        format::plural(f.files.len(), "file", "files"),
                        ago(f.at)
                    ),
                    Tone::Warning,
                )
            }
        }
        Command::Init => {
            let i = r.init.as_ref()?;
            if i.ok {
                (format!("Done · {}", ago(i.at)), Tone::Success)
            } else {
                (format!("Failed · {}", ago(i.at)), Tone::Danger)
            }
        }
        Command::Validate => {
            let v = r.validation.as_ref()?;
            match &v.result {
                Ok(x) if x.valid && x.warnings == 0 => {
                    (format!("Valid · {}", ago(v.at)), Tone::Success)
                }
                Ok(x) if x.valid => (
                    format!(
                        "Valid, {} · {}",
                        format::plural(x.warnings, "warning", "warnings"),
                        ago(v.at)
                    ),
                    Tone::Warning,
                ),
                Ok(x) => (
                    format!(
                        "{} · {}",
                        format::plural(x.errors, "error", "errors"),
                        ago(v.at)
                    ),
                    Tone::Danger,
                ),
                Err(_) => (format!("Failed · {}", ago(v.at)), Tone::Danger),
            }
        }
        Command::Plan => {
            let p = r.plan.as_ref()?;
            match p.outcome {
                PlanOutcome::NoChanges => (format!("No changes · {}", ago(p.at)), Tone::Success),
                PlanOutcome::Changes => (
                    format!(
                        "{} · {}",
                        format::plural(plan_counts(p).total(), "change", "changes"),
                        ago(p.at)
                    ),
                    Tone::Warning,
                ),
                PlanOutcome::Failed => (format!("Failed · {}", ago(p.at)), Tone::Danger),
                PlanOutcome::Stopped => (format!("Stopped · {}", ago(p.at)), Tone::Neutral),
            }
        }
        Command::Drift => {
            let d = r.drift.as_ref()?;
            match d.outcome {
                DriftOutcome::InSync => (format!("In sync · {}", ago(d.at)), Tone::Success),
                DriftOutcome::Drift => (format!("Drift found · {}", ago(d.at)), Tone::Warning),
                DriftOutcome::Failed => (format!("Failed · {}", ago(d.at)), Tone::Danger),
                DriftOutcome::Stopped => (format!("Stopped · {}", ago(d.at)), Tone::Neutral),
            }
        }
    })
}

fn cause_box(ui: &mut Ui, cause: Option<&str>) {
    if let Some(c) = cause {
        kit::callout(ui, Tone::Warning, "Likely cause", c);
    }
}

fn diagnostics(ui: &mut Ui, list: &[Diagnostic], actions: &mut Vec<Action>) {
    for d in list.iter().take(20) {
        let tone = if d.is_error() {
            Tone::Danger
        } else {
            Tone::Warning
        };
        kit::callout_with(ui, tone, &d.summary, "", |ui| {
            if let Some(loc) = d.location() {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(kit::mono(loc).color(pal(ui).text));
                    if let Some(c) = &d.context {
                        ui.label(
                            RichText::new(format!("in {c}"))
                                .size(size::SMALL)
                                .color(pal(ui).text_2),
                        );
                    }
                });
            }
            if let Some(code) = &d.code {
                kit::code_block(ui, code, actions);
            }
            if !d.detail.is_empty() {
                ui.add(
                    egui::Label::new(
                        RichText::new(&d.detail)
                            .size(size::SMALL)
                            .color(pal(ui).text),
                    )
                    .wrap(),
                );
            }
        });
    }
    if list.len() > 20 {
        common::note(ui, &format!("and {} more", list.len() - 20));
    }
}

fn change_table(ui: &mut Ui, id: &str, list: &[ResourceChange]) {
    let rows: Vec<Vec<Cell>> = list
        .iter()
        .take(200)
        .map(|r| {
            vec![
                Cell::Tone(
                    format!("{}  {}", r.action.symbol(), r.action.label()),
                    action_tone(r.action),
                ),
                Cell::Mono(r.address.clone()),
                Cell::Sec(r.resource_type.clone()),
                Cell::Muted(r.reason.clone().unwrap_or_default()),
            ]
        })
        .collect();
    grid(
        ui,
        id,
        &[
            Col::text("Action"),
            Col::text("Address"),
            Col::text("Type"),
            Col::text("Why"),
        ],
        &rows,
        false,
        |_, _| {},
    );
    if list.len() > 200 {
        common::note(ui, &format!("and {} more", list.len() - 200));
    }
}

fn results(
    ui: &mut Ui,
    cx: &Cx,
    info: &terraform::TfDir,
    r: &RootRuns,
    binary: Option<Binary>,
    actions: &mut Vec<Action>,
) {
    if let Some(v) = &r.validation {
        kit::section(ui, "Validation", None);
        common::note(ui, &common::when(Some(v.at), cx.now));
        match &v.result {
            Ok(x) if x.valid && x.diagnostics.is_empty() => {
                kit::callout(ui, Tone::Success, "The configuration is valid", "")
            }
            Ok(x) => {
                kit::secondary(
                    ui,
                    &format!(
                        "{} and {}",
                        format::plural(x.errors, "error", "errors"),
                        format::plural(x.warnings, "warning", "warnings")
                    ),
                );
                cause_box(ui, v.cause.as_deref());
                diagnostics(ui, &x.diagnostics, actions);
            }
            Err(e) => {
                cause_box(ui, v.cause.as_deref());
                kit::code_block(ui, e, actions);
            }
        }
    }
    if let Some(f) = &r.fmt {
        kit::section(ui, "Format check", Some(f.files.len()));
        if let Some(e) = &f.error {
            kit::code_block(ui, e, actions);
        } else if f.files.is_empty() {
            kit::callout(ui, Tone::Success, "Every file is in canonical format", "");
        } else {
            kit::secondary(
                ui,
                &format!(
                    "These files differ from `fmt` output (the diff is in the console). `{} fmt -recursive` in {} rewrites them.",
                    binary.map_or("terraform", Binary::word),
                    info.display_dir()
                ),
            );
            for file in &f.files {
                ui.label(kit::mono(file).color(pal(ui).text));
            }
        }
    }
    if let Some(i) = &r.init
        && !i.ok
    {
        kit::section(ui, "Init failed", None);
        cause_box(ui, i.cause.as_deref());
        kit::code_block(ui, &i.tail, actions);
    }
    if let Some(p) = &r.plan {
        plan_section(ui, cx, p, actions);
    }
    if let Some(d) = &r.drift {
        kit::section(ui, "Drift check", Some(d.text.drift.len()));
        common::note(
            ui,
            &format!(
                "{} · {}",
                common::when(Some(d.at), cx.now),
                format::millis(d.ms)
            ),
        );
        match d.outcome {
            DriftOutcome::InSync => kit::callout(
                ui,
                Tone::Success,
                "No drift",
                "The real infrastructure matches the last applied state.",
            ),
            DriftOutcome::Drift => {
                kit::callout(
                    ui,
                    Tone::Warning,
                    &format!(
                        "{} changed outside Terraform",
                        format::plural(d.text.drift.len().max(1), "resource", "resources")
                    ),
                    "Someone (or something) changed these since the last apply. A normal plan will try to undo it; adopt the change in the configuration or apply -refresh-only to accept it.",
                );
                change_table(ui, "tf_drift", &d.text.drift);
            }
            DriftOutcome::Failed => {
                cause_box(ui, d.cause.as_deref());
                diagnostics(ui, &d.text.errors, actions);
                if d.text.errors.is_empty() {
                    kit::code_block(ui, &d.tail, actions);
                }
            }
            DriftOutcome::Stopped => common::note(ui, "Stopped before it finished."),
        }
    }
}

fn plan_section(ui: &mut Ui, cx: &Cx, p: &PlanResult, actions: &mut Vec<Action>) {
    let (label, tone) = match p.outcome {
        PlanOutcome::NoChanges => ("No changes", Tone::Success),
        PlanOutcome::Changes => ("Changes", Tone::Warning),
        PlanOutcome::Failed => ("Failed", Tone::Danger),
        PlanOutcome::Stopped => ("Stopped", Tone::Neutral),
    };
    kit::section_with(ui, "Plan", None, |ui| {
        kit::badge(ui, label, tone);
    });
    common::note(
        ui,
        &format!(
            "{} · {} · exit code 0 = no changes, 2 = changes, 1 = error",
            common::when(Some(p.at), cx.now),
            format::millis(p.ms)
        ),
    );
    match p.outcome {
        PlanOutcome::Failed => {
            cause_box(ui, p.cause.as_deref());
            diagnostics(ui, &p.text.errors, actions);
            if p.text.errors.is_empty() {
                kit::code_block(ui, &p.tail, actions);
            }
            return;
        }
        PlanOutcome::Stopped => {
            common::note(ui, "Stopped before it finished.");
            return;
        }
        _ => {}
    }
    let c = plan_counts(p);
    crate::views::insights::stats(
        ui,
        &[
            Stat::new("to add", format::count(c.add)).tone_if(c.add > 0, Tone::Success),
            Stat::new("to change", format::count(c.change)).tone_if(c.change > 0, Tone::Warning),
            Stat::new("to destroy", format::count(c.destroy)).tone_if(c.destroy > 0, Tone::Danger),
            Stat::new("to import", format::count(c.import)),
            Stat::new("outputs changing", format::count(p.text.outputs.len())),
        ],
    );
    let (resources, drift, source): (&[ResourceChange], &[ResourceChange], &str) = match &p.json {
        Some(Ok(j)) => (&j.resources, &j.drift, "from show -json"),
        _ => (&p.text.resources, &p.text.drift, "from the plan output"),
    };
    if p.json_loading {
        kit::loading(ui, "Reading the saved plan (show -json)…");
    }
    if let Some(Err(e)) = &p.json {
        kit::callout(
            ui,
            Tone::Neutral,
            "show -json failed; the table comes from the plan text",
            e,
        );
    }
    if let Some(Ok(j)) = &p.json {
        common::note(
            ui,
            &format!(
                "Terraform {} · plan format {} · {} unchanged",
                j.terraform_version,
                j.format_version,
                format::plural(j.unchanged, "resource", "resources")
            ),
        );
    }
    if p.outcome == PlanOutcome::NoChanges && resources.is_empty() {
        kit::callout(
            ui,
            Tone::Success,
            "No changes",
            "The infrastructure matches the configuration.",
        );
    } else {
        kit::subtitle(ui, &format!("Resources ({source})"));
        change_table(ui, "tf_plan", resources);
    }
    if !drift.is_empty() {
        kit::subtitle(ui, "Changed outside Terraform");
        change_table(ui, "tf_plan_drift", drift);
    }
    if !p.text.outputs.is_empty() {
        kit::subtitle(ui, "Outputs");
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 4.0);
            for (name, sym) in &p.text.outputs {
                kit::badge(
                    ui,
                    &format!("{sym} {name}"),
                    match sym.as_str() {
                        "+" => Tone::Success,
                        "-" => Tone::Danger,
                        _ => Tone::Warning,
                    },
                );
            }
        });
    }
    if !p.text.warnings.is_empty() {
        kit::subtitle(ui, "Warnings");
        diagnostics(ui, &p.text.warnings, actions);
    }
}

fn configuration(ui: &mut Ui, root: &TfRoot, actions: &mut Vec<Action>) {
    let info = &root.info;
    kit::section(ui, "Providers", Some(info.providers.len()));
    if info.providers.is_empty() {
        common::note(ui, "No required_providers and no provider blocks.");
    } else {
        let rows: Vec<Vec<Cell>> = info
            .providers
            .iter()
            .map(|p| {
                vec![
                    Cell::Text(p.name.clone()),
                    if p.source.is_empty() {
                        Cell::Muted(format!("hashicorp/{} (implied)", p.name))
                    } else {
                        Cell::Mono(p.source.clone())
                    },
                    if p.version.is_empty() {
                        Cell::Muted("any".into())
                    } else {
                        Cell::Mono(p.version.clone())
                    },
                    match &p.locked {
                        Some(v) => Cell::Mono(v.clone()),
                        None => Cell::Muted("–".into()),
                    },
                ]
            })
            .collect();
        grid(
            ui,
            "tf_providers",
            &[
                Col::text("Provider"),
                Col::text("Source"),
                Col::text("Constraint"),
                Col::text("Locked"),
            ],
            &rows,
            false,
            |_, _| {},
        );
    }
    kit::section(ui, "Modules", Some(info.modules.len()));
    if info.modules.is_empty() {
        common::note(ui, "No module calls.");
    } else {
        let rows: Vec<Vec<Cell>> = info
            .modules
            .iter()
            .map(|m| {
                vec![
                    Cell::Text(m.name.clone()),
                    Cell::Mono(m.source.clone()),
                    if m.version.is_empty() {
                        Cell::Muted("–".into())
                    } else {
                        Cell::Mono(m.version.clone())
                    },
                    match &m.local_dir {
                        Some(d) => Cell::Sec(format!("local: {d}")),
                        None => Cell::Muted("registry / remote".into()),
                    },
                ]
            })
            .collect();
        grid(
            ui,
            "tf_modules",
            &[
                Col::text("Module"),
                Col::text("Source"),
                Col::text("Version"),
                Col::text("Where"),
            ],
            &rows,
            false,
            |_, _| {},
        );
        if !root.module_dirs.is_empty() {
            common::note(
                ui,
                &format!(
                    "Local module directories reached (changes there count for this root): {}",
                    root.module_dirs.join(", ")
                ),
            );
        }
    }
    let req = info.required_variables();
    kit::section(ui, "Variables", Some(info.variables.len()));
    if info.variables.is_empty() {
        common::note(ui, "No variables.");
    } else {
        if !req.is_empty() {
            kit::callout(
                ui,
                Tone::Neutral,
                &format!(
                    "{} without a default",
                    format::plural(req.len(), "variable", "variables")
                ),
                &format!(
                    "Plans run with -input=false, so {} must come from terraform.tfvars, a *.auto.tfvars file or TF_VAR_ environment variables.",
                    req.iter()
                        .map(|v| v.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        }
        let rows: Vec<Vec<Cell>> = info
            .variables
            .iter()
            .map(|v| {
                vec![
                    Cell::Mono(v.name.clone()),
                    if v.has_default {
                        Cell::Muted("has a default".into())
                    } else {
                        Cell::Tone("required".into(), Tone::Warning)
                    },
                    if v.sensitive {
                        Cell::Badges(vec![("Sensitive".into(), Tone::Neutral)])
                    } else {
                        Cell::Muted(String::new())
                    },
                    Cell::Sec(v.description.clone()),
                ]
            })
            .collect();
        grid(
            ui,
            "tf_vars",
            &[
                Col::text("Variable"),
                Col::text("Default"),
                Col::text(""),
                Col::text("Description"),
            ],
            &rows,
            false,
            |_, _| {},
        );
    }
    kit::section(ui, "Contents", None);
    kit::kv(ui, 130.0, |kv| {
        kv.text(
            "Resources",
            format!(
                "{} managed · {}",
                format::count(info.resources.len()),
                format::plural(info.data_sources, "data source", "data sources")
            ),
        );
        kv.text("Outputs", format::count(info.outputs));
        kv.text("Locals", format::count(info.locals));
    });
    if !info.resources.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 4.0);
            for r in info.resources.iter().take(40) {
                ui.label(kit::mono(r).color(pal(ui).text_2));
            }
        });
        if info.resources.len() > 40 {
            common::note(ui, &format!("and {} more", info.resources.len() - 40));
        }
    }
    kit::section(ui, "Files", Some(info.files.len()));
    let rows: Vec<Vec<Cell>> = info
        .files
        .iter()
        .map(|f| {
            vec![
                Cell::Mono(f.file_name().to_owned()),
                Cell::Sec(format::count(f.lines)),
                Cell::Sec(format::bytes(f.bytes)),
            ]
        })
        .collect();
    let paths: Vec<String> = info.files.iter().map(|f| f.path.clone()).collect();
    grid(
        ui,
        "tf_files",
        &[Col::text("File"), Col::num("Lines"), Col::num("Size")],
        &rows,
        true,
        |r, resp| {
            resp.context_menu(|ui| {
                if ui.button("Copy path").clicked() {
                    actions.push(Action::Copy(paths[r].clone()));
                    ui.close();
                }
            });
        },
    );
}
