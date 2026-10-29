//! What the forge pages show until the CLI is installed and signed in:
//! which tool, why it is needed, the exact commands that fix it, "Check
//! again", and for GitLab the public read-only fallback.

use egui::{RichText, Ui};

use gitvis_core::devops::forge::tools::{self as ftools, ToolState};
use gitvis_core::devops::forge::{ForgeKind, ForgeRemote};

use super::{Act, State};
use crate::state::Action;
use crate::theme::{Tone, pal, semibold, size, space};
use crate::views::icons::{self, Icon, IconButton};
use crate::views::kit;

/// Draw the gate when the CLI is not usable; returns true when the page
/// should draw its content instead.
pub fn show(
    ui: &mut Ui,
    remote: &ForgeRemote,
    st: &State,
    page_title: &str,
    actions: &mut Vec<Action>,
) -> bool {
    if st.can_read() {
        return true;
    }
    let tool = ftools::tool(remote.kind);
    kit::page_header(
        ui,
        page_title,
        &format!(
            "{} · {} on {}",
            remote.kind.label(),
            remote.path,
            remote.host
        ),
        |_| {},
    );
    egui::ScrollArea::vertical()
        .id_salt("forge_gate")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(28, 24))
                .show(ui, |ui| {
                    ui.set_max_width(ui.available_width().min(kit::PROSE + 40.0));
                    let Some(state) = &st.tool else {
                        kit::loading(ui, &format!("Checking {} ({})…", tool.name(), tool.program()));
                        kit::muted(
                            ui,
                            match remote.kind {
                                ForgeKind::GitLab => "glab auth status, then glab api user",
                                ForgeKind::Azure => {
                                    "az extension show --name azure-devops, then az account show"
                                }
                            },
                        );
                        return;
                    };
                    let p = *pal(ui);
                    let (icon, tone, title) = match state {
                        ToolState::Missing => (
                            Icon::Warning,
                            Tone::Warning,
                            format!("{} ({}) is not installed", tool.name(), tool.program()),
                        ),
                        ToolState::NoExtension { .. } => (
                            Icon::Warning,
                            Tone::Warning,
                            "The azure-devops extension of the Azure CLI is missing".to_owned(),
                        ),
                        ToolState::SignedOut { .. } => (
                            Icon::Lock,
                            Tone::Warning,
                            format!("{} is not signed in to {}", tool.name(), remote.host),
                        ),
                        ToolState::Ready { .. } => (Icon::Check, Tone::Success, "Ready".to_owned()),
                    };
                    ui.horizontal(|ui| {
                        icons::show(ui, icon, 22.0, tone.color(&p));
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(title)
                                .size(size::HEADING)
                                .family(semibold())
                                .color(p.text_strong),
                        );
                    });
                    ui.add_space(space::S);
                    let why = match remote.kind {
                        ForgeKind::GitLab => format!(
                            "This repository's remote is on GitLab ({}). gitvis reads and changes merge requests and pipelines through the GitLab CLI, which keeps your sign-in, so gitvis never stores a token.",
                            remote.web_url
                        ),
                        ForgeKind::Azure => format!(
                            "This repository's remote is on Azure DevOps ({}). gitvis reads and changes pull requests and pipelines through the Azure CLI with its azure-devops extension, which keeps your sign-in, so gitvis never stores a token.",
                            remote.web_url
                        ),
                    };
                    kit::body(ui, &why);
                    ui.add_space(space::S);
                    match state {
                        ToolState::SignedOut { version, detail } => {
                            kit::kv(ui, 120.0, |kv| {
                                kv.text("Installed", version.clone());
                                if !detail.is_empty() {
                                    kv.text("It said", detail.clone());
                                }
                            });
                        }
                        ToolState::NoExtension { version } => {
                            kit::kv(ui, 120.0, |kv| kv.text("Installed", version.clone()));
                        }
                        _ => {}
                    }
                    kit::section(ui, "How to fix it", None);
                    kit::secondary(ui, "Run these in a terminal, then check again:");
                    let cmds = ftools::fix_commands(remote, state).join("\n");
                    kit::code_block(ui, &cmds, actions);
                    if remote.kind == ForgeKind::Azure {
                        kit::muted(
                            ui,
                            &format!(
                                "A personal access token in the {} environment variable also works (set it before starting gitvis).",
                                ftools::AZURE_PAT_ENV
                            ),
                        );
                    }
                    ui.add_space(space::M);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = space::S;
                        if st.checking {
                            kit::loading(ui, "Checking…");
                        } else if IconButton::new(Icon::Rerun, "Check again")
                            .primary(true)
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Act::CheckTool.into());
                        }
                        if remote.kind == ForgeKind::GitLab
                            && IconButton::new(Icon::Eye, "Read public data without signing in")
                                .show(ui)
                                .on_hover_text(
                                    "Merge requests and pipelines of a public project through unauthenticated curl (read only)",
                                )
                                .clicked()
                        {
                            actions.push(Act::UsePublic(true).into());
                        }
                        if IconButton::new(Icon::External, "Open on the web")
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Action::OpenUrl(remote.web_url.clone()));
                        }
                    });
                    if remote.kind == ForgeKind::GitLab {
                        ui.add_space(space::S);
                        kit::secondary(
                            ui,
                            &format!(
                                "Without signing in, gitvis can read a public project through https://{}/api/v4: read only and rate limited, with job logs taken from each job's web page.",
                                remote.host
                            ),
                        );
                    }
                });
        });
    false
}

/// A slim strip under the page header while public mode is on: what it
/// means and the way out. (In the header it crowded the title on a narrow
/// window.)
pub fn public_strip(ui: &mut Ui, remote: &ForgeRemote, st: &State, actions: &mut Vec<Action>) {
    if !st.public {
        return;
    }
    let p = *pal(ui);
    egui::Frame::NONE
        .fill(
            p.warning
                .gamma_multiply(if ui.visuals().dark_mode { 0.10 } else { 0.08 }),
        )
        .inner_margin(egui::Margin::symmetric(24, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                icons::show(ui, Icon::Eye, 14.0, p.warning);
                ui.label(
                    RichText::new("Public, read only")
                        .size(size::SMALL)
                        .family(semibold())
                        .color(p.text_strong),
                );
                ui.label(
                    RichText::new(format!(
                        "Reading {} without signing in; changes are off.",
                        remote.host
                    ))
                    .size(size::SMALL)
                    .color(p.text),
                );
                if IconButton::text("Leave public mode")
                    .compact()
                    .show(ui)
                    .clicked()
                {
                    actions.push(Act::UsePublic(false).into());
                }
            });
        });
    kit::hairline(ui);
}

/// The part of the line under a page title that says who is signed in
/// (empty in public mode, which [`public_strip`] explains).
pub fn mode_line(st: &State) -> String {
    if st.public {
        return String::new();
    }
    match &st.tool {
        Some(ToolState::Ready { version, user, via }) => {
            let who = if user.is_empty() {
                via.clone()
            } else {
                format!("{user} via {via}")
            };
            format!("{who}, {version}")
        }
        _ => String::new(),
    }
}
