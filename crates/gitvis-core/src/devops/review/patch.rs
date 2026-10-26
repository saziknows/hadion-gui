//! GitHub's per-file `patch` (bare `@@` hunks, no `diff --git` header) as
//! lines with old and new numbers, and the anchors review comments use.
//!
//! A review comment points at `(side, line)`: removed lines are on the
//! `LEFT` with their old number, added and context lines on the `RIGHT`
//! with their new number. A multi-line comment needs both ends in the same
//! hunk.

use super::model::Side;
use crate::diff::LineKind;

/// One line of a patch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchLine {
    pub kind: LineKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    /// Without the `+` / `-` / ` ` prefix.
    pub text: String,
}

impl PatchLine {
    /// Where a comment on this line sits.
    pub fn anchor(&self) -> Option<(Side, u32)> {
        match self.kind {
            LineKind::Removed => self.old.map(|n| (Side::Left, n)),
            LineKind::Added | LineKind::Context => self.new.map(|n| (Side::Right, n)),
            LineKind::NoNewline => None,
        }
    }

    /// Whether a thread at `(side, line)` belongs under this line.
    pub fn matches(&self, side: Side, line: u32) -> bool {
        match side {
            Side::Right => self.kind != LineKind::Removed && self.new == Some(line),
            Side::Left => self.kind != LineKind::Added && self.old == Some(line),
        }
    }
}

/// One `@@ -a,b +c,d @@ context` hunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchHunk {
    pub header: String,
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    /// The function or section after the second `@@`.
    pub context: String,
    pub lines: Vec<PatchLine>,
}

/// `-12,7` → (12, 7); `-12` → (12, 1).
fn range(token: &str) -> (u32, u32) {
    let t = &token[1..];
    let mut it = t.splitn(2, ',');
    let start = it.next().and_then(|n| n.parse().ok()).unwrap_or(1);
    let len = it.next().and_then(|n| n.parse().ok()).unwrap_or(1);
    (start, len)
}

fn header_of(line: &str) -> Option<(u32, u32, u32, u32, String)> {
    let rest = line.strip_prefix("@@ ")?;
    let end = rest.find(" @@")?;
    let ranges = &rest[..end];
    let context = rest[end + 3..].trim().to_owned();
    let mut old = (1, 1);
    let mut new = (1, 1);
    for tok in ranges.split_whitespace() {
        if tok.starts_with('-') {
            old = range(tok);
        } else if tok.starts_with('+') {
            new = range(tok);
        }
    }
    Some((old.0, old.1, new.0, new.1, context))
}

/// Parse GitHub's bare hunks. Lines before the first hunk are ignored;
/// CR before LF is dropped from the text.
pub fn parse_patch(patch: &str) -> Vec<PatchHunk> {
    let mut hunks: Vec<PatchHunk> = Vec::new();
    let (mut old_no, mut new_no) = (0u32, 0u32);
    for raw in patch.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.starts_with("@@") {
            if let Some((os, ol, ns, nl, context)) = header_of(line) {
                hunks.push(PatchHunk {
                    header: line.to_owned(),
                    old_start: os,
                    old_len: ol,
                    new_start: ns,
                    new_len: nl,
                    context,
                    lines: Vec::new(),
                });
                old_no = os;
                new_no = ns;
                continue;
            }
        }
        let Some(h) = hunks.last_mut() else {
            continue;
        };
        let (kind, text) = match line.as_bytes().first() {
            Some(b'+') => (LineKind::Added, &line[1..]),
            Some(b'-') => (LineKind::Removed, &line[1..]),
            Some(b'\\') => (LineKind::NoNewline, line),
            Some(b' ') => (LineKind::Context, &line[1..]),
            // An empty line inside a hunk is an empty context line (some
            // tools strip the trailing space); a trailing newline at the
            // very end of the patch is not a line.
            None => (LineKind::Context, ""),
            _ => (LineKind::Context, line),
        };
        let (old, new) = match kind {
            LineKind::Added => {
                new_no += 1;
                (None, Some(new_no - 1))
            }
            LineKind::Removed => {
                old_no += 1;
                (Some(old_no - 1), None)
            }
            LineKind::Context => {
                old_no += 1;
                new_no += 1;
                (Some(old_no - 1), Some(new_no - 1))
            }
            LineKind::NoNewline => (None, None),
        };
        h.lines.push(PatchLine {
            kind,
            old,
            new,
            text: text.to_owned(),
        });
    }
    // A final empty "line" that only comes from the patch ending in '\n'
    // is dropped when the hunk already has all the lines its header counts.
    for h in &mut hunks {
        let counted = |h: &PatchHunk| {
            let olds = h.lines.iter().filter(|l| l.old.is_some()).count() as u32;
            let news = h.lines.iter().filter(|l| l.new.is_some()).count() as u32;
            (olds, news)
        };
        while h
            .lines
            .last()
            .is_some_and(|l| l.kind == LineKind::Context && l.text.is_empty())
            && {
                let (o, n) = counted(h);
                o > h.old_len || n > h.new_len
            }
        {
            h.lines.pop();
        }
    }
    hunks
}

/// A position in a parsed patch: hunk index and line index in the hunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LinePos {
    pub hunk: usize,
    pub line: usize,
}

/// Where a comment on the lines from `a` to `b` (either order) goes:
/// `(start, end)` anchors, `start` `None` for a single line. `Err` when the
/// range crosses hunks or touches a line that cannot be commented.
pub fn range_anchor(
    hunks: &[PatchHunk],
    a: LinePos,
    b: LinePos,
) -> Result<(Option<(Side, u32)>, (Side, u32)), &'static str> {
    let (first, last) = if a <= b { (a, b) } else { (b, a) };
    if first.hunk != last.hunk {
        return Err("A comment can cover lines of one hunk only.");
    }
    let lines = &hunks.get(first.hunk).ok_or("No such hunk.")?.lines;
    let end = lines
        .get(last.line)
        .and_then(PatchLine::anchor)
        .ok_or("That line cannot be commented on.")?;
    if first == last {
        return Ok((None, end));
    }
    let start = lines
        .get(first.line)
        .and_then(PatchLine::anchor)
        .ok_or("That line cannot be commented on.")?;
    Ok((Some(start), end))
}

/// The text of the head-side lines in a range, for a ```suggestion block
/// (removed lines are not part of the head and are left out).
pub fn suggestion_lines(hunks: &[PatchHunk], a: LinePos, b: LinePos) -> Option<String> {
    let (first, last) = if a <= b { (a, b) } else { (b, a) };
    if first.hunk != last.hunk {
        return None;
    }
    let lines = &hunks.get(first.hunk)?.lines;
    let picked: Vec<&str> = lines
        .get(first.line..=last.line)?
        .iter()
        .filter(|l| matches!(l.kind, LineKind::Added | LineKind::Context))
        .map(|l| l.text.as_str())
        .collect();
    if picked.is_empty() {
        return None;
    }
    Some(picked.join("\n"))
}

/// A suggestion block replacing the selected lines with `text`.
pub fn suggestion_block(text: &str) -> String {
    format!("```suggestion\n{text}\n```")
}

/// The line positions where a thread at `(side, line)` shows.
pub fn find_line(hunks: &[PatchHunk], side: Side, line: u32) -> Option<LinePos> {
    for (hi, h) in hunks.iter().enumerate() {
        for (li, l) in h.lines.iter().enumerate() {
            if l.matches(side, line) {
                return Some(LinePos { hunk: hi, line: li });
            }
        }
    }
    None
}

/// `(additions, deletions)` counted from the lines.
pub fn stats(hunks: &[PatchHunk]) -> (usize, usize) {
    let mut a = 0;
    let mut d = 0;
    for h in hunks {
        for l in &h.lines {
            match l.kind {
                LineKind::Added => a += 1,
                LineKind::Removed => d += 1,
                _ => {}
            }
        }
    }
    (a, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "@@ -16,7 +16,8 @@ enum Tags {\n   kSkusBindings,\n   kCardanoProvider,\n-  kPolkadotProvider\n+  kPolkadotProvider,\n+  kPolkadotInjected\n // clang-format on\n \n #include <x.h>\n@@ -40,3 +41,2 @@\n a\n-b\n c\n\\ No newline at end of file";

    #[test]
    fn numbers_on_both_sides() {
        let h = parse_patch(PATCH);
        assert_eq!(h.len(), 2);
        assert_eq!(
            (h[0].old_start, h[0].old_len, h[0].new_start, h[0].new_len),
            (16, 7, 16, 8)
        );
        assert_eq!(h[0].context, "enum Tags {");
        let l = &h[0].lines;
        assert_eq!(l.len(), 8);
        assert_eq!((l[0].old, l[0].new), (Some(16), Some(16)));
        assert_eq!(l[0].text, "  kSkusBindings,");
        assert_eq!(
            (l[2].kind, l[2].old, l[2].new),
            (LineKind::Removed, Some(18), None)
        );
        assert_eq!(
            (l[3].kind, l[3].old, l[3].new),
            (LineKind::Added, None, Some(18))
        );
        assert_eq!((l[4].kind, l[4].new), (LineKind::Added, Some(19)));
        assert_eq!((l[5].old, l[5].new), (Some(19), Some(20)));
        assert_eq!((l[7].old, l[7].new), (Some(21), Some(22)));
        let l = &h[1].lines;
        assert_eq!((l[0].old, l[0].new), (Some(40), Some(41)));
        assert_eq!((l[1].kind, l[1].old), (LineKind::Removed, Some(41)));
        assert_eq!((l[2].old, l[2].new), (Some(42), Some(42)));
        assert_eq!(l[3].kind, LineKind::NoNewline);
        assert_eq!(stats(&h), (2, 2));
    }

    #[test]
    fn empty_context_lines_and_trailing_newline() {
        let h = parse_patch("@@ -1,3 +1,3 @@\n a\n\n-b\n+c\n");
        assert_eq!(
            h[0].lines.len(),
            4,
            "the empty line is context, the final newline is not a line"
        );
        assert_eq!(h[0].lines[1].kind, LineKind::Context);
        assert_eq!((h[0].lines[1].old, h[0].lines[1].new), (Some(2), Some(2)));
        let one = parse_patch("@@ -0,0 +1 @@\n+only\r\n");
        assert_eq!(one[0].new_len, 1);
        assert_eq!(one[0].lines.len(), 1);
        assert_eq!(one[0].lines[0].text, "only");
        assert!(parse_patch("").is_empty());
        assert!(parse_patch("Binary files differ").is_empty());
    }

    #[test]
    fn anchors_and_ranges() {
        let h = parse_patch(PATCH);
        let p = |hunk, line| LinePos { hunk, line };
        assert_eq!(h[0].lines[2].anchor(), Some((Side::Left, 18)));
        assert_eq!(h[0].lines[3].anchor(), Some((Side::Right, 18)));
        assert_eq!(h[0].lines[0].anchor(), Some((Side::Right, 16)));
        assert_eq!(h[1].lines[3].anchor(), None);
        assert_eq!(
            range_anchor(&h, p(0, 3), p(0, 3)),
            Ok((None, (Side::Right, 18)))
        );
        assert_eq!(
            range_anchor(&h, p(0, 4), p(0, 2)),
            Ok((Some((Side::Left, 18)), (Side::Right, 19)))
        );
        assert!(range_anchor(&h, p(0, 1), p(1, 0)).is_err());
        assert!(range_anchor(&h, p(1, 3), p(1, 3)).is_err());
        assert_eq!(find_line(&h, Side::Right, 19), Some(p(0, 4)));
        assert_eq!(find_line(&h, Side::Left, 41), Some(p(1, 1)));
        assert_eq!(
            find_line(&h, Side::Left, 16),
            Some(p(0, 0)),
            "context lines carry both numbers"
        );
        assert_eq!(find_line(&h, Side::Right, 999), None);
    }

    #[test]
    fn suggestions_take_head_lines() {
        let h = parse_patch(PATCH);
        let p = |hunk, line| LinePos { hunk, line };
        assert_eq!(
            suggestion_lines(&h, p(0, 2), p(0, 4)).as_deref(),
            Some("  kPolkadotProvider,\n  kPolkadotInjected")
        );
        assert_eq!(suggestion_lines(&h, p(0, 2), p(0, 2)), None);
        assert_eq!(suggestion_block("x"), "```suggestion\nx\n```");
    }
}
