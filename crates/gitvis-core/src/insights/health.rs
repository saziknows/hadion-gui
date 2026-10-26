//! Repository health: things that break a checkout on another machine, line
//! ending and attribute trouble, executable bits, repository weight and
//! submodule state.
//!
//! `analyze` is cheap (index listing plus a few `git config`/`count-objects`
//! calls); [`weight`] walks every object in history and runs on demand.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use super::{Progress, Severity, TrackedFile, dir_of, ext_of, git_ok, git_stdin, git_z};
use crate::secrets::basename;
use crate::{Error, Oid, Result, cli};

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

/// `git ls-files --eol` reads every index blob and every worktree file
/// (6 s warm for 32k files on Windows). Up to this many files it is the
/// exact source; above it the index side comes from two threaded
/// `git grep --cached` passes and the worktree side from the content probe.
const EOL_LISTING_MAX_FILES: usize = 3_000;
/// Above that, the files with CR line endings (one `git grep --cached`)
/// get the exact listing when there are at most this many; beyond it the
/// probe's worktree bytes tell CRLF from mixed.
const EOL_EXACT_MAX_PATHS: usize = 10_000;
/// Pathspec bytes per `ls-files --eol -- …` call (Windows command lines
/// hold 32 KB).
const PATHSPEC_CHUNK_BYTES: usize = 20_000;
/// Listing chunks run at once (a git process each).
const LISTING_THREADS: usize = 8;
/// Files whose content is read for the BOM / UTF-8 / shebang / LFS checks.
const MAX_PROBED_FILES: usize = 40_000;
/// Text files up to this size are read whole; anything else by its head.
const MAX_TEXT_BYTES: u64 = 2 * 1024 * 1024;
/// Head read for binaries and oversized files (NUL, shebang, LFS pointer).
const PROBE_HEAD_BYTES: usize = 8 * 1024;
/// A HEAD blob this big is listed in `WeightSummary::head_blobs`.
const HEAD_BLOB_MIN: u64 = 256 * 1024;
const MAX_HEAD_BLOBS: usize = 60;
const LFS_CANDIDATE_MIN: u64 = 1024 * 1024;
const MAX_LFS_CANDIDATES: usize = 40;
/// Git LFS pointer files are 120–140 bytes.
const LFS_POINTER_MAX: u64 = 200;
const LFS_POINTER_PREFIX: &[u8] = b"version https://git-lfs";
const MAX_RANKED: usize = 30;
const HUGE_DIR_ENTRIES: usize = 5_000;
const MAX_PATH_BYTES: usize = 260;
const MAX_IGNORED_LISTED: usize = 200;
const MAX_NESTED_LISTED: usize = 20;
const MAX_EXAMPLES: usize = 50;
/// A history blob this big is kept in `Weight::blobs`.
const HISTORY_BLOB_MIN: u64 = 512 * 1024;
const MAX_HISTORY_BLOBS: usize = 200;
const MAX_HISTORY_RANKED: usize = 40;
/// Objects between two progress reports / cancel checks of [`weight`].
const PROGRESS_EVERY: u64 = 5_000;

// ---------------------------------------------------------------------------
// Checkout landmines
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LandmineKind {
    /// Two tracked paths differ only by case: one of them is lost on
    /// macOS/Windows and `git status` shows a permanent modification.
    CaseCollision,
    /// `aux`, `con`, `nul`, `com1`…, with or without extension: cannot be
    /// created on Windows at all.
    WindowsReserved,
    /// Characters Windows forbids in file names: `< > : " | ? *` and
    /// control characters.
    InvalidChars,
    /// Trailing space or dot in a path component (Windows strips them).
    TrailingSpaceOrDot,
    /// Path longer than 260 bytes (Windows MAX_PATH without the registry
    /// opt-in / `core.longpaths`).
    LongPath,
    /// Two paths that are equal after Unicode NFC/NFD normalisation
    /// (macOS normalises, Linux does not).
    UnicodeNormalization,
    /// Symbolic links: Windows checks them out as text files unless
    /// Developer Mode and `core.symlinks` are on.
    Symlink,
    /// A tracked file that `.gitignore` also matches: edits are tracked,
    /// `git add .` style workflows get confusing.
    IgnoredTracked,
    /// A directory with more than 5,000 direct entries (slow on every
    /// file system, pathological on Windows).
    HugeDirectory,
    /// A tracked `.gitmodules`-less gitlink, or a nested repository folder
    /// committed as plain files (`foo/.git/config` tracked).
    NestedRepository,
    /// Empty file committed with the executable bit, or a zero-byte file
    /// named like a placeholder (`.gitkeep` is fine).
    EmptyExecutable,
}

impl LandmineKind {
    pub fn label(self) -> &'static str {
        match self {
            LandmineKind::CaseCollision => "Case collision",
            LandmineKind::WindowsReserved => "Windows reserved name",
            LandmineKind::InvalidChars => "Invalid characters",
            LandmineKind::TrailingSpaceOrDot => "Trailing space or dot",
            LandmineKind::LongPath => "Path longer than 260 bytes",
            LandmineKind::UnicodeNormalization => "Unicode normalisation clash",
            LandmineKind::Symlink => "Symbolic link",
            LandmineKind::IgnoredTracked => "Tracked but ignored",
            LandmineKind::HugeDirectory => "Huge directory",
            LandmineKind::NestedRepository => "Nested repository",
            LandmineKind::EmptyExecutable => "Empty executable",
        }
    }

    /// Why it matters, for the detail view.
    pub fn why(self) -> &'static str {
        match self {
            LandmineKind::CaseCollision => {
                "Case-insensitive file systems (macOS default, Windows) can hold only one of these paths. Clones there silently lose a file and show a permanent modification that cannot be committed away."
            }
            LandmineKind::WindowsReserved => {
                "Windows refuses to create files with these names, so `git clone` fails half-way on every Windows machine."
            }
            LandmineKind::InvalidChars => {
                "Windows cannot create these paths; the checkout stops with an error and the working tree is left incomplete."
            }
            LandmineKind::TrailingSpaceOrDot => {
                "Windows strips trailing spaces and dots from names, so the checked-out file never matches the index and shows as deleted or modified."
            }
            LandmineKind::LongPath => {
                "Windows limits paths to 260 characters unless long paths are enabled system-wide and in git (`core.longpaths`). Deep clones fail without it."
            }
            LandmineKind::UnicodeNormalization => {
                "macOS stores file names in a normalised Unicode form, Linux keeps them as written. Two names that normalise to the same string collide on macOS."
            }
            LandmineKind::Symlink => {
                "Windows checks symbolic links out as plain text files containing the target path unless Developer Mode and `core.symlinks=true` are set. Builds that follow the link break."
            }
            LandmineKind::IgnoredTracked => {
                "The file is tracked although .gitignore matches it: changes are versioned, but `git add` of the directory skips it and tooling that trusts .gitignore misses it."
            }
            LandmineKind::HugeDirectory => {
                "Directories with thousands of direct entries make every status, checkout and IDE refresh slow; split them or generate them at build time."
            }
            LandmineKind::NestedRepository => {
                "A repository committed as plain files (its .git folder tracked) or an unregistered gitlink confuses every clone: nested history is frozen and `git status` cannot explain the directory."
            }
            LandmineKind::EmptyExecutable => {
                "An empty file with the executable bit is usually a placeholder whose mode was copied by accident; it is harmless but noisy in mode-sensitive reviews."
            }
        }
    }

    /// Default severity of the kind.
    pub fn severity(self) -> Severity {
        match self {
            LandmineKind::CaseCollision
            | LandmineKind::WindowsReserved
            | LandmineKind::InvalidChars => Severity::High,
            LandmineKind::TrailingSpaceOrDot
            | LandmineKind::LongPath
            | LandmineKind::UnicodeNormalization
            | LandmineKind::IgnoredTracked
            | LandmineKind::NestedRepository => Severity::Medium,
            LandmineKind::Symlink | LandmineKind::HugeDirectory | LandmineKind::EmptyExecutable => {
                Severity::Low
            }
        }
    }

    /// Display order (the enum's declaration order).
    fn rank(self) -> u8 {
        match self {
            LandmineKind::CaseCollision => 0,
            LandmineKind::WindowsReserved => 1,
            LandmineKind::InvalidChars => 2,
            LandmineKind::TrailingSpaceOrDot => 3,
            LandmineKind::LongPath => 4,
            LandmineKind::UnicodeNormalization => 5,
            LandmineKind::Symlink => 6,
            LandmineKind::IgnoredTracked => 7,
            LandmineKind::HugeDirectory => 8,
            LandmineKind::NestedRepository => 9,
            LandmineKind::EmptyExecutable => 10,
        }
    }
}

/// One landmine and the paths it concerns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Landmine {
    pub kind: LandmineKind,
    pub severity: Severity,
    /// The paths involved (both sides of a collision, the offending file…).
    pub paths: Vec<String>,
    /// Short human explanation specific to these paths.
    pub detail: String,
}

impl Landmine {
    fn new(kind: LandmineKind, paths: Vec<String>, detail: impl Into<String>) -> Landmine {
        Landmine {
            kind,
            severity: kind.severity(),
            paths,
            detail: detail.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Line endings and attributes
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineEndings {
    /// Files whose index content is LF / CRLF / mixed (`git ls-files --eol`).
    pub lf: usize,
    pub crlf: usize,
    pub mixed: Vec<String>,
    /// Some CRLF example paths (first 50).
    pub crlf_examples: Vec<String>,
    /// Working-tree line endings differ from the index for these files
    /// (`i/lf w/crlf` and the like): the next commit flips every line.
    pub fighting: Vec<String>,
    /// Files with a UTF-8 BOM (checked for text files under 2 MB).
    pub bom: Vec<String>,
    /// Text files that are not valid UTF-8 (probably a legacy code page).
    pub non_utf8: Vec<String>,
    /// `core.autocrlf` as configured (`""` when unset).
    pub autocrlf: String,
    /// `core.eol` as configured.
    pub eol: String,
    /// A `.gitattributes` with `text=`/`eol=` rules exists.
    pub attributes_rules: bool,
    /// Paths of the `.gitattributes` files found.
    pub attributes_files: Vec<String>,
    /// Files whose `.gitattributes` say `-text`/`binary` but look like text,
    /// or `text` but contain NULs (first 50).
    pub misattributed: Vec<String>,
}

// ---------------------------------------------------------------------------
// Executable bits
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecBits {
    /// Scripts (shebang or `.sh`/`.py`/`.pl`/`.rb` with a shebang) without
    /// the executable bit.
    pub scripts_without: Vec<String>,
    /// Non-script files (sources, images, docs, data) with the executable bit.
    pub non_scripts_with: Vec<String>,
    /// `core.fileMode` (None when unset; false means modes are ignored here).
    pub file_mode: Option<bool>,
    pub executables: usize,
}

// ---------------------------------------------------------------------------
// Weight (cheap part)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BigBlob {
    pub path: String,
    pub size: u64,
    pub oid: Oid,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WeightSummary {
    /// `git count-objects -v` as `(key, value)` pairs, in order.
    pub count_objects: Vec<(String, String)>,
    pub loose_objects: u64,
    pub loose_bytes: u64,
    pub packs: u64,
    pub pack_bytes: u64,
    pub garbage: u64,
    /// Sum of HEAD blob sizes.
    pub head_bytes: u64,
    /// Largest HEAD blobs (≥ 256 KB, top 60, size desc).
    pub head_blobs: Vec<BigBlob>,
    /// HEAD bytes per top-level directory (desc, top 30).
    pub dirs_by_size: Vec<(String, u64)>,
    /// HEAD bytes per extension (desc, top 30): `(ext, files, bytes)`.
    pub ext_by_size: Vec<(String, usize, u64)>,
    /// Files that are LFS pointers (`version https://git-lfs…`).
    pub lfs_pointers: usize,
    /// Big binary files (≥ 1 MB) that are not LFS pointers.
    pub lfs_candidates: Vec<BigBlob>,
    /// `.git/objects/info/commit-graph` exists.
    pub commit_graph: bool,
    /// A multi-pack index exists.
    pub multi_pack_index: bool,
    pub shallow: bool,
    /// `remote.<name>.partialclonefilter`, when set.
    pub partial_clone: Option<String>,
    /// `.git/objects/info/alternates` exists.
    pub alternates: bool,
    /// Linked worktrees (`git worktree list`, excluding the main one).
    pub worktrees: Vec<String>,
    /// Loose refs vs packed refs: `(loose, packed)`.
    pub refs: (usize, usize),
    /// Advice such as "run git gc" / "enable commit-graph", one line each.
    pub advice: Vec<String>,
}

// ---------------------------------------------------------------------------
// Submodules
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submodule {
    pub path: String,
    pub url: String,
    /// `branch` from .gitmodules, if any.
    pub branch: Option<String>,
    /// Commit the superproject records (gitlink).
    pub recorded: Option<Oid>,
    /// Commit checked out inside the submodule, when initialised.
    pub checked_out: Option<Oid>,
    /// `ok`, `uninitialised`, `different commit`, `modified`, `missing
    /// commit` (recorded commit absent from the submodule), `not in
    /// .gitmodules` (a gitlink without an entry), `no gitlink` (an entry
    /// without a gitlink in the index).
    pub status: &'static str,
    pub detail: String,
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct Health {
    pub landmines: Vec<Landmine>,
    pub line_endings: LineEndings,
    pub exec_bits: ExecBits,
    pub weight: WeightSummary,
    pub submodules: Vec<Submodule>,
    pub files: usize,
    pub symlinks: usize,
    /// Highest severity among the landmines, if any.
    pub worst: Option<Severity>,
}

impl Health {
    /// Landmines of a kind.
    pub fn count(&self, kind: LandmineKind) -> usize {
        self.landmines.iter().filter(|l| l.kind == kind).count()
    }

    /// Everything that deserves attention: landmines + fighting line
    /// endings + misattributed + broken submodules.
    pub fn issues(&self) -> usize {
        self.landmines.len()
            + self.line_endings.fighting.len()
            + self.line_endings.mixed.len()
            + self.line_endings.misattributed.len()
            + self.submodules.iter().filter(|s| s.status != "ok").count()
    }
}

/// Health from the index listing and a few config/plumbing calls. Never
/// fails: unavailable parts stay empty.
pub fn analyze(workdir: &Path, files: &[TrackedFile], commands: &mut Vec<String>) -> Health {
    let mut health = Health {
        files: files.len(),
        symlinks: files.iter().filter(|f| f.is_symlink()).count(),
        ..Health::default()
    };

    // The attribute files and .gitmodules are tiny: read them up front.
    let attributes_files: Vec<String> = files
        .iter()
        .filter(|f| is_regular(f.mode) && basename(&f.path) == ".gitattributes")
        .map(|f| f.path.clone())
        .collect();
    let mut attributes_rules = false;
    let mut lfs_configured = false;
    for path in &attributes_files {
        if let Ok(text) = std::fs::read_to_string(workdir.join(path)) {
            let (rules, lfs) = attribute_flags(&text);
            attributes_rules |= rules;
            lfs_configured |= lfs;
        }
    }
    let gitmodules = if files.iter().any(|f| f.path == ".gitmodules") {
        std::fs::read_to_string(workdir.join(".gitmodules"))
            .map(|t| parse_gitmodules(&t))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let registered: HashSet<&str> = gitmodules.iter().map(|m| m.path.as_str()).collect();

    // The git calls are independent of each other and of the content
    // probe; on a 30k-file tree each takes about a second, so they run
    // side by side and the pure path checks fill the wait.
    let use_listing = files.len() <= EOL_LISTING_MAX_FILES;
    let (mut landmines, gathered) = std::thread::scope(|s| {
        let ignored = s.spawn(|| {
            let mut c = Vec::new();
            let v = git_z(
                workdir,
                &["ls-files", "-z", "-i", "-c", "--exclude-standard"],
                &mut c,
            )
            .unwrap_or_default();
            (v, c)
        });
        let listing = use_listing.then(|| {
            s.spawn(|| {
                let mut c = Vec::new();
                let v = git_z(workdir, &["ls-files", "--eol", "-z"], &mut c).unwrap_or_default();
                (v, c)
            })
        });
        let grep = (!use_listing).then(|| s.spawn(|| grep_eol(workdir, EOL_EXACT_MAX_PATHS)));
        let attrs = (!use_listing).then(|| s.spawn(|| check_attr(workdir, files)));
        let plumbing = s.spawn(|| Plumbing::read(workdir));
        let probes = s.spawn(|| probe_files(workdir, files));

        let landmines = path_landmines(files, &registered);

        let gathered = Gathered {
            ignored: joined(ignored),
            listing: listing.map(joined),
            grep: grep.map(joined),
            attrs: attrs.map(joined),
            plumbing: joined(plumbing),
            probes: joined(probes),
        };
        (landmines, gathered)
    });
    let Gathered {
        ignored: (ignored, ignored_cmds),
        listing,
        grep,
        attrs,
        plumbing: (plumbing, plumbing_cmds),
        probes,
    } = gathered;
    commands.extend(ignored_cmds);

    // Tracked-but-ignored files, then the final landmine order.
    if !ignored.is_empty() {
        let mut paths = ignored.clone();
        paths.truncate(MAX_IGNORED_LISTED);
        let detail = if ignored.len() > MAX_IGNORED_LISTED {
            format!(
                "{} tracked files are matched by .gitignore (first {} listed)",
                ignored.len(),
                MAX_IGNORED_LISTED
            )
        } else {
            format!("{} tracked files are matched by .gitignore", ignored.len())
        };
        landmines.push(Landmine::new(LandmineKind::IgnoredTracked, paths, detail));
    }
    landmines.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then(a.kind.rank().cmp(&b.kind.rank()))
            .then(a.paths.cmp(&b.paths))
    });
    health.worst = landmines.iter().map(|l| l.severity).max();
    health.landmines = landmines;

    // Line endings: rows aligned with `files`.
    let rows: Vec<Option<EolRow>> = match listing {
        Some((entries, cmds)) => {
            commands.extend(cmds);
            let index_of: HashMap<&str, usize> = files
                .iter()
                .enumerate()
                .map(|(i, f)| (f.path.as_str(), i))
                .collect();
            let mut rows = vec![None; files.len()];
            for entry in &entries {
                if let Some((path, row)) = parse_eol_entry(entry)
                    && let Some(&i) = index_of.get(path)
                {
                    rows[i] = Some(row);
                }
            }
            rows
        }
        None => {
            let (grep, c1) = grep.unwrap_or_default();
            let (attrs, c2) = attrs.unwrap_or_default();
            commands.extend(c1);
            commands.extend(c2);
            rows_from_grep(files, &probes, &grep, &attrs)
        }
    };
    commands.extend(plumbing_cmds);

    let autocrlf = plumbing.config("core.autocrlf");
    let mut le = LineEndings {
        autocrlf: autocrlf.clone(),
        eol: plumbing.config("core.eol"),
        attributes_rules,
        attributes_files,
        ..LineEndings::default()
    };
    for (i, f) in files.iter().enumerate() {
        let probe = probes[i];
        if let Some(row) = &rows[i] {
            match row.index {
                Eol::Lf => le.lf += 1,
                Eol::Crlf => {
                    le.crlf += 1;
                    if le.crlf_examples.len() < MAX_EXAMPLES {
                        le.crlf_examples.push(f.path.clone());
                    }
                }
                Eol::Mixed => le.mixed.push(f.path.clone()),
                _ => {}
            }
            if is_fighting(row.index, row.worktree, normalisation(&row.attr, &autocrlf)) {
                le.fighting.push(f.path.clone());
            }
            if let Some(p) = probe
                && le.misattributed.len() < MAX_EXAMPLES
            {
                let (declared_text, declared_binary) = declared_attrs(&row.attr);
                let text_like = p.read > 0 && p.utf8 && !p.nul;
                if (declared_binary && text_like) || (declared_text && p.nul) {
                    le.misattributed.push(f.path.clone());
                }
            }
        }
        if let Some(p) = probe
            && !p.nul
        {
            if p.bom {
                le.bom.push(f.path.clone());
            }
            if !p.utf8 {
                le.non_utf8.push(f.path.clone());
            }
        }
    }
    health.line_endings = le;

    // Executable bits.
    let mut eb = ExecBits {
        file_mode: parse_bool(&plumbing.config("core.filemode")),
        ..ExecBits::default()
    };
    for (i, f) in files.iter().enumerate() {
        if !is_regular(f.mode) {
            continue;
        }
        let ext = ext_of(&f.path);
        let shebang = probes[i].is_some_and(|p| p.shebang);
        if f.is_executable() {
            eb.executables += 1;
            if !shebang && NON_EXEC_EXTS.contains(&ext.as_str()) {
                eb.non_scripts_with.push(f.path.clone());
            }
        } else if shebang || SHELL_EXTS.contains(&ext.as_str()) {
            eb.scripts_without.push(f.path.clone());
        }
    }
    health.exec_bits = eb;

    health.weight = weight_summary(workdir, files, &probes, &plumbing, lfs_configured, commands);
    health.submodules = submodules(workdir, files, &gitmodules, &registered, commands);
    health
}

/// Results of the parallel section of [`analyze`]; every git-backed part
/// carries the commands it ran.
struct Gathered {
    ignored: (Vec<String>, Vec<String>),
    listing: Option<(Vec<String>, Vec<String>)>,
    grep: Option<(GrepEol, Vec<String>)>,
    attrs: Option<(HashMap<String, String>, Vec<String>)>,
    plumbing: (Plumbing, Vec<String>),
    probes: Vec<Option<Probe>>,
}

/// A thread's result, or the default when it panicked (never expected).
fn joined<T: Default>(handle: std::thread::ScopedJoinHandle<'_, T>) -> T {
    handle.join().unwrap_or_default()
}

fn is_regular(mode: u32) -> bool {
    mode & 0o170000 == 0o100000
}

// ---------------------------------------------------------------------------
// Path landmines (pure)
// ---------------------------------------------------------------------------

/// Everything that can be told from the paths and modes alone.
fn path_landmines(files: &[TrackedFile], registered: &HashSet<&str>) -> Vec<Landmine> {
    let mut out = Vec::new();

    // Every directory, deduplicated: once a prefix is known all of its
    // parents are too.
    let mut dirs: HashSet<&str> = HashSet::new();
    for f in files {
        let mut p = f.path.as_str();
        while let Some(i) = p.rfind('/') {
            p = &p[..i];
            if !dirs.insert(p) {
                break;
            }
        }
    }

    // Case collisions among files and directories alike (a file `Foo` and
    // a directory `foo` cannot coexist either). A group is reported where
    // the spellings start to differ: `Foo/A` vs `foo/a` is the `Foo`/`foo`
    // directory clash, not a file one.
    let mut groups: HashMap<String, Vec<(&str, bool)>> = HashMap::new();
    for f in files {
        groups
            .entry(f.path.to_lowercase())
            .or_default()
            .push((f.path.as_str(), false));
    }
    for d in &dirs {
        groups.entry(d.to_lowercase()).or_default().push((d, true));
    }
    let mut collisions: Vec<Vec<(&str, bool)>> = groups
        .into_values()
        .filter(|g| g.len() > 1)
        .filter(|g| {
            let parent = parent_of(g[0].0);
            g.iter().all(|(p, _)| parent_of(p) == parent)
        })
        .collect();
    collisions.iter_mut().for_each(|g| g.sort());
    collisions.sort();
    for group in collisions {
        let dirs_in = group.iter().filter(|(_, d)| *d).count();
        let paths: Vec<String> = group
            .iter()
            .map(|(p, d)| if *d { format!("{p}/") } else { (*p).to_owned() })
            .collect();
        let detail = match dirs_in {
            0 => format!("{} files differ only by case", paths.len()),
            n if n == paths.len() => format!("{} directories differ only by case", paths.len()),
            _ => "a file and a directory differ only by case".to_owned(),
        };
        out.push(Landmine::new(LandmineKind::CaseCollision, paths, detail));
    }

    // Per-path Windows checks.
    for f in files {
        let path = f.path.as_str();
        let mut reserved = None;
        let mut invalid: Vec<char> = Vec::new();
        let mut trailing = None;
        for comp in path.split('/') {
            if reserved.is_none() && is_windows_reserved(comp) {
                reserved = Some(comp);
            }
            for c in comp.chars() {
                if is_invalid_char(c) && !invalid.contains(&c) {
                    invalid.push(c);
                }
            }
            if trailing.is_none()
                && !comp.is_empty()
                && (comp.ends_with(' ') || comp.ends_with('.'))
            {
                trailing = Some(comp);
            }
        }
        if let Some(comp) = reserved {
            out.push(Landmine::new(
                LandmineKind::WindowsReserved,
                vec![path.to_owned()],
                format!("`{comp}` is a reserved device name on Windows"),
            ));
        }
        if !invalid.is_empty() {
            let list: Vec<String> = invalid.iter().map(|c| format!("{c:?}")).collect();
            out.push(Landmine::new(
                LandmineKind::InvalidChars,
                vec![path.to_owned()],
                format!("contains {}", list.join(", ")),
            ));
        }
        if let Some(comp) = trailing {
            let what = if comp.ends_with(' ') { "space" } else { "dot" };
            out.push(Landmine::new(
                LandmineKind::TrailingSpaceOrDot,
                vec![path.to_owned()],
                format!("`{comp}` ends with a {what}"),
            ));
        }
        if path.len() > MAX_PATH_BYTES {
            out.push(Landmine::new(
                LandmineKind::LongPath,
                vec![path.to_owned()],
                format!("{} bytes", path.len()),
            ));
        }
    }

    // Unicode normalisation clashes (heuristic, see `nfd_key`). Groups
    // that are plain case collisions are already reported above.
    let mut by_key: HashMap<String, Vec<&str>> = HashMap::new();
    for f in files.iter().filter(|f| !f.path.is_ascii()) {
        by_key.entry(nfd_key(&f.path)).or_default().push(&f.path);
    }
    let mut clashes: Vec<Vec<&str>> = by_key
        .into_values()
        .filter(|g| g.len() > 1)
        .filter(|g| {
            let first = g[0].to_lowercase();
            g.iter().any(|p| p.to_lowercase() != first)
        })
        .collect();
    clashes.iter_mut().for_each(|g| g.sort());
    clashes.sort();
    for group in clashes {
        out.push(Landmine::new(
            LandmineKind::UnicodeNormalization,
            group.iter().map(|p| (*p).to_owned()).collect(),
            format!(
                "{} paths are the same name in NFC and NFD form",
                group.len()
            ),
        ));
    }

    // Modes.
    for f in files {
        if f.is_symlink() {
            out.push(Landmine::new(
                LandmineKind::Symlink,
                vec![f.path.clone()],
                "symbolic link in the index (mode 120000)",
            ));
        } else if f.is_executable() && f.size == Some(0) {
            out.push(Landmine::new(
                LandmineKind::EmptyExecutable,
                vec![f.path.clone()],
                "zero bytes with mode 100755",
            ));
        } else if f.is_gitlink() && !registered.contains(f.path.as_str()) {
            out.push(Landmine::new(
                LandmineKind::NestedRepository,
                vec![f.path.clone()],
                format!(
                    "gitlink {} is not registered in .gitmodules",
                    short_oid(&f.oid)
                ),
            ));
        }
    }

    // A `.git` directory committed as plain files (git itself refuses to
    // add these today, so they come from old or crafted history).
    let mut nested: HashMap<&str, Vec<&str>> = HashMap::new();
    for f in files {
        if let Some(root) = nested_git_root(&f.path) {
            nested.entry(root).or_default().push(&f.path);
        }
    }
    let mut nested: Vec<(&str, Vec<&str>)> = nested.into_iter().collect();
    nested.sort();
    for (root, mut paths) in nested {
        paths.sort();
        let total = paths.len();
        paths.truncate(MAX_NESTED_LISTED);
        let shown = if root.is_empty() { ".git" } else { root };
        out.push(Landmine::new(
            LandmineKind::NestedRepository,
            paths.iter().map(|p| (*p).to_owned()).collect(),
            format!("{total} files under `{shown}/.git` are tracked"),
        ));
    }

    // Huge directories: direct files plus direct subdirectories.
    let mut children: HashMap<&str, (usize, usize)> = HashMap::new();
    for f in files {
        children.entry(parent_of(&f.path)).or_default().0 += 1;
    }
    for d in &dirs {
        children.entry(parent_of(d)).or_default().1 += 1;
    }
    let mut huge: Vec<(&str, usize, usize)> = children
        .into_iter()
        .filter(|(_, (files, dirs))| files + dirs > HUGE_DIR_ENTRIES)
        .map(|(d, (f, s))| (d, f, s))
        .collect();
    huge.sort();
    for (dir, nfiles, ndirs) in huge {
        let shown = if dir.is_empty() {
            ".".to_owned()
        } else {
            format!("{dir}/")
        };
        out.push(Landmine::new(
            LandmineKind::HugeDirectory,
            vec![shown],
            format!(
                "{} direct entries ({nfiles} files, {ndirs} subdirectories)",
                nfiles + ndirs
            ),
        ));
    }

    out
}

/// Directory part of a path (`""` at the root).
fn parent_of(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

/// `CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`, in any case,
/// with any extension (`aux.txt`), as Windows reserves them.
fn is_windows_reserved(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ');
    if !(3..=4).contains(&stem.len()) || !stem.is_ascii() {
        return false;
    }
    let upper = stem.to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        _ => {
            upper.len() == 4
                && (upper.starts_with("COM") || upper.starts_with("LPT"))
                && upper.as_bytes()[3].is_ascii_digit()
                && upper.as_bytes()[3] != b'0'
        }
    }
}

/// Characters Windows cannot put in a file name. Backslash is a separator
/// there, so a component containing one cannot be created either.
fn is_invalid_char(c: char) -> bool {
    matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\') || (c as u32) < 0x20
}

/// Root of a nested repository committed as files: `foo` for
/// `foo/.git/config`, `""` for `.git/config`; `None` otherwise.
fn nested_git_root(path: &str) -> Option<&str> {
    let mut offset = 0;
    for comp in path.split('/') {
        if comp.eq_ignore_ascii_case(".git") && offset + comp.len() < path.len() {
            return Some(&path[..offset.saturating_sub(1)]);
        }
        offset += comp.len() + 1;
    }
    None
}

/// Lower-case precomposed Latin letters and the (base, combining mark)
/// they decompose to. Enough for Western European and Central European
/// file names; the heuristic knows nothing about other scripts or
/// stacked marks, so `nfd_key` is an approximation of NFD.
const DECOMPOSE: &[(char, char, char)] = &[
    // grave
    ('à', 'a', '\u{300}'),
    ('è', 'e', '\u{300}'),
    ('ì', 'i', '\u{300}'),
    ('ò', 'o', '\u{300}'),
    ('ù', 'u', '\u{300}'),
    // acute
    ('á', 'a', '\u{301}'),
    ('é', 'e', '\u{301}'),
    ('í', 'i', '\u{301}'),
    ('ó', 'o', '\u{301}'),
    ('ú', 'u', '\u{301}'),
    ('ý', 'y', '\u{301}'),
    ('ć', 'c', '\u{301}'),
    ('ĺ', 'l', '\u{301}'),
    ('ń', 'n', '\u{301}'),
    ('ŕ', 'r', '\u{301}'),
    ('ś', 's', '\u{301}'),
    ('ź', 'z', '\u{301}'),
    // circumflex
    ('â', 'a', '\u{302}'),
    ('ê', 'e', '\u{302}'),
    ('î', 'i', '\u{302}'),
    ('ô', 'o', '\u{302}'),
    ('û', 'u', '\u{302}'),
    ('ĉ', 'c', '\u{302}'),
    ('ĝ', 'g', '\u{302}'),
    ('ĥ', 'h', '\u{302}'),
    ('ĵ', 'j', '\u{302}'),
    ('ŝ', 's', '\u{302}'),
    ('ŵ', 'w', '\u{302}'),
    ('ŷ', 'y', '\u{302}'),
    // tilde
    ('ã', 'a', '\u{303}'),
    ('ñ', 'n', '\u{303}'),
    ('õ', 'o', '\u{303}'),
    ('ĩ', 'i', '\u{303}'),
    ('ũ', 'u', '\u{303}'),
    // macron
    ('ā', 'a', '\u{304}'),
    ('ē', 'e', '\u{304}'),
    ('ī', 'i', '\u{304}'),
    ('ō', 'o', '\u{304}'),
    ('ū', 'u', '\u{304}'),
    // breve
    ('ă', 'a', '\u{306}'),
    ('ğ', 'g', '\u{306}'),
    ('ŭ', 'u', '\u{306}'),
    // dot above
    ('ċ', 'c', '\u{307}'),
    ('ė', 'e', '\u{307}'),
    ('ġ', 'g', '\u{307}'),
    ('ż', 'z', '\u{307}'),
    // diaeresis
    ('ä', 'a', '\u{308}'),
    ('ë', 'e', '\u{308}'),
    ('ï', 'i', '\u{308}'),
    ('ö', 'o', '\u{308}'),
    ('ü', 'u', '\u{308}'),
    ('ÿ', 'y', '\u{308}'),
    // ring above
    ('å', 'a', '\u{30a}'),
    ('ů', 'u', '\u{30a}'),
    // double acute
    ('ő', 'o', '\u{30b}'),
    ('ű', 'u', '\u{30b}'),
    // caron
    ('č', 'c', '\u{30c}'),
    ('ď', 'd', '\u{30c}'),
    ('ě', 'e', '\u{30c}'),
    ('ľ', 'l', '\u{30c}'),
    ('ň', 'n', '\u{30c}'),
    ('ř', 'r', '\u{30c}'),
    ('š', 's', '\u{30c}'),
    ('ť', 't', '\u{30c}'),
    ('ž', 'z', '\u{30c}'),
    // cedilla
    ('ç', 'c', '\u{327}'),
    ('ģ', 'g', '\u{327}'),
    ('ķ', 'k', '\u{327}'),
    ('ļ', 'l', '\u{327}'),
    ('ņ', 'n', '\u{327}'),
    ('ŗ', 'r', '\u{327}'),
    ('ş', 's', '\u{327}'),
    ('ţ', 't', '\u{327}'),
    // ogonek
    ('ą', 'a', '\u{328}'),
    ('ę', 'e', '\u{328}'),
    ('į', 'i', '\u{328}'),
    ('ų', 'u', '\u{328}'),
];

/// Case-folded, NFD-like key of a path: precomposed Latin letters become
/// base letter + combining mark, so `café` (NFC) and `cafe\u{301}` (NFD)
/// agree while `cafe` stays distinct. A heuristic, not full Unicode
/// normalisation (see [`DECOMPOSE`]).
fn nfd_key(path: &str) -> String {
    let mut key = String::with_capacity(path.len() + 8);
    for c in path.chars().flat_map(char::to_lowercase) {
        match DECOMPOSE.iter().find(|(pre, _, _)| *pre == c) {
            Some((_, base, mark)) => {
                key.push(*base);
                key.push(*mark);
            }
            None => key.push(c),
        }
    }
    key
}

fn short_oid(oid: &Oid) -> String {
    oid.to_string().chars().take(7).collect()
}

// ---------------------------------------------------------------------------
// Content probe
// ---------------------------------------------------------------------------

/// What one read of a working-tree file tells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Probe {
    /// Bytes read.
    read: usize,
    /// The whole file was read (text files under 2 MB).
    full: bool,
    bom: bool,
    /// The bytes read are valid UTF-8 (a sequence cut off by the read
    /// limit is tolerated).
    utf8: bool,
    /// NUL in the first 8 KB.
    nul: bool,
    shebang: bool,
    lfs_pointer: bool,
    lf: u32,
    crlf: u32,
}

impl Probe {
    fn from_bytes(buf: &[u8], full: bool) -> Probe {
        let head = &buf[..buf.len().min(PROBE_HEAD_BYTES)];
        let utf8 = match std::str::from_utf8(buf) {
            Ok(_) => true,
            Err(e) => !full && e.error_len().is_none(),
        };
        let (mut lf, mut crlf) = (0u32, 0u32);
        let mut prev_cr = false;
        for &b in buf {
            if b == b'\n' {
                if prev_cr {
                    crlf += 1;
                } else {
                    lf += 1;
                }
            }
            prev_cr = b == b'\r';
        }
        Probe {
            read: buf.len(),
            full,
            bom: buf.starts_with(&[0xEF, 0xBB, 0xBF]),
            utf8,
            nul: head.contains(&0),
            // `#!/…` or `#! /…`; Rust's `#![no_std]` is not a shebang.
            shebang: buf.starts_with(b"#!") && matches!(buf.get(2), Some(b'/') | Some(b' ')),
            lfs_pointer: buf.starts_with(LFS_POINTER_PREFIX),
            lf,
            crlf,
        }
    }

    fn worktree_eol(&self) -> Eol {
        if self.nul {
            return Eol::Binary;
        }
        match (self.lf > 0, self.crlf > 0) {
            (true, true) => Eol::Mixed,
            (true, false) => Eol::Lf,
            (false, true) => Eol::Crlf,
            (false, false) => Eol::None,
        }
    }
}

/// Read the regular files once, on all cores: text files whole (up to
/// 2 MB), binaries and oversized files by their first 8 KB. Returns one
/// entry per tracked file, `None` where nothing was read.
fn probe_files(workdir: &Path, files: &[TrackedFile]) -> Vec<Option<Probe>> {
    let mut order: Vec<usize> = (0..files.len())
        .filter(|&i| is_regular(files[i].mode))
        .collect();
    if order.len() > MAX_PROBED_FILES {
        // Keep the files a check cannot do without, then index order.
        order.sort_by_key(|&i| !must_probe(&files[i]));
        order.truncate(MAX_PROBED_FILES);
    }
    let next = AtomicUsize::new(0);
    // File opens dominate, so more threads than cores still help.
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 16)
        .min(order.len().max(1));
    let results: Vec<Vec<(usize, Probe)>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let (next, order) = (&next, &order);
                s.spawn(move || {
                    let mut out = Vec::new();
                    loop {
                        let k = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&i) = order.get(k) else {
                            break;
                        };
                        if let Some(p) = probe_one(workdir, &files[i]) {
                            out.push((i, p));
                        }
                    }
                    out
                })
            })
            .collect();
        handles.into_iter().map(joined).collect()
    });
    let mut probes = vec![None; files.len()];
    for (i, p) in results.into_iter().flatten() {
        probes[i] = Some(p);
    }
    probes
}

/// Files whose checks need the content: shebang for executables and
/// extension-less files, LFS pointers for tiny files, NULs for big ones.
fn must_probe(f: &TrackedFile) -> bool {
    let ext = ext_of(&f.path);
    f.is_executable()
        || ext.is_empty()
        || SHELL_EXTS.contains(&ext.as_str())
        || SCRIPT_EXTS.contains(&ext.as_str())
        || f.size
            .is_some_and(|s| !(LFS_POINTER_MAX..LFS_CANDIDATE_MIN).contains(&s))
}

fn probe_one(workdir: &Path, f: &TrackedFile) -> Option<Probe> {
    let mut file = File::open(workdir.join(&f.path)).ok()?;
    let size = match f.size {
        Some(s) => s,
        None => file.metadata().ok()?.len(),
    };
    let text = size < MAX_TEXT_BYTES && !is_binary_ext(&f.path);
    let limit = if text {
        MAX_TEXT_BYTES as usize
    } else {
        PROBE_HEAD_BYTES
    };
    let mut buf = Vec::with_capacity((size as usize).min(limit) + 1);
    (&mut file)
        .take(limit as u64 + 1)
        .read_to_end(&mut buf)
        .ok()?;
    let full = buf.len() <= limit;
    buf.truncate(limit);
    Some(Probe::from_bytes(&buf, full))
}

/// Extensions that are never text.
fn is_binary_ext(path: &str) -> bool {
    matches!(
        ext_of(path).as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "bmp"
            | "ico"
            | "icns"
            | "cur"
            | "webp"
            | "tif"
            | "tiff"
            | "psd"
            | "ai"
            | "heic"
            | "avif"
            | "xcf"
            | "mp3"
            | "wav"
            | "ogg"
            | "oga"
            | "flac"
            | "aac"
            | "m4a"
            | "wma"
            | "mp4"
            | "m4v"
            | "mov"
            | "avi"
            | "mkv"
            | "webm"
            | "wmv"
            | "ogv"
            | "zip"
            | "gz"
            | "tgz"
            | "bz2"
            | "xz"
            | "zst"
            | "7z"
            | "rar"
            | "tar"
            | "jar"
            | "war"
            | "lz4"
            | "br"
            | "cab"
            | "msi"
            | "deb"
            | "rpm"
            | "dmg"
            | "iso"
            | "img"
            | "pkg"
            | "apk"
            | "ipa"
            | "aab"
            | "crx"
            | "xpi"
            | "snap"
            | "exe"
            | "dll"
            | "so"
            | "dylib"
            | "a"
            | "o"
            | "obj"
            | "lib"
            | "pdb"
            | "bin"
            | "wasm"
            | "class"
            | "pyc"
            | "pyo"
            | "pyd"
            | "node"
            | "dex"
            | "elf"
            | "rlib"
            | "rmeta"
            | "mo"
            | "ttf"
            | "otf"
            | "ttc"
            | "woff"
            | "woff2"
            | "eot"
            | "pdf"
            | "doc"
            | "docx"
            | "xls"
            | "xlsx"
            | "ppt"
            | "pptx"
            | "odt"
            | "ods"
            | "odp"
            | "db"
            | "sqlite"
            | "sqlite3"
            | "parquet"
            | "arrow"
            | "feather"
            | "npy"
            | "npz"
            | "pkl"
            | "pickle"
            | "h5"
            | "hdf5"
            | "onnx"
            | "pb"
            | "pt"
            | "pth"
            | "safetensors"
            | "gguf"
            | "dat"
            | "pak"
            | "swf"
            | "nib"
            | "sig"
            | "gpg"
            | "pfx"
            | "p12"
            | "jks"
            | "keystore"
            | "der"
            | "ser"
            | "lockb"
    )
}

/// Shell scripts: flagged without the executable bit even without a shebang.
const SHELL_EXTS: &[&str] = &["sh", "bash", "zsh", "fish"];
/// Interpreted sources: flagged without the executable bit only when they
/// carry a shebang (modules are not meant to be run).
const SCRIPT_EXTS: &[&str] = &["py", "pl", "rb"];
/// Never executable (unless a shebang says otherwise).
const NON_EXEC_EXTS: &[&str] = &[
    "rs", "c", "h", "cpp", "hpp", "cc", "hh", "cxx", "hxx", "go", "java", "kt", "kts", "ts", "tsx",
    "js", "jsx", "mjs", "cjs", "json", "md", "txt", "yml", "yaml", "toml", "ini", "cfg", "png",
    "jpg", "jpeg", "gif", "svg", "ico", "webp", "pdf", "html", "htm", "css", "scss", "less", "xml",
    "csv", "lock", "sql", "proto", "gradle", "cs", "swift", "m", "mm", "dart", "scala", "lua",
    "vue", "svelte", "woff", "woff2", "ttf", "otf", "zip", "gz", "tar",
];

// ---------------------------------------------------------------------------
// Line endings
// ---------------------------------------------------------------------------

/// Line ending state of one side of a file (`git ls-files --eol` words).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Eol {
    Lf,
    Crlf,
    Mixed,
    /// No line ending at all.
    None,
    /// `-text`: binary content.
    Binary,
    #[default]
    Unknown,
}

impl Eol {
    fn parse(word: &str) -> Eol {
        match word {
            "lf" => Eol::Lf,
            "crlf" => Eol::Crlf,
            "mixed" => Eol::Mixed,
            "none" => Eol::None,
            "-text" => Eol::Binary,
            _ => Eol::Unknown,
        }
    }

    fn has_lines(self) -> bool {
        matches!(self, Eol::Lf | Eol::Crlf | Eol::Mixed)
    }
}

/// One file's line-ending facts: index side, worktree side, eol attributes
/// in the `attr/` column form (`text eol=lf`, `-text`, `text=auto`, `""`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct EolRow {
    index: Eol,
    worktree: Eol,
    attr: String,
}

/// Parse one `git ls-files --eol -z` entry:
/// `i/lf    w/crlf  attr/text eol=lf      \tpath`.
fn parse_eol_entry(entry: &str) -> Option<(&str, EolRow)> {
    let (meta, path) = entry.split_once('\t')?;
    let rest = meta.strip_prefix("i/")?;
    let (i_txt, rest) = split_word(rest);
    let rest = rest.trim_start().strip_prefix("w/")?;
    let (w_txt, rest) = split_word(rest);
    let attr = rest.trim_start().strip_prefix("attr/")?.trim();
    Some((
        path,
        EolRow {
            index: Eol::parse(i_txt),
            worktree: Eol::parse(w_txt),
            attr: attr.to_owned(),
        },
    ))
}

/// `("lf", "    w/crlf …")`; an empty word when the text starts with space.
fn split_word(s: &str) -> (&str, &str) {
    match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    }
}

/// `git grep --cached -l` paths with a line matching `pattern` in the
/// index. The raw CR in the pattern is recorded escaped so the activity log
/// stays printable.
fn grep_cached(workdir: &Path, pattern: &str) -> (HashSet<String>, Vec<String>) {
    let args = ["grep", "--cached", "-l", "-I", "-z", "-G", "-e", pattern];
    let commands = vec![format!(
        "git grep --cached -l -I -z -G -e '{}'",
        pattern.replace('\r', "\\r")
    )];
    // Exit code 1 means "no match": an empty set, not an error.
    let set = cli::run(workdir, &args, None)
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .split('\0')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    (set, commands)
}

/// `git check-attr -z --stdin text eol` for the regular files, rendered in
/// the `attr/` column form so one code path reads both sources.
fn check_attr(workdir: &Path, files: &[TrackedFile]) -> (HashMap<String, String>, Vec<String>) {
    let mut stdin = Vec::new();
    let mut n = 0usize;
    for f in files.iter().filter(|f| is_regular(f.mode)) {
        stdin.extend_from_slice(f.path.as_bytes());
        stdin.push(0);
        n += 1;
    }
    let mut commands = Vec::new();
    if n == 0 {
        return (HashMap::new(), commands);
    }
    let out = git_stdin(
        workdir,
        &["check-attr", "-z", "--stdin", "text", "eol"],
        &stdin,
        &format!("{n} paths"),
        &mut commands,
    )
    .unwrap_or_default();
    let text = String::from_utf8_lossy(&out);
    let mut raw: HashMap<&str, (&str, &str)> = HashMap::with_capacity(n);
    let mut it = text.split('\0');
    while let (Some(path), Some(attr), Some(value)) = (it.next(), it.next(), it.next()) {
        let entry = raw.entry(path).or_insert(("unspecified", "unspecified"));
        match attr {
            "text" => entry.0 = value,
            "eol" => entry.1 = value,
            _ => {}
        }
    }
    let map = raw
        .into_iter()
        .map(|(path, (text, eol))| {
            let mut s = match text {
                "set" => "text".to_owned(),
                "unset" => "-text".to_owned(),
                "unspecified" => String::new(),
                other => format!("text={other}"),
            };
            if eol != "unspecified" {
                if !s.is_empty() {
                    s.push(' ');
                }
                s.push_str("eol=");
                s.push_str(eol);
            }
            (path.to_owned(), s)
        })
        .collect();
    (map, commands)
}

/// Index-side line endings of a large repository.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct GrepEol {
    /// Files with CR-terminated lines in the index.
    crlf: HashSet<String>,
    /// Exact `--eol` rows for the CRLF files, when there were few enough.
    exact: HashMap<String, EolRow>,
}

/// One grep for the files with CR line endings, then the exact listing
/// restricted to them (usually a handful; chunked and parallel up to
/// `max_exact` paths). A grep cannot tell `crlf` from `mixed`: a final line
/// without a newline looks like a bare-LF line and an empty line matches
/// nothing, so the exact listing is the only reliable source.
fn grep_eol(workdir: &Path, max_exact: usize) -> (GrepEol, Vec<String>) {
    let (crlf, mut commands) = grep_cached(workdir, "\r$");
    let mut out = GrepEol {
        crlf,
        exact: HashMap::new(),
    };
    if !out.crlf.is_empty() && out.crlf.len() <= max_exact {
        let mut paths: Vec<&str> = out.crlf.iter().map(String::as_str).collect();
        paths.sort_unstable();
        for entry in eol_listing_for(workdir, &paths, &mut commands) {
            if let Some((path, row)) = parse_eol_entry(&entry) {
                out.exact.insert(path.to_owned(), row);
            }
        }
    }
    (out, commands)
}

/// `git ls-files --eol -z -- paths…` in command-line-sized chunks, several
/// at a time; recorded once with the counts instead of the paths.
fn eol_listing_for(workdir: &Path, paths: &[&str], commands: &mut Vec<String>) -> Vec<String> {
    let mut chunks: Vec<&[&str]> = Vec::new();
    let mut start = 0;
    while start < paths.len() {
        let mut end = start;
        let mut bytes = 0;
        while end < paths.len() && (end == start || bytes + paths[end].len() < PATHSPEC_CHUNK_BYTES)
        {
            bytes += paths[end].len() + 3;
            end += 1;
        }
        chunks.push(&paths[start..end]);
        start = end;
    }
    commands.push(format!(
        "git ls-files --eol -z -- …   # {} paths in {} chunks",
        paths.len(),
        chunks.len()
    ));
    let next = AtomicUsize::new(0);
    let threads = LISTING_THREADS.min(chunks.len().max(1));
    let per_thread: Vec<Vec<String>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let (next, chunks) = (&next, &chunks);
                s.spawn(move || {
                    let mut entries = Vec::new();
                    loop {
                        let k = next.fetch_add(1, Ordering::Relaxed);
                        let Some(chunk) = chunks.get(k) else {
                            break;
                        };
                        let mut args = vec!["ls-files", "--eol", "-z", "--"];
                        args.extend_from_slice(chunk);
                        if let Ok(out) = cli::run(workdir, &args, None) {
                            entries.extend(
                                String::from_utf8_lossy(&out.stdout)
                                    .split('\0')
                                    .filter(|s| !s.is_empty())
                                    .map(str::to_owned),
                            );
                        }
                    }
                    entries
                })
            })
            .collect();
        handles.into_iter().map(joined).collect()
    });
    per_thread.into_iter().flatten().collect()
}

/// Rows for a large repository: exact rows where they exist, otherwise the
/// index side from the grep set and the probe, the worktree side from the
/// probe, attributes from `check-attr`.
fn rows_from_grep(
    files: &[TrackedFile],
    probes: &[Option<Probe>],
    grep: &GrepEol,
    attrs: &HashMap<String, String>,
) -> Vec<Option<EolRow>> {
    files
        .iter()
        .enumerate()
        .map(|(i, f)| {
            if !is_regular(f.mode) {
                return None;
            }
            if let Some(row) = grep.exact.get(&f.path) {
                return Some(row.clone());
            }
            let probe = probes[i];
            let index = if grep.crlf.contains(&f.path) {
                // Too many CRLF files for the exact listing: an unmodified
                // worktree copy has the index's mix of endings.
                match probe.map(|p| p.worktree_eol()) {
                    Some(Eol::Mixed) => Eol::Mixed,
                    _ => Eol::Crlf,
                }
            } else {
                match probe {
                    Some(p) if p.nul => Eol::Binary,
                    // Not CRLF in the index, newlines in the worktree: LF.
                    Some(p) if p.lf + p.crlf > 0 => Eol::Lf,
                    Some(_) => Eol::None,
                    None => Eol::Unknown,
                }
            };
            Some(EolRow {
                index,
                worktree: probe.map_or(Eol::Unknown, |p| p.worktree_eol()),
                attr: attrs.get(&f.path).cloned().unwrap_or_default(),
            })
        })
        .collect()
}

/// How `git add` would convert a file's line endings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Normalise {
    /// `text` or `eol=` attribute: CRLF always becomes LF.
    Always,
    /// `text=auto`, or `core.autocrlf=true|input` without an attribute: CRLF
    /// becomes LF unless the index blob already has CRLF (git's safety
    /// against renormalising noise).
    Auto,
    Never,
}

fn normalisation(attr: &str, autocrlf: &str) -> Normalise {
    let mut words = attr.split_whitespace();
    if words.clone().any(|w| w == "-text") {
        return Normalise::Never;
    }
    if words.clone().any(|w| w == "text=auto") {
        return Normalise::Auto;
    }
    if words.any(|w| w == "text" || w.starts_with("eol=")) {
        return Normalise::Always;
    }
    match autocrlf {
        "true" | "input" => Normalise::Auto,
        _ => Normalise::Never,
    }
}

/// The next `git add` would store different line endings than the index
/// has: every line of the file flips in the next commit.
fn is_fighting(index: Eol, worktree: Eol, normalise: Normalise) -> bool {
    if !index.has_lines() || !worktree.has_lines() {
        return false;
    }
    let after_add = match normalise {
        Normalise::Always => Eol::Lf,
        Normalise::Auto if !matches!(index, Eol::Crlf | Eol::Mixed) => Eol::Lf,
        _ => worktree,
    };
    after_add != index
}

/// `(declared text, declared binary)` from the `attr/` column. `text=auto`
/// leaves the decision to git, so it declares neither.
fn declared_attrs(attr: &str) -> (bool, bool) {
    let mut text = false;
    let mut binary = false;
    for w in attr.split_whitespace() {
        match w {
            "-text" => binary = true,
            "text" => text = true,
            _ if w.starts_with("eol=") => text = true,
            _ => {}
        }
    }
    (text, binary)
}

/// `(has text/eol rules, configures LFS)` of a `.gitattributes` file.
fn attribute_flags(text: &str) -> (bool, bool) {
    let mut rules = false;
    let mut lfs = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        for w in line.split_whitespace().skip(1) {
            let w = w.trim_start_matches(['-', '!']);
            if w == "text" || w.starts_with("text=") || w.starts_with("eol=") || w == "binary" {
                rules = true;
            }
            if w == "filter=lfs" {
                lfs = true;
            }
        }
    }
    (rules, lfs)
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Plumbing: config, count-objects, git dir, worktrees
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Plumbing {
    /// Lower-cased keys of the few config values the report reads.
    config: HashMap<String, String>,
    count_objects: Vec<(String, String)>,
    git_dir: PathBuf,
    common_dir: PathBuf,
    shallow: bool,
    worktrees: Vec<String>,
}

impl Plumbing {
    fn read(workdir: &Path) -> (Plumbing, Vec<String>) {
        let mut c = Vec::new();
        let mut p = Plumbing::default();
        // One targeted query instead of `config --list`, which could carry
        // credentials embedded in remote URLs.
        let out = git_ok(
            workdir,
            &[
                "config",
                "--get-regexp",
                r"^(core\.(autocrlf|eol|filemode)|remote\..*\.partialclonefilter)$",
            ],
            &mut c,
        );
        for line in out.lines() {
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            p.config
                .insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
        p.count_objects = parse_count_objects(&git_ok(workdir, &["count-objects", "-v"], &mut c));
        let out = git_ok(
            workdir,
            &[
                "rev-parse",
                "--git-dir",
                "--git-common-dir",
                "--is-shallow-repository",
            ],
            &mut c,
        );
        let mut lines = out.lines().map(str::trim);
        p.git_dir = workdir.join(lines.next().unwrap_or(".git"));
        p.common_dir = workdir.join(lines.next().unwrap_or(".git"));
        p.shallow = lines.next() == Some("true");
        let out = git_ok(workdir, &["worktree", "list", "--porcelain"], &mut c);
        // Blank lines separate the worktrees; the first one is the main one.
        p.worktrees = out
            .split("\n\n")
            .skip(1)
            .filter_map(|block| {
                block
                    .lines()
                    .find_map(|l| l.strip_prefix("worktree ").map(|w| w.trim().to_owned()))
            })
            .collect();
        (p, c)
    }

    fn config(&self, key: &str) -> String {
        self.config.get(key).cloned().unwrap_or_default()
    }

    fn count(&self, key: &str) -> u64 {
        self.count_objects
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(0)
    }
}

/// `count: 6\nsize: 20\n…` → ordered pairs.
fn parse_count_objects(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect()
}

// ---------------------------------------------------------------------------
// Weight summary
// ---------------------------------------------------------------------------

fn weight_summary(
    workdir: &Path,
    files: &[TrackedFile],
    probes: &[Option<Probe>],
    plumbing: &Plumbing,
    lfs_configured: bool,
    commands: &mut Vec<String>,
) -> WeightSummary {
    let mut w = WeightSummary {
        count_objects: plumbing.count_objects.clone(),
        loose_objects: plumbing.count("count"),
        loose_bytes: plumbing.count("size") * 1024,
        packs: plumbing.count("packs"),
        pack_bytes: plumbing.count("size-pack") * 1024,
        garbage: plumbing.count("garbage"),
        shallow: plumbing.shallow,
        partial_clone: plumbing
            .config
            .iter()
            .find(|(k, _)| k.starts_with("remote.") && k.ends_with(".partialclonefilter"))
            .map(|(_, v)| v.clone()),
        worktrees: plumbing.worktrees.clone(),
        ..WeightSummary::default()
    };

    // HEAD content.
    let mut dirs: HashMap<String, u64> = HashMap::new();
    let mut exts: HashMap<String, (usize, u64)> = HashMap::new();
    let mut unconfirmed_small: Vec<usize> = Vec::new();
    for (i, f) in files.iter().enumerate() {
        if !is_regular(f.mode) {
            continue;
        }
        let Some(size) = f.size else {
            continue;
        };
        w.head_bytes += size;
        *dirs.entry(dir_of(&f.path, 1)).or_default() += size;
        let e = exts.entry(ext_of(&f.path)).or_default();
        e.0 += 1;
        e.1 += size;
        if size >= HEAD_BLOB_MIN {
            w.head_blobs.push(BigBlob {
                path: f.path.clone(),
                size,
                oid: f.oid,
            });
        }
        let probe = probes[i];
        if size < LFS_POINTER_MAX {
            if probe.is_some_and(|p| p.lfs_pointer) {
                w.lfs_pointers += 1;
            } else if size >= 100 {
                unconfirmed_small.push(i);
            }
        } else if size >= LFS_CANDIDATE_MIN
            && (is_binary_ext(&f.path) || probe.is_some_and(|p| p.nul))
        {
            w.lfs_candidates.push(BigBlob {
                path: f.path.clone(),
                size,
                oid: f.oid,
            });
        }
    }
    // With the LFS filter installed the worktree holds the real content and
    // only the HEAD blob is the pointer: ask git for those small blobs.
    if lfs_configured && !unconfirmed_small.is_empty() {
        let stdin: String = unconfirmed_small
            .iter()
            .map(|&i| format!("{}\n", files[i].oid))
            .collect();
        if let Ok(out) = git_stdin(
            workdir,
            &["cat-file", "--batch"],
            stdin.as_bytes(),
            &format!("{} small blobs: LFS pointer check", unconfirmed_small.len()),
            commands,
        ) {
            w.lfs_pointers += count_lfs_pointers(&out);
        }
    }
    w.head_blobs
        .sort_by(|a, b| b.size.cmp(&a.size).then(a.path.cmp(&b.path)));
    w.head_blobs.truncate(MAX_HEAD_BLOBS);
    w.lfs_candidates
        .sort_by(|a, b| b.size.cmp(&a.size).then(a.path.cmp(&b.path)));
    w.lfs_candidates.truncate(MAX_LFS_CANDIDATES);
    w.dirs_by_size = ranked(dirs.into_iter().collect(), MAX_RANKED);
    let mut exts: Vec<(String, usize, u64)> =
        exts.into_iter().map(|(e, (n, b))| (e, n, b)).collect();
    exts.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    exts.truncate(MAX_RANKED);
    w.ext_by_size = exts;

    // Repository layout.
    let objects = plumbing.common_dir.join("objects");
    w.commit_graph = objects.join("info").join("commit-graph").exists()
        || objects
            .join("info")
            .join("commit-graphs")
            .join("commit-graph-chain")
            .exists();
    w.multi_pack_index = objects.join("pack").join("multi-pack-index").exists();
    w.alternates = objects.join("info").join("alternates").exists();
    w.shallow |= plumbing.git_dir.join("shallow").exists();
    w.refs = (
        count_files(&plumbing.common_dir.join("refs")),
        std::fs::read_to_string(plumbing.common_dir.join("packed-refs"))
            .map(|t| t.lines().filter(|l| is_packed_ref_line(l)).count())
            .unwrap_or(0),
    );

    // Advice.
    if w.loose_objects > 5_000 {
        w.advice.push(format!(
            "Run `git gc`: {} loose objects ({})",
            w.loose_objects,
            mb(w.loose_bytes)
        ));
    }
    if w.packs > 20 {
        w.advice.push(format!(
            "{} pack files: `git gc` or `git repack -d` would consolidate them",
            w.packs
        ));
    }
    if !w.commit_graph && plumbing.count("in-pack") > 50_000 {
        w.advice.push(
            "Enable the commit-graph: `git config core.commitGraph true && git commit-graph write --reachable`"
                .to_owned(),
        );
    }
    if !w.lfs_candidates.is_empty() {
        let bytes: u64 = w.lfs_candidates.iter().map(|b| b.size).sum();
        w.advice.push(format!(
            "{} large binaries ({}) are stored as plain blobs: consider Git LFS",
            w.lfs_candidates.len(),
            mb(bytes)
        ));
    }
    if w.garbage > 0 {
        w.advice.push(format!(
            "{} garbage files in .git/objects ({}): `git gc` removes them",
            w.garbage,
            mb(plumbing.count("size-garbage") * 1024)
        ));
    }
    if w.refs.0 > 1_000 {
        w.advice.push(format!(
            "Run `git pack-refs --all`: {} loose refs",
            w.refs.0
        ));
    }
    w
}

/// Pairs sorted by value desc (name asc on ties), cut to `n`.
fn ranked(mut pairs: Vec<(String, u64)>, n: usize) -> Vec<(String, u64)> {
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    pairs.truncate(n);
    pairs
}

/// Pointers among `git cat-file --batch` records (`<oid> blob <size>\n`
/// followed by the content and a newline).
fn count_lfs_pointers(out: &[u8]) -> usize {
    let mut n = 0;
    let mut pos = 0;
    while pos < out.len() {
        let Some(nl) = out[pos..].iter().position(|&b| b == b'\n') else {
            break;
        };
        let header = String::from_utf8_lossy(&out[pos..pos + nl]);
        pos += nl + 1;
        let mut words = header.split_whitespace();
        let (Some(_oid), Some(kind), Some(size)) = (words.next(), words.next(), words.next())
        else {
            continue;
        };
        if kind == "missing" {
            continue;
        }
        let size: usize = size.parse().unwrap_or(0);
        let end = (pos + size).min(out.len());
        if kind == "blob" && out[pos..end].starts_with(LFS_POINTER_PREFIX) {
            n += 1;
        }
        pos = end + 1;
    }
    n
}

/// Files under `dir`, recursively (loose refs).
fn count_files(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => count_files(&e.path()),
            Ok(t) if t.is_file() => 1,
            _ => 0,
        })
        .sum()
}

/// `<40 hex> refs/…` (not `# pack-refs with:` nor `^<peeled>` lines).
fn is_packed_ref_line(line: &str) -> bool {
    let hex = line.split(' ').next().unwrap_or("");
    hex.len() >= 40 && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

// ---------------------------------------------------------------------------
// Submodules
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct GitmoduleEntry {
    name: String,
    path: String,
    url: String,
    branch: Option<String>,
}

/// `[submodule "name"]` sections with `path`, `url`, `branch`.
fn parse_gitmodules(text: &str) -> Vec<GitmoduleEntry> {
    let mut out: Vec<GitmoduleEntry> = Vec::new();
    let mut current: Option<GitmoduleEntry> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            if let Some(e) = current.take()
                && !e.path.is_empty()
            {
                out.push(e);
            }
            let rest = rest.trim_end_matches(']').trim();
            if let Some(name) = rest.strip_prefix("submodule") {
                let name = name.trim().trim_matches('"').to_owned();
                current = Some(GitmoduleEntry {
                    name,
                    ..GitmoduleEntry::default()
                });
            }
            continue;
        }
        let (Some(entry), Some((key, value))) = (current.as_mut(), line.split_once('=')) else {
            continue;
        };
        let value = value.trim().trim_matches('"').to_owned();
        match key.trim().to_ascii_lowercase().as_str() {
            "path" => entry.path = value,
            "url" => entry.url = value,
            "branch" => entry.branch = Some(value),
            _ => {}
        }
    }
    if let Some(e) = current
        && !e.path.is_empty()
    {
        out.push(e);
    }
    out
}

fn submodules(
    workdir: &Path,
    files: &[TrackedFile],
    gitmodules: &[GitmoduleEntry],
    registered: &HashSet<&str>,
    commands: &mut Vec<String>,
) -> Vec<Submodule> {
    let gitlinks: HashMap<&str, Oid> = files
        .iter()
        .filter(|f| f.is_gitlink())
        .map(|f| (f.path.as_str(), f.oid))
        .collect();
    let mut out: Vec<Submodule> = gitmodules
        .iter()
        .map(|m| inspect_submodule(workdir, m, gitlinks.get(m.path.as_str()).copied(), commands))
        .collect();
    for (path, oid) in gitlinks {
        if !registered.contains(path) {
            out.push(Submodule {
                path: path.to_owned(),
                url: String::new(),
                branch: None,
                recorded: Some(oid),
                checked_out: None,
                status: "not in .gitmodules",
                detail: format!(
                    "gitlink {} has no .gitmodules entry: clones cannot fetch it",
                    short_oid(&oid)
                ),
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn inspect_submodule(
    workdir: &Path,
    m: &GitmoduleEntry,
    recorded: Option<Oid>,
    commands: &mut Vec<String>,
) -> Submodule {
    let mut sub = Submodule {
        path: m.path.clone(),
        url: m.url.clone(),
        branch: m.branch.clone(),
        recorded,
        checked_out: None,
        status: "ok",
        detail: String::new(),
    };
    let Some(recorded) = recorded else {
        sub.status = "no gitlink";
        sub.detail = "listed in .gitmodules but not in the index".to_owned();
        return sub;
    };
    let dir = workdir.join(&m.path);
    // `.git` is a file (`gitdir: …`) for submodules checked out by git.
    if !dir.join(".git").exists() {
        sub.status = "uninitialised";
        sub.detail = "not checked out: `git submodule update --init`".to_owned();
        return sub;
    }
    let mut run = |args: &[&str]| -> Option<String> {
        commands.push(format!("git -C {} {}", m.path, args.join(" ")));
        cli::run(&dir, args, None)
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
    };
    let head = run(&["rev-parse", "HEAD"]).and_then(|s| Oid::from_str(&s).ok());
    sub.checked_out = head;
    let Some(head) = head else {
        sub.status = "uninitialised";
        sub.detail = "no HEAD inside the submodule".to_owned();
        return sub;
    };
    let recorded_hex = recorded.to_string();
    if run(&["cat-file", "-e", &recorded_hex]).is_none() {
        sub.status = "missing commit";
        sub.detail = format!(
            "recorded {} is not in the submodule: fetch it or update the gitlink",
            short_oid(&recorded)
        );
    } else if head != recorded {
        sub.status = "different commit";
        sub.detail = format!(
            "{} checked out, {} recorded",
            short_oid(&head),
            short_oid(&recorded)
        );
    } else if run(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty()) {
        sub.status = "modified";
        sub.detail = "uncommitted changes inside the submodule".to_owned();
    }
    sub
}

// ---------------------------------------------------------------------------
// Weight (history walk, on demand)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryBlob {
    pub oid: Oid,
    pub size: u64,
    pub disk_size: u64,
    /// Last known path (from `rev-list --objects`).
    pub path: String,
    /// Still present at this path in HEAD.
    pub in_head: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Weight {
    /// Largest blobs in all of history (≥ 512 KB, top 200, size desc).
    pub blobs: Vec<HistoryBlob>,
    pub total_blobs: u64,
    pub total_bytes: u64,
    pub total_disk: u64,
    pub commits: u64,
    pub trees: u64,
    /// `(ext, blobs, bytes)` desc by bytes, top 40.
    pub by_ext: Vec<(String, u64, u64)>,
    /// `(top-level dir, bytes)` desc, top 40.
    pub by_dir: Vec<(String, u64)>,
    /// Bytes of big blobs whose path is no longer in HEAD ("dead weight").
    pub deleted_bytes: u64,
    pub elapsed_ms: u64,
    pub cancelled: bool,
    pub commands: Vec<String>,
}

/// Walk every object reachable from any ref (`git rev-list --objects --all`
/// piped through `git cat-file --batch-check`) and aggregate sizes.
/// Streams both commands; checks `cancel` and reports progress every few
/// thousand objects.
pub fn weight(
    workdir: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<Weight> {
    let started = Instant::now();
    let mut w = Weight::default();
    let rev_args = ["rev-list", "--objects", "--all"];
    // `%(rest)` echoes the path rev-list printed after each oid, so no
    // oid → path map is needed (millions of entries on big repositories).
    let cat_args = [
        "cat-file",
        "--buffer",
        "--batch-check=%(objecttype) %(objectname) %(objectsize) %(objectsize:disk) %(rest)",
    ];
    w.commands.push(format!("git {}", rev_args.join(" ")));
    w.commands.push(format!(
        "git {}   # stdin: the rev-list output",
        cat_args.join(" ")
    ));

    let mut rev = cli::spawn(workdir, &rev_args)?;
    let mut cat = match spawn_with_stdin(workdir, &cat_args) {
        Ok(c) => c,
        Err(e) => {
            let _ = rev.kill();
            let _ = rev.wait();
            return Err(e);
        }
    };
    let (Some(rev_out), Some(cat_in), Some(cat_out)) =
        (rev.stdout.take(), cat.stdin.take(), cat.stdout.take())
    else {
        let _ = rev.kill();
        let _ = cat.kill();
        return Err(Error::new("could not open the git pipes"));
    };
    // The pump copies rev-list into cat-file; closing cat-file's stdin at
    // the end is what makes it exit.
    let pump = std::thread::spawn(move || {
        let mut from = BufReader::with_capacity(1 << 20, rev_out);
        let mut to = BufWriter::with_capacity(1 << 20, cat_in);
        let _ = std::io::copy(&mut from, &mut to);
        let _ = to.flush();
    });

    let mut reader = BufReader::with_capacity(1 << 20, cat_out);
    let mut line: Vec<u8> = Vec::new();
    let mut agg = Aggregate::default();
    let mut prog = Progress {
        phase: "Walking objects".to_owned(),
        ..Progress::default()
    };
    let mut seen = 0u64;
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        agg.add(&line);
        seen += 1;
        if seen.is_multiple_of(PROGRESS_EVERY) {
            if cancel.load(Ordering::Relaxed) {
                w.cancelled = true;
                let _ = rev.kill();
                let _ = cat.kill();
                break;
            }
            prog.done = seen as usize;
            prog.bytes = agg.bytes;
            progress(&prog);
        }
    }
    let rev_status = rev.wait();
    let _ = cat.wait();
    let _ = pump.join();
    if seen == 0 && rev_status.map(|s| !s.success()).unwrap_or(true) {
        return Err(Error::new("git rev-list --objects --all failed"));
    }

    agg.trim_big(MAX_HISTORY_BLOBS);
    w.blobs = agg.big;
    if !w.blobs.is_empty() {
        let in_head: HashSet<String> = git_z(
            workdir,
            &["ls-tree", "-r", "--name-only", "-z", "HEAD"],
            &mut w.commands,
        )
        .unwrap_or_default()
        .into_iter()
        .collect();
        for b in &mut w.blobs {
            b.in_head = in_head.contains(&b.path);
            if !b.in_head {
                w.deleted_bytes += b.size;
            }
        }
    }
    w.total_blobs = agg.blobs;
    w.total_bytes = agg.bytes;
    w.total_disk = agg.disk;
    w.commits = agg.commits;
    w.trees = agg.trees;
    let mut by_ext: Vec<(String, u64, u64)> = agg
        .by_ext
        .into_iter()
        .map(|(e, (n, b))| (e, n, b))
        .collect();
    by_ext.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    by_ext.truncate(MAX_HISTORY_RANKED);
    w.by_ext = by_ext;
    w.by_dir = ranked(agg.by_dir.into_iter().collect(), MAX_HISTORY_RANKED);
    w.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(w)
}

/// Like `cli::spawn`, with stdin open: that helper closes it so a git that
/// wants a terminal never waits, but the batch reader is fed through it.
fn spawn_with_stdin(workdir: &Path, args: &[&str]) -> Result<std::process::Child> {
    let mut cmd = Command::new("git");
    cmd.current_dir(workdir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn().map_err(|e| {
        Error::new(format!(
            "Could not start git ({e}). Is it installed and on PATH?"
        ))
    })
}

/// Running totals of the object walk.
#[derive(Default)]
struct Aggregate {
    commits: u64,
    trees: u64,
    blobs: u64,
    bytes: u64,
    disk: u64,
    by_ext: HashMap<String, (u64, u64)>,
    by_dir: HashMap<String, u64>,
    big: Vec<HistoryBlob>,
}

impl Aggregate {
    /// One `<type> <oid> <size> <disk> <path>` line.
    fn add(&mut self, line: &[u8]) {
        let text = String::from_utf8_lossy(line);
        let text = text.trim_end_matches(['\r', '\n']);
        let mut parts = text.splitn(5, ' ');
        let (Some(kind), Some(oid), Some(size), Some(disk)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return;
        };
        let path = parts.next().unwrap_or("");
        match kind {
            "commit" => self.commits += 1,
            "tree" => self.trees += 1,
            "blob" => {
                let size: u64 = size.parse().unwrap_or(0);
                let disk: u64 = disk.parse().unwrap_or(0);
                self.blobs += 1;
                self.bytes += size;
                self.disk += disk;
                // Borrowed keys on the hot path: millions of blobs.
                let ext = ext_slice(path);
                match self.by_ext.get_mut(ext.as_ref()) {
                    Some(e) => {
                        e.0 += 1;
                        e.1 += size;
                    }
                    None => {
                        self.by_ext.insert(ext.into_owned(), (1, size));
                    }
                }
                let dir = path.split_once('/').map_or(".", |(d, _)| d);
                match self.by_dir.get_mut(dir) {
                    Some(b) => *b += size,
                    None => {
                        self.by_dir.insert(dir.to_owned(), size);
                    }
                }
                if size >= HISTORY_BLOB_MIN {
                    self.big.push(HistoryBlob {
                        oid: Oid::from_str(oid).unwrap_or(Oid::ZERO_SHA1),
                        size,
                        disk_size: disk,
                        path: path.to_owned(),
                        in_head: false,
                    });
                    if self.big.len() > MAX_HISTORY_BLOBS * 20 {
                        self.trim_big(MAX_HISTORY_BLOBS);
                    }
                }
            }
            _ => {}
        }
    }

    fn trim_big(&mut self, keep: usize) {
        self.big
            .sort_by(|a, b| b.size.cmp(&a.size).then(a.path.cmp(&b.path)));
        self.big.truncate(keep);
    }
}

/// [`ext_of`] without an allocation for the common all-lower-case case.
fn ext_slice(path: &str) -> Cow<'_, str> {
    let base = basename(path);
    let ext = match base.rfind('.') {
        Some(i) if i > 0 => &base[i + 1..],
        _ => "",
    };
    if ext.chars().any(char::is_uppercase) {
        Cow::Owned(ext.to_lowercase())
    } else {
        Cow::Borrowed(ext)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn tf(path: &str, mode: u32, size: Option<u64>) -> TrackedFile {
        TrackedFile {
            path: path.to_owned(),
            mode,
            oid: Oid::ZERO_SHA1,
            size,
        }
    }

    fn of_kind(landmines: &[Landmine], kind: LandmineKind) -> Vec<&Landmine> {
        landmines.iter().filter(|l| l.kind == kind).collect()
    }

    #[test]
    fn case_collisions_report_where_spellings_diverge() {
        let files = [
            tf("README.md", 0o100644, Some(1)),
            tf("readme.md", 0o100644, Some(1)),
            tf("Src/A.rs", 0o100644, Some(1)),
            tf("src/b.rs", 0o100644, Some(1)),
            tf("src/x/C.rs", 0o100644, Some(1)),
            tf("Src/x/c.rs", 0o100644, Some(1)),
            tf("ok/fine.rs", 0o100644, Some(1)),
        ];
        let l = path_landmines(&files, &HashSet::new());
        let cc = of_kind(&l, LandmineKind::CaseCollision);
        assert_eq!(cc.len(), 2, "{cc:#?}");
        assert_eq!(cc[0].paths, ["README.md", "readme.md"]);
        assert_eq!(cc[0].severity, Severity::High);
        assert_eq!(cc[1].paths, ["Src/", "src/"]);
        assert!(cc[1].detail.contains("directories"));
    }

    #[test]
    fn windows_names_and_long_paths() {
        let long = format!("{}/f.txt", "d".repeat(270));
        let files = [
            tf("docs/aux.txt", 0o100644, Some(1)),
            tf("COM1", 0o100644, Some(1)),
            tf("com0.c", 0o100644, Some(1)),
            tf("lpt9.tar.gz", 0o100644, Some(1)),
            tf("auxiliary.rs", 0o100644, Some(1)),
            tf("con.d/x", 0o100644, Some(1)),
            tf("a:b.txt", 0o100644, Some(1)),
            tf("what?.md", 0o100644, Some(1)),
            tf("tab\there", 0o100644, Some(1)),
            tf("dir./file", 0o100644, Some(1)),
            tf("trailing ", 0o100644, Some(1)),
            tf(&long, 0o100644, Some(1)),
        ];
        let l = path_landmines(&files, &HashSet::new());
        let reserved: Vec<&str> = of_kind(&l, LandmineKind::WindowsReserved)
            .iter()
            .map(|x| x.paths[0].as_str())
            .collect();
        assert_eq!(reserved, ["docs/aux.txt", "COM1", "lpt9.tar.gz", "con.d/x"]);
        let invalid: Vec<&str> = of_kind(&l, LandmineKind::InvalidChars)
            .iter()
            .map(|x| x.paths[0].as_str())
            .collect();
        assert_eq!(invalid, ["a:b.txt", "what?.md", "tab\there"]);
        let trailing: Vec<&str> = of_kind(&l, LandmineKind::TrailingSpaceOrDot)
            .iter()
            .map(|x| x.paths[0].as_str())
            .collect();
        assert_eq!(trailing, ["dir./file", "trailing "]);
        let long_paths = of_kind(&l, LandmineKind::LongPath);
        assert_eq!(long_paths.len(), 1);
        assert_eq!(long_paths[0].paths[0], long);
        assert_eq!(long_paths[0].severity, Severity::Medium);
    }

    #[test]
    fn modes_symlinks_gitlinks_empty_executables() {
        let files = [
            tf("link", 0o120000, Some(5)),
            tf("run", 0o100755, Some(0)),
            tf("ok.sh", 0o100755, Some(10)),
            tf("empty.txt", 0o100644, Some(0)),
            tf("vendor/lib", 0o160000, None),
            tf("registered", 0o160000, None),
            tf("old/.git/config", 0o100644, Some(3)),
            tf("old/.git/HEAD", 0o100644, Some(3)),
            tf(".github/CODEOWNERS", 0o100644, Some(3)),
            tf(".gitmodules", 0o100644, Some(3)),
        ];
        let registered: HashSet<&str> = ["registered"].into_iter().collect();
        let l = path_landmines(&files, &registered);
        assert_eq!(of_kind(&l, LandmineKind::Symlink).len(), 1);
        assert_eq!(
            of_kind(&l, LandmineKind::Symlink)[0].severity,
            Severity::Low
        );
        let empty = of_kind(&l, LandmineKind::EmptyExecutable);
        assert_eq!(empty.len(), 1);
        assert_eq!(empty[0].paths, ["run"]);
        let nested = of_kind(&l, LandmineKind::NestedRepository);
        assert_eq!(nested.len(), 2, "{nested:#?}");
        assert_eq!(nested[0].paths, ["vendor/lib"]);
        assert!(nested[0].detail.contains("not registered"));
        assert_eq!(nested[1].paths, ["old/.git/HEAD", "old/.git/config"]);
        assert!(nested[1].detail.contains("`old/.git`"));
        assert!(of_kind(&l, LandmineKind::CaseCollision).is_empty());
    }

    #[test]
    fn huge_directories() {
        let mut files: Vec<TrackedFile> = (0..5_001)
            .map(|i| tf(&format!("gen/f{i}.rs"), 0o100644, Some(1)))
            .collect();
        files.push(tf("small/one.rs", 0o100644, Some(1)));
        let l = path_landmines(&files, &HashSet::new());
        let huge = of_kind(&l, LandmineKind::HugeDirectory);
        assert_eq!(huge.len(), 1);
        assert_eq!(huge[0].paths, ["gen/"]);
        assert!(huge[0].detail.starts_with("5001 direct entries"));
    }

    #[test]
    fn unicode_clash_heuristic() {
        assert_eq!(nfd_key("café.txt"), nfd_key("cafe\u{301}.txt"));
        assert_eq!(nfd_key("CAFÉ.txt"), nfd_key("cafe\u{301}.txt"));
        assert_ne!(nfd_key("cafe.txt"), nfd_key("café.txt"));
        assert_eq!(nfd_key("Straße"), "straße");
        let files = [
            tf("docs/café.md", 0o100644, Some(1)),
            tf("docs/cafe\u{301}.md", 0o100644, Some(1)),
            tf("docs/cafe.md", 0o100644, Some(1)),
            tf("ü.txt", 0o100644, Some(1)),
            tf("Ü.txt", 0o100644, Some(1)),
        ];
        let l = path_landmines(&files, &HashSet::new());
        let u = of_kind(&l, LandmineKind::UnicodeNormalization);
        assert_eq!(u.len(), 1, "{u:#?}");
        assert_eq!(u[0].paths, ["docs/cafe\u{301}.md", "docs/café.md"]);
        assert_eq!(u[0].severity, Severity::Medium);
        // `ü` vs `Ü` is a case collision, not a normalisation clash.
        assert_eq!(of_kind(&l, LandmineKind::CaseCollision).len(), 1);
    }

    #[test]
    fn reserved_name_edge_cases() {
        for ok in [
            "aux",
            "AUX.TXT",
            "con.tar.gz",
            "Nul",
            "COM9",
            "lpt1.c",
            "prn ",
        ] {
            assert!(is_windows_reserved(ok), "{ok}");
        }
        for not in [
            "auxiliary",
            "com0",
            "COM10",
            "lpt",
            "conf",
            ".aux",
            "",
            "cön",
        ] {
            assert!(!is_windows_reserved(not), "{not}");
        }
        assert_eq!(nested_git_root("a/.git/config"), Some("a"));
        assert_eq!(nested_git_root(".git/config"), Some(""));
        assert_eq!(nested_git_root("a/b/.GIT/x"), Some("a/b"));
        assert_eq!(nested_git_root(".gitignore"), None);
        assert_eq!(nested_git_root("x/.github/w.yml"), None);
        assert_eq!(
            nested_git_root("x/.git"),
            None,
            "a file named .git is a gitlink marker"
        );
    }

    #[test]
    fn eol_entries_and_fighting() {
        let (path, row) =
            parse_eol_entry("i/lf    w/crlf  attr/text eol=lf      \tsrc/a b.rs").unwrap();
        assert_eq!(path, "src/a b.rs");
        assert_eq!((row.index, row.worktree), (Eol::Lf, Eol::Crlf));
        assert_eq!(row.attr, "text eol=lf");
        let (_, row) = parse_eol_entry("i/      w/none  attr/                 \tlink").unwrap();
        assert_eq!((row.index, row.worktree), (Eol::Unknown, Eol::None));
        assert_eq!(row.attr, "");
        let (_, row) = parse_eol_entry("i/-text w/-text attr/-text            \tx.bin").unwrap();
        assert_eq!((row.index, row.worktree), (Eol::Binary, Eol::Binary));
        assert!(parse_eol_entry("garbage").is_none());

        // Index LF, worktree CRLF: fine when git normalises, fighting otherwise.
        assert!(!is_fighting(
            Eol::Lf,
            Eol::Crlf,
            normalisation("text eol=lf", "")
        ));
        assert!(!is_fighting(Eol::Lf, Eol::Crlf, normalisation("", "true")));
        assert!(!is_fighting(
            Eol::Lf,
            Eol::Crlf,
            normalisation("text=auto", "false")
        ));
        assert!(is_fighting(Eol::Lf, Eol::Crlf, normalisation("", "false")));
        assert!(is_fighting(
            Eol::Lf,
            Eol::Crlf,
            normalisation("-text", "true")
        ));
        // Index CRLF, worktree LF: the next add always stores LF.
        assert!(is_fighting(Eol::Crlf, Eol::Lf, normalisation("", "true")));
        assert!(is_fighting(Eol::Crlf, Eol::Lf, normalisation("", "false")));
        // Auto mode leaves a CRLF index blob alone.
        assert!(!is_fighting(
            Eol::Crlf,
            Eol::Crlf,
            normalisation("", "true")
        ));
        assert!(is_fighting(Eol::Crlf, Eol::Crlf, normalisation("text", "")));
        assert!(!is_fighting(Eol::Lf, Eol::Lf, normalisation("", "true")));
        assert!(!is_fighting(Eol::Binary, Eol::Lf, normalisation("", "")));
        assert!(!is_fighting(Eol::Lf, Eol::None, normalisation("", "")));

        assert_eq!(declared_attrs("-text"), (false, true));
        assert_eq!(declared_attrs("text eol=lf"), (true, false));
        assert_eq!(declared_attrs("text=auto"), (false, false));
        assert_eq!(
            attribute_flags("* text=auto\n*.png binary\n"),
            (true, false)
        );
        assert_eq!(
            attribute_flags("# text\n*.psd filter=lfs diff=lfs\n"),
            (false, true)
        );
        assert_eq!(attribute_flags("*.rs diff=rust\n"), (false, false));
    }

    #[test]
    fn probe_classifies_bytes() {
        let p = Probe::from_bytes(b"#!/bin/sh\necho hi\r\n", true);
        assert!(p.shebang && p.utf8 && !p.nul && !p.bom);
        assert_eq!((p.lf, p.crlf), (1, 1));
        assert_eq!(p.worktree_eol(), Eol::Mixed);
        let p = Probe::from_bytes(&[0xEF, 0xBB, 0xBF, b'a', b'\n'], true);
        assert!(p.bom && p.utf8);
        assert_eq!(p.worktree_eol(), Eol::Lf);
        let p = Probe::from_bytes(&[b'c', b'a', b'f', 0xE9, b'\r', b'\n'], true);
        assert!(!p.utf8, "Latin-1 is not UTF-8");
        assert_eq!(p.worktree_eol(), Eol::Crlf);
        let p = Probe::from_bytes(&[b'a', 0, b'b'], true);
        assert!(p.nul);
        assert_eq!(p.worktree_eol(), Eol::Binary);
        // A multi-byte sequence cut by the read limit is not an error.
        let p = Probe::from_bytes(&[b'a', 0xC3], false);
        assert!(p.utf8);
        let p = Probe::from_bytes(&[b'a', 0xC3], true);
        assert!(!p.utf8);
        assert!(!Probe::from_bytes(b"#![no_std]\nfn x() {}\n", true).shebang);
        assert!(Probe::from_bytes(b"#! /usr/bin/env python\n", true).shebang);
        let p = Probe::from_bytes(
            b"version https://git-lfs.github.com/spec/v1\noid sha256:abc\nsize 5\n",
            true,
        );
        assert!(p.lfs_pointer);
        assert_eq!(Probe::from_bytes(b"", true).worktree_eol(), Eol::None);
    }

    #[test]
    fn gitmodules_and_count_objects_parse() {
        let text = r#"
[submodule "libs/foo"]
	path = libs/foo
	url = https://example.com/foo.git
	branch = main
# comment
[submodule "bar"]
    url = ../bar.git
    path = third_party/bar
[core]
    x = y
"#;
        let m = parse_gitmodules(text);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].name, "libs/foo");
        assert_eq!(m[0].path, "libs/foo");
        assert_eq!(m[0].url, "https://example.com/foo.git");
        assert_eq!(m[0].branch.as_deref(), Some("main"));
        assert_eq!(m[1].path, "third_party/bar");
        assert_eq!(m[1].branch, None);

        let co = parse_count_objects(
            "count: 6\nsize: 20\nin-pack: 1618446\npacks: 3\nsize-pack: 5015870\nprune-packable: 0\ngarbage: 0\nsize-garbage: 0\n",
        );
        assert_eq!(co.len(), 8);
        assert_eq!(co[0], ("count".to_owned(), "6".to_owned()));
        let p = Plumbing {
            count_objects: co,
            ..Plumbing::default()
        };
        assert_eq!(p.count("in-pack"), 1_618_446);
        assert_eq!(p.count("size-pack") * 1024, 5_136_250_880);
        assert_eq!(p.count("missing"), 0);
    }

    #[test]
    fn small_parsers() {
        assert_eq!(parse_bool("true"), Some(true));
        assert_eq!(parse_bool("False"), Some(false));
        assert_eq!(parse_bool(""), None);
        assert_eq!(parse_bool("maybe"), None);
        assert!(is_packed_ref_line(
            "184e35392920b397f50720fcbb20358feb61e0bf refs/heads/main"
        ));
        assert!(!is_packed_ref_line(
            "# pack-refs with: peeled fully-peeled sorted"
        ));
        assert!(!is_packed_ref_line(
            "^184e35392920b397f50720fcbb20358feb61e0bf"
        ));
        assert_eq!(ext_slice("a/B.RS"), "rs");
        assert_eq!(ext_slice("Makefile"), "");
        assert_eq!(ext_slice(".gitignore"), "");
        assert_eq!(mb(5_242_880), "5.0 MB");

        let pointer = b"version https://git-lfs.github.com/spec/v1\noid sha256:ab\nsize 1\n";
        let mut out = Vec::new();
        out.extend_from_slice(format!("{} blob {}\n", "a".repeat(40), pointer.len()).as_bytes());
        out.extend_from_slice(pointer);
        out.push(b'\n');
        out.extend_from_slice(format!("{} missing\n", "b".repeat(40)).as_bytes());
        out.extend_from_slice(format!("{} blob 5\nhello\n", "c".repeat(40)).as_bytes());
        assert_eq!(count_lfs_pointers(&out), 1);

        let mut agg = Aggregate::default();
        agg.add(b"commit 0123456789abcdef0123456789abcdef01234567 254 176 \n");
        agg.add(b"tree 0123456789abcdef0123456789abcdef01234567 100 80 src\n");
        agg.add(b"blob 0123456789abcdef0123456789abcdef01234567 10 8 src/Main.RS\n");
        agg.add(
            b"blob 0123456789abcdef0123456789abcdef01234568 600000 1000 big/data with space.bin\n",
        );
        agg.add(b"blob 0123456789abcdef0123456789abcdef01234569 3 3 README\n");
        assert_eq!((agg.commits, agg.trees, agg.blobs), (1, 1, 3));
        assert_eq!(agg.bytes, 600_013);
        assert_eq!(agg.disk, 1_011);
        assert_eq!(agg.by_ext["rs"], (1, 10));
        assert_eq!(agg.by_ext[""], (1, 3));
        assert_eq!(agg.by_dir["big"], 600_000);
        assert_eq!(agg.by_dir["."], 3);
        assert_eq!(agg.big.len(), 1);
        assert_eq!(agg.big[0].path, "big/data with space.bin");
    }

    /// Both line-ending sources (the `--eol` listing and the grep + probe
    /// path for large repositories) must agree on a real repository.
    #[test]
    fn eol_sources_agree_on_a_real_repository() {
        let dir = std::env::temp_dir().join(format!(
            "gitvis-health-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "core.autocrlf", "false"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "T"]);
        std::fs::write(
            dir.join(".gitattributes"),
            "*.bin binary\n*.sh text eol=lf\n",
        )
        .unwrap();
        std::fs::write(dir.join("lf.txt"), "a\nb\n").unwrap();
        std::fs::write(dir.join("crlf.txt"), "a\r\nb\r\n").unwrap();
        std::fs::write(dir.join("mixed.txt"), "a\r\nb\n").unwrap();
        std::fs::write(dir.join("none.txt"), "abc").unwrap();
        std::fs::write(dir.join("text.bin"), "plain\n").unwrap();
        std::fs::write(dir.join("real.bin"), [0u8, 1, 2, b'\n']).unwrap();
        std::fs::write(dir.join("run.sh"), "#!/bin/sh\necho\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "eol"]);
        // Worktree drift after the commit: CRLF on an LF file.
        std::fs::write(dir.join("lf.txt"), "a\r\nb\r\n").unwrap();

        let mut cmds = Vec::new();
        let files = super::super::tracked_files(&dir, &mut cmds).unwrap();
        let probes = probe_files(&dir, &files);
        let entries = git_z(&dir, &["ls-files", "--eol", "-z"], &mut cmds).unwrap();
        let mut listed: HashMap<String, EolRow> = HashMap::new();
        for e in &entries {
            let (p, row) = parse_eol_entry(e).unwrap();
            listed.insert(p.to_owned(), row);
        }
        let (attrs, _) = check_attr(&dir, &files);
        // Both large-repository branches: the exact listing restricted to
        // the CRLF files, and the second grep for the mixed ones.
        for max_exact in [EOL_EXACT_MAX_PATHS, 0] {
            let (grep, cmds) = grep_eol(&dir, max_exact);
            // crlf.txt and mixed.txt have CR-terminated lines in the index;
            // the drifted lf.txt is still LF there.
            assert_eq!(grep.crlf.len(), 2, "{grep:?}");
            assert!(grep.crlf.contains("crlf.txt") && grep.crlf.contains("mixed.txt"));
            let exact = usize::from(max_exact > 0);
            assert_eq!(grep.exact.len(), 2 * exact);
            assert_eq!(cmds.len(), 1 + exact, "{cmds:?}");
            let grepped = rows_from_grep(&files, &probes, &grep, &attrs);
            for (i, f) in files.iter().enumerate() {
                let a = &listed[&f.path];
                let b = grepped[i].as_ref().unwrap();
                assert_eq!(a, b, "{} (max_exact {max_exact})", f.path);
            }
        }
        assert_eq!(listed["crlf.txt"].index, Eol::Crlf);
        assert_eq!(listed["mixed.txt"].index, Eol::Mixed);
        assert_eq!(listed["none.txt"].index, Eol::None);
        assert_eq!(listed["real.bin"].index, Eol::Binary);
        assert_eq!(listed["text.bin"].attr, "-text");
        assert_eq!(listed["run.sh"].attr, "text eol=lf");
        assert_eq!(
            (listed["lf.txt"].index, listed["lf.txt"].worktree),
            (Eol::Lf, Eol::Crlf)
        );

        let health = analyze(&dir, &files, &mut cmds);
        assert_eq!(health.line_endings.fighting, ["lf.txt"]);
        assert_eq!(health.line_endings.misattributed, ["text.bin"]);
        assert_eq!(health.line_endings.mixed, ["mixed.txt"]);
        assert_eq!(
            health.line_endings.crlf, 1,
            "crlf.txt only: mixed.txt is listed as mixed"
        );
        assert_eq!(health.line_endings.crlf_examples, ["crlf.txt"]);
        assert!(health.line_endings.lf >= 4, "{}", health.line_endings.lf);
        assert_eq!(health.line_endings.autocrlf, "false");
        assert!(health.line_endings.attributes_rules);
        assert_eq!(health.line_endings.attributes_files, [".gitattributes"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
