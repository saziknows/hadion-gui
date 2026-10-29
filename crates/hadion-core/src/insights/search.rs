//! History search: pickaxe (`git log -S`, occurrences of a string
//! added/removed), regex (`-G`), and message search (`--grep`), with the
//! paths each hit touched.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::{Error, Oid, Result, cli};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SearchMode {
    /// `-S`: commits that change the number of occurrences of the text.
    #[default]
    Pickaxe,
    /// `-G`: commits whose diff lines match the regex.
    Regex,
    /// `--grep`: commit messages matching the regex.
    Message,
    /// `--author`.
    Author,
}

impl SearchMode {
    pub const ALL: [SearchMode; 4] = [
        SearchMode::Pickaxe,
        SearchMode::Regex,
        SearchMode::Message,
        SearchMode::Author,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SearchMode::Pickaxe => "Text (-S)",
            SearchMode::Regex => "Diff regex (-G)",
            SearchMode::Message => "Message",
            SearchMode::Author => "Author",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            SearchMode::Pickaxe => {
                "Commits where the number of occurrences of this exact text changed: when it was introduced, moved or removed"
            }
            SearchMode::Regex => {
                "Commits whose added or removed lines match this regular expression"
            }
            SearchMode::Message => "Commits whose message matches this regular expression",
            SearchMode::Author => "Commits whose author name or e-mail matches",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SearchQuery {
    pub text: String,
    pub mode: SearchMode,
    /// `--all` instead of HEAD.
    pub all_refs: bool,
    /// Limit to a path or directory.
    pub path: Option<String>,
    pub ignore_case: bool,
    /// Maximum hits to return (0 = 500).
    pub max: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub id: Oid,
    pub time: i64,
    pub author: String,
    pub summary: String,
    /// Files the commit touched that match the search (`--name-only`).
    pub paths: Vec<String>,
    /// Branches/tags containing the hit are not resolved here (use
    /// `repo::branches_containing` on demand).
    pub is_merge: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SearchResult {
    pub query: SearchQuery,
    /// Newest first.
    pub hits: Vec<SearchHit>,
    /// More hits exist beyond `max`.
    pub truncated: bool,
    pub elapsed_ms: u64,
    pub command: String,
    pub cancelled: bool,
    /// First (oldest) and last (newest) hit times, when any.
    pub span: Option<(i64, i64)>,
    /// Distinct paths across hits, desc by hits: `(path, hits)`.
    pub paths: Vec<(String, u32)>,
    /// Distinct authors, desc.
    pub authors: Vec<(String, u32)>,
}

const DEFAULT_MAX: usize = 500;

/// Run the search with `git log … --name-only --format=…`, streaming so a
/// pickaxe over a large history can be cancelled. Never puts the query
/// into the activity log with secrets: the caller logs `command`.
pub fn search(workdir: &Path, query: &SearchQuery, cancel: &AtomicBool) -> Result<SearchResult> {
    if query.text.trim().is_empty() {
        return Err(Error::new("Nothing to search for"));
    }
    let started = Instant::now();
    let max = if query.max == 0 {
        DEFAULT_MAX
    } else {
        query.max
    };
    let args = build_args(query);
    let mut out = SearchResult {
        query: query.clone(),
        command: render(&args),
        ..Default::default()
    };

    let mut child = cli::spawn(workdir, &args)?;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut reader = BufReader::with_capacity(1 << 16, stdout);
    let mut buf: Vec<u8> = Vec::new();
    let mut stopped = false;
    loop {
        buf.clear();
        let n = match reader.read_until(0x1e, &mut buf) {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 {
            break;
        }
        let ended = buf.last() == Some(&0x1e);
        if ended {
            buf.pop();
        }
        // The separator opens each record, so the chunk before it is the
        // previous record (empty before the first one).
        if let Some(hit) = parse_record(&buf) {
            if out.hits.len() >= max {
                // A further record exists beyond the cap.
                out.truncated = true;
                stopped = true;
                break;
            }
            out.hits.push(hit);
        }
        if !ended {
            break;
        }
        if cancel.load(Ordering::Relaxed) {
            out.cancelled = true;
            stopped = true;
            break;
        }
    }
    if stopped {
        let _ = child.kill();
    }
    let _ = child.wait();

    // Aggregates.
    let mut paths: HashMap<&str, u32> = HashMap::new();
    let mut authors: HashMap<&str, u32> = HashMap::new();
    for hit in &out.hits {
        *authors.entry(hit.author.as_str()).or_insert(0) += 1;
        for p in &hit.paths {
            *paths.entry(p.as_str()).or_insert(0) += 1;
        }
    }
    out.paths = sorted_counts(paths);
    out.authors = sorted_counts(authors);
    out.span = out
        .hits
        .iter()
        .map(|h| h.time)
        .fold(None, |acc, t| match acc {
            None => Some((t, t)),
            Some((lo, hi)) => Some((lo.min(t), hi.max(t))),
        });
    out.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(out)
}

/// The `git log` command line for a query.
fn build_args(query: &SearchQuery) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-c".into(),
        "core.quotepath=false".into(),
        "log".into(),
        "--format=%x1e%H%x1f%P%x1f%an%x1f%at%x1f%s".into(),
        "--name-only".into(),
    ];
    let text = &query.text;
    match query.mode {
        SearchMode::Pickaxe => args.push(format!("-S{text}")),
        SearchMode::Regex => args.push(format!("-G{text}")),
        SearchMode::Message => args.push(format!("--grep={text}")),
        SearchMode::Author => args.push(format!("--author={text}")),
    }
    if query.ignore_case {
        // Applies to -G, --grep and --author; a fixed-string -S folds
        // case with it too (no --pickaxe-regex needed, which would turn
        // the text into a regex).
        args.push("--regexp-ignore-case".into());
    }
    if query.all_refs {
        args.push("--all".into());
    } else {
        args.push("HEAD".into());
    }
    if let Some(path) = query
        .path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        args.push("--".into());
        args.push(path.to_owned());
    }
    args
}

/// `git …` with arguments quoted for display when they contain spaces or
/// quotes.
fn render(args: &[String]) -> String {
    let mut s = String::from("git");
    for a in args {
        s.push(' ');
        if a.is_empty() || a.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
            s.push('"');
            s.push_str(&a.replace('\\', "\\\\").replace('"', "\\\""));
            s.push('"');
        } else {
            s.push_str(a);
        }
    }
    s
}

/// One record: `<H>\x1f<P>\x1f<an>\x1f<at>\x1f<s>\n\n<path>\n<path>…`.
fn parse_record(chunk: &[u8]) -> Option<SearchHit> {
    let text = String::from_utf8_lossy(chunk);
    let text = text.trim_start_matches(['\n', '\r']);
    if text.is_empty() {
        return None;
    }
    let (header, rest) = text.split_once('\n').unwrap_or((text, ""));
    let mut f = header.trim_end_matches('\r').splitn(5, '\x1f');
    let id = Oid::from_str(f.next()?.trim()).ok()?;
    let parents = f.next()?.split_whitespace().count();
    let author = f.next()?.to_owned();
    let time = f.next()?.trim().parse().unwrap_or(0);
    let summary = f.next().unwrap_or("").trim().to_owned();
    let paths = rest
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    Some(SearchHit {
        id,
        time,
        author,
        summary,
        paths,
        is_merge: parents > 1,
    })
}

fn sorted_counts(map: HashMap<&str, u32>) -> Vec<(String, u32)> {
    let mut v: Vec<(String, u32)> = map.into_iter().map(|(k, n)| (k.to_owned(), n)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_commands() {
        let q = SearchQuery {
            text: "needle".into(),
            mode: SearchMode::Pickaxe,
            ..Default::default()
        };
        let args = build_args(&q);
        assert!(args.contains(&"-Sneedle".to_owned()));
        assert_eq!(args.last().map(String::as_str), Some("HEAD"));
        assert!(!args.iter().any(|a| a == "--regexp-ignore-case"));

        let q = SearchQuery {
            text: "fix.*login".into(),
            mode: SearchMode::Message,
            all_refs: true,
            path: Some(" src/auth ".into()),
            ignore_case: true,
            max: 10,
        };
        let args = build_args(&q);
        assert!(args.contains(&"--grep=fix.*login".to_owned()));
        assert!(args.contains(&"--regexp-ignore-case".to_owned()));
        assert!(args.contains(&"--all".to_owned()));
        assert_eq!(&args[args.len() - 2..], ["--", "src/auth"]);
        let cmd = render(&args);
        assert!(cmd.starts_with("git -c core.quotepath=false log "));
        assert!(cmd.contains("--grep=fix.*login"));

        let q = SearchQuery {
            text: "two words".into(),
            mode: SearchMode::Regex,
            ..Default::default()
        };
        assert!(render(&build_args(&q)).contains("\"-Gtwo words\""));
        let q = SearchQuery {
            text: "Ada".into(),
            mode: SearchMode::Author,
            ..Default::default()
        };
        assert!(build_args(&q).contains(&"--author=Ada".to_owned()));
    }

    #[test]
    fn parses_records() {
        let rec = b"0123456789abcdef0123456789abcdef01234567\x1faaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\x1fAda\x1f1700000000\x1fAdd thing\n\nsrc/a.rs\nsrc/b.rs\n";
        let h = parse_record(rec).expect("parses");
        assert_eq!(h.author, "Ada");
        assert_eq!(h.time, 1_700_000_000);
        assert_eq!(h.summary, "Add thing");
        assert!(h.is_merge);
        assert_eq!(h.paths, ["src/a.rs", "src/b.rs"]);
        let no_files = b"\n0123456789abcdef0123456789abcdef01234567\x1f\x1fAda\x1f5\x1fRoot\n";
        let h = parse_record(no_files).expect("parses");
        assert!(h.paths.is_empty());
        assert!(!h.is_merge);
        assert!(parse_record(b"").is_none());
        assert!(parse_record(b"\n").is_none());
    }

    #[test]
    fn empty_query_is_an_error() {
        let q = SearchQuery {
            text: "   ".into(),
            ..Default::default()
        };
        assert!(search(Path::new("."), &q, &AtomicBool::new(false)).is_err());
    }
}
