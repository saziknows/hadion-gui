//! The Govern controller: what each section reads when it is shown, the
//! confirmations in front of every change, and the replies.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gitvis_core::devops::gh::{Gh, Request};
use gitvis_core::devops::govern::alerts;
use gitvis_core::devops::govern::codeowners;
use gitvis_core::devops::govern::config::{self, Scope};
use gitvis_core::devops::govern::docker::{self, Scanner};
use gitvis_core::devops::govern::envs::{self, EnvForm};
use gitvis_core::devops::govern::protection::{self, ProtectionForm};
use gitvis_core::devops::govern::sbom;
use gitvis_core::devops::govern::{self as gcore, http_status};
use gitvis_core::devops::proc::{ProcSpec, run_capture};
use gitvis_core::devops::tools::{self, Tool};

use super::{
    Act, AlertKind, Changed, CoFile, DismissForm, EnvEdit, Msg, ScanFailure, ScanRun,
    ScannerStatus, SecretForm, State, Tab, VariableForm, alerts_view, config_view, supply_view,
};
use crate::devops::console::ConsoleRun;
use crate::devops::ctx::Ctx;
use crate::state::{RepoState, ToastKind};

/// How long CODEOWNERS edits settle before the file checks run again.
pub const ANALYZE_DEBOUNCE: Duration = Duration::from_millis(600);

fn s(e: impl ToString) -> String {
    e.to_string()
}

/// The page became visible.
pub fn page_shown(cx: &mut Ctx, repo: &mut RepoState) {
    let st = &mut repo.devops.govern;
    if cx.gh.is_some() && st.access.untouched() {
        load_access(cx, st);
    }
    tab_shown(cx, repo, false);
}

fn load_access(cx: &mut Ctx, st: &mut State) {
    if cx.gh.is_none() || !st.access.begin() {
        return;
    }
    cx.gh_read("Repository role and default branch", |gh, log| {
        Msg::Access(
            gcore::fetch_access(gh, &mut log.calls)
                .map(Box::new)
                .map_err(s),
        )
    });
}

/// Fetch what the current section needs (everything again with `force`).
fn tab_shown(cx: &mut Ctx, repo: &mut RepoState, force: bool) {
    let tab = repo.devops.govern.tab;
    let gh = cx.gh.is_some();
    let st = &mut repo.devops.govern;
    match tab {
        Tab::Dependabot => {
            if gh && (force || st.dependabot.untouched()) {
                load_dependabot(cx, st);
            }
            if st.dep_packages.is_none() {
                if let Some(s) = st.sbom.ok() {
                    st.dep_packages = Some(s.packages.clone());
                } else if !st.sbom.loading {
                    load_sbom(cx, st);
                }
            }
        }
        Tab::CodeScanning => {
            if gh && (force || st.code.untouched()) {
                load_code(cx, st);
            }
        }
        Tab::Sbom => {
            if force || st.sbom.untouched() {
                load_sbom(cx, st);
            }
            if force && st.gh_sbom.ok().is_some() {
                load_gh_sbom(cx, st);
            }
        }
        Tab::Images => {
            if force || st.images.untouched() {
                load_images(cx, st);
            }
            if force || st.scanners.untouched() {
                if force {
                    tools::refresh();
                }
                detect_scanners(cx, st);
            }
        }
        Tab::Protection => {
            if !gh {
                return;
            }
            if (force || st.protected.untouched()) && st.protected.begin() {
                cx.gh_read("Protected branches", |gh, log| {
                    Msg::Protected(
                        protection::fetch_protected_branches(gh, &mut log.calls).map_err(s),
                    )
                });
            }
            if force || st.rulesets.untouched() {
                load_rulesets(cx, st);
            }
            if st.branch.is_empty() {
                choose_branch(cx, st);
            } else if force || st.protection.untouched() || st.prot_branch != st.branch {
                let b = st.branch.clone();
                load_protection(cx, st, &b);
            }
        }
        Tab::Codeowners => {
            if force || st.co.file.untouched() {
                load_codeowners(cx, st);
            }
            if gh && (force || st.co.errors.untouched()) {
                load_co_errors(cx, st);
            }
        }
        Tab::Environments => {
            if gh && (force || st.envs.untouched()) {
                load_envs(cx, st);
            }
            if force && let Some(e) = st.env_selected.clone() {
                load_policies(cx, st, &e);
            }
        }
        Tab::Secrets => {
            if !gh {
                return;
            }
            if st.envs.untouched() {
                load_envs(cx, st);
            }
            let scope = st.secret_scope.clone();
            if force || st.secrets.get(&scope).is_none_or(|f| f.untouched()) {
                load_secrets(cx, st, scope);
            }
            // Every scope (organisation, up to ten environments), so the
            // workflow references can say where each secret is defined.
            let mut more = Vec::new();
            if st.access.ok().is_some_and(|a| a.is_org()) {
                more.push(Scope::Organization);
            }
            if let Some(envs) = st.envs.ok() {
                more.extend(
                    envs.iter()
                        .take(10)
                        .map(|e| Scope::Environment(e.name.clone())),
                );
            }
            for s in more {
                if force || st.secrets.get(&s).is_none_or(|f| f.untouched()) {
                    load_secrets(cx, st, s);
                }
            }
            if force || st.workflow_secrets.untouched() {
                load_workflow_secrets(cx, st);
            }
        }
        Tab::Variables => {
            if !gh {
                return;
            }
            if st.envs.untouched() {
                load_envs(cx, st);
            }
            let scope = st.var_scope.clone();
            if force || st.variables.get(&scope).is_none_or(|f| f.untouched()) {
                load_variables(cx, st, scope);
            }
        }
    }
}

fn load_dependabot(cx: &mut Ctx, st: &mut State) {
    if cx.gh.is_none() || !st.dependabot.begin() {
        return;
    }
    let filter = st.dep_filter;
    cx.gh_read(
        format!("Dependabot alerts ({})", filter.label().to_lowercase()),
        move |gh, log| {
            Msg::Dependabot(alerts::fetch_dependabot(gh, filter, &mut log.calls).map_err(s))
        },
    );
}

fn load_code(cx: &mut Ctx, st: &mut State) {
    if cx.gh.is_none() || !st.code.begin() {
        return;
    }
    let filter = st.code_filter;
    cx.gh_read(
        format!("Code scanning alerts ({})", filter.label().to_lowercase()),
        move |gh, log| Msg::Code(alerts::fetch_code(gh, filter, &mut log.calls).map_err(s)),
    );
}

fn load_sbom(cx: &mut Ctx, st: &mut State) {
    if !st.sbom.begin() {
        return;
    }
    let started = cx.read("Read the lockfiles", |dir, log| {
        let tracked = match gcore::tracked_files(dir, &mut log.commands) {
            Ok(t) => t,
            Err(e) => return Msg::Sbom(Err(e.to_string())),
        };
        let s = sbom::local_sbom(dir, &tracked);
        for (f, _) in &s.lockfiles {
            log.command(format!("read {f}"));
        }
        Msg::Sbom(Ok(Box::new(s)))
    });
    if !started {
        st.sbom.loading = false;
    }
}

fn load_gh_sbom(cx: &mut Ctx, st: &mut State) {
    if cx.gh.is_none() || !st.gh_sbom.begin() {
        return;
    }
    cx.gh_read("GitHub dependency graph SBOM", |gh, log| {
        Msg::GhSbom(
            sbom::fetch_gh_sbom(gh, &mut log.calls)
                .map(Box::new)
                .map_err(s),
        )
    });
}

fn load_images(cx: &mut Ctx, st: &mut State) {
    if !st.images.begin() {
        return;
    }
    let started = cx.read("Find and lint Dockerfiles", |dir, log| {
        let tracked = match gcore::tracked_files(dir, &mut log.commands) {
            Ok(t) => t,
            Err(e) => return Msg::Images(Err(e.to_string())),
        };
        let r = docker::scan_tree(dir, &tracked);
        for (df, _) in &r.dockerfiles {
            log.command(format!("read {}", df.path));
        }
        Msg::Images(Ok(Box::new(r)))
    });
    if !started {
        st.images.loading = false;
    }
}

fn detect_scanners(cx: &mut Ctx, st: &mut State) {
    if !st.scanners.begin() {
        return;
    }
    let started = cx.read("Look for image scanners", |dir, log| {
        let mut v = Vec::new();
        for sc in [Scanner::Trivy, Scanner::Grype] {
            log.command(format!("{} --version", sc.tool().program()));
            let found = tools::detect(sc.tool());
            v.push(ScannerStatus {
                scanner: sc,
                version: found.as_ref().map(|f| f.version.clone()),
                path: found.map(|f| f.path.display().to_string()),
                note: None,
            });
        }
        let docker = tools::detect(Tool::Docker);
        let mut scout = ScannerStatus {
            scanner: Scanner::Scout,
            version: None,
            path: docker.as_ref().map(|d| d.path.display().to_string()),
            note: None,
        };
        match docker {
            None => scout.note = Some("Docker is not installed.".into()),
            Some(_) => {
                log.command("docker scout version");
                let spec = ProcSpec::new("docker", dir).args(["scout", "version"]);
                match run_capture(&spec, Some(Duration::from_secs(30))) {
                    Ok(c) if c.success() => {
                        scout.version = docker::scout_version(&c.stdout_text());
                        if scout.version.is_none() {
                            scout.note = Some("docker scout answered without a version.".into());
                        }
                    }
                    Ok(c) => {
                        scout.note = Some(format!(
                            "The docker scout plugin is not available: {}",
                            gcore::clip(&c.error_text(), 200)
                        ))
                    }
                    Err(e) => scout.note = Some(e.to_string()),
                }
            }
        }
        v.push(scout);
        Msg::Scanners(v)
    });
    if !started {
        st.scanners.loading = false;
    }
}

fn load_rulesets(cx: &mut Ctx, st: &mut State) {
    if cx.gh.is_none() || !st.rulesets.begin() {
        return;
    }
    cx.gh_read("Rulesets", |gh, log| {
        Msg::Rulesets(protection::fetch_rulesets(gh, &mut log.calls).map_err(s))
    });
}

/// Pick the branch to show: the default branch, else the first protected.
fn choose_branch(cx: &mut Ctx, st: &mut State) {
    if !st.branch.is_empty() {
        return;
    }
    let default = st
        .access
        .ok()
        .map(|a| a.default_branch.clone())
        .filter(|b| !b.is_empty())
        .or_else(|| st.protected.ok().and_then(|p| p.first().cloned()));
    if let Some(b) = default {
        st.branch = b.clone();
        load_protection(cx, st, &b);
    }
}

fn load_protection(cx: &mut Ctx, st: &mut State, branch: &str) {
    if cx.gh.is_none() || branch.is_empty() {
        return;
    }
    if st.protection.loading && st.prot_branch == branch {
        return;
    }
    st.prot_branch = branch.to_owned();
    st.protection.loading = true;
    st.prot_edit = None;
    let b = branch.to_owned();
    cx.gh_read(format!("Branch protection of {b}"), move |gh, log| {
        let r = match protection::fetch_protection(gh, &b, &mut log.calls) {
            Ok(f) => Ok(Some(Box::new(f))),
            Err(e) => {
                let m = e.to_string();
                if http_status(&m) == Some(404) && m.contains("Branch not protected") {
                    Ok(None)
                } else {
                    Err(m)
                }
            }
        };
        Msg::Protection(b, r)
    });
    st.rules.loading = true;
    let b = branch.to_owned();
    cx.gh_read(format!("Rules in effect on {b}"), move |gh, log| {
        Msg::Rules(
            b.clone(),
            protection::fetch_effective(gh, &b, &mut log.calls).map_err(s),
        )
    });
}

fn load_codeowners(cx: &mut Ctx, st: &mut State) {
    if !st.co.file.begin() {
        return;
    }
    let started = cx.read("Read CODEOWNERS", |dir, log| {
        let found = codeowners::locate(dir);
        log.command(format!("look for {}", codeowners::LOCATIONS.join(", ")));
        let tracked = match gcore::tracked_files(dir, &mut log.commands) {
            Ok(t) => t,
            Err(e) => return Msg::CoFile(Err(e.to_string())),
        };
        let mut file = CoFile {
            path: found.first().cloned(),
            ignored: found.iter().skip(1).cloned().collect(),
            text: String::new(),
        };
        if let Some(p) = &file.path {
            log.command(format!("read {p}"));
            match std::fs::read_to_string(dir.join(p)) {
                Ok(t) => file.text = t,
                Err(e) => return Msg::CoFile(Err(format!("Could not read {p}: {e}"))),
            }
        }
        Msg::CoFile(Ok(Box::new((file, Arc::new(tracked)))))
    });
    if !started {
        st.co.file.loading = false;
    }
}

fn load_co_errors(cx: &mut Ctx, st: &mut State) {
    if cx.gh.is_none() || !st.co.errors.begin() {
        return;
    }
    cx.gh_read("CODEOWNERS errors on GitHub", |gh, log| {
        Msg::CoErrors(codeowners::fetch_errors(gh, None, &mut log.calls).map_err(s))
    });
}

/// Check the current CODEOWNERS text against the tracked files.
fn analyze_codeowners(cx: &mut Ctx, st: &mut State) {
    let Some(tracked) = st.co.tracked.clone() else {
        return;
    };
    if st.co.analysis.loading {
        return;
    }
    if st.co.parsed.is_none() {
        st.co.reparse();
    }
    let generation = st.co.generation;
    let path = st.co.path();
    let text = st.co.current_text().to_owned();
    st.co.analysis.loading = true;
    cx.read(
        "Match CODEOWNERS against the tracked files",
        move |_, log| {
            log.command(format!(
                "match {} rules against {} tracked files",
                text.lines().count(),
                tracked.len()
            ));
            let f = codeowners::parse(&path, &text);
            Msg::CoAnalysis(
                generation,
                codeowners::analyze(&f, &tracked).map(Box::new).map_err(s),
            )
        },
    );
    update_lookup(st);
}

/// Recompute "who owns this path" for the current text.
fn update_lookup(st: &mut State) {
    let path = st.co.test_path.trim().to_owned();
    if path.is_empty() {
        st.co.lookup = None;
        return;
    }
    if st.co.parsed.is_none() {
        st.co.reparse();
    }
    let (Some(parsed), Some(tracked)) = (&st.co.parsed, &st.co.tracked) else {
        st.co.lookup = None;
        return;
    };
    st.co.lookup = codeowners::Matcher::new(parsed)
        .ok()
        .map(|m| codeowners::lookup(&m, &path, tracked));
}

fn load_envs(cx: &mut Ctx, st: &mut State) {
    if cx.gh.is_none() || !st.envs.begin() {
        return;
    }
    cx.gh_read("Environments", |gh, log| {
        Msg::Envs(envs::fetch_environments(gh, &mut log.calls).map_err(s))
    });
}

fn load_policies(cx: &mut Ctx, st: &mut State, env: &str) {
    if cx.gh.is_none() {
        return;
    }
    let f = st.policies.entry(env.to_owned()).or_default();
    if !f.begin() {
        return;
    }
    let e = env.to_owned();
    cx.gh_read(
        format!("Deployment branch policies of {e}"),
        move |gh, log| {
            Msg::Policies(
                e.clone(),
                envs::fetch_branch_policies(gh, &e, &mut log.calls).map_err(s),
            )
        },
    );
}

fn load_secrets(cx: &mut Ctx, st: &mut State, scope: Scope) {
    if cx.gh.is_none() {
        return;
    }
    let f = st.secrets.entry(scope.clone()).or_default();
    if !f.begin() {
        return;
    }
    cx.gh_read(format!("Secrets ({})", scope.label()), move |gh, log| {
        Msg::Secrets(
            scope.clone(),
            config::fetch_secrets(gh, &scope, &mut log.calls).map_err(s),
        )
    });
}

fn load_variables(cx: &mut Ctx, st: &mut State, scope: Scope) {
    if cx.gh.is_none() {
        return;
    }
    let f = st.variables.entry(scope.clone()).or_default();
    if !f.begin() {
        return;
    }
    cx.gh_read(format!("Variables ({})", scope.label()), move |gh, log| {
        Msg::Variables(
            scope.clone(),
            config::fetch_variables(gh, &scope, &mut log.calls).map_err(s),
        )
    });
}

fn load_workflow_secrets(cx: &mut Ctx, st: &mut State) {
    if !st.workflow_secrets.begin() {
        return;
    }
    let started = cx.read("Secrets referenced by workflows", |dir, log| {
        log.command("read .github/workflows/*.yml");
        Msg::WorkflowSecrets(config::workflow_secret_refs(dir))
    });
    if !started {
        st.workflow_secrets.loading = false;
    }
}

/// `owner/repo` of the remote.
fn full_name(repo: &RepoState) -> Option<String> {
    repo.github_remote.as_ref().map(|(o, r)| format!("{o}/{r}"))
}

/// Request lines for a confirmation: the command, then the pretty body.
fn request_details(req: &Request) -> Vec<String> {
    let mut d = vec![req.display()];
    if let Some(b) = req.body_preview() {
        d.push(b);
    }
    d
}

/// Pretty JSON of an object with `first` keys at the top (the body that is
/// sent is the same object; only the preview is ordered).
pub fn ordered_preview(v: &serde_json::Value, first: &[&str]) -> String {
    let Some(obj) = v.as_object() else {
        return serde_json::to_string_pretty(v).unwrap_or_default();
    };
    let mut keys: Vec<&String> = first
        .iter()
        .filter_map(|k| obj.get_key_value(*k).map(|(k, _)| k))
        .collect();
    keys.extend(obj.keys().filter(|k| !first.contains(&k.as_str())));
    let mut out = String::from("{\n");
    for (i, k) in keys.iter().enumerate() {
        let val = serde_json::to_string_pretty(&obj[k.as_str()]).unwrap_or_default();
        let val = val.replace('\n', "\n  ");
        out.push_str(&format!(
            "  \"{k}\": {val}{}\n",
            if i + 1 < keys.len() { "," } else { "" }
        ));
    }
    out.push('}');
    out
}

/// A GitHub change: logged with its request, toasted, then `ok` on
/// success (or `ChangeFailed`).
fn send(cx: &mut Ctx, label: String, request: Request, ok: Changed) {
    let done = label.clone();
    cx.gh_change(label, move |gh, log| {
        let r = gh.send(&request, &mut log.calls).map(|_| done).map_err(s);
        let msg = if r.is_ok() {
            Msg::Changed(ok)
        } else {
            Msg::ChangeFailed
        };
        (r, Some(msg))
    });
}

fn gh_of(cx: &Ctx) -> Option<Gh> {
    cx.gh.clone()
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::SetTab(t) => {
            repo.devops.govern.tab = t;
            tab_shown(cx, repo, false);
        }
        Act::Refresh => {
            let st = &mut repo.devops.govern;
            if cx.gh.is_some() && !st.access.loading {
                load_access(cx, st);
            }
            tab_shown(cx, repo, true);
        }

        // -------------------------------------------------------------
        // Alerts
        // -------------------------------------------------------------
        Act::SetAlertFilter(kind, f) => {
            let st = &mut repo.devops.govern;
            match kind {
                AlertKind::Dependabot => {
                    if st.dep_filter != f {
                        st.dep_filter = f;
                        st.dep_selected = None;
                        load_dependabot(cx, st);
                    }
                }
                AlertKind::Code => {
                    if st.code_filter != f {
                        st.code_filter = f;
                        st.code_selected = None;
                        load_code(cx, st);
                    }
                }
            }
        }
        Act::SelectAlert(kind, n) => {
            let st = &mut repo.devops.govern;
            if st.dismiss.as_ref().is_some_and(|d| Some(d.number) != n) {
                st.dismiss = None;
            }
            match kind {
                AlertKind::Dependabot => st.dep_selected = n,
                AlertKind::Code => {
                    st.code_selected = n;
                    if let Some(n) = n
                        && !st.snippets.contains_key(&n)
                        && let Some(a) = st.code.ok().and_then(|v| v.iter().find(|a| a.number == n))
                    {
                        let (path, start, end) = (a.path.clone(), a.start_line, a.end_line);
                        cx.read(format!("Code around {path}:{start}"), move |dir, log| {
                            log.command(format!("read {path}"));
                            Msg::Snippet(n, alerts::snippet(dir, &path, start, end, 4))
                        });
                    }
                }
            }
        }
        Act::OpenDismiss(kind, number) => {
            repo.devops.govern.dismiss = Some(DismissForm {
                kind,
                number,
                dep_reason: Default::default(),
                code_reason: Default::default(),
                comment: String::new(),
            });
        }
        Act::CloseDismiss => repo.devops.govern.dismiss = None,
        Act::ReviewDismiss => {
            let Some(gh) = gh_of(cx) else { return };
            let Some(d) = &repo.devops.govern.dismiss else {
                return;
            };
            let (request, reason, what) = match d.kind {
                AlertKind::Dependabot => (
                    alerts::dependabot_dismiss(&gh, d.number, d.dep_reason, &d.comment),
                    d.dep_reason.label(),
                    "Dependabot",
                ),
                AlertKind::Code => (
                    alerts::code_dismiss(&gh, d.number, d.code_reason, &d.comment),
                    d.code_reason.label(),
                    "code scanning",
                ),
            };
            let label = format!("Dismiss {what} alert #{}", d.number);
            crate::devops::pages::explain::confirm(
                cx,
                format!("Dismiss {what} alert #{}?", d.number),
                format!(
                    "Reason: {reason}. GitHub records who dismissed it and why; it can be reopened later."
                ),
                "Dismiss alert",
                false,
                request_details(&request),
                Act::PatchAlert {
                    kind: d.kind,
                    label,
                    request,
                }
                .into(),
            );
        }
        Act::ReviewReopen(kind, number) => {
            let Some(gh) = gh_of(cx) else { return };
            let (request, what) = match kind {
                AlertKind::Dependabot => (alerts::dependabot_reopen(&gh, number), "Dependabot"),
                AlertKind::Code => (alerts::code_reopen(&gh, number), "code scanning"),
            };
            crate::devops::pages::explain::confirm(
                cx,
                format!("Reopen {what} alert #{number}?"),
                "The alert becomes open again and counts towards the repository's open alerts.",
                "Reopen alert",
                false,
                request_details(&request),
                Act::PatchAlert {
                    kind,
                    label: format!("Reopen {what} alert #{number}"),
                    request,
                }
                .into(),
            );
        }
        Act::PatchAlert {
            kind,
            label,
            request,
        } => {
            let ok = match kind {
                AlertKind::Dependabot => Changed::Dependabot,
                AlertKind::Code => Changed::Code,
            };
            send(cx, label, request, ok);
        }

        // -------------------------------------------------------------
        // SBOM
        // -------------------------------------------------------------
        Act::LoadSbom => {
            let st = &mut repo.devops.govern;
            load_sbom(cx, st);
        }
        Act::FetchGhSbom => {
            let st = &mut repo.devops.govern;
            load_gh_sbom(cx, st);
        }
        Act::Export(format) => {
            let name = repo.name.clone();
            let file = match format.as_str() {
                "cyclonedx" => format!("{name}.cdx.json"),
                "spdx" => format!("{name}.spdx.json"),
                _ => format!("{name}.github.spdx.json"),
            };
            let Some(path) = rfd::FileDialog::new()
                .set_title("Save the SBOM")
                .set_file_name(&file)
                .add_filter("JSON", &["json"])
                .save_file()
            else {
                return;
            };
            export(cx, repo, &format, path);
        }
        Act::ExportTo(format, path) => export(cx, repo, &format, path),
        Act::SelectPackage(k) => repo.devops.govern.sbom_selected = k,

        // -------------------------------------------------------------
        // Images
        // -------------------------------------------------------------
        Act::LoadImages => {
            let st = &mut repo.devops.govern;
            load_images(cx, st);
        }
        Act::SelectDockerfile(p) => repo.devops.govern.image_selected = p,
        Act::DetectScanners => {
            tools::refresh();
            let st = &mut repo.devops.govern;
            detect_scanners(cx, st);
        }
        Act::SetScanner(s) => repo.devops.govern.scan_with = Some(s),
        Act::Scan(scanner, image) => {
            let image = image.trim().to_owned();
            if image.is_empty() || image.contains(char::is_whitespace) || image.starts_with('-') {
                cx.toast(
                    ToastKind::Error,
                    "Enter an image reference such as alpine:3.20",
                );
                return;
            }
            let st = &mut repo.devops.govern;
            if st
                .scans
                .iter()
                .any(|r| r.result.is_none() && r.image == image && r.scanner == scanner)
            {
                return;
            }
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let report = scanner.report_path(stamp);
            let spec = scanner.spec(&image, &report, &repo.workdir);
            let id = cx.console(format!("{} scan of {image}", scanner.label()), spec);
            st.scans.insert(
                0,
                ScanRun {
                    id,
                    scanner,
                    image: image.clone(),
                    report,
                    started: Instant::now(),
                    result: None,
                },
            );
            st.scans.truncate(12);
            st.scan_image = image;
        }

        // -------------------------------------------------------------
        // Protection
        // -------------------------------------------------------------
        Act::SelectBranch(b) => {
            let st = &mut repo.devops.govern;
            let b = b.trim().to_owned();
            if b.is_empty() {
                return;
            }
            st.branch = b.clone();
            if st.tab == Tab::Protection {
                load_protection(cx, st, &b);
            }
        }
        Act::StartProtecting => {
            let st = &mut repo.devops.govern;
            if st.prot_edit.is_none() {
                st.prot_edit = Some(
                    st.protection
                        .ok()
                        .and_then(|p| p.clone())
                        .unwrap_or_else(ProtectionForm::recommended),
                );
            }
        }
        Act::ReviewProtection => {
            let Some(gh) = gh_of(cx) else { return };
            let st = &repo.devops.govern;
            let Some(form) = &st.prot_edit else { return };
            let branch = st.prot_branch.clone();
            let old = st.protection.ok().and_then(|p| p.as_ref());
            let request = protection::protection_put(&gh, &branch, form);
            let mut details = vec![request.display()];
            if let Some(b) = &request.body {
                details.push(ordered_preview(
                    b,
                    &[
                        "required_status_checks",
                        "enforce_admins",
                        "required_pull_request_reviews",
                        "restrictions",
                    ],
                ));
            }
            let changes = protection::changes(old, form);
            let what = if old.is_none() {
                format!("{branch} is not protected yet; this creates its protection rule.")
            } else if changes.is_empty() {
                "No setting changes; the rule is written again as it is.".to_owned()
            } else {
                format!("Changes: {}.", changes.join("; "))
            };
            let body = format!(
                "{what} The PUT replaces the whole rule: the four required keys come first, null where a setting is off."
            );
            crate::devops::pages::explain::confirm(
                cx,
                format!("Save branch protection of {branch}?"),
                body,
                "Save protection",
                false,
                details,
                Act::SaveProtection { branch, request }.into(),
            );
        }
        Act::SaveProtection { branch, request } => {
            send(
                cx,
                format!("Save branch protection of {branch}"),
                request,
                Changed::Protection(branch),
            );
        }
        Act::ReviewRemoveProtection => {
            let Some(gh) = gh_of(cx) else { return };
            let branch = repo.devops.govern.prot_branch.clone();
            let request = protection::protection_delete(&gh, &branch);
            crate::devops::pages::explain::confirm(
                cx,
                format!("Remove branch protection from {branch}?"),
                "Anyone with write access can then push, force-push or delete the branch (rulesets still apply).",
                "Remove protection",
                true,
                request_details(&request),
                Act::RemoveProtection { branch, request }.into(),
            );
        }
        Act::RemoveProtection { branch, request } => {
            send(
                cx,
                format!("Remove branch protection of {branch}"),
                request,
                Changed::Protection(branch),
            );
        }

        // -------------------------------------------------------------
        // CODEOWNERS
        // -------------------------------------------------------------
        Act::LoadCodeowners => {
            let st = &mut repo.devops.govern;
            load_codeowners(cx, st);
        }
        Act::AnalyzeCodeowners => {
            let st = &mut repo.devops.govern;
            analyze_codeowners(cx, st);
        }
        Act::TestPath(p) => {
            let st = &mut repo.devops.govern;
            st.co.test_path = p;
            update_lookup(st);
        }
        Act::EditCodeowners(on) => {
            let st = &mut repo.devops.govern;
            if on && st.co.file.ok().is_none() {
                // Never open the editor on a file that is still being read
                // (saving would replace it with the template).
                st.co.edit_when_loaded = true;
                if st.co.file.untouched() {
                    load_codeowners(cx, st);
                }
                return;
            }
            st.co.edit_when_loaded = false;
            st.co.editing = on;
            if on {
                let saved = st.co.file.ok().map(|f| f.text.clone()).unwrap_or_default();
                st.co.text = if saved.is_empty() {
                    TEMPLATE.to_owned()
                } else {
                    saved
                };
                st.co.edited_at = Some(Instant::now());
            }
            st.co.reparse();
            update_lookup(st);
            if !on {
                analyze_codeowners(cx, st);
            }
        }
        Act::ReviewCodeownersSave => {
            let st = &repo.devops.govern;
            if !st.co.editing {
                return;
            }
            let path = st.co.path();
            let old = st.co.file.ok().map(|f| f.text.clone()).unwrap_or_default();
            let text = st.co.text.clone();
            if text == old {
                cx.toast(ToastKind::Info, format!("{path} has no changes to save"));
                return;
            }
            let mut details = vec![format!(
                "write {path}   ({} bytes, {} lines; in {})",
                text.len(),
                text.lines().count(),
                repo.workdir.display()
            )];
            details.extend(codeowners::line_diff(&old, &text, 200));
            let errors = st.co.parsed.as_ref().map(|p| p.errors()).unwrap_or(0);
            crate::devops::pages::explain::confirm(
                cx,
                format!("Save {path}?"),
                format!(
                    "The file is written in the working tree; commit and push it for GitHub to use it.{}",
                    if errors > 0 {
                        format!(
                            " It still has {errors} {}: GitHub skips those lines.",
                            if errors == 1 { "error" } else { "errors" }
                        )
                    } else {
                        String::new()
                    }
                ),
                "Save file",
                false,
                details,
                Act::SaveCodeowners { path, text }.into(),
            );
        }
        Act::SaveCodeowners { path, text } => {
            let label = format!("Save {path}");
            cx.local_change(label, move |dir, log| {
                log.command(format!("write {path} ({} bytes)", text.len()));
                let full = dir.join(&path);
                let r = full
                    .parent()
                    .map(std::fs::create_dir_all)
                    .unwrap_or(Ok(()))
                    .and_then(|_| std::fs::write(&full, text.as_bytes()))
                    .map(|_| format!("Wrote {path} ({} lines)", text.lines().count()))
                    .map_err(|e| format!("Could not write {path}: {e}"));
                let msg = if r.is_ok() {
                    Msg::Changed(Changed::Codeowners)
                } else {
                    Msg::ChangeFailed
                };
                (r, Some(msg))
            });
        }

        // -------------------------------------------------------------
        // Environments
        // -------------------------------------------------------------
        Act::SelectEnv(e) => {
            let st = &mut repo.devops.govern;
            st.env_selected = e.clone();
            if let Some(name) = e {
                let custom = st
                    .envs
                    .ok()
                    .and_then(|v| v.iter().find(|x| x.name == name))
                    .is_some_and(|x| x.policy == envs::PolicyMode::Custom);
                if custom && st.policies.get(&name).is_none_or(|f| f.untouched()) {
                    load_policies(cx, st, &name);
                }
            }
        }
        Act::NewEnv => {
            repo.devops.govern.env_edit = Some(EnvEdit {
                form: EnvForm::new_env(),
                editing: None,
                reviewer_buf: String::new(),
                wait_text: "0".into(),
                resolving: false,
                error: String::new(),
                known: Vec::new(),
            });
        }
        Act::EditEnv(name) => {
            let st = &mut repo.devops.govern;
            if let Some(e) = st.envs.ok().and_then(|v| v.iter().find(|x| x.name == name)) {
                st.env_edit = Some(EnvEdit {
                    form: EnvForm::from_env(e),
                    editing: Some(name),
                    reviewer_buf: String::new(),
                    wait_text: e.wait_timer.to_string(),
                    resolving: false,
                    error: String::new(),
                    known: e.reviewers.clone(),
                });
            }
        }
        Act::CancelEnv => repo.devops.govern.env_edit = None,
        Act::ReviewEnv => {
            let st = &mut repo.devops.govern;
            let Some(edit) = &mut st.env_edit else { return };
            match edit.wait_text.trim().parse::<u32>() {
                Ok(n) => edit.form.wait_timer = n,
                Err(_) if edit.wait_text.trim().is_empty() => edit.form.wait_timer = 0,
                Err(_) => {
                    edit.error = "The wait timer is a number of minutes (0 to 43200).".into();
                    return;
                }
            }
            let problems = edit.form.problems();
            if !problems.is_empty() {
                edit.error = problems.join(" ");
                return;
            }
            if edit.resolving {
                return;
            }
            edit.error.clear();
            edit.resolving = true;
            let handles = edit.form.reviewers.clone();
            let known = edit.known.clone();
            if !cx.gh_read("Resolve reviewer ids", move |gh, log| {
                Msg::Resolved(
                    envs::resolve_reviewers(gh, &handles, &known, &mut log.calls).map_err(s),
                )
            }) {
                edit.resolving = false;
            }
        }
        Act::SaveEnv { name, request } => {
            send(
                cx,
                format!("Save environment {name}"),
                request,
                Changed::Envs,
            );
        }
        Act::ReviewDeleteEnv(name) => {
            let Some(gh) = gh_of(cx) else { return };
            let request = envs::environment_delete(&gh, &name);
            crate::devops::pages::explain::confirm(
                cx,
                format!("Delete the environment {name}?"),
                "Its protection rules, branch policies, secrets and variables are deleted with it. Deployments that target it stop waiting for approval.",
                "Delete environment",
                true,
                request_details(&request),
                Act::DeleteEnv { name, request }.into(),
            );
        }
        Act::DeleteEnv { name, request } => {
            repo.devops.govern.env_selected = None;
            send(
                cx,
                format!("Delete environment {name}"),
                request,
                Changed::Envs,
            );
        }
        Act::ReviewAddPolicy(env) => {
            let Some(gh) = gh_of(cx) else { return };
            let st = &repo.devops.govern;
            let pattern = st.policy_buf.trim().to_owned();
            if pattern.is_empty() {
                return;
            }
            let request = envs::branch_policy_create(&gh, &env, &pattern, st.policy_tag);
            let kind = if st.policy_tag { "tag" } else { "branch" };
            crate::devops::pages::explain::confirm(
                cx,
                format!("Allow {kind}es matching {pattern} to deploy to {env}?"),
                "Patterns use fnmatch syntax: * matches anything but /, ** also matches /.",
                "Add policy",
                false,
                request_details(&request),
                Act::PolicyChange {
                    env: env.clone(),
                    label: format!("Add {kind} policy {pattern} to {env}"),
                    request,
                }
                .into(),
            );
        }
        Act::ReviewDeletePolicy { env, id, name } => {
            let Some(gh) = gh_of(cx) else { return };
            let request = envs::branch_policy_delete(&gh, &env, id);
            crate::devops::pages::explain::confirm(
                cx,
                format!("Remove the policy {name} from {env}?"),
                "Refs matching only this pattern can no longer deploy to the environment.",
                "Remove policy",
                true,
                request_details(&request),
                Act::PolicyChange {
                    env: env.clone(),
                    label: format!("Remove policy {name} from {env}"),
                    request,
                }
                .into(),
            );
        }
        Act::PolicyChange {
            env,
            label,
            request,
        } => {
            repo.devops.govern.policy_buf.clear();
            send(cx, label, request, Changed::Policies(env));
        }

        // -------------------------------------------------------------
        // Secrets
        // -------------------------------------------------------------
        Act::SetSecretScope(scope) => {
            let st = &mut repo.devops.govern;
            st.secret_scope = scope.clone();
            if st.secrets.get(&scope).is_none_or(|f| f.untouched()) {
                load_secrets(cx, st, scope);
            }
        }
        Act::NewSecret(scope, name) => {
            let st = &mut repo.devops.govern;
            if let Some(f) = &mut st.secret_form {
                wipe_string(&mut f.value);
            }
            st.secret_form = Some(SecretForm {
                scope,
                existing: name.is_some(),
                name: name.unwrap_or_default(),
                value: String::new(),
                fresh: true,
                closing: false,
            });
        }
        Act::CancelSecret => {
            if let Some(f) = &mut repo.devops.govern.secret_form {
                wipe_string(&mut f.value);
                f.closing = true;
            }
        }
        Act::ReviewSecret => {
            let Some(full) = full_name(repo) else { return };
            let st = &repo.devops.govern;
            let Some(f) = &st.secret_form else { return };
            if f.closing || config::name_problem(&f.name).is_some() || f.value.is_empty() {
                return;
            }
            let name = config::normalize_name(&f.name);
            let args = config::secret_set_args(&full, &name, f.scope.env());
            let preview = config::secret_set_preview(&args, f.value.chars().count());
            let existing = st
                .secrets
                .get(&f.scope)
                .and_then(|x| x.ok())
                .is_some_and(|v| v.iter().any(|x| x.name == name));
            crate::devops::pages::explain::confirm(
                cx,
                format!(
                    "{} the secret {name}?",
                    if existing { "Update" } else { "Create" }
                ),
                format!(
                    "{} on {full}. GitHub encrypts the value; nobody can read it back, only replace it.{}",
                    f.scope.label(),
                    if existing {
                        " The current value is overwritten."
                    } else {
                        ""
                    }
                ),
                if existing {
                    "Update secret"
                } else {
                    "Create secret"
                },
                existing,
                vec![preview],
                Act::SetSecretConfirmed.into(),
            );
        }
        Act::SetSecretConfirmed => {
            let Some(full) = full_name(repo) else { return };
            let st = &mut repo.devops.govern;
            let Some(f) = &mut st.secret_form else {
                return;
            };
            if f.closing || f.value.is_empty() {
                return;
            }
            f.closing = true;
            let name = config::normalize_name(&f.name);
            // Move the value out of the form; nothing else holds it.
            let value = std::mem::take(&mut f.value).into_bytes();
            let mut set = config::secret_set(&full, &name, f.scope.env(), value);
            let scope = f.scope.clone();
            cx.gh_change(format!("Set secret {name}"), move |gh, log| {
                let args: Vec<&str> = set.args.iter().map(String::as_str).collect();
                let r = gh
                    .cli(&args, Some(&set.stdin), &mut log.calls)
                    .map(|_| format!("Secret {name} set ({})", scope.label()))
                    .map_err(s);
                set.wipe();
                let msg = if r.is_ok() {
                    Msg::Changed(Changed::Secrets(scope))
                } else {
                    Msg::ChangeFailed
                };
                (r, Some(msg))
            });
        }
        Act::ReviewDeleteSecret(scope, name) => {
            let Some(full) = full_name(repo) else { return };
            let args = config::secret_delete_args(&full, &name, scope.env());
            crate::devops::pages::explain::confirm(
                cx,
                format!("Delete the secret {name}?"),
                format!(
                    "{} on {full}. Workflows that read secrets.{name} get an empty string from then on.",
                    scope.label()
                ),
                "Delete secret",
                true,
                vec![format!(
                    "gh {}",
                    gitvis_core::devops::proc::render_args(
                        &args.iter().map(String::as_str).collect::<Vec<_>>()
                    )
                )],
                Act::DeleteSecret(scope, name).into(),
            );
        }
        Act::DeleteSecret(scope, name) => {
            let Some(full) = full_name(repo) else { return };
            let args = config::secret_delete_args(&full, &name, scope.env());
            cx.gh_change(format!("Delete secret {name}"), move |gh, log| {
                let a: Vec<&str> = args.iter().map(String::as_str).collect();
                let r = gh
                    .cli(&a, None, &mut log.calls)
                    .map(|_| format!("Secret {name} deleted ({})", scope.label()))
                    .map_err(s);
                let msg = if r.is_ok() {
                    Msg::Changed(Changed::Secrets(scope))
                } else {
                    Msg::ChangeFailed
                };
                (r, Some(msg))
            });
        }

        // -------------------------------------------------------------
        // Variables
        // -------------------------------------------------------------
        Act::SetVariableScope(scope) => {
            let st = &mut repo.devops.govern;
            st.var_scope = scope.clone();
            if st.variables.get(&scope).is_none_or(|f| f.untouched()) {
                load_variables(cx, st, scope);
            }
        }
        Act::NewVariable(scope) => {
            repo.devops.govern.variable_form = Some(VariableForm {
                scope,
                original: None,
                name: String::new(),
                value: String::new(),
            });
        }
        Act::EditVariable(scope, name) => {
            let st = &mut repo.devops.govern;
            let value = st
                .variables
                .get(&scope)
                .and_then(|f| f.ok())
                .and_then(|v| v.iter().find(|x| x.name == name))
                .map(|v| v.value.clone())
                .unwrap_or_default();
            st.variable_form = Some(VariableForm {
                scope,
                original: Some(name.clone()),
                name,
                value,
            });
        }
        Act::CancelVariable => repo.devops.govern.variable_form = None,
        Act::ReviewVariable => {
            let Some(gh) = gh_of(cx) else { return };
            let st = &repo.devops.govern;
            let Some(f) = &st.variable_form else { return };
            if config::name_problem(&f.name).is_some() {
                return;
            }
            let name = config::normalize_name(&f.name);
            let (request, title, label) = match &f.original {
                Some(orig) => (
                    config::variable_update(&gh, &f.scope, orig, &name, &f.value),
                    if *orig == name {
                        format!("Update the variable {name}?")
                    } else {
                        format!("Rename {orig} to {name} and update it?")
                    },
                    format!("Update variable {name}"),
                ),
                None => (
                    config::variable_create(&gh, &f.scope, &name, &f.value),
                    format!("Create the variable {name}?"),
                    format!("Create variable {name}"),
                ),
            };
            crate::devops::pages::explain::confirm(
                cx,
                title,
                format!(
                    "{}. Variables are not secret: anyone who can read workflow logs can see them.",
                    f.scope.label()
                ),
                if f.original.is_some() {
                    "Update variable"
                } else {
                    "Create variable"
                },
                false,
                request_details(&request),
                Act::VariableChange {
                    scope: f.scope.clone(),
                    label,
                    request,
                }
                .into(),
            );
        }
        Act::VariableChange {
            scope,
            label,
            request,
        } => send(cx, label, request, Changed::Variables(scope)),
        Act::ReviewDeleteVariable(scope, name) => {
            let Some(gh) = gh_of(cx) else { return };
            let request = config::variable_delete(&gh, &scope, &name);
            crate::devops::pages::explain::confirm(
                cx,
                format!("Delete the variable {name}?"),
                format!(
                    "{}. Workflows that read vars.{name} get an empty string from then on.",
                    scope.label()
                ),
                "Delete variable",
                true,
                request_details(&request),
                Act::VariableChange {
                    scope,
                    label: format!("Delete variable {name}"),
                    request,
                }
                .into(),
            );
        }

        // -------------------------------------------------------------
        // Script driver
        // -------------------------------------------------------------
        Act::ScriptItem(n) => script_item(cx, repo, n),
        Act::ScriptDismiss => {
            let st = &repo.devops.govern;
            let pick = match st.tab {
                Tab::Dependabot => st.dep_selected.map(|n| (AlertKind::Dependabot, n)),
                Tab::CodeScanning => st.code_selected.map(|n| (AlertKind::Code, n)),
                _ => None,
            };
            if let Some((k, n)) = pick {
                apply(cx, repo, Act::OpenDismiss(k, n));
                if let Some(d) = &mut repo.devops.govern.dismiss {
                    d.comment = "Example comment from the script driver".into();
                }
            }
        }
        Act::ScriptSecretForm => {
            apply(cx, repo, Act::NewSecret(Scope::Repository, None));
            if let Some(f) = &mut repo.devops.govern.secret_form {
                f.name = "EXAMPLE_TOKEN".into();
                f.value = "not-a-real-secret".into();
            }
        }
        Act::ScriptEnvForm => {
            apply(cx, repo, Act::NewEnv);
            if let Some(e) = &mut repo.devops.govern.env_edit {
                e.form.name = "qa".into();
                e.form.reviewers = vec!["@alice".into(), "gitvis-test/release-managers".into()];
                e.wait_text = "15".into();
                e.form.prevent_self_review = true;
                e.form.policy = envs::PolicyMode::Protected;
            }
        }
        Act::ScriptCodeownersAppend(line) => {
            let st = &mut repo.devops.govern;
            if st.co.editing {
                if !st.co.text.ends_with('\n') && !st.co.text.is_empty() {
                    st.co.text.push('\n');
                }
                st.co.text.push_str(&line);
                st.co.text.push('\n');
                st.co.edited_at = Some(Instant::now());
                st.co.reparse();
            }
        }
        Act::ScriptVariableForm => {
            apply(cx, repo, Act::NewVariable(Scope::Repository));
            if let Some(f) = &mut repo.devops.govern.variable_form {
                f.name = "EXAMPLE_REGION".into();
                f.value = "eu-west-1".into();
            }
        }
        Act::ScriptDockerfile(n) => {
            let st = &mut repo.devops.govern;
            if let Some(p) = st
                .images
                .ok()
                .and_then(|r| r.dockerfiles.get(n))
                .map(|(d, _)| d.path.clone())
            {
                st.image_selected = Some(p);
            }
        }
    }
}

/// Overwrite a string's bytes before dropping it.
fn wipe_string(s: &mut String) {
    let mut bytes = std::mem::take(s).into_bytes();
    bytes.fill(0);
}

/// Select the N-th item of the current list.
fn script_item(cx: &mut Ctx, repo: &mut RepoState, n: usize) {
    let st = &repo.devops.govern;
    let act = match st.tab {
        Tab::Dependabot => alerts_view::visible_dependabot(st)
            .get(n)
            .map(|a| Act::SelectAlert(AlertKind::Dependabot, Some(a.number))),
        Tab::CodeScanning => alerts_view::visible_code(st)
            .get(n)
            .map(|a| Act::SelectAlert(AlertKind::Code, Some(a.number))),
        Tab::Sbom => supply_view::visible_packages(st)
            .get(n)
            .map(|p| Act::SelectPackage(Some(supply_view::package_key(p)))),
        Tab::Images => st
            .images
            .ok()
            .and_then(|r| r.dockerfiles.get(n))
            .map(|(d, _)| Act::SelectDockerfile(Some(d.path.clone()))),
        Tab::Protection => {
            let default = st
                .access
                .ok()
                .map(|a| a.default_branch.clone())
                .unwrap_or_default();
            protection::branch_choices(
                &default,
                st.protected.ok().map(|v| v.as_slice()).unwrap_or(&[]),
            )
            .get(n)
            .map(|b| Act::SelectBranch(b.clone()))
        }
        Tab::Environments => st
            .envs
            .ok()
            .and_then(|v| v.get(n))
            .map(|e| Act::SelectEnv(Some(e.name.clone()))),
        Tab::Secrets => config_view::scopes(repo)
            .get(n)
            .map(|s| Act::SetSecretScope(s.clone())),
        Tab::Variables => config_view::scopes(repo)
            .get(n)
            .map(|s| Act::SetVariableScope(s.clone())),
        Tab::Codeowners => None,
    };
    if let Some(a) = act {
        apply(cx, repo, a);
    }
}

/// Write an SBOM file.
fn export(cx: &mut Ctx, repo: &mut RepoState, format: &str, path: PathBuf) {
    let st = &repo.devops.govern;
    let version = env!("CARGO_PKG_VERSION");
    let (doc, what, packages) = match format {
        "cyclonedx" | "spdx" => {
            let Some(s) = st.sbom.ok() else {
                cx.toast(ToastKind::Error, "Read the lockfiles first");
                return;
            };
            let id = gcore::new_uuid();
            let now = gcore::iso_now();
            if format == "cyclonedx" {
                (
                    sbom::cyclonedx(&repo.name, &s.packages, &id, &now, version),
                    "CycloneDX 1.5",
                    s.unique().len(),
                )
            } else {
                (
                    sbom::spdx(&repo.name, &s.packages, &id, &now, version),
                    "SPDX 2.3",
                    s.unique().len(),
                )
            }
        }
        _ => {
            let Some(g) = st.gh_sbom.ok() else {
                cx.toast(ToastKind::Error, "Fetch GitHub's SBOM first");
                return;
            };
            (g.raw.clone(), "GitHub SPDX", g.packages.len())
        }
    };
    let text = match serde_json::to_string_pretty(&doc) {
        Ok(t) => t,
        Err(e) => {
            cx.toast(ToastKind::Error, format!("Could not build the SBOM: {e}"));
            return;
        }
    };
    let label = format!("Export {what} SBOM");
    cx.remote_change::<Msg>(label, move |_, log| {
        log.command(format!("write {} ({} bytes)", path.display(), text.len()));
        let r = std::fs::write(&path, text.as_bytes())
            .map(|_| {
                format!(
                    "{what} SBOM with {packages} packages saved to {} ({})",
                    path.display(),
                    crate::format::bytes(text.len())
                )
            })
            .map_err(|e| format!("Could not write {}: {e}", path.display()));
        (r, None)
    });
}

const TEMPLATE: &str = "\
# Code owners are requested for review when a pull request changes their files.
# The last matching pattern wins. Docs: https://docs.github.com/articles/about-code-owners
#
# Default owners for everything:
# *       @org/team
#
# A directory at the root and everything below it:
# /docs/  @org/docs-team
#
# Files by extension, anywhere:
# *.rs    @org/rust-team
";

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    let st = &mut repo.devops.govern;
    match msg {
        Msg::Access(r) => {
            st.access.finish(r.map(|b| *b));
            if st.tab == Tab::Protection && st.branch.is_empty() {
                choose_branch(cx, st);
            }
        }
        Msg::Dependabot(r) => {
            st.dependabot.finish(r);
            if let Some(n) = st.dep_selected
                && !st
                    .dependabot
                    .ok()
                    .is_some_and(|v| v.iter().any(|a| a.number == n))
            {
                st.dep_selected = None;
            }
        }
        Msg::Code(r) => {
            st.code.finish(r);
            st.snippets.clear();
            if let Some(n) = st.code_selected {
                st.code_selected = None;
                apply(cx, repo, Act::SelectAlert(AlertKind::Code, Some(n)));
            }
        }
        Msg::Snippet(n, s) => {
            st.snippets.insert(n, s);
        }
        Msg::Sbom(r) => {
            if st.dep_packages.is_none()
                && let Ok(s) = &r
            {
                st.dep_packages = Some(s.packages.clone());
            }
            st.sbom.finish(r.map(|b| *b));
        }
        Msg::GhSbom(r) => st.gh_sbom.finish(r.map(|b| *b)),
        Msg::Images(Err(e)) => st.images.finish(Err(e)),
        Msg::Images(Ok(r)) => {
            if let Some(p) = &st.image_selected
                && !r.dockerfiles.iter().any(|(d, _)| &d.path == p)
            {
                st.image_selected = None;
            }
            if st.scan_image.is_empty()
                && let Some(i) = r.images().first()
            {
                st.scan_image = i.clone();
            }
            st.images.finish(Ok(*r));
        }
        Msg::Scanners(v) => {
            if st.scan_with.is_none() {
                st.scan_with = v
                    .iter()
                    .find(|s| s.version.is_some())
                    .map(|s| s.scanner)
                    .or(Some(Scanner::Scout));
            }
            st.scanners.finish(Ok(v));
        }
        Msg::ScanParsed(id, r) => {
            if let Some(run) = st.scans.iter_mut().find(|r| r.id == id) {
                run.result = Some(r.map(|b| *b).map_err(ScanFailure::Failed));
            }
        }
        Msg::Protected(r) => {
            st.protected.finish(r);
            if st.tab == Tab::Protection && st.branch.is_empty() && !st.access.loading {
                choose_branch(cx, st);
            }
        }
        Msg::Protection(b, r) => {
            if b != st.prot_branch {
                return;
            }
            st.protection.finish(r.map(|o| o.map(|b| *b)));
            st.prot_edit = None;
        }
        Msg::Rules(b, r) => {
            if b == st.prot_branch {
                st.rules.finish(r);
            }
        }
        Msg::Rulesets(r) => st.rulesets.finish(r),
        Msg::CoFile(r) => {
            let ok = r.is_ok();
            match r {
                Ok(b) => {
                    let (f, tracked) = *b;
                    st.co.tracked = Some(tracked);
                    st.co.file.finish(Ok(f));
                }
                Err(e) => st.co.file.finish(Err(e)),
            }
            if ok && st.co.edit_when_loaded {
                apply(cx, repo, Act::EditCodeowners(true));
                return;
            }
            if ok && !st.co.editing {
                st.co.reparse();
                if st.co.file.ok().is_some_and(|f| f.path.is_some()) {
                    st.co.analysis = Default::default();
                    analyze_codeowners(cx, st);
                } else {
                    st.co.analysis = Default::default();
                }
            }
        }
        Msg::CoAnalysis(generation, r) => {
            st.co.analysis.loading = false;
            if generation == st.co.generation {
                st.co.analysis.finish(r.map(|b| *b));
                st.co.analysis_gen = generation;
            }
        }
        Msg::CoErrors(r) => st.co.errors.finish(r),
        Msg::Envs(r) => {
            st.envs.finish(r);
            if let Some(sel) = st.env_selected.clone() {
                let still = st
                    .envs
                    .ok()
                    .and_then(|v| v.iter().find(|e| e.name == sel))
                    .map(|e| e.policy == envs::PolicyMode::Custom);
                match still {
                    None => st.env_selected = None,
                    Some(true) => load_policies(cx, st, &sel),
                    Some(false) => {}
                }
            }
        }
        Msg::Policies(env, r) => st.policies.entry(env).or_default().finish(r),
        Msg::Resolved(r) => {
            let Some(gh) = gh_of(cx) else { return };
            let Some(edit) = &mut st.env_edit else { return };
            edit.resolving = false;
            match r {
                Err(e) => edit.error = e,
                Ok(ids) => {
                    let pairs: Vec<(envs::ReviewerKind, u64)> =
                        ids.iter().map(|(k, id, _)| (*k, *id)).collect();
                    let request = envs::environment_put(&gh, &edit.form, &pairs);
                    let name = edit.form.name.trim().to_owned();
                    let mut details = request_details(&request);
                    if !ids.is_empty() {
                        details.push(format!(
                            "Reviewer ids: {}",
                            ids.iter()
                                .map(|(_, id, h)| format!("{h} = {id}"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                    let create = edit.editing.is_none();
                    crate::devops::pages::explain::confirm(
                        cx,
                        if create {
                            format!("Create the environment {name}?")
                        } else {
                            format!("Save the environment {name}?")
                        },
                        "The PUT sets every protection rule of the environment at once: what is not in the body is removed.",
                        if create {
                            "Create environment"
                        } else {
                            "Save environment"
                        },
                        false,
                        details,
                        Act::SaveEnv { name, request }.into(),
                    );
                }
            }
        }
        Msg::Secrets(scope, r) => st.secrets.entry(scope).or_default().finish(r),
        Msg::WorkflowSecrets(v) => st.workflow_secrets.finish(Ok(v)),
        Msg::Variables(scope, r) => st.variables.entry(scope).or_default().finish(r),
        Msg::Changed(c) => match c {
            Changed::Dependabot => {
                st.dismiss = None;
                st.dependabot.loading = false;
                load_dependabot(cx, st);
            }
            Changed::Code => {
                st.dismiss = None;
                st.code.loading = false;
                load_code(cx, st);
            }
            Changed::Protection(b) => {
                st.prot_edit = None;
                st.protection.loading = false;
                st.protected.loading = false;
                load_protection(cx, st, &b);
                if st.protected.begin() {
                    cx.gh_read("Protected branches", |gh, log| {
                        Msg::Protected(
                            protection::fetch_protected_branches(gh, &mut log.calls).map_err(s),
                        )
                    });
                }
            }
            Changed::Codeowners => {
                st.co.editing = false;
                st.co.file.loading = false;
                load_codeowners(cx, st);
            }
            Changed::Envs => {
                st.env_edit = None;
                st.envs.loading = false;
                if let Some(e) = &st.env_selected {
                    st.policies.remove(e);
                }
                load_envs(cx, st);
            }
            Changed::Policies(env) => {
                st.policies.remove(&env);
                load_policies(cx, st, &env);
            }
            Changed::Secrets(scope) => {
                st.secrets.remove(&scope);
                load_secrets(cx, st, scope);
            }
            Changed::Variables(scope) => {
                st.variable_form = None;
                st.variables.remove(&scope);
                load_variables(cx, st, scope);
            }
        },
        Msg::ChangeFailed => {
            if let Some(e) = &mut st.env_edit {
                e.resolving = false;
            }
        }
    }
}

/// A console run ended: finish the image scan it belongs to.
pub fn on_console_done(cx: &mut Ctx, repo: &mut RepoState, run: &ConsoleRun) {
    let st = &mut repo.devops.govern;
    let Some(scan) = st.scans.iter_mut().find(|s| s.id == run.id) else {
        return;
    };
    if run.succeeded() {
        let (id, scanner, report) = (scan.id, scan.scanner, scan.report.clone());
        cx.read(
            format!("Read the {} report", scanner.label()),
            move |_, log| {
                log.command(format!("read {}", report.display()));
                let r = std::fs::read(&report)
                    .map_err(|e| format!("Could not read {}: {e}", report.display()))
                    .and_then(|b| scanner.parse(&b).map(Box::new).map_err(s));
                let _ = std::fs::remove_file(&report);
                Msg::ScanParsed(id, r)
            },
        );
        return;
    }
    let out = run.text();
    let tail = run.tail(12);
    scan.result = Some(Err(if run.cancelled() {
        ScanFailure::Failed("Stopped.".into())
    } else if scan.scanner == Scanner::Scout && docker::scout_needs_login(&out) {
        ScanFailure::NeedsLogin(tail)
    } else {
        ScanFailure::Failed(match &run.exit {
            Some(Err(e)) => e.clone(),
            _ => format!("{}\n{tail}", run.status_text()),
        })
    }));
    let _ = std::fs::remove_file(&scan.report);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_preview_puts_required_keys_first() {
        let v = serde_json::json!({"allow_deletions": false, "enforce_admins": null, "restrictions": null, "lock_branch": true});
        let s = ordered_preview(&v, &["restrictions", "enforce_admins", "missing"]);
        let r = s.find("\"restrictions\"").unwrap();
        let e = s.find("\"enforce_admins\"").unwrap();
        let a = s.find("\"allow_deletions\"").unwrap();
        assert!(r < e && e < a, "{s}");
        // Still the same JSON.
        let back: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(back, v);
    }
}
