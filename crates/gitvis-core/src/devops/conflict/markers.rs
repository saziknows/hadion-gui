//! Conflict markers in a working file, parsed into stable text and
//! conflict hunks.
//!
//! The rules follow git's own (`rerere.c`, `is_cmarker`): a marker is a
//! line that starts with exactly `size` copies of `<`, `|`, `=` or `>`
//! (`size` is 7 unless the `conflict-marker-size` attribute says
//! otherwise). `<` and `>` must be followed by a space and a label, `|`
//! and `=` by whitespace or the end of the line. A longer run (`========`)
//! is ordinary text.
//!
//! A conflict is `<<<<<<<`, an optional `|||||||` base section (diff3 and
//! zdiff3 styles), `=======` and `>>>>>>>`. A start marker without that
//! structure is kept as text, so a lone marker-like line never swallows the
//! rest of the file. When a hunk contains more than one separator or end
//! marker candidate (a Markdown `=======` heading inside one side), the
//! split that matches a known hunk from the stage blobs wins; otherwise
//! git's rule applies (the first separator, the first end after it).
//!
//! Bytes are kept exactly: lines carry their own line endings (`\n`,
//! `\r\n` or none on a last line without a newline), and a hunk keeps its
//! raw text, so an unresolved hunk assembles back to what was read.

/// Default marker length (`conflict-marker-size`).
pub const DEFAULT_MARKER_SIZE: usize = 7;

/// A line ending.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Eol {
    #[default]
    Lf,
    CrLf,
    /// The last line of a file without a final newline.
    None,
}

impl Eol {
    pub fn bytes(self) -> &'static [u8] {
        match self {
            Eol::Lf => b"\n",
            Eol::CrLf => b"\r\n",
            Eol::None => b"",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Eol::Lf => "LF",
            Eol::CrLf => "CRLF",
            Eol::None => "none",
        }
    }
}

/// The four marker lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    /// `<<<<<<< ours`
    Start,
    /// `||||||| base` (diff3, zdiff3)
    Base,
    /// `=======`
    Sep,
    /// `>>>>>>> theirs`
    End,
}

/// Split into lines, each with its line ending (the last one may have
/// none).
pub fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            out.push(&bytes[start..=i]);
            start = i + 1;
        }
    }
    if start < bytes.len() {
        out.push(&bytes[start..]);
    }
    out
}

/// A line without its line ending.
pub fn line_text(line: &[u8]) -> &[u8] {
    let l = line.strip_suffix(b"\n").unwrap_or(line);
    l.strip_suffix(b"\r").unwrap_or(l)
}

/// The line ending of a line.
pub fn line_eol(line: &[u8]) -> Eol {
    if line.ends_with(b"\r\n") {
        Eol::CrLf
    } else if line.ends_with(b"\n") {
        Eol::Lf
    } else {
        Eol::None
    }
}

/// Number of lines in `bytes` (a last line without a newline counts).
pub fn line_count(bytes: &[u8]) -> usize {
    let n = bytes.iter().filter(|&&b| b == b'\n').count();
    if bytes.last().is_some_and(|&b| b != b'\n') {
        n + 1
    } else {
        n
    }
}

/// Classify a line (with or without its ending) as a marker of `size`,
/// returning the label after it.
pub fn marker(line: &[u8], size: usize) -> Option<(Marker, &[u8])> {
    let text = line_text(line);
    if size == 0 || text.len() < size {
        return None;
    }
    let c = text[0];
    let m = match c {
        b'<' => Marker::Start,
        b'|' => Marker::Base,
        b'=' => Marker::Sep,
        b'>' => Marker::End,
        _ => return None,
    };
    if !text[..size].iter().all(|&b| b == c) {
        return None;
    }
    let rest = &text[size..];
    match rest.first() {
        // `<` and `>` always carry a label; `|` may not, `=` never does.
        None => matches!(m, Marker::Base | Marker::Sep).then_some((m, &rest[..0])),
        Some(b' ') => Some((m, trim(&rest[1..]))),
        Some(b'\t') if matches!(m, Marker::Base | Marker::Sep) => Some((m, trim(&rest[1..]))),
        _ => None,
    }
}

fn trim(b: &[u8]) -> &[u8] {
    let start = b
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    let end = b
        .iter()
        .rposition(|c| !c.is_ascii_whitespace())
        .map(|i| i + 1)
        .unwrap_or(start);
    &b[start..end.max(start)]
}

/// One conflict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// 0-based line of the `<<<<<<<` marker in the parsed text.
    pub start_line: usize,
    /// Lines from the start marker to the end marker, inclusive.
    pub marker_lines: usize,
    pub ours: Vec<u8>,
    /// The common ancestor's text: from the file (diff3 / zdiff3) or, for
    /// the merge style, from the stage blobs when a matching hunk was found
    /// (`base_from_stages`).
    pub base: Option<Vec<u8>>,
    pub base_from_stages: bool,
    pub theirs: Vec<u8>,
    pub ours_label: String,
    pub base_label: Option<String>,
    pub theirs_label: String,
    /// The exact bytes, markers included.
    pub raw: Vec<u8>,
    /// Line ending of the marker lines (what new text in this hunk gets).
    pub eol: Eol,
}

/// Stable text or a conflict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    Text(Vec<u8>),
    Conflict(Hunk),
}

/// Conflict style as seen in the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// No conflicts at all.
    None,
    /// `<<<<<<<` / `=======` / `>>>>>>>` only.
    Merge,
    /// With `|||||||` base sections (diff3 or zdiff3).
    Diff3,
    /// Some hunks with a base section, some without.
    Mixed,
}

impl Style {
    pub fn label(self) -> &'static str {
        match self {
            Style::None => "no conflicts",
            Style::Merge => "merge style",
            Style::Diff3 => "diff3 / zdiff3 style",
            Style::Mixed => "mixed styles",
        }
    }
}

/// A parsed working file.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Doc {
    pub segments: Vec<Segment>,
    pub marker_size: usize,
    /// 0-based lines that look like a start or end marker but are not part
    /// of a conflict.
    pub stray: Vec<usize>,
    /// The most common line ending.
    pub eol: Eol,
    /// Lines ending in CRLF and in LF.
    pub crlf_lines: usize,
    pub lf_lines: usize,
    /// The text ends with a newline (or is empty).
    pub final_newline: bool,
    pub total_lines: usize,
}

/// The merge of the three stage blobs as git computes it in diff3 style
/// (`git merge-file -p --diff3`), used to pick the right split of
/// ambiguous hunks and to give merge-style hunks their base.
///
/// It keeps the three versions line by line and the points where all
/// three are aligned (inside or at the edge of text both sides agree on).
/// A working-file hunk whose ours side starts at ours line `a` and theirs
/// side at theirs line `b` gets a base only when both sides match the
/// stage merge there and both its ends are aligned points: one merge-style
/// conflict that spans several diff3 conflicts (git joins conflicts less
/// than four lines apart) gets the base of the whole span; a split it
/// cannot place exactly gets none, never a wrong one. Lines are compared
/// without their line endings (autocrlf, a missing final newline).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Hints {
    ours: Vec<Vec<u8>>,
    base: Vec<Vec<u8>>,
    theirs: Vec<Vec<u8>>,
    /// `(ours line, theirs line)` → base line where the three align.
    cuts: std::collections::HashMap<(usize, usize), usize>,
    /// Conflicts in the stage merge.
    pub conflicts: usize,
}

impl Hints {
    /// From `git merge-file -p --diff3` output. Empty when the output is
    /// not diff3 style.
    pub fn from_diff3(bytes: &[u8], size: usize) -> Hints {
        let d = parse(bytes, size);
        let mut h = Hints::default();
        let texts = |b: &[u8]| -> Vec<Vec<u8>> {
            split_lines(b)
                .into_iter()
                .map(|l| line_text(l).to_vec())
                .collect()
        };
        h.cuts.insert((0, 0), 0);
        for s in &d.segments {
            match s {
                Segment::Text(t) => {
                    for l in texts(t) {
                        h.ours.push(l.clone());
                        h.base.push(l.clone());
                        h.theirs.push(l);
                        h.cuts.insert((h.ours.len(), h.theirs.len()), h.base.len());
                    }
                }
                Segment::Conflict(c) => {
                    let Some(base) = &c.base else {
                        return Hints::default();
                    };
                    h.ours.extend(texts(&c.ours));
                    h.base.extend(texts(base));
                    h.theirs.extend(texts(&c.theirs));
                    h.cuts.insert((h.ours.len(), h.theirs.len()), h.base.len());
                    h.conflicts += 1;
                }
            }
        }
        h
    }

    pub fn is_empty(&self) -> bool {
        self.ours.is_empty() && self.theirs.is_empty()
    }

    /// The base (LF line endings) of a conflict at ours line `a` / theirs
    /// line `b`, when the stage merge agrees with it.
    pub fn base_for(&self, a: usize, ours: &[u8], b: usize, theirs: &[u8]) -> Option<Vec<u8>> {
        let same = |lines: &[Vec<u8>], at: usize, side: &[u8]| -> Option<usize> {
            let want: Vec<&[u8]> = split_lines(side).into_iter().map(line_text).collect();
            let have = lines.get(at..at + want.len())?;
            have.iter()
                .zip(&want)
                .all(|(x, y)| x.as_slice() == *y)
                .then_some(want.len())
        };
        let n = same(&self.ours, a, ours)?;
        let m = same(&self.theirs, b, theirs)?;
        let b0 = *self.cuts.get(&(a, b))?;
        let b1 = *self.cuts.get(&(a + n, b + m))?;
        if b1 < b0 {
            return None;
        }
        let mut out = Vec::new();
        for l in &self.base[b0..b1] {
            out.extend_from_slice(l);
            out.push(b'\n');
        }
        Some(out)
    }
}

/// Lines of each side before every hunk: `(ours line, theirs line)` of
/// hunk `k`'s start, counting stable text and earlier hunks' sides.
fn side_offsets(segments: &[Segment]) -> Vec<(usize, usize)> {
    let (mut o, mut t) = (0, 0);
    let mut out = Vec::new();
    for s in segments {
        match s {
            Segment::Text(x) => {
                let n = line_count(x);
                o += n;
                t += n;
            }
            Segment::Conflict(h) => {
                out.push((o, t));
                o += line_count(&h.ours);
                t += line_count(&h.theirs);
            }
        }
    }
    out
}

/// `\r\n` → `\n`, for comparisons across line-ending conversion.
pub fn to_lf(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\r' && b.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Give every line ending in `b` the style `eol` (`Lf` or `CrLf`).
pub fn with_eol(b: &[u8], eol: Eol) -> Vec<u8> {
    let lf = to_lf(b);
    if eol != Eol::CrLf {
        return lf;
    }
    let mut out = Vec::with_capacity(lf.len() + lf.len() / 20);
    for &c in &lf {
        if c == b'\n' {
            out.push(b'\r');
        }
        out.push(c);
    }
    out
}

impl Doc {
    pub fn hunks(&self) -> impl Iterator<Item = &Hunk> {
        self.segments.iter().filter_map(|s| match s {
            Segment::Conflict(h) => Some(h),
            Segment::Text(_) => None,
        })
    }

    pub fn hunks_mut(&mut self) -> impl Iterator<Item = &mut Hunk> {
        self.segments.iter_mut().filter_map(|s| match s {
            Segment::Conflict(h) => Some(h),
            Segment::Text(_) => None,
        })
    }

    pub fn hunk_count(&self) -> usize {
        self.hunks().count()
    }

    pub fn style(&self) -> Style {
        let (mut with, mut without) = (0, 0);
        for h in self.hunks() {
            if h.base.is_some() && !h.base_from_stages {
                with += 1;
            } else {
                without += 1;
            }
        }
        match (with, without) {
            (0, 0) => Style::None,
            (_, 0) => Style::Diff3,
            (0, _) => Style::Merge,
            _ => Style::Mixed,
        }
    }

    /// The text back, every hunk unresolved (equals the parsed input).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for s in &self.segments {
            match s {
                Segment::Text(t) => out.extend_from_slice(t),
                Segment::Conflict(h) => out.extend_from_slice(&h.raw),
            }
        }
        out
    }

    /// The conflict is the last thing in the file (nothing after its end
    /// marker).
    pub fn ends_with_conflict(&self) -> bool {
        matches!(self.segments.last(), Some(Segment::Conflict(_)))
    }

    /// Fill in the base of hunks that have none from the stage merge.
    pub fn fill_bases(&mut self, hints: &Hints) {
        if hints.is_empty() {
            return;
        }
        let offsets = side_offsets(&self.segments);
        for (h, (a, b)) in self.hunks_mut().zip(offsets) {
            if h.base.is_some() {
                continue;
            }
            if let Some(base) = hints.base_for(a, &h.ours, b, &h.theirs) {
                h.base = Some(with_eol(&base, h.eol));
                h.base_from_stages = true;
            }
        }
    }

    /// Give every hunk without a base an empty one (add/add: the common
    /// ancestor is an empty file).
    pub fn empty_bases(&mut self) {
        for h in self.hunks_mut() {
            if h.base.is_none() {
                h.base = Some(Vec::new());
                h.base_from_stages = true;
            }
        }
    }
}

/// Parse `bytes` with markers of `size`.
pub fn parse(bytes: &[u8], size: usize) -> Doc {
    parse_with_hints(bytes, size, &Hints::default())
}

/// [`parse`], using `hints` to settle ambiguous hunks.
pub fn parse_with_hints(bytes: &[u8], size: usize, hints: &Hints) -> Doc {
    let lines = split_lines(bytes);
    let kinds: Vec<Option<Marker>> = lines.iter().map(|l| marker(l, size).map(|m| m.0)).collect();
    let mut doc = Doc {
        marker_size: size,
        total_lines: lines.len(),
        final_newline: bytes.is_empty() || bytes.ends_with(b"\n"),
        ..Default::default()
    };
    for l in &lines {
        match line_eol(l) {
            Eol::CrLf => doc.crlf_lines += 1,
            Eol::Lf => doc.lf_lines += 1,
            Eol::None => {}
        }
    }
    doc.eol = if doc.crlf_lines > doc.lf_lines {
        Eol::CrLf
    } else {
        Eol::Lf
    };
    let mut text: Vec<u8> = Vec::new();
    // Lines of each side so far (for the hints).
    let (mut o, mut t) = (0usize, 0usize);
    let mut i = 0;
    while i < lines.len() {
        if kinds[i] == Some(Marker::Start)
            && let Some((hunk, next)) = conflict_at(&lines, &kinds, i, size, hints, (o, t))
        {
            if !text.is_empty() {
                doc.segments.push(Segment::Text(std::mem::take(&mut text)));
            }
            o += line_count(&hunk.ours);
            t += line_count(&hunk.theirs);
            doc.segments.push(Segment::Conflict(hunk));
            i = next;
            continue;
        }
        if matches!(kinds[i], Some(Marker::Start | Marker::End)) {
            doc.stray.push(i);
        }
        text.extend_from_slice(lines[i]);
        o += 1;
        t += 1;
        i += 1;
    }
    if !text.is_empty() {
        doc.segments.push(Segment::Text(text));
    }
    doc
}

/// A conflict starting at line `start`, and the line after it.
fn conflict_at(
    lines: &[&[u8]],
    kinds: &[Option<Marker>],
    start: usize,
    size: usize,
    hints: &Hints,
    (o, t): (usize, usize),
) -> Option<(Hunk, usize)> {
    let (mut bases, mut seps, mut ends) = (Vec::new(), Vec::new(), Vec::new());
    for (j, k) in kinds.iter().enumerate().skip(start + 1) {
        match k {
            Some(Marker::Start) => break,
            Some(Marker::Base) => bases.push(j),
            Some(Marker::Sep) => seps.push(j),
            Some(Marker::End) => ends.push(j),
            None => {}
        }
    }
    // git's split: the first separator, the first end after it.
    let s0 = *seps.first()?;
    let e0 = *ends.iter().find(|&&e| e > s0)?;
    let build = |s: usize, e: usize| -> Hunk {
        let base = bases.iter().copied().find(|&b| b < s);
        build_hunk(lines, start, base, s, e, size)
    };
    if !hints.is_empty() && (seps.len() > 1 || ends.len() > 1) {
        for &e in &ends {
            for &s in seps.iter().filter(|&&s| s < e) {
                let h = build(s, e);
                if hints.base_for(o, &h.ours, t, &h.theirs).is_some() {
                    return Some((h, e + 1));
                }
            }
        }
    }
    Some((build(s0, e0), e0 + 1))
}

fn build_hunk(
    lines: &[&[u8]],
    start: usize,
    base: Option<usize>,
    sep: usize,
    end: usize,
    size: usize,
) -> Hunk {
    let join = |from: usize, to: usize| -> Vec<u8> { lines[from..to].concat() };
    let label = |i: usize| -> String {
        marker(lines[i], size)
            .map(|(_, l)| String::from_utf8_lossy(l).into_owned())
            .unwrap_or_default()
    };
    let ours_end = base.unwrap_or(sep);
    Hunk {
        start_line: start,
        marker_lines: end - start + 1,
        ours: join(start + 1, ours_end),
        base: base.map(|b| join(b + 1, sep)),
        base_from_stages: false,
        theirs: join(sep + 1, end),
        ours_label: label(start),
        base_label: base.map(label),
        theirs_label: label(end),
        raw: join(start, end + 1),
        eol: match line_eol(lines[start]) {
            Eol::None => Eol::Lf,
            e => e,
        },
    }
}

/// 1-based lines of the start markers of conflicts still in `bytes`.
pub fn conflict_lines(bytes: &[u8], size: usize) -> Vec<usize> {
    parse(bytes, size)
        .hunks()
        .map(|h| h.start_line + 1)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MERGE: &str = "a\n<<<<<<< HEAD\nours 1\n=======\ntheirs 1\n>>>>>>> feature\nb\nc\n<<<<<<< HEAD\nours 2\nours 2b\n=======\n>>>>>>> feature\nend\n";

    #[test]
    fn markers_follow_git() {
        assert_eq!(
            marker(b"<<<<<<< HEAD\n", 7),
            Some((Marker::Start, &b"HEAD"[..]))
        );
        assert_eq!(
            marker(b">>>>>>> abc1234 (subject)\r\n", 7),
            Some((Marker::End, &b"abc1234 (subject)"[..]))
        );
        assert_eq!(marker(b"=======\n", 7), Some((Marker::Sep, &b""[..])));
        assert_eq!(marker(b"=======", 7), Some((Marker::Sep, &b""[..])));
        assert_eq!(marker(b"=======\r\n", 7), Some((Marker::Sep, &b""[..])));
        assert_eq!(marker(b"|||||||\n", 7), Some((Marker::Base, &b""[..])));
        assert_eq!(
            marker(b"||||||| merged common ancestors\n", 7),
            Some((Marker::Base, &b"merged common ancestors"[..]))
        );
        // Longer runs, missing labels and other sizes are text.
        assert_eq!(marker(b"========\n", 7), None);
        assert_eq!(marker(b"<<<<<<<<\n", 7), None);
        assert_eq!(marker(b"<<<<<<<\n", 7), None, "start needs a label");
        assert_eq!(marker(b">>>>>>>\n", 7), None, "end needs a label");
        assert_eq!(marker(b"=======x\n", 7), None);
        assert_eq!(marker(b"<<<<<<<HEAD\n", 7), None);
        assert_eq!(marker(b"====\n", 7), None);
        assert_eq!(marker(b"<<<<<<< HEAD\n", 9), None);
        assert_eq!(
            marker(b"<<<<<<<<< HEAD\n", 9),
            Some((Marker::Start, &b"HEAD"[..]))
        );
    }

    #[test]
    fn lines_keep_endings() {
        assert_eq!(
            split_lines(b"a\r\nb\nc"),
            vec![&b"a\r\n"[..], &b"b\n"[..], &b"c"[..]]
        );
        assert!(split_lines(b"").is_empty());
        assert_eq!(line_text(b"x\r\n"), b"x");
        assert_eq!(line_eol(b"x\r\n"), Eol::CrLf);
        assert_eq!(line_eol(b"x"), Eol::None);
        assert_eq!(line_count(b"a\nb"), 2);
        assert_eq!(line_count(b"a\nb\n"), 2);
        assert_eq!(line_count(b""), 0);
    }

    #[test]
    fn parses_merge_style() {
        let d = parse(MERGE.as_bytes(), 7);
        assert_eq!(d.hunk_count(), 2);
        assert_eq!(d.style(), Style::Merge);
        let h: Vec<&Hunk> = d.hunks().collect();
        assert_eq!(h[0].ours, b"ours 1\n");
        assert_eq!(h[0].theirs, b"theirs 1\n");
        assert_eq!(h[0].base, None);
        assert_eq!(h[0].ours_label, "HEAD");
        assert_eq!(h[0].theirs_label, "feature");
        assert_eq!(h[0].start_line, 1);
        assert_eq!(h[0].marker_lines, 5);
        assert_eq!(h[1].ours, b"ours 2\nours 2b\n");
        assert_eq!(h[1].theirs, b"", "theirs deleted the lines");
        assert_eq!(h[1].start_line, 8);
        assert!(matches!(&d.segments[0], Segment::Text(t) if t == b"a\n"));
        assert!(matches!(&d.segments[2], Segment::Text(t) if t == b"b\nc\n"));
        assert!(matches!(&d.segments[4], Segment::Text(t) if t == b"end\n"));
        assert_eq!(d.to_bytes(), MERGE.as_bytes(), "round trip");
        assert_eq!(conflict_lines(MERGE.as_bytes(), 7), vec![2, 9]);
    }

    #[test]
    fn parses_diff3_and_zdiff3() {
        let text = "x\n<<<<<<< HEAD\nB1\n||||||| 06b28cf\nb\n=======\nB2\n>>>>>>> feature\ny\n";
        let d = parse(text.as_bytes(), 7);
        assert_eq!(d.style(), Style::Diff3);
        let h = d.hunks().next().unwrap();
        assert_eq!(h.base.as_deref(), Some(&b"b\n"[..]));
        assert_eq!(h.base_label.as_deref(), Some("06b28cf"));
        assert_eq!(d.to_bytes(), text.as_bytes());
        // zdiff3 with an empty base section and an unlabelled base marker.
        let z = "<<<<<<< ours\nadded ours\n|||||||\n=======\nadded theirs\n>>>>>>> theirs\n";
        let d = parse(z.as_bytes(), 7);
        let h = d.hunks().next().unwrap();
        assert_eq!(h.base.as_deref(), Some(&b""[..]));
        assert_eq!(h.base_label.as_deref(), Some(""));
        assert_eq!(h.ours, b"added ours\n");
        assert!(d.ends_with_conflict());
    }

    #[test]
    fn marker_lookalikes_are_text() {
        // A setext heading, a lone start marker and a lone end marker
        // outside conflicts; then a real conflict.
        let text = "Title\n=======\n<<<<<<< not a conflict\nplain\n>>>>>>> stray\n<<<<<<< HEAD\nA\n=======\nB\n>>>>>>> other\n";
        let d = parse(text.as_bytes(), 7);
        assert_eq!(d.hunk_count(), 1);
        assert_eq!(d.stray, vec![2, 4]);
        let h = d.hunks().next().unwrap();
        assert_eq!(h.ours, b"A\n");
        assert_eq!(h.start_line, 5);
        assert_eq!(d.to_bytes(), text.as_bytes());
        // A start marker that never ends.
        let open = "a\n<<<<<<< HEAD\nb\n=======\nc\n";
        let d = parse(open.as_bytes(), 7);
        assert_eq!(d.hunk_count(), 0);
        assert_eq!(d.to_bytes(), open.as_bytes());
        // An end before any separator.
        let bad = "<<<<<<< HEAD\nb\n>>>>>>> x\n=======\n";
        assert_eq!(parse(bad.as_bytes(), 7).hunk_count(), 0);
        // Separator lines inside theirs are theirs' content.
        let rst = "<<<<<<< HEAD\nA\n=======\nHeading\n=======\n>>>>>>> x\n";
        let h = parse(rst.as_bytes(), 7).hunks().next().cloned().unwrap();
        assert_eq!(h.theirs, b"Heading\n=======\n");
    }

    #[test]
    fn hints_settle_ambiguous_separators() {
        // Ours contains a "=======" line; git's rule would split there.
        let text = "<<<<<<< HEAD\nTitle\n=======\n=======\nOther\n>>>>>>> x\n";
        let plain = parse(text.as_bytes(), 7);
        assert_eq!(plain.hunks().next().unwrap().ours, b"Title\n");
        // The stage merge is asked for with longer markers, so its own
        // output is not ambiguous.
        let hints = Hints::from_diff3(
            b"<<<<<<<<<<<< ours\nTitle\n=======\n|||||||||||| base\nold\n============\nOther\n>>>>>>>>>>>> theirs\n",
            12,
        );
        assert_eq!(hints.conflicts, 1);
        let mut d = parse_with_hints(text.as_bytes(), 7, &hints);
        let h = d.hunks().next().unwrap().clone();
        assert_eq!(h.ours, b"Title\n=======\n");
        assert_eq!(h.theirs, b"Other\n");
        d.fill_bases(&hints);
        let h = d.hunks().next().unwrap();
        assert_eq!(h.base.as_deref(), Some(&b"old\n"[..]));
        assert!(h.base_from_stages);
        assert_eq!(
            d.style(),
            Style::Merge,
            "a filled base is not the file's style"
        );
        assert_eq!(d.to_bytes(), text.as_bytes());
    }

    #[test]
    fn longer_markers() {
        let text =
            "<<<<<<<<<<<< HEAD\nA\n=======\n<<<<<<< inner\n============\nB\n>>>>>>>>>>>> x\n";
        let d = parse(text.as_bytes(), 12);
        assert_eq!(d.hunk_count(), 1);
        let h = d.hunks().next().unwrap();
        assert_eq!(h.ours, b"A\n=======\n<<<<<<< inner\n");
        assert_eq!(h.theirs, b"B\n");
        assert!(d.stray.is_empty());
        assert_eq!(parse(text.as_bytes(), 7).hunk_count(), 0);
    }

    #[test]
    fn crlf_and_missing_final_newline() {
        let text = "a\r\n<<<<<<< HEAD\r\nours\r\n=======\r\ntheirs\r\n>>>>>>> b\r\nlast";
        let d = parse(text.as_bytes(), 7);
        assert_eq!(d.eol, Eol::CrLf);
        assert!(!d.final_newline);
        let h = d.hunks().next().unwrap();
        assert_eq!(h.eol, Eol::CrLf);
        assert_eq!(h.ours, b"ours\r\n");
        assert_eq!(h.theirs_label, "b");
        assert!(matches!(d.segments.last(), Some(Segment::Text(t)) if t == b"last"));
        assert_eq!(d.to_bytes(), text.as_bytes());
        // Hints are LF; bases are converted to the hunk's endings.
        let mut d = d;
        d.fill_bases(&Hints::from_diff3(
            b"a\n<<<<<<< o\nours\n||||||| b\nbase\nmore\n=======\ntheirs\n>>>>>>> t\nlast",
            7,
        ));
        assert_eq!(
            d.hunks().next().unwrap().base.as_deref(),
            Some(&b"base\r\nmore\r\n"[..])
        );
    }

    #[test]
    fn eol_helpers() {
        assert_eq!(to_lf(b"a\r\nb\r\n\rc\n"), b"a\nb\n\rc\n");
        assert_eq!(with_eol(b"a\nb\r\n", Eol::CrLf), b"a\r\nb\r\n");
        assert_eq!(with_eol(b"a\r\nb\n", Eol::Lf), b"a\nb\n");
    }

    #[test]
    fn hints_from_merge_file_output() {
        let out = "<<<<<<< ours\nB1\n||||||| base\nb\n=======\nB2\n>>>>>>> theirs\nc\n";
        let h = Hints::from_diff3(out.as_bytes(), 7);
        assert_eq!(h.conflicts, 1);
        let mut d = parse(b"<<<<<<< HEAD\nB1\n=======\nB2\n>>>>>>> f\nc\n", 7);
        d.fill_bases(&h);
        assert_eq!(d.hunks().next().unwrap().base.as_deref(), Some(&b"b\n"[..]));
        let mut aa = parse(b"<<<<<<< HEAD\nB1\n=======\nB2\n>>>>>>> f\n", 7);
        aa.empty_bases();
        assert_eq!(aa.hunks().next().unwrap().base.as_deref(), Some(&b""[..]));
        // Merge-style output is not a hint.
        assert!(Hints::from_diff3(b"<<<<<<< a\nx\n=======\ny\n>>>>>>> b\n", 7).is_empty());
    }

    #[test]
    fn joined_hunks_get_the_base_of_the_span() {
        // diff3: two conflicts two lines apart; the merge style joins them.
        let diff3 = "x\n<<<<<<< o\n1 ours\n||||||| b\n1\n=======\n1 theirs\n>>>>>>> t\nkeep\nkeep\n<<<<<<< o\n2 ours\n||||||| b\n2\n=======\n2 theirs\n>>>>>>> t\ny\n";
        let hints = Hints::from_diff3(diff3.as_bytes(), 7);
        assert_eq!(hints.conflicts, 2);
        let merged = "x\n<<<<<<< HEAD\n1 ours\nkeep\nkeep\n2 ours\n=======\n1 theirs\nkeep\nkeep\n2 theirs\n>>>>>>> f\ny\n";
        let mut d = parse(merged.as_bytes(), 7);
        d.fill_bases(&hints);
        assert_eq!(
            d.hunks().next().unwrap().base.as_deref(),
            Some(&b"1\nkeep\nkeep\n2\n"[..])
        );
        // The second of two separate hunks, after edited stable text that
        // keeps its line count: still placed.
        let split = "x edited\n<<<<<<< HEAD\n1 ours\n=======\n1 theirs\n>>>>>>> f\nkeep\nkeep\n<<<<<<< HEAD\n2 ours\n=======\n2 theirs\n>>>>>>> f\ny\n";
        let mut d = parse(split.as_bytes(), 7);
        d.fill_bases(&hints);
        let bases: Vec<Option<Vec<u8>>> = d.hunks().map(|h| h.base.clone()).collect();
        assert_eq!(bases, vec![Some(b"1\n".to_vec()), Some(b"2\n".to_vec())]);
        // A line inserted above shifts the sides: no base rather than a
        // wrong one.
        let shifted = format!("new line\n{merged}");
        let mut d = parse(shifted.as_bytes(), 7);
        d.fill_bases(&hints);
        assert_eq!(d.hunks().next().unwrap().base, None);
        // A hunk trimmed inside a stage conflict (a common first line
        // moved out) cannot be placed.
        let diff3b = "<<<<<<< o\nsame\nA\n||||||| b\nold\n=======\nsame\nB\n>>>>>>> t\n";
        let trimmed = "same\n<<<<<<< HEAD\nA\n=======\nB\n>>>>>>> f\n";
        let mut d = parse(trimmed.as_bytes(), 7);
        d.fill_bases(&Hints::from_diff3(diff3b.as_bytes(), 7));
        assert_eq!(d.hunks().next().unwrap().base, None);
    }
}
