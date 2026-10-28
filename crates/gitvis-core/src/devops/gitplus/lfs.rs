//! Git LFS: whether it is installed (`git lfs version`) and set up (the
//! filter in the configuration, the hooks in this repository), the tracked
//! patterns (`.gitattributes` lines with `filter=lfs`, plus `git lfs
//! track`), the files (`git lfs ls-files --json`), `git lfs status`, locks
//! (`git lfs locks --json`, a network call) and the command lines of the
//! changes.

use std::path::Path;

use serde::Deserialize;

use super::{git_probe, git_raw, output_text, unquote};

/// The `git-lfs/3.7.1 (GitHub; …)` version, `None` when LFS is missing.
pub fn parse_version(text: &str) -> Option<String> {
    let l = text.lines().find(|l| l.trim().starts_with("git-lfs/"))?;
    let v = l.trim().trim_start_matches("git-lfs/");
    Some(v.split_whitespace().next().unwrap_or(v).to_owned())
}

/// A tracked pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    pub pattern: String,
    /// The `.gitattributes` file it is in.
    pub source: String,
    /// 1-based line in `source`, when read from the file.
    pub line: Option<usize>,
    /// `lockable`: files are read-only until locked.
    pub lockable: bool,
    /// The attributes as written (`filter=lfs diff=lfs merge=lfs -text`).
    pub attrs: String,
}

/// Lines of a `.gitattributes` file that send files through LFS.
pub fn parse_gitattributes(text: &str, source: &str) -> Vec<Pattern> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (pattern, attrs) = if let Some(rest) = line.strip_prefix('"') {
            match rest.find('"') {
                Some(end) => (unquote(&line[..end + 2]), rest[end + 1..].trim()),
                None => continue,
            }
        } else {
            match line.split_once(char::is_whitespace) {
                Some((p, a)) => (p.to_owned(), a.trim()),
                None => continue,
            }
        };
        let words: Vec<&str> = attrs.split_whitespace().collect();
        if !words.contains(&"filter=lfs") {
            continue;
        }
        out.push(Pattern {
            pattern,
            source: source.to_owned(),
            line: Some(i + 1),
            lockable: words.contains(&"lockable"),
            attrs: attrs.to_owned(),
        });
    }
    out
}

/// A pattern `git lfs track` lists: (pattern, source file, lockable).
pub type Listed = (String, String, bool);

/// `git lfs track` with no arguments: (tracked, excluded `(pattern,
/// source file)`).
pub fn parse_track(text: &str) -> (Vec<Listed>, Vec<(String, String)>) {
    let mut tracked = Vec::new();
    let mut excluded = Vec::new();
    let mut in_excluded = false;
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with("Listing tracked patterns") {
            in_excluded = false;
            continue;
        }
        if t.starts_with("Listing excluded patterns") {
            in_excluded = true;
            continue;
        }
        if t.is_empty() || !l.starts_with(char::is_whitespace) {
            continue;
        }
        // `*.psd [lockable] (.gitattributes)`
        let (rest, source) = match t.rfind(" (") {
            Some(i) if t.ends_with(')') => (&t[..i], t[i + 2..t.len() - 1].to_owned()),
            _ => (t, String::new()),
        };
        let lockable = rest.ends_with(" [lockable]");
        let pattern = rest.trim_end_matches(" [lockable]").trim().to_owned();
        if in_excluded {
            excluded.push((pattern, source));
        } else {
            tracked.push((pattern, source, lockable));
        }
    }
    (tracked, excluded)
}

/// Patterns from the root `.gitattributes`, completed with the ones
/// `git lfs track` lists from other `.gitattributes` files.
pub fn merge_patterns(root: Vec<Pattern>, listed: &[Listed]) -> Vec<Pattern> {
    let mut out = root;
    for (p, source, lockable) in listed {
        if !out.iter().any(|x| &x.pattern == p && &x.source == source) {
            out.push(Pattern {
                pattern: p.clone(),
                source: source.clone(),
                line: None,
                lockable: *lockable,
                attrs: String::new(),
            });
        }
    }
    out
}

/// A file stored in LFS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LfsFile {
    pub path: String,
    pub oid: String,
    pub size: Option<u64>,
    /// The content is in the local LFS store.
    pub downloaded: bool,
    /// The working tree has the content (not just the pointer).
    pub checkout: bool,
}

#[derive(Deserialize)]
struct LsJson {
    #[serde(default)]
    files: Vec<LsJsonFile>,
}

#[derive(Deserialize)]
struct LsJsonFile {
    name: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    checkout: Option<bool>,
    #[serde(default)]
    downloaded: Option<bool>,
    #[serde(default)]
    oid: String,
}

/// `git lfs ls-files --json`.
pub fn parse_ls_files_json(bytes: &[u8]) -> Result<Vec<LfsFile>, String> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    let v: LsJson =
        serde_json::from_slice(bytes).map_err(|e| format!("Unreadable git lfs output: {e}"))?;
    Ok(v.files
        .into_iter()
        .map(|f| LfsFile {
            path: f.name,
            oid: f.oid,
            size: f.size,
            downloaded: f.downloaded.unwrap_or(true),
            checkout: f.checkout.unwrap_or(true),
        })
        .collect())
}

/// `git lfs ls-files -s -l` (older git-lfs without `--json`):
/// `<oid> <*|-> <path> (<size>)`; `*` = content present, `-` = pointer.
pub fn parse_ls_files(text: &str) -> Vec<LfsFile> {
    let mut out = Vec::new();
    for l in text.lines() {
        let Some((oid, rest)) = l.split_once(' ') else {
            continue;
        };
        let Some((mark, rest)) = rest.split_once(' ') else {
            continue;
        };
        if mark != "*" && mark != "-" {
            continue;
        }
        let (path, size) = match rest.rfind(" (") {
            Some(i) if rest.ends_with(')') => {
                (&rest[..i], parse_size(&rest[i + 2..rest.len() - 1]))
            }
            _ => (rest, None),
        };
        out.push(LfsFile {
            path: path.to_owned(),
            oid: oid.to_owned(),
            size,
            downloaded: mark == "*",
            checkout: mark == "*",
        });
    }
    out
}

/// `3.0 KB` → 3072 (approximate; git-lfs rounds).
pub fn parse_size(s: &str) -> Option<u64> {
    let mut it = s.split_whitespace();
    let n: f64 = it.next()?.parse().ok()?;
    let mult = match it.next().unwrap_or("B").to_ascii_uppercase().as_str() {
        "B" => 1.0,
        "KB" => 1024.0,
        "MB" => 1024.0 * 1024.0,
        "GB" => 1024.0 * 1024.0 * 1024.0,
        "TB" => 1024f64.powi(4),
        _ => return None,
    };
    Some((n * mult).round() as u64)
}

/// One section of `git lfs status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusSection {
    /// `Objects to be committed`, `Objects not staged for commit`, …
    pub title: String,
    /// `(path, detail)`: `(two.bin, LFS: 94f38fc)`.
    pub entries: Vec<(String, String)>,
}

/// Parse `git lfs status`.
pub fn parse_status(text: &str) -> Vec<StatusSection> {
    let mut out: Vec<StatusSection> = Vec::new();
    for l in text.lines() {
        if l.trim().is_empty() {
            continue;
        }
        if l.starts_with('\t') || l.starts_with("    ") {
            let t = l.trim();
            let (path, detail) = match t.rfind(" (") {
                Some(i) if t.ends_with(')') => {
                    (t[..i].to_owned(), t[i + 2..t.len() - 1].to_owned())
                }
                _ => (t.to_owned(), String::new()),
            };
            if let Some(s) = out.last_mut() {
                s.entries.push((path, detail));
            }
            continue;
        }
        if let Some(title) = l.trim().strip_suffix(':') {
            out.push(StatusSection {
                title: title.to_owned(),
                entries: Vec::new(),
            });
        }
    }
    out
}

/// A lock on the LFS server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lock {
    pub id: String,
    pub path: String,
    pub owner: String,
    pub locked_at: String,
}

#[derive(Deserialize)]
struct LockJson {
    #[serde(default)]
    id: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    owner: Option<LockOwner>,
    #[serde(default)]
    locked_at: String,
}

#[derive(Deserialize)]
struct LockOwner {
    #[serde(default)]
    name: String,
}

/// `git lfs locks --json`.
pub fn parse_locks_json(bytes: &[u8]) -> Result<Vec<Lock>, String> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    let v: Vec<LockJson> =
        serde_json::from_slice(bytes).map_err(|e| format!("Unreadable git lfs output: {e}"))?;
    Ok(v.into_iter()
        .map(|l| Lock {
            id: l.id,
            path: l.path,
            owner: l.owner.map(|o| o.name).unwrap_or_default(),
            locked_at: l.locked_at,
        })
        .collect())
}

/// Everything the tab shows (no network).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overview {
    /// `None` when `git lfs` is not installed.
    pub version: Option<String>,
    /// What `git lfs version` printed when it failed.
    pub missing_reason: String,
    /// `filter.lfs.process` / `clean` are configured (by `git lfs
    /// install`, globally or here).
    pub filter_configured: bool,
    /// The pre-push hook calls git-lfs (`git lfs install` in this
    /// repository).
    pub hooks_installed: bool,
    pub patterns: Vec<Pattern>,
    pub excluded: Vec<(String, String)>,
    pub files: Vec<LfsFile>,
    pub files_error: Option<String>,
    pub status: Vec<StatusSection>,
    pub status_error: Option<String>,
    pub elapsed_ms: u64,
}

impl Overview {
    pub fn installed(&self) -> bool {
        self.version.is_some()
    }

    pub fn total_size(&self) -> u64 {
        self.files.iter().filter_map(|f| f.size).sum()
    }

    pub fn pointers_only(&self) -> usize {
        self.files.iter().filter(|f| !f.checkout).count()
    }
}

fn hooks_call_lfs(dir: &Path, cmds: &mut Vec<String>) -> bool {
    let Some(hooks) = git_probe(dir, &["rev-parse", "--git-path", "hooks"], cmds) else {
        return false;
    };
    let pre_push = super::resolve_in(dir, &hooks).join("pre-push");
    std::fs::read_to_string(pre_push)
        .map(|t| t.contains("git lfs") || t.contains("git-lfs"))
        .unwrap_or(false)
}

/// Read the LFS state of a repository (local only).
pub fn load(dir: &Path, cmds: &mut Vec<String>) -> Overview {
    let started = std::time::Instant::now();
    let mut o = Overview::default();
    match git_raw(dir, &["lfs", "version"], &[], cmds) {
        Ok(out) if out.success => {
            o.version = parse_version(&String::from_utf8_lossy(&out.stdout));
            if o.version.is_none() {
                o.missing_reason = output_text(&out);
            }
        }
        Ok(out) => o.missing_reason = output_text(&out),
        Err(e) => o.missing_reason = e,
    }
    let root = std::fs::read_to_string(dir.join(".gitattributes")).unwrap_or_default();
    let mut patterns = parse_gitattributes(&root, ".gitattributes");
    if o.version.is_none() {
        o.patterns = patterns;
        o.elapsed_ms = started.elapsed().as_millis() as u64;
        return o;
    }
    o.filter_configured = git_probe(dir, &["config", "--get", "filter.lfs.process"], cmds)
        .or_else(|| git_probe(dir, &["config", "--get", "filter.lfs.clean"], cmds))
        .is_some();
    o.hooks_installed = hooks_call_lfs(dir, cmds);
    if let Ok(out) = git_raw(dir, &["lfs", "track"], &[], cmds)
        && out.success
    {
        let (tracked, excluded) = parse_track(&String::from_utf8_lossy(&out.stdout));
        patterns = merge_patterns(patterns, &tracked);
        o.excluded = excluded;
    }
    o.patterns = patterns;
    match git_raw(dir, &["lfs", "ls-files", "--json"], &[], cmds) {
        Ok(out) if out.success => match parse_ls_files_json(&out.stdout) {
            Ok(f) => o.files = f,
            Err(e) => o.files_error = Some(e),
        },
        _ => match git_raw(dir, &["lfs", "ls-files", "-s", "-l"], &[], cmds) {
            Ok(out) if out.success => {
                o.files = parse_ls_files(&String::from_utf8_lossy(&out.stdout))
            }
            Ok(out) => o.files_error = Some(output_text(&out)),
            Err(e) => o.files_error = Some(e),
        },
    }
    match git_raw(dir, &["lfs", "status"], &[], cmds) {
        Ok(out) if out.success => o.status = parse_status(&String::from_utf8_lossy(&out.stdout)),
        Ok(out) => o.status_error = Some(output_text(&out)),
        Err(e) => o.status_error = Some(e),
    }
    o.elapsed_ms = started.elapsed().as_millis() as u64;
    o
}

fn v(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

/// `git lfs track [--lockable] PATTERN` (writes `.gitattributes`).
pub fn track_args(pattern: &str, lockable: bool) -> Vec<String> {
    let mut a = v(&["lfs", "track"]);
    if lockable {
        a.push("--lockable".into());
    }
    a.push(pattern.trim().to_owned());
    a
}

/// `git lfs untrack PATTERN` (writes `.gitattributes`).
pub fn untrack_args(pattern: &str) -> Vec<String> {
    let mut a = v(&["lfs", "untrack"]);
    a.push(pattern.trim().to_owned());
    a
}

/// `git lfs install --local`: the filter and hooks for this repository
/// only (never the global configuration).
pub fn install_local_args() -> Vec<String> {
    v(&["lfs", "install", "--local"])
}

/// `git lfs pull`: download and check out the content. Network.
pub fn pull_args() -> Vec<String> {
    v(&["lfs", "pull"])
}

/// `git lfs fetch`: download without checking out. Network.
pub fn fetch_args() -> Vec<String> {
    v(&["lfs", "fetch"])
}

/// `git lfs prune [--dry-run --verbose]`: delete old local copies.
pub fn prune_args(dry_run: bool) -> Vec<String> {
    if dry_run {
        v(&["lfs", "prune", "--dry-run", "--verbose"])
    } else {
        v(&["lfs", "prune"])
    }
}

/// `git lfs locks --json`. Network.
pub fn locks_args() -> Vec<String> {
    v(&["lfs", "locks", "--json"])
}

/// `git lfs lock PATH`. A write on the LFS server.
pub fn lock_args(path: &str) -> Vec<String> {
    let mut a = v(&["lfs", "lock"]);
    a.push(path.to_owned());
    a
}

/// `git lfs unlock [--force] PATH`. A write on the LFS server.
pub fn unlock_args(path: &str, force: bool) -> Vec<String> {
    let mut a = v(&["lfs", "unlock"]);
    if force {
        a.push("--force".into());
    }
    a.push(path.to_owned());
    a
}

/// Summary of `git lfs prune --dry-run --verbose` (its last lines).
pub fn prune_summary(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| l.contains("prune") || l.contains("would") || l.contains("local objects"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_line() {
        assert_eq!(
            parse_version("git-lfs/3.7.1 (GitHub; windows amd64; go 1.25.1; git b84b3384)\n"),
            Some("3.7.1".into())
        );
        assert_eq!(parse_version("git: 'lfs' is not a git command."), None);
    }

    #[test]
    fn gitattributes_patterns() {
        let text = "# LFS\n*.bin filter=lfs diff=lfs merge=lfs -text\n*.txt text eol=lf\nassets/** filter=lfs diff=lfs merge=lfs -text lockable\n\"with space/*.psd\" filter=lfs -text\n*.zip -filter=lfs\n";
        let v = parse_gitattributes(text, ".gitattributes");
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].pattern, "*.bin");
        assert_eq!(v[0].line, Some(2));
        assert!(!v[0].lockable);
        assert_eq!(v[1].pattern, "assets/**");
        assert!(v[1].lockable);
        assert_eq!(v[2].pattern, "with space/*.psd");
    }

    #[test]
    fn track_listing() {
        let text = "Listing tracked patterns\n    *.bin (.gitattributes)\n    *.psd [lockable] (art/.gitattributes)\nListing excluded patterns\n    *.tmp (.gitattributes)\n";
        let (t, e) = parse_track(text);
        assert_eq!(t[0], ("*.bin".into(), ".gitattributes".into(), false));
        assert_eq!(t[1], ("*.psd".into(), "art/.gitattributes".into(), true));
        assert_eq!(e, vec![("*.tmp".into(), ".gitattributes".into())]);
        let merged = merge_patterns(
            parse_gitattributes("*.bin filter=lfs\n", ".gitattributes"),
            &t,
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[1].line, None);
    }

    #[test]
    fn ls_files_both_forms() {
        let json = br#"{"files":[{"name":"assets/a.png","size":4,"checkout":true,"downloaded":true,"oid_type":"sha256","oid":"6fb6","version":"x"},{"name":"big.bin","size":3000,"checkout":false,"downloaded":false,"oid":"b936"}]}"#;
        let v = parse_ls_files_json(json).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].size, Some(3000));
        assert!(!v[1].checkout);
        assert!(parse_ls_files_json(b"\n").unwrap().is_empty());
        assert!(parse_ls_files_json(b"nope").is_err());
        let text = "6fb6d3835a3563bf834b561a53b234839f9c9449ab9b3cf01c531ef30198dcbe * assets/a b.png (4 B)\nb936 - big.bin (3.0 KB)\n";
        let t = parse_ls_files(text);
        assert_eq!(t[0].path, "assets/a b.png");
        assert_eq!(t[0].size, Some(4));
        assert!(t[0].downloaded);
        assert_eq!(t[1].size, Some(3072));
        assert!(!t[1].downloaded);
        assert_eq!(parse_size("1.5 MB"), Some(1_572_864));
    }

    #[test]
    fn status_sections() {
        let text = "On branch main\n\nObjects to be committed:\n\n\ttwo.bin (LFS: 94f38fc)\n\nObjects not staged for commit:\n\n\tassets/a.png (LFS: 6fb6d38 -> File: d6234d9)\n\n";
        let s = parse_status(text);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].title, "Objects to be committed");
        assert_eq!(
            s[0].entries,
            vec![("two.bin".into(), "LFS: 94f38fc".into())]
        );
        assert_eq!(s[1].entries[0].1, "LFS: 6fb6d38 -> File: d6234d9");
    }

    #[test]
    fn locks() {
        let json = br#"[{"id":"2","path":"foo/bar.zip","owner":{"name":"Jane Doe"},"locked_at":"2016-05-17T15:49:06+00:00"}]"#;
        let v = parse_locks_json(json).unwrap();
        assert_eq!(v[0].owner, "Jane Doe");
        assert_eq!(v[0].id, "2");
        assert!(parse_locks_json(b"[]").unwrap().is_empty());
    }

    #[test]
    fn command_lines() {
        assert_eq!(
            track_args(" *.psd ", true),
            vec!["lfs", "track", "--lockable", "*.psd"]
        );
        assert_eq!(untrack_args("*.bin"), vec!["lfs", "untrack", "*.bin"]);
        assert_eq!(install_local_args(), vec!["lfs", "install", "--local"]);
        assert_eq!(
            prune_args(true),
            vec!["lfs", "prune", "--dry-run", "--verbose"]
        );
        assert_eq!(
            unlock_args("a.psd", true),
            vec!["lfs", "unlock", "--force", "a.psd"]
        );
        assert_eq!(lock_args("a.psd"), vec!["lfs", "lock", "a.psd"]);
        // Safe mode treats lock / unlock as remote writes.
        let a = lock_args("x");
        let args: Vec<&str> = a.iter().map(String::as_str).collect();
        assert!(crate::devops::safety::remote_write("git", &args, None).is_some());
    }
}
