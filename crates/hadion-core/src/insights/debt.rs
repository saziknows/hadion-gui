//! Technical-debt markers (TODO, FIXME, HACK, XXX, BUG, OPTIMIZE,
//! DEPRECATED, NOCOMMIT, KLUDGE, TEMP) in the tracked text files, and on
//! demand their age and author through `git blame`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aho_corasick::AhoCorasick;
use regex::Regex;

use super::{Progress, TrackedFile, dir_of, ext_of};
use crate::secrets::{basename, is_binary, skip_content};
use crate::{Oid, Result, cli};

/// Marker kinds, in display order.
pub const KINDS: &[&str] = &[
    "FIXME",
    "BUG",
    "HACK",
    "XXX",
    "TODO",
    "OPTIMIZE",
    "DEPRECATED",
    "NOCOMMIT",
    "KLUDGE",
    "TEMP",
];

/// Markers kept at most; the scan stops reading files once reached.
pub const MAX_MARKERS: usize = 20_000;

/// Text files larger than this are not read (debt and licence scans).
pub(crate) const MAX_TEXT_BYTES: u64 = 2 * 1024 * 1024;

/// `git blame` processes run at once by [`date`].
const BLAME_THREADS: usize = 4;

/// Blame commands recorded verbatim before the list is summarised.
const MAX_COMMANDS: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marker {
    pub path: String,
    /// 1-based.
    pub line: u32,
    pub kind: &'static str,
    /// The comment text after the marker (trimmed, ≤ 200 chars).
    pub text: String,
    /// Assignee in `TODO(name):` / `TODO @name` form.
    pub assignee: Option<String>,
    /// Ticket key or issue number mentioned (`ABC-123`, `#123`).
    pub ticket: Option<String>,
    /// Filled in by [`date`].
    pub author: Option<String>,
    pub time: Option<i64>,
    pub commit: Option<Oid>,
}

#[derive(Clone, Debug, Default)]
pub struct Debt {
    /// Grouped by file then line.
    pub markers: Vec<Marker>,
    /// `(kind, count)` in [`KINDS`] order, zero counts omitted.
    pub by_kind: Vec<(&'static str, u32)>,
    /// `(dir depth 2, count)` desc, top 40.
    pub by_dir: Vec<(String, u32)>,
    /// `(assignee, count)` desc.
    pub by_assignee: Vec<(String, u32)>,
    pub files_with_markers: usize,
    pub files_scanned: usize,
    pub bytes_scanned: u64,
    /// Markers per 1,000 lines of scanned text.
    pub density: f64,
    pub lines_scanned: u64,
    /// `date` ran: `author`/`time` are filled where blame succeeded.
    pub dated: bool,
    pub dated_files: usize,
    pub cancelled: bool,
    /// The scan stopped at [`MAX_MARKERS`]: files after that point were not
    /// read and the counts are lower bounds.
    pub truncated: bool,
}

impl Debt {
    pub fn count(&self, kind: &str) -> u32 {
        self.by_kind
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, n)| *n)
            .unwrap_or(0)
    }

    /// Oldest dated marker, if any.
    pub fn oldest(&self) -> Option<&Marker> {
        self.markers
            .iter()
            .filter(|m| m.time.is_some())
            .min_by_key(|m| m.time)
    }
}

// ---------------------------------------------------------------------------
// File selection and parallel reading (shared with the licence scan)
// ---------------------------------------------------------------------------

/// Extensions that are never text, beyond what `secrets::skip_content` knows.
const BINARY_EXTS: &[&str] = &[
    "exe",
    "dll",
    "so",
    "dylib",
    "pdb",
    "o",
    "a",
    "lib",
    "obj",
    "bin",
    "wasm",
    "pyc",
    "pyo",
    "class",
    "jar",
    "apk",
    "aab",
    "ipa",
    "dex",
    "elf",
    "node",
    "pyd",
    "ico",
    "cur",
    "icns",
    "dat",
    "db",
    "sqlite",
    "sqlite3",
    "ttf",
    "otf",
    "ttc",
    "woff",
    "woff2",
    "eot",
    "mo",
    "nib",
    "swf",
    "dmg",
    "iso",
    "img",
    "vhd",
    "msi",
    "cab",
    "deb",
    "rpm",
    "tar",
    "tgz",
    "zip",
    "gz",
    "bz2",
    "xz",
    "zst",
    "7z",
    "rar",
    "lz4",
    "br",
    "jpg",
    "jpeg",
    "png",
    "gif",
    "bmp",
    "tif",
    "tiff",
    "webp",
    "heic",
    "avif",
    "psd",
    "xcf",
    "mp3",
    "mp4",
    "m4a",
    "m4v",
    "mov",
    "avi",
    "mkv",
    "wav",
    "ogg",
    "oga",
    "ogv",
    "flac",
    "aac",
    "wma",
    "wmv",
    "webm",
    "pdf",
    "doc",
    "docx",
    "xls",
    "xlsx",
    "ppt",
    "pptx",
    "odt",
    "ods",
    "odp",
    "pak",
    "crx",
    "xpi",
    "snap",
    "pb",
    "onnx",
    "h5",
    "npy",
    "npz",
    "parquet",
    "arrow",
    "feather",
    "pkl",
    "pickle",
    "safetensors",
    "gguf",
    "sig",
    "gpg",
    "pfx",
    "p12",
    "jks",
    "keystore",
    "der",
    "ser",
    "rlib",
    "rmeta",
    "lockb",
];

/// Directories whose contents are generated or belong to someone else.
const SKIP_DIRS: &[&str] = &[
    "/node_modules/",
    "/vendor/",
    "/third_party/",
    "/third-party/",
    "/dist/",
    "/build/",
    "/.git/",
];

/// Paths the text scans never read: vendored and generated trees,
/// lockfiles, minified bundles, binaries by extension.
pub(crate) fn skip_path(path: &str) -> bool {
    let slashed = format!("/{path}");
    if SKIP_DIRS.iter().any(|d| slashed.contains(d)) || slashed.contains("/.min.") {
        return true;
    }
    let base = basename(path).to_lowercase();
    if base.ends_with(".lock")
        || base.contains(".min.")
        || matches!(
            base.as_str(),
            "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "cargo.lock" | "go.sum"
        )
    {
        return true;
    }
    if skip_content(path) {
        return true;
    }
    BINARY_EXTS.contains(&ext_of(path).as_str())
}

/// Regular tracked files worth reading as text (not symlinks, gitlinks,
/// skipped paths or files over [`MAX_TEXT_BYTES`] in HEAD).
pub(crate) fn text_candidates(files: &[TrackedFile]) -> Vec<&TrackedFile> {
    files
        .iter()
        .filter(|f| {
            !f.is_symlink()
                && !f.is_gitlink()
                && f.size.is_none_or(|s| s <= MAX_TEXT_BYTES)
                && !skip_path(&f.path)
        })
        .collect()
}

/// Read a candidate from the working tree; `None` when it is missing,
/// larger than [`MAX_TEXT_BYTES`] on disk, or binary (NUL in the first 8 KB).
pub(crate) fn read_text(workdir: &Path, file: &TrackedFile) -> Option<Vec<u8>> {
    let full = workdir.join(&file.path);
    if file.size.is_none() {
        let meta = std::fs::metadata(&full).ok()?;
        if !meta.is_file() || meta.len() > MAX_TEXT_BYTES {
            return None;
        }
    }
    let data = std::fs::read(&full).ok()?;
    if data.len() as u64 > MAX_TEXT_BYTES || is_binary(&data) {
        return None;
    }
    Some(data)
}

/// Run `work` over `files` on all cores through a shared work queue,
/// reporting progress under `phase` about every 500 files. `stop` lets the
/// caller end the scan early (marker cap). Returns the results in input
/// order and how many files were taken from the queue.
pub(crate) fn scan_parallel<T, F>(
    files: &[&TrackedFile],
    phase: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
    stop: &(dyn Fn() -> bool + Sync),
    work: F,
) -> (Vec<(usize, T)>, usize)
where
    T: Send,
    F: Fn(&TrackedFile) -> Option<T> + Sync,
{
    let total = files.len();
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 16)
        .min(total.max(1));
    let work = &work;
    let report = |progress: &mut dyn FnMut(&Progress), done: usize| {
        progress(&Progress {
            phase: phase.to_owned(),
            done,
            total: Some(total),
            bytes: 0,
            findings: 0,
        });
    };
    report(progress, 0);
    let results: Vec<Vec<(usize, T)>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let next = &next;
                s.spawn(move || {
                    let mut out = Vec::new();
                    loop {
                        if cancel.load(Ordering::Relaxed) || stop() {
                            break;
                        }
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(file) = files.get(i) else {
                            break;
                        };
                        if let Some(t) = work(file) {
                            out.push((i, t));
                        }
                    }
                    out
                })
            })
            .collect();
        let mut reported = 0usize;
        loop {
            let done = next.load(Ordering::Relaxed).min(total);
            if done >= total || cancel.load(Ordering::Relaxed) || stop() {
                break;
            }
            if done - reported >= 500 {
                reported = done;
                report(progress, done);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });
    let visited = next.load(Ordering::Relaxed).min(total);
    report(progress, visited);
    let mut all: Vec<(usize, T)> = results.into_iter().flatten().collect();
    all.sort_by_key(|(i, _)| *i);
    (all, visited)
}

// ---------------------------------------------------------------------------
// Marker matching
// ---------------------------------------------------------------------------

fn automaton() -> &'static AhoCorasick {
    static AC: OnceLock<AhoCorasick> = OnceLock::new();
    AC.get_or_init(|| AhoCorasick::new(KINDS).expect("marker automaton"))
}

fn ticket_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b([A-Z][A-Z0-9]+)-(\d+)\b|#(\d+)").expect("ticket regex"))
}

/// Upper-case prefixes that look like ticket keys but are not.
const NOT_TICKETS: &[&str] = &[
    "UTF", "SHA", "ISO", "X86", "MD", "AES", "RSA", "TLS", "SSL", "IPV", "UUID", "HTML", "CSS",
    "ES", "GL", "EC", "WIN", "C", "RGB", "RGBA", "ARM", "RFC", "COVID", "HTTP", "HTTP2", "H",
    "SHA1", "SHA2", "SHA3", "IEEE", "UTF8", "UTF16", "ARMV", "X", "Y", "Z", "I", "V", "P", "T",
    "LZ", "CRC", "WPA", "WPA2", "PBKDF", "PBKDF2", "AMD", "NEON", "AVX", "AVX2", "SSE", "SSE2",
    "SSE4", "MIPS", "WIN32", "WIN64", "ASCII", "BASE", "BASE64", "COM", "UNC", "ID", "ECMA",
    "POSIX", "GCC", "MSVC", "LLVM", "CL", "NUMBER", "NODE", "V8", "BUG",
];

/// Markdown-like files where prose markers count without a comment leader.
fn is_prose(path: &str) -> bool {
    matches!(
        ext_of(path).as_str(),
        "md" | "markdown" | "mdx" | "txt" | "text" | "org" | "rst" | "adoc" | "asciidoc"
    )
}

/// The line before the marker contains a comment leader.
fn has_comment_leader(prefix: &[u8]) -> bool {
    const LEADERS: &[&[u8]] = &[
        b"//", b"#", b"/*", b"*", b"<!--", b"--", b";", b"%", b"'", b"\"\"\"",
    ];
    if LEADERS.iter().any(|l| contains(prefix, l)) {
        return true;
    }
    // Batch `REM` (any case) at the start of the line or after a separator.
    prefix.windows(4).enumerate().any(|(i, w)| {
        w[..3].eq_ignore_ascii_case(b"rem")
            && matches!(w[3], b' ' | b'\t')
            && (i == 0 || matches!(prefix[i - 1], b' ' | b'\t' | b'@' | b'&'))
    })
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    needle.len() <= hay.len() && hay.windows(needle.len()).any(|w| w == needle)
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// `TODO(bob)`, `TODO @bob`, `TODO bob:`. Returns the assignee and the
/// text that follows it (or the whole input when there is none).
fn split_assignee(rest: &str) -> (Option<String>, &str) {
    if let Some(inner) = rest.strip_prefix('(')
        && let Some(close) = inner.find(')')
    {
        let name = inner[..close].trim();
        if valid_assignee(name) {
            return (Some(name.to_owned()), &inner[close + 1..]);
        }
        return (None, rest);
    }
    let trimmed = rest.trim_start_matches([' ', '\t', ':', '-']);
    if let Some(after_at) = trimmed.strip_prefix('@') {
        let end = after_at
            .find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.')))
            .unwrap_or(after_at.len());
        let name = after_at[..end].trim_end_matches('.');
        if !name.is_empty() {
            return (Some(name.to_owned()), &after_at[end..]);
        }
    }
    let spaced = rest.trim_start_matches([' ', '\t']);
    if let Some(colon) = spaced.find(':') {
        let name = &spaced[..colon];
        let after = &spaced[colon + 1..];
        let scheme = matches!(
            name.to_ascii_lowercase().as_str(),
            "http" | "https" | "ftp" | "file" | "mailto" | "ssh" | "git" | "note" | "see"
        );
        if (2..=32).contains(&name.len())
            && name.chars().next().is_some_and(|c| c.is_alphabetic())
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
            && !scheme
            && after.chars().next().is_none_or(|c| c.is_whitespace())
        {
            return (Some(name.to_owned()), after);
        }
    }
    (None, rest)
}

fn valid_assignee(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && !name.contains('/')
        && !name.contains(' ')
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '@' || c == '_')
}

/// Strip the separators after the marker and the comment closers at the
/// end, cut to 200 characters.
fn clean_text(s: &str) -> String {
    let mut t = s.trim();
    loop {
        let before = t;
        for closer in ["*/", "-->", "--}}", "#}", "%>", "?>"] {
            t = t.trim_end_matches(closer).trim_end();
        }
        if t == before {
            break;
        }
    }
    let t = t
        .trim_start_matches([' ', '\t', ':', '-'])
        .trim_end_matches([' ', '\t', '-', '*'])
        .trim();
    match t.char_indices().nth(200) {
        Some((i, _)) => t[..i].trim_end().to_owned(),
        None => t.to_owned(),
    }
}

fn find_ticket(s: &str) -> Option<String> {
    for cap in ticket_regex().captures_iter(s) {
        if let Some(num) = cap.get(3) {
            return Some(format!("#{}", num.as_str()));
        }
        let key = cap.get(1)?.as_str();
        if NOT_TICKETS.contains(&key) {
            continue;
        }
        return Some(cap.get(0)?.as_str().to_owned());
    }
    None
}

/// Validate a candidate match and build the marker.
fn match_marker(
    path: &str,
    line_no: u32,
    line: &[u8],
    at: usize,
    len: usize,
    kind: &'static str,
    prose: bool,
) -> Option<Marker> {
    if at > 0 && is_word_byte(line[at - 1]) {
        return None;
    }
    match line.get(at + len) {
        None | Some(b':' | b'(' | b' ' | b'\t' | b'!' | b'-' | b'@' | b']' | b'\r') => {}
        _ => return None,
    }
    if !prose && !has_comment_leader(&line[..at]) {
        return None;
    }
    let rest = String::from_utf8_lossy(&line[at + len..]);
    let rest = rest.trim_end_matches('\r');
    let (assignee, after) = split_assignee(rest);
    Some(Marker {
        path: path.to_owned(),
        line: line_no,
        kind,
        text: clean_text(after),
        assignee,
        ticket: find_ticket(rest),
        author: None,
        time: None,
        commit: None,
    })
}

struct FileScan {
    markers: Vec<Marker>,
    bytes: u64,
    lines: u64,
}

fn count_newlines(data: &[u8]) -> usize {
    data.iter().filter(|&&b| b == b'\n').count()
}

/// Find the markers of one file (at most one per line).
fn scan_bytes(path: &str, data: &[u8]) -> FileScan {
    let prose = is_prose(path);
    let mut markers = Vec::new();
    let mut line_no: u32 = 1;
    let mut cursor = 0usize;
    let mut last_line_start = usize::MAX;
    for m in automaton().find_iter(data) {
        let start = m.start();
        line_no += count_newlines(&data[cursor..start]) as u32;
        cursor = start;
        let line_start = data[..start]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |p| p + 1);
        if line_start == last_line_start {
            continue;
        }
        let line_end = data[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(data.len(), |p| start + p);
        let line = &data[line_start..line_end];
        let kind = KINDS[m.pattern().as_usize()];
        if let Some(marker) = match_marker(
            path,
            line_no,
            line,
            start - line_start,
            m.end() - start,
            kind,
            prose,
        ) {
            markers.push(marker);
            last_line_start = line_start;
        }
    }
    let lines = count_newlines(data) + usize::from(!data.is_empty() && data.last() != Some(&b'\n'));
    FileScan {
        markers,
        bytes: data.len() as u64,
        lines: lines as u64,
    }
}

// ---------------------------------------------------------------------------
// The scan
// ---------------------------------------------------------------------------

/// Scan the tracked text files (skip binaries, minified files, lockfiles,
/// vendored directories, files over 2 MB) in parallel for markers at the
/// start of a comment or after a comment leader. Checks `cancel` and
/// reports progress per ~500 files.
pub fn scan(
    workdir: &Path,
    files: &[TrackedFile],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Debt {
    let selected = text_candidates(files);
    let found = AtomicUsize::new(0);
    let stop = || found.load(Ordering::Relaxed) >= MAX_MARKERS;
    let (results, visited) = scan_parallel(
        &selected,
        "Scanning for debt markers",
        cancel,
        progress,
        &stop,
        |file| {
            let data = read_text(workdir, file)?;
            let out = scan_bytes(&file.path, &data);
            found.fetch_add(out.markers.len(), Ordering::Relaxed);
            Some(out)
        },
    );
    let hit_cap = found.load(Ordering::Relaxed) >= MAX_MARKERS;
    let mut debt = Debt {
        files_scanned: results.len(),
        cancelled: cancel.load(Ordering::Relaxed),
        truncated: hit_cap || visited < selected.len() && !cancel.load(Ordering::Relaxed),
        ..Debt::default()
    };
    let mut markers = Vec::new();
    for (_, scan) in results {
        debt.bytes_scanned += scan.bytes;
        debt.lines_scanned += scan.lines;
        markers.extend(scan.markers);
    }
    markers.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    if markers.len() > MAX_MARKERS {
        markers.truncate(MAX_MARKERS);
        debt.truncated = true;
    }
    debt.markers = markers;
    aggregate(&mut debt);
    debt
}

/// Recompute the summaries from `markers`.
fn aggregate(debt: &mut Debt) {
    let mut by_kind: HashMap<&str, u32> = HashMap::new();
    let mut by_dir: HashMap<String, u32> = HashMap::new();
    let mut by_assignee: HashMap<&str, u32> = HashMap::new();
    let mut files: Vec<&str> = Vec::new();
    for m in &debt.markers {
        *by_kind.entry(m.kind).or_default() += 1;
        *by_dir.entry(dir_of(&m.path, 2)).or_default() += 1;
        if let Some(a) = &m.assignee {
            *by_assignee.entry(a.as_str()).or_default() += 1;
        }
        if files.last() != Some(&m.path.as_str()) {
            files.push(&m.path);
        }
    }
    files.dedup();
    debt.files_with_markers = files.len();
    debt.by_kind = KINDS
        .iter()
        .filter_map(|k| by_kind.get(k).map(|n| (*k, *n)))
        .collect();
    let mut dirs: Vec<(String, u32)> = by_dir.into_iter().collect();
    dirs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    dirs.truncate(40);
    debt.by_dir = dirs;
    let mut assignees: Vec<(String, u32)> = by_assignee
        .into_iter()
        .map(|(a, n)| (a.to_owned(), n))
        .collect();
    assignees.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    debt.by_assignee = assignees;
    debt.density = if debt.lines_scanned > 0 {
        debt.markers.len() as f64 * 1000.0 / debt.lines_scanned as f64
    } else {
        0.0
    };
}

// ---------------------------------------------------------------------------
// Dating through git blame
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct Dated {
    /// The input markers with `author`/`time`/`commit` filled where known.
    pub markers: Vec<Marker>,
    pub files_blamed: usize,
    pub files_skipped: usize,
    pub elapsed_ms: u64,
    pub cancelled: bool,
    pub commands: Vec<String>,
}

/// One line of `git blame --porcelain`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct BlameLine {
    author: String,
    time: i64,
    sha: String,
}

/// Parse `git blame --porcelain` into final line number → origin. Later
/// groups of an already-seen commit omit the header fields, so they are
/// cached per sha.
fn parse_porcelain(text: &str) -> HashMap<u32, BlameLine> {
    let mut out = HashMap::new();
    let mut cache: HashMap<String, (String, i64)> = HashMap::new();
    let mut current: Option<(String, u32)> = None;
    for line in text.lines() {
        if let Some((sha, final_line)) = current.take() {
            if line.starts_with('\t') {
                let (author, time) = cache.get(&sha).cloned().unwrap_or_default();
                out.insert(final_line, BlameLine { author, time, sha });
                continue;
            }
            // A header field of the current commit.
            let entry = cache.entry(sha.clone()).or_default();
            match line.split_once(' ') {
                Some(("author", v)) => entry.0 = v.to_owned(),
                Some(("author-time", v)) => entry.1 = v.trim().parse().unwrap_or(0),
                _ => {}
            }
            current = Some((sha, final_line));
            continue;
        }
        // `<sha> <orig_line> <final_line> [<group_lines>]`
        let mut parts = line.split(' ');
        if let (Some(sha), Some(_orig), Some(fin)) = (parts.next(), parts.next(), parts.next())
            && sha.len() >= 40
            && sha.bytes().all(|b| b.is_ascii_hexdigit())
            && let Ok(final_line) = fin.parse::<u32>()
        {
            cache.entry(sha.to_owned()).or_default();
            current = Some((sha.to_owned(), final_line));
        }
    }
    out
}

/// Date the markers: `git blame --porcelain HEAD -- file` once per file
/// (files ordered by marker count desc, at most `max_files`), several
/// files in parallel. Lines are matched by number, so run it on the
/// markers of the same scan.
pub fn date(
    workdir: &Path,
    markers: &[Marker],
    max_files: usize,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<Dated> {
    let started = Instant::now();
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for m in markers {
        *counts.entry(m.path.as_str()).or_default() += 1;
    }
    let mut paths: Vec<(&str, usize)> = counts.into_iter().collect();
    paths.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let total_files = paths.len();
    let chosen: Vec<&str> = paths.iter().take(max_files).map(|(p, _)| *p).collect();
    let total = chosen.len();

    let phase = "Dating markers (git blame)";
    let report = |progress: &mut dyn FnMut(&Progress), done: usize| {
        progress(&Progress {
            phase: phase.to_owned(),
            done,
            total: Some(total),
            bytes: 0,
            findings: 0,
        });
    };
    report(progress, 0);

    type Blamed = (usize, std::result::Result<HashMap<u32, BlameLine>, String>);
    let next = AtomicUsize::new(0);
    let threads = BLAME_THREADS.min(total.max(1));
    let results: Vec<Vec<Blamed>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let next = &next;
                let chosen = &chosen;
                s.spawn(move || {
                    let mut out: Vec<Blamed> = Vec::new();
                    loop {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(path) = chosen.get(i) else {
                            break;
                        };
                        let args = ["blame", "--porcelain", "HEAD", "--", path];
                        let res = cli::run(workdir, &args, None)
                            .map(|o| parse_porcelain(&String::from_utf8_lossy(&o.stdout)))
                            .map_err(|e| e.to_string());
                        out.push((i, res));
                    }
                    out
                })
            })
            .collect();
        let mut reported = 0usize;
        loop {
            let done = next.load(Ordering::Relaxed).min(total);
            if done >= total || cancel.load(Ordering::Relaxed) {
                break;
            }
            if done > reported {
                reported = done;
                report(progress, done);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });

    let mut blamed: Vec<Blamed> = results.into_iter().flatten().collect();
    blamed.sort_by_key(|(i, _)| *i);
    let mut commands = Vec::new();
    let mut by_path: HashMap<&str, HashMap<u32, BlameLine>> = HashMap::new();
    let mut first_error: Option<String> = None;
    for (i, res) in blamed {
        let path = chosen[i];
        if commands.len() < MAX_COMMANDS {
            commands.push(format!("git blame --porcelain HEAD -- {path}"));
        }
        match res {
            Ok(map) => {
                by_path.insert(path, map);
            }
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    let attempted = next.load(Ordering::Relaxed).min(total);
    if attempted > MAX_COMMANDS {
        commands.push(format!("… and {} more", attempted - MAX_COMMANDS));
    }
    let files_blamed = by_path.len();
    if files_blamed == 0
        && let Some(e) = first_error
        && !cancel.load(Ordering::Relaxed)
    {
        return Err(crate::Error::new(format!("git blame failed: {e}")));
    }
    let mut markers = markers.to_vec();
    for m in &mut markers {
        if let Some(line) = by_path
            .get(m.path.as_str())
            .and_then(|map| map.get(&m.line))
        {
            m.author = Some(line.author.clone());
            m.time = Some(line.time);
            m.commit = Oid::from_str(&line.sha).ok();
        }
    }
    report(progress, attempted);
    Ok(Dated {
        markers,
        files_blamed,
        files_skipped: total_files - files_blamed,
        elapsed_ms: started.elapsed().as_millis() as u64,
        cancelled: cancel.load(Ordering::Relaxed),
        commands,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn markers_of(path: &str, text: &str) -> Vec<Marker> {
        scan_bytes(path, text.as_bytes()).markers
    }

    #[test]
    fn matches_markers_after_comment_leaders() {
        let text = "fn a() {}\n// TODO: x\n# FIXME(bob): y\n/* HACK */\n<!-- TODO -->\n-- XXX\nlet s = 1; // BUG: boom\n";
        let m = markers_of("src/a.rs", text);
        let kinds: Vec<&str> = m.iter().map(|m| m.kind).collect();
        assert_eq!(kinds, ["TODO", "FIXME", "HACK", "TODO", "XXX", "BUG"]);
        assert_eq!(m[0].line, 2);
        assert_eq!(m[0].text, "x");
        assert_eq!(m[1].assignee.as_deref(), Some("bob"));
        assert_eq!(m[1].text, "y");
        assert_eq!(m[2].text, "");
        assert_eq!(m[3].text, "");
        assert_eq!(m[5].line, 7);
        assert_eq!(m[5].text, "boom");
    }

    #[test]
    fn prose_and_word_boundaries() {
        // Lower-case prose, identifiers and words containing the marker never match.
        let text = "let todo = 1; // the todo list\nlet x = TODOS;\nfn do_TODO_thing() {}\nlet s = \"TODO\";\n// TODOS are many\n// MYTODO\nconst TODO: u8 = 1;\n";
        assert!(markers_of("src/a.rs", text).is_empty());
        // Markdown counts prose markers without a leader.
        let m = markers_of("README.md", "TODO: write docs\nnot a todo\n");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].text, "write docs");
        // Plain text without a leader in a source file does not.
        assert!(markers_of("a.py", "TODO: write docs\n").is_empty());
        assert_eq!(markers_of("a.py", "# TODO: write docs\n").len(), 1);
        assert_eq!(markers_of("a.bat", "REM TODO: write docs\n").len(), 1);
    }

    #[test]
    fn assignees_and_tickets() {
        let m = markers_of("src/a.rs", "// TODO(bob): fix #12\n");
        assert_eq!(m[0].assignee.as_deref(), Some("bob"));
        assert_eq!(m[0].ticket.as_deref(), Some("#12"));
        assert_eq!(m[0].text, "fix #12");
        let m = markers_of("src/a.rs", "// TODO @alice: ABC-123 needs work\n");
        assert_eq!(m[0].assignee.as_deref(), Some("alice"));
        assert_eq!(m[0].ticket.as_deref(), Some("ABC-123"));
        assert_eq!(m[0].text, "ABC-123 needs work");
        let m = markers_of("src/a.rs", "// TODO carol: handle UTF-8 input\n");
        assert_eq!(m[0].assignee.as_deref(), Some("carol"));
        assert_eq!(m[0].ticket, None, "UTF-8 is not a ticket");
        let m = markers_of("src/a.rs", "// TODO(b/12345): google style bug\n");
        assert_eq!(m[0].assignee, None);
        assert_eq!(m[0].text, "(b/12345): google style bug");
        let m = markers_of("src/a.rs", "// TODO https://example.com/x\n");
        assert_eq!(m[0].assignee, None);
        let m = markers_of("src/a.c", "/* FIXME: trailing closer */\n");
        assert_eq!(m[0].text, "trailing closer");
    }

    #[test]
    fn one_marker_per_line_and_counts() {
        let text = "// TODO FIXME both\n\n// XXX\nlast line no newline";
        let scan = scan_bytes("a.rs", text.as_bytes());
        assert_eq!(scan.markers.len(), 2);
        assert_eq!(scan.markers[0].kind, "TODO");
        assert_eq!(scan.lines, 4);
        assert_eq!(scan.bytes, text.len() as u64);
    }

    #[test]
    fn text_is_capped_at_200_chars() {
        let long = "x".repeat(300);
        let m = markers_of("a.rs", &format!("// TODO: {long}\n"));
        assert_eq!(m[0].text.chars().count(), 200);
    }

    #[test]
    fn skips_generated_and_binary_paths() {
        assert!(skip_path("node_modules/a/index.js"));
        assert!(skip_path("src/vendor/x.c"));
        assert!(skip_path("app/dist/bundle.js"));
        assert!(skip_path("static/app.min.js"));
        assert!(skip_path("Cargo.lock"));
        assert!(skip_path("go.sum"));
        assert!(skip_path("img/logo.png"));
        assert!(skip_path("bin/tool.exe"));
        assert!(!skip_path("src/main.rs"));
        assert!(!skip_path("README.md"));
        assert!(!skip_path("build.rs"));
        assert!(!skip_path("builder/x.rs"));
    }

    #[test]
    fn parses_blame_porcelain() {
        let sha_a = "a".repeat(40);
        let sha_b = "b".repeat(40);
        let text = format!(
            "{sha_a} 1 1 2\nauthor Ada\nauthor-mail <ada@example.com>\nauthor-time 1700000000\nauthor-tz +0100\ncommitter Ada\ncommitter-mail <ada@example.com>\ncommitter-time 1700000000\ncommitter-tz +0100\nsummary First\nboundary\nfilename a.rs\n\tline one\n{sha_a} 2 2\n\tline two\n{sha_b} 1 3 1\nauthor Bob\nauthor-mail <bob@example.com>\nauthor-time 1800000000\nauthor-tz +0000\ncommitter Bob\ncommitter-mail <bob@example.com>\ncommitter-time 1800000000\ncommitter-tz +0000\nsummary Second\nprevious {sha_a} a.rs\nfilename a.rs\n\tline three\n{sha_a} 3 4 1\n\tline four\n"
        );
        let map = parse_porcelain(&text);
        assert_eq!(map.len(), 4);
        assert_eq!(map[&1].author, "Ada");
        assert_eq!(map[&1].time, 1_700_000_000);
        assert_eq!(map[&2].author, "Ada", "second line of the first group");
        assert_eq!(map[&3].author, "Bob");
        assert_eq!(map[&3].sha, sha_b);
        assert_eq!(map[&4].author, "Ada", "later group of a seen commit");
        assert_eq!(map[&4].time, 1_700_000_000);
    }

    #[test]
    fn aggregates() {
        let mut debt = Debt {
            markers: vec![
                Marker {
                    path: "src/a/b.rs".into(),
                    line: 1,
                    kind: "TODO",
                    text: String::new(),
                    assignee: Some("bob".into()),
                    ticket: None,
                    author: None,
                    time: None,
                    commit: None,
                },
                Marker {
                    path: "src/a/b.rs".into(),
                    line: 9,
                    kind: "FIXME",
                    text: String::new(),
                    assignee: Some("bob".into()),
                    ticket: None,
                    author: None,
                    time: None,
                    commit: None,
                },
                Marker {
                    path: "x.rs".into(),
                    line: 2,
                    kind: "TODO",
                    text: String::new(),
                    assignee: None,
                    ticket: None,
                    author: None,
                    time: None,
                    commit: None,
                },
            ],
            lines_scanned: 1500,
            ..Debt::default()
        };
        aggregate(&mut debt);
        assert_eq!(debt.by_kind, vec![("FIXME", 1), ("TODO", 2)]);
        assert_eq!(
            debt.by_dir,
            vec![("src/a".to_owned(), 2), (".".to_owned(), 1)]
        );
        assert_eq!(debt.by_assignee, vec![("bob".to_owned(), 2)]);
        assert_eq!(debt.files_with_markers, 2);
        assert_eq!(debt.count("TODO"), 2);
        assert!((debt.density - 2.0).abs() < 1e-9);
    }
}
