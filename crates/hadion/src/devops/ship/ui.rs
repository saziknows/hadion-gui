//! Pieces the Ship tabs share: gates (no GitHub remote, `gh` missing),
//! loading / error / empty states, state badges and commit links.

use egui::{Sense, Ui};

use hadion_core::Oid;
use hadion_core::devops::ship::deploy::DeployState;
use hadion_core::devops::ship::dora::Band;
use hadion_core::devops::tools::Tool;

use super::{Act, Load};
use crate::state::{Action, RepoState};
use crate::theme::{Tone, pal};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit;

/// Can the GitHub-backed content be shown? Draws the reason when not.
pub fn gate(ui: &mut Ui, repo: &RepoState, actions: &mut Vec<Action>) -> bool {
    if repo.github_remote.is_none() {
        kit::empty_state(
            ui,
            Icon::Cloud,
            "No GitHub remote",
            "Releases, environments and deployments come from GitHub.\nAdd a remote on github.com (git remote add origin https://github.com/OWNER/REPO.git) and open this page again.",
            None,
        );
        return false;
    }
    if repo.devops.ship.gh_ok == Some(false) {
        tool_missing(ui, actions);
        return false;
    }
    true
}

/// The GitHub CLI is not installed: what, why, how, check again.
pub fn tool_missing(ui: &mut Ui, actions: &mut Vec<Action>) {
    let tool = Tool::Gh;
    if kit::empty_state(
        ui,
        Icon::Warning,
        &format!("{} ({}) is not installed", tool.name(), tool.program()),
        &format!(
            "Hadion reads releases, environments and deployments with your `gh auth` login and publishes releases through `gh release create`. {}",
            tool.install_hint()
        ),
        Some((Icon::Rerun, "Check again")),
    ) {
        actions.push(Act::CheckTools.into());
    }
}

/// The loading / error part of a [`Load`]: returns the data when there
/// is some (possibly being refreshed), else draws the state and returns
/// `None`. `retry` is pushed by the error state's button.
pub fn loaded<'a, T>(
    ui: &mut Ui,
    load: &'a Load<T>,
    what: &str,
    loading_text: &str,
    retry: Act,
    actions: &mut Vec<Action>,
) -> Option<&'a T> {
    match &load.result {
        Some(Ok(v)) => Some(v),
        Some(Err(e)) if !load.loading => {
            if kit::empty_state(
                ui,
                Icon::Warning,
                &format!("Could not load {what}"),
                e,
                Some((Icon::Rerun, "Try again")),
            ) {
                actions.push(retry.into());
            }
            None
        }
        _ => {
            if load.loading {
                kit::empty_state(ui, Icon::Clock, loading_text, "", None);
            } else if kit::empty_state(
                ui,
                Icon::Cloud,
                &format!("{what} not loaded"),
                "",
                Some((Icon::Fetch, "Load")),
            ) {
                actions.push(retry.into());
            }
            None
        }
    }
}

/// An inline error with a retry link (sections inside a page).
pub fn inline_error(ui: &mut Ui, what: &str, error: &str, retry: Act, actions: &mut Vec<Action>) {
    kit::callout_with(
        ui,
        Tone::Danger,
        &format!("Could not load {what}"),
        error,
        |ui| {
            if IconButton::new(Icon::Rerun, "Try again")
                .compact()
                .show(ui)
                .clicked()
            {
                actions.push(retry.into());
            }
        },
    );
}

pub fn state_tone(s: DeployState) -> Tone {
    match s {
        DeployState::Active | DeployState::Success => Tone::Success,
        DeployState::Failure | DeployState::Error => Tone::Danger,
        DeployState::Waiting => Tone::Warning,
        DeployState::Pending | DeployState::Queued | DeployState::InProgress => Tone::Accent,
        _ => Tone::Neutral,
    }
}

pub fn state_badge(s: DeployState) -> (String, Tone) {
    (s.label().to_owned(), state_tone(s))
}

pub fn band_tone(b: Band) -> Tone {
    match b {
        Band::Elite => Tone::Success,
        Band::High => Tone::Accent,
        Band::Medium => Tone::Warning,
        Band::Low => Tone::Danger,
    }
}

/// A short commit id: accent and clickable (shows it in the graph) when
/// the commit is in the graph, plain otherwise. Returns true when clicked.
pub fn commit_link(ui: &mut Ui, repo: &RepoState, sha: &str, actions: &mut Vec<Action>) -> bool {
    let p = *pal(ui);
    let short = &sha[..sha.len().min(10)];
    let oid = Oid::from_str(sha).ok();
    let shown = oid.is_some_and(|o| {
        repo.snapshot
            .as_ref()
            .is_some_and(|s| s.index_of.contains_key(&o))
    });
    let resp = ui.add(
        egui::Label::new(kit::mono(short).color(if shown { p.accent } else { p.text }))
            .sense(Sense::click()),
    );
    let resp = if shown {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    };
    resp.context_menu(|ui| {
        if ui.button("Copy commit id").clicked() {
            actions.push(Action::Copy(sha.to_owned()));
            ui.close();
        }
    });
    if resp.clicked()
        && let (true, Some(o)) = (shown, oid)
    {
        actions.push(Action::JumpTo(o));
        return true;
    }
    false
}

/// Is the commit in the graph?
pub fn in_graph(repo: &RepoState, sha: &str) -> bool {
    Oid::from_str(sha).ok().is_some_and(|o| {
        repo.snapshot
            .as_ref()
            .is_some_and(|s| s.index_of.contains_key(&o))
    })
}

/// "fetched 3m ago" for a header.
pub fn fetched<T>(load: &Load<T>, now: i64) -> String {
    match load.at {
        Some(at) if load.loading => format!(
            "refreshing (last {})",
            crate::format::relative(now - at.elapsed().as_secs() as i64, now)
        ),
        Some(at) => format!(
            "fetched {}",
            crate::format::relative(now - at.elapsed().as_secs() as i64, now)
        ),
        None if load.loading => "loading".to_owned(),
        None => String::new(),
    }
}

/// `2026-10-23 20:19  ·  3d ago` (relative only when recent).
pub fn when(t: i64, now: i64) -> String {
    crate::views::insights::when(t, now)
}

/// A duration such as `4m 08s`.
pub fn dur(secs: i64) -> String {
    crate::format::duration(secs)
}

/// A long duration in the largest sensible unit: `3.2 h`, `4.5 d`.
pub fn span(secs: i64) -> String {
    let s = secs.max(0) as f64;
    if s < 3_600.0 {
        format!("{:.0} min", (s / 60.0).max(1.0))
    } else if s < 86_400.0 {
        format!("{:.1} h", s / 3_600.0)
    } else {
        format!("{:.1} d", s / 86_400.0)
    }
}

/// A scrolling page (the script driver's `ship-scroll N` scrolls it).
pub fn scroll_page(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    crate::devops::pages::layout::page(ui, id, add);
}
