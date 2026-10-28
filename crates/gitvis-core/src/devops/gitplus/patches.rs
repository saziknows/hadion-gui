//! Patches: `git format-patch -o DIR` for a range or the last N commits,
//! and applying patch files: a preview (`git apply --numstat --summary`,
//! `git apply --check`, the mails of a mailbox), then `git apply --3way` or
//! `git am --3way`.

use std::path::{Path, PathBuf};

use super::{git, git_raw, output_text, parse_numstat, unquote};

/// Which commits to export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// `A..B`, or one revision (`A` = the commits after A up to HEAD).
    Range(String),
    /// The last N commits of the current branch.
    Last(usize),
}

impl Source {
    /// The revision arguments of `git format-patch` / `git log`.
    pub fn rev_args(&self) -> Vec<String> {
        match self {
            Source::Range(r) => vec![r.trim().to_owned()],
            Source::Last(n) => vec![format!("-{n}")],
        }
    }

    /// `git log` arguments listing the same commits, oldest first.
    pub fn log_args(&self) -> Vec<String> {
        let mut v: Vec<String> = vec![
            "log".into(),
            "--no-merges".into(),
            "--format=%H%x1f%h%x1f%s%x1f%an".into(),
        ];
        match self {
            Source::Range(r) => {
                let r = r.trim();
                if r.contains("..") {
                    v.push(r.to_owned());
                } else {
                    v.push(format!("{r}..HEAD"));
                }
            }
            Source::Last(n) => {
                v.push(format!("-{n}"));
                v.push("HEAD".into());
            }
        }
        v
    }

    pub fn describe(&self) -> String {
        match self {
            Source::Range(r) if r.contains("..") => r.trim().to_owned(),
            Source::Range(r) => format!("{}..HEAD", r.trim()),
            Source::Last(1) => "the last commit".to_owned(),
            Source::Last(n) => format!("the last {n} commits"),
        }
    }
}

/// `git format-patch -o DIR <range | -N>`.
pub fn format_patch_args(out_dir: &Path, source: &Source) -> Vec<String> {
    let mut v: Vec<String> = vec![
        "format-patch".into(),
        "-o".into(),
        out_dir.to_string_lossy().into_owned(),
    ];
    v.extend(source.rev_args());
    v
}

/// A commit that will be exported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RangeCommit {
    pub sha: String,
    pub short: String,
    pub subject: String,
    pub author: String,
}

/// Parse the [`Source::log_args`] output (newest first) into oldest first.
pub fn parse_range_log(text: &str) -> Vec<RangeCommit> {
    let mut v: Vec<RangeCommit> = text
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(4, '\u{1f}').collect();
            (f.len() == 4).then(|| RangeCommit {
                sha: f[0].to_owned(),
                short: f[1].to_owned(),
                subject: f[2].to_owned(),
                author: f[3].to_owned(),
            })
        })
        .collect();
    v.reverse();
    v
}

/// The commits `source` covers (at most `cap`).
pub fn range_commits(
    dir: &Path,
    source: &Source,
    cmds: &mut Vec<String>,
) -> Result<Vec<RangeCommit>, String> {
    if let Source::Range(r) = source
        && r.trim().is_empty()
    {
        return Err("Enter a range such as main..feature, or pick the last N commits.".into());
    }
    if let Source::Last(0) = source {
        return Err("Pick at least one commit.".into());
    }
    let text = git(dir, &source.log_args(), cmds)?;
    let v = parse_range_log(&text);
    if v.is_empty() {
        return Err(format!("No commits in {}.", source.describe()));
    }
    Ok(v)
}

/// Paths `git format-patch` printed (one per line).
pub fn parse_created(stdout: &str, cwd: &Path) -> Vec<PathBuf> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| super::resolve_in(cwd, l))
        .collect()
}

/// Run format-patch; the files it wrote.
pub fn create(
    dir: &Path,
    out_dir: &Path,
    source: &Source,
    cmds: &mut Vec<String>,
) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(out_dir)
        .map_err(|e| format!("Could not create {}: {e}", out_dir.display()))?;
    let text = git(dir, &format_patch_args(out_dir, source), cmds)?;
    Ok(parse_created(&text, dir))
}

/// One mail of a mailbox (`git format-patch` output).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mail {
    /// The commit in the `From <sha> Mon Sep 17 …` line.
    pub sha: Option<String>,
    pub from: String,
    pub date: String,
    /// The subject without `[PATCH n/m]`.
    pub subject: String,
    /// `1/3` from `[PATCH 1/3]`.
    pub number: Option<String>,
}

/// Decode RFC 2047 `=?UTF-8?q?…?=` words (git's encoding of non-ASCII
/// names and subjects); other encodings are left as they are.
pub fn decode_words(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    let mut after_word = false;
    while let Some(start) = rest.find("=?") {
        let mut parts = rest[start + 2..].splitn(3, '?');
        let (Some(charset), Some(enc), Some(tail)) = (parts.next(), parts.next(), parts.next())
        else {
            break;
        };
        let Some(end) = tail.find("?=") else {
            break;
        };
        if !charset.eq_ignore_ascii_case("utf-8") || !enc.eq_ignore_ascii_case("q") {
            break;
        }
        let between = &rest[..start];
        // Whitespace between two encoded words is dropped.
        if !(after_word && between.trim().is_empty()) {
            out.push_str(between);
        }
        out.push_str(&decode_q(&tail[..end]));
        after_word = true;
        rest = &tail[end + 2..];
    }
    out.push_str(rest);
    out
}

/// The `Q` encoding: `_` is a space, `=XX` a byte.
fn decode_q(text: &str) -> String {
    let b = text.as_bytes();
    let mut bytes = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'_' => bytes.push(b' '),
            b'=' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(v) => {
                        bytes.push(v);
                        i += 2;
                    }
                    Err(_) => bytes.push(b'='),
                }
            }
            c => bytes.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Is this a mailbox (mail headers) rather than a bare diff?
pub fn is_mailbox(text: &str) -> bool {
    let head = match text.find("\ndiff --git ") {
        Some(i) => &text[..i],
        None => text,
    };
    (text.starts_with("From ") || head.contains("\nFrom: ") || head.starts_with("From: "))
        && (head.contains("\nSubject: ") || head.starts_with("Subject: "))
}

/// The mails of a mailbox, in order.
pub fn parse_mbox(text: &str) -> Vec<Mail> {
    let mut out: Vec<Mail> = Vec::new();
    let mut cur: Option<Mail> = None;
    let mut in_headers = false;
    let mut last_header = String::new();
    let mut prev_blank = true;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with("From ") && prev_blank && !in_headers {
            if let Some(m) = cur.take() {
                out.push(m);
            }
            let sha = line
                .split_whitespace()
                .nth(1)
                .filter(|s| s.len() >= 7 && s.chars().all(|c| c.is_ascii_hexdigit()))
                .map(str::to_owned);
            cur = Some(Mail {
                sha,
                ..Default::default()
            });
            in_headers = true;
            prev_blank = false;
            continue;
        }
        prev_blank = line.is_empty();
        if cur.is_none() && (line.starts_with("From: ") || line.starts_with("Subject: ")) {
            cur = Some(Mail::default());
            in_headers = true;
        }
        let Some(m) = cur.as_mut() else { continue };
        if !in_headers {
            continue;
        }
        if line.is_empty() {
            in_headers = false;
            continue;
        }
        if line.starts_with([' ', '\t']) {
            // A folded header line.
            let more = line.trim();
            match last_header.as_str() {
                "subject" => {
                    m.subject.push(' ');
                    m.subject.push_str(more);
                }
                "from" => {
                    m.from.push(' ');
                    m.from.push_str(more);
                }
                _ => {}
            }
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            last_header = name.trim().to_ascii_lowercase();
            let value = value.trim().to_owned();
            match last_header.as_str() {
                "from" => m.from = value,
                "date" => m.date = value,
                "subject" => m.subject = value,
                _ => {}
            }
        }
    }
    if let Some(m) = cur.take() {
        out.push(m);
    }
    for m in &mut out {
        m.from = decode_words(&m.from);
        let subject = decode_words(&m.subject);
        let (number, subject) = strip_patch_prefix(&subject);
        m.number = number;
        m.subject = subject;
    }
    out
}

/// `[PATCH 2/3] Fix x` → (`2/3`, `Fix x`); `[PATCH] y` → (None, `y`).
pub fn strip_patch_prefix(subject: &str) -> (Option<String>, String) {
    let s = subject.trim();
    if let Some(rest) = s.strip_prefix('[')
        && let Some(end) = rest.find(']')
    {
        let tag = &rest[..end];
        if tag.to_ascii_uppercase().contains("PATCH") {
            let number = tag
                .split_whitespace()
                .find(|w| w.contains('/') && w.chars().next().is_some_and(|c| c.is_ascii_digit()))
                .map(str::to_owned);
            return (number, rest[end + 1..].trim().to_owned());
        }
    }
    (None, s.to_owned())
}

/// The `--summary` lines of `git apply` (` create mode 100644 x`).
pub fn parse_summary(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| l.starts_with(' '))
        .map(|l| l.trim().to_owned())
        .filter(|l| {
            l.starts_with("create mode")
                || l.starts_with("delete mode")
                || l.starts_with("rename ")
                || l.starts_with("copy ")
                || l.starts_with("mode change")
                || l.starts_with("rewrite ")
        })
        .collect()
}

/// A file a patch changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStat {
    pub path: String,
    pub added: Option<u64>,
    pub deleted: Option<u64>,
}

/// Whether the patch goes in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applies {
    /// `git apply --check` passed.
    Cleanly,
    /// It does not apply as is, but `--3way` can merge it (conflicts are
    /// possible); the error of the plain check.
    WithThreeWay(String),
    /// Neither; the error.
    No(String),
}

/// What a patch file holds and whether it applies here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    pub path: PathBuf,
    pub bytes: u64,
    pub mails: Vec<Mail>,
    pub files: Vec<FileStat>,
    pub summary: Vec<String>,
    pub applies: Applies,
}

impl Preview {
    pub fn is_mailbox(&self) -> bool {
        !self.mails.is_empty()
    }

    pub fn added(&self) -> u64 {
        self.files.iter().filter_map(|f| f.added).sum()
    }

    pub fn deleted(&self) -> u64 {
        self.files.iter().filter_map(|f| f.deleted).sum()
    }
}

fn path_arg(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

pub fn numstat_args(file: &Path) -> Vec<String> {
    vec![
        "apply".into(),
        "--numstat".into(),
        "--summary".into(),
        path_arg(file),
    ]
}

pub fn check_args(file: &Path, three_way: bool) -> Vec<String> {
    let mut v: Vec<String> = vec!["apply".into(), "--check".into()];
    if three_way {
        v.push("--3way".into());
    }
    v.push(path_arg(file));
    v
}

/// Read a patch file and check it against this repository.
pub fn preview(dir: &Path, file: &Path, cmds: &mut Vec<String>) -> Result<Preview, String> {
    let bytes =
        std::fs::read(file).map_err(|e| format!("Could not read {}: {e}", file.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    let mails = if is_mailbox(&text) {
        parse_mbox(&text)
    } else {
        Vec::new()
    };
    let out = git_raw(dir, &numstat_args(file), &[], cmds)?;
    if !out.success {
        let e = output_text(&out);
        return Err(if e.is_empty() {
            format!("{} is not a patch git can read.", file.display())
        } else {
            e
        });
    }
    let stat_text = String::from_utf8_lossy(&out.stdout);
    let files: Vec<FileStat> = parse_numstat(&stat_text)
        .into_iter()
        .map(|(path, added, deleted)| FileStat {
            path: unquote(&path),
            added,
            deleted,
        })
        .collect();
    if files.is_empty() && mails.is_empty() {
        return Err(format!("{} has no changes git can read.", file.display()));
    }
    let summary = parse_summary(&stat_text);
    let plain = git_raw(dir, &check_args(file, false), &[], cmds)?;
    let applies = if plain.success {
        Applies::Cleanly
    } else {
        let err = output_text(&plain);
        let three = git_raw(dir, &check_args(file, true), &[], cmds)?;
        if three.success {
            Applies::WithThreeWay(err)
        } else {
            Applies::No(err)
        }
    };
    Ok(Preview {
        path: file.to_path_buf(),
        bytes: bytes.len() as u64,
        mails,
        files,
        summary,
        applies,
    })
}

/// `git apply [--3way] FILES…`: the changes into the working tree (with
/// `--3way`, also the index, merging where it does not apply cleanly).
pub fn apply_args(files: &[PathBuf], three_way: bool) -> Vec<String> {
    let mut v: Vec<String> = vec!["apply".into()];
    if three_way {
        v.push("--3way".into());
    }
    v.extend(files.iter().map(|f| path_arg(f)));
    v
}

/// `git am [--3way] FILES…`: one commit per mail, author and message kept.
pub fn am_args(files: &[PathBuf], three_way: bool) -> Vec<String> {
    let mut v: Vec<String> = vec!["am".into()];
    if three_way {
        v.push("--3way".into());
    }
    v.extend(files.iter().map(|f| path_arg(f)));
    v
}

/// `git am --skip` / `--abort` / `--continue`.
pub fn am_step_args(step: &str) -> Vec<String> {
    vec!["am".into(), format!("--{step}")]
}

/// `git am --show-current-patch=diff` (while `git am` is stopped).
pub fn current_patch_args() -> Vec<String> {
    vec!["am".into(), "--show-current-patch=diff".into()]
}

#[cfg(test)]
mod tests {
    use super::*;

    const MBOX: &str = "From 11350472763239071041e6ad46eca92e87b40b9e Mon Sep 17 00:00:00 2001\n\
From: Tess T <t@x>\n\
Date: Tue, 27 Oct 2026 23:45:38 +0200\n\
Subject: [PATCH 1/2] two: add new\n\
\n\
body line\n\
---\n\
 a.txt | 1 +\n\
\n\
diff --git a/a.txt b/a.txt\n\
--- a/a.txt\n\
+++ b/a.txt\n\
@@ -1 +1,2 @@\n\
 a\n\
+b\n\
-- \n\
2.55.0\n\
\n\
From 22350472763239071041e6ad46eca92e87b40b9e Mon Sep 17 00:00:00 2001\n\
From: =?UTF-8?q?Ren=C3=A9=20M?= <r@x>\n\
Date: Tue, 27 Oct 2026 23:46:00 +0200\n\
Subject: [PATCH 2/2] A subject that is long enough to be\n\
\x20folded onto a second line\n\
\n\
---\n";

    #[test]
    fn mailbox_mails() {
        assert!(is_mailbox(MBOX));
        assert!(!is_mailbox("diff --git a/x b/x\n--- a/x\n+++ b/x\n"));
        let v = parse_mbox(MBOX);
        assert_eq!(v.len(), 2);
        assert_eq!(
            v[0].sha.as_deref(),
            Some("11350472763239071041e6ad46eca92e87b40b9e")
        );
        assert_eq!(v[0].from, "Tess T <t@x>");
        assert_eq!(v[0].subject, "two: add new");
        assert_eq!(v[0].number.as_deref(), Some("1/2"));
        assert_eq!(v[1].from, "René M <r@x>");
        assert_eq!(
            v[1].subject,
            "A subject that is long enough to be folded onto a second line"
        );
    }

    #[test]
    fn subjects_and_words() {
        assert_eq!(
            strip_patch_prefix("[PATCH v2 3/7] Fix"),
            (Some("3/7".into()), "Fix".into())
        );
        assert_eq!(strip_patch_prefix("[PATCH] Only"), (None, "Only".into()));
        assert_eq!(strip_patch_prefix("[WIP] x"), (None, "[WIP] x".into()));
        assert_eq!(decode_words("plain"), "plain");
        assert_eq!(
            decode_words("=?UTF-8?q?Ren=C3=A9?= =?UTF-8?q?_M?= <r@x>"),
            "René M <r@x>"
        );
    }

    #[test]
    fn summary_lines() {
        let text = "1\t0\ta.txt\n1\t0\tnew.txt\n create mode 100644 new.txt\n rename new.txt => renamed.txt (100%)\n";
        assert_eq!(
            parse_summary(text),
            vec![
                "create mode 100644 new.txt",
                "rename new.txt => renamed.txt (100%)"
            ]
        );
    }

    #[test]
    fn sources_and_args() {
        let r = Source::Range("main..feature".into());
        assert_eq!(r.rev_args(), vec!["main..feature"]);
        assert_eq!(r.log_args().last().unwrap(), "main..feature");
        assert_eq!(
            Source::Range("v1.0".into()).log_args().last().unwrap(),
            "v1.0..HEAD"
        );
        assert_eq!(Source::Last(3).rev_args(), vec!["-3"]);
        assert_eq!(Source::Last(1).describe(), "the last commit");
        assert_eq!(
            format_patch_args(Path::new("/tmp/out dir"), &Source::Last(2)),
            vec!["format-patch", "-o", "/tmp/out dir", "-2"]
        );
        let files = vec![
            PathBuf::from("/p/0001-x.patch"),
            PathBuf::from("/p/0002-y.patch"),
        ];
        assert_eq!(
            am_args(&files, true),
            vec!["am", "--3way", "/p/0001-x.patch", "/p/0002-y.patch"]
        );
        assert_eq!(
            apply_args(&files[..1], false),
            vec!["apply", "/p/0001-x.patch"]
        );
        assert_eq!(am_step_args("skip"), vec!["am", "--skip"]);
        let log = "bbbb\u{1f}bb\u{1f}second\u{1f}T\naaaa\u{1f}aa\u{1f}first\u{1f}T\n";
        let c = parse_range_log(log);
        assert_eq!(c[0].subject, "first");
        assert_eq!(c[1].short, "bb");
        let created = parse_created("../pout/0001-a.patch\n", Path::new("/w/repo"));
        assert_eq!(
            created[0],
            Path::new("/w/repo").join("../pout/0001-a.patch")
        );
    }
}
