//! Everything derived from one streamed `git log --numstat` pass: hotspots,
//! temporal coupling, code age, ownership and bus factor, commit hygiene and
//! provenance, contributor timeline and language breakdown. Plus the
//! on-demand blame summary of a single file.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use regex::Regex;

use super::{
    FileChange, InsightsOptions, LogCommit, LogRequest, Progress, TrackedFile, days_between,
    dir_of, ext_of, git_ok, stream_log,
};
use crate::{Error, Oid, Result, cli};

// ---------------------------------------------------------------------------
// Hotspots, coupling, age
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Hotspot {
    pub path: String,
    pub commits: u32,
    pub added: u64,
    pub deleted: u64,
    /// Distinct author e-mails.
    pub authors: u32,
    /// Commits whose message looks like a bug fix (fix/bug/crash/regression…).
    pub fixes: u32,
    pub first: i64,
    pub last: i64,
    /// Current size in HEAD (None when the file is gone).
    pub size: Option<u64>,
    /// Churn score: `commits × ln(1 + lines)` weighted towards recent
    /// changes (half-life one year). Exactly:
    /// `Σ over non-merge commits c touching the path of
    /// ln(1 + lines_c) × 0.5^(age_c / 365)` where `lines_c` is the commit's
    /// added + deleted lines in the file (a binary change counts as one
    /// line) and `age_c` the days between the commit and the newest commit
    /// in the log, so the newest change weighs 1 whatever the repository's
    /// age.
    pub score: f64,
}

/// Two files that keep changing in the same commits.
#[derive(Clone, Debug, PartialEq)]
pub struct Coupling {
    pub a: String,
    pub b: String,
    /// Commits touching both.
    pub together: u32,
    pub a_commits: u32,
    pub b_commits: u32,
    /// `together / min(a_commits, b_commits)`.
    pub confidence: f64,
    /// The two live in different top-level directories.
    pub cross_dir: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirAge {
    pub dir: String,
    pub files: u32,
    /// Median days since each file's last change.
    pub median_age_days: u32,
    pub oldest: i64,
    pub newest: i64,
    /// Files untouched for more than two years.
    pub stale_files: u32,
    /// Files changed in the last 90 days.
    pub active_files: u32,
}

// ---------------------------------------------------------------------------
// Ownership
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Owner {
    pub name: String,
    pub email: String,
    /// Lines added (log pass) or surviving lines (blame).
    pub lines: u64,
    pub commits: u32,
    pub last: i64,
    /// Share of the directory's / file's lines, 0..1.
    pub share: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DirOwnership {
    pub dir: String,
    pub files: u32,
    pub lines: u64,
    pub commits: u32,
    /// Top owners by lines added, desc (at most 8).
    pub owners: Vec<Owner>,
    /// Fewest owners whose shares reach 50%.
    pub bus_factor: u32,
    /// Bus factor 1 and that owner's last commit anywhere is older than a year.
    pub orphaned: bool,
    pub last_activity: i64,
}

// ---------------------------------------------------------------------------
// Commit quality: hygiene and provenance
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitNote {
    pub id: Oid,
    pub summary: String,
    pub author: String,
    pub time: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevertNote {
    pub commit: CommitNote,
    /// The commit it reverts (from "This reverts commit …").
    pub reverts: Oid,
    /// The reverted commit was itself a revert (a revert of a revert).
    pub double: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GiantCommit {
    pub commit: CommitNote,
    pub files: u32,
    pub lines: u64,
    pub is_merge: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DateAnomaly {
    pub commit: CommitNote,
    /// `future` (dated after now + 1 day), `epoch` (before 2000),
    /// `rebased` (committer later than author by more than a day),
    /// `backdated` (committer earlier than author), `offset-jump` (author's
    /// UTC offset differs from their usual by ≥ 5 hours).
    pub kind: &'static str,
    pub detail: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Signatures {
    /// Newest commits checked with `%G?`.
    pub checked: u32,
    pub good: u32,
    pub bad: u32,
    /// `U`: good signature, unknown validity.
    pub unknown: u32,
    /// `X`/`Y`: expired signature or key.
    pub expired: u32,
    pub revoked: u32,
    /// `E`: could not be checked (missing key, no gpg).
    pub error: u32,
    pub none: u32,
    /// `(author, signed, total)` per author among the checked commits.
    pub by_author: Vec<(String, u32, u32)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CommitQuality {
    pub total: u32,
    pub merges: u32,
    pub reverts: Vec<RevertNote>,
    /// `fixup!`/`squash!`/`amend!` commits that were never squashed.
    pub fixups: Vec<CommitNote>,
    /// `WIP`, `wip`, `asdf`, `fix`, `.`, `test` and similar throwaway messages.
    pub wip: Vec<CommitNote>,
    pub one_word: u32,
    /// Summary only, no body.
    pub no_body: u32,
    /// Summary longer than 72 characters.
    pub long_summary: u32,
    /// Message references an issue (`#123`, `ABC-123`, `Fixes …`).
    pub with_issue_ref: u32,
    /// Conventional Commits style (`type(scope)!: …`).
    pub conventional: u32,
    /// `(type, count)` desc.
    pub conventional_types: Vec<(String, u32)>,
    /// Commits touching ≥ 50 files or ≥ 5,000 lines (desc by lines, top 40).
    pub giant: Vec<GiantCommit>,
    pub median_files: u32,
    pub median_lines: u32,
    /// Authors or committers that look like bots (`[bot]`, dependabot,
    /// renovate, github-actions…).
    pub bot_commits: u32,
    pub bots: Vec<(String, u32)>,
    /// Author differs from committer (rebases, cherry-picks, web merges).
    pub author_committer_differ: u32,
    /// At most 50 examples per kind; `anomaly_counts` has the full counts.
    pub date_anomalies: Vec<DateAnomaly>,
    /// `(kind, commits)` for every [`DateAnomaly::kind`], in the order
    /// `future`, `epoch`, `rebased`, `backdated`, `offset-jump`, zero
    /// counts included.
    pub anomaly_counts: Vec<(&'static str, u32)>,
    pub signatures: Option<Signatures>,
    /// `(year, merges, total)` ascending.
    pub merges_by_year: Vec<(i32, u32, u32)>,
    /// `(year, conventional, total)` ascending.
    pub conventional_by_year: Vec<(i32, u32, u32)>,
}

// ---------------------------------------------------------------------------
// Timeline
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Month {
    /// `2026-10`.
    pub ym: String,
    pub commits: u32,
    pub merges: u32,
    pub authors: u32,
    /// Authors whose first commit falls in this month.
    pub new_authors: u32,
    /// Authors whose last commit falls in this month (and is older than
    /// 90 days before the newest commit).
    pub departed: u32,
    pub added: u64,
    pub deleted: u64,
    pub files: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorSpan {
    pub name: String,
    pub email: String,
    pub first: i64,
    pub last: i64,
    pub commits: u32,
    pub active_months: u32,
    pub added: u64,
    pub deleted: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Language {
    pub name: String,
    pub files: usize,
    pub bytes: u64,
    /// Share of bytes, 0..1.
    pub share: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Timeline {
    /// Ascending by month; months without commits are included between the
    /// first and last.
    pub months: Vec<Month>,
    /// Desc by commits.
    pub authors: Vec<AuthorSpan>,
    /// Active in the last 90 days.
    pub active_authors: u32,
    pub languages: Vec<Language>,
    pub total_files: usize,
    pub total_bytes: u64,
    pub first_commit: i64,
    pub last_commit: i64,
    pub age_days: u32,
    /// Busiest weekday (0 = Monday) and hour (local to each author).
    pub peak_weekday: u8,
    pub peak_hour: u8,
    pub heat: [[u32; 24]; 7],
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct History {
    /// Desc by score, top 500.
    pub hotspots: Vec<Hotspot>,
    /// Desc by `together`, pairs with together ≥ 3 and confidence ≥ 0.5,
    /// top 300.
    pub couplings: Vec<Coupling>,
    /// Directories at depth 2, desc by median age.
    pub age: Vec<DirAge>,
    /// Directories at depth 2, orphaned first, then bus factor asc.
    pub ownership: Vec<DirOwnership>,
    pub quality: CommitQuality,
    pub timeline: Timeline,
    /// Commits the pass read.
    pub commits: usize,
    /// Distinct paths seen.
    pub paths: usize,
}

// ---------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------

/// Half-life of the hotspot recency weight.
const HALF_LIFE_DAYS: f64 = 365.0;
const HOTSPOT_LIMIT: usize = 500;
/// Below this many hotspot paths still in HEAD, deleted paths stay listed.
const HOTSPOT_MIN_EXISTING: usize = 50;
const COUPLING_LIMIT: usize = 300;
const COUPLING_MIN_TOGETHER: u32 = 3;
const COUPLING_MIN_CONFIDENCE: f64 = 0.5;
/// Commits touching more files than this are bulk changes, not coupling.
const COUPLING_MAX_FILES: usize = 25;
const STALE_DAYS: u64 = 730;
const ACTIVE_DAYS: u64 = 90;
const OWNERSHIP_MIN_LINES: u64 = 100;
const OWNERS_SHOWN: usize = 8;
const ORPHAN_SECS: i64 = 365 * 86_400;
const GIANT_FILES: usize = 50;
const GIANT_LINES: u64 = 5_000;
const GIANT_LIMIT: usize = 40;
const ANOMALY_EXAMPLES: u32 = 50;
const ANOMALY_KINDS: [&str; 5] = ["future", "epoch", "rebased", "backdated", "offset-jump"];
const OFFSET_JUMP_MINUTES: i32 = 300;
/// An author's usual UTC offset is only trusted after this many commits.
const OFFSET_JUMP_MIN_COMMITS: u32 = 10;
/// 2000-01-01T00:00:00Z.
const EPOCH_2000: i64 = 946_684_800;
const SIGNATURE_COMMITS: usize = 300;
const SIGNATURE_AUTHORS: usize = 20;
const PROGRESS_EVERY: usize = 200;
/// Longest month range the timeline renders; garbage dates would otherwise
/// stretch it back to 1970.
const TIMELINE_MAX_MONTHS: i32 = 1_200;
const DEPARTED_SECS: i64 = 90 * 86_400;

const WIP_WORDS: &[&str] = &[
    "wip", "fix", "fixes", "test", "tests", "asdf", "update", "updates", "changes", "stuff",
    "misc", "tmp", "temp", "oops", "typo", ".", "...", "-",
];

struct Patterns {
    fix: Regex,
    issue: Regex,
    conventional: Regex,
    revert: Regex,
    bot: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        fix: Regex::new(
            r"(?i)\b(fix(es|ed)?|bug|crash|regression|hotfix|leak|race|overflow|null ?pointer|segfault)\b",
        )
        .expect("fix regex"),
        issue: Regex::new(r"#\d+|\b[A-Z][A-Z0-9]+-\d+\b|(?i:\b(fixes|closes|resolves|refs?)\b)")
            .expect("issue regex"),
        conventional: Regex::new(
            r"^(feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert)(\([^)]*\))?!?:\s",
        )
        .expect("conventional regex"),
        revert: Regex::new(r"This reverts commit ([0-9a-f]{40})").expect("revert regex"),
        bot: Regex::new(
            r"(?i)\[bot\]|dependabot|renovate|github-actions|greenkeeper|snyk-bot|mergify|semantic-release|codecov|imgbot|allcontributors|\bbot@",
        )
        .expect("bot regex"),
    })
}

// ---------------------------------------------------------------------------
// Small pure helpers
// ---------------------------------------------------------------------------

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Proleptic Gregorian `(year, month, day)` of a day count since
/// 1970-01-01 (Howard Hinnant's `civil_from_days`), valid for negative
/// days too.
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year as i32, month, day)
}

/// UTC `(year, month)` of an epoch time.
fn year_month(epoch: i64) -> (i32, u32) {
    let (y, m, _) = civil_from_days(epoch.div_euclid(86_400));
    (y, m)
}

/// Months since year 0, so consecutive months differ by one.
fn month_index(year: i32, month: u32) -> i32 {
    year * 12 + month as i32 - 1
}

/// `2026-10` for a month index.
fn ym_label(index: i32) -> String {
    format!(
        "{:04}-{:02}",
        index.div_euclid(12),
        index.rem_euclid(12) + 1
    )
}

/// `2026-10-26` of an epoch time (UTC).
fn ymd_label(epoch: i64) -> String {
    let (y, m, d) = civil_from_days(epoch.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// `(weekday Monday=0, hour)` of an epoch time at a UTC offset in minutes.
fn local_slot(at: i64, offset_min: i32) -> (usize, usize) {
    let local = at + offset_min as i64 * 60;
    let days = local.div_euclid(86_400);
    // 1970-01-01 was a Thursday: Monday-based index 3.
    let weekday = (days + 3).rem_euclid(7) as usize;
    let hour = (local.rem_euclid(86_400) / 3600) as usize;
    (weekday, hour)
}

/// `+02:00`, `-05:30`.
fn fmt_offset(minutes: i32) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    let m = minutes.abs();
    format!("{sign}{:02}:{:02}", m / 60, m % 60)
}

/// `5 min`, `3 h`, `2 days` for a positive span in seconds.
fn human_span(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 3_600 {
        format!("{} min", (secs / 60).max(1))
    } else if secs < 86_400 {
        format!("{} h", secs / 3_600)
    } else {
        format!("{} days", secs / 86_400)
    }
}

/// Throwaway commit messages: `wip`, `fix`, `asdf`, `.`, two characters…
fn is_wip(summary: &str) -> bool {
    let s = summary.trim().to_lowercase();
    s.chars().count() <= 2
        || WIP_WORDS.contains(&s.as_str())
        || s.starts_with("wip ")
        || s.starts_with("wip:")
}

fn is_fixup(summary: &str) -> bool {
    summary.starts_with("fixup!") || summary.starts_with("squash!") || summary.starts_with("amend!")
}

/// Fewest owners whose cumulative share reaches 50%, over shares sorted
/// descending; at least 1 when there is anyone, 0 for nobody.
fn bus_factor(shares_desc: &[f64]) -> u32 {
    let mut sum = 0.0;
    for (i, share) in shares_desc.iter().enumerate() {
        sum += share;
        if sum >= 0.5 - 1e-9 {
            return i as u32 + 1;
        }
    }
    shares_desc.len() as u32
}

/// Lower median (the `(n-1)/2`-th smallest); 0 for no values. Sorts in place.
fn lower_median(values: &mut [u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    values[(values.len() - 1) / 2]
}

/// Language name of a path by extension (or well-known file name); anything
/// unknown is `Other`, so a large tree does not produce a hundred
/// one-file "languages".
fn language_of(path: &str) -> String {
    let base = crate::secrets::basename(path).to_lowercase();
    match base.as_str() {
        "makefile" | "gnumakefile" => return "Make".into(),
        "dockerfile" => return "Dockerfile".into(),
        "cmakelists.txt" => return "CMake".into(),
        _ => {}
    }
    let ext = ext_of(path);
    let name = match ext.as_str() {
        "rs" => "Rust",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" => "Python",
        "go" => "Go",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "swift" => "Swift",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "C++",
        "cs" => "C#",
        "rb" => "Ruby",
        "php" => "PHP",
        "scala" => "Scala",
        "dart" => "Dart",
        "m" | "mm" => "Objective-C",
        "sh" | "bash" | "zsh" => "Shell",
        "ps1" | "psm1" => "PowerShell",
        "html" | "htm" => "HTML",
        "css" | "scss" | "less" => "CSS",
        "json" => "JSON",
        "yml" | "yaml" => "YAML",
        "toml" => "TOML",
        "md" | "markdown" => "Markdown",
        "sql" => "SQL",
        "proto" => "Protobuf",
        "lua" => "Lua",
        "r" => "R",
        "ex" | "exs" => "Elixir",
        "erl" => "Erlang",
        "hs" => "Haskell",
        "clj" | "cljs" => "Clojure",
        "vue" => "Vue",
        "svelte" => "Svelte",
        "zig" => "Zig",
        "nim" => "Nim",
        "pl" | "pm" => "Perl",
        "gradle" => "Gradle",
        "cmake" => "CMake",
        "mk" => "Make",
        "dockerfile" => "Dockerfile",
        "xml" => "XML",
        "txt" | "text" | "rst" | "adoc" => "Text",
        "gn" | "gni" => "GN",
        "gyp" | "gypi" => "GYP",
        "grd" | "grdp" | "xtb" | "resx" | "strings" | "po" | "pot" => "Resources",
        "patch" | "diff" => "Patch",
        "svg" => "SVG",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" | "bmp" | "icns" => "Images",
        "lock" => "Lockfiles",
        "idl" | "mojom" | "thrift" | "fbs" => "IDL",
        "wasm" | "wat" => "WebAssembly",
        "asm" | "s" => "Assembly",
        "tf" | "tfvars" | "hcl" => "Terraform",
        "bat" | "cmd" => "Batch",
        "csv" | "tsv" | "ndjson" => "Data",
        "ini" | "cfg" | "conf" | "properties" | "env" => "Config",
        "key" | "pem" | "crt" | "cer" | "p12" | "pfx" => "Keys and certificates",
        "vcxproj" | "sln" | "csproj" | "props" | "targets" => "Visual Studio",
        "ttf" | "otf" | "woff" | "woff2" => "Fonts",
        "wxs" | "nsi" | "iss" => "Installers",
        _ => "Other",
    };
    name.to_owned()
}

fn note(c: &LogCommit) -> CommitNote {
    CommitNote {
        id: c.id,
        summary: c.summary.clone(),
        author: c.author.clone(),
        time: c.time,
    }
}

// ---------------------------------------------------------------------------
// The accumulator behind `analyze`
// ---------------------------------------------------------------------------

/// Per-path totals. Paths are interned; a rename moves the accumulator of
/// the old name into the new one (see [`Acc::rename`]).
struct PathAcc {
    commits: u32,
    added: u64,
    deleted: u64,
    /// Author ids, one per touching commit; deduplicated in `finish`.
    authors: Vec<u32>,
    fixes: u32,
    first: i64,
    last: i64,
    /// Recency-weighted churn, see [`Hotspot::score`]. Accumulated with
    /// `now` as the reference and rescaled to the newest commit in `finish`.
    score: f64,
}

impl Default for PathAcc {
    fn default() -> Self {
        Self {
            commits: 0,
            added: 0,
            deleted: 0,
            authors: Vec::new(),
            fixes: 0,
            first: i64::MAX,
            last: i64::MIN,
            score: 0.0,
        }
    }
}

impl PathAcc {
    fn absorb(&mut self, other: PathAcc) {
        self.commits += other.commits;
        self.added += other.added;
        self.deleted += other.deleted;
        self.authors.extend(other.authors);
        self.fixes += other.fixes;
        self.first = self.first.min(other.first);
        self.last = self.last.max(other.last);
        self.score += other.score;
    }
}

struct AuthorAcc {
    /// The most recent name seen for the e-mail (the log is newest first).
    name: String,
    email: String,
    first: i64,
    last: i64,
    commits: u32,
    added: u64,
    deleted: u64,
    /// Month indexes with a commit; deduplicated in `finish`.
    months: Vec<i32>,
    /// `(utc offset minutes, commits)` histogram.
    offsets: Vec<(i32, u32)>,
}

#[derive(Default)]
struct MonthAcc {
    commits: u32,
    merges: u32,
    authors: HashSet<u32>,
    added: u64,
    deleted: u64,
    files: HashSet<u32>,
}

#[derive(Default)]
struct YearAcc {
    total: u32,
    merges: u32,
    conventional: u32,
}

#[derive(Default)]
struct DirAcc {
    /// Distinct commits touching the directory.
    commits: u32,
    last_activity: i64,
}

#[derive(Default)]
struct DirAuthorAcc {
    lines: u64,
    commits: u32,
    last: i64,
}

struct Acc<'a> {
    now: i64,
    head: HashMap<&'a str, &'a TrackedFile>,
    // Paths.
    paths: Vec<String>,
    path_ids: HashMap<String, u32>,
    path_acc: Vec<PathAcc>,
    /// Attribution redirect: commits naming path `i` (older than the rename
    /// that introduced the redirect) count towards `alias[i]`.
    alias: Vec<u32>,
    /// Where an accumulator went when a rename merged it (`moved[i] == i`
    /// when it stayed).
    moved: Vec<u32>,
    exists: Vec<bool>,
    /// Depth-2 directory id of each path.
    path_dir: Vec<u32>,
    dirs: Vec<String>,
    dir_ids: HashMap<String, u32>,
    dir_acc: Vec<DirAcc>,
    dir_author: HashMap<(u32, u32), DirAuthorAcc>,
    // Authors.
    authors: Vec<AuthorAcc>,
    author_ids: HashMap<String, u32>,
    // Coupling: file ids of every non-merge commit with 2..=25 files, flat.
    pair_commits: Vec<u32>,
    pair_ranges: Vec<(u32, u32)>,
    // Calendar.
    months: HashMap<i32, MonthAcc>,
    years: BTreeMap<i32, YearAcc>,
    heat: [[u32; 24]; 7],
    first_time: i64,
    last_time: i64,
    // Quality.
    quality: CommitQuality,
    reverts: Vec<(CommitNote, Oid)>,
    conv_types: HashMap<String, u32>,
    bots: HashMap<String, u32>,
    anomaly_counts: [u32; 5],
    files_per_commit: Vec<u64>,
    lines_per_commit: Vec<u64>,
    // Scratch buffers reused per commit.
    id_buf: Vec<u32>,
    dir_buf: Vec<u32>,
    commits: usize,
}

impl<'a> Acc<'a> {
    fn new(files: &'a [TrackedFile], now: i64) -> Self {
        Self {
            now,
            head: files.iter().map(|f| (f.path.as_str(), f)).collect(),
            paths: Vec::new(),
            path_ids: HashMap::new(),
            path_acc: Vec::new(),
            alias: Vec::new(),
            moved: Vec::new(),
            exists: Vec::new(),
            path_dir: Vec::new(),
            dirs: Vec::new(),
            dir_ids: HashMap::new(),
            dir_acc: Vec::new(),
            dir_author: HashMap::new(),
            authors: Vec::new(),
            author_ids: HashMap::new(),
            pair_commits: Vec::new(),
            pair_ranges: Vec::new(),
            months: HashMap::new(),
            years: BTreeMap::new(),
            heat: [[0; 24]; 7],
            first_time: i64::MAX,
            last_time: i64::MIN,
            quality: CommitQuality::default(),
            reverts: Vec::new(),
            conv_types: HashMap::new(),
            bots: HashMap::new(),
            anomaly_counts: [0; 5],
            files_per_commit: Vec::new(),
            lines_per_commit: Vec::new(),
            id_buf: Vec::new(),
            dir_buf: Vec::new(),
            commits: 0,
        }
    }

    fn intern(&mut self, path: String) -> u32 {
        if let Some(&id) = self.path_ids.get(path.as_str()) {
            return id;
        }
        let id = self.paths.len() as u32;
        let dir = self.dir(dir_of(&path, 2));
        self.exists.push(self.head.contains_key(path.as_str()));
        self.path_dir.push(dir);
        self.path_acc.push(PathAcc::default());
        self.alias.push(id);
        self.moved.push(id);
        self.paths.push(path.clone());
        self.path_ids.insert(path, id);
        id
    }

    fn dir(&mut self, dir: String) -> u32 {
        if let Some(&id) = self.dir_ids.get(dir.as_str()) {
            return id;
        }
        let id = self.dirs.len() as u32;
        self.dir_acc.push(DirAcc::default());
        self.dirs.push(dir.clone());
        self.dir_ids.insert(dir, id);
        id
    }

    /// The accumulator a path name currently feeds (follows rename
    /// redirects, with path compression).
    fn resolve(&mut self, id: u32) -> u32 {
        let mut root = id;
        while self.alias[root as usize] != root {
            root = self.alias[root as usize];
        }
        let mut cur = id;
        while self.alias[cur as usize] != cur {
            let next = self.alias[cur as usize];
            self.alias[cur as usize] = root;
            cur = next;
        }
        root
    }

    /// Where the accumulator of `id` ended up after rename merges.
    fn home(&self, mut id: u32) -> u32 {
        while self.moved[id as usize] != id {
            id = self.moved[id as usize];
        }
        id
    }

    /// `old => new` seen in a commit (the log is newest first, so every
    /// commit still to come that names `old` means the file now called
    /// `new`). The history already accumulated under `old` (from newer
    /// commits: edits on another branch before the merge) moves to `new`,
    /// unless `old` is a live file in HEAD, which means a different file
    /// was created under that name later and keeps its own history.
    fn rename(&mut self, old: String, new_id: u32) -> u32 {
        let old_id = self.intern(old);
        let target = self.resolve(new_id);
        if old_id == target {
            return target;
        }
        if self.alias[old_id as usize] == old_id && !self.exists[old_id as usize] {
            let taken = std::mem::take(&mut self.path_acc[old_id as usize]);
            self.path_acc[target as usize].absorb(taken);
            self.moved[old_id as usize] = target;
        }
        self.alias[old_id as usize] = target;
        target
    }

    fn author(&mut self, email: &str, name: &str) -> u32 {
        let key = email.trim().to_lowercase();
        if let Some(&id) = self.author_ids.get(&key) {
            return id;
        }
        let id = self.authors.len() as u32;
        self.authors.push(AuthorAcc {
            name: name.trim().to_owned(),
            email: key.clone(),
            first: i64::MAX,
            last: i64::MIN,
            commits: 0,
            added: 0,
            deleted: 0,
            months: Vec::new(),
            offsets: Vec::new(),
        });
        self.author_ids.insert(key, id);
        id
    }

    fn commit(&mut self, c: LogCommit) {
        let p = patterns();
        self.commits += 1;
        let is_merge = c.is_merge();
        let time = c.time;
        let author = self.author(&c.email, &c.author);
        self.first_time = self.first_time.min(time);
        self.last_time = self.last_time.max(time);
        let (year, month) = year_month(time);
        let midx = month_index(year, month);
        let added = c.added();
        let deleted = c.deleted();

        // Author span and time-zone habit.
        let offset_jump = {
            let a = &mut self.authors[author as usize];
            a.commits += 1;
            a.first = a.first.min(time);
            a.last = a.last.max(time);
            a.added += added;
            a.deleted += deleted;
            if a.months.last() != Some(&midx) {
                a.months.push(midx);
            }
            let seen = a.commits - 1;
            let usual = a.offsets.iter().max_by_key(|(_, n)| *n).map(|(o, _)| *o);
            match a.offsets.iter_mut().find(|(o, _)| *o == c.offset_minutes) {
                Some(slot) => slot.1 += 1,
                None => a.offsets.push((c.offset_minutes, 1)),
            }
            usual.filter(|u| {
                seen >= OFFSET_JUMP_MIN_COMMITS
                    && (c.offset_minutes - u).abs() >= OFFSET_JUMP_MINUTES
            })
        };

        // Calendar.
        {
            let m = self.months.entry(midx).or_default();
            m.commits += 1;
            m.merges += u32::from(is_merge);
            m.authors.insert(author);
            m.added += added;
            m.deleted += deleted;
        }
        let (wd, hour) = local_slot(time, c.offset_minutes);
        self.heat[wd][hour] += 1;
        let conventional = p
            .conventional
            .captures(&c.summary)
            .map(|cap| cap[1].to_owned());
        {
            let y = self.years.entry(year).or_default();
            y.total += 1;
            y.merges += u32::from(is_merge);
            y.conventional += u32::from(conventional.is_some());
        }

        self.hygiene(&c, is_merge, conventional, offset_jump);

        let nfiles = c.files.len();
        let lines = added + deleted;
        if (nfiles >= GIANT_FILES || lines >= GIANT_LINES) && (!is_merge || nfiles > 0) {
            self.quality.giant.push(GiantCommit {
                commit: note(&c),
                files: nfiles as u32,
                lines,
                is_merge,
            });
            if self.quality.giant.len() > GIANT_LIMIT * 4 {
                self.quality
                    .giant
                    .sort_by_key(|g| std::cmp::Reverse(g.lines));
                self.quality.giant.truncate(GIANT_LIMIT);
            }
        }
        // Per-file stats skip merges: their numstat is relative to the first
        // parent and would count the merged work twice.
        if is_merge {
            return;
        }
        self.files_per_commit.push(nfiles as u64);
        self.lines_per_commit.push(lines);
        let fix = p.fix.is_match(&c.summary);
        // Weight relative to now; `finish` rescales so the newest commit
        // weighs 1 (the ratio between commits is unchanged).
        let age_days = (self.now - time).max(0) as f64 / 86_400.0;
        let weight = 0.5f64.powf(age_days / HALF_LIFE_DAYS);
        self.files(c.files, author, time, fix, weight, midx);
    }

    fn hygiene(
        &mut self,
        c: &LogCommit,
        is_merge: bool,
        conventional: Option<String>,
        offset_jump: Option<i32>,
    ) {
        let p = patterns();
        let q = &mut self.quality;
        q.total += 1;
        q.merges += u32::from(is_merge);
        let summary = c.summary.as_str();
        if let Some(cap) = p
            .revert
            .captures(&c.body)
            .or_else(|| p.revert.captures(summary))
            && let Ok(id) = Oid::from_str(&cap[1])
        {
            self.reverts.push((note(c), id));
        }
        if is_fixup(summary) {
            q.fixups.push(note(c));
        } else if is_wip(summary) {
            q.wip.push(note(c));
        }
        if summary.split_whitespace().count() == 1 {
            q.one_word += 1;
        }
        if c.body.is_empty() {
            q.no_body += 1;
        }
        if summary.chars().count() > 72 {
            q.long_summary += 1;
        }
        if p.issue.is_match(summary) || p.issue.is_match(&c.body) {
            q.with_issue_ref += 1;
        }
        if let Some(kind) = conventional {
            q.conventional += 1;
            *self.conv_types.entry(kind).or_insert(0) += 1;
        }
        let bot = if p.bot.is_match(&c.author) || p.bot.is_match(&c.email) {
            Some(&c.author)
        } else if p.bot.is_match(&c.committer) || p.bot.is_match(&c.committer_email) {
            Some(&c.committer)
        } else {
            None
        };
        if let Some(name) = bot {
            q.bot_commits += 1;
            *self.bots.entry(name.clone()).or_insert(0) += 1;
        }
        if !c.email.eq_ignore_ascii_case(&c.committer_email) {
            q.author_committer_differ += 1;
        }

        let counts = &mut self.anomaly_counts;
        let now = self.now;
        if c.time > now + 86_400 {
            let detail = format!("authored {} days from now", days_between(now, c.time));
            Self::anomaly(q, counts, 0, c, detail);
        }
        if c.time < EPOCH_2000 {
            let detail = format!("authored {}", ymd_label(c.time));
            Self::anomaly(q, counts, 1, c, detail);
        }
        if c.commit_time - c.time > 86_400 {
            let detail = format!(
                "committed {} after authoring",
                human_span(c.commit_time - c.time)
            );
            Self::anomaly(q, counts, 2, c, detail);
        }
        if c.commit_time < c.time - 300 {
            let detail = format!(
                "committed {} before authoring",
                human_span(c.time - c.commit_time)
            );
            Self::anomaly(q, counts, 3, c, detail);
        }
        if let Some(usual) = offset_jump {
            let detail = format!(
                "UTC{} instead of the usual UTC{}",
                fmt_offset(c.offset_minutes),
                fmt_offset(usual)
            );
            Self::anomaly(q, counts, 4, c, detail);
        }
    }

    fn anomaly(
        q: &mut CommitQuality,
        counts: &mut [u32; 5],
        kind: usize,
        c: &LogCommit,
        detail: String,
    ) {
        counts[kind] += 1;
        if counts[kind] <= ANOMALY_EXAMPLES {
            q.date_anomalies.push(DateAnomaly {
                commit: note(c),
                kind: ANOMALY_KINDS[kind],
                detail,
            });
        }
    }

    fn files(
        &mut self,
        files: Vec<FileChange>,
        author: u32,
        time: i64,
        fix: bool,
        weight: f64,
        midx: i32,
    ) {
        let mut ids = std::mem::take(&mut self.id_buf);
        let mut dirs = std::mem::take(&mut self.dir_buf);
        ids.clear();
        dirs.clear();
        for f in files {
            let id = self.intern(f.path);
            let id = match f.old_path {
                Some(old) => self.rename(old, id),
                None => self.resolve(id),
            };
            let added = u64::from(f.added.unwrap_or(0));
            let deleted = u64::from(f.deleted.unwrap_or(0));
            let weighted = if f.added.is_none() {
                1
            } else {
                added + deleted
            };
            let pa = &mut self.path_acc[id as usize];
            pa.commits += 1;
            pa.added += added;
            pa.deleted += deleted;
            pa.authors.push(author);
            pa.fixes += u32::from(fix);
            pa.first = pa.first.min(time);
            pa.last = pa.last.max(time);
            pa.score += (1.0 + weighted as f64).ln() * weight;
            ids.push(id);
            let dir = self.path_dir[id as usize];
            dirs.push(dir);
            self.dir_author.entry((dir, author)).or_default().lines += added;
            if let Some(m) = self.months.get_mut(&midx) {
                m.files.insert(id);
            }
        }
        ids.sort_unstable();
        ids.dedup();
        dirs.sort_unstable();
        dirs.dedup();
        for &dir in &dirs {
            let da = self.dir_author.entry((dir, author)).or_default();
            da.commits += 1;
            da.last = da.last.max(time);
            let d = &mut self.dir_acc[dir as usize];
            d.commits += 1;
            d.last_activity = d.last_activity.max(time);
        }
        if (2..=COUPLING_MAX_FILES).contains(&ids.len()) {
            let start = self.pair_commits.len() as u32;
            self.pair_commits.extend_from_slice(&ids);
            self.pair_ranges.push((start, ids.len() as u32));
        }
        self.id_buf = ids;
        self.dir_buf = dirs;
    }

    fn finish(mut self, files: &[TrackedFile]) -> History {
        let newest = if self.commits > 0 {
            self.last_time
        } else {
            self.now
        };
        // Scores were weighted against `now`; make the newest commit weigh 1.
        let rescale = 2f64.powf((self.now - newest).max(0) as f64 / 86_400.0 / HALF_LIFE_DAYS);
        for pa in &mut self.path_acc {
            if pa.commits > 0 {
                pa.authors.sort_unstable();
                pa.authors.dedup();
                pa.score *= rescale;
            }
        }
        for a in &mut self.authors {
            a.months.sort_unstable();
            a.months.dedup();
        }
        let hotspots = self.hotspots();
        let couplings = self.couplings();
        let age = self.age(files);
        let ownership = self.ownership(newest);
        let quality = self.quality_report();
        let timeline = self.timeline(files, newest);
        History {
            hotspots,
            couplings,
            age,
            ownership,
            quality,
            timeline,
            commits: self.commits,
            paths: self.paths.len(),
        }
    }

    fn hotspots(&self) -> Vec<Hotspot> {
        let mut live: Vec<u32> = (0..self.paths.len() as u32)
            .filter(|&i| self.path_acc[i as usize].commits > 0)
            .collect();
        let existing = live.iter().filter(|&&i| self.exists[i as usize]).count();
        if existing >= HOTSPOT_MIN_EXISTING {
            live.retain(|&i| self.exists[i as usize]);
        }
        live.sort_by(|&a, &b| {
            let (x, y) = (&self.path_acc[a as usize], &self.path_acc[b as usize]);
            y.score
                .total_cmp(&x.score)
                .then_with(|| y.commits.cmp(&x.commits))
                .then_with(|| self.paths[a as usize].cmp(&self.paths[b as usize]))
        });
        live.truncate(HOTSPOT_LIMIT);
        live.iter()
            .map(|&i| {
                let pa = &self.path_acc[i as usize];
                let path = &self.paths[i as usize];
                Hotspot {
                    path: path.clone(),
                    commits: pa.commits,
                    added: pa.added,
                    deleted: pa.deleted,
                    authors: pa.authors.len() as u32,
                    fixes: pa.fixes,
                    first: pa.first,
                    last: pa.last,
                    size: self.head.get(path.as_str()).and_then(|f| f.size),
                    score: pa.score,
                }
            })
            .collect()
    }

    fn couplings(&self) -> Vec<Coupling> {
        // Only files with ≥ 3 commits in HEAD can form a reported pair, which
        // prunes most of the pair space before counting.
        let eligible = |id: u32| -> Option<u32> {
            let home = self.home(id);
            (self.exists[home as usize]
                && self.path_acc[home as usize].commits >= COUPLING_MIN_TOGETHER)
                .then_some(home)
        };
        let mut pairs: HashMap<u64, u32> = HashMap::new();
        let mut ids: Vec<u32> = Vec::new();
        for &(start, len) in &self.pair_ranges {
            ids.clear();
            ids.extend(
                self.pair_commits[start as usize..(start + len) as usize]
                    .iter()
                    .filter_map(|&id| eligible(id)),
            );
            ids.sort_unstable();
            ids.dedup();
            for (i, &a) in ids.iter().enumerate() {
                for &b in &ids[i + 1..] {
                    *pairs.entry(u64::from(a) << 32 | u64::from(b)).or_insert(0) += 1;
                }
            }
        }
        let mut out: Vec<Coupling> = pairs
            .into_iter()
            .filter(|&(_, together)| together >= COUPLING_MIN_TOGETHER)
            .filter_map(|(key, together)| {
                let (a, b) = ((key >> 32) as u32, key as u32);
                let a_commits = self.path_acc[a as usize].commits;
                let b_commits = self.path_acc[b as usize].commits;
                let confidence = f64::from(together) / f64::from(a_commits.min(b_commits).max(1));
                (confidence >= COUPLING_MIN_CONFIDENCE).then(|| {
                    let (pa, pb) = (&self.paths[a as usize], &self.paths[b as usize]);
                    Coupling {
                        a: pa.clone(),
                        b: pb.clone(),
                        together,
                        a_commits,
                        b_commits,
                        confidence,
                        cross_dir: dir_of(pa, 1) != dir_of(pb, 1),
                    }
                })
            })
            .collect();
        out.sort_by(|x, y| {
            y.together
                .cmp(&x.together)
                .then_with(|| y.confidence.total_cmp(&x.confidence))
                .then_with(|| x.a.cmp(&y.a))
                .then_with(|| x.b.cmp(&y.b))
        });
        out.truncate(COUPLING_LIMIT);
        out
    }

    fn age(&self, files: &[TrackedFile]) -> Vec<DirAge> {
        let mut by_dir: HashMap<String, Vec<i64>> = HashMap::new();
        for f in files {
            let Some(&id) = self.path_ids.get(f.path.as_str()) else {
                continue;
            };
            let pa = &self.path_acc[id as usize];
            if pa.commits == 0 {
                continue; // never seen in the (possibly truncated) log
            }
            by_dir.entry(dir_of(&f.path, 2)).or_default().push(pa.last);
        }
        let mut out: Vec<DirAge> = by_dir
            .into_iter()
            .map(|(dir, lasts)| {
                let mut ages: Vec<u64> = lasts
                    .iter()
                    .map(|&l| u64::from(days_between(l, self.now)))
                    .collect();
                let median_age_days = lower_median(&mut ages) as u32;
                DirAge {
                    dir,
                    files: lasts.len() as u32,
                    median_age_days,
                    oldest: lasts.iter().copied().min().unwrap_or(0),
                    newest: lasts.iter().copied().max().unwrap_or(0),
                    stale_files: ages.iter().filter(|&&a| a > STALE_DAYS).count() as u32,
                    active_files: ages.iter().filter(|&&a| a <= ACTIVE_DAYS).count() as u32,
                }
            })
            .collect();
        out.sort_by(|a, b| {
            b.median_age_days
                .cmp(&a.median_age_days)
                .then_with(|| a.dir.cmp(&b.dir))
        });
        out
    }

    fn ownership(&self, newest: i64) -> Vec<DirOwnership> {
        struct Share {
            author: u32,
            lines: u64,
            commits: u32,
            last: i64,
        }
        let mut by_dir: HashMap<u32, Vec<Share>> = HashMap::new();
        for (&(dir, author), acc) in &self.dir_author {
            by_dir.entry(dir).or_default().push(Share {
                author,
                lines: acc.lines,
                commits: acc.commits,
                last: acc.last,
            });
        }
        let mut files_per_dir: HashMap<u32, u32> = HashMap::new();
        for (id, pa) in self.path_acc.iter().enumerate() {
            if pa.commits > 0 {
                *files_per_dir.entry(self.path_dir[id]).or_insert(0) += 1;
            }
        }
        // Small repositories have no directory with 100 added lines; keep
        // everything then, so the tab is never empty while data exists.
        let big_dirs = by_dir
            .values()
            .filter(|shares| shares.iter().map(|s| s.lines).sum::<u64>() >= OWNERSHIP_MIN_LINES)
            .count();
        let min_lines = if big_dirs >= 10 {
            OWNERSHIP_MIN_LINES
        } else {
            1
        };
        let mut out: Vec<DirOwnership> = by_dir
            .into_iter()
            .filter_map(|(dir, mut shares)| {
                let total: u64 = shares.iter().map(|s| s.lines).sum();
                if total < min_lines {
                    return None;
                }
                shares.sort_by(|a, b| {
                    b.lines
                        .cmp(&a.lines)
                        .then_with(|| b.commits.cmp(&a.commits))
                        .then_with(|| {
                            self.authors[a.author as usize]
                                .email
                                .cmp(&self.authors[b.author as usize].email)
                        })
                });
                let fractions: Vec<f64> = shares
                    .iter()
                    .map(|s| s.lines as f64 / total as f64)
                    .collect();
                let bus = bus_factor(&fractions);
                let top = &self.authors[shares[0].author as usize];
                let orphaned = bus == 1 && top.last < newest - ORPHAN_SECS;
                let owners = shares
                    .iter()
                    .zip(&fractions)
                    .take(OWNERS_SHOWN)
                    .map(|(s, &share)| {
                        let a = &self.authors[s.author as usize];
                        Owner {
                            name: a.name.clone(),
                            email: a.email.clone(),
                            lines: s.lines,
                            commits: s.commits,
                            last: s.last,
                            share,
                        }
                    })
                    .collect();
                let d = &self.dir_acc[dir as usize];
                Some(DirOwnership {
                    dir: self.dirs[dir as usize].clone(),
                    files: files_per_dir.get(&dir).copied().unwrap_or(0),
                    lines: total,
                    commits: d.commits,
                    owners,
                    bus_factor: bus,
                    orphaned,
                    last_activity: d.last_activity,
                })
            })
            .collect();
        out.sort_by(|a, b| {
            b.orphaned
                .cmp(&a.orphaned)
                .then_with(|| a.bus_factor.cmp(&b.bus_factor))
                .then_with(|| b.lines.cmp(&a.lines))
                .then_with(|| a.dir.cmp(&b.dir))
        });
        out
    }

    fn quality_report(&mut self) -> CommitQuality {
        let mut q = std::mem::take(&mut self.quality);
        let revert_ids: HashSet<Oid> = self.reverts.iter().map(|(n, _)| n.id).collect();
        q.reverts = self
            .reverts
            .drain(..)
            .map(|(commit, reverts)| RevertNote {
                double: revert_ids.contains(&reverts),
                commit,
                reverts,
            })
            .collect();
        q.conventional_types = self.conv_types.drain().collect();
        q.conventional_types
            .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        q.giant
            .sort_by(|a, b| b.lines.cmp(&a.lines).then_with(|| b.files.cmp(&a.files)));
        q.giant.truncate(GIANT_LIMIT);
        q.median_files = lower_median(&mut self.files_per_commit) as u32;
        q.median_lines = lower_median(&mut self.lines_per_commit).min(u64::from(u32::MAX)) as u32;
        q.bots = self.bots.drain().collect();
        q.bots
            .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        q.anomaly_counts = ANOMALY_KINDS
            .iter()
            .zip(self.anomaly_counts)
            .map(|(&k, n)| (k, n))
            .collect();
        q.merges_by_year = self
            .years
            .iter()
            .map(|(&y, a)| (y, a.merges, a.total))
            .collect();
        q.conventional_by_year = self
            .years
            .iter()
            .map(|(&y, a)| (y, a.conventional, a.total))
            .collect();
        q
    }

    fn timeline(&self, files: &[TrackedFile], newest: i64) -> Timeline {
        let mut t = Timeline::default();
        if self.commits > 0 {
            let (fy, fm) = year_month(self.first_time);
            let (ly, lm) = year_month(self.last_time);
            let last_idx = month_index(ly, lm);
            let first_idx = month_index(fy, fm).max(last_idx - TIMELINE_MAX_MONTHS + 1);
            let mut new_authors: HashMap<i32, u32> = HashMap::new();
            let mut departed: HashMap<i32, u32> = HashMap::new();
            for a in &self.authors {
                let (y, m) = year_month(a.first);
                *new_authors.entry(month_index(y, m)).or_insert(0) += 1;
                if a.last < newest - DEPARTED_SECS {
                    let (y, m) = year_month(a.last);
                    *departed.entry(month_index(y, m)).or_insert(0) += 1;
                }
            }
            t.months = (first_idx..=last_idx)
                .map(|idx| {
                    let mut month = Month {
                        ym: ym_label(idx),
                        new_authors: new_authors.get(&idx).copied().unwrap_or(0),
                        departed: departed.get(&idx).copied().unwrap_or(0),
                        ..Month::default()
                    };
                    if let Some(m) = self.months.get(&idx) {
                        month.commits = m.commits;
                        month.merges = m.merges;
                        month.authors = m.authors.len() as u32;
                        month.added = m.added;
                        month.deleted = m.deleted;
                        month.files = m.files.len() as u32;
                    }
                    month
                })
                .collect();
            t.authors = self
                .authors
                .iter()
                .map(|a| AuthorSpan {
                    name: a.name.clone(),
                    email: a.email.clone(),
                    first: a.first,
                    last: a.last,
                    commits: a.commits,
                    active_months: a.months.len() as u32,
                    added: a.added,
                    deleted: a.deleted,
                })
                .collect();
            t.authors.sort_by(|a, b| {
                b.commits
                    .cmp(&a.commits)
                    .then_with(|| b.last.cmp(&a.last))
                    .then_with(|| a.email.cmp(&b.email))
            });
            t.active_authors = t
                .authors
                .iter()
                .filter(|a| a.last >= newest - DEPARTED_SECS)
                .count() as u32;
            t.first_commit = self.first_time;
            t.last_commit = self.last_time;
            t.age_days = days_between(self.first_time, self.last_time);
        }

        // Languages from the tracked files.
        let mut by_lang: HashMap<String, (usize, u64)> = HashMap::new();
        for f in files {
            if f.is_gitlink() {
                continue;
            }
            let e = by_lang.entry(language_of(&f.path)).or_insert((0, 0));
            e.0 += 1;
            e.1 += f.size.unwrap_or(0);
        }
        t.total_files = files.len();
        t.total_bytes = by_lang.values().map(|&(_, b)| b).sum();
        t.languages = by_lang
            .into_iter()
            .map(|(name, (files, bytes))| Language {
                name,
                files,
                bytes,
                share: if t.total_bytes > 0 {
                    bytes as f64 / t.total_bytes as f64
                } else {
                    0.0
                },
            })
            .collect();
        t.languages.sort_by(|a, b| {
            b.bytes
                .cmp(&a.bytes)
                .then_with(|| b.files.cmp(&a.files))
                .then_with(|| a.name.cmp(&b.name))
        });

        t.heat = self.heat;
        let (mut best_day, mut best_hour) = (0u32, 0u32);
        let mut hours = [0u32; 24];
        for (wd, row) in self.heat.iter().enumerate() {
            let total: u32 = row.iter().sum();
            if total > best_day {
                best_day = total;
                t.peak_weekday = wd as u8;
            }
            for (h, n) in hours.iter_mut().zip(row) {
                *h += n;
            }
        }
        for (hour, &n) in hours.iter().enumerate() {
            if n > best_hour {
                best_hour = n;
                t.peak_hour = hour as u8;
            }
        }
        t
    }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Stream the log once (`super::stream_log` with `numstat: true`) and
/// derive everything above. Reports progress as commits read (`total` from a
/// `rev-list --count` first, when `max_commits` is 0 use the count, else the
/// smaller of the two). Returns `(history, commits_read, log_bytes)`.
///
/// Signature checking (`Signatures`) is a second, shorter pass over the
/// newest 300 commits with `signatures: true` because `%G?` runs gpg per
/// signed commit.
pub fn analyze(
    workdir: &Path,
    opts: &InsightsOptions,
    files: &[TrackedFile],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
    commands: &mut Vec<String>,
) -> Result<(History, usize, u64)> {
    let now = now_epoch();
    let count_args: &[&str] = if opts.all_refs {
        &["rev-list", "--count", "--branches", "--remotes", "--tags"]
    } else {
        &["rev-list", "--count", "HEAD"]
    };
    let total = git_ok(workdir, count_args, commands)
        .trim()
        .parse::<usize>()
        .ok()
        .map(|n| {
            if opts.max_commits > 0 {
                n.min(opts.max_commits)
            } else {
                n
            }
        });
    let mut prog = Progress {
        phase: "Reading history (numstat)".to_owned(),
        total,
        ..Progress::default()
    };
    progress(&prog);

    let mut acc = Acc::new(files, now);
    let req = LogRequest {
        max_commits: opts.max_commits,
        all_refs: opts.all_refs,
        numstat: true,
        ..LogRequest::default()
    };
    let (commits, bytes) = stream_log(workdir, &req, cancel, commands, &mut |c| {
        acc.commit(c);
        if acc.commits.is_multiple_of(PROGRESS_EVERY) {
            prog.done = acc.commits;
            progress(&prog);
        }
        true
    })?;
    prog.done = commits;
    progress(&prog);

    let mut history = acc.finish(files);
    if commits > 0 && !cancel.load(Ordering::Relaxed) {
        history.quality.signatures = signatures(workdir, cancel, commands);
    }
    Ok((history, commits, bytes))
}

/// `%G?` over the newest commits; `None` when the log could not be read.
fn signatures(
    workdir: &Path,
    cancel: &AtomicBool,
    commands: &mut Vec<String>,
) -> Option<Signatures> {
    let req = LogRequest {
        max_commits: SIGNATURE_COMMITS,
        signatures: true,
        ..LogRequest::default()
    };
    let mut sig = Signatures::default();
    let mut by_author: HashMap<String, (u32, u32)> = HashMap::new();
    stream_log(workdir, &req, cancel, commands, &mut |c| {
        sig.checked += 1;
        let status = c.signature.unwrap_or('N');
        match status {
            'G' => sig.good += 1,
            'B' => sig.bad += 1,
            'U' => sig.unknown += 1,
            'X' | 'Y' => sig.expired += 1,
            'R' => sig.revoked += 1,
            'E' => sig.error += 1,
            _ => sig.none += 1,
        }
        let e = by_author.entry(c.author).or_insert((0, 0));
        e.1 += 1;
        e.0 += u32::from(matches!(status, 'G' | 'U'));
        true
    })
    .ok()?;
    if sig.checked == 0 {
        return None;
    }
    sig.by_author = by_author
        .into_iter()
        .map(|(author, (signed, total))| (author, signed, total))
        .collect();
    sig.by_author.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.0.cmp(&b.0))
    });
    sig.by_author.truncate(SIGNATURE_AUTHORS);
    Some(sig)
}

// ---------------------------------------------------------------------------
// Blame of one file (on demand)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileBlame {
    pub path: String,
    pub lines: u32,
    /// Surviving lines per author, desc.
    pub owners: Vec<Owner>,
    /// Distinct commits still represented.
    pub commits: u32,
    pub oldest: i64,
    pub newest: i64,
    /// Lines per year of last change, ascending: `(year, lines)`.
    pub by_year: Vec<(i32, u32)>,
    pub bus_factor: u32,
    pub elapsed_ms: u64,
    pub command: String,
}

/// `git blame --line-porcelain HEAD -- path` summarised. Errors for
/// binary or untracked files.
pub fn blame(workdir: &Path, path: &str) -> Result<FileBlame> {
    let started = Instant::now();
    let args = ["blame", "--line-porcelain", "HEAD", "--", path];
    let out = cli::run(workdir, &args, None)?;
    let mut summary = parse_blame(&out.stdout)?;
    summary.path = path.to_owned();
    summary.command = format!("git {}", args.join(" "));
    summary.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(summary)
}

/// Parse `--line-porcelain` (or `--porcelain`, where a commit's header is
/// only printed in full the first time) output.
fn parse_blame(bytes: &[u8]) -> Result<FileBlame> {
    #[derive(Default)]
    struct Commit {
        name: String,
        mail: String,
        time: i64,
        author: Option<u32>,
    }
    struct Author {
        name: String,
        email: String,
        lines: u32,
        last: i64,
        commits: HashSet<u32>,
    }
    let mut commit_ids: HashMap<String, u32> = HashMap::new();
    let mut commits: Vec<Commit> = Vec::new();
    let mut author_ids: HashMap<String, u32> = HashMap::new();
    let mut authors: Vec<Author> = Vec::new();
    let mut by_year: BTreeMap<i32, u32> = BTreeMap::new();
    let mut current: Option<u32> = None;
    let (mut total, mut oldest, mut newest) = (0u32, i64::MAX, i64::MIN);

    for line in bytes.split(|&b| b == b'\n') {
        if let Some(content) = line.strip_prefix(b"\t") {
            let Some(ci) = current.take() else {
                continue;
            };
            if content.contains(&0) {
                return Err(Error::new("binary file"));
            }
            let commit = &mut commits[ci as usize];
            let aid = match commit.author {
                Some(a) => a,
                None => {
                    let key = if commit.mail.is_empty() {
                        commit.name.to_lowercase()
                    } else {
                        commit.mail.to_lowercase()
                    };
                    let a = *author_ids.entry(key.clone()).or_insert_with(|| {
                        authors.push(Author {
                            name: commit.name.clone(),
                            email: key,
                            lines: 0,
                            last: i64::MIN,
                            commits: HashSet::new(),
                        });
                        authors.len() as u32 - 1
                    });
                    commit.author = Some(a);
                    a
                }
            };
            let author = &mut authors[aid as usize];
            author.lines += 1;
            if commit.time > author.last {
                author.last = commit.time;
                author.name = commit.name.clone();
            }
            author.commits.insert(ci);
            *by_year
                .entry(civil_from_days(commit.time.div_euclid(86_400)).0)
                .or_insert(0) += 1;
            oldest = oldest.min(commit.time);
            newest = newest.max(commit.time);
            total += 1;
            continue;
        }
        let text = String::from_utf8_lossy(line);
        let text = text.trim_end_matches('\r');
        match current {
            None => {
                // `<sha> <orig line> <final line> [lines in group]`
                let Some(sha) = text.split(' ').next() else {
                    continue;
                };
                if sha.len() < 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                    continue;
                }
                let ci = *commit_ids.entry(sha.to_owned()).or_insert_with(|| {
                    commits.push(Commit::default());
                    commits.len() as u32 - 1
                });
                current = Some(ci);
            }
            Some(ci) => {
                let commit = &mut commits[ci as usize];
                if let Some(v) = text.strip_prefix("author ") {
                    commit.name = v.trim().to_owned();
                } else if let Some(v) = text.strip_prefix("author-mail ") {
                    commit.mail = v
                        .trim()
                        .trim_start_matches('<')
                        .trim_end_matches('>')
                        .to_owned();
                } else if let Some(v) = text.strip_prefix("author-time ") {
                    commit.time = v.trim().parse().unwrap_or(0);
                }
            }
        }
    }

    let mut owners: Vec<Owner> = authors
        .iter()
        .map(|a| Owner {
            name: a.name.clone(),
            email: a.email.clone(),
            lines: u64::from(a.lines),
            commits: a.commits.len() as u32,
            last: a.last,
            share: if total > 0 {
                f64::from(a.lines) / f64::from(total)
            } else {
                0.0
            },
        })
        .collect();
    owners.sort_by(|a, b| {
        b.lines
            .cmp(&a.lines)
            .then_with(|| b.last.cmp(&a.last))
            .then_with(|| a.email.cmp(&b.email))
    });
    let shares: Vec<f64> = owners.iter().map(|o| o.share).collect();
    Ok(FileBlame {
        path: String::new(),
        lines: total,
        owners,
        commits: commits.len() as u32,
        oldest: if total > 0 { oldest } else { 0 },
        newest: if total > 0 { newest } else { 0 },
        by_year: by_year.into_iter().collect(),
        bus_factor: bus_factor(&shares),
        elapsed_ms: 0,
        command: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(year_month(EPOCH_2000), (2000, 1));
        assert_eq!(civil_from_days(951_782_400 / 86_400), (2000, 2, 29));
        assert_eq!(civil_from_days(1_735_603_200 / 86_400), (2024, 12, 31));
        assert_eq!(ymd_label(1_700_000_000), "2023-11-14");
        assert_eq!(ym_label(month_index(2026, 10)), "2026-10");
        assert_eq!(month_index(2026, 1) - month_index(2025, 12), 1);
        assert_eq!(ym_label(month_index(2025, 12) + 1), "2026-01");
        // 1970-01-01 was a Thursday.
        assert_eq!(local_slot(0, 0), (3, 0));
        assert_eq!(local_slot(0, 120), (3, 2));
        assert_eq!(local_slot(0, -60), (2, 23));
        assert_eq!(fmt_offset(120), "+02:00");
        assert_eq!(fmt_offset(-330), "-05:30");
    }

    #[test]
    fn message_classes() {
        assert!(is_wip("WIP"));
        assert!(is_wip("wip: parser"));
        assert!(is_wip("..."));
        assert!(is_wip("ok"));
        assert!(is_wip("Fix"));
        assert!(!is_wip("Fix crash on start"));
        assert!(!is_wip("Add parser"));
        assert!(is_fixup("fixup! Add parser"));
        assert!(is_fixup("squash! x"));
        assert!(!is_fixup("Fixup the parser"));
        let p = patterns();
        assert_eq!(
            &p.conventional.captures("feat(ui)!: thing").unwrap()[1],
            "feat"
        );
        assert_eq!(&p.conventional.captures("fix: thing").unwrap()[1], "fix");
        assert!(p.conventional.captures("Feature: thing").is_none());
        assert!(p.conventional.captures("fix thing").is_none());
        assert!(p.issue.is_match("Fixes #12"));
        assert!(p.issue.is_match("ABC-123 done"));
        assert!(p.issue.is_match("closes the loop"));
        assert!(!p.issue.is_match("nothing here"));
        assert!(p.fix.is_match("Fix crash on start"));
        assert!(p.fix.is_match("Fixed the regression"));
        assert!(!p.fix.is_match("Prefix the names"));
        assert!(p.bot.is_match("dependabot[bot]"));
        assert!(p.bot.is_match("ci-bot@example.com"));
        assert!(!p.bot.is_match("robot-overlord@example.com"));
        let cap = p
            .revert
            .captures(
                "Revert \"x\"\n\nThis reverts commit 0123456789abcdef0123456789abcdef01234567.",
            )
            .unwrap();
        assert_eq!(cap[1].len(), 40);
    }

    #[test]
    fn bus_factor_median_language() {
        assert_eq!(bus_factor(&[0.6, 0.4]), 1);
        assert_eq!(bus_factor(&[0.4, 0.3, 0.3]), 2);
        assert_eq!(bus_factor(&[0.25, 0.25, 0.25, 0.25]), 2);
        assert_eq!(bus_factor(&[0.2; 5]), 3);
        assert_eq!(bus_factor(&[]), 0);
        assert_eq!(lower_median(&mut [5, 1, 3]), 3);
        assert_eq!(lower_median(&mut [4, 1, 3, 2]), 2);
        assert_eq!(lower_median(&mut []), 0);
        assert_eq!(language_of("src/a.rs"), "Rust");
        assert_eq!(language_of("Makefile"), "Make");
        assert_eq!(language_of("docker/Dockerfile"), "Dockerfile");
        assert_eq!(language_of("CMakeLists.txt"), "CMake");
        assert_eq!(language_of("web/App.TSX"), "TypeScript");
        assert_eq!(language_of("BUILD.gn"), "GN");
        assert_eq!(language_of("x.verylongext"), "Other");
        assert_eq!(language_of("LICENSE"), "Other");
        assert_eq!(human_span(90), "1 min");
        assert_eq!(human_span(7_200), "2 h");
        assert_eq!(human_span(3 * 86_400), "3 days");
    }

    fn commit(
        n: u8,
        time: i64,
        author: (&str, &str),
        summary: &str,
        files: &[(&str, Option<&str>, u32, u32)],
        merge: bool,
    ) -> LogCommit {
        let hex = format!("{n:02x}").repeat(20);
        let parents = if merge { 2 } else { 1 };
        LogCommit {
            id: Oid::from_str(&hex).unwrap(),
            parents: vec![Oid::ZERO_SHA1; parents],
            author: author.0.to_owned(),
            email: author.1.to_owned(),
            time,
            committer: author.0.to_owned(),
            committer_email: author.1.to_owned(),
            commit_time: time,
            offset_minutes: 60,
            summary: summary.to_owned(),
            body: String::new(),
            files: files
                .iter()
                .map(|&(p, old, a, d)| FileChange {
                    path: p.to_owned(),
                    old_path: old.map(str::to_owned),
                    added: Some(a),
                    deleted: Some(d),
                })
                .collect(),
            signature: None,
        }
    }

    fn tracked(path: &str, size: u64) -> TrackedFile {
        TrackedFile {
            path: path.to_owned(),
            mode: 0o100644,
            oid: Oid::ZERO_SHA1,
            size: Some(size),
        }
    }

    #[test]
    fn accumulator_follows_renames_and_pairs() {
        let files = [tracked("src/b.rs", 4_000), tracked("src/t.rs", 1_000)];
        let now = 1_800_000_000; // 2027-01-15
        let ann = ("Ann", "ann@x");
        let bob = ("Bob", "bob@x");
        let mut acc = Acc::new(&files, now);
        // Newest first, as the log streams.
        acc.commit(commit(
            3,
            1_790_000_000,
            ann,
            "Rename module",
            &[
                ("src/b.rs", Some("src/a.rs"), 0, 0),
                ("src/t.rs", None, 10, 0),
            ],
            false,
        ));
        acc.commit(commit(
            2,
            1_780_000_000,
            bob,
            "Tweak",
            &[("src/a.rs", None, 20, 5), ("src/t.rs", None, 5, 0)],
            false,
        ));
        acc.commit(commit(
            1,
            1_770_000_000,
            ann,
            "Fix crash in parser",
            &[("src/a.rs", None, 1_000, 0), ("src/t.rs", None, 50, 0)],
            false,
        ));
        acc.commit(commit(0, 1_760_000_000, ann, "Merge branch x", &[], true));
        let h = acc.finish(&files);

        assert_eq!(h.commits, 4);
        assert_eq!(h.paths, 3, "a, b, t");
        assert_eq!(h.quality.total, 4);
        assert_eq!(h.quality.merges, 1);
        assert_eq!(h.quality.wip.len(), 0);

        let b = h
            .hotspots
            .iter()
            .find(|s| s.path == "src/b.rs")
            .expect("b.rs hotspot");
        assert_eq!(b.commits, 3, "a.rs history carried over");
        assert_eq!(b.authors, 2);
        assert_eq!(b.fixes, 1);
        assert_eq!(b.added, 1_020);
        assert_eq!(b.deleted, 5);
        assert_eq!(b.size, Some(4_000));
        assert!(h.hotspots.iter().all(|s| s.path != "src/a.rs"));
        assert_eq!(h.hotspots[0].path, "src/b.rs", "more lines than t.rs");
        // The newest commit weighs 1; older ones less (the rename itself
        // changed no lines and adds ln(1) = 0).
        let expected = (1.0f64).ln()
            + (26.0f64).ln() * 0.5f64.powf(10_000_000.0 / 86_400.0 / 365.0)
            + (1_001.0f64).ln() * 0.5f64.powf(20_000_000.0 / 86_400.0 / 365.0);
        assert!(
            (b.score - expected).abs() < 1e-9,
            "{} vs {expected}",
            b.score
        );

        assert_eq!(h.couplings.len(), 1);
        let c = &h.couplings[0];
        assert_eq!((c.a.as_str(), c.b.as_str()), ("src/b.rs", "src/t.rs"));
        assert_eq!(c.together, 3);
        assert_eq!((c.a_commits, c.b_commits), (3, 3));
        assert!((c.confidence - 1.0).abs() < 1e-12);
        assert!(!c.cross_dir);

        assert_eq!(h.ownership.len(), 1);
        let o = &h.ownership[0];
        assert_eq!(o.dir, "src");
        assert_eq!(o.files, 2);
        assert_eq!(o.lines, 1_085);
        assert_eq!(o.commits, 3);
        assert_eq!(o.bus_factor, 1);
        assert!(!o.orphaned);
        assert_eq!(o.owners[0].email, "ann@x");
        assert_eq!(o.owners[0].lines, 1_060);
        assert_eq!(o.owners[1].commits, 1);

        assert_eq!(h.age.len(), 1);
        assert_eq!(h.age[0].files, 2);
        assert_eq!(h.age[0].active_files, 0);
        assert_eq!(h.age[0].stale_files, 0);

        let t = &h.timeline;
        assert_eq!(t.months.first().unwrap().ym, "2025-10");
        assert_eq!(t.months.last().unwrap().ym, "2026-09");
        assert_eq!(t.months.len(), 12);
        assert_eq!(t.months.iter().map(|m| m.commits).sum::<u32>(), 4);
        assert_eq!(t.months.iter().map(|m| m.new_authors).sum::<u32>(), 2);
        assert_eq!(t.authors.len(), 2);
        assert_eq!(t.authors[0].email, "ann@x");
        assert_eq!(t.authors[0].commits, 3);
        assert_eq!(t.active_authors, 1, "Bob's last commit is 115 days old");
        assert_eq!(t.languages.len(), 1);
        assert_eq!(t.languages[0].name, "Rust");
        assert_eq!(t.languages[0].files, 2);
        assert!((t.languages[0].share - 1.0).abs() < 1e-12);
        assert_eq!(t.heat.iter().flatten().sum::<u32>(), 4);
        assert_eq!(t.age_days, 347);
    }

    #[test]
    fn date_anomalies_and_bots() {
        let files: [TrackedFile; 0] = [];
        let now = 1_800_000_000;
        let mut acc = Acc::new(&files, now);
        let mut c = commit(1, now + 5 * 86_400, ("Ann", "ann@x"), "future", &[], false);
        c.commit_time = c.time;
        acc.commit(c);
        let mut c = commit(2, 100, ("dependabot[bot]", "x@y"), "epoch", &[], false);
        c.commit_time = c.time + 3 * 86_400; // rebased too
        acc.commit(c);
        let mut c = commit(3, now - 100, ("Ann", "ann@x"), "backdated", &[], false);
        c.commit_time = c.time - 3_600;
        c.committer_email = "other@x".into();
        acc.commit(c);
        let h = acc.finish(&files);
        let q = &h.quality;
        assert_eq!(q.bot_commits, 1);
        assert_eq!(q.bots, vec![("dependabot[bot]".to_owned(), 1)]);
        assert_eq!(q.author_committer_differ, 1);
        let kinds: Vec<&str> = q.date_anomalies.iter().map(|a| a.kind).collect();
        assert_eq!(kinds, ["future", "epoch", "rebased", "backdated"]);
        assert_eq!(
            q.anomaly_counts,
            vec![
                ("future", 1),
                ("epoch", 1),
                ("rebased", 1),
                ("backdated", 1),
                ("offset-jump", 0)
            ]
        );
        assert_eq!(q.date_anomalies[1].detail, "authored 1970-01-01");
        assert_eq!(q.one_word, 3);
        assert_eq!(q.no_body, 3);
    }

    #[test]
    fn blame_porcelain() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let text = format!(
            "{a} 1 1 2\nauthor Ada\nauthor-mail <ada@example.com>\nauthor-time 1700000000\nauthor-tz +0100\n\
             committer Ada\ncommitter-mail <ada@example.com>\ncommitter-time 1700000000\ncommitter-tz +0100\n\
             summary first\nfilename f.rs\n\tline one\n\
             {a} 2 2\n\tline two\n\
             {b} 1 3 1\nauthor Bob\nauthor-mail <bob@example.com>\nauthor-time 1600000000\nauthor-tz +0000\n\
             committer Bob\ncommitter-mail <bob@example.com>\ncommitter-time 1600000000\ncommitter-tz +0000\n\
             summary second\nprevious {a} f.rs\nfilename f.rs\n\t\n"
        );
        let fb = parse_blame(text.as_bytes()).expect("parses");
        assert_eq!(fb.lines, 3);
        assert_eq!(fb.commits, 2);
        assert_eq!(fb.owners.len(), 2);
        assert_eq!(fb.owners[0].email, "ada@example.com");
        assert_eq!(fb.owners[0].name, "Ada");
        assert_eq!(fb.owners[0].lines, 2);
        assert_eq!(fb.owners[0].commits, 1);
        assert_eq!(fb.owners[0].last, 1_700_000_000);
        assert!((fb.owners[0].share - 2.0 / 3.0).abs() < 1e-12);
        assert_eq!(fb.owners[1].lines, 1);
        assert_eq!(fb.oldest, 1_600_000_000);
        assert_eq!(fb.newest, 1_700_000_000);
        assert_eq!(fb.by_year, vec![(2020, 1), (2023, 2)]);
        assert_eq!(fb.bus_factor, 1);

        let empty = parse_blame(b"").expect("empty file");
        assert_eq!(empty.lines, 0);
        assert_eq!(empty.bus_factor, 0);

        let binary = format!("{a} 1 1 1\nauthor X\nauthor-mail <x@y>\nauthor-time 1\n\t\u{0}PNG\n");
        assert!(parse_blame(binary.as_bytes()).is_err());
    }
}
