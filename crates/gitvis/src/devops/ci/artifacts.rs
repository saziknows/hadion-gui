//! The Artifacts tab: every artifact of the repository with size, age,
//! expiry, run, workflow, branch and commit; download (into a folder picked
//! in a dialog, through `gh run download`) and delete.

use std::collections::HashMap;

use egui::Ui;

use gitvis_core::devops::ci::model::Artifact;
use gitvis_core::github::WorkflowRun;

use super::widgets::{self, bytes};
use super::{Act, ArtifactScope, EXPIRES_SOON, State};
use crate::devops::console::Console;
use crate::format;
use crate::state::{Action, RepoState};
use crate::theme::{Tone, pal, space};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit;

fn accepts(a: &Artifact, scope: ArtifactScope, needle: &str, now: i64) -> bool {
    let in_scope = match scope {
        ArtifactScope::All => true,
        ArtifactScope::Expiring => a.expires_within(now, EXPIRES_SOON),
        ArtifactScope::Expired => a.expired,
    };
    in_scope
        && (needle.is_empty()
            || a.name.to_lowercase().contains(needle)
            || a.branch().to_lowercase().contains(needle)
            || a.sha().starts_with(needle)
            || a.run_id().is_some_and(|r| r.to_string() == needle))
}

/// The artifacts the list shows, in order.
pub fn visible(repo: &RepoState) -> Vec<&Artifact> {
    let ci = &repo.devops.ci;
    let now = format::now_secs();
    let needle = ci.artifact_filter.trim().to_lowercase();
    ci.artifacts
        .ok()
        .map(|l| {
            l.items
                .iter()
                .filter(|a| accepts(a, ci.artifact_scope, &needle, now))
                .collect()
        })
        .unwrap_or_default()
}

fn expiry(a: &Artifact, now: i64) -> Option<(String, Tone)> {
    if a.expired {
        return Some(("Expired".to_owned(), Tone::Neutral));
    }
    let e = a.expires()?;
    let left = e - now;
    if left <= EXPIRES_SOON {
        let text = if left < 3600 {
            "Expires within the hour".to_owned()
        } else if left < 86_400 {
            format!("Expires in {}h", left / 3600)
        } else {
            format!("Expires in {}d", left / 86_400)
        };
        return Some((text, Tone::Warning));
    }
    None
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, console: &Console, actions: &mut Vec<Action>) {
    let now = format::now_secs();
    let runs: HashMap<u64, WorkflowRun> = repo
        .github
        .as_ref()
        .map(|d| d.runs.iter().map(|r| (r.id, r.clone())).collect())
        .unwrap_or_default();
    let in_graph = |sha: &str| {
        gitvis_core::Oid::from_str(sha).ok().filter(|o| {
            repo.snapshot
                .as_ref()
                .is_some_and(|s| s.index_of.contains_key(o))
        })
    };
    let selected_sha = repo
        .devops
        .ci
        .artifact_selected
        .and_then(|id| {
            repo.devops
                .ci
                .artifacts
                .ok()
                .and_then(|l| l.items.iter().find(|a| a.id == id))
        })
        .map(|a| a.sha().to_owned())
        .unwrap_or_default();
    let jump = in_graph(&selected_sha);
    let State {
        artifacts,
        artifact_filter,
        artifact_scope,
        artifact_selected,
        artifact_busy,
        downloads,
        ..
    } = &mut repo.devops.ci;
    let key = artifact_selected.map(|i| i.to_string()).unwrap_or_default();
    let selected = *artifact_selected;
    let list = artifacts.ok();
    let items: Vec<Artifact> = list.map(|l| l.items.clone()).unwrap_or_default();
    let load = &*artifacts;
    kit::split_keyed(
        ui,
        "ci_artifacts",
        &key,
        actions,
        |ui, actions| {
            let hint = format!("Filter {} artifacts", items.len());
            ui.horizontal(|ui| {
                if !artifact_filter.is_empty() {
                    ui.set_max_width(ui.available_width() - 8.0);
                }
                kit::search_box(ui, "ci_artifact_filter", artifact_filter, &hint);
            });
            ui.add_space(space::S);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                ui.add_space(2.0);
                for (s, label) in [
                    (ArtifactScope::All, "All"),
                    (ArtifactScope::Expiring, "Expiring soon"),
                    (ArtifactScope::Expired, "Expired"),
                ] {
                    let n = items.iter().filter(|a| accepts(a, s, "", now)).count();
                    if kit::chip(
                        ui,
                        &format!("{label}  {}", format::count(n)),
                        *artifact_scope == s,
                    )
                    .clicked()
                    {
                        *artifact_scope = s;
                    }
                }
            });
            ui.add_space(space::S);
            if list.is_none() {
                widgets::loaded(
                    ui,
                    load,
                    "Loading the artifacts…",
                    Act::Refresh(super::Tab::Artifacts).into(),
                    actions,
                );
                return;
            }
            let needle = artifact_filter.trim().to_lowercase();
            let shown: Vec<&Artifact> = items
                .iter()
                .filter(|a| accepts(a, *artifact_scope, &needle, now))
                .collect();
            if items.is_empty() {
                kit::muted(
                    ui,
                    "This repository has no artifacts. Workflows create them with actions/upload-artifact; GitHub keeps them 90 days unless the workflow sets retention-days.",
                );
                return;
            }
            if shown.is_empty() {
                kit::muted(ui, "No artifacts match the filter.");
                return;
            }
            kit::list(
                ui,
                "ci_artifact_list",
                shown.len(),
                kit::ROW_H,
                |i| selected == Some(shown[i].id),
                |ui, rect, resp, i| {
                    let a = shown[i];
                    let wf = a
                        .run_id()
                        .and_then(|r| runs.get(&r))
                        .map(|r| r.name.clone());
                    let mut sub: Vec<String> = Vec::new();
                    if let Some(w) = wf {
                        sub.push(w);
                    }
                    if let Some(r) = a.run_id() {
                        sub.push(format!("run {r}"));
                    }
                    if !a.branch().is_empty() {
                        sub.push(a.branch().to_owned());
                    }
                    if !a.sha().is_empty() {
                        sub.push(widgets::short(a.sha()).to_owned());
                    }
                    let mut badges = Vec::new();
                    if let Some(b) = expiry(a, now) {
                        badges.push(b);
                    }
                    let meta = format!(
                        "{}  ·  {}",
                        bytes(a.size_in_bytes),
                        a.created()
                            .map(|c| format::relative(c, now))
                            .unwrap_or_default()
                    );
                    kit::paint_row(
                        ui,
                        rect,
                        &kit::Row::new(&a.name)
                            .leading(kit::Leading::Icon(Icon::Package, pal(ui).text_3))
                            .subtitle(&sub.join("  ·  "))
                            .meta(&meta)
                            .badges(&badges)
                            .dimmed(a.expired),
                        selected == Some(a.id),
                    );
                    if resp.clicked() {
                        actions.push(Act::SelectArtifact(Some(a.id)).into());
                    }
                },
            );
        },
        |ui, actions| {
            let Some(l) = list else {
                kit::title(ui, "Artifacts");
                widgets::loaded(
                    ui,
                    load,
                    "Loading the artifacts…",
                    Act::Refresh(super::Tab::Artifacts).into(),
                    actions,
                );
                return;
            };
            match selected.and_then(|id| l.items.iter().find(|a| a.id == id)) {
                None => overview(ui, &l.items, l.total, &runs, now),
                Some(a) => detail(
                    ui,
                    a,
                    &runs,
                    jump,
                    artifact_busy.contains(&a.id),
                    downloads.get(&a.id).and_then(|id| console.get(*id)),
                    now,
                    actions,
                ),
            }
        },
    );
}

fn overview(
    ui: &mut Ui,
    items: &[Artifact],
    total: u64,
    runs: &HashMap<u64, WorkflowRun>,
    now: i64,
) {
    kit::title(ui, "Artifacts");
    kit::secondary(
        ui,
        "Files uploaded by workflow runs with actions/upload-artifact. They count against the repository's Actions storage until they expire; delete large or stale ones, or download one to look at it.",
    );
    let size: u64 = items
        .iter()
        .filter(|a| !a.expired)
        .map(|a| a.size_in_bytes)
        .sum();
    let soon = items
        .iter()
        .filter(|a| a.expires_within(now, EXPIRES_SOON))
        .count();
    let expired = items.iter().filter(|a| a.expired).count();
    kit::stats(
        ui,
        &[
            kit::Stat::new(
                "Artifacts",
                if total as usize > items.len() {
                    format!("{} of {}", items.len(), format::count(total as usize))
                } else {
                    format::count(items.len())
                },
            ),
            kit::Stat::new("Stored", bytes(size))
                .tip("Size of the artifacts that have not expired"),
            kit::Stat::new("Expire within 3 days", format::count(soon))
                .tone_if(soon > 0, Tone::Warning),
            kit::Stat::new("Expired", format::count(expired)),
        ],
    );
    if total as usize > items.len() {
        kit::muted(
            ui,
            &format!(
                "The newest {} are listed (3 pages of 100); GitHub reports {} in total.",
                items.len(),
                format::count(total as usize)
            ),
        );
    }
    if items.is_empty() {
        return;
    }
    let mut largest: Vec<&Artifact> = items.iter().filter(|a| !a.expired).collect();
    largest.sort_by(|a, b| b.size_in_bytes.cmp(&a.size_in_bytes));
    kit::section(ui, "Largest", Some(largest.len().min(8)));
    let bars: Vec<(String, f64)> = largest
        .iter()
        .take(8)
        .map(|a| (a.name.clone(), a.size_in_bytes as f64))
        .collect();
    kit::bars(ui, &bars, pal(ui).accent, 8, |v| bytes(v as u64));
    let mut by_wf: HashMap<String, (u64, usize)> = HashMap::new();
    for a in items.iter().filter(|a| !a.expired) {
        let wf = a
            .run_id()
            .and_then(|r| runs.get(&r))
            .map(|r| r.name.clone())
            .unwrap_or_else(|| "Runs not in the run list".to_owned());
        let e = by_wf.entry(wf).or_default();
        e.0 += a.size_in_bytes;
        e.1 += 1;
    }
    let mut wf: Vec<(String, (u64, usize))> = by_wf.into_iter().collect();
    wf.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    kit::section(ui, "By workflow", Some(wf.len()));
    let bars: Vec<(String, f64)> = wf
        .iter()
        .map(|(n, (b, c))| (format!("{n} ({c})"), *b as f64))
        .collect();
    kit::bars(ui, &bars, pal(ui).purple, 10, |v| bytes(v as u64));
}

#[allow(clippy::too_many_arguments)]
fn detail(
    ui: &mut Ui,
    a: &Artifact,
    runs: &HashMap<u64, WorkflowRun>,
    jump: Option<gitvis_core::Oid>,
    busy: bool,
    download: Option<&crate::devops::console::ConsoleRun>,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let run = a.run_id().and_then(|r| runs.get(&r));
    widgets::header(ui, &a.name, |ui| {
        if busy {
            ui.add(egui::Spinner::new().size(14.0));
        } else if IconButton::new(Icon::Trash, "Delete")
            .compact()
            .danger()
            .show(ui)
            .on_hover_text("DELETE actions/artifacts/{id}")
            .clicked()
        {
            actions.push(Act::AskDeleteArtifact(a.id).into());
        }
        if !a.expired
            && a.run_id().is_some()
            && IconButton::new(Icon::Download, "Download…")
                .compact()
                .primary(true)
                .enabled(download.is_none_or(|d| !d.running()))
                .show(ui)
                .on_hover_text("Pick a folder, then gh run download RUN -n NAME -D FOLDER")
                .clicked()
        {
            actions.push(
                Act::DownloadArtifact {
                    id: a.id,
                    dir: None,
                }
                .into(),
            );
        }
    });
    let mut b: Vec<(String, Tone)> = vec![(bytes(a.size_in_bytes), Tone::Neutral)];
    if let Some(e) = expiry(a, now) {
        b.push(e);
    }
    widgets::badges(ui, &b);
    kit::kv(ui, 92.0, |kv| {
        kv.text(
            "Size",
            format!(
                "{}  ({} bytes)",
                bytes(a.size_in_bytes),
                format::count(a.size_in_bytes as usize)
            ),
        );
        match a.created() {
            Some(c) => kv.text("Created", widgets::when(c, now)),
            None => kv.muted("Created", "unknown"),
        }
        match a.expires() {
            Some(e) if a.expired => kv.text("Expired", widgets::when(e, now)),
            Some(e) => kv.row("Expires", |ui| {
                let soon = a.expires_within(now, EXPIRES_SOON);
                let c = if soon { pal(ui).warning } else { pal(ui).text };
                ui.label(
                    egui::RichText::new(format!(
                        "{}  ·  in {}",
                        format::absolute(e),
                        format::duration(e - now)
                    ))
                    .color(c),
                );
            }),
            None => kv.muted("Expires", "never"),
        }
        if let Some(r) = a.run_id() {
            kv.row("Run", |ui| {
                let label = match run {
                    Some(x) => format!("{} #{}", x.name, x.run_number),
                    None => format!("run {r}"),
                };
                if kit::link(ui, &label)
                    .on_hover_text("Show the run on the Runs tab")
                    .clicked()
                {
                    actions.push(Action::OpenRun { run: r, job: None });
                }
            });
        }
        if let Some(x) = run {
            kv.text("Workflow", kit::mono(&x.path));
            kv.text("Trigger", &x.event);
        }
        if !a.branch().is_empty() {
            kv.text("Branch", a.branch());
        }
        if !a.sha().is_empty() {
            kv.row("Commit", |ui| {
                let r = kit::link(ui, widgets::short(a.sha()));
                let r = r.on_hover_text(if jump.is_some() {
                    "Show this commit in the graph"
                } else {
                    "Copy the full SHA (the commit is not in the graph)"
                });
                if r.clicked() {
                    match jump {
                        Some(o) => actions.push(Action::JumpTo(o)),
                        None => actions.push(Action::Copy(a.sha().to_owned())),
                    }
                }
            });
        }
        if let Some(d) = &a.digest {
            kv.text("Digest", kit::mono(d));
        }
        kv.text("Artifact id", kit::mono(a.id.to_string()));
    });
    if let Some(d) = download {
        kit::section(ui, "Download", None);
        let tone = crate::views::console::status_tone(d);
        let title = if d.running() {
            "Downloading…".to_owned()
        } else {
            d.status_text()
        };
        kit::callout_with(ui, tone, &title, &d.command, |ui| {
            let tail = d.tail(4);
            if !tail.trim().is_empty() {
                ui.label(kit::mono(tail).color(pal(ui).text_2));
            }
            ui.add_space(2.0);
            widgets::note(
                ui,
                &format!(
                    "{}  ·  the full output is in the activity log",
                    format::millis(d.elapsed_ms())
                ),
            );
        });
    }
    if a.expired {
        kit::callout(
            ui,
            Tone::Neutral,
            "Expired",
            "GitHub deleted the files when the artifact expired; only this record is left. Deleting it removes the record.",
        );
    }
}
