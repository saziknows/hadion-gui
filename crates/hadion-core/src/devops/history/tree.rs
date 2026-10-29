//! The files of a commit: `git ls-tree -r -l -z REV` into a folder tree
//! with sizes (folders sum their files), flattened into rows for a
//! virtualised list with expanded folders and a search, and a file's
//! content at the commit with binary and size guards.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::git_bytes;

/// Files larger than this are not loaded into the viewer.
pub const MAX_VIEW_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Blob,
    /// A submodule (gitlink, mode 160000).
    Commit,
    Other,
}

/// One `ls-tree` line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub mode: String,
    pub kind: EntryKind,
    pub oid: String,
    /// Bytes; `None` for submodules.
    pub size: Option<u64>,
    pub path: String,
}

impl Entry {
    pub fn executable(&self) -> bool {
        self.mode == "100755"
    }

    pub fn symlink(&self) -> bool {
        self.mode == "120000"
    }
}

pub fn ls_tree_args(rev: &str) -> Vec<String> {
    vec![
        "ls-tree".into(),
        "-r".into(),
        "-l".into(),
        "-z".into(),
        "--full-tree".into(),
        rev.to_owned(),
    ]
}

/// Parse `<mode> SP <type> SP <oid> SP+ <size> TAB <path> NUL` records.
pub fn parse_ls_tree(bytes: &[u8]) -> Vec<Entry> {
    let mut out = Vec::new();
    for rec in bytes.split(|&b| b == 0) {
        if rec.is_empty() {
            continue;
        }
        let rec = String::from_utf8_lossy(rec);
        let Some((meta, path)) = rec.split_once('\t') else {
            continue;
        };
        let f: Vec<&str> = meta.split_whitespace().collect();
        if f.len() < 4 {
            continue;
        }
        out.push(Entry {
            mode: f[0].to_owned(),
            kind: match f[1] {
                "blob" => EntryKind::Blob,
                "commit" => EntryKind::Commit,
                _ => EntryKind::Other,
            },
            oid: f[2].to_owned(),
            size: f[3].parse().ok(),
            path: path.trim_start_matches(['\n', '\r']).to_owned(),
        });
    }
    out
}

/// A folder or a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    /// Full path (`""` for the root).
    pub path: String,
    pub dir: bool,
    /// Bytes of the file, or of every file below the folder.
    pub size: u64,
    /// Files below a folder (1 for a file).
    pub files: usize,
    /// Child node indices, folders first, then by name.
    pub children: Vec<usize>,
    /// The `ls-tree` entry of a file.
    pub entry: Option<usize>,
}

/// The tree of a commit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tree {
    pub rev: String,
    pub sha: String,
    pub entries: Vec<Entry>,
    /// `nodes[0]` is the root.
    pub nodes: Vec<Node>,
    pub elapsed_ms: u64,
}

impl Tree {
    pub fn build(entries: Vec<Entry>) -> Tree {
        let mut nodes = vec![Node {
            name: String::new(),
            path: String::new(),
            dir: true,
            size: 0,
            files: 0,
            children: Vec::new(),
            entry: None,
        }];
        let mut dirs: HashMap<String, usize> = HashMap::new();
        dirs.insert(String::new(), 0);
        for (ei, e) in entries.iter().enumerate() {
            let mut parent = 0usize;
            let mut prefix = String::new();
            let parts: Vec<&str> = e.path.split('/').collect();
            for (k, part) in parts.iter().enumerate() {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                let last = k == parts.len() - 1;
                if last {
                    let idx = nodes.len();
                    nodes.push(Node {
                        name: (*part).to_owned(),
                        path: e.path.clone(),
                        dir: false,
                        size: e.size.unwrap_or(0),
                        files: 1,
                        children: Vec::new(),
                        entry: Some(ei),
                    });
                    nodes[parent].children.push(idx);
                } else {
                    parent = match dirs.get(&prefix) {
                        Some(&d) => d,
                        None => {
                            let idx = nodes.len();
                            nodes.push(Node {
                                name: (*part).to_owned(),
                                path: prefix.clone(),
                                dir: true,
                                size: 0,
                                files: 0,
                                children: Vec::new(),
                                entry: None,
                            });
                            nodes[parent].children.push(idx);
                            dirs.insert(prefix.clone(), idx);
                            idx
                        }
                    };
                }
            }
        }
        // Sizes and counts bottom-up (children always have larger indices).
        for i in (0..nodes.len()).rev() {
            if nodes[i].dir {
                let (s, f) = nodes[i].children.iter().fold((0u64, 0usize), |(s, f), &c| {
                    (s + nodes[c].size, f + nodes[c].files)
                });
                nodes[i].size = s;
                nodes[i].files = f;
            }
        }
        // Folders first, then case-insensitive names.
        let keys: Vec<(bool, String)> = nodes
            .iter()
            .map(|n| (!n.dir, n.name.to_lowercase()))
            .collect();
        for n in nodes.iter_mut() {
            n.children.sort_by(|a, b| keys[*a].cmp(&keys[*b]));
        }
        Tree {
            rev: String::new(),
            sha: String::new(),
            entries,
            nodes,
            elapsed_ms: 0,
        }
    }

    /// Total bytes and files.
    pub fn totals(&self) -> (u64, usize) {
        (self.nodes[0].size, self.nodes[0].files)
    }

    pub fn find(&self, path: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.path == path && !n.dir)
    }

    /// Rows to draw: `(node, depth)`. Without a query, children of
    /// `expanded` folders; with one, every file whose path contains it
    /// (case-insensitive) or matches its letters in order, with its folders
    /// open.
    pub fn rows(&self, expanded: &HashSet<String>, query: &str) -> Vec<(usize, usize)> {
        let q = query.trim().to_lowercase();
        let mut out = Vec::new();
        if q.is_empty() {
            self.walk(0, 0, &mut |n, _| expanded.contains(&n.path), &mut out);
            return out;
        }
        // Which nodes contain a match.
        let mut keep = vec![false; self.nodes.len()];
        for i in (1..self.nodes.len()).rev() {
            let n = &self.nodes[i];
            keep[i] = if n.dir {
                n.children.iter().any(|&c| keep[c])
            } else {
                matches(&n.path.to_lowercase(), &q)
            };
        }
        let mut stack: Vec<(usize, usize)> = self.nodes[0]
            .children
            .iter()
            .rev()
            .map(|&c| (c, 0))
            .collect();
        while let Some((i, depth)) = stack.pop() {
            if !keep[i] {
                continue;
            }
            out.push((i, depth));
            for &c in self.nodes[i].children.iter().rev() {
                stack.push((c, depth + 1));
            }
        }
        out
    }

    fn walk(
        &self,
        i: usize,
        depth: usize,
        open: &mut dyn FnMut(&Node, usize) -> bool,
        out: &mut Vec<(usize, usize)>,
    ) {
        for &c in &self.nodes[i].children {
            out.push((c, depth));
            if self.nodes[c].dir && open(&self.nodes[c], depth) {
                self.walk(c, depth + 1, open, out);
            }
        }
    }

    /// Folders on the way to `path` (to open them when a file is shown).
    pub fn ancestors(path: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut acc = String::new();
        let parts: Vec<&str> = path.split('/').collect();
        for p in &parts[..parts.len().saturating_sub(1)] {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(p);
            out.push(acc.clone());
        }
        out
    }
}

/// Substring, or the query's letters in order (`kitrs` → `src/views/kit.rs`).
fn matches(path: &str, q: &str) -> bool {
    if path.contains(q) {
        return true;
    }
    let mut it = path.chars();
    q.chars()
        .filter(|c| !c.is_whitespace())
        .all(|qc| it.any(|pc| pc == qc))
}

/// List the files of `rev`.
pub fn load(dir: &Path, rev: &str, cmds: &mut Vec<String>) -> Result<Tree, String> {
    let started = std::time::Instant::now();
    let r = super::compare::resolve(dir, rev, cmds)?;
    let out = git_bytes(dir, &ls_tree_args(&r.sha), cmds)?;
    let mut t = Tree::build(parse_ls_tree(&out));
    t.rev = rev.trim().to_owned();
    t.sha = r.sha;
    t.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(t)
}

/// A file's content at a commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Text(Vec<u8>),
    Binary {
        size: u64,
    },
    TooLarge {
        size: u64,
    },
    /// A submodule: the commit it points at.
    Submodule {
        commit: String,
    },
}

pub fn show_args(rev: &str, path: &str) -> Vec<String> {
    vec!["show".into(), format!("{rev}:{path}")]
}

/// Read `path` at `rev` unless the listing says it is too large.
pub fn content(
    dir: &Path,
    rev: &str,
    entry: &Entry,
    cmds: &mut Vec<String>,
) -> Result<Content, String> {
    if entry.kind == EntryKind::Commit {
        return Ok(Content::Submodule {
            commit: entry.oid.clone(),
        });
    }
    let size = entry.size.unwrap_or(0);
    if size > MAX_VIEW_BYTES {
        return Ok(Content::TooLarge { size });
    }
    let bytes = git_bytes(dir, &show_args(rev, &entry.path), cmds)?;
    Ok(if crate::diff::looks_binary(&bytes) {
        Content::Binary {
            size: bytes.len() as u64,
        }
    } else {
        Content::Text(bytes)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LS: &[u8] = b"100644 blob 2b6265df09537c9d8c4d65d216118185029eb348      45\t.gitattributes\x00100644 blob d393875424cdbaa1effed3405373f4591950da98   25711\tsrc/views/kit.rs\x00100755 blob 051445f071b3f5a8b5fcbb73b38428b22a440895     100\tsrc/run me.sh\x00160000 commit 1111111111111111111111111111111111111111       -\tvendor/lib\x00100644 blob f267bc2647ac82d765783bf275bf580d15d352b1     782\tsrc/views/graph.rs\x00100644 blob 0000000000000000000000000000000000000000     10\tREADME\0";

    #[test]
    fn parses_listing() {
        let e = parse_ls_tree(LS);
        assert_eq!(e.len(), 6);
        assert_eq!(e[0].path, ".gitattributes");
        assert_eq!(e[0].size, Some(45));
        assert_eq!(e[2].path, "src/run me.sh");
        assert!(e[2].executable());
        assert_eq!(e[3].kind, EntryKind::Commit);
        assert_eq!(e[3].size, None);
    }

    #[test]
    fn builds_tree_with_sizes() {
        let t = Tree::build(parse_ls_tree(LS));
        assert_eq!(t.totals(), (45 + 25711 + 100 + 782 + 10, 6));
        let root = &t.nodes[0];
        let names: Vec<&str> = root
            .children
            .iter()
            .map(|&c| t.nodes[c].name.as_str())
            .collect();
        assert_eq!(names, vec!["src", "vendor", ".gitattributes", "README"]);
        let src = root.children[0];
        assert_eq!(t.nodes[src].size, 25711 + 100 + 782);
        assert_eq!(t.nodes[src].files, 3);
        let views = t.nodes[src].children[0];
        assert_eq!(t.nodes[views].path, "src/views");
        let views_files: Vec<&str> = t.nodes[views]
            .children
            .iter()
            .map(|&c| t.nodes[c].name.as_str())
            .collect();
        assert_eq!(views_files, vec!["graph.rs", "kit.rs"]);
        assert!(t.find("src/views/kit.rs").is_some());
        assert!(t.find("src/views").is_none(), "folders are not files");
    }

    #[test]
    fn rows_follow_expansion_and_search() {
        let t = Tree::build(parse_ls_tree(LS));
        let none = HashSet::new();
        assert_eq!(t.rows(&none, "").len(), 4, "top level only");
        let open: HashSet<String> = ["src".to_owned()].into_iter().collect();
        let rows = t.rows(&open, "");
        let paths: Vec<&str> = rows
            .iter()
            .map(|(i, _)| t.nodes[*i].path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec![
                "src",
                "src/views",
                "src/run me.sh",
                "vendor",
                ".gitattributes",
                "README"
            ]
        );
        assert_eq!(rows[1].1, 1, "depth of src/views");
        let found = t.rows(&none, "KIT");
        let paths: Vec<&str> = found
            .iter()
            .map(|(i, _)| t.nodes[*i].path.as_str())
            .collect();
        assert_eq!(paths, vec!["src", "src/views", "src/views/kit.rs"]);
        let fuzzy = t.rows(&none, "grrs");
        assert!(
            fuzzy
                .iter()
                .any(|(i, _)| t.nodes[*i].path == "src/views/graph.rs")
        );
        assert!(t.rows(&none, "zzz").is_empty());
        assert_eq!(
            Tree::ancestors("a/b/c.rs"),
            vec!["a".to_owned(), "a/b".to_owned()]
        );
        assert!(Tree::ancestors("top.rs").is_empty());
    }
}
