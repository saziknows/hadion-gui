//! The Environments tab: what is deployed where, differences between
//! consecutive environments, pending approvals, promote and roll back,
//! and each environment's deployment history.

use egui::Ui;

use gitvis_core::devops::ship::deploy::{self, Approval, Deployment};

use super::{Act, State, count, ui as sui};
use crate::devops::pages::explain;
use crate::format;
use crate::state::{Action, RepoState, Settings};
use crate::theme::{Tone, pal};
use crate::views::form;
use crate::views::icons::{Icon, IconButton};
use crate::views::insights::{self as ins, Cell};
use crate::views::kit::{self, Col, Leading, Row};

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

/// The roll-back of the selected environment: back to the previous
/// successful deployment's commit, by re-running its run or by a new
/// deployment.
pub(super) fn rollback_action(st: &State, rerun: bool) -> Option<Act> {
    let env = st.selected_env.clone()?;
    let list = st.deployments_of(&env);
    let cur = deploy::current(&list)?;
    let prev = deploy::previous_success(&list, cur)?;
    let now = format::now_secs();
    if rerun {
        let run = prev.run_id()?;
        return Some(Act::Rerun {
            run,
            title: format!("Roll {env} back by re-running run {run}?"),
            body: format!(
                "Re-runs the workflow run that deployed {} to {env} {}.\nA re-run checks out the run's original commit, so {env} goes back from {} to {}.\nApprovals and wait timers of the environment apply again.",
                short(&prev.sha),
                format::relative(prev.created, now),
                short(&cur.sha),
                short(&prev.sha)
            ),
        });
    }
    Some(Act::Deploy {
        sha: prev.sha.clone(),
        env: env.clone(),
        description: format!(
            "Roll back {env} from {} to {} (gitvis)",
            short(&cur.sha),
            short(&prev.sha)
        ),
        title: format!("Roll {env} back to {}?", short(&prev.sha)),
    })
}

/// Promote the selected environment's commit to `to`.
pub(super) fn promote_action(st: &State, to: &str, rerun: bool) -> Option<Act> {
    let from = st.selected_env.clone()?;
    let to = if to.trim().is_empty() {
        st.promote_to.clone()
    } else {
        to.trim().to_owned()
    };
    if to.is_empty() || to == from {
        return None;
    }
    let list = st.deployments_of(&from);
    let cur = deploy::current(&list)?;
    if rerun {
        let run = cur.run_id()?;
        return Some(Act::Rerun {
            run,
            title: format!(
                "Promote {} to {to} by re-running run {run}?",
                short(&cur.sha)
            ),
            body: format!(
                "Re-runs the workflow run that deployed {} to {from}.\nIt reaches {to} only when that workflow deploys there after {from} (one pipeline with a job per environment, {to} usually behind an approval).",
                short(&cur.sha)
            ),
        });
    }
    Some(Act::Deploy {
        sha: cur.sha.clone(),
        env: to.clone(),
        description: format!("Promote {} from {from} to {to} (gitvis)", short(&cur.sha)),
        title: format!("Promote {} from {from} to {to}?", short(&cur.sha)),
    })
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, _settings: &Settings, actions: &mut Vec<Action>) {
    let now = format::now_secs();
    let st = &repo.devops.ship;
    let subtitle = match st.envs.ok() {
        Some(d) => {
            let mut s = format!(
                "{} · newest {} of {} deployments",
                count(d.environments.len(), "environment", "environments"),
                format::count(d.deployments.len()),
                format::count(d.total_deployments as usize)
            );
            if d.source == deploy::Source::Rest {
                s.push_str(" · REST");
            }
            let f = sui::fetched(&st.envs, now);
            if !f.is_empty() {
                s.push_str(&format!(" · {f}"));
            }
            s
        }
        None => "What is deployed where, pending approvals, promote and roll back".to_owned(),
    };
    let can = repo.github_remote.is_some() && st.gh_ok != Some(false);
    crate::devops::pages::layout::page_header(ui, "Environments", &subtitle, |ui| {
        if can
            && IconButton::new(Icon::Rerun, "Refresh")
                .compact()
                .enabled(!st.envs.loading)
                .show(ui)
                .on_hover_text("One GraphQL query for environments and deployments, plus the runs waiting for review")
                .clicked()
        {
            actions.push(Act::RefreshEnvs.into());
        }
        if st.envs.loading || st.approvals.loading || st.env_local_loading {
            ui.add(egui::Spinner::new().size(14.0));
        }
    });
    if !sui::gate(ui, repo, actions) {
        return;
    }
    let st = &repo.devops.ship;
    if st.envs.ok().is_none() {
        crate::devops::pages::layout::state_page(ui, "ship_envs_state", |ui| {
            sui::loaded(
                ui,
                &st.envs,
                "environments",
                "Loading environments and deployments",
                Act::RefreshEnvs,
                actions,
            );
        });
        return;
    }
    let order = st.env_order();
    let only_side = st
        .envs
        .ok()
        .is_some_and(|d| d.deployments.is_empty() && order.iter().all(|e| deploy::is_side(e)));
    if order.is_empty() || only_side {
        let st = &mut repo.devops.ship;
        let urgent =
            st.approvals.err().is_some() || st.approvals.ok().is_some_and(|l| !l.is_empty());
        crate::devops::pages::layout::state_page(ui, "ship_envs_empty", |ui| {
            if urgent {
                approvals_section(ui, st, now, actions);
            }
            if kit::empty_state(
                ui,
                Icon::Cloud,
                "No deployment environments",
                &if order.is_empty() {
                    "GitHub creates an environment the first time a workflow job names one (`environment: production`) or a deployment is created for it.\nThis repository has none, so nothing is deployed anywhere yet.\nReleases and tags still work as a DORA source.".to_owned()
                } else {
                    format!(
                        "The only environments are GitHub's own ({}) and nothing was deployed to them.\nA workflow job that names an environment (`environment: production`) creates one with deployments.\nReleases and tags still work as a DORA source.",
                        order.join(", ")
                    )
                },
                Some((Icon::Gauge, "DORA metrics from releases")),
            ) {
                actions.push(Act::DoraSource(super::DoraSource::Releases).into());
                actions.push(Action::DevOps(crate::devops::DevOpsAction::SetShipTab(
                    crate::devops::ShipTab::Dora,
                )));
            }
        });
        return;
    }
    // Rows for the list pane.
    struct EnvRow {
        name: String,
        subtitle: String,
        meta: String,
        badges: Vec<(String, Tone)>,
        dimmed: bool,
    }
    let rows: Vec<EnvRow> = order
        .iter()
        .map(|name| {
            let list = st.deployments_of(name);
            let cur = deploy::current(&list);
            let newest = list.first();
            let mut badges = Vec::new();
            if let Some(n) = newest {
                let s = n.effective();
                if cur.is_none_or(|c| c.id != n.id) || !s.succeeded() {
                    badges.push(sui::state_badge(s));
                }
            }
            if deploy::is_production(name) {
                badges.push(("Production".to_owned(), Tone::Purple));
            }
            let subtitle = match cur {
                Some(c) => format!(
                    "{} · {}{}",
                    short(&c.sha),
                    format::relative(c.created, now),
                    if c.creator.is_empty() {
                        String::new()
                    } else {
                        format!(" · {}", c.creator)
                    }
                ),
                None if list.is_empty() => "never deployed (in the data read)".to_owned(),
                None => "no successful deployment".to_owned(),
            };
            let behind = st.env_local.get(name).and_then(|l| l.behind);
            EnvRow {
                name: name.clone(),
                subtitle,
                meta: match behind {
                    Some(0) => "up to date".to_owned(),
                    Some(n) => format!("{} behind", format::count(n as usize)),
                    None => String::new(),
                },
                badges,
                dimmed: cur.is_none(),
            }
        })
        .collect();
    let selected = st.selected_env.clone();
    let key = selected.clone().unwrap_or_default();
    crate::devops::pages::layout::split_keyed(
        ui,
        "ship_envs",
        &key,
        actions,
        |ui, actions| {
            kit::list_header(ui, "Pipeline", Some(rows.len()), |ui| {
                if selected.is_some() && kit::link(ui, "Overview").clicked() {
                    actions.push(Act::SelectEnv(None).into());
                }
            });
            ui.add_space(6.0);
            kit::list(
                ui,
                "ship_env_list",
                rows.len(),
                kit::ROW_H,
                |i| selected.as_deref() == Some(rows[i].name.as_str()),
                |ui, rect, resp, i| {
                    let r = &rows[i];
                    let p = *pal(ui);
                    let fitted =
                        crate::devops::pages::layout::fit_badges(ui, rect.width(), &r.badges);
                    let row = Row::new(&r.name)
                        .leading(Leading::Icon(
                            Icon::Cloud,
                            if r.dimmed { p.text_3 } else { p.text_2 },
                        ))
                        .subtitle(&r.subtitle)
                        .meta(&r.meta)
                        .badges(&fitted)
                        .dimmed(r.dimmed);
                    kit::paint_row(ui, rect, &row, selected.as_deref() == Some(r.name.as_str()));
                    if resp.clicked() {
                        actions.push(Act::SelectEnv(Some(r.name.clone())).into());
                    }
                },
            );
        },
        |ui, actions| match selected.as_deref() {
            Some(env) => env_detail(ui, repo, env, now, actions),
            None => overview(ui, repo, now, actions),
        },
    );
}

fn overview(ui: &mut Ui, repo: &mut RepoState, now: i64, actions: &mut Vec<Action>) {
    // Approvals that need someone come first; a quiet "none" goes last.
    let urgent = {
        let a = &repo.devops.ship.approvals;
        a.err().is_some() || a.ok().is_some_and(|l| !l.is_empty())
    };
    if urgent {
        approvals_section(ui, &mut repo.devops.ship, now, actions);
        ui.add_space(12.0);
    }
    where_section(ui, repo, now, actions);
    if !urgent {
        approvals_section(ui, &mut repo.devops.ship, now, actions);
    }
}

fn where_section(ui: &mut Ui, repo: &RepoState, now: i64, actions: &mut Vec<Action>) {
    let st = &repo.devops.ship;
    let Some(data) = st.envs.ok() else { return };
    let order = st.env_order();
    kit::title(ui, "What is deployed where");
    let default = st
        .default_branch
        .clone()
        .unwrap_or_else(|| "the default branch".to_owned());
    kit::secondary(
        ui,
        "The commit each environment runs, and how far it is from the default branch and from the next environment.",
    );
    explain::details(
        ui,
        "ship_envs_where_terms",
        "What the columns mean",
        false,
        |ui| {
            explain::terms(
                ui,
                130.0,
                &[
                    (
                        "Commit",
                        "The newest active deployment, else the newest successful one.",
                    ),
                    (
                        "Behind",
                        &format!("Commits on {default} that are not deployed there."),
                    ),
                    (
                        "Ahead of next",
                        "Commits the environment has that the next one in the pipeline does not.",
                    ),
                ],
            );
        },
    );
    let cols = [
        Col::text("Environment"),
        Col::text("Commit"),
        Col::text("Ref"),
        Col::text("Deployed"),
        Col::text("By"),
        Col::num("Behind"),
        Col::text("Ahead of next"),
    ];
    let mut names = Vec::new();
    let rows: Vec<Vec<Cell>> = order
        .iter()
        .map(|name| {
            names.push(name.clone());
            let list = st.deployments_of(name);
            let cur = deploy::current(&list);
            let local = st.env_local.get(name);
            let (commit, rf, when, by) = match cur {
                Some(c) => (
                    if sui::in_graph(repo, &c.sha) {
                        Cell::MonoLink(short(&c.sha).to_owned())
                    } else {
                        Cell::Mono(short(&c.sha).to_owned())
                    },
                    ins::sec(c.ref_name.clone()),
                    ins::muted(format::relative(c.created, now)),
                    ins::sec(c.creator.clone()),
                ),
                None => (
                    ins::muted("none"),
                    ins::muted(""),
                    ins::muted(""),
                    ins::muted(""),
                ),
            };
            let behind = match local {
                Some(l) if !l.present => ins::muted("not local"),
                Some(l) => match l.behind {
                    Some(0) => Cell::Tone("0".into(), Tone::Success),
                    Some(n) => Cell::Tone(format::count(n as usize), Tone::Warning),
                    None => ins::muted("?"),
                },
                None => ins::muted(""),
            };
            let ahead = match local.and_then(|l| l.ahead_of_next.as_ref()) {
                Some((next, n, _)) if *n > 0 => ins::text(format!(
                    "{} not in {next}",
                    count(*n as usize, "commit", "commits")
                )),
                Some((next, _, _)) => ins::muted(format!("same as {next}")),
                None => ins::muted(""),
            };
            vec![ins::text(name.clone()), commit, rf, when, by, behind, ahead]
        })
        .collect();
    crate::devops::pages::layout::wide(ui, "ship_env_where", 760.0, |ui| {
        ins::grid(ui, "ship_env_where", &cols, &rows, true, |i, resp| {
            if resp.clicked() {
                actions.push(Act::SelectEnv(Some(names[i].clone())).into());
            }
        });
    });
    if st.env_local_loading {
        kit::loading(ui, "Comparing deployed commits with local history…");
    }
    let missing: Vec<&String> = order
        .iter()
        .filter(|n| st.env_local.get(*n).is_some_and(|l| !l.present))
        .collect();
    if !missing.is_empty() {
        kit::callout(
            ui,
            Tone::Warning,
            "Some deployed commits are not in the local history",
            &format!(
                "Not in this clone: the commits of {}.\nBehind and ahead are unknown there until you fetch (git fetch --all).",
                missing
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }

    // Pipeline order.
    kit::section_with(ui, "Pipeline order", Some(order.len()), |ui| {
        if !st.order.is_empty()
            && IconButton::new(Icon::Undo, "Reset")
                .compact()
                .show(ui)
                .on_hover_text("Back to the order guessed from the names")
                .clicked()
        {
            actions.push(Act::ResetOrder.into());
        }
    });
    kit::secondary(
        ui,
        "Changes flow from top to bottom; each environment is compared with the next one.",
    );
    kit::secondary(
        ui,
        "The order starts as a guess from the names (development, test, staging, production); the arrows change it.",
    );
    for (i, name) in order.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if IconButton::icon(Icon::ChevronUp)
                .compact()
                .enabled(i > 0)
                .show(ui)
                .clicked()
            {
                actions.push(Act::MoveEnv(name.clone(), -1).into());
            }
            if IconButton::icon(Icon::ChevronDown)
                .compact()
                .enabled(i + 1 < order.len())
                .show(ui)
                .clicked()
            {
                actions.push(Act::MoveEnv(name.clone(), 1).into());
            }
            kit::body(ui, &format!("{}. {name}", i + 1));
        });
    }
    if !data.notes.is_empty() {
        kit::section(ui, "About this data", None);
        for n in &data.notes {
            kit::secondary(ui, n);
        }
    }
}

fn approvals_section(ui: &mut Ui, st: &mut State, now: i64, actions: &mut Vec<Action>) {
    if st.approvals.idle() {
        return;
    }
    let count_n = st.approvals.ok().map(|a| a.len());
    let loading = st.approvals.loading;
    kit::section_with(ui, "Waiting for review", count_n, |ui| {
        if IconButton::new(Icon::Rerun, "Check again")
            .compact()
            .enabled(!loading)
            .show(ui)
            .on_hover_text("GET actions/runs?status=waiting, then each run's pending deployments")
            .clicked()
        {
            actions.push(Act::RefreshApprovals.into());
        }
    });
    if let Some(e) = st.approvals.err() {
        sui::inline_error(ui, "pending approvals", e, Act::RefreshApprovals, actions);
        return;
    }
    let Some(list) = st.approvals.ok() else {
        kit::loading(ui, "Looking for runs waiting for a deployment review…");
        return;
    };
    if list.is_empty() {
        kit::secondary(ui, "No run is waiting for a deployment review.");
        kit::secondary(
            ui,
            "Runs wait here when an environment requires reviewers or has a wait timer.",
        );
        return;
    }
    let any_mine = list.iter().any(|a| !a.approvable().is_empty());
    for a in list {
        approval_card(ui, a, now, actions);
        ui.add_space(6.0);
    }
    if any_mine {
        form::field(
            ui,
            "Review comment",
            Some("Sent with the approval or rejection; empty sends \"Reviewed in gitvis\"."),
            |ui| {
                form::text(
                    ui,
                    "ship_review_comment",
                    &mut st.approval_comment,
                    "Looks good",
                )
            },
        );
    }
}

fn approval_card(ui: &mut Ui, a: &Approval, now: i64, actions: &mut Vec<Action>) {
    let run = &a.run;
    form::card(ui, Tone::Warning, |ui| {
        ins::detail_title(
            ui,
            &format!("{} #{}", run.name, run.run_number),
            &[("Waiting".to_owned(), Tone::Warning)],
        );
        kit::secondary(ui, &run.display_title);
        kit::secondary(
            ui,
            &format!(
                "{} on {}, started {} by {}",
                short(&run.head_sha),
                run.head_branch.clone().unwrap_or_default(),
                run.created_at
                    .as_deref()
                    .and_then(gitvis_core::github::parse_time)
                    .map(|t| format::relative(t, now))
                    .unwrap_or_default(),
                run.actor.as_ref().map(|x| x.login.as_str()).unwrap_or("?")
            ),
        );
        // One block per environment the run waits for.
        let rows: Vec<(String, String)> = a
            .pending
            .iter()
            .map(|p| {
                let reviewers: Vec<String> = p
                    .reviewers
                    .iter()
                    .map(|r| {
                        if r.kind == "Team" {
                            format!("{} (team)", r.name)
                        } else {
                            r.name.clone()
                        }
                    })
                    .collect();
                let mut lines = vec![if reviewers.is_empty() {
                    "No reviewers required.".to_owned()
                } else {
                    format!("Reviewers: {}", reviewers.join(", "))
                }];
                if p.wait_timer > 0 {
                    let mut t = format!("Wait timer: {} min", p.wait_timer);
                    if let Some(s) = p.wait_timer_started {
                        let left = s + p.wait_timer as i64 * 60 - now;
                        if left > 0 {
                            t.push_str(&format!(" ({} left)", sui::dur(left)));
                        }
                    }
                    lines.push(t);
                }
                lines.push(
                    if p.current_user_can_approve {
                        "You can approve."
                    } else {
                        "You cannot approve."
                    }
                    .to_owned(),
                );
                (p.environment.name.clone(), lines.join("\n"))
            })
            .collect();
        let refs: Vec<(&str, &str)> = rows.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        ui.add_space(2.0);
        explain::terms(ui, 160.0, &refs);
        let mine: Vec<(u64, String)> = a
            .approvable()
            .iter()
            .map(|p| (p.environment.id, p.environment.name.clone()))
            .collect();
        ins::button_row(ui, |ui| {
            if !mine.is_empty() {
                if form::primary(ui, "Approve…", true).clicked() {
                    actions.push(
                        Act::Review {
                            run: run.id,
                            envs: mine.clone(),
                            approve: true,
                        }
                        .into(),
                    );
                }
                if form::danger(ui, "Reject…", true).clicked() {
                    actions.push(
                        Act::Review {
                            run: run.id,
                            envs: mine.clone(),
                            approve: false,
                        }
                        .into(),
                    );
                }
            }
            if IconButton::new(Icon::External, "Open run")
                .compact()
                .show(ui)
                .clicked()
            {
                actions.push(Action::OpenUrl(run.html_url.clone()));
            }
        });
    });
}

fn env_detail(ui: &mut Ui, repo: &mut RepoState, env: &str, now: i64, actions: &mut Vec<Action>) {
    let st = &repo.devops.ship;
    let Some(data) = st.envs.ok() else { return };
    let info = data.env(env).cloned();
    let list: Vec<Deployment> = st.deployments_of(env).into_iter().cloned().collect();
    let refs: Vec<&Deployment> = list.iter().collect();
    let cur = deploy::current(&refs).cloned();
    let newest = list.first().cloned();
    let mut badges = Vec::new();
    if let Some(c) = &cur {
        badges.push(sui::state_badge(c.effective()));
    }
    if deploy::is_production(env) {
        badges.push(("Production".to_owned(), Tone::Purple));
    }
    ins::detail_title(ui, env, &badges);
    let (owner, name) = repo.github_remote.clone().unwrap_or_default();
    ins::button_row(ui, |ui| {
        if IconButton::new(Icon::External, "Open on GitHub")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::OpenUrl(format!(
                "https://github.com/{owner}/{name}/deployments/{}",
                gitvis_core::devops::gh::encode(env)
            )));
        }
        if let Some(c) = &cur
            && let Some(url) = c
                .status
                .as_ref()
                .map(|s| s.environment_url.clone())
                .filter(|u| !u.is_empty())
            && IconButton::new(Icon::Link, "Open the deployed app")
                .compact()
                .show(ui)
                .clicked()
        {
            actions.push(Action::OpenUrl(url));
        }
        if IconButton::new(Icon::Rerun, "Refresh")
            .compact()
            .enabled(!st.envs.loading)
            .show(ui)
            .clicked()
        {
            actions.push(Act::RefreshEnvs.into());
        }
    });
    ui.add_space(4.0);
    let local = st.env_local.get(env).cloned();
    match &cur {
        None => {
            kit::callout(
                ui,
                Tone::Neutral,
                "Nothing deployed here yet",
                &match &newest {
                    Some(n) => format!(
                        "The newest deployment ({}, {}) did not succeed: {}.",
                        short(&n.sha),
                        format::relative(n.created, now),
                        n.effective().label().to_lowercase()
                    ),
                    None => "No deployment to this environment was found.".to_owned(),
                },
            );
        }
        Some(c) => {
            kit::kv(ui, 130.0, |kv| {
                kv.row("Commit", |ui| {
                    ui.horizontal_wrapped(|ui| {
                        sui::commit_link(ui, repo, &c.sha, actions);
                        if !c.commit_summary.is_empty() {
                            ui.label(&c.commit_summary);
                        }
                    });
                });
                if !c.ref_name.is_empty() {
                    kv.text("Ref", kit::mono(&c.ref_name));
                }
                kv.text("Deployed", sui::when(c.created, now));
                if !c.creator.is_empty() {
                    kv.text("By", c.creator.as_str());
                }
                if let Some(s) = &c.status {
                    let mut t = s.state.label().to_owned();
                    if let Some(at) = s.created {
                        t.push_str(&format!(" · {}", format::relative(at, now)));
                    }
                    if !s.description.is_empty() {
                        t.push_str(&format!(" · {}", s.description));
                    }
                    kv.text("Status", t);
                }
                if let Some(d) = c.duration_secs() {
                    kv.text("Took", sui::dur(d));
                }
                if let Some(run) = c.run_id() {
                    kv.row("Run", |ui| {
                        if kit::link(ui, &format!("Actions run {run}")).clicked() {
                            actions.push(Action::OpenRun { run, job: None });
                        }
                    });
                }
                match &local {
                    Some(l) if !l.present => kv.muted(
                        "Behind default",
                        "unknown: the commit is not in the local history (fetch)",
                    ),
                    Some(l) => match l.behind {
                        Some(0) => kv.text(
                            "Behind default",
                            format!(
                                "up to date with {}",
                                st.default_branch.as_deref().unwrap_or("the default branch")
                            ),
                        ),
                        Some(n) => kv.text(
                            "Behind default",
                            format!(
                                "{} on {} not deployed here",
                                count(n as usize, "commit", "commits"),
                                st.default_branch.as_deref().unwrap_or("the default branch")
                            ),
                        ),
                        None => kv.muted("Behind default", "no remote default branch found"),
                    },
                    None => kv.muted("Behind default", "comparing…"),
                }
                if let Some(i) = &info
                    && !i.protection.is_empty()
                {
                    kv.text("Protection", i.protection.join("\n"));
                }
            });
        }
    }

    // Ahead of the next environment.
    if let Some((next, n, commits)) = local.as_ref().and_then(|l| l.ahead_of_next.clone()) {
        kit::section(ui, &format!("Not in {next} yet"), Some(n as usize));
        if n == 0 {
            kit::muted(ui, &format!("{next} runs everything {env} has."));
        } else {
            let cols = [
                Col::text("Commit"),
                Col::text("Summary"),
                Col::text("Author"),
                Col::text("When"),
            ];
            let rows: Vec<Vec<Cell>> = commits
                .iter()
                .map(|c| {
                    vec![
                        if sui::in_graph(repo, &c.sha) {
                            Cell::MonoLink(short(&c.sha).to_owned())
                        } else {
                            Cell::Mono(short(&c.sha).to_owned())
                        },
                        ins::text(c.subject.clone()),
                        ins::sec(c.author.clone()),
                        ins::muted(format::relative(c.commit_time, now)),
                    ]
                })
                .collect();
            ins::grid(ui, "ship_env_ahead", &cols, &rows, true, |i, resp| {
                if resp.clicked() {
                    actions.push(Act::JumpTo(commits[i].sha.clone()).into());
                }
            });
            ins::more(ui, n as usize, commits.len());
        }
    }

    // Promote and roll back.
    if cur.is_some() {
        promote_section(ui, repo, env, &refs, now, actions);
    }

    // History.
    let st = &repo.devops.ship;
    let h = st.history.get(env);
    kit::section_with(ui, "History", Some(list.len()), |ui| {
        if h.is_some_and(|h| h.loading) {
            ui.add(egui::Spinner::new().size(12.0));
        }
    });
    if let Some(e) = h.and_then(|h| h.err()) {
        sui::inline_error(
            ui,
            "this environment's history",
            e,
            Act::RefreshEnvs,
            actions,
        );
    }
    if list.is_empty() {
        kit::muted(ui, "No deployment to this environment was found.");
        return;
    }
    let cols = [
        Col::text("Commit"),
        Col::text("Ref"),
        Col::text("State"),
        Col::text("Creator"),
        Col::text("Created"),
        Col::num("Took"),
        Col::text("Description"),
    ];
    let shown: Vec<&Deployment> = list.iter().take(60).collect();
    let rows: Vec<Vec<Cell>> = shown
        .iter()
        .map(|d| {
            let s = d.effective();
            vec![
                if sui::in_graph(repo, &d.sha) {
                    Cell::MonoLink(short(&d.sha).to_owned())
                } else {
                    Cell::Mono(short(&d.sha).to_owned())
                },
                ins::sec(d.ref_name.clone()),
                Cell::Badges(vec![sui::state_badge(s)]),
                ins::sec(d.creator.clone()),
                ins::muted(sui::when(d.created, now)),
                d.duration_secs()
                    .map(|x| ins::sec(sui::dur(x)))
                    .unwrap_or_else(|| ins::muted("")),
                ins::muted(d.description.clone().chars().take(120).collect::<String>()),
            ]
        })
        .collect();
    crate::devops::pages::layout::wide(ui, "ship_env_history", 860.0, |ui| {
        ins::grid(ui, "ship_env_history", &cols, &rows, true, |i, resp| {
            let d = shown[i];
            if resp.clicked() {
                actions.push(Act::JumpTo(d.sha.clone()).into());
            }
            resp.context_menu(|ui| {
                if ui.button("Copy commit id").clicked() {
                    actions.push(Action::Copy(d.sha.clone()));
                    ui.close();
                }
                if let Some(run) = d.run_id()
                    && ui.button(format!("Show run {run}")).clicked()
                {
                    actions.push(Action::OpenRun { run, job: None });
                    ui.close();
                }
                if d.effective().succeeded() && ui.button("Redeploy this commit here…").clicked()
                {
                    actions.push(
                        Act::Deploy {
                            sha: d.sha.clone(),
                            env: d.environment.clone(),
                            description: format!("Redeploy {} (gitvis)", short(&d.sha)),
                            title: format!("Deploy {} to {} again?", short(&d.sha), d.environment),
                        }
                        .into(),
                    );
                    ui.close();
                }
            });
        });
    });
    ins::more(ui, list.len(), shown.len());
    kit::secondary(
        ui,
        "Took: from the deployment's creation to its successful status.",
    );
    kit::secondary(
        ui,
        "Click a row to show the commit in the graph; right-click for more.",
    );
}

fn promote_section(
    ui: &mut Ui,
    repo: &mut RepoState,
    env: &str,
    list: &[&Deployment],
    now: i64,
    actions: &mut Vec<Action>,
) {
    let Some(cur) = deploy::current(list) else {
        return;
    };
    let prev = deploy::previous_success(list, cur);
    let order = repo.devops.ship.env_order();
    let st = &mut repo.devops.ship;
    kit::section(ui, "Promote and roll back", None);
    kit::secondary(
        ui,
        "Two ways to move a commit; which one fits depends on how the pipeline is built. Both ask first and show the exact request.",
    );
    explain::details(ui, "ship_envs_two_ways", "Which one to use", false, |ui| {
        explain::terms(
            ui,
            170.0,
            &[
                (
                    "Create a deployment",
                    "Sends GitHub's deployment event.\nRight when a workflow listens to `on: deployment`, or an external deployer watches the API.",
                ),
                (
                    "Re-run",
                    "Repeats an Actions run on its original commit.\nRight when one workflow deploys every environment.",
                ),
            ],
        );
    });
    form::card(ui, Tone::Neutral, |ui| {
        kit::subtitle(ui, &format!("Promote {} from {env}", short(&cur.sha)));
        let others: Vec<(String, String)> = order
            .iter()
            .filter(|e| *e != env && !deploy::is_side(e))
            .map(|e| (e.clone(), e.clone()))
            .collect();
        let downstream = order
            .iter()
            .skip_while(|e| *e != env)
            .skip(1)
            .any(|e| !deploy::is_side(e));
        if others.is_empty() {
            kit::muted(ui, "There is no other environment to promote to.");
        } else {
            if !downstream {
                kit::secondary(
                    ui,
                    &format!(
                        "{env} is the last stage of the pipeline order; pick where to copy its commit (the order is on the overview)."
                    ),
                );
            }
            if !others.iter().any(|(e, _)| *e == st.promote_to) {
                st.promote_to = others[0].0.clone();
            }
            form::row(ui, 28.0, "To", |ui| {
                form::select(ui, "ship_promote_to", &mut st.promote_to, &others, 200.0);
            });
            let to = st.promote_to.clone();
            ins::button_row(ui, |ui| {
                if form::primary(ui, &format!("Create deployment to {to}…"), true).clicked()
                    && let Some(a) = promote_action(st, &to, false)
                {
                    actions.push(a.into());
                }
                let rerun = cur.run_id();
                let label = match rerun {
                    Some(r) => format!("Re-run run {r}…"),
                    None => "Re-run (no run known)".to_owned(),
                };
                if form::button(ui, &label, rerun.is_some()).clicked()
                    && let Some(a) = promote_action(st, &to, true)
                {
                    actions.push(a.into());
                }
            });
        }
        ui.add_space(8.0);
        kit::subtitle(ui, &format!("Roll {env} back"));
        match prev {
            None => {
                kit::secondary(
                    ui,
                    "No earlier successful deployment of a different commit in the history read.",
                );
            }
            Some(p) => {
                kit::secondary(
                    ui,
                    &format!(
                        "Back to {} (deployed {} by {}), the previous successful deployment.",
                        short(&p.sha),
                        format::relative(p.created, now),
                        if p.creator.is_empty() {
                            "?"
                        } else {
                            &p.creator
                        }
                    ),
                );
                ins::button_row(ui, |ui| {
                    if form::danger(ui, &format!("Deploy {} again…", short(&p.sha)), true).clicked()
                        && let Some(a) = rollback_action(st, false)
                    {
                        actions.push(a.into());
                    }
                    let rerun = p.run_id();
                    let label = match rerun {
                        Some(r) => format!("Re-run run {r}…"),
                        None => "Re-run (no run known)".to_owned(),
                    };
                    if form::button(ui, &label, rerun.is_some()).clicked()
                        && let Some(a) = rollback_action(st, true)
                    {
                        actions.push(a.into());
                    }
                });
            }
        }
    });
}
