//! Repository insights: the useful things git knows but no client shows.
//!
//! * **Health** ([`health`]): checkout landmines (case collisions, Windows
//!   reserved names, long paths, symlinks), line endings and attributes,
//!   executable bits, repository weight, submodules.
//! * **History analytics** ([`history`]): hotspots and temporal coupling,
//!   code age, ownership and bus factor, commit hygiene and provenance,
//!   contributor timeline, language breakdown; on demand a blame summary
//!   of one file.
//! * **Refs** ([`refs`]): branch intelligence (stale, merged, gone,
//!   unmerged, conflict prediction, duplicate patches) and the release view
//!   (tags, unreleased work, cadence, version bumps).
//! * **Debt** ([`debt`]): TODO/FIXME/HACK markers, dated through `git blame`
//!   on demand.
//! * **CI** ([`ci`]): GitHub Actions workflows — unpinned actions,
//!   dangerous triggers, missing permissions, secrets usage, runners.
//! * **Licences** ([`license`]): the project licence, dependency licences
//!   found offline, copyright headers from other projects.
//! * **Search** ([`search`]): pickaxe (`git log -S/-G`) across history.
//! * **Recovery** ([`recover`]): commits and stashes no ref reaches, with
//!   enough detail to restore them.
//!
//! Everything reads through the git binary and is read-only; the report
//! records every command it ran so the activity log can show them.

pub mod ci;
pub mod debt;
pub mod health;
pub mod history;
pub mod license;
pub mod recover;
pub mod refs;
pub mod search;

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub use crate::secrets::{Progress, Severity};
use crate::{Oid, Result, cli};

// ---------------------------------------------------------------------------
// Options, report
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InsightsOptions {
    /// Newest commits the history pass reads with `--numstat` (0 = all).
    /// Line counts need the blob contents, so this is the expensive part.
    pub max_commits: usize,
    /// Walk all branches and tags instead of HEAD only.
    pub all_refs: bool,
    /// Days without a commit after which a branch counts as stale.
    pub stale_days: u32,
}

impl Default for InsightsOptions {
    fn default() -> Self {
        Self {
            max_commits: 10_000,
            all_refs: false,
            stale_days: 90,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct InsightsStats {
    /// Commits read by the history pass.
    pub commits: usize,
    /// Tracked files.
    pub files: usize,
    /// Bytes of `git log --numstat` output parsed.
    pub log_bytes: u64,
    pub elapsed_ms: u64,
    /// `(phase, milliseconds)` in run order.
    pub phases: Vec<(String, u64)>,
    pub cancelled: bool,
}

#[derive(Clone, Debug, Default)]
pub struct InsightsReport {
    pub health: health::Health,
    pub history: history::History,
    pub refs: refs::Refs,
    pub debt: debt::Debt,
    pub ci: ci::Ci,
    pub license: license::Licensing,
    pub stats: InsightsStats,
    pub commands: Vec<String>,
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// A tracked file as the index knows it (`git ls-files -s`), with the HEAD
/// blob size when the file is in HEAD.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackedFile {
    pub path: String,
    /// Index mode: `0o100644`, `0o100755`, `0o120000` (symlink),
    /// `0o160000` (submodule / gitlink).
    pub mode: u32,
    pub oid: Oid,
    pub size: Option<u64>,
}

impl TrackedFile {
    pub fn is_symlink(&self) -> bool {
        self.mode == 0o120000
    }

    pub fn is_gitlink(&self) -> bool {
        self.mode == 0o160000
    }

    pub fn is_executable(&self) -> bool {
        self.mode == 0o100755
    }
}

/// Run `git args…`, record the command, return stdout (lossy UTF-8).
pub(crate) fn git(workdir: &Path, args: &[&str], commands: &mut Vec<String>) -> Result<String> {
    commands.push(format!("git {}", args.join(" ")));
    let out = cli::run(workdir, args, None)?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Like [`git`], but a failure yields an empty string (for optional data).
pub(crate) fn git_ok(workdir: &Path, args: &[&str], commands: &mut Vec<String>) -> String {
    git(workdir, args, commands).unwrap_or_default()
}

/// Run `git args…` with `stdin`, record the command with a note, return stdout bytes.
pub(crate) fn git_stdin(
    workdir: &Path,
    args: &[&str],
    stdin: &[u8],
    note: &str,
    commands: &mut Vec<String>,
) -> Result<Vec<u8>> {
    commands.push(format!("git {}   # {note}", args.join(" ")));
    Ok(cli::run(workdir, args, Some(stdin))?.stdout)
}

/// NUL-separated output of `git args…` as owned strings.
pub(crate) fn git_z(
    workdir: &Path,
    args: &[&str],
    commands: &mut Vec<String>,
) -> Result<Vec<String>> {
    crate::secrets::git_lines(workdir, args, commands)
}

/// Tracked files with index modes and HEAD blob sizes.
pub fn tracked_files(workdir: &Path, commands: &mut Vec<String>) -> Result<Vec<TrackedFile>> {
    let mut files = Vec::new();
    for entry in git_z(workdir, &["ls-files", "-s", "-z"], commands)? {
        // `100644 <oid> 0\t<path>`
        let Some((meta, path)) = entry.split_once('\t') else {
            continue;
        };
        let mut parts = meta.split_whitespace();
        let mode = parts
            .next()
            .and_then(|m| u32::from_str_radix(m, 8).ok())
            .unwrap_or(0o100644);
        let oid = parts
            .next()
            .and_then(|o| Oid::from_str(o).ok())
            .unwrap_or(Oid::ZERO_SHA1);
        let stage = parts.next().unwrap_or("0");
        if stage != "0" {
            // Conflicted entries appear once per stage; keep one.
            if files.last().is_some_and(|f: &TrackedFile| f.path == path) {
                continue;
            }
        }
        files.push(TrackedFile {
            path: path.to_owned(),
            mode,
            oid,
            size: None,
        });
    }
    // Sizes from HEAD (`ls-tree -l` prints `-` for gitlinks).
    let mut sizes = std::collections::HashMap::new();
    for entry in
        git_z(workdir, &["ls-tree", "-r", "-l", "-z", "HEAD"], commands).unwrap_or_default()
    {
        let Some((meta, path)) = entry.split_once('\t') else {
            continue;
        };
        if let Some(size) = meta
            .split_whitespace()
            .nth(3)
            .and_then(|s| s.parse::<u64>().ok())
        {
            sizes.insert(path.to_owned(), size);
        }
    }
    for f in &mut files {
        f.size = sizes.get(&f.path).copied();
    }
    Ok(files)
}

/// Directory of `path` cut to `depth` components (`src/a/b.rs`, 2 → `src/a`);
/// files at the root map to `.`.
pub fn dir_of(path: &str, depth: usize) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 1 {
        return ".".into();
    }
    let n = (parts.len() - 1).min(depth);
    parts[..n].join("/")
}

/// Lower-case extension of a path without the dot (`""` when none).
pub fn ext_of(path: &str) -> String {
    let base = crate::secrets::basename(path);
    match base.rfind('.') {
        Some(i) if i > 0 => base[i + 1..].to_lowercase(),
        _ => String::new(),
    }
}

/// Whole days between two epoch timestamps (never negative).
pub fn days_between(from: i64, to: i64) -> u32 {
    ((to - from).max(0) / 86_400) as u32
}

// ---------------------------------------------------------------------------
// Streamed `git log --numstat`
// ---------------------------------------------------------------------------

/// One file of a commit from `--numstat`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    /// Previous path for renames (`-M` output).
    pub old_path: Option<String>,
    /// Lines added/deleted; `None` for binary files.
    pub added: Option<u32>,
    pub deleted: Option<u32>,
}

impl FileChange {
    pub fn is_binary(&self) -> bool {
        self.added.is_none()
    }

    pub fn lines(&self) -> u64 {
        self.added.unwrap_or(0) as u64 + self.deleted.unwrap_or(0) as u64
    }
}

/// A commit from the streamed log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogCommit {
    pub id: Oid,
    pub parents: Vec<Oid>,
    pub author: String,
    pub email: String,
    /// Author time, epoch seconds.
    pub time: i64,
    pub committer: String,
    pub committer_email: String,
    pub commit_time: i64,
    /// Author's UTC offset in minutes (from `%ai`).
    pub offset_minutes: i32,
    pub summary: String,
    /// Message body without the summary (trimmed).
    pub body: String,
    pub files: Vec<FileChange>,
    /// Signature status from `%G?` when asked for (`G`, `B`, `U`, `X`, `Y`,
    /// `R`, `E`, `N`), else `None`.
    pub signature: Option<char>,
}

impl Default for LogCommit {
    fn default() -> Self {
        Self {
            id: Oid::ZERO_SHA1,
            parents: Vec::new(),
            author: String::new(),
            email: String::new(),
            time: 0,
            committer: String::new(),
            committer_email: String::new(),
            commit_time: 0,
            offset_minutes: 0,
            summary: String::new(),
            body: String::new(),
            files: Vec::new(),
            signature: None,
        }
    }
}

impl LogCommit {
    pub fn is_merge(&self) -> bool {
        self.parents.len() > 1
    }

    pub fn added(&self) -> u64 {
        self.files.iter().map(|f| f.added.unwrap_or(0) as u64).sum()
    }

    pub fn deleted(&self) -> u64 {
        self.files
            .iter()
            .map(|f| f.deleted.unwrap_or(0) as u64)
            .sum()
    }
}

/// Options of [`stream_log`].
#[derive(Clone, Debug, Default)]
pub struct LogRequest {
    /// Newest commits to read (0 = all).
    pub max_commits: usize,
    /// `--branches --remotes --tags` instead of HEAD.
    pub all_refs: bool,
    /// Add `--numstat -M` (per-file line counts; reads blob contents).
    pub numstat: bool,
    /// Add `%G?` (runs gpg/ssh-keygen per signed commit: slow).
    pub signatures: bool,
    /// Extra revision arguments appended verbatim (e.g. `["main..feature"]`).
    pub revisions: Vec<String>,
    /// `--first-parent`.
    pub first_parent: bool,
}

/// Stream `git log` and hand every commit to `sink`; the sink returns
/// `false` to stop early. Returns `(commits, bytes)` read. Cancellation is
/// checked between commits.
pub fn stream_log(
    workdir: &Path,
    req: &LogRequest,
    cancel: &AtomicBool,
    commands: &mut Vec<String>,
    sink: &mut dyn FnMut(LogCommit) -> bool,
) -> Result<(usize, u64)> {
    let mut format =
        "--format=%x1e%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%cn%x1f%ce%x1f%ct%x1f%ai%x1f%s%x1f%b%x1f"
            .to_owned();
    if req.signatures {
        format.push_str("%G?%x1f");
    }
    let mut args: Vec<String> = vec!["log".into(), format];
    if req.numstat {
        args.push("--numstat".into());
        args.push("-M".into());
    }
    if req.first_parent {
        args.push("--first-parent".into());
    }
    if req.max_commits > 0 {
        args.push("-n".into());
        args.push(req.max_commits.to_string());
    }
    if req.revisions.is_empty() {
        if req.all_refs {
            args.extend(["--branches".into(), "--remotes".into(), "--tags".into()]);
        } else {
            args.push("HEAD".into());
        }
    } else {
        args.extend(req.revisions.iter().cloned());
    }
    commands.push(format!("git {}", args.join(" ")));
    let mut child = cli::spawn(workdir, &args)?;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut reader = BufReader::with_capacity(1 << 20, stdout);
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk: Vec<u8> = Vec::new();
    let mut commits = 0usize;
    let mut bytes = 0u64;
    let mut stopped = false;
    loop {
        buf.clear();
        let n = match reader.read_until(0x1e, &mut buf) {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 {
            if !chunk.is_empty()
                && let Some(c) = parse_chunk(&chunk, req.signatures)
            {
                commits += 1;
                if !sink(c) {
                    stopped = true;
                }
            }
            break;
        }
        bytes += n as u64;
        let ended = buf.last() == Some(&0x1e);
        if ended {
            buf.pop();
        }
        chunk.extend_from_slice(&buf);
        if !ended {
            continue;
        }
        if !chunk.is_empty() {
            if let Some(c) = parse_chunk(&chunk, req.signatures) {
                commits += 1;
                if !sink(c) {
                    stopped = true;
                    break;
                }
            }
            chunk.clear();
        }
        if cancel.load(Ordering::Relaxed) {
            stopped = true;
            break;
        }
    }
    if stopped {
        let _ = child.kill();
    }
    let _ = child.wait();
    Ok((commits, bytes))
}

/// Parse one `%x1e`-delimited commit record.
fn parse_chunk(chunk: &[u8], signatures: bool) -> Option<LogCommit> {
    let text = String::from_utf8_lossy(chunk);
    let text = text.trim_start_matches('\n');
    if text.is_empty() {
        return None;
    }
    let fields = 11 + usize::from(signatures);
    let mut parts = text.splitn(fields + 1, '\x1f');
    let id = Oid::from_str(parts.next()?.trim()).ok()?;
    let parents = parts
        .next()?
        .split_whitespace()
        .filter_map(|p| Oid::from_str(p).ok())
        .collect();
    let author = parts.next()?.to_owned();
    let email = parts.next()?.to_owned();
    let time = parts.next()?.trim().parse().unwrap_or(0);
    let committer = parts.next()?.to_owned();
    let committer_email = parts.next()?.to_owned();
    let commit_time = parts.next()?.trim().parse().unwrap_or(0);
    let offset_minutes = parse_offset(parts.next()?);
    let summary = parts.next()?.trim().to_owned();
    let body = parts.next()?.trim().to_owned();
    let signature = if signatures {
        parts.next()?.trim().chars().next()
    } else {
        None
    };
    let rest = parts.next().unwrap_or("");
    let mut files = Vec::new();
    for line in rest.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let mut cols = line.splitn(3, '\t');
        let (Some(a), Some(d), Some(p)) = (cols.next(), cols.next(), cols.next()) else {
            continue;
        };
        let added = a.parse::<u32>().ok();
        let deleted = d.parse::<u32>().ok();
        let (path, old_path) = split_rename(p);
        files.push(FileChange {
            path,
            old_path,
            added,
            deleted,
        });
    }
    Some(LogCommit {
        id,
        parents,
        author,
        email,
        time,
        committer,
        committer_email,
        commit_time,
        offset_minutes,
        summary,
        body,
        files,
        signature,
    })
}

/// `2026-10-26 12:00:00 +0200` → 120.
fn parse_offset(iso: &str) -> i32 {
    let Some(tz) = iso.split_whitespace().nth(2) else {
        return 0;
    };
    let sign = if tz.starts_with('-') { -1 } else { 1 };
    let digits: String = tz.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() != 4 {
        return 0;
    }
    let h: i32 = digits[..2].parse().unwrap_or(0);
    let m: i32 = digits[2..].parse().unwrap_or(0);
    sign * (h * 60 + m)
}

/// `--numstat -M` rename forms: `dir/{old => new}.rs`, `old.rs => new.rs`.
/// Returns `(new_path, Some(old_path))` or `(path, None)`.
pub fn split_rename(p: &str) -> (String, Option<String>) {
    if let (Some(open), Some(close)) = (p.find('{'), p.rfind('}'))
        && open < close
        && let Some((old, new)) = p[open + 1..close].split_once(" => ")
    {
        let prefix = &p[..open];
        let suffix = &p[close + 1..];
        let fix = |s: &str| {
            format!("{prefix}{s}{suffix}")
                .replace("//", "/")
                .trim_start_matches('/')
                .to_owned()
        };
        return (fix(new), Some(fix(old)));
    }
    if let Some((old, new)) = p.split_once(" => ") {
        return (new.to_owned(), Some(old.to_owned()));
    }
    (p.to_owned(), None)
}

// ---------------------------------------------------------------------------
// The full analysis
// ---------------------------------------------------------------------------

/// Everything except the on-demand extras (weight, debt dating, blame,
/// search, recovery). Each phase is timed into `stats.phases`.
pub fn analyze(
    workdir: &Path,
    opts: &InsightsOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<InsightsReport> {
    let started = Instant::now();
    let mut commands = Vec::new();
    let mut report = InsightsReport::default();
    let mut phase_started = Instant::now();
    let mut phase = |name: &str, report: &mut InsightsReport, prog: &mut Progress| {
        if !prog.phase.is_empty() {
            report.stats.phases.push((
                prog.phase.clone(),
                phase_started.elapsed().as_millis() as u64,
            ));
        }
        phase_started = Instant::now();
        prog.phase = name.to_owned();
        prog.done = 0;
        prog.total = None;
    };
    let mut prog = Progress::default();
    let cancelled = |report: &mut InsightsReport| {
        report.stats.cancelled = true;
    };

    phase("Listing files", &mut report, &mut prog);
    progress(&prog);
    let files = tracked_files(workdir, &mut commands)?;
    report.stats.files = files.len();

    phase("Checking repository health", &mut report, &mut prog);
    progress(&prog);
    report.health = health::analyze(workdir, &files, &mut commands);
    if cancel.load(Ordering::Relaxed) {
        cancelled(&mut report);
        return finish(report, commands, started, prog);
    }

    phase("Reading history (numstat)", &mut report, &mut prog);
    progress(&prog);
    let (history, commits, log_bytes) =
        history::analyze(workdir, opts, &files, cancel, progress, &mut commands)?;
    report.history = history;
    report.stats.commits = commits;
    report.stats.log_bytes = log_bytes;
    if cancel.load(Ordering::Relaxed) {
        cancelled(&mut report);
        return finish(report, commands, started, prog);
    }

    phase("Reading branches and tags", &mut report, &mut prog);
    progress(&prog);
    report.refs = refs::analyze(workdir, opts, cancel, &mut commands);
    if cancel.load(Ordering::Relaxed) {
        cancelled(&mut report);
        return finish(report, commands, started, prog);
    }

    phase("Scanning for debt markers", &mut report, &mut prog);
    progress(&prog);
    report.debt = debt::scan(workdir, &files, cancel, progress);
    if cancel.load(Ordering::Relaxed) {
        cancelled(&mut report);
        return finish(report, commands, started, prog);
    }

    phase("Reading CI configuration", &mut report, &mut prog);
    progress(&prog);
    report.ci = ci::analyze(workdir, &files);

    phase("Reading licences", &mut report, &mut prog);
    progress(&prog);
    report.license = license::analyze(workdir, &files, cancel, progress, &mut commands);

    phase("", &mut report, &mut prog);
    finish(report, commands, started, prog)
}

fn finish(
    mut report: InsightsReport,
    commands: Vec<String>,
    started: Instant,
    _prog: Progress,
) -> Result<InsightsReport> {
    report.commands = commands;
    report.stats.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_forms() {
        assert_eq!(
            split_rename("src/{old => new}/a.rs"),
            ("src/new/a.rs".to_owned(), Some("src/old/a.rs".to_owned()))
        );
        assert_eq!(
            split_rename("{ => sub}/a.rs"),
            ("sub/a.rs".to_owned(), Some("a.rs".to_owned()))
        );
        assert_eq!(
            split_rename("a.rs => b.rs"),
            ("b.rs".to_owned(), Some("a.rs".to_owned()))
        );
        assert_eq!(split_rename("plain.rs"), ("plain.rs".to_owned(), None));
    }

    #[test]
    fn offsets_dirs_exts() {
        assert_eq!(parse_offset("2026-10-26 12:00:00 +0200"), 120);
        assert_eq!(parse_offset("2026-10-26 12:00:00 -0530"), -330);
        assert_eq!(parse_offset("garbage"), 0);
        assert_eq!(dir_of("src/a/b.rs", 2), "src/a");
        assert_eq!(dir_of("src/a/b/c.rs", 2), "src/a");
        assert_eq!(dir_of("README.md", 2), ".");
        assert_eq!(dir_of("src/x.rs", 2), "src");
        assert_eq!(ext_of("a/B.RS"), "rs");
        assert_eq!(ext_of(".gitignore"), "");
        assert_eq!(ext_of("Makefile"), "");
        assert_eq!(days_between(0, 86_400 * 3 + 5), 3);
    }

    #[test]
    fn parses_a_numstat_chunk() {
        let chunk = "0123456789abcdef0123456789abcdef01234567\x1fabcdefabcdefabcdefabcdefabcdefabcdefabcd\x1fAda\x1fada@example.com\x1f1700000000\x1fBob\x1fbob@example.com\x1f1700000100\x1f2023-11-14 22:13:20 +0100\x1fAdd thing\x1fLonger body\nline two\x1f\n\n3\t1\tsrc/a.rs\n-\t-\timg.png\n0\t2\tdir/{old => new}/b.rs\n";
        let c = parse_chunk(chunk.as_bytes(), false).expect("parses");
        assert_eq!(c.author, "Ada");
        assert_eq!(c.committer_email, "bob@example.com");
        assert_eq!(c.parents.len(), 1);
        assert_eq!(c.offset_minutes, 60);
        assert_eq!(c.summary, "Add thing");
        assert_eq!(c.body, "Longer body\nline two");
        assert_eq!(c.files.len(), 3);
        assert_eq!(c.files[0].added, Some(3));
        assert!(c.files[1].is_binary());
        assert_eq!(c.files[2].path, "dir/new/b.rs");
        assert_eq!(c.files[2].old_path.as_deref(), Some("dir/old/b.rs"));
        assert_eq!(c.added(), 3);
        assert_eq!(c.deleted(), 3);
        assert!(c.signature.is_none());
    }

    #[test]
    fn parses_signature_field() {
        let chunk = "0123456789abcdef0123456789abcdef01234567\x1f\x1fAda\x1fa@x\x1f1\x1fAda\x1fa@x\x1f1\x1f2023-11-14 22:13:20 +0000\x1fRoot\x1f\x1fG\x1f\n";
        let c = parse_chunk(chunk.as_bytes(), true).expect("parses");
        assert_eq!(c.signature, Some('G'));
        assert!(c.parents.is_empty());
        assert!(c.files.is_empty());
    }
}
