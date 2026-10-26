//! Background threads: one serial reader for libgit2 work, one thread per
//! git operation, clone or GitHub request.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;

use gitvis_core::github::{
    AdminSurface, Annotation, ApiCall, CheckRun, CodeAlert, DependabotAlert, DorkResult, Fork,
    GitHubData, Job as GhJob, PrDetail, RepoInfo, SecretAlert, WorkflowRun,
};
use gitvis_core::insights::debt::{Dated, Marker};
use gitvis_core::insights::health::Weight;
use gitvis_core::insights::history::FileBlame;
use gitvis_core::insights::recover::Lost;
use gitvis_core::insights::search::{SearchQuery, SearchResult};
use gitvis_core::insights::{self, InsightsOptions, InsightsReport};
use gitvis_core::joblog::JobLog;
use gitvis_core::ops::{GitOp, Refresh};
use gitvis_core::osint::{self, OsintOptions, OsintReport, Package, PrRefs, Unreachable, Vuln};
use gitvis_core::reflog::ReflogEntry;
use gitvis_core::secrets::{self, Progress, Report, ScanOptions};
use gitvis_core::{
    CommitDetails, DiffOptions, DiffTarget, LoadOptions, Oid, Pending, Snapshot, WorkingTree, cli,
    github, joblog, reflog, repo,
};

use crate::state::{DiffDoc, RepoId};

pub enum Job {
    Load {
        repo: RepoId,
        path: PathBuf,
        opts: LoadOptions,
        generation: u64,
    },
    Status {
        repo: RepoId,
        path: PathBuf,
        generation: u64,
    },
    Details {
        repo: RepoId,
        path: PathBuf,
        id: Oid,
    },
    Diff {
        repo: RepoId,
        path: PathBuf,
        target: DiffTarget,
        opts: DiffOptions,
        dark: bool,
        generation: u64,
    },
    Reflog {
        repo: RepoId,
        path: PathBuf,
    },
    /// `git branch -a --contains`.
    Contains {
        repo: RepoId,
        path: PathBuf,
        id: Oid,
    },
}

pub enum Event {
    Loaded {
        repo: RepoId,
        generation: u64,
        elapsed_ms: u64,
        result: Result<Box<Snapshot>, String>,
    },
    Status {
        repo: RepoId,
        generation: u64,
        elapsed_ms: u64,
        result: Result<(WorkingTree, Pending), String>,
    },
    Details {
        repo: RepoId,
        id: Oid,
        elapsed_ms: u64,
        result: Result<CommitDetails, String>,
    },
    Diff {
        repo: RepoId,
        generation: u64,
        elapsed_ms: u64,
        target: DiffTarget,
        command: Option<String>,
        result: Result<Box<DiffDoc>, String>,
    },
    Reflog {
        repo: RepoId,
        entries: Vec<ReflogEntry>,
    },
    Contains {
        repo: RepoId,
        id: Oid,
        elapsed_ms: u64,
        result: Result<Vec<String>, String>,
    },
    GitHub {
        repo: RepoId,
        calls: Vec<ApiCall>,
        result: Result<Box<GitHubData>, String>,
    },
    /// Jobs of a run; `run` is set when the run itself had to be fetched.
    Jobs {
        repo: RepoId,
        run_id: u64,
        run: Option<WorkflowRun>,
        calls: Vec<ApiCall>,
        result: Result<Vec<GhJob>, String>,
    },
    JobLog {
        repo: RepoId,
        run_id: u64,
        job_id: u64,
        calls: Vec<ApiCall>,
        result: Result<Box<JobLog>, String>,
    },
    Annotations {
        repo: RepoId,
        run_id: u64,
        job_id: u64,
        calls: Vec<ApiCall>,
        result: Result<Vec<Annotation>, String>,
    },
    Checks {
        repo: RepoId,
        sha: Oid,
        calls: Vec<ApiCall>,
        result: Result<Vec<CheckRun>, String>,
    },
    PrDetail {
        repo: RepoId,
        number: u64,
        calls: Vec<ApiCall>,
        result: Result<PrDetail, String>,
    },
    /// Progress of a running secrets scan.
    ScanProgress {
        repo: RepoId,
        generation: u64,
        progress: Progress,
    },
    ScanDone {
        repo: RepoId,
        generation: u64,
        elapsed_ms: u64,
        result: Result<Box<Report>, String>,
    },
    /// Quick scan of staged, unstaged and untracked changes.
    QuickScan {
        repo: RepoId,
        generation: u64,
        elapsed_ms: u64,
        result: Result<Box<Report>, String>,
    },
    /// Repository visibility and secret-scanning alerts from GitHub.
    Exposure {
        repo: RepoId,
        calls: Vec<ApiCall>,
        info: Result<RepoInfo, String>,
        alerts: Result<Vec<SecretAlert>, String>,
    },
    OsintProgress {
        repo: RepoId,
        generation: u64,
        progress: Progress,
    },
    OsintDone {
        repo: RepoId,
        generation: u64,
        elapsed_ms: u64,
        result: Result<Box<OsintReport>, String>,
    },
    UnreachableProgress {
        repo: RepoId,
        progress: Progress,
    },
    UnreachableDone {
        repo: RepoId,
        elapsed_ms: u64,
        result: Result<Box<Unreachable>, String>,
    },
    OsvProgress {
        repo: RepoId,
        progress: Progress,
    },
    OsvDone {
        repo: RepoId,
        elapsed_ms: u64,
        packages: usize,
        result: Result<Vec<Vuln>, String>,
    },
    GhSurface {
        repo: RepoId,
        calls: Vec<ApiCall>,
        forks: Result<Vec<Fork>, String>,
        dependabot: Result<Vec<DependabotAlert>, String>,
        code_alerts: Result<Vec<CodeAlert>, String>,
        admin: Box<AdminSurface>,
    },
    Dorks {
        repo: RepoId,
        calls: Vec<ApiCall>,
        results: Vec<DorkResult>,
    },
    Served {
        repo: RepoId,
        calls: Vec<ApiCall>,
        sha: Oid,
        result: Result<bool, String>,
    },
    PrRefs {
        repo: RepoId,
        elapsed_ms: u64,
        result: Result<PrRefs, String>,
    },
    InsightsProgress {
        repo: RepoId,
        generation: u64,
        progress: Progress,
    },
    InsightsDone {
        repo: RepoId,
        generation: u64,
        elapsed_ms: u64,
        result: Result<Box<InsightsReport>, String>,
    },
    WeightProgress {
        repo: RepoId,
        progress: Progress,
    },
    WeightDone {
        repo: RepoId,
        elapsed_ms: u64,
        result: Result<Box<Weight>, String>,
    },
    DatingProgress {
        repo: RepoId,
        progress: Progress,
    },
    DatingDone {
        repo: RepoId,
        elapsed_ms: u64,
        result: Result<Box<Dated>, String>,
    },
    BlameDone {
        repo: RepoId,
        path: String,
        elapsed_ms: u64,
        result: Result<FileBlame, String>,
    },
    SearchDone {
        repo: RepoId,
        elapsed_ms: u64,
        result: Result<Box<SearchResult>, String>,
    },
    LostProgress {
        repo: RepoId,
        progress: Progress,
    },
    LostDone {
        repo: RepoId,
        elapsed_ms: u64,
        result: Result<Box<Lost>, String>,
    },
    OpDone {
        repo: RepoId,
        label: String,
        refresh: Refresh,
        network: bool,
        remote_only: bool,
        commands: Vec<String>,
        elapsed_ms: u64,
        result: Result<String, String>,
    },
    Watch {
        repo: RepoId,
        full: bool,
    },
    CloneDone {
        dest: PathBuf,
        command: String,
        elapsed_ms: u64,
        result: Result<String, String>,
    },
}

pub struct Worker {
    jobs: Sender<Job>,
    events: Sender<Event>,
    pub inbox: Receiver<Event>,
    ctx: egui::Context,
}

/// `git push origin "a b"` style rendering of a command for the log.
pub fn render_command(program: &str, args: &[String]) -> String {
    let mut s = program.to_owned();
    for a in args {
        s.push(' ');
        if a.is_empty() || a.contains(|c: char| c.is_whitespace() || c == '"') {
            s.push('"');
            s.push_str(&a.replace('"', "\\\""));
            s.push('"');
        } else {
            s.push_str(a);
        }
    }
    s
}

/// A repository's GitHub coordinates, cloned into worker threads.
#[derive(Clone)]
pub struct GhRepo {
    pub repo: RepoId,
    pub workdir: PathBuf,
    pub owner: String,
    pub name: String,
}

impl Worker {
    pub fn new(ctx: egui::Context) -> Self {
        let (jobs, job_rx) = channel::<Job>();
        let (events, inbox) = channel::<Event>();
        let tx = events.clone();
        let thread_ctx = ctx.clone();
        std::thread::Builder::new()
            .name("gitvis-reader".into())
            .spawn(move || reader_loop(job_rx, tx, thread_ctx))
            .expect("spawn reader thread");
        Self {
            jobs,
            events,
            inbox,
            ctx,
        }
    }

    pub fn send(&self, job: Job) {
        let _ = self.jobs.send(job);
    }

    pub fn event_sender(&self) -> (Sender<Event>, egui::Context) {
        (self.events.clone(), self.ctx.clone())
    }

    /// Run `f` on a fresh thread and deliver the event it produces.
    fn spawn(&self, name: &str, f: impl FnOnce() -> Event + Send + 'static) {
        let tx = self.events.clone();
        let ctx = self.ctx.clone();
        std::thread::Builder::new()
            .name(format!("gitvis-{name}"))
            .spawn(move || {
                let _ = tx.send(f());
                ctx.request_repaint();
            })
            .expect("spawn worker thread");
    }

    /// `git clone url dest` on its own thread; `dest` must not exist yet.
    pub fn run_clone(&self, url: String, dest: PathBuf) {
        self.spawn("clone", move || {
            let parent = dest
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            let args = [
                "clone".to_owned(),
                "--".to_owned(),
                url,
                dest.to_string_lossy().into_owned(),
            ];
            let command = render_command("git", &args);
            let started = Instant::now();
            let result = cli::run(&parent, &args, None)
                .map(|o| o.stderr)
                .map_err(|e| e.to_string());
            Event::CloneDone {
                dest,
                command,
                elapsed_ms: started.elapsed().as_millis() as u64,
                result,
            }
        });
    }

    pub fn run_op(&self, repo: RepoId, workdir: PathBuf, op: GitOp) {
        self.spawn("op", move || {
            let label = op.label();
            let refresh = op.refresh();
            let network = op.is_network();
            let remote_only = op.is_remote_only();
            let program = op.program();
            let commands = op
                .commands()
                .iter()
                .map(|c| render_command(program, c))
                .collect();
            let started = Instant::now();
            let result = op.run(&workdir).map_err(|e| e.to_string());
            Event::OpDone {
                repo,
                label,
                refresh,
                network,
                remote_only,
                commands,
                elapsed_ms: started.elapsed().as_millis() as u64,
                result,
            }
        });
    }

    /// Pull requests, workflow runs and workflows through `gh`.
    pub fn fetch_github(&self, gh: GhRepo) {
        self.spawn("github", move || {
            let mut calls = Vec::new();
            let result = github::fetch(&gh.workdir, &gh.owner, &gh.name, &mut calls)
                .map(Box::new)
                .map_err(|e| e.to_string());
            Event::GitHub {
                repo: gh.repo,
                calls,
                result,
            }
        });
    }

    /// Jobs (and steps) of a run; the run itself too when `need_run`.
    pub fn fetch_jobs(&self, gh: GhRepo, run_id: u64, need_run: bool) {
        self.spawn("gh-jobs", move || {
            let mut calls = Vec::new();
            let run = need_run
                .then(|| {
                    github::fetch_run(&gh.workdir, &gh.owner, &gh.name, run_id, &mut calls).ok()
                })
                .flatten();
            let result = github::fetch_jobs(&gh.workdir, &gh.owner, &gh.name, run_id, &mut calls)
                .map_err(|e| e.to_string());
            Event::Jobs {
                repo: gh.repo,
                run_id,
                run,
                calls,
                result,
            }
        });
    }

    /// One job's log, parsed off the UI thread.
    pub fn fetch_job_log(&self, gh: GhRepo, run_id: u64, job_id: u64) {
        self.spawn("gh-log", move || {
            let mut calls = Vec::new();
            let result =
                github::fetch_job_log(&gh.workdir, &gh.owner, &gh.name, job_id, &mut calls)
                    .map(|raw| Box::new(joblog::parse(&raw)))
                    .map_err(|e| e.to_string());
            Event::JobLog {
                repo: gh.repo,
                run_id,
                job_id,
                calls,
                result,
            }
        });
    }

    pub fn fetch_annotations(&self, gh: GhRepo, run_id: u64, job_id: u64) {
        self.spawn("gh-annotations", move || {
            let mut calls = Vec::new();
            let result =
                github::fetch_annotations(&gh.workdir, &gh.owner, &gh.name, job_id, &mut calls)
                    .map_err(|e| e.to_string());
            Event::Annotations {
                repo: gh.repo,
                run_id,
                job_id,
                calls,
                result,
            }
        });
    }

    pub fn fetch_checks(&self, gh: GhRepo, sha: Oid) {
        self.spawn("gh-checks", move || {
            let mut calls = Vec::new();
            let result = github::fetch_check_runs(
                &gh.workdir,
                &gh.owner,
                &gh.name,
                &sha.to_string(),
                &mut calls,
            )
            .map_err(|e| e.to_string());
            Event::Checks {
                repo: gh.repo,
                sha,
                calls,
                result,
            }
        });
    }

    pub fn fetch_pr(&self, gh: GhRepo, number: u64) {
        self.spawn("gh-pr", move || {
            let mut calls = Vec::new();
            let result = github::fetch_pr(&gh.workdir, &gh.owner, &gh.name, number, &mut calls)
                .map_err(|e| e.to_string());
            Event::PrDetail {
                repo: gh.repo,
                number,
                calls,
                result,
            }
        });
    }

    /// Repository visibility and open secret-scanning alerts.
    pub fn fetch_exposure(&self, gh: GhRepo) {
        self.spawn("gh-exposure", move || {
            let mut calls = Vec::new();
            let info = github::fetch_repo_info(&gh.workdir, &gh.owner, &gh.name, &mut calls)
                .map_err(|e| e.to_string());
            let alerts = github::fetch_secret_alerts(&gh.workdir, &gh.owner, &gh.name, &mut calls)
                .map_err(|e| e.to_string());
            Event::Exposure {
                repo: gh.repo,
                calls,
                info,
                alerts,
            }
        });
    }

    /// Full secrets scan on its own thread, streaming progress events.
    pub fn scan_secrets(
        &self,
        repo: RepoId,
        workdir: PathBuf,
        opts: ScanOptions,
        cancel: Arc<AtomicBool>,
        generation: u64,
    ) {
        let tx = self.events.clone();
        let ctx = self.ctx.clone();
        std::thread::Builder::new()
            .name("gitvis-secrets".into())
            .spawn(move || {
                let started = Instant::now();
                let progress_tx = tx.clone();
                let progress_ctx = ctx.clone();
                let mut report = |p: &Progress| {
                    let _ = progress_tx.send(Event::ScanProgress {
                        repo,
                        generation,
                        progress: p.clone(),
                    });
                    progress_ctx.request_repaint();
                };
                let result = secrets::scan(&workdir, &opts, &cancel, &mut report)
                    .map(Box::new)
                    .map_err(|e| e.to_string());
                let _ = tx.send(Event::ScanDone {
                    repo,
                    generation,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    result,
                });
                ctx.request_repaint();
            })
            .expect("spawn scan thread");
    }

    /// The OSINT analysis on its own thread, with progress events.
    pub fn run_osint(
        &self,
        repo: RepoId,
        workdir: PathBuf,
        opts: OsintOptions,
        cancel: Arc<AtomicBool>,
        generation: u64,
    ) {
        let tx = self.events.clone();
        let ctx = self.ctx.clone();
        std::thread::Builder::new()
            .name("gitvis-osint".into())
            .spawn(move || {
                let started = Instant::now();
                let (ptx, pctx) = (tx.clone(), ctx.clone());
                let mut report = |p: &Progress| {
                    let _ = ptx.send(Event::OsintProgress {
                        repo,
                        generation,
                        progress: p.clone(),
                    });
                    pctx.request_repaint();
                };
                let result = osint::analyze(&workdir, &opts, &cancel, &mut report)
                    .map(Box::new)
                    .map_err(|e| e.to_string());
                let _ = tx.send(Event::OsintDone {
                    repo,
                    generation,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    result,
                });
                ctx.request_repaint();
            })
            .expect("spawn osint thread");
    }

    /// `git fsck --unreachable` and a scan of the unreachable blobs.
    pub fn run_unreachable(&self, repo: RepoId, workdir: PathBuf, cancel: Arc<AtomicBool>) {
        let tx = self.events.clone();
        let ctx = self.ctx.clone();
        std::thread::Builder::new()
            .name("gitvis-fsck".into())
            .spawn(move || {
                let started = Instant::now();
                let (ptx, pctx) = (tx.clone(), ctx.clone());
                let mut report = |p: &Progress| {
                    let _ = ptx.send(Event::UnreachableProgress {
                        repo,
                        progress: p.clone(),
                    });
                    pctx.request_repaint();
                };
                let result = osint::unreachable(&workdir, &cancel, &mut report)
                    .map(Box::new)
                    .map_err(|e| e.to_string());
                let _ = tx.send(Event::UnreachableDone {
                    repo,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    result,
                });
                ctx.request_repaint();
            })
            .expect("spawn fsck thread");
    }

    /// osv.dev lookup of lockfile packages (network, through curl).
    pub fn run_osv(&self, repo: RepoId, packages: Vec<Package>, cancel: Arc<AtomicBool>) {
        let tx = self.events.clone();
        let ctx = self.ctx.clone();
        std::thread::Builder::new()
            .name("gitvis-osv".into())
            .spawn(move || {
                let started = Instant::now();
                let (ptx, pctx) = (tx.clone(), ctx.clone());
                let mut report = |p: &Progress| {
                    let _ = ptx.send(Event::OsvProgress {
                        repo,
                        progress: p.clone(),
                    });
                    pctx.request_repaint();
                };
                let result =
                    osint::osv_check(&packages, &cancel, &mut report).map_err(|e| e.to_string());
                let _ = tx.send(Event::OsvDone {
                    repo,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    packages: packages.len(),
                    result,
                });
                ctx.request_repaint();
            })
            .expect("spawn osv thread");
    }

    /// Forks, Dependabot and code-scanning alerts, and the admin surface.
    pub fn fetch_gh_surface(&self, gh: GhRepo, default_branch: String) {
        self.spawn("gh-surface", move || {
            let mut calls = Vec::new();
            let forks = github::fetch_forks(&gh.workdir, &gh.owner, &gh.name, &mut calls)
                .map_err(|e| e.to_string());
            let dependabot =
                github::fetch_dependabot_alerts(&gh.workdir, &gh.owner, &gh.name, &mut calls)
                    .map_err(|e| e.to_string());
            let code_alerts =
                github::fetch_code_alerts(&gh.workdir, &gh.owner, &gh.name, &mut calls)
                    .map_err(|e| e.to_string());
            let admin = github::fetch_admin_surface(
                &gh.workdir,
                &gh.owner,
                &gh.name,
                &default_branch,
                &mut calls,
            );
            Event::GhSurface {
                repo: gh.repo,
                calls,
                forks,
                dependabot,
                code_alerts,
                admin: Box::new(admin),
            }
        });
    }

    /// Code-search dorks, one request each, stopping at the first rate limit.
    pub fn run_dorks(&self, gh: GhRepo, is_org: bool) {
        self.spawn("gh-dorks", move || {
            let mut calls = Vec::new();
            let mut results = Vec::new();
            for (label, query) in github::dorks_for(&gh.owner, is_org) {
                match github::search_code(&gh.workdir, &query, &mut calls) {
                    Ok((total, hits)) => results.push(DorkResult {
                        label,
                        query,
                        total,
                        hits,
                        error: None,
                    }),
                    Err(e) => {
                        let m = e.to_string();
                        let limited = m.contains("403") || m.contains("rate limit");
                        results.push(DorkResult {
                            label,
                            query,
                            total: 0,
                            hits: Vec::new(),
                            error: Some(m),
                        });
                        if limited {
                            break;
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(700));
            }
            Event::Dorks {
                repo: gh.repo,
                calls,
                results,
            }
        });
    }

    /// Ask GitHub about each commit in turn.
    pub fn check_served(&self, gh: GhRepo, shas: Vec<Oid>) {
        let tx = self.events.clone();
        let ctx = self.ctx.clone();
        std::thread::Builder::new()
            .name("gitvis-gh-served".into())
            .spawn(move || {
                for sha in shas {
                    let mut calls = Vec::new();
                    let result = github::commit_served(
                        &gh.workdir,
                        &gh.owner,
                        &gh.name,
                        &sha.to_string(),
                        &mut calls,
                    )
                    .map_err(|e| e.to_string());
                    let _ = tx.send(Event::Served {
                        repo: gh.repo,
                        calls,
                        sha,
                        result,
                    });
                    ctx.request_repaint();
                }
            })
            .expect("spawn served thread");
    }

    /// `git ls-remote` for the pull-request refs of a remote.
    pub fn check_pr_refs(&self, repo: RepoId, workdir: PathBuf, remote: String) {
        self.spawn("pr-refs", move || {
            let started = Instant::now();
            let result = osint::pr_refs(&workdir, &remote).map_err(|e| e.to_string());
            Event::PrRefs {
                repo,
                elapsed_ms: started.elapsed().as_millis() as u64,
                result,
            }
        });
    }

    /// The insights analysis on its own thread, with progress events.
    pub fn run_insights(
        &self,
        repo: RepoId,
        workdir: PathBuf,
        opts: InsightsOptions,
        cancel: Arc<AtomicBool>,
        generation: u64,
    ) {
        let tx = self.events.clone();
        let ctx = self.ctx.clone();
        std::thread::Builder::new()
            .name("gitvis-insights".into())
            .spawn(move || {
                let started = Instant::now();
                let (ptx, pctx) = (tx.clone(), ctx.clone());
                let mut report = |p: &Progress| {
                    let _ = ptx.send(Event::InsightsProgress {
                        repo,
                        generation,
                        progress: p.clone(),
                    });
                    pctx.request_repaint();
                };
                let result = insights::analyze(&workdir, &opts, &cancel, &mut report)
                    .map(Box::new)
                    .map_err(|e| e.to_string());
                let _ = tx.send(Event::InsightsDone {
                    repo,
                    generation,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    result,
                });
                ctx.request_repaint();
            })
            .expect("spawn insights thread");
    }

    /// `git rev-list --objects --all` through `cat-file --batch-check`.
    pub fn run_weight(&self, repo: RepoId, workdir: PathBuf, cancel: Arc<AtomicBool>) {
        let tx = self.events.clone();
        let ctx = self.ctx.clone();
        std::thread::Builder::new()
            .name("gitvis-weight".into())
            .spawn(move || {
                let started = Instant::now();
                let (ptx, pctx) = (tx.clone(), ctx.clone());
                let mut report = |p: &Progress| {
                    let _ = ptx.send(Event::WeightProgress {
                        repo,
                        progress: p.clone(),
                    });
                    pctx.request_repaint();
                };
                let result = insights::health::weight(&workdir, &cancel, &mut report)
                    .map(Box::new)
                    .map_err(|e| e.to_string());
                let _ = tx.send(Event::WeightDone {
                    repo,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    result,
                });
                ctx.request_repaint();
            })
            .expect("spawn weight thread");
    }

    /// `git blame` of the files with debt markers.
    pub fn run_dating(
        &self,
        repo: RepoId,
        workdir: PathBuf,
        markers: Vec<Marker>,
        max_files: usize,
        cancel: Arc<AtomicBool>,
    ) {
        let tx = self.events.clone();
        let ctx = self.ctx.clone();
        std::thread::Builder::new()
            .name("gitvis-debt-blame".into())
            .spawn(move || {
                let started = Instant::now();
                let (ptx, pctx) = (tx.clone(), ctx.clone());
                let mut report = |p: &Progress| {
                    let _ = ptx.send(Event::DatingProgress {
                        repo,
                        progress: p.clone(),
                    });
                    pctx.request_repaint();
                };
                let result =
                    insights::debt::date(&workdir, &markers, max_files, &cancel, &mut report)
                        .map(Box::new)
                        .map_err(|e| e.to_string());
                let _ = tx.send(Event::DatingDone {
                    repo,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    result,
                });
                ctx.request_repaint();
            })
            .expect("spawn dating thread");
    }

    /// Blame summary of one file.
    pub fn run_blame(&self, repo: RepoId, workdir: PathBuf, path: String) {
        self.spawn("blame", move || {
            let started = Instant::now();
            let result = insights::history::blame(&workdir, &path).map_err(|e| e.to_string());
            Event::BlameDone {
                repo,
                path,
                elapsed_ms: started.elapsed().as_millis() as u64,
                result,
            }
        });
    }

    /// History search (`git log -S/-G/--grep/--author`).
    pub fn run_search(
        &self,
        repo: RepoId,
        workdir: PathBuf,
        query: SearchQuery,
        cancel: Arc<AtomicBool>,
    ) {
        self.spawn("history-search", move || {
            let started = Instant::now();
            let result = insights::search::search(&workdir, &query, &cancel)
                .map(Box::new)
                .map_err(|e| e.to_string());
            Event::SearchDone {
                repo,
                elapsed_ms: started.elapsed().as_millis() as u64,
                result,
            }
        });
    }

    /// Lost commits and stashes (`git fsck --unreachable --no-reflogs`).
    pub fn run_lost(&self, repo: RepoId, workdir: PathBuf, cancel: Arc<AtomicBool>) {
        let tx = self.events.clone();
        let ctx = self.ctx.clone();
        std::thread::Builder::new()
            .name("gitvis-lost".into())
            .spawn(move || {
                let started = Instant::now();
                let (ptx, pctx) = (tx.clone(), ctx.clone());
                let mut report = |p: &Progress| {
                    let _ = ptx.send(Event::LostProgress {
                        repo,
                        progress: p.clone(),
                    });
                    pctx.request_repaint();
                };
                let result = insights::recover::lost(&workdir, &cancel, &mut report)
                    .map(Box::new)
                    .map_err(|e| e.to_string());
                let _ = tx.send(Event::LostDone {
                    repo,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    result,
                });
                ctx.request_repaint();
            })
            .expect("spawn lost thread");
    }

    /// Quick scan of what is about to be committed.
    pub fn scan_changes(&self, repo: RepoId, workdir: PathBuf, generation: u64) {
        self.spawn("quick-scan", move || {
            let started = Instant::now();
            let result = secrets::scan_changes(&workdir)
                .map(Box::new)
                .map_err(|e| e.to_string());
            Event::QuickScan {
                repo,
                generation,
                elapsed_ms: started.elapsed().as_millis() as u64,
                result,
            }
        });
    }
}

fn reader_loop(rx: Receiver<Job>, tx: Sender<Event>, ctx: egui::Context) {
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        while let Ok(job) = rx.try_recv() {
            batch.push(job);
        }
        coalesce(&mut batch);
        for job in batch {
            let started = Instant::now();
            let ms = |s: Instant| s.elapsed().as_millis() as u64;
            let event = match job {
                Job::Load {
                    repo,
                    path,
                    opts,
                    generation,
                } => {
                    let result = repo::load(&path, &opts)
                        .map(Box::new)
                        .map_err(|e| e.to_string());
                    Event::Loaded {
                        repo,
                        generation,
                        elapsed_ms: ms(started),
                        result,
                    }
                }
                Job::Status {
                    repo,
                    path,
                    generation,
                } => {
                    let result = repo::load_status(&path).map_err(|e| e.to_string());
                    Event::Status {
                        repo,
                        generation,
                        elapsed_ms: ms(started),
                        result,
                    }
                }
                Job::Details { repo, path, id } => {
                    let result = repo::commit_details(&path, id).map_err(|e| e.to_string());
                    Event::Details {
                        repo,
                        id,
                        elapsed_ms: ms(started),
                        result,
                    }
                }
                Job::Diff {
                    repo,
                    path,
                    target,
                    opts,
                    dark,
                    generation,
                } => {
                    let command =
                        gitvis_core::diff_args(&target, &opts).map(|a| render_command("git", &a));
                    let result = gitvis_core::load_diff(&path, &target, &opts)
                        .map(|diff| {
                            let spans = crate::highlight::highlight(&diff, target.path(), dark);
                            Box::new(DiffDoc {
                                language: crate::highlight::language_for(target.path()),
                                diff,
                                spans,
                            })
                        })
                        .map_err(|e| e.to_string());
                    Event::Diff {
                        repo,
                        generation,
                        elapsed_ms: ms(started),
                        target,
                        command,
                        result,
                    }
                }
                Job::Reflog { repo, path } => Event::Reflog {
                    repo,
                    entries: reflog::read(&path, 200).unwrap_or_default(),
                },
                Job::Contains { repo, path, id } => {
                    let result = repo::branches_containing(&path, id).map_err(|e| e.to_string());
                    Event::Contains {
                        repo,
                        id,
                        elapsed_ms: ms(started),
                        result,
                    }
                }
            };
            if tx.send(event).is_err() {
                return;
            }
            ctx.request_repaint();
        }
    }
}

/// Drop work that a later job in the same batch makes pointless.
fn coalesce(batch: &mut Vec<Job>) {
    let n = batch.len();
    let mut keep = vec![true; n];
    for i in 0..n {
        for j in (i + 1)..n {
            let superseded = match (&batch[i], &batch[j]) {
                (Job::Load { repo: a, .. }, Job::Load { repo: b, .. }) => a == b,
                (
                    Job::Status { repo: a, .. },
                    Job::Load { repo: b, .. } | Job::Status { repo: b, .. },
                ) => a == b,
                (Job::Diff { repo: a, .. }, Job::Diff { repo: b, .. }) => a == b,
                (Job::Reflog { repo: a, .. }, Job::Reflog { repo: b, .. }) => a == b,
                _ => false,
            };
            if superseded {
                keep[i] = false;
                break;
            }
        }
    }
    let mut idx = 0;
    batch.retain(|_| {
        let k = keep[idx];
        idx += 1;
        k
    });
}

#[cfg(test)]
mod tests {
    use super::render_command;

    #[test]
    fn renders_commands_for_the_log() {
        let args = ["commit".to_owned(), "-m".to_owned(), "two words".to_owned()];
        assert_eq!(render_command("git", &args), "git commit -m \"two words\"");
    }
}
