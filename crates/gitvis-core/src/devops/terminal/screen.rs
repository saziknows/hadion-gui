//! The terminal screen: a `vt100` parser plus gitvis's own scrollback, the
//! styled runs the panel draws, selections and their text, ANSI palettes.
//!
//! Why its own scrollback: vt100 0.15 computes `rows - scrollback_offset`
//! when it reads a scrolled view, which overflows (a panic in debug builds)
//! as soon as the view is more than one screen up. So [`Screen`] keeps
//! vt100's offset small: it feeds output in segments that scroll at most
//! `rows - 1` lines, counts the lines each segment scrolled off (vt100
//! moves a non-zero offset along with every line it pushes, so an offset
//! of 1 works as a counter even when vt100's own small scrollback is full),
//! reads them while they are still within one screen, and stores them as
//! [`Line`]s. The panel's viewport is [`Screen::offset`]: history lines
//! above the live screen.
//!
//! Rows have stable numbers: history line `i` is row `dropped + i` (lines
//! dropped from the front of a full history keep their numbers), screen
//! row `r` is `dropped + history + r`. A row's number follows its text as
//! output scrolls, so selections stay on what was selected.

use std::collections::VecDeque;

use super::keys::{MouseEncoding, MouseMode};
use super::stream::decode_fg;

/// A cell colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Color {
    /// The terminal's default foreground or background.
    #[default]
    Default,
    /// Palette entry (0–15 theme colours, 16–231 cube, 232–255 greys).
    Idx(u8),
    Rgb(u8, u8, u8),
}

impl From<vt100::Color> for Color {
    fn from(c: vt100::Color) -> Self {
        match c {
            vt100::Color::Default => Color::Default,
            vt100::Color::Idx(n) => Color::Idx(n),
            vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
        }
    }
}

/// How a run of cells is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub dim: bool,
}

impl Style {
    fn of(cell: &vt100::Cell) -> Self {
        let (fg, dim) = decode_fg(cell.fgcolor());
        Style {
            fg: fg.into(),
            bg: cell.bgcolor().into(),
            bold: cell.bold(),
            italic: cell.italic(),
            underline: cell.underline(),
            inverse: cell.inverse(),
            dim,
        }
    }

    /// Spaces in this style draw nothing.
    fn invisible_blank(&self) -> bool {
        self.bg == Color::Default && !self.inverse && !self.underline
    }
}

/// Consecutive cells of one style. In a run that is not `wide` and covers
/// more than one cell, each character is one cell; a cell holding a
/// combining sequence or a double-width character is a run of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    /// First column.
    pub col: u16,
    /// Columns covered.
    pub cells: u16,
    pub text: String,
    pub style: Style,
    /// A single double-width character.
    pub wide: bool,
}

/// One row of text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    /// Runs that draw something (blank cells on the default background
    /// are left out; place runs by `col`).
    pub runs: Vec<Run>,
    /// The text continues on the next row (soft wrap).
    pub wrapped: bool,
}

impl Line {
    /// One string per column (empty for blank cells and the second half
    /// of wide characters).
    pub fn cells(&self, cols: u16) -> Vec<String> {
        let mut out = vec![String::new(); cols as usize];
        for run in &self.runs {
            let col = run.col as usize;
            if run.wide || run.cells == 1 {
                if let Some(c) = out.get_mut(col) {
                    *c = run.text.clone();
                }
                continue;
            }
            for (i, ch) in run.text.chars().enumerate() {
                if let Some(c) = out.get_mut(col + i) {
                    if ch != ' ' {
                        *c = ch.to_string();
                    }
                }
            }
        }
        out
    }
}

/// Read visible row `r` of a vt100 screen.
fn line_of(screen: &vt100::Screen, r: u16) -> Line {
    let cols = screen.size().1;
    let mut runs: Vec<Run> = Vec::new();
    let mut col = 0u16;
    while col < cols {
        let Some(cell) = screen.cell(r, col) else {
            break;
        };
        if cell.is_wide_continuation() {
            col += 1;
            continue;
        }
        let style = Style::of(cell);
        let wide = cell.is_wide();
        let width = if wide { 2 } else { 1 };
        let contents = cell.contents();
        let single = contents.chars().count() <= 1;
        let text = if contents.is_empty() { " " } else { &contents };
        match runs.last_mut() {
            // Only one-character cells merge (a run of two or more cells is
            // one character per cell by construction).
            Some(last)
                if last.style == style
                    && !last.wide
                    && !wide
                    && single
                    && (last.cells > 1 || last.text.chars().count() == 1)
                    && last.col + last.cells == col =>
            {
                last.text.push_str(text);
                last.cells += 1;
            }
            _ => runs.push(Run {
                col,
                cells: width,
                text: text.to_owned(),
                style,
                wide,
            }),
        }
        col += width;
    }
    // Trailing blanks of invisible runs draw nothing; neither do runs of
    // nothing but blanks.
    for run in &mut runs {
        if run.style.invisible_blank()
            && !run.wide
            && run.text.chars().count() == run.cells as usize
        {
            let trimmed = run.text.trim_end_matches(' ').len();
            let removed = run.text.len() - trimmed;
            run.text.truncate(trimmed);
            run.cells -= removed as u16;
        }
    }
    runs.retain(|run| !(run.style.invisible_blank() && run.text.trim_matches(' ').is_empty()));
    Line {
        runs,
        wrapped: screen.row_wrapped(r),
    }
}

/// Everything the panel needs to draw a session, read under one lock.
#[derive(Clone, Debug, Default)]
pub struct View {
    pub rows: u16,
    pub cols: u16,
    pub lines: Vec<Line>,
    /// Stable number of the first row shown.
    pub top_row: usize,
    /// Cursor (row, col) in this view, when it is on screen.
    pub cursor: Option<(u16, u16)>,
    /// The program hid the cursor (`ESC[?25l`).
    pub cursor_hidden: bool,
    /// History lines above the screen that are shown (0 = live screen).
    pub offset: usize,
    /// History lines kept.
    pub scrollback: usize,
    pub alt_screen: bool,
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub mouse_mode: MouseMode,
    pub mouse_encoding: MouseEncoding,
    pub title: String,
    pub bells: usize,
}

// ---------------------------------------------------------------------------
// Selections
// ---------------------------------------------------------------------------

/// A cell: stable row number and column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Point {
    pub row: usize,
    pub col: u16,
}

/// A selection from `anchor` (where the drag started) to `head` (where the
/// pointer is), both cells included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Point,
    pub head: Point,
}

impl Selection {
    pub fn new(anchor: Point, head: Point) -> Self {
        Self { anchor, head }
    }

    /// Start and end in reading order.
    pub fn ordered(&self) -> (Point, Point) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    pub fn contains(&self, p: Point) -> bool {
        let (a, b) = self.ordered();
        a <= p && p <= b
    }

    /// The selected columns of row `row` (inclusive), if any.
    pub fn cols_in_row(&self, row: usize, cols: u16) -> Option<(u16, u16)> {
        let (a, b) = self.ordered();
        if row < a.row || row > b.row || cols == 0 {
            return None;
        }
        let start = if row == a.row { a.col } else { 0 };
        let end = if row == b.row { b.col } else { cols - 1 };
        let end = end.min(cols - 1);
        (start <= end).then_some((start, end))
    }
}

/// Characters that belong to a word for double-click selection (paths,
/// URLs, branch names and flags select as one).
fn is_word_char(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || "-_.~/\\:@#%+=?&$".contains(c))
}

// ---------------------------------------------------------------------------
// Screen
// ---------------------------------------------------------------------------

/// vt100's own scrollback: just enough to read lines right after they
/// scrolled off (more than any realistic screen height).
const VT_SCROLLBACK: usize = 512;

/// A vt100 screen with gitvis's scrollback history.
pub struct Screen {
    parser: vt100::Parser,
    hist: VecDeque<Line>,
    cap: usize,
    dropped: usize,
    offset: usize,
}

impl Screen {
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Self {
        Self {
            parser: vt100::Parser::new(rows.max(2), cols.max(2), VT_SCROLLBACK),
            hist: VecDeque::new(),
            cap: scrollback,
            dropped: 0,
            offset: 0,
        }
    }

    /// The vt100 screen (cursor, modes, title; the live screen's cells).
    pub fn vt(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    pub fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.set_size(rows.max(2), cols.max(2));
    }

    /// History lines kept.
    pub fn scrollback_len(&self) -> usize {
        self.hist.len()
    }

    /// History lines above the screen shown in the viewport.
    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn set_offset(&mut self, offset: usize) {
        self.offset = offset.min(self.hist.len());
    }

    /// Scroll the viewport up (`lines > 0`, into the history) or down.
    pub fn scroll_by(&mut self, lines: isize) {
        let o = self.offset as isize + lines;
        self.set_offset(o.max(0) as usize);
    }

    /// Stable number of the oldest kept row and of the last screen row.
    pub fn row_range(&self) -> (usize, usize) {
        let rows = self.size().0 as usize;
        (self.dropped, self.dropped + self.hist.len() + rows - 1)
    }

    /// Drop the history (`ESC[3J`, Clear).
    pub fn clear_scrollback(&mut self) {
        self.dropped += self.hist.len();
        self.hist.clear();
        self.offset = 0;
    }

    fn vt_len(&mut self) -> usize {
        self.parser.set_scrollback(usize::MAX);
        let n = self.parser.screen().scrollback();
        self.parser.set_scrollback(0);
        n
    }

    fn push_hist(&mut self, line: Line) {
        if self.cap == 0 {
            self.dropped += 1;
            return;
        }
        self.hist.push_back(line);
        while self.hist.len() > self.cap {
            self.hist.pop_front();
            self.dropped += 1;
        }
    }

    /// Feed output (complete or partial escape sequences both work).
    pub fn process(&mut self, bytes: &[u8]) {
        let rows = self.size().0 as usize;
        let max = rows.saturating_sub(1).clamp(1, VT_SCROLLBACK - 1);
        let mut start = 0;
        let mut count = 0usize;
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if b == 0x1b && bytes.get(i + 1) == Some(&b'[') {
                // CSI: a private mode change goes alone (it may switch
                // screens); `CSI n S` scrolls n lines.
                let mut j = i + 2;
                let private = bytes.get(j) == Some(&b'?');
                while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                    j += 1;
                }
                let end = (j + 1).min(bytes.len());
                if private {
                    self.segment(&bytes[start..i], true);
                    self.segment(&bytes[i..end], false);
                    start = end;
                    count = 0;
                    i = end;
                    continue;
                }
                if bytes.get(j) == Some(&b'S') {
                    let n = std::str::from_utf8(&bytes[i + 2..j])
                        .ok()
                        .and_then(|s| s.parse::<usize>().ok())
                        .unwrap_or(1)
                        .max(1);
                    if count + n > max {
                        self.segment(&bytes[start..i], true);
                        start = i;
                        count = 0;
                    }
                    count += n;
                }
                i = end;
                continue;
            }
            let scrolls = matches!(b, b'\n' | 0x0b | 0x0c)
                || (i > 0 && bytes[i - 1] == 0x1b && matches!(b, b'D' | b'E'));
            if scrolls {
                if count + 1 > max {
                    self.segment(&bytes[start..i], true);
                    start = i;
                    count = 0;
                }
                count += 1;
            }
            i += 1;
        }
        self.segment(&bytes[start..], true);
    }

    /// Process one segment; with `capture`, move the lines it scrolled off
    /// into the history.
    fn segment(&mut self, seg: &[u8], capture: bool) {
        if seg.is_empty() {
            return;
        }
        let alt_before = self.parser.screen().alternate_screen();
        let len_before = if capture { self.vt_len() } else { 0 };
        let sentinel = len_before.min(1);
        self.parser
            .set_scrollback(if capture { sentinel } else { 0 });
        self.parser.process(seg);
        let alt_after = self.parser.screen().alternate_screen();
        if !capture || alt_before != alt_after {
            // Switching screens: the counter belongs to the other grid.
            self.parser.set_scrollback(0);
            return;
        }
        let k = if sentinel == 0 {
            let after = self.vt_len();
            after.saturating_sub(len_before)
        } else {
            self.parser.screen().scrollback().saturating_sub(sentinel)
        };
        if k > 0 {
            let rows = self.size().0 as usize;
            let available = self.vt_len();
            let readable = k.min(rows.saturating_sub(1)).min(available);
            // Lines that scrolled off beyond what can be read (a huge
            // `CSI n S`) are kept as blank lines, so numbering stays right.
            for _ in readable..k {
                self.push_hist(Line::default());
            }
            self.parser.set_scrollback(readable);
            for v in 0..readable {
                let line = line_of(self.parser.screen(), v as u16);
                self.push_hist(line);
            }
            if self.offset > 0 {
                self.offset = (self.offset + k).min(self.hist.len());
            }
        }
        self.parser.set_scrollback(0);
    }

    /// The row with stable number `row`.
    pub fn line(&self, row: usize) -> Option<Line> {
        let idx = row.checked_sub(self.dropped)?;
        if idx < self.hist.len() {
            return self.hist.get(idx).cloned();
        }
        let r = idx - self.hist.len();
        let rows = self.size().0 as usize;
        (r < rows).then(|| line_of(self.parser.screen(), r as u16))
    }

    /// The view of the current viewport.
    pub fn view(&self) -> View {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let hist = self.hist.len();
        let offset = self.offset.min(hist);
        let top = hist - offset;
        let mut lines = Vec::with_capacity(rows as usize);
        for v in 0..rows as usize {
            let idx = top + v;
            if idx < hist {
                lines.push(self.hist[idx].clone());
            } else {
                lines.push(line_of(screen, (idx - hist) as u16));
            }
        }
        let (crow, ccol) = screen.cursor_position();
        let visible = crow as usize + offset;
        let cursor =
            (visible < rows as usize).then_some((visible as u16, ccol.min(cols.saturating_sub(1))));
        let mouse_mode = match screen.mouse_protocol_mode() {
            vt100::MouseProtocolMode::None => MouseMode::None,
            vt100::MouseProtocolMode::Press => MouseMode::Press,
            vt100::MouseProtocolMode::PressRelease => MouseMode::PressRelease,
            vt100::MouseProtocolMode::ButtonMotion => MouseMode::ButtonMotion,
            vt100::MouseProtocolMode::AnyMotion => MouseMode::AnyMotion,
        };
        let mouse_encoding = match screen.mouse_protocol_encoding() {
            vt100::MouseProtocolEncoding::Default => MouseEncoding::Default,
            vt100::MouseProtocolEncoding::Utf8 => MouseEncoding::Utf8,
            vt100::MouseProtocolEncoding::Sgr => MouseEncoding::Sgr,
        };
        View {
            rows,
            cols,
            lines,
            top_row: self.dropped + top,
            cursor,
            cursor_hidden: screen.hide_cursor(),
            offset,
            scrollback: hist,
            alt_screen: screen.alternate_screen(),
            app_cursor: screen.application_cursor(),
            bracketed_paste: screen.bracketed_paste(),
            mouse_mode,
            mouse_encoding,
            title: screen.title().to_owned(),
            bells: screen.audible_bell_count(),
        }
    }

    /// The text of a selection: trailing blanks of each line removed, soft
    /// wrapped rows joined without a line break.
    pub fn selection_text(&self, sel: &Selection) -> String {
        let (a, b) = sel.ordered();
        let cols = self.size().1;
        let (first, last_row) = self.row_range();
        let mut out = String::new();
        let mut pending = String::new();
        for row in a.row.max(first)..=b.row.min(last_row) {
            let Some(line) = self.line(row) else {
                continue;
            };
            let Some((c0, c1)) = sel.cols_in_row(row, cols) else {
                continue;
            };
            let cells = line.cells(cols);
            for cell in &cells[c0 as usize..=c1 as usize] {
                if cell.is_empty() {
                    pending.push(' ');
                } else {
                    pending.push_str(cell);
                }
            }
            let last = row == b.row.min(last_row);
            let joins = line.wrapped && c1 + 1 >= cols && !last;
            if !joins {
                out.push_str(pending.trim_end_matches(' '));
                pending.clear();
                if !last {
                    out.push('\n');
                }
            }
        }
        out.push_str(pending.trim_end_matches(' '));
        out
    }

    /// A selection of everything kept.
    pub fn select_all(&self) -> Selection {
        let (first, last) = self.row_range();
        let cols = self.size().1;
        Selection::new(
            Point { row: first, col: 0 },
            Point {
                row: last,
                col: cols - 1,
            },
        )
    }

    /// Every line in the history and on the screen.
    pub fn all_text(&self) -> String {
        self.selection_text(&self.select_all())
            .trim_end_matches('\n')
            .to_owned()
    }

    /// The word around `p` (inclusive columns), or the cell itself.
    pub fn word_at(&self, p: Point) -> (u16, u16) {
        let cols = self.size().1;
        let Some(line) = self.line(p.row) else {
            return (p.col, p.col);
        };
        let cells = line.cells(cols);
        let c = p.col as usize;
        if c >= cells.len() || !is_word_char(&cells[c]) {
            return (p.col, p.col);
        }
        let mut start = c;
        while start > 0 && is_word_char(&cells[start - 1]) {
            start -= 1;
        }
        let mut end = c;
        while end + 1 < cells.len() && is_word_char(&cells[end + 1]) {
            end += 1;
        }
        (start as u16, end as u16)
    }

    /// Escape codes that rebuild the live screen (for tests and debugging).
    pub fn contents(&self) -> String {
        self.parser.screen().contents()
    }
}

// ---------------------------------------------------------------------------
// Colours
// ---------------------------------------------------------------------------

/// The 16 theme colours on a dark background (the job log viewer's
/// palette, so colours match across gitvis).
pub const ANSI_DARK: [[u8; 3]; 16] = [
    [0x5c, 0x63, 0x70],
    [0xf0, 0x5d, 0x6c],
    [0x3d, 0xc9, 0x7e],
    [0xf5, 0xb0, 0x2e],
    [0x4f, 0x8c, 0xff],
    [0xb2, 0x7c, 0xf0],
    [0x2e, 0xc4, 0xd0],
    [0xd6, 0xda, 0xe2],
    [0x8a, 0x92, 0xa0],
    [0xff, 0x7b, 0x88],
    [0x6c, 0xe3, 0x9e],
    [0xff, 0xc9, 0x5c],
    [0x7f, 0xae, 0xff],
    [0xcc, 0x9f, 0xff],
    [0x5f, 0xdd, 0xe8],
    [0xff, 0xff, 0xff],
];

/// The 16 theme colours on a light background: darker, and "white" is a
/// grey that stays readable on white.
pub const ANSI_LIGHT: [[u8; 3]; 16] = [
    [0x24, 0x29, 0x2f],
    [0xc4, 0x1e, 0x3a],
    [0x11, 0x7a, 0x3e],
    [0x8a, 0x5a, 0x00],
    [0x1f, 0x5f, 0xd6],
    [0x82, 0x50, 0xdf],
    [0x1b, 0x7c, 0x83],
    [0x6e, 0x77, 0x81],
    [0x57, 0x60, 0x6a],
    [0xa4, 0x0e, 0x26],
    [0x1a, 0x7f, 0x37],
    [0x63, 0x3c, 0x01],
    [0x21, 0x8b, 0xff],
    [0xa4, 0x75, 0xf9],
    [0x31, 0x92, 0xaa],
    [0x8c, 0x95, 0x9f],
];

/// A palette entry: the 16 theme colours, the 6×6×6 cube, the grey ramp.
pub fn indexed(n: u8, ansi: &[[u8; 3]; 16]) -> [u8; 3] {
    match n {
        0..=15 => ansi[n as usize],
        16..=231 => {
            let n = n - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            [level(n / 36), level((n / 6) % 6), level(n % 6)]
        }
        _ => {
            let g = 8 + (n - 232) * 10;
            [g, g, g]
        }
    }
}

/// Resolved colours of a style: (foreground, background). Bold palette
/// colours 0–7 use their bright variant; inverse swaps the two; dim blends
/// the foreground halfway to the background.
pub fn resolve(
    style: &Style,
    ansi: &[[u8; 3]; 16],
    default_fg: [u8; 3],
    default_bg: [u8; 3],
) -> ([u8; 3], [u8; 3]) {
    let fg = match style.fg {
        Color::Default => default_fg,
        Color::Idx(n) if style.bold && n < 8 => indexed(n + 8, ansi),
        Color::Idx(n) => indexed(n, ansi),
        Color::Rgb(r, g, b) => [r, g, b],
    };
    let bg = match style.bg {
        Color::Default => default_bg,
        Color::Idx(n) => indexed(n, ansi),
        Color::Rgb(r, g, b) => [r, g, b],
    };
    let (mut fg, bg) = if style.inverse { (bg, fg) } else { (fg, bg) };
    if style.dim {
        for i in 0..3 {
            fg[i] = ((fg[i] as u16 + bg[i] as u16) / 2) as u8;
        }
    }
    (fg, bg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(rows: u16, cols: u16, sb: usize, input: &[u8]) -> Screen {
        let mut s = Screen::new(rows, cols, sb);
        s.process(input);
        s
    }

    fn texts(line: &Line) -> Vec<&str> {
        line.runs.iter().map(|r| r.text.as_str()).collect()
    }

    #[test]
    fn runs_group_styles_and_skip_blanks() {
        let s = screen(3, 20, 0, b"ab\x1b[31mcd\x1b[0m  \x1b[44m  \x1b[0mx");
        let v = s.view();
        let runs = &v.lines[0].runs;
        assert_eq!(texts(&v.lines[0]), vec!["ab", "cd", "  ", "x"], "{runs:?}");
        assert_eq!(runs[1].style.fg, Color::Idx(1));
        // Default-background spaces are dropped; coloured ones are kept.
        assert_eq!((runs[2].col, runs[2].cells), (6, 2));
        assert_eq!(runs[2].style.bg, Color::Idx(4));
        assert_eq!((runs[3].col, runs[3].cells), (8, 1));
        assert!(v.lines[1].runs.is_empty());
        assert_eq!(v.cursor, Some((0, 9)));
    }

    #[test]
    fn wide_and_combined_cells_are_their_own_run() {
        let s = screen(2, 10, 0, "a漢be\u{301}c".as_bytes());
        let v = s.view();
        let runs = &v.lines[0].runs;
        assert_eq!(
            texts(&v.lines[0]),
            vec!["a", "漢", "b", "e\u{301}", "c"],
            "{runs:?}"
        );
        assert_eq!((runs[1].col, runs[1].cells, runs[1].wide), (1, 2, true));
        assert_eq!(runs[2].col, 3);
        assert_eq!(runs[4].col, 5);
        let cells = v.lines[0].cells(10);
        assert_eq!(&cells[..6], &["a", "漢", "", "b", "e\u{301}", "c"]);
    }

    #[test]
    fn attributes_reach_the_view() {
        let s = screen(2, 20, 0, b"\x1b[1;3;4;7mx\x1b[0m\x1b[32my");
        let v = s.view();
        let st = v.lines[0].runs[0].style;
        assert!(st.bold && st.italic && st.underline && st.inverse && !st.dim);
        assert_eq!(v.lines[0].runs[1].style.fg, Color::Idx(2));
    }

    #[test]
    fn modes_and_cursor_visibility() {
        let mut s = screen(
            5,
            20,
            0,
            b"\x1b[?1h\x1b[?2004h\x1b[?25l\x1b[?1000h\x1b[?1006h\x1b]0;my title\x07",
        );
        let v = s.view();
        assert!(v.app_cursor && v.bracketed_paste && v.cursor_hidden);
        assert_eq!(v.mouse_mode, MouseMode::PressRelease);
        assert_eq!(v.mouse_encoding, MouseEncoding::Sgr);
        assert_eq!(v.title, "my title");
        s.process(b"\x1b[?1049h");
        assert!(s.view().alt_screen);
    }

    fn lines(n: usize) -> Vec<u8> {
        (0..n)
            .map(|i| format!("line {i}\r\n"))
            .collect::<String>()
            .into_bytes()
    }

    fn first_text(s: &Screen, row: usize) -> String {
        s.line(row)
            .and_then(|l| l.runs.first().map(|r| r.text.clone()))
            .unwrap_or_default()
    }

    #[test]
    fn history_keeps_every_line_in_one_big_chunk() {
        // 1000 lines in one write to a 5-row screen: far more than vt100's
        // own scrollback or one screen.
        let s = screen(5, 30, 5000, &lines(1000));
        // 1000 lines + the empty prompt row; 4 rows of text stay on screen.
        assert_eq!(s.scrollback_len(), 996);
        for i in [0, 1, 500, 995] {
            assert_eq!(first_text(&s, i), format!("line {i}"));
        }
        // Screen rows follow the history.
        assert_eq!(first_text(&s, 996), "line 996");
        assert_eq!(first_text(&s, 999), "line 999");
    }

    #[test]
    fn history_in_small_pieces_and_with_csi_scroll() {
        let mut s = Screen::new(4, 20, 100);
        for i in 0..20 {
            s.process(format!("l{i}\r").as_bytes());
            s.process(b"\n");
        }
        assert_eq!(s.scrollback_len(), 17);
        assert_eq!(first_text(&s, 0), "l0");
        assert_eq!(first_text(&s, 16), "l16");
        // `CSI 2 S` scrolls two lines up (both into the history).
        s.process(b"\x1b[H\x1b[2S");
        assert_eq!(s.scrollback_len(), 19);
        assert_eq!(first_text(&s, 17), "l17");
        assert_eq!(first_text(&s, 18), "l18");
    }

    #[test]
    fn full_history_drops_oldest_and_keeps_numbers() {
        let s = screen(3, 20, 10, &lines(40));
        assert_eq!(s.scrollback_len(), 10);
        let (first, last) = s.row_range();
        assert_eq!((first, last), (28, 40));
        assert_eq!(first_text(&s, 28), "line 28");
        assert!(s.line(5).is_none());
    }

    #[test]
    fn viewport_stays_on_its_text() {
        let mut s = screen(3, 20, 100, &lines(10));
        assert_eq!(s.scrollback_len(), 8);
        s.set_offset(2);
        let v = s.view();
        assert_eq!((v.offset, v.scrollback, v.top_row), (2, 8, 6));
        assert_eq!(v.lines[0].runs[0].text, "line 6");
        assert_eq!(v.cursor, None);
        s.process(b"more\r\n");
        let v = s.view();
        assert_eq!(v.offset, 3);
        assert_eq!(v.lines[0].runs[0].text, "line 6");
        s.scroll_by(-100);
        assert_eq!(s.offset(), 0);
        s.scroll_by(1000);
        assert_eq!(s.offset(), s.scrollback_len());
    }

    #[test]
    fn alternate_screen_does_not_touch_history() {
        let mut s = screen(3, 20, 100, &lines(5));
        let before = s.scrollback_len();
        s.process(b"\x1b[?1049h");
        s.process(&lines(10));
        assert_eq!(s.scrollback_len(), before);
        s.process(b"\x1b[?1049l");
        s.process(&lines(2));
        assert_eq!(s.scrollback_len(), before + 2);
        // The main screen came back; its rows scroll into the history.
        assert_eq!(first_text(&s, before), "line 3");
        assert_eq!(first_text(&s, before + 1), "line 4");
    }

    #[test]
    fn selection_text_trims_and_joins() {
        let s = screen(4, 10, 50, b"hello   \r\nwrapped-line-x\r\nlast");
        // Row 1 is "wrapped-li" (wraps), row 2 is "ne-x".
        let all = Selection::new(Point { row: 0, col: 0 }, Point { row: 3, col: 9 });
        assert_eq!(s.selection_text(&all), "hello\nwrapped-line-x\nlast");
        let part = Selection::new(Point { row: 1, col: 8 }, Point { row: 0, col: 1 });
        assert_eq!(s.selection_text(&part), "ello\nwrapped-l");
        let one = Selection::new(Point { row: 3, col: 0 }, Point { row: 3, col: 1 });
        assert_eq!(s.selection_text(&one), "la");
    }

    #[test]
    fn selection_reaches_into_history() {
        let s = screen(3, 20, 100, &lines(10));
        let sel = Selection::new(Point { row: 0, col: 0 }, Point { row: 1, col: 19 });
        assert_eq!(s.selection_text(&sel), "line 0\nline 1");
        let text = s.all_text();
        assert!(text.starts_with("line 0\nline 1\n"), "{text}");
        assert!(text.ends_with("line 9"), "{text}");
        // Wrapped lines in the history join too.
        let s = screen(3, 6, 100, b"abcdefghij\r\nx\r\ny\r\nz\r\n");
        assert_eq!(s.all_text(), "abcdefghij\nx\ny\nz");
    }

    #[test]
    fn words() {
        let s = screen(2, 40, 0, b"git log src/main.rs --oneline");
        assert_eq!(s.word_at(Point { row: 0, col: 10 }), (8, 18));
        assert_eq!(s.word_at(Point { row: 0, col: 1 }), (0, 2));
        assert_eq!(s.word_at(Point { row: 0, col: 3 }), (3, 3));
        assert_eq!(s.word_at(Point { row: 0, col: 22 }), (20, 28));
    }

    #[test]
    fn selection_rows() {
        let s = Selection::new(Point { row: 5, col: 3 }, Point { row: 3, col: 7 });
        assert_eq!(s.cols_in_row(2, 10), None);
        assert_eq!(s.cols_in_row(3, 10), Some((7, 9)));
        assert_eq!(s.cols_in_row(4, 10), Some((0, 9)));
        assert_eq!(s.cols_in_row(5, 10), Some((0, 3)));
        assert!(s.contains(Point { row: 4, col: 0 }));
        assert!(!s.contains(Point { row: 3, col: 6 }));
    }

    #[test]
    fn colours() {
        assert_eq!(indexed(1, &ANSI_DARK), ANSI_DARK[1]);
        assert_eq!(indexed(16, &ANSI_DARK), [0, 0, 0]);
        assert_eq!(indexed(196, &ANSI_DARK), [255, 0, 0]);
        assert_eq!(indexed(231, &ANSI_DARK), [255, 255, 255]);
        assert_eq!(indexed(232, &ANSI_DARK), [8, 8, 8]);
        assert_eq!(indexed(255, &ANSI_DARK), [238, 238, 238]);
        let fg = [200, 200, 200];
        let bg = [0, 0, 0];
        let plain = Style::default();
        assert_eq!(resolve(&plain, &ANSI_DARK, fg, bg), (fg, bg));
        let bold_red = Style {
            fg: Color::Idx(1),
            bold: true,
            ..Style::default()
        };
        assert_eq!(resolve(&bold_red, &ANSI_DARK, fg, bg).0, ANSI_DARK[9]);
        let inverse = Style {
            inverse: true,
            ..Style::default()
        };
        assert_eq!(resolve(&inverse, &ANSI_DARK, fg, bg), (bg, fg));
        let dim = Style {
            dim: true,
            ..Style::default()
        };
        assert_eq!(resolve(&dim, &ANSI_DARK, fg, bg).0, [100, 100, 100]);
        // Light palette: "white" is readable on white.
        assert!(ANSI_LIGHT[7].iter().all(|c| *c < 0xa0));
    }
}
