//! "Before publishing: the release commit" in the Ship → Releases publish
//! form: the version manifests found in the tracked files with a checkbox
//! each, the CHANGELOG.md section, the commit message, then a confirmation
//! with the exact diffs and a local commit (never a push: the section points
//! to the push dialog afterwards).

use std::collections::HashSet;
use std::path::Path;

use egui::{RichText, Ui};

use gitvis_core::devops::automation::groups::same_path;
use gitvis_core::devops::automation::release::{self as core, Detected, FileEdit};

use super::shared::{self, Found, ReleaseRt};
use super::{Act, Msg, PrepareReq, ReleasePlan};
use crate::devops::ctx::Ctx;
use crate::devops::global::GlobalCx;
use crate::format;
use crate::state::{Action, Dialog, ToastKind};
use crate::theme::{Tone, pal, size, space};
use crate::views::form;
use crate::views::icons::{Icon, IconButton};
use crate::views::kit;

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn detect(cx: &mut Ctx, dir: &Path) {
    {
        let mut s = shared::lock();
        let rt = s.release.entry(dir.to_path_buf()).or_default();
        if rt.loading {
            return;
        }
        rt.loading = true;
    }
    let d = dir.to_path_buf();
    let ran = cx.read("Find version manifests", move |_wd, log| Msg::Detected {
        result: core::discover(&d, &mut log.commands).map_err(|e| e.to_string()),
        dir: d,
    });
    if !ran && let Some(rt) = shared::lock().release.get_mut(dir) {
        rt.loading = false;
    }
}

/// Read the files again and build the edits (in the background).
fn build_plan(req: &PrepareReq, log: &mut Vec<String>) -> Result<ReleasePlan, String> {
    let (found, changelog) = core::discover(&req.dir, log).map_err(|e| e.to_string())?;
    let chosen: Vec<(String, String)> = if req.auto {
        core::default_selection(&found, &req.version)
            .into_iter()
            .map(|(i, j)| (found[i].path.clone(), found[i].fields[j].label.clone()))
            .collect()
    } else {
        req.fields.clone()
    };
    let mut edits = Vec::new();
    for d in &found {
        if !chosen.iter().any(|(p, _)| *p == d.path) {
            continue;
        }
        let content = std::fs::read_to_string(req.dir.join(&d.path))
            .map_err(|e| format!("{}: {e}", d.path))?;
        let fresh = core::detect(&d.path, &content).unwrap_or_else(|| d.clone());
        let fields: Vec<&core::Field> = fresh
            .fields
            .iter()
            .filter(|f| chosen.contains(&(d.path.clone(), f.label.clone())))
            .collect();
        if fields.is_empty() {
            continue;
        }
        let what = fields
            .iter()
            .map(|f| {
                format!(
                    "{} {} → {}",
                    f.label,
                    f.version,
                    core::styled(&f.version, &req.version)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let new = core::bump(&content, &fields, &req.version);
        if new != content {
            edits.push(FileEdit {
                path: d.path.clone(),
                old: Some(content),
                new,
                what,
            });
        }
    }
    let want_changelog = if req.auto {
        changelog.is_some()
    } else {
        req.changelog
    };
    match changelog {
        Some((path, content)) if want_changelog => {
            let use_unreleased =
                req.use_unreleased || (req.auto && core::unreleased_body(&content).is_some());
            let e = core::changelog_insert(
                &content,
                &req.version,
                &req.tag,
                &req.date,
                &req.notes,
                use_unreleased,
            )
            .map_err(|e| e.to_string())?;
            let mut what = format!(
                "section ## [{}] - {} at line {}",
                req.version, req.date, e.line
            );
            if e.links > 0 {
                what.push_str(&format!(", {} compare links", e.links));
            }
            if e.moved_unreleased {
                what.push_str(", the Unreleased entries moved into it");
            }
            edits.push(FileEdit {
                path,
                old: Some(content),
                new: e.content,
                what,
            });
        }
        None if req.create_changelog && !req.auto => edits.push(FileEdit {
            path: "CHANGELOG.md".into(),
            old: None,
            new: core::new_changelog(&req.version, &req.date, &req.notes),
            what: "new file in Keep a Changelog style".into(),
        }),
        _ => {}
    }
    if edits.is_empty() {
        return Err("Nothing to change: no manifest is selected (or all are at this version already) and CHANGELOG.md is not updated.".into());
    }
    Ok(ReleasePlan {
        dir: req.dir.clone(),
        version: req.version.clone(),
        tag: req.tag.clone(),
        message: req.message.clone(),
        edits,
    })
}

fn prepare(cx: &mut Ctx, req: PrepareReq) {
    {
        let mut s = shared::lock();
        let rt = s.release.entry(req.dir.clone()).or_default();
        if rt.preparing || rt.committing {
            return;
        }
        rt.preparing = true;
        rt.error = None;
    }
    let dir = req.dir.clone();
    let key = req.dir.clone();
    let now = req.now;
    let ran = cx.read(
        format!("Prepare the release commit for {}", req.version),
        move |_wd, log| Msg::Prepared {
            result: build_plan(&req, &mut log.commands),
            dir,
            now,
        },
    );
    if !ran && let Some(rt) = shared::lock().release.get_mut(&key) {
        rt.preparing = false;
    }
}

fn commit(cx: &mut Ctx, plan: ReleasePlan) {
    if cx
        .workdir
        .as_deref()
        .is_none_or(|w| !same_path(w, &plan.dir))
    {
        cx.toast(
            ToastKind::Error,
            "Switch back to the repository the release commit was prepared for",
        );
        return;
    }
    {
        let mut s = shared::lock();
        let rt = s.release.entry(plan.dir.clone()).or_default();
        if rt.committing {
            return;
        }
        rt.committing = true;
    }
    let label = format!("Commit the release {}", plan.tag);
    let d = plan.dir.clone();
    cx.local_change(label, move |dir, log| {
        let result = core::apply(dir, &plan.edits, &plan.message, &mut log.commands)
            .map_err(|e| e.to_string());
        (
            result.clone().map(|s| format!("Committed {s}")),
            Some(Msg::Committed { dir: d, result }),
        )
    });
}

pub fn apply(cx: &mut Ctx, g: &mut GlobalCx, act: Act) {
    match act {
        Act::ReleaseOpen(open) => {
            let Some(dir) = cx.workdir.clone() else {
                return;
            };
            let needs = {
                let mut s = shared::lock();
                let rt = s.release.entry(dir.clone()).or_default();
                rt.open = open;
                rt.reveal = open;
                rt.template = Some(g.settings.ext.automation.release.message.clone());
                rt.found.is_none()
            };
            if needs {
                detect(cx, &dir);
            }
        }
        Act::ReleaseDetect(dir) => {
            {
                let mut s = shared::lock();
                let rt = s.release.entry(dir.clone()).or_default();
                if rt.template.is_none() {
                    rt.template = Some(g.settings.ext.automation.release.message.clone());
                }
            }
            detect(cx, &dir);
        }
        Act::ReleasePrepare(req) => prepare(cx, *req),
        Act::ReleaseCommit(plan) => commit(cx, *plan),
        Act::ReleaseAuto { version: v, now } => {
            let Some(dir) = cx.workdir.clone() else {
                return;
            };
            let Some(version) = core::version_of_tag(&v) else {
                cx.toast(ToastKind::Error, format!("Not a version: {v}"));
                return;
            };
            let tag = if v.starts_with(['v', 'V']) {
                v.clone()
            } else {
                format!("v{version}")
            };
            let notes = g
                .active_repo()
                .map(|r| r.devops.ship.form.notes.clone())
                .unwrap_or_default();
            let message =
                core::commit_message(&g.settings.ext.automation.release.message, &version, &tag);
            prepare(
                cx,
                PrepareReq {
                    dir,
                    tag,
                    version,
                    notes,
                    fields: Vec::new(),
                    changelog: true,
                    create_changelog: false,
                    use_unreleased: false,
                    message,
                    date: today(),
                    auto: true,
                    now,
                },
            );
        }
        Act::SetReleaseTemplate(t) => {
            g.settings.ext.automation.release.message = t.clone();
            for rt in shared::lock().release.values_mut() {
                rt.template = Some(t.clone());
            }
            cx.toast(
                ToastKind::Success,
                format!("Release commits now default to \"{t}\""),
            );
        }
        Act::OpenPush => {
            let Some(snap) = g.active_repo().and_then(|r| r.snapshot.as_ref()) else {
                return;
            };
            let Some(b) = snap.current_branch() else {
                cx.toast(
                    ToastKind::Info,
                    "HEAD is detached: check out a branch to push",
                );
                return;
            };
            let remotes: Vec<String> = snap.remotes.iter().map(|r| r.name.clone()).collect();
            if remotes.is_empty() {
                cx.toast(ToastKind::Info, "This repository has no remote to push to");
                return;
            }
            let remote = b
                .upstream
                .as_deref()
                .and_then(|u| remotes.iter().position(|r| u.starts_with(&format!("{r}/"))))
                .unwrap_or(0);
            cx.action(Action::Dialog(Dialog::Push {
                branch: b.name.clone(),
                remotes,
                remote,
                set_upstream: b.upstream.is_none(),
                force: false,
            }));
        }
        _ => {}
    }
}

pub fn on_msg(cx: &mut Ctx, g: &mut GlobalCx, msg: Msg) {
    match msg {
        Msg::Detected { dir, result } => {
            let mut s = shared::lock();
            let rt = s.release.entry(dir).or_default();
            rt.loading = false;
            rt.found = Some(result.map(|(manifests, changelog)| Found {
                manifests,
                changelog,
                at: std::time::Instant::now(),
            }));
        }
        Msg::Prepared { result, dir, now } => {
            {
                let mut s = shared::lock();
                let rt = s.release.entry(dir).or_default();
                rt.preparing = false;
                if let Err(e) = &result {
                    rt.error = Some(e.clone());
                }
            }
            let Ok(plan) = result else { return };
            if now {
                commit(cx, plan);
                return;
            }
            let mut details = core::commands(&plan.edits, &plan.message);
            details.push(String::new());
            for e in &plan.edits {
                details.push(format!("# {}: {}", e.path, e.what));
                details.push(core::unified(e).trim_end().to_owned());
                details.push(String::new());
            }
            let paths: Vec<&str> = plan.edits.iter().map(|e| e.path.as_str()).collect();
            cx.confirm(
                format!("Create the release commit for {}?", plan.tag),
                format!(
                    "Writes {} ({}), stages them and commits only them as \"{}\". Other staged changes stay staged. Nothing is pushed: push, then publish.",
                    format::plural(plan.edits.len(), "file", "files"),
                    paths.join(", "),
                    plan.message
                ),
                "Commit",
                false,
                details,
                Act::ReleaseCommit(Box::new(plan)).into(),
            );
        }
        Msg::Committed { dir, result } => {
            let mut s = shared::lock();
            let rt = s.release.entry(dir).or_default();
            rt.committing = false;
            match result {
                Ok(c) => {
                    rt.committed = Some(c);
                    rt.found = None;
                    rt.checked = None;
                    rt.message = None;
                    rt.error = None;
                }
                Err(e) => rt.error = Some(e),
            }
        }
        _ => {}
    }
    let _ = g;
}

// ---------------------------------------------------------------------------
// The section in the publish form
// ---------------------------------------------------------------------------

fn default_checked(found: &[Detected], version: &str) -> HashSet<(String, String)> {
    core::default_selection(found, version)
        .into_iter()
        .map(|(i, j)| (found[i].path.clone(), found[i].fields[j].label.clone()))
        .collect()
}

/// The section, drawn at the end of the publish form. `tag` and `notes` are
/// the form's.
pub fn section(ui: &mut Ui, dir: &Path, tag: &str, notes: &str, actions: &mut Vec<Action>) {
    let mut s = shared::lock();
    let rt = s.release.entry(dir.to_path_buf()).or_default();
    if rt.found.is_none() && !rt.loading {
        actions.push(Act::ReleaseDetect(dir.to_path_buf()).into());
    }
    let version = core::version_of_tag(tag);
    ui.add_space(space::L);
    let top = ui.cursor().top();
    kit::section_with(ui, "Before publishing: the release commit", None, |ui| {
        if IconButton::new(
            if rt.open {
                Icon::ChevronUp
            } else {
                Icon::ChevronDown
            },
            if rt.open { "Hide" } else { "Prepare…" },
        )
        .compact()
        .show(ui)
        .clicked()
        {
            rt.open = !rt.open;
            rt.reveal = rt.open;
        }
    });
    if let Some(c) = rt.committed.clone() {
        committed(ui, &c, actions);
        if !rt.open {
            return;
        }
    }
    if !rt.open {
        summary_line(ui, rt, version.as_deref());
        return;
    }
    form::card(ui, Tone::Neutral, |ui| {
        body(ui, dir, rt, tag, version.as_deref(), notes, actions)
    });
    if std::mem::take(&mut rt.reveal) {
        let r = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=ui.cursor().top());
        ui.scroll_to_rect(r, Some(egui::Align::Min));
    }
}

fn summary_line(ui: &mut Ui, rt: &ReleaseRt, version: Option<&str>) {
    let text = match &rt.found {
        Some(Ok(f)) => {
            let n: usize = f.manifests.iter().map(|d| d.fields.len()).sum();
            let names: Vec<&str> = f
                .manifests
                .iter()
                .filter(|d| !d.fields.is_empty())
                .map(|d| d.path.as_str())
                .take(4)
                .collect();
            let to = version.map(|v| format!(" to {v}")).unwrap_or_default();
            match (n, &f.changelog) {
                (0, None) => {
                    "No version manifest or CHANGELOG.md in the tracked files: nothing to bump."
                        .to_owned()
                }
                (0, Some(_)) => format!(
                    "Add the version's section to CHANGELOG.md{to} in a local commit before publishing."
                ),
                (n, cl) => format!(
                    "Bump {}{to} ({}{}){} in one local commit before publishing.",
                    format::plural(n, "version field", "version fields"),
                    names.join(", "),
                    if f.manifests.len() > names.len() {
                        ", …"
                    } else {
                        ""
                    },
                    if cl.is_some() {
                        " and add its section to CHANGELOG.md"
                    } else {
                        ""
                    }
                ),
            }
        }
        Some(Err(e)) => format!("Could not look for version manifests: {e}"),
        None => "Looking for version manifests…".to_owned(),
    };
    kit::secondary(ui, &text);
}

fn committed(ui: &mut Ui, c: &str, actions: &mut Vec<Action>) {
    kit::callout_with(
        ui,
        Tone::Success,
        &format!("Committed {c}"),
        "Push, then publish: the release's tag is created from the target on GitHub, so the release commit has to be on the remote first.",
        |ui| {
            ui.horizontal(|ui| {
                if IconButton::new(Icon::Push, "Push…")
                    .compact()
                    .show(ui)
                    .clicked()
                {
                    actions.push(Act::OpenPush.into());
                }
                kit::muted(
                    ui,
                    "Opens the push dialog (it asks before sending anything).",
                );
            });
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn body(
    ui: &mut Ui,
    dir: &Path,
    rt: &mut ReleaseRt,
    tag: &str,
    version: Option<&str>,
    notes: &str,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    kit::secondary(
        ui,
        "Bumps the version in the manifests you tick (only the version text changes, the files keep their formatting), adds the release's section to CHANGELOG.md, and commits only those files. You confirm the exact diffs first; nothing is pushed.",
    );
    ui.add_space(space::S);
    let Some(version) = version else {
        kit::callout(
            ui,
            Tone::Neutral,
            "Type the version tag first",
            if tag.trim().is_empty() {
                "The Tag field above gives the version: v1.2.0 bumps the manifests to 1.2.0."
            } else {
                "The tag above does not look like a version (v1.2.0, 1.2.0-rc.1)."
            },
        );
        return;
    };
    let found = match &rt.found {
        None => {
            kit::loading(ui, "Looking for version manifests in the tracked files…");
            return;
        }
        Some(Err(e)) => {
            form::error(ui, &format!("Could not list the tracked files: {e}"));
            if form::button(ui, "Try again", true).clicked() {
                rt.found = None;
                actions.push(Act::ReleaseDetect(dir.to_path_buf()).into());
            }
            return;
        }
        Some(Ok(f)) => f.clone(),
    };
    let defaults = default_checked(&found.manifests, version);
    let mut checked = rt.checked.clone().unwrap_or_else(|| defaults.clone());
    let mut changed = false;

    // Manifests.
    let bumpable: usize = found.manifests.iter().map(|d| d.fields.len()).sum();
    let age = found.at.elapsed().as_secs();
    kit::subtitle(
        ui,
        &format!(
            "Version manifests · {bumpable} · read {}",
            if age < 60 {
                format!("{age} s ago")
            } else {
                format!("{} min ago", age / 60)
            }
        ),
    );
    if found.manifests.is_empty() {
        kit::muted(
            ui,
            "None in the tracked files (package.json, Cargo.toml, pyproject.toml, *.csproj, pom.xml, build.gradle(.kts), VERSION, Chart.yaml; up to four folders deep, not in node_modules, vendor, target and the like).",
        );
    }
    for d in &found.manifests {
        for f in &d.fields {
            let key = (d.path.clone(), f.label.clone());
            let same = f.version.trim_start_matches(['v', 'V']) == version;
            let mut on = checked.contains(&key) && !same;
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!same, |ui| {
                    if form::checkbox(ui, &mut on, &d.path).clicked() {
                        changed = true;
                        if on {
                            checked.insert(key.clone());
                        } else {
                            checked.remove(&key);
                        }
                    }
                });
                ui.label(
                    RichText::new(format!("{} · {}", d.format.label(), f.label))
                        .size(size::SMALL)
                        .color(p.text_3),
                );
                let text = if same {
                    format!("already {}", f.version)
                } else {
                    format!("{}  →  {}", f.version, core::styled(&f.version, version))
                };
                ui.label(
                    RichText::new(text)
                        .monospace()
                        .size(size::MONO)
                        .color(if same || !on { p.text_3 } else { p.text }),
                );
                ui.label(
                    RichText::new(format!("line {}", f.line))
                        .size(size::SMALL)
                        .color(p.text_3),
                );
            });
        }
        for n in &d.notes {
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                ui.label(
                    RichText::new(format!("{}: {n}", d.path))
                        .size(size::SMALL)
                        .color(p.text_3),
                );
            });
        }
    }
    if changed {
        rt.checked = Some(checked.clone());
    }

    // CHANGELOG.md.
    ui.add_space(space::S);
    let date = today();
    match &found.changelog {
        Some((path, content)) if core::section_line(content, version).is_some() => {
            kit::subtitle(ui, path);
            kit::muted(
                ui,
                &format!(
                    "Already has a section for {version} (line {}): nothing to add.",
                    core::section_line(content, version).unwrap_or(0)
                ),
            );
        }
        Some((path, content)) => {
            kit::subtitle(ui, path);
            let mut on = rt.changelog.unwrap_or(true);
            if form::checkbox(ui, &mut on, &format!("Add ## [{version}] - {date}")).clicked() {
                rt.changelog = Some(on);
            }
            let unreleased = core::unreleased_body(content);
            if on {
                if let Some(u) = &unreleased {
                    form::checkbox(
                        ui,
                        &mut rt.use_unreleased,
                        &format!(
                            "Use the {} under Unreleased as this version's entries",
                            format::plural(
                                u.lines().filter(|l| !l.trim().is_empty()).count(),
                                "line",
                                "lines"
                            )
                        ),
                    );
                }
                let source = if rt.use_unreleased && unreleased.is_some() {
                    "The Unreleased entries move into the new section; Unreleased stays, empty."
                        .to_owned()
                } else if notes.trim().is_empty() {
                    "The release notes above are empty: the section gets only its heading."
                        .to_owned()
                } else {
                    format!(
                        "With the release notes above ({}); their headings become level 3 so they stay inside the section.",
                        format::plural(notes.lines().count(), "line", "lines")
                    )
                };
                kit::muted(
                    ui,
                    &format!(
                        "{source} Above the previous version, compare links updated when the file has them."
                    ),
                );
            }
        }
        None => {
            kit::subtitle(ui, "CHANGELOG.md");
            form::checkbox(
                ui,
                &mut rt.create_changelog,
                &format!("Create CHANGELOG.md with ## [{version}] - {date} (Keep a Changelog)"),
            );
        }
    }

    // Message.
    ui.add_space(space::S);
    let template = rt
        .template
        .clone()
        .unwrap_or_else(|| "chore(release): v{version}".to_owned());
    let default_msg = core::commit_message(&template, version, tag.trim());
    let mut message = rt.message.clone().unwrap_or_else(|| default_msg.clone());
    form::field(ui, "Commit message", None, |ui| {
        if form::input(
            ui,
            "automation_release_message",
            &mut message,
            form::InputOpts::new(&default_msg).mono(),
        )
        .changed()
        {
            rt.message = Some(message.clone());
        }
    });
    if rt
        .message
        .as_deref()
        .is_some_and(|m| m != default_msg && m.contains(version))
    {
        let t = message.replace(version, "{version}");
        ui.horizontal(|ui| {
            kit::muted(ui, &format!("Default for every release: \"{t}\""));
            if kit::link(ui, "Save as the default").clicked() {
                actions.push(Act::SetReleaseTemplate(t.clone()).into());
            }
        });
    }

    if let Some(e) = &rt.error {
        form::error(ui, e);
    }
    let any_field = found
        .manifests
        .iter()
        .flat_map(|d| d.fields.iter().map(move |f| (d, f)))
        .any(|(d, f)| {
            checked.contains(&(d.path.clone(), f.label.clone()))
                && f.version.trim_start_matches(['v', 'V']) != version
        });
    let has_section = found
        .changelog
        .as_ref()
        .is_some_and(|(_, c)| core::section_line(c, version).is_some());
    let cl = match &found.changelog {
        Some(_) => rt.changelog.unwrap_or(true) && !has_section,
        None => rt.create_changelog,
    };
    let busy = rt.preparing || rt.committing;
    form::actions_row(ui, |ui| {
        let label = if rt.committing {
            "Committing…"
        } else if rt.preparing {
            "Preparing…"
        } else {
            "Review the commit…"
        };
        if form::primary(
            ui,
            label,
            (any_field || cl) && !busy && !message.trim().is_empty(),
        )
        .clicked()
        {
            let fields: Vec<(String, String)> = checked.iter().cloned().collect();
            actions.push(
                Act::ReleasePrepare(Box::new(PrepareReq {
                    dir: dir.to_path_buf(),
                    tag: tag.trim().to_owned(),
                    version: version.to_owned(),
                    notes: notes.to_owned(),
                    fields,
                    changelog: cl && found.changelog.is_some(),
                    create_changelog: found.changelog.is_none() && rt.create_changelog,
                    use_unreleased: rt.use_unreleased,
                    message: message.trim().to_owned(),
                    date: date.clone(),
                    auto: false,
                    now: false,
                }))
                .into(),
            );
        }
        if form::button(ui, "Look again", !busy && !rt.loading).clicked() {
            rt.found = None;
            actions.push(Act::ReleaseDetect(dir.to_path_buf()).into());
        }
        if !any_field && !cl {
            kit::muted(ui, "Tick a manifest or the changelog");
        }
    });
}
