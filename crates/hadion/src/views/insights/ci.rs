//! CI: GitHub Actions workflows and findings.

use egui::Ui;

use hadion_core::insights::ci::{Ci, CiFinding};
use hadion_core::insights::{InsightsReport, Severity};

use super::{
    Cell, Cx, detail_title, dot_color, grid, list_none, list_top, mono, more, muted, num,
    path_label, path_menu, sec, sev_badge, severity_tone, text, why,
};

use crate::state::Action;
use crate::theme::pal;
use crate::views::kit::{self, Col, Leading, Row, Stat, Tone};

pub fn show(
    ui: &mut Ui,
    cx: &Cx,
    rep: &InsightsReport,
    selected: &mut Option<String>,
    actions: &mut Vec<Action>,
) {
    let c = &rep.ci;
    let sel = selected.clone();
    kit::split_keyed(
        ui,
        "ins_ci",
        sel.as_deref().unwrap_or(""),
        actions,
        |ui, actions| finding_list(ui, c, &sel, actions),
        |ui, actions| match sel
            .as_deref()
            .and_then(|k| k.strip_prefix("ci:"))
            .and_then(|n| n.parse::<usize>().ok())
            .and_then(|n| c.findings.get(n))
        {
            Some(f) => finding_detail(ui, cx, f, actions),
            None => overview(ui, cx, c, actions),
        },
    );
}

fn finding_list(ui: &mut Ui, c: &Ci, sel: &Option<String>, actions: &mut Vec<Action>) {
    list_top(
        ui,
        "Findings",
        c.findings.len(),
        sel.is_some(),
        None,
        actions,
    );
    if c.findings.is_empty() {
        list_none(
            ui,
            if c.workflow_files == 0 {
                "No GitHub Actions workflows under .github/workflows."
            } else {
                "No findings: actions are pinned, triggers are safe, permissions are explicit."
            },
        );
        return;
    }
    let p = *pal(ui);
    kit::list(
        ui,
        "ins_ci_list",
        c.findings.len(),
        kit::ROW_H,
        |i| sel.as_deref() == Some(&format!("ci:{i}")),
        |ui, rect, resp, i| {
            let f = &c.findings[i];
            let sub = match &f.job {
                Some(j) => format!("{} › {j}", f.workflow),
                None => f.workflow.clone(),
            };
            let badges = [sev_badge(f.severity)];
            kit::paint_row(
                ui,
                rect,
                &Row::new(&f.title)
                    .leading(Leading::Dot(dot_color(&p, severity_tone(f.severity))))
                    .subtitle(&sub)
                    .badges(&badges),
                sel.as_deref() == Some(&format!("ci:{i}")),
            );
            if resp.clicked() {
                actions.push(Action::SelectInsight(Some(format!("ci:{i}"))));
            }
            resp.context_menu(|ui| {
                if ui.button("Copy workflow path").clicked() {
                    actions.push(Action::Copy(f.workflow.clone()));
                    ui.close();
                }
            });
        },
    );
}

fn finding_detail(ui: &mut Ui, cx: &Cx, f: &CiFinding, actions: &mut Vec<Action>) {
    detail_title(ui, &f.title, &[sev_badge(f.severity)]);
    ui.add_space(4.0);
    kit::kv(ui, 100.0, |kv| {
        kv.row("Workflow", |ui| {
            path_label(ui, cx, &f.workflow, actions);
        });
        if let Some(j) = &f.job {
            kv.text("Job", j.as_str());
        }
        if !f.lines.is_empty() {
            kv.text(
                "Lines",
                f.lines
                    .iter()
                    .map(|l| l.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        kv.text("Rule", kit::mono(f.id));
    });
    ui.add_space(4.0);
    kit::body(ui, &f.detail);
    ui.add_space(4.0);
    why(ui, f.why);
    kit::section(ui, "How to fix", None);
    let (text, code) = fix_for(f);
    kit::body(ui, text);
    if !code.is_empty() {
        kit::code_block(ui, code, actions);
    }
}

fn fix_for(f: &CiFinding) -> (&'static str, &'static str) {
    match f.id {
        "unpinned-action" => (
            "Pin the action to a full commit SHA and keep the version as a comment so Dependabot/Renovate can still update it:",
            "- uses: owner/action@a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0  # v4.1.0",
        ),
        "pull-request-target-checkout" => (
            "Do not check out the pull request head in a pull_request_target workflow. Use `pull_request` (no secrets, read-only token) for anything that runs the PR's code, and keep pull_request_target for labelling/commenting only.",
            "on: pull_request   # instead of pull_request_target",
        ),
        "script-injection" => (
            "Never interpolate event data into a shell script. Pass it through an environment variable and quote it:",
            "- env:\n    TITLE: ${{ github.event.pull_request.title }}\n  run: echo \"$TITLE\"",
        ),
        "no-permissions" => (
            "Give the token the least it needs, at the top of the workflow or per job:",
            "permissions:\n  contents: read",
        ),
        "write-all" => (
            "Replace write-all with the specific scopes the jobs use:",
            "permissions:\n  contents: read\n  pull-requests: write",
        ),
        "secrets-to-third-party" => (
            "A third-party action with a secret can exfiltrate it. Pin the action to a SHA, read its source, or pass a scoped token (fine-grained PAT or GitHub App token) that can do only what the step needs.",
            "",
        ),
        "self-hosted-public" => (
            "Forks can run arbitrary code on your runners. Use ephemeral runners, or restrict fork-triggered workflows to GitHub-hosted runners; require approval for first-time contributors in the repository settings.",
            "",
        ),
        "curl-pipe-sh" => (
            "Download, verify (checksum or signature), then run; or install from a pinned release asset:",
            "curl -fsSLo installer.sh https://example.com/install.sh\necho \"<sha256>  installer.sh\" | sha256sum -c -\nsh installer.sh",
        ),
        "no-timeout" => (
            "Set a timeout so a hung job cannot burn six hours:",
            "jobs:\n  build:\n    timeout-minutes: 30",
        ),
        "deprecated-command" => (
            "Replace the deprecated workflow command / action version:",
            "echo \"name=value\" >> \"$GITHUB_OUTPUT\"   # instead of ::set-output\nuses: actions/checkout@v4             # instead of v1/v2",
        ),
        "continue-on-error" => (
            "A job that cannot fail hides regressions. Remove continue-on-error, or move the flaky step behind a matrix `experimental` flag that is shown as such.",
            "",
        ),
        "persist-credentials" => (
            "In workflows that run untrusted code, keep the token out of the checkout:",
            "- uses: actions/checkout@v4\n  with:\n    persist-credentials: false",
        ),
        "cache-poisoning" => (
            "Caches restored in a privileged workflow can be poisoned from a fork's run that shares the key. Scope cache keys to the base branch or skip the cache under pull_request_target.",
            "",
        ),
        _ => ("See the GitHub Actions security hardening guide.", ""),
    }
}

fn overview(ui: &mut Ui, cx: &Cx, c: &Ci, actions: &mut Vec<Action>) {
    let high = c.count(Severity::Critical) + c.count(Severity::High);
    let medium = c.count(Severity::Medium);
    let fork = c.workflows.iter().filter(|w| w.fork_triggerable).count();
    let wf_tip = format!("{} workflow files", c.workflow_files);
    super::stats(
        ui,
        &[
            Stat::new("High", high.to_string())
                .tone_if(high > 0, Tone::Danger)
                .tip("Findings of high or critical severity"),
            Stat::new("Medium", medium.to_string()).tone_if(medium > 0, Tone::Warning),
            Stat::new("Low", c.count(Severity::Low).to_string()),
            Stat::new("Workflows", c.workflows.len().to_string()).tip(&wf_tip),
            Stat::new("Actions", c.actions.len().to_string()).tip("Distinct uses: targets"),
            Stat::new("Pinned to a SHA", c.pinned_sha.to_string())
                .tip("Actions pinned to a full commit"),
            Stat::new("Unpinned third-party", c.unpinned_third_party.to_string())
                .tone_if(c.unpinned_third_party > 0, Tone::Warning)
                .tip("Third-party actions pinned to a tag or branch"),
            Stat::new("Secrets", c.secrets.len().to_string())
                .tip("Distinct secrets.NAME references"),
            Stat::new("Runner labels", c.runners.len().to_string()).tip("Distinct runs-on labels"),
            Stat::new("Fork-triggerable", fork.to_string())
                .tip("Workflows a fork's pull request can start"),
        ],
    );
    kit::secondary(
        ui,
        "Workflows under .github/workflows, the actions they use, the secrets they read and the runners they need. Select a finding on the left for the fix.",
    );
    if c.workflow_files == 0 && c.other_ci.is_empty() {
        kit::muted(ui, "No CI configuration found.");
    }

    kit::section(ui, "Workflows", Some(c.workflows.len()));
    for w in &c.workflows {
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            kit::subtitle(ui, &w.name);
            if let Some(e) = &w.parse_error {
                kit::tip(kit::badge(ui, "Parse error", Tone::Danger), e);
            }
            if w.fork_triggerable {
                kit::badge(ui, "Fork-triggerable", Tone::Warning);
            }
            if w.findings > 0 {
                kit::badge(
                    ui,
                    &super::count_of(w.findings, "finding", "findings"),
                    Tone::Warning,
                );
            }
        });
        kit::kv(ui, 110.0, |kv| {
            kv.row("File", |ui| {
                path_label(ui, cx, &w.path, actions);
            });
            kv.text("Triggers", w.triggers.join(", "));
            if !w.schedules.is_empty() {
                kv.text("Schedules", kit::mono(w.schedules.join("  |  ")));
            }
            match &w.permissions {
                Some(p) => kv.text("Permissions", p.as_str()),
                None => kv.muted("Permissions", "default"),
            }
            if w.secrets.is_empty() {
                kv.muted("Secrets", "none");
            } else {
                kv.text("Secrets", w.secrets.join(", "));
            }
            if w.concurrency {
                kv.text("Concurrency", "Yes");
            }
        });
        if !w.jobs.is_empty() {
            // Optional columns only when some job has a value.
            type Get = fn(&hadion_core::insights::ci::JobInfo) -> Option<&str>;
            let optional: Vec<(&str, Get)> = [
                ("Environment", (|j| j.environment.as_deref()) as Get),
                ("Container", |j| j.container.as_deref()),
                ("Reusable workflow", |j| j.reusable.as_deref()),
            ]
            .into_iter()
            .filter(|(_, get)| w.jobs.iter().any(|j| get(j).is_some()))
            .collect();
            let mut cols = vec![
                Col::text("Job"),
                Col::text("Runs on"),
                Col::num("Steps"),
                Col::text("Timeout"),
                Col::text("Permissions"),
            ];
            cols.extend(optional.iter().map(|(t, _)| Col::text(t)));
            cols.push(Col::text(""));
            let rows: Vec<Vec<Cell>> = w
                .jobs
                .iter()
                .map(|j| {
                    let mut row = vec![
                        text(&j.name),
                        if j.runs_on.is_empty() {
                            muted("?")
                        } else {
                            sec(j.runs_on.join(", "))
                        },
                        num(j.steps as usize),
                        match j.timeout_minutes {
                            Some(t) => sec(format!("{t} min")),
                            None => muted("none"),
                        },
                        j.permissions
                            .as_deref()
                            .map(sec)
                            .unwrap_or_else(|| muted("inherited")),
                    ];
                    row.extend(
                        optional
                            .iter()
                            .map(|(_, get)| get(j).map(sec).unwrap_or_else(|| muted("–"))),
                    );
                    row.push(super::badges(&[
                        (
                            if j.self_hosted { "Self-hosted" } else { "" },
                            Tone::Warning,
                        ),
                        (
                            if j.continue_on_error {
                                "Continue on error"
                            } else {
                                ""
                            },
                            Tone::Neutral,
                        ),
                    ]));
                    row
                })
                .collect();
            ui.add_space(2.0);
            grid(
                ui,
                &format!("ins_ci_jobs_{}", w.path),
                &cols,
                &rows,
                false,
                |_, _| {},
            );
        }
    }

    kit::section(ui, "Actions used", Some(c.actions.len()));
    if !c.actions.is_empty() {
        let rows: Vec<Vec<Cell>> = c
            .actions
            .iter()
            .take(80)
            .map(|a| {
                let (t, tone) = if a.local {
                    ("Local", Tone::Neutral)
                } else if a.docker {
                    ("Docker", Tone::Neutral)
                } else if a.pinned_sha {
                    ("SHA", Tone::Success)
                } else if a.first_party {
                    ("Tag, GitHub", Tone::Neutral)
                } else {
                    ("Tag, third-party", Tone::Warning)
                };
                vec![
                    mono(&a.uses),
                    super::badges(&[(t, tone)]),
                    num(a.count as usize),
                    sec(a.workflows.join(", ")),
                ]
            })
            .collect();
        grid(
            ui,
            "ins_ci_actions",
            &[
                Col::text("Action"),
                Col::text("Pinned to"),
                Col::num("Uses"),
                Col::text("Workflows"),
            ],
            &rows,
            false,
            |r, resp| {
                let a = &c.actions[r];
                resp.context_menu(|ui| {
                    if ui.button("Copy").clicked() {
                        actions.push(Action::Copy(a.uses.clone()));
                        ui.close();
                    }
                });
            },
        );
        more(ui, c.actions.len(), 80);
    }
    if !c.secrets.is_empty() {
        kit::section(ui, "Secrets referenced", Some(c.secrets.len()));
        let rows: Vec<Vec<Cell>> = c
            .secrets
            .iter()
            .map(|(s, ws)| vec![mono(s), sec(ws.join(", "))])
            .collect();
        grid(
            ui,
            "ins_ci_secrets",
            &[Col::text("Secret"), Col::text("Workflows")],
            &rows,
            false,
            |_, _| {},
        );
    }
    if !c.runners.is_empty() {
        kit::section(ui, "Runners", Some(c.runners.len()));
        let rows: Vec<Vec<Cell>> = c
            .runners
            .iter()
            .map(|(r, n)| vec![mono(r), num(*n as usize)])
            .collect();
        grid(
            ui,
            "ins_ci_runners",
            &[Col::text("Label"), Col::num("Jobs")],
            &rows,
            false,
            |_, _| {},
        );
    }
    kit::section(ui, "Other CI and dependency bots", None);
    if c.other_ci.is_empty() && c.dependabot.is_none() && !c.renovate {
        kit::muted(
            ui,
            "No other CI system configuration, no Dependabot or Renovate configuration.",
        );
    } else {
        if !c.other_ci.is_empty() {
            let rows: Vec<Vec<Cell>> = c
                .other_ci
                .iter()
                .map(|(p, name)| vec![text(name), mono(p)])
                .collect();
            grid(
                ui,
                "ins_ci_other",
                &[Col::text("System"), Col::text("File")],
                &rows,
                false,
                |r, resp| path_menu(resp, cx, &c.other_ci[r].0, actions),
            );
        }
        kit::kv(ui, 110.0, |kv| {
            if let Some(eco) = &c.dependabot {
                kv.text(
                    "Dependabot",
                    if eco.is_empty() {
                        "configured".to_owned()
                    } else {
                        eco.join(", ")
                    },
                );
            }
            if c.renovate {
                kv.text("Renovate", "configured");
            }
        });
    }
    ui.add_space(12.0);
}
