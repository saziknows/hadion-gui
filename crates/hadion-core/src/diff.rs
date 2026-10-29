//! Parser for `git diff` unified output.
//!
//! Raw bytes are kept next to the display text so a single hunk, or a
//! selection of lines inside one, can be fed back to `git apply`
//! byte-for-byte (CRLF and non-UTF-8 files included).

use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
    /// `\ No newline at end of file`
    NoNewline,
}

#[derive(Clone, Debug)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
    /// Char ranges of `text` that differ from the paired line.
    pub emph: Vec<(u32, u32)>,
    /// Index within the hunk of the removed/added line this one pairs with.
    pub pair: Option<u32>,
    /// Byte range of the raw line inside the hunk's raw bytes.
    raw: (u32, u32),
}

impl DiffLine {
    fn new(kind: LineKind, old: Option<u32>, new: Option<u32>, text: String) -> Self {
        Self {
            kind,
            old,
            new,
            text,
            emph: Vec::new(),
            pair: None,
            raw: (0, 0),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
    raw: Vec<u8>,
}

impl Hunk {
    /// `(old_start, new_start)` from the header.
    pub fn starts(&self) -> (u32, u32) {
        parse_hunk_header(&self.header)
    }

    /// The function/context text after the `@@ … @@` range.
    pub fn context(&self) -> &str {
        let mut it = self.header.splitn(3, "@@");
        it.next();
        it.next();
        it.next().map(str::trim).unwrap_or("")
    }

    /// The `-a,b +c,d` part of the header.
    pub fn range_text(&self) -> &str {
        let mut it = self.header.splitn(3, "@@");
        it.next();
        it.next().map(str::trim).unwrap_or("")
    }
}

#[derive(Clone, Debug, Default)]
pub struct FileDiff {
    pub hunks: Vec<Hunk>,
    pub binary: bool,
    /// Combined diff of a conflicted file; hunks cannot be applied.
    pub combined: bool,
    /// Built locally (e.g. an untracked file), not from git; hunks cannot be applied.
    pub synthetic: bool,
    /// Produced with options (ignored whitespace) that make hunks unusable
    /// for `git apply`.
    pub approximate: bool,
    /// Too big to show.
    pub too_large: bool,
    header_raw: Vec<u8>,
}

impl FileDiff {
    pub fn too_large() -> FileDiff {
        FileDiff {
            too_large: true,
            ..Default::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.hunks.is_empty() && !self.binary && !self.too_large
    }

    pub fn can_apply_hunks(&self) -> bool {
        !self.combined && !self.synthetic && !self.binary && !self.too_large && !self.approximate
    }

    /// A patch containing only hunk `index`, suitable for `git apply`.
    pub fn hunk_patch(&self, index: usize) -> Option<Vec<u8>> {
        if !self.can_apply_hunks() {
            return None;
        }
        let hunk = self.hunks.get(index)?;
        let mut out = self.header_raw.clone();
        out.extend_from_slice(&hunk.raw);
        if !out.ends_with(b"\n") {
            out.push(b'\n');
        }
        Some(out)
    }

    /// A patch containing only the `selected` line indices of hunk `index`
    /// (indices into `hunk.lines`). `reverse` builds the patch for
    /// `git apply -R` (unstage / discard): unselected added lines then stay
    /// as context and unselected removed lines are dropped, mirroring what
    /// the forward case does. Returns `None` when nothing selected changes
    /// anything.
    pub fn selected_lines_patch(
        &self,
        index: usize,
        selected: &[usize],
        reverse: bool,
    ) -> Option<Vec<u8>> {
        if !self.can_apply_hunks() {
            return None;
        }
        let hunk = self.hunks.get(index)?;
        let selected: HashSet<usize> = selected.iter().copied().collect();
        let mut body: Vec<u8> = Vec::new();
        let (mut old_count, mut new_count) = (0u32, 0u32);
        let mut any = false;
        let mut prev_kept = false;
        for (i, line) in hunk.lines.iter().enumerate() {
            let raw = &hunk.raw[line.raw.0 as usize..line.raw.1 as usize];
            let sel = selected.contains(&i);
            let as_context = |body: &mut Vec<u8>| {
                body.push(b' ');
                body.extend_from_slice(&raw[1.min(raw.len())..]);
            };
            match line.kind {
                LineKind::Context => {
                    body.extend_from_slice(raw);
                    old_count += 1;
                    new_count += 1;
                    prev_kept = true;
                }
                LineKind::NoNewline => {
                    if prev_kept {
                        body.extend_from_slice(raw);
                    }
                }
                LineKind::Added => {
                    if sel {
                        body.extend_from_slice(raw);
                        new_count += 1;
                        any = true;
                        prev_kept = true;
                    } else if reverse {
                        as_context(&mut body);
                        old_count += 1;
                        new_count += 1;
                        prev_kept = true;
                    } else {
                        prev_kept = false;
                    }
                }
                LineKind::Removed => {
                    if sel {
                        body.extend_from_slice(raw);
                        old_count += 1;
                        any = true;
                        prev_kept = true;
                    } else if !reverse {
                        as_context(&mut body);
                        old_count += 1;
                        new_count += 1;
                        prev_kept = true;
                    } else {
                        prev_kept = false;
                    }
                }
            }
        }
        if !any {
            return None;
        }
        let (old_start, new_start) = hunk.starts();
        let mut out = self.header_raw.clone();
        out.extend_from_slice(
            format!("@@ -{old_start},{old_count} +{new_start},{new_count} @@\n").as_bytes(),
        );
        out.extend_from_slice(&body);
        if !out.ends_with(b"\n") {
            out.push(b'\n');
        }
        Some(out)
    }

    pub fn stats(&self) -> (usize, usize) {
        let mut add = 0;
        let mut del = 0;
        for line in self.hunks.iter().flat_map(|h| &h.lines) {
            match line.kind {
                LineKind::Added => add += 1,
                LineKind::Removed => del += 1,
                _ => {}
            }
        }
        (add, del)
    }

    /// Show a whole file as added lines (untracked files).
    pub fn from_new_file(bytes: &[u8]) -> FileDiff {
        if looks_binary(bytes) {
            return FileDiff {
                binary: true,
                synthetic: true,
                ..Default::default()
            };
        }
        let text = String::from_utf8_lossy(bytes);
        let lines: Vec<DiffLine> = text
            .lines()
            .enumerate()
            .map(|(i, l)| {
                DiffLine::new(
                    LineKind::Added,
                    None,
                    Some(i as u32 + 1),
                    display_text(l.as_bytes()),
                )
            })
            .collect();
        let header = format!("@@ -0,0 +1,{} @@ new file", lines.len());
        FileDiff {
            hunks: if lines.is_empty() {
                Vec::new()
            } else {
                vec![Hunk {
                    header,
                    lines,
                    raw: Vec::new(),
                }]
            },
            synthetic: true,
            ..Default::default()
        }
    }
}

pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|&b| b == 0)
}

fn display_text(raw: &[u8]) -> String {
    let raw = raw.strip_suffix(b"\n").unwrap_or(raw);
    let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
    String::from_utf8_lossy(raw).replace('\t', "    ")
}

/// Split raw diff output into lines that keep their trailing `\n`.
fn split_lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.split_inclusive(|&b| b == b'\n')
}

/// Parse the output of `git diff` for one or more files.
pub fn parse(bytes: &[u8]) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut cur: Option<FileDiff> = None;
    // (parents in a combined diff, next old line, next new line)
    let mut state = (1usize, 0u32, 0u32);

    for line in split_lines(bytes) {
        let is_file_start = line.starts_with(b"diff --git ")
            || line.starts_with(b"diff --cc ")
            || line.starts_with(b"diff --combined ");
        if is_file_start {
            if let Some(f) = cur.take() {
                files.push(f);
            }
            cur = Some(FileDiff {
                combined: !line.starts_with(b"diff --git "),
                header_raw: line.to_vec(),
                ..Default::default()
            });
            continue;
        }
        let file = cur.get_or_insert_with(FileDiff::default);

        if line.starts_with(b"@@") {
            let header = display_text(line);
            let ats = line.iter().take_while(|&&b| b == b'@').count();
            let parents = ats.saturating_sub(1).max(1);
            let (old_start, new_start) = parse_hunk_header(&header);
            state = (parents, old_start, new_start);
            file.hunks.push(Hunk {
                header,
                lines: Vec::new(),
                raw: line.to_vec(),
            });
            continue;
        }

        let Some(hunk) = file.hunks.last_mut() else {
            // Still in the file header.
            if line.starts_with(b"Binary files ") || line.starts_with(b"GIT binary patch") {
                file.binary = true;
            }
            file.header_raw.extend_from_slice(line);
            continue;
        };

        let raw_start = hunk.raw.len() as u32;
        hunk.raw.extend_from_slice(line);
        let raw_range = (raw_start, hunk.raw.len() as u32);
        let (parents, old_no, new_no) = &mut state;
        if line.starts_with(b"\\") {
            let mut l = DiffLine::new(LineKind::NoNewline, None, None, display_text(line));
            l.raw = raw_range;
            hunk.lines.push(l);
            continue;
        }
        let prefix_len = (*parents).min(line.len());
        let (prefix, rest) = line.split_at(prefix_len);
        let text = display_text(rest);
        let kind = if prefix.contains(&b'+') {
            LineKind::Added
        } else if prefix.contains(&b'-') {
            LineKind::Removed
        } else {
            LineKind::Context
        };
        let (old, new) = match kind {
            LineKind::Added => {
                *new_no += 1;
                (None, Some(*new_no - 1))
            }
            LineKind::Removed => {
                *old_no += 1;
                (Some(*old_no - 1), None)
            }
            _ => {
                *old_no += 1;
                *new_no += 1;
                (Some(*old_no - 1), Some(*new_no - 1))
            }
        };
        let mut l = DiffLine::new(kind, old, new, text);
        l.raw = raw_range;
        hunk.lines.push(l);
    }
    if let Some(f) = cur {
        files.push(f);
    }
    for file in &mut files {
        if !file.combined {
            for hunk in &mut file.hunks {
                pair_lines(hunk);
            }
        }
    }
    files
}

/// `@@ -12,7 +14,8 @@ ctx` -> (12, 14). Combined headers use the last `+` range.
fn parse_hunk_header(header: &str) -> (u32, u32) {
    let mut old = 1;
    let mut new = 1;
    for token in header.split_whitespace().skip(1) {
        if token.starts_with("@@") {
            break;
        }
        let start = |t: &str| {
            t[1..]
                .split(',')
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(1)
        };
        if token.starts_with('-') {
            old = start(token);
        } else if token.starts_with('+') {
            new = start(token);
        }
    }
    (old, new)
}

/// Pair runs of removed lines with the added lines that follow them and
/// compute word-level emphasis for each pair.
fn pair_lines(hunk: &mut Hunk) {
    let n = hunk.lines.len();
    let mut i = 0;
    while i < n {
        if hunk.lines[i].kind != LineKind::Removed {
            i += 1;
            continue;
        }
        let mut removed = Vec::new();
        let mut j = i;
        while j < n && matches!(hunk.lines[j].kind, LineKind::Removed | LineKind::NoNewline) {
            if hunk.lines[j].kind == LineKind::Removed {
                removed.push(j);
            }
            j += 1;
        }
        let mut added = Vec::new();
        while j < n && matches!(hunk.lines[j].kind, LineKind::Added | LineKind::NoNewline) {
            if hunk.lines[j].kind == LineKind::Added {
                added.push(j);
            }
            j += 1;
        }
        for (&a, &b) in removed.iter().zip(added.iter()) {
            hunk.lines[a].pair = Some(b as u32);
            hunk.lines[b].pair = Some(a as u32);
            if let Some((ea, eb)) = word_diff(&hunk.lines[a].text, &hunk.lines[b].text) {
                hunk.lines[a].emph = ea;
                hunk.lines[b].emph = eb;
            }
        }
        i = j.max(i + 1);
    }
}

/// Runs of word characters, runs of whitespace, and single punctuation marks.
fn tokens(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut kind = 0u8; // 0 none, 1 word, 2 space, 3 punct
    for (i, c) in s.char_indices() {
        let k = if c.is_alphanumeric() || c == '_' {
            1
        } else if c.is_whitespace() {
            2
        } else {
            3
        };
        if i > 0 && (k != kind || k == 3) {
            out.push(&s[start..i]);
            start = i;
        }
        kind = k;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Char ranges that differ between two lines, or `None` when the lines are
/// too different for emphasis to help.
type Ranges = Vec<(u32, u32)>;

fn word_diff(a: &str, b: &str) -> Option<(Ranges, Ranges)> {
    let ta = tokens(a);
    let tb = tokens(b);
    if ta.is_empty() || tb.is_empty() || ta.len() > 1500 || tb.len() > 1500 {
        return None;
    }
    let offsets = |t: &[&str]| -> Vec<u32> {
        let mut v = Vec::with_capacity(t.len() + 1);
        let mut n = 0u32;
        v.push(0);
        for s in t {
            n += s.chars().count() as u32;
            v.push(n);
        }
        v
    };
    let (oa, ob) = (offsets(&ta), offsets(&tb));
    let ops = similar::capture_diff_slices(similar::Algorithm::Myers, &ta, &tb);
    let mut ea: Vec<(u32, u32)> = Vec::new();
    let mut eb: Vec<(u32, u32)> = Vec::new();
    let push = |v: &mut Vec<(u32, u32)>, s: u32, e: u32| {
        if e <= s {
            return;
        }
        match v.last_mut() {
            Some(last) if last.1 >= s => last.1 = last.1.max(e),
            _ => v.push((s, e)),
        }
    };
    for op in ops {
        match op {
            similar::DiffOp::Equal { .. } => {}
            similar::DiffOp::Delete {
                old_index, old_len, ..
            } => push(&mut ea, oa[old_index], oa[old_index + old_len]),
            similar::DiffOp::Insert {
                new_index, new_len, ..
            } => push(&mut eb, ob[new_index], ob[new_index + new_len]),
            similar::DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                push(&mut ea, oa[old_index], oa[old_index + old_len]);
                push(&mut eb, ob[new_index], ob[new_index + new_len]);
            }
        }
    }
    let changed = |e: &[(u32, u32)]| e.iter().map(|(s, t)| t - s).sum::<u32>() as f32;
    let (la, lb) = (*oa.last().unwrap() as f32, *ob.last().unwrap() as f32);
    // Mostly rewritten lines read better without speckled emphasis.
    if changed(&ea) > la * 0.7 && changed(&eb) > lb * 0.7 {
        return None;
    }
    Some((ea, eb))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        [
            "diff --git a/src/lib.rs b/src/lib.rs\r\n",
            "index 1111111..2222222 100644\n",
            "--- a/src/lib.rs\n",
            "+++ b/src/lib.rs\n",
            "@@ -1,3 +1,4 @@ mod a;\n",
            " one\n",
            "-two\n",
            "+TWO\n",
            "+three\n",
            " four\r\n",
            "@@ -10,2 +11,2 @@\n",
            " ten\n",
            "-eleven\n",
            "\\ No newline at end of file\n",
            "+ELEVEN\n",
            "\\ No newline at end of file\n",
        ]
        .concat()
        .into_bytes()
    }

    #[test]
    fn parses_hunks_and_line_numbers() {
        let files = parse(&sample());
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.hunks.len(), 2);
        let h = &f.hunks[0];
        assert_eq!(h.lines.len(), 5);
        assert_eq!((h.lines[0].old, h.lines[0].new), (Some(1), Some(1)));
        assert_eq!(
            (h.lines[1].kind, h.lines[1].old),
            (LineKind::Removed, Some(2))
        );
        assert_eq!(
            (h.lines[2].kind, h.lines[2].new),
            (LineKind::Added, Some(2))
        );
        assert_eq!((h.lines[4].old, h.lines[4].new), (Some(3), Some(4)));
        assert_eq!(h.lines[4].text, "four", "CR is stripped for display");
        assert_eq!(f.stats(), (3, 2));
        assert_eq!(f.hunks[1].lines[2].kind, LineKind::NoNewline);
        assert_eq!(h.context(), "mod a;");
        assert_eq!(h.range_text(), "-1,3 +1,4");
        assert_eq!(h.starts(), (1, 1));
    }

    #[test]
    fn hunk_patch_is_byte_exact() {
        let files = parse(&sample());
        let patch = files[0].hunk_patch(0).unwrap();
        let text = String::from_utf8(patch).unwrap();
        assert!(text.starts_with("diff --git a/src/lib.rs b/src/lib.rs\r\nindex"));
        assert!(text.contains("+++ b/src/lib.rs\n@@ -1,3 +1,4 @@ mod a;\n"));
        assert!(text.ends_with(" four\r\n"));
        assert!(!text.contains("@@ -10,2"));
    }

    #[test]
    fn selected_lines_forward_and_reverse() {
        let files = parse(&sample());
        let f = &files[0];
        // Stage only "+TWO": "-two" stays as context, "+three" is dropped.
        let p = String::from_utf8(f.selected_lines_patch(0, &[2], false).unwrap()).unwrap();
        assert!(
            p.ends_with("@@ -1,3 +1,4 @@\n one\n two\n+TWO\n four\r\n"),
            "{p}"
        );
        // Unstage only "+three": "+TWO" stays as context, "-two" is dropped.
        let p = String::from_utf8(f.selected_lines_patch(0, &[3], true).unwrap()).unwrap();
        assert!(
            p.ends_with("@@ -1,3 +1,4 @@\n one\n TWO\n+three\n four\r\n"),
            "{p}"
        );
        // The no-newline marker follows its line; the unselected "+ELEVEN"
        // (and its marker) is left out.
        let p = String::from_utf8(f.selected_lines_patch(1, &[1], false).unwrap()).unwrap();
        assert!(
            p.ends_with("@@ -10,2 +11,1 @@\n ten\n-eleven\n\\ No newline at end of file\n"),
            "{p}"
        );
        assert!(
            f.selected_lines_patch(0, &[0], false).is_none(),
            "context only"
        );
    }

    #[test]
    fn pairs_and_word_emphasis() {
        let text = concat!(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n",
            "-let total = price * qty;\n",
            "-unchanged line\n",
            "+let total = price * quantity;\n",
            "+completely different content here\n",
            " ctx\n"
        );
        let files = parse(text.as_bytes());
        let h = &files[0].hunks[0];
        assert_eq!(h.lines[0].pair, Some(2));
        assert_eq!(h.lines[2].pair, Some(0));
        assert_eq!(h.lines[1].pair, Some(3));
        assert_eq!(h.lines[0].emph, vec![(20, 23)], "qty");
        assert_eq!(h.lines[2].emph, vec![(20, 28)], "quantity");
        assert!(h.lines[1].emph.is_empty(), "too different for emphasis");
        assert!(h.lines[3].emph.is_empty());
    }

    #[test]
    fn tokenizer() {
        assert_eq!(tokens("a.b  cd"), vec!["a", ".", "b", "  ", "cd"]);
        assert_eq!(tokens("((x"), vec!["(", "(", "x"]);
    }

    #[test]
    fn detects_binary() {
        let files = parse(b"diff --git a/x.png b/x.png\nindex 1..2 100644\nBinary files a/x.png and b/x.png differ\n");
        assert!(files[0].binary);
        assert!(!files[0].can_apply_hunks());
    }

    #[test]
    fn combined_diff_lines() {
        let files = parse(b"diff --cc f.txt\nindex 1,2..3\n--- a/f.txt\n+++ b/f.txt\n@@@ -1,1 -1,1 +1,5 @@@\n++<<<<<<< HEAD\n +ours\n++=======\n+ theirs\n++>>>>>>> b\n");
        let f = &files[0];
        assert!(f.combined);
        assert!(f.hunks[0].lines.iter().all(|l| l.kind == LineKind::Added));
        assert_eq!(f.hunks[0].lines[1].text, "ours");
    }

    #[test]
    fn new_file_view() {
        let f = FileDiff::from_new_file(b"a\nb\n");
        assert_eq!(f.hunks[0].lines.len(), 2);
        assert!(!f.can_apply_hunks());
        assert!(FileDiff::from_new_file(b"\0\x01").binary);
    }
}
