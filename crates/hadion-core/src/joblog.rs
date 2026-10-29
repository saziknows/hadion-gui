//! Parser for GitHub Actions job logs: one line per entry with a leading
//! timestamp, runner markers (`##[group]`, `##[error]`, …) and ANSI colours.

use crate::github::parse_timestamp;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogKind {
    Text,
    /// `##[group]title`: the lines up to `##[endgroup]` belong to it.
    Group,
    Error,
    Warning,
    Notice,
    Debug,
    /// `##[command]` or the `[command]` prefix used by actions/checkout.
    Command,
    Section,
}

/// A coloured run of characters (char offsets into `text`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorSpan {
    pub start: u32,
    pub end: u32,
    pub rgb: [u8; 3],
    pub bold: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogLine {
    /// Epoch milliseconds of the runner timestamp, if the line had one.
    pub time_ms: Option<i64>,
    /// `HH:MM:SS.mmm` in UTC, or empty.
    pub clock: String,
    /// The message with timestamp, marker and escape sequences removed.
    pub text: String,
    pub kind: LogKind,
    pub spans: Vec<ColorSpan>,
    /// Index into [`JobLog::groups`] when inside a group.
    pub group: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// Line index of the `##[group]` header.
    pub header: usize,
    /// Number of lines after the header that belong to the group.
    pub len: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JobLog {
    pub lines: Vec<LogLine>,
    pub groups: Vec<Group>,
    /// Indices of `##[error]` lines.
    pub errors: Vec<usize>,
    pub warnings: Vec<usize>,
    pub bytes: usize,
    /// The longest display text, in characters.
    pub max_chars: usize,
}

impl JobLog {
    /// The first line of each step, matched by timestamp: a step owns the
    /// lines from its start until the next step starts. Steps that never
    /// started are left out.
    pub fn step_starts(&self, steps: &[(u32, Option<i64>)]) -> Vec<(u32, usize)> {
        let mut out: Vec<(u32, usize)> = Vec::new();
        let mut cursor = 0usize;
        for (number, started) in steps {
            let Some(started) = started else { continue };
            let start_ms = started * 1000;
            while cursor < self.lines.len()
                && self.lines[cursor].time_ms.is_some_and(|t| t < start_ms)
            {
                cursor += 1;
            }
            out.push((*number, cursor));
        }
        out
    }

    /// Lines whose text contains `needle` (case-insensitive), with group
    /// headers kept so matches stay in context.
    pub fn matching(&self, needle: &str) -> Vec<usize> {
        let needle = needle.to_lowercase();
        if needle.is_empty() {
            return (0..self.lines.len()).collect();
        }
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.text.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }
}

/// Palette for the 16 ANSI colours, tuned for a dark background.
const ANSI: [[u8; 3]; 16] = [
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

fn xterm_256(n: u8) -> [u8; 3] {
    match n {
        0..=15 => ANSI[n as usize],
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

struct Pen {
    rgb: Option<[u8; 3]>,
    bold: bool,
}

impl Pen {
    fn apply(&mut self, params: &str) {
        let codes: Vec<u16> = params
            .split(';')
            .map(|c| c.parse::<u16>().unwrap_or(0))
            .collect();
        let mut i = 0;
        while i < codes.len() {
            match codes[i] {
                0 => {
                    self.rgb = None;
                    self.bold = false;
                }
                1 => self.bold = true,
                22 => self.bold = false,
                30..=37 => self.rgb = Some(ANSI[(codes[i] - 30) as usize]),
                90..=97 => self.rgb = Some(ANSI[(codes[i] - 90 + 8) as usize]),
                39 => self.rgb = None,
                38 => match codes.get(i + 1) {
                    Some(5) => {
                        self.rgb = codes.get(i + 2).map(|&n| xterm_256(n as u8));
                        i += 2;
                    }
                    Some(2) => {
                        if let (Some(&r), Some(&g), Some(&b)) =
                            (codes.get(i + 2), codes.get(i + 3), codes.get(i + 4))
                        {
                            self.rgb = Some([r as u8, g as u8, b as u8]);
                        }
                        i += 4;
                    }
                    _ => {}
                },
                48 => match codes.get(i + 1) {
                    Some(5) => i += 2,
                    Some(2) => i += 4,
                    _ => {}
                },
                _ => {}
            }
            i += 1;
        }
    }
}

/// Strip CSI escape sequences, turning SGR colours into spans.
pub fn strip_ansi(s: &str) -> (String, Vec<ColorSpan>) {
    if !s.contains('\x1b') {
        return (s.to_owned(), Vec::new());
    }
    let mut out = String::with_capacity(s.len());
    let mut spans = Vec::new();
    let mut pen = Pen {
        rgb: None,
        bold: false,
    };
    let mut chars = s.chars().peekable();
    let mut pos: u32 = 0;
    let mut open: Option<(u32, [u8; 3], bool)> = None;
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                let mut params = String::new();
                let mut terminator = None;
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() || c == '@' || c == '`' || c == '~' {
                        terminator = Some(c);
                        break;
                    }
                    params.push(c);
                }
                if terminator == Some('m') {
                    pen.apply(&params);
                    if let Some((start, rgb, bold)) = open.take()
                        && start < pos
                    {
                        spans.push(ColorSpan {
                            start,
                            end: pos,
                            rgb,
                            bold,
                        });
                    }
                    if let Some(rgb) = pen.rgb {
                        open = Some((pos, rgb, pen.bold));
                    } else if pen.bold {
                        // Bold without a colour: keep default text, no span.
                        open = None;
                    }
                }
            } else if chars.peek() == Some(&']') {
                // OSC: skip to BEL or ST.
                chars.next();
                let mut prev = '\0';
                for c in chars.by_ref() {
                    if c == '\x07' || (prev == '\x1b' && c == '\\') {
                        break;
                    }
                    prev = c;
                }
            }
            continue;
        }
        if c == '\r' {
            continue;
        }
        out.push(c);
        pos += 1;
    }
    if let Some((start, rgb, bold)) = open
        && start < pos
    {
        spans.push(ColorSpan {
            start,
            end: pos,
            rgb,
            bold,
        });
    }
    (out, spans)
}

/// Split a `YYYY-MM-DDTHH:MM:SS.fffffffZ ` prefix from a line.
fn split_timestamp(line: &str) -> (Option<(i64, u32)>, &str) {
    let end = line.find(' ').unwrap_or(line.len());
    let head = &line[..end];
    if head.len() >= 20
        && head.ends_with('Z')
        && head.as_bytes()[10] == b'T'
        && let Some(t) = parse_timestamp(head)
    {
        return (Some(t), line.get(end + 1..).unwrap_or(""));
    }
    (None, line)
}

fn clock(secs: i64, millis: u32) -> String {
    let day = secs.rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        day / 3600,
        (day / 60) % 60,
        day % 60,
        millis
    )
}

pub fn parse(raw: &[u8]) -> JobLog {
    let text = String::from_utf8_lossy(raw);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut log = JobLog {
        bytes: raw.len(),
        ..Default::default()
    };
    let mut open_group: Option<usize> = None;
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let (ts, rest) = split_timestamp(line);
        let (mut rest, spans) = strip_ansi(rest);
        let mut kind = LogKind::Text;
        if let Some(after) = rest.strip_prefix("##[") {
            if let Some((marker, msg)) = after.split_once(']') {
                let k = match marker {
                    "group" => Some(LogKind::Group),
                    "endgroup" => {
                        if let Some(g) = open_group.take() {
                            log.groups[g].len = log.lines.len() - log.groups[g].header - 1;
                        }
                        continue;
                    }
                    "error" => Some(LogKind::Error),
                    "warning" => Some(LogKind::Warning),
                    "notice" => Some(LogKind::Notice),
                    "debug" => Some(LogKind::Debug),
                    "command" => Some(LogKind::Command),
                    "section" => Some(LogKind::Section),
                    _ => None,
                };
                if let Some(k) = k {
                    kind = k;
                    rest = msg.to_owned();
                }
            }
        } else if let Some(cmd) = rest.strip_prefix("[command]") {
            kind = LogKind::Command;
            rest = cmd.to_owned();
        }
        // Spans were computed on the text before the marker was removed.
        let spans = if kind == LogKind::Text {
            spans
        } else {
            Vec::new()
        };
        let idx = log.lines.len();
        let group = match kind {
            LogKind::Group => {
                if let Some(g) = open_group.take() {
                    log.groups[g].len = idx - log.groups[g].header - 1;
                }
                open_group = Some(log.groups.len());
                log.groups.push(Group {
                    header: idx,
                    len: 0,
                });
                None
            }
            _ => open_group.map(|g| g as u32),
        };
        match kind {
            LogKind::Error => log.errors.push(idx),
            LogKind::Warning => log.warnings.push(idx),
            _ => {}
        }
        log.max_chars = log.max_chars.max(rest.chars().count());
        log.lines.push(LogLine {
            time_ms: ts.map(|(s, ms)| s * 1000 + ms as i64),
            clock: ts.map(|(s, ms)| clock(s, ms)).unwrap_or_default(),
            text: rest,
            kind,
            spans,
            group,
        });
    }
    // Drop a trailing empty line left by the final newline.
    if log
        .lines
        .last()
        .is_some_and(|l| l.text.is_empty() && l.time_ms.is_none())
    {
        log.lines.pop();
    }
    if let Some(g) = open_group {
        log.groups[g].len = log.lines.len() - log.groups[g].header - 1;
    }
    log
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\u{feff}2026-10-26T04:50:27.3645819Z Current runner version: '2.338.0'\n\
2026-10-26T04:50:27.3664672Z ##[group]Runner Image Provisioner\n\
2026-10-26T04:50:27.3665222Z Hosted Compute Agent\n\
2026-10-26T04:50:27.3665627Z Version: 20261021.618\n\
2026-10-26T04:50:27.3668276Z ##[endgroup]\n\
2026-10-26T04:53:03.9596432Z \x1b[36;1mset -eu\x1b[0m\n\
2026-10-26T04:54:31.8229048Z [command]/usr/bin/git config --local\n\
2026-10-26T04:54:40.0000000Z ##[error]Dependabot encountered an error\n\
2026-10-26T04:54:41.0000000Z ##[warning]careful\n";

    #[test]
    fn parses_markers_groups_and_colours() {
        let log = parse(SAMPLE.as_bytes());
        assert_eq!(log.lines.len(), 8, "endgroup lines are folded away");
        assert_eq!(log.lines[0].text, "Current runner version: '2.338.0'");
        assert_eq!(log.lines[0].clock, "04:50:27.364");
        assert_eq!(log.lines[1].kind, LogKind::Group);
        assert_eq!(log.lines[1].text, "Runner Image Provisioner");
        assert_eq!(log.groups, vec![Group { header: 1, len: 2 }]);
        assert_eq!(log.lines[2].group, Some(0));
        assert_eq!(log.lines[4].group, None);
        assert_eq!(log.lines[4].text, "set -eu");
        assert_eq!(
            log.lines[4].spans,
            vec![ColorSpan {
                start: 0,
                end: 7,
                rgb: ANSI[6],
                bold: true
            }]
        );
        assert_eq!(log.lines[5].kind, LogKind::Command);
        assert_eq!(log.lines[5].text, "/usr/bin/git config --local");
        assert_eq!(log.errors, vec![6]);
        assert_eq!(log.warnings, vec![7]);
        assert_eq!(log.lines[6].text, "Dependabot encountered an error");
        assert_eq!(log.max_chars, "Current runner version: '2.338.0'".len());
    }

    #[test]
    fn steps_by_timestamp() {
        let log = parse(SAMPLE.as_bytes());
        let t = |s: &str| parse_timestamp(s).map(|(s, _)| s);
        let steps = [
            (1, t("2026-10-26T04:50:27Z")),
            (2, t("2026-10-26T04:53:03Z")),
            (3, None),
            (4, t("2026-10-26T04:54:40Z")),
        ];
        assert_eq!(log.step_starts(&steps), vec![(1, 0), (2, 4), (4, 6)]);
    }

    #[test]
    fn filter_and_plain_lines() {
        let log = parse(b"no timestamp here\nsecond\x1b[31m red\x1b[0m line\n");
        assert_eq!(log.lines.len(), 2);
        assert!(log.lines[0].time_ms.is_none());
        assert_eq!(log.lines[1].text, "second red line");
        assert_eq!(log.lines[1].spans[0].start, 6);
        assert_eq!(log.lines[1].spans[0].end, 10);
        assert_eq!(log.matching("RED"), vec![1]);
        assert_eq!(log.matching(""), vec![0, 1]);
    }

    #[test]
    fn extended_colours() {
        let (text, spans) = strip_ansi("\x1b[38;5;196mX\x1b[38;2;1;2;3mY\x1b[0m");
        assert_eq!(text, "XY");
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].rgb, xterm_256(196));
        assert_eq!(spans[1].rgb, [1, 2, 3]);
        assert_eq!(xterm_256(16), [0, 0, 0]);
        assert_eq!(xterm_256(231), [255, 255, 255]);
    }
}
