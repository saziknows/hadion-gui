//! `git diff --raw --numstat -M -z` (and `git log --raw --numstat -z`):
//! the raw records give each file's change kind and modes, the numstat
//! records its added and deleted lines (`-` for binary files). With `-z`
//! no path is quoted, so names with spaces, tabs or non-ASCII characters
//! come through as they are.

/// How a file changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Added,
    Modified,
    Deleted,
    /// Renamed, with git's similarity in percent.
    Renamed(u8),
    /// Copied, with the similarity in percent.
    Copied(u8),
    /// File type changed (file ↔ symlink ↔ submodule).
    TypeChanged,
    Unmerged,
    Unknown,
}

impl Change {
    fn from_status(s: &str) -> Change {
        let score = s.get(1..).and_then(|n| n.parse::<u8>().ok()).unwrap_or(0);
        match s.chars().next() {
            Some('A') => Change::Added,
            Some('M') => Change::Modified,
            Some('D') => Change::Deleted,
            Some('R') => Change::Renamed(score),
            Some('C') => Change::Copied(score),
            Some('T') => Change::TypeChanged,
            Some('U') => Change::Unmerged,
            _ => Change::Unknown,
        }
    }

    /// One letter, as `git status` shows it.
    pub fn letter(self) -> &'static str {
        match self {
            Change::Added => "A",
            Change::Modified => "M",
            Change::Deleted => "D",
            Change::Renamed(_) => "R",
            Change::Copied(_) => "C",
            Change::TypeChanged => "T",
            Change::Unmerged => "U",
            Change::Unknown => "?",
        }
    }

    pub fn label(self) -> String {
        match self {
            Change::Added => "Added".into(),
            Change::Modified => "Modified".into(),
            Change::Deleted => "Deleted".into(),
            Change::Renamed(s) => format!("Renamed ({s}% similar)"),
            Change::Copied(s) => format!("Copied ({s}% similar)"),
            Change::TypeChanged => "Type changed".into(),
            Change::Unmerged => "Unmerged".into(),
            Change::Unknown => "Changed".into(),
        }
    }
}

/// One changed file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStat {
    pub path: String,
    /// The name before a rename or copy.
    pub old_path: Option<String>,
    pub change: Change,
    pub added: u64,
    pub deleted: u64,
    pub binary: bool,
    pub old_mode: String,
    pub new_mode: String,
}

impl FileStat {
    /// The mode changed (e.g. the executable bit).
    pub fn mode_changed(&self) -> bool {
        self.change == Change::Modified
            && !self.old_mode.is_empty()
            && self.old_mode != self.new_mode
    }

    /// A submodule (mode 160000) on either side.
    pub fn submodule(&self) -> bool {
        self.old_mode == "160000" || self.new_mode == "160000"
    }
}

/// Parse the NUL-separated output of `--raw --numstat -z` (either part may
/// be missing). Files keep the order of the raw records; numstat-only
/// output (no `--raw`) still yields files, as modified.
pub fn parse(bytes: &[u8]) -> Vec<FileStat> {
    let tokens: Vec<String> = bytes
        .split(|&b| b == 0)
        .map(|t| String::from_utf8_lossy(t).into_owned())
        .collect();
    let mut files: Vec<FileStat> = Vec::new();
    let mut by_path: std::collections::HashMap<String, usize> = Default::default();
    let mut i = 0;
    while i < tokens.len() {
        // A commit header from `git log` may sit before the first record
        // with a newline; records always start a token.
        let tok = tokens[i].trim_start_matches(['\n', '\r']);
        if tok.is_empty() {
            i += 1;
            continue;
        }
        if let Some(raw) = tok.strip_prefix(':') {
            // :old_mode new_mode old_sha new_sha STATUS
            let f: Vec<&str> = raw.split_whitespace().collect();
            let status = f.get(4).copied().unwrap_or("");
            let change = Change::from_status(status);
            let (old_path, path, used) = match change {
                Change::Renamed(_) | Change::Copied(_) => (
                    tokens.get(i + 1).cloned(),
                    tokens.get(i + 2).cloned().unwrap_or_default(),
                    3,
                ),
                _ => (None, tokens.get(i + 1).cloned().unwrap_or_default(), 2),
            };
            by_path.insert(path.clone(), files.len());
            files.push(FileStat {
                path,
                old_path,
                change,
                added: 0,
                deleted: 0,
                binary: false,
                old_mode: f.first().copied().unwrap_or("").to_owned(),
                new_mode: f.get(1).copied().unwrap_or("").to_owned(),
            });
            i += used;
            continue;
        }
        // added \t deleted \t path   (path empty for renames: old, new follow)
        let mut parts = tok.splitn(3, '\t');
        let (Some(a), Some(d), Some(p)) = (parts.next(), parts.next(), parts.next()) else {
            i += 1;
            continue;
        };
        let (old, path, used) = if p.is_empty() {
            (
                tokens.get(i + 1).cloned(),
                tokens.get(i + 2).cloned().unwrap_or_default(),
                3,
            )
        } else {
            (None, p.to_owned(), 1)
        };
        let binary = a == "-" && d == "-";
        let added = a.parse().unwrap_or(0);
        let deleted = d.parse().unwrap_or(0);
        match by_path.get(&path) {
            Some(&k) => {
                let f = &mut files[k];
                f.added = added;
                f.deleted = deleted;
                f.binary = binary;
                if f.old_path.is_none() {
                    f.old_path = old;
                }
            }
            None => {
                by_path.insert(path.clone(), files.len());
                files.push(FileStat {
                    change: if old.is_some() {
                        Change::Renamed(0)
                    } else {
                        Change::Modified
                    },
                    path,
                    old_path: old,
                    added,
                    deleted,
                    binary,
                    old_mode: String::new(),
                    new_mode: String::new(),
                });
            }
        }
        i += used;
    }
    files
}

/// Lines added and deleted over every file.
pub fn totals(files: &[FileStat]) -> (u64, u64) {
    files
        .iter()
        .fold((0, 0), |(a, d), f| (a + f.added, d + f.deleted))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = b":100644 100644 4aff34c d393875 M\0CLAUDE.md\0:000000 100644 0000000 7227e6d A\0src/new file.rs\0:100644 100644 1111111 2222222 R086\0old/name.rs\0new/name.rs\0:100644 000000 3333333 0000000 D\0gone.txt\0:100644 100644 4444444 5555555 M\0logo.png\0:100644 100755 6666666 6666666 M\0run.sh\0\
14\t9\tCLAUDE.md\0230\t0\tsrc/new file.rs\03\t1\t\0old/name.rs\0new/name.rs\00\t12\tgone.txt\0-\t-\tlogo.png\00\t0\trun.sh\0";

    #[test]
    fn raw_and_numstat_merge() {
        let f = parse(SAMPLE);
        assert_eq!(f.len(), 6);
        assert_eq!(f[0].path, "CLAUDE.md");
        assert_eq!((f[0].added, f[0].deleted), (14, 9));
        assert_eq!(f[0].change, Change::Modified);
        assert_eq!(f[1].path, "src/new file.rs");
        assert_eq!(f[1].change, Change::Added);
        assert_eq!(f[1].added, 230);
        assert_eq!(f[2].change, Change::Renamed(86));
        assert_eq!(f[2].old_path.as_deref(), Some("old/name.rs"));
        assert_eq!(f[2].path, "new/name.rs");
        assert_eq!((f[2].added, f[2].deleted), (3, 1));
        assert_eq!(f[3].change, Change::Deleted);
        assert_eq!(f[3].deleted, 12);
        assert!(f[4].binary);
        assert!(f[5].mode_changed());
        assert_eq!(totals(&f), (247, 22));
    }

    #[test]
    fn numstat_only() {
        let f = parse(b"1\t2\ta.txt\05\t0\t\0x.rs\0y.rs\0");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].change, Change::Modified);
        assert_eq!(f[1].change, Change::Renamed(0));
        assert_eq!(f[1].old_path.as_deref(), Some("x.rs"));
        assert_eq!(f[1].path, "y.rs");
    }

    #[test]
    fn empty_and_letters() {
        assert!(parse(b"").is_empty());
        assert_eq!(Change::from_status("C075"), Change::Copied(75));
        assert_eq!(Change::Copied(75).letter(), "C");
        assert_eq!(Change::Renamed(90).label(), "Renamed (90% similar)");
    }
}
