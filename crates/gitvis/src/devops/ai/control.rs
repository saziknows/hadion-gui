//! The AI controller: gathering each request's material in the background,
//! the confirmation, the streamed run, filling in the answer, and the key.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use gitvis_core::devops::ai::budget;
use gitvis_core::devops::ai::inputs;
use gitvis_core::devops::ai::output;
use gitvis_core::devops::ai::prompts::{self, FailureInput, PrInput, SummaryInput};
use gitvis_core::devops::ai::redact::{self, Redactions};
use gitvis_core::devops::ai::request;
use gitvis_core::devops::ai::runner::{self, KeySource, Live};
use gitvis_core::devops::ai::{Failure, Model, Usage, cost, dollars, estimate_tokens};
use gitvis_core::devops::review::model::{PrDetail, PrFile, ReviewThread};
use gitvis_core::github::{Annotation, Job, WorkflowRun};
use gitvis_core::joblog::JobLog;

use super::{Act, Ask, KeyOp, Msg, Phase, Prepared, Reply, Target, session};
use crate::devops::ctx::Ctx;
use crate::state::{RepoState, ToastKind};

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::Start(t) => start(cx, repo, t),
        Act::Send { target, id } => send(cx, repo, &target, id),
        Act::SendPending => {
            let st = &repo.devops.ai;
            let pending = st.last_pending.clone().and_then(|t| {
                st.asks
                    .get(&t)
                    .filter(|a| matches!(a.phase, Phase::Confirming))
                    .map(|a| (t, a.id))
            });
            match pending {
                Some((t, id)) => send(cx, repo, &t, id),
                None => cx.toast(ToastKind::Info, "No request is waiting to be sent"),
            }
        }
        Act::Reconfirm(t) => {
            if let Some(a) = repo.devops.ai.asks.get(&t)
                && matches!(a.phase, Phase::Confirming)
            {
                confirm(cx, a);
            }
        }
        Act::Cancel(t) => cancel(repo, &t),
        Act::Dismiss(t) => {
            cancel(repo, &t);
            repo.devops.ai.asks.remove(&t);
            repo.devops.ai.collapsed.remove(&t);
        }
        Act::Use { target, id } => use_answer(cx, repo, &target, id, true),
        Act::ToggleCollapsed(t) => {
            let c = &mut repo.devops.ai.collapsed;
            if !c.remove(&t) {
                c.insert(t);
            }
        }
        Act::SeenJob { run, job } => {
            let failed = repo
                .gh_page
                .runs
                .get(&run)
                .and_then(|d| d.jobs().iter().find(|j| j.id == job))
                .map(job_failed);
            repo.devops.ai.job_failed.insert(job, failed);
        }
        Act::ExplainShownJob => {
            let run = repo.gh_page.selected_run;
            let job = run
                .and_then(|r| repo.gh_page.runs.get(&r))
                .and_then(|d| d.selected_job);
            match (run, job) {
                (Some(run), Some(job)) => start(cx, repo, Target::Job { run, job }),
                _ => cx.toast(
                    ToastKind::Info,
                    "Open a job's log on the Actions page first",
                ),
            }
        }
        Act::SummariseSelected => match repo.devops.review.selected {
            Some(n) => start(cx, repo, Target::Pr(n)),
            None => cx.toast(
                ToastKind::Info,
                "Open a pull request on the Review page first",
            ),
        },
        Act::ExplainHead => {
            let head = repo
                .snapshot
                .as_ref()
                .and_then(|s| s.head.target())
                .map(|o| o.to_string());
            match head {
                Some(sha) => start(cx, repo, Target::Commit(sha)),
                None => cx.toast(ToastKind::Info, "This repository has no commits yet"),
            }
        }
        Act::CheckKey => check_key(cx),
        Act::SaveKey(secret) => {
            session::with(|s| {
                s.key_busy = Some("Saving…");
                s.key_error = None;
            });
            let name = runner::key_name();
            let started = cx.read(
                "Claude: store the API key in the keychain",
                move |_, log| {
                    log.command(format!(
                        "keychain: write service gitvis, entry {name} (the value is not logged)"
                    ));
                    let result = runner::store_key(&secret.0);
                    drop(secret);
                    Msg::Key {
                        op: KeyOp::Save,
                        result,
                        curl: runner::curl_path(),
                    }
                },
            );
            if !started {
                session::with(|s| {
                    s.key_busy = None;
                    s.key_error = Some("Open a repository first: keychain work runs in a repository's background queue.".to_owned());
                });
            }
        }
        Act::RemoveKey => {
            cx.confirm(
                "Remove the stored Claude API key?",
                "gitvis deletes its keychain entry. AI requests then use ANTHROPIC_API_KEY if it is set, and otherwise stop working until you store a key again.",
                "Remove the key",
                true,
                vec![format!(
                    "Delete the keychain entry: service gitvis, entry {}",
                    runner::key_name()
                )],
                Act::RemoveKeyConfirmed.into(),
            );
        }
        Act::RemoveKeyConfirmed => {
            session::with(|s| {
                s.key_busy = Some("Removing…");
                s.key_error = None;
            });
            let name = runner::key_name();
            cx.read(
                "Claude: remove the API key from the keychain",
                move |_, log| {
                    log.command(format!("keychain: delete service gitvis, entry {name}"));
                    Msg::Key {
                        op: KeyOp::Remove,
                        result: runner::remove_key(),
                        curl: runner::curl_path(),
                    }
                },
            );
        }
        Act::SetModelOverride(m) => {
            session::with(|s| s.model_override = m);
            if let Some(m) = m {
                cx.toast(
                    ToastKind::Info,
                    format!("AI requests use {} for this session", m.label()),
                );
            }
        }
        Act::SetAskOverride(a) => session::with(|s| s.ask_override = a),
    }
}

/// Failed or timed out.
fn job_failed(j: &Job) -> bool {
    use gitvis_core::github::Outcome;
    matches!(j.outcome(), Outcome::Failure | Outcome::TimedOut)
}

fn check_key(cx: &mut Ctx) {
    session::with(|s| s.key = session::KeyState::Checking);
    let name = runner::key_name();
    let started = cx.read("Claude: look up the API key", move |_, log| {
        log.command(format!(
            "keychain: read service gitvis, entry {name} (only its last four characters leave the background task)"
        ));
        Msg::Key {
            op: KeyOp::Check,
            result: Ok(runner::key_status()),
            curl: runner::curl_path(),
        }
    });
    if !started {
        session::with(|s| s.key = session::KeyState::Unknown);
    }
}

fn cancel(repo: &mut RepoState, t: &Target) {
    let st = &mut repo.devops.ai;
    let Some(a) = st.asks.get(t) else {
        return;
    };
    match &a.phase {
        Phase::Running { cancel, .. } => cancel.store(true, Ordering::Relaxed),
        Phase::Preparing | Phase::Confirming => {
            st.asks.remove(t);
            if st.last_pending.as_ref() == Some(t) {
                st.last_pending = None;
            }
        }
        Phase::Done(_) => {}
    }
}

/// What a request needs from the repository state, cloned for the
/// background task.
enum Material {
    Staged,
    NewPr {
        base: String,
        head: String,
        base_ref: String,
        template: Option<(String, String)>,
    },
    Pr {
        detail: Box<PrDetail>,
        files: Option<Vec<PrFile>>,
        owner_repo: (String, String),
    },
    Job {
        run: Option<Box<WorkflowRun>>,
        job: Option<Box<Job>>,
        log: Option<Box<JobLog>>,
        annotations: Vec<Annotation>,
        owner_repo: (String, String),
        job_id: u64,
    },
    Commit(String),
}

fn gather(repo: &RepoState, t: &Target) -> Result<Material, String> {
    match t {
        Target::Wip => {
            let staged = repo
                .snapshot
                .as_ref()
                .is_some_and(|s| !s.status.staged.is_empty());
            if !staged {
                return Err("Nothing is staged: stage the changes to describe first.".to_owned());
            }
            Ok(Material::Staged)
        }
        Target::NewPr => {
            let f = repo
                .devops
                .review
                .new_pr
                .as_ref()
                .ok_or("Open the new pull request form first.")?;
            if f.head.trim().is_empty() {
                return Err("The form has no head branch.".to_owned());
            }
            let template = match (&f.templates, f.template) {
                (Some(Ok(list)), Some(i)) => list.get(i).map(|t| (t.path.clone(), t.body.clone())),
                _ => None,
            };
            Ok(Material::NewPr {
                base: f.base.clone(),
                head: f.head.clone(),
                base_ref: f.base_ref.clone(),
                template,
            })
        }
        Target::Pr(n) => {
            let pv = repo.devops.review.prs.get(n);
            let detail = pv.and_then(|p| p.detail.data.clone()).ok_or_else(|| {
                format!("Open pull request #{n} first, so its description is loaded.")
            })?;
            let owner_repo = repo
                .github_remote
                .clone()
                .ok_or("This repository has no GitHub remote.")?;
            Ok(Material::Pr {
                detail: Box::new(detail),
                files: pv.and_then(|p| p.files.data.clone()),
                owner_repo,
            })
        }
        Target::Job { run, job } => {
            let owner_repo = repo
                .github_remote
                .clone()
                .ok_or("This repository has no GitHub remote.")?;
            let detail = repo.gh_page.runs.get(run);
            let log = detail
                .and_then(|d| d.logs.get(job))
                .and_then(|l| l.doc.as_ref())
                .and_then(|d| d.as_ref().ok())
                .map(|d| Box::new(d.log.clone()));
            let annotations = detail
                .and_then(|d| d.annotations.get(job))
                .and_then(|a| a.as_ref().ok())
                .cloned()
                .unwrap_or_default();
            Ok(Material::Job {
                run: repo
                    .github
                    .as_ref()
                    .and_then(|g| g.runs.iter().find(|r| r.id == *run))
                    .cloned()
                    .map(Box::new),
                job: detail
                    .and_then(|d| d.jobs().iter().find(|j| j.id == *job))
                    .cloned()
                    .map(Box::new),
                log,
                annotations,
                owner_repo,
                job_id: *job,
            })
        }
        Target::Commit(sha) => Ok(Material::Commit(sha.clone())),
    }
}

/// Start a request: gather and redact in the background, then confirm.
fn start(cx: &mut Ctx, repo: &mut RepoState, target: Target) {
    if repo
        .devops
        .ai
        .asks
        .get(&target)
        .is_some_and(|a| a.running())
    {
        return;
    }
    let feature = target.feature();
    let prefs = cx.ext.ai.clone();
    let (model, effort, ask_first) = (prefs.model_now(), prefs.effort(feature), prefs.ask_now());
    let (diff_budget, log_budget) = (prefs.diff_budget(), prefs.log_budget());
    let st = &mut repo.devops.ai;
    st.next_id += 1;
    let id = st.next_id;
    let mut ask = Ask {
        id,
        target: target.clone(),
        model,
        effort,
        ask_first,
        phase: Phase::Preparing,
        prepared: None,
        started: Instant::now(),
        applied: false,
        unapplied: false,
    };
    let material = match gather(repo, &target) {
        Ok(m) => m,
        Err(e) => {
            ask.phase = Phase::Done(Err(Failure::Prepare(e)));
            repo.devops.ai.asks.insert(target, ask);
            return;
        }
    };
    repo.devops.ai.asks.insert(target.clone(), ask);
    let label = format!(
        "Claude: gather the material ({})",
        feature.label().to_lowercase()
    );
    let t = target.clone();
    let started = match material {
        Material::Staged => cx.read(label, move |dir, log| Msg::Prepared {
            target: t,
            id,
            result: preflight().and_then(|key| prepare_staged(dir, diff_budget, &mut log.commands, key)),
        }),
        Material::NewPr {
            base,
            head,
            base_ref,
            template,
        } => cx.read(label, move |dir, log| Msg::Prepared {
            target: t,
            id,
            result: preflight().and_then(|key| {
                prepare_new_pr(dir, &base, &head, &base_ref, template, &mut log.commands, key)
            }),
        }),
        Material::Commit(sha) => cx.read(label, move |dir, log| Msg::Prepared {
            target: t,
            id,
            result: preflight().and_then(|key| {
                let (input, m) = inputs::commit(dir, &sha, diff_budget, &mut log.commands)
                    .map_err(Failure::Prepare)?;
                let prompt = prompts::explain_commit(&input);
                Ok(Prepared {
                    sources: vec![
                        ("Commit".to_owned(), short(&sha).to_owned()),
                        ("Diff".to_owned(), m.fitted.summary()),
                        ("Message and stat".to_owned(), "full".to_owned()),
                    ],
                    files: m.fitted.included.clone(),
                    redactions: m.redactions,
                    prompt,
                    key,
                })
            }),
        }),
        Material::Pr {
            detail,
            files,
            owner_repo,
        } => cx.gh_read(label, move |gh, log| Msg::Prepared {
            target: t,
            id,
            result: preflight().and_then(|key| {
                let files = match files {
                    Some(f) => f,
                    None => gitvis_core::devops::review::fetch::fetch_pr_files(
                        gh,
                        detail.pr.number,
                        &mut log.calls,
                    )
                    .map_err(|e| Failure::Prepare(e.to_string()))?,
                };
                let _ = &owner_repo;
                Ok(prepare_summary(&detail, &files, diff_budget, key))
            }),
        }),
        Material::Job {
            run,
            job,
            log: job_log,
            annotations,
            owner_repo,
            job_id,
        } => cx.gh_read(label, move |gh, log| Msg::Prepared {
            target: t,
            id,
            result: preflight().and_then(|key| {
                let job_log = match job_log {
                    Some(l) => *l,
                    None => {
                        let raw = gitvis_core::github::fetch_job_log(
                            gh.dir(),
                            &owner_repo.0,
                            &owner_repo.1,
                            job_id,
                            &mut log.calls,
                        )
                        .map_err(|e| {
                            Failure::Prepare(format!(
                                "Could not load the job's log: {e}. Logs are available only after a job has finished."
                            ))
                        })?;
                        gitvis_core::joblog::parse(&raw)
                    }
                };
                Ok(prepare_failure(
                    gh.dir(),
                    run.as_deref(),
                    job.as_deref(),
                    &job_log,
                    &annotations,
                    log_budget,
                    &mut log.commands,
                    key,
                ))
            }),
        }),
    };
    if !started && let Some(a) = repo.devops.ai.asks.get_mut(&target) {
        a.phase = Phase::Done(Err(Failure::Prepare(
            "This needs a GitHub remote (and gh signed in).".to_owned(),
        )));
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

/// Key and curl, before any material is read.
fn preflight() -> Result<KeySource, Failure> {
    if runner::curl_path().is_none() {
        return Err(Failure::NoCurl);
    }
    runner::key_status().source().ok_or(Failure::NoKey)
}

fn prepare_staged(
    dir: &Path,
    budget: usize,
    commands: &mut Vec<String>,
    key: KeySource,
) -> Result<Prepared, Failure> {
    let (input, m) = inputs::staged(dir, budget, commands).map_err(Failure::Prepare)?;
    let prompt = prompts::commit_message(&input);
    Ok(Prepared {
        sources: vec![
            ("Staged diff".to_owned(), m.fitted.summary()),
            (
                "Staged files".to_owned(),
                format!("{} (names and status)", input.files.lines().count()),
            ),
            (
                "Diff stat".to_owned(),
                "git diff --cached --stat".to_owned(),
            ),
            (
                "Recent subjects".to_owned(),
                format!("{} (to match the style)", input.recent_subjects.len()),
            ),
            ("Branch".to_owned(), input.branch.clone()),
        ],
        files: m.fitted.included.clone(),
        redactions: m.redactions,
        prompt,
        key,
    })
}

#[allow(clippy::too_many_arguments)]
fn prepare_new_pr(
    dir: &Path,
    base: &str,
    head: &str,
    base_ref: &str,
    template: Option<(String, String)>,
    commands: &mut Vec<String>,
    key: KeySource,
) -> Result<Prepared, Failure> {
    let (base_ref, range_note) =
        inputs::resolve_base(dir, base, base_ref, head, commands).map_err(Failure::Prepare)?;
    let commits =
        inputs::commits_between(dir, &base_ref, head, commands).map_err(Failure::Prepare)?;
    let stat = inputs::stat_between(dir, &base_ref, head, commands).unwrap_or_default();
    let mut r = Redactions::default();
    let commits: Vec<(String, String)> = commits
        .into_iter()
        .map(|(s, b)| {
            let (s, r1) = redact::text(&s, "COMMIT_EDITMSG");
            let (b, r2) = redact::text(&b, "COMMIT_EDITMSG");
            r.merge(&r1);
            r.merge(&r2);
            (s, b)
        })
        .collect();
    let template = template.map(|(p, b)| {
        let (b, r3) = redact::text(&b, &p);
        r.merge(&r3);
        (p, b)
    });
    let n = commits.len();
    let input = PrInput {
        head: head.to_owned(),
        base: base.to_owned(),
        commits,
        stat,
        template: template.clone(),
        note: String::new(),
    };
    let prompt = prompts::pr_description(&input);
    let mut sources = vec![
        (
            "Commits".to_owned(),
            format!("{n} ({range_note}), subjects and bodies"),
        ),
        (
            "Diff stat".to_owned(),
            format!(
                "git diff --stat {}...{head}",
                match base_ref.strip_suffix('^') {
                    // The parent of the branch's oldest own commit.
                    Some(sha) if sha.len() >= 40 => format!("{}^", &sha[..10]),
                    _ => base_ref.clone(),
                }
            ),
        ),
    ];
    if let Some((p, _)) = &template {
        sources.push(("Template".to_owned(), p.clone()));
    }
    Ok(Prepared {
        sources,
        files: Vec::new(),
        redactions: r,
        prompt,
        key,
    })
}

/// A pull request's files as one unified diff.
fn pr_diff(files: &[PrFile]) -> String {
    let mut d = String::new();
    for f in files {
        let old = f.previous.as_deref().unwrap_or(&f.path);
        d.push_str(&format!("diff --git a/{old} b/{}\n", f.path));
        match &f.patch {
            Some(p) => {
                d.push_str(&format!("--- a/{old}\n+++ b/{}\n", f.path));
                d.push_str(p);
                if !p.ends_with('\n') {
                    d.push('\n');
                }
            }
            None => d.push_str(&format!(
                "Binary files a/{old} and b/{} differ (or the diff is too large for the API)\n",
                f.path
            )),
        }
    }
    d
}

fn thread_text(t: &ReviewThread) -> String {
    let state = if t.resolved {
        "resolved"
    } else if t.outdated {
        "outdated"
    } else {
        "open"
    };
    let line = t
        .line
        .or(t.original_line)
        .map(|l| format!(":{l}"))
        .unwrap_or_default();
    let mut s = format!("{}{line} ({state})", t.path);
    for c in t.comments.iter().take(8) {
        let body: String = c.body.chars().take(700).collect();
        s.push_str(&format!(
            "\n  @{}: {}",
            c.author,
            body.replace('\n', "\n    ")
        ));
    }
    s
}

fn prepare_summary(detail: &PrDetail, files: &[PrFile], budget: usize, key: KeySource) -> Prepared {
    let pr = &detail.pr;
    let m = inputs::prepare_diff(&pr_diff(files), budget);
    let (body, _) = budget::cut_text(&detail.body, 16 * 1024);
    let (body, mut r) = redact::text(&body, "pr-body.md");
    r.merge(&m.redactions);
    let mut threads = Vec::new();
    for t in detail.threads.iter().take(30) {
        let (s, r2) = redact::text(&thread_text(t), "review-thread.md");
        r.merge(&r2);
        threads.push(s);
    }
    let input = SummaryInput {
        number: pr.number,
        title: pr.title.clone(),
        author: pr.author.clone(),
        head: pr.head.clone(),
        base: pr.base.clone(),
        body,
        threads: threads.clone(),
        diff: m.text.clone(),
        diff_note: m.fitted.summary(),
    };
    let prompt = prompts::summarise_pr(&input);
    Prepared {
        sources: vec![
            (
                "Pull request".to_owned(),
                format!("#{} title, author, branches, description", pr.number),
            ),
            ("Diff".to_owned(), m.fitted.summary()),
            (
                "Review threads".to_owned(),
                if threads.is_empty() {
                    "none loaded".to_owned()
                } else {
                    format!("{} of {}", threads.len(), detail.threads.len())
                },
            ),
        ],
        files: m.fitted.included.clone(),
        redactions: r,
        prompt,
        key,
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_failure(
    dir: &Path,
    run: Option<&WorkflowRun>,
    job: Option<&Job>,
    log: &JobLog,
    annotations: &[Annotation],
    budget: usize,
    commands: &mut Vec<String>,
    key: KeySource,
) -> Prepared {
    let ex = budget::log_excerpt(log, 40, budget);
    let (log_text, mut r) = redact::text(&ex.text, "ci.log");
    let steps: Vec<String> = job
        .map(|j| {
            j.steps
                .iter()
                .map(|s| {
                    format!(
                        "{}. {}: {}",
                        s.number,
                        s.name,
                        s.conclusion.as_deref().unwrap_or(&s.status)
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let annotations: Vec<String> = annotations
        .iter()
        .take(30)
        .map(|a| {
            let (m, r2) = redact::text(
                &format!(
                    "{} {}:{} {} {}",
                    a.level, a.path, a.start_line, a.title, a.message
                ),
                "annotation.txt",
            );
            r.merge(&r2);
            m
        })
        .collect();
    let job_name = job.map(|j| j.name.clone()).unwrap_or_default();
    let excerpt = run
        .and_then(|r| inputs::workflow_file(dir, &r.sha, &r.path, commands))
        .map(|text| inputs::workflow_excerpt(&text, &job_name, 220))
        .unwrap_or_default();
    let (excerpt, r3) = redact::text(&excerpt, ".github/workflows/ci.yml");
    r.merge(&r3);
    let input = FailureInput {
        workflow: run.map(|r| r.name.clone()).unwrap_or_default(),
        workflow_path: run.map(|r| r.path.clone()).unwrap_or_default(),
        job: job_name.clone(),
        run_title: run
            .map(|r| {
                if r.head_message.is_empty() {
                    r.title.clone()
                } else {
                    r.head_message.lines().next().unwrap_or_default().to_owned()
                }
            })
            .unwrap_or_default(),
        branch: run.map(|r| r.branch.clone()).unwrap_or_default(),
        event: run.map(|r| r.event.clone()).unwrap_or_default(),
        steps,
        annotations: annotations.clone(),
        workflow_excerpt: excerpt.clone(),
        log: log_text,
        log_note: ex.summary(),
    };
    let prompt = prompts::explain_failure(&input);
    let mut sources = vec![
        ("Log excerpt".to_owned(), ex.summary()),
        (
            "Steps".to_owned(),
            format!("{} with their conclusions", input.steps.len()),
        ),
        (
            "Annotations".to_owned(),
            if annotations.is_empty() {
                "none".to_owned()
            } else {
                annotations.len().to_string()
            },
        ),
    ];
    sources.push((
        "Workflow file".to_owned(),
        if excerpt.is_empty() {
            "not found locally".to_owned()
        } else {
            format!(
                "{} ({} lines: header and this job)",
                input.workflow_path,
                excerpt.lines().count()
            )
        },
    ));
    sources.push(("Commit subject".to_owned(), input.run_title.clone()));
    Prepared {
        sources,
        files: ex
            .windows
            .iter()
            .map(|(a, b)| format!("log lines {a}–{b}"))
            .collect(),
        redactions: r,
        prompt,
        key,
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    match msg {
        Msg::Prepared { target, id, result } => {
            let st = &mut repo.devops.ai;
            let Some(a) = st.asks.get_mut(&target).filter(|a| a.id == id) else {
                return; // cancelled or replaced meanwhile
            };
            match result {
                Err(f) => a.phase = Phase::Done(Err(f)),
                Ok(p) => {
                    a.prepared = Some(p);
                    if a.ask_first {
                        a.phase = Phase::Confirming;
                        st.last_pending = Some(target.clone());
                        if let Some(a) = st.asks.get(&target) {
                            confirm(cx, a);
                        }
                    } else {
                        send(cx, repo, &target, id);
                    }
                }
            }
        }
        Msg::Finished {
            target,
            id,
            feature,
            result,
        } => {
            let Some(a) = repo.devops.ai.asks.get_mut(&target).filter(|a| a.id == id) else {
                return;
            };
            // What streamed before a failure still counts.
            let live_usage: Option<(String, Usage)> = match &a.phase {
                Phase::Running { live, .. } => {
                    let l = live.lock().unwrap_or_else(|e| e.into_inner());
                    Some((l.answer.model.clone(), l.answer.usage))
                }
                _ => None,
            };
            match &result {
                Ok(d) => {
                    session::record(
                        &d.answer.model,
                        a.model,
                        feature,
                        &d.answer.usage,
                        !d.answer.refused(),
                    );
                }
                Err(Failure::NoKey | Failure::NoCurl | Failure::Prepare(_)) => {}
                Err(_) => {
                    let (m, u) = live_usage.unwrap_or_default();
                    session::record(&m, a.model, feature, &u, false);
                }
            }
            let requested = a.model;
            let priced = |d: &runner::Done| {
                Model::parse(&d.answer.model)
                    .map(|m| cost(m, &d.answer.usage))
                    .unwrap_or_else(|| cost(requested, &d.answer.usage))
            };
            let no_key = matches!(result, Err(Failure::NoKey));
            a.phase = Phase::Done(result.map(|done| Reply {
                cost: priced(&done),
                done,
            }));
            let fill = a
                .reply()
                .is_some_and(|r| !r.done.answer.refused() && !r.done.answer.hit_max_tokens())
                && matches!(target, Target::Wip | Target::NewPr);
            if fill {
                use_answer(cx, repo, &target, id, false);
            }
            if no_key {
                session::with(|s| s.key = session::KeyState::Unknown);
            }
        }
        Msg::Key { op, result, curl } => {
            session::with(|s| {
                s.key_busy = None;
                s.curl = Some(curl);
                match &result {
                    Ok(k) => {
                        s.key = session::KeyState::Known(k.clone());
                        s.key_error = None;
                    }
                    Err(e) => {
                        s.key_error = Some(e.clone());
                        if op == KeyOp::Check {
                            s.key = session::KeyState::Unknown;
                        }
                    }
                }
            });
            match (&result, op) {
                (Ok(_), KeyOp::Save) => {
                    cx.toast(ToastKind::Success, "Claude API key stored in the keychain")
                }
                (Ok(_), KeyOp::Remove) => cx.toast(ToastKind::Info, "Claude API key removed"),
                _ => {}
            }
        }
    }
}

/// The confirmation: what is sent, where, how big, what was redacted.
fn confirm(cx: &mut Ctx, a: &Ask) {
    let Some(p) = &a.prepared else {
        return;
    };
    let chars = p.prompt.chars();
    let tokens = estimate_tokens(chars);
    let est = cost(
        a.model,
        &Usage {
            input_tokens: tokens,
            ..Usage::default()
        },
    );
    let n = p.redactions.values();
    let redacted = if p.redactions.is_empty() {
        "The secret scanner found nothing to redact.".to_owned()
    } else {
        format!("Redacted before sending: {}.", p.redactions.summary())
    };
    let body = format!(
        "{} with {} at {} effort. About {} tokens ({} characters, input about {}) go to api.anthropic.com. {redacted}",
        a.feature().action(),
        a.model.label(),
        a.effort.as_str(),
        crate::format::count(tokens as usize),
        crate::format::count(chars),
        dollars(est),
    );
    let mut details = vec![
        format!("POST {}", request::API_URL),
        format!(
            "model {} · effort {} · max_tokens {} · stream{}",
            a.model.id(),
            a.effort.as_str(),
            request::MAX_TOKENS,
            if a.model.supports_fallbacks() {
                " · fallbacks default"
            } else {
                ""
            }
        ),
        format!("API key: from {} (never shown)", p.key.label()),
        format!(
            "size: {} characters · about {} tokens · input about {}",
            crate::format::count(chars),
            crate::format::count(tokens as usize),
            dollars(est)
        ),
    ];
    for (what, detail) in &p.sources {
        details.push(format!("{what}: {detail}"));
    }
    details.push(format!(
        "redacted: {}",
        if n == 0 && p.redactions.withheld.is_empty() {
            "nothing found".to_owned()
        } else {
            p.redactions.summary()
        }
    ));
    if !p.files.is_empty() {
        let shown: Vec<&str> = p.files.iter().take(40).map(String::as_str).collect();
        let more = p.files.len().saturating_sub(shown.len());
        details.push(format!(
            "included: {}{}",
            shown.join(", "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        ));
    }
    details.push(request::curl_display());
    details.push(String::new());
    details.push("The message as sent (start):".to_owned());
    let preview: String = p.prompt.user.chars().take(2_500).collect();
    details.push(preview);
    let rest = p.prompt.user.chars().count().saturating_sub(2_500);
    if rest > 0 {
        details.push(format!("… {} more characters", crate::format::count(rest)));
    }
    cx.confirm(
        "Send to Claude?",
        body,
        "Send",
        false,
        details,
        Act::Send {
            target: a.target.clone(),
            id: a.id,
        }
        .into(),
    );
}

/// Run the request (after the confirmation, or right away).
fn send(cx: &mut Ctx, repo: &mut RepoState, target: &Target, id: u64) {
    let st = &mut repo.devops.ai;
    let Some(a) = st.asks.get_mut(target).filter(|a| a.id == id) else {
        return;
    };
    if !matches!(a.phase, Phase::Confirming | Phase::Preparing) {
        return;
    }
    let Some(p) = a.prepared.as_ref() else {
        return;
    };
    if st.last_pending.as_ref() == Some(target) {
        st.last_pending = None;
    }
    let live = Arc::new(Mutex::new(Live::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let (model, effort, feature) = (a.model, a.effort, a.feature());
    let prompt = p.prompt.clone();
    a.phase = Phase::Running {
        live: live.clone(),
        cancel: stop.clone(),
    };
    let t = target.clone();
    let label = format!("Claude: {}", feature.action().to_lowercase());
    let started = cx.read(label, move |dir, log| {
        log.command(request::curl_display());
        let result = runner::send(dir, model, effort, &prompt, &stop, &live);
        Msg::Finished {
            target: t,
            id,
            feature,
            result,
        }
    });
    if !started && let Some(a) = repo.devops.ai.asks.get_mut(target) {
        a.phase = Phase::Done(Err(Failure::Prepare("No repository is open.".to_owned())));
    }
}

/// Fill in an answer: right away when the field is empty (or `force`),
/// otherwise after "Replace your message?".
fn use_answer(cx: &mut Ctx, repo: &mut RepoState, target: &Target, id: u64, force: bool) {
    let Some(a) = repo.devops.ai.asks.get(target).filter(|a| a.id == id) else {
        return;
    };
    let Some(reply) = a.reply() else {
        return;
    };
    let text = reply.done.answer.text.clone();
    match target {
        Target::Wip => {
            let m = output::commit_message(&text);
            if m.message.is_empty() {
                return;
            }
            let current = repo.commit_message.trim().to_owned();
            if force || current.is_empty() || current == m.message.trim() {
                repo.commit_message = m.message;
                mark(repo, target, true);
            } else {
                mark(repo, target, false);
                cx.confirm(
                    "Replace your message?",
                    format!(
                        "Claude wrote a commit message. Your current message ({} characters) will be replaced and is not kept.",
                        crate::format::count(current.chars().count())
                    ),
                    "Replace",
                    false,
                    m.message.lines().map(str::to_owned).collect(),
                    Act::Use {
                        target: target.clone(),
                        id,
                    }
                    .into(),
                );
            }
        }
        Target::NewPr => {
            let (title, body) = output::pr_description(&text);
            let Some(f) = repo.devops.review.new_pr.as_mut() else {
                mark(repo, target, false);
                return;
            };
            let template_body = match (&f.templates, f.template) {
                (Some(Ok(list)), Some(i)) => list.get(i).map(|t| t.body.trim().to_owned()),
                _ => None,
            };
            let commits_body = match &f.commits {
                Some(Ok(c)) => gitvis_core::devops::review::local::body_from_commits(c)
                    .trim()
                    .to_owned(),
                _ => String::new(),
            };
            let b = f.body.trim();
            let body_free = b.is_empty()
                || template_body.as_deref() == Some(b)
                || (!commits_body.is_empty() && b == commits_body);
            let title_free = f.title.trim().is_empty() || !f.title_edited;
            if force || (body_free && title_free) {
                if !title.is_empty() {
                    f.title = title;
                    f.title_edited = true;
                }
                f.body = body;
                mark(repo, target, true);
            } else {
                mark(repo, target, false);
                let mut details = vec![format!("Title: {title}"), String::new()];
                details.extend(body.lines().map(str::to_owned));
                cx.confirm(
                    "Replace the title and description?",
                    "Claude wrote a title and description. What you typed in the form is replaced and not kept.",
                    "Replace",
                    false,
                    details,
                    Act::Use {
                        target: target.clone(),
                        id,
                    }
                    .into(),
                );
            }
        }
        _ => {}
    }
}

fn mark(repo: &mut RepoState, target: &Target, applied: bool) {
    if let Some(a) = repo.devops.ai.asks.get_mut(target) {
        a.applied = applied;
        a.unapplied = !applied;
    }
}

/// Seconds a retryable error asks to wait, counted from when it arrived.
pub fn retry_wait(a: &Ask) -> Option<u64> {
    let Phase::Done(Err(Failure::Api(e))) = &a.phase else {
        return None;
    };
    let after = e.retry_after?;
    let waited = a.started.elapsed().as_secs();
    (after > waited).then(|| after - waited)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_files_become_a_diff() {
        let files = vec![
            PrFile {
                sha: "1".into(),
                path: "src/a.rs".into(),
                previous: None,
                status: Default::default(),
                additions: 1,
                deletions: 0,
                patch: Some("@@ -1 +1 @@\n-x\n+y".into()),
            },
            PrFile {
                sha: "2".into(),
                path: "logo.png".into(),
                previous: Some("old.png".into()),
                status: Default::default(),
                additions: 0,
                deletions: 0,
                patch: None,
            },
        ];
        let d = pr_diff(&files);
        assert!(d.starts_with("diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-x\n+y\n"));
        assert!(d.contains("diff --git a/old.png b/logo.png\nBinary files"));
        let split = budget::split_diff(&d);
        assert_eq!(split.len(), 2);
        assert!(split[1].binary);
    }
}
