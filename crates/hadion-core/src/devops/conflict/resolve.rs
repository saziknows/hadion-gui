//! Per-hunk resolutions and assembling the result file.

use std::borrow::Cow;

use super::markers::{self, Doc, Eol, Hunk, Segment};

/// How one conflict is resolved.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Resolution {
    /// Markers stay in the file.
    #[default]
    Unresolved,
    Ours,
    Theirs,
    /// Both: ours, then theirs.
    OursThenTheirs,
    /// Both: theirs, then ours.
    TheirsThenOurs,
    /// The common ancestor's text (undo both changes).
    Base,
    /// Text typed by the user (line endings already in the file's style).
    Custom(Vec<u8>),
}

impl Resolution {
    /// Short label for badges.
    pub fn label(&self) -> &'static str {
        match self {
            Resolution::Unresolved => "Unresolved",
            Resolution::Ours => "Ours",
            Resolution::Theirs => "Theirs",
            Resolution::OursThenTheirs => "Both: ours first",
            Resolution::TheirsThenOurs => "Both: theirs first",
            Resolution::Base => "Base",
            Resolution::Custom(_) => "Edited",
        }
    }

    /// Names accepted by the `conflict-hunk` script command.
    pub fn from_name(s: &str) -> Option<Resolution> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "ours" | "mine" => Resolution::Ours,
            "theirs" => Resolution::Theirs,
            "both" | "ours-theirs" | "both-ours" => Resolution::OursThenTheirs,
            "theirs-ours" | "both-theirs" => Resolution::TheirsThenOurs,
            "base" => Resolution::Base,
            "unresolved" | "reset" => Resolution::Unresolved,
            _ => return None,
        })
    }
}

/// The bytes a hunk contributes to the result. Unresolved hunks (and
/// `Base` when the base is unknown) keep their markers.
pub fn hunk_bytes<'a>(h: &'a Hunk, r: &'a Resolution) -> Cow<'a, [u8]> {
    match r {
        Resolution::Unresolved => Cow::Borrowed(&h.raw),
        Resolution::Ours => Cow::Borrowed(&h.ours),
        Resolution::Theirs => Cow::Borrowed(&h.theirs),
        Resolution::OursThenTheirs => Cow::Owned([&h.ours[..], &h.theirs[..]].concat()),
        Resolution::TheirsThenOurs => Cow::Owned([&h.theirs[..], &h.ours[..]].concat()),
        Resolution::Base => match &h.base {
            Some(b) => Cow::Borrowed(b),
            None => Cow::Borrowed(&h.raw),
        },
        Resolution::Custom(b) => Cow::Borrowed(b),
    }
}

/// Whether each stage blob ends with a newline (`None`: no such stage).
/// A conflict at the very end of a file gets its sides' final newline
/// back: git adds one before each marker, so the working file cannot say.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tails {
    pub base: Option<bool>,
    pub ours: Option<bool>,
    pub theirs: Option<bool>,
}

/// The result file: stable text as read, each hunk as resolved.
pub fn assemble(doc: &Doc, res: &[Resolution], tails: Tails) -> Vec<u8> {
    let mut out = Vec::new();
    let mut k = 0;
    let mut last: Option<&Resolution> = None;
    for s in &doc.segments {
        match s {
            Segment::Text(t) => {
                out.extend_from_slice(t);
                last = None;
            }
            Segment::Conflict(h) => {
                let r = res.get(k).unwrap_or(&Resolution::Unresolved);
                out.extend_from_slice(&hunk_bytes(h, r));
                last = Some(r);
                k += 1;
            }
        }
    }
    // A side chosen at the end of the file keeps that side's (missing)
    // final newline.
    let tail = match last {
        Some(Resolution::Ours | Resolution::TheirsThenOurs) => tails.ours,
        Some(Resolution::Theirs | Resolution::OursThenTheirs) => tails.theirs,
        Some(Resolution::Base) => tails.base,
        _ => None,
    };
    if tail == Some(false) && out.ends_with(b"\n") {
        out.pop();
        if out.ends_with(b"\r") {
            out.pop();
        }
    }
    out
}

/// Is this hunk resolved? Edited text that still holds a conflict is not.
pub fn is_resolved(h: &Hunk, r: &Resolution, marker_size: usize) -> bool {
    match r {
        Resolution::Unresolved => false,
        Resolution::Base => h.base.is_some(),
        Resolution::Custom(b) => markers::parse(b, marker_size).hunk_count() == 0,
        _ => true,
    }
}

/// `(resolved, total)` hunks.
pub fn progress(doc: &Doc, res: &[Resolution]) -> (usize, usize) {
    let mut done = 0;
    let mut total = 0;
    for (k, h) in doc.hunks().enumerate() {
        total += 1;
        if is_resolved(
            h,
            res.get(k).unwrap_or(&Resolution::Unresolved),
            doc.marker_size,
        ) {
            done += 1;
        }
    }
    (done, total)
}

/// Text for an edit box: lossy UTF-8, `\r\n` shown as `\n`.
pub fn edit_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&markers::to_lf(bytes)).into_owned()
}

/// Bytes of edited hunk text: line endings in `eol`, and a final line
/// ending added unless the hunk is the last thing in the file (where a
/// missing newline is kept as typed). An empty edit removes the hunk.
pub fn custom_bytes(text: &str, eol: Eol, at_eof: bool) -> Vec<u8> {
    let eol = if eol == Eol::None { Eol::Lf } else { eol };
    let mut out = markers::with_eol(text.as_bytes(), eol);
    if !out.is_empty() && !at_eof && !out.ends_with(b"\n") {
        out.extend_from_slice(eol.bytes());
    }
    out
}

/// Bytes of a whole edited file: line endings in the file's main style.
pub fn whole_file_bytes(text: &str, eol: Eol) -> Vec<u8> {
    markers::with_eol(text.as_bytes(), eol)
}

/// Which lines of the result still hold conflicts (1-based start lines),
/// and stray start / end markers outside conflicts.
pub fn remaining(result: &[u8], marker_size: usize) -> (Vec<usize>, Vec<usize>) {
    let d = markers::parse(result, marker_size);
    (
        d.hunks().map(|h| h.start_line + 1).collect(),
        d.stray.iter().map(|l| l + 1).collect(),
    )
}

/// "Line 12", "Lines 12 and 40", "Lines 12, 40 and 77", "Lines 12, 40, 77
/// and 3 more".
pub fn lines_phrase(lines: &[usize]) -> String {
    match lines {
        [] => String::new(),
        [a] => format!("Line {a}"),
        [a, b] => format!("Lines {a} and {b}"),
        _ if lines.len() <= 4 => {
            let (last, rest) = lines.split_last().expect("non-empty");
            let rest: Vec<String> = rest.iter().map(usize::to_string).collect();
            format!("Lines {} and {last}", rest.join(", "))
        }
        _ => {
            let head: Vec<String> = lines[..3].iter().map(usize::to_string).collect();
            format!("Lines {} and {} more", head.join(", "), lines.len() - 3)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::conflict::markers::parse;

    const TEXT: &str = "top\n<<<<<<< HEAD\nO1\n||||||| base\nB1\n=======\nT1\n>>>>>>> f\nmid\n<<<<<<< HEAD\nO2\n=======\nT2\n>>>>>>> f\n";

    fn doc() -> Doc {
        parse(TEXT.as_bytes(), 7)
    }

    #[test]
    fn every_resolution() {
        let d = doc();
        let t = Tails::default();
        let r = |a: Resolution, b: Resolution| String::from_utf8(assemble(&d, &[a, b], t)).unwrap();
        assert_eq!(r(Resolution::Unresolved, Resolution::Unresolved), TEXT);
        assert_eq!(
            r(Resolution::Ours, Resolution::Theirs),
            "top\nO1\nmid\nT2\n"
        );
        assert_eq!(
            r(Resolution::OursThenTheirs, Resolution::TheirsThenOurs),
            "top\nO1\nT1\nmid\nT2\nO2\n"
        );
        assert_eq!(r(Resolution::Base, Resolution::Ours), "top\nB1\nmid\nO2\n");
        // No base known for the second hunk: it stays as it was.
        assert!(r(Resolution::Ours, Resolution::Base).ends_with(">>>>>>> f\n"));
        assert_eq!(
            r(
                Resolution::Custom(b"mine\n".to_vec()),
                Resolution::Custom(Vec::new())
            ),
            "top\nmine\nmid\n"
        );
        // Missing resolutions count as unresolved.
        assert_eq!(String::from_utf8(assemble(&d, &[], t)).unwrap(), TEXT);
    }

    #[test]
    fn progress_counts() {
        let d = doc();
        assert_eq!(progress(&d, &[]), (0, 2));
        assert_eq!(progress(&d, &[Resolution::Ours]), (1, 2));
        assert_eq!(
            progress(&d, &[Resolution::Ours, Resolution::Base]),
            (1, 2),
            "base unknown for hunk 2"
        );
        let still = Resolution::Custom(b"<<<<<<< a\nx\n=======\ny\n>>>>>>> b\n".to_vec());
        assert_eq!(progress(&d, &[still, Resolution::Theirs]), (1, 2));
    }

    #[test]
    fn final_newline_of_the_chosen_side() {
        let text = "a\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> f\n";
        let d = parse(text.as_bytes(), 7);
        let tails = Tails {
            base: Some(true),
            ours: Some(false),
            theirs: Some(true),
        };
        assert_eq!(assemble(&d, &[Resolution::Ours], tails), b"a\nours");
        assert_eq!(assemble(&d, &[Resolution::Theirs], tails), b"a\ntheirs\n");
        assert_eq!(
            assemble(&d, &[Resolution::TheirsThenOurs], tails),
            b"a\ntheirs\nours"
        );
        // Not at the end of the file: nothing changes.
        let d2 = parse(format!("{text}tail\n").as_bytes(), 7);
        assert_eq!(
            assemble(&d2, &[Resolution::Ours], tails),
            b"a\nours\ntail\n"
        );
        // CRLF.
        let crlf = "<<<<<<< HEAD\r\nours\r\n=======\r\ntheirs\r\n>>>>>>> f\r\n";
        let d3 = parse(crlf.as_bytes(), 7);
        assert_eq!(assemble(&d3, &[Resolution::Ours], tails), b"ours");
        assert_eq!(assemble(&d3, &[Resolution::Theirs], tails), b"theirs\r\n");
    }

    #[test]
    fn crlf_is_kept_by_side_choices_and_edits() {
        let text = "a\r\n<<<<<<< HEAD\r\nours\r\n=======\r\ntheirs\r\n>>>>>>> f\r\nz\r\n";
        let d = parse(text.as_bytes(), 7);
        assert_eq!(
            assemble(&d, &[Resolution::Theirs], Tails::default()),
            b"a\r\ntheirs\r\nz\r\n"
        );
        let h = d.hunks().next().unwrap();
        assert_eq!(
            edit_text(&h.raw),
            "<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> f\n"
        );
        let typed = custom_bytes("one\ntwo", h.eol, false);
        assert_eq!(typed, b"one\r\ntwo\r\n");
        assert_eq!(
            assemble(&d, &[Resolution::Custom(typed)], Tails::default()),
            b"a\r\none\r\ntwo\r\nz\r\n"
        );
        assert_eq!(custom_bytes("end", Eol::Lf, true), b"end");
        assert_eq!(custom_bytes("", Eol::CrLf, false), b"");
        assert_eq!(custom_bytes("x\r\n", Eol::CrLf, false), b"x\r\n");
        assert_eq!(whole_file_bytes("a\nb", Eol::CrLf), b"a\r\nb");
    }

    #[test]
    fn remaining_markers() {
        let (c, s) = remaining(TEXT.as_bytes(), 7);
        assert_eq!(c, vec![2, 10]);
        assert!(s.is_empty());
        let (c, s) = remaining(b"a\n<<<<<<< left over\nb\n", 7);
        assert!(c.is_empty());
        assert_eq!(s, vec![2]);
        assert_eq!(lines_phrase(&[12]), "Line 12");
        assert_eq!(lines_phrase(&[12, 40]), "Lines 12 and 40");
        assert_eq!(lines_phrase(&[12, 40, 77]), "Lines 12, 40 and 77");
        assert_eq!(
            lines_phrase(&[1, 2, 3, 4, 5, 6]),
            "Lines 1, 2, 3 and 3 more"
        );
    }

    #[test]
    fn names() {
        assert_eq!(Resolution::from_name("ours"), Some(Resolution::Ours));
        assert_eq!(
            Resolution::from_name("both"),
            Some(Resolution::OursThenTheirs)
        );
        assert_eq!(
            Resolution::from_name("both-theirs"),
            Some(Resolution::TheirsThenOurs)
        );
        assert_eq!(Resolution::from_name("base"), Some(Resolution::Base));
        assert_eq!(Resolution::from_name("nope"), None);
        assert_eq!(Resolution::Custom(vec![]).label(), "Edited");
    }
}
