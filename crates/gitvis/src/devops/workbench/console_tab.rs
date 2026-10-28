//! Console: every process gitvis streamed for this repository (task runs,
//! `git bisect run`, and the other pages' runs), newest first, with the
//! selected run's output.

use egui::{RichText, Ui};

use super::{Act, ui as wui};
use crate::devops::console::{Console, ConsoleAction};
use crate::format;
use crate::state::{Action, RepoState};
use crate::theme::{pal, size};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit;

pub fn show(ui: &mut Ui, repo: &mut RepoState, console: &Console, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let runs: Vec<_> = console.for_repo(repo.id).collect();
    let running = runs.iter().filter(|r| r.running()).count();
    let finished = runs.len() - running;
    let failed = runs.iter().filter(|r| r.failed()).count();
    crate::devops::pages::layout::page_header(ui, "Console", super::Tab::Console.hint(), |ui| {
        if IconButton::new(Icon::Trash, "Clear finished")
            .compact()
            .enabled(finished > 0)
            .show(ui)
            .on_hover_text("Remove every finished run of this repository (running ones stay)")
            .clicked()
        {
            actions.push(Action::Console(ConsoleAction::ClearFinished));
        }
        if !runs.is_empty() {
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!(
                    "{}  ·  {} running  ·  {} failed  ·  kept until gitvis closes",
                    format::plural(runs.len(), "run", "runs"),
                    running,
                    failed
                ))
                .size(size::SMALL)
                .color(p.text_2),
            );
        }
    });
    if runs.is_empty() {
        wui::empty_pane(ui, |ui| {
            if kit::empty_state(
                ui,
                Icon::Terminal,
                "Nothing has run yet",
                "Task runs, git bisect run, local workflow runs, terraform plans, kubectl logs and image scans started for this repository appear here with their full output, from every page.",
                Some((Icon::Play, "Go to tasks")),
            ) {
                actions.push(Act::SetTab(super::Tab::Tasks).into());
            }
        });
        return;
    }
    let wb = &mut repo.devops.workbench;
    let selected = wb
        .console_selected
        .filter(|id| runs.iter().any(|r| r.id == *id))
        .or_else(|| runs.first().map(|r| r.id));
    let repo_id = repo.id;
    wui::list_pane(ui, "wb_console_list", 360.0, |ui| {
        kit::list_header(ui, "Runs", Some(runs.len()), |_| {});
        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .id_salt("wb_console_runs")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some(id) =
                    crate::views::console::run_list(ui, console, repo_id, selected, |_| true)
                {
                    actions.push(Act::SelectRun(id).into());
                }
            });
    });
    let Some(run) = selected.and_then(|id| console.get(id)) else {
        return;
    };
    let fill = if ui.visuals().dark_mode {
        p.chrome
    } else {
        p.canvas
    };
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(fill))
        .show(ui, |ui| {
            crate::views::console::output(ui, run, "wb_console", actions);
        });
}
