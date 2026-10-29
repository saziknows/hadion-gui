//! The pseudo-terminal's output on its way to the VT100 screen.
//!
//! [`Filter`] passes everything through to `vt100` except:
//! - Terminal queries, which it takes out of the stream so the session can
//!   answer them ([`answer`]). ConPTY asks for the cursor position
//!   (`ESC[6n`) before it sends anything else and waits for the reply, so
//!   without this no shell ever appears.
//! - `ESC[3J` (erase the scrollback, sent by `clear`), which vt100 0.15
//!   ignores: it is passed on and also reported as
//!   [`Item::ClearScrollback`].
//! - Dim text (SGR 2), which vt100 0.15 drops. While dim is on, the
//!   foreground colour is rewritten to a marker that [`decode_fg`] turns
//!   back into "this colour, dimmed": `rgb(1,2,3)` is the default colour,
//!   `rgb(1,3,n)` palette colour `n`; true colours are blended towards the
//!   background here. Nothing is rewritten while dim is off.
//!
//! Escape sequences split across reads are kept until they are complete.

/// What a chunk of output turned into, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    /// Bytes for the screen.
    Data(Vec<u8>),
    /// A question the program asked the terminal.
    Query(Query),
    /// `ESC[3J`: drop the scrollback.
    ClearScrollback,
}

/// A terminal query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Query {
    /// `ESC[6n` (DSR): where is the cursor?
    CursorPosition,
    /// `ESC[?6n` (DECXCPR).
    CursorPositionDec,
    /// `ESC[5n`: are you there?
    Status,
    /// `ESC[c` / `ESC[0c` (DA1): what are you?
    PrimaryAttributes,
    /// `ESC[>c` (DA2).
    SecondaryAttributes,
    /// `ESC[>q` (XTVERSION).
    Version,
    /// `ESC[18t`: the text area in characters.
    TextAreaChars,
    /// `ESC[?n$p` (DECRQM for a private mode).
    DecMode(u16),
    /// `ESC[n$p` (DECRQM for an ANSI mode).
    AnsiMode(u16),
    /// `OSC 10;?`: the default foreground colour (`st`: terminated by ST,
    /// else BEL; the reply uses the same).
    Foreground { st: bool },
    /// `OSC 11;?`: the default background colour.
    Background { st: bool },
}

/// The default colours a session reports and dims towards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colors {
    pub fg: [u8; 3],
    pub bg: [u8; 3],
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            fg: [0xdc, 0xdf, 0xe6],
            bg: [0x1c, 0x1f, 0x25],
        }
    }
}

/// Foreground marker for "default colour, dimmed".
pub const DIM_DEFAULT: (u8, u8, u8) = (1, 2, 3);
/// First two components of the marker for "palette colour n, dimmed".
pub const DIM_INDEXED: (u8, u8) = (1, 3);

/// A foreground colour as decoded from the screen: the colour, and whether
/// it is dimmed.
pub fn decode_fg(c: vt100::Color) -> (vt100::Color, bool) {
    match c {
        vt100::Color::Rgb(r, g, b) if (r, g, b) == DIM_DEFAULT => (vt100::Color::Default, true),
        vt100::Color::Rgb(r, g, n) if (r, g) == DIM_INDEXED => (vt100::Color::Idx(n), true),
        other => (other, false),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fg {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum St {
    Ground,
    Esc,
    Csi,
    Osc,
    /// ESC seen inside an OSC (the start of ST, or an abort).
    OscEsc,
    /// An OSC too long to keep: passed through until its end.
    OscPass,
    OscPassEsc,
    /// DCS / SOS / PM / APC: passed through until ST.
    Str,
    StrEsc,
}

const MAX_CSI: usize = 256;
const MAX_OSC: usize = 4096;

/// The output filter of one session (it keeps state between reads).
pub struct Filter {
    state: St,
    seq: Vec<u8>,
    dim: bool,
    fg: Fg,
    /// Background true colours are dimmed towards.
    pub dim_bg: [u8; 3],
}

impl Default for Filter {
    fn default() -> Self {
        Self::new()
    }
}

impl Filter {
    pub fn new() -> Self {
        Self {
            state: St::Ground,
            seq: Vec::new(),
            dim: false,
            fg: Fg::Default,
            dim_bg: Colors::default().bg,
        }
    }

    /// Split `input` into screen data, queries and scrollback clears.
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<Item>) {
        let mut data: Vec<u8> = Vec::with_capacity(input.len());
        let mut i = 0;
        while i < input.len() {
            let b = input[i];
            match self.state {
                St::Ground => {
                    if b == 0x1b {
                        self.seq.clear();
                        self.seq.push(b);
                        self.state = St::Esc;
                    } else {
                        data.push(b);
                    }
                }
                St::Esc => match b {
                    b'[' => {
                        self.seq.push(b);
                        self.state = St::Csi;
                    }
                    b']' => {
                        self.seq.push(b);
                        self.state = St::Osc;
                    }
                    b'P' | b'X' | b'^' | b'_' => {
                        data.append(&mut self.seq);
                        data.push(b);
                        self.state = St::Str;
                    }
                    0x1b => {
                        // ESC ESC: the first one stands alone.
                        data.push(0x1b);
                    }
                    _ => {
                        data.append(&mut self.seq);
                        data.push(b);
                        self.state = St::Ground;
                    }
                },
                St::Csi => {
                    if b == 0x1b {
                        // Aborted by a new sequence.
                        data.append(&mut self.seq);
                        self.seq.push(b);
                        self.state = St::Esc;
                    } else if b == 0x18 || b == 0x1a {
                        data.append(&mut self.seq);
                        data.push(b);
                        self.state = St::Ground;
                    } else {
                        self.seq.push(b);
                        if (0x40..=0x7e).contains(&b) {
                            self.finish_csi(&mut data, out);
                            self.state = St::Ground;
                        } else if self.seq.len() > MAX_CSI {
                            data.append(&mut self.seq);
                            self.state = St::Ground;
                        }
                    }
                }
                St::Osc => {
                    if b == 0x07 {
                        self.seq.push(b);
                        self.finish_osc(false, &mut data, out);
                        self.state = St::Ground;
                    } else if b == 0x1b {
                        self.state = St::OscEsc;
                    } else if b == 0x18 || b == 0x1a {
                        data.append(&mut self.seq);
                        data.push(b);
                        self.state = St::Ground;
                    } else {
                        self.seq.push(b);
                        if self.seq.len() > MAX_OSC {
                            data.append(&mut self.seq);
                            self.state = St::OscPass;
                        }
                    }
                }
                St::OscEsc => {
                    if b == b'\\' {
                        self.seq.extend_from_slice(b"\x1b\\");
                        self.finish_osc(true, &mut data, out);
                        self.state = St::Ground;
                    } else {
                        // ESC ends the OSC and starts a new sequence.
                        data.append(&mut self.seq);
                        self.seq.push(0x1b);
                        self.state = St::Esc;
                        continue; // look at `b` again in the Esc state
                    }
                }
                St::OscPass => {
                    data.push(b);
                    if b == 0x07 {
                        self.state = St::Ground;
                    } else if b == 0x1b {
                        self.state = St::OscPassEsc;
                    }
                }
                St::OscPassEsc => {
                    if b == b'\\' {
                        data.push(b);
                        self.state = St::Ground;
                    } else {
                        // The ESC already went out; treat it as a new start.
                        data.pop();
                        self.seq.clear();
                        self.seq.push(0x1b);
                        self.state = St::Esc;
                        continue;
                    }
                }
                St::Str => {
                    data.push(b);
                    if b == 0x1b {
                        self.state = St::StrEsc;
                    }
                }
                St::StrEsc => {
                    data.push(b);
                    self.state = if b == b'\\' { St::Ground } else { St::Str };
                }
            }
            i += 1;
        }
        if !data.is_empty() {
            out.push(Item::Data(data));
        }
    }

    fn flush(data: &mut Vec<u8>, out: &mut Vec<Item>) {
        if !data.is_empty() {
            out.push(Item::Data(std::mem::take(data)));
        }
    }

    fn finish_csi(&mut self, data: &mut Vec<u8>, out: &mut Vec<Item>) {
        let seq = std::mem::take(&mut self.seq);
        let fin = *seq.last().unwrap_or(&0);
        let mut body = &seq[2..seq.len() - 1];
        let prefix = match body.first() {
            Some(c @ (b'<' | b'=' | b'>' | b'?')) => {
                body = &body[1..];
                Some(*c)
            }
            _ => None,
        };
        let split = body
            .iter()
            .position(|c| (0x20..=0x2f).contains(c))
            .unwrap_or(body.len());
        let (params, inter) = body.split_at(split);
        let query = match (prefix, params, inter, fin) {
            (None, b"" | b"0", b"", b'c') => Some(Query::PrimaryAttributes),
            (Some(b'>'), b"" | b"0", b"", b'c') => Some(Query::SecondaryAttributes),
            (None, b"5", b"", b'n') => Some(Query::Status),
            (None, b"6", b"", b'n') => Some(Query::CursorPosition),
            (Some(b'?'), b"6", b"", b'n') => Some(Query::CursorPositionDec),
            (Some(b'>'), b"" | b"0", b"", b'q') => Some(Query::Version),
            (None, b"18", b"", b't') => Some(Query::TextAreaChars),
            (Some(b'?'), n, b"$", b'p') => parse_u16(n).map(Query::DecMode),
            (None, n, b"$", b'p') => parse_u16(n).map(Query::AnsiMode),
            _ => None,
        };
        if let Some(q) = query {
            Self::flush(data, out);
            out.push(Item::Query(q));
            return;
        }
        if prefix.is_none() && inter.is_empty() && fin == b'J' && params == b"3" {
            data.extend_from_slice(&seq);
            Self::flush(data, out);
            out.push(Item::ClearScrollback);
            return;
        }
        if prefix.is_none() && inter.is_empty() && fin == b'm' {
            let before = self.dim;
            self.apply_sgr(params);
            if before || self.dim {
                data.extend_from_slice(b"\x1b[");
                data.extend_from_slice(if params.is_empty() { b"0" } else { params });
                data.push(b';');
                data.extend_from_slice(self.fg_params().as_bytes());
                data.push(b'm');
                return;
            }
        }
        data.extend_from_slice(&seq);
    }

    fn finish_osc(&mut self, st: bool, data: &mut Vec<u8>, out: &mut Vec<Item>) {
        let seq = std::mem::take(&mut self.seq);
        let end = if st { seq.len() - 2 } else { seq.len() - 1 };
        let body = &seq[2..end];
        let query = match body {
            b"10;?" => Some(Query::Foreground { st }),
            b"11;?" => Some(Query::Background { st }),
            _ => None,
        };
        match query {
            Some(q) => {
                Self::flush(data, out);
                out.push(Item::Query(q));
            }
            None => data.extend_from_slice(&seq),
        }
    }

    /// Track dim and the foreground colour through one SGR sequence (the
    /// same reading vt100 gives it).
    fn apply_sgr(&mut self, params: &[u8]) {
        let groups: Vec<Vec<u16>> = if params.is_empty() {
            vec![vec![0]]
        } else {
            params
                .split(|c| *c == b';')
                .map(|g| {
                    g.split(|c| *c == b':')
                        .map(|n| parse_u16(n).unwrap_or(0))
                        .collect()
                })
                .collect()
        };
        let mut i = 0;
        while i < groups.len() {
            let g = &groups[i];
            let n = g.first().copied().unwrap_or(0);
            if g.len() > 1 {
                // Colon form: `38:5:n`, `38:2:r:g:b`, `38:2:cs:r:g:b`.
                if n == 38 {
                    match g.as_slice() {
                        [_, 5, idx] => self.fg = Fg::Idx(*idx as u8),
                        [_, 2, r, gg, b] => self.fg = Fg::Rgb(*r as u8, *gg as u8, *b as u8),
                        [_, 2, _, r, gg, b, ..] => self.fg = Fg::Rgb(*r as u8, *gg as u8, *b as u8),
                        _ => {}
                    }
                }
                i += 1;
                continue;
            }
            match n {
                0 => {
                    self.dim = false;
                    self.fg = Fg::Default;
                }
                2 => self.dim = true,
                22 => self.dim = false,
                30..=37 => self.fg = Fg::Idx((n - 30) as u8),
                39 => self.fg = Fg::Default,
                90..=97 => self.fg = Fg::Idx((n - 82) as u8),
                38 | 48 => {
                    let kind = groups.get(i + 1).and_then(|g| g.first()).copied();
                    let val = |k: usize| {
                        groups
                            .get(i + k)
                            .and_then(|g| g.first())
                            .copied()
                            .unwrap_or(0) as u8
                    };
                    match kind {
                        Some(5) => {
                            if n == 38 {
                                self.fg = Fg::Idx(val(2));
                            }
                            i += 2;
                        }
                        Some(2) => {
                            if n == 38 {
                                self.fg = Fg::Rgb(val(2), val(3), val(4));
                            }
                            i += 4;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    /// SGR parameters that set the foreground vt100 should store now.
    fn fg_params(&self) -> String {
        if self.dim {
            match self.fg {
                Fg::Default => {
                    let (r, g, b) = DIM_DEFAULT;
                    format!("38;2;{r};{g};{b}")
                }
                Fg::Idx(n) => {
                    let (r, g) = DIM_INDEXED;
                    format!("38;2;{r};{g};{n}")
                }
                Fg::Rgb(r, g, b) => {
                    let [br, bg, bb] = self.dim_bg;
                    let mix = |a: u8, z: u8| ((a as u16 + z as u16) / 2) as u8;
                    format!("38;2;{};{};{}", mix(r, br), mix(g, bg), mix(b, bb))
                }
            }
        } else {
            match self.fg {
                Fg::Default => "39".to_owned(),
                Fg::Idx(n) if n < 8 => format!("{}", 30 + n as u16),
                Fg::Idx(n) if n < 16 => format!("{}", 90 + (n - 8) as u16),
                Fg::Idx(n) => format!("38;5;{n}"),
                Fg::Rgb(r, g, b) => format!("38;2;{r};{g};{b}"),
            }
        }
    }
}

fn parse_u16(s: &[u8]) -> Option<u16> {
    if s.is_empty() || s.len() > 5 || !s.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(s).ok()?.parse().ok()
}

/// The reply to a query, from the current screen.
pub fn answer(q: Query, screen: &vt100::Screen, colors: &Colors) -> Vec<u8> {
    let osc_color = |n: u8, [r, g, b]: [u8; 3], st: bool| {
        let mut s = format!("\x1b]{n};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}");
        s.push_str(if st { "\x1b\\" } else { "\x07" });
        s.into_bytes()
    };
    match q {
        Query::CursorPosition => {
            let (row, col) = screen.cursor_position();
            format!("\x1b[{};{}R", row + 1, col + 1).into_bytes()
        }
        Query::CursorPositionDec => {
            let (row, col) = screen.cursor_position();
            format!("\x1b[?{};{}R", row + 1, col + 1).into_bytes()
        }
        Query::Status => b"\x1b[0n".to_vec(),
        Query::PrimaryAttributes => b"\x1b[?1;2c".to_vec(),
        Query::SecondaryAttributes => b"\x1b[>0;10;1c".to_vec(),
        Query::Version => b"\x1bP>|hadion\x1b\\".to_vec(),
        Query::TextAreaChars => {
            let (rows, cols) = screen.size();
            format!("\x1b[8;{rows};{cols}t").into_bytes()
        }
        Query::DecMode(n) => {
            use vt100::{MouseProtocolEncoding as E, MouseProtocolMode as M};
            let set = |on: bool| if on { 1 } else { 2 };
            let mode = screen.mouse_protocol_mode();
            let enc = screen.mouse_protocol_encoding();
            let v = match n {
                1 => set(screen.application_cursor()),
                25 => set(!screen.hide_cursor()),
                47 | 1047 | 1049 => set(screen.alternate_screen()),
                66 => set(screen.application_keypad()),
                2004 => set(screen.bracketed_paste()),
                9 => set(mode == M::Press),
                1000 => set(mode == M::PressRelease),
                1002 => set(mode == M::ButtonMotion),
                1003 => set(mode == M::AnyMotion),
                1005 => set(enc == E::Utf8),
                1006 => set(enc == E::Sgr),
                _ => 0,
            };
            format!("\x1b[?{n};{v}$y").into_bytes()
        }
        Query::AnsiMode(n) => format!("\x1b[{n};0$y").into_bytes(),
        Query::Foreground { st } => osc_color(10, colors.fg, st),
        Query::Background { st } => osc_color(11, colors.bg, st),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(chunks: &[&[u8]]) -> Vec<Item> {
        let mut f = Filter::new();
        let mut out = Vec::new();
        for c in chunks {
            f.feed(c, &mut out);
        }
        // Merge adjacent data items so splitting does not change results.
        let mut merged: Vec<Item> = Vec::new();
        for item in out {
            match (merged.last_mut(), item) {
                (Some(Item::Data(a)), Item::Data(b)) => a.extend(b),
                (_, item) => merged.push(item),
            }
        }
        merged
    }

    fn data(s: &[u8]) -> Item {
        Item::Data(s.to_vec())
    }

    #[test]
    fn plain_output_passes_through() {
        let s: &[u8] = b"hello \x1b[1;31mred\x1b[0m \x1b]0;title\x07 \x1b[?25l\x1b[H";
        assert_eq!(feed_all(&[s]), vec![data(s)]);
    }

    #[test]
    fn conpty_cursor_query_is_taken_out() {
        let out = feed_all(&[b"\x1b[6n"]);
        assert_eq!(out, vec![Item::Query(Query::CursorPosition)]);
        let out = feed_all(&[b"ab\x1b[6ncd"]);
        assert_eq!(
            out,
            vec![data(b"ab"), Item::Query(Query::CursorPosition), data(b"cd")]
        );
    }

    #[test]
    fn queries_split_across_reads() {
        let out = feed_all(&[b"x\x1b", b"[", b"6", b"n", b"y"]);
        assert_eq!(
            out,
            vec![data(b"x"), Item::Query(Query::CursorPosition), data(b"y")]
        );
        let out = feed_all(&[b"\x1b]11", b";?\x1b", b"\\"]);
        assert_eq!(out, vec![Item::Query(Query::Background { st: true })]);
    }

    #[test]
    fn every_query_kind() {
        let cases: &[(&[u8], Query)] = &[
            (b"\x1b[c", Query::PrimaryAttributes),
            (b"\x1b[0c", Query::PrimaryAttributes),
            (b"\x1b[>c", Query::SecondaryAttributes),
            (b"\x1b[>0c", Query::SecondaryAttributes),
            (b"\x1b[5n", Query::Status),
            (b"\x1b[?6n", Query::CursorPositionDec),
            (b"\x1b[>q", Query::Version),
            (b"\x1b[18t", Query::TextAreaChars),
            (b"\x1b[?2004$p", Query::DecMode(2004)),
            (b"\x1b[4$p", Query::AnsiMode(4)),
            (b"\x1b]10;?\x07", Query::Foreground { st: false }),
            (b"\x1b]11;?\x1b\\", Query::Background { st: true }),
        ];
        for (input, q) in cases {
            assert_eq!(feed_all(&[input]), vec![Item::Query(*q)], "{input:?}");
        }
        // Look-alikes are not queries.
        for s in [
            &b"\x1b[2n"[..],
            b"\x1b[=c",
            b"\x1b[14t",
            b"\x1b]10;#fff\x07",
        ] {
            assert_eq!(feed_all(&[s]), vec![data(s)], "{s:?}");
        }
    }

    #[test]
    fn erase_scrollback_is_reported() {
        let out = feed_all(&[b"\x1b[H\x1b[2J\x1b[3J"]);
        assert_eq!(
            out,
            vec![data(b"\x1b[H\x1b[2J\x1b[3J"), Item::ClearScrollback]
        );
    }

    #[test]
    fn strings_are_not_parsed_inside() {
        // A DCS payload that looks like a query stays data.
        let s: &[u8] = b"\x1bPq\x1b[6n-ish\x1b\\after\x1b_apc\x1b\\";
        assert_eq!(feed_all(&[s]), vec![data(s)]);
        // Long OSCs (hyperlinks, titles) pass through untouched.
        let long = format!("\x1b]8;;https://example.com/{}\x07x", "a".repeat(5000));
        assert_eq!(feed_all(&[long.as_bytes()]), vec![data(long.as_bytes())]);
    }

    fn parse(bytes: &[u8]) -> vt100::Parser {
        let mut f = Filter::new();
        let mut items = Vec::new();
        f.feed(bytes, &mut items);
        let mut p = vt100::Parser::new(4, 40, 0);
        for i in items {
            if let Item::Data(d) = i {
                p.process(&d);
            }
        }
        p
    }

    #[test]
    fn dim_is_kept_in_the_colour() {
        let p = parse(b"\x1b[2ma\x1b[22mb\x1b[2;31mc\x1b[0md\x1b[2;38;2;200;100;0me\x1b[mf");
        let s = p.screen();
        let fg = |col| decode_fg(s.cell(0, col).unwrap().fgcolor());
        assert_eq!(fg(0), (vt100::Color::Default, true));
        assert_eq!(fg(1), (vt100::Color::Default, false));
        assert_eq!(fg(2), (vt100::Color::Idx(1), true));
        assert_eq!(fg(3), (vt100::Color::Default, false));
        // True colour: blended towards the default background.
        let bg = Colors::default().bg;
        let mix = |a: u8, z: u8| ((a as u16 + z as u16) / 2) as u8;
        assert_eq!(
            fg(4),
            (
                vt100::Color::Rgb(mix(200, bg[0]), mix(100, bg[1]), mix(0, bg[2])),
                false
            )
        );
        assert_eq!(fg(5), (vt100::Color::Default, false));
    }

    #[test]
    fn colour_changes_while_dim_stay_dim() {
        let p = parse(b"\x1b[2;34ma\x1b[32mb\x1b[39mc\x1b[22md\x1b[95;2me\x1b[22mf");
        let s = p.screen();
        let fg = |col| decode_fg(s.cell(0, col).unwrap().fgcolor());
        assert_eq!(fg(0), (vt100::Color::Idx(4), true));
        assert_eq!(fg(1), (vt100::Color::Idx(2), true));
        assert_eq!(fg(2), (vt100::Color::Default, true));
        assert_eq!(fg(3), (vt100::Color::Default, false));
        assert_eq!(fg(4), (vt100::Color::Idx(13), true));
        // Leaving dim restores the real colour.
        assert_eq!(fg(5), (vt100::Color::Idx(13), false));
    }

    #[test]
    fn bold_and_other_attributes_survive_the_rewrite() {
        let p = parse(b"\x1b[1;2;4ma");
        let c = p.screen().cell(0, 0).unwrap().clone();
        assert!(c.bold());
        assert!(c.underline());
        assert_eq!(decode_fg(c.fgcolor()), (vt100::Color::Default, true));
    }

    #[test]
    fn untouched_without_dim() {
        let mut f = Filter::new();
        let mut out = Vec::new();
        f.feed(b"\x1b[1;38;5;196mx", &mut out);
        assert_eq!(out, vec![data(b"\x1b[1;38;5;196mx")]);
    }

    #[test]
    fn answers() {
        let mut p = vt100::Parser::new(24, 80, 0);
        p.process(b"\x1b[5;10H");
        let s = p.screen();
        let c = Colors {
            fg: [0xff, 0x00, 0x80],
            bg: [0x01, 0x02, 0x03],
        };
        assert_eq!(answer(Query::CursorPosition, s, &c), b"\x1b[5;10R");
        assert_eq!(answer(Query::CursorPositionDec, s, &c), b"\x1b[?5;10R");
        assert_eq!(answer(Query::PrimaryAttributes, s, &c), b"\x1b[?1;2c");
        assert_eq!(answer(Query::Status, s, &c), b"\x1b[0n");
        assert_eq!(answer(Query::TextAreaChars, s, &c), b"\x1b[8;24;80t");
        assert_eq!(
            answer(Query::Foreground { st: false }, s, &c),
            b"\x1b]10;rgb:ffff/0000/8080\x07"
        );
        assert_eq!(
            answer(Query::Background { st: true }, s, &c),
            b"\x1b]11;rgb:0101/0202/0303\x1b\\"
        );
        assert_eq!(answer(Query::DecMode(2004), s, &c), b"\x1b[?2004;2$y");
        assert_eq!(answer(Query::DecMode(25), s, &c), b"\x1b[?25;1$y");
        assert_eq!(answer(Query::DecMode(7777), s, &c), b"\x1b[?7777;0$y");
        p.process(b"\x1b[?2004h\x1b[?1h\x1b[?1049h");
        let s = p.screen();
        assert_eq!(answer(Query::DecMode(2004), s, &c), b"\x1b[?2004;1$y");
        assert_eq!(answer(Query::DecMode(1), s, &c), b"\x1b[?1;1$y");
        assert_eq!(answer(Query::DecMode(1049), s, &c), b"\x1b[?1049;1$y");
    }
}
