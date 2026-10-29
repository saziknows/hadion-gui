//! The Containers tab: the repository's compose projects and containers on
//! a docker context, live while shown.

use std::time::Instant;

use egui::{Frame, Margin, RichText, Sense, Ui, vec2};

use hadion_core::devops::runtime::docker::commands::{self, ComposeOp, ComposeTarget, ContainerOp};
use hadion_core::devops::runtime::docker::compose::{self, ComposeConfig, Service};
use hadion_core::devops::runtime::docker::model::{
    Container, HealthStatus, Image, State as CState,
};
use hadion_core::devops::runtime::docker::revision::{self, Evidence, Revision};
use hadion_core::devops::runtime::docker::scope::Project;
use hadion_core::devops::runtime::docker::triage::{self, Problem};
use hadion_core::devops::runtime::docker::{endpoint, model};
use hadion_core::devops::runtime::kube;
use hadion_core::devops::tools::Tool;

use super::common::{self, Cx};
use super::docker::{self, Act, BuildSource, LiveMode, Row, Sel, State};
use super::{PkgState, RefCache};
use crate::devops::console::Console;
use crate::devops::pages::{explain, layout};
use crate::format;
use crate::state::{Action, RepoState, Settings};
use crate::theme::{Palette, pal, size};
use crate::views::form;
use crate::views::icons::{Icon, IconButton};
use crate::views::insights::{Cell, detail_title, grid};
use crate::views::kit::{self, Col, Leading, Stat, Tone};

fn act(a: Act) -> Action {
    super::Act::Docker(a).into()
}

/// What the view needs from the repository besides [`Cx`].
pub struct DockerCx {
    pub remotes: Vec<String>,
    pub safe_mode: bool,
    pub repo_name: String,
    /// `(full id, short id, tag at HEAD, working tree dirty)`.
    pub head: Option<(String, String, Option<String>, bool)>,
    /// `org.opencontainers.image.source` for builds.
    pub source: Option<String>,
}

impl DockerCx {
    pub fn of(repo: &RepoState, settings: &Settings) -> Self {
        let head = repo.snapshot.as_ref().and_then(|s| {
            let h = s.head.target()?;
            let full = h.to_string();
            let tag = s
                .tags
                .iter()
                .filter(|t| t.target == h)
                .map(|t| t.name.clone())
                .next();
            Some((
                full.clone(),
                full[..7].to_owned(),
                tag,
                !s.status.is_clean(),
            ))
        });
        Self {
            remotes: repo
                .snapshot
                .as_ref()
                .map(|s| s.remotes.iter().map(|r| r.url.clone()).collect())
                .unwrap_or_default(),
            safe_mode: settings.safe_mode,
            repo_name: repo
                .snapshot
                .as_ref()
                .map(|s| s.name.clone())
                .unwrap_or_else(|| repo.name.clone()),
            head,
            source: docker::source_url(repo),
        }
    }
}

// ---------------------------------------------------------------------------
// Tones and texts
// ---------------------------------------------------------------------------

fn state_tone(c: &Container) -> Tone {
    match c.state {
        CState::Running if c.unhealthy() => Tone::Danger,
        CState::Running if c.health_status() == HealthStatus::Starting => Tone::Accent,
        CState::Running => Tone::Success,
        CState::Restarting => Tone::Warning,
        CState::Paused => Tone::Purple,
        CState::Exited(0) | CState::Created => Tone::Neutral,
        CState::Exited(_) if c.started.is_none() => Tone::Neutral,
        CState::Exited(_) | CState::Dead => Tone::Danger,
        CState::Removing | CState::Unknown => Tone::Neutral,
    }
}

/// The state alone (health has its own badge): for state badges and
/// columns.
fn run_tone(c: &Container) -> Tone {
    match c.state {
        CState::Running => Tone::Success,
        CState::Restarting => Tone::Warning,
        CState::Paused => Tone::Purple,
        CState::Exited(code) if code != 0 && c.started.is_some() => Tone::Danger,
        CState::Dead => Tone::Danger,
        _ => Tone::Neutral,
    }
}

/// `40 s ago` for the last two minutes, else the usual relative time.
fn secs_ago(t: Option<i64>, now: i64) -> String {
    match t {
        Some(t) if (0..120).contains(&(now - t)) => format!("{} s ago", now - t),
        other => common::ago(other, now),
    }
}

fn dot(p: &Palette, t: Tone) -> egui::Color32 {
    match t {
        Tone::Neutral => p.text_3,
        t => t.color(p),
    }
}

fn health_badge(h: HealthStatus) -> Option<(String, Tone)> {
    match h {
        HealthStatus::Healthy => Some(("Healthy".into(), Tone::Success)),
        HealthStatus::Unhealthy => Some(("Unhealthy".into(), Tone::Danger)),
        HealthStatus::Starting => Some(("Health: starting".into(), Tone::Accent)),
        HealthStatus::None => None,
    }
}

/// The badges of a container, most important first.
fn container_badges(c: &Container) -> Vec<(String, Tone)> {
    let mut b = Vec::new();
    if c.oom_killed {
        b.push(("Out of memory".into(), Tone::Danger));
    }
    match c.state {
        CState::Restarting => b.push((format!("Restarting · {}×", c.restart_count), Tone::Warning)),
        CState::Exited(code) if code != 0 && c.started.is_some() => {
            b.push((format!("Exit {code}"), Tone::Danger))
        }
        CState::Paused => b.push(("Paused".into(), Tone::Purple)),
        CState::Dead => b.push(("Dead".into(), Tone::Danger)),
        _ => {}
    }
    if let Some(h) = health_badge(c.health_status()).filter(|_| c.state.up()) {
        b.push(h);
    }
    b
}

/// `8080 → 80`, the first published port.
fn first_port(c: &Container) -> Option<String> {
    let p = c.ports.iter().find(|p| p.published())?;
    let more = c.ports.iter().filter(|p| p.published()).count() - 1;
    Some(format!(
        "{} → {}{}",
        p.host_port.unwrap_or(0),
        p.container_port,
        if more > 0 {
            format!(" +{more}")
        } else {
            String::new()
        }
    ))
}

fn status_text(c: &Container) -> String {
    if c.status_text.is_empty() {
        c.state.label()
    } else {
        c.status_text.clone()
    }
}

/// `web`, `web-2`, `migrate (run)` inside a project.
fn service_title(c: &Container) -> String {
    match &c.compose {
        Some(i) if !i.service.is_empty() => {
            let mut t = i.service.clone();
            if i.number.is_some_and(|n| n > 1) {
                t.push_str(&format!("-{}", i.number.unwrap_or(1)));
            }
            if i.oneoff {
                t.push_str(" (run)");
            }
            t
        }
        _ => c.name.clone(),
    }
}

fn short_image(image: &str) -> String {
    let i = kube::parse_image(image);
    match &i.tag {
        Some(t) => format!("{}:{t}", i.name()),
        None => i.name().to_owned(),
    }
}

/// The commit a container runs, matched like the Packages tab does (and
/// through a GHCR package version's tags for a pulled digest).
fn running_commit(
    c: &Container,
    image: Option<&Image>,
    refs: &RefCache,
    pkg: &PkgState,
    d: &DockerCx,
) -> Revision {
    let mut r = revision::revision(refs.idx.as_ref(), c, image, &d.remotes);
    if r.commit().is_none()
        && let (Some(img), Some(idx)) = (image, refs.idx.as_ref())
    {
        for full in &img.digests {
            if let Some(dg) = kube::digest_of(full)
                && let Some((_, v)) = pkg.tags_for_digest(dg, None)
                && let Some(m) = idx.best(v.tags())
                && m.commit.is_some()
            {
                r.found = Some((Evidence::Digest(dg.to_owned()), m));
                break;
            }
        }
    }
    r
}

/// The commit column of tables: `1a2b3c4`, `tag v1.2.3`, `compose build, no label`.
fn commit_cell(r: &Revision) -> Cell {
    match &r.found {
        Some((_, m)) if m.commit.is_some() => Cell::MonoLink(
            m.commit
                .map(|o| o.to_string()[..7].to_owned())
                .unwrap_or_default(),
        ),
        Some((_, m)) => Cell::Sec(common::match_text(m, None)),
        None if r.compose_built => Cell::Muted("compose build, no label".into()),
        None if r.label.is_some() => Cell::Muted("label names no commit here".into()),
        None => Cell::Muted("–".into()),
    }
}

fn relative(t: Option<i64>, now: i64) -> String {
    t.map(|t| common::since(t, now))
        .unwrap_or_else(|| "–".into())
}

// ---------------------------------------------------------------------------
// The tab
// ---------------------------------------------------------------------------

pub fn show(
    ui: &mut Ui,
    cx: &Cx,
    d: &DockerCx,
    rt: &mut super::State,
    console: &Console,
    actions: &mut Vec<Action>,
) {
    let super::State {
        docker: st,
        pkg,
        refs,
        ..
    } = rt;
    if st.egui.is_none() {
        st.egui = Some(ui.ctx().clone());
    }
    let needs = docker::frame(st, Instant::now());
    for a in needs.acts {
        actions.push(act(a));
    }
    if let Some(after) = needs.repaint {
        ui.ctx().request_repaint_after(after);
    }
    header(ui, cx, st, actions);
    if st.tool == Some(false) {
        common::page(ui, "docker_missing", |ui| {
            common::missing_tool(
                ui,
                "docker is not installed",
                "The Containers tab reads your containers and compose projects through the docker CLI, on the docker context you pick. Hadion never changes your docker configuration or the current context.",
                &[Tool::Docker.install_hint()],
                super::Act::Docker(Act::CheckTool),
                actions,
            );
        });
        return;
    }
    if st.contexts.is_none() {
        common::page(ui, "docker_loading", |ui| {
            kit::empty_state(
                ui,
                Icon::Container,
                if st.loading {
                    "Asking Docker"
                } else {
                    "Containers"
                },
                "Contexts come from your docker configuration; containers, images and compose projects from the chosen context.",
                None,
            );
        });
        return;
    }
    if let Some(Err(e)) = &st.contexts {
        let e = e.clone();
        common::page(ui, "docker_ctx_err", |ui| {
            common::error_block(
                ui,
                "docker could not list its contexts",
                &e,
                Some((
                    "docker context ls reads ~/.docker (or DOCKER_CONFIG).",
                    None,
                )),
                Some(("Try again", super::Act::Docker(Act::Refresh))),
                actions,
            );
        });
        return;
    }
    toolbar(ui, d, st, actions);
    if st.listing.is_none() {
        common::page(ui, "docker_wait", |ui| {
            kit::empty_state(
                ui,
                Icon::Container,
                if st.loading {
                    "Asking Docker"
                } else {
                    "Not loaded"
                },
                "Reading the version, the containers and their images, and the compose projects.",
                None,
            );
        });
        return;
    }
    if let Some((problem, e)) = st.problem() {
        let ctx = st.context_name().unwrap_or_default();
        let ep = st.endpoint();
        common::page(ui, "docker_problem", |ui| {
            problem_page(ui, st, problem, &e, &ctx, &ep, actions)
        });
        return;
    }
    common::list_pane(ui, "docker_list", |ui| list(ui, cx, st, actions));
    common::console_panel(
        ui,
        "docker_console",
        console,
        st.console_run,
        super::Act::Docker(Act::HideConsole),
        actions,
    );
    let key = format!("{:?}{}", st.selected, st.build.is_some());
    common::detail_pane(ui, ("docker_detail", key), |ui| {
        if st.build.is_some() {
            build_card(ui, d, st, console, actions);
            common::gap(ui);
        }
        match st.selected.clone() {
            None => overview(ui, cx, d, st, refs, pkg, actions),
            Some(Sel::Project(name)) => project_detail(ui, cx, d, st, refs, pkg, &name, actions),
            Some(Sel::File(rel)) => file_detail(ui, cx, st, &rel, actions),
            Some(Sel::Container(id)) => match st.container(&id).cloned() {
                Some(c) => container_detail(ui, cx, d, st, refs, pkg, &c, actions),
                None => overview(ui, cx, d, st, refs, pkg, actions),
            },
        }
    });
}

fn live_badge(ui: &mut Ui, st: &State) {
    let now = Instant::now();
    match &st.live.mode {
        LiveMode::Live => {
            let since = st
                .live
                .stream
                .as_ref()
                .map(|s| format::millis(now.duration_since(s.started).as_millis() as u64))
                .unwrap_or_default();
            let tip = match st.live.last_event() {
                Some((e, at)) => format!(
                    "Following docker events for {since}: {} so far; the last, {} ago: {}.\nThe list is read again half a second after things settle.",
                    format::plural(st.live.total_events as usize, "change", "changes"),
                    format::millis(now.duration_since(at).as_millis() as u64),
                    e.text()
                ),
                None => format!(
                    "Following docker events for {since}: the list is read again half a second after a container changes."
                ),
            };
            kit::badge(ui, "Live", Tone::Success).on_hover_text(tip);
        }
        LiveMode::Polling(why) => {
            kit::badge(ui, "Polling every 10 s", Tone::Warning).on_hover_text(format!(
                "{why}\nThe list is read again every 10 seconds while this tab is shown; Refresh tries live updates again."
            ));
        }
        LiveMode::Off if st.reachable() => {
            kit::badge(ui, "Connecting", Tone::Neutral);
        }
        LiveMode::Off => {}
    }
}

fn header(ui: &mut Ui, cx: &Cx, st: &State, actions: &mut Vec<Action>) {
    let mut parts: Vec<String> = Vec::new();
    if let Some(v) = &st.tool_version {
        parts.push(
            v.trim_start_matches("Docker version ")
                .to_owned()
                .replacen(", build", " build", 1),
        );
    }
    if let Some(l) = &st.listing {
        match &l.version {
            Ok(v) => {
                if let Some(s) = &v.server {
                    parts.push(format!(
                        "engine {} · {}/{}{}",
                        s.version,
                        s.os,
                        s.arch,
                        if s.platform.is_empty() {
                            String::new()
                        } else {
                            format!(" · {}", s.platform)
                        }
                    ));
                }
            }
            Err(_) => parts.push("engine not answering".into()),
        }
    }
    let f = common::fetched(st.fetched, Some(st.load_ms), cx.now);
    if !f.is_empty() {
        parts.push(f);
    }
    let sub = if parts.is_empty() {
        "Containers and compose projects of this repository on Docker".to_owned()
    } else {
        format!("docker {}", parts.join(" · "))
    };
    let busy = st.loading || st.listing_loading;
    kit::page_header(ui, "Containers", &sub, |ui| {
        if IconButton::new(Icon::Rerun, "Refresh")
            .compact()
            .enabled(!st.loading && st.tool != Some(false))
            .show(ui)
            .on_hover_text("Read the contexts, containers, images and compose projects again")
            .clicked()
        {
            actions.push(act(Act::Refresh));
        }
        if busy {
            ui.add(egui::Spinner::new().size(14.0));
        }
        live_badge(ui, st);
    });
}

fn toolbar(ui: &mut Ui, d: &DockerCx, st: &State, actions: &mut Vec<Action>) {
    let Some(Ok(contexts)) = &st.contexts else {
        return;
    };
    let p = *pal(ui);
    let current = st.context_name().unwrap_or_default();
    let info = st.context_info().cloned();
    let all = st.containers();
    let mine: Vec<&Container> = st
        .scoped
        .as_ref()
        .map(|s| {
            s.repo_containers()
                .into_iter()
                .filter_map(|i| all.get(i))
                .collect()
        })
        .unwrap_or_default();
    let running = mine.iter().filter(|c| c.state.running()).count();
    let stopped = mine.iter().filter(|c| !c.state.up()).count();
    let unhealthy = mine.iter().filter(|c| c.unhealthy()).count();
    let restarting = mine
        .iter()
        .filter(|c| c.state == CState::Restarting)
        .count();
    Frame::NONE
        .inner_margin(Margin {
            left: 24,
            right: 16,
            top: 8,
            bottom: 8,
        })
        .show(ui, |ui| {
            let w = ui.available_width();
            let wide = w >= 900.0;
            let row = |ui: &mut Ui, add: &mut dyn FnMut(&mut Ui)| {
                ui.allocate_ui_with_layout(
                    vec2(w, 30.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        add(ui)
                    },
                );
            };
            let mut later: Vec<Action> = Vec::new();
            let mut later2: Vec<Action> = Vec::new();
            let mut context_row = |ui: &mut Ui| {
                ui.label(RichText::new("Context").size(size::SMALL).color(p.text_2));
                let mut ctx = current.clone();
                let opts: Vec<(String, String)> = contexts
                    .iter()
                    .map(|c| {
                        let mut label = c.name.clone();
                        if c.current {
                            label.push_str("  (current)");
                        }
                        if c.remote() {
                            label.push_str("  · remote");
                        }
                        (c.name.clone(), label)
                    })
                    .collect();
                if form::select(ui, "docker_context", &mut ctx, &opts, 200.0) {
                    later.push(act(Act::Context(ctx)));
                }
                if let Some(c) = &info {
                    if c.remote() {
                        kit::badge(ui, "Remote", Tone::Warning).on_hover_text(
                            "This context's engine runs on another machine: every change asks first and says so, and safe mode refuses them.",
                        );
                        if d.safe_mode {
                            kit::badge(ui, "Safe mode: read-only", Tone::Neutral).on_hover_text(
                                "Safe mode is on: changes to containers on this remote Docker are refused (Settings → Safety). Logs, stats and inspect still work.",
                            );
                        }
                    }
                    ui.add(
                        egui::Label::new(kit::mono(&c.endpoint).color(p.text_2))
                            .truncate(),
                    );
                }
            };
            let mut counts_row = |ui: &mut Ui| {
                if st.scoped.is_some() {
                    kit::badge(ui, &format!("{running} running"), if running > 0 { Tone::Success } else { Tone::Neutral });
                    kit::badge(ui, &format!("{stopped} stopped"), Tone::Neutral);
                    if unhealthy > 0 {
                        kit::badge(ui, &format!("{unhealthy} unhealthy"), Tone::Danger);
                    }
                    if restarting > 0 {
                        kit::badge(ui, &format!("{restarting} restarting"), Tone::Warning);
                    }
                }
                ui.add_space(6.0);
                let mut on = st.show_all;
                if form::checkbox(ui, &mut on, "Show all containers on this Docker").changed() {
                    later2.push(act(Act::ShowAll(on)));
                }
            };
            if wide {
                row(ui, &mut |ui| {
                    context_row(ui);
                    ui.add_space(10.0);
                    counts_row(ui);
                });
            } else {
                row(ui, &mut |ui| context_row(ui));
                row(ui, &mut |ui| counts_row(ui));
            }
            actions.extend(later);
            actions.extend(later2);
        });
    kit::hairline(ui);
}

fn problem_page(
    ui: &mut Ui,
    st: &State,
    problem: Problem,
    e: &str,
    ctx: &str,
    ep: &str,
    actions: &mut Vec<Action>,
) {
    let fix = triage::fix(problem, ctx, ep);
    common::error_block(
        ui,
        &format!("{}: {ctx}", problem.title()),
        e,
        Some((&fix.text, fix.command.as_deref())),
        Some(("Try again", super::Act::Docker(Act::Refresh))),
        actions,
    );
    kit::kv(ui, 130.0, |kv| {
        kv.text("Context", ctx);
        if ep.is_empty() {
            kv.muted("Endpoint", "unknown");
        } else {
            kv.text("Endpoint", kit::mono(ep));
        }
        kv.text(
            "Where",
            if endpoint::is_remote(ep) {
                "another machine (remote)"
            } else {
                "this machine"
            },
        );
        if let Some(c) = st.listing.as_ref().and_then(|l| l.client.clone()) {
            kv.text("docker CLI", c);
        }
        kv.text(
            "Ran",
            kit::mono(commands::display(&commands::version_args(ctx))),
        );
    });
    common::note(
        ui,
        "Nothing else was asked while the engine does not answer, and no change is ever sent to it.\nPick another context above, or fix this one and press Try again.",
    );
    if !st.files.compose.is_empty() || !st.files.dockerfiles.is_empty() {
        kit::section(ui, "In the repository", None);
        for f in &st.files.compose {
            ui.label(kit::mono(&f.rel).color(pal(ui).text));
        }
        for f in &st.files.dockerfiles {
            ui.label(kit::mono(f).color(pal(ui).text));
        }
    }
}

// ---------------------------------------------------------------------------
// The list
// ---------------------------------------------------------------------------

fn list(ui: &mut Ui, cx: &Cx, st: &mut State, actions: &mut Vec<Action>) {
    let total = st.containers().len();
    let selected = st.selected.is_some();
    kit::list_header(ui, "Containers", Some(total), |ui| {
        if selected {
            ui.add_space(4.0);
            if kit::link(ui, "Overview").clicked() {
                actions.push(act(Act::Select(None)));
            }
        }
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        ui.set_width(ui.available_width() - 4.0);
        kit::search_box(
            ui,
            "docker_filter",
            &mut st.filter,
            "Filter by name, service or image",
        );
    });
    ui.add_space(6.0);
    let rows = docker::rows(st);
    let p = *pal(ui);
    let all = st.containers();
    egui::ScrollArea::vertical()
        .id_salt("docker_rows")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let w = ui.available_width();
            for row in &rows {
                match row {
                    Row::Group(title, n) => {
                        let (rect, _) = ui.allocate_exact_size(vec2(w, 32.0), Sense::hover());
                        let g = ui.painter().layout_no_wrap(
                            title.to_uppercase(),
                            egui::FontId::new(size::CAPTION, crate::theme::medium()),
                            p.text_3,
                        );
                        let y = rect.bottom() - 9.0 - g.size().y / 2.0;
                        let gw = g.size().x;
                        ui.painter().galley(
                            egui::pos2(rect.left() + 12.0, y - g.size().y / 2.0),
                            g,
                            p.text_3,
                        );
                        ui.painter().text(
                            egui::pos2(rect.left() + 20.0 + gw, y),
                            egui::Align2::LEFT_CENTER,
                            format::count(*n),
                            egui::FontId::proportional(size::CAPTION),
                            p.text_3,
                        );
                    }
                    Row::Empty => {
                        let (rect, _) = ui.allocate_exact_size(vec2(w, 46.0), Sense::hover());
                        ui.painter().text(
                            egui::pos2(rect.left() + 12.0, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            if st.filter.trim().is_empty() {
                                "Nothing of this repository on this Docker."
                            } else {
                                "Nothing matches the filter."
                            },
                            egui::FontId::proportional(size::SMALL),
                            p.text_3,
                        );
                    }
                    Row::Hidden(n) => {
                        let (rect, resp) = ui.allocate_exact_size(vec2(w, 40.0), Sense::click());
                        kit::row_background(ui, rect, resp.hovered(), false);
                        ui.painter().text(
                            egui::pos2(rect.left() + 12.0, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            format!(
                                "{} of other projects on this Docker · show them",
                                format::plural(*n, "container", "containers")
                            ),
                            egui::FontId::proportional(size::SMALL),
                            p.text_2,
                        );
                        if resp
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                        {
                            actions.push(act(Act::ShowAll(true)));
                        }
                    }
                    Row::Project { name, in_repo: _ } => {
                        let (rect, resp) =
                            ui.allocate_exact_size(vec2(w, kit::ROW_H), Sense::click());
                        if !ui.is_rect_visible(rect) {
                            continue;
                        }
                        let sel = st.selected.as_ref() == Some(&Sel::Project(name.clone()));
                        kit::row_background(ui, rect, resp.hovered(), sel);
                        if let Some(pr) = st.scoped.as_ref().and_then(|s| s.project(name)) {
                            paint_project(ui, rect, st, pr, all, sel);
                        }
                        if resp
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                        {
                            actions.push(act(Act::Select(Some(Sel::Project(name.clone())))));
                        }
                    }
                    Row::Container { index, nested } => {
                        let Some(c) = all.get(*index) else { continue };
                        let (full, resp) =
                            ui.allocate_exact_size(vec2(w, kit::ROW_H), Sense::click());
                        if !ui.is_rect_visible(full) {
                            continue;
                        }
                        let rect = if *nested {
                            egui::Rect::from_min_max(
                                egui::pos2(full.left() + 16.0, full.top()),
                                full.max,
                            )
                        } else {
                            full
                        };
                        let sel = st.selected.as_ref() == Some(&Sel::Container(c.id.clone()));
                        kit::row_background(ui, rect, resp.hovered(), sel);
                        let title = if *nested {
                            service_title(c)
                        } else {
                            c.name.clone()
                        };
                        let mut sub = status_text(c);
                        if !*nested {
                            sub = format!("{} · {sub}", short_image(&c.image));
                        }
                        if let Some(port) = first_port(c) {
                            sub.push_str(&format!(" · {port}"));
                        }
                        let badges = layout::fit_badges(ui, rect.width(), &container_badges(c));
                        let meta = common::age(c.created, cx.now);
                        kit::paint_row(
                            ui,
                            rect,
                            &kit::Row::new(&title)
                                .leading(Leading::Dot(dot(&p, state_tone(c))))
                                .subtitle(&sub)
                                .meta(&meta)
                                .badges(&badges)
                                .dimmed(!c.state.up() && !c.failed()),
                            sel,
                        );
                        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                        if resp.clicked() {
                            actions.push(act(Act::Select(Some(Sel::Container(c.id.clone())))));
                        }
                        resp.context_menu(|ui| {
                            if ui.button("Copy container ID").clicked() {
                                actions.push(Action::Copy(c.id.clone()));
                                ui.close();
                            }
                            if ui.button("Copy name").clicked() {
                                actions.push(Action::Copy(c.name.clone()));
                                ui.close();
                            }
                        });
                    }
                    Row::File(rel) => {
                        let (rect, resp) =
                            ui.allocate_exact_size(vec2(w, kit::ROW_H), Sense::click());
                        if !ui.is_rect_visible(rect) {
                            continue;
                        }
                        let sel = st.selected.as_ref() == Some(&Sel::File(rel.clone()));
                        kit::row_background(ui, rect, resp.hovered(), sel);
                        let services = st
                            .file_target(&cx.workdir, rel)
                            .and_then(|t| st.config(&t))
                            .and_then(|c| c.result.as_ref())
                            .and_then(|r| r.as_ref().ok())
                            .map(|c| c.services.len());
                        let sub = match services {
                            Some(n) => format!(
                                "Compose file · {}",
                                format::plural(n, "service", "services")
                            ),
                            None => "Compose file · Up starts it".to_owned(),
                        };
                        let badges = vec![("Not started".to_owned(), Tone::Neutral)];
                        kit::paint_row(
                            ui,
                            rect,
                            &kit::Row::new(rel)
                                .leading(Leading::Icon(Icon::Container, p.text_3))
                                .subtitle(&sub)
                                .badges(&layout::fit_badges(ui, rect.width(), &badges)),
                            sel,
                        );
                        if resp
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                        {
                            actions.push(act(Act::Select(Some(Sel::File(rel.clone())))));
                        }
                    }
                }
            }
            ui.add_space(8.0);
        });
}

fn project_tone(pr: &Project, all: &[Container]) -> Tone {
    let kids: Vec<&Container> = pr.containers.iter().filter_map(|i| all.get(*i)).collect();
    let services: Vec<&&Container> = kids
        .iter()
        .filter(|c| !c.compose.as_ref().is_some_and(|i| i.oneoff))
        .collect();
    if kids.iter().any(|c| c.troubled()) {
        Tone::Danger
    } else if !services.is_empty() && services.iter().all(|c| c.state.running()) {
        Tone::Success
    } else if kids.iter().any(|c| c.state.up()) {
        Tone::Warning
    } else {
        Tone::Neutral
    }
}

fn paint_project(
    ui: &Ui,
    rect: egui::Rect,
    st: &State,
    pr: &Project,
    all: &[Container],
    sel: bool,
) {
    let p = *pal(ui);
    let running = pr.running(all);
    let total = pr.containers.len();
    let file = pr
        .file
        .and_then(|i| st.files.compose.get(i))
        .map(|f| f.rel.clone())
        .or_else(|| {
            pr.config_files
                .first()
                .map(|f| f.rsplit(['/', '\\']).next().unwrap_or(f).to_owned())
        })
        .unwrap_or_else(|| "compose".into());
    let sub = format!("{file} · running {running} of {total}");
    let mut badges = Vec::new();
    let unhealthy = pr
        .containers
        .iter()
        .filter(|i| all.get(**i).is_some_and(Container::unhealthy))
        .count();
    if unhealthy > 0 {
        badges.push((format!("{unhealthy} unhealthy"), Tone::Danger));
    }
    let failed = pr
        .containers
        .iter()
        .filter(|i| {
            all.get(**i)
                .is_some_and(|c| c.failed() || c.state == CState::Restarting)
        })
        .count();
    if failed > 0 {
        badges.push((format!("{failed} failing"), Tone::Warning));
    }
    let tone = project_tone(pr, all);
    kit::paint_row(
        ui,
        rect,
        &kit::Row::new(&pr.name)
            .leading(Leading::Icon(Icon::Container, dot(&p, tone)))
            .subtitle(&sub)
            .badges(&layout::fit_badges(ui, rect.width(), &badges)),
        sel,
    );
}

// ---------------------------------------------------------------------------
// Overview
// ---------------------------------------------------------------------------

fn last_event_line(ui: &mut Ui, st: &State) {
    let now = Instant::now();
    let text = match (&st.live.mode, st.live.last_event()) {
        (LiveMode::Live, Some((e, at))) => format!(
            "Live: the last change, {} ago, was {}.",
            format::millis(now.duration_since(at).as_millis() as u64),
            e.text()
        ),
        (LiveMode::Live, None) => "Live: no container has changed since the tab opened.".into(),
        (LiveMode::Polling(why), _) => format!("Polling every 10 s: {why}."),
        _ => String::new(),
    };
    if !text.is_empty() {
        kit::secondary(ui, &text);
    }
}

#[allow(clippy::too_many_arguments)]
fn overview(
    ui: &mut Ui,
    cx: &Cx,
    d: &DockerCx,
    st: &State,
    refs: &mut RefCache,
    pkg: &PkgState,
    actions: &mut Vec<Action>,
) {
    let Some(s) = &st.scoped else {
        kit::loading(ui, "Sorting the containers by project…");
        return;
    };
    let ctx = st.context_name().unwrap_or_default();
    let all = st.containers();
    let mine: Vec<usize> = s.repo_containers();
    ui.horizontal_wrapped(|ui| {
        kit::title(ui, &format!("{} on {ctx}", d.repo_name));
        if st.remote() {
            kit::badge(ui, "Remote", Tone::Warning);
        }
    });
    last_event_line(ui, st);
    let cs: Vec<&Container> = mine.iter().filter_map(|i| all.get(*i)).collect();
    let running = cs.iter().filter(|c| c.state.running()).count();
    let stopped = cs.iter().filter(|c| !c.state.up()).count();
    let unhealthy = cs.iter().filter(|c| c.unhealthy()).count();
    let restarting = cs.iter().filter(|c| c.state == CState::Restarting).count();
    let failed = cs.iter().filter(|c| c.failed()).count();
    common::gap(ui);
    crate::views::insights::stats(
        ui,
        &[
            Stat::new("running", format::count(running))
                .tone_if(running > 0 && unhealthy == 0, Tone::Success),
            Stat::new("stopped", format::count(stopped)),
            Stat::new("unhealthy", format::count(unhealthy)).tone_if(unhealthy > 0, Tone::Danger),
            Stat::new("restarting", format::count(restarting))
                .tone_if(restarting > 0, Tone::Warning),
            Stat::new("exited with an error", format::count(failed))
                .tone_if(failed > 0, Tone::Danger),
            Stat::new("compose projects", format::count(s.projects.len())),
            Stat::new("not started", format::count(s.not_started.len())),
        ],
    );
    if let Some(e) = st.compose_problem() {
        let p = triage::classify(&e, st.remote());
        let fix = triage::fix(p, &ctx, &st.endpoint());
        kit::callout_with(ui, Tone::Warning, p.title(), &fix.text, |ui| {
            if let Some(c) = &fix.command {
                kit::code_block(ui, c, actions);
            }
            kit::code_block(ui, &e, actions);
        });
    }
    if mine.is_empty() && s.not_started.is_empty() {
        kit::callout(
            ui,
            Tone::Neutral,
            "Nothing of this repository runs on this Docker",
            "A compose project counts when its working directory or a compose file is inside this repository; a single container when its image's org.opencontainers.image.source names this repository's remote, or its revision label names one of its commits.",
        );
    }
    let troubled = st.troubled();
    if !troubled.is_empty() {
        kit::section(ui, "Needs attention", Some(troubled.len()));
        let rows: Vec<Vec<Cell>> = troubled
            .iter()
            .filter_map(|i| all.get(*i))
            .map(|c| {
                vec![
                    Cell::Text(c.name.clone()),
                    Cell::Tone(problem_text(c), Tone::Danger),
                    Cell::Sec(problem_detail(c)),
                    Cell::Muted(relative(c.finished.or(c.started), cx.now)),
                ]
            })
            .collect();
        let ids: Vec<String> = troubled
            .iter()
            .filter_map(|i| all.get(*i))
            .map(|c| c.id.clone())
            .collect();
        layout::wide(ui, "docker_trouble", 620.0, |ui| {
            grid(
                ui,
                "docker_trouble",
                &[
                    Col::text("Container"),
                    Col::text("Problem"),
                    Col::text("Detail"),
                    Col::num("Since"),
                ],
                &rows,
                true,
                |r, resp| {
                    if resp.clicked() {
                        actions.push(act(Act::Select(Some(Sel::Container(ids[r].clone())))));
                    }
                },
            );
        });
    }
    if !cs.is_empty() {
        kit::section(ui, "Containers of this repository", Some(cs.len()));
        container_table(ui, cx, d, st, refs, pkg, &mine, "docker_all_rows", actions);
    }
    if !s.not_started.is_empty() {
        kit::section(ui, "Compose files not started", Some(s.not_started.len()));
        for fi in &s.not_started {
            let Some(f) = st.files.compose.get(*fi) else {
                continue;
            };
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if kit::link(ui, &f.rel).clicked() {
                    actions.push(act(Act::Select(Some(Sel::File(f.rel.clone())))));
                }
                if let Some(t) = st.file_target(&cx.workdir, &f.rel)
                    && IconButton::new(Icon::Play, "Up")
                        .compact()
                        .enabled(st.compose_problem().is_none())
                        .show(ui)
                        .on_hover_text(commands::display(
                            &ComposeOp::Up {
                                build: false,
                                service: None,
                            }
                            .args(&ctx, &t, true),
                        ))
                        .clicked()
                {
                    actions.push(act(Act::Compose {
                        target: t,
                        label: f.rel.clone(),
                        op: ComposeOp::Up {
                            build: false,
                            service: None,
                        },
                    }));
                }
            });
        }
    }
    if !st.files.dockerfiles.is_empty() {
        kit::section(ui, "Dockerfiles", Some(st.files.dockerfiles.len()));
        kit::secondary(
            ui,
            "Build an image with OCI labels naming this commit and remote, so this tab (and GHCR, and the Kubernetes tab) can tell which commit runs.",
        );
        for df in st.files.dockerfiles.iter().take(20) {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.label(kit::mono(df).color(pal(ui).text));
                if IconButton::new(Icon::Hammer, "Build with labels…")
                    .compact()
                    .show(ui)
                    .clicked()
                {
                    actions.push(act(Act::BuildForm(BuildSource::Dockerfile(df.clone()))));
                }
            });
        }
    }
    kit::section(ui, "How this tab works", None);
    explain::details(
        ui,
        "docker_how",
        "Scope, live updates and the commands",
        false,
        |ui| {
            explain::terms(
                ui,
                150.0,
                &[
                    (
                        "This repository",
                        "Compose projects whose working directory or a compose file is inside this repository, containers whose image names this repository (org.opencontainers.image.source) or one of its commits (org.opencontainers.image.revision), and its compose files no project was started from.",
                    ),
                    (
                        "Live",
                        "While this tab is shown, docker events streams container changes; the list is read again half a second after they settle. The stream stops when the tab is hidden, and falls back to polling every 10 s if docker ends it.",
                    ),
                    (
                        "Context",
                        "Every command names the context (docker --context NAME …); your current context and docker configuration are never changed.",
                    ),
                    (
                        "Changes",
                        "Start, stop, restart, pause, compose up and builds run in the console. Kill, remove and compose down ask first and show the exact command; on a remote context every change asks first.",
                    ),
                    (
                        "Secrets",
                        "Environment variables are shown by name only: their values are dropped where docker's answer is read and never reach the screen or the activity log.",
                    ),
                ],
            );
        },
    );
}

fn problem_text(c: &Container) -> String {
    if c.unhealthy() {
        "Unhealthy".into()
    } else if c.state == CState::Restarting {
        format!("Restarting ({}×)", c.restart_count)
    } else if c.oom_killed {
        "Out of memory".into()
    } else {
        c.state.label()
    }
}

fn problem_detail(c: &Container) -> String {
    if let Some(h) = c.health.as_ref().filter(|_| c.unhealthy()) {
        let last = h.log.last();
        return match last {
            Some(p) => format!(
                "{} probes failed in a row; the last exited {}: {}",
                h.failing_streak,
                p.exit_code,
                p.output.lines().next().unwrap_or("")
            ),
            None => format!("{} probes failed in a row", h.failing_streak),
        };
    }
    if !c.error.is_empty() {
        return c.error.clone();
    }
    if c.oom_killed {
        return "The kernel killed it for using too much memory".into();
    }
    match (c.state.clone(), c.started, c.finished) {
        (CState::Restarting, ..) => format!("restart policy {}", c.restart_policy),
        (CState::Exited(_), Some(a), Some(b)) => format!(
            "ran for {}; its logs keep the last output",
            format::millis(((b - a).max(0) as u64) * 1000)
        ),
        _ => status_text(c),
    }
}

#[allow(clippy::too_many_arguments)]
fn container_table(
    ui: &mut Ui,
    cx: &Cx,
    d: &DockerCx,
    st: &State,
    refs: &RefCache,
    pkg: &PkgState,
    idx: &[usize],
    id: &str,
    actions: &mut Vec<Action>,
) {
    let all = st.containers();
    let listing = st.listing.as_ref();
    let cs: Vec<&Container> = idx.iter().filter_map(|i| all.get(*i)).collect();
    let rows: Vec<Vec<Cell>> = cs
        .iter()
        .map(|c| {
            let image = listing.and_then(|l| l.image(&c.image_id));
            let r = running_commit(c, image, refs, pkg, d);
            vec![
                Cell::Text(c.name.clone()),
                Cell::Tone(c.state.label(), run_tone(c)),
                match health_badge(c.health_status()) {
                    Some((t, tone)) => Cell::Badges(vec![(t, tone)]),
                    None => Cell::Muted("–".into()),
                },
                Cell::Mono(first_port(c).unwrap_or_else(|| "–".into())),
                commit_cell(&r),
                Cell::Sec(short_image(&c.image)),
                Cell::Muted(common::age(c.created, cx.now)),
            ]
        })
        .collect();
    let ids: Vec<String> = cs.iter().map(|c| c.id.clone()).collect();
    layout::wide(ui, id, 760.0, |ui| {
        grid(
            ui,
            id,
            &[
                Col::text("Container"),
                Col::text("State"),
                Col::text("Health"),
                Col::text("Port"),
                Col::text("Commit"),
                Col::text("Image"),
                Col::num("Age"),
            ],
            &rows,
            true,
            |r, resp| {
                if resp.clicked() {
                    actions.push(act(Act::Select(Some(Sel::Container(ids[r].clone())))));
                }
            },
        );
    });
}

// ---------------------------------------------------------------------------
// Compose projects and files
// ---------------------------------------------------------------------------

/// Buttons for a project or a file, each running (or confirming) its
/// compose command; `started` says whether containers exist.
fn compose_buttons(
    ui: &mut Ui,
    st: &State,
    target: &ComposeTarget,
    label: &str,
    started: bool,
    actions: &mut Vec<Action>,
) {
    let ctx = st.context_name().unwrap_or_default();
    let missing = !target.missing_files().is_empty();
    let compose_ok = st.compose_problem().is_none();
    let mut push = |ui: &mut Ui, icon: Icon, text: &str, op: ComposeOp, enabled: bool| {
        let line = commands::display(&op.args(&ctx, target, !missing));
        if IconButton::new(icon, text)
            .compact()
            .enabled(enabled && compose_ok)
            .show(ui)
            .on_hover_text(line)
            .clicked()
        {
            actions.push(act(Act::Compose {
                target: target.clone(),
                label: label.to_owned(),
                op,
            }));
        }
    };
    common::buttons(ui, |ui| {
        push(
            ui,
            Icon::Play,
            "Up",
            ComposeOp::Up {
                build: false,
                service: None,
            },
            !missing,
        );
        push(
            ui,
            Icon::Hammer,
            "Up with build",
            ComposeOp::Up {
                build: true,
                service: None,
            },
            !missing,
        );
        if started {
            push(ui, Icon::Play, "Start", ComposeOp::Start(None), true);
            push(ui, Icon::Stop, "Stop", ComposeOp::Stop(None), true);
            push(ui, Icon::Rerun, "Restart", ComposeOp::Restart(None), true);
        }
        push(ui, Icon::Download, "Pull", ComposeOp::Pull(None), !missing);
        push(ui, Icon::Hammer, "Build", ComposeOp::Build(None), !missing);
        if started {
            push(ui, Icon::Logs, "Logs", ComposeOp::Logs, true);
        }
    });
    if started {
        let volumes = st.down_volumes;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let op = ComposeOp::Down {
                volumes,
                remove_orphans: false,
            };
            if form::danger(
                ui,
                if volumes {
                    "Down and delete volumes…"
                } else {
                    "Down…"
                },
                compose_ok,
            )
            .on_hover_text(commands::display(&op.args(&ctx, target, !missing)))
            .clicked()
            {
                actions.push(act(Act::Compose {
                    target: target.clone(),
                    label: label.to_owned(),
                    op,
                }));
            }
            let mut v = volumes;
            if form::checkbox(ui, &mut v, "Also remove volumes (deletes their data)").changed() {
                actions.push(act(Act::DownVolumes(v)));
            }
        });
    }
}

/// Services of a project: from its compose config (all of them, also those
/// not running) merged with the containers that exist.
#[allow(clippy::too_many_arguments)]
fn services_table(
    ui: &mut Ui,
    d: &DockerCx,
    st: &State,
    refs: &RefCache,
    pkg: &PkgState,
    config: Option<&ComposeConfig>,
    containers: &[usize],
    project: &str,
    actions: &mut Vec<Action>,
) {
    let all = st.containers();
    let listing = st.listing.as_ref();
    let mut names: Vec<String> = config
        .map(|c| c.services.iter().map(|s| s.name.clone()).collect())
        .unwrap_or_default();
    for i in containers {
        if let Some(svc) = all
            .get(*i)
            .and_then(|c| c.compose.as_ref())
            .map(|i| i.service.clone())
            && !names.contains(&svc)
        {
            names.push(svc);
        }
    }
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    let mut picks: Vec<Option<String>> = Vec::new();
    for name in &names {
        let svc: Option<&Service> = config.and_then(|c| c.service(name));
        let kids: Vec<&Container> = containers
            .iter()
            .filter_map(|i| all.get(*i))
            .filter(|c| c.compose.as_ref().is_some_and(|i| &i.service == name))
            .collect();
        let first = kids.first().copied();
        let image = match (svc, first) {
            (Some(s), _) if s.build.is_some() => match &s.image {
                Some(i) => format!("build → {i}"),
                None => format!("build → {}", compose::built_image_name(project, name)),
            },
            (Some(s), _) if s.image.is_some() => s.image.clone().unwrap_or_default(),
            (_, Some(c)) => c.image.clone(),
            _ => "–".into(),
        };
        let state = match first {
            Some(c) if kids.len() > 1 => Cell::Tone(
                format!("{} · {} replicas", c.state.label(), kids.len()),
                run_tone(c),
            ),
            Some(c) => Cell::Tone(c.state.label(), run_tone(c)),
            None if svc.is_some_and(|s| !s.profiles.is_empty()) => Cell::Muted(format!(
                "profile {} only",
                svc.map(|s| s.profiles.join(", ")).unwrap_or_default()
            )),
            None => Cell::Muted("not created".into()),
        };
        let health = match first.map(Container::health_status) {
            Some(h) if h != HealthStatus::None => match health_badge(h) {
                Some((t, tone)) => Cell::Badges(vec![(t, tone)]),
                None => Cell::Muted("–".into()),
            },
            _ if svc.is_some_and(|s| s.healthcheck.is_some()) => Cell::Muted("has a check".into()),
            _ => Cell::Muted("–".into()),
        };
        let ports = match (first, svc) {
            (Some(c), _) if c.ports.iter().any(|p| p.published()) => {
                Cell::Mono(first_port(c).unwrap_or_default())
            }
            (_, Some(s)) if !s.ports.is_empty() => Cell::Mono(
                s.ports
                    .iter()
                    .map(|p| p.text())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            _ => Cell::Muted("–".into()),
        };
        let commit = match first {
            Some(c) => commit_cell(&running_commit(
                c,
                listing.and_then(|l| l.image(&c.image_id)),
                refs,
                pkg,
                d,
            )),
            None => Cell::Muted("–".into()),
        };
        rows.push(vec![
            Cell::Text(name.clone()),
            Cell::Sec(image),
            state,
            health,
            ports,
            commit,
        ]);
        picks.push(first.map(|c| c.id.clone()));
    }
    if rows.is_empty() {
        common::note(ui, "No services known yet.");
        return;
    }
    layout::wide(ui, ("docker_services", project), 720.0, |ui| {
        grid(
            ui,
            &format!("docker_services_{project}"),
            &[
                Col::text("Service"),
                Col::text("Image or build"),
                Col::text("State"),
                Col::text("Health"),
                Col::text("Ports"),
                Col::text("Commit"),
            ],
            &rows,
            true,
            |r, resp| {
                if resp.clicked()
                    && let Some(Some(id)) = picks.get(r)
                {
                    actions.push(act(Act::Select(Some(Sel::Container(id.clone())))));
                }
            },
        );
    });
}

fn build_buttons(ui: &mut Ui, config: &ComposeConfig, project: &str, actions: &mut Vec<Action>) {
    let buildable: Vec<&Service> = config
        .services
        .iter()
        .filter(|s| s.build.is_some())
        .collect();
    if buildable.is_empty() {
        return;
    }
    common::buttons(ui, |ui| {
        for s in buildable {
            let image = s
                .image
                .clone()
                .unwrap_or_else(|| compose::built_image_name(project, &s.name));
            if IconButton::new(Icon::Hammer, &format!("Build {} with labels…", s.name))
                .compact()
                .show(ui)
                .on_hover_text(format!(
                    "docker build with OCI labels naming this commit, tagged {image} so the next Up runs it"
                ))
                .clicked()
            {
                actions.push(act(Act::BuildForm(BuildSource::Service {
                    project: project.to_owned(),
                    service: s.name.clone(),
                    image,
                })));
            }
        }
    });
}

fn config_notes(ui: &mut Ui, config: &ComposeConfig) {
    let mut notes: Vec<String> = Vec::new();
    for s in &config.services {
        if !s.profiles.is_empty() {
            notes.push(format!(
                "{} starts only with --profile {}.",
                s.name,
                s.profiles.join(" / ")
            ));
        }
        for (dep, cond) in &s.depends_on {
            if cond == "service_healthy" {
                notes.push(format!("{} waits until {dep} is healthy.", s.name));
            }
        }
    }
    for n in notes.iter().take(8) {
        common::note(ui, n);
    }
}

fn config_section(ui: &mut Ui, st: &State, target: &ComposeTarget) -> Option<ComposeConfig> {
    match st.config(target).map(|c| (&c.result, c.loading)) {
        Some((Some(Ok(c)), _)) => Some(c.clone()),
        Some((Some(Err(e)), _)) => {
            kit::callout(ui, Tone::Warning, "docker compose config failed", e);
            None
        }
        Some((None, true)) => {
            kit::loading(ui, "Reading the compose configuration…");
            None
        }
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn project_detail(
    ui: &mut Ui,
    cx: &Cx,
    d: &DockerCx,
    st: &State,
    refs: &mut RefCache,
    pkg: &PkgState,
    name: &str,
    actions: &mut Vec<Action>,
) {
    let Some(pr) = st.scoped.as_ref().and_then(|s| s.project(name)).cloned() else {
        return;
    };
    let Some(target) = st.project_target(name) else {
        return;
    };
    let all = st.containers();
    let running = pr.running(all);
    let mut badges = vec![(
        format!("running {running} of {}", pr.containers.len()),
        project_tone(&pr, all),
    )];
    if !pr.in_repo {
        badges.push(("Another project".into(), Tone::Neutral));
    }
    if st.remote() {
        badges.push(("Remote".into(), Tone::Warning));
    }
    detail_title(ui, name, &badges);
    if !pr.status.is_empty() {
        kit::secondary(ui, &format!("docker compose ls: {}", pr.status));
    }
    kit::kv(ui, 130.0, |kv| {
        kv.row("Compose files", |ui| {
            for f in &pr.config_files {
                let exists = std::path::Path::new(f).is_file();
                ui.horizontal_wrapped(|ui| {
                    ui.label(kit::mono(f).color(if exists {
                        pal(ui).text
                    } else {
                        pal(ui).text_3
                    }));
                    if !exists {
                        kit::badge(ui, "gone", Tone::Warning);
                    }
                });
            }
        });
        if !pr.working_dir.is_empty() {
            kv.text("Working dir", kit::mono(&pr.working_dir));
        }
        if let Some(f) = pr.file.and_then(|i| st.files.compose.get(i)) {
            kv.text("In the repository", kit::mono(&f.rel));
        }
        kv.text("Context", st.context_name().unwrap_or_default());
    });
    if !target.missing_files().is_empty() {
        kit::callout(
            ui,
            Tone::Warning,
            "A compose file of this project is gone",
            "Start, stop, restart, down and logs still work: they address the project by name and compose finds its containers by label. Up, pull and build need the files.",
        );
    }
    compose_buttons(ui, st, &target, name, true, actions);
    kit::section(ui, "Services", None);
    let config = config_section(ui, st, &target);
    services_table(
        ui,
        d,
        st,
        refs,
        pkg,
        config.as_ref(),
        &pr.containers,
        name,
        actions,
    );
    if let Some(c) = &config {
        config_notes(ui, c);
        build_buttons(ui, c, name, actions);
        if !c.volumes.is_empty() {
            common::note(ui, &format!("Named volumes: {}.", c.volumes.join(", ")));
        }
    }
    kit::section(ui, "Containers", Some(pr.containers.len()));
    container_table(
        ui,
        cx,
        d,
        st,
        refs,
        pkg,
        &pr.containers,
        &format!("docker_pc_{name}"),
        actions,
    );
    commands_fold(ui, st, &target, name, actions);
}

fn commands_fold(
    ui: &mut Ui,
    st: &State,
    target: &ComposeTarget,
    label: &str,
    actions: &mut Vec<Action>,
) {
    let ctx = st.context_name().unwrap_or_default();
    let with_files = target.missing_files().is_empty();
    explain::details(
        ui,
        ("docker_cmds", label),
        "The exact commands",
        false,
        |ui| {
            let ops = [
                ComposeOp::Up {
                    build: false,
                    service: None,
                },
                ComposeOp::Up {
                    build: true,
                    service: None,
                },
                ComposeOp::Start(None),
                ComposeOp::Stop(None),
                ComposeOp::Restart(None),
                ComposeOp::Pull(None),
                ComposeOp::Build(None),
                ComposeOp::Logs,
                ComposeOp::Down {
                    volumes: false,
                    remove_orphans: false,
                },
                ComposeOp::Down {
                    volumes: true,
                    remove_orphans: false,
                },
            ];
            let text = ops
                .iter()
                .map(|op| commands::display(&op.args(&ctx, target, with_files)))
                .collect::<Vec<_>>()
                .join("\n");
            kit::code_block(ui, &text, actions);
            explain::quiet(
                ui,
                &format!("in {}", target.cwd(std::path::Path::new(".")).display()),
            );
        },
    );
}

fn file_detail(ui: &mut Ui, cx: &Cx, st: &State, rel: &str, actions: &mut Vec<Action>) {
    let Some(target) = st.file_target(&cx.workdir, rel) else {
        return;
    };
    let f = st.files.compose.iter().find(|f| f.rel == rel);
    detail_title(ui, rel, &[("Not started".into(), Tone::Neutral)]);
    kit::secondary(
        ui,
        "A compose file of this repository that no project on this Docker was started from.",
    );
    kit::kv(ui, 130.0, |kv| {
        kv.text("File", kit::mono(rel));
        if let Some(f) = f
            && !f.overrides.is_empty()
        {
            kv.text("Merged with", kit::mono(f.overrides.join(", ")));
        }
        kv.text("Context", st.context_name().unwrap_or_default());
    });
    compose_buttons(ui, st, &target, rel, false, actions);
    kit::section(ui, "Services", None);
    if let Some(c) = config_section(ui, st, &target) {
        if !c.name.is_empty() {
            common::note(ui, &format!("Up creates the project {}.", c.name));
        }
        let rows: Vec<Vec<Cell>> = c
            .services
            .iter()
            .map(|s| {
                vec![
                    Cell::Text(s.name.clone()),
                    Cell::Sec(match (&s.build, &s.image) {
                        (Some(_), Some(i)) => format!("build → {i}"),
                        (Some(_), None) => {
                            format!("build → {}", compose::built_image_name(&c.name, &s.name))
                        }
                        (None, Some(i)) => i.clone(),
                        (None, None) => "–".into(),
                    }),
                    Cell::Mono(if s.ports.is_empty() {
                        "–".into()
                    } else {
                        s.ports
                            .iter()
                            .map(|p| p.text())
                            .collect::<Vec<_>>()
                            .join(", ")
                    }),
                    Cell::Sec(s.healthcheck.clone().unwrap_or_else(|| "–".into())),
                    Cell::Sec(if s.profiles.is_empty() {
                        "–".into()
                    } else {
                        s.profiles.join(", ")
                    }),
                    Cell::Sec(format::plural(s.env_names.len(), "variable", "variables")),
                ]
            })
            .collect();
        layout::wide(ui, ("docker_file_services", rel), 720.0, |ui| {
            grid(
                ui,
                &format!("docker_file_services_{rel}"),
                &[
                    Col::text("Service"),
                    Col::text("Image or build"),
                    Col::text("Ports"),
                    Col::text("Health check"),
                    Col::text("Profiles"),
                    Col::num("Environment"),
                ],
                &rows,
                false,
                |_, _| {},
            );
        });
        config_notes(ui, &c);
        build_buttons(
            ui,
            &c,
            if c.name.is_empty() {
                "project"
            } else {
                &c.name
            },
            actions,
        );
    }
    commands_fold(ui, st, &target, rel, actions);
}

// ---------------------------------------------------------------------------
// A container
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn container_detail(
    ui: &mut Ui,
    cx: &Cx,
    d: &DockerCx,
    st: &State,
    refs: &mut RefCache,
    pkg: &PkgState,
    c: &Container,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let ctx = st.context_name().unwrap_or_default();
    let image = st
        .listing
        .as_ref()
        .and_then(|l| l.image(&c.image_id))
        .cloned();
    let mut badges = vec![(c.state.label(), run_tone(c))];
    if let Some(h) = health_badge(c.health_status()) {
        badges.push(h);
    }
    if let Some(i) = &c.compose {
        badges.push((format!("{} · {}", i.project, i.service), Tone::Neutral));
    }
    if st.remote() {
        badges.push(("Remote".into(), Tone::Warning));
    }
    detail_title(ui, &c.name, &badges);
    kit::secondary(
        ui,
        &format!(
            "{} · created {} · {}",
            status_text(c),
            relative(c.created, cx.now),
            c.short_id()
        ),
    );
    // What is wrong, plainly.
    if c.oom_killed {
        kit::callout(
            ui,
            Tone::Danger,
            "Killed for using too much memory",
            "The kernel's out-of-memory killer ended the process (exit 137). Raise the memory limit (mem_limit / deploy.resources) or find what grows.",
        );
    }
    if !c.error.is_empty() {
        kit::callout(ui, Tone::Danger, "docker could not run it", &c.error);
    }
    match c.state {
        CState::Restarting => kit::callout(
            ui,
            Tone::Warning,
            &format!("Restarting: {} restarts so far", c.restart_count),
            &format!(
                "The process keeps ending and the restart policy ({}) starts it again. Its logs show why.",
                c.restart_policy
            ),
        ),
        CState::Exited(code) if code != 0 && c.started.is_some() => kit::callout(
            ui,
            Tone::Danger,
            &format!("Exited with code {code}"),
            &format!(
                "It ran from {} and ended {}. Its logs keep the last output.",
                relative(c.started, cx.now),
                relative(c.finished, cx.now)
            ),
        ),
        _ => {}
    }
    container_actions(ui, st, c, &ctx, actions);
    if let Some(h) = &c.health {
        kit::section(ui, "Health", None);
        health_section(ui, cx, c, h);
    } else if let Some(hc) = &c.healthcheck {
        kit::section(ui, "Health", None);
        common::note(
            ui,
            &format!("Health check {} (no probe has run yet).", hc.test),
        );
    }
    kit::section(ui, "Running commit", None);
    form::card(ui, Tone::Neutral, |ui| {
        commit_card(ui, cx, d, st, refs, pkg, c, image.as_ref(), actions);
    });
    if !c.ports.is_empty() {
        kit::section(ui, "Ports", Some(c.ports.len()));
        let host = endpoint::port_host(&st.endpoint());
        for port in &c.ports {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                match port.url(&host) {
                    Some(url) if c.state.running() => {
                        if kit::link(ui, &url).clicked() {
                            actions.push(Action::OpenUrl(url.clone()));
                        }
                        let on = match port.host_ip.as_str() {
                            "" | "0.0.0.0" | "::" => "every interface".to_owned(),
                            ip => ip.to_owned(),
                        };
                        ui.label(
                            RichText::new(format!(
                                "→ {}/{} in the container · bound on {on}{}",
                                port.container_port,
                                port.proto,
                                if port.ipv6 { ", IPv4 and IPv6" } else { "" }
                            ))
                            .size(size::SMALL)
                            .color(p.text_2),
                        );
                    }
                    _ => {
                        ui.label(kit::mono(port.text()).color(p.text));
                    }
                }
            });
        }
        if c.ports.iter().any(|p| p.url(&host).is_some()) {
            common::note(ui, "Links open in your browser, only when clicked.");
        }
    }
    kit::section(ui, "Image", None);
    image_section(ui, cx, c, image.as_ref(), actions);
    if !c.mounts.is_empty() {
        kit::section(ui, "Mounts", Some(c.mounts.len()));
        let rows: Vec<Vec<Cell>> = c
            .mounts
            .iter()
            .map(|m| {
                vec![
                    Cell::Sec(m.kind.clone()),
                    Cell::Mono(m.source.clone()),
                    Cell::Mono(m.destination.clone()),
                    Cell::Sec(if m.read_only {
                        "read-only".into()
                    } else {
                        "read-write".into()
                    }),
                ]
            })
            .collect();
        layout::wide(ui, "docker_mounts", 620.0, |ui| {
            grid(
                ui,
                "docker_mounts",
                &[
                    Col::text("Type"),
                    Col::text("Source"),
                    Col::text("Destination"),
                    Col::text("Mode"),
                ],
                &rows,
                false,
                |_, _| {},
            );
        });
    }
    kit::section(ui, "Details", None);
    kit::kv(ui, 130.0, |kv| {
        if !c.networks.is_empty() {
            kv.text(
                "Networks",
                c.networks
                    .iter()
                    .map(|n| {
                        if n.ip.is_empty() {
                            n.name.clone()
                        } else {
                            format!("{} ({})", n.name, n.ip)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        } else if !c.network_mode.is_empty() {
            kv.text("Network", c.network_mode.as_str());
        }
        kv.text(
            "Restart policy",
            if c.restart_max > 0 {
                format!(
                    "{} (at most {}) · restarted {}×",
                    c.restart_policy, c.restart_max, c.restart_count
                )
            } else {
                format!("{} · restarted {}×", c.restart_policy, c.restart_count)
            },
        );
        if c.auto_remove {
            kv.text("Auto remove", "removed when it stops (--rm)");
        }
        kv.text("Created", common::when(c.created, cx.now));
        kv.text("Started", common::when(c.started, cx.now));
        if !c.state.up() {
            kv.text("Finished", common::when(c.finished, cx.now));
            kv.text("Exit code", c.exit_code.to_string());
        }
        if c.state.up() && c.pid > 0 {
            kv.text("PID", c.pid.to_string());
        }
        kv.text("Command", kit::mono(&c.command));
        if !c.user.is_empty() {
            kv.text("User", c.user.as_str());
        }
        if !c.working_dir.is_empty() {
            kv.text("Working dir", kit::mono(&c.working_dir));
        }
        if !c.platform.is_empty() {
            kv.text("Platform", c.platform.as_str());
        }
        kv.row("ID", |ui| common::copy_line(ui, &c.id, actions));
    });
    kit::section(ui, "Environment", Some(c.env_names.len()));
    if c.env_names.is_empty() {
        common::note(ui, "No environment variables.");
    } else {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(10.0, 4.0);
            for n in &c.env_names {
                ui.label(kit::mono(n).color(p.text));
            }
        });
        common::note(
            ui,
            "Names only: Hadion drops the values where docker's answer is read (they often hold passwords).",
        );
    }
    if c.state.running() {
        kit::section(ui, "Live stats", None);
        stats_section(ui, st, c);
    }
    kit::section(ui, "Labels", Some(c.labels.len()));
    explain::details(
        ui,
        ("docker_labels", &c.id),
        &format!("Show the {} labels", c.labels.len()),
        false,
        |ui| {
            kit::kv(ui, 280.0, |kv| {
                for (k, v) in &c.labels {
                    kv.text(k, kit::mono(model::redact(v)));
                }
            });
        },
    );
    kit::section(ui, "Inspect", None);
    match &st.inspect {
        Some((id, None)) if id == &c.id => kit::loading(ui, "Reading docker inspect…"),
        Some((id, Some(Ok(json)))) if id == &c.id => {
            common::note(
                ui,
                "docker inspect with every environment value replaced by •••, then the exposure rules.",
            );
            kit::code_block(ui, json, actions);
            if IconButton::new(Icon::Close, "Hide")
                .compact()
                .show(ui)
                .clicked()
            {
                actions.push(act(Act::HideInspect));
            }
        }
        Some((id, Some(Err(e)))) if id == &c.id => {
            kit::callout(ui, Tone::Danger, "docker inspect failed", e)
        }
        _ => {
            if IconButton::new(Icon::Eye, "Show the raw inspect (values masked)")
                .compact()
                .show(ui)
                .on_hover_text(commands::display(&commands::inspect_args(
                    &ctx,
                    std::slice::from_ref(&c.id),
                )))
                .clicked()
            {
                actions.push(act(Act::Inspect(c.id.clone())));
            }
        }
    }
}

fn container_actions(ui: &mut Ui, st: &State, c: &Container, ctx: &str, actions: &mut Vec<Action>) {
    let mut ops: Vec<(Icon, ContainerOp)> = Vec::new();
    match c.state {
        CState::Running => {
            ops.push((Icon::Stop, ContainerOp::Stop));
            ops.push((Icon::Rerun, ContainerOp::Restart));
            ops.push((Icon::Pause, ContainerOp::Pause));
        }
        CState::Paused => {
            ops.push((Icon::Play, ContainerOp::Unpause));
            ops.push((Icon::Stop, ContainerOp::Stop));
        }
        CState::Restarting => {
            ops.push((Icon::Stop, ContainerOp::Stop));
            ops.push((Icon::Rerun, ContainerOp::Restart));
        }
        _ => ops.push((Icon::Play, ContainerOp::Start)),
    }
    common::buttons(ui, |ui| {
        for (icon, op) in &ops {
            if IconButton::new(*icon, op.verb())
                .compact()
                .show(ui)
                .on_hover_text(commands::display(&op.args(ctx, &c.id)))
                .clicked()
            {
                actions.push(act(Act::Container {
                    id: c.id.clone(),
                    op: *op,
                }));
            }
        }
        if IconButton::new(Icon::Logs, "Logs")
            .compact()
            .show(ui)
            .on_hover_text(commands::display(&commands::logs_args(
                ctx,
                &c.id,
                true,
                commands::LOG_TAIL,
                true,
            )))
            .clicked()
        {
            actions.push(act(Act::Logs(c.id.clone())));
        }
        if IconButton::new(Icon::Terminal, "Shell")
            .compact()
            .enabled(c.state.running())
            .show(ui)
            .on_hover_text(format!(
                "A terminal tab running {}",
                commands::display(&commands::exec_shell_args(ctx, &c.id))
            ))
            .clicked()
        {
            actions.push(act(Act::Shell(c.id.clone())));
        }
        if IconButton::new(Icon::Copy, "Copy ID")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::Copy(c.id.clone()));
        }
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        if c.state.up() {
            let kill = ContainerOp::Kill;
            if form::danger(ui, "Kill…", true)
                .on_hover_text(commands::display(&kill.args(ctx, &c.id)))
                .clicked()
            {
                actions.push(act(Act::Container {
                    id: c.id.clone(),
                    op: kill,
                }));
            }
        }
        let force = st.force_remove && c.state.up();
        let rm = ContainerOp::Remove { force };
        let can = !c.state.up() || st.force_remove;
        if form::danger(
            ui,
            if force {
                "Force remove…"
            } else {
                "Remove…"
            },
            can,
        )
        .on_hover_text(commands::display(&rm.args(ctx, &c.id)))
        .clicked()
        {
            actions.push(act(Act::Container {
                id: c.id.clone(),
                op: rm,
            }));
        }
        if c.state.up() {
            let mut f = st.force_remove;
            if form::checkbox(ui, &mut f, "Force (kill it first)").changed() {
                actions.push(act(Act::ForceRemove(f)));
            }
        }
    });
}

fn health_section(ui: &mut Ui, cx: &Cx, c: &Container, h: &model::Health) {
    kit::kv(ui, 130.0, |kv| {
        kv.row("Status", |ui| {
            ui.horizontal(|ui| {
                if let Some((t, tone)) = health_badge(h.status) {
                    kit::badge(ui, &t, tone);
                }
                if h.failing_streak > 0 {
                    ui.label(
                        RichText::new(format!(
                            "{} failed in a row",
                            format::plural(h.failing_streak as usize, "probe", "probes")
                        ))
                        .size(size::SMALL)
                        .color(pal(ui).text_2),
                    );
                }
            });
        });
        if let Some(hc) = &c.healthcheck {
            kv.text("Check", kit::mono(&hc.test));
            kv.text(
                "Timing",
                format!(
                    "every {} s, {} s timeout, unhealthy after {} failures{}",
                    hc.interval_s,
                    hc.timeout_s,
                    hc.retries,
                    if hc.start_period_s > 0.0 {
                        format!(", {} s to start", hc.start_period_s)
                    } else {
                        String::new()
                    }
                ),
            );
        }
    });
    if h.log.is_empty() {
        return;
    }
    let rows: Vec<Vec<Cell>> = h
        .log
        .iter()
        .rev()
        .map(|pr| {
            vec![
                Cell::Muted(secs_ago(pr.end.or(pr.start), cx.now)),
                Cell::Sec(match (pr.start, pr.end) {
                    (Some(a), Some(b)) => format!("{} s", (b - a).max(0)),
                    _ => "–".into(),
                }),
                if pr.exit_code == 0 {
                    Cell::Tone("0 · passed".into(), Tone::Success)
                } else {
                    Cell::Tone(format!("{} · failed", pr.exit_code), Tone::Danger)
                },
                Cell::Mono(pr.output.lines().next().unwrap_or("").to_owned()),
            ]
        })
        .collect();
    layout::wide(ui, ("docker_probes", &c.id), 620.0, |ui| {
        grid(
            ui,
            &format!("docker_probes_{}", c.id),
            &[
                Col::text("When"),
                Col::num("Took"),
                Col::text("Exit"),
                Col::text("Output"),
            ],
            &rows,
            false,
            |_, _| {},
        );
    });
    common::note(
        ui,
        "The last probes docker keeps, newest first; the output is shortened and passed through the exposure rules.",
    );
}

#[allow(clippy::too_many_arguments)]
fn commit_card(
    ui: &mut Ui,
    cx: &Cx,
    d: &DockerCx,
    st: &State,
    refs: &mut RefCache,
    pkg: &PkgState,
    c: &Container,
    image: Option<&Image>,
    actions: &mut Vec<Action>,
) {
    let r = running_commit(c, image, refs, pkg, d);
    match &r.found {
        Some((ev, m)) => {
            if let Some(id) = m.commit {
                let rel = refs.distance(cx.snap, id).text();
                ui.label(
                    RichText::new(format!(
                        "Running commit {}{}",
                        &id.to_string()[..7],
                        if rel.is_empty() {
                            String::new()
                        } else {
                            format!(" · {rel}")
                        }
                    ))
                    .size(size::BODY)
                    .family(crate::theme::semibold())
                    .color(pal(ui).text_strong),
                );
            }
            common::match_rows(ui, cx, refs, m, "Matched by", actions);
            common::note(ui, &format!("Found through the {}.", ev.text()));
        }
        None => {
            if r.compose_built {
                kit::body(
                    ui,
                    "Built by docker compose from this repository: compose does not record which commit it built, so the image carries no revision label.",
                );
                common::note(
                    ui,
                    "Build it with labels (below) to see the commit here next time; the next Up (without --build) runs that image.",
                );
                if let Some(i) = &c.compose
                    && let Some(t) = st.project_target(&i.project)
                    && let Some(Some(Ok(cfg))) = st.config(&t).map(|x| x.result.as_ref())
                    && let Some(s) = cfg.service(&i.service)
                    && s.build.is_some()
                {
                    let image = s
                        .image
                        .clone()
                        .unwrap_or_else(|| compose::built_image_name(&i.project, &i.service));
                    if IconButton::new(Icon::Hammer, &format!("Build {} with labels…", i.service))
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(act(Act::BuildForm(BuildSource::Service {
                            project: i.project.clone(),
                            service: i.service.clone(),
                            image,
                        })));
                    }
                }
            } else if let Some((k, v)) = &r.label {
                kit::body(
                    ui,
                    &format!(
                        "The image's {k} label says {v}, which is not a commit of this repository's loaded history."
                    ),
                );
                common::note(
                    ui,
                    "It may come from a base image, or from a commit not fetched yet.",
                );
            } else if image.is_some_and(|i| !i.digests.is_empty()) && !r.source_matches {
                kit::body(
                    ui,
                    &format!(
                        "{} comes from a registry and was not built from this repository: there is no commit of it to show.",
                        short_image(&c.image)
                    ),
                );
            } else {
                kit::body(
                    ui,
                    "The image has no revision label, and none of its tags names a commit, git tag or branch of this repository.",
                );
            }
        }
    }
    if let Some(src) = &r.source {
        common::note(
            ui,
            &format!(
                "Source label: {src}{}",
                if r.source_matches {
                    " (this repository)"
                } else {
                    " (another repository)"
                }
            ),
        );
    }
}

fn image_section(
    ui: &mut Ui,
    cx: &Cx,
    c: &Container,
    image: Option<&Image>,
    actions: &mut Vec<Action>,
) {
    kit::kv(ui, 130.0, |kv| {
        kv.row("Reference", |ui| common::copy_line(ui, &c.image, actions));
        match image {
            Some(i) => {
                kv.text("ID", kit::mono(i.short_id()));
                kv.text("Created", common::when(i.created, cx.now));
                kv.text("Size", format::bytes(i.size as usize));
                if !i.os.is_empty() {
                    kv.text("Platform", format!("{}/{}", i.os, i.arch));
                }
                if !i.tags.is_empty() {
                    kv.text("Tags", kit::mono(i.tags.join(", ")));
                }
                if i.digests.is_empty() {
                    kv.muted("Digests", "none (built here, never pushed or pulled)");
                } else {
                    kv.row("Digests", |ui| {
                        for dg in &i.digests {
                            common::copy_line(ui, dg, actions);
                        }
                    });
                }
                let oci: Vec<(&String, &String)> = i
                    .labels
                    .iter()
                    .filter(|(k, _)| {
                        k.starts_with("org.opencontainers.")
                            || k.starts_with("org.label-schema.")
                            || k.starts_with("hadion.")
                    })
                    .collect();
                if !oci.is_empty() {
                    kv.row("OCI labels", |ui| {
                        for (k, v) in oci {
                            ui.add(
                                egui::Label::new(
                                    kit::mono(format!("{k}={}", model::redact(v)))
                                        .color(pal(ui).text),
                                )
                                .wrap(),
                            );
                        }
                    });
                }
            }
            None => kv.muted(
                "ID",
                &format!("{} (not found: removed or not inspectable)", c.image_id),
            ),
        }
    });
}

fn stats_section(ui: &mut Ui, st: &State, c: &Container) {
    match st.stats.get(&c.id) {
        Some(s) => match &s.sample {
            Some(Ok(x)) => {
                let (used, limit) = x.mem.split_once(" / ").unwrap_or((x.mem.as_str(), ""));
                crate::views::insights::stats(
                    ui,
                    &[
                        Stat::new("CPU", x.cpu.map_or("–".into(), |c| format!("{c:.1}%")))
                            .tone_if(x.cpu.is_some_and(|c| c > 90.0), Tone::Warning),
                        Stat::new(
                            if limit.is_empty() {
                                "memory"
                            } else {
                                "memory used"
                            },
                            used.to_owned(),
                        ),
                        Stat::new(
                            "memory limit",
                            if limit.is_empty() {
                                "–".into()
                            } else {
                                limit.to_owned()
                            },
                        ),
                        Stat::new("network in / out", x.net.clone()),
                        Stat::new("disk read / write", x.block.clone()),
                        Stat::new("processes", x.pids.map_or("–".into(), |p| p.to_string())),
                    ],
                );
                common::note(
                    ui,
                    &format!(
                        "docker stats --no-stream every 3 s while this container is selected and shown{}.",
                        s.at.map(|t| format!(
                            " · sampled {} ago",
                            format::millis(t.elapsed().as_millis() as u64)
                        ))
                        .unwrap_or_default()
                    ),
                );
            }
            Some(Err(e)) => kit::callout(ui, Tone::Warning, "docker stats failed", e),
            None => kit::loading(ui, "Sampling…"),
        },
        None => kit::loading(ui, "Sampling…"),
    }
}

// ---------------------------------------------------------------------------
// Build form
// ---------------------------------------------------------------------------

fn build_card(ui: &mut Ui, d: &DockerCx, st: &State, console: &Console, actions: &mut Vec<Action>) {
    let Some(mut f) = st.build.clone() else {
        return;
    };
    let ctx = st.context_name().unwrap_or_default();
    let before = f.clone();
    form::card(ui, Tone::Accent, |ui| {
        ui.horizontal(|ui| {
            kit::subtitle(ui, "Build an image with OCI labels");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if IconButton::icon(Icon::Close)
                    .compact()
                    .show(ui)
                    .on_hover_text("Close the form")
                    .clicked()
                {
                    actions.push(act(Act::BuildClose));
                }
            });
        });
        match &f.source {
            BuildSource::Service {
                project,
                service,
                image,
            } => {
                kit::secondary(
                    ui,
                    &format!(
                        "From the {service} service of {project}. Compose runs the image {image} for it: built under that name with these labels, the next Up (without --build) runs it and this tab can say which commit it is."
                    ),
                );
            }
            BuildSource::Dockerfile(rel) => {
                kit::secondary(ui, &format!("From {rel}."));
            }
        }
        let w = layout::input_width(ui, 520.0);
        form::field(ui, "Dockerfile", None, |ui| {
            if st.files.dockerfiles.len() > 1 || !st.files.dockerfiles.contains(&f.dockerfile) {
                let mut opts: Vec<(String, String)> = st
                    .files
                    .dockerfiles
                    .iter()
                    .map(|d| (d.clone(), d.clone()))
                    .collect();
                if !opts.iter().any(|(v, _)| *v == f.dockerfile) {
                    opts.insert(0, (f.dockerfile.clone(), f.dockerfile.clone()));
                }
                // The inputs' frame adds 22 points to their width.
                form::select(ui, "docker_build_df", &mut f.dockerfile, &opts, w + 22.0);
            } else {
                form::input(
                    ui,
                    "docker_build_df_text",
                    &mut f.dockerfile,
                    form::InputOpts::new("Dockerfile").width(w).mono(),
                );
            }
        });
        form::field(
            ui,
            "Context",
            Some("The folder sent to docker, relative to the repository."),
            |ui| {
                form::input(
                    ui,
                    "docker_build_ctx",
                    &mut f.context_dir,
                    form::InputOpts::new(".").width(w).mono(),
                );
            },
        );
        form::field(ui, "Tag", None, |ui| {
            form::input(
                ui,
                "docker_build_tag",
                &mut f.tag,
                form::InputOpts::new("name:tag").width(w).mono(),
            );
        });
        let labels = preview_labels(d);
        form::field(
            ui,
            "Labels",
            Some("Set from the repository; created is the moment the build starts."),
            |ui| {
                kit::code_block(ui, &labels.join("\n"), actions);
            },
        );
        if f.scanners.is_empty() {
            common::note(
                ui,
                "Install Trivy or Grype to scan the image after the build.",
            );
        } else {
            let label = format!("Scan with {} after the build", f.scanners[0].label());
            form::checkbox(ui, &mut f.scan, &label);
        }
        let preview = commands::BuildPlan {
            dockerfile: f.dockerfile.clone(),
            context_dir: f.context_dir.clone(),
            tag: f.tag.clone(),
            labels: Vec::new(),
        };
        let mut line = commands::display(&preview.args(&ctx));
        if let Some((cmd, rest)) = line.split_once(" -t ") {
            // Show the labels where they go.
            let (tag, tail) = rest.split_once(' ').unwrap_or((rest, ""));
            line = format!("{cmd} -t {tag} --label … ({} labels) {tail}", labels.len());
        }
        explain::quiet(ui, &line);
        form::actions_row(ui, |ui| {
            if form::primary(
                ui,
                if st.remote() {
                    "Build (remote)…"
                } else {
                    "Build"
                },
                d.head.is_some(),
            )
            .clicked()
            {
                actions.push(act(Act::Build));
            }
        });
        if d.head.is_none() {
            form::error(
                ui,
                "The repository has no commit yet: there is nothing to label the image with.",
            );
        }
    });
    if f != before {
        actions.push(act(Act::BuildEdit(f)));
    }
    for s in &st.scans {
        scan_result(ui, s, console, actions);
    }
}

fn preview_labels(d: &DockerCx) -> Vec<String> {
    let Some((full, _, tag, dirty)) = &d.head else {
        return Vec::new();
    };
    commands::build_labels(&commands::BuildInputs {
        head: full.clone(),
        source: d.source.clone(),
        version: tag.clone(),
        dirty: *dirty,
        created: "<when the build starts>".into(),
    })
    .into_iter()
    .map(|(k, v)| format!("{k}={v}"))
    .collect()
}

fn scan_result(ui: &mut Ui, s: &docker::ScanResult, console: &Console, actions: &mut Vec<Action>) {
    ui.add_space(6.0);
    form::card(ui, Tone::Neutral, |ui| {
        ui.horizontal(|ui| {
            kit::subtitle(ui, &format!("{} scan of {}", s.scanner.label(), s.tag));
            let (t, tone) = match &s.result {
                None => (
                    if console.get(s.run).is_some_and(|r| r.running()) {
                        "Running"
                    } else {
                        "Reading the report"
                    },
                    Tone::Accent,
                ),
                Some(Ok(_)) => ("Done", Tone::Success),
                Some(Err(_)) => ("Failed", Tone::Danger),
            };
            kit::badge(ui, t, tone);
        });
        match &s.result {
            Some(Ok(sum)) => {
                crate::views::insights::stats(
                    ui,
                    &[
                        Stat::new("critical", sum.counts[0].to_string())
                            .tone_if(sum.counts[0] > 0, Tone::Danger),
                        Stat::new("high", sum.counts[1].to_string())
                            .tone_if(sum.counts[1] > 0, Tone::Danger),
                        Stat::new("medium", sum.counts[2].to_string())
                            .tone_if(sum.counts[2] > 0, Tone::Warning),
                        Stat::new("low", sum.counts[3].to_string()),
                        Stat::new("fixable", sum.fixable.to_string()),
                    ],
                );
                for v in sum.top.iter().take(8) {
                    common::note(
                        ui,
                        &format!(
                            "{} · {} {} · {}",
                            v.id,
                            v.package,
                            v.installed,
                            v.fixed
                                .as_deref()
                                .map_or("no fix yet".to_owned(), |f| format!("fixed in {f}"))
                        ),
                    );
                }
                crate::views::insights::more(ui, sum.total, sum.top.len().min(8));
            }
            Some(Err(e)) => kit::code_block(ui, e, actions),
            None => common::note(ui, "The scan runs in the console below."),
        }
    });
}
