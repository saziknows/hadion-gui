//! File history: `git log --follow -M --numstat -- PATH`, streamed and
//! capped, with renames tracked (each commit knows the name the file had
//! then), per-commit stats, and helpers for the diff of one commit and for
//! fuzzy path suggestions.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::Instant;

use super::git_line;
use crate::cli;

/// Most commits read.
pub const MAX_COMMITS: usize = 2_000;

/// One commit that touched the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileCommit {
    pub sha: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub subject: String,
    /// The file's name in this commit.
    pub path: String,
    /// The name before this commit, when it renamed the file.
    pub renamed_from: Option<String>,
    pub added: u64,
    pub deleted: u64,
    pub binary: bool,
}

impl FileCommit {
    pub fn short(&self) -> &str {
        &self.sha[..self.sha.len().min(10)]
    }
}

/// The history of one path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileHistory {
    pub path: String,
    /// Newest first.
    pub commits: Vec<FileCommit>,
    /// More commits exist beyond the cap.
    pub truncated: bool,
    pub elapsed_ms: u64,
    pub command: String,
    /// Authors by commits, most first.
    pub authors: Vec<(String, u32)>,
    /// Every name the file had, newest first.
    pub names: Vec<String>,
}

impl FileHistory {
    pub fn renames(&self) -> usize {
        self.commits
            .iter()
            .filter(|c| c.renamed_from.is_some())
            .count()
    }

    pub fn churn(&self) -> (u64, u64) {
        self.commits
            .iter()
            .fold((0, 0), |(a, d), c| (a + c.added, d + c.deleted))
    }
}

/// `src/{old => new}/x.rs` (and `a => b`) → `(old path, new path)`.
pub fn split_rename(p: &str) -> (Option<String>, String) {
    if let (Some(open), Some(close)) = (p.find('{'), p.rfind('}'))
        && open < close
        && let Some((old, new)) = p[open + 1..close].split_once(" => ")
    {
        let (pre, post) = (&p[..open], &p[close + 1..]);
        let join = |mid: &str| {
            let s = format!("{pre}{mid}{post}");
            // `{ => sub}/x` leaves a double or a leading slash.
            s.replace("//", "/").trim_start_matches('/').to_owned()
        };
        return (Some(join(old)), join(new));
    }
    if let Some((old, new)) = p.split_once(" => ") {
        return (Some(old.to_owned()), new.to_owned());
    }
    (None, p.to_owned())
}

/// The `git log` arguments.
pub fn log_args(path: &str) -> Vec<String> {
    vec![
        "-c".into(),
        "core.quotepath=false".into(),
        "log".into(),
        "--follow".into(),
        "-M".into(),
        "--format=%x1e%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%s".into(),
        "--numstat".into(),
        "--".into(),
        path.into(),
    ]
}

/// Parse one record (the text after a `\x1e`).
fn parse_record(rec: &str, current: &str) -> Option<FileCommit> {
    let mut lines = rec.lines();
    let head = lines.next()?;
    let f: Vec<&str> = head.splitn(6, '\u{1f}').collect();
    if f.len() < 6 {
        return None;
    }
    let mut c = FileCommit {
        sha: f[0].trim().to_owned(),
        parents: f[1].split_whitespace().map(str::to_owned).collect(),
        author: f[2].to_owned(),
        email: f[3].to_owned(),
        time: f[4].parse().unwrap_or(0),
        subject: f[5].to_owned(),
        path: current.to_owned(),
        renamed_from: None,
        added: 0,
        deleted: 0,
        binary: false,
    };
    for l in lines {
        let mut p = l.splitn(3, '\t');
        let (Some(a), Some(d), Some(path)) = (p.next(), p.next(), p.next()) else {
            continue;
        };
        let (old, new) = split_rename(path);
        // With --follow only the followed file is listed, but be strict.
        if new != current && old.as_deref() != Some(current) && path != current {
            continue;
        }
        c.binary = a == "-";
        c.added = a.parse().unwrap_or(0);
        c.deleted = d.parse().unwrap_or(0);
        c.path = new;
        c.renamed_from = old;
        break;
    }
    Some(c)
}

/// Parse the whole output (newest first), following the names.
pub fn parse(text: &str, path: &str, max: usize) -> (Vec<FileCommit>, bool) {
    let mut out = Vec::new();
    let mut current = path.to_owned();
    for rec in text.split('\u{1e}') {
        if rec.trim().is_empty() {
            continue;
        }
        if out.len() >= max {
            return (out, true);
        }
        if let Some(c) = parse_record(rec, &current) {
            if let Some(old) = &c.renamed_from {
                current = old.clone();
            }
            out.push(c);
        }
    }
    (out, false)
}

fn finish(h: &mut FileHistory) {
    let mut authors: HashMap<&str, u32> = HashMap::new();
    for c in &h.commits {
        *authors.entry(c.author.as_str()).or_insert(0) += 1;
    }
    let mut a: Vec<(String, u32)> = authors
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
    a.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));
    h.authors = a;
    let mut names = vec![h.path.clone()];
    for c in &h.commits {
        if let Some(old) = &c.renamed_from
            && !names.contains(old)
        {
            names.push(old.clone());
        }
    }
    h.names = names;
}

/// Stream the history of `path`, stopping at `max` commits.
pub fn history(
    dir: &Path,
    path: &str,
    max: usize,
    cmds: &mut Vec<String>,
) -> Result<FileHistory, String> {
    let path = path.trim().replace('\\', "/");
    if path.is_empty() {
        return Err("Enter a file path.".to_owned());
    }
    let started = Instant::now();
    let args = log_args(&path);
    let command = git_line(&args);
    cmds.push(command.clone());
    let mut child = cli::spawn(dir, &args).map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let mut reader = BufReader::with_capacity(1 << 16, stdout);
    let mut buf = Vec::new();
    let mut commits = Vec::new();
    let mut current = path.clone();
    let mut truncated = false;
    loop {
        buf.clear();
        match reader.read_until(0x1e, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if buf.last() == Some(&0x1e) {
            buf.pop();
        }
        let rec = String::from_utf8_lossy(&buf);
        if rec.trim().is_empty() {
            continue;
        }
        if commits.len() >= max {
            truncated = true;
            break;
        }
        if let Some(c) = parse_record(&rec, &current) {
            if let Some(old) = &c.renamed_from {
                current = old.clone();
            }
            commits.push(c);
        }
    }
    if truncated {
        let _ = child.kill();
    }
    let _ = child.wait();
    if commits.is_empty() && !truncated {
        // Tell an unknown path from a path without history.
        let known = cli::run(dir, &["log", "-1", "--format=%H", "--", &path], None)
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false);
        if !known {
            return Err(format!(
                "No commit touches \"{path}\". Check the spelling (paths are relative to the repository root)."
            ));
        }
    }
    let mut h = FileHistory {
        path,
        commits,
        truncated,
        elapsed_ms: started.elapsed().as_millis() as u64,
        command,
        ..Default::default()
    };
    finish(&mut h);
    Ok(h)
}

/// `git show --format= -M COMMIT -- PATH [OLD]`: the change to the file in
/// one commit (the old name too, so a rename shows as one diff).
pub fn show_args(sha: &str, path: &str, renamed_from: Option<&str>) -> Vec<String> {
    let mut v = vec![
        "-c".to_owned(),
        "core.quotepath=false".to_owned(),
        "show".to_owned(),
        "--format=".to_owned(),
        "-M".to_owned(),
        sha.to_owned(),
        "--".to_owned(),
        path.to_owned(),
    ];
    if let Some(old) = renamed_from {
        v.push(old.to_owned());
    }
    v
}

/// Tracked files (`git ls-files`), for suggestions.
pub fn tracked_files(dir: &Path, cmds: &mut Vec<String>) -> Result<Vec<String>, String> {
    let args = ["-c", "core.quotepath=false", "ls-files"];
    cmds.push(git_line(&args));
    let out = cli::run(dir, &args, None).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

/// Fuzzy match score of `query` against `path` (higher is better), `None`
/// when the letters of the query do not all appear in order. Matches in
/// the file name, at word starts and in runs score more.
pub fn fuzzy_score(query: &str, path: &str) -> Option<i32> {
    let q: Vec<char> = query
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if q.is_empty() {
        return Some(0);
    }
    let p: Vec<char> = path.to_lowercase().chars().collect();
    let name_start = path
        .rfind('/')
        .map(|i| path[..=i].chars().count())
        .unwrap_or(0);
    let mut score = 0i32;
    let mut qi = 0;
    let mut prev: Option<usize> = None;
    for (i, c) in p.iter().enumerate() {
        if qi < q.len() && *c == q[qi] {
            let mut s = 1;
            if prev == Some(i.wrapping_sub(1)) {
                s += 5;
            }
            if i == 0 || matches!(p[i - 1], '/' | '_' | '-' | '.' | ' ') {
                s += 4;
            }
            if i >= name_start {
                s += 2;
            }
            score += s;
            prev = Some(i);
            qi += 1;
        }
    }
    if qi < q.len() {
        return None;
    }
    // Shorter paths first among equals; an exact substring wins.
    if path.to_lowercase().contains(&query.to_lowercase()) {
        score += 20;
    }
    Some(score * 4 - (p.len() as i32).min(200) / 8)
}

/// The best `n` paths for `query`.
pub fn suggest<'a>(query: &str, paths: &'a [String], n: usize) -> Vec<&'a str> {
    let mut scored: Vec<(i32, &str)> = paths
        .iter()
        .filter_map(|p| fuzzy_score(query, p).map(|s| (s, p.as_str())))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.len().cmp(&b.1.len())));
    scored.into_iter().take(n).map(|(_, p)| p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renames() {
        assert_eq!(
            split_rename("src/{old => new}/file.rs"),
            (Some("src/old/file.rs".into()), "src/new/file.rs".into())
        );
        assert_eq!(
            split_rename("src/new/{file.rs => renamed.rs}"),
            (Some("src/new/file.rs".into()), "src/new/renamed.rs".into())
        );
        assert_eq!(
            split_rename("src/new/renamed.rs => top.rs"),
            (Some("src/new/renamed.rs".into()), "top.rs".into())
        );
        assert_eq!(
            split_rename("{ => sub}/x.rs"),
            (Some("x.rs".into()), "sub/x.rs".into())
        );
        assert_eq!(split_rename("plain.rs"), (None, "plain.rs".into()));
    }

    /// Real output of `git log --follow -M --numstat` over a file moved
    /// three times.
    const LOG: &str = "\u{1e}bbd9\u{1f}229c\u{1f}T\u{1f}t@e.com\u{1f}1793041752\u{1f}move to top\n\n0\t0\tsrc/new/renamed.rs => top.rs\n\u{1e}229c\u{1f}7977\u{1f}T\u{1f}t@e.com\u{1f}1793041751\u{1f}rename+edit\n\n1\t0\tsrc/new/{file.rs => renamed.rs}\n\u{1e}7977\u{1f}35a7\u{1f}T\u{1f}t@e.com\u{1f}1793041751\u{1f}move dir\n\n0\t0\tsrc/{old => new}/file.rs\n\u{1e}35a7\u{1f}455b\u{1f}U\u{1f}u@e.com\u{1f}1793041751\u{1f}edit file\n\n1\t0\tsrc/old/file.rs\n\u{1e}455b\u{1f}\u{1f}T\u{1f}t@e.com\u{1f}1793041751\u{1f}add file\n\n6\t0\tsrc/old/file.rs\n";

    #[test]
    fn parses_a_followed_history() {
        let (c, truncated) = parse(LOG, "top.rs", 100);
        assert!(!truncated);
        assert_eq!(c.len(), 5);
        assert_eq!(c[0].path, "top.rs");
        assert_eq!(c[0].renamed_from.as_deref(), Some("src/new/renamed.rs"));
        assert_eq!(c[1].path, "src/new/renamed.rs");
        assert_eq!(c[1].renamed_from.as_deref(), Some("src/new/file.rs"));
        assert_eq!(c[1].added, 1);
        assert_eq!(c[2].renamed_from.as_deref(), Some("src/old/file.rs"));
        assert_eq!(c[3].path, "src/old/file.rs");
        assert_eq!(c[3].renamed_from, None);
        assert_eq!(c[4].parents, Vec::<String>::new());
        assert_eq!(c[4].added, 6);
        let mut h = FileHistory {
            path: "top.rs".into(),
            commits: c,
            ..Default::default()
        };
        finish(&mut h);
        assert_eq!(h.renames(), 3);
        assert_eq!(h.churn(), (8, 0));
        assert_eq!(h.authors[0], ("T".to_owned(), 4));
        assert_eq!(
            h.names,
            vec![
                "top.rs",
                "src/new/renamed.rs",
                "src/new/file.rs",
                "src/old/file.rs"
            ]
        );
        let (capped, truncated) = parse(LOG, "top.rs", 2);
        assert_eq!(capped.len(), 2);
        assert!(truncated);
    }

    #[test]
    fn binary_and_merge_records() {
        let log = "\u{1e}a\u{1f}p1 p2\u{1f}M\u{1f}m@x\u{1f}5\u{1f}Merge\n\u{1e}b\u{1f}p\u{1f}B\u{1f}b@x\u{1f}4\u{1f}Image\n\n-\t-\tlogo.png\n";
        let (c, _) = parse(log, "logo.png", 10);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].parents.len(), 2);
        assert!(!c[0].binary);
        assert!(c[1].binary);
    }

    #[test]
    fn fuzzy() {
        let paths: Vec<String> = [
            "crates/gitvis/src/app.rs",
            "crates/gitvis-core/src/repo.rs",
            "crates/gitvis/src/views/kit.rs",
            "README.md",
            "apps/rust.rs",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        assert_eq!(
            suggest("app.rs", &paths, 1),
            vec!["crates/gitvis/src/app.rs"]
        );
        assert_eq!(
            suggest("kit", &paths, 1),
            vec!["crates/gitvis/src/views/kit.rs"]
        );
        assert_eq!(
            suggest("crrepo", &paths, 1),
            vec!["crates/gitvis-core/src/repo.rs"]
        );
        assert!(fuzzy_score("zzz", "README.md").is_none());
        assert_eq!(suggest("", &paths, 10).len(), 5);
    }

    #[test]
    fn show_command() {
        assert_eq!(
            show_args("abc", "top.rs", Some("old.rs")),
            vec![
                "-c",
                "core.quotepath=false",
                "show",
                "--format=",
                "-M",
                "abc",
                "--",
                "top.rs",
                "old.rs"
            ]
        );
    }
}
