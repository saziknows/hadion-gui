//! One file open in the conflict editor: what was loaded, the resolution
//! of every hunk, unsaved edits, and what the panes draw (lines with syntax
//! colours, the aligned row layout), cached until something changes.

use std::collections::HashMap;
use std::sync::Arc;

use hadion_core::devops::conflict::load::{FileLoad, Presentation};
use hadion_core::devops::conflict::markers::{self, Doc, Hunk, Segment};
use hadion_core::devops::conflict::resolve::{self, Resolution};

use crate::state::Span;

/// Height of a code row and of a hunk header row.
pub const LINE_H: f32 = 20.0;
pub const HEADER_H: f32 = 34.0;

pub struct Session {
    pub load: FileLoad,
    /// The conflict as edited (whole-file edits replace it).
    pub doc: Doc,
    pub res: Vec<Resolution>,
    /// Edits not written to disk yet.
    pub dirty: bool,
    /// Hash of the file on disk this session matches (loaded or saved).
    pub disk_hash: u64,
    /// The file changed on disk while edits were unsaved: the new load.
    pub external: Option<Box<FileLoad>>,
    /// The hunk being edited and its text.
    pub editing: Option<(usize, String)>,
    /// The hunk the keyboard and the toolbar act on.
    pub focus: usize,
    /// Scroll the panes to this hunk on the next frame.
    pub scroll_to: Option<usize>,
    /// Shared horizontal scroll of the panes.
    pub hscroll: f32,
    /// Bumped on every change (layout cache key).
    pub rev: u64,
    parts: HashMap<PartKey, Part>,
    parts_key: (u64, bool),
    layout: Option<((u64, bool, bool), Arc<Layout>)>,
    /// Give the hunk editor the keyboard on its first frame.
    pub edit_focus: bool,
}

/// A piece of text shown in a pane, split into lines with colours.
#[derive(Clone, Default)]
pub struct Part {
    pub lines: Vec<String>,
    pub spans: Vec<Vec<Span>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PartKey {
    Text(usize),
    Ours(usize),
    Theirs(usize),
    Base(usize),
    Custom(u64),
}

/// The four panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Col {
    Ours,
    Result,
    Theirs,
    Base,
}

/// What a result row shows inside an unresolved hunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawLine {
    Marker,
    Ours(usize),
    Base(usize),
    Theirs(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// A line of stable text (the same in every pane).
    Text { seg: usize, line: usize },
    /// The bar above a conflict.
    Header { hunk: usize },
    /// Row `row` of a conflict's block.
    Block { hunk: usize, row: usize },
}

#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub kind: RowKind,
    pub y: f32,
    pub h: f32,
    /// Line numbers (1-based) in ours, result, theirs, base.
    pub nums: [Option<u32>; 4],
}

#[derive(Clone, Default)]
pub struct Layout {
    pub rows: Vec<Row>,
    pub total_h: f32,
    /// Index of each hunk's header row.
    pub headers: Vec<usize>,
    /// Rows of each hunk's block.
    pub block_rows: Vec<usize>,
    /// First result line of each hunk (1-based).
    pub result_line: Vec<u32>,
    pub max_chars: usize,
}

impl Session {
    pub fn new(mut load: FileLoad) -> Session {
        let doc = load.doc.take().unwrap_or_default();
        let n = doc.hunk_count();
        let disk_hash = load.hash;
        Session {
            load,
            doc,
            res: vec![Resolution::Unresolved; n],
            dirty: false,
            disk_hash,
            external: None,
            editing: None,
            focus: 0,
            scroll_to: None,
            hscroll: 0.0,
            rev: 1,
            parts: HashMap::new(),
            parts_key: (0, false),
            layout: None,
            edit_focus: false,
        }
    }

    pub fn path(&self) -> &str {
        self.load.path()
    }

    pub fn presentation(&self) -> Presentation {
        self.load.presentation
    }

    pub fn is_text(&self) -> bool {
        self.load.presentation.is_text()
    }

    pub fn hunks(&self) -> Vec<&Hunk> {
        self.doc.hunks().collect()
    }

    pub fn hunk(&self, k: usize) -> Option<&Hunk> {
        self.doc.hunks().nth(k)
    }

    pub fn hunk_count(&self) -> usize {
        self.res.len()
    }

    /// Something changed: caches go, the file is dirty.
    pub fn touch(&mut self) {
        self.rev += 1;
        self.dirty = true;
        self.layout = None;
    }

    /// The bytes the file would have if saved now.
    pub fn result(&self) -> Vec<u8> {
        resolve::assemble(&self.doc, &self.res, self.load.tails)
    }

    pub fn resolved(&self, k: usize) -> bool {
        match (self.hunk(k), self.res.get(k)) {
            (Some(h), Some(r)) => resolve::is_resolved(h, r, self.doc.marker_size),
            _ => false,
        }
    }

    /// Hunks not resolved yet.
    pub fn unresolved(&self) -> usize {
        (0..self.hunk_count())
            .filter(|&k| !self.resolved(k))
            .count()
    }

    /// Set a hunk's resolution (a `Base` without a known base is ignored).
    pub fn set(&mut self, k: usize, r: Resolution) -> bool {
        let Some(h) = self.hunk(k) else { return false };
        if r == Resolution::Base && h.base.is_none() {
            return false;
        }
        // Text typed equal to one side is that side.
        let r = match r {
            Resolution::Custom(b) if b == h.ours => Resolution::Ours,
            Resolution::Custom(b) if b == h.theirs => Resolution::Theirs,
            r => r,
        };
        if self.res.get(k) == Some(&r) {
            return false;
        }
        self.res[k] = r;
        self.touch();
        true
    }

    /// Replace the whole conflict with edited text (re-parsed).
    pub fn replace_text(&mut self, bytes: &[u8]) {
        let size = self.doc.marker_size;
        self.doc = markers::parse(bytes, size);
        self.res = vec![Resolution::Unresolved; self.doc.hunk_count()];
        self.editing = None;
        self.focus = 0;
        self.parts.clear();
        self.parts_key = (0, false);
        self.touch();
    }

    /// The next (or previous) hunk from the focus, unresolved ones first.
    pub fn step(&self, forward: bool) -> Option<usize> {
        let n = self.hunk_count();
        if n == 0 {
            return None;
        }
        let order: Vec<usize> = if forward {
            (1..=n).map(|d| (self.focus + d) % n).collect()
        } else {
            (1..=n).map(|d| (self.focus + n - d % n) % n).collect()
        };
        order
            .iter()
            .copied()
            .find(|&k| !self.resolved(k))
            .or_else(|| order.first().copied())
    }

    /// First unresolved hunk.
    pub fn first_unresolved(&self) -> Option<usize> {
        (0..self.hunk_count()).find(|&k| !self.resolved(k))
    }

    /// Text and colours of a part (cached; `dark` and the path pick the
    /// syntax theme).
    pub fn part(&mut self, key: PartKey, dark: bool) -> &Part {
        if self.parts_key != (self.doc_gen(), dark) {
            self.parts.clear();
            self.parts_key = (self.doc_gen(), dark);
        }
        if !self.parts.contains_key(&key) {
            let bytes: Vec<u8> = match key {
                PartKey::Text(s) => match self.doc.segments.get(s) {
                    Some(Segment::Text(t)) => t.clone(),
                    _ => Vec::new(),
                },
                PartKey::Ours(k) => self.hunk(k).map(|h| h.ours.clone()).unwrap_or_default(),
                PartKey::Theirs(k) => self.hunk(k).map(|h| h.theirs.clone()).unwrap_or_default(),
                PartKey::Base(k) => self
                    .hunk(k)
                    .and_then(|h| h.base.clone())
                    .unwrap_or_default(),
                PartKey::Custom(hash) => self
                    .res
                    .iter()
                    .find_map(|r| match r {
                        Resolution::Custom(b)
                            if hadion_core::devops::conflict::load::hash(b) == hash =>
                        {
                            Some(b.clone())
                        }
                        _ => None,
                    })
                    .unwrap_or_default(),
            };
            let part = make_part(&bytes, self.load.path(), dark);
            self.parts.insert(key, part);
        }
        &self.parts[&key]
    }

    /// The doc's identity for the part cache (changes when the text is
    /// replaced, not when a resolution changes).
    fn doc_gen(&self) -> u64 {
        // Segments only change through `replace_text`, which clears the
        // cache itself; the hunk count and line total are a cheap guard.
        (self.doc.segments.len() as u64) << 32 | self.doc.total_lines as u64
    }

    /// Number of lines the result shows for hunk `k`.
    fn result_lines(&self, k: usize, h: &Hunk) -> usize {
        match &self.res[k] {
            Resolution::Unresolved => raw_layout(h).len(),
            Resolution::Ours => markers::line_count(&h.ours),
            Resolution::Theirs => markers::line_count(&h.theirs),
            Resolution::OursThenTheirs | Resolution::TheirsThenOurs => {
                markers::line_count(&h.ours) + markers::line_count(&h.theirs)
            }
            Resolution::Base => match &h.base {
                Some(b) => markers::line_count(b),
                None => raw_layout(h).len(),
            },
            Resolution::Custom(b) => markers::line_count(b),
        }
    }

    /// The aligned rows of every pane (cached per change and toggle).
    pub fn layout(&mut self, show_base: bool, dark: bool) -> Arc<Layout> {
        let key = (self.rev, show_base, dark);
        if self.layout.as_ref().map(|(k, _)| *k) != Some(key) {
            let l = self.build_layout(show_base);
            self.layout = Some((key, Arc::new(l)));
        }
        self.layout.as_ref().expect("just built").1.clone()
    }

    fn build_layout(&self, show_base: bool) -> Layout {
        let mut l = Layout::default();
        let (mut o, mut r, mut t, mut b) = (0u32, 0u32, 0u32, 0u32);
        let mut y = 0.0;
        let mut k = 0usize;
        let mut push = |l: &mut Layout, kind: RowKind, h: f32, nums: [Option<u32>; 4]| {
            l.rows.push(Row { kind, y, h, nums });
            y += h;
        };
        for (si, seg) in self.doc.segments.iter().enumerate() {
            match seg {
                Segment::Text(text) => {
                    for (li, line) in markers::split_lines(text).iter().enumerate() {
                        o += 1;
                        r += 1;
                        t += 1;
                        b += 1;
                        l.max_chars = l.max_chars.max(markers::line_text(line).len());
                        push(
                            &mut l,
                            RowKind::Text { seg: si, line: li },
                            LINE_H,
                            [Some(o), Some(r), Some(t), Some(b)],
                        );
                    }
                }
                Segment::Conflict(h) => {
                    l.headers.push(l.rows.len());
                    push(&mut l, RowKind::Header { hunk: k }, HEADER_H, [None; 4]);
                    let on = markers::line_count(&h.ours);
                    let tn = markers::line_count(&h.theirs);
                    let bn = h.base.as_deref().map(markers::line_count).unwrap_or(0);
                    let rn = self.result_lines(k, h);
                    let edit_rows = match &self.editing {
                        Some((e, text)) if *e == k => text.lines().count().max(1) + 2,
                        _ => 0,
                    };
                    for side in [&h.ours, &h.theirs, &h.raw] {
                        for line in markers::split_lines(side) {
                            l.max_chars = l.max_chars.max(markers::line_text(line).len());
                        }
                    }
                    let rows = on
                        .max(tn)
                        .max(if show_base { bn } else { 0 })
                        .max(rn)
                        .max(edit_rows)
                        .max(1);
                    l.block_rows.push(rows);
                    l.result_line.push(r + 1);
                    for row in 0..rows {
                        let mut nums = [None; 4];
                        if row < on {
                            o += 1;
                            nums[0] = Some(o);
                        }
                        if row < rn && edit_rows == 0 {
                            r += 1;
                            nums[1] = Some(r);
                        }
                        if row < tn {
                            t += 1;
                            nums[2] = Some(t);
                        }
                        if row < bn {
                            b += 1;
                            nums[3] = Some(b);
                        }
                        push(&mut l, RowKind::Block { hunk: k, row }, LINE_H, nums);
                    }
                    if edit_rows > 0 {
                        r += rn as u32;
                    }
                    k += 1;
                }
            }
        }
        l.total_h = y;
        l
    }
}

/// What each row of an unresolved hunk's result shows: the file as it
/// is, markers included.
pub fn raw_layout(h: &Hunk) -> Vec<RawLine> {
    let mut v = vec![RawLine::Marker];
    v.extend((0..markers::line_count(&h.ours)).map(RawLine::Ours));
    if let Some(base) = h.base.as_ref().filter(|_| !h.base_from_stages) {
        v.push(RawLine::Marker);
        v.extend((0..markers::line_count(base)).map(RawLine::Base));
    }
    v.push(RawLine::Marker);
    v.extend((0..markers::line_count(&h.theirs)).map(RawLine::Theirs));
    v.push(RawLine::Marker);
    v
}

/// The marker lines of a hunk's raw text, in order (the rows
/// [`raw_layout`] calls `Marker`).
pub fn marker_texts(h: &Hunk) -> Vec<String> {
    let lines = markers::split_lines(&h.raw);
    let text = |i: usize| -> String {
        lines
            .get(i)
            .map(|l| String::from_utf8_lossy(markers::line_text(l)).into_owned())
            .unwrap_or_default()
    };
    let mut v = vec![text(0)];
    let mut i = 1 + markers::line_count(&h.ours);
    if let Some(base) = h.base.as_ref().filter(|_| !h.base_from_stages) {
        v.push(text(i));
        i += 1 + markers::line_count(base);
    }
    v.push(text(i));
    i += 1 + markers::line_count(&h.theirs);
    v.push(text(i));
    v
}

/// Lines and syntax spans of `bytes` (lossy UTF-8, tabs as four spaces).
pub fn make_part(bytes: &[u8], path: &str, dark: bool) -> Part {
    let lines: Vec<String> = markers::split_lines(bytes)
        .into_iter()
        .map(|l| String::from_utf8_lossy(markers::line_text(l)).replace('\t', "    "))
        .collect();
    let spans = highlight_lines(&lines, path, dark);
    Part { lines, spans }
}

/// Syntax colours per line, through `crate::highlight` (one synthetic
/// hunk of context lines, so the grammar sees the part as a whole).
pub fn highlight_lines(lines: &[String], path: &str, dark: bool) -> Vec<Vec<Span>> {
    let empty = || vec![Vec::new(); lines.len()];
    if lines.is_empty() || crate::highlight::language_for(path).is_none() && !has_fallback(path) {
        return empty();
    }
    let n = lines.len();
    let mut text = format!("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,{n} +1,{n} @@\n");
    for l in lines {
        text.push(' ');
        text.push_str(l);
        text.push('\n');
    }
    let mut files = hadion_core::diff::parse(text.as_bytes());
    let Some(fd) = files.pop() else {
        return empty();
    };
    let mut spans = crate::highlight::highlight(&fd, path, dark);
    match spans.pop() {
        Some(v) if v.len() == n => v,
        _ => empty(),
    }
}

/// Paths `crate::highlight` colours through a stand-in grammar.
fn has_fallback(path: &str) -> bool {
    let ext = path.rsplit('.').next().unwrap_or("");
    matches!(
        ext,
        "ts" | "tsx"
            | "mts"
            | "cts"
            | "jsx"
            | "mjs"
            | "cjs"
            | "kt"
            | "kts"
            | "swift"
            | "toml"
            | "yml"
            | "ps1"
            | "vue"
            | "svelte"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use hadion_core::devops::conflict::load::Working;
    use hadion_core::devops::conflict::resolve::Tails;
    use hadion_core::devops::conflict::{ConflictEntry, ConflictKind};

    const TEXT: &str = "a\n<<<<<<< HEAD\nO1\nO1b\n=======\nT1\n>>>>>>> f\nb\n<<<<<<< HEAD\nO2\n||||||| base\nB2\n=======\nT2\n>>>>>>> f\nc\n";

    pub fn session(text: &str) -> Session {
        let doc = markers::parse(text.as_bytes(), 7);
        Session::new(FileLoad {
            entry: ConflictEntry {
                path: "src/x.rs".into(),
                kind: ConflictKind::BothModified,
                stages: [None, None, None],
                worktree_mode: "100644".into(),
            },
            stages: [None, None, None],
            working: Working::Text(text.as_bytes().to_vec()),
            attrs: Default::default(),
            presentation: Presentation::Text,
            doc: Some(doc),
            stage_conflicts: None,
            tails: Tails::default(),
            hash: 1,
            utf8: true,
            elapsed_ms: 0,
        })
    }

    #[test]
    fn resolving_and_stepping() {
        let mut s = session(TEXT);
        assert_eq!(s.hunk_count(), 2);
        assert_eq!(s.unresolved(), 2);
        assert!(!s.dirty);
        assert!(s.set(0, Resolution::Theirs));
        assert!(s.dirty);
        assert!(!s.set(0, Resolution::Theirs), "no change");
        assert_eq!(s.unresolved(), 1);
        assert_eq!(s.step(true), Some(1));
        s.focus = 1;
        assert_eq!(s.step(true), Some(1), "the only unresolved one");
        assert!(s.set(1, Resolution::Base));
        assert_eq!(s.unresolved(), 0);
        assert_eq!(s.step(true), Some(0), "all resolved: plain order");
        assert_eq!(s.step(false), Some(0));
        assert_eq!(String::from_utf8(s.result()).unwrap(), "a\nT1\nb\nB2\nc\n");
        // Typed text equal to a side is that side.
        assert!(s.set(0, Resolution::Custom(b"O1\nO1b\n".to_vec())));
        assert_eq!(s.res[0], Resolution::Ours);
    }

    #[test]
    fn base_needs_a_base() {
        let mut s = session(TEXT);
        assert!(!s.set(0, Resolution::Base));
        assert_eq!(s.res[0], Resolution::Unresolved);
    }

    #[test]
    fn layout_aligns_the_panes() {
        let mut s = session(TEXT);
        let l = (*s.layout(false, true)).clone();
        // a, header, block (raw: 1 + 2 + 1 + 1 + 1 = 6 rows), b, header,
        // block (raw with base: 1 + 1 + 1 + 1 + 1 + 1 + 1 = 7), c.
        assert_eq!(l.headers.len(), 2);
        assert_eq!(l.block_rows, vec![6, 7]);
        assert_eq!(l.rows.len(), 1 + 1 + 6 + 1 + 1 + 7 + 1);
        assert_eq!(l.result_line, vec![2, 9]);
        let last = l.rows.last().unwrap();
        assert_eq!(last.nums[0], Some(6), "ours: a O1 O1b b O2 c");
        assert_eq!(last.nums[2], Some(5), "theirs: a T1 b T2 c");
        assert_eq!(last.nums[1], Some(16), "result: the file as it is");
        // Resolved: blocks shrink to the larger side.
        s.set(0, Resolution::Ours);
        s.set(1, Resolution::Theirs);
        let l = (*s.layout(false, true)).clone();
        assert_eq!(l.block_rows, vec![2, 1]);
        assert_eq!(l.rows.last().unwrap().nums[1], Some(6), "a O1 O1b b T2 c");
        // The base pane only counts when shown.
        s.set(1, Resolution::Unresolved);
        assert_eq!(s.layout(true, true).block_rows[1], 7);
    }

    #[test]
    fn raw_rows_and_markers() {
        let s = session(TEXT);
        let h = s.hunk(1).unwrap();
        assert_eq!(
            raw_layout(h),
            vec![
                RawLine::Marker,
                RawLine::Ours(0),
                RawLine::Marker,
                RawLine::Base(0),
                RawLine::Marker,
                RawLine::Theirs(0),
                RawLine::Marker
            ]
        );
        assert_eq!(
            marker_texts(h),
            vec!["<<<<<<< HEAD", "||||||| base", "=======", ">>>>>>> f"]
        );
    }

    #[test]
    fn parts_and_colours() {
        let mut s = session(TEXT);
        let p = s.part(PartKey::Ours(0), true).clone();
        assert_eq!(p.lines, vec!["O1", "O1b"]);
        assert_eq!(p.spans.len(), 2);
        let t = s.part(PartKey::Text(0), true).clone();
        assert_eq!(t.lines, vec!["a"]);
        let colored = highlight_lines(&["fn main() { let x = 1; }".into()], "a.rs", true);
        assert!(!colored[0].is_empty());
        let plain = highlight_lines(&["anything".into()], "notes.unknownext", true);
        assert!(plain[0].is_empty());
        // Whole-file edits re-parse.
        s.replace_text(b"x\n<<<<<<< a\n1\n=======\n2\n>>>>>>> b\n");
        assert_eq!(s.hunk_count(), 1);
        assert!(s.dirty);
        assert_eq!(s.part(PartKey::Text(0), true).lines, vec!["x"]);
    }
}
