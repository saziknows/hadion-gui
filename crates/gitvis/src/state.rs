use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use gitvis_core::devops::forge::ForgeRemote;
use gitvis_core::diff::FileDiff;
use gitvis_core::github::{
    AdminSurface, Annotation, CheckRun, CodeAlert, DependabotAlert, DorkResult, Fork, GitHubData,
    Job, Outcome, RateLimit, RepoInfo, SecretAlert,
};
use gitvis_core::insights::InsightsReport;
use gitvis_core::insights::health::Weight;
use gitvis_core::insights::history::FileBlame;
use gitvis_core::insights::recover::Lost;
use gitvis_core::insights::search::{SearchMode, SearchResult};
use gitvis_core::joblog::JobLog;
use gitvis_core::ops::{GitOp, PullMode, ResetMode};
use gitvis_core::osint::{OsintReport, PrRefs, Unreachable, Vuln};
use gitvis_core::reflog::{ReflogEntry, Undo};
use gitvis_core::secrets::{Progress, Report, Severity};
use gitvis_core::{CommitDetails, DiffTarget, GraphFilter, Oid, Snapshot};
use serde::{Deserialize, Serialize};

use crate::devops::{ConsoleAction, DevOpsAction, DevOpsPrefs, DevOpsState};

/// A coloured run of characters in a diff line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub color: egui::Color32,
}

/// A loaded diff plus its syntax colouring (hunk → line → spans).
#[derive(Clone, Debug)]
pub struct DiffDoc {
    pub diff: FileDiff,
    pub spans: Vec<Vec<Vec<Span>>>,
    pub language: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DiffMode {
    #[default]
    Unified,
    Split,
}

/// Selected rows of one hunk (row indices in the current mode's row list).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffSelection {
    pub hunk: usize,
    pub anchor: usize,
    pub end: usize,
    pub mode: DiffMode,
    pub dragging: bool,
}

impl DiffSelection {
    pub fn range(&self) -> std::ops::RangeInclusive<usize> {
        self.anchor.min(self.end)..=self.anchor.max(self.end)
    }
}

/// What the graph shows next to a commit that has CI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CiInfo {
    pub outcome: Outcome,
    /// The run to open on click (newest or first failing).
    pub run: Option<u64>,
    /// `3 checks · 1 failed` style summary for the tooltip.
    pub summary: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ActivityTab {
    #[default]
    Log,
    Reflog,
}

/// What produced an activity entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActivityKind {
    /// A git (or gh) operation that changed something.
    Op,
    /// A background read: repository load, status, diff, commit details.
    Read,
    /// A GitHub API request.
    Api,
    /// A full exposure scan (always logged, like an operation).
    Scan,
}

impl ActivityKind {
    pub const ALL: [ActivityKind; 4] = [
        ActivityKind::Op,
        ActivityKind::Read,
        ActivityKind::Api,
        ActivityKind::Scan,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ActivityKind::Op => "Operations",
            ActivityKind::Read => "Reads",
            ActivityKind::Api => "GitHub API",
            ActivityKind::Scan => "Scans",
        }
    }

    /// Kept even when the verbose log is off.
    pub fn always_logged(self) -> bool {
        matches!(self, ActivityKind::Op | ActivityKind::Scan)
    }
}

/// One thing gitvis did, for the activity log.
#[derive(Clone, Debug)]
pub struct ActivityEntry {
    /// Wall-clock time, `HH:MM:SS.mmm`.
    pub at: String,
    pub kind: ActivityKind,
    pub repo: String,
    pub label: String,
    pub commands: Vec<String>,
    pub output: String,
    /// Where the commands ran.
    pub cwd: String,
    pub ok: bool,
    pub elapsed_ms: u64,
    /// Bytes of output or response, when known.
    pub bytes: Option<usize>,
    pub expanded: bool,
}

pub type RepoId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    None,
    Wip,
    Commit(Oid),
}

pub struct Dirty {
    pub first: Instant,
    pub last: Instant,
    pub full: bool,
}

/// What the centre of the window shows for a repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Page {
    /// Every open repository at a glance and the GitHub inbox.
    Home,
    #[default]
    Graph,
    /// Pull requests, reviews and issues (merge requests on GitLab).
    Review,
    /// Actions: runs, workflows, artifacts, caches, validation, local runs
    /// (pipelines on GitLab / Azure DevOps).
    GitHub,
    /// Releases, deployments, where a commit is, DORA metrics, packages,
    /// Kubernetes, Terraform.
    Ship,
    /// Alerts, SBOM, image scans, protection, CODEOWNERS, environments,
    /// secrets and variables.
    Govern,
    /// Tasks, interactive rebase, bisect, worktrees, file history, console.
    Workbench,
    /// Secrets, sensitive files and leaky configuration.
    Security,
    /// Health, hotspots, ownership, branches, commits, releases, timeline,
    /// debt, CI, licences, history search and recovery.
    Insights,
}

/// Which exposure findings the list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ExposureFilter {
    #[default]
    All,
    /// Would go into the next commit.
    Staged,
    /// Present in the HEAD tree.
    AtHead,
    /// Committed once, gone from HEAD, still recoverable.
    HistoryOnly,
    /// Reachable from a remote-tracking branch.
    Pushed,
    Files,
    Config,
}

impl ExposureFilter {
    pub const ALL: [ExposureFilter; 7] = [
        ExposureFilter::All,
        ExposureFilter::Staged,
        ExposureFilter::AtHead,
        ExposureFilter::HistoryOnly,
        ExposureFilter::Pushed,
        ExposureFilter::Files,
        ExposureFilter::Config,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ExposureFilter::All => "All",
            ExposureFilter::Staged => "Staged",
            ExposureFilter::AtHead => "In HEAD",
            ExposureFilter::HistoryOnly => "History only",
            ExposureFilter::Pushed => "Pushed",
            ExposureFilter::Files => "Files",
            ExposureFilter::Config => "Config",
        }
    }
}

/// A scan in progress.
pub struct ScanRun {
    pub started: Instant,
    pub progress: Progress,
    pub cancel: Arc<AtomicBool>,
    pub history: bool,
    /// Started by the app (scan on open), not by the user: its result shows
    /// in the toolbar badge and status bar, never as a toast.
    pub automatic: bool,
}

impl ScanRun {
    pub fn new(cancel: Arc<AtomicBool>, history: bool) -> Self {
        Self {
            started: Instant::now(),
            progress: Progress::default(),
            cancel,
            history,
            automatic: false,
        }
    }
}

/// Tabs of the Exposure page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SecurityTab {
    #[default]
    Secrets,
    People,
    Footprint,
    Files,
    History,
    GitHub,
    Supply,
}

impl SecurityTab {
    pub const ALL: [SecurityTab; 7] = [
        SecurityTab::Secrets,
        SecurityTab::People,
        SecurityTab::Footprint,
        SecurityTab::Files,
        SecurityTab::History,
        SecurityTab::GitHub,
        SecurityTab::Supply,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SecurityTab::Secrets => "Secrets",
            SecurityTab::People => "People",
            SecurityTab::Footprint => "Footprint",
            SecurityTab::Files => "Files",
            SecurityTab::History => "History",
            SecurityTab::GitHub => "GitHub",
            SecurityTab::Supply => "Supply chain",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            SecurityTab::Secrets => {
                "Credentials and sensitive files in the tree, staged changes and history"
            }
            SecurityTab::People => {
                "Who commits, from where and when: e-mail identities, aliases, time zones, working hours"
            }
            SecurityTab::Footprint => {
                "Infrastructure and personal data named in the files: hosts, IPs, cloud accounts, buckets, e-mails, cards"
            }
            SecurityTab::Files => {
                "Metadata inside images, documents and binaries; junk files from developer machines"
            }
            SecurityTab::History => {
                "What the history still holds: incident messages, rewritten commits, stashes, backup branches, deleted files, unreachable objects"
            }
            SecurityTab::GitHub => {
                "What GitHub exposes: visibility, forks, alerts, admin surface, CODEOWNERS, code-search dorks"
            }
            SecurityTab::Supply => {
                "Dependencies pinned to git, known vulnerable packages, committed hooks and drivers"
            }
        }
    }

    /// Needs the OSINT analysis (everything but the secrets tab).
    pub fn needs_osint(self) -> bool {
        !matches!(self, SecurityTab::Secrets)
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s.to_lowercase().as_str() {
            "secrets" => SecurityTab::Secrets,
            "people" => SecurityTab::People,
            "footprint" => SecurityTab::Footprint,
            "files" => SecurityTab::Files,
            "history" => SecurityTab::History,
            "github" => SecurityTab::GitHub,
            "supply" | "supply-chain" => SecurityTab::Supply,
            _ => return None,
        })
    }
}

/// OSINT analysis results and the on-demand extras around them.
#[derive(Default)]
#[allow(clippy::type_complexity)]
pub struct OsintState {
    pub report: Option<OsintReport>,
    pub report_at: Option<Instant>,
    pub run: Option<ScanRun>,
    pub generation: u64,
    pub error: Option<String>,
    /// Selected item of the current tab (email, fingerprint, path).
    pub selected: Option<String>,
    pub filter: String,
    pub unreachable: Option<Result<Unreachable, String>>,
    pub unreachable_run: Option<ScanRun>,
    pub osv: Option<Result<Vec<Vuln>, String>>,
    pub osv_run: Option<ScanRun>,
    pub forks: Option<Result<Vec<Fork>, String>>,
    pub dependabot: Option<Result<Vec<DependabotAlert>, String>>,
    pub code_alerts: Option<Result<Vec<CodeAlert>, String>>,
    pub admin: Option<AdminSurface>,
    pub gh_loading: bool,
    pub gh_at: Option<Instant>,
    pub dorks: Option<Vec<DorkResult>>,
    pub dorks_running: bool,
    /// Does GitHub still serve a commit? `None` while checking.
    pub served: HashMap<Oid, Option<Result<bool, String>>>,
    pub pr_refs: Option<Result<PrRefs, String>>,
    pub pr_refs_running: bool,
}

impl OsintState {
    pub fn running(&self) -> bool {
        self.run.is_some()
    }
}

/// Tabs of the Insights page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum InsightsTab {
    #[default]
    Health,
    Hotspots,
    Ownership,
    Branches,
    Commits,
    Releases,
    Timeline,
    Debt,
    Ci,
    Licences,
    Search,
    Recovery,
}

impl InsightsTab {
    pub const ALL: [InsightsTab; 12] = [
        InsightsTab::Health,
        InsightsTab::Hotspots,
        InsightsTab::Ownership,
        InsightsTab::Branches,
        InsightsTab::Commits,
        InsightsTab::Releases,
        InsightsTab::Timeline,
        InsightsTab::Debt,
        InsightsTab::Ci,
        InsightsTab::Licences,
        InsightsTab::Search,
        InsightsTab::Recovery,
    ];

    pub fn label(self) -> &'static str {
        match self {
            InsightsTab::Health => "Health",
            InsightsTab::Hotspots => "Hotspots",
            InsightsTab::Ownership => "Owners",
            InsightsTab::Branches => "Branches",
            InsightsTab::Commits => "Commits",
            InsightsTab::Releases => "Releases",
            InsightsTab::Timeline => "Timeline",
            InsightsTab::Debt => "Debt",
            InsightsTab::Ci => "CI",
            InsightsTab::Licences => "Licences",
            InsightsTab::Search => "Search",
            InsightsTab::Recovery => "Recovery",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            InsightsTab::Health => {
                "Checkout landmines, line endings and attributes, executable bits, repository weight, submodules"
            }
            InsightsTab::Hotspots => {
                "Files that change most, files that always change together, how old each area of the code is"
            }
            InsightsTab::Ownership => {
                "Who owns which directory, bus factor, orphaned areas; blame of a single file on demand"
            }
            InsightsTab::Branches => {
                "Stale, merged and gone branches, unique commits, predicted merge conflicts, duplicate patches"
            }
            InsightsTab::Commits => {
                "Commit hygiene (WIP, fixups, giant commits, conventions) and provenance (signatures, bots, odd dates)"
            }
            InsightsTab::Releases => {
                "Tags, what is unreleased since the last one, release cadence, version bumps, suggested next version"
            }
            InsightsTab::Timeline => {
                "Contributors per month, joins and departures, lines over time, languages, when work happens"
            }
            InsightsTab::Debt => "TODO, FIXME, HACK and friends, by directory, assignee and age",
            InsightsTab::Ci => {
                "GitHub Actions workflows: unpinned actions, dangerous triggers, permissions, secrets, runners"
            }
            InsightsTab::Licences => {
                "The project licence, dependency licences found offline, copyright headers from other projects"
            }
            InsightsTab::Search => {
                "Find when a string was added or removed anywhere in history (git log -S / -G)"
            }
            InsightsTab::Recovery => {
                "Commits and stashes no branch reaches any more, and how to bring them back"
            }
        }
    }

    /// Needs the full analysis (everything but search and recovery).
    pub fn needs_report(self) -> bool {
        !matches!(self, InsightsTab::Search | InsightsTab::Recovery)
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s.to_lowercase().as_str() {
            "health" => InsightsTab::Health,
            "hotspots" => InsightsTab::Hotspots,
            "owners" | "ownership" => InsightsTab::Ownership,
            "branches" => InsightsTab::Branches,
            "commits" => InsightsTab::Commits,
            "releases" => InsightsTab::Releases,
            "timeline" => InsightsTab::Timeline,
            "debt" => InsightsTab::Debt,
            "ci" => InsightsTab::Ci,
            "licences" | "licenses" => InsightsTab::Licences,
            "search" => InsightsTab::Search,
            "recovery" | "recover" => InsightsTab::Recovery,
            _ => return None,
        })
    }
}

/// State of the Insights page: the analysis report and the on-demand extras.
#[allow(clippy::type_complexity)]
pub struct InsightsPage {
    pub report: Option<InsightsReport>,
    pub report_at: Option<Instant>,
    pub run: Option<ScanRun>,
    pub generation: u64,
    pub error: Option<String>,
    pub tab: InsightsTab,
    /// Selected item of the current tab (path, branch name, sha…).
    pub selected: Option<String>,
    pub filter: String,
    /// Newest commits the history pass reads (0 = all).
    pub depth: usize,
    pub all_refs: bool,
    /// History weight walk (`git rev-list --objects --all`).
    pub weight: Option<Result<Weight, String>>,
    pub weight_run: Option<ScanRun>,
    /// Debt dating through `git blame`.
    pub dating_run: Option<ScanRun>,
    /// Blame summaries per path; `None` while loading.
    pub blame: HashMap<String, Option<Result<FileBlame, String>>>,
    pub search_text: String,
    pub search_mode: SearchMode,
    pub search_all: bool,
    pub search_path: String,
    pub search_ignore_case: bool,
    pub search: Option<Result<SearchResult, String>>,
    pub search_run: Option<ScanRun>,
    pub lost: Option<Result<Lost, String>>,
    pub lost_run: Option<ScanRun>,
    /// Show remote-tracking branches on the Branches tab.
    pub show_remote: bool,
}

impl Default for InsightsPage {
    fn default() -> Self {
        Self {
            report: None,
            report_at: None,
            run: None,
            generation: 0,
            error: None,
            tab: InsightsTab::Health,
            selected: None,
            filter: String::new(),
            depth: 10_000,
            all_refs: false,
            weight: None,
            weight_run: None,
            dating_run: None,
            blame: HashMap::new(),
            search_text: String::new(),
            search_mode: SearchMode::Pickaxe,
            search_all: true,
            search_path: String::new(),
            search_ignore_case: false,
            search: None,
            search_run: None,
            lost: None,
            lost_run: None,
            show_remote: false,
        }
    }
}

impl InsightsPage {
    pub fn running(&self) -> bool {
        self.run.is_some()
    }
}

/// State of the exposure page and the background scans feeding it.
pub struct SecurityPage {
    /// Last full scan merged with the quick scan of uncommitted changes.
    pub report: Option<Report>,
    /// Last full scan as returned by the scanner.
    pub full: Option<Report>,
    /// Quick scan of staged/unstaged/untracked changes (runs after every
    /// status refresh).
    pub quick: Option<Report>,
    pub report_at: Option<Instant>,
    pub quick_at: Option<Instant>,
    pub scan: Option<ScanRun>,
    pub scan_gen: u64,
    pub quick_gen: u64,
    pub quick_loading: bool,
    pub error: Option<String>,
    /// Something changed in the repository since the last full scan.
    pub stale: bool,
    /// Fingerprint (or `alert:N`) of the selected item.
    pub selected: Option<String>,
    pub filter: String,
    pub hidden: HashSet<Severity>,
    pub state: ExposureFilter,
    pub show_dismissed: bool,
    /// Findings whose secret is shown in clear.
    pub revealed: HashSet<String>,
    /// Newest commits a history scan reads (0 = all).
    pub history_depth: usize,
    pub all_refs: bool,
    pub info: Option<Result<RepoInfo, String>>,
    pub alerts: Option<Result<Vec<SecretAlert>, String>>,
    pub gh_loading: bool,
    pub gh_at: Option<Instant>,
    pub tab: SecurityTab,
    pub osint: OsintState,
}

impl Default for SecurityPage {
    fn default() -> Self {
        Self {
            tab: SecurityTab::Secrets,
            osint: OsintState::default(),
            report: None,
            full: None,
            quick: None,
            report_at: None,
            quick_at: None,
            scan: None,
            scan_gen: 0,
            quick_gen: 0,
            quick_loading: false,
            error: None,
            stale: false,
            selected: None,
            filter: String::new(),
            hidden: HashSet::new(),
            state: ExposureFilter::All,
            show_dismissed: false,
            revealed: HashSet::new(),
            history_depth: 2_000,
            all_refs: false,
            info: None,
            alerts: None,
            gh_loading: false,
            gh_at: None,
        }
    }
}

impl SecurityPage {
    /// Recompute the merged report after either scan changed.
    pub fn rebuild(&mut self) {
        self.report = match (&self.full, &self.quick) {
            (Some(f), Some(q)) => {
                let mut r = f.clone();
                r.merge(q);
                Some(r)
            }
            (Some(f), None) => Some(f.clone()),
            (None, Some(q)) => Some(q.clone()),
            (None, None) => None,
        };
    }

    pub fn scanning(&self) -> bool {
        self.scan.is_some()
    }

    /// Secrets and sensitive files that would go into the next commit.
    pub fn staged_count(&self) -> usize {
        self.report
            .as_ref()
            .map(|r| r.staged().count() + r.staged_files().count())
            .unwrap_or(0)
    }

    /// Critical and high findings, dismissed ones excluded.
    pub fn urgent_count(&self, dismissed: &HashSet<String>) -> usize {
        let Some(r) = &self.report else { return 0 };
        r.findings
            .iter()
            .filter(|f| f.severity >= Severity::High && !dismissed.contains(&f.fingerprint))
            .count()
            + r.files
                .iter()
                .filter(|f| f.severity >= Severity::High && !dismissed.contains(&f.fingerprint))
                .count()
            + r.config
                .iter()
                .filter(|f| f.severity >= Severity::High && !dismissed.contains(&f.fingerprint))
                .count()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RunFilter {
    #[default]
    All,
    Failed,
    Running,
    Success,
}

impl RunFilter {
    pub fn label(self) -> &'static str {
        match self {
            RunFilter::All => "All",
            RunFilter::Failed => "Failed",
            RunFilter::Running => "Running",
            RunFilter::Success => "Passed",
        }
    }

    pub fn accepts(self, o: Outcome) -> bool {
        match self {
            RunFilter::All => true,
            RunFilter::Failed => o.is_bad() || o == Outcome::Cancelled,
            RunFilter::Running => o.is_running(),
            RunFilter::Success => o.is_good(),
        }
    }
}

/// A fetched job log plus the viewer's state for it.
pub struct LogDoc {
    pub log: JobLog,
    /// Group indices folded away.
    pub collapsed: HashSet<u32>,
    /// Lines matching the current filter (all lines when empty filter).
    pub filter: String,
    pub matches: Vec<usize>,
    pub selected: Option<usize>,
    /// Scroll to this line on the next frame.
    pub scroll_to: Option<usize>,
    /// `(step number, first line)` once attributed from the job's steps.
    pub steps: Option<Vec<(u32, usize)>>,
}

impl LogDoc {
    pub fn new(log: JobLog) -> Self {
        let matches = (0..log.lines.len()).collect();
        Self {
            log,
            collapsed: HashSet::new(),
            filter: String::new(),
            matches,
            selected: None,
            scroll_to: None,
            steps: None,
        }
    }

    pub fn set_filter(&mut self, filter: &str) {
        if self.filter != filter {
            self.filter = filter.to_owned();
            self.matches = self.log.matching(filter);
        }
    }
}

pub struct LogState {
    pub loading: bool,
    pub doc: Option<Result<LogDoc, String>>,
}

/// Everything loaded for one workflow run.
#[derive(Default)]
pub struct RunDetail {
    pub jobs: Option<Result<Vec<Job>, String>>,
    pub jobs_loading: bool,
    pub jobs_at: Option<Instant>,
    pub expanded: HashSet<u64>,
    /// The job whose log the viewer shows.
    pub selected_job: Option<u64>,
    pub logs: HashMap<u64, LogState>,
    pub annotations: HashMap<u64, Result<Vec<Annotation>, String>>,
    pub annotations_loading: HashSet<u64>,
}

impl RunDetail {
    pub fn jobs(&self) -> &[Job] {
        match &self.jobs {
            Some(Ok(j)) => j,
            _ => &[],
        }
    }
}

/// State of the Actions page's run list.
#[derive(Default)]
pub struct GitHubPage {
    pub run_filter: String,
    pub run_status: RunFilter,
    pub selected_run: Option<u64>,
    /// A run asked for that is not in the list yet (being fetched).
    pub pending_run: Option<u64>,
    pub runs: HashMap<u64, RunDetail>,
    pub show_timestamps: bool,
    pub log_filter: String,
    /// `(job, step number)` to scroll the log to once it is loaded.
    pub scroll_to_step: Option<(u64, u32)>,
}

/// Check runs of one commit.
pub struct ChecksState {
    pub loading: bool,
    pub at: Option<Instant>,
    pub result: Option<Result<Vec<CheckRun>, String>>,
}

pub struct RepoState {
    pub id: RepoId,
    pub workdir: PathBuf,
    pub name: String,
    pub snapshot: Option<Snapshot>,
    pub loading: bool,
    pub load_error: Option<String>,
    /// How long the last full load took, and when it finished.
    pub load_ms: Option<u64>,
    pub loaded_at: Option<Instant>,
    pub filter: GraphFilter,
    pub selection: Selection,
    pub details: HashMap<Oid, CommitDetails>,
    pub details_loading: Option<Oid>,
    /// Branches containing a commit (`None` while loading).
    pub contains: HashMap<Oid, Option<Vec<String>>>,
    pub file: Option<DiffTarget>,
    pub diff: Option<Result<DiffDoc, String>>,
    pub diff_gen: u64,
    pub diff_selection: Option<DiffSelection>,
    pub reflog: Vec<ReflogEntry>,
    pub undo: Option<Undo>,
    /// `(owner, repo)` when a remote points at github.com.
    pub github_remote: Option<(String, String)>,
    /// A GitLab or Azure DevOps remote (when there is no GitHub one).
    pub forge_remote: Option<ForgeRemote>,
    pub github: Option<GitHubData>,
    pub github_error: Option<String>,
    pub github_loading: bool,
    pub github_at: Option<Instant>,
    pub rate: Option<RateLimit>,
    /// CI status per commit, from the workflow runs and fetched check runs.
    pub ci: HashMap<Oid, CiInfo>,
    pub checks: HashMap<Oid, ChecksState>,
    /// A commit whose checks should be fetched once the selection settles.
    pub checks_wanted: Option<(Oid, Instant)>,
    pub page: Page,
    pub gh_page: GitHubPage,
    pub sec_page: SecurityPage,
    pub ins_page: InsightsPage,
    /// Review, CI control, Ship, Govern, Workbench, forge state.
    pub devops: DevOpsState,
    /// Fingerprints of exposure findings marked as false positives.
    pub dismissed: HashSet<String>,
    pub commit_message: String,
    pub amend: bool,
    /// Label of the running operation.
    pub busy: Option<String>,
    /// Remaining operations of a sequence (run after `busy` finishes).
    pub queued: Vec<GitOp>,
    /// A reload asked for while an operation ran (`Some(full)`).
    pub pending_reload: Option<bool>,
    pub scroll_to_row: Option<usize>,
    pub visible_rows: Range<usize>,
    pub search: String,
    pub search_hits: Vec<usize>,
    pub search_cursor: usize,
    pub sidebar_filter: String,
    pub collapsed: HashSet<String>,
    pub dirty: Option<Dirty>,
    pub suppress_watch_until: Option<Instant>,
    pub generation: u64,
    pub watcher: Option<crate::watcher::Watcher>,
}

impl RepoState {
    pub fn new(id: RepoId, workdir: PathBuf) -> Self {
        let name = workdir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| workdir.display().to_string());
        Self {
            id,
            workdir,
            name,
            snapshot: None,
            loading: false,
            load_error: None,
            load_ms: None,
            loaded_at: None,
            filter: GraphFilter::default(),
            selection: Selection::None,
            details: HashMap::new(),
            details_loading: None,
            contains: HashMap::new(),
            file: None,
            diff: None,
            diff_gen: 0,
            diff_selection: None,
            reflog: Vec::new(),
            undo: None,
            github_remote: None,
            forge_remote: None,
            github: None,
            github_error: None,
            github_loading: false,
            github_at: None,
            rate: None,
            ci: HashMap::new(),
            checks: HashMap::new(),
            checks_wanted: None,
            page: Page::Graph,
            gh_page: GitHubPage::default(),
            sec_page: SecurityPage::default(),
            ins_page: InsightsPage::default(),
            devops: DevOpsState::default(),
            dismissed: HashSet::new(),
            commit_message: String::new(),
            amend: false,
            busy: None,
            queued: Vec::new(),
            pending_reload: None,
            scroll_to_row: None,
            visible_rows: 0..0,
            search: String::new(),
            search_hits: Vec::new(),
            search_cursor: 0,
            sidebar_filter: String::new(),
            collapsed: HashSet::new(),
            dirty: None,
            suppress_watch_until: None,
            generation: 0,
            watcher: None,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.busy.is_some()
    }

    pub fn selected_row(&self) -> Option<usize> {
        let snap = self.snapshot.as_ref()?;
        match self.selection {
            Selection::None => None,
            Selection::Wip => snap.wip_row.then_some(0),
            Selection::Commit(id) => snap.index_of.get(&id).map(|&i| snap.row_of_commit(i)),
        }
    }

    pub fn selection_for_row(&self, row: usize) -> Selection {
        let Some(snap) = self.snapshot.as_ref() else {
            return Selection::None;
        };
        match snap.commit_of_row(row) {
            None => Selection::Wip,
            Some(i) => snap
                .commits
                .get(i)
                .map(|c| Selection::Commit(c.id))
                .unwrap_or(Selection::None),
        }
    }

    pub fn recompute_search(&mut self) {
        self.search_hits.clear();
        let q = self.search.trim().to_lowercase();
        let Some(snap) = self.snapshot.as_ref() else {
            return;
        };
        if q.is_empty() {
            return;
        }
        for (i, c) in snap.commits.iter().enumerate() {
            let hit = c.summary.to_lowercase().contains(&q)
                || c.author.to_lowercase().contains(&q)
                || c.id.to_string().starts_with(&q)
                || snap.refs.get(&c.id).is_some_and(|labels| {
                    labels.iter().any(|l| l.name.to_lowercase().contains(&q))
                });
            if hit {
                self.search_hits.push(snap.row_of_commit(i));
            }
        }
        if self.search_cursor >= self.search_hits.len() {
            self.search_cursor = 0;
        }
    }

    pub fn prefs(&self) -> RepoPrefs {
        let mut dismissed: Vec<String> = self.dismissed.iter().cloned().collect();
        dismissed.sort();
        RepoPrefs {
            hidden: self.filter.hidden.clone(),
            hide_remotes: self.filter.hide_remotes,
            hide_tags: self.filter.hide_tags,
            collapsed: self.collapsed.iter().cloned().collect(),
            dismissed,
            history_depth: Some(self.sec_page.history_depth),
            scan_all_refs: self.sec_page.all_refs,
            insights_depth: Some(self.ins_page.depth),
            insights_all_refs: self.ins_page.all_refs,
            devops: self.devops.prefs(),
        }
    }

    pub fn apply_prefs(&mut self, p: &RepoPrefs) {
        self.filter.hidden = p.hidden.clone();
        self.filter.hide_remotes = p.hide_remotes;
        self.filter.hide_tags = p.hide_tags;
        self.collapsed = p.collapsed.iter().cloned().collect();
        self.dismissed = p.dismissed.iter().cloned().collect();
        if let Some(d) = p.history_depth {
            self.sec_page.history_depth = d;
        }
        self.sec_page.all_refs = p.scan_all_refs;
        if let Some(d) = p.insights_depth {
            self.ins_page.depth = d;
        }
        self.ins_page.all_refs = p.insights_all_refs;
        self.devops.apply_prefs(&p.devops);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PullPref {
    #[default]
    Merge,
    FastForwardOnly,
    Rebase,
}

impl PullPref {
    pub fn mode(self) -> PullMode {
        match self {
            PullPref::Merge => PullMode::FastForwardOrMerge,
            PullPref::FastForwardOnly => PullMode::FastForwardOnly,
            PullPref::Rebase => PullMode::Rebase,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PullPref::Merge => "Pull (fast-forward or merge)",
            PullPref::FastForwardOnly => "Pull (fast-forward only)",
            PullPref::Rebase => "Pull (rebase)",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoPrefs {
    pub hidden: Vec<String>,
    pub hide_remotes: bool,
    pub hide_tags: bool,
    pub collapsed: Vec<String>,
    /// Exposure findings marked as false positives (fingerprints).
    pub dismissed: Vec<String>,
    /// Commits a history scan reads (0 = all); `None` keeps the default.
    pub history_depth: Option<usize>,
    pub scan_all_refs: bool,
    /// Commits the insights history pass reads (0 = all).
    pub insights_depth: Option<usize>,
    pub insights_all_refs: bool,
    /// Review, CI, Ship, Govern, Workbench and forge preferences.
    pub devops: DevOpsPrefs,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub open_repos: Vec<PathBuf>,
    pub active: usize,
    pub recent: Vec<PathBuf>,
    pub dark: bool,
    pub relative_dates: bool,
    pub show_author: bool,
    pub pull: PullPref,
    pub repo_prefs: HashMap<String, RepoPrefs>,
    pub diff_mode: DiffMode,
    pub diff_ignore_ws: bool,
    pub diff_context: u32,
    pub show_activity: bool,
    /// Log background reads and API calls too, and expand every entry.
    pub verbose: bool,
    pub show_status_bar: bool,
    /// Scan the working tree for secrets when a repository is opened.
    pub scan_on_open: bool,
    /// Home page (inbox) preferences.
    pub home: crate::devops::home::Prefs,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            open_repos: Vec::new(),
            active: 0,
            recent: Vec::new(),
            dark: true,
            relative_dates: true,
            show_author: true,
            pull: PullPref::default(),
            repo_prefs: HashMap::new(),
            diff_mode: DiffMode::Unified,
            diff_ignore_ws: false,
            diff_context: 3,
            show_activity: false,
            verbose: true,
            show_status_bar: true,
            scan_on_open: true,
            home: Default::default(),
        }
    }
}

/// A branch being dragged from the sidebar or a graph label.
#[derive(Clone, Debug)]
pub struct DragBranch {
    /// Short name, e.g. `feature` or `origin/feature`.
    pub name: String,
    pub is_remote: bool,
    pub target: Oid,
}

/// Where a drag was released.
#[derive(Clone, Debug)]
pub struct DropTarget {
    pub commit: Oid,
    pub short: String,
    /// Local branches whose tip is this commit.
    pub local_branches: Vec<String>,
    pub is_head: bool,
}

#[derive(Clone, Debug)]
pub enum Dialog {
    CreateBranch {
        name: String,
        start: String,
        start_label: String,
        checkout: bool,
    },
    CreateTag {
        name: String,
        target: String,
        target_label: String,
        message: String,
    },
    RenameBranch {
        from: String,
        to: String,
    },
    DeleteBranch {
        name: String,
        force: bool,
        remote: Option<(String, String)>,
        delete_remote: bool,
    },
    Confirm {
        title: String,
        body: String,
        confirm: String,
        danger: bool,
        ops: Vec<GitOp>,
    },
    /// Confirm, then run any action (DevOps changes). `details` are shown
    /// in a monospace block: the exact commands and request bodies.
    ConfirmAction {
        title: String,
        body: String,
        confirm: String,
        danger: bool,
        details: Vec<String>,
        action: Box<Action>,
    },
    Reset {
        branch: String,
        target: String,
        target_label: String,
        mode: ResetMode,
    },
    Push {
        branch: String,
        remotes: Vec<String>,
        remote: usize,
        set_upstream: bool,
        force: bool,
    },
    Stash {
        message: String,
        include_untracked: bool,
    },
    Drop {
        source: DragBranch,
        target: DropTarget,
        head_branch: Option<String>,
    },
    Clone {
        url: String,
        dir: String,
        name: String,
        /// The folder name was typed by hand; stop deriving it from the URL.
        name_edited: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

pub struct Toast {
    pub kind: ToastKind,
    pub title: String,
    pub body: String,
    pub created: Instant,
}

impl Toast {
    pub fn ttl_secs(&self) -> f32 {
        match self.kind {
            ToastKind::Error => 12.0,
            _ => 4.5,
        }
    }
}

/// Everything a view can ask the app to do.
#[derive(Clone, Debug)]
pub enum Action {
    Run(GitOp),
    RunSeq(Vec<GitOp>),
    Dialog(Dialog),
    Select(Selection),
    /// Select a commit and scroll it into view.
    JumpTo(Oid),
    OpenFile(DiffTarget),
    CloseFile,
    Reload(bool),
    PickRepo,
    CloneDialog,
    Clone {
        url: String,
        dest: PathBuf,
    },
    OpenRepo(PathBuf),
    CloseRepo(usize),
    Activate(usize),
    ToggleTheme,
    ToggleRelativeDates,
    ToggleAuthor,
    ToggleVerbose,
    ToggleStatusBar,
    SetPull(PullPref),
    Toast(ToastKind, String),
    Copy(String),
    OpenPath(PathBuf),
    ToggleHidden(String),
    Solo(Option<String>),
    ToggleRemotes,
    ToggleTags,
    FocusSearch,
    SearchStep(i32),
    SearchChanged,
    MoveSelection(i32),
    SetDiffMode(DiffMode),
    ToggleDiffWhitespace,
    SetDiffContext(u32),
    ToggleActivity,
    ClearActivity,
    RefreshGitHub,
    Undo,
    OpenUrl(String),
    /// Escape in the diff view: clear the line selection, else close the file.
    DiffEscape,
    ShowPage(Page),
    /// Review, CI, Ship, Govern, Workbench, forge and home actions.
    DevOps(DevOpsAction),
    /// Stop, re-run, copy or remove a console run.
    Console(ConsoleAction),
    /// Show a run on the GitHub page, loading its jobs; optionally focus a job.
    OpenRun {
        run: u64,
        job: Option<u64>,
    },
    /// Reload the jobs of a run.
    RefreshRun(u64),
    /// Show (and fetch if needed) a job's log, scrolled to a step.
    ShowLog {
        run: u64,
        job: u64,
        step: Option<u32>,
    },
    LoadAnnotations {
        run: u64,
        job: u64,
    },
    /// Show a pull request on the Review page.
    OpenPr(u64),
    RefreshChecks(Oid),
    FindContaining(Oid),
    /// Scan the working tree (and history when asked) for secrets.
    ScanSecrets {
        history: bool,
    },
    CancelScan,
    /// Select an exposure finding by fingerprint (`alert:N` for GitHub alerts).
    SelectFinding(Option<String>),
    /// Mark a finding as a false positive / take that back.
    DismissFinding(String),
    RestoreFinding(String),
    /// Show or hide the clear-text secret of a finding.
    ToggleReveal(String),
    /// Append a pattern to the repository's .gitignore.
    AddToGitignore(String),
    /// Fetch repository visibility and secret-scanning alerts from GitHub.
    FetchExposure,
    SetHistoryDepth(usize),
    ToggleAllRefs,
    ToggleScanOnOpen,
    SetSecurityTab(SecurityTab),
    /// Run (or re-run) the OSINT analysis: people, footprint, metadata,
    /// history leftovers, supply chain.
    RunOsint,
    CancelOsint,
    /// `git fsck --unreachable` plus a secret scan of the unreachable blobs.
    RunUnreachable,
    /// Check the lockfile packages against osv.dev.
    RunOsv,
    /// Forks, Dependabot and code-scanning alerts, admin surface.
    FetchGhSurface,
    /// Run the code-search dorks against the owner.
    RunDorks,
    /// Ask GitHub whether it still serves these commits.
    CheckServed(Vec<Oid>),
    /// List the pull-request refs the remote advertises.
    CheckPrRefs,
    SelectOsint(Option<String>),
    SetInsightsTab(InsightsTab),
    /// Run (or re-run) the insights analysis.
    RunInsights,
    CancelInsights,
    SelectInsight(Option<String>),
    SetInsightsDepth(usize),
    ToggleInsightsAllRefs,
    /// Walk every object in history for the weight report.
    RunWeight,
    CancelWeight,
    /// Date the debt markers through `git blame`.
    DateDebt,
    CancelDating,
    /// Blame summary of one file.
    BlameFile(String),
    /// Run the history search with the query on the page.
    RunSearch,
    CancelSearch,
    /// Find lost commits and stashes.
    RunLost,
    CancelLost,
}
