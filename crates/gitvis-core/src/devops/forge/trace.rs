//! Job logs of GitLab and Azure Pipelines in one shape: lines with colour
//! spans, a kind (error, warning, command, section header) and the
//! (possibly nested) collapsible section they belong to.
//!
//! - GitLab traces mark sections with
//!   `\e[0Ksection_start:UNIX:NAME[collapsed=true]\r\e[0KHeader` and
//!   `\e[0Ksection_end:UNIX:NAME\r\e[0K`; newer runners prefix every line
//!   with `2026-10-26T18:02:51.489457Z 00O ` (`00O+` continues the previous
//!   line).
//! - Azure DevOps logs use the GitHub Actions format (timestamps,
//!   `##[group]`, `##[error]`, `##[section]`), parsed by [`crate::joblog`].

use std::collections::HashSet;

use crate::github::parse_timestamp;
use crate::joblog::{self, ColorSpan, JobLog, LogKind, strip_ansi};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Text,
    /// The first line of a section (click to fold).
    Header,
    Error,
    Warning,
    /// A command the job ran (`$ make`).
    Command,
    /// A step banner that does not fold (`##[section]Starting: Build`).
    Banner,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub spans: Vec<ColorSpan>,
    pub kind: LineKind,
    /// Innermost section containing the line (for a header: the section
    /// around it, not the one it opens).
    pub section: Option<u32>,
    /// For a header line: the section it opens.
    pub opens: Option<u32>,
    /// `HH:MM:SS.mmm` (UTC) when the log had timestamps.
    pub clock: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    /// GitLab section name (`step_script`), or the group title.
    pub name: String,
    /// Line index of the header.
    pub header: usize,
    /// Lines after the header that belong to it (nested sections included).
    pub len: usize,
    pub parent: Option<u32>,
    pub depth: u8,
    /// Seconds between start and end markers.
    pub duration: Option<i64>,
    /// Folded when the log opens (`[collapsed=true]`).
    pub collapsed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForgeLog {
    pub lines: Vec<Line>,
    pub sections: Vec<Section>,
    pub errors: Vec<usize>,
    pub warnings: Vec<usize>,
    pub bytes: usize,
    pub max_chars: usize,
}

impl ForgeLog {
    /// Is line `i` inside a folded section?
    pub fn hidden(&self, i: usize, folded: &HashSet<u32>) -> bool {
        let mut s = self.lines[i].section;
        while let Some(id) = s {
            if folded.contains(&id) {
                return true;
            }
            s = self.sections.get(id as usize).and_then(|x| x.parent);
        }
        false
    }

    /// Sections folded when the log opens.
    pub fn initially_folded(&self) -> HashSet<u32> {
        self.sections
            .iter()
            .enumerate()
            .filter(|(_, s)| s.collapsed)
            .map(|(i, _)| i as u32)
            .collect()
    }

    /// Lines containing `needle` (case-insensitive); all lines when empty.
    pub fn matching(&self, needle: &str) -> Vec<usize> {
        let n = needle.trim().to_lowercase();
        if n.is_empty() {
            return (0..self.lines.len()).collect();
        }
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.text.to_lowercase().contains(&n))
            .map(|(i, _)| i)
            .collect()
    }

    /// The lines of a section, header included, as text.
    pub fn section_text(&self, id: u32) -> String {
        let Some(s) = self.sections.get(id as usize) else {
            return String::new();
        };
        self.lines[s.header..(s.header + 1 + s.len).min(self.lines.len())]
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn push(&mut self, mut line: Line) -> usize {
        let i = self.lines.len();
        if line.kind == LineKind::Text {
            let t = line.text.trim_start();
            if t.starts_with("ERROR:") || t.starts_with("ERROR ") {
                line.kind = LineKind::Error;
            } else if t.starts_with("WARNING:") {
                line.kind = LineKind::Warning;
            } else if t.starts_with("$ ") {
                line.kind = LineKind::Command;
            }
        }
        match line.kind {
            LineKind::Error => self.errors.push(i),
            LineKind::Warning => self.warnings.push(i),
            _ => {}
        }
        self.max_chars = self.max_chars.max(line.text.chars().count());
        self.lines.push(line);
        i
    }
}

/// `2026-10-26T18:02:51.489457Z 00O+rest` → (`18:02:51.489`, continues, `rest`).
fn split_timestamp(line: &str) -> Option<(String, bool, &str)> {
    let (ts, rest) = line.split_once(' ')?;
    let (secs, ms) = parse_timestamp(ts)?;
    let b = rest.as_bytes();
    if b.len() < 4
        || !b[0].is_ascii_hexdigit()
        || !b[1].is_ascii_hexdigit()
        || !matches!(b[2], b'O' | b'E')
        || !matches!(b[3], b' ' | b'+')
    {
        return None;
    }
    let day = secs.rem_euclid(86_400);
    let clock = format!(
        "{:02}:{:02}:{:02}.{ms:03}",
        day / 3600,
        (day / 60) % 60,
        day % 60
    );
    Some((clock, b[3] == b'+', &rest[4..]))
}

/// Remove leading `\e[0K` (erase line) sequences.
fn strip_erase(s: &str) -> &str {
    let mut s = s;
    while let Some(r) = s.strip_prefix("\x1b[0K") {
        s = r;
    }
    s
}

/// A section marker at the start of `s`: (`start`?, unix time, name,
/// options, rest after the marker's `\r`).
fn marker(s: &str) -> Option<(bool, i64, String, String, &str)> {
    let s = strip_erase(s);
    let (start, body) = if let Some(b) = s.strip_prefix("section_start:") {
        (true, b)
    } else if let Some(b) = s.strip_prefix("section_end:") {
        (false, b)
    } else {
        return None;
    };
    let (head, rest) = match body.find('\r') {
        Some(i) => (&body[..i], &body[i + 1..]),
        None => (body, ""),
    };
    let (ts, name) = head.split_once(':')?;
    let ts: i64 = ts.trim().parse().ok()?;
    let (name, opts) = match name.find('[') {
        Some(i) => (&name[..i], name[i + 1..].trim_end_matches(']')),
        None => (name, ""),
    };
    Some((
        start,
        ts,
        name.trim().to_owned(),
        opts.to_owned(),
        strip_erase(rest),
    ))
}

/// Keep the last state of a line rewritten with `\r` (progress output).
fn last_state(s: &str) -> &str {
    let s = s.trim_end_matches('\r');
    match s.rfind('\r') {
        Some(i) => &s[i + 1..],
        None => s,
    }
}

/// Parse a GitLab job trace.
pub fn parse_gitlab(raw: &[u8]) -> ForgeLog {
    let text = String::from_utf8_lossy(raw);
    // Join timestamped continuation lines first.
    let mut logical: Vec<(String, String)> = Vec::new();
    for physical in text.split('\n') {
        let physical = physical.strip_suffix('\r').unwrap_or(physical);
        match (split_timestamp(physical), logical.last_mut()) {
            (Some((_, true, rest)), Some(last)) => last.1.push_str(rest),
            (Some((clock, _, rest)), _) => logical.push((clock, rest.to_owned())),
            (None, _) => logical.push((String::new(), physical.to_owned())),
        }
    }
    let mut log = ForgeLog {
        bytes: raw.len(),
        ..Default::default()
    };
    // Open sections: (id, name, start time).
    let mut stack: Vec<(u32, String, i64)> = Vec::new();
    for (clock, line) in &logical {
        let mut rest: &str = line;
        let mut opened: Option<u32> = None;
        while let Some((start, ts, name, opts, after)) = marker(rest) {
            rest = after;
            if start {
                let id = log.sections.len() as u32;
                let parent = stack.last().map(|(p, _, _)| *p);
                log.sections.push(Section {
                    name: name.clone(),
                    header: log.lines.len(),
                    len: 0,
                    parent,
                    depth: stack.len().min(255) as u8,
                    duration: None,
                    collapsed: opts.contains("collapsed=true"),
                });
                stack.push((id, name, ts));
                opened = Some(id);
            } else if let Some(pos) = stack.iter().rposition(|(_, n, _)| *n == name) {
                for (id, _, started) in stack.drain(pos..).rev() {
                    let s = &mut log.sections[id as usize];
                    s.len = log.lines.len().saturating_sub(s.header + 1);
                    s.duration = Some((ts - started).max(0));
                    if opened == Some(id) {
                        // Opened and closed on one line: no header line.
                        opened = None;
                    }
                }
            }
        }
        let content = last_state(rest);
        let (plain, spans) = strip_ansi(content);
        match opened {
            Some(id) => {
                let s = &log.sections[id as usize];
                let title = if plain.trim().is_empty() {
                    s.name.clone()
                } else {
                    plain
                };
                let section = s.parent;
                let i = log.push(Line {
                    text: title,
                    spans: if content.trim().is_empty() {
                        Vec::new()
                    } else {
                        spans
                    },
                    kind: LineKind::Header,
                    section,
                    opens: Some(id),
                    clock: clock.clone(),
                });
                log.sections[id as usize].header = i;
            }
            None => {
                if plain.is_empty() && content.is_empty() && line.contains("section_end:") {
                    continue;
                }
                log.push(Line {
                    text: plain,
                    spans,
                    kind: LineKind::Text,
                    section: stack.last().map(|(id, _, _)| *id),
                    opens: None,
                    clock: clock.clone(),
                });
            }
        }
    }
    // Sections never closed run to the end.
    for (id, _, _) in stack {
        let s = &mut log.sections[id as usize];
        s.len = log.lines.len().saturating_sub(s.header + 1);
    }
    // Drop a trailing empty line left by the final newline.
    if log
        .lines
        .last()
        .is_some_and(|l| l.text.is_empty() && l.opens.is_none())
    {
        log.lines.pop();
        for s in &mut log.sections {
            s.len = s.len.min(log.lines.len().saturating_sub(s.header + 1));
        }
    }
    log
}

/// An Azure DevOps (GitHub Actions style) log.
pub fn parse_azure(raw: &[u8]) -> ForgeLog {
    from_joblog(joblog::parse(raw))
}

/// Convert a [`JobLog`] (flat `##[group]` sections) to a [`ForgeLog`].
pub fn from_joblog(j: JobLog) -> ForgeLog {
    let mut log = ForgeLog {
        bytes: j.bytes,
        ..Default::default()
    };
    log.sections = j
        .groups
        .iter()
        .map(|g| Section {
            name: j
                .lines
                .get(g.header)
                .map(|l| l.text.clone())
                .unwrap_or_default(),
            header: g.header,
            len: g.len,
            parent: None,
            depth: 0,
            duration: None,
            collapsed: false,
        })
        .collect();
    for (i, l) in j.lines.into_iter().enumerate() {
        let opens = (l.kind == LogKind::Group)
            .then(|| {
                j.groups
                    .iter()
                    .position(|g| g.header == i)
                    .map(|g| g as u32)
            })
            .flatten();
        let kind = match l.kind {
            LogKind::Group => LineKind::Header,
            LogKind::Error => LineKind::Error,
            LogKind::Warning => LineKind::Warning,
            LogKind::Command => LineKind::Command,
            LogKind::Section => LineKind::Banner,
            _ => LineKind::Text,
        };
        log.push(Line {
            text: l.text,
            spans: l.spans,
            kind,
            section: if opens.is_some() { None } else { l.group },
            opens,
            clock: l.clock,
        });
    }
    log
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shaped like a real gitlab-runner trace (timestamps, continuation
    // lines, nested and collapsed sections, ANSI colours).
    const TRACE: &str = "2026-10-26T18:02:51.489457Z 00O \x1b[0KRunning with gitlab-runner 19.6.0\x1b[0;m\n\
2026-10-26T18:02:51.489890Z 00O section_start:1793037771:prepare_executor\r\x1b[0K\n\
2026-10-26T18:02:51.489895Z 00O+\x1b[0K\x1b[36;1mPreparing the \"custom\" executor\x1b[0;m\x1b[0;m\n\
2026-10-26T18:02:51.502253Z 00O Creating virtual machine for the job...\n\
2026-10-26T18:09:32.871693Z 00O \x1b[0;33mWARNING: Cleanup script failed: exit status 2\x1b[0;m\n\
2026-10-26T18:12:06.989002Z 00O section_end:1793038326:prepare_executor\r\x1b[0K\n\
2026-10-26T18:12:06.989144Z 00O+section_start:1793038326:step_script[collapsed=true]\r\x1b[0K\n\
2026-10-26T18:12:06.989150Z 00O+\x1b[0K\x1b[36;1mExecuting \"step_script\" stage\x1b[0;m\n\
2026-10-26T18:12:07.000000Z 01O \x1b[32;1m$ make test\x1b[0;m\n\
2026-10-26T18:12:07.100000Z 01O section_start:1793038327:inner\r\x1b[0KInner block\n\
2026-10-26T18:12:07.200000Z 01O 10%\r50%\r100%\n\
2026-10-26T18:12:08.000000Z 01O section_end:1793038328:inner\r\x1b[0K\n\
2026-10-26T18:12:09.000000Z 01O ok\n\
2026-10-26T18:14:19.371150Z 00O section_end:1793038459:step_script\r\x1b[0K\n\
2026-10-26T18:14:44.530046Z 00O \x1b[31;1mERROR: Job failed: exit status 1\x1b[0;m\n";

    #[test]
    fn parses_gitlab_sections() {
        let log = parse_gitlab(TRACE.as_bytes());
        let texts: Vec<&str> = log.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "Running with gitlab-runner 19.6.0",
                "Preparing the \"custom\" executor",
                "Creating virtual machine for the job...",
                "WARNING: Cleanup script failed: exit status 2",
                "Executing \"step_script\" stage",
                "$ make test",
                "Inner block",
                "100%",
                "ok",
                "ERROR: Job failed: exit status 1",
            ]
        );
        assert_eq!(log.sections.len(), 3);
        let prep = &log.sections[0];
        assert_eq!(
            (prep.name.as_str(), prep.header, prep.len),
            ("prepare_executor", 1, 2)
        );
        assert_eq!(prep.duration, Some(1793038326 - 1793037771));
        assert!(!prep.collapsed);
        let script = &log.sections[1];
        assert_eq!((script.header, script.len, script.depth), (4, 4, 0));
        assert!(script.collapsed);
        let inner = &log.sections[2];
        assert_eq!(
            (inner.header, inner.len, inner.parent, inner.depth),
            (6, 1, Some(1), 1)
        );
        assert_eq!(inner.duration, Some(1));
        assert_eq!(log.lines[1].kind, LineKind::Header);
        assert_eq!(log.lines[1].opens, Some(0));
        assert_eq!(log.lines[1].section, None);
        assert_eq!(
            log.lines[6].section,
            Some(1),
            "a header belongs to its parent"
        );
        assert_eq!(log.lines[7].section, Some(2));
        assert_eq!(log.lines[5].kind, LineKind::Command);
        assert_eq!(log.errors, vec![9]);
        assert_eq!(log.warnings, vec![3]);
        assert_eq!(log.lines[0].clock, "18:02:51.489");
        assert!(!log.lines[1].spans.is_empty());

        let folded = log.initially_folded();
        assert_eq!(folded, [1u32].into_iter().collect());
        let visible: Vec<usize> = (0..log.lines.len())
            .filter(|i| !log.hidden(*i, &folded))
            .collect();
        assert_eq!(visible, vec![0, 1, 2, 3, 4, 9]);
        let none = HashSet::new();
        assert!((0..log.lines.len()).all(|i| !log.hidden(i, &none)));
        assert_eq!(log.matching("make").len(), 1);
        assert!(log.section_text(2).starts_with("Inner block\n100%"));
    }

    #[test]
    fn plain_traces_without_timestamps() {
        let raw = "\x1b[0Ksection_start:100:get_sources\r\x1b[0K\x1b[0K\x1b[36;1mGetting source\x1b[0;m\nFetching changes...\n\x1b[0Ksection_end:105:get_sources\r\x1b[0K\n$ echo hi\nhi\n";
        let log = parse_gitlab(raw.as_bytes());
        let texts: Vec<&str> = log.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["Getting source", "Fetching changes...", "$ echo hi", "hi"]
        );
        assert_eq!(log.sections[0].duration, Some(5));
        assert_eq!(log.sections[0].len, 1);
        assert_eq!(log.lines[2].section, None);
        assert_eq!(log.lines[0].clock, "");
    }

    #[test]
    fn unclosed_sections_and_empty_headers() {
        let raw = "section_start:1:build\r\x1b[0K\nline\n";
        let log = parse_gitlab(raw.as_bytes());
        assert_eq!(
            log.lines[0].text, "build",
            "empty header text falls back to the name"
        );
        assert_eq!(log.sections[0].len, 1);
        assert_eq!(log.sections[0].duration, None);
    }

    #[test]
    fn azure_logs() {
        let raw = "2026-10-26T04:50:27.3645819Z ##[section]Starting: Build\n\
2026-10-26T04:50:27.3664672Z ##[group]Task details\n\
2026-10-26T04:50:27.3665222Z Version: 2.0\n\
2026-10-26T04:50:27.3668276Z ##[endgroup]\n\
2026-10-26T04:54:40.0000000Z ##[error]Bash exited with code '1'.\n\
2026-10-26T04:54:41.0000000Z ##[warning]careful\n";
        let log = parse_azure(raw.as_bytes());
        let kinds: Vec<LineKind> = log.lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![
                LineKind::Banner,
                LineKind::Header,
                LineKind::Text,
                LineKind::Error,
                LineKind::Warning
            ]
        );
        assert_eq!(log.sections.len(), 1);
        assert_eq!(log.sections[0].name, "Task details");
        assert_eq!(log.lines[1].opens, Some(0));
        assert_eq!(log.lines[2].section, Some(0));
        assert_eq!(log.errors, vec![3]);
        let folded: HashSet<u32> = [0u32].into_iter().collect();
        assert!(log.hidden(2, &folded));
        assert!(!log.hidden(1, &folded));
    }
}
