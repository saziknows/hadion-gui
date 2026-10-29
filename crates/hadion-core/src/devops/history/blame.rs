//! Blame of a file at any revision: `git blame -w -M -C --porcelain REV --
//! PATH`, streamed (a commit's header is printed in full only the first
//! time it appears) into one entry per line plus the commits behind them,
//! each with its `previous` version (the parent and the file's name there)
//! for "blame the previous version".

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use super::{Streamed, git_line};

/// Lines read before the rest is cut.
pub const MAX_LINES: usize = 100_000;

/// A commit some lines come from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlameCommit {
    pub sha: String,
    pub author: String,
    pub mail: String,
    pub time: i64,
    pub summary: String,
    /// The parent and the file's path in it (absent for the commit that
    /// added the file, and for boundary commits).
    pub previous: Option<(String, String)>,
    /// The commit is at the edge of the blamed range (usually the root).
    pub boundary: bool,
    /// The file's name in this commit (differs after a rename or `-C`).
    pub filename: String,
    /// Lines of the file that come from it.
    pub lines: usize,
}

impl BlameCommit {
    pub fn short(&self) -> &str {
        super::short(&self.sha)
    }

    /// Lines not yet committed (`git blame` on the working tree).
    pub fn uncommitted(&self) -> bool {
        self.sha.bytes().all(|b| b == b'0')
    }
}

/// One line of the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameLine {
    /// Index into [`Blame::commits`].
    pub commit: u32,
    /// The line's number in that commit's version of the file.
    pub orig: u32,
    /// The line's number in the blamed version (1-based).
    pub line: u32,
    pub text: String,
}

/// The blame of one file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Blame {
    pub rev: String,
    pub path: String,
    pub commits: Vec<BlameCommit>,
    pub lines: Vec<BlameLine>,
    pub truncated: bool,
    pub elapsed_ms: u64,
    pub command: String,
}

impl Blame {
    /// Oldest and newest author time over the lines.
    pub fn time_range(&self) -> Option<(i64, i64)> {
        let mut it = self.commits.iter().filter(|c| c.lines > 0).map(|c| c.time);
        let first = it.next()?;
        Some(it.fold((first, first), |(lo, hi), t| (lo.min(t), hi.max(t))))
    }

    /// 0 for the oldest line, 1 for the newest.
    pub fn age(&self, commit: u32) -> f32 {
        let Some((lo, hi)) = self.time_range() else {
            return 1.0;
        };
        let t = self.commits[commit as usize].time;
        if hi <= lo {
            1.0
        } else {
            ((t - lo) as f64 / (hi - lo) as f64) as f32
        }
    }

    /// Authors by lines, most first: `(name, lines)`.
    pub fn authors(&self) -> Vec<(String, usize)> {
        let mut by: HashMap<&str, usize> = HashMap::new();
        for c in &self.commits {
            *by.entry(c.author.as_str()).or_insert(0) += c.lines;
        }
        let mut v: Vec<(String, usize)> = by
            .into_iter()
            .filter(|(_, n)| *n > 0)
            .map(|(a, n)| (a.to_owned(), n))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    }

    /// Does line `i` start a new run of lines from one commit?
    pub fn starts_group(&self, i: usize) -> bool {
        i == 0
            || self
                .lines
                .get(i - 1)
                .zip(self.lines.get(i))
                .is_none_or(|(a, b)| a.commit != b.commit)
    }
}

pub fn args(rev: &str, path: &str) -> Vec<String> {
    vec![
        "blame".into(),
        "-w".into(),
        "-M".into(),
        "-C".into(),
        "--porcelain".into(),
        rev.to_owned(),
        "--".into(),
        path.to_owned(),
    ]
}

/// Incremental porcelain parser.
#[derive(Default)]
pub struct Parser {
    commits: Vec<BlameCommit>,
    index: HashMap<String, u32>,
    lines: Vec<BlameLine>,
    /// The commit and line numbers of the header just read.
    current: Option<(u32, u32, u32)>,
    pub binary: bool,
}

impl Parser {
    pub fn line(&mut self, raw: &str) {
        if let Some(text) = raw.strip_prefix('\t') {
            let Some((c, orig, line)) = self.current.take() else {
                return;
            };
            if text.contains('\0') {
                self.binary = true;
            }
            self.commits[c as usize].lines += 1;
            self.lines.push(BlameLine {
                commit: c,
                orig,
                line,
                text: text.trim_end_matches('\r').replace('\t', "    "),
            });
            return;
        }
        let (key, rest) = raw.split_once(' ').unwrap_or((raw, ""));
        if key.len() == 40 && key.bytes().all(|b| b.is_ascii_hexdigit()) {
            let mut nums = rest
                .split_whitespace()
                .map(|n| n.parse::<u32>().unwrap_or(0));
            let orig = nums.next().unwrap_or(0);
            let line = nums.next().unwrap_or(0);
            let idx = match self.index.get(key) {
                Some(&i) => i,
                None => {
                    let i = self.commits.len() as u32;
                    self.commits.push(BlameCommit {
                        sha: key.to_owned(),
                        ..Default::default()
                    });
                    self.index.insert(key.to_owned(), i);
                    i
                }
            };
            self.current = Some((idx, orig, line));
            return;
        }
        let Some((c, _, _)) = self.current else {
            return;
        };
        let commit = &mut self.commits[c as usize];
        match key {
            "author" => commit.author = rest.to_owned(),
            "author-mail" => {
                commit.mail = rest
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_owned()
            }
            "author-time" => commit.time = rest.trim().parse().unwrap_or(0),
            "summary" => commit.summary = rest.to_owned(),
            "boundary" => commit.boundary = true,
            "filename" => commit.filename = rest.to_owned(),
            "previous" => {
                if let Some((sha, file)) = rest.split_once(' ') {
                    commit.previous = Some((sha.to_owned(), file.to_owned()));
                }
            }
            _ => {}
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn finish(self, rev: &str, path: &str) -> Blame {
        Blame {
            rev: rev.to_owned(),
            path: path.to_owned(),
            commits: self.commits,
            lines: self.lines,
            truncated: false,
            elapsed_ms: 0,
            command: String::new(),
        }
    }
}

/// Parse a whole porcelain output (tests, small files).
pub fn parse(text: &str, rev: &str, path: &str) -> Blame {
    let mut p = Parser::default();
    for l in text.lines() {
        p.line(l);
    }
    p.finish(rev, path)
}

/// Run and stream the blame.
pub fn run(
    dir: &Path,
    rev: &str,
    path: &str,
    cancel: &AtomicBool,
    cmds: &mut Vec<String>,
) -> Result<Blame, String> {
    let started = Instant::now();
    let path = path.trim().replace('\\', "/");
    let rev = if rev.trim().is_empty() {
        "HEAD".to_owned()
    } else {
        rev.trim().to_owned()
    };
    let a = args(&rev, &path);
    let command = git_line(&a);
    cmds.push(command.clone());
    let mut proc = Streamed::start(dir, &a)?;
    let stdout = proc.child.stdout.take().ok_or("no output")?;
    let mut parser = Parser::default();
    let mut truncated = false;
    let mut killed = false;
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => return Err(format!("Could not read git blame: {e}")),
        }
        if cancel.load(Ordering::Relaxed) {
            proc.kill();
            let _ = proc.finish(true);
            return Err("cancelled".into());
        }
        let line = String::from_utf8_lossy(&buf);
        parser.line(line.trim_end_matches('\n'));
        if parser.binary {
            proc.kill();
            let _ = proc.finish(true);
            return Err(format!(
                "{path} is a binary file: there are no lines to blame."
            ));
        }
        if parser.len() >= MAX_LINES {
            truncated = true;
            killed = true;
            proc.kill();
            break;
        }
    }
    proc.finish(killed)?;
    let mut b = parser.finish(&rev, &path);
    b.truncated = truncated;
    b.command = command;
    b.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "bf9ab0dbc8183763d9043c2863edcdbcc6ab5360 1 1 2
author Ann Lee
author-mail <ann@example.com>
author-time 1000
author-tz +0200
committer Ann Lee
committer-mail <ann@example.com>
committer-time 1000
committer-tz +0200
summary Initial commit
boundary
filename Cargo.toml
\t[workspace]
bf9ab0dbc8183763d9043c2863edcdbcc6ab5360 2 2
\tresolver = \"3\"
77e11462b51748bdbe82e09c88d7d8950f193dc7 7 3 1
author Bob
author-mail <bob@example.com>
author-time 2000
author-tz +0000
committer Bob
committer-mail <bob@example.com>
committer-time 2000
committer-tz +0000
summary Add members
previous 9f7857d76089f8dcfa49d27f274c14a36d6b12f9 old/Cargo.toml
filename Cargo.toml
\tmembers = [\"a\",\t\"b\"]
bf9ab0dbc8183763d9043c2863edcdbcc6ab5360 3 4 1
filename Cargo.toml
\t
";

    #[test]
    fn parses_porcelain() {
        let b = parse(SAMPLE, "HEAD", "Cargo.toml");
        assert_eq!(b.lines.len(), 4);
        assert_eq!(b.commits.len(), 2);
        let first = &b.commits[0];
        assert_eq!(first.author, "Ann Lee");
        assert_eq!(first.mail, "ann@example.com");
        assert!(first.boundary);
        assert!(first.previous.is_none());
        assert_eq!(first.lines, 3);
        let second = &b.commits[1];
        assert_eq!(second.summary, "Add members");
        assert_eq!(
            second.previous,
            Some((
                "9f7857d76089f8dcfa49d27f274c14a36d6b12f9".into(),
                "old/Cargo.toml".into()
            ))
        );
        assert_eq!(b.lines[2].commit, 1);
        assert_eq!(b.lines[2].orig, 7);
        assert_eq!(b.lines[2].line, 3);
        assert_eq!(b.lines[2].text, "members = [\"a\",    \"b\"]");
        assert_eq!(b.lines[3].text, "");
        assert_eq!(b.lines[3].commit, 0);
        assert!(b.starts_group(0));
        assert!(!b.starts_group(1));
        assert!(b.starts_group(2));
        assert!(b.starts_group(3));
        assert_eq!(b.time_range(), Some((1000, 2000)));
        assert_eq!(b.age(0), 0.0);
        assert_eq!(b.age(1), 1.0);
        assert_eq!(
            b.authors(),
            vec![("Ann Lee".to_owned(), 3), ("Bob".to_owned(), 1)]
        );
        assert_eq!(args("abc", "a b.rs")[5..], ["abc", "--", "a b.rs"]);
    }

    #[test]
    fn binary_lines_are_flagged() {
        let mut p = Parser::default();
        p.line("bf9ab0dbc8183763d9043c2863edcdbcc6ab5360 1 1 1");
        p.line("\tPNG\0\0");
        assert!(p.binary);
        assert_eq!(p.len(), 1);
    }
}
