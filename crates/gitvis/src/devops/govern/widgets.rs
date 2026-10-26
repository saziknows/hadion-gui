//! Pieces shared by the Govern views: loading, error and refusal states,
//! the "read N ago" label, missing tools, list headers, code snippets.

use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Margin, RichText, Sense, Stroke, Ui, vec2,
};

use gitvis_core::devops::gh::Gh;
use gitvis_core::devops::govern::alerts::Severity;
use gitvis_core::devops::govern::{self as gcore, Feature};
use gitvis_core::devops::tools::Tool;

use super::{Act, Fetch, Tab};
use crate::format;
use crate::state::{Action, RepoState};
use crate::theme::{pal, size};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Tone};

/// `gh api` bound to the repository, for links and explanations.
pub fn gh(repo: &RepoState) -> Option<Gh> {
    repo.github_remote
        .as_ref()
        .map(|(o, r)| Gh::new(repo.workdir.clone(), o.clone(), r.clone()))
}

pub fn severity_tone(s: Severity) -> Tone {
    match s {
        Severity::Critical | Severity::High => Tone::Danger,
        Severity::Medium => Tone::Warning,
        Severity::Low | Severity::Unknown => Tone::Neutral,
    }
}

pub fn severity_badge(s: Severity) -> (String, Tone) {
    let label = match s {
        Severity::Unknown => "Info".to_owned(),
        s => s.label().to_owned(),
    };
    (label, severity_tone(s))
}

/// Colour of a list dot for a severity.
pub fn severity_dot(ui: &Ui, s: Severity) -> Color32 {
    let p = pal(ui);
    match severity_tone(s) {
        Tone::Neutral => p.text_3,
        t => t.color(p),
    }
}

/// Header controls: a refresh button (spinner while reading).
pub fn refresh(ui: &mut Ui, loading: bool, tip: &str, actions: &mut Vec<Action>) {
    if loading {
        ui.add(egui::Spinner::new().size(14.0).color(pal(ui).text_2));
        return;
    }
    if IconButton::new(Icon::Rerun, "Refresh")
        .compact()
        .show(ui)
        .on_hover_text(tip)
        .clicked()
    {
        actions.push(Act::Refresh.into());
    }
}

/// "read 2 min ago" for a header.
pub fn fetched<T>(ui: &mut Ui, f: &Fetch<T>, extra: &str) {
    let Some(at) = f.at else { return };
    let now = format::now_secs();
    let when = format::relative(now - at.elapsed().as_secs() as i64, now);
    let text = if extra.is_empty() {
        format!("read {when}")
    } else {
        format!("{extra}  ·  read {when}")
    };
    ui.label(RichText::new(text).size(size::SMALL).color(pal(ui).text_2));
}

/// A centred spinner with a line of text.
pub fn loading(ui: &mut Ui, text: &str) {
    ui.add_space(24.0);
    ui.horizontal(|ui| {
        ui.add_space(28.0);
        kit::loading(ui, text);
    });
}

/// A refusal or failure, explained: what GitHub said, why, and what to do.
pub fn denied(
    ui: &mut Ui,
    gh: Option<&Gh>,
    access: Option<&gcore::Access>,
    feature: Feature,
    error: &str,
    actions: &mut Vec<Action>,
) {
    let Some(gh) = gh else {
        failed(ui, "The request failed", error, Act::Refresh, actions);
        return;
    };
    let d = gcore::explain(feature, error, access, gh);
    // A known refusal or a feature that is off is a warning; anything
    // unexplained is a failure.
    let tone = if d.title == "GitHub refused the request" {
        Tone::Danger
    } else {
        Tone::Warning
    };
    kit::callout_with(ui, tone, &d.title, &d.body, |ui| {
        if !d.steps.is_empty() {
            ui.add_space(2.0);
            for (i, step) in d.steps.iter().enumerate() {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("{}. {step}", i + 1))
                            .size(size::SMALL)
                            .color(pal(ui).text),
                    )
                    .wrap(),
                );
            }
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new("GitHub said")
                .size(size::SMALL)
                .color(pal(ui).text_2),
        );
        let (said, docs) = split_docs(error);
        kit::code_block(ui, said, actions);
        if let Some(url) = docs
            && kit::link(ui, &format!("GitHub docs: {url}")).clicked()
        {
            actions.push(Action::OpenUrl(url.to_owned()));
        }
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if IconButton::new(Icon::Rerun, "Try again")
                .compact()
                .show(ui)
                .clicked()
            {
                actions.push(Act::Refresh.into());
            }
            if let Some(url) = &d.url
                && IconButton::new(Icon::External, "Open the settings")
                    .compact()
                    .show(ui)
                    .clicked()
            {
                actions.push(Action::OpenUrl(url.clone()));
            }
        });
    });
}

/// `message (HTTP 403)⏎See https://docs…` → the message and the docs URL.
pub fn split_docs(error: &str) -> (&str, Option<&str>) {
    match error.find("\nSee https://") {
        Some(i) => (error[..i].trim_end(), Some(error[i + 5..].trim())),
        None => (error, None),
    }
}

/// A local failure with its exact error and a retry.
pub fn failed(ui: &mut Ui, title: &str, error: &str, retry: Act, actions: &mut Vec<Action>) {
    kit::callout_with(ui, Tone::Danger, title, "", |ui| {
        kit::code_block(ui, error, actions);
        if IconButton::new(Icon::Rerun, "Try again")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(retry.into());
        }
    });
}

/// The section needs a GitHub remote and the repository has none.
pub fn no_github(ui: &mut Ui, repo: &RepoState, tab: Tab) {
    let other = repo
        .forge_remote
        .as_ref()
        .map(|f| format!(" This repository's remote is on {}.", f.host))
        .unwrap_or_default();
    kit::empty_state(
        ui,
        Icon::Lock,
        "No GitHub remote",
        &format!(
            "{} lives on GitHub: add a remote on github.com (git remote add origin https://github.com/OWNER/REPO.git) and gitvis reads it with your gh login.{other} SBOM, container images and CODEOWNERS work without one.",
            tab.label()
        ),
        None,
    );
}

/// A tool that is not installed: what it is, why it is needed, how to get it.
pub fn missing_tool(ui: &mut Ui, tool: Tool, why: &str, actions: &mut Vec<Action>) {
    kit::callout_with(
        ui,
        Tone::Warning,
        &format!("{} is not installed", tool.name()),
        why,
        |ui| {
            ui.label(
                RichText::new("Install")
                    .size(size::SMALL)
                    .color(pal(ui).text_2),
            );
            kit::code_block(ui, tool.install_hint(), actions);
            if IconButton::new(Icon::Rerun, "Check again")
                .compact()
                .show(ui)
                .clicked()
            {
                actions.push(Act::DetectScanners.into());
            }
        },
    );
}

/// A list pane header: title, count, and an optional "Overview" link.
pub fn list_head(
    ui: &mut Ui,
    title: &str,
    count: usize,
    selected: bool,
    deselect: Act,
    actions: &mut Vec<Action>,
) {
    kit::list_header(ui, title, Some(count), |ui| {
        if selected {
            ui.add_space(4.0);
            if kit::link(ui, "Overview").clicked() {
                actions.push(deselect.into());
            }
        }
    });
    ui.add_space(6.0);
}

/// A filter box in a list pane.
pub fn list_filter(ui: &mut Ui, id: &str, text: &mut String, hint: &str) {
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        ui.set_width(ui.available_width() - 4.0);
        kit::search_box(ui, id, text, hint);
    });
    ui.add_space(6.0);
}

/// A "nothing here" note in a list pane.
pub fn list_none(ui: &mut Ui, text: &str) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        kit::muted(ui, text);
    });
}

/// Lines of code with line numbers; `highlight` lines are tinted.
pub fn code_lines(ui: &mut Ui, first: u64, lines: &[String], highlight: (u64, u64)) {
    let p = *pal(ui);
    let dark = ui.visuals().dark_mode;
    let line_h = 19.0;
    let font = FontId::monospace(size::MONO);
    Frame::NONE
        .fill(if dark { p.chrome } else { p.raised })
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(crate::theme::radius::M))
        .inner_margin(Margin::symmetric(0, 6))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            let w = ui.available_width();
            for (i, l) in lines.iter().enumerate() {
                let n = first + i as u64;
                let (rect, _) = ui.allocate_exact_size(vec2(w, line_h), Sense::hover());
                if n >= highlight.0 && n <= highlight.1 {
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::ZERO,
                        p.warning.gamma_multiply(0.16),
                    );
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(rect.min, vec2(3.0, line_h)),
                        CornerRadius::ZERO,
                        p.warning,
                    );
                }
                ui.painter().text(
                    egui::pos2(rect.left() + 46.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    n.to_string(),
                    FontId::monospace(size::CAPTION),
                    p.text_3,
                );
                let g = crate::views::truncated(
                    ui.painter(),
                    &l.replace('\t', "    "),
                    font.clone(),
                    p.text,
                    (w - 70.0).max(40.0),
                );
                ui.painter().galley(
                    egui::pos2(rect.left() + 58.0, rect.center().y - g.size().y / 2.0),
                    g,
                    p.text,
                );
            }
        });
}

/// Severity stat tiles: critical, high, medium, low.
pub fn severity_tiles(counts: &[usize; 5]) -> Vec<kit::Stat<'static>> {
    vec![
        kit::Stat::new("Critical", counts[0].to_string()).tone_if(counts[0] > 0, Tone::Danger),
        kit::Stat::new("High", counts[1].to_string()).tone_if(counts[1] > 0, Tone::Danger),
        kit::Stat::new("Medium", counts[2].to_string()).tone_if(counts[2] > 0, Tone::Warning),
        kit::Stat::new("Low", counts[3].to_string()),
    ]
}

/// `2026-10-26 10:04  ·  3d ago` (or `· 2.6 y ago`), or a dash.
pub fn when(t: Option<i64>) -> String {
    match t {
        Some(t) => crate::views::insights::when(t, format::now_secs()),
        None => "–".into(),
    }
}

/// The role line under a header ("Your role: admin").
pub fn role_badge(ui: &mut Ui, repo: &RepoState) {
    if let Some(a) = repo.devops.govern.access.ok() {
        let tone = if a.admin {
            Tone::Success
        } else {
            Tone::Neutral
        };
        kit::badge(ui, &format!("Role: {}", a.role()), tone);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docs_links_are_split_off() {
        let (m, d) = split_docs("Not Found (HTTP 404)\nSee https://docs.github.com/rest");
        assert_eq!(m, "Not Found (HTTP 404)");
        assert_eq!(d, Some("https://docs.github.com/rest"));
        assert_eq!(split_docs("plain"), ("plain", None));
    }
}
