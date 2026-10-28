//! The Kubernetes tab: workloads, rollouts, images → commits, pods, events,
//! history and logs, through kubectl.

use egui::{Frame, Margin, RichText, Ui};

use gitvis_core::devops::runtime::kube::{
    self, ErrorKind, Event, KubeConfig, NamespaceLoad, Pod, RolloutState, Workload,
};
use gitvis_core::devops::runtime::matching::RefMatch;
use gitvis_core::devops::tools::Tool;

use super::common::{self, Cx};
use super::{Act, KubeState, PkgState, Probe, RefCache, State, WriteKind};
use crate::devops::console::Console;
use crate::format;
use crate::state::Action;
use crate::theme::{pal, size};
use crate::views::icons::{Icon, IconButton};
use crate::views::insights::{Cell, detail_title, grid, list_none, list_top};
use crate::views::kit::{self, Col, Leading, Row, Stat, Tone};

fn state_tone(s: RolloutState) -> Tone {
    match s {
        RolloutState::Complete => Tone::Success,
        RolloutState::Progressing => Tone::Accent,
        RolloutState::Failed => Tone::Danger,
        RolloutState::Stalled => Tone::Warning,
        RolloutState::ScaledToZero => Tone::Neutral,
    }
}

fn status_tone(status: &str) -> Tone {
    match status {
        "Running" | "Completed" | "Succeeded" => Tone::Neutral,
        "Pending" | "ContainerCreating" | "PodInitializing" | "Terminating" => Tone::Accent,
        s if s.starts_with("Init:") && !s.contains("Err") && !s.contains("BackOff") => Tone::Accent,
        s if kube::is_failure_reason(s.trim_start_matches("Init:"))
            || matches!(s, "Error" | "OOMKilled" | "Failed" | "Evicted")
            || s.starts_with("Init:Error")
            || s.starts_with("ExitCode") =>
        {
            Tone::Danger
        }
        _ => Tone::Warning,
    }
}

/// `api-gateway:sha-9a9d91b` from a full image reference.
fn short_image(image: &str) -> String {
    let i = kube::parse_image(image);
    match (&i.tag, &i.digest) {
        (Some(t), _) => format!("{}:{t}", i.name()),
        (None, Some(d)) => format!(
            "{}@{}",
            i.name(),
            gitvis_core::devops::runtime::packages::short_digest(d)
        ),
        _ => i.name().to_owned(),
    }
}

pub fn show(ui: &mut Ui, cx: &Cx, rt: &mut State, console: &Console, actions: &mut Vec<Action>) {
    let State {
        kube: k, pkg, refs, ..
    } = rt;
    header(ui, cx, k, actions);
    if k.tool == Some(false) {
        common::page(ui, "k8s_missing", |ui| {
            common::missing_tool(
                ui,
                "kubectl is not installed",
                "The Kubernetes tab reads your clusters through kubectl, with the contexts of your kubeconfig. gitvis never changes the kubeconfig or the current context.",
                &[Tool::Kubectl.install_hint()],
                Act::KubeCheckTool,
                actions,
            );
        });
        return;
    }
    let cfg = match &k.config {
        None => {
            common::page(ui, "k8s_loading", |ui| {
                kit::empty_state(
                    ui,
                    Icon::Wheel,
                    if k.loading {
                        "Reading the kubeconfig"
                    } else {
                        "Kubernetes"
                    },
                    "Contexts come from your kubeconfig; workloads and pods from the chosen context and namespace.",
                    None,
                );
            });
            return;
        }
        Some(Err(e)) => {
            let e = e.clone();
            common::page(ui, "k8s_cfg_err", |ui| {
                common::error_block(
                    ui,
                    "kubectl could not read the kubeconfig",
                    &e,
                    Some((
                        "kubectl reads ~/.kube/config, or the files listed in KUBECONFIG.",
                        None,
                    )),
                    Some(("Try again", Act::KubeRefresh)),
                    actions,
                );
            });
            return;
        }
        Some(Ok(c)) => c.clone(),
    };
    if cfg.contexts.is_empty() {
        common::page(ui, "k8s_noctx", |ui| {
            kit::empty_state(
                ui,
                Icon::Wheel,
                "No contexts in the kubeconfig",
                "kubectl has no cluster to talk to. Create one locally (kind create cluster, k3d cluster create) or add a context with your cloud's CLI (aws eks update-kubeconfig, gcloud container clusters get-credentials, az aks get-credentials), then refresh.",
                None,
            );
        });
        return;
    }
    toolbar(ui, k, &cfg, actions);
    let Some(load) = &k.load else {
        common::page(ui, "k8s_wait", |ui| {
            kit::empty_state(
                ui,
                Icon::Wheel,
                if k.loading {
                    "Asking the cluster"
                } else {
                    "Not loaded"
                },
                "Reading namespaces, deployments, stateful sets, daemon sets and pods.",
                None,
            );
        });
        return;
    };
    if let Some(e) = load.fatal() {
        let e = e.to_owned();
        common::page(ui, "k8s_unreachable", |ui| {
            unreachable(ui, cx, k, &cfg, load, &e, actions)
        });
        return;
    }
    if let Some(Err(e)) = &load.workloads {
        let e = e.clone();
        let target = load.target.clone();
        common::page(ui, "k8s_wl_err", |ui| {
            let kind = kube::classify(&e);
            let (hint, cmd) = kube::hint(kind, &target.context);
            common::error_block(
                ui,
                &format!("Could not list the workloads of {}", target.namespace),
                &e,
                Some((&hint, cmd.as_deref())),
                Some(("Try again", Act::KubeRefresh)),
                actions,
            );
        });
        return;
    }
    common::list_pane(ui, "k8s_list", |ui| workload_list(ui, cx, k, actions));
    common::console_panel(
        ui,
        "k8s_logs",
        console,
        k.log_run,
        Act::KubeHideLogs,
        actions,
    );
    let sel = k.selected.clone();
    common::detail_pane(ui, ("k8s_detail", sel.clone()), |ui| {
        match sel.as_ref().and_then(|s| k.workload(s)).cloned() {
            Some(w) => workload_detail(ui, cx, k, pkg, refs, &w, actions),
            None => overview(ui, cx, k, actions),
        }
    });
}

fn header(ui: &mut Ui, cx: &Cx, k: &KubeState, actions: &mut Vec<Action>) {
    let mut parts: Vec<String> = Vec::new();
    if let Some(v) = &k.version {
        parts.push(format!(
            "kubectl {}",
            v.trim_start_matches("Client Version: ")
        ));
    }
    if let Some(t) = k.target() {
        let server = k
            .config
            .as_ref()
            .and_then(|c| c.as_ref().ok())
            .and_then(|c| c.context(&t.context))
            .and_then(|c| c.server.clone())
            .map(|s| format!(" ({s})"))
            .unwrap_or_default();
        parts.push(format!("context {}{server}", t.context));
        parts.push(format!("namespace {}", t.namespace));
    }
    let f = common::fetched(k.fetched, Some(k.load_ms), cx.now);
    if !f.is_empty() {
        parts.push(f);
    }
    let sub = if parts.is_empty() {
        "Deployments, images, rollouts and pod logs through kubectl".to_owned()
    } else {
        parts.join(" · ")
    };
    let loading = k.loading;
    crate::devops::pages::layout::page_header(ui, "Kubernetes", &sub, |ui| {
        if loading {
            ui.add(egui::Spinner::new().size(14.0));
        }
        if IconButton::new(Icon::Rerun, "Refresh")
            .compact()
            .enabled(!loading && k.tool != Some(false))
            .show(ui)
            .on_hover_text("Read the kubeconfig, namespaces, workloads and pods again")
            .clicked()
        {
            actions.push(Act::KubeRefresh.into());
        }
    });
}

fn toolbar(ui: &mut Ui, k: &KubeState, cfg: &KubeConfig, actions: &mut Vec<Action>) {
    let Some(target) = k.target() else { return };
    let p = *pal(ui);
    Frame::NONE
        .inner_margin(Margin {
            left: 24,
            right: 16,
            top: 8,
            bottom: 8,
        })
        .show(ui, |ui| {
            let w = ui.available_width();
            // One row on a wide window, two on a narrow one: combo boxes do
            // not wrap, and a row wider than the page widens the panes below.
            let wide = w >= 780.0;
            let row = |ui: &mut Ui, add: &mut dyn FnMut(&mut Ui)| {
                ui.allocate_ui_with_layout(
                    egui::vec2(w, 32.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        add(ui)
                    },
                );
            };
            let label_w = if wide { 0.0 } else { 76.0 };
            let label = |ui: &mut Ui, text: &str| {
                let r = ui.label(RichText::new(text).size(size::SMALL).color(p.text_2));
                if r.rect.width() < label_w {
                    ui.add_space(label_w - r.rect.width());
                }
            };
            let mut context = |ui: &mut Ui| {
                label(ui, "Context");
                let mut ctx = target.context.clone();
                let opts: Vec<(String, String)> = cfg
                    .contexts
                    .iter()
                    .map(|c| {
                        let mut label = c.name.clone();
                        if cfg.current.as_deref() == Some(c.name.as_str()) {
                            label.push_str("  (current)");
                        }
                        (c.name.clone(), label)
                    })
                    .collect();
                if crate::views::form::select(ui, "k8s_context", &mut ctx, &opts, 220.0) {
                    actions.push(Act::KubeContext(ctx).into());
                }
            };
            let namespace = |ui: &mut Ui, actions: &mut Vec<Action>| {
                label(ui, "Namespace");
                let mut ns = target.namespace.clone();
                let mut opts: Vec<(String, String)> = match k.load.as_ref().map(|l| &l.namespaces) {
                    Some(Ok(list)) => list
                        .iter()
                        .map(|n| {
                            let label = if n.phase == "Terminating" {
                                format!("{} (terminating)", n.name)
                            } else {
                                n.name.clone()
                            };
                            (n.name.clone(), label)
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                if !opts.iter().any(|(n, _)| *n == ns) {
                    opts.insert(0, (ns.clone(), ns.clone()));
                }
                if crate::views::form::select(ui, "k8s_namespace", &mut ns, &opts, 200.0) {
                    actions.push(Act::KubeNamespace(ns).into());
                }
                ui.add_space(8.0);
                if IconButton::new(Icon::Search, "Check all contexts")
                    .compact()
                    .show(ui)
                    .on_hover_text("Ask every context of the kubeconfig whether its cluster answers (kubectl get namespaces, 5 s timeout each)")
                    .clicked()
                {
                    actions.push(Act::KubeProbeAll.into());
                }
            };
            let mut later: Vec<Action> = Vec::new();
            if wide {
                row(ui, &mut |ui| {
                    context(ui);
                    ui.add_space(8.0);
                    namespace(ui, &mut later);
                });
            } else {
                row(ui, &mut |ui| context(ui));
                row(ui, &mut |ui| namespace(ui, &mut later));
            }
            actions.extend(later);
        });
    kit::hairline(ui);
}

/// A table-sized name for a kubectl failure.
fn short_error(k: ErrorKind) -> &'static str {
    match k {
        ErrorKind::Unreachable => "unreachable",
        ErrorKind::Timeout => "timed out",
        ErrorKind::Unauthorized => "credentials rejected",
        ErrorKind::Forbidden => "forbidden",
        ErrorKind::NotFound => "not found",
        ErrorKind::NoContext => "no such context",
        ErrorKind::NotInstalled => "kubectl missing",
        ErrorKind::Other => "failed",
    }
}

fn probe_table(ui: &mut Ui, k: &KubeState, cfg: &KubeConfig, actions: &mut Vec<Action>) {
    let current = k.target().map(|t| t.context);
    let rows: Vec<Vec<Cell>> = cfg
        .contexts
        .iter()
        .map(|c| {
            let (status, time) = match k.probes.get(&c.name) {
                None => (
                    Cell::Muted("not checked".into()),
                    Cell::Muted(String::new()),
                ),
                Some(Probe::Running) => (Cell::Sec("checking…".into()), Cell::Muted(String::new())),
                Some(Probe::Done { result: Ok(n), ms }) => (
                    Cell::Tone(
                        format!(
                            "answers · {}",
                            format::plural(*n, "namespace", "namespaces")
                        ),
                        Tone::Success,
                    ),
                    Cell::Muted(format::millis(*ms)),
                ),
                Some(Probe::Done { result: Err(e), ms }) => (
                    Cell::Tone(short_error(kube::classify(e)).to_owned(), Tone::Danger),
                    Cell::Muted(format::millis(*ms)),
                ),
            };
            let mut name = c.name.clone();
            if current.as_deref() == Some(c.name.as_str()) {
                name.push_str("  (shown)");
            }
            vec![
                Cell::Text(name),
                Cell::Mono(c.server.clone().unwrap_or_else(|| "–".into())),
                status,
                time,
            ]
        })
        .collect();
    let names: Vec<String> = cfg.contexts.iter().map(|c| c.name.clone()).collect();
    grid(
        ui,
        "k8s_probes",
        &[
            Col::text("Context"),
            Col::text("API server"),
            Col::text("Status"),
            Col::num("Took"),
        ],
        &rows,
        true,
        |r, resp| {
            if resp.clicked() {
                actions.push(Act::KubeContext(names[r].clone()).into());
            }
        },
    );
    let errors: Vec<(&str, &str)> = cfg
        .contexts
        .iter()
        .filter_map(|c| match k.probes.get(&c.name) {
            Some(Probe::Done { result: Err(e), .. }) => Some((c.name.as_str(), e.as_str())),
            _ => None,
        })
        .collect();
    if !errors.is_empty() {
        let text = errors
            .iter()
            .map(|(c, e)| format!("{c}: {e}"))
            .collect::<Vec<_>>()
            .join("\n");
        kit::code_block(ui, &text, actions);
    }
}

fn unreachable(
    ui: &mut Ui,
    _cx: &Cx,
    k: &KubeState,
    cfg: &KubeConfig,
    load: &NamespaceLoad,
    e: &str,
    actions: &mut Vec<Action>,
) {
    let kind = kube::classify(e);
    let t = &load.target;
    let (hint, cmd) = kube::hint(kind, &t.context);
    common::error_block(
        ui,
        &format!("{}: {}", kind.title(), t.context),
        e,
        Some((&hint, cmd.as_deref())),
        Some(("Try again", Act::KubeRefresh)),
        actions,
    );
    if let Some(c) = cfg.context(&t.context) {
        kit::kv(ui, 120.0, |kv| {
            kv.text("Context", c.name.as_str());
            kv.text("Cluster", c.cluster.as_str());
            match &c.server {
                Some(s) => kv.text("API server", kit::mono(s)),
                None => kv.muted("API server", "not in the kubeconfig"),
            }
            kv.text("User", c.user.as_str());
            kv.text("Namespace", t.namespace.as_str());
            kv.text(
                "Ran",
                kit::mono(kube::display(&kube::namespaces_args(&t.context))),
            );
        });
    }
    if kind == ErrorKind::Unreachable || kind == ErrorKind::Timeout {
        common::note(
            ui,
            "gitvis gave up quickly (10 s client timeout) and skipped the other calls. Commands that change the cluster are never run while it is unreachable.",
        );
    }
    kit::section_with(
        ui,
        "Contexts in the kubeconfig",
        Some(cfg.contexts.len()),
        |ui| {
            if IconButton::new(Icon::Search, "Check all")
                .compact()
                .show(ui)
                .clicked()
            {
                actions.push(Act::KubeProbeAll.into());
            }
        },
    );
    kit::secondary(
        ui,
        "Check which clusters answer (read-only: kubectl get namespaces with a 5 s timeout), then click one to switch. The kubeconfig's current context is not changed.",
    );
    probe_table(ui, k, cfg, actions);
}

fn workload_list(ui: &mut Ui, cx: &Cx, k: &mut KubeState, actions: &mut Vec<Action>) {
    let total = k.workloads().len();
    let selected = k.selected.is_some();
    list_top(
        ui,
        "Workloads",
        total,
        selected,
        Some(("k8s_filter", &mut k.filter, "Filter by name or image")),
        actions,
    );
    let vis = k.visible();
    if vis.is_empty() {
        list_none(
            ui,
            if total == 0 {
                "No deployments, stateful sets or daemon sets in this namespace. Pick another namespace above."
            } else {
                "No workload matches the filter."
            },
        );
        return;
    }
    let p = *pal(ui);
    type WorkloadRow = (
        (kube::WorkloadKind, String),
        String,
        String,
        Tone,
        common::Badges,
    );
    let rows: Vec<WorkloadRow> = vis
        .iter()
        .map(|w| {
            let r = kube::rollout(w);
            let tone = state_tone(r.state);
            let failing = k
                .pods_of(w)
                .iter()
                .filter(|p| p.failing().is_some())
                .count();
            let image = w
                .containers
                .first()
                .map(|c| short_image(&c.image))
                .unwrap_or_default();
            let more = if w.containers.len() > 1 {
                format!(" +{}", w.containers.len() - 1)
            } else {
                String::new()
            };
            let sub = format!(
                "{} · {}/{} ready · {image}{more}",
                w.kind.label(),
                w.ready,
                w.desired
            );
            let mut badges = vec![(r.state.label().to_owned(), tone)];
            if failing > 0 {
                badges.insert(0, (format!("{failing} failing"), Tone::Danger));
            }
            ((w.kind, w.name.clone()), w.name.clone(), sub, tone, badges)
        })
        .collect();
    let sel = k.selected.clone();
    kit::list(
        ui,
        "k8s_rows",
        rows.len(),
        kit::ROW_H,
        |i| sel.as_ref() == Some(&rows[i].0),
        |ui, rect, resp, i| {
            let (key, name, sub, tone, badges) = &rows[i];
            let color = match tone {
                Tone::Neutral => p.text_3,
                t => t.color(&p),
            };
            let meta = common::age(vis.get(i).and_then(|w| w.created), cx.now);
            kit::paint_row(
                ui,
                rect,
                &Row::new(name)
                    .leading(Leading::Dot(color))
                    .subtitle(sub)
                    .meta(&meta)
                    .badges(badges),
                sel.as_ref() == Some(key),
            );
            if resp.clicked() {
                actions.push(Act::KubeSelect(Some(key.clone())).into());
            }
            resp.context_menu(|ui| {
                if ui.button("Copy name").clicked() {
                    actions.push(Action::Copy(name.clone()));
                    ui.close();
                }
            });
        },
    );
}

fn overview(ui: &mut Ui, cx: &Cx, k: &KubeState, actions: &mut Vec<Action>) {
    let Some(t) = k.target() else { return };
    kit::title(ui, &format!("{} / {}", t.context, t.namespace));
    let w = k.workloads();
    let pods = k.pods();
    let count = |s: RolloutState| w.iter().filter(|x| kube::rollout(x).state == s).count();
    let failing = k.failing();
    common::gap(ui);
    crate::views::insights::stats(
        ui,
        &[
            Stat::new("workloads", format::count(w.len())),
            Stat::new("rolled out", format::count(count(RolloutState::Complete))).tone_if(
                count(RolloutState::Complete) == w.len() && !w.is_empty(),
                Tone::Success,
            ),
            Stat::new(
                "progressing",
                format::count(count(RolloutState::Progressing)),
            ),
            Stat::new("stalled", format::count(count(RolloutState::Stalled)))
                .tone_if(count(RolloutState::Stalled) > 0, Tone::Warning),
            Stat::new("failed", format::count(count(RolloutState::Failed)))
                .tone_if(count(RolloutState::Failed) > 0, Tone::Danger),
            Stat::new("pods", format::count(pods.len())),
            Stat::new("pods failing", format::count(failing.len()))
                .tone_if(!failing.is_empty(), Tone::Danger),
        ],
    );
    if !failing.is_empty() {
        kit::section(ui, "Failing pods", Some(failing.len()));
        let rows: Vec<Vec<Cell>> = failing
            .iter()
            .map(|(p, reason)| {
                vec![
                    Cell::Mono(p.name.clone()),
                    Cell::Tone(reason.clone(), Tone::Danger),
                    Cell::Sec(format::count(p.restarts() as usize)),
                    Cell::Muted(
                        p.last_termination()
                            .map(|(c, t)| format!("{c}: {}", t.text()))
                            .unwrap_or_else(|| "–".into()),
                    ),
                    Cell::Muted(common::age(p.created, cx.now)),
                ]
            })
            .collect();
        let owners: Vec<Option<(kube::WorkloadKind, String)>> = failing
            .iter()
            .map(|(p, _)| {
                w.iter()
                    .find(|x| x.selector.matches(&p.labels))
                    .map(|x| (x.kind, x.name.clone()))
            })
            .collect();
        grid(
            ui,
            "k8s_failing",
            &[
                Col::text("Pod"),
                Col::text("Reason"),
                Col::num("Restarts"),
                Col::text("Last termination"),
                Col::num("Age"),
            ],
            &rows,
            true,
            |r, resp| {
                if resp.clicked()
                    && let Some(key) = &owners[r]
                {
                    actions.push(Act::KubeSelect(Some(key.clone())).into());
                    actions.push(Act::KubePod(Some(failing[r].0.name.clone())).into());
                }
            },
        );
    }
    kit::section(ui, "Workloads", Some(w.len()));
    if w.is_empty() {
        common::note(ui, "Nothing runs in this namespace.");
    }
    let vis = k.visible();
    let rows: Vec<Vec<Cell>> = vis
        .iter()
        .map(|x| {
            let r = kube::rollout(x);
            vec![
                Cell::Text(x.name.clone()),
                Cell::Sec(x.kind.label().to_owned()),
                Cell::Tone(r.state.label().to_owned(), state_tone(r.state)),
                Cell::Sec(format!("{}/{}", x.ready, x.desired)),
                Cell::Mono(
                    x.containers
                        .first()
                        .map(|c| short_image(&c.image))
                        .unwrap_or_default(),
                ),
                Cell::Muted(common::age(x.created, cx.now)),
            ]
        })
        .collect();
    let keys: Vec<(kube::WorkloadKind, String)> =
        vis.iter().map(|x| (x.kind, x.name.clone())).collect();
    grid(
        ui,
        "k8s_overview",
        &[
            Col::text("Name"),
            Col::text("Kind"),
            Col::text("Rollout"),
            Col::num("Ready"),
            Col::text("Image"),
            Col::num("Age"),
        ],
        &rows,
        true,
        |r, resp| {
            if resp.clicked() {
                actions.push(Act::KubeSelect(Some(keys[r].clone())).into());
            }
        },
    );
    kit::section(ui, "Other contexts", None);
    if let Some(Ok(cfg)) = &k.config {
        probe_table(ui, k, cfg, actions);
    }
}

#[allow(clippy::too_many_arguments)]
fn workload_detail(
    ui: &mut Ui,
    cx: &Cx,
    k: &KubeState,
    pkg: &PkgState,
    refs: &mut RefCache,
    w: &Workload,
    actions: &mut Vec<Action>,
) {
    let r = kube::rollout(w);
    let tone = state_tone(r.state);
    detail_title(
        ui,
        &w.name,
        &[
            (w.kind.label().to_owned(), Tone::Neutral),
            (r.state.label().to_owned(), tone),
        ],
    );
    let detail = k
        .detail
        .as_ref()
        .filter(|d| d.key == (w.kind, w.name.clone()));
    let loading = detail.is_some_and(|d| d.loading);
    if r.state == RolloutState::Complete {
        kit::secondary(ui, &format!("{}. {}", r.reason, r.message));
    } else {
        kit::callout(ui, tone, &r.reason, &r.message);
    }
    let pods = k.pods_of(w);
    let restarts: i64 = pods.iter().map(|p| p.restarts()).sum();
    let failing = pods.iter().filter(|p| p.failing().is_some()).count();
    crate::views::insights::stats(
        ui,
        &[
            Stat::new(
                if w.kind == kube::WorkloadKind::DaemonSet {
                    "desired (nodes)"
                } else {
                    "desired"
                },
                format::count(w.desired.max(0) as usize),
            ),
            Stat::new("ready", format::count(w.ready.max(0) as usize))
                .tone_if(w.ready < w.desired, Tone::Warning),
            Stat::new("up to date", format::count(w.updated.max(0) as usize)),
            Stat::new("available", format::count(w.available.max(0) as usize))
                .tone_if(w.available < w.desired, Tone::Warning),
            Stat::new("restarts", format::count(restarts.max(0) as usize))
                .tone_if(restarts > 0, Tone::Warning),
            Stat::new("pods failing", format::count(failing)).tone_if(failing > 0, Tone::Danger),
        ],
    );
    kit::section_with(ui, "Pods", Some(pods.len()), |ui| {
        if loading {
            ui.add(egui::Spinner::new().size(13.0));
        }
    });
    if let Some(Err(e)) = detail.and_then(|d| d.data.as_ref()).map(|d| &d.pods) {
        common::error_block(
            ui,
            "kubectl get pods failed",
            e,
            None,
            Some(("Try again", Act::KubeRefresh)),
            actions,
        );
    }
    if pods.is_empty() {
        common::note(
            ui,
            if w.desired == 0 {
                "Scaled to zero: no pods."
            } else {
                "No pod matches the selector yet."
            },
        );
    } else {
        pod_table(ui, cx, k, &pods, actions);
    }
    if let Some(name) = &k.pod
        && let Some(p) = pods.iter().find(|p| &p.name == name)
    {
        // Bring the pod's card into view when the selection changes.
        let mem = ui.make_persistent_id("k8s_scrolled_pod");
        let last: Option<String> = ui.data(|d| d.get_temp(mem));
        if last.as_deref() != Some(name.as_str()) {
            ui.scroll_to_cursor(Some(egui::Align::TOP));
            ui.data_mut(|d| d.insert_temp(mem, name.clone()));
        }
        crate::views::form::card(ui, Tone::Neutral, |ui| pod_detail(ui, cx, k, p, actions));
    }

    kit::section(
        ui,
        "What it runs",
        Some(w.containers.len() + w.init_containers.len()),
    );
    let rev = w.revision_hint();
    if let Some((key, v)) = rev {
        common::note(ui, &format!("{key} = {v}"));
    }
    for (c, init) in w
        .containers
        .iter()
        .map(|c| (c, false))
        .chain(w.init_containers.iter().map(|c| (c, true)))
    {
        crate::views::form::card(ui, Tone::Neutral, |ui| {
            ui.horizontal(|ui| {
                kit::subtitle(ui, &c.name);
                if init {
                    kit::badge(ui, "Init container", Tone::Neutral);
                }
            });
            common::copy_line(ui, &c.image, actions);
            let img = kube::parse_image(&c.image);
            let digest = pods.iter().find_map(|p| {
                p.containers
                    .iter()
                    .chain(&p.init_containers)
                    .find(|s| s.name == c.name)
                    .and_then(|s| s.digest().map(str::to_owned))
            });
            let by_digest = digest
                .as_deref()
                .or(img.digest.as_deref())
                .and_then(|d| pkg.tags_for_digest(d, img.ghcr_package().map(|(_, n)| n)));
            let mut m: Option<RefMatch> = refs.idx.as_ref().and_then(|idx| {
                kube::image_match(idx, &img, rev.map(|(_, v)| v), w.version_label())
            });
            if m.as_ref().is_none_or(|m| m.commit.is_none())
                && let Some((_, v)) = by_digest
                && let Some(better) = refs.idx.as_ref().and_then(|i| i.best(v.tags()))
                && better.commit.is_some()
            {
                m = Some(better);
            }
            match &m {
                Some(m) => {
                    if let Some(id) = m.commit {
                        let d = refs.distance(cx.snap, id);
                        let rel = d.text();
                        ui.label(
                            RichText::new(format!(
                                "Running commit {}{}",
                                gitvis_core::repo::short_id(id),
                                if rel.is_empty() {
                                    String::new()
                                } else {
                                    format!(" ({rel})")
                                }
                            ))
                            .size(size::BODY)
                            .color(pal(ui).text_strong),
                        );
                    }
                    common::match_rows(ui, cx, refs, m, "Matched by", actions);
                }
                None => common::note(
                    ui,
                    "Neither the tag, the digest nor a revision label names a commit, git tag or branch of this repository.",
                ),
            }
            if let Some(d) = &digest {
                kit::kv(ui, 120.0, |kv| {
                    kv.row("Pulled digest", |ui| common::copy_line(ui, d, actions));
                    if let Some((name, v)) = by_digest {
                        kv.text(
                            "Package",
                            format!(
                                "{name} version {} · tags {}",
                                v.id,
                                if v.tags().is_empty() {
                                    "none".into()
                                } else {
                                    v.tags().join(", ")
                                }
                            ),
                        );
                    }
                });
            }
        });
    }

    let data = detail.and_then(|d| d.data.as_ref());
    kit::section(
        ui,
        "Events",
        data.and_then(|d| d.events.as_ref().ok()).map(Vec::len),
    );
    match data.map(|d| &d.events) {
        None if loading => kit::loading(ui, "Loading events…"),
        None => common::note(ui, "Not loaded."),
        Some(Err(e)) => common::error_block(
            ui,
            "kubectl get events failed",
            e,
            None,
            Some(("Try again", Act::KubeRefresh)),
            actions,
        ),
        Some(Ok(ev)) if ev.is_empty() => common::note(
            ui,
            "No events for this workload. Kubernetes keeps events for about an hour; pod events are under each pod.",
        ),
        Some(Ok(ev)) => events_table(ui, cx, "k8s_events", ev),
    }

    kit::section(
        ui,
        "Rollout history",
        data.and_then(|d| d.history.as_ref().ok()).map(Vec::len),
    );
    match data.map(|d| &d.history) {
        None if loading => kit::loading(ui, "Loading history…"),
        None => common::note(ui, "Not loaded."),
        Some(Err(e)) => common::error_block(
            ui,
            "kubectl rollout history failed",
            e,
            None,
            Some(("Try again", Act::KubeRefresh)),
            actions,
        ),
        Some(Ok(h)) if h.is_empty() => common::note(ui, "No revisions recorded."),
        Some(Ok(h)) => {
            let current = h.first().map(|r| r.number);
            for rev in h.iter().take(15) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(kit::mono(format!("#{}", rev.number)));
                    if Some(rev.number) == current {
                        kit::badge(ui, "Current", Tone::Success);
                    }
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                rev.cause.as_deref().unwrap_or("no change cause recorded"),
                            )
                            .size(size::SMALL)
                            .color(pal(ui).text_2),
                        )
                        .wrap(),
                    );
                    if Some(rev.number) != current
                        && IconButton::text(&format!("Roll back to #{}", rev.number))
                            .compact()
                            .show(ui)
                            .clicked()
                    {
                        actions.push(Act::KubeAsk(WriteKind::Undo(Some(rev.number))).into());
                    }
                });
            }
        }
    }

    kit::section(ui, "Details", None);
    kit::kv(ui, 120.0, |kv| {
        kv.text("Resource", kit::mono(w.resource()));
        kv.text("Namespace", w.namespace.as_str());
        kv.text("Created", common::when(w.created, cx.now));
        kv.text(
            "Strategy",
            match w.partition {
                Some(p) if p > 0 => format!("{} (partition {p})", w.strategy),
                _ => w.strategy.clone(),
            },
        );
        if let Some(rev) = w.revision() {
            kv.text("Revision", rev);
        }
        if !w.update_revision.is_empty() {
            kv.text(
                "Revisions",
                format!(
                    "current {} · update {}",
                    w.current_revision, w.update_revision
                ),
            );
        }
        kv.text(
            "Generation",
            format!("{} (observed {})", w.generation, w.observed_generation),
        );
        if !w.selector.is_empty() {
            kv.text("Selector", kit::mono(w.selector.to_arg()));
        }
        for c in &w.conditions {
            kv.text(
                &format!("{} ", c.kind),
                format!(
                    "{} · {}{}",
                    c.status,
                    c.reason,
                    c.updated
                        .or(c.changed)
                        .map(|t| format!(" · {}", format::relative(t, cx.now)))
                        .unwrap_or_default()
                ),
            );
        }
    });

    kit::section(ui, "Change", None);
    kit::secondary(
        ui,
        "Each of these asks first and shows the exact kubectl command. They change the cluster.",
    );
    common::buttons(ui, |ui| {
        if crate::views::form::button(ui, "Restart…", true)
            .on_hover_text("kubectl rollout restart: replaces every pod")
            .clicked()
        {
            actions.push(Act::KubeAsk(WriteKind::Restart).into());
        }
        if crate::views::form::button(ui, "Roll back to the previous revision…", true).clicked() {
            actions.push(Act::KubeAsk(WriteKind::Undo(None)).into());
        }
    });
    if w.kind.scalable() {
        scale_row(ui, k, w, actions);
    }
}

fn scale_row(ui: &mut Ui, k: &KubeState, w: &Workload, actions: &mut Vec<Action>) {
    let mut text = k.scale_to.clone();
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(
            RichText::new(format!("Scale (now {})", w.desired))
                .size(size::SMALL)
                .color(pal(ui).text_2),
        );
        let resp = crate::views::form::input(
            ui,
            "k8s_scale",
            &mut text,
            crate::views::form::InputOpts::new("replicas")
                .width(80.0)
                .mono(),
        );
        changed = resp.changed();
        let n = text.trim().parse::<u32>().ok();
        if crate::views::form::button(ui, "Scale…", n.is_some()).clicked()
            && let Some(n) = n
        {
            actions.push(Act::KubeAsk(WriteKind::Scale(n)).into());
        }
    });
    if changed {
        actions.push(Act::KubeScaleText(text).into());
    }
}

fn pod_table(ui: &mut Ui, cx: &Cx, k: &KubeState, pods: &[&Pod], actions: &mut Vec<Action>) {
    let rows: Vec<Vec<Cell>> = pods
        .iter()
        .map(|p| {
            let status = p.status_text();
            let (ready, total) = p.ready();
            let name = if k.pod.as_deref() == Some(p.name.as_str()) {
                Cell::Tone(p.name.clone(), Tone::Accent)
            } else {
                Cell::Mono(p.name.clone())
            };
            vec![
                name,
                Cell::Tone(status.clone(), status_tone(&status)),
                Cell::Sec(format!("{ready}/{total}")),
                if p.restarts() > 0 {
                    Cell::Tone(format::count(p.restarts() as usize), Tone::Warning)
                } else {
                    Cell::Muted("0".into())
                },
                Cell::Muted(common::age(p.created, cx.now)),
                Cell::Sec(if p.node.is_empty() {
                    "–".into()
                } else {
                    p.node.clone()
                }),
                Cell::Muted(
                    p.last_termination()
                        .map(|(c, t)| format!("{c}: {}", t.text()))
                        .unwrap_or_else(|| "–".into()),
                ),
            ]
        })
        .collect();
    let names: Vec<String> = pods.iter().map(|p| p.name.clone()).collect();
    grid(
        ui,
        "k8s_pods",
        &[
            Col::text("Pod"),
            Col::text("Status"),
            Col::num("Ready"),
            Col::num("Restarts"),
            Col::num("Age"),
            Col::text("Node"),
            Col::text("Last termination"),
        ],
        &rows,
        true,
        |r, resp| {
            if resp.clicked() {
                actions.push(Act::KubePod(Some(names[r].clone())).into());
            }
            resp.context_menu(|ui| {
                if ui.button("Copy pod name").clicked() {
                    actions.push(Action::Copy(names[r].clone()));
                    ui.close();
                }
            });
        },
    );
}

fn pod_detail(ui: &mut Ui, cx: &Cx, k: &KubeState, p: &Pod, actions: &mut Vec<Action>) {
    let status = p.status_text();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        kit::subtitle(ui, &p.name);
        kit::badge(ui, &status, status_tone(&status));
    });
    kit::kv(ui, 120.0, |kv| {
        kv.text("Phase", p.phase.as_str());
        if !p.node.is_empty() {
            kv.text("Node", p.node.as_str());
        }
        if !p.ip.is_empty() {
            kv.text("Pod IP", kit::mono(&p.ip));
        }
        kv.text("Created", common::when(p.created, cx.now));
        if let Some((kind, name)) = &p.owner {
            kv.text("Owner", format!("{kind} {name}"));
        }
        if !p.message.is_empty() {
            kv.text("Message", p.message.as_str());
        }
    });
    let mut form = k.log;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        let f = crate::views::form::checkbox(ui, &mut form.follow, "Follow (stream until stopped)");
        let t = crate::views::form::checkbox(ui, &mut form.timestamps, "Timestamps");
        if f.changed() || t.changed() {
            actions.push(Act::KubeLogForm(form.follow, form.timestamps).into());
        }
    });
    let statuses = p.containers_in_order();
    let names: Vec<String> = if statuses.is_empty() {
        p.container_names.clone()
    } else {
        statuses.iter().map(|c| c.name.clone()).collect()
    };
    for (i, name) in names.iter().enumerate() {
        let s = statuses.get(i);
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(kit::mono(name).color(pal(ui).text_strong));
            if let Some(s) = s {
                let t = s.state.text();
                kit::badge(ui, &t, status_tone(&t));
                ui.label(
                    RichText::new(format!(
                        "{} · {}",
                        if s.ready { "ready" } else { "not ready" },
                        format::plural(s.restarts.max(0) as usize, "restart", "restarts")
                    ))
                    .size(size::SMALL)
                    .color(pal(ui).text_2),
                );
            }
            if IconButton::new(Icon::Terminal, "Logs")
                .compact()
                .show(ui)
                .on_hover_text(format!(
                    "kubectl logs {} -c {name} --tail=500{}",
                    p.name,
                    if k.log.follow { " -f" } else { "" }
                ))
                .clicked()
            {
                actions.push(
                    Act::KubeLogs {
                        pod: p.name.clone(),
                        container: Some(name.clone()),
                        previous: false,
                    }
                    .into(),
                );
            }
            let has_previous = s.is_some_and(|s| s.last.is_some() || s.restarts > 0);
            if has_previous
                && IconButton::new(Icon::Undo, "Previous instance")
                    .compact()
                    .show(ui)
                    .on_hover_text("Logs of the container before its last restart (--previous)")
                    .clicked()
            {
                actions.push(
                    Act::KubeLogs {
                        pod: p.name.clone(),
                        container: Some(name.clone()),
                        previous: true,
                    }
                    .into(),
                );
            }
        });
        if let Some(s) = s {
            if let kube::ContainerState::Waiting { message, .. } = &s.state
                && !message.is_empty()
            {
                kit::secondary(ui, message);
            }
            if let Some(t) = &s.last {
                kit::secondary(
                    ui,
                    &format!(
                        "Last termination: {}{}{}",
                        t.text(),
                        t.finished
                            .map(|f| format!(", {}", format::relative(f, cx.now)))
                            .unwrap_or_default(),
                        if t.message.is_empty() {
                            String::new()
                        } else {
                            format!(" · {}", t.message)
                        }
                    ),
                );
            }
            ui.label(
                RichText::new(&s.image)
                    .monospace()
                    .size(size::MONO - 0.5)
                    .color(pal(ui).text_3),
            );
        }
    }
    ui.add_space(4.0);
    kit::subtitle(ui, "Pod events");
    match k.pod_events.get(&p.name) {
        None => common::note(ui, "Not loaded."),
        Some(e) if e.loading => kit::loading(ui, "Loading events…"),
        Some(e) => match &e.result {
            None => common::note(ui, "Not loaded."),
            Some(Err(err)) => common::error_block(
                ui,
                "kubectl get events failed",
                err,
                None,
                Some(("Try again", Act::KubeRefresh)),
                actions,
            ),
            Some(Ok(ev)) if ev.is_empty() => {
                common::note(ui, "No events (they expire after about an hour).")
            }
            Some(Ok(ev)) => events_table(ui, cx, "k8s_pod_events", ev),
        },
    }
}

fn events_table(ui: &mut Ui, cx: &Cx, id: &str, ev: &[Event]) {
    let rows: Vec<Vec<Cell>> = ev
        .iter()
        .take(40)
        .map(|e| {
            vec![
                Cell::Badges(vec![(
                    e.kind.clone(),
                    if e.warning() {
                        Tone::Warning
                    } else {
                        Tone::Neutral
                    },
                )]),
                Cell::Text(e.reason.clone()),
                Cell::Sec(e.message.clone()),
                Cell::Sec(e.object.clone()),
                Cell::Sec(if e.count > 1 {
                    format!("×{}", format::count(e.count as usize))
                } else {
                    "1".into()
                }),
                Cell::Muted(common::ago(e.last, cx.now)),
            ]
        })
        .collect();
    grid(
        ui,
        id,
        &[
            Col::text("Type"),
            Col::text("Reason"),
            Col::text("Message"),
            Col::text("Object"),
            Col::num("Count"),
            Col::num("Last seen"),
        ],
        &rows,
        false,
        |_, _| {},
    );
    if ev.len() > 40 {
        common::note(ui, &format!("and {} more", ev.len() - 40));
    }
}
